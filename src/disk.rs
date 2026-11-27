use crate::journal::state_dir;
use std::{
    cmp::Reverse,
    env,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};
use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives};
use winrust_common::{
    mft::{ROOT, human, path_of, scan, totals},
    proc::elevated,
};

const DRIVE_FIXED: u32 = 3;

#[derive(Clone, Debug)]
pub struct DriveInfo {
    pub letter: char,
    pub free: u64,
    pub total: u64,
    pub free_pct: f64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CleanRisk {
    Safe,
    Rebuild,
}

#[derive(Clone, Debug)]
pub struct CleanTarget {
    pub risk: CleanRisk,
    pub label: &'static str,
    pub note: &'static str,
    pub paths: Vec<PathBuf>,
    pub bytes: u64,
}

pub fn fixed_drives() -> Vec<DriveInfo> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| (mask >> i) & 1 == 1)
        .filter_map(|i| {
            let letter = (b'A' + i) as char;
            let root: Vec<u16> = format!("{letter}:\\").encode_utf16().chain([0]).collect();
            if unsafe { GetDriveTypeW(root.as_ptr()) } != DRIVE_FIXED {
                return None;
            }
            let (mut free, mut total, mut _avail_all) = (0u64, 0u64, 0u64);
            if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free, &mut total, &mut _avail_all) } == 0 {
                return None;
            }
            let free_pct = if total > 0 { (free as f64 / total as f64) * 100.0 } else { 0.0 };
            Some(DriveInfo {
                letter,
                free,
                total,
                free_pct,
            })
        })
        .collect()
}

pub fn dir_size(p: &Path) -> u64 {
    if !p.exists() {
        return 0;
    }
    if p.is_file() {
        return p.metadata().map_or(0, |m| m.len());
    }
    let Ok(rd) = fs::read_dir(p) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_symlink() => 0,
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            _ => e.metadata().map_or(0, |m| m.len()),
        })
        .sum()
}

/// Empties directory contents without removing the root directory itself and without following symlinks.
pub fn wipe_dir_contents(p: &Path) {
    let Ok(rd) = fs::read_dir(p) else {
        return;
    };
    for e in rd.flatten() {
        let Ok(ft) = e.file_type() else {
            continue;
        };
        if ft.is_symlink() {
            continue;
        }
        let child = e.path();
        if ft.is_dir() {
            let _ = fs::remove_dir_all(&child);
        } else {
            let _ = fs::remove_file(&child);
        }
    }
}

fn subdirs(base: &Path, pred: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    fs::read_dir(base)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink())
                && pred(&e.file_name().to_string_lossy())
        })
        .map(|e| e.path())
        .collect()
}

pub fn build_clean_targets() -> Vec<CleanTarget> {
    let var = |k: &str| env::var_os(k).map(PathBuf::from);
    let (local, roam, home, win, pdata) = (
        var("LOCALAPPDATA"),
        var("APPDATA"),
        var("USERPROFILE"),
        var("SystemRoot"),
        var("ProgramData"),
    );
    let at = |b: &Option<PathBuf>, rel: &[&str]| -> Vec<PathBuf> {
        b.iter()
            .flat_map(|b| rel.iter().map(move |r| b.join(r)))
            .collect()
    };

    let mut targets = vec![];
    let mut add = |risk, label, note, paths: Vec<PathBuf>| {
        let existing: Vec<PathBuf> = paths.into_iter().filter(|p| p.exists()).collect();
        if existing.is_empty() {
            return;
        }
        let bytes: u64 = existing.iter().map(|p| dir_size(p)).sum();
        if bytes >= 1 << 20 {
            targets.push(CleanTarget {
                risk,
                label,
                note,
                paths: existing,
                bytes,
            });
        }
    };

    add(
        CleanRisk::Safe,
        "Temporary files (%TEMP%, Windows\\Temp)",
        "Leftover installer and app temp files; safe to remove.",
        [var("TEMP").into_iter().collect::<Vec<_>>(), at(&win, &["Temp"])].concat(),
    );
    add(
        CleanRisk::Safe,
        "Error reports & crash dumps",
        "WER archives, CrashDumps, and Minidump diagnostics.",
        [
            at(&pdata, &["Microsoft/Windows/WER/ReportArchive", "Microsoft/Windows/WER/ReportQueue"]),
            at(&local, &["CrashDumps"]),
            at(&win, &["Minidump"]),
        ]
        .concat(),
    );
    add(
        CleanRisk::Safe,
        "Windows Update & Delivery Optimization cache",
        "Already-installed Windows Update payloads (needs admin for system dirs).",
        at(
            &win,
            &[
                "SoftwareDistribution/Download",
                "ServiceProfiles/NetworkService/AppData/Local/Microsoft/Windows/DeliveryOptimization/Cache",
            ],
        ),
    );
    add(
        CleanRisk::Safe,
        "Windows INetCache",
        "Temporary internet cache for system components.",
        at(&local, &["Microsoft/Windows/INetCache"]),
    );

    for (name, rel) in [
        ("Google Chrome cache", "Google/Chrome/User Data"),
        ("Microsoft Edge cache", "Microsoft/Edge/User Data"),
        ("Brave cache", "BraveSoftware/Brave-Browser/User Data"),
        ("GP Browser cache", "GemaPro/GPBrowser/User Data"),
    ] {
        if let Some(base) = local.as_ref().map(|l| l.join(rel))
            && base.is_dir()
        {
            let ps = subdirs(&base, |n| n == "Default" || n.starts_with("Profile "))
                .into_iter()
                .flat_map(|p| {
                    ["Cache/Cache_Data", "Code Cache", "GPUCache", "Service Worker/CacheStorage"]
                        .map(|c| p.join(c))
                })
                .collect();
            add(
                CleanRisk::Rebuild,
                name,
                "Cached web assets only; passwords, history, and bookmarks are untouched.",
                ps,
            );
        }
    }

    if let Some(ff) = local.as_ref().map(|l| l.join("Mozilla/Firefox/Profiles"))
        && ff.is_dir()
    {
        let ps = subdirs(&ff, |_| true).into_iter().map(|p| p.join("cache2")).collect();
        add(
            CleanRisk::Rebuild,
            "Firefox cache",
            "Cached web assets only; profiles and passwords stay intact.",
            ps,
        );
    }

    add(
        CleanRisk::Rebuild,
        "Visual Studio Code cache",
        "Editor cache & cached V8 bytecode; recreated automatically.",
        at(&roam, &["Code/Cache", "Code/CachedData", "Code/GPUCache", "Code/Code Cache"]),
    );
    add(
        CleanRisk::Rebuild,
        "Discord / Teams / Telegram media caches",
        "Cached chat media & electron UI caches; re-downloaded on demand.",
        [
            at(&roam, &["discord/Cache", "discord/Code Cache", "discord/GPUCache"]),
            at(&roam, &["Microsoft/Teams/Cache", "Microsoft/Teams/blob_storage", "Microsoft/Teams/GPUCache", "Microsoft/Teams/tmp"]),
            at(&roam, &["Telegram Desktop/tdata/user_data/media_cache", "Telegram Desktop/tdata/user_data/cache"]),
        ]
        .concat(),
    );
    add(
        CleanRisk::Rebuild,
        "GPU shader caches (NVIDIA, AMD, DirectX)",
        "Compiled shaders; first game/3D launch rebuilds them.",
        at(&local, &["NVIDIA/DXCache", "NVIDIA/GLCache", "D3DSCache", "AMD/DxCache", "AMD/DxcCache"]),
    );
    add(
        CleanRisk::Rebuild,
        "Steam browser cache",
        "Steam embedded browser htmlcache (games untouched).",
        at(&local, &["Steam/htmlcache"]),
    );
    add(
        CleanRisk::Rebuild,
        "Developer package caches (cargo, npm, pip, NuGet)",
        "Downloaded package tarballs/wheels; re-fetched on next build if needed.",
        [
            at(&home, &[".cargo/registry/cache", ".cargo/registry/src"]),
            at(&local, &["npm-cache", "pip/cache", "NuGet/v3-cache"]),
            at(&roam, &["npm-cache"]),
        ]
        .concat(),
    );

    targets.sort_by_key(|t| (t.risk as u8, Reverse(t.bytes)));
    targets
}

pub fn run_scan(args: &[String]) -> i32 {
    let mut drive = 'C';
    let mut top = 20usize;
    let mut depth = 2usize;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a.len() == 2 && a.ends_with(':') {
            drive = a.chars().next().unwrap_or('C').to_ascii_uppercase();
        } else if a == "--top" {
            if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                top = v;
                i += 1;
            }
        } else if a == "--depth" {
            if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                depth = v;
                i += 1;
            }
        }
        i += 1;
    }

    if !elevated() {
        eprintln!("NTFS MFT raw scan requires administrator privileges.");
        eprintln!("Run from an elevated terminal: gp-pc-boost disk scan {drive}:");
        return 1;
    }

    let t = std::time::Instant::now();
    let recs = match scan(drive) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("MFT scan error: {e}");
            return 1;
        }
    };
    let tot = totals(&recs, false);
    let mut dirs: Vec<(u32, u64)> = vec![];
    let mut files: Vec<(u32, u64)> = vec![];
    for (&id, &sz) in &tot {
        match recs.get(&id) {
            Some(r) if r.dir && id != ROOT => dirs.push((id, sz)),
            Some(_) if id != ROOT => files.push((id, sz)),
            _ => {}
        }
    }
    dirs.retain(|&(id, _)| path_of(&recs, id, drive).1 <= depth);
    dirs.sort_by_key(|&(_, s)| Reverse(s));
    files.sort_by_key(|&(_, s)| Reverse(s));

    println!(
        "NTFS MFT Scan ({drive}:) — {} records, {} allocated, {:.2}s",
        recs.len(),
        human(tot.get(&ROOT).copied().unwrap_or(0)),
        t.elapsed().as_secs_f32()
    );
    println!("\nTop Folders (depth <= {depth}):");
    for &(id, s) in dirs.iter().take(top) {
        println!("  {:>10}  {}", human(s), path_of(&recs, id, drive).0);
    }
    println!("\nTop Files:");
    for &(id, s) in files.iter().take(top) {
        println!("  {:>10}  {}", human(s), path_of(&recs, id, drive).0);
    }
    0
}

pub fn run_clean_preview() -> i32 {
    println!("Fixed Drives:");
    for d in fixed_drives() {
        let warn = if d.free_pct < 15.0 { " [LOW FREE SPACE < 15%]" } else { "" };
        println!(
            "  {}:  {:>9} free / {:>9} total ({:.1}% free){warn}",
            d.letter,
            human(d.free),
            human(d.total),
            d.free_pct
        );
    }

    let targets = build_clean_targets();
    let safe_bytes: u64 = targets
        .iter()
        .filter(|t| t.risk == CleanRisk::Safe)
        .map(|t| t.bytes)
        .sum();
    let rebuild_bytes: u64 = targets
        .iter()
        .filter(|t| t.risk == CleanRisk::Rebuild)
        .map(|t| t.bytes)
        .sum();

    println!("\nReclaimable Cache & Temp Preview (read-only):");
    for t in &targets {
        let tag = match t.risk {
            CleanRisk::Safe => "SAFE   ",
            CleanRisk::Rebuild => "REBUILD",
        };
        println!("  [{tag}] {:>9}  {} — {}", human(t.bytes), t.label, t.note);
    }
    println!(
        "\nSummary: {} Safe, {} Rebuild ({} total).",
        human(safe_bytes),
        human(rebuild_bytes),
        human(safe_bytes + rebuild_bytes)
    );
    println!("Nothing deleted. Use `gp-pc-boost disk clean --safe --apply` or `--rebuild --apply` to clean.");
    0
}

pub fn clean_selected_targets(targets: &[CleanTarget]) -> (u64, PathBuf) {
    let dir = state_dir();
    let _ = fs::create_dir_all(&dir);
    let log_path = dir.join("clean-log.txt");
    let mut log = File::create(&log_path).ok();
    let mut freed_total = 0u64;

    for t in targets {
        let mut left = 0u64;
        for p in &t.paths {
            if let Some(f) = log.as_mut() {
                let _ = writeln!(f, "{}\t{}", t.label, p.display());
            }
            if p.is_dir() {
                wipe_dir_contents(p);
                left += dir_size(p);
            }
        }
        let freed = t.bytes.saturating_sub(left);
        freed_total += freed;
        println!(
            "  Cleaned {:>9}  {} (in-use/locked remaining: {})",
            human(freed),
            t.label,
            human(left)
        );
    }
    (freed_total, log_path)
}

pub fn run_clean(args: &[String]) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    let include_rebuild = args.iter().any(|a| a == "--rebuild" || a == "--all");
    let _safe_flag = args.iter().any(|a| a == "--safe");

    let targets: Vec<CleanTarget> = build_clean_targets()
        .into_iter()
        .filter(|t| t.risk == CleanRisk::Safe || include_rebuild)
        .collect();

    if targets.is_empty() {
        println!("No cleanable cache directories >= 1 MB found.");
        return 0;
    }

    let total: u64 = targets.iter().map(|t| t.bytes).sum();
    if !apply {
        println!("Dry run (pass --apply to delete files in these locations):");
        for t in &targets {
            println!("  {:>9}  {}", human(t.bytes), t.label);
        }
        println!("Total eligible: {}", human(total));
        return 0;
    }

    let (freed_total, log_path) = clean_selected_targets(&targets);
    println!(
        "\nDisk cleanup complete: {} freed. Log: {}",
        human(freed_total),
        log_path.display()
    );
    0
}

pub fn run_rsdu(args: &[String]) -> i32 {
    let rsdu = Path::new(r"D:\Project\rust-goose\WinRust\target\release\rs-du.exe");
    if !rsdu.is_file() {
        eprintln!("WinRust rs-du.exe not found at {}", rsdu.display());
        return 1;
    }
    match Command::new(rsdu).args(args).status() {
        Ok(st) => st.code().unwrap_or(1),
        Err(e) => {
            eprintln!("Failed to launch rs-du.exe: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_wipe_empties_dir_and_keeps_root() {
        let d = env::temp_dir().join("gp-pc-boost-wipe-test");
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("a.tmp"), [1u8; 128]).unwrap();
        fs::write(d.join("sub/b.tmp"), [2u8; 64]).unwrap();
        assert_eq!(dir_size(&d), 192);
        wipe_dir_contents(&d);
        assert!(d.is_dir());
        assert_eq!(dir_size(&d), 0);
        let _ = fs::remove_dir(&d);
    }
}
