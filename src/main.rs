use gp_pc_boost::{
    audit, backup, bench, bloat, disk, gui, mem, power, profile, replace, report, startup, tools, tune,
};
use std::{env, process::{Command, exit}};

fn print_help() {
    println!("gp-pc-boost — Native Windows Performance Optimizer (WinRust + Goose + rust-anything)");
    println!();
    println!("USAGE:");
    println!("  gp-pc-boost                       Run full read-only PC performance audit");
    println!("  gp-pc-boost ui                    Launch the native desktop GUI window (gp-pc-boost-ui.exe)");
    println!("  gp-pc-boost audit                 Run full read-only PC performance audit");
    println!("  gp-pc-boost report [--out path]   Generate live compatibility & selection REPORT.md");
    println!("  gp-pc-boost backup [create|list]  Snapshot mapped drives, Edge profile, Quick Access & Appx list");
    println!("  gp-pc-boost replace status        Show stock vs WinRust status for all 8 Windows components");
    println!("  gp-pc-boost replace preview [p]   Dry-run preview (safe-all | express | full | component...)");
    println!("  gp-pc-boost replace apply <...>   Install WinRust replacements with automatic pre-change backup");
    println!("  gp-pc-boost replace revert <...>  Restore original Windows components (`all` or component names)");
    println!("  gp-pc-boost bloat preview         List Tier 1 Pure Junk, Tier 2 Work Apps, telemetry tasks, OneDrive/Edge");
    println!("  gp-pc-boost bloat purge [opts]    Irreversible purge (--tier junk|all --onedrive --edge --confirm-irreversible)");
    println!("  gp-pc-boost mem status            Show RAM, Free+Zero, Standby, and Modified lists");
    println!("  gp-pc-boost mem purge             Purge Windows standby list now (requires admin)");
    println!("  gp-pc-boost mem watch [opts]      Foreground ISLC-style standby guard (--interval, --min-free-mb)");
    println!("  gp-pc-boost power list            List Windows power plans");
    println!("  gp-pc-boost power set <target>    Switch power plan (balanced|high|ultimate|index), saves original");
    println!("  gp-pc-boost power restore         Restore saved original power plan");
    println!("  gp-pc-boost disk scan [X:]        Fast NTFS MFT scan via winrust-common (requires admin)");
    println!("  gp-pc-boost disk clean-preview    Read-only drive health & reclaimable cache summary");
    println!("  gp-pc-boost disk clean [opts]     Clean caches (--safe | --rebuild; add --apply to delete)");
    println!("  gp-pc-boost disk rsdu [args...]   Run WinRust rs-du.exe directly");
    println!("  gp-pc-boost startup list          List Run, Startup, Tasks, and 3rd-party auto-start services");
    println!("  gp-pc-boost startup boot-slow     Show Windows Diagnostics-Performance boot times & slow apps");
    println!("  gp-pc-boost startup disable <n>   Reversibly disable a startup item");
    println!("  gp-pc-boost startup enable <n>    Re-enable a startup item");
    println!("  gp-pc-boost tune preview [group]  Dry-run preview (ntfs|telemetry|edge|gamedvr|all-safe)");
    println!("  gp-pc-boost tune status           Show active tweaks and undo journal count");
    println!("  gp-pc-boost tune apply <group>    Apply reversible tweaks (records undo journal first; admin)");
    println!("  gp-pc-boost tune undo             Restore all tweaks from undo journal (admin)");
    println!("  gp-pc-boost profile work|game     Close distracting apps/services & switch power plan");
    println!("  gp-pc-boost profile restore       Restore stopped services, relaunch apps, restore power plan");
    println!("  gp-pc-boost profile status        Show active profile");
    println!("  gp-pc-boost bench                 Run Goose + Rust CPU/RAM/I/O baseline benchmark");
    println!("  gp-pc-boost tools list            Check WinRust, Goose, and rust-anything availability");
    println!("  gp-pc-boost tools launch <name>   Launch safe standalone WinRust tool (taskmgr|wiztree|settings|rs-du)");
    println!("  gp-pc-boost tools profile [cmd]   Show rust-anything workflow to accelerate slow scripts");
}

fn launch_ui() -> i32 {
    if let Ok(cur) = env::current_exe()
        && let Some(dir) = cur.parent()
    {
        let ui_exe = dir.join("gp-pc-boost-ui.exe");
        if ui_exe.is_file() && Command::new(&ui_exe).spawn().is_ok() {
            println!("Launched {}", ui_exe.display());
            return 0;
        }
    }
    gui::run_window()
}

fn main() {
    winrust_common::utf8_console();
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        exit(audit::run_audit());
    }

    let code = match args[0].as_str() {
        "-h" | "--help" | "help" => {
            print_help();
            0
        }
        "ui" | "gui" => launch_ui(),
        "audit" => audit::run_audit(),
        "report" => report::run_report(&args[1..]),
        "backup" => backup::run_backup(&args[1..]),
        "replace" => match args.get(1).map(String::as_str) {
            Some("status") | None => replace::run_status(),
            Some("preview") => replace::run_preview(&args[2..]),
            Some("apply") => replace::run_apply(&args[2..]),
            Some("revert") => replace::run_revert(&args[2..]),
            Some(other) => {
                eprintln!("Unknown `replace` subcommand '{other}'. Expected: status | preview | apply | revert");
                2
            }
        },
        "bloat" => match args.get(1).map(String::as_str) {
            Some("preview") | None => bloat::run_preview(),
            Some("purge") => bloat::run_purge(&args[2..]),
            Some(other) => {
                eprintln!("Unknown `bloat` subcommand '{other}'. Expected: preview | purge");
                2
            }
        },
        "bench" => bench::run_bench(),
        "mem" => match args.get(1).map(String::as_str) {
            Some("status") | None => mem::run_status(),
            Some("purge") => mem::run_purge(),
            Some("watch") => mem::run_watch(&args[2..]),
            Some(other) => {
                eprintln!("Unknown `mem` subcommand '{other}'.");
                2
            }
        },
        "power" => match args.get(1).map(String::as_str) {
            Some("list") | None => power::run_list(),
            Some("set") => match args.get(2) {
                Some(target) => power::run_set(target),
                None => {
                    eprintln!("Usage: gp-pc-boost power set <balanced|high|ultimate|index>");
                    2
                }
            },
            Some("restore") => power::run_restore(),
            Some(other) => {
                eprintln!("Unknown `power` subcommand '{other}'.");
                2
            }
        },
        "disk" => match args.get(1).map(String::as_str) {
            Some("scan") => disk::run_scan(&args[2..]),
            Some("clean-preview") | None => disk::run_clean_preview(),
            Some("clean") => disk::run_clean(&args[2..]),
            Some("rsdu") => disk::run_rsdu(&args[2..]),
            Some(other) => {
                eprintln!("Unknown `disk` subcommand '{other}'.");
                2
            }
        },
        "startup" => match args.get(1).map(String::as_str) {
            Some("list") | None => startup::run_list(),
            Some("boot-slow") => startup::run_boot_slow(),
            Some("disable") => match args.get(2) {
                Some(n) => startup::run_toggle(n, false),
                None => {
                    eprintln!("Usage: gp-pc-boost startup disable <name>");
                    2
                }
            },
            Some("enable") => match args.get(2) {
                Some(n) => startup::run_toggle(n, true),
                None => {
                    eprintln!("Usage: gp-pc-boost startup enable <name>");
                    2
                }
            },
            Some(other) => {
                eprintln!("Unknown `startup` subcommand '{other}'.");
                2
            }
        },
        "tune" => match args.get(1).map(String::as_str) {
            Some("preview") | None => {
                let g = args
                    .get(2)
                    .and_then(|s| tune::Group::parse(s))
                    .unwrap_or(tune::Group::AllSafe);
                tune::run_preview(g)
            }
            Some("status") => tune::run_status(),
            Some("apply") => match args.get(2).and_then(|s| tune::Group::parse(s)) {
                Some(g) => tune::run_apply(g),
                None => {
                    eprintln!("Usage: gp-pc-boost tune apply <ntfs|telemetry|edge|gamedvr|all-safe>");
                    2
                }
            },
            Some("undo") => tune::run_undo(),
            Some(other) => {
                eprintln!("Unknown `tune` subcommand '{other}'.");
                2
            }
        },
        "profile" => match args.get(1).map(String::as_str) {
            Some("status") | None => profile::run_status(),
            Some("restore") => profile::run_restore(),
            Some(kind @ ("work" | "game")) => profile::run_activate(kind),
            Some(other) => {
                eprintln!("Unknown `profile` subcommand '{other}'. Expected: work | game | restore | status");
                2
            }
        },
        "tools" => match args.get(1).map(String::as_str) {
            Some("list") | None => tools::run_list(),
            Some("launch") => match args.get(2) {
                Some(name) => tools::run_launch(name, &args[3..]),
                None => {
                    eprintln!("Usage: gp-pc-boost tools launch <taskmgr|wiztree|settings|rs-du>");
                    2
                }
            },
            Some("profile") => tools::run_profile_guide(&args[2..]),
            Some(other) => {
                eprintln!("Unknown `tools` subcommand '{other}'.");
                2
            }
        },
        other => {
            eprintln!("Unknown command '{other}'. Run `gp-pc-boost --help` for usage.");
            2
        }
    };
    exit(code);
}
