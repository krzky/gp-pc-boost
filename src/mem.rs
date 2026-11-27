use std::{
    ptr::null_mut,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};
use windows_sys::Win32::System::{
    Console::SetConsoleCtrlHandler,
    Performance::*,
    SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
};
use winrust_common::{
    mem::{Lists, lists, purge_standby, should_purge, total_ram},
    proc::elevated,
};

#[derive(Clone, Copy, Debug)]
pub struct MemSnapshot {
    pub total: u64,
    pub avail: u64,
    pub free: Option<u64>,
    pub standby: Option<u64>,
    pub modified: Option<u64>,
    pub commit_load_pct: u32,
    pub from_kernel_lists: bool,
}

pub fn fmt_bytes(b: u64) -> String {
    const GB: f64 = (1u64 << 30) as f64;
    const MB: f64 = (1u64 << 20) as f64;
    if b >= 1u64 << 30 {
        format!("{:.2} GB", b as f64 / GB)
    } else {
        format!("{:.0} MB", b as f64 / MB)
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn pdh_memory_lists() -> Option<(u64, u64, u64)> {
    unsafe {
        let mut q: PDH_HQUERY = null_mut();
        if PdhOpenQueryW(std::ptr::null(), 0, &mut q) != 0 {
            return None;
        }
        let add = |path: &str| -> Option<PDH_HCOUNTER> {
            let mut c: PDH_HCOUNTER = null_mut();
            (PdhAddEnglishCounterW(q, wide(path).as_ptr(), 0, &mut c) == 0).then_some(c)
        };
        let c_free = add(r"\Memory\Free & Zero Page List Bytes");
        let c_sb_res = add(r"\Memory\Standby Cache Reserve Bytes");
        let c_sb_norm = add(r"\Memory\Standby Cache Normal Priority Bytes");
        let c_sb_core = add(r"\Memory\Standby Cache Core Bytes");
        let c_mod = add(r"\Memory\Modified Page List Bytes");
        PdhCollectQueryData(q);
        let val = |c: Option<PDH_HCOUNTER>| -> u64 {
            let Some(c) = c else { return 0 };
            let mut v: PDH_FMT_COUNTERVALUE = std::mem::zeroed();
            if PdhGetFormattedCounterValue(c, PDH_FMT_LARGE, null_mut(), &mut v) == 0 && v.CStatus == 0 {
                v.Anonymous.largeValue.max(0) as u64
            } else {
                0
            }
        };
        let free = val(c_free);
        let standby = val(c_sb_res) + val(c_sb_norm) + val(c_sb_core);
        let modified = val(c_mod);
        PdhCloseQuery(q);
        Some((free, standby, modified))
    }
}

pub fn snapshot() -> MemSnapshot {
    let mut ms: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    ms.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
    unsafe { GlobalMemoryStatusEx(&mut ms) };

    if let Some(l) = lists() {
        let pdh_mod = pdh_memory_lists().map(|x| x.2);
        return MemSnapshot {
            total: ms.ullTotalPhys,
            avail: ms.ullAvailPhys,
            free: Some(l.free),
            standby: Some(l.standby),
            modified: pdh_mod,
            commit_load_pct: ms.dwMemoryLoad,
            from_kernel_lists: true,
        };
    }

    let (free, standby, modified) = match pdh_memory_lists() {
        Some((f, s, m)) => (Some(f), Some(s), Some(m)),
        None => (None, None, None),
    };

    MemSnapshot {
        total: ms.ullTotalPhys,
        avail: ms.ullAvailPhys,
        free,
        standby,
        modified,
        commit_load_pct: ms.dwMemoryLoad,
        from_kernel_lists: false,
    }
}

pub fn run_status() -> i32 {
    let s = snapshot();
    let used = s.total.saturating_sub(s.avail);
    println!("Memory Status:");
    println!("  Total RAM      : {}", fmt_bytes(s.total));
    println!("  In Use         : {} ({}%)", fmt_bytes(used), s.commit_load_pct);
    println!("  Available      : {}", fmt_bytes(s.avail));
    if let (Some(free), Some(standby)) = (s.free, s.standby) {
        println!(
            "  Free + Zero    : {} ({})",
            fmt_bytes(free),
            if s.from_kernel_lists { "NtQuerySystemInformation" } else { "PDH" }
        );
        println!("  Standby Cache  : {}", fmt_bytes(standby));
        if let Some(m) = s.modified {
            println!("  Modified List  : {}", fmt_bytes(m));
        }
        let rec = should_purge(Lists { free, standby }, s.total);
        println!(
            "  Purge Advised  : {}",
            if rec {
                "YES (free < max(512 MB, 5% RAM) and standby > 1 GB)"
            } else {
                "NO (free memory is healthy)"
            }
        );
    } else {
        println!("  Free / Standby : unavailable without administrator or PDH counters");
    }
    0
}

pub fn run_purge() -> i32 {
    if !elevated() {
        eprintln!("Administrator privileges (SeProfileSingleProcessPrivilege) are required to purge the Windows standby list.");
        eprintln!("Run from an elevated terminal: gp-pc-boost mem purge");
        return 1;
    }
    let before = lists();
    match purge_standby() {
        Ok(()) => {
            let after = lists();
            match (before, after) {
                (Some(b), Some(a)) => {
                    let gained = a.free.saturating_sub(b.free);
                    let dropped = b.standby.saturating_sub(a.standby);
                    println!(
                        "Standby cache purged: free {} -> {} (+{}), standby {} -> {} (-{}).",
                        fmt_bytes(b.free),
                        fmt_bytes(a.free),
                        fmt_bytes(gained),
                        fmt_bytes(b.standby),
                        fmt_bytes(a.standby),
                        fmt_bytes(dropped),
                    );
                }
                _ => println!("Standby cache purged."),
            }
            0
        }
        Err(st) => {
            eprintln!("NtSetSystemInformation(MemoryPurgeStandbyList) failed with NTSTATUS 0x{:08X}", st as u32);
            1
        }
    }
}

pub fn run_watch(args: &[String]) -> i32 {
    if !elevated() {
        eprintln!("Administrator privileges are required to monitor and purge standby memory.");
        return 1;
    }
    let mut interval_s: u64 = 10;
    let mut min_free_mb: Option<u64> = None;
    let mut min_standby_mb: u64 = 1024;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--interval" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<u64>().ok()) {
                    interval_s = v.max(2);
                    i += 1;
                }
            }
            "--min-free-mb" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<u64>().ok()) {
                    min_free_mb = Some(v);
                    i += 1;
                }
            }
            "--min-standby-mb" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<u64>().ok()) {
                    min_standby_mb = v;
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    let total = total_ram();
    let free_thresh = min_free_mb
        .map(|mb| mb << 20)
        .unwrap_or_else(|| (512 << 20).max(total / 20));
    let standby_thresh = min_standby_mb << 20;

    static STOP: AtomicBool = AtomicBool::new(false);
    STOP.store(false, Ordering::SeqCst);
    unsafe extern "system" fn ctrl_handler(_ctrl_type: u32) -> i32 {
        STOP.store(true, Ordering::SeqCst);
        1
    }
    unsafe {
        SetConsoleCtrlHandler(Some(ctrl_handler), 1);
    }

    println!(
        "Foreground standby RAM guard active (interval={}s, trigger: free < {} & standby > {}, cooldown=60s). Press Ctrl+C to stop.",
        interval_s,
        fmt_bytes(free_thresh),
        fmt_bytes(standby_thresh)
    );

    let mut last_purge: Option<Instant> = None;
    while !STOP.load(Ordering::Relaxed) {
        if let Some(l) = lists() {
            let cooldown_ok = last_purge.is_none_or(|t| t.elapsed() >= Duration::from_secs(60));
            if cooldown_ok && l.free < free_thresh && l.standby > standby_thresh {
                if purge_standby().is_ok() {
                    let after = lists().map(|x| x.free).unwrap_or(l.free);
                    println!(
                        "[standby-guard] purged standby ({} -> free {})",
                        fmt_bytes(l.standby),
                        fmt_bytes(after)
                    );
                    last_purge = Some(Instant::now());
                }
            }
        }
        thread::sleep(Duration::from_secs(interval_s));
    }
    0
}
