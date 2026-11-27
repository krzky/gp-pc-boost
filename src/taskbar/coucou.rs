//! Native Coucou (Mochi) Assistant bridge for the Windows 12 center Dynamic Capsule.
//!
//! Reads Coucou's non-virtualized `%USERPROFILE%\.coucou\` runtime files off-thread
//! (`now-playing.json`, `notifications.json`, `widgets/{focus,todos,water}.json`),
//! detects whether Coucou is running, and sends instant commands over Coucou's
//! per-user named pipe (`\\.\pipe\coucou-<SID>`).

use crate::taskbar::windows::wide;
use std::{
    env, fs,
    io::Write,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{
        GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_WRITE, OPEN_EXISTING,
    },
    System::Threading::{GetCurrentProcess, OpenProcessToken},
    UI::{
        Shell::ShellExecuteW,
        WindowsAndMessaging::PostMessageW,
    },
};

#[link(name = "advapi32")]
unsafe extern "system" {
    fn ConvertSidToStringSidW(sid: *mut core::ffi::c_void, string_sid: *mut *mut u16) -> i32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LocalFree(hmem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    fn WaitNamedPipeW(lp_named_pipe_name: *const u16, n_time_out: u32) -> i32;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoucouNotice {
    pub app: String,
    pub title: String,
    pub body: String,
    pub time_ms: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoucouTodo {
    pub text: String,
    pub color: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoucouSnapshot {
    pub running: bool,
    pub media_playing: bool,
    pub media_title: String,
    pub media_artist: String,
    pub media_player: String,
    pub focus_phase: String, // "idle" | "focus" | "short_break" | "long_break"
    pub focus_remaining_sec: u32,
    pub focus_task: String,
    pub water_glasses: u32,
    pub water_due: bool,
    pub open_todos: usize,
    pub top_todos: Vec<CoucouTodo>,
    pub recent_notices: Vec<CoucouNotice>,
    pub latest_notice_ms: u64,
}

impl CoucouSnapshot {
    /// Produces the concise, context-aware label for the center capsule beside the Calendar button.
    pub fn pill_summary(&self) -> (String, CoucouMood) {
        if !self.running {
            return ("Coucou".into(), CoucouMood::Offline);
        }
        if self.focus_phase != "idle" && !self.focus_phase.is_empty() && self.focus_remaining_sec > 0 {
            let m = self.focus_remaining_sec / 60;
            let s = self.focus_remaining_sec % 60;
            let tag = if self.focus_phase == "focus" { "Focus" } else { "Break" };
            return (format!("{tag} {m:02}:{s:02}"), CoucouMood::Focus);
        }
        if self.media_playing && !self.media_title.is_empty() {
            return (truncate_chars(&self.media_title, 20), CoucouMood::Music);
        }
        if self.water_due {
            return (format!("Water · {}", self.water_glasses), CoucouMood::Water);
        }
        if self.open_todos > 0 {
            return (format!("Mochi · {} todo{}", self.open_todos, if self.open_todos == 1 { "" } else { "s" }), CoucouMood::Ready);
        }
        ("Mochi".into(), CoucouMood::Ready)
    }

    pub fn tooltip_text(&self) -> String {
        if !self.running {
            return "Coucou Assistant (Offline)  ·  Click to launch Mochi".into();
        }
        let mut parts = vec!["Coucou (Mochi)".to_string()];
        if self.focus_phase != "idle" && !self.focus_phase.is_empty() && self.focus_remaining_sec > 0 {
            let m = self.focus_remaining_sec / 60;
            let s = self.focus_remaining_sec % 60;
            parts.push(format!("Focus {m:02}:{s:02}"));
        }
        if self.media_playing && !self.media_title.is_empty() {
            if self.media_artist.is_empty() {
                parts.push(format!("Playing: {}", self.media_title));
            } else {
                parts.push(format!("Playing: {} — {}", self.media_title, self.media_artist));
            }
        }
        if self.open_todos > 0 {
            parts.push(format!("{} open todos", self.open_todos));
        }
        parts.push("Click: Expand Mochi  ·  Middle: Play/Pause or Toggle  ·  Right: Menu".into());
        parts.join("  ·  ")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CoucouMood {
    Offline,
    Ready,
    Music,
    Focus,
    Water,
}

pub struct CoucouState {
    snap: Mutex<CoucouSnapshot>,
    pub host_hwnd: AtomicIsize,
    pub stop: AtomicBool,
}

impl Default for CoucouState {
    fn default() -> Self {
        Self::new()
    }
}

impl CoucouState {
    pub fn new() -> Self {
        Self {
            snap: Mutex::new(CoucouSnapshot::default()),
            host_hwnd: AtomicIsize::new(0),
            stop: AtomicBool::new(false),
        }
    }

    pub fn snapshot(&self) -> CoucouSnapshot {
        self.snap.lock().unwrap().clone()
    }
}

pub fn coucou_home_dir() -> Option<PathBuf> {
    env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".coucou"))
}

fn current_user_sid() -> Option<String> {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 || token.is_null() {
            return None;
        }
        let mut needed = 0u32;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
        if needed == 0 {
            CloseHandle(token);
            return None;
        }
        let mut buf = vec![0u8; needed as usize];
        let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr() as *mut _, needed, &mut needed) != 0;
        CloseHandle(token);
        if !ok {
            return None;
        }
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut wide_sid: *mut u16 = null_mut();
        if ConvertSidToStringSidW(tu.User.Sid, &mut wide_sid) == 0 || wide_sid.is_null() {
            return None;
        }
        let mut len = 0usize;
        while *wide_sid.add(len) != 0 {
            len += 1;
        }
        let s = String::from_utf16_lossy(std::slice::from_raw_parts(wide_sid, len));
        LocalFree(wide_sid as *mut _);
        Some(s)
    }
}

pub fn coucou_pipe_name() -> String {
    let key = current_user_sid()
        .unwrap_or_else(|| env::var("USERNAME").unwrap_or_else(|_| "user".into()));
    format!(r"\\.\pipe\coucou-{key}")
}

/// Fast non-blocking probe checking whether Coucou's named pipe server is alive.
pub fn is_coucou_pipe_alive() -> bool {
    let name = coucou_pipe_name();
    let w: Vec<u16> = std::ffi::OsStr::new(&name).encode_wide().chain([0]).collect();
    unsafe { WaitNamedPipeW(w.as_ptr(), 15) != 0 }
}

/// Sends a lightweight JSON command to Coucou over `\\.\pipe\coucou-<SID>` on a worker thread
/// so the taskbar UI thread never blocks even if Coucou is busy.
pub fn send_coucou_cmd_async(cmd: &'static str) {
    let payload = format!("{{\"taskbar_cmd\":\"{cmd}\"}}\n");
    send_coucou_json_async(payload);
}

pub fn send_coucou_json_async(payload: String) {
    thread::spawn(move || {
        let _ = send_coucou_json_sync(&payload);
    });
}

pub fn send_coucou_cmd_sync(cmd: &str) -> bool {
    let payload = format!("{{\"taskbar_cmd\":\"{cmd}\"}}\n");
    send_coucou_json_sync(&payload)
}

pub fn send_coucou_json_sync(payload: &str) -> bool {
    let name = coucou_pipe_name();
    let w: Vec<u16> = std::ffi::OsStr::new(&name).encode_wide().chain([0]).collect();
    unsafe {
        if WaitNamedPipeW(w.as_ptr(), 120) == 0 {
            return false;
        }
        let h = CreateFileW(
            w.as_ptr(),
            FILE_GENERIC_WRITE,
            0,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        );
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            return false;
        }
        use std::os::windows::io::FromRawHandle;
        let mut f = fs::File::from_raw_handle(h as _);
        let line = if payload.ends_with('\n') {
            payload.to_string()
        } else {
            format!("{payload}\n")
        };
        f.write_all(line.as_bytes()).is_ok() && f.flush().is_ok()
    }
}

/// Launches the installed Coucou MSIX app via its AUMID (or falls back to the dev release binary).
pub unsafe fn launch_coucou() {
    let aumid = wide(r"shell:AppsFolder\GemaPro.Coucou_p72brzk738wa6!App");
    let r = ShellExecuteW(null_mut(), wide("open").as_ptr(), aumid.as_ptr(), null(), null(), 1) as isize;
    if r <= 32 {
        let fallback = PathBuf::from(r"D:\Project\coucou\windows\src-tauri\target\release\coucou.exe");
        if fallback.is_file() {
            let wp = wide(&fallback.to_string_lossy());
            ShellExecuteW(null_mut(), wide("open").as_ptr(), wp.as_ptr(), null(), null(), 1);
        }
    }
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let taken: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{taken}…")
    } else {
        taken
    }
}

fn read_snapshot_from_dir(dir: &Path, running: bool, now_ms: u64) -> CoucouSnapshot {
    let mut snap = CoucouSnapshot {
        running,
        focus_phase: "idle".into(),
        ..CoucouSnapshot::default()
    };

    if let Ok(text) = fs::read_to_string(dir.join("now-playing.json")) {
        parse_now_playing(&text, &mut snap);
    }
    let widgets = dir.join("widgets");
    if let Ok(text) = fs::read_to_string(widgets.join("focus.json")) {
        parse_focus(&text, now_ms, &mut snap);
    }
    if let Ok(text) = fs::read_to_string(widgets.join("water.json")) {
        parse_water(&text, &mut snap);
    }
    if let Ok(text) = fs::read_to_string(widgets.join("todos.json")) {
        parse_todos(&text, &mut snap);
    }
    if let Ok(text) = fs::read_to_string(dir.join("notifications.json")) {
        parse_notifications(&text, &mut snap);
    }
    snap
}

pub fn spawn_poller(state: Arc<CoucouState>, notify_msg: u32) {
    thread::spawn(move || unsafe {
        let mut last = state.snapshot();
        loop {
            if state.stop.load(Relaxed) {
                break;
            }
            let running = is_coucou_pipe_alive();
            let next = if let Some(dir) = coucou_home_dir() {
                read_snapshot_from_dir(&dir, running, now_unix_ms())
            } else {
                CoucouSnapshot { running, ..CoucouSnapshot::default() }
            };
            if next != last {
                *state.snap.lock().unwrap() = next.clone();
                last = next;
                let hwnd = state.host_hwnd.load(Relaxed);
                if hwnd != 0 {
                    PostMessageW(hwnd as _, notify_msg, 0, 0);
                }
            }
            // Tick faster (1s) when a Focus timer is actively counting down, else gentle 1.5s poll
            let sleep_ms = if last.focus_phase != "idle" && last.focus_remaining_sec > 0 { 1000 } else { 1500 };
            thread::sleep(Duration::from_millis(sleep_ms));
        }
    });
}

// ---------- lightweight, zero-dependency JSON extractors for Coucou's well-known state files ----------

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

/// Parses a JSON string literal starting at `b[i] == b'"'`, returning `(unescaped, next_index)`.
fn parse_json_str(b: &[u8], i: usize) -> Option<(String, usize)> {
    if i >= b.len() || b[i] != b'"' {
        return None;
    }
    let mut out = String::new();
    let mut idx = i + 1;
    while idx < b.len() {
        let c = b[idx];
        if c == b'"' {
            return Some((out, idx + 1));
        }
        if c == b'\\' && idx + 1 < b.len() {
            idx += 1;
            match b[idx] {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'n' => out.push(' '),
                b'r' => {}
                b't' => out.push(' '),
                b'u' if idx + 4 < b.len() => {
                    if let Ok(hex) = std::str::from_utf8(&b[idx + 1..idx + 5])
                        && let Ok(code) = u32::from_str_radix(hex, 16)
                        && let Some(ch) = char::from_u32(code)
                    {
                        out.push(ch);
                    }
                    idx += 4;
                }
                other => out.push(other as char),
            }
            idx += 1;
        } else {
            let s = std::str::from_utf8(&b[idx..]).ok()?;
            let ch = s.chars().next()?;
            out.push(ch);
            idx += ch.len_utf8();
        }
    }
    None
}

fn find_key_value<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{key}\"");
    let b = text.as_bytes();
    let mut search_from = 0usize;
    while let Some(rel) = text[search_from..].find(&needle) {
        let after_key = search_from + rel + needle.len();
        let colon = skip_ws(b, after_key);
        if colon < b.len() && b[colon] == b':' {
            let val_start = skip_ws(b, colon + 1);
            return Some(&text[val_start..]);
        }
        search_from = after_key;
    }
    None
}

fn json_str_field(text: &str, key: &str) -> Option<String> {
    let tail = find_key_value(text, key)?;
    if tail.starts_with("null") {
        return None;
    }
    parse_json_str(tail.as_bytes(), 0).map(|(s, _)| s)
}

fn json_bool_field(text: &str, key: &str) -> Option<bool> {
    let tail = find_key_value(text, key)?;
    if tail.starts_with("true") {
        Some(true)
    } else if tail.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

fn json_u64_field(text: &str, key: &str) -> Option<u64> {
    let tail = find_key_value(text, key)?;
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() { None } else { digits.parse().ok() }
}

/// Splits top-level `{ ... }` object slices inside a JSON array slice starting at `[`.
fn split_json_objects(array_slice: &str) -> Vec<&str> {
    let b = array_slice.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut obj_start: Option<usize> = None;

    while i < b.len() {
        let c = b[i];
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'{' => {
                if depth == 0 {
                    obj_start = Some(i);
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0
                    && let Some(st) = obj_start.take()
                {
                    out.push(&array_slice[st..=i]);
                }
            }
            b']' if depth == 0 => break,
            _ => {}
        }
        i += 1;
    }
    out
}

pub fn parse_now_playing(text: &str, snap: &mut CoucouSnapshot) {
    if find_key_value(text, "session").is_some_and(|v| v.starts_with("null")) {
        return;
    }
    snap.media_playing = json_bool_field(text, "playing").unwrap_or(false);
    snap.media_title = json_str_field(text, "title").unwrap_or_default();
    snap.media_artist = json_str_field(text, "artist").unwrap_or_default();
    snap.media_player = json_str_field(text, "player").unwrap_or_default();
}

pub fn parse_focus(text: &str, now_ms: u64, snap: &mut CoucouSnapshot) {
    snap.focus_phase = json_str_field(text, "phase").unwrap_or_else(|| "idle".into());
    snap.focus_task = json_str_field(text, "task").unwrap_or_default();
    if let Some(ends) = json_u64_field(text, "ends_at_ms") {
        snap.focus_remaining_sec = (ends.saturating_sub(now_ms) / 1000) as u32;
    } else if let Some(rem) = json_u64_field(text, "paused_remaining_ms") {
        snap.focus_remaining_sec = (rem / 1000) as u32;
    } else {
        snap.focus_remaining_sec = 0;
    }
}

pub fn parse_water(text: &str, snap: &mut CoucouSnapshot) {
    snap.water_glasses = json_u64_field(text, "glasses").unwrap_or(0) as u32;
    let alerted = json_u64_field(text, "alerted_at_ms").unwrap_or(0);
    let last_drink = json_u64_field(text, "last_drink_ms").unwrap_or(0);
    let deferred = json_bool_field(text, "deferred").unwrap_or(false);
    snap.water_due = !deferred && alerted > 0 && alerted > last_drink;
}

pub fn parse_todos(text: &str, snap: &mut CoucouSnapshot) {
    let Some(tail) = find_key_value(text, "items") else {
        return;
    };
    let Some(arr_pos) = tail.find('[') else {
        return;
    };
    let objs = split_json_objects(&tail[arr_pos..]);
    let mut open = 0usize;
    let mut top = Vec::new();
    for obj in objs {
        let done = json_bool_field(obj, "done").unwrap_or(false);
        if !done {
            open += 1;
            if top.len() < 4
                && let Some(t) = json_str_field(obj, "text")
                && !t.trim().is_empty()
            {
                top.push(CoucouTodo {
                    text: t.trim().to_string(),
                    color: json_str_field(obj, "color").unwrap_or_default(),
                });
            }
        }
    }
    snap.open_todos = open;
    snap.top_todos = top;
}

pub fn parse_notifications(text: &str, snap: &mut CoucouSnapshot) {
    let Some(arr_pos) = text.find('[') else {
        return;
    };
    let objs = split_json_objects(&text[arr_pos..]);
    let mut notices = Vec::new();
    for obj in objs.into_iter().take(4) {
        let app = json_str_field(obj, "app").unwrap_or_default();
        let title = json_str_field(obj, "title").unwrap_or_default();
        let body = json_str_field(obj, "body").unwrap_or_default();
        let time_ms = json_u64_field(obj, "time").unwrap_or(0);
        if time_ms > snap.latest_notice_ms {
            snap.latest_notice_ms = time_ms;
        }
        if !app.is_empty() || !title.is_empty() {
            notices.push(CoucouNotice { app, title, body, time_ms });
        }
    }
    snap.recent_notices = notices;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_coucou_live_json_files_cleanly() {
        let mut snap = CoucouSnapshot { running: true, ..Default::default() };
        parse_now_playing(
            r#"{"session":{"title":"Midnight City","artist":"M83","player":"GPBrowser","playing":true}}"#,
            &mut snap,
        );
        assert!(snap.media_playing);
        assert_eq!(snap.media_title, "Midnight City");
        assert_eq!(snap.media_artist, "M83");
        assert_eq!(snap.pill_summary(), ("Midnight City".into(), CoucouMood::Music));

        parse_focus(
            r#"{"phase":"focus","ends_at_ms":1700000125000,"task":"Ship Coucou"}"#,
            1700000000000,
            &mut snap,
        );
        assert_eq!(snap.focus_phase, "focus");
        assert_eq!(snap.focus_remaining_sec, 125);
        assert_eq!(snap.pill_summary(), ("Focus 02:05".into(), CoucouMood::Focus));

        parse_todos(
            r#"{"rev":9,"items":[{"text":"Done task","done":true},{"text":"Review PR","done":false,"color":"amber"},{"text":"Test Mochi","done":false,"color":"blue"}]}"#,
            &mut snap,
        );
        assert_eq!(snap.open_todos, 2);
        assert_eq!(snap.top_todos.len(), 2);
        assert_eq!(snap.top_todos[0].text, "Review PR");

        parse_notifications(
            r#"[{"id":1,"app":"Claude","title":"Build OK","body":"All green","time":99999}]"#,
            &mut snap,
        );
        assert_eq!(snap.recent_notices.len(), 1);
        assert_eq!(snap.recent_notices[0].app, "Claude");
        assert_eq!(snap.latest_notice_ms, 99999);
    }
}
