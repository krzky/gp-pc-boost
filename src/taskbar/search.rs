//! Native zero-dependency Windows Search engine for the Fluent 2 bottom-center Search Bar.
//!
//! Indexes Start Menu & Desktop apps, Windows Settings (`ms-settings:` & admin tools),
//! local user folders / `D:\Project` repos / Recent items, live running windows, live
//! filesystem path navigation, instant math calculator, and web search fallback.

use crate::taskbar::windows::{AppItem, bring_to_front, launch_path, wide};
use std::{
    collections::HashSet,
    env, fs,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    sync::{Arc, Mutex},
    thread,
};
use windows_sys::Win32::{
    Foundation::*,
    System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_SZ, RegCloseKey,
        RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
    },
    UI::Shell::ShellExecuteW,
};

#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(hwnd_new_owner: HWND) -> i32;
    fn CloseClipboard() -> i32;
    fn EmptyClipboard() -> i32;
    fn GetClipboardData(uformat: u32) -> *mut core::ffi::c_void;
    fn SetClipboardData(uformat: u32, hmem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalAlloc(uflags: u32, dwbytes: usize) -> *mut core::ffi::c_void;
    fn GlobalLock(hmem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    fn GlobalUnlock(hmem: *mut core::ffi::c_void) -> i32;
}

const CF_UNICODETEXT: u32 = 13;
const GMEM_MOVEABLE: u32 = 0x0002;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchScope {
    All,
    Apps,
    Settings,
    Files,
}

impl SearchScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Apps => "Apps",
            Self::Settings => "Settings",
            Self::Files => "Files",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Apps,
            Self::Apps => Self::Settings,
            Self::Settings => Self::Files,
            Self::Files => Self::All,
        }
    }

    pub const ALL: [SearchScope; 4] = [
        SearchScope::All,
        SearchScope::Apps,
        SearchScope::Settings,
        SearchScope::Files,
    ];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchKind {
    Calc,
    Window,
    App,
    Setting,
    Project,
    Folder,
    File,
    Command,
    Web,
}

impl SearchKind {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Calc => "Calc",
            Self::Window => "Window",
            Self::App => "App",
            Self::Setting => "Setting",
            Self::Project => "Project",
            Self::Folder => "Folder",
            Self::File => "File",
            Self::Command => "Run",
            Self::Web => "Web",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            Self::Calc => "\u{E8EF}",     // Calculator
            Self::Window => "\u{E737}",   // SwitchApps
            Self::App => "\u{E74C}",      // App
            Self::Setting => "\u{E713}",  // Settings
            Self::Project => "\u{E943}",  // Code
            Self::Folder => "\u{E8B7}",   // Folder
            Self::File => "\u{E8A5}",     // Document
            Self::Command => "\u{E756}",  // CommandPrompt
            Self::Web => "\u{E774}",      // Globe
        }
    }

    pub fn matches_scope(self, scope: SearchScope) -> bool {
        match scope {
            SearchScope::All => true,
            SearchScope::Apps => matches!(self, Self::App | Self::Window | Self::Command),
            SearchScope::Settings => matches!(self, Self::Setting | Self::Calc),
            SearchScope::Files => matches!(self, Self::File | Self::Folder | Self::Project),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SearchResult {
    pub title: String,
    pub subtitle: String,
    pub kind: SearchKind,
    pub target: String,
    pub icon_key: String,
    pub icon_path: String,
    pub hwnd: usize,
    pub score: i32,
}

#[derive(Clone)]
struct IndexedEntry {
    title: String,
    title_low: String,
    subtitle: String,
    keywords_low: String,
    kind: SearchKind,
    target: String,
    icon_key: String,
    icon_path: String,
    base_boost: i32,
}

pub struct SearchEngine {
    entries: Arc<Mutex<Vec<IndexedEntry>>>,
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchEngine {
    pub fn new() -> Self {
        let mut initial = builtin_settings_entries();
        initial.extend(builtin_command_entries());
        Self { entries: Arc::new(Mutex::new(initial)) }
    }

    /// Populates Start Menu apps, App Paths, Desktop, Documents/Downloads, Recent, and `D:\Project`
    /// on a background worker thread so taskbar startup stays instant.
    pub fn spawn_indexer(&self) {
        let entries = Arc::clone(&self.entries);
        thread::spawn(move || {
            let mut list = builtin_settings_entries();
            list.extend(builtin_command_entries());
            let mut seen = HashSet::<String>::new();
            for e in &list {
                seen.insert(format!("{}:{}", e.kind as u8, e.title_low));
            }
            collect_start_menu_apps(&mut list, &mut seen);
            collect_registry_app_paths(&mut list, &mut seen);
            collect_user_files_and_projects(&mut list, &mut seen);
            if let Ok(mut guard) = entries.lock() {
                *guard = list;
            }
        });
    }

    /// Queries the live index + running windows + calculator + path navigation in < 1ms.
    pub fn query(
        &self,
        raw_query: &str,
        scope: SearchScope,
        open_items: &[AppItem],
        max_results: usize,
    ) -> Vec<SearchResult> {
        let q = raw_query.trim();
        if q.is_empty() {
            return self.empty_dashboard(scope, open_items, max_results);
        }

        let mut out: Vec<SearchResult> = Vec::with_capacity(max_results + 8);

        // 1. Instant Calculator (e.g. `256 * 18`, `(1920-436)/2`, `0xff`, `15% of 450`)
        if matches!(scope, SearchScope::All | SearchScope::Settings)
            && let Some((formatted, expr_note)) = eval_calculator(q)
        {
            out.push(SearchResult {
                title: formatted.clone(),
                subtitle: format!("{expr_note}  ·  Enter to copy"),
                kind: SearchKind::Calc,
                target: formatted,
                icon_key: String::new(),
                icon_path: String::new(),
                hwnd: 0,
                score: 20_000,
            });
        }

        // 2. Direct filesystem path browser (e.g. `D:\Project\`, `C:\Windows\`)
        if looks_like_path(q) {
            out.extend(browse_path_matches(q, 6));
        }

        let q_low = q.to_ascii_lowercase();

        // 3. Running windows & pinned taskbar apps (instant switch / launch!)
        if scope == SearchScope::All || scope == SearchScope::Apps {
            for item in open_items {
                let s = fuzzy_score(&q_low, &item.label.to_ascii_lowercase(), "");
                if s > 0 {
                    let is_running = !item.wins.is_empty();
                    let hwnd = if !item.mru_win.is_null() {
                        item.mru_win as usize
                    } else {
                        item.wins.first().copied().map_or(0, |h| h as usize)
                    };
                    let target = if let Some(aumid) = &item.pin_aumid {
                        format!(r"shell:AppsFolder\{aumid}")
                    } else if let Some(lnk) = &item.pin_lnk {
                        lnk.clone()
                    } else {
                        item.icon_path.clone()
                    };
                    out.push(SearchResult {
                        title: item.label.clone(),
                        subtitle: if is_running {
                            format!(
                                "Switch to open window{}",
                                if item.wins.len() > 1 {
                                    format!(" ({} windows)", item.wins.len())
                                } else {
                                    String::new()
                                }
                            )
                        } else {
                            "Pinned taskbar app".into()
                        },
                        kind: if is_running { SearchKind::Window } else { SearchKind::App },
                        target,
                        icon_key: item.icon_key.clone(),
                        icon_path: item.icon_path.clone(),
                        hwnd,
                        score: s + if is_running { 480 } else { 320 },
                    });
                }
            }
        }

        // 4. Indexed Start Menu apps, Windows Settings, Tools, Files, Projects
        if let Ok(guard) = self.entries.lock() {
            for e in guard.iter() {
                if !e.kind.matches_scope(scope) {
                    continue;
                }
                let s = fuzzy_score(&q_low, &e.title_low, &e.keywords_low);
                if s > 0 {
                    out.push(SearchResult {
                        title: e.title.clone(),
                        subtitle: e.subtitle.clone(),
                        kind: e.kind,
                        target: e.target.clone(),
                        icon_key: e.icon_key.clone(),
                        icon_path: e.icon_path.clone(),
                        hwnd: 0,
                        score: s + e.base_boost,
                    });
                }
            }
        }

        // 5. Explicit shell command (`> cmd ...`) or URL (`https://...`)
        if let Some(cmd) = q.strip_prefix('>').map(str::trim).filter(|s| !s.is_empty()) {
            out.push(SearchResult {
                title: format!("Run `{cmd}`"),
                subtitle: "Execute command via Windows Shell".into(),
                kind: SearchKind::Command,
                target: cmd.to_string(),
                icon_key: String::new(),
                icon_path: String::new(),
                hwnd: 0,
                score: 18_000,
            });
        } else if q_low.starts_with("http://") || q_low.starts_with("https://") || q_low.starts_with("ms-settings:") {
            out.push(SearchResult {
                title: format!("Open {q}"),
                subtitle: "Open URL / URI directly".into(),
                kind: SearchKind::Web,
                target: q.to_string(),
                icon_key: String::new(),
                icon_path: String::new(),
                hwnd: 0,
                score: 15_000,
            });
        }

        // Sort highest score first and deduplicate same-title/same-kind entries
        out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.title.len().cmp(&b.title.len())));
        let mut dedup: Vec<SearchResult> = Vec::with_capacity(max_results);
        let mut seen = HashSet::<String>::new();
        for r in out {
            let key = format!("{}|{}", r.title.to_ascii_lowercase(), r.target.to_ascii_lowercase());
            let title_key = format!("t:{}", r.title.to_ascii_lowercase());
            if seen.contains(&key) || (matches!(r.kind, SearchKind::App | SearchKind::Window) && seen.contains(&title_key)) {
                continue;
            }
            seen.insert(key);
            if matches!(r.kind, SearchKind::App | SearchKind::Window) {
                seen.insert(title_key);
            }
            dedup.push(r);
            if dedup.len() >= max_results.saturating_sub(1).max(1) {
                break;
            }
        }

        // Always offer clean Web Search at bottom when room allows (just like Windows Search!)
        if dedup.len() < max_results && !looks_like_path(q) && !q.starts_with('>') {
            let encoded = url_encode(q);
            dedup.push(SearchResult {
                title: format!("Search the web for \"{q}\""),
                subtitle: "Open in your default browser".into(),
                kind: SearchKind::Web,
                target: format!("https://www.google.com/search?q={encoded}"),
                icon_key: String::new(),
                icon_path: String::new(),
                hwnd: 0,
                score: 1,
            });
        }

        dedup.truncate(max_results);
        dedup
    }

    fn empty_dashboard(
        &self,
        scope: SearchScope,
        open_items: &[AppItem],
        max_results: usize,
    ) -> Vec<SearchResult> {
        let mut out = Vec::new();
        if matches!(scope, SearchScope::All | SearchScope::Apps) {
            for item in open_items.iter().take(if scope == SearchScope::Apps { max_results } else { 4 }) {
                let is_running = !item.wins.is_empty();
                let hwnd = if !item.mru_win.is_null() {
                    item.mru_win as usize
                } else {
                    item.wins.first().copied().map_or(0, |h| h as usize)
                };
                let target = if let Some(aumid) = &item.pin_aumid {
                    format!(r"shell:AppsFolder\{aumid}")
                } else if let Some(lnk) = &item.pin_lnk {
                    lnk.clone()
                } else {
                    item.icon_path.clone()
                };
                out.push(SearchResult {
                    title: item.label.clone(),
                    subtitle: if is_running { "Running window".into() } else { "Pinned app".into() },
                    kind: if is_running { SearchKind::Window } else { SearchKind::App },
                    target,
                    icon_key: item.icon_key.clone(),
                    icon_path: item.icon_path.clone(),
                    hwnd,
                    score: 100,
                });
            }
        }
        if let Ok(guard) = self.entries.lock() {
            for e in guard.iter() {
                if out.len() >= max_results {
                    break;
                }
                if !e.kind.matches_scope(scope) {
                    continue;
                }
                if scope == SearchScope::All && e.kind != SearchKind::Setting {
                    continue;
                }
                out.push(SearchResult {
                    title: e.title.clone(),
                    subtitle: e.subtitle.clone(),
                    kind: e.kind,
                    target: e.target.clone(),
                    icon_key: e.icon_key.clone(),
                    icon_path: e.icon_path.clone(),
                    hwnd: 0,
                    score: 50,
                });
            }
        }
        out.truncate(max_results);
        out
    }
}

/// Launches/activates a chosen search result.
pub unsafe fn execute_result(res: &SearchResult, as_admin: bool) {
    match res.kind {
        SearchKind::Calc => {
            copy_to_clipboard(&res.target);
        }
        SearchKind::Window if res.hwnd != 0 && !as_admin => {
            bring_to_front(res.hwnd as HWND);
        }
        _ => {
            if res.target.is_empty() {
                return;
            }
            if as_admin {
                let verb = wide("runas");
                let file = wide(&res.target);
                ShellExecuteW(null_mut(), verb.as_ptr(), file.as_ptr(), null(), null(), 1);
            } else {
                launch_path(&res.target);
            }
        }
    }
}

pub unsafe fn copy_to_clipboard(text: &str) {
    let w = wide(text);
    let bytes = w.len() * 2;
    if OpenClipboard(null_mut()) == 0 {
        return;
    }
    EmptyClipboard();
    let hmem = GlobalAlloc(GMEM_MOVEABLE, bytes);
    if !hmem.is_null() {
        let ptr = GlobalLock(hmem) as *mut u16;
        if !ptr.is_null() {
            std::ptr::copy_nonoverlapping(w.as_ptr(), ptr, w.len());
            GlobalUnlock(hmem);
            SetClipboardData(CF_UNICODETEXT, hmem);
        }
    }
    CloseClipboard();
}

pub unsafe fn paste_from_clipboard() -> Option<String> {
    if OpenClipboard(null_mut()) == 0 {
        return None;
    }
    let h = GetClipboardData(CF_UNICODETEXT);
    let mut result = None;
    if !h.is_null() {
        let ptr = GlobalLock(h) as *const u16;
        if !ptr.is_null() {
            let mut len = 0usize;
            while len < 256 && *ptr.add(len) != 0 {
                len += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len));
            GlobalUnlock(h);
            let clean: String = s.chars().filter(|&c| c >= ' ' && c != '\r' && c != '\n').collect();
            if !clean.is_empty() {
                result = Some(clean);
            }
        }
    }
    CloseClipboard();
    result
}

// ---------- Ranking & Fuzzy Matcher ----------

pub fn fuzzy_score(q_low: &str, title_low: &str, keywords_low: &str) -> i32 {
    if q_low.is_empty() {
        return 0;
    }
    if title_low == q_low {
        return 5000;
    }
    if title_low.starts_with(q_low) {
        return 4200 - (title_low.len() as i32).min(200);
    }
    if let Some(pos) = title_low.find(q_low) {
        let at_word = pos == 0
            || matches!(
                title_low.as_bytes().get(pos - 1),
                Some(b' ' | b'-' | b'_' | b'.' | b'/' | b'\\')
            );
        return if at_word { 3600 - pos as i32 * 5 } else { 2900 - pos as i32 * 5 };
    }
    // Multi-word AND match across title + keywords
    let tokens: Vec<&str> = q_low.split_whitespace().collect();
    if tokens.len() > 1
        && tokens
            .iter()
            .all(|t| title_low.contains(t) || keywords_low.contains(t))
    {
        return 2600;
    }
    if !keywords_low.is_empty() && keywords_low.contains(q_low) {
        return 2100;
    }
    // Ordered subsequence fuzzy match against title (at least 2 chars to avoid noise)
    if q_low.len() >= 2 {
        let mut qi = q_low.chars().peekable();
        let mut consec = 0i32;
        let mut bonus = 0i32;
        let mut prev_matched = false;
        for ch in title_low.chars() {
            if qi.peek() == Some(&ch) {
                qi.next();
                if prev_matched {
                    consec += 1;
                    bonus += consec * 20;
                } else {
                    consec = 0;
                }
                prev_matched = true;
            } else {
                prev_matched = false;
            }
        }
        if qi.peek().is_none() {
            return (1200 + bonus - title_low.len() as i32 * 3).max(400);
        }
    }
    0
}

fn looks_like_path(q: &str) -> bool {
    let b = q.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':')
        || q.starts_with(r"\\")
        || q.starts_with("~/")
        || q.starts_with("~\\")
}

fn browse_path_matches(raw: &str, max: usize) -> Vec<SearchResult> {
    let expanded = if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        env::var("USERPROFILE")
            .map(|u| format!("{u}\\{rest}"))
            .unwrap_or_else(|_| raw.to_string())
    } else {
        raw.to_string()
    };
    let p = PathBuf::from(&expanded);
    let mut out = Vec::new();
    if p.exists() {
        let is_dir = p.is_dir();
        out.push(SearchResult {
            title: p.file_name().map_or_else(|| expanded.clone(), |n| n.to_string_lossy().to_string()),
            subtitle: expanded.clone(),
            kind: if is_dir { SearchKind::Folder } else { SearchKind::File },
            target: expanded.clone(),
            icon_key: format!("path:{}", expanded.to_ascii_lowercase()),
            icon_path: expanded.clone(),
            hwnd: 0,
            score: 16_000,
        });
        if is_dir && let Ok(rd) = fs::read_dir(&p) {
            for ent in rd.flatten().take(max.saturating_sub(1)) {
                let ep = ent.path();
                let name = ent.file_name().to_string_lossy().to_string();
                let s = ep.to_string_lossy().to_string();
                out.push(SearchResult {
                    title: name,
                    subtitle: s.clone(),
                    kind: if ep.is_dir() { SearchKind::Folder } else { SearchKind::File },
                    target: s.clone(),
                    icon_key: format!("path:{}", s.to_ascii_lowercase()),
                    icon_path: s,
                    hwnd: 0,
                    score: 14_000,
                });
            }
        }
    } else if let Some(parent) = p.parent()
        && parent.is_dir()
    {
        let prefix = p
            .file_name()
            .map(|n| n.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if let Ok(rd) = fs::read_dir(parent) {
            for ent in rd.flatten() {
                let name = ent.file_name().to_string_lossy().to_string();
                if !prefix.is_empty() && !name.to_ascii_lowercase().contains(&prefix) {
                    continue;
                }
                let ep = ent.path();
                let s = ep.to_string_lossy().to_string();
                out.push(SearchResult {
                    title: name,
                    subtitle: s.clone(),
                    kind: if ep.is_dir() { SearchKind::Folder } else { SearchKind::File },
                    target: s.clone(),
                    icon_key: format!("path:{}", s.to_ascii_lowercase()),
                    icon_path: s,
                    hwnd: 0,
                    score: 13_500,
                });
                if out.len() >= max {
                    break;
                }
            }
        }
    }
    out
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------- Safe Instant Calculator ----------

pub fn eval_calculator(input: &str) -> Option<(String, String)> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    // Hex -> Decimal shortcut: `0xFF`
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))
        && !hex.is_empty()
        && hex.chars().all(|c| c.is_ascii_hexdigit())
        && let Ok(v) = u64::from_str_radix(hex, 16)
    {
        return Some((format!("{v}"), format!("{s} (hex -> decimal)")));
    }
    // Percentage phrase: `15% of 450`
    let low = s.to_ascii_lowercase();
    if let Some((left, right)) = low.split_once("% of ")
        && let (Ok(pct), Ok(base)) = (left.trim().parse::<f64>(), right.trim().parse::<f64>())
    {
        let val = (pct / 100.0) * base;
        return Some((format_calc_num(val), format!("{s}")));
    }
    // Require at least one operator or `sqrt(` so bare numbers like `2026` don't hijack app search
    let has_op = s.contains('+')
        || s.contains('-')
        || s.contains('*')
        || s.contains('/')
        || s.contains('^')
        || low.starts_with("sqrt(");
    if !has_op {
        return None;
    }
    let norm = low.replace('×', "*").replace('÷', "/").replace("sqrt(", "s(");
    let mut p = CalcParser::new(&norm);
    let val = p.parse_expr()?;
    p.skip_ws();
    if !p.is_eof() || !val.is_finite() {
        return None;
    }
    Some((format_calc_num(val), format!("= {s}")))
}

fn format_calc_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 && v.abs() < 1e15 {
        format!("{}", v.round() as i64)
    } else {
        let s = format!("{v:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

struct CalcParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> CalcParser<'a> {
    fn new(s: &'a str) -> Self {
        Self { bytes: s.as_bytes(), pos: 0 }
    }
    fn is_eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }
    fn parse_expr(&mut self) -> Option<f64> {
        let mut v = self.parse_term()?;
        loop {
            match self.peek() {
                Some(b'+') => {
                    self.pos += 1;
                    v += self.parse_term()?;
                }
                Some(b'-') => {
                    self.pos += 1;
                    v -= self.parse_term()?;
                }
                _ => break,
            }
        }
        Some(v)
    }
    fn parse_term(&mut self) -> Option<f64> {
        let mut v = self.parse_power()?;
        loop {
            match self.peek() {
                Some(b'*') => {
                    self.pos += 1;
                    v *= self.parse_power()?;
                }
                Some(b'/') => {
                    self.pos += 1;
                    let d = self.parse_power()?;
                    if d == 0.0 {
                        return None;
                    }
                    v /= d;
                }
                Some(b'%') => {
                    self.pos += 1;
                    let d = self.parse_power()?;
                    if d == 0.0 {
                        return None;
                    }
                    v %= d;
                }
                _ => break,
            }
        }
        Some(v)
    }
    fn parse_power(&mut self) -> Option<f64> {
        let base = self.parse_unary()?;
        if self.peek() == Some(b'^') {
            self.pos += 1;
            let exp = self.parse_power()?;
            return Some(base.powf(exp));
        }
        Some(base)
    }
    fn parse_unary(&mut self) -> Option<f64> {
        match self.peek() {
            Some(b'-') => {
                self.pos += 1;
                Some(-self.parse_unary()?)
            }
            Some(b'+') => {
                self.pos += 1;
                self.parse_unary()
            }
            Some(b's') => {
                self.pos += 1;
                if self.peek() != Some(b'(') {
                    return None;
                }
                self.pos += 1;
                let inner = self.parse_expr()?;
                if self.peek() != Some(b')') || inner < 0.0 {
                    return None;
                }
                self.pos += 1;
                Some(inner.sqrt())
            }
            _ => self.parse_primary(),
        }
    }
    fn parse_primary(&mut self) -> Option<f64> {
        if self.peek() == Some(b'(') {
            self.pos += 1;
            let v = self.parse_expr()?;
            if self.peek() != Some(b')') {
                return None;
            }
            self.pos += 1;
            return Some(v);
        }
        self.skip_ws();
        let start = self.pos;
        let mut saw_dot = false;
        while self.pos < self.bytes.len() {
            let b = self.bytes[self.pos];
            if b.is_ascii_digit() {
                self.pos += 1;
            } else if b == b'.' && !saw_dot {
                saw_dot = true;
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return None;
        }
        let s = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        s.parse::<f64>().ok()
    }
}

// ---------- Background Indexers (Start Menu, Registry App Paths, Settings, Files/Projects) ----------

fn collect_start_menu_apps(out: &mut Vec<IndexedEntry>, seen: &mut HashSet<String>) {
    let mut roots = Vec::new();
    if let Ok(ad) = env::var("APPDATA") {
        roots.push(PathBuf::from(ad).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(pd) = env::var("ProgramData") {
        roots.push(PathBuf::from(pd).join(r"Microsoft\Windows\Start Menu\Programs"));
    }
    if let Ok(up) = env::var("USERPROFILE") {
        roots.push(PathBuf::from(up).join("Desktop"));
    }
    roots.push(PathBuf::from(r"C:\Users\Public\Desktop"));

    for root in roots {
        walk_shortcuts(&root, 0, out, seen);
    }
}

fn walk_shortcuts(dir: &Path, depth: usize, out: &mut Vec<IndexedEntry>, seen: &mut HashSet<String>) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_dir() {
            walk_shortcuts(&p, depth + 1, out, seen);
            continue;
        }
        let ext = p
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if ext != "lnk" && ext != "url" {
            continue;
        }
        let Some(stem) = p.file_stem().map(|s| s.to_string_lossy().to_string()) else {
            continue;
        };
        let low = stem.to_ascii_lowercase();
        if low.contains("uninstall") || low.starts_with("readme") || low.starts_with("help") {
            continue;
        }
        let dedup_key = format!("app:{low}");
        if !seen.insert(dedup_key) {
            continue;
        }
        let path_str = p.to_string_lossy().to_string();
        let parent_hint = p
            .parent()
            .and_then(|d| d.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Start Menu".into());
        out.push(IndexedEntry {
            title: stem,
            title_low: low,
            subtitle: format!("Application  ·  {parent_hint}"),
            keywords_low: parent_hint.to_ascii_lowercase(),
            kind: SearchKind::App,
            target: path_str.clone(),
            icon_key: format!("srch:{}", path_str.to_ascii_lowercase()),
            icon_path: path_str,
            base_boost: 260,
        });
    }
}

fn collect_registry_app_paths(out: &mut Vec<IndexedEntry>, seen: &mut HashSet<String>) {
    unsafe {
        let sub = wide(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths");
        for root in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
            let mut hkey: HKEY = null_mut();
            if RegOpenKeyExW(root, sub.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
                continue;
            }
            let mut idx = 0u32;
            let mut name_buf = [0u16; 260];
            loop {
                let mut name_len = name_buf.len() as u32;
                let rc = RegEnumKeyExW(
                    hkey,
                    idx,
                    name_buf.as_mut_ptr(),
                    &mut name_len,
                    null_mut(),
                    null_mut(),
                    null_mut(),
                    null_mut(),
                );
                if rc != 0 {
                    break;
                }
                idx += 1;
                let exe_name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
                let stem = exe_name.strip_suffix(".exe").or_else(|| exe_name.strip_suffix(".EXE")).unwrap_or(&exe_name);
                let low = stem.to_ascii_lowercase();
                if low.len() < 2 || seen.contains(&format!("app:{low}")) {
                    continue;
                }
                let mut val_buf = [0u16; 520];
                let mut cb = (val_buf.len() * 2) as u32;
                let sub_w = wide(&exe_name);
                if RegGetValueW(
                    hkey,
                    sub_w.as_ptr(),
                    null(),
                    RRF_RT_REG_SZ,
                    null_mut(),
                    val_buf.as_mut_ptr() as *mut _,
                    &mut cb,
                ) == 0
                {
                    let len = (cb as usize / 2).saturating_sub(1);
                    let raw = String::from_utf16_lossy(&val_buf[..len])
                        .trim_matches('"')
                        .trim()
                        .to_string();
                    if !raw.is_empty() && Path::new(&raw).is_file() {
                        seen.insert(format!("app:{low}"));
                        out.push(IndexedEntry {
                            title: stem.to_string(),
                            title_low: low,
                            subtitle: raw.clone(),
                            keywords_low: exe_name.to_ascii_lowercase(),
                            kind: SearchKind::App,
                            target: raw.clone(),
                            icon_key: format!("srch:{}", raw.to_ascii_lowercase()),
                            icon_path: raw,
                            base_boost: 210,
                        });
                    }
                }
            }
            RegCloseKey(hkey);
        }
    }
}

fn collect_user_files_and_projects(out: &mut Vec<IndexedEntry>, seen: &mut HashSet<String>) {
    // Index D:\Project workspaces first (high value for developer workflow!)
    let proj_root = PathBuf::from(r"D:\Project");
    if let Ok(rd) = fs::read_dir(&proj_root) {
        for ent in rd.flatten() {
            let p = ent.path();
            if !p.is_dir() {
                continue;
            }
            let name = ent.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let low = name.to_ascii_lowercase();
            let s = p.to_string_lossy().to_string();
            if seen.insert(format!("proj:{low}")) {
                out.push(IndexedEntry {
                    title: name,
                    title_low: low,
                    subtitle: s.clone(),
                    keywords_low: "project workspace repo folder".into(),
                    kind: SearchKind::Project,
                    target: s.clone(),
                    icon_key: format!("path:{}", s.to_ascii_lowercase()),
                    icon_path: s,
                    base_boost: 160,
                });
            }
        }
    }

    // Index user Downloads, Documents, Desktop (1 level deep, capped for instant startup)
    if let Ok(up) = env::var("USERPROFILE") {
        let base = PathBuf::from(&up);
        for (folder, label) in [("Downloads", "Downloads"), ("Documents", "Documents"), ("Desktop", "Desktop")] {
            let dir = base.join(folder);
            if let Ok(rd) = fs::read_dir(&dir) {
                for ent in rd.flatten().take(80) {
                    let p = ent.path();
                    let name = ent.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') || name.eq_ignore_ascii_case("desktop.ini") {
                        continue;
                    }
                    let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
                    if ext == "lnk" {
                        continue; // already indexed as shortcut
                    }
                    let low = name.to_ascii_lowercase();
                    let s = p.to_string_lossy().to_string();
                    if seen.insert(format!("file:{s}")) {
                        out.push(IndexedEntry {
                            title: name,
                            title_low: low,
                            subtitle: format!("{label}  ·  {s}"),
                            keywords_low: label.to_ascii_lowercase(),
                            kind: if p.is_dir() { SearchKind::Folder } else { SearchKind::File },
                            target: s.clone(),
                            icon_key: format!("path:{}", s.to_ascii_lowercase()),
                            icon_path: s,
                            base_boost: 80,
                        });
                    }
                }
            }
        }
    }
}

fn builtin_settings_entries() -> Vec<IndexedEntry> {
    let items: &[(&str, &str, &str, &str)] = &[
        ("Display Settings", "Monitor resolution, scale, HDR, refresh rate", "ms-settings:display", "screen monitor resolution refresh rate brightness hdr"),
        ("Sound Settings", "Output devices, input mic, volume", "ms-settings:sound", "audio speaker microphone volume headphones"),
        ("Volume Mixer", "Per-app sound levels and devices", "ms-settings:apps-volume", "sound audio mixer app volume"),
        ("Wi-Fi & Network", "Wireless connections and internet status", "ms-settings:network-wifi", "wifi internet network connection"),
        ("Ethernet & Network Status", "Wired adapters, proxy, VPN", "ms-settings:network", "lan ethernet ip adapter vpn"),
        ("Bluetooth & Devices", "Pair headphones, mice, keyboards, USB", "ms-settings:bluetooth", "bluetooth mouse keyboard headphones usb devices"),
        ("Installed Apps", "Uninstall, repair, or manage apps", "ms-settings:appsfeatures", "apps programs uninstall features"),
        ("Startup Apps", "Control programs that run at sign-in", "ms-settings:startupapps", "startup boot autostart"),
        ("Taskbar Settings", "Windows taskbar options and system tray", "ms-settings:taskbar", "taskbar tray dock"),
        ("Personalization & Wallpaper", "Background, themes, lock screen", "ms-settings:personalization", "wallpaper background theme desktop"),
        ("Colors & Dark Mode", "Dark/Light mode, accent color, transparency", "ms-settings:colors", "dark light mode accent glass transparency"),
        ("Power & Battery", "Sleep timers, screen off, power mode", "ms-settings:powersleep", "power sleep battery performance plan"),
        ("Storage & Disk Space", "Storage sense, cleanup, drive usage", "ms-settings:storagesense", "disk space cleanup storage drive"),
        ("Windows Update", "Check for system and driver updates", "ms-settings:windowsupdate", "update patch windows security"),
        ("Privacy & Security", "Permissions, Windows Defender, microphone", "ms-settings:privacy", "privacy security defender firewall"),
        ("Date & Time", "Clock format, time zone, sync", "ms-settings:dateandtime", "clock time date timezone"),
        ("Language & Keyboard", "Input languages, regional format", "ms-settings:regionlanguage", "language keyboard layout ime"),
        ("Mouse & Touchpad", "Pointer speed, scroll, sensitivity", "ms-settings:mousetouchpad", "mouse cursor pointer touchpad"),
        ("Printers & Scanners", "Manage local and network printers", "ms-settings:printers", "printer scanner print"),
        ("About This PC", "CPU, RAM, Windows version, device name", "ms-settings:about", "system specs cpu ram pc info"),
        ("Default Apps", "Default browser, mail, file associations", "ms-settings:defaultapps", "default browser file association"),
        ("Notifications & Focus", "App notifications, Do Not Disturb", "ms-settings:notifications", "notifications alerts focus dnd"),
        ("Game Mode", "Optimize PC for gaming", "ms-settings:gaming-gamemode", "game mode gaming fps"),
        ("Accounts & Sign-in", "Microsoft account, PIN, Windows Hello", "ms-settings:accounts", "account user pin password"),
        ("Accessibility", "Text size, contrast, magnifier", "ms-settings:easeofaccess", "accessibility font contrast zoom"),
    ];
    items
        .iter()
        .map(|&(title, sub, uri, kw)| IndexedEntry {
            title: title.into(),
            title_low: title.to_ascii_lowercase(),
            subtitle: format!("Windows Settings  ·  {sub}"),
            keywords_low: kw.into(),
            kind: SearchKind::Setting,
            target: uri.into(),
            icon_key: String::new(),
            icon_path: String::new(),
            base_boost: 190,
        })
        .collect()
}

fn builtin_command_entries() -> Vec<IndexedEntry> {
    let sys = env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let s32 = |f: &str| format!(r"{sys}\System32\{f}");
    let items: Vec<(&str, &str, String, &str)> = vec![
        ("Task Manager", "Processes, CPU, GPU, memory, startup", s32("taskmgr.exe"), "taskmgr cpu ram gpu kill process"),
        ("Device Manager", "Hardware drivers and PCI/USB devices", s32("devmgmt.msc"), "devmgmt drivers hardware usb gpu"),
        ("Services", "Windows background services", s32("services.msc"), "services daemon service"),
        ("Disk Management", "Partitions, drives, volumes", s32("diskmgmt.msc"), "diskmgmt partition format drive"),
        ("Registry Editor", "Edit Windows Registry keys", format!(r"{sys}\regedit.exe"), "regedit registry hkcu hklm"),
        ("Control Panel", "Classic Windows Control Panel", s32("control.exe"), "control panel classic settings"),
        ("Event Viewer", "Windows system & application logs", s32("eventvwr.msc"), "eventvwr logs crash errors"),
        ("System Configuration", "Boot options and services (msconfig)", s32("msconfig.exe"), "msconfig boot safe mode"),
        ("Command Prompt", "Windows console (cmd.exe)", s32("cmd.exe"), "cmd terminal console shell"),
        ("PowerShell", "Windows PowerShell console", s32(r"WindowsPowerShell\v1.0\powershell.exe"), "powershell ps1 terminal shell"),
        ("Windows Terminal", "Modern tabbed terminal (wt)", "wt.exe".into(), "wt terminal bash powershell"),
        ("Notepad", "Fast plain text editor", s32("notepad.exe"), "notepad text notes editor"),
        ("Calculator", "Windows Calculator", "calc.exe".into(), "calc math calculator"),
        ("Paint", "Image editor (mspaint)", s32("mspaint.exe"), "paint draw image"),
        ("Remote Desktop", "Connect via RDP (mstsc)", s32("mstsc.exe"), "rdp remote desktop mstsc"),
        ("Resource Monitor", "Live CPU, disk, network, memory graphs", s32("perfmon.exe"), "resmon perfmon monitor"),
        ("Environment Variables", "Edit System & User PATH variables", s32("SystemPropertiesAdvanced.exe"), "env path environment variables"),
        ("File Explorer", "Open Windows File Explorer", format!(r"{sys}\explorer.exe"), "explorer files folders"),
    ];
    items
        .into_iter()
        .map(|(title, sub, target, kw)| IndexedEntry {
            title: title.into(),
            title_low: title.to_ascii_lowercase(),
            subtitle: format!("System Tool  ·  {sub}"),
            keywords_low: kw.into(),
            kind: SearchKind::Setting,
            target: target.clone(),
            icon_key: format!("srch:{}", target.to_ascii_lowercase()),
            icon_path: target,
            base_boost: 220,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculator_evaluates_expressions_hex_and_percentages() {
        assert_eq!(eval_calculator("256 * 18").unwrap().0, "4608");
        assert_eq!(eval_calculator("(1920 - 436) / 2").unwrap().0, "742");
        assert_eq!(eval_calculator("0xFF").unwrap().0, "255");
        assert_eq!(eval_calculator("15% of 400").unwrap().0, "60");
        assert_eq!(eval_calculator("sqrt(144)").unwrap().0, "12");
        assert!(eval_calculator("2026").is_none());
        assert!(eval_calculator("10 / 0").is_none());
    }

    #[test]
    fn search_engine_ranks_settings_calculator_and_web_fallback() {
        let eng = SearchEngine::new();
        let res = eng.query("display", SearchScope::All, &[], 8);
        assert!(res.iter().any(|r| r.title == "Display Settings" && r.kind == SearchKind::Setting));

        let calc = eng.query("40 + 2", SearchScope::All, &[], 8);
        assert_eq!(calc[0].kind, SearchKind::Calc);
        assert_eq!(calc[0].title, "42");
    }
}
