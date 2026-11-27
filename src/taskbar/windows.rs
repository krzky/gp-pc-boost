//! Multi-monitor enumeration, per-monitor window filtering, Taskband + MSIX pins, drag order & native actions.

use crate::taskbar::{
    config::DockSize,
    icons::{
        find_windowsapps_root, msix_manifest_info, parse_lnk_info, resolve_msix_packages,
    },
};
use std::{
    collections::{HashMap, HashSet},
    env, fs,
    mem::{size_of, zeroed},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    ptr::{null, null_mut},
};
use windows_sys::Win32::{
    Foundation::*,
    Globalization::GetLocaleInfoW,
    Graphics::{
        Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        Gdi::{
            EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST,
            MONITORINFO, MonitorFromWindow,
        },
    },
    System::{
        Registry::{HKEY_CURRENT_USER, RRF_RT_REG_BINARY, RegGetValueW},
        Threading::{
            AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
    },
    UI::{
        HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
        Input::KeyboardAndMouse::{
            GetKeyboardLayout, GetKeyboardLayoutList, HKL, INPUT, INPUT_0, INPUT_KEYBOARD,
            KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VK_MENU,
        },
        Shell::ShellExecuteW,
        WindowsAndMessaging::*,
    },
};

#[derive(Clone, Copy)]
pub struct MonitorSpec {
    pub hmon: isize,
    pub rect: RECT,
    pub primary: bool,
    pub scale: f32,
}

pub unsafe fn enumerate_monitors() -> Vec<MonitorSpec> {
    let mut out: Vec<MonitorSpec> = Vec::new();
    EnumDisplayMonitors(
        null_mut(),
        null(),
        Some(enum_mon_proc),
        &mut out as *mut Vec<MonitorSpec> as LPARAM,
    );
    out.sort_by_key(|m| (!m.primary, m.rect.left, m.rect.top));
    out
}

unsafe extern "system" fn enum_mon_proc(
    hmon: HMONITOR,
    _: HDC,
    _: *mut RECT,
    lp: LPARAM,
) -> i32 {
    let out = &mut *(lp as *mut Vec<MonitorSpec>);
    let mut mi: MONITORINFO = zeroed();
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(hmon, &mut mi) != 0 {
        let mut dpix = 96u32;
        let mut dpiy = 96u32;
        let _ = GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dpix, &mut dpiy);
        out.push(MonitorSpec {
            hmon: hmon as isize,
            rect: mi.rcMonitor,
            primary: (mi.dwFlags & 1) != 0,
            scale: (dpix as f32 / 96.0).max(1.0),
        });
    }
    1
}

#[derive(Clone, Debug)]
pub struct PinnedApp {
    pub key: String,
    pub lnk_path: Option<String>,
    pub aumid: Option<String>,
    pub package_root: Option<String>, // lowercase WindowsApps root for matching live windows
    pub target_exe: String,           // lowercase target exe for Win32 .lnk pins
    pub icon_path: String,
    pub label: String,
}

#[derive(Clone)]
pub struct AppItem {
    pub pin_lnk: Option<String>,
    pub pin_aumid: Option<String>,
    pub icon_key: String,
    pub icon_path: String,
    pub label: String,
    pub wins: Vec<HWND>,
    pub win_mons: Vec<isize>,
    /// Most-recently-opened or most-recently-focused window of this app (used by default click).
    pub mru_win: HWND,
    pub active: bool,
    pub minimized: bool,
    pub flashing: bool,
}

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn read_taskband_favorites() -> Option<Vec<u8>> {
    unsafe {
        let sub = wide(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Taskband");
        let val = wide("Favorites");
        let mut cb = 0u32;
        if RegGetValueW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_BINARY,
            null_mut(),
            null_mut(),
            &mut cb,
        ) != 0
            || cb == 0
        {
            return None;
        }
        let mut buf = vec![0u8; cb as usize];
        if RegGetValueW(
            HKEY_CURRENT_USER,
            sub.as_ptr(),
            val.as_ptr(),
            RRF_RT_REG_BINARY,
            null_mut(),
            buf.as_mut_ptr() as *mut _,
            &mut cb,
        ) != 0
        {
            return None;
        }
        buf.truncate(cb as usize);
        Some(buf)
    }
}

/// Extracts ordered UTF-16LE tokens (>= 5 printable ASCII chars) from Windows 11 `Taskband\Favorites`.
pub fn extract_taskband_utf16_tokens(bytes: &[u8]) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut cur = String::new();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        let u = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        if (0x20..=0x7E).contains(&u) {
            cur.push(u as u8 as char);
            i += 2;
        } else {
            if cur.len() >= 5 {
                tokens.push(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
            i += 1;
        }
    }
    if cur.len() >= 5 {
        tokens.push(cur);
    }
    tokens
}

fn looks_like_aumid(s: &str) -> Option<(String, String)> {
    let (fam, app_id) = s.split_once('!')?;
    if fam.is_empty() || app_id.is_empty() || fam.contains('\\') || fam.contains(' ') || app_id.contains(' ') {
        return None;
    }
    let (_, pub_id) = fam.rsplit_once('_')?;
    if pub_id.len() != 13 || !pub_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some((fam.to_string(), app_id.to_string()))
}

fn make_lnk_pin(path: &Path) -> PinnedApp {
    let label = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    let lnk_str = path.to_string_lossy().into_owned();
    let target_exe = fs::read(path)
        .ok()
        .and_then(|b| parse_lnk_info(&b))
        .and_then(|info| info.target_exe)
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    let package_root = if !target_exe.is_empty() {
        find_windowsapps_root(&target_exe).map(|p| p.to_string_lossy().to_ascii_lowercase())
    } else {
        None
    };
    PinnedApp {
        key: format!("pin:{}", lnk_str.to_ascii_lowercase()),
        lnk_path: Some(lnk_str.clone()),
        aumid: None,
        package_root,
        target_exe,
        icon_path: lnk_str,
        label,
    }
}

/// Loads pinned taskbar apps in exact Windows 11 `Taskband\Favorites` order (both `.lnk` & MSIX AUMIDs).
pub fn load_pins() -> Vec<PinnedApp> {
    let mut lnk_files: Vec<PathBuf> = env::var_os("APPDATA")
        .map(|a| PathBuf::from(a).join(r"Microsoft\Internet Explorer\Quick Launch\User Pinned\TaskBar"))
        .into_iter()
        .flat_map(|dir| fs::read_dir(dir).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("lnk")))
        .collect();
    lnk_files.sort();

    let mut lnk_by_name: Vec<(String, PathBuf)> = lnk_files
        .iter()
        .filter_map(|p| Some((p.file_name()?.to_string_lossy().to_ascii_lowercase(), p.clone())))
        .collect();
    // Match longer filenames first so `RouterOS management tool (2).lnk` doesn't lose to shorter suffix
    lnk_by_name.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    let msix_pkgs = unsafe { resolve_msix_packages() };
    let mut pins = Vec::new();
    let mut seen_keys = HashSet::<String>::new();
    let mut used_lnks = HashSet::<String>::new();

    if let Some(fav_bytes) = read_taskband_favorites() {
        for tok in extract_taskband_utf16_tokens(&fav_bytes) {
            let t_low = tok.to_ascii_lowercase();
            // 1. Match .lnk pins from User Pinned\TaskBar (handles optional 1-char binary prefix before name)
            if t_low.ends_with(".lnk") {
                if let Some((fname, path)) = lnk_by_name.iter().find(|(fnm, _)| t_low.ends_with(fnm.as_str())) {
                    if used_lnks.insert(fname.clone()) {
                        let pin = make_lnk_pin(path);
                        if seen_keys.insert(pin.key.clone()) {
                            pins.push(pin);
                        }
                    }
                    continue;
                }
            }
            // 2. Match installed MSIX AUMID pins (e.g. Claude_pzs8sxrjxfjjc!Claude, WhatsApp Beta)
            if let Some((fam, app_id)) = looks_like_aumid(&tok) {
                let fam_low = fam.to_ascii_lowercase();
                if let Some(root) = msix_pkgs.get(&fam_low) {
                    let aumid = format!("{fam}!{app_id}");
                    let key = format!("aumid:{}", aumid.to_ascii_lowercase());
                    if seen_keys.insert(key.clone()) {
                        let msix_info = msix_manifest_info(root, Some(&app_id));
                        let label = msix_info
                            .as_ref()
                            .and_then(|i| i.display_name.clone())
                            .unwrap_or_else(|| {
                                let short = fam.split('_').next().unwrap_or(&fam);
                                short.rsplit('.').next().unwrap_or(short).to_string()
                            });
                        let icon_path = msix_info
                            .and_then(|i| i.logo_path)
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        pins.push(PinnedApp {
                            key,
                            lnk_path: None,
                            aumid: Some(aumid),
                            package_root: Some(root.to_string_lossy().to_ascii_lowercase()),
                            target_exe: String::new(),
                            icon_path,
                            label,
                        });
                    }
                }
            }
        }
    }

    // Append any remaining .lnk files not referenced in Taskband\Favorites
    for path in lnk_files {
        let fname = path.file_name().unwrap_or_default().to_string_lossy().to_ascii_lowercase();
        if used_lnks.insert(fname) {
            let pin = make_lnk_pin(&path);
            if seen_keys.insert(pin.key.clone()) {
                pins.push(pin);
            }
        }
    }

    // Apply user custom extra pins & hidden unpins from %APPDATA%\gp-pc-boost\
    let hidden = load_string_set(&hidden_pins_path());
    pins.retain(|p| !hidden.contains(&p.key));

    for line in load_string_lines(&extra_pins_path()) {
        let mut parts = line.splitn(3, '|');
        let (Some(key), Some(label), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if hidden.contains(key) || !seen_keys.insert(key.to_string()) {
            continue;
        }
        let target_exe = if path.to_ascii_lowercase().ends_with(".exe") {
            path.to_ascii_lowercase()
        } else {
            String::new()
        };
        let package_root = find_windowsapps_root(path).map(|p| p.to_string_lossy().to_ascii_lowercase());
        let icon_path = package_root
            .as_deref()
            .and_then(|r| msix_manifest_info(Path::new(r), None))
            .and_then(|i| i.logo_path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        pins.push(PinnedApp {
            key: key.to_string(),
            lnk_path: Some(path.to_string()),
            aumid: None,
            package_root,
            target_exe,
            icon_path,
            label: label.to_string(),
        });
    }
    pins
}

fn appdata_dir() -> PathBuf {
    env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(env::temp_dir).join("gp-pc-boost")
}

fn extra_pins_path() -> PathBuf {
    appdata_dir().join("extra_pins.txt")
}

fn hidden_pins_path() -> PathBuf {
    appdata_dir().join("hidden_pins.txt")
}

fn load_string_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

fn load_string_set(path: &Path) -> HashSet<String> {
    load_string_lines(path).into_iter().collect()
}

fn save_string_lines(path: &Path, lines: &[String]) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, lines.join("\n"));
}

/// Pins an unpinned app or unpins an already-pinned app without touching Explorer's Taskband registry.
pub fn toggle_user_pin(item: &AppItem) -> bool {
    let is_pinned = item.pin_lnk.is_some() || item.pin_aumid.is_some();
    let extra_p = extra_pins_path();
    let hidden_p = hidden_pins_path();
    let mut extras = load_string_lines(&extra_p);
    let mut hidden = load_string_lines(&hidden_p);

    if is_pinned {
        let before = extras.len();
        extras.retain(|l| l.split('|').next() != Some(item.icon_key.as_str()));
        if extras.len() != before {
            save_string_lines(&extra_p, &extras);
        } else if !hidden.contains(&item.icon_key) {
            hidden.push(item.icon_key.clone());
            save_string_lines(&hidden_p, &hidden);
        }
        true
    } else {
        let target = if !item.icon_path.is_empty() && Path::new(&item.icon_path).is_file() {
            item.icon_path.clone()
        } else {
            return false;
        };
        hidden.retain(|k| k != &item.icon_key);
        save_string_lines(&hidden_p, &hidden);
        if !extras.iter().any(|l| l.split('|').next() == Some(item.icon_key.as_str())) {
            let clean_label = item.label.replace('|', " ");
            extras.push(format!("{}|{}|{}", item.icon_key, clean_label, target));
            save_string_lines(&extra_p, &extras);
        }
        true
    }
}

pub fn custom_order_path() -> PathBuf {
    let base = env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(env::temp_dir);
    base.join("gp-pc-boost").join("taskbar_order.txt")
}

pub fn load_custom_order() -> Vec<String> {
    fs::read_to_string(custom_order_path())
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

pub fn save_custom_order(keys: &[String]) {
    let p = custom_order_path();
    if let Some(dir) = p.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(p, keys.join("\n"));
}

pub fn apply_custom_order(items: &mut Vec<AppItem>, order: &[String]) {
    if order.is_empty() || items.len() <= 1 {
        return;
    }
    let pos: HashMap<&str, usize> = order.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
    let indexed: Vec<(usize, AppItem)> = items.drain(..).enumerate().collect();
    let mut sorted = indexed;
    sorted.sort_by_key(|(orig_idx, item)| {
        pos.get(item.icon_key.as_str()).copied().unwrap_or(usize::MAX / 2 + orig_idx)
    });
    *items = sorted.into_iter().map(|(_, item)| item).collect();
}

unsafe fn find_uwp_child_pid(hwnd: HWND, frame_pid: u32) -> Option<u32> {
    struct Ctx {
        frame_pid: u32,
        found: Option<u32>,
    }
    unsafe extern "system" fn cb(child: HWND, lp: LPARAM) -> i32 {
        let ctx = &mut *(lp as *mut Ctx);
        let mut cpid = 0u32;
        GetWindowThreadProcessId(child, &mut cpid);
        if cpid != 0 && cpid != ctx.frame_pid {
            ctx.found = Some(cpid);
            return 0;
        }
        1
    }
    let mut ctx = Ctx { frame_pid, found: None };
    EnumChildWindows(hwnd, Some(cb), &mut ctx as *mut _ as LPARAM);
    ctx.found
}

pub unsafe fn process_exe_path(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }
    let mut exe = String::new();
    let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
    if !proc.is_null() {
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        if QueryFullProcessImageNameW(proc, 0, buf.as_mut_ptr(), &mut len) != 0 {
            exe = String::from_utf16_lossy(&buf[..len as usize]).to_ascii_lowercase();
        }
        CloseHandle(proc);
    }
    exe
}

pub struct WindowTracker {
    exe_cache: HashMap<usize, (u32, String)>,
    order: Vec<HWND>,
    /// Monotonic MRU sequence stamp per HWND so default click always restores/opens the last active window.
    mru_stamp: HashMap<usize, u64>,
    mru_seq: u64,
    flashing_wins: HashSet<usize>,
    last_app_fg: HWND,
    own_pid: u32,
}

impl Default for WindowTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowTracker {
    pub fn new() -> Self {
        Self {
            exe_cache: HashMap::new(),
            order: Vec::new(),
            mru_stamp: HashMap::new(),
            mru_seq: 1,
            flashing_wins: HashSet::new(),
            last_app_fg: null_mut(),
            own_pid: std::process::id(),
        }
    }

    pub fn note_active(&mut self, hwnd: HWND) {
        if hwnd.is_null() {
            return;
        }
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == self.own_pid {
                return;
            }
            let root = resolve_top_owner(hwnd);
            if !root.is_null() {
                self.mru_seq = self.mru_seq.wrapping_add(1);
                self.mru_stamp.insert(root as usize, self.mru_seq);
                self.flashing_wins.remove(&(root as usize));
                self.last_app_fg = root;
            }
        }
    }

    pub fn note_flash(&mut self, hwnd: HWND, on: bool) {
        if hwnd.is_null() {
            return;
        }
        unsafe {
            let root = resolve_top_owner(hwnd);
            if !root.is_null() {
                if on {
                    self.flashing_wins.insert(root as usize);
                } else {
                    self.flashing_wins.remove(&(root as usize));
                }
            }
        }
    }

    pub fn last_app_window(&self) -> HWND {
        self.last_app_fg
    }

    unsafe fn exe_of(&mut self, hwnd: HWND) -> String {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == 0 {
            return String::new();
        }
        // Unwrap UWP/MSIX ApplicationFrameWindow to its real CoreWindow process PID
        let mut cls = [0u16; 48];
        let cn = GetClassNameW(hwnd, cls.as_mut_ptr(), 48).max(0) as usize;
        if String::from_utf16_lossy(&cls[..cn]) == "ApplicationFrameWindow"
            && let Some(cpid) = find_uwp_child_pid(hwnd, pid)
        {
            pid = cpid;
        }
        if let Some((cached_pid, exe)) = self.exe_cache.get(&(hwnd as usize))
            && *cached_pid == pid
            && !exe.is_empty()
        {
            return exe.clone();
        }
        let exe = process_exe_path(pid);
        if self.exe_cache.len() > 512 {
            self.exe_cache.clear();
        }
        self.exe_cache.insert(hwnd as usize, (pid, exe.clone()));
        exe
    }

    pub unsafe fn build_items(&mut self, pins: &[PinnedApp]) -> Vec<AppItem> {
        let mut raw_wins: Vec<HWND> = Vec::new();
        let mut ctx = CollectCtx {
            out: &mut raw_wins,
            own_pid: self.own_pid,
        };
        EnumWindows(Some(collect_window), &mut ctx as *mut _ as LPARAM);

        let fg = GetForegroundWindow();
        let fg_root = resolve_top_owner(fg);
        if !fg_root.is_null() {
            let mut fg_pid = 0u32;
            GetWindowThreadProcessId(fg_root, &mut fg_pid);
            if fg_pid != self.own_pid && raw_wins.contains(&fg_root) {
                self.mru_seq = self.mru_seq.wrapping_add(1);
                self.mru_stamp.insert(fg_root as usize, self.mru_seq);
                self.flashing_wins.remove(&(fg_root as usize));
                self.last_app_fg = fg_root;
            }
        }

        // Stamp newly discovered windows (newer windows get higher sequence than untouched older ones)
        // and prune closed windows from MRU / flash tables.
        let all_live_keys: HashSet<usize> = raw_wins.iter().map(|&w| w as usize).collect();
        self.mru_stamp.retain(|k, _| all_live_keys.contains(k));
        self.flashing_wins.retain(|k| all_live_keys.contains(k));
        // EnumWindows returns top-to-bottom Z-order; stamp unseen windows in reverse so topmost gets higher seq
        for &w in raw_wins.iter().rev() {
            if !self.mru_stamp.contains_key(&(w as usize)) {
                self.mru_seq = self.mru_seq.wrapping_add(1);
                self.mru_stamp.insert(w as usize, self.mru_seq);
            }
        }

        let mut items: Vec<AppItem> = pins
            .iter()
            .map(|p| AppItem {
                pin_lnk: p.lnk_path.clone(),
                pin_aumid: p.aumid.clone(),
                icon_key: p.key.clone(),
                icon_path: p.icon_path.clone(),
                label: p.label.clone(),
                wins: Vec::new(),
                win_mons: Vec::new(),
                mru_win: null_mut(),
                active: false,
                minimized: false,
                flashing: false,
            })
            .collect();

        let mut loose = Vec::<(HWND, String)>::new();
        for w in raw_wins {
            let exe = self.exe_of(w);
            let pin_idx = if !exe.is_empty() {
                pins.iter().position(|p| {
                    (!p.target_exe.is_empty() && p.target_exe == exe)
                        || p.package_root
                            .as_deref()
                            .is_some_and(|root| !root.is_empty() && exe.starts_with(root))
                })
            } else {
                None
            };
            let wmon = MonitorFromWindow(w, MONITOR_DEFAULTTONEAREST) as isize;
            match pin_idx {
                Some(i) => {
                    items[i].wins.push(w);
                    items[i].win_mons.push(wmon);
                }
                None => loose.push((w, exe)),
            }
        }

        let live_set: Vec<HWND> = loose.iter().map(|(w, _)| *w).collect();
        self.order.retain(|w| live_set.contains(w));
        for &(w, _) in &loose {
            if !self.order.contains(&w) {
                self.order.push(w);
            }
        }

        let exe_map: HashMap<usize, String> = loose.into_iter().map(|(w, e)| (w as usize, e)).collect();
        let mut unpinned_groups: Vec<(String, AppItem)> = Vec::new();
        for &w in &self.order {
            let exe = exe_map.get(&(w as usize)).cloned().unwrap_or_default();
            let msix_root = find_windowsapps_root(&exe);
            let host = is_generic_host(&exe);
            let group_key = if let Some(ref root) = msix_root {
                format!("msix:{}", root.to_string_lossy().to_ascii_lowercase())
            } else if exe.is_empty() || host {
                format!("hwnd:{}", w as usize)
            } else {
                format!("exe:{exe}")
            };
            let wmon = MonitorFromWindow(w, MONITOR_DEFAULTTONEAREST) as isize;
            if let Some((_, existing)) = unpinned_groups.iter_mut().find(|(k, _)| *k == group_key) {
                existing.wins.push(w);
                existing.win_mons.push(wmon);
            } else {
                let title = window_title(w);
                let label = if title.is_empty() {
                    Path::new(&exe)
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                } else {
                    title
                };
                let icon_path = msix_root
                    .as_deref()
                    .and_then(|r| msix_manifest_info(r, None))
                    .and_then(|i| i.logo_path)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or(exe);
                unpinned_groups.push((
                    group_key.clone(),
                    AppItem {
                        pin_lnk: None,
                        pin_aumid: None,
                        icon_key: group_key,
                        icon_path,
                        label,
                        wins: vec![w],
                        win_mons: vec![wmon],
                        mru_win: w,
                        active: false,
                        minimized: false,
                        flashing: false,
                    },
                ));
            }
        }

        items.extend(unpinned_groups.into_iter().map(|(_, item)| item));
        for item in &mut items {
            if !item.wins.is_empty() {
                // Order windows by MRU descending so wins[0] and mru_win are ALWAYS the last-used / last-opened window
                let mut paired: Vec<(HWND, isize, u64)> = item
                    .wins
                    .iter()
                    .copied()
                    .zip(item.win_mons.iter().copied())
                    .map(|(w, m)| (w, m, self.mru_stamp.get(&(w as usize)).copied().unwrap_or(0)))
                    .collect();
                paired.sort_by(|a, b| b.2.cmp(&a.2));
                item.wins = paired.iter().map(|x| x.0).collect();
                item.win_mons = paired.iter().map(|x| x.1).collect();
                item.mru_win = item.wins[0];
                item.active = (!fg_root.is_null() && item.wins.contains(&fg_root)) || item.wins.contains(&fg);
                item.minimized = item.wins.iter().all(|&w| IsIconic(w) != 0);
                item.flashing = item.wins.iter().any(|&w| self.flashing_wins.contains(&(w as usize)));
                let t = window_title(item.mru_win);
                if !t.is_empty() {
                    item.label = if item.wins.len() == 1 {
                        t
                    } else {
                        format!("{t} (+{})", item.wins.len() - 1)
                    };
                }
            }
        }
        items
    }
}

/// Resolves an owned dialog or child window back to its top-level root owner window.
pub unsafe fn resolve_top_owner(hwnd: HWND) -> HWND {
    if hwnd.is_null() {
        return null_mut();
    }
    let root = GetAncestor(hwnd, GA_ROOTOWNER);
    if !root.is_null() { root } else { hwnd }
}

/// Filters `all` app items for monitor `mon` according to Windows `MMTaskbarMode` (0 = all, 1 = main + where open, 2 = where open).
pub fn filter_items_for_monitor(all: &[AppItem], mon: &MonitorSpec, mm_mode: u32) -> Vec<AppItem> {
    if mm_mode == 0 || (mm_mode == 1 && mon.primary) {
        return all.to_vec();
    }
    let mut out = Vec::new();
    for item in all {
        let mut sub_wins = Vec::new();
        let mut sub_mons = Vec::new();
        for (&w, &m) in item.wins.iter().zip(item.win_mons.iter()) {
            if m == mon.hmon {
                sub_wins.push(w);
                sub_mons.push(m);
            }
        }
        if !sub_wins.is_empty() || (mm_mode == 2 && mon.primary && (item.pin_lnk.is_some() || item.pin_aumid.is_some())) {
            let mut copy = item.clone();
            copy.mru_win = sub_wins.first().copied().unwrap_or(null_mut());
            copy.wins = sub_wins;
            copy.win_mons = sub_mons;
            copy.active = item.active && !copy.wins.is_empty();
            copy.minimized = !copy.wins.is_empty() && item.minimized;
            copy.flashing = item.flashing && !copy.wins.is_empty();
            out.push(copy);
        }
    }
    out
}

pub unsafe fn active_window_summary(own_pid: u32) -> String {
    let fg = GetForegroundWindow();
    if fg.is_null() {
        return "Desktop".into();
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(fg, &mut pid);
    if pid == own_pid {
        return "Desktop".into();
    }
    let mut cls = [0u16; 64];
    let n = GetClassNameW(fg, cls.as_mut_ptr(), 64).max(0) as usize;
    if matches!(
        String::from_utf16_lossy(&cls[..n]).as_str(),
        "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
    ) {
        return "Desktop".into();
    }
    let title = window_title(fg);
    if title.is_empty() {
        "Workspace".into()
    } else {
        title
    }
}

pub unsafe fn hide_all_stock_taskbars() {
    EnumWindows(Some(hide_tray_cb), 1);
}

pub unsafe fn restore_all_stock_taskbars() {
    EnumWindows(Some(hide_tray_cb), 0);
}

unsafe extern "system" fn hide_tray_cb(hwnd: HWND, show: LPARAM) -> i32 {
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == std::process::id() {
        return 1;
    }
    let mut cls = [0u16; 48];
    let n = GetClassNameW(hwnd, cls.as_mut_ptr(), 48).max(0) as usize;
    let name = String::from_utf16_lossy(&cls[..n]);
    if name == "Shell_TrayWnd" || name == "Shell_SecondaryTrayWnd" {
        if show == 0 {
            ShowWindow(hwnd, SW_SHOWNA);
        } else if IsWindowVisible(hwnd) != 0 {
            ShowWindow(hwnd, SW_HIDE);
        }
    }
    1
}

struct CollectCtx<'a> {
    out: &'a mut Vec<HWND>,
    own_pid: u32,
}

unsafe extern "system" fn collect_window(hwnd: HWND, lp: LPARAM) -> i32 {
    let ctx = &mut *(lp as *mut CollectCtx<'_>);
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == ctx.own_pid {
        return 1;
    }
    if IsWindowVisible(hwnd) == 0 || !GetWindow(hwnd, GW_OWNER).is_null() {
        return 1;
    }
    let ex = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if (ex & WS_EX_TOOLWINDOW) != 0 && (ex & WS_EX_APPWINDOW) == 0 {
        return 1;
    }
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, &mut cloaked as *mut _ as *mut _, 4) == 0
        && cloaked != 0
    {
        return 1;
    }
    if GetWindowTextLengthW(hwnd) <= 0 {
        return 1;
    }
    ctx.out.push(hwnd);
    1
}

fn is_generic_host(exe: &str) -> bool {
    const HOSTS: [&str; 7] = [
        "applicationframehost.exe",
        "javaw.exe",
        "java.exe",
        "pythonw.exe",
        "python.exe",
        "mmc.exe",
        "dllhost.exe",
    ];
    let fname = exe.rsplit('\\').next().unwrap_or(exe);
    HOSTS.iter().any(|h| *h == fname)
}

pub unsafe fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
    if n <= 0 {
        String::new()
    } else {
        String::from_utf16_lossy(&buf[..n as usize])
    }
}

pub unsafe fn send_chord(vks: &[u16]) {
    let send_one = |vk: u16, up: bool| {
        let mut input: INPUT = zeroed();
        input.r#type = INPUT_KEYBOARD;
        input.Anonymous = INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        };
        SendInput(1, &input, size_of::<INPUT>() as i32);
    };
    for &vk in vks {
        send_one(vk, false);
    }
    for &vk in vks.iter().rev() {
        send_one(vk, true);
    }
}

pub unsafe fn activate_or_toggle(item: &AppItem) {
    if item.wins.is_empty() {
        launch_new_instance(item);
        return;
    }
    let fg = GetForegroundWindow();
    let fg_root = resolve_top_owner(fg);
    let active_pos = item
        .wins
        .iter()
        .position(|&w| w == fg || (!fg_root.is_null() && w == fg_root));

    if let Some(pos) = active_pos {
        let cur = item.wins[pos];
        if item.wins.len() == 1 {
            if IsIconic(cur) != 0 {
                bring_to_front(cur);
            } else {
                ShowWindow(cur, SW_MINIMIZE);
            }
        } else {
            // Multiple windows & one is already foreground -> cycle to next MRU window
            let next = item.wins[(pos + 1) % item.wins.len()];
            bring_to_front(next);
        }
    } else {
        // Native Windows behavior: clicking an app icon opens/restores the last-active (MRU) window of that app
        let target = if !item.mru_win.is_null() && IsWindow(item.mru_win) != 0 {
            item.mru_win
        } else {
            item.wins[0]
        };
        bring_to_front(target);
    }
}

/// Reliably restores (if minimized) and raises `hwnd` to the foreground even when called from a
/// `WS_EX_NOACTIVATE` taskbar surface (bypasses Windows foreground-lock timeout like Explorer).
pub unsafe fn bring_to_front(hwnd: HWND) {
    if hwnd.is_null() || IsWindow(hwnd) == 0 {
        return;
    }
    if IsIconic(hwnd) != 0 {
        ShowWindow(hwnd, SW_RESTORE);
    } else if IsWindowVisible(hwnd) == 0 {
        ShowWindow(hwnd, SW_SHOW);
    }
    let mut target_pid = 0u32;
    let target_tid = GetWindowThreadProcessId(hwnd, &mut target_pid);
    if target_pid != 0 {
        AllowSetForegroundWindow(target_pid);
    }
    if SetForegroundWindow(hwnd) != 0 && GetForegroundWindow() == hwnd {
        BringWindowToTop(hwnd);
        return;
    }

    // 1. Attach input queue temporarily to current foreground thread and target thread
    let me = GetCurrentThreadId();
    let fg = GetForegroundWindow();
    let fg_tid = if !fg.is_null() { GetWindowThreadProcessId(fg, null_mut()) } else { 0 };
    let att_fg = fg_tid != 0 && fg_tid != me && AttachThreadInput(me, fg_tid, 1) != 0;
    let att_tgt = target_tid != 0 && target_tid != me && target_tid != fg_tid && AttachThreadInput(me, target_tid, 1) != 0;
    BringWindowToTop(hwnd);
    SetForegroundWindow(hwnd);
    if att_tgt {
        AttachThreadInput(me, target_tid, 0);
    }
    if att_fg {
        AttachThreadInput(me, fg_tid, 0);
    }

    // 2. Fallback: SwitchToThisWindow + Alt-key unlock when foreground is empty or elevated (UIPI)
    if GetForegroundWindow() != hwnd {
        let send_k = |vk: u16, up: bool| {
            let mut inp: INPUT = zeroed();
            inp.r#type = INPUT_KEYBOARD;
            inp.Anonymous = INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
            };
            SendInput(1, &inp, size_of::<INPUT>() as i32);
        };
        send_k(VK_MENU, false);
        send_k(0xE8, false);
        send_k(0xE8, true);
        SwitchToThisWindow(hwnd, 1);
        SetForegroundWindow(hwnd);
        send_k(VK_MENU, true);
    }
}

/// Returns `(installed_count, short_code)` for the active keyboard layout (e.g. `(2, "ENG")`).
pub unsafe fn input_language_status(target_hwnd: HWND) -> (usize, String) {
    let mut list: [HKL; 32] = [null_mut(); 32];
    let n = GetKeyboardLayoutList(32, list.as_mut_ptr()).clamp(0, 32) as usize;
    if n <= 1 {
        return (n, String::new());
    }
    let tid = if !target_hwnd.is_null() && IsWindow(target_hwnd) != 0 {
        GetWindowThreadProcessId(target_hwnd, null_mut())
    } else {
        let fg = GetForegroundWindow();
        if !fg.is_null() { GetWindowThreadProcessId(fg, null_mut()) } else { 0 }
    };
    let hkl = GetKeyboardLayout(tid);
    let lang_id = (hkl as usize as u32) & 0xFFFF;
    let mut buf = [0u16; 16];
    let len = GetLocaleInfoW(lang_id, 3, buf.as_mut_ptr(), 16).max(1) as usize - 1;
    let short = String::from_utf16_lossy(&buf[..len]).to_uppercase();
    let code = if short == "ENU" || short.is_empty() { "ENG".into() } else { short };
    (n, code)
}

/// Right-click window management menu for individual preview cards (Restore / Minimize / Maximize / Close).
pub unsafe fn show_window_context_menu(owner: HWND, screen_x: i32, screen_y: i32, target: HWND) {
    if target.is_null() || IsWindow(target) == 0 {
        return;
    }
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }
    let iconic = IsIconic(target) != 0;
    let zoomed = IsZoomed(target) != 0;
    AppendMenuW(menu, MF_STRING | if !iconic && !zoomed { MF_GRAYED } else { 0 }, 1, wide("Restore").as_ptr());
    AppendMenuW(menu, MF_STRING | if iconic { MF_GRAYED } else { 0 }, 2, wide("Minimize").as_ptr());
    AppendMenuW(menu, MF_STRING | if zoomed { MF_GRAYED } else { 0 }, 3, wide("Maximize").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, 4, wide("Close window").as_ptr());

    SetForegroundWindow(owner);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, screen_x, screen_y, 0, owner, null());
    DestroyMenu(menu);

    match cmd as usize {
        1 => {
            ShowWindow(target, SW_RESTORE);
            bring_to_front(target);
        }
        2 => {
            ShowWindow(target, SW_MINIMIZE);
        }
        3 => {
            ShowWindow(target, SW_MAXIMIZE);
            bring_to_front(target);
        }
        4 => close_window_graceful(target),
        _ => {}
    }
}

/// Pins a dropped file (`.exe` or `.lnk`) directly onto the custom taskbar.
pub fn pin_dropped_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if ext != "exe" && ext != "lnk" {
        return false;
    }
    let path_str = path.to_string_lossy().into_owned();
    let label = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .replace('|', " ");
    let key = format!("drop:{}", path_str.to_ascii_lowercase());
    let extra_p = extra_pins_path();
    let hidden_p = hidden_pins_path();
    let mut extras = load_string_lines(&extra_p);
    let mut hidden = load_string_lines(&hidden_p);
    hidden.retain(|k| k != &key);
    save_string_lines(&hidden_p, &hidden);
    if !extras.iter().any(|l| l.split('|').next() == Some(key.as_str())) {
        extras.push(format!("{key}|{label}|{path_str}"));
        save_string_lines(&extra_p, &extras);
    }
    true
}

pub unsafe fn launch_path(path: &str) {
    let wpath = wide(path);
    ShellExecuteW(null_mut(), wide("open").as_ptr(), wpath.as_ptr(), null(), null(), SW_SHOWNORMAL);
}

pub unsafe fn launch_new_instance(item: &AppItem) {
    if let Some(lnk) = &item.pin_lnk {
        launch_path(lnk);
    } else if let Some(aumid) = &item.pin_aumid {
        launch_path(&format!("shell:AppsFolder\\{aumid}"));
    } else if !item.icon_path.is_empty() {
        launch_path(&item.icon_path);
    }
}

pub unsafe fn close_window_graceful(hwnd: HWND) {
    if PostMessageW(hwnd, WM_CLOSE, 0, 0) == 0 {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != 0 {
            let _ = Command::new("taskkill.exe")
                .args(["/PID", &pid.to_string()])
                .creation_flags(0x0800_0000)
                .spawn();
        }
    }
}

pub unsafe fn launch_as_admin(item: &AppItem) {
    let target = item.pin_lnk.as_deref().or((!item.icon_path.is_empty()).then_some(item.icon_path.as_str()));
    if let Some(p) = target {
        let wp = wide(p);
        ShellExecuteW(null_mut(), wide("runas").as_ptr(), wp.as_ptr(), null(), null(), SW_SHOWNORMAL);
    }
}

pub unsafe fn open_file_location(item: &AppItem) {
    let mut path = item.icon_path.clone();
    if let Some(lnk) = &item.pin_lnk
        && lnk.to_ascii_lowercase().ends_with(".lnk")
        && let Ok(bytes) = fs::read(lnk)
        && let Some(info) = parse_lnk_info(&bytes)
        && let Some(exe) = info.target_exe
    {
        path = exe;
    }
    if !path.is_empty() {
        let _ = Command::new("explorer.exe").arg(format!("/select,\"{path}\"")).spawn();
    }
}

pub unsafe fn show_app_context_menu(owner: HWND, screen_x: i32, screen_y: i32, item: &AppItem) -> bool {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return false;
    }
    let has_wins = !item.wins.is_empty();
    let is_pinned = item.pin_lnk.is_some() || item.pin_aumid.is_some();
    if has_wins {
        for (idx, &w) in item.wins.iter().take(6).enumerate() {
            let t = window_title(w);
            let label = if t.is_empty() { item.label.clone() } else { t };
            AppendMenuW(menu, MF_STRING, 100 + idx, wide(&label).as_ptr());
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
    }
    if is_pinned || !item.icon_path.is_empty() {
        AppendMenuW(menu, MF_STRING, 1, wide("Open new window").as_ptr());
        if item.pin_aumid.is_none() {
            AppendMenuW(menu, MF_STRING, 4, wide("Run as administrator").as_ptr());
            AppendMenuW(menu, MF_STRING, 5, wide("Open file location").as_ptr());
        }
    }
    if is_pinned || (!item.icon_path.is_empty() && Path::new(&item.icon_path).is_file()) {
        AppendMenuW(
            menu,
            MF_STRING,
            6,
            wide(if is_pinned { "Unpin from taskbar" } else { "Pin to taskbar" }).as_ptr(),
        );
    }
    if has_wins {
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        AppendMenuW(
            menu,
            MF_STRING,
            2,
            wide(if item.minimized { "Restore" } else { "Minimize" }).as_ptr(),
        );
        AppendMenuW(
            menu,
            MF_STRING,
            3,
            wide(if item.wins.len() > 1 { "Close all windows" } else { "Close window" }).as_ptr(),
        );
    }

    SetForegroundWindow(owner);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, screen_x, screen_y, 0, owner, null());
    DestroyMenu(menu);

    let mut pins_changed = false;
    match cmd as usize {
        1 => launch_new_instance(item),
        2 => {
            for &w in &item.wins {
                if item.minimized {
                    bring_to_front(w);
                } else {
                    ShowWindow(w, SW_MINIMIZE);
                }
            }
        }
        3 => {
            for &w in &item.wins {
                close_window_graceful(w);
            }
        }
        4 => launch_as_admin(item),
        5 => open_file_location(item),
        6 => pins_changed = toggle_user_pin(item),
        id if id >= 100 => {
            if let Some(&w) = item.wins.get(id - 100) {
                bring_to_front(w);
            }
        }
        _ => {}
    }
    pins_changed
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TaskbarMenuCmd {
    None,
    ShowDesktop,
    TaskManager,
    RunDialog,
    SetDockSize(DockSize),
    ToggleStatusAutoHide,
    ToggleDockAutoHide,
    ToggleGamingMode,
    TogglePreviews,
    ToggleClockSeconds,
    OpenSettingsPopup,
    WindowsTaskbarSettings,
    RefreshPins,
    OpenBoostUi,
    RestoreStock,
}

pub unsafe fn show_taskbar_context_menu(
    owner: HWND,
    screen_x: i32,
    screen_y: i32,
    dock_size: DockSize,
    dock_autohide: bool,
    status_autohide: bool,
    gaming_mode: bool,
    show_previews: bool,
    show_seconds: bool,
) -> TaskbarMenuCmd {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return TaskbarMenuCmd::None;
    }
    let size_sub = CreatePopupMenu();
    if !size_sub.is_null() {
        for (id, sz) in [
            (20usize, DockSize::Compact),
            (21, DockSize::Small),
            (22, DockSize::Standard),
            (23, DockSize::Large),
        ] {
            AppendMenuW(
                size_sub,
                MF_STRING | if dock_size == sz { MF_CHECKED } else { 0 },
                id,
                wide(sz.display_name()).as_ptr(),
            );
        }
    }

    AppendMenuW(menu, MF_STRING, 1, wide("Show the desktop (Win+D)").as_ptr());
    AppendMenuW(menu, MF_STRING, 2, wide("Task Manager").as_ptr());
    AppendMenuW(menu, MF_STRING, 3, wide("Run... (Win+R)").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    if !size_sub.is_null() {
        AppendMenuW(menu, MF_POPUP, size_sub as usize, wide("Taskbar Size").as_ptr());
    }
    AppendMenuW(
        menu,
        MF_STRING | if status_autohide { MF_CHECKED } else { 0 },
        4,
        wide("Status Strip: Hover Edge to Show").as_ptr(),
    );
    AppendMenuW(
        menu,
        MF_STRING | if dock_autohide { MF_CHECKED } else { 0 },
        5,
        wide("Auto-Hide App Dock").as_ptr(),
    );
    AppendMenuW(
        menu,
        MF_STRING | if gaming_mode { MF_CHECKED } else { 0 },
        13,
        wide("Gaming Mode (Hide in Fullscreen)").as_ptr(),
    );
    AppendMenuW(
        menu,
        MF_STRING | if show_previews { MF_CHECKED } else { 0 },
        11,
        wide("Hover Window Previews").as_ptr(),
    );
    AppendMenuW(
        menu,
        MF_STRING | if show_seconds { MF_CHECKED } else { 0 },
        12,
        wide("Show Seconds in Clock").as_ptr(),
    );
    AppendMenuW(menu, MF_STRING, 6, wide("All Taskbar Settings...").as_ptr());
    AppendMenuW(menu, MF_STRING, 7, wide("Windows Taskbar Settings").as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, 8, wide("Refresh Pinned Apps & Tray Icons").as_ptr());
    AppendMenuW(menu, MF_STRING, 9, wide("Open GP PC Boost UI").as_ptr());
    AppendMenuW(menu, MF_STRING, 10, wide("Restore Stock Windows Taskbar").as_ptr());

    SetForegroundWindow(owner);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, screen_x, screen_y, 0, owner, null());
    DestroyMenu(menu);

    match cmd as usize {
        1 => TaskbarMenuCmd::ShowDesktop,
        2 => TaskbarMenuCmd::TaskManager,
        3 => TaskbarMenuCmd::RunDialog,
        4 => TaskbarMenuCmd::ToggleStatusAutoHide,
        5 => TaskbarMenuCmd::ToggleDockAutoHide,
        6 => TaskbarMenuCmd::OpenSettingsPopup,
        7 => TaskbarMenuCmd::WindowsTaskbarSettings,
        8 => TaskbarMenuCmd::RefreshPins,
        9 => TaskbarMenuCmd::OpenBoostUi,
        10 => TaskbarMenuCmd::RestoreStock,
        11 => TaskbarMenuCmd::TogglePreviews,
        12 => TaskbarMenuCmd::ToggleClockSeconds,
        13 => TaskbarMenuCmd::ToggleGamingMode,
        20 => TaskbarMenuCmd::SetDockSize(DockSize::Compact),
        21 => TaskbarMenuCmd::SetDockSize(DockSize::Small),
        22 => TaskbarMenuCmd::SetDockSize(DockSize::Standard),
        23 => TaskbarMenuCmd::SetDockSize(DockSize::Large),
        _ => TaskbarMenuCmd::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mm_mode_1_filters_secondary_windows_only() {
        let m1 = MonitorSpec {
            hmon: 1,
            rect: RECT { left: 0, top: 0, right: 1920, bottom: 1080 },
            primary: true,
            scale: 1.0,
        };
        let m2 = MonitorSpec {
            hmon: 2,
            rect: RECT { left: 1920, top: -464, right: 3000, bottom: 1456 },
            primary: false,
            scale: 1.0,
        };
        let all = vec![
            AppItem {
                pin_lnk: Some("a.lnk".into()),
                pin_aumid: None,
                icon_key: "a".into(),
                icon_path: "a".into(),
                label: "PinOnly".into(),
                wins: vec![],
                win_mons: vec![],
                mru_win: null_mut(),
                active: false,
                minimized: false,
                flashing: false,
            },
            AppItem {
                pin_lnk: None,
                pin_aumid: None,
                icon_key: "b".into(),
                icon_path: "b".into(),
                label: "OnMon2".into(),
                wins: vec![0x20usize as HWND],
                win_mons: vec![2],
                mru_win: 0x20usize as HWND,
                active: true,
                minimized: false,
                flashing: false,
            },
        ];

        assert_eq!(filter_items_for_monitor(&all, &m1, 1).len(), 2);
        let f2 = filter_items_for_monitor(&all, &m2, 1);
        assert_eq!(f2.len(), 1);
        assert_eq!(f2[0].label, "OnMon2");

        let mut reordered = all.clone();
        apply_custom_order(&mut reordered, &["b".into(), "a".into()]);
        assert_eq!(reordered[0].icon_key, "b");
        assert_eq!(reordered[1].icon_key, "a");
    }

    #[test]
    fn aumid_parser_accepts_valid_msix_and_rejects_paths() {
        assert_eq!(
            looks_like_aumid("Claude_pzs8sxrjxfjjc!Claude"),
            Some(("Claude_pzs8sxrjxfjjc".into(), "Claude".into()))
        );
        assert_eq!(
            looks_like_aumid("5319275A.51895FA4EA97F_cv1g1gvanyjgm!App"),
            Some(("5319275A.51895FA4EA97F_cv1g1gvanyjgm".into(), "App".into()))
        );
        assert!(looks_like_aumid(r"C:\Program Files\foo!bar").is_none());
    }
}
