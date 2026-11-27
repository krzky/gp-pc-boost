use std::{
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
};

const WINRUST_RELEASE: &str = r"D:\Project\rust-goose\WinRust\target\release";

struct ToolEntry {
    cli_name: &'static str,
    exe: &'static str,
    role: &'static str,
    gui: bool,
}

const ALLOWED_WINRUST_TOOLS: &[ToolEntry] = &[
    ToolEntry {
        cli_name: "taskmgr",
        exe: "rtaskmgr-win32.exe",
        role: "WinRust native Task Manager, startup/boot analyzer, DPC/hangs diagnostics",
        gui: true,
    },
    ToolEntry {
        cli_name: "wiztree",
        exe: "wiztree.exe",
        role: "WinRust fast NTFS MFT disk-usage treemap GUI",
        gui: true,
    },
    ToolEntry {
        cli_name: "rs-du",
        exe: "rs-du.exe",
        role: "WinRust fast NTFS MFT disk analyzer & 30-day quarantine cleaner CLI",
        gui: false,
    },
    ToolEntry {
        cli_name: "settings",
        exe: "rust-settings.exe",
        role: "WinRust native power/display/sound/network Settings GUI",
        gui: true,
    },
    ToolEntry {
        cli_name: "debloat",
        exe: "debloat.exe",
        role: "WinRust reversible telemetry/NTFS/DNS CLI (dry-run by default)",
        gui: false,
    },
    ToolEntry {
        cli_name: "photoview",
        exe: "photoview.exe",
        role: "WinRust fast native image viewer",
        gui: true,
    },
];

const BLOCKED_WINRUST_TOOLS: &[(&str, &str)] = &[
    (
        "plugin-launcher.exe",
        "BLOCKED by safety policy (replaces Explorer/Windows Search and adds a SYSTEM updater task)",
    ),
    (
        "purge.exe",
        "BLOCKED by safety policy (irreversible Windows package removal)",
    ),
];

fn tool_version(prog: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(prog)
        .args(args)
        .creation_flags(0x0800_0000)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).lines().next()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}

pub fn run_list() -> i32 {
    println!("WinRust Standalone Tools ({WINRUST_RELEASE}):");
    for t in ALLOWED_WINRUST_TOOLS {
        let p = Path::new(WINRUST_RELEASE).join(t.exe);
        let status = if p.is_file() { "READY" } else { "MISSING" };
        println!("  [{status:<7}] {:<10} ({:<18}) — {}", t.cli_name, t.exe, t.role);
    }
    for (exe, reason) in BLOCKED_WINRUST_TOOLS {
        println!("  [BLOCKED] {:<10} ({:<18}) — {}", "-", exe, reason);
    }

    println!("\nCompanion Compilers & Engines:");
    for (name, bin, flag) in [
        ("rustc", "rustc", "--version"),
        ("cargo", "cargo", "--version"),
        ("goose", "goose", "--version"),
        ("rust-anything", "rust-anything", "--version"),
    ] {
        match tool_version(bin, &[flag]) {
            Some(v) => println!("  [READY  ] {name:<14} {v}"),
            None => println!("  [MISSING] {name:<14} not found on PATH"),
        }
    }
    println!("\nUse `gp-pc-boost tools launch <taskmgr|wiztree|settings|rs-du>` to open a safe standalone WinRust tool.");
    0
}

pub fn run_launch(name: &str, extra: &[String]) -> i32 {
    let q = name.trim().to_ascii_lowercase();
    if q.contains("plugin-launcher") || q.contains("purge") {
        eprintln!("Refusing to launch '{name}': blocked by safety policy (see D:\\Project\\rust-goose\\SECURITY-CHECK.md).");
        return 1;
    }
    let Some(t) = ALLOWED_WINRUST_TOOLS.iter().find(|t| t.cli_name == q || t.exe.eq_ignore_ascii_case(&q)) else {
        eprintln!("Unknown tool '{name}'. Allowed: taskmgr, wiztree, settings, rs-du, debloat, photoview");
        return 1;
    };
    let path: PathBuf = Path::new(WINRUST_RELEASE).join(t.exe);
    if !path.is_file() {
        eprintln!("Binary not found at {}", path.display());
        return 1;
    }
    if t.gui {
        match Command::new(&path).args(extra).spawn() {
            Ok(_) => {
                println!("Launched {} ({})", t.cli_name, path.display());
                0
            }
            Err(e) => {
                eprintln!("Failed to launch {}: {e}", path.display());
                1
            }
        }
    } else {
        match Command::new(&path).args(extra).status() {
            Ok(st) => st.code().unwrap_or(1),
            Err(e) => {
                eprintln!("Failed to run {}: {e}", path.display());
                1
            }
        }
    }
}

pub fn run_profile_guide(args: &[String]) -> i32 {
    if args.is_empty() {
        println!("Accelerate slow local Python/Node/Go scripts into native Rust using `rust-anything`:");
        println!("  1. Profile and port the hottest loop automatically:");
        println!("     rust-anything go -- <your-command>");
        println!("  2. Or pass an AI coding agent:");
        println!("     rust-anything go --agent claude -- <your-command>");
        println!("  3. Revert anytime: rust-anything disable <port-dir>");
        return 0;
    }
    println!("To profile and accelerate this command with rust-anything, run:");
    println!("  rust-anything go -- {}", args.join(" "));
    0
}
