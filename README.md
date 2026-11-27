# gp-pc-boost

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

Native **Windows 12 Fluent 2 multi-monitor taskbar (`gp-taskbar`)** and Windows PC performance/audit toolkit written in Rust & Win32/DWM — zero Electron/WebView runtime overhead, single standalone native executable.

## Features (`gp-taskbar`)

- **Three Fluent 2 Layouts**: `Win12 Split` (floating app dock + floating status/clock/tray modules), `Unified Top`, and `Unified Bottom`.
- **Hardware-VSync Spring Animations**: 60–240 Hz `DwmFlush()`-paced critically-damped springs for bar reveals, auto-hide glides, flyouts, and search morphing (0% idle CPU).
- **Native Windows Fullscreen Gaming Mode**: Automatically suppresses all taskbar bars, popups, tooltips, and edge sensors per-monitor during exclusive or borderless fullscreen games (while ignoring normal maximized desktop windows). Pressing the **Windows key** immediately reveals the taskbar over anything.
- **Built-in Native Search**: Instant app/settings/calculator/web search bar with smooth pill morph and zero focus-steal issues.
- **Multi-Monitor & Live DWM Previews**: Per-monitor filtering, live thumbnail hover previews, drag-to-reorder pinned apps, system tray overflow flyout, CPU/RAM/battery telemetry, and system accent/dark/light/glass theming.
- **100% Reversible**: Right-click → **Restore Stock Taskbar** or run `Uninstall-Taskbar.bat` to restore the default Windows taskbar cleanly at any time.

## Install (Windows 10 / 11 x64)

### Option 1 — Quick Install from Releases (Recommended)
1. Download **`gp-pc-boost-v0.1.0-windows-x64.zip`** from the [Releases](https://github.com/krzky/gp-pc-boost/releases) page.
2. Extract the zip anywhere and double-click **`Install-Taskbar.bat`**.
   - Installs `gp-taskbar.exe` (plus `gp-pc-boost-ui.exe` and `gp-pc-boost.exe`) into `%LOCALAPPDATA%\gp-pc-boost\bin`.
   - Registers automatic startup on login (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run\gp-taskbar`) — no administrator rights required.
   - Launches `gp-taskbar.exe` immediately.

### Option 2 — Portable Zero-Install
- Download **`gp-taskbar-v0.1.0-windows-x64.exe`** from [Releases](https://github.com/krzky/gp-pc-boost/releases) and double-click it directly.

### Uninstall / Restore Stock Windows Taskbar
- Double-click **`Uninstall-Taskbar.bat`** (or run `gp-taskbar.exe --restore` to restore the stock Windows taskbar without uninstalling).

## Configuration

Settings are stored in `%APPDATA%\gp-pc-boost\taskbar.ini` and can be toggled live from the taskbar's **Settings** gear button or right-click menu:
- Taskbar Size (`Compact`, `Small`, `Standard`, `Large`)
- Layout (`Win12 Split`, `Unified Top`, `Unified Bottom`) & Dock Edge (`Top`, `Bottom`)
- Theme (`System`, `Dark`, `Light`) & Glass Style (`System`, `Frosted Glass`, `Solid`)
- Auto-Hide Dock, Status Strip (`Hover Edge` / `Always On`), and **Gaming Mode** (`Fullscreen` / `Off`)
- Tray icon density, hover previews, clock seconds, Search, Task View, CPU/RAM telemetry, Show Desktop strip

## Build from Source

Requires Rust (2024 edition) on Windows x64:

```powershell
cargo test --lib
cargo build --release --bin gp-taskbar
# Optional full suite (requires local winrust-common workspace):
cargo build --release
```

## Desktop Boost UI & CLI

```powershell
# Launch native dark-mode Win32 desktop UI:
.\gp-pc-boost-ui.exe

# Read-only CLI system inspection:
.\gp-pc-boost.exe audit
.\gp-pc-boost.exe report
.\gp-pc-boost.exe disk clean-preview
.\gp-pc-boost.exe tune preview
```

## Credits & Upstream Projects

This project would not exist without the work of these open-source authors:

- **[WinRust](https://github.com/Lemxy/WinRust)** by **Lemxy** (MIT / Apache-2.0) — foundational `winrust-common` crate and native Windows component replacement architecture.
- **[rust-anything](https://github.com/erkamyaman/rust-anything)** by **Erkam Yaman (`erkamyaman`)** (MIT) — fast local NTFS file indexing & search inspiration.
- **[Coucou](https://github.com/Louis-CFM/coucou)** by **Louis Raillé (`Louis-CFM`)** (MIT source; brand/character assets reserved) — optional assistant island integration and spring motion inspiration.
- **[Goose](https://github.com/aardappel/goose)** by **Wouter van Oortmerssen (`aardappel`)** (Apache-2.0) — benchmark script language (`bench/speed_bench.goose`).
- **[windows-rs / windows-sys](https://github.com/microsoft/windows-rs)** by **Microsoft** (MIT / Apache-2.0) — Rust Win32 & DWM bindings.

## License

Licensed under the [MIT License](LICENSE). Full upstream license texts and copyright notices are preserved in [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).
