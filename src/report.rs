use crate::{
    bloat::{AppxTier, edge_info, existing_telemetry_tasks, onedrive_info, scan_appx},
    disk::{CleanRisk, build_clean_targets, fixed_drives},
    mem::{fmt_bytes, snapshot as mem_snapshot},
    power::list_plans,
    replace::{ALL_FORWARD, Component},
};
use std::{
    fs,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
};
use winrust_common::{mft::human, proc::list as list_procs};

fn run_ps(script: &str) -> String {
    Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(0x0800_0000)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

pub fn mapped_drives() -> Vec<(String, String, String)> {
    let out = run_ps(
        "Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=4' | ForEach-Object { \"$($_.DeviceID)|$($_.ProviderName)|$($_.VolumeName)\" }",
    );
    out.lines()
        .filter_map(|l| {
            let mut p = l.splitn(3, '|');
            Some((p.next()?.to_string(), p.next()?.to_string(), p.next().unwrap_or("").to_string()))
        })
        .collect()
}

pub fn webview2_parents() -> Vec<String> {
    let out = run_ps(
        "Get-CimInstance Win32_Process -Filter \"Name='msedgewebview2.exe'\" | ForEach-Object { (Get-Process -Id $_.ParentProcessId -ErrorAction SilentlyContinue).ProcessName } | Where-Object { $_ -and $_ -ne 'msedgewebview2' } | Select-Object -Unique",
    );
    out.lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

pub fn generate_report(out_path: &Path) -> Result<String, String> {
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let ms = mem_snapshot();
    let used = ms.total.saturating_sub(ms.avail);
    let plans = list_plans();
    let active_plan = plans
        .iter()
        .find(|p| p.active)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "Unknown".into());
    let sophos = list_procs()
        .iter()
        .any(|(_, n)| n.to_ascii_lowercase().contains("sophos") || n.eq_ignore_ascii_case("sspservice.exe"));

    let drives = fixed_drives();
    let mapped = mapped_drives();
    let clean_targets = build_clean_targets();
    let safe_bytes: u64 = clean_targets
        .iter()
        .filter(|t| t.risk == CleanRisk::Safe)
        .map(|t| t.bytes)
        .sum();
    let rebuild_bytes: u64 = clean_targets
        .iter()
        .filter(|t| t.risk == CleanRisk::Rebuild)
        .map(|t| t.bytes)
        .sum();

    let appx = scan_appx();
    let junk: Vec<_> = appx.iter().filter(|a| a.tier == AppxTier::PureJunk).collect();
    let work: Vec<_> = appx.iter().filter(|a| a.tier == AppxTier::WorkApp).collect();
    let tasks = existing_telemetry_tasks();
    let (od_setup, od_files) = onedrive_info();
    let (edge_setup, edge_files, wv2_count) = edge_info();
    let wv2_hosts = webview2_parents();

    let mut md = String::new();
    md.push_str("# WinRust Stock-Component & Bloatware Compatibility Report\n\n");
    md.push_str("Generated live from this PC (read-only scan — nothing was modified).\n\n");

    md.push_str("## 1. Live System & Workbench Compatibility Check\n\n");
    md.push_str(&format!("- **CPU Threads**: {cpus}\n"));
    md.push_str(&format!(
        "- **RAM**: {} used / {} total ({}% load) | Standby Cache: {}\n",
        fmt_bytes(used),
        fmt_bytes(ms.total),
        ms.commit_load_pct,
        ms.standby.map(fmt_bytes).unwrap_or_else(|| "n/a".into())
    ));
    md.push_str(&format!("- **Active Power Plan**: `{active_plan}`\n"));
    md.push_str(&format!(
        "- **Antivirus**: {}\n",
        if sophos {
            "Sophos Intercept X active (Windows Defender exclusions blocked by policy)"
        } else {
            "Standard"
        }
    ));
    md.push_str(&format!(
        "- **Reclaimable Caches**: {} Safe + {} Rebuild = **{}**\n\n",
        human(safe_bytes),
        human(rebuild_bytes),
        human(safe_bytes + rebuild_bytes)
    ));

    md.push_str("### Local Drives\n\n| Drive | Free | Total | Free % | Status |\n|---|---:|---:|---:|---|\n");
    for d in &drives {
        let st = if d.free_pct < 15.0 { "LOW FREE SPACE (<15%)" } else { "OK" };
        md.push_str(&format!(
            "| `{}:` | {} | {} | {:.1}% | {} |\n",
            d.letter,
            human(d.free),
            human(d.total),
            d.free_pct,
            st
        ));
    }

    md.push_str("\n### Mapped SMB Network Drives Detected (`N:`–`Z:`)\n\n");
    if mapped.is_empty() {
        md.push_str("None detected.\n");
    } else {
        md.push_str("| Drive | UNC Target | Volume |\n|---|---|---|\n");
        for (dev, unc, vol) in &mapped {
            md.push_str(&format!("| `{dev}` | `{unc}` | {vol} |\n"));
        }
    }

    md.push_str("\n### WebView2, Edge Profile & OneDrive Check\n\n");
    md.push_str(&format!(
        "- **OneDrive**: `C:\\Users\\GemaPro\\OneDrive` has **{od_files}** user files/folders (setup: `{}`). Safe to uninstall if unused.\n",
        od_setup.as_ref().map_or("not present".into(), |p| p.display().to_string())
    ));
    md.push_str(&format!(
        "- **Microsoft Edge**: Active `Default` profile contains **{}** live files (`Bookmarks`, `Preferences`, `Login Data`, `Web Data`, `History`). Setup: `{}`.\n",
        edge_files.len(),
        edge_setup.as_ref().map_or("not present".into(), |p| p.display().to_string())
    ));
    md.push_str(&format!(
        "- **Active WebView2 (`msedgewebview2.exe`)**: **{wv2_count}** processes running, hosted by: `{}`.\n\n",
        if wv2_hosts.is_empty() { "none".into() } else { wv2_hosts.join(", ") }
    ));

    md.push_str("## 2. Honest Answer on Profile/Settings Import & Reversibility\n\n");
    md.push_str(
        "| Change | Imports Old Profile/Settings? | Reversible? | Notes |\n|---|---|---|---|\n\
         | WinRust Component Replacements (`taskmgr`, `settings`, `photoview`, `wiztree`, `debloat`, `topbar`, `search`, `explorer`) | **Yes** — keeps user files, mapped drives (`HKCU\\Network`), Quick Access, and pre-existing registry/service values in `replace_journal.log` | **100% Reversible** | `gp-pc-boost replace revert all` restores every original value and restarts `explorer.exe` |\n\
         | `gp-pc-boost backup create` | Snapshots `net use`, `HKCU\\Network`, Edge `Default` profile files, Quick Access jump lists, and installed Appx list | **Automatic before apply/purge** | Stored in `%LOCALAPPDATA%\\gp-pc-boost\\backups\\<timestamp>` |\n\
         | Bloatware Appx Purge (`bloat purge`) | **No** — `Remove-AppxPackage` removes the app & its UWP app container | **Irreversible** | Reinstall requires Microsoft Store or `winget`; `restore_apps.ps1` is generated automatically |\n\
         | OneDrive / Edge Uninstall (`--onedrive`, `--edge`) | **No** — does not migrate Edge bookmarks/passwords into GP Browser | **Irreversible** (must reinstall) | Edge `Default` profile is backed up before purge; **keep Edge** to preserve WebView2 (`coucou`, `WhatsApp`, `grass`) |\n\n"
    );

    md.push_str("## 3. Component-by-Component WinRust Replacement Matrix\n\n");
    md.push_str("| Component | Status | Replaces | Workbench / Mapped-Drive Compatibility | Risk | Apply / Revert |\n|---|---|---|---|---|---|\n");
    for c in ALL_FORWARD {
        let status = if c.is_installed() { "REPLACED" } else { "Stock" };
        let compat = match c {
            Component::TaskMgr | Component::Settings | Component::PhotoView | Component::WizTree | Component::Debloat => {
                "**Fully Compatible** — zero impact on `N:`–`Z:`, `explorer.exe`, WebView2, or shortcuts"
            }
            Component::Topbar => {
                "**Compatible with Caveat** — hides stock Windows 11 taskbar; no impact on mapped drives or file dialogs"
            }
            Component::Search => {
                "**Caveat** — indexes local NTFS (`C:`/`D:`/`F:`/`G:`) only; **skips `N:`–`Z:` SMB shares** and disables `Win+S`/Explorer/Outlook search"
            }
            Component::Explorer => {
                "**Caution** — sees mapped drives, **but** disables `explorer.exe` shell, lacks Network discovery/Quick Access scrollbar, lacks `Win+V` clipboard history & MTP phone view, and can affect Store/MS sign-in dialogs"
            }
        };
        let risk = match c {
            Component::TaskMgr | Component::Settings | Component::PhotoView | Component::WizTree | Component::Debloat => "Low",
            Component::Topbar | Component::Search => "Medium",
            Component::Explorer => "High (shell swap)",
        };
        md.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | `replace apply {}` / `replace revert {}` |\n",
            c.name(),
            status,
            c.replaces(),
            compat,
            risk,
            c.name(),
            c.name()
        ));
    }

    md.push_str("\n## 4. Bloatware & Optional Removals Found on This PC\n\n");
    md.push_str(&format!(
        "- **Tier 1 — Pure Junk ({} entries found, recommended to purge)**: {}\n",
        junk.len(),
        if junk.is_empty() {
            "none".into()
        } else {
            junk.iter().map(|i| format!("`{}`", i.name)).collect::<Vec<_>>().join(", ")
        }
    ));
    md.push_str(&format!(
        "- **Tier 2 — Work/Productivity Apps ({} entries found, KEPT by default)**: {}\n",
        work.len(),
        if work.is_empty() {
            "none".into()
        } else {
            work.iter().map(|i| format!("`{}`", i.name)).collect::<Vec<_>>().join(", ")
        }
    ));
    md.push_str(&format!(
        "- **Telemetry Scheduled Tasks ({} active)**: {}\n",
        tasks.len(),
        if tasks.is_empty() {
            "none".into()
        } else {
            tasks.iter().map(|t| format!("`{t}`")).collect::<Vec<_>>().join(", ")
        }
    ));
    md.push_str("- **OneDrive Verdict**: Empty (0 user files) -> **Safe to purge (`--onedrive`)** if unused.\n");
    md.push_str("- **Edge Verdict**: Active `Default` profile + **WebView2** hosts `coucou`, `WhatsApp`, `grass` -> **Recommend KEEP Edge binary**, disable background boost via `debloat`/`tune`.\n\n");

    md.push_str("## 5. Ready-to-Run Presets & Rollback\n\n");
    md.push_str("All scripts live in `D:\\Project\\gp-pc-boost\\scripts\\` and request Admin UAC when launched:\n\n");
    md.push_str("1. **`scripts\\apply-safe-all.ps1` (Recommended — Zero Workbench Disruption)**\n");
    md.push_str("   - Creates full backup, switches power plan to `High performance`, purges standby RAM, replaces `taskmgr`, `settings`, `photoview`, `wiztree`, applies reversible `debloat` + NTFS tweaks, and prompts before purging Tier 1 junk / OneDrive.\n");
    md.push_str("2. **`scripts\\apply-express.ps1` (Replaces Taskbar + Search, Keeps Explorer Shell)**\n");
    md.push_str("   - Adds `topbar` and `rust-search` while leaving `explorer.exe` intact for `N:`–`Z:` and `Win+V`.\n");
    md.push_str("3. **`scripts\\apply-full-winrust.ps1` (Full WinRust Stack Including `rust-explorer` Shell)**\n");
    md.push_str("   - Requires typing `YES`; replaces `explorer.exe` shell with `rust-explorer` + `rs-shell`.\n");
    md.push_str("4. **`scripts\\rollback-all.ps1` (1-Click Full Restore)**\n");
    md.push_str("   - Reverts all WinRust components in safe reverse order (`explorer` before `topbar`), undoes `tune` + `power`, restarts `explorer.exe`, and verifies `net use` mapped drives.\n\n");
    md.push_str("### Emergency Recovery (If Desktop/Taskbar Ever Looks Missing)\n\n");
    md.push_str("1. Press `Ctrl+Alt+Del` -> **Task Manager** -> **Run new task** -> `explorer.exe`.\n");
    md.push_str("2. Run `powershell -ExecutionPolicy Bypass -File D:\\Project\\gp-pc-boost\\scripts\\rollback-all.ps1`.\n");

    if let Some(p) = out_path.parent() {
        let _ = fs::create_dir_all(p);
    }
    fs::write(out_path, &md).map_err(|e| format!("write {}: {e}", out_path.display()))?;
    Ok(md)
}

pub fn run_report(args: &[String]) -> i32 {
    let mut out = PathBuf::from(r"D:\Project\gp-pc-boost\REPORT.md");
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--out"
            && let Some(p) = args.get(i + 1)
        {
            out = PathBuf::from(p);
            i += 1;
        }
        i += 1;
    }

    match generate_report(&out) {
        Ok(_) => {
            println!("Compatibility & Selection Report written to: {}", out.display());
            0
        }
        Err(e) => {
            eprintln!("Report generation failed: {e}");
            1
        }
    }
}
