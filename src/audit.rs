use crate::{
    disk::{CleanRisk, build_clean_targets, fixed_drives},
    mem::{fmt_bytes, snapshot as mem_snapshot},
    power::list_plans,
    startup::{list as list_startup, read_boot_log},
    tune::{REG_TWEAKS, RegState, SVC_TWEAKS, query_reg_state, query_svc_mode},
};
use std::{cmp::Reverse, ptr::null_mut, thread, time::Duration};
use windows_sys::Win32::{
    Foundation::CloseHandle,
    System::{
        Performance::*,
        ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS},
        Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ},
    },
};
use winrust_common::{
    mft::human,
    proc::{elevated, list as list_procs},
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn process_working_set(pid: u32) -> u64 {
    unsafe {
        let mut h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
        if h.is_null() {
            h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        }
        if h.is_null() {
            return 0;
        }
        let mut pmc: PROCESS_MEMORY_COUNTERS = std::mem::zeroed();
        pmc.cb = size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = K32GetProcessMemoryInfo(h, &mut pmc, pmc.cb) != 0;
        CloseHandle(h);
        if ok { pmc.WorkingSetSize as u64 } else { 0 }
    }
}

fn sample_pdh_live() -> Option<(f64, f64, f64)> {
    unsafe {
        let mut q: PDH_HQUERY = null_mut();
        if PdhOpenQueryW(std::ptr::null(), 0, &mut q) != 0 {
            return None;
        }
        let add = |path: &str| -> Option<PDH_HCOUNTER> {
            let mut c: PDH_HCOUNTER = null_mut();
            (PdhAddEnglishCounterW(q, wide(path).as_ptr(), 0, &mut c) == 0).then_some(c)
        };
        let cpu = add(r"\Processor Information(_Total)\% Processor Performance");
        let disk_act = add(r"\PhysicalDisk(_Total)\% Disk Time");
        let disk_lat = add(r"\PhysicalDisk(_Total)\Avg. Disk sec/Transfer");
        PdhCollectQueryData(q);
        thread::sleep(Duration::from_millis(300));
        PdhCollectQueryData(q);

        let val = |c: Option<PDH_HCOUNTER>| -> f64 {
            let Some(c) = c else { return 0.0 };
            let mut v: PDH_FMT_COUNTERVALUE = std::mem::zeroed();
            if PdhGetFormattedCounterValue(c, PDH_FMT_DOUBLE, null_mut(), &mut v) == 0 && v.CStatus == 0 {
                v.Anonymous.doubleValue
            } else {
                0.0
            }
        };
        let res = (val(cpu), val(disk_act).clamp(0.0, 100.0), val(disk_lat) * 1000.0);
        PdhCloseQuery(q);
        Some(res)
    }
}

pub fn run_audit() -> i32 {
    println!("================================================================");
    println!("  gp-pc-boost — Windows Performance Audit (Read-Only)");
    println!("================================================================");

    let is_admin = elevated();
    let procs = list_procs();
    let sophos_active = procs.iter().any(|(_, n)| {
        let l = n.to_ascii_lowercase();
        l.contains("sophos") || l == "sspservice.exe"
    });
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);

    println!("\n[1] System & Safety Context");
    println!("  Logical CPUs   : {cpus}");
    println!("  Privileges     : {}", if is_admin { "Administrator (full kernel metrics)" } else { "Standard User (read-only fallback)" });
    println!("  Active AV      : {}", if sophos_active { "Sophos Intercept X detected (Defender exclusions disabled by policy)" } else { "Standard" });

    let mut score: i32 = 100;
    let mut actions: Vec<(u8, String, String)> = vec![];

    // [2] Power Plan
    println!("\n[2] Power Plan");
    let plans = list_plans();
    let active_plan = plans.iter().find(|p| p.active);
    let has_faster_plan = plans.iter().any(|p| {
        let l = p.name.to_ascii_lowercase();
        l.contains("high performance") || l.contains("ultimate")
    });
    if let Some(ap) = active_plan {
        println!("  Active Plan    : {}", ap.name);
        let low = ap.name.to_ascii_lowercase();
        if (low.contains("balanced") || low.contains("power saver")) && has_faster_plan {
            score -= 10;
            actions.push((
                1,
                format!("Power plan is '{}' on a {cpus}-thread system", ap.name),
                "gp-pc-boost power set high   (or `ultimate` / `gp-pc-boost profile work`)".into(),
            ));
        }
    }

    // [3] Memory & Top Processes
    println!("\n[3] Memory & Standby Cache");
    let ms = mem_snapshot();
    let used = ms.total.saturating_sub(ms.avail);
    println!("  RAM            : {} used / {} total ({}% load)", fmt_bytes(used), fmt_bytes(ms.total), ms.commit_load_pct);
    if let (Some(free), Some(standby)) = (ms.free, ms.standby) {
        println!("  Free / Standby : {} free | {} standby cache", fmt_bytes(free), fmt_bytes(standby));
        let pct_free = if ms.total > 0 { (free as f64 / ms.total as f64) * 100.0 } else { 100.0 };
        if pct_free < 5.0 && standby > (1u64 << 30) {
            score -= 10;
            actions.push((
                2,
                format!("Free RAM is low ({}) while standby cache holds {}", fmt_bytes(free), fmt_bytes(standby)),
                "gp-pc-boost mem purge   (elevated)".into(),
            ));
        }
    }

    let mut proc_mem: Vec<(u64, u32, String)> = procs
        .iter()
        .map(|(pid, name)| (process_working_set(*pid), *pid, name.clone()))
        .filter(|(ws, _, _)| *ws > 0)
        .collect();
    proc_mem.sort_by_key(|x| Reverse(x.0));
    println!("  Top 8 Processes by Working Set:");
    for (ws, pid, name) in proc_mem.iter().take(8) {
        println!("    {:>9}  [PID {:>6}]  {}", fmt_bytes(*ws), pid, name);
    }

    // [4] Disks & Reclaimable Cache
    println!("\n[4] Storage Health & Reclaimable Caches");
    for d in fixed_drives() {
        let low = d.free_pct < 15.0;
        println!(
            "  {}:  {:>9} free / {:>9} total ({:.1}% free){}",
            d.letter,
            human(d.free),
            human(d.total),
            d.free_pct,
            if low { "  <-- LOW SPACE (<15%)" } else { "" }
        );
        if low {
            score -= 15;
            actions.push((
                1,
                format!("Drive {}: is at {:.1}% free space ({} free)", d.letter, d.free_pct, human(d.free)),
                format!("gp-pc-boost disk scan {}:   (elevated) or `gp-pc-boost tools launch wiztree`", d.letter),
            ));
        }
    }

    let clean_targets = build_clean_targets();
    let safe_bytes: u64 = clean_targets.iter().filter(|t| t.risk == CleanRisk::Safe).map(|t| t.bytes).sum();
    let rebuild_bytes: u64 = clean_targets.iter().filter(|t| t.risk == CleanRisk::Rebuild).map(|t| t.bytes).sum();
    println!(
        "  Reclaimable    : {} Safe + {} Rebuild = {} total",
        human(safe_bytes),
        human(rebuild_bytes),
        human(safe_bytes + rebuild_bytes)
    );
    if safe_bytes + rebuild_bytes >= 1 << 30 {
        score -= 10;
        actions.push((
            1,
            format!("{} of temporary/rebuildable caches found", human(safe_bytes + rebuild_bytes)),
            "gp-pc-boost disk clean-preview  ->  `gp-pc-boost disk clean --safe --apply`".into(),
        ));
    }

    // [5] NTFS & Windows Policy Tuning
    println!("\n[5] NTFS & Background Overhead");
    let mut ntfs_unopt = 0;
    let mut bg_unopt = 0;
    for t in REG_TWEAKS {
        let ok = matches!(query_reg_state(t.key, t.name), RegState::Dword(v) if v == t.desired);
        if !ok {
            if t.group == crate::tune::Group::Ntfs {
                ntfs_unopt += 1;
            } else {
                bg_unopt += 1;
            }
        }
    }
    let mut svc_running = 0;
    for s in SVC_TWEAKS {
        if let Some(mode) = query_svc_mode(s.name)
            && mode != "disabled"
        {
            svc_running += 1;
        }
    }
    println!("  NTFS tweaks pending       : {ntfs_unopt}/2 (LastAccessUpdate, 8.3 short names)");
    println!("  Background policies open  : {bg_unopt}/{} (Telemetry, Copilot, Recall, Edge boost, GameDVR)", REG_TWEAKS.len() - 2);
    println!("  Telemetry services active : {svc_running}/{}", SVC_TWEAKS.len());
    if ntfs_unopt > 0 {
        score -= 8;
        actions.push((
            2,
            "NTFS still updates LastAccess timestamps / 8.3 names".into(),
            "gp-pc-boost tune apply ntfs   (elevated, reversible via `tune undo`)".into(),
        ));
    }
    if bg_unopt > 0 || svc_running > 0 {
        score -= 10;
        actions.push((
            2,
            format!("{bg_unopt} background policies & {svc_running} telemetry services still enabled"),
            "gp-pc-boost tune apply all-safe   (elevated, reversible via `tune undo`)".into(),
        ));
    }

    // [6] Startup & Boot Log
    println!("\n[6] Startup & Boot Diagnostics");
    let startup_items = list_startup();
    let enabled_startup = startup_items.iter().filter(|i| i.enabled).count();
    println!("  Startup entries           : {} enabled / {} total", enabled_startup, startup_items.len());
    let boot = read_boot_log();
    if let Some((date, total_ms, main_ms)) = boot.boots.first() {
        println!(
            "  Last logged boot ({date}) : {:.2}s total ({:.2}s main path)",
            *total_ms as f64 / 1000.0,
            *main_ms as f64 / 1000.0
        );
    } else {
        println!("  Boot performance log      : unavailable without admin (run `gp-pc-boost startup boot-slow` elevated)");
    }

    // [7] Live PDH Telemetry
    if let Some((cpu_perf, disk_act, disk_ms)) = sample_pdh_live() {
        println!("\n[7] Live Telemetry (300ms window)");
        println!("  CPU Performance : {:.1}% of base clock", cpu_perf);
        println!("  Disk Active/Lat : {:.1}% active, {:.2} ms avg transfer", disk_act, disk_ms);
    }

    score = score.clamp(0, 100);
    println!("\n================================================================");
    println!("  PC Speed Score: {score} / 100");
    println!("================================================================");
    if actions.is_empty() {
        println!("  No major bottlenecks detected.");
    } else {
        actions.sort_by_key(|a| a.0);
        println!("  Recommended Next Steps (all reversible):");
        for (idx, (_, issue, cmd)) in actions.iter().enumerate() {
            println!("  {}. {issue}", idx + 1);
            println!("     -> {cmd}");
        }
    }
    0
}
