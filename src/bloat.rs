use crate::backup::create_backup;
use crate::journal::state_dir;
use std::{
    env, fs,
    os::windows::process::CommandExt,
    path::PathBuf,
    process::Command,
};
use winrust_common::proc::{elevated, list as list_procs};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppxTier {
    PureJunk,
    WorkApp,
    Protected,
    Other,
}

#[derive(Clone, Debug)]
pub struct AppxEntry {
    pub name: String,
    pub full_name: String,
    pub provisioned: bool,
    pub tier: AppxTier,
}

const PURE_JUNK: &[&str] = &[
    "Clipchamp.Clipchamp",
    "Microsoft.BingNews",
    "Microsoft.BingWeather",
    "Microsoft.BingSearch",
    "Microsoft.Copilot",
    "Microsoft.Windows.Ai.Copilot.Provider",
    "Microsoft.549981C3F5F10", // Cortana
    "Microsoft.GamingApp",
    "Microsoft.XboxGameOverlay",
    "Microsoft.XboxGamingOverlay",
    "Microsoft.XboxSpeechToTextOverlay",
    "Microsoft.Xbox.TCUI",
    "Microsoft.GetHelp",
    "Microsoft.Getstarted",
    "Microsoft.MicrosoftOfficeHub",
    "Microsoft.MicrosoftSolitaireCollection",
    "Microsoft.People",
    "Microsoft.WindowsAlarms",
    "Microsoft.WindowsFeedbackHub",
    "Microsoft.WindowsMaps",
    "Microsoft.WindowsSoundRecorder",
    "Microsoft.ZuneMusic",
    "Microsoft.ZuneVideo",
    "MicrosoftCorporationII.MicrosoftFamily",
    "MicrosoftCorporationII.QuickAssist",
    "Microsoft.Windows.DevHome",
    "MicrosoftWindows.Client.WebExperience",
    "Microsoft.SkypeApp",
    "Microsoft.MixedReality.Portal",
    "Microsoft.Microsoft3DViewer",
    "Microsoft.Print3D",
    "Microsoft.Wallet",
    "SpotifyAB.SpotifyMusic",
    "Disney.",
    "Netflix",
    "TikTok",
    "Facebook",
    "Instagram",
    "LinkedIn",
    "Twitter",
    "AmazonVideo",
    "king.com.",
    "Duolingo",
];

const WORK_APPS: &[&str] = &[
    "MSTeams",
    "MicrosoftTeams",
    "Microsoft.OutlookForWindows",
    "Microsoft.Office.OneNote",
    "Microsoft.YourPhone",
    "Microsoft.Todos",
    "Microsoft.PowerAutomateDesktop",
    "microsoft.windowscommunicationsapps",
];

const PROTECTED: &[&str] = &[
    "Microsoft.WindowsStore",
    "Microsoft.StorePurchaseApp",
    "Microsoft.DesktopAppInstaller",
    "windows.immersivecontrolpanel",
    "Microsoft.Windows.ShellExperienceHost",
    "Microsoft.Windows.StartMenuExperienceHost",
    "MicrosoftWindows.Client.",
    "Microsoft.Windows.",
    "Microsoft.NET",
    "Microsoft.VCLibs",
    "Microsoft.UI.Xaml",
    "Microsoft.WindowsAppRuntime",
    "Microsoft.SecHealthUI",
    "Microsoft.AAD.BrokerPlugin",
    "Microsoft.AccountsControl",
    "Microsoft.LockApp",
    "Microsoft.XboxIdentityProvider",
    "Microsoft.GamingServices",
    "Microsoft.WindowsTerminal",
    "Microsoft.WindowsCalculator",
    "Microsoft.Paint",
    "Microsoft.ScreenSketch",
    "Microsoft.Windows.Photos",
    "Microsoft.WindowsNotepad",
];

pub const TELEMETRY_TASKS: &[&str] = &[
    r"\Microsoft\Windows\Application Experience\Microsoft Compatibility Appraiser",
    r"\Microsoft\Windows\Application Experience\ProgramDataUpdater",
    r"\Microsoft\Windows\Application Experience\StartupAppTask",
    r"\Microsoft\Windows\Customer Experience Improvement Program\Consolidator",
    r"\Microsoft\Windows\Customer Experience Improvement Program\UsbCeip",
    r"\Microsoft\Windows\Customer Experience Improvement Program\KernelCeipTask",
    r"\Microsoft\Windows\Feedback\Siuf\DmClient",
    r"\Microsoft\Windows\Feedback\Siuf\DmClientOnScenarioDownload",
    r"\Microsoft\Windows\Windows Error Reporting\QueueReporting",
];

fn starts(name: &str, list: &[&str]) -> bool {
    let n = name.to_ascii_lowercase();
    list.iter().any(|p| n.starts_with(&p.to_ascii_lowercase()))
}

pub fn classify_appx(name: &str) -> AppxTier {
    let keep_protected = starts(name, PROTECTED)
        && !starts(
            name,
            &[
                "Microsoft.Windows.Ai.Copilot.Provider",
                "Microsoft.Windows.DevHome",
                "MicrosoftWindows.Client.WebExperience",
            ],
        );
    if keep_protected {
        return AppxTier::Protected;
    }
    let n = name.to_ascii_lowercase();
    if WORK_APPS
        .iter()
        .any(|p| n.starts_with(&p.to_ascii_lowercase()))
    {
        return AppxTier::WorkApp;
    }
    if PURE_JUNK.iter().any(|p| {
        let pl = p.to_ascii_lowercase();
        n.starts_with(&pl) || (pl.len() >= 5 && !pl.contains('.') && n.contains(&pl))
    }) {
        return AppxTier::PureJunk;
    }
    AppxTier::Other
}

fn run_cmd(prog: &str, args: &[&str]) -> (bool, String) {
    match Command::new(prog).args(args).creation_flags(0x0800_0000).output() {
        Ok(o) => (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned()),
        Err(e) => (false, e.to_string()),
    }
}

pub fn scan_appx() -> Vec<AppxEntry> {
    let script = if elevated() {
        r#"Get-AppxPackage -AllUsers | ForEach-Object { "appx|$($_.PackageFullName)|$($_.Name)|$($_.NonRemovable)" }; Get-AppxProvisionedPackage -Online | ForEach-Object { "prov|$($_.PackageName)|$($_.DisplayName)" }"#
    } else {
        r#"Get-AppxPackage | ForEach-Object { "appx|$($_.PackageFullName)|$($_.Name)|$($_.NonRemovable)" }"#
    };
    let (_, out) = run_cmd("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", script]);
    let mut list = vec![];
    for line in out.lines().map(str::trim) {
        let f: Vec<&str> = line.split('|').collect();
        if f.iter().any(|x| x.contains('\'')) {
            continue;
        }
        match f.as_slice() {
            ["appx", full, name, nr] => {
                let tier = classify_appx(name);
                if matches!(tier, AppxTier::PureJunk | AppxTier::WorkApp) && *nr != "True" {
                    list.push(AppxEntry {
                        name: (*name).into(),
                        full_name: (*full).into(),
                        provisioned: false,
                        tier,
                    });
                }
            }
            ["prov", pkg, name] => {
                let tier = classify_appx(name);
                if matches!(tier, AppxTier::PureJunk | AppxTier::WorkApp) {
                    list.push(AppxEntry {
                        name: (*name).into(),
                        full_name: (*pkg).into(),
                        provisioned: true,
                        tier,
                    });
                }
            }
            _ => {}
        }
    }
    list
}

pub fn existing_telemetry_tasks() -> Vec<&'static str> {
    TELEMETRY_TASKS
        .iter()
        .copied()
        .filter(|t| run_cmd("schtasks.exe", &["/Query", "/TN", t]).0)
        .collect()
}

pub fn onedrive_info() -> (Option<PathBuf>, usize) {
    let root = PathBuf::from(env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into()));
    let setup = ["SysWOW64", "System32"]
        .iter()
        .map(|d| root.join(d).join("OneDriveSetup.exe"))
        .find(|p| p.is_file());
    let file_count = env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|u| u.join("OneDrive"))
        .and_then(|d| fs::read_dir(d).ok())
        .map(|rd| {
            rd.flatten()
                .filter(|e| !e.file_name().to_string_lossy().eq_ignore_ascii_case("desktop.ini"))
                .count()
        })
        .unwrap_or(0);
    (setup, file_count)
}

pub fn edge_info() -> (Option<PathBuf>, Vec<(String, u64)>, usize) {
    let base = PathBuf::from(
        env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into()),
    )
    .join(r"Microsoft\Edge\Application");
    let mut vs: Vec<PathBuf> = fs::read_dir(base)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join(r"Installer\setup.exe"))
        .filter(|p| p.is_file())
        .collect();
    vs.sort();
    let setup = vs.pop();

    let mut profile_files = vec![];
    if let Some(local) = env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        let ed = local.join(r"Microsoft\Edge\User Data\Default");
        for f in ["Bookmarks", "Preferences", "Login Data", "Web Data", "History"] {
            if let Ok(meta) = fs::metadata(ed.join(f)) {
                profile_files.push((f.to_string(), meta.len()));
            }
        }
    }
    let wv2_count = list_procs()
        .into_iter()
        .filter(|(_, n)| n.eq_ignore_ascii_case("msedgewebview2.exe"))
        .count();
    (setup, profile_files, wv2_count)
}

pub fn run_preview() -> i32 {
    println!("WinRust Bloatware Purge Preview (read-only, nothing removed):");
    let items = scan_appx();
    let junk: Vec<&AppxEntry> = items.iter().filter(|i| i.tier == AppxTier::PureJunk).collect();
    let work: Vec<&AppxEntry> = items.iter().filter(|i| i.tier == AppxTier::WorkApp).collect();

    println!("\n  [Tier 1 — Pure Junk] ({} entries, safe to remove):", junk.len());
    for i in &junk {
        let kind = if i.provisioned { "provisioned" } else { "installed  " };
        println!("    - [{kind}] {}", i.name);
    }

    println!("\n  [Tier 2 — Work / Productivity Apps] ({} entries, KEPT by default; included only with `--tier all`):", work.len());
    for i in &work {
        let kind = if i.provisioned { "provisioned" } else { "installed  " };
        println!("    - [{kind}] {}", i.name);
    }

    let tasks = existing_telemetry_tasks();
    println!("\n  [Telemetry Scheduled Tasks] ({} present):", tasks.len());
    for t in &tasks {
        println!("    - {t}");
    }

    let (od_setup, od_files) = onedrive_info();
    println!(
        "\n  [OneDrive] setup={}, user folder non-ini files={}",
        od_setup.as_ref().map_or("not present".into(), |p| p.display().to_string()),
        od_files
    );

    let (edge_setup, edge_files, wv2) = edge_info();
    println!(
        "  [Edge & WebView2] setup={}, Default profile files={}, active msedgewebview2.exe processes={}",
        edge_setup.as_ref().map_or("not present".into(), |p| p.display().to_string()),
        edge_files.len(),
        wv2
    );
    println!(
        "\n  NOTE: Appx/OneDrive/Edge purge is IRREVERSIBLE and does NOT auto-import profiles or settings.\n  \
         A winget reinstall script is generated before removal, and Edge profiles are backed up by `gp-pc-boost backup`."
    );
    0
}

/// Purges the explicitly selected `(name, full_name, provisioned)` Appx items, plus optional tasks/OneDrive/Edge.
pub fn purge_selection(
    appx_selected: &[(String, String, bool)],
    delete_tasks: bool,
    do_od: bool,
    do_edge: bool,
) -> Result<String, String> {
    if !elevated() {
        return Err("Administrator privileges are required to purge Appx packages and telemetry tasks.".into());
    }
    let bak = create_backup()?;
    let mut restore_ps1 = String::from("# Best-effort reinstall script for purged Microsoft Store apps.\r\n");
    for (name, _, provisioned) in appx_selected {
        if !provisioned {
            restore_ps1.push_str(&format!("# winget install --name \"{name}\"\r\n"));
        }
    }
    let restore_path = state_dir().join("restore_apps.ps1");
    let _ = fs::create_dir_all(state_dir());
    let _ = fs::write(&restore_path, restore_ps1);

    let mut removed = 0usize;
    let mut failed = 0usize;
    for (name, full_name, provisioned) in appx_selected {
        let cmd = if *provisioned {
            format!("Remove-AppxProvisionedPackage -Online -PackageName '{full_name}' -ErrorAction Stop")
        } else {
            format!("Remove-AppxPackage -Package '{full_name}' -AllUsers -ErrorAction Stop")
        };
        let (ok, _) = run_cmd("powershell.exe", &["-NoProfile", "-NonInteractive", "-Command", &cmd]);
        if ok {
            removed += 1;
        } else {
            failed += 1;
        }
        println!("  [{}] {name}", if ok { "removed" } else { "failed " });
    }

    if delete_tasks {
        for t in existing_telemetry_tasks() {
            let (ok, _) = run_cmd("schtasks.exe", &["/Delete", "/TN", t, "/F"]);
            if ok {
                removed += 1;
            } else {
                failed += 1;
            }
            println!("  [{}] task {t}", if ok { "removed" } else { "failed " });
        }
    }

    if do_od {
        let _ = run_cmd("taskkill.exe", &["/f", "/im", "OneDrive.exe"]);
        if let (Some(setup), _) = onedrive_info() {
            let (ok, _) = run_cmd(&setup.to_string_lossy(), &["/uninstall"]);
            if ok {
                removed += 1;
            } else {
                failed += 1;
            }
            println!("  [{}] OneDrive", if ok { "removed" } else { "failed " });
        }
    }

    if do_edge
        && let (Some(setup), _, _) = edge_info()
    {
        let _ = run_cmd(
            "reg.exe",
            &["add", r"HKLM\SOFTWARE\Microsoft\EdgeUpdateDev", "/v", "AllowUninstall", "/t", "REG_SZ", "/d", "", "/f"],
        );
        let (ok, _) = run_cmd(
            &setup.to_string_lossy(),
            &["--uninstall", "--system-level", "--verbose-logging", "--force-uninstall"],
        );
        if ok {
            removed += 1;
        }
        println!("  [{}] Edge (best effort)", if ok { "removed" } else { "skipped/refused" });
    }

    let msg = format!(
        "Purged {removed} item(s) ({failed} errors). Backup: {} | Reinstall hints: {}",
        bak.display(),
        restore_path.display()
    );
    if failed > 0 { Err(msg) } else { Ok(msg) }
}

pub fn run_purge(args: &[String]) -> i32 {
    let confirm = args.iter().any(|a| a == "--confirm-irreversible");
    if !confirm {
        eprintln!("Refusing irreversible purge without `--confirm-irreversible`.");
        eprintln!("Run `gp-pc-boost bloat preview` first to review exact packages.");
        return 2;
    }

    let mut include_work = false;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--tier"
            && let Some(t) = args.get(i + 1)
        {
            if t.eq_ignore_ascii_case("all") {
                include_work = true;
            }
            i += 1;
        }
        i += 1;
    }
    let do_od = args.iter().any(|a| a == "--onedrive");
    let do_edge = args.iter().any(|a| a == "--edge");

    let items: Vec<(String, String, bool)> = scan_appx()
        .into_iter()
        .filter(|e| e.tier == AppxTier::PureJunk || (include_work && e.tier == AppxTier::WorkApp))
        .map(|e| (e.name, e.full_name, e.provisioned))
        .collect();

    match purge_selection(&items, true, do_od, do_edge) {
        Ok(msg) => {
            println!("\n{msg}");
            0
        }
        Err(e) => {
            eprintln!("\n{e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_junk_work_and_protected_correctly() {
        assert_eq!(classify_appx("Clipchamp.Clipchamp"), AppxTier::PureJunk);
        assert_eq!(classify_appx("Microsoft.BingNews"), AppxTier::PureJunk);
        assert_eq!(classify_appx("MicrosoftWindows.Client.WebExperience"), AppxTier::PureJunk);
        assert_eq!(classify_appx("MSTeams"), AppxTier::WorkApp);
        assert_eq!(classify_appx("Microsoft.OutlookForWindows"), AppxTier::WorkApp);
        assert_eq!(classify_appx("Microsoft.WindowsStore"), AppxTier::Protected);
        assert_eq!(classify_appx("Microsoft.WindowsCalculator"), AppxTier::Protected);
        assert_eq!(classify_appx("Microsoft.Windows.Photos"), AppxTier::Protected);
    }
}
