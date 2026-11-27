#![allow(unsafe_op_in_unsafe_fn)]

use crate::{
    backup::create_backup,
    bench::run_bench_result,
    bloat::{AppxEntry, AppxTier, edge_info, existing_telemetry_tasks, onedrive_info, purge_selection, scan_appx},
    disk::{CleanRisk, CleanTarget, build_clean_targets, clean_selected_targets, fixed_drives, DriveInfo},
    mem::{MemSnapshot, fmt_bytes, snapshot as mem_snapshot},
    power::{PlanInfo, list_plans, restore_plan, set_plan_by_target},
    replace::{ALL_FORWARD, ALL_REVERSE, Component, EXPRESS, SAFE_ALL, apply_components, hot_update_taskbar, revert_components},
    report::{generate_report, mapped_drives, webview2_parents},
    startup::{Item as StartupItem, list as list_startup, set_item_enabled},
    tools::run_launch,
    tune::{Group as TuneGroup, REG_TWEAKS, RegState, SVC_TWEAKS, query_reg_state, query_svc_mode, run_undo as tune_undo_cli},
};
use std::{
    env,
    mem::zeroed,
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicIsize, Ordering},
    },
    thread,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{
        Dwm::DwmSetWindowAttribute,
        Gdi::*,
        GdiPlus::{
            FillModeAlternate, GdipAddPathArc, GdipClosePathFigure, GdipCreateFromHDC, GdipCreatePath,
            GdipCreateSolidFill, GdipDeleteBrush, GdipDeleteGraphics, GdipDeletePath, GdipFillPath,
            GdipSetSmoothingMode, GdiplusStartup, GdiplusStartupInput, GpBrush, GpGraphics, GpPath,
            GpSolidFill, SmoothingModeAntiAlias,
        },
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent},
        Shell::ShellExecuteW,
        WindowsAndMessaging::*,
    },
};
use winrust_common::{
    mem::purge_standby,
    mft::human,
    proc::{elevated, list as list_procs, start_shell_explorer},
};

const WM_APP_REFRESH: u32 = WM_APP + 1;
const WM_APP_ACTION: u32 = WM_APP + 2;
const WM_MOUSELEAVE_EVT: u32 = 0x02A3;

const WIN_W: f32 = 1080.0;
const WIN_H: f32 = 720.0;
const SIDE_W: f32 = 224.0;
const HEAD_H: f32 = 64.0;
const FOOT_H: f32 = 54.0;
const PAD: f32 = 24.0;
const CONTENT_X: f32 = SIDE_W + PAD;
const CONTENT_R: f32 = WIN_W - PAD;
const CONTENT_TOP: f32 = HEAD_H + 14.0;
const CONTENT_BOT: f32 = WIN_H - FOOT_H - 12.0;

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

const BG: u32 = rgb(24, 24, 28);
const SIDE_BG: u32 = rgb(18, 18, 22);
const HEAD_BG: u32 = rgb(16, 16, 20);
const CARD: u32 = rgb(33, 33, 40);
const CARD_ALT: u32 = rgb(28, 28, 34);
const HOVER: u32 = rgb(44, 44, 54);
const BORDER: u32 = rgb(56, 56, 68);
const TEXT: u32 = rgb(242, 242, 246);
const DIM: u32 = rgb(156, 156, 170);
const ACCENT: u32 = rgb(10, 132, 255);
const GREEN: u32 = rgb(48, 209, 88);
const AMBER: u32 = rgb(255, 159, 10);
const RED: u32 = rgb(255, 69, 58);
const ON_ACCENT: u32 = rgb(255, 255, 255);

// Clickable IDs
const ID_ELEVATE: usize = 1;
const ID_OPEN_REPORT: usize = 2;
const ID_REFRESH: usize = 3;
const ID_BACKUP: usize = 4;
const ID_TAB_BASE: usize = 10; // 10..15

const ID_OV_PURGE_MEM: usize = 20;
const ID_OV_POWER_HIGH: usize = 21;
const ID_OV_POWER_ULT: usize = 22;
const ID_OV_POWER_RESTORE: usize = 23;
const ID_OV_BENCH: usize = 24;

const ID_REP_SAFE: usize = 30;
const ID_REP_EXPRESS: usize = 31;
const ID_REP_FULL: usize = 32;
const ID_REP_NONE: usize = 33;
const ID_REP_APPLY: usize = 34;
const ID_REP_REVERT_SEL: usize = 35;
const ID_REP_ROLLBACK_ALL: usize = 36;
const ID_REP_UPDATE_BAR: usize = 37;
const ID_REP_ROW_BASE: usize = 100; // 100..108

const ID_BLOAT_JUNK: usize = 40;
const ID_BLOAT_ALL: usize = 41;
const ID_BLOAT_CLEAR: usize = 42;
const ID_BLOAT_OD: usize = 43;
const ID_BLOAT_TASKS: usize = 44;
const ID_BLOAT_PURGE: usize = 45;
const ID_BLOAT_ROW_BASE: usize = 1000;

const ID_DISK_SAFE: usize = 50;
const ID_DISK_ALL: usize = 51;
const ID_DISK_CLEAR: usize = 52;
const ID_DISK_CLEAN: usize = 53;
const ID_DISK_WIZTREE: usize = 54;
const ID_DISK_ROW_BASE: usize = 2000;

const ID_TOOL_TASKMGR: usize = 60;
const ID_TOOL_WIZTREE: usize = 61;
const ID_TOOL_SETTINGS: usize = 62;
const ID_TOOL_REPORT: usize = 63;
const ID_STARTUP_ROW_BASE: usize = 3000;

type Box4 = [f32; 4];

#[derive(Clone)]
struct Hit {
    id: usize,
    b: Box4,
}

struct AppData {
    elevated: bool,
    cpus: usize,
    sophos: bool,
    mem: MemSnapshot,
    plans: Vec<PlanInfo>,
    drives: Vec<DriveInfo>,
    mapped: Vec<(String, String, String)>,
    wv2_hosts: Vec<String>,
    wv2_count: usize,
    replace_installed: [bool; 8],
    appx: Vec<AppxEntry>,
    telemetry_tasks: Vec<String>,
    od_present: bool,
    od_files: usize,
    edge_present: bool,
    edge_profile_files: usize,
    clean_targets: Vec<CleanTarget>,
    startup_items: Vec<StartupItem>,
    ntfs_opt: usize,
    bg_opt: usize,
    svc_disabled: usize,
}

struct AppState {
    scanning: bool,
    busy: bool,
    tab: usize,
    status: String,
    bench_summary: Option<String>,
    data: Option<AppData>,
    replace_checked: [bool; 8],
    replace_selected: usize,
    appx_checked: Vec<bool>,
    purge_tasks: bool,
    purge_onedrive: bool,
    clean_checked: Vec<bool>,
    scroll: [f32; 5],
    max_scroll: [f32; 5],
    hover_id: usize,
    hits: Vec<Hit>,
}

impl AppState {
    fn new() -> Self {
        let mut replace_checked = [false; 8];
        for (i, c) in ALL_FORWARD.iter().enumerate() {
            replace_checked[i] = SAFE_ALL.contains(c);
        }
        Self {
            scanning: true,
            busy: false,
            tab: 0,
            status: "Scanning PC hardware, WinRust status, bloatware, and drives...".into(),
            bench_summary: None,
            data: None,
            replace_checked,
            replace_selected: 0,
            appx_checked: vec![],
            purge_tasks: true,
            purge_onedrive: false,
            clean_checked: vec![],
            scroll: [0.0; 5],
            max_scroll: [0.0; 5],
            hover_id: 0,
            hits: vec![],
        }
    }
}

static STATE: OnceLock<Mutex<AppState>> = OnceLock::new();
static HWND_MAIN: AtomicIsize = AtomicIsize::new(0);

fn state() -> &'static Mutex<AppState> {
    STATE.get_or_init(|| Mutex::new(AppState::new()))
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

const fn argb(c: u32, a: u8) -> u32 {
    (a as u32) << 24 | (c & 255) << 16 | ((c >> 8) & 255) << 8 | ((c >> 16) & 255)
}

fn lerp(a: u32, b: u32, t: f32) -> u32 {
    let m = |sh: u32| (((a >> sh & 255) as f32) * (1.0 - t) + ((b >> sh & 255) as f32) * t) as u32;
    m(0) | (m(8) << 8) | (m(16) << 16)
}

fn hit_test(hits: &[Hit], x: f32, y: f32) -> Option<usize> {
    hits.iter()
        .rev()
        .find(|h| x >= h.b[0] && x <= h.b[2] && y >= h.b[1] && y <= h.b[3])
        .map(|h| h.id)
}

pub fn clamp_scroll(cur: f32, max_scroll: f32) -> f32 {
    cur.clamp(0.0, max_scroll.max(0.0))
}

struct Ui {
    dc: HDC,
    g: *mut GpGraphics,
    s: f32,
}

impl Ui {
    fn px(&self, v: f32) -> i32 {
        (v * self.s).round() as i32
    }

    unsafe fn shape(&self, b: Box4, rad: f32, color: u32) {
        let mut path = null_mut::<GpPath>();
        let (x, y, w, h) = (
            b[0] * self.s,
            b[1] * self.s,
            ((b[2] - b[0]) * self.s).max(1.0),
            ((b[3] - b[1]) * self.s).max(1.0),
        );
        let d = (rad * 2.0 * self.s).min(h).min(w).max(0.01);
        GdipCreatePath(FillModeAlternate, &mut path);
        GdipAddPathArc(path, x, y, d, d, 180.0, 90.0);
        GdipAddPathArc(path, x + w - d, y, d, d, 270.0, 90.0);
        GdipAddPathArc(path, x + w - d, y + h - d, d, d, 0.0, 90.0);
        GdipAddPathArc(path, x, y + h - d, d, d, 90.0, 90.0);
        GdipClosePathFigure(path);
        let mut br = null_mut::<GpSolidFill>();
        GdipCreateSolidFill(color, &mut br);
        GdipFillPath(self.g, br as *mut GpBrush, path);
        GdipDeleteBrush(br as *mut GpBrush);
        GdipDeletePath(path);
    }

    unsafe fn round(&self, b: Box4, rad: f32, fill: u32, line: u32) {
        if line != fill {
            self.shape(b, rad, argb(line, 255));
            let i = 1.0 / self.s;
            self.shape(
                [b[0] + i, b[1] + i, b[2] - i, b[3] - i],
                (rad - i).max(0.5),
                argb(fill, 255),
            );
        } else {
            self.shape(b, rad, argb(fill, 255));
        }
    }

    unsafe fn font(&self, size: f32, weight: i32, face: &str) -> HFONT {
        CreateFontW(
            -self.px(size),
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            wide(face).as_ptr(),
        )
    }

    unsafe fn text_in(&self, s: &str, b: Box4, size: f32, weight: i32, color: u32, flags: u32, face: &str) {
        let f = self.font(size, weight, face);
        let of = SelectObject(self.dc, f);
        SetTextColor(self.dc, color);
        SetBkMode(self.dc, TRANSPARENT as i32);
        let mut r = RECT {
            left: self.px(b[0]),
            top: self.px(b[1]),
            right: self.px(b[2]),
            bottom: self.px(b[3]),
        };
        DrawTextW(self.dc, wide(s).as_ptr(), -1, &mut r, flags | DT_NOPREFIX);
        SelectObject(self.dc, of);
        DeleteObject(f);
    }

    unsafe fn text(&self, s: &str, b: Box4, size: f32, weight: i32, color: u32, flags: u32) {
        self.text_in(s, b, size, weight, color, flags, winrust_common::style::sans_face());
    }
}

unsafe fn draw_button(ui: &Ui, b: Box4, label: &str, fill: u32, line: u32, fg: u32, hot: bool) {
    let f = if hot { lerp(fill, rgb(255, 255, 255), 0.14) } else { fill };
    let l = if hot { lerp(line, rgb(255, 255, 255), 0.22) } else { line };
    ui.round(b, 6.0, f, l);
    ui.text(label, b, 12.0, 600, fg, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
}

unsafe fn draw_pill(ui: &Ui, b: Box4, label: &str, bg: u32, fg: u32) {
    ui.round(b, 9.0, lerp(bg, BG, 0.72), bg);
    ui.text(label, b, 10.5, 700, fg, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
}

unsafe fn draw_check(ui: &Ui, x: f32, y: f32, checked: bool) {
    let b = [x, y, x + 16.0, y + 16.0];
    if checked {
        ui.round(b, 4.0, ACCENT, ACCENT);
        ui.text_in(
            "\u{E73E}",
            b,
            10.5,
            700,
            ON_ACCENT,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            winrust_common::style::icon_face(),
        );
    } else {
        ui.round(b, 4.0, CARD_ALT, rgb(115, 115, 130));
    }
}

fn collect_data() -> AppData {
    let is_admin = elevated();
    let cpus = thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let sophos = list_procs().iter().any(|(_, n)| {
        let l = n.to_ascii_lowercase();
        l.contains("sophos") || l == "sspservice.exe"
    });
    let mem = mem_snapshot();
    let plans = list_plans();
    let drives = fixed_drives();
    let mapped = mapped_drives();
    let wv2_hosts = webview2_parents();
    let mut replace_installed = [false; 8];
    for (i, c) in ALL_FORWARD.iter().enumerate() {
        replace_installed[i] = c.is_installed();
    }
    let appx = scan_appx();
    let telemetry_tasks = existing_telemetry_tasks()
        .into_iter()
        .map(String::from)
        .collect();
    let (od_setup, od_files) = onedrive_info();
    let (edge_setup, edge_files, wv2_count) = edge_info();
    let clean_targets = build_clean_targets();
    let startup_items = list_startup();

    let mut ntfs_opt = 0usize;
    let mut bg_opt = 0usize;
    for t in REG_TWEAKS {
        if matches!(query_reg_state(t.key, t.name), RegState::Dword(v) if v == t.desired) {
            if t.group == TuneGroup::Ntfs {
                ntfs_opt += 1;
            } else {
                bg_opt += 1;
            }
        }
    }
    let svc_disabled = SVC_TWEAKS
        .iter()
        .filter(|s| query_svc_mode(s.name) == Some("disabled"))
        .count();

    AppData {
        elevated: is_admin,
        cpus,
        sophos,
        mem,
        plans,
        drives,
        mapped,
        wv2_hosts,
        wv2_count,
        replace_installed,
        appx,
        telemetry_tasks,
        od_present: od_setup.is_some(),
        od_files,
        edge_present: edge_setup.is_some(),
        edge_profile_files: edge_files.len(),
        clean_targets,
        startup_items,
        ntfs_opt,
        bg_opt,
        svc_disabled,
    }
}

fn spawn_refresh() {
    {
        let mut st = state().lock().unwrap();
        st.scanning = true;
    }
    thread::spawn(|| {
        let d = collect_data();
        {
            let mut st = state().lock().unwrap();
            let first_load = st.data.is_none();
            let junk_n = d.appx.iter().filter(|a| a.tier == AppxTier::PureJunk).count();
            let work_n = d.appx.iter().filter(|a| a.tier == AppxTier::WorkApp).count();

            if first_load || st.appx_checked.len() != d.appx.len() {
                st.appx_checked = d.appx.iter().map(|a| a.tier == AppxTier::PureJunk).collect();
            }
            if first_load || st.clean_checked.len() != d.clean_targets.len() {
                st.clean_checked = d
                    .clean_targets
                    .iter()
                    .map(|t| t.risk == CleanRisk::Safe)
                    .collect();
            }
            if !st.busy {
                st.status = format!(
                    "Ready — {} Pure Junk + {} Work apps, {} mapped SMB drives, {} reclaimable caches.",
                    junk_n,
                    work_n,
                    d.mapped.len(),
                    human(d.clean_targets.iter().map(|t| t.bytes).sum())
                );
            }
            st.data = Some(d);
            st.scanning = false;
        }
        let h = HWND_MAIN.load(Ordering::Relaxed);
        if h != 0 {
            unsafe { PostMessageW(h as HWND, WM_APP_REFRESH, 0, 0) };
        }
    });
}

fn spawn_action<F>(label: &str, f: F)
where
    F: FnOnce() -> Result<String, String> + Send + 'static,
{
    {
        let mut st = state().lock().unwrap();
        if st.busy {
            return;
        }
        st.busy = true;
        st.status = format!("Working: {label}...");
    }
    if let Some(h) = non_null_hwnd() {
        unsafe { InvalidateRect(h, null(), 0) };
    }
    thread::spawn(move || {
        let res = f();
        let d = collect_data();
        {
            let mut st = state().lock().unwrap();
            st.busy = false;
            st.status = match res {
                Ok(msg) => msg,
                Err(err) => format!("Error: {err}"),
            };
            st.appx_checked = d.appx.iter().map(|a| a.tier == AppxTier::PureJunk).collect();
            st.clean_checked = d
                .clean_targets
                .iter()
                .map(|t| t.risk == CleanRisk::Safe)
                .collect();
            st.data = Some(d);
        }
        if let Some(h) = non_null_hwnd() {
            unsafe { PostMessageW(h, WM_APP_ACTION, 0, 0) };
        }
    });
}

fn non_null_hwnd() -> Option<HWND> {
    let h = HWND_MAIN.load(Ordering::Relaxed);
    (h != 0).then_some(h as HWND)
}

fn scale_for_window() -> f32 {
    unsafe {
        let dc = GetDC(null_mut());
        let mut s = GetDeviceCaps(dc, LOGPIXELSX as i32) as f32 / 96.0;
        ReleaseDC(null_mut(), dc);
        let mut wa: RECT = zeroed();
        if SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut wa as *mut _ as *mut _, 0) != 0 {
            let frame = GetSystemMetrics(SM_CYCAPTION) + 2 * GetSystemMetrics(SM_CYFIXEDFRAME) + 8;
            let sx = ((wa.right - wa.left - 16) as f32 / WIN_W).max(0.5);
            let sy = ((wa.bottom - wa.top - frame) as f32 / WIN_H).max(0.5);
            s = s.min(sx).min(sy);
        }
        s.max(0.75)
    }
}

fn ask_confirm(hwnd: HWND, title: &str, body: &str) -> bool {
    let t = wide(title);
    let b = wide(body);
    unsafe { MessageBoxW(hwnd, b.as_ptr(), t.as_ptr(), MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2) == IDYES }
}

fn show_info(hwnd: HWND, title: &str, body: &str) {
    let t = wide(title);
    let b = wide(body);
    unsafe { MessageBoxW(hwnd, b.as_ptr(), t.as_ptr(), MB_OK | MB_ICONINFORMATION) };
}

fn relaunch_as_admin(hwnd: HWND) {
    let Ok(exe) = env::current_exe() else {
        return;
    };
    let verb = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let rc = unsafe { ShellExecuteW(hwnd, verb.as_ptr(), file.as_ptr(), null(), null(), SW_SHOWNORMAL) as isize };
    if rc > 32 {
        unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) };
    }
}

fn open_report_file(hwnd: HWND) {
    let path = PathBuf::from(r"D:\Project\gp-pc-boost\REPORT.md");
    let _ = generate_report(&path);
    let verb = wide("open");
    let file = wide(&path.to_string_lossy());
    unsafe { ShellExecuteW(hwnd, verb.as_ptr(), file.as_ptr(), null(), null(), SW_SHOWNORMAL) };
}

unsafe fn paint_window(hwnd: HWND, ui: &Ui) {
    let mut hits: Vec<Hit> = vec![];
    let mut push_hit = |id: usize, b: Box4| hits.push(Hit { id, b });

    let (tab, scanning, busy, status, hover_id, bench_summary, data_opt, replace_checked, replace_selected, appx_checked, purge_tasks, purge_onedrive, clean_checked, scroll) = {
        let st = state().lock().unwrap();
        (
            st.tab,
            st.scanning,
            st.busy,
            st.status.clone(),
            st.hover_id,
            st.bench_summary.clone(),
            st.data.as_ref().map(|d| (
                d.elevated,
                d.cpus,
                d.sophos,
                d.mem,
                d.plans.clone(),
                d.drives.clone(),
                d.mapped.clone(),
                d.wv2_hosts.clone(),
                d.wv2_count,
                d.replace_installed,
                d.appx.clone(),
                d.telemetry_tasks.clone(),
                d.od_present,
                d.od_files,
                d.edge_present,
                d.edge_profile_files,
                d.clean_targets.clone(),
                d.startup_items.clone(),
                d.ntfs_opt,
                d.bg_opt,
                d.svc_disabled,
            )),
            st.replace_checked,
            st.replace_selected,
            st.appx_checked.clone(),
            st.purge_tasks,
            st.purge_onedrive,
            st.clean_checked.clone(),
            st.scroll,
        )
    };

    // Canvas & Sidebar
    ui.shape([0.0, 0.0, WIN_W, WIN_H], 0.0, argb(BG, 255));
    ui.shape([0.0, 0.0, SIDE_W, WIN_H], 0.0, argb(SIDE_BG, 255));
    ui.shape([SIDE_W, 0.0, WIN_W, HEAD_H], 0.0, argb(HEAD_BG, 255));
    ui.shape([SIDE_W, 0.0, SIDE_W + 1.0, WIN_H], 0.0, argb(BORDER, 255));
    ui.shape([SIDE_W, HEAD_H, WIN_W, HEAD_H + 1.0], 0.0, argb(BORDER, 255));
    ui.shape([SIDE_W, WIN_H - FOOT_H, WIN_W, WIN_H - FOOT_H + 1.0], 0.0, argb(BORDER, 255));

    // Sidebar title & privilege badge
    ui.text("GP PC Boost", [20.0, 18.0, SIDE_W - 16.0, 44.0], 19.0, 700, TEXT, DT_LEFT | DT_SINGLELINE);
    ui.text("WinRust · Goose · Native", [20.0, 44.0, SIDE_W - 16.0, 62.0], 11.0, 400, DIM, DT_LEFT | DT_SINGLELINE);

    let is_admin = data_opt.as_ref().map(|d| d.0).unwrap_or_else(elevated);
    if is_admin {
        draw_pill(ui, [20.0, 74.0, SIDE_W - 20.0, 98.0], "ADMINISTRATOR", GREEN, GREEN);
    } else {
        draw_pill(ui, [20.0, 74.0, SIDE_W - 20.0, 98.0], "STANDARD USER (READ-ONLY)", AMBER, AMBER);
        let b = [20.0, 106.0, SIDE_W - 20.0, 136.0];
        draw_button(ui, b, "Relaunch as Admin", ACCENT, ACCENT, ON_ACCENT, hover_id == ID_ELEVATE);
        push_hit(ID_ELEVATE, b);
    }

    // Sidebar Tabs
    let replaced_count = data_opt
        .as_ref()
        .map(|d| d.9.iter().filter(|&&x| x).count())
        .unwrap_or(0);
    let bloat_count = data_opt.as_ref().map(|d| d.10.len()).unwrap_or(0);
    let clean_total: u64 = data_opt
        .as_ref()
        .map(|d| d.16.iter().map(|t| t.bytes).sum())
        .unwrap_or(0);
    let startup_en = data_opt
        .as_ref()
        .map(|d| d.17.iter().filter(|i| i.enabled).count())
        .unwrap_or(0);

    let tabs = [
        ("Overview".to_string(), "".to_string()),
        ("WinRust Replace".to_string(), format!("{replaced_count}/8")),
        ("Bloatware & Purge".to_string(), format!("{bloat_count}")),
        ("Disk & Caches".to_string(), human(clean_total)),
        ("Startup & Tools".to_string(), format!("{startup_en}")),
    ];

    let tab_top = if is_admin { 120.0 } else { 154.0 };
    for (idx, (title, badge)) in tabs.iter().enumerate() {
        let y = tab_top + idx as f32 * 44.0;
        let b = [14.0, y, SIDE_W - 14.0, y + 36.0];
        let active = tab == idx;
        let hot = hover_id == ID_TAB_BASE + idx;
        if active {
            ui.round(b, 8.0, rgb(34, 48, 72), ACCENT);
            ui.round([16.0, y + 8.0, 20.0, y + 28.0], 2.0, ACCENT, ACCENT);
        } else if hot {
            ui.round(b, 8.0, HOVER, HOVER);
        }
        ui.text(
            title,
            [28.0, y, SIDE_W - 72.0, y + 36.0],
            12.5,
            if active { 700 } else { 500 },
            if active { TEXT } else { DIM },
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
        if !badge.is_empty() {
            ui.text(
                badge,
                [SIDE_W - 82.0, y, SIDE_W - 24.0, y + 36.0],
                11.0,
                600,
                if active { ACCENT } else { DIM },
                DT_RIGHT | DT_VCENTER | DT_SINGLELINE,
            );
        }
        push_hit(ID_TAB_BASE + idx, b);
    }

    // Bottom sidebar buttons
    let rep_btn = [20.0, WIN_H - 94.0, SIDE_W - 20.0, WIN_H - 62.0];
    draw_button(ui, rep_btn, "Open REPORT.md", CARD, BORDER, TEXT, hover_id == ID_OPEN_REPORT);
    push_hit(ID_OPEN_REPORT, rep_btn);

    let bak_btn = [20.0, WIN_H - 54.0, SIDE_W - 20.0, WIN_H - 22.0];
    draw_button(ui, bak_btn, "Backup Profile & Drives", CARD, BORDER, TEXT, hover_id == ID_BACKUP);
    push_hit(ID_BACKUP, bak_btn);

    // Header
    let head_title = match tab {
        0 => "System Overview & Workbench Health",
        1 => "WinRust Stock-Component Replacement",
        2 => "Windows Bloatware & Purge Manager",
        3 => "Storage Health & Cache Cleaner",
        _ => "Startup Apps & Standalone WinRust Tools",
    };
    ui.text(head_title, [CONTENT_X, 14.0, CONTENT_R - 120.0, 42.0], 17.5, 700, TEXT, DT_LEFT | DT_SINGLELINE);
    ui.text(
        if busy { "Applying action..." } else if scanning { "Refreshing live scan..." } else { "All presets & actions verified for this PC" },
        [CONTENT_X, 38.0, CONTENT_R - 120.0, 58.0],
        11.5,
        400,
        DIM,
        DT_LEFT | DT_SINGLELINE,
    );
    let ref_btn = [CONTENT_R - 96.0, 17.0, CONTENT_R, 47.0];
    draw_button(ui, ref_btn, "Refresh", CARD, BORDER, TEXT, hover_id == ID_REFRESH);
    push_hit(ID_REFRESH, ref_btn);

    // Footer status strip
    ui.text(
        &status,
        [CONTENT_X, WIN_H - FOOT_H, CONTENT_R, WIN_H],
        11.5,
        500,
        if status.starts_with("Error:") { RED } else { DIM },
        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
    );

    let mut max_scroll = 0.0f32;
    let Some((
        d_admin,
        d_cpus,
        d_sophos,
        d_mem,
        d_plans,
        d_drives,
        d_mapped,
        d_wv2_hosts,
        d_wv2_count,
        d_rep_inst,
        d_appx,
        d_tasks,
        d_od_pres,
        d_od_files,
        d_edge_pres,
        d_edge_files,
        d_clean,
        d_startup,
        d_ntfs_opt,
        d_bg_opt,
        d_svc_dis,
    )) = data_opt else {
        ui.text("Scanning PC components, drives, and installed packages...", [CONTENT_X, CONTENT_TOP + 20.0, CONTENT_R, CONTENT_TOP + 60.0], 14.0, 500, DIM, DT_LEFT | DT_SINGLELINE);
        let mut st = state().lock().unwrap();
        st.hits = hits;
        return;
    };
    let _ = d_admin;

    match tab {
        0 => {
            // Overview: 2x2 KPI cards + Mapped Drives + Bench
            let half = (CONTENT_R - CONTENT_X - 14.0) / 2.0;
            let c1 = [CONTENT_X, CONTENT_TOP, CONTENT_X + half, CONTENT_TOP + 126.0];
            let c2 = [CONTENT_X + half + 14.0, CONTENT_TOP, CONTENT_R, CONTENT_TOP + 126.0];
            let c3 = [CONTENT_X, CONTENT_TOP + 140.0, CONTENT_X + half, CONTENT_TOP + 266.0];
            let c4 = [CONTENT_X + half + 14.0, CONTENT_TOP + 140.0, CONTENT_R, CONTENT_TOP + 266.0];

            // Card 1: Memory
            ui.round(c1, 8.0, CARD, BORDER);
            ui.text("Memory & Standby Cache", [c1[0] + 16.0, c1[1] + 12.0, c1[2] - 16.0, c1[1] + 32.0], 13.0, 700, TEXT, DT_LEFT | DT_SINGLELINE);
            let used = d_mem.total.saturating_sub(d_mem.avail);
            ui.text(
                &format!("{} used / {} ({}%)", fmt_bytes(used), fmt_bytes(d_mem.total), d_mem.commit_load_pct),
                [c1[0] + 16.0, c1[1] + 38.0, c1[2] - 16.0, c1[1] + 58.0],
                12.0,
                500,
                TEXT,
                DT_LEFT | DT_SINGLELINE,
            );
            let sb_txt = format!(
                "Free: {} | Standby: {}",
                d_mem.free.map(fmt_bytes).unwrap_or_else(|| "n/a".into()),
                d_mem.standby.map(fmt_bytes).unwrap_or_else(|| "n/a".into())
            );
            ui.text(&sb_txt, [c1[0] + 16.0, c1[1] + 60.0, c1[2] - 16.0, c1[1] + 80.0], 11.5, 400, AMBER, DT_LEFT | DT_SINGLELINE);
            let pb = [c1[0] + 16.0, c1[3] - 38.0, c1[0] + 160.0, c1[3] - 12.0];
            draw_button(ui, pb, "Purge Standby RAM", ACCENT, ACCENT, ON_ACCENT, hover_id == ID_OV_PURGE_MEM);
            push_hit(ID_OV_PURGE_MEM, pb);

            // Card 2: Power Plan
            ui.round(c2, 8.0, CARD, BORDER);
            let act_name = d_plans.iter().find(|p| p.active).map(|p| p.name.as_str()).unwrap_or("Unknown");
            ui.text("Power Plan", [c2[0] + 16.0, c2[1] + 12.0, c2[2] - 16.0, c2[1] + 32.0], 13.0, 700, TEXT, DT_LEFT | DT_SINGLELINE);
            ui.text(&format!("Active: {act_name} ({d_cpus} threads)"), [c2[0] + 16.0, c2[1] + 38.0, c2[2] - 16.0, c2[1] + 58.0], 12.0, 500, if act_name.to_ascii_lowercase().contains("balanced") { AMBER } else { GREEN }, DT_LEFT | DT_SINGLELINE);
            ui.text("Switching saves original GUID for 1-click restore.", [c2[0] + 16.0, c2[1] + 60.0, c2[2] - 16.0, c2[1] + 80.0], 11.5, 400, DIM, DT_LEFT | DT_SINGLELINE);
            let b_hi = [c2[0] + 16.0, c2[3] - 38.0, c2[0] + 126.0, c2[3] - 12.0];
            let b_ul = [c2[0] + 134.0, c2[3] - 38.0, c2[0] + 244.0, c2[3] - 12.0];
            let b_rs = [c2[0] + 252.0, c2[3] - 38.0, c2[0] + 340.0, c2[3] - 12.0];
            draw_button(ui, b_hi, "High Perf", ACCENT, ACCENT, ON_ACCENT, hover_id == ID_OV_POWER_HIGH);
            draw_button(ui, b_ul, "Ultimate", CARD_ALT, BORDER, TEXT, hover_id == ID_OV_POWER_ULT);
            draw_button(ui, b_rs, "Restore", CARD_ALT, BORDER, TEXT, hover_id == ID_OV_POWER_RESTORE);
            push_hit(ID_OV_POWER_HIGH, b_hi);
            push_hit(ID_OV_POWER_ULT, b_ul);
            push_hit(ID_OV_POWER_RESTORE, b_rs);

            // Card 3: Storage & Tuning
            ui.round(c3, 8.0, CARD, BORDER);
            ui.text("Storage & Kernel Tuning", [c3[0] + 16.0, c3[1] + 12.0, c3[2] - 16.0, c3[1] + 32.0], 13.0, 700, TEXT, DT_LEFT | DT_SINGLELINE);
            let c_drv = d_drives.iter().find(|d| d.letter == 'C');
            let c_line = c_drv.map(|d| format!("C: {} free / {} ({:.1}%)", human(d.free), human(d.total), d.free_pct)).unwrap_or_default();
            ui.text(&c_line, [c3[0] + 16.0, c3[1] + 38.0, c3[2] - 16.0, c3[1] + 58.0], 12.0, 500, if c_drv.is_some_and(|d| d.free_pct < 15.0) { RED } else { GREEN }, DT_LEFT | DT_SINGLELINE);
            ui.text(&format!("Reclaimable caches: {}", human(clean_total)), [c3[0] + 16.0, c3[1] + 62.0, c3[2] - 16.0, c3[1] + 82.0], 11.5, 400, DIM, DT_LEFT | DT_SINGLELINE);
            ui.text(&format!("NTFS opt: {d_ntfs_opt}/2 | Policies: {d_bg_opt}/9 | Telemetry svcs off: {d_svc_dis}/6"), [c3[0] + 16.0, c3[1] + 86.0, c3[2] - 16.0, c3[1] + 106.0], 11.5, 400, DIM, DT_LEFT | DT_SINGLELINE);

            // Card 4: Safety & Workbench
            ui.round(c4, 8.0, CARD, BORDER);
            ui.text("Workbench & Safety Guardrails", [c4[0] + 16.0, c4[1] + 12.0, c4[2] - 16.0, c4[1] + 32.0], 13.0, 700, TEXT, DT_LEFT | DT_SINGLELINE);
            ui.text(if d_sophos { "AV: Sophos Intercept X (Defender tweaks blocked)" } else { "AV: Standard" }, [c4[0] + 16.0, c4[1] + 38.0, c4[2] - 16.0, c4[1] + 58.0], 11.5, 500, GREEN, DT_LEFT | DT_SINGLELINE);
            ui.text(&format!("WebView2 ({d_wv2_count} procs): {}", if d_wv2_hosts.is_empty() { "none".into() } else { d_wv2_hosts.join(", ") }), [c4[0] + 16.0, c4[1] + 62.0, c4[2] - 16.0, c4[1] + 82.0], 11.5, 400, AMBER, DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS);
            ui.text(&format!("OneDrive: {} user files | Edge profile: {d_edge_files} files", d_od_files), [c4[0] + 16.0, c4[1] + 86.0, c4[2] - 16.0, c4[1] + 106.0], 11.5, 400, DIM, DT_LEFT | DT_SINGLELINE);

            // Mapped Drives Card
            let mc = [CONTENT_X, CONTENT_TOP + 280.0, CONTENT_R, CONTENT_TOP + 474.0];
            ui.round(mc, 8.0, CARD, BORDER);
            ui.text(&format!("Mapped SMB Drives Detected ({} active)", d_mapped.len()), [mc[0] + 16.0, mc[1] + 12.0, mc[2] - 16.0, mc[1] + 32.0], 13.0, 700, TEXT, DT_LEFT | DT_SINGLELINE);
            if d_mapped.is_empty() {
                ui.text("No mapped network drives detected.", [mc[0] + 16.0, mc[1] + 42.0, mc[2] - 16.0, mc[1] + 64.0], 12.0, 400, DIM, DT_LEFT | DT_SINGLELINE);
            } else {
                for (idx, (dev, unc, vol)) in d_mapped.iter().take(8).enumerate() {
                    let col = idx % 2;
                    let row = idx / 2;
                    let rx = mc[0] + 16.0 + col as f32 * ((mc[2] - mc[0] - 32.0) / 2.0);
                    let ry = mc[1] + 42.0 + row as f32 * 34.0;
                    ui.round([rx, ry, rx + ((mc[2] - mc[0] - 46.0) / 2.0), ry + 28.0], 5.0, CARD_ALT, BORDER);
                    ui.text(&format!("{dev}  ->  {unc}  ({vol})"), [rx + 10.0, ry, rx + ((mc[2] - mc[0] - 56.0) / 2.0), ry + 28.0], 11.5, 500, TEXT, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
                }
            }

            // Benchmark Card
            let bc = [CONTENT_X, CONTENT_TOP + 488.0, CONTENT_R, CONTENT_BOT];
            ui.round(bc, 8.0, CARD, BORDER);
            ui.text("Goose v2026.4 + Rust Benchmark Baseline", [bc[0] + 16.0, bc[1] + 12.0, bc[2] - 200.0, bc[1] + 32.0], 13.0, 700, TEXT, DT_LEFT | DT_SINGLELINE);
            let btext = bench_summary.as_deref().unwrap_or("Click 'Run Goose + Rust Bench' to measure live CPU arena, memory GB/s, and small-file I/O latency.");
            ui.text(btext, [bc[0] + 16.0, bc[1] + 38.0, bc[2] - 200.0, bc[3] - 10.0], 12.0, 500, if bench_summary.is_some() { GREEN } else { DIM }, DT_LEFT | DT_WORDBREAK);
            let bb = [bc[2] - 184.0, bc[1] + 20.0, bc[2] - 16.0, bc[1] + 54.0];
            draw_button(ui, bb, "Run Goose + Rust Bench", ACCENT, ACCENT, ON_ACCENT, hover_id == ID_OV_BENCH);
            push_hit(ID_OV_BENCH, bb);
        }
        1 => {
            // WinRust Replace Tab (8 items + preset bar + compat detail + footer actions)
            let presets = [
                (ID_REP_SAFE, "Safe-All (Recommended)"),
                (ID_REP_EXPRESS, "Express (Keep Explorer)"),
                (ID_REP_FULL, "Full WinRust (+Explorer)"),
                (ID_REP_NONE, "Clear"),
            ];
            let mut px = CONTENT_X;
            for (id, label) in presets {
                let w = if id == ID_REP_NONE { 76.0 } else { 176.0 };
                let b = [px, CONTENT_TOP, px + w, CONTENT_TOP + 30.0];
                draw_button(ui, b, label, if id == ID_REP_SAFE { rgb(34, 48, 72) } else { CARD }, BORDER, TEXT, hover_id == id);
                push_hit(id, b);
                px += w + 8.0;
            }

            let list_top = CONTENT_TOP + 40.0;
            let row_h = 44.0;
            for (i, c) in ALL_FORWARD.iter().enumerate() {
                let y = list_top + i as f32 * (row_h + 6.0);
                let b = [CONTENT_X, y, CONTENT_R, y + row_h];
                let selected = replace_selected == i;
                let hot = hover_id == ID_REP_ROW_BASE + i;
                let fill = if hot { HOVER } else if selected { rgb(38, 42, 54) } else { CARD };
                let line = if selected { ACCENT } else { BORDER };
                ui.round(b, 7.0, fill, line);
                draw_check(ui, CONTENT_X + 14.0, y + 14.0, replace_checked[i]);
                ui.text(c.name(), [CONTENT_X + 42.0, y + 6.0, CONTENT_X + 150.0, y + 24.0], 13.0, 700, TEXT, DT_LEFT | DT_SINGLELINE);
                ui.text(c.replaces(), [CONTENT_X + 152.0, y + 6.0, CONTENT_R - 230.0, y + 24.0], 12.0, 400, DIM, DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS);

                let (compat_lbl, compat_col) = match c {
                    Component::Explorer => ("SHELL SWAP", RED),
                    Component::Search => ("SKIPS N:-Z:", AMBER),
                    Component::Topbar => ("HIDES TASKBAR", AMBER),
                    _ => ("SAFE · N:-Z: OK", GREEN),
                };
                draw_pill(ui, [CONTENT_R - 216.0, y + 11.0, CONTENT_R - 104.0, y + 33.0], compat_lbl, compat_col, compat_col);
                let inst = d_rep_inst[i];
                draw_pill(ui, [CONTENT_R - 94.0, y + 11.0, CONTENT_R - 12.0, y + 33.0], if inst { "REPLACED" } else { "STOCK" }, if inst { GREEN } else { BORDER }, if inst { GREEN } else { DIM });
                push_hit(ID_REP_ROW_BASE + i, b);
            }

            // Compatibility Detail Box
            let det_y = list_top + 8.0 * (row_h + 6.0) + 6.0;
            let det_b = [CONTENT_X, det_y, CONTENT_R, CONTENT_BOT - 46.0];
            ui.round(det_b, 8.0, CARD_ALT, BORDER);
            let sel_comp = ALL_FORWARD[replace_selected.min(7)];
            let detail = match sel_comp {
                Component::TaskMgr | Component::Settings | Component::PhotoView | Component::WizTree | Component::Debloat => {
                    format!("{}: Fully compatible with your 8 mapped drives (N:-Z:), explorer.exe, WebView2 apps, and Win+V. Zero SYSTEM updater task; 100% reversible.", sel_comp.name())
                }
                Component::Topbar => {
                    "topbar (Fluent 2 Win12 Split-Plane): Multi-monitor taskbar following Windows accent, dark/light, transparency, alignment & MMTaskbarMode. Use 'Hot-Update Win12 Bar' anytime without reverting first.".into()
                }
                Component::Search => {
                    "search (rust-search): Indexes local NTFS drives (C:, D:, F:, G:) in ~45ms, but SKIPS all 8 SMB mapped drives (N:-Z:) and disables WSearch + SearchHost.exe. Reversible.".into()
                }
                Component::Explorer => {
                    "explorer (rust-explorer + rs-shell): Sees N:-Z: drive letters, BUT disables explorer.exe shell, lacks Network share discovery, Quick Access scrollbar, Win+V clipboard history, and MTP phones. Reversible via rollback.".into()
                }
            };
            ui.text(&detail, [det_b[0] + 14.0, det_b[1] + 10.0, det_b[2] - 14.0, det_b[3] - 8.0], 11.5, 400, TEXT, DT_LEFT | DT_WORDBREAK);

            let fy = CONTENT_BOT - 36.0;
            let b_apply = [CONTENT_X, fy, CONTENT_X + 148.0, CONTENT_BOT];
            let b_rev = [CONTENT_X + 158.0, fy, CONTENT_X + 306.0, CONTENT_BOT];
            let b_upd = [CONTENT_X + 316.0, fy, CONTENT_X + 502.0, CONTENT_BOT];
            let b_all_rev = [CONTENT_R - 166.0, fy, CONTENT_R, CONTENT_BOT];
            draw_button(ui, b_apply, "Apply Selected", ACCENT, ACCENT, ON_ACCENT, hover_id == ID_REP_APPLY);
            draw_button(ui, b_rev, "Revert Selected", CARD, BORDER, TEXT, hover_id == ID_REP_REVERT_SEL);
            draw_button(ui, b_upd, "Hot-Update Win12 Bar", rgb(34, 48, 72), ACCENT, TEXT, hover_id == ID_REP_UPDATE_BAR);
            draw_button(ui, b_all_rev, "Full Rollback All", RED, RED, ON_ACCENT, hover_id == ID_REP_ROLLBACK_ALL);
            push_hit(ID_REP_APPLY, b_apply);
            push_hit(ID_REP_REVERT_SEL, b_rev);
            push_hit(ID_REP_UPDATE_BAR, b_upd);
            push_hit(ID_REP_ROLLBACK_ALL, b_all_rev);
        }
        2 => {
            // Bloatware & Purge Tab (Scrollable list)
            let b_j = [CONTENT_X, CONTENT_TOP, CONTENT_X + 130.0, CONTENT_TOP + 30.0];
            let b_a = [CONTENT_X + 138.0, CONTENT_TOP, CONTENT_X + 248.0, CONTENT_TOP + 30.0];
            let b_c = [CONTENT_X + 256.0, CONTENT_TOP, CONTENT_X + 324.0, CONTENT_TOP + 30.0];
            let b_od = [CONTENT_X + 334.0, CONTENT_TOP, CONTENT_X + 516.0, CONTENT_TOP + 30.0];
            let b_tk = [CONTENT_X + 524.0, CONTENT_TOP, CONTENT_X + 686.0, CONTENT_TOP + 30.0];
            draw_button(ui, b_j, "Select Pure Junk", rgb(34, 48, 72), BORDER, TEXT, hover_id == ID_BLOAT_JUNK);
            draw_button(ui, b_a, "Select All", CARD, BORDER, TEXT, hover_id == ID_BLOAT_ALL);
            draw_button(ui, b_c, "Clear", CARD, BORDER, TEXT, hover_id == ID_BLOAT_CLEAR);
            draw_button(
                ui,
                b_od,
                &format!("[{}] OneDrive ({d_od_files} files)", if purge_onedrive { "X" } else { " " }),
                if purge_onedrive { rgb(54, 42, 24) } else { CARD },
                if purge_onedrive { AMBER } else { BORDER },
                TEXT,
                hover_id == ID_BLOAT_OD,
            );
            draw_button(
                ui,
                b_tk,
                &format!("[{}] Telemetry ({})", if purge_tasks { "X" } else { " " }, d_tasks.len()),
                if purge_tasks { rgb(34, 48, 72) } else { CARD },
                BORDER,
                TEXT,
                hover_id == ID_BLOAT_TASKS,
            );
            push_hit(ID_BLOAT_JUNK, b_j);
            push_hit(ID_BLOAT_ALL, b_a);
            push_hit(ID_BLOAT_CLEAR, b_c);
            push_hit(ID_BLOAT_OD, b_od);
            push_hit(ID_BLOAT_TASKS, b_tk);

            // Edge safety banner
            let eb = [CONTENT_X, CONTENT_TOP + 38.0, CONTENT_R, CONTENT_TOP + 68.0];
            ui.round(eb, 6.0, rgb(48, 28, 30), RED);
            ui.text(
                &format!("Edge ({}) & WebView2 ({} procs: {}): KEPT SAFE — purge cannot auto-import Edge Default profile ({} files).", if d_edge_pres { "installed" } else { "missing" }, d_wv2_count, d_wv2_hosts.join(", "), d_edge_files),
                [eb[0] + 12.0, eb[1], eb[2] - 12.0, eb[3]],
                11.5,
                500,
                TEXT,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
            );

            // Scrollable Appx list viewport
            let vp_top = CONTENT_TOP + 76.0;
            let vp_bot = CONTENT_BOT - 46.0;
            let row_h = 34.0;
            let total_h = d_appx.len() as f32 * (row_h + 4.0);
            max_scroll = (total_h - (vp_bot - vp_top)).max(0.0);
            let sc = clamp_scroll(scroll[2], max_scroll);

            let clip = CreateRectRgn(ui.px(CONTENT_X), ui.px(vp_top), ui.px(CONTENT_R), ui.px(vp_bot));
            SelectClipRgn(ui.dc, clip);
            for (i, app) in d_appx.iter().enumerate() {
                let y = vp_top + i as f32 * (row_h + 4.0) - sc;
                if y + row_h < vp_top || y > vp_bot {
                    continue;
                }
                let b = [CONTENT_X, y, CONTENT_R - 10.0, y + row_h];
                let checked = appx_checked.get(i).copied().unwrap_or(false);
                let hot = hover_id == ID_BLOAT_ROW_BASE + i;
                ui.round(b, 6.0, if hot { HOVER } else { CARD }, BORDER);
                draw_check(ui, CONTENT_X + 12.0, y + 9.0, checked);
                ui.text(&app.name, [CONTENT_X + 38.0, y, CONTENT_R - 230.0, y + row_h], 12.0, 600, TEXT, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
                let (tag, col) = if app.tier == AppxTier::PureJunk {
                    ("PURE JUNK", GREEN)
                } else {
                    ("WORK APP — KEEP", AMBER)
                };
                draw_pill(ui, [CONTENT_R - 216.0, y + 6.0, CONTENT_R - 92.0, y + 28.0], tag, col, col);
                ui.text(if app.provisioned { "prov" } else { "appx" }, [CONTENT_R - 84.0, y, CONTENT_R - 18.0, y + row_h], 11.0, 400, DIM, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
                push_hit(ID_BLOAT_ROW_BASE + i, [b[0], b[1].max(vp_top), b[2], b[3].min(vp_bot)]);
            }
            SelectClipRgn(ui.dc, null_mut());
            DeleteObject(clip);

            let sel_n = appx_checked.iter().filter(|&&x| x).count();
            let pb = [CONTENT_R - 260.0, CONTENT_BOT - 36.0, CONTENT_R, CONTENT_BOT];
            draw_button(ui, pb, &format!("Purge {sel_n} Selected (Irreversible)"), RED, RED, ON_ACCENT, hover_id == ID_BLOAT_PURGE);
            push_hit(ID_BLOAT_PURGE, pb);
            ui.text(
                "Creates backup + restore_apps.ps1 before removing selected packages.",
                [CONTENT_X, CONTENT_BOT - 36.0, CONTENT_R - 274.0, CONTENT_BOT],
                11.5,
                400,
                DIM,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE,
            );
            let _ = d_od_pres;
        }
        3 => {
            // Disk & Caches Tab
            let mut dx = CONTENT_X;
            let dw = (CONTENT_R - CONTENT_X - (d_drives.len().saturating_sub(1) as f32) * 10.0) / d_drives.len().max(1) as f32;
            for d in &d_drives {
                let db = [dx, CONTENT_TOP, dx + dw, CONTENT_TOP + 62.0];
                let warn = d.free_pct < 15.0;
                ui.round(db, 7.0, CARD, if warn { RED } else { BORDER });
                ui.text(&format!("{}: Drive", d.letter), [db[0] + 12.0, db[1] + 8.0, db[2] - 12.0, db[1] + 28.0], 13.0, 700, if warn { RED } else { TEXT }, DT_LEFT | DT_SINGLELINE);
                ui.text(&format!("{} free ({:.1}%)", human(d.free), d.free_pct), [db[0] + 12.0, db[1] + 32.0, db[2] - 12.0, db[1] + 52.0], 11.5, 500, if warn { RED } else { DIM }, DT_LEFT | DT_SINGLELINE);
                dx += dw + 10.0;
            }

            let bs = [CONTENT_X, CONTENT_TOP + 74.0, CONTENT_X + 110.0, CONTENT_TOP + 104.0];
            let ba = [CONTENT_X + 118.0, CONTENT_TOP + 74.0, CONTENT_X + 220.0, CONTENT_TOP + 104.0];
            let bc = [CONTENT_X + 228.0, CONTENT_TOP + 74.0, CONTENT_X + 300.0, CONTENT_TOP + 104.0];
            draw_button(ui, bs, "Select Safe", rgb(34, 48, 72), BORDER, TEXT, hover_id == ID_DISK_SAFE);
            draw_button(ui, ba, "Select All", CARD, BORDER, TEXT, hover_id == ID_DISK_ALL);
            draw_button(ui, bc, "Clear", CARD, BORDER, TEXT, hover_id == ID_DISK_CLEAR);
            push_hit(ID_DISK_SAFE, bs);
            push_hit(ID_DISK_ALL, ba);
            push_hit(ID_DISK_CLEAR, bc);

            let vp_top = CONTENT_TOP + 114.0;
            let vp_bot = CONTENT_BOT - 46.0;
            let row_h = 42.0;
            let total_h = d_clean.len() as f32 * (row_h + 6.0);
            max_scroll = (total_h - (vp_bot - vp_top)).max(0.0);
            let sc = clamp_scroll(scroll[3], max_scroll);

            let clip = CreateRectRgn(ui.px(CONTENT_X), ui.px(vp_top), ui.px(CONTENT_R), ui.px(vp_bot));
            SelectClipRgn(ui.dc, clip);
            for (i, t) in d_clean.iter().enumerate() {
                let y = vp_top + i as f32 * (row_h + 6.0) - sc;
                if y + row_h < vp_top || y > vp_bot {
                    continue;
                }
                let b = [CONTENT_X, y, CONTENT_R - 10.0, y + row_h];
                let checked = clean_checked.get(i).copied().unwrap_or(false);
                let hot = hover_id == ID_DISK_ROW_BASE + i;
                ui.round(b, 6.0, if hot { HOVER } else { CARD }, BORDER);
                draw_check(ui, CONTENT_X + 12.0, y + 13.0, checked);
                ui.text(t.label, [CONTENT_X + 38.0, y + 4.0, CONTENT_R - 200.0, y + 22.0], 12.0, 600, TEXT, DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS);
                ui.text(t.note, [CONTENT_X + 38.0, y + 22.0, CONTENT_R - 200.0, y + 38.0], 10.5, 400, DIM, DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS);
                let (tag, col) = if t.risk == CleanRisk::Safe { ("SAFE", GREEN) } else { ("REBUILD", AMBER) };
                draw_pill(ui, [CONTENT_R - 190.0, y + 10.0, CONTENT_R - 114.0, y + 32.0], tag, col, col);
                ui.text(&human(t.bytes), [CONTENT_R - 106.0, y, CONTENT_R - 18.0, y + row_h], 12.0, 700, TEXT, DT_RIGHT | DT_VCENTER | DT_SINGLELINE);
                push_hit(ID_DISK_ROW_BASE + i, [b[0], b[1].max(vp_top), b[2], b[3].min(vp_bot)]);
            }
            SelectClipRgn(ui.dc, null_mut());
            DeleteObject(clip);

            let sel_bytes: u64 = d_clean
                .iter()
                .enumerate()
                .filter(|(i, _)| clean_checked.get(*i).copied().unwrap_or(false))
                .map(|(_, t)| t.bytes)
                .sum();
            let b_clean = [CONTENT_R - 230.0, CONTENT_BOT - 36.0, CONTENT_R, CONTENT_BOT];
            let b_wiz = [CONTENT_X, CONTENT_BOT - 36.0, CONTENT_X + 150.0, CONTENT_BOT];
            draw_button(ui, b_wiz, "Launch WizTree", CARD, BORDER, TEXT, hover_id == ID_DISK_WIZTREE);
            draw_button(ui, b_clean, &format!("Clean Selected ({})", human(sel_bytes)), ACCENT, ACCENT, ON_ACCENT, hover_id == ID_DISK_CLEAN);
            push_hit(ID_DISK_WIZTREE, b_wiz);
            push_hit(ID_DISK_CLEAN, b_clean);
        }
        _ => {
            // Startup & Tools Tab
            let vp_top = CONTENT_TOP;
            let vp_bot = CONTENT_BOT - 54.0;
            let row_h = 36.0;
            let total_h = d_startup.len() as f32 * (row_h + 4.0);
            max_scroll = (total_h - (vp_bot - vp_top)).max(0.0);
            let sc = clamp_scroll(scroll[4], max_scroll);

            let clip = CreateRectRgn(ui.px(CONTENT_X), ui.px(vp_top), ui.px(CONTENT_R), ui.px(vp_bot));
            SelectClipRgn(ui.dc, clip);
            for (i, it) in d_startup.iter().enumerate() {
                let y = vp_top + i as f32 * (row_h + 4.0) - sc;
                if y + row_h < vp_top || y > vp_bot {
                    continue;
                }
                let b = [CONTENT_X, y, CONTENT_R - 10.0, y + row_h];
                let hot = hover_id == ID_STARTUP_ROW_BASE + i;
                ui.round(b, 6.0, if hot { HOVER } else { CARD }, BORDER);
                draw_pill(
                    ui,
                    [CONTENT_X + 10.0, y + 7.0, CONTENT_X + 88.0, y + 29.0],
                    if it.enabled { "ENABLED" } else { "DISABLED" },
                    if it.enabled { GREEN } else { BORDER },
                    if it.enabled { GREEN } else { DIM },
                );
                ui.text(&it.name, [CONTENT_X + 98.0, y, CONTENT_X + 310.0, y + row_h], 12.0, 600, TEXT, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
                ui.text(it.source_label(), [CONTENT_X + 316.0, y, CONTENT_X + 510.0, y + row_h], 11.0, 400, DIM, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
                ui.text(&it.command, [CONTENT_X + 516.0, y, CONTENT_R - 18.0, y + row_h], 11.0, 400, DIM, DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
                push_hit(ID_STARTUP_ROW_BASE + i, [b[0], b[1].max(vp_top), b[2], b[3].min(vp_bot)]);
            }
            SelectClipRgn(ui.dc, null_mut());
            DeleteObject(clip);

            let ty = CONTENT_BOT - 38.0;
            let tools_btns = [
                (ID_TOOL_TASKMGR, "WinRust TaskMgr"),
                (ID_TOOL_WIZTREE, "WinRust WizTree"),
                (ID_TOOL_SETTINGS, "WinRust Settings"),
                (ID_TOOL_REPORT, "Open REPORT.md"),
            ];
            let mut tx = CONTENT_X;
            for (id, lbl) in tools_btns {
                let b = [tx, ty, tx + 150.0, CONTENT_BOT];
                draw_button(ui, b, lbl, CARD, BORDER, TEXT, hover_id == id);
                push_hit(id, b);
                tx += 160.0;
            }
        }
    }

    let _ = hwnd;
    let mut st = state().lock().unwrap();
    st.max_scroll[tab] = max_scroll;
    st.scroll[tab] = clamp_scroll(st.scroll[tab], max_scroll);
    st.hits = hits;
}

fn handle_click(hwnd: HWND, id: usize) {
    if (ID_TAB_BASE..ID_TAB_BASE + 5).contains(&id) {
        let mut st = state().lock().unwrap();
        st.tab = id - ID_TAB_BASE;
        unsafe { InvalidateRect(hwnd, null(), 0) };
        return;
    }

    match id {
        ID_ELEVATE => relaunch_as_admin(hwnd),
        ID_OPEN_REPORT | ID_TOOL_REPORT => open_report_file(hwnd),
        ID_REFRESH => spawn_refresh(),
        ID_BACKUP => spawn_action("Creating backup snapshot", || {
            let p = create_backup()?;
            Ok(format!("Backup saved to {}", p.display()))
        }),
        ID_OV_PURGE_MEM => {
            if !elevated() {
                show_info(hwnd, "Admin Required", "Click 'Relaunch as Admin' in the sidebar to purge the Windows standby RAM list.");
                return;
            }
            spawn_action("Purging standby RAM", || {
                purge_standby().map_err(|st| format!("NTSTATUS 0x{:08X}", st as u32))?;
                Ok("Standby RAM cache purged successfully.".into())
            });
        }
        ID_OV_POWER_HIGH => spawn_action("Switching power plan to High Performance", || {
            let p = set_plan_by_target("high")?;
            Ok(format!("Active power plan: {}", p.name))
        }),
        ID_OV_POWER_ULT => spawn_action("Switching power plan to Ultimate Performance", || {
            let p = set_plan_by_target("ultimate")?;
            Ok(format!("Active power plan: {}", p.name))
        }),
        ID_OV_POWER_RESTORE => spawn_action("Restoring original power plan", || {
            match restore_plan()? {
                Some(n) => Ok(format!("Power plan restored to '{n}'.")),
                None => Ok("No saved power plan to restore.".into()),
            }
        }),
        ID_OV_BENCH => spawn_action("Running Goose + Rust Benchmark", || {
            let (res, _, _, snap) = run_bench_result()?;
            let summary = format!(
                "Goose ({}) = {:.1} ms median | Memory Stream = {:.2} GB/s | Small-File I/O = {:.1} ms\nSnapshot: {}",
                res.goose_mode, res.goose_median_ms, res.mem_gb_per_sec, res.io_total_ms, snap.display()
            );
            let mut st = state().lock().unwrap();
            st.bench_summary = Some(summary.clone());
            Ok(summary)
        }),
        ID_REP_SAFE => {
            let mut st = state().lock().unwrap();
            for (i, c) in ALL_FORWARD.iter().enumerate() {
                st.replace_checked[i] = SAFE_ALL.contains(c);
            }
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        ID_REP_EXPRESS => {
            let mut st = state().lock().unwrap();
            for (i, c) in ALL_FORWARD.iter().enumerate() {
                st.replace_checked[i] = EXPRESS.contains(c);
            }
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        ID_REP_FULL => {
            let mut st = state().lock().unwrap();
            st.replace_checked = [true; 8];
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        ID_REP_NONE => {
            let mut st = state().lock().unwrap();
            st.replace_checked = [false; 8];
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        ID_REP_APPLY => {
            if !elevated() {
                show_info(hwnd, "Admin Required", "Click 'Relaunch as Admin' in the sidebar to install WinRust component replacements.");
                return;
            }
            let selected: Vec<Component> = {
                let st = state().lock().unwrap();
                ALL_FORWARD
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| st.replace_checked[*i])
                    .map(|(_, c)| *c)
                    .collect()
            };
            if selected.is_empty() {
                show_info(hwnd, "No Selection", "Tick at least one component to apply.");
                return;
            }
            let has_explorer = selected.contains(&Component::Explorer);
            if has_explorer
                && !ask_confirm(
                    hwnd,
                    "Confirm Explorer Shell Replacement",
                    "You selected `explorer` (rust-explorer).\nThis disables explorer.exe as shell (no Win+V, no MTP phones, rust-search skips N:-Z:).\nEverything is 100% reversible via 'Full Rollback All'. Proceed?",
                )
            {
                return;
            }
            spawn_action("Installing selected WinRust components", move || {
                apply_components(&selected, has_explorer)
            });
        }
        ID_REP_REVERT_SEL => {
            if !elevated() {
                show_info(hwnd, "Admin Required", "Click 'Relaunch as Admin' to revert WinRust components.");
                return;
            }
            let selected: Vec<Component> = {
                let st = state().lock().unwrap();
                ALL_FORWARD
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| st.replace_checked[*i])
                    .map(|(_, c)| *c)
                    .collect()
            };
            spawn_action("Reverting selected WinRust components", move || {
                revert_components(&selected)
            });
        }
        ID_REP_UPDATE_BAR => {
            spawn_action("Hot-updating Windows 12 taskbar", hot_update_taskbar);
        }
        ID_REP_ROLLBACK_ALL => {
            if !elevated() {
                show_info(hwnd, "Admin Required", "Click 'Relaunch as Admin' to run Full Rollback.");
                return;
            }
            if !ask_confirm(
                hwnd,
                "Confirm Full Rollback",
                "Revert all WinRust replacements, restore Windows Explorer shell, undo tune registry/service tweaks, and restore original power plan?",
            ) {
                return;
            }
            spawn_action("Running full rollback", || {
                let rep = revert_components(&ALL_REVERSE)?;
                let _ = tune_undo_cli();
                let _ = restore_plan();
                if winrust_common::proc::shell_explorers().is_empty() {
                    let _ = start_shell_explorer();
                }
                Ok(format!("{rep} Explorer shell verified."))
            });
        }
        ID_BLOAT_JUNK => {
            let mut st = state().lock().unwrap();
            if let Some(d) = st.data.as_ref() {
                st.appx_checked = d.appx.iter().map(|a| a.tier == AppxTier::PureJunk).collect();
            }
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        ID_BLOAT_ALL => {
            let mut st = state().lock().unwrap();
            for x in &mut st.appx_checked {
                *x = true;
            }
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        ID_BLOAT_CLEAR => {
            let mut st = state().lock().unwrap();
            for x in &mut st.appx_checked {
                *x = false;
            }
            st.purge_onedrive = false;
            st.purge_tasks = false;
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        ID_BLOAT_OD => {
            let mut st = state().lock().unwrap();
            st.purge_onedrive = !st.purge_onedrive;
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        ID_BLOAT_TASKS => {
            let mut st = state().lock().unwrap();
            st.purge_tasks = !st.purge_tasks;
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        ID_BLOAT_PURGE => {
            if !elevated() {
                show_info(hwnd, "Admin Required", "Click 'Relaunch as Admin' in the sidebar to purge bloatware packages.");
                return;
            }
            let (selected, tasks, od) = {
                let st = state().lock().unwrap();
                let Some(d) = st.data.as_ref() else { return };
                let sel: Vec<(String, String, bool)> = d
                    .appx
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| st.appx_checked.get(*i).copied().unwrap_or(false))
                    .map(|(_, a)| (a.name.clone(), a.full_name.clone(), a.provisioned))
                    .collect();
                (sel, st.purge_tasks, st.purge_onedrive)
            };
            if selected.is_empty() && !tasks && !od {
                show_info(hwnd, "No Selection", "Select at least one bloatware package, task, or OneDrive to purge.");
                return;
            }
            let msg = format!(
                "IRREVERSIBLE PURGE:\n- {} Appx package(s)\n- Telemetry tasks: {}\n- OneDrive: {}\n\nAppx purge does NOT import old app data. A backup + restore_apps.ps1 will be saved first. Continue?",
                selected.len(),
                if tasks { "YES" } else { "NO" },
                if od { "YES" } else { "NO" }
            );
            if !ask_confirm(hwnd, "Confirm Irreversible Bloatware Purge", &msg) {
                return;
            }
            spawn_action("Purging selected bloatware", move || {
                purge_selection(&selected, tasks, od, false)
            });
        }
        ID_DISK_SAFE => {
            let mut st = state().lock().unwrap();
            if let Some(d) = st.data.as_ref() {
                st.clean_checked = d.clean_targets.iter().map(|t| t.risk == CleanRisk::Safe).collect();
            }
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        ID_DISK_ALL => {
            let mut st = state().lock().unwrap();
            for x in &mut st.clean_checked {
                *x = true;
            }
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        ID_DISK_CLEAR => {
            let mut st = state().lock().unwrap();
            for x in &mut st.clean_checked {
                *x = false;
            }
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        ID_DISK_CLEAN => {
            let targets: Vec<CleanTarget> = {
                let st = state().lock().unwrap();
                let Some(d) = st.data.as_ref() else { return };
                d.clean_targets
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| st.clean_checked.get(*i).copied().unwrap_or(false))
                    .map(|(_, t)| t.clone())
                    .collect()
            };
            if targets.is_empty() {
                show_info(hwnd, "No Selection", "Tick at least one cache category to clean.");
                return;
            }
            let total: u64 = targets.iter().map(|t| t.bytes).sum();
            if !ask_confirm(
                hwnd,
                "Confirm Cache Cleanup",
                &format!("Delete files inside {} selected cache categories (~{})?", targets.len(), human(total)),
            ) {
                return;
            }
            spawn_action("Cleaning selected disk caches", move || {
                let (freed, log) = clean_selected_targets(&targets);
                Ok(format!("Cleaned {} of cached files. Log: {}", human(freed), log.display()))
            });
        }
        ID_DISK_WIZTREE | ID_TOOL_WIZTREE => {
            let _ = run_launch("wiztree", &[]);
        }
        ID_TOOL_TASKMGR => {
            let _ = run_launch("taskmgr", &[]);
        }
        ID_TOOL_SETTINGS => {
            let _ = run_launch("settings", &[]);
        }
        _ if (ID_REP_ROW_BASE..ID_REP_ROW_BASE + 8).contains(&id) => {
            let idx = id - ID_REP_ROW_BASE;
            let mut st = state().lock().unwrap();
            st.replace_selected = idx;
            st.replace_checked[idx] = !st.replace_checked[idx];
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        _ if id >= ID_BLOAT_ROW_BASE && id < ID_DISK_ROW_BASE => {
            let idx = id - ID_BLOAT_ROW_BASE;
            let mut st = state().lock().unwrap();
            if let Some(v) = st.appx_checked.get_mut(idx) {
                *v = !*v;
            }
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        _ if id >= ID_DISK_ROW_BASE && id < ID_STARTUP_ROW_BASE => {
            let idx = id - ID_DISK_ROW_BASE;
            let mut st = state().lock().unwrap();
            if let Some(v) = st.clean_checked.get_mut(idx) {
                *v = !*v;
            }
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        _ if id >= ID_STARTUP_ROW_BASE => {
            let idx = id - ID_STARTUP_ROW_BASE;
            let item = {
                let st = state().lock().unwrap();
                st.data.as_ref().and_then(|d| d.startup_items.get(idx)).cloned()
            };
            if let Some(it) = item {
                if it.needs_admin() && !elevated() {
                    show_info(hwnd, "Admin Required", "This HKLM/Task/Service startup entry requires Admin rights to toggle.");
                    return;
                }
                let next = !it.enabled;
                spawn_action(&format!("Toggling startup item {}", it.name), move || {
                    set_item_enabled(&it, next)?;
                    Ok(format!(
                        "Startup item '{}' is now {}.",
                        it.name,
                        if next { "ENABLED" } else { "DISABLED" }
                    ))
                });
            }
        }
        _ => {}
    }
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: usize, lp: isize) -> isize {
    match msg {
        WM_ERASEBKGND => 1,
        WM_APP_REFRESH | WM_APP_ACTION => {
            InvalidateRect(hwnd, null(), 0);
            0
        }
        WM_MOUSEMOVE => {
            let s = scale_for_window();
            let x = (lp as i16) as f32 / s;
            let y = ((lp >> 16) as i16) as f32 / s;
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut tme);
            let changed = {
                let mut st = state().lock().unwrap();
                let next = hit_test(&st.hits, x, y).unwrap_or(0);
                let ch = next != st.hover_id;
                st.hover_id = next;
                ch
            };
            if changed {
                InvalidateRect(hwnd, null(), 0);
            }
            0
        }
        WM_MOUSELEAVE_EVT => {
            let changed = {
                let mut st = state().lock().unwrap();
                let ch = st.hover_id != 0;
                st.hover_id = 0;
                ch
            };
            if changed {
                InvalidateRect(hwnd, null(), 0);
            }
            0
        }
        WM_LBUTTONUP => {
            let s = scale_for_window();
            let x = (lp as i16) as f32 / s;
            let y = ((lp >> 16) as i16) as f32 / s;
            let id = {
                let st = state().lock().unwrap();
                hit_test(&st.hits, x, y)
            };
            if let Some(id) = id {
                handle_click(hwnd, id);
            }
            0
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp >> 16) as i16) as f32;
            let mut st = state().lock().unwrap();
            let t = st.tab;
            if t < 5 {
                let next = st.scroll[t] - (delta / 120.0) * 42.0;
                st.scroll[t] = clamp_scroll(next, st.max_scroll[t]);
                InvalidateRect(hwnd, null(), 0);
            }
            0
        }
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut cr: RECT = zeroed();
            GetClientRect(hwnd, &mut cr);
            let (w, h) = (cr.right.max(1), cr.bottom.max(1));
            let memdc = CreateCompatibleDC(dc);
            let bmp = CreateCompatibleBitmap(dc, w, h);
            let old = SelectObject(memdc, bmp);
            let mut g = null_mut::<GpGraphics>();
            GdipCreateFromHDC(memdc, &mut g);
            GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
            let ui = Ui {
                dc: memdc,
                g,
                s: scale_for_window(),
            };
            paint_window(hwnd, &ui);
            GdipDeleteGraphics(g);
            BitBlt(dc, 0, 0, w, h, memdc, 0, 0, SRCCOPY);
            SelectObject(memdc, old);
            DeleteObject(bmp);
            DeleteDC(memdc);
            EndPaint(hwnd, &ps);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

pub fn run_window() -> i32 {
    winrust_common::utf8_console();
    unsafe {
        let (mut token, mut input): (usize, GdiplusStartupInput) = (0, zeroed());
        input.GdiplusVersion = 1;
        GdiplusStartup(&mut token, &input, null_mut());
        let hi = GetModuleHandleW(null());
        let cls = wide("GpPcBoostWindow");
        let wc = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hi,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            lpszClassName: cls.as_ptr(),
            ..zeroed()
        };
        RegisterClassW(&wc);
        let s = scale_for_window();
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut r = RECT {
            left: 0,
            top: 0,
            right: (WIN_W * s).round() as i32,
            bottom: (WIN_H * s).round() as i32,
        };
        AdjustWindowRect(&mut r, style, 0);
        let title = wide("GP PC Boost — WinRust Replacement & Speedup Manager");
        let hwnd = CreateWindowExW(
            0,
            cls.as_ptr(),
            title.as_ptr(),
            style | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            r.right - r.left,
            r.bottom - r.top,
            null_mut(),
            null_mut(),
            hi,
            null(),
        );
        if hwnd.is_null() {
            return 1;
        }
        HWND_MAIN.store(hwnd as isize, Ordering::Relaxed);
        DwmSetWindowAttribute(hwnd, 20, &1i32 as *const i32 as *const _, 4);
        DwmSetWindowAttribute(hwnd, 35, &HEAD_BG as *const u32 as *const _, 4);
        spawn_refresh();

        let mut m: MSG = zeroed();
        while GetMessageW(&mut m, null_mut(), 0, 0) > 0 {
            TranslateMessage(&m);
            DispatchMessageW(&m);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_picks_topmost_matching_box() {
        let hits = vec![
            Hit { id: 1, b: [10.0, 10.0, 100.0, 50.0] },
            Hit { id: 2, b: [20.0, 15.0, 80.0, 40.0] },
        ];
        assert_eq!(hit_test(&hits, 50.0, 25.0), Some(2));
        assert_eq!(hit_test(&hits, 15.0, 25.0), Some(1));
        assert_eq!(hit_test(&hits, 200.0, 200.0), None);
    }

    #[test]
    fn scroll_clamps_within_bounds() {
        assert_eq!(clamp_scroll(-20.0, 100.0), 0.0);
        assert_eq!(clamp_scroll(45.0, 100.0), 45.0);
        assert_eq!(clamp_scroll(150.0, 100.0), 100.0);
        assert_eq!(clamp_scroll(50.0, 0.0), 0.0);
    }
}
