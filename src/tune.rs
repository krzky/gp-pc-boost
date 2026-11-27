use crate::journal::{clear_journal, default_journal_path, read_entries, record_if_new};
use std::{os::windows::process::CommandExt, process::Command};
use winrust_common::{
    journal::Entry,
    proc::elevated,
    reg::{dword, get_dword, value},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Ntfs,
    Telemetry,
    Edge,
    GameDvr,
    AllSafe,
}

impl Group {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ntfs" => Some(Self::Ntfs),
            "telemetry" => Some(Self::Telemetry),
            "edge" => Some(Self::Edge),
            "gamedvr" | "game-dvr" => Some(Self::GameDvr),
            "all-safe" | "all" => Some(Self::AllSafe),
            _ => None,
        }
    }
}

pub struct RegTweak {
    pub group: Group,
    pub key: &'static str,
    pub name: &'static str,
    pub desired: u32,
    pub note: &'static str,
}

pub struct SvcTweak {
    pub group: Group,
    pub name: &'static str,
    pub note: &'static str,
}

pub const REG_TWEAKS: &[RegTweak] = &[
    RegTweak {
        group: Group::Ntfs,
        key: r"HKLM\SYSTEM\CurrentControlSet\Control\FileSystem",
        name: "NtfsDisableLastAccessUpdate",
        desired: 0x8000_0001,
        note: "Skip writing NTFS last-access timestamp on every read (takes effect after reboot)",
    },
    RegTweak {
        group: Group::Ntfs,
        key: r"HKLM\SYSTEM\CurrentControlSet\Control\FileSystem",
        name: "NtfsDisable8dot3NameCreation",
        desired: 1,
        note: "Disable 8.3 short-filename generation on NTFS volumes (takes effect after reboot)",
    },
    RegTweak {
        group: Group::Telemetry,
        key: r"HKLM\SOFTWARE\Policies\Microsoft\Windows\DataCollection",
        name: "AllowTelemetry",
        desired: 0,
        note: "Set Windows telemetry policy to minimum",
    },
    RegTweak {
        group: Group::Telemetry,
        key: r"HKLM\SOFTWARE\Policies\Microsoft\Windows\DataCollection",
        name: "DoNotShowFeedbackNotifications",
        desired: 1,
        note: "Disable Windows feedback popups",
    },
    RegTweak {
        group: Group::Telemetry,
        key: r"HKLM\SOFTWARE\Policies\Microsoft\Windows\WindowsCopilot",
        name: "TurnOffWindowsCopilot",
        desired: 1,
        note: "Disable Windows Copilot background policy",
    },
    RegTweak {
        group: Group::Telemetry,
        key: r"HKLM\SOFTWARE\Policies\Microsoft\Windows\WindowsAI",
        name: "DisableAIDataAnalysis",
        desired: 1,
        note: "Disable Recall / AI snapshot data analysis",
    },
    RegTweak {
        group: Group::Telemetry,
        key: r"HKLM\SOFTWARE\Policies\Microsoft\Windows\CloudContent",
        name: "DisableWindowsConsumerFeatures",
        desired: 1,
        note: "Prevent Windows consumer promoted apps",
    },
    RegTweak {
        group: Group::Telemetry,
        key: r"HKLM\SOFTWARE\Policies\Microsoft\Windows\CloudContent",
        name: "DisableSoftLanding",
        desired: 1,
        note: "Disable Windows tips and soft-landing suggestions",
    },
    RegTweak {
        group: Group::Edge,
        key: r"HKLM\SOFTWARE\Policies\Microsoft\Edge",
        name: "StartupBoostEnabled",
        desired: 0,
        note: "Stop Microsoft Edge from pre-launching at sign-in",
    },
    RegTweak {
        group: Group::Edge,
        key: r"HKLM\SOFTWARE\Policies\Microsoft\Edge",
        name: "BackgroundModeEnabled",
        desired: 0,
        note: "Stop Microsoft Edge from staying alive in background after closing",
    },
    RegTweak {
        group: Group::GameDvr,
        key: r"HKLM\SOFTWARE\Policies\Microsoft\Windows\GameDVR",
        name: "AllowGameDVR",
        desired: 0,
        note: "Disable background Game Bar DVR capture overhead",
    },
];

pub const SVC_TWEAKS: &[SvcTweak] = &[
    SvcTweak {
        group: Group::Telemetry,
        name: "DiagTrack",
        note: "Connected User Experiences and Telemetry",
    },
    SvcTweak {
        group: Group::Telemetry,
        name: "dmwappushservice",
        note: "WAP Push Message Routing (telemetry helper)",
    },
    SvcTweak {
        group: Group::Telemetry,
        name: "MapsBroker",
        note: "Downloaded Maps Manager",
    },
    SvcTweak {
        group: Group::Telemetry,
        name: "WerSvc",
        note: "Windows Error Reporting Service",
    },
    SvcTweak {
        group: Group::Telemetry,
        name: "RetailDemo",
        note: "Retail Demo Service",
    },
    SvcTweak {
        group: Group::Telemetry,
        name: "WSAIFabricSvc",
        note: "Windows AI Fabric Service",
    },
];

fn matches_group(item: Group, target: Group) -> bool {
    target == Group::AllSafe || item == target
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

#[derive(Debug, PartialEq)]
pub enum RegState {
    Missing,
    Dword(u32),
    Other(String),
}

pub fn query_reg_state(key: &str, name: &str) -> RegState {
    if let Some(d) = get_dword(key, name) {
        return RegState::Dword(d);
    }
    let (ok, out) = run_cmd("reg.exe", &["query", key, "/v", name]);
    if !ok {
        return RegState::Missing;
    }
    match value(&out) {
        None => RegState::Missing,
        Some(("REG_DWORD", _)) => dword(&out).map_or_else(|| RegState::Other("REG_DWORD".into()), RegState::Dword),
        Some((ty, data)) => RegState::Other(format!("{ty}:{data}")),
    }
}

pub fn query_svc_mode(svc: &str) -> Option<&'static str> {
    let (ok, out) = run_cmd("sc.exe", &["qc", svc]);
    let line = out.lines().find(|l| ok && l.contains("START_TYPE"))?;
    if line.contains("DISABLED") {
        Some("disabled")
    } else if line.contains("DEMAND_START") {
        Some("demand")
    } else if line.contains("AUTO_START") {
        if line.contains("DELAYED") {
            Some("delayed-auto")
        } else {
            Some("auto")
        }
    } else {
        None
    }
}

pub fn run_preview(group: Group) -> i32 {
    println!("Tune Preview (read-only, nothing modified):");
    println!("  Registry Tweaks:");
    for t in REG_TWEAKS.iter().filter(|t| matches_group(t.group, group)) {
        let cur = match query_reg_state(t.key, t.name) {
            RegState::Dword(v) if v == t.desired => format!("0x{v:X} (already optimal)"),
            RegState::Dword(v) => format!("0x{v:X} -> 0x{:X}", t.desired),
            RegState::Missing => format!("(missing) -> 0x{:X}", t.desired),
            RegState::Other(ty) => format!("{ty} (non-DWORD: skipped)"),
        };
        println!("    {:<30} {:<26} {}", t.name, cur, t.note);
    }
    let svcs: Vec<&SvcTweak> = SVC_TWEAKS
        .iter()
        .filter(|s| matches_group(s.group, group))
        .collect();
    if !svcs.is_empty() {
        println!("  Services:");
        for s in svcs {
            let st = match query_svc_mode(s.name) {
                None => "not present".into(),
                Some("disabled") => "disabled (already optimal)".into(),
                Some(m) => format!("{m} -> disabled"),
            };
            println!("    {:<20} {:<26} {}", s.name, st, s.note);
        }
    }
    0
}

pub fn run_status() -> i32 {
    let mut reg_on = 0usize;
    for t in REG_TWEAKS {
        if matches!(query_reg_state(t.key, t.name), RegState::Dword(v) if v == t.desired) {
            reg_on += 1;
        }
    }
    let mut svc_on = 0usize;
    for s in SVC_TWEAKS {
        if query_svc_mode(s.name) == Some("disabled") {
            svc_on += 1;
        }
    }
    let jpath = default_journal_path();
    let jcount = read_entries(&jpath).map(|v| v.len()).unwrap_or(0);
    println!("Tune Status:");
    println!("  Registry optimizations active : {reg_on}/{}", REG_TWEAKS.len());
    println!("  Telemetry services disabled   : {svc_on}/{}", SVC_TWEAKS.len());
    println!("  Undo journal entries          : {jcount} ({})", jpath.display());
    0
}

pub fn run_apply(group: Group) -> i32 {
    if !elevated() {
        eprintln!("Administrator privileges are required to apply HKLM registry and service optimizations.");
        return 1;
    }
    let jpath = default_journal_path();
    let mut changed = 0usize;
    let mut failed = 0usize;

    for t in REG_TWEAKS.iter().filter(|t| matches_group(t.group, group)) {
        let old = match query_reg_state(t.key, t.name) {
            RegState::Dword(v) if v == t.desired => {
                println!("  [skip] {} already 0x{:X}", t.name, t.desired);
                continue;
            }
            RegState::Other(ty) => {
                println!("  [skip] {} is {ty}, leaving untouched", t.name);
                continue;
            }
            RegState::Missing => None,
            RegState::Dword(v) => Some(("REG_DWORD".to_string(), v.to_string())),
        };
        let entry = Entry::Reg {
            key: t.key.into(),
            name: t.name.into(),
            old,
        };
        if let Err(e) = record_if_new(&jpath, &entry) {
            eprintln!("Journal write failed before modifying {}: {e}", t.name);
            return 1;
        }
        let val_str = t.desired.to_string();
        let (ok, out) = run_cmd(
            "reg.exe",
            &["add", t.key, "/v", t.name, "/t", "REG_DWORD", "/d", &val_str, "/f"],
        );
        if ok {
            changed += 1;
            println!("  [done] {} = 0x{:X}", t.name, t.desired);
        } else {
            failed += 1;
            eprintln!("  [fail] {}: {}", t.name, out.trim());
        }
    }

    for s in SVC_TWEAKS.iter().filter(|s| matches_group(s.group, group)) {
        match query_svc_mode(s.name) {
            None => {
                println!("  [skip] service {} not present", s.name);
                continue;
            }
            Some("disabled") => {
                println!("  [skip] service {} already disabled", s.name);
                continue;
            }
            Some(mode) => {
                let entry = Entry::Svc {
                    name: s.name.into(),
                    mode: mode.into(),
                };
                if let Err(e) = record_if_new(&jpath, &entry) {
                    eprintln!("Journal write failed before modifying {}: {e}", s.name);
                    return 1;
                }
                let (ok, out) = run_cmd("sc.exe", &["config", s.name, "start=", "disabled"]);
                let _ = run_cmd("sc.exe", &["stop", s.name]);
                if ok {
                    changed += 1;
                    println!("  [done] service {}: {mode} -> disabled", s.name);
                } else {
                    failed += 1;
                    eprintln!("  [fail] service {}: {}", s.name, out.trim());
                }
            }
        }
    }

    println!("\nApplied {changed} changes ({failed} failed). Undo journal: {}", jpath.display());
    if failed > 0 { 1 } else { 0 }
}

pub fn run_undo() -> i32 {
    if !elevated() {
        eprintln!("Administrator privileges are required to restore HKLM registry and service settings.");
        return 1;
    }
    let jpath = default_journal_path();
    let entries = match read_entries(&jpath) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if entries.is_empty() {
        println!("Nothing to undo (journal is empty).");
        return 0;
    }

    let mut failed = 0usize;
    for e in entries.iter().rev() {
        let ok = match e {
            Entry::Reg { key, name, old: None } => {
                let (ok, _) = run_cmd("reg.exe", &["delete", key, "/v", name, "/f"]);
                let missing = query_reg_state(key, name) == RegState::Missing;
                println!("  [{}] delete {name}", if ok || missing { "undo" } else { "fail" });
                ok || missing
            }
            Entry::Reg {
                key,
                name,
                old: Some((ty, data)),
            } => {
                let (ok, _) = run_cmd("reg.exe", &["add", key, "/v", name, "/t", ty, "/d", data, "/f"]);
                println!("  [{}] restore {name} = {data}", if ok { "undo" } else { "fail" });
                ok
            }
            Entry::Svc { name, mode } => {
                let (ok, _) = run_cmd("sc.exe", &["config", name, "start=", mode]);
                println!("  [{}] restore service {name} -> {mode}", if ok { "undo" } else { "fail" });
                ok
            }
        };
        if !ok {
            failed += 1;
        }
    }

    if failed == 0 {
        let _ = clear_journal(&jpath);
        println!("\nAll changes restored cleanly and journal cleared.");
        0
    } else {
        eprintln!("\n{failed} restore step(s) failed; journal kept at {}", jpath.display());
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tweaks_are_unique_and_never_touch_protected_subsystems() {
        let mut seen = std::collections::HashSet::new();
        for t in REG_TWEAKS {
            assert!(t.key.starts_with("HKLM\\") || t.key.starts_with("HKCU\\"));
            assert!(seen.insert((t.key, t.name)), "duplicate tweak {} {}", t.key, t.name);
            let low = t.key.to_ascii_lowercase();
            assert!(!low.contains("windows defender"));
            assert!(!low.contains("winlogon"));
            assert!(!low.contains("windowsupdate"));
        }
        for s in SVC_TWEAKS {
            let low = s.name.to_ascii_lowercase();
            assert_ne!(low, "wsearch");
            assert_ne!(low, "wuauserv");
            assert!(!low.contains("sophos"));
        }
    }
}
