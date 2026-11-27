use crate::journal::state_dir;
use std::{
    env, fs,
    hint::black_box,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Debug)]
pub struct BenchResult {
    pub goose_median_ms: f64,
    pub goose_mode: String,
    pub mem_gb_per_sec: f64,
    pub io_total_ms: f64,
}

fn find_goose_bench() -> (Option<PathBuf>, Option<PathBuf>) {
    let mut roots = vec![];
    if let Ok(cwd) = env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = env::current_exe() {
        for anc in exe.ancestors().take(5) {
            roots.push(anc.to_path_buf());
        }
    }
    roots.push(PathBuf::from(r"D:\Project\gp-pc-boost"));

    let mut exe_found = None;
    let mut src_found = None;
    for r in roots {
        let e = r.join("bench").join("speed_bench.exe");
        if exe_found.is_none() && e.is_file() {
            exe_found = Some(e);
        }
        let s = r.join("bench").join("speed_bench.goose");
        if src_found.is_none() && s.is_file() {
            src_found = Some(s);
        }
    }
    (exe_found, src_found)
}

fn run_goose_once(exe: Option<&Path>, src: Option<&Path>) -> Result<(f64, String, String), String> {
    let t0 = Instant::now();
    let (out, mode) = if let Some(exe_path) = exe {
        let o = Command::new(exe_path)
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|e| format!("failed to run {}: {e}", exe_path.display()))?;
        (o, "native-exe")
    } else if let Some(src_path) = src {
        let o = Command::new("goose")
            .args(["run", &src_path.to_string_lossy()])
            .creation_flags(0x0800_0000)
            .output()
            .map_err(|e| format!("failed to run goose on {}: {e}", src_path.display()))?;
        (o, "goose-run")
    } else {
        return Err("could not locate bench/speed_bench.exe or bench/speed_bench.goose".into());
    };
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    if !out.status.success() {
        return Err(format!(
            "goose benchmark failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((elapsed_ms, stdout, mode.to_string()))
}

fn memory_bandwidth_probe() -> f64 {
    let bytes = 64 * 1024 * 1024usize;
    let mut buf = vec![0u64; bytes / 8];
    let t0 = Instant::now();
    for (i, slot) in buf.iter_mut().enumerate() {
        *slot = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
    let mut acc = 0u64;
    for &v in &buf {
        acc = acc.wrapping_add(v);
    }
    black_box(acc);
    let secs = t0.elapsed().as_secs_f64().max(1e-6);
    // 1 write pass + 1 read pass over 64 MB = 128 MB transferred
    ((bytes * 2) as f64 / (1024.0 * 1024.0 * 1024.0)) / secs
}

fn small_file_io_probe() -> Result<f64, String> {
    let dir = env::temp_dir().join("gp-pc-boost-io-bench");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).map_err(|e| format!("create temp bench dir: {e}"))?;
    let payload = [0x5Au8; 4096];
    let t0 = Instant::now();
    for i in 0..256 {
        let p = dir.join(format!("f_{i}.tmp"));
        fs::write(&p, payload).map_err(|e| e.to_string())?;
    }
    let mut sum = 0usize;
    for i in 0..256 {
        let p = dir.join(format!("f_{i}.tmp"));
        sum += fs::read(&p).map_err(|e| e.to_string())?.len();
    }
    black_box(sum);
    for i in 0..256 {
        let p = dir.join(format!("f_{i}.tmp"));
        let _ = fs::remove_file(&p);
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let _ = fs::remove_dir_all(&dir);
    Ok(elapsed_ms)
}

fn bench_store_dir() -> PathBuf {
    let repo_state = PathBuf::from(r"D:\Project\gp-pc-boost");
    if repo_state.is_dir() {
        repo_state.join("state").join("benchmarks")
    } else {
        state_dir().join("benchmarks")
    }
}

fn load_previous_snapshot(dir: &Path) -> Option<(String, f64, f64, f64)> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    files.sort();
    let latest = files.last()?;
    let text = fs::read_to_string(latest).ok()?;
    let mut g = None;
    let mut m = None;
    let mut io = None;
    for l in text.lines() {
        if let Some(v) = l.strip_prefix("goose_median_ms=") {
            g = v.parse().ok();
        } else if let Some(v) = l.strip_prefix("mem_gb_per_sec=") {
            m = v.parse().ok();
        } else if let Some(v) = l.strip_prefix("io_total_ms=") {
            io = v.parse().ok();
        }
    }
    Some((latest.file_name()?.to_string_lossy().into_owned(), g?, m?, io?))
}

pub fn run_bench_result() -> Result<(BenchResult, Vec<f64>, String, PathBuf), String> {
    let (exe, src) = find_goose_bench();
    let mut goose_times = vec![];
    let mut goose_out = String::new();
    let mut goose_mode = String::new();
    for run_idx in 0..3 {
        let (ms, out, mode) = run_goose_once(exe.as_deref(), src.as_deref())?;
        if run_idx == 0 {
            goose_out = out;
            goose_mode = mode;
        } else if out != goose_out {
            return Err(format!("Goose benchmark output mismatch: '{out}' vs '{goose_out}'"));
        }
        goose_times.push(ms);
    }
    let mut sorted = goose_times.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let goose_median_ms = sorted[1];

    let mem_gb_per_sec = memory_bandwidth_probe();
    let io_total_ms = small_file_io_probe()?;

    let res = BenchResult {
        goose_median_ms,
        goose_mode,
        mem_gb_per_sec,
        io_total_ms,
    };

    let dir = bench_store_dir();
    let _ = fs::create_dir_all(&dir);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let snap = dir.join(format!("bench_{ts}.txt"));
    let body = format!(
        "timestamp={ts}\ngoose_mode={}\ngoose_median_ms={:.3}\nmem_gb_per_sec={:.3}\nio_total_ms={:.3}\ngoose_output={goose_out}\n",
        res.goose_mode, res.goose_median_ms, res.mem_gb_per_sec, res.io_total_ms
    );
    let _ = fs::write(&snap, body);
    Ok((res, goose_times, goose_out, snap))
}

pub fn run_bench() -> i32 {
    println!("Running gp-pc-boost Native Benchmark Suite (Goose + Rust)...\n");
    let dir = bench_store_dir();
    let prev = load_previous_snapshot(&dir);

    let (res, goose_times, goose_out, snap) = match run_bench_result() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Benchmark error: {e}");
            return 1;
        }
    };

    println!(
        "  [1] Goose Stack-Arena & Scan ({}): {:.1} ms median (runs: {:.1}, {:.1}, {:.1} ms)",
        res.goose_mode, res.goose_median_ms, goose_times[0], goose_times[1], goose_times[2]
    );
    println!("      Output: {goose_out}");
    println!("  [2] Rust 64 MB Memory Stream (R+W): {:.2} GB/s", res.mem_gb_per_sec);
    println!("  [3] Rust 256x4 KB Small-File I/O   : {:.1} ms", res.io_total_ms);

    if let Some((prev_name, pg, pm, pio)) = prev {
        println!("\nComparison vs previous baseline ({prev_name}):");
        println!(
            "  Goose CPU/Arena : {:+.1} ms ({:+.1}%)",
            res.goose_median_ms - pg,
            ((res.goose_median_ms - pg) / pg) * 100.0
        );
        println!(
            "  Memory Stream   : {:+.2} GB/s ({:+.1}%)",
            res.mem_gb_per_sec - pm,
            ((res.mem_gb_per_sec - pm) / pm) * 100.0
        );
        println!(
            "  Small-File I/O  : {:+.1} ms ({:+.1}%)",
            res.io_total_ms - pio,
            ((res.io_total_ms - pio) / pio) * 100.0
        );
    }

    if snap.is_file() {
        println!("\nSaved benchmark snapshot: {}", snap.display());
    }
    0
}
