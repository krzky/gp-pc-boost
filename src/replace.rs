use crate::{
    backup::create_backup,
    disk::fixed_drives,
    journal::{read_entries, record_if_new, state_dir},
    tune::{RegState, query_reg_state, query_svc_mode},
};
use std::{
    env, fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};
use winrust_common::{
    journal::Entry,
    launch::program,
    proc::{elevated, list as list_procs, start_shell_explorer},
    reg::{get_dword, value},
};

pub const PAYLOAD_DIR: &str = r"D:\Project\rust-goose\WinRust\payload";
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const TM_IFEO: &str = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe";
const SH_IFEO: &str = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\SearchHost.exe";
const SB_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Search";
const WINLOGON_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon";
const RX_SWITCH: &str = r"HKCU\Software\rs-shell";
const SPLIT_KEY: &str = r"HKLM\SYSTEM\CurrentControlSet\Control";
const SPLIT_VALUE: &str = "SvcHostSplitThresholdInKB";
const RX_FOLDERS: [&str; 3] = [
    r"HKCU\Software\Classes\Directory\shell\open",
    r"HKCU\Software\Classes\Folder\shell\open",
    r"HKCU\Software\Classes\Drive\shell\open",
];
const PV_EXTS: [&str; 11] = [
    "jpg", "jpeg", "jfif", "png", "bmp", "gif", "tif", "tiff", "webp", "heic", "avif",
];
const PV_PROG: &str = "WinRustPhoto";
const PV_APP: &str = "WinRust Photo Viewer";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Component {
    TaskMgr,
    Settings,
    PhotoView,
    WizTree,
    Debloat,
    Topbar,
    Search,
    Explorer,
}

/// Forward install order (Topbar strictly before Explorer).
pub const ALL_FORWARD: [Component; 8] = [
    Component::TaskMgr,
    Component::Settings,
    Component::PhotoView,
    Component::WizTree,
    Component::Debloat,
    Component::Topbar,
    Component::Search,
    Component::Explorer,
];

/// Reverse revert order (Explorer strictly before Topbar).
pub const ALL_REVERSE: [Component; 8] = [
    Component::Explorer,
    Component::Topbar,
    Component::Search,
    Component::Debloat,
    Component::TaskMgr,
    Component::Settings,
    Component::PhotoView,
    Component::WizTree,
];

pub const SAFE_ALL: [Component; 5] = [
    Component::TaskMgr,
    Component::Settings,
    Component::PhotoView,
    Component::WizTree,
    Component::Debloat,
];

pub const EXPRESS: [Component; 7] = [
    Component::TaskMgr,
    Component::Settings,
    Component::PhotoView,
    Component::WizTree,
    Component::Debloat,
    Component::Topbar,
    Component::Search,
];

impl Component {
    pub fn name(self) -> &'static str {
        match self {
            Self::TaskMgr => "taskmgr",
            Self::Settings => "settings",
            Self::PhotoView => "photoview",
            Self::WizTree => "wiztree",
            Self::Debloat => "debloat",
            Self::Topbar => "topbar",
            Self::Search => "search",
            Self::Explorer => "explorer",
        }
    }

    pub fn replaces(self) -> &'static str {
        match self {
            Self::TaskMgr => "Windows Task Manager (Ctrl+Shift+Esc / taskmgr.exe)",
            Self::Settings => "Windows Settings UI (display/sound/power/network)",
            Self::PhotoView => "Windows Photos ('Open with' WinRust Photo Viewer)",
            Self::WizTree => "Slow folder size checks (fast NTFS MFT treemap)",
            Self::Debloat => "Telemetry services, Copilot/Recall/Edge policies, NTFS flags",
            Self::Topbar => "Windows 11 Taskbar (hides stock taskbar; adds Win+1..9 hotkeys)",
            Self::Search => "Windows Search (WSearch off, SearchHost blocked, Alt+Space)",
            Self::Explorer => "Windows File Explorer shell (disables explorer.exe as shell)",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "taskmgr" | "rtaskmgr" | "rtaskmgr-win32" => Some(Self::TaskMgr),
            "settings" | "rust-settings" => Some(Self::Settings),
            "photoview" | "photo" => Some(Self::PhotoView),
            "wiztree" | "disk" => Some(Self::WizTree),
            "debloat" => Some(Self::Debloat),
            "topbar" | "bar" | "taskbar" => Some(Self::Topbar),
            "search" | "rust-search" => Some(Self::Search),
            "explorer" | "rust-explorer" => Some(Self::Explorer),
            _ => None,
        }
    }

    pub fn is_installed(self) -> bool {
        let pf = program_files();
        match self {
            Self::TaskMgr => {
                pf.join(r"rtaskmgr\rtaskmgr-win32.exe").is_file()
                    && query_reg_str(TM_IFEO, "Debugger")
                        .is_some_and(|d| d.to_ascii_lowercase().contains("rtaskmgr"))
            }
            Self::Settings => pf.join(r"rust-settings\rust-settings.exe").is_file(),
            Self::PhotoView => pf.join(r"photoview\photoview.exe").is_file(),
            Self::WizTree => pf.join(r"wiztree\wiztree.exe").is_file(),
            Self::Debloat => {
                pf.join(r"debloat\debloat.exe").is_file()
                    && program_data().join(r"winrust-debloat\undo.txt").is_file()
            }
            Self::Topbar => user_taskbar_bin().is_file() || pf.join(r"topbar\topbar.exe").is_file(),
            Self::Search => pf.join(r"rs-search\rs.exe").is_file(),
            Self::Explorer => pf.join(r"rust-explorer\rust-explorer.exe").is_file(),
        }
    }
}

fn replace_journal_path() -> PathBuf {
    state_dir().join("replace_journal.log")
}

fn program_files() -> PathBuf {
    PathBuf::from(env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into()))
}

fn program_data() -> PathBuf {
    PathBuf::from(env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into()))
}

fn start_lnk(name: &str) -> PathBuf {
    program_data().join(format!(r"Microsoft\Windows\Start Menu\Programs\{name}.lnk"))
}

fn run_cmd(prog: &str, args: &[&str]) -> Result<String, String> {
    let o = Command::new(prog)
        .args(args)
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| format!("{prog}: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        Err(format!(
            "{prog} {}: {}{}",
            args.join(" "),
            String::from_utf8_lossy(&o.stdout).trim(),
            String::from_utf8_lossy(&o.stderr).trim()
        ))
    }
}

fn query_reg_str(key: &str, name: &str) -> Option<String> {
    let out = run_cmd("reg.exe", &["query", key, "/v", name]).ok()?;
    value(&out).map(|(_, d)| d.to_string())
}

fn copy_payload(src_name: &str, dst: &Path) -> Result<(), String> {
    let src = Path::new(PAYLOAD_DIR).join(src_name);
    if !src.is_file() {
        return Err(format!("missing WinRust payload binary: {}", src.display()));
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    let mut r = fs::copy(&src, dst).map(|_| ());
    for _ in 0..25 {
        if !matches!(&r, Err(e) if e.raw_os_error() == Some(32) || e.raw_os_error() == Some(5)) {
            break;
        }
        thread::sleep(Duration::from_millis(200));
        r = fs::copy(&src, dst).map(|_| ());
    }
    r.map_err(|e| format!("copy {} -> {}: {e}", src.display(), dst.display()))
}

fn create_shortcut(lnk: &Path, target: &Path) {
    let script = format!(
        "$s=(New-Object -ComObject WScript.Shell).CreateShortcut('{}'); $s.TargetPath='{}'; $s.Save()",
        lnk.display(),
        target.display()
    );
    let _ = run_cmd("powershell.exe", &["-NoProfile", "-Command", &script]);
}

fn record_reg_baseline(key: &str, name: &str) -> Result<(), String> {
    let jp = replace_journal_path();
    let old = match query_reg_str(key, name) {
        None => None,
        Some(_) => {
            let out = run_cmd("reg.exe", &["query", key, "/v", name]).ok();
            let parsed = out.as_deref().and_then(value);
            match parsed {
                Some(("REG_DWORD", _)) => get_dword(key, name).map(|d| ("REG_DWORD".into(), d.to_string())),
                Some((ty @ ("REG_SZ" | "REG_EXPAND_SZ"), data)) => Some((ty.into(), data.into())),
                _ => None,
            }
        }
    };
    record_if_new(
        &jp,
        &Entry::Reg {
            key: key.into(),
            name: name.into(),
            old,
        },
    )?;
    Ok(())
}

fn record_svc_baseline(svc: &str) -> Result<(), String> {
    if let Some(mode) = query_svc_mode(svc) {
        record_if_new(
            &replace_journal_path(),
            &Entry::Svc {
                name: svc.into(),
                mode: mode.into(),
            },
        )?;
    }
    Ok(())
}

fn restore_journal_entry(key: &str, name: &str) -> Result<bool, String> {
    let entries = read_entries(&replace_journal_path())?;
    for e in &entries {
        if let Entry::Reg {
            key: ek,
            name: en,
            old,
        } = e
            && ek.eq_ignore_ascii_case(key)
            && en.eq_ignore_ascii_case(name)
        {
            match old {
                None => {
                    let _ = run_cmd("reg.exe", &["delete", key, "/v", name, "/f"]);
                }
                Some((ty, data)) => {
                    run_cmd("reg.exe", &["add", key, "/v", name, "/t", ty, "/d", data, "/f"])?;
                }
            }
            return Ok(true);
        }
    }
    Ok(false)
}

fn restore_svc_from_journal(svc: &str, fallback_mode: &str) {
    let entries = read_entries(&replace_journal_path()).unwrap_or_default();
    let mode = entries
        .iter()
        .find_map(|e| match e {
            Entry::Svc { name, mode } if name.eq_ignore_ascii_case(svc) => Some(mode.as_str()),
            _ => None,
        })
        .unwrap_or(fallback_mode);
    let _ = run_cmd("sc.exe", &["config", svc, "start=", mode]);
    if mode.ends_with("auto") {
        let _ = run_cmd("sc.exe", &["start", svc]);
    }
}

fn install_component(c: Component) -> Result<(), String> {
    let pf = program_files();
    match c {
        Component::TaskMgr => {
            let dir = pf.join("rtaskmgr");
            let dst = dir.join("rtaskmgr-win32.exe");
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "rtaskmgr-win32.exe"]);
            copy_payload("Task manager.exe", &dst)?;
            copy_payload("rs-du.exe", &dir.join("rs-du.exe"))?;
            copy_payload("debloat.exe", &dir.join("debloat.exe"))?;
            create_shortcut(&start_lnk("Task manager"), &dst);
            record_reg_baseline(TM_IFEO, "Debugger")?;
            run_cmd(
                "reg.exe",
                &[
                    "add",
                    TM_IFEO,
                    "/v",
                    "Debugger",
                    "/t",
                    "REG_SZ",
                    "/d",
                    &format!("\"{}\"", dst.display()),
                    "/f",
                ],
            )?;
            Ok(())
        }
        Component::Settings => {
            let dir = pf.join("rust-settings");
            let dst = dir.join("rust-settings.exe");
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "rust-settings.exe"]);
            copy_payload("rust-settings.exe", &dst)?;
            create_shortcut(&start_lnk("WinRust Settings"), &dst);
            Ok(())
        }
        Component::PhotoView => {
            let dir = pf.join("photoview");
            let dst = dir.join("photoview.exe");
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "photoview.exe"]);
            copy_payload("photoview.exe", &dst)?;
            create_shortcut(&start_lnk("Photo viewer"), &dst);
            let exe = dst.display().to_string().replace('\\', r"\\");
            let mut reg = format!(
                "Windows Registry Editor Version 5.00\r\n\r\n\
                 [HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\{PV_PROG}]\r\n@=\"Picture\"\r\n\r\n\
                 [HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\{PV_PROG}\\shell\\open\\command]\r\n\
                 @=\"\\\"{exe}\\\" \\\"%1\\\"\"\r\n\r\n\
                 [HKEY_LOCAL_MACHINE\\SOFTWARE\\WinRust\\PhotoView\\Capabilities]\r\n\
                 \"ApplicationName\"=\"{PV_APP}\"\r\n\
                 \"ApplicationDescription\"=\"Fast picture viewer\"\r\n\r\n\
                 [HKEY_LOCAL_MACHINE\\SOFTWARE\\WinRust\\PhotoView\\Capabilities\\FileAssociations]\r\n"
            );
            for e in PV_EXTS {
                reg += &format!("\".{e}\"=\"{PV_PROG}\"\r\n");
            }
            reg += &format!(
                "\r\n[HKEY_LOCAL_MACHINE\\SOFTWARE\\RegisteredApplications]\r\n\
                 \"{PV_APP}\"=\"SOFTWARE\\\\WinRust\\\\PhotoView\\\\Capabilities\"\r\n"
            );
            for e in PV_EXTS {
                reg += &format!(
                    "\r\n[HKEY_LOCAL_MACHINE\\SOFTWARE\\Classes\\.{e}\\OpenWithProgids]\r\n\
                     \"{PV_PROG}\"=hex(0):\r\n"
                );
            }
            let reg_file = dir.join("register.reg");
            fs::write(&reg_file, reg).map_err(|e| e.to_string())?;
            let r = run_cmd("reg.exe", &["import", &reg_file.to_string_lossy()]);
            let _ = fs::remove_file(&reg_file);
            r.map(|_| ())
        }
        Component::WizTree => {
            let dir = pf.join("wiztree");
            let dst = dir.join("wiztree.exe");
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "wiztree.exe"]);
            copy_payload("wiztree.exe", &dst)?;
            copy_payload("rs-du.exe", &dir.join("rs-du.exe"))?;
            create_shortcut(&start_lnk("WinRust Disk"), &dst);
            Ok(())
        }
        Component::Debloat => {
            let dir = pf.join("debloat");
            let dst = dir.join("debloat.exe");
            copy_payload("debloat.exe", &dst)?;
            let exe = dst.to_string_lossy().into_owned();
            run_cmd(&exe, &["--apply"])?;
            run_cmd(&exe, &["--apply", "ntfs"])?;
            Ok(())
        }
        Component::Topbar => {
            let dir = pf.join("topbar");
            let bar = dir.join("topbar.exe");
            let rsh = dir.join("reshell.exe");
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "reshell.exe"]);
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "topbar.exe"]);
            let custom_bar = Path::new(r"D:\Project\gp-pc-boost\target\release\gp-taskbar.exe");
            if custom_bar.is_file() {
                return install_gp_taskbar(custom_bar, &dir, &bar).map(|_| ());
            }
            copy_payload("topbar.exe", &bar)?;
            copy_payload("reshell.exe", &rsh)?;
            let _ = run_cmd(&bar.to_string_lossy(), &["--restore"]);
            run_cmd(
                "reg.exe",
                &["add", RUN_KEY, "/v", "reshell", "/t", "REG_SZ", "/d", &format!("\"{}\"", rsh.display()), "/f"],
            )?;
            run_cmd(
                "reg.exe",
                &["add", RUN_KEY, "/v", "topbar", "/t", "REG_SZ", "/d", &format!("\"{}\"", bar.display()), "/f"],
            )?;
            let _ = program(&rsh.to_string_lossy());
            let _ = program(&bar.to_string_lossy());
            Ok(())
        }
        Component::Search => {
            let dir = pf.join("rs-search");
            let rs_dst = dir.join("rs.exe");
            let rsl_dst = dir.join("rs-launcher.exe");
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "rs-launcher.exe"]);
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "rs.exe"]);
            copy_payload("rs.exe", &rs_dst)?;
            copy_payload("rs-launcher.exe", &rsl_dst)?;
            run_cmd(&rs_dst.to_string_lossy(), &["--install"])?;
            run_cmd(
                "reg.exe",
                &["add", RUN_KEY, "/v", "rs-launcher", "/t", "REG_SZ", "/d", &format!("\"{}\"", rsl_dst.display()), "/f"],
            )?;
            let _ = program(&rsl_dst.to_string_lossy());

            if query_svc_mode("WSearch").is_some() {
                record_svc_baseline("WSearch")?;
                run_cmd("sc.exe", &["config", "WSearch", "start=", "disabled"])?;
                let _ = run_cmd("sc.exe", &["stop", "WSearch"]);
            }
            record_reg_baseline(SH_IFEO, "Debugger")?;
            run_cmd(
                "reg.exe",
                &["add", SH_IFEO, "/v", "Debugger", "/t", "REG_SZ", "/d", "cmd.exe /c exit", "/f"],
            )?;
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "SearchHost.exe"]);
            record_reg_baseline(SB_KEY, "SearchboxTaskbarMode")?;
            run_cmd(
                "reg.exe",
                &["add", SB_KEY, "/v", "SearchboxTaskbarMode", "/t", "REG_DWORD", "/d", "0", "/f"],
            )?;
            Ok(())
        }
        Component::Explorer => {
            if !Component::Topbar.is_installed() {
                return Err("`explorer` replacement requires `topbar` to be installed first".into());
            }
            let dir = pf.join("rust-explorer");
            let fm = dir.join("rust-explorer.exe");
            let sh = dir.join("rs-shell.exe");
            for n in ["rs-shell.exe", "rust-explorer.exe"] {
                let _ = run_cmd("taskkill.exe", &["/f", "/im", n]);
            }
            copy_payload("rust-explorer.exe", &fm)?;
            copy_payload("rs-shell.exe", &sh)?;

            record_reg_baseline(WINLOGON_KEY, "AutoRestartShell")?;
            record_reg_baseline(SPLIT_KEY, SPLIT_VALUE)?;
            for svc in ["InventorySvc", "lfsvc", "DoSvc"] {
                record_svc_baseline(svc)?;
            }

            for (i, k) in RX_FOLDERS.iter().enumerate() {
                let bak = dir.join(format!("folders-{i}.reg"));
                let ours = query_reg_str(&format!(r"{k}\command"), "")
                    .is_some_and(|c| c.to_ascii_lowercase().contains("rust-explorer"));
                if !bak.is_file() && !ours && run_cmd("reg.exe", &["query", k]).is_ok() {
                    let _ = run_cmd("reg.exe", &["export", k, &bak.to_string_lossy(), "/y"]);
                }
            }
            for k in RX_FOLDERS {
                let cmd_key = format!(r"{k}\command");
                run_cmd(
                    "reg.exe",
                    &["add", &cmd_key, "/ve", "/t", "REG_SZ", "/d", &format!("\"{}\" \"%1\"", fm.display()), "/f"],
                )?;
                run_cmd(
                    "reg.exe",
                    &["add", &cmd_key, "/v", "DelegateExecute", "/t", "REG_SZ", "/d", "", "/f"],
                )?;
            }
            run_cmd(
                "reg.exe",
                &["add", RUN_KEY, "/v", "rs-overlay", "/t", "REG_SZ", "/d", &format!("\"{}\" --overlay", sh.display()), "/f"],
            )?;
            run_cmd(
                "reg.exe",
                &["add", RX_SWITCH, "/v", "ReplaceExplorer", "/t", "REG_DWORD", "/d", "1", "/f"],
            )?;
            run_cmd(
                "reg.exe",
                &["add", WINLOGON_KEY, "/v", "AutoRestartShell", "/t", "REG_DWORD", "/d", "0", "/f"],
            )?;
            let _ = run_cmd("sc.exe", &["config", "InventorySvc", "start=", "disabled"]);
            let _ = run_cmd("sc.exe", &["stop", "InventorySvc"]);
            let _ = run_cmd("sc.exe", &["config", "lfsvc", "start=", "disabled"]);
            let _ = run_cmd("sc.exe", &["stop", "lfsvc"]);
            let _ = run_cmd("sc.exe", &["config", "DoSvc", "start=", "demand"]);
            let _ = run_cmd(
                "reg.exe",
                &["add", SPLIT_KEY, "/v", SPLIT_VALUE, "/t", "REG_DWORD", "/d", &0x4000000u32.to_string(), "/f"],
            );
            create_shortcut(&start_lnk("Rust Explorer"), &fm);
            let _ = Command::new(&sh).arg("--overlay").spawn();
            Ok(())
        }
    }
}

fn revert_component(c: Component) -> Result<(), String> {
    let pf = program_files();
    match c {
        Component::TaskMgr => {
            let dir = pf.join("rtaskmgr");
            let exe = dir.join("rtaskmgr-win32.exe");
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "rtaskmgr-win32.exe"]);
            if exe.is_file() {
                let _ = run_cmd(&exe.to_string_lossy(), &["--undo-speed"]);
            }
            if !restore_journal_entry(TM_IFEO, "Debugger")?
                && query_reg_str(TM_IFEO, "Debugger")
                    .is_some_and(|d| d.to_ascii_lowercase().contains("rtaskmgr"))
            {
                let _ = run_cmd("reg.exe", &["delete", TM_IFEO, "/v", "Debugger", "/f"]);
            }
            let _ = fs::remove_file(start_lnk("Task manager"));
            let _ = fs::remove_dir_all(&dir);
            Ok(())
        }
        Component::Settings => {
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "rust-settings.exe"]);
            let _ = run_cmd("reg.exe", &["delete", RUN_KEY, "/v", "WinRustNightLight", "/f"]);
            let _ = run_cmd("reg.exe", &["delete", r"HKCU\Software\WinRust\NightLight", "/f"]);
            let _ = fs::remove_file(start_lnk("WinRust Settings"));
            let _ = fs::remove_dir_all(pf.join("rust-settings"));
            Ok(())
        }
        Component::PhotoView => {
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "photoview.exe"]);
            for e in PV_EXTS {
                let _ = run_cmd(
                    "reg.exe",
                    &["delete", &format!(r"HKLM\SOFTWARE\Classes\.{e}\OpenWithProgids"), "/v", PV_PROG, "/f"],
                );
            }
            let _ = run_cmd("reg.exe", &["delete", r"HKLM\SOFTWARE\RegisteredApplications", "/v", PV_APP, "/f"]);
            let _ = run_cmd("reg.exe", &["delete", &format!(r"HKLM\SOFTWARE\Classes\{PV_PROG}"), "/f"]);
            let _ = run_cmd("reg.exe", &["delete", r"HKLM\SOFTWARE\WinRust\PhotoView", "/f"]);
            let _ = fs::remove_file(start_lnk("Photo viewer"));
            let _ = fs::remove_dir_all(pf.join("photoview"));
            Ok(())
        }
        Component::WizTree => {
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "wiztree.exe"]);
            let _ = fs::remove_file(start_lnk("WinRust Disk"));
            let _ = fs::remove_dir_all(pf.join("wiztree"));
            Ok(())
        }
        Component::Debloat => {
            let dir = pf.join("debloat");
            let exe = dir.join("debloat.exe");
            if exe.is_file() {
                let s = exe.to_string_lossy().into_owned();
                let _ = run_cmd(&s, &["--undo", "ntfs"]);
                let _ = run_cmd(&s, &["--undo"]);
            }
            let _ = fs::remove_dir_all(&dir);
            Ok(())
        }
        Component::Topbar => {
            let dir = pf.join("topbar");
            let bar = dir.join("topbar.exe");
            let ubar = user_taskbar_bin();
            let _ = run_cmd("reg.exe", &["delete", RUN_KEY, "/v", "topbar", "/f"]);
            let _ = run_cmd("reg.exe", &["delete", RUN_KEY, "/v", "reshell", "/f"]);
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "reshell.exe"]);
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "topbar.exe"]);
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "gp-taskbar.exe"]);
            if ubar.is_file() {
                let _ = run_cmd(&ubar.to_string_lossy(), &["--restore"]);
                let _ = fs::remove_file(&ubar);
            } else if bar.is_file() {
                let _ = run_cmd(&bar.to_string_lossy(), &["--restore"]);
            }
            let _ = fs::remove_dir_all(&dir);
            Ok(())
        }
        Component::Search => {
            if !restore_journal_entry(SH_IFEO, "Debugger")? {
                let _ = run_cmd("reg.exe", &["delete", SH_IFEO, "/v", "Debugger", "/f"]);
            }
            if !restore_journal_entry(SB_KEY, "SearchboxTaskbarMode")? {
                let _ = run_cmd("reg.exe", &["add", SB_KEY, "/v", "SearchboxTaskbarMode", "/t", "REG_DWORD", "/d", "2", "/f"]);
            }
            restore_svc_from_journal("WSearch", "delayed-auto");
            let _ = run_cmd("reg.exe", &["delete", RUN_KEY, "/v", "rs-launcher", "/f"]);
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "rs-launcher.exe"]);
            let dir = pf.join("rs-search");
            let rs = dir.join("rs.exe");
            if rs.is_file() {
                let _ = run_cmd(&rs.to_string_lossy(), &["--uninstall"]);
            }
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "rs.exe"]);
            let _ = fs::remove_dir_all(&dir);
            Ok(())
        }
        Component::Explorer => {
            let dir = pf.join("rust-explorer");
            if !restore_journal_entry(WINLOGON_KEY, "AutoRestartShell")? {
                let _ = run_cmd("reg.exe", &["add", WINLOGON_KEY, "/v", "AutoRestartShell", "/t", "REG_DWORD", "/d", "1", "/f"]);
            }
            let _ = restore_journal_entry(SPLIT_KEY, SPLIT_VALUE);
            restore_svc_from_journal("InventorySvc", "demand");
            restore_svc_from_journal("lfsvc", "demand");
            restore_svc_from_journal("DoSvc", "delayed-auto");
            let _ = run_cmd("reg.exe", &["delete", RUN_KEY, "/v", "rs-overlay", "/f"]);
            let _ = run_cmd("reg.exe", &["delete", RX_SWITCH, "/f"]);
            for (i, k) in RX_FOLDERS.iter().enumerate() {
                let _ = run_cmd("reg.exe", &["delete", k, "/f"]);
                let bak = dir.join(format!("folders-{i}.reg"));
                if bak.is_file() {
                    let _ = run_cmd("reg.exe", &["import", &bak.to_string_lossy()]);
                }
            }
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "rs-shell.exe"]);
            let _ = run_cmd("taskkill.exe", &["/f", "/im", "rust-explorer.exe"]);
            let has_shell = !winrust_common::proc::shell_explorers().is_empty();
            if !has_shell {
                let _ = start_shell_explorer();
            }
            let _ = fs::remove_file(start_lnk("Rust Explorer"));
            let _ = fs::remove_dir_all(&dir);
            Ok(())
        }
    }
}

pub fn expand_targets(args: &[String]) -> Result<(Vec<Component>, bool), String> {
    let mut allow_shell = false;
    let mut out: Vec<Component> = vec![];
    for a in args {
        if a == "--allow-shell-replace" {
            allow_shell = true;
            continue;
        }
        match a.trim().to_ascii_lowercase().as_str() {
            "safe-all" => {
                for c in SAFE_ALL {
                    if !out.contains(&c) {
                        out.push(c);
                    }
                }
            }
            "express" => {
                for c in EXPRESS {
                    if !out.contains(&c) {
                        out.push(c);
                    }
                }
            }
            "full" | "all" => {
                for c in ALL_FORWARD {
                    if !out.contains(&c) {
                        out.push(c);
                    }
                }
            }
            other => {
                let c = Component::parse(other)
                    .ok_or_else(|| format!("unknown component or preset '{other}'"))?;
                if !out.contains(&c) {
                    out.push(c);
                }
            }
        }
    }
    if out.is_empty() {
        return Err("specify a preset (safe-all | express | full) or component name(s)".into());
    }
    // Enforce forward dependency order (Topbar before Explorer)
    let ordered: Vec<Component> = ALL_FORWARD.into_iter().filter(|c| out.contains(c)).collect();
    Ok((ordered, allow_shell))
}

pub fn revert_order(selected: &[Component]) -> Vec<Component> {
    ALL_REVERSE
        .into_iter()
        .filter(|c| selected.contains(c))
        .collect()
}

pub fn run_status() -> i32 {
    println!("WinRust Stock-Component Replacement Status:");
    for c in ALL_FORWARD {
        let state = if c.is_installed() { "REPLACED" } else { "STOCK   " };
        println!("  [{state}] {:<10} — {}", c.name(), c.replaces());
    }
    let payload_ok = Path::new(PAYLOAD_DIR).is_dir();
    println!(
        "\nPayload directory: {} ({})",
        PAYLOAD_DIR,
        if payload_ok { "ready" } else { "MISSING" }
    );
    0
}

pub fn run_preview(args: &[String]) -> i32 {
    let target = if args.is_empty() {
        vec!["full".to_string()]
    } else {
        args.to_vec()
    };
    let (list, _) = match expand_targets(&target) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: {e}");
            return 2;
        }
    };
    println!("Replacement Preview (read-only, nothing changed):");
    for c in &list {
        let installed = if c.is_installed() { "already installed" } else { "stock -> WinRust" };
        let caveat = match c {
            Component::Explorer => " [CAUTION: replaces explorer.exe shell; use --allow-shell-replace; lacks Win+V & MTP]",
            Component::Search => " [NOTE: local NTFS only; skips 8 SMB drives N:-Z:]",
            _ => "",
        };
        println!("  {:<10} {:<18} {}{}", c.name(), installed, c.replaces(), caveat);
    }
    0
}

pub fn apply_components(list: &[Component], allow_shell: bool) -> Result<String, String> {
    let ordered: Vec<Component> = ALL_FORWARD
        .into_iter()
        .filter(|c| list.contains(c))
        .collect();
    if ordered.is_empty() {
        return Err("No components selected.".into());
    }
    if ordered.contains(&Component::Explorer) && !allow_shell {
        return Err("Replacing `explorer` requires explicit confirmation (`--allow-shell-replace`).".into());
    }
    if !elevated() {
        return Err("Administrator privileges are required to install WinRust replacements.".into());
    }
    let bak = create_backup()?;
    let mut ok_count = 0usize;
    let mut errs = vec![];
    for c in ordered {
        match install_component(c) {
            Ok(()) => ok_count += 1,
            Err(e) => errs.push(format!("{}: {e}", c.name())),
        }
    }
    if errs.is_empty() {
        Ok(format!(
            "Applied {ok_count} WinRust component(s). Backup saved at {}",
            bak.display()
        ))
    } else {
        Err(format!("Applied {ok_count}, failed: {}", errs.join("; ")))
    }
}

pub fn revert_components(list: &[Component]) -> Result<String, String> {
    if !elevated() {
        return Err("Administrator privileges are required to revert WinRust replacements.".into());
    }
    let ordered = revert_order(list);
    if ordered.is_empty() {
        return Err("No components selected.".into());
    }
    let mut ok_count = 0usize;
    let mut errs = vec![];
    for c in ordered {
        match revert_component(c) {
            Ok(()) => ok_count += 1,
            Err(e) => errs.push(format!("{}: {e}", c.name())),
        }
    }
    if errs.is_empty() {
        Ok(format!("Reverted {ok_count} component(s) to stock Windows."))
    } else {
        Err(format!("Reverted {ok_count}, failed: {}", errs.join("; ")))
    }
}

pub fn run_apply(args: &[String]) -> i32 {
    let (list, allow_shell) = match expand_targets(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Error: {e}");
            return 2;
        }
    };
    if list.contains(&Component::Explorer) && !allow_shell {
        eprintln!("Refusing to replace Windows File Explorer shell (`explorer`) without `--allow-shell-replace`.");
        eprintln!("This PC has 8 mapped SMB drives (N:-Z:) and active WebView2 apps; read REPORT.md first.");
        eprintln!("Use `replace apply express` to keep explorer.exe as shell, or add `--allow-shell-replace`.");
        return 2;
    }
    match apply_components(&list, allow_shell) {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

pub fn run_revert(args: &[String]) -> i32 {
    if !elevated() {
        eprintln!("Administrator privileges are required to revert WinRust replacements.");
        return 1;
    }
    let list = if args.is_empty() || args.iter().any(|a| a == "all") {
        ALL_REVERSE.to_vec()
    } else {
        let mut sel = vec![];
        for a in args {
            match Component::parse(a) {
                Some(c) if !sel.contains(&c) => sel.push(c),
                Some(_) => {}
                None => {
                    eprintln!("Unknown component '{a}'.");
                    return 2;
                }
            }
        }
        revert_order(&sel)
    };

    let code = match revert_components(&list) {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    };

    let shell_ok = !winrust_common::proc::shell_explorers().is_empty();
    let drives: Vec<String> = fixed_drives().into_iter().map(|d| format!("{}:", d.letter)).collect();
    let running_procs = list_procs().len();
    println!(
        "\nPost-revert check: explorer shell active={shell_ok}, fixed drives={}, total processes={running_procs}.",
        drives.join(",")
    );
    let _ = matches!(query_reg_state(WINLOGON_KEY, "AutoRestartShell"), RegState::Dword(1));
    code
}

pub fn user_taskbar_bin() -> PathBuf {
    let base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| state_dir());
    base.join("gp-pc-boost").join("bin").join("gp-taskbar.exe")
}

fn install_gp_taskbar(src: &Path, pf_dir: &Path, pf_dst: &Path) -> Result<String, String> {
    if !src.is_file() {
        return Err(format!("Built taskbar binary not found: {}", src.display()));
    }
    let _ = run_cmd("reg.exe", &["delete", RUN_KEY, "/v", "reshell", "/f"]);
    let user_dst = user_taskbar_bin();
    if user_dst.is_file() {
        let _ = run_cmd(&user_dst.to_string_lossy(), &["--restore"]);
    } else if pf_dst.is_file() {
        let _ = run_cmd(&pf_dst.to_string_lossy(), &["--restore"]);
    }
    let _ = run_cmd("taskkill.exe", &["/f", "/im", "reshell.exe"]);
    let _ = run_cmd("taskkill.exe", &["/f", "/im", "topbar.exe"]);
    let _ = run_cmd("taskkill.exe", &["/f", "/im", "gp-taskbar.exe"]);
    thread::sleep(Duration::from_millis(250));

    if let Some(udir) = user_dst.parent() {
        fs::create_dir_all(udir).map_err(|e| format!("create dir {}: {e}", udir.display()))?;
    }
    fs::copy(src, &user_dst).map_err(|e| format!("copy {} -> {}: {e}", src.display(), user_dst.display()))?;

    // Also sync C:\Program Files\topbar\topbar.exe whenever elevated so legacy paths match
    if elevated() && fs::create_dir_all(pf_dir).is_ok() {
        let bak = pf_dir.join("topbar.exe.winrust.bak");
        if pf_dst.is_file() && !bak.is_file() {
            let _ = fs::copy(pf_dst, &bak);
        }
        let _ = fs::copy(src, pf_dst);
    }

    run_cmd(
        "reg.exe",
        &["add", RUN_KEY, "/v", "topbar", "/t", "REG_SZ", "/d", &format!("\"{}\"", user_dst.display()), "/f"],
    )?;
    let _ = program(&user_dst.to_string_lossy());
    Ok(format!("Hot-updated & started taskbar -> {}", user_dst.display()))
}

/// Hot-swaps the running taskbar binary cleanly in user space (no UAC or revert-and-apply needed).
pub fn hot_update_taskbar() -> Result<String, String> {
    let pf = program_files();
    let dir = pf.join("topbar");
    let dst = dir.join("topbar.exe");
    let custom_bar = Path::new(r"D:\Project\gp-pc-boost\target\release\gp-taskbar.exe");
    install_gp_taskbar(custom_bar, &dir, &dst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_preset_requires_explicit_shell_flag() {
        let (list, allow) = expand_targets(&["full".into()]).unwrap();
        assert!(list.contains(&Component::Explorer));
        assert!(!allow);
        let (safe, _) = expand_targets(&["safe-all".into()]).unwrap();
        assert!(!safe.contains(&Component::Explorer));
        assert!(!safe.contains(&Component::Topbar));
    }

    #[test]
    fn revert_order_puts_explorer_before_topbar() {
        let rev = revert_order(&ALL_FORWARD);
        let exp_pos = rev.iter().position(|c| *c == Component::Explorer).unwrap();
        let bar_pos = rev.iter().position(|c| *c == Component::Topbar).unwrap();
        assert!(exp_pos < bar_pos);
    }
}
