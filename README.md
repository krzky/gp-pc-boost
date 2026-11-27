# gp-pc-boost

Native Windows PC performance audit, WinRust component replacer, bloatware inspector, and benchmark suite built on **WinRust** (`winrust-common` + `payload/`), **Goose v2026.4** (`bench/speed_bench.goose`), and **rust-anything**.

## Design & Safety Boundaries

- **Never runs `plugin-launcher.exe`**: Avoids the `WinRust-update` SYSTEM scheduled task while still using the locally audited WinRust binaries directly.
- **Sophos-safe**: Never modifies Windows Defender exclusions.
- **Pre-change safety backups**: Every `replace apply` and `bloat purge` automatically snapshots `net use`, `HKCU\Network`, Edge `Default` profile files, Quick Access jump lists, and the installed Appx list into `%LOCALAPPDATA%\gp-pc-boost\backups\<timestamp>`.
- **Full rollback**: Every WinRust component replacement is recorded in `%LOCALAPPDATA%\gp-pc-boost\replace_journal.log` and cleanly restored in reverse order via `gp-pc-boost replace revert all` or `scripts\rollback-all.ps1`.

## Build

```powershell
cd D:\Project\gp-pc-boost
goose build bench\speed_bench.goose
cargo test --offline
cargo build --release --offline
```

## Desktop App UI

Launch the native dark-mode Win32/GDI+ GUI (shows live lists for WinRust replacements, tiered bloatware, drives/caches, startup items, and mapped drives):

```powershell
D:\Project\gp-pc-boost\target\release\gp-pc-boost-ui.exe
# or:
D:\Project\gp-pc-boost\target\release\gp-pc-boost.exe ui
# or double-click:
D:\Project\gp-pc-boost\Launch-UI.bat
```

## Quick Commands

### Read-Only Inspection & Compatibility Report (No Admin Required)
```powershell
.\target\release\gp-pc-boost.exe audit
.\target\release\gp-pc-boost.exe report
.\target\release\gp-pc-boost.exe replace status
.\target\release\gp-pc-boost.exe replace preview full
.\target\release\gp-pc-boost.exe bloat preview
.\target\release\gp-pc-boost.exe bench
.\target\release\gp-pc-boost.exe disk clean-preview
.\target\release\gp-pc-boost.exe tune preview
```

### 1-Click Self-Elevating Presets (`scripts\`)
```powershell
# Recommended: replaces taskmgr, settings, photoview, wiztree + debloat + power + mem purge (keeps Explorer, Search, WebView2, N:-Z:)
powershell -ExecutionPolicy Bypass -File D:\Project\gp-pc-boost\scripts\apply-safe-all.ps1

# Also replaces Taskbar (topbar) and Search (Alt+Space), keeps Explorer shell:
powershell -ExecutionPolicy Bypass -File D:\Project\gp-pc-boost\scripts\apply-express.ps1

# Full WinRust stack including rust-explorer shell (prompts for YES):
powershell -ExecutionPolicy Bypass -File D:\Project\gp-pc-boost\scripts\apply-full-winrust.ps1

# Full 1-click rollback (restores stock Explorer, Taskbar, Search, TaskMgr, services, power plan):
powershell -ExecutionPolicy Bypass -File D:\Project\gp-pc-boost\scripts\rollback-all.ps1
```
