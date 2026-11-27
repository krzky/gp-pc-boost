use crate::{
    journal::state_dir,
    power::list_plans,
    tune::{query_reg_state, query_svc_mode},
};
use std::{
    env,
    fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
use winrust_common::proc::elevated;

fn backups_root() -> PathBuf {
    state_dir().join("backups")
}

fn run_cmd(prog: &str, args: &[&str]) -> (bool, String) {
    match Command::new(prog).args(args).creation_flags(0x0800_0000).output() {
        Ok(o) => (
            o.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        ),
        Err(e) => (false, e.to_string()),
    }
}

fn copy_if_exists(src: &Path, dst: &Path) -> bool {
    if !src.is_file() {
        return false;
    }
    if let Some(p) = dst.parent() {
        let _ = fs::create_dir_all(p);
    }
    fs::copy(src, dst).is_ok()
}

fn copy_dir_files(src_dir: &Path, dst_dir: &Path) -> usize {
    let Ok(rd) = fs::read_dir(src_dir) else {
        return 0;
    };
    let mut count = 0usize;
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_file() {
            let dst = dst_dir.join(entry.file_name());
            if copy_if_exists(&p, &dst) {
                count += 1;
            }
        }
    }
    count
}

pub fn create_backup_at(dir: &Path) -> Result<Vec<String>, String> {
    fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let mut summary = vec![];

    // 1. Mapped SMB network drives (`net use` + HKCU\Network export + restore script)
    let (_, net_out) = run_cmd("net.exe", &["use"]);
    let _ = fs::write(dir.join("mapped_drives.txt"), &net_out);
    let reg_net = dir.join("hkcu_network.reg");
    let (reg_ok, _) = run_cmd("reg.exe", &["export", r"HKCU\Network", &reg_net.to_string_lossy(), "/y"]);
    let restore_drives_ps1 = format!(
        "# Restores HKCU\\Network mapped drive definitions saved before WinRust changes.\r\n\
         $reg = Join-Path $PSScriptRoot 'hkcu_network.reg'\r\n\
         if (Test-Path $reg) {{ reg.exe import $reg }}\r\n\
         Write-Output 'Current net use:'\r\n\
         net.exe use\r\n"
    );
    let _ = fs::write(dir.join("restore_mapped_drives.ps1"), restore_drives_ps1);
    summary.push(format!(
        "mapped_drives.txt + hkcu_network.reg (exported={reg_ok})"
    ));

    // 2. Microsoft Edge Default profile files (Bookmarks, Preferences, Login Data, Web Data)
    if let Some(local) = env::var_os("LOCALAPPDATA").map(PathBuf::from) {
        let edge_def = local.join(r"Microsoft\Edge\User Data\Default");
        let edge_bak = dir.join("edge_profile_backup");
        let mut copied = 0usize;
        for name in ["Bookmarks", "Preferences", "Login Data", "Web Data"] {
            if copy_if_exists(&edge_def.join(name), &edge_bak.join(name)) {
                copied += 1;
            }
        }
        summary.push(format!("edge_profile_backup ({copied}/4 profile files copied)"));
    }

    // 3. Quick Access & Jump Lists
    if let Some(roam) = env::var_os("APPDATA").map(PathBuf::from) {
        let qa_dst = dir.join("quick_access_backup");
        let n1 = copy_dir_files(
            &roam.join(r"Microsoft\Windows\Recent\AutomaticDestinations"),
            &qa_dst.join("AutomaticDestinations"),
        );
        let n2 = copy_dir_files(
            &roam.join(r"Microsoft\Windows\Recent\CustomDestinations"),
            &qa_dst.join("CustomDestinations"),
        );
        summary.push(format!("quick_access_backup ({n1} auto + {n2} custom)"));
    }

    // 4. Installed Appx package manifest
    let appx_cmd = if elevated() {
        "Get-AppxPackage -AllUsers | Select-Object Name, PackageFullName, NonRemovable | Out-String -Width 400"
    } else {
        "Get-AppxPackage | Select-Object Name, PackageFullName, NonRemovable | Out-String -Width 400"
    };
    let (appx_ok, appx_out) = run_cmd(
        "powershell.exe",
        &["-NoProfile", "-NonInteractive", "-Command", appx_cmd],
    );
    if appx_ok {
        let _ = fs::write(dir.join("appx_installed.txt"), &appx_out);
        summary.push("appx_installed.txt".into());
    }

    // 5. System pre-change baseline
    let tm_dbg = query_reg_state(
        r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe",
        "Debugger",
    );
    let sh_dbg = query_reg_state(
        r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\SearchHost.exe",
        "Debugger",
    );
    let ars = query_reg_state(
        r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon",
        "AutoRestartShell",
    );
    let wsearch = query_svc_mode("WSearch");
    let active_plan = list_plans()
        .into_iter()
        .find(|p| p.active)
        .map(|p| p.name)
        .unwrap_or_else(|| "unknown".into());
    let pre_state = format!(
        "elevated={}\r\ntaskmgr_debugger={:?}\r\nsearchhost_debugger={:?}\r\nauto_restart_shell={:?}\r\nwsearch_start={:?}\r\nactive_power_plan={}\r\n",
        elevated(),
        tm_dbg,
        sh_dbg,
        ars,
        wsearch,
        active_plan
    );
    let _ = fs::write(dir.join("system_pre_state.txt"), pre_state);
    summary.push("system_pre_state.txt".into());

    Ok(summary)
}

pub fn create_backup() -> Result<PathBuf, String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let dir = backups_root().join(ts.to_string());
    let _ = create_backup_at(&dir)?;
    Ok(dir)
}

pub fn run_backup(args: &[String]) -> i32 {
    match args.first().map(String::as_str) {
        Some("list") => {
            let root = backups_root();
            let Ok(rd) = fs::read_dir(&root) else {
                println!("No backups created yet ({}).", root.display());
                return 0;
            };
            let mut dirs: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            dirs.sort();
            if dirs.is_empty() {
                println!("No backups created yet ({}).", root.display());
            } else {
                println!("Saved Backups ({}):", root.display());
                for d in dirs {
                    println!("  {}", d.display());
                }
            }
            0
        }
        Some("create") | None => {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let dir = backups_root().join(ts.to_string());
            match create_backup_at(&dir) {
                Ok(items) => {
                    println!("Pre-change backup created: {}", dir.display());
                    for item in items {
                        println!("  - {item}");
                    }
                    0
                }
                Err(e) => {
                    eprintln!("Backup failed: {e}");
                    1
                }
            }
        }
        Some(other) => {
            eprintln!("Unknown `backup` subcommand '{other}'. Expected: create | list");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_backup_directory_and_restore_script() {
        let dir = env::temp_dir().join("gp-pc-boost-backup-test");
        let _ = fs::remove_dir_all(&dir);
        let summary = create_backup_at(&dir).unwrap();
        assert!(!summary.is_empty());
        assert!(dir.join("mapped_drives.txt").is_file());
        assert!(dir.join("restore_mapped_drives.ps1").is_file());
        assert!(dir.join("system_pre_state.txt").is_file());
        let _ = fs::remove_dir_all(&dir);
    }
}
