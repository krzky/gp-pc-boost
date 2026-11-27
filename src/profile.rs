use crate::{
    journal::state_dir,
    mem::run_purge,
    power::{restore_plan, set_plan_by_target},
};
use std::{
    fs,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::Command,
};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW},
};
use winrust_common::{
    launch::program,
    proc::{elevated, kill, list as list_procs},
};

type ProcSpec = (&'static str, Option<&'static str>);

const WORK_SVCS: &[&str] = &[
    "Steam Client Service",
    "XblAuthManager",
    "XblGameSave",
    "XboxNetApiSvc",
    "EABackgroundService",
    "EpicOnlineServices",
];

const WORK_PROCS: &[ProcSpec] = &[
    ("steam.exe", Some("-silent")),
    ("steamwebhelper.exe", None),
    ("epicgameslauncher.exe", Some("-silent")),
    ("epicwebhelper.exe", None),
    ("battle.net.exe", Some("")),
    ("eadesktop.exe", Some("")),
    ("upc.exe", Some("")),
    ("galaxyclient.exe", Some("")),
    ("overwolf.exe", None),
    ("xboxpcapp.exe", None),
    ("gamebar.exe", None),
    ("gamebarftserver.exe", None),
];

const GAME_SVCS: &[&str] = &["DoSvc", "DiagTrack", "MapsBroker", "WerSvc"];

const GAME_PROCS: &[ProcSpec] = &[
    ("widgets.exe", None),
    ("phoneexperiencehost.exe", None),
];

fn profile_file() -> PathBuf {
    state_dir().join("active_profile.txt")
}

fn exe_path(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return None;
        }
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(h);
        ok.then(|| String::from_utf16_lossy(&buf[..len as usize]))
    }
}

fn run_sc(args: &[&str]) -> (bool, String) {
    match Command::new("sc.exe").args(args).creation_flags(0x0800_0000).output() {
        Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned()),
        Err(e) => (false, e.to_string()),
    }
}

fn is_service_running(svc: &str) -> bool {
    let (ok, out) = run_sc(&["query", svc]);
    ok && out.contains("RUNNING")
}

pub fn run_status() -> i32 {
    let f = profile_file();
    let Ok(text) = fs::read_to_string(&f) else {
        println!("Active Profile: none");
        return 0;
    };
    let mut lines = text.lines();
    let name = lines.next().unwrap_or("unknown");
    let svcs: Vec<&str> = lines.clone().filter_map(|l| l.strip_prefix("svc|")).collect();
    let runs: Vec<&str> = lines.filter_map(|l| l.strip_prefix("run|")).collect();
    println!("Active Profile: {name}");
    println!("  Stopped services ({}): {}", svcs.len(), if svcs.is_empty() { "none".into() } else { svcs.join(", ") });
    println!("  Closed apps to relaunch ({}): {}", runs.len(), if runs.is_empty() { "none".into() } else { runs.join(", ") });
    0
}

pub fn run_activate(kind: &str) -> i32 {
    let is_work = match kind.to_ascii_lowercase().as_str() {
        "work" => true,
        "game" => false,
        _ => {
            eprintln!("Unknown profile '{kind}'. Expected: work | game");
            return 1;
        }
    };

    if profile_file().exists() {
        println!("Restoring previously active profile before switching...");
        if run_restore() != 0 {
            return 1;
        }
    }

    let (svcs, procs, power_target) = if is_work {
        (WORK_SVCS, WORK_PROCS, "high")
    } else {
        (GAME_SVCS, GAME_PROCS, "ultimate")
    };

    let mut state_text = format!("{kind}\n");
    let mut stopped_svcs = 0usize;

    if elevated() {
        for s in svcs {
            if is_service_running(s) && run_sc(&["stop", s]).0 {
                state_text.push_str(&format!("svc|{s}\n"));
                stopped_svcs += 1;
            }
        }
    } else {
        println!("Note: running without admin; skipping service stops (will still close user apps and switch power plan).");
    }

    let mut closed_procs = 0usize;
    let mut relaunch_cmds: Vec<String> = vec![];
    for (pid, pname) in list_procs() {
        let low = pname.to_ascii_lowercase();
        let Some((_, again)) = procs.iter().find(|p| p.0 == low) else {
            continue;
        };
        let path = exe_path(pid);
        kill(pid);
        closed_procs += 1;
        if let (Some(args), Some(p)) = (again, path) {
            let cmd = if args.is_empty() {
                format!("\"{p}\"")
            } else {
                format!("\"{p}\" {args}")
            };
            if !relaunch_cmds.contains(&cmd) {
                relaunch_cmds.push(cmd);
            }
        }
    }
    for r in &relaunch_cmds {
        state_text.push_str(&format!("run|{r}\n"));
    }

    let f = profile_file();
    if let Some(parent) = f.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Err(e) = fs::write(&f, state_text) {
        eprintln!("Failed to save profile state: {e}");
        return 1;
    }

    match set_plan_by_target(power_target) {
        Ok(p) => println!("Power plan switched to '{}'.", p.name),
        Err(e) => println!("Power plan note: {e}"),
    }

    if elevated() {
        let _ = run_purge();
    }

    println!(
        "Profile '{kind}' activated: {stopped_svcs} service(s) stopped, {closed_procs} process(es) closed."
    );
    println!("Run `gp-pc-boost profile restore` when done.");
    0
}

pub fn run_restore() -> i32 {
    let f = profile_file();
    let Ok(text) = fs::read_to_string(&f) else {
        println!("No active profile to restore.");
        return 0;
    };
    let mut started = 0usize;
    let mut launched = 0usize;
    for line in text.lines().skip(1) {
        if let Some(svc) = line.strip_prefix("svc|") {
            if elevated() && run_sc(&["start", svc]).0 {
                started += 1;
            }
        } else if let Some(cmd) = line.strip_prefix("run|")
            && let Some(exe) = cmd.strip_prefix('"').and_then(|s| s.split('"').next())
            && program(exe)
        {
            launched += 1;
        }
    }
    match restore_plan() {
        Ok(Some(name)) => println!("Power plan restored to '{name}'."),
        Ok(None) => {}
        Err(e) => eprintln!("Power plan restore warning: {e}"),
    }
    let _ = fs::remove_file(&f);
    println!("Profile restored: {started} service(s) restarted, {launched} app(s) relaunched.");
    0
}
