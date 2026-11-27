//! Windows Notification Area (System Tray) receiver, icon extraction, click dispatch & overflow pinning.
//! Hosts a hidden topmost `Shell_TrayWnd` to receive `Shell_NotifyIconW` (`WM_COPYDATA`) messages
//! from background apps (Steam, Discord, Docker, Voicemod, etc.) while forwarding non-tray
//! `SHAppBarMessage` calls to Explorer's real `Shell_TrayWnd`.

use crate::taskbar::{
    icons::{IconBitmap, extract_hicon_bitmap},
    windows::{process_exe_path, wide},
};
use std::{
    collections::HashSet,
    env, fs,
    mem::zeroed,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::Arc,
};
use windows_sys::Win32::{
    Foundation::*,
    System::SystemInformation::GetTickCount,
    UI::{
        Input::KeyboardAndMouse::GetDoubleClickTime,
        WindowsAndMessaging::*,
    },
};

/// Platform-neutral `COPYDATASTRUCT` layout for `WM_COPYDATA`.
#[repr(C)]
pub struct CopyDataStruct {
    pub dw_data: usize,
    pub cb_data: u32,
    pub lp_data: *const u8,
}

#[derive(Clone)]
pub struct TrayIcon {
    pub hwnd: HWND,
    pub uid: u32,
    pub guid: [u8; 16],
    pub cb_msg: u32,
    pub version: u32,
    pub hidden: bool,
    pub tip: String,
    pub key: String, // lowercase exe filename (e.g. "discord.exe") or fallback id
    pub hicon: HICON,
    pub bitmap: Option<Arc<IconBitmap>>,
    pub bmp_px: i32,
}

pub struct TrayManager {
    pub icons: Vec<TrayIcon>,
    /// User-pinned exe keys that stay directly on the bar.
    pub pinned_keys: Vec<String>,
    /// User-hidden exe keys explicitly demoted to the overflow chevron flyout.
    pub overflow_keys: HashSet<String>,
    /// Whether the user has customized pinned/overflow lists yet.
    pub has_custom_prefs: bool,
    last_click: (usize, u32, u32), // (hwnd, uid, tick_ms) for native double-click detection
}

/// Default cap for automatically surfacing active program icons directly beside the control menu bar
/// when the user has not explicitly customized their pinned tray icons yet.
pub const DEFAULT_BAR_TRAY_LIMIT: usize = 6;

impl Default for TrayManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TrayManager {
    pub fn new() -> Self {
        let pinned_path = tray_pinned_path();
        let hidden_path = tray_hidden_path();
        let has_custom_prefs = pinned_path.is_file() || hidden_path.is_file();
        let pinned_keys = load_lines(&pinned_path);
        let overflow_keys = load_lines(&hidden_path).into_iter().collect();
        Self {
            icons: Vec::new(),
            pinned_keys,
            overflow_keys,
            has_custom_prefs,
            last_click: (0, 0, 0),
        }
    }

    pub unsafe fn clear(&mut self) {
        for ti in self.icons.drain(..) {
            if !ti.hicon.is_null() {
                DestroyIcon(ti.hicon);
            }
        }
    }

    /// Removes tray entries whose owner window no longer exists. Returns true if anything changed.
    pub unsafe fn prune_dead(&mut self) -> bool {
        let before = self.icons.len();
        let mut i = 0;
        while i < self.icons.len() {
            if self.icons[i].hwnd.is_null() || IsWindow(self.icons[i].hwnd) == 0 {
                let dead = self.icons.remove(i);
                if !dead.hicon.is_null() {
                    DestroyIcon(dead.hicon);
                }
            } else {
                i += 1;
            }
        }
        self.icons.len() != before
    }

    /// Processes a `Shell_NotifyIconW` `WM_COPYDATA` payload (`dwData == 1`, `cbData >= 964`).
    /// Returns true if the visible icon set, icons, or tooltips changed.
    pub unsafe fn handle_notify_data(&mut self, data: &[u8], target_px: i32) -> bool {
        if data.len() < 960 {
            return false;
        }
        let u = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap_or([0; 4]));
        let op = u(4);
        let h = u(12) as i32 as isize as HWND;
        let uid = u(16);
        let flags = u(20);
        let guid: [u8; 16] = if (flags & 0x20) != 0 {
            data[944..960].try_into().unwrap_or([0; 16])
        } else {
            [0; 16]
        };

        let pos = self.icons.iter().position(|x| {
            if guid != [0; 16] && x.guid != [0; 16] {
                x.guid == guid
            } else if guid != [0; 16] {
                x.guid == guid || (x.hwnd == h && x.uid == uid)
            } else {
                x.hwnd == h && x.uid == uid
            }
        });

        match (op, pos) {
            // NIM_DELETE = 2
            (2, Some(i)) => {
                let removed = self.icons.remove(i);
                if !removed.hicon.is_null() {
                    DestroyIcon(removed.hicon);
                }
                true
            }
            // NIM_SETVERSION = 4
            (4, Some(i)) => {
                self.icons[i].version = u(808);
                false
            }
            // NIM_ADD = 0, NIM_MODIFY = 1
            (0 | 1, _) => {
                let idx = match pos {
                    Some(i) => {
                        if guid != [0; 16] && self.icons[i].guid == [0; 16] {
                            self.icons[i].guid = guid;
                        }
                        i
                    }
                    // Adopt NIM_MODIFY if the app sends a valid icon for an active window we missed
                    None if op == 1 && ((flags & 0x02) == 0 || h.is_null() || IsWindow(h) == 0) => return false,
                    None => {
                        let mut pid = 0u32;
                        GetWindowThreadProcessId(h, &mut pid);
                        if pid == std::process::id() {
                            return false;
                        }
                        let exe = process_exe_path(pid);
                        let fname = exe.rsplit('\\').next().unwrap_or("").to_ascii_lowercase();
                        // Skip Explorer's redundant system volume/network/power icons (the bar renders them natively)
                        // and skip our own taskbar binaries.
                        if (fname == "explorer.exe" && is_builtin_system_tray_guid(&guid))
                            || fname == "gp-taskbar.exe"
                            || fname == "topbar.exe"
                        {
                            return false;
                        }
                        let key = if fname.is_empty() {
                            format!("hwnd:{}:{uid}", h as usize)
                        } else {
                            fname
                        };
                        self.icons.push(TrayIcon {
                            hwnd: h,
                            uid,
                            guid,
                            cb_msg: 0,
                            version: 0,
                            hidden: false,
                            tip: String::new(),
                            key,
                            hicon: null_mut(),
                            bitmap: None,
                            bmp_px: 0,
                        });
                        self.icons.len() - 1
                    }
                };

                let entry = &mut self.icons[idx];
                // NIF_MESSAGE = 0x01
                if (flags & 0x01) != 0 {
                    entry.cb_msg = u(24);
                }
                // NIF_ICON = 0x02
                if (flags & 0x02) != 0 {
                    let raw_icon = u(28) as i32 as isize as HICON;
                    if !raw_icon.is_null() {
                        let copied = CopyIcon(raw_icon);
                        if !copied.is_null() {
                            if !entry.hicon.is_null() {
                                DestroyIcon(entry.hicon);
                            }
                            entry.hicon = copied;
                            entry.bmp_px = target_px.max(16);
                            entry.bitmap = extract_hicon_bitmap(copied, entry.bmp_px).map(Arc::new);
                        }
                    }
                }
                // NIF_TIP = 0x04 (UTF-16LE at offset 32..288 = 128 WCHARs)
                if (flags & 0x04) != 0 {
                    let w: Vec<u16> = data[32..288]
                        .chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .take_while(|&c| c != 0)
                        .collect();
                    let tip = String::from_utf16_lossy(&w).trim().to_string();
                    if !tip.is_empty() {
                        entry.tip = tip;
                    }
                }
                // NIF_STATE = 0x08 (dwState@288, dwStateMask@292, NIS_HIDDEN = 0x01)
                if (flags & 0x08) != 0 && (u(292) & 0x01) != 0 {
                    entry.hidden = (u(288) & 0x01) != 0;
                }
                true
            }
            _ => false,
        }
    }

    /// Re-extracts cached `IconBitmap`s if DPI scale changed.
    pub unsafe fn ensure_icon_size(&mut self, target_px: i32) {
        let px = target_px.max(16);
        for entry in &mut self.icons {
            if !entry.hicon.is_null() && (entry.bitmap.is_none() || entry.bmp_px != px) {
                entry.bmp_px = px;
                entry.bitmap = extract_hicon_bitmap(entry.hicon, px).map(Arc::new);
            }
        }
    }

    /// Finds the first registered tray icon matching an executable key (e.g. `"coucou.exe"`).
    pub fn find_by_key(&self, key_lower: &str) -> Option<usize> {
        self.icons.iter().position(|x| !x.hidden && x.key == key_lower)
    }

    /// Indices into `self.icons` displayed directly beside the control menu bar.
    pub fn bar_indices(&self, max_auto: usize) -> Vec<usize> {
        let visible: Vec<usize> = self
            .icons
            .iter()
            .enumerate()
            .filter(|(_, x)| !x.hidden && (x.bitmap.is_some() || !x.hicon.is_null()))
            .map(|(i, _)| i)
            .collect();

        if self.has_custom_prefs {
            let mut out = Vec::new();
            for key in &self.pinned_keys {
                for &idx in &visible {
                    if self.icons[idx].key == *key && !out.contains(&idx) {
                        out.push(idx);
                    }
                }
            }
            // Also surface newly registered icons up to max_auto unless explicitly demoted to overflow
            for &idx in &visible {
                if out.len() >= max_auto {
                    break;
                }
                let k = &self.icons[idx].key;
                if k != "coucou.exe" && !self.overflow_keys.contains(k) && !out.contains(&idx) {
                    out.push(idx);
                }
            }
            out
        } else {
            // Keep coucou.exe tucked in overflow by default since Coucou has its own dedicated center pill
            visible
                .into_iter()
                .filter(|&idx| self.icons[idx].key != "coucou.exe")
                .take(max_auto)
                .collect()
        }
    }

    /// Indices into `self.icons` living inside the overflow chevron flyout.
    pub fn overflow_indices(&self, max_auto: usize) -> Vec<usize> {
        let bar = self.bar_indices(max_auto);
        self.icons
            .iter()
            .enumerate()
            .filter(|(i, x)| !x.hidden && !bar.contains(i) && (x.bitmap.is_some() || !x.hicon.is_null()))
            .map(|(i, _)| i)
            .collect()
    }

    /// All visible non-hidden tray icon indices (used when overflow is empty so chevron still lets user inspect/manage icons).
    pub fn all_visible_indices(&self) -> Vec<usize> {
        self.icons
            .iter()
            .enumerate()
            .filter(|(_, x)| !x.hidden && (x.bitmap.is_some() || !x.hicon.is_null()))
            .map(|(i, _)| i)
            .collect()
    }

    fn ensure_custom_prefs_seeded(&mut self, max_auto: usize) {
        if self.has_custom_prefs {
            return;
        }
        let current_bar = self.bar_indices(max_auto);
        let current_over = self.overflow_indices(max_auto);
        self.pinned_keys = current_bar
            .into_iter()
            .map(|i| self.icons[i].key.clone())
            .collect();
        self.overflow_keys = current_over
            .into_iter()
            .map(|i| self.icons[i].key.clone())
            .collect();
        self.has_custom_prefs = true;
    }

    pub fn save_prefs(&self) {
        save_lines(&tray_pinned_path(), &self.pinned_keys);
        let mut hidden_vec: Vec<String> = self.overflow_keys.iter().cloned().collect();
        hidden_vec.sort();
        save_lines(&tray_hidden_path(), &hidden_vec);
    }

    /// Pins `key` onto the main bar (at `insert_slot` if specified, otherwise at the end) and saves.
    pub fn pin_on_bar_at(&mut self, key: &str, max_auto: usize, insert_slot: Option<usize>) {
        self.ensure_custom_prefs_seeded(max_auto);
        self.overflow_keys.remove(key);
        self.pinned_keys.retain(|k| k != key);
        let pos = insert_slot.map_or(self.pinned_keys.len(), |s| s.min(self.pinned_keys.len()));
        self.pinned_keys.insert(pos, key.to_string());
        self.save_prefs();
    }

    /// Moves `key` from the main bar into the hidden overflow flyout and saves.
    pub fn hide_to_overflow(&mut self, key: &str, max_auto: usize) {
        self.ensure_custom_prefs_seeded(max_auto);
        self.pinned_keys.retain(|k| k != key);
        self.overflow_keys.insert(key.to_string());
        self.save_prefs();
    }

    /// Reorders two visible bar slots live while dragging across `TopRight`.
    pub fn reorder_on_bar(&mut self, from_slot: usize, to_slot: usize, max_auto: usize) -> bool {
        if from_slot == to_slot {
            return false;
        }
        self.ensure_custom_prefs_seeded(max_auto);
        let bar = self.bar_indices(max_auto);
        if from_slot >= bar.len() || to_slot >= bar.len() {
            return false;
        }
        let mut bar_keys: Vec<String> = bar.into_iter().map(|i| self.icons[i].key.clone()).collect();
        let moved = bar_keys.remove(from_slot);
        bar_keys.insert(to_slot, moved);
        // Preserve any offline pinned keys after the live bar order
        for k in &self.pinned_keys {
            if !bar_keys.contains(k) {
                bar_keys.push(k.clone());
            }
        }
        self.pinned_keys = bar_keys;
        true
    }

    /// Toggles whether `key` is pinned directly on the bar vs tucked in the overflow flyout.
    pub fn toggle_pin_on_bar(&mut self, key: &str, currently_on_bar: bool, max_auto: usize) {
        if currently_on_bar {
            self.hide_to_overflow(key, max_auto);
        } else {
            self.pin_on_bar_at(key, max_auto, None);
        }
    }

    /// Dispatches native Explorer-identical tray notification messages (left, double-click, right-click).
    pub unsafe fn dispatch_click(&mut self, idx: usize, left: bool) {
        let Some(ti) = self.icons.get(idx) else {
            return;
        };
        let (hwnd, uid, cb, ver) = (ti.hwnd, ti.uid, ti.cb_msg, ti.version);
        if cb == 0 || hwnd.is_null() || IsWindow(hwnd) == 0 {
            return;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid != 0 {
            AllowSetForegroundWindow(pid);
        }
        let mut pt: POINT = zeroed();
        GetCursorPos(&mut pt);

        let post = |ev: u32| {
            if ver >= 4 {
                let wp = ((pt.x as u32 & 0xFFFF) | ((pt.y as u32 & 0xFFFF) << 16)) as WPARAM;
                let lp = ((ev & 0xFFFF) | ((uid & 0xFFFF) << 16)) as LPARAM;
                PostMessageW(hwnd, cb, wp, lp);
            } else {
                PostMessageW(hwnd, cb, uid as WPARAM, ev as LPARAM);
            }
        };

        let now = GetTickCount();
        let prev = if left {
            let old = self.last_click;
            self.last_click = (hwnd as usize, uid, now);
            old
        } else {
            (0, 0, 0)
        };
        let double = left
            && prev.0 == hwnd as usize
            && prev.1 == uid
            && now.wrapping_sub(prev.2) <= GetDoubleClickTime();
        if double {
            self.last_click = (0, 0, 0);
        }

        if left {
            post(WM_LBUTTONDOWN);
            post(WM_LBUTTONUP);
            if ver >= 4 {
                post(0x0400); // NIN_SELECT
            }
            if double {
                post(WM_LBUTTONDBLCLK);
            }
        } else {
            post(WM_RBUTTONDOWN);
            post(WM_RBUTTONUP);
            if ver >= 4 {
                post(WM_CONTEXTMENU);
            }
        }
    }
}

/// Explorer's built-in volume, network, and power icon GUIDs (`{7820AE73..75-23E3-4229-82C1-E41CB67D5B9C}`),
/// which the taskbar already renders directly in its control area.
fn is_builtin_system_tray_guid(guid: &[u8; 16]) -> bool {
    matches!(guid[0], 0x73..=0x75)
        && guid[1..] == [0xAE, 0x20, 0x78, 0xE3, 0x23, 0x29, 0x42, 0x82, 0xC1, 0xE4, 0x1C, 0xB6, 0x7D, 0x5B, 0x9C]
}

/// Finds the first top-level `Shell_TrayWnd` that is NOT our own `own_host` (i.e. Explorer's real taskbar).
pub unsafe fn find_other_shell_tray(own_host: HWND) -> HWND {
    let cls = wide("Shell_TrayWnd");
    let mut h = null_mut();
    loop {
        h = FindWindowExW(null_mut(), h, cls.as_ptr(), null());
        if h.is_null() || h != own_host {
            return h;
        }
    }
}

/// Broadcasts `TaskbarCreated` so running apps re-register their tray icons with the active `Shell_TrayWnd`.
pub unsafe fn broadcast_taskbar_created() {
    let msg = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
    if msg != 0 {
        PostMessageW(HWND_BROADCAST, msg, 0, 0);
    }
}

fn appdata_dir() -> PathBuf {
    env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir)
        .join("gp-pc-boost")
}

fn tray_pinned_path() -> PathBuf {
    appdata_dir().join("tray_pinned.txt")
}

fn tray_hidden_path() -> PathBuf {
    appdata_dir().join("tray_hidden.txt")
}

fn load_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
        .unwrap_or_default()
}

fn save_lines(path: &Path, lines: &[String]) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let _ = fs::write(path, lines.join("\n"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_explorer_guid_filter_matches_only_vol_net_power() {
        let vol = [0x73, 0xAE, 0x20, 0x78, 0xE3, 0x23, 0x29, 0x42, 0x82, 0xC1, 0xE4, 0x1C, 0xB6, 0x7D, 0x5B, 0x9C];
        let net = [0x74, 0xAE, 0x20, 0x78, 0xE3, 0x23, 0x29, 0x42, 0x82, 0xC1, 0xE4, 0x1C, 0xB6, 0x7D, 0x5B, 0x9C];
        let pwr = [0x75, 0xAE, 0x20, 0x78, 0xE3, 0x23, 0x29, 0x42, 0x82, 0xC1, 0xE4, 0x1C, 0xB6, 0x7D, 0x5B, 0x9C];
        let usb = [0x78, 0xAE, 0x20, 0x78, 0xE3, 0x23, 0x29, 0x42, 0x82, 0xC1, 0xE4, 0x1C, 0xB6, 0x7D, 0x5B, 0x9C];
        assert!(is_builtin_system_tray_guid(&vol));
        assert!(is_builtin_system_tray_guid(&net));
        assert!(is_builtin_system_tray_guid(&pwr));
        assert!(!is_builtin_system_tray_guid(&usb));
    }

    #[test]
    fn bar_and_overflow_partition_active_icons() {
        let mut mgr = TrayManager {
            icons: Vec::new(),
            pinned_keys: Vec::new(),
            overflow_keys: HashSet::new(),
            has_custom_prefs: false,
            last_click: (0, 0, 0),
        };
        let dummy = Arc::new(IconBitmap { w: 16, h: 16, pixels: vec![0xFFFF_FFFF; 256] });
        for i in 0..8usize {
            mgr.icons.push(TrayIcon {
                hwnd: (i + 1) as HWND,
                uid: i as u32,
                guid: [0; 16],
                cb_msg: 0x8001,
                version: 4,
                hidden: false,
                tip: format!("App {i}"),
                key: format!("app{i}.exe"),
                hicon: null_mut(),
                bitmap: Some(Arc::clone(&dummy)),
                bmp_px: 16,
            });
        }
        assert_eq!(mgr.bar_indices(6), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(mgr.overflow_indices(6), vec![6, 7]);

        // Dragging an overflow icon to main bar at slot 1 pins it there without losing existing icons
        mgr.ensure_custom_prefs_seeded(6);
        mgr.overflow_keys.remove("app6.exe");
        mgr.pinned_keys.insert(1, "app6.exe".into());
        assert_eq!(mgr.bar_indices(6), vec![0, 6, 1, 2, 3, 4, 5]);
        assert_eq!(mgr.overflow_indices(6), vec![7]);

        // Dragging an icon from main bar to hide moves ONLY that icon to overflow
        mgr.pinned_keys.retain(|k| k != "app0.exe");
        mgr.overflow_keys.insert("app0.exe".into());
        assert_eq!(mgr.bar_indices(6), vec![6, 1, 2, 3, 4, 5]);
        assert_eq!(mgr.overflow_indices(6), vec![0, 7]);

        // Live horizontal reorder on main bar
        assert!(mgr.reorder_on_bar(0, 2, 6));
        assert_eq!(mgr.bar_indices(6), vec![1, 2, 6, 3, 4, 5]);
    }
}
