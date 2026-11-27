//! Multi-monitor event-driven Fluent 2 Windows 12 taskbar runtime.

pub mod anim;
pub mod config;
pub mod coucou;
pub mod dwm;
pub mod icons;
pub mod search;
pub mod sys_prefs;
pub mod telemetry;
pub mod tray;
pub mod ui;
pub mod windows;

use anim::{MOTION_COLLAPSE, MOTION_EXPAND, MOTION_FOLLOW, MOTION_HIDE, MOTION_PEEK, SpringVal, VsyncPacer};
use config::{AlignPref, Config, DockEdge, DockSize, GamingModePref, LayoutMode, MultiMonPref, ThemePref, load_config, save_config};
use coucou::{
    CoucouState, launch_coucou, send_coucou_cmd_async, send_coucou_json_async, send_coucou_json_sync,
    spawn_poller as spawn_coucou_poller,
};
use dwm::{
    BackdropKind, Surface, TextRenderer, apply_backdrop, apply_frameless_layered,
    move_layered_alpha, present_layered, present_layered_alpha,
};
use icons::{IconBitmap, IconCache};
use search::{SearchEngine, SearchResult, SearchScope, execute_result, paste_from_clipboard};
use std::{
    cell::RefCell,
    env,
    fs::OpenOptions,
    io::Write,
    mem::{size_of, zeroed},
    path::PathBuf,
    ptr::{null, null_mut},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicIsize, Ordering::Relaxed},
    },
    thread,
    time::Duration,
};
use sys_prefs::{SysPrefs, read_system_prefs};
use telemetry::{TelemetryState, spawn_poller};
use tray::{CopyDataStruct, TrayManager, broadcast_taskbar_created, find_other_shell_tray};
use ui::{
    CoucouHubAction, CoucouHubLayout, DragVisual, HitZone, MonitorLayout, Palette, PopupAction,
    PreviewHit, PreviewLayout, SearchBarHit, SearchBarLayout, SearchPanelHit, SearchPanelLayout,
    SurfaceKind, TrayFlyoutLayout, compute_coucou_hub_layout, compute_monitor_layout,
    compute_popup_layout, compute_preview_layout, compute_search_bar_layout,
    compute_search_panel_layout, compute_tray_flyout_layout, fluent_palette, hit_test,
    hit_test_coucou_hub, hit_test_popup, hit_test_preview, hit_test_search_bar,
    hit_test_search_panel, hit_test_tray_flyout, paint_coucou_hub, paint_dock, paint_drag_ghost,
    paint_popup, paint_preview, paint_search_bar, paint_search_panel, paint_tooltip,
    paint_top_center, paint_top_left, paint_top_right, paint_tray_flyout, sc, zone_anchor,
};
use windows::{
    AppItem, MonitorSpec, PinnedApp, TaskbarMenuCmd, WindowTracker, active_window_summary,
    activate_or_toggle, apply_custom_order, bring_to_front, close_window_graceful,
    enumerate_monitors, filter_items_for_monitor, hide_all_stock_taskbars, input_language_status,
    launch_as_admin, launch_new_instance, launch_path, load_custom_order, load_pins,
    pin_dropped_file, restore_all_stock_taskbars, save_custom_order, send_chord,
    show_app_context_menu, show_taskbar_context_menu, show_window_context_menu, wide, window_title,
};
use windows_sys::Win32::{
    Foundation::*,
    Globalization::{GetDateFormatW, GetTimeFormatW},
    Graphics::{
        Dwm::{
            DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION,
            DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE, DWMWA_CLOAKED, DwmGetWindowAttribute,
            DwmQueryThumbnailSourceSize, DwmRegisterThumbnail, DwmUnregisterThumbnail,
            DwmUpdateThumbnailProperties,
        },
        Gdi::{
            BeginPaint, BitBlt, ClientToScreen, EndPaint, GetMonitorInfoW, InvalidateRect,
            MONITOR_DEFAULTTOPRIMARY, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
            MonitorFromWindow, PAINTSTRUCT, SRCCOPY,
        },
        GdiPlus::{GdiplusShutdown, GdiplusStartup, GdiplusStartupInput},
    },
    System::{
        LibraryLoader::GetModuleHandleW,
        SystemInformation::{GetLocalTime, GetTickCount64},
        Threading::{
            CreateMutexW, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
        },
    },
    UI::{
        Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent},
        Input::KeyboardAndMouse::{
            GetCapture, GetKeyState, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE,
            TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LWIN, VK_RETURN,
            VK_RWIN, VK_SHIFT, VK_TAB, VK_UP,
        },
        Shell::{
            DragAcceptFiles, DragFinish, DragQueryFileW, HDROP, QUNS_PRESENTATION_MODE,
            QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
        },
        WindowsAndMessaging::*,
    },
};

const WM_MOUSELEAVE_EVT: u32 = 0x02A3;
const WM_DWMCOLORIZATIONCOLORCHANGED: u32 = 0x0320;
const WM_APP_ICON_READY: u32 = WM_APP + 10;
const WM_APP_TELEM_CHANGED: u32 = WM_APP + 11;
const WM_APP_COUCOU_CHANGED: u32 = WM_APP + 12;
const WM_APP_COUCOU_EXPANDED: u32 = WM_APP + 13;
const WM_APP_OPEN_SEARCH: u32 = WM_APP + 14;
const WM_APP_ANIM_TICK: u32 = WM_APP + 15;
const WM_APP_COUCOU_RECT: u32 = WM_APP + 16;
const WM_APP_WIN_KEY_TAP: u32 = WM_APP + 17;

static KB_HOST_HWND: AtomicIsize = AtomicIsize::new(0);
static WIN_KEY_DOWN: AtomicBool = AtomicBool::new(false);
static WIN_KEY_COMBO: AtomicBool = AtomicBool::new(false);

const TIMER_CLOCK: usize = 1;
const TIMER_EVENTS: usize = 2;
const TIMER_TIP: usize = 4;
const TIMER_AUTOHIDE_HOLD: usize = 5;
const TIMER_SMOKE_DONE: usize = 6;
const TIMER_PREVIEW_SHOW: usize = 7;
const TIMER_PREVIEW_HIDE: usize = 8;
const TIMER_STATUS_HIDE: usize = 9;
const TIMER_SEARCH_CARET: usize = 12;

#[derive(Clone, Copy)]
struct DragState {
    mon_idx: usize,
    start_x: i32,
    cur_x: i32,
    cur_idx: usize,
    active: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TrayDragOrigin {
    FromBar { slot: usize },
    FromFlyout { cell_idx: usize },
}

#[derive(Clone)]
struct TrayDragState {
    origin: TrayDragOrigin,
    mon_idx: usize,
    key: String,
    bitmap: Option<Arc<IconBitmap>>,
    start_pt: POINT,
    cur_pt: POINT,
    active: bool,
    over_bar_slot: Option<usize>,
    over_hide: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct CoucouLiveState {
    /// 0 = hidden/idle, 1 = live horizontal pill (music/agents), 2 = full expanded panel (user clicked Mochi)
    mode: u8,
    left: i32,
    right: i32,
    hmon: isize,
}

struct MonWindows {
    spec: MonitorSpec,
    lay: MonitorLayout,
    items: Vec<AppItem>,
    hwnd_tl: HWND,
    hwnd_tc: HWND,
    hwnd_tr: HWND,
    hwnd_dock: HWND,
    hwnd_sensor: HWND,
    hwnd_status_sensor: HWND,
    surf_tl: Surface,
    surf_tc: Surface,
    surf_tr: Surface,
    surf_dock: Surface,
    text_ren: TextRenderer,
    fullscreen: bool,
    anim_spring: SpringVal,
    mouse_inside: bool,
    status_spring: SpringVal,
    status_mouse_inside: bool,
    tc_x_spring: SpringVal,
    paints: u64,
}

impl MonWindows {
    unsafe fn status_bars(&self) -> [HWND; 3] {
        [self.hwnd_tl, self.hwnd_tc, self.hwnd_tr]
    }

    fn kind_for(&self, hwnd: HWND) -> Option<SurfaceKind> {
        if hwnd == self.hwnd_tl {
            Some(SurfaceKind::TopLeft)
        } else if hwnd == self.hwnd_tc {
            Some(SurfaceKind::TopCenter)
        } else if hwnd == self.hwnd_tr {
            Some(SurfaceKind::TopRight)
        } else if hwnd == self.hwnd_dock {
            Some(SurfaceKind::Dock)
        } else {
            None
        }
    }

    unsafe fn destroy_all(&mut self) {
        for hwnd in [
            &mut self.hwnd_tl,
            &mut self.hwnd_tc,
            &mut self.hwnd_tr,
            &mut self.hwnd_dock,
            &mut self.hwnd_sensor,
            &mut self.hwnd_status_sensor,
        ] {
            if !(*hwnd).is_null() {
                DestroyWindow(*hwnd);
                *hwnd = null_mut();
            }
        }
    }
}

struct AppState {
    hi: HINSTANCE,
    cfg: Config,
    sys: SysPrefs,
    pal: Palette,
    pins: Vec<PinnedApp>,
    tracker: WindowTracker,
    all_items: Vec<AppItem>,
    icons: IconCache,
    tray: TrayManager,
    telem: Arc<TelemetryState>,
    coucou: Arc<CoucouState>,
    pacer: Arc<VsyncPacer>,
    search: SearchEngine,
    monitors: Vec<MonWindows>,

    hwnd_host: HWND,
    hwnd_tray_host: HWND,
    hwnd_tray_fly: HWND,
    hwnd_coucou_hub: HWND,
    hwnd_search_bar: HWND,
    hwnd_search_panel: HWND,
    hwnd_drag_ghost: HWND,
    hwnd_popup: HWND,
    hwnd_tip: HWND,
    hwnd_preview: HWND,
    surf_tray_fly: Surface,
    surf_coucou_hub: Surface,
    surf_search_bar: Surface,
    surf_search_panel: Surface,
    surf_drag_ghost: Surface,
    surf_popup: Surface,
    surf_tip: Surface,
    surf_preview: Surface,
    ui_text_ren: TextRenderer,
    popup_rect: RECT,
    tip_rect: RECT,

    tray_fly_open: bool,
    tray_fly_layout: Option<TrayFlyoutLayout>,
    tray_fly_hover: Option<usize>,
    tray_fly_mon: usize,
    tray_fly_spring: SpringVal,
    tray_drag: Option<TrayDragState>,

    coucou_hub_open: bool,
    coucou_hub_layout: Option<CoucouHubLayout>,
    coucou_hub_hover: Option<CoucouHubAction>,
    coucou_hub_mon: usize,
    coucou_hub_spring: SpringVal,
    coucou_seen_notice_ms: u64,
    coucou_anim_tick: u32,
    coucou_launching: bool,
    coucou_live: CoucouLiveState,

    search_open: bool,
    search_mon: usize,
    search_query: String,
    search_scope: SearchScope,
    search_results: Vec<SearchResult>,
    search_selected: usize,
    search_bar_layout: Option<SearchBarLayout>,
    search_panel_layout: Option<SearchPanelLayout>,
    search_bar_hover: SearchBarHit,
    search_panel_hover: SearchPanelHit,
    search_spring: SpringVal,
    search_panel_spring: SpringVal,
    search_caret_on: bool,

    hot: Option<(usize, SurfaceKind, HitZone)>,
    pressed: Option<(usize, SurfaceKind, HitZone)>,
    drag: Option<DragState>,
    custom_order: Vec<String>,
    tracking_hwnd: HWND,
    popup_open: bool,
    popup_mon: usize,
    popup_spring: SpringVal,
    menu_open: bool,
    start_menu_open: bool,
    start_menu_arrived: bool,
    win_key_override_until: u64,
    popup_hover: Option<PopupAction>,
    tip_text: String,
    tip_target: Option<(usize, SurfaceKind, HitZone)>,
    tip_last_closed_ms: u64,

    preview_open: bool,
    preview_mon: usize,
    preview_layout: Option<PreviewLayout>,
    preview_thumbs: Vec<isize>,
    preview_icon_key: String,
    preview_target: Option<(usize, usize)>,
    preview_hover: PreviewHit,
    preview_tracking: bool,
    preview_last_closed_ms: u64,

    workspace: String,
    lang_code: String,
    time_str: String,
    date_str: String,
    split_clock_str: String,
    primary_work_reserved: Option<RECT>,
    shell_hook_msg: u32,
    taskbar_created_msg: u32,
    event_dirty: bool,
    smoke_test: bool,
    total_paints: u64,
    ulw_ok: u64,
}

thread_local! {
    static APP: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

fn with_app<R>(f: impl FnOnce(&mut AppState) -> R) -> Option<R> {
    APP.with(|cell| {
        let mut borrow = cell.try_borrow_mut().ok()?;
        borrow.as_mut().map(f)
    })
}

fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let path = env::temp_dir().join("gp-taskbar-crash.log");
        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&path) {
            let loc = info.location().map_or_else(|| "unknown".into(), |l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
            let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = info.payload().downcast_ref::<String>() {
                s.clone()
            } else {
                "non-string panic payload".into()
            };
            let _ = writeln!(f, "gp-taskbar panic at {loc}: {msg}");
        }
        prev(info);
    }));
}

pub fn run() {
    install_panic_hook();
    unsafe {
        if env::args().any(|a| a == "--quit") {
            let prev = FindWindowW(wide("gp_taskbar_host").as_ptr(), null());
            if !prev.is_null() {
                PostMessageW(prev, WM_CLOSE, 0, 0);
            }
            return;
        }
        if env::args().any(|a| a == "--restore") {
            restore_system();
            return;
        }
        let smoke_test = env::args().any(|a| a == "--smoke-test");

        let mutex_name = if smoke_test { "Local\\gp-taskbar-smoke" } else { "Local\\gp-taskbar-single" };
        if !smoke_test {
            let prev = FindWindowW(wide("gp_taskbar_host").as_ptr(), null());
            if !prev.is_null() {
                PostMessageW(prev, WM_CLOSE, 0, 0);
            }
        }
        let mut mutex = null_mut();
        let tries = if smoke_test { 1 } else { 30 };
        for _ in 0..tries {
            let m = CreateMutexW(null(), 0, wide(mutex_name).as_ptr());
            if !m.is_null() && GetLastError() != ERROR_ALREADY_EXISTS {
                mutex = m;
                break;
            }
            if !m.is_null() {
                CloseHandle(m);
            }
            if tries > 1 {
                thread::sleep(Duration::from_millis(100));
            }
        }
        if mutex.is_null() {
            return;
        }

        SetProcessDPIAware();

        let mut gdi_tok: usize = 0;
        let mut gdi_in: GdiplusStartupInput = zeroed();
        gdi_in.GdiplusVersion = 1;
        GdiplusStartup(&mut gdi_tok, &gdi_in, null_mut());

        let hi = GetModuleHandleW(null());
        register_classes(hi);

        let mut cfg = load_config();
        for arg in env::args() {
            match arg.as_str() {
                "--top" => cfg.layout = LayoutMode::UnifiedTop,
                "--bottom" => cfg.layout = LayoutMode::UnifiedBottom,
                "--split" => cfg.layout = LayoutMode::Win12Split,
                "--compact" => cfg.dock_size = DockSize::Compact,
                "--small" => cfg.dock_size = DockSize::Small,
                "--standard" => cfg.dock_size = DockSize::Standard,
                "--large" => cfg.dock_size = DockSize::Large,
                "--autohide" => cfg.autohide = true,
                _ => {}
            }
        }

        let sys = read_system_prefs();
        let pal = fluent_palette(&cfg, &sys);
        let pins = load_pins();
        let custom_order = load_custom_order();
        let mut tracker = WindowTracker::new();
        let mut all_items = tracker.build_items(&pins);
        apply_custom_order(&mut all_items, &custom_order);
        let telem = Arc::new(TelemetryState::new());
        let coucou = Arc::new(CoucouState::new());
        let pacer = VsyncPacer::new();
        let search = SearchEngine::new();
        let icons = IconCache::new();
        let tray = TrayManager::new();
        let (time_str, date_str, split_clock_str) = format_clock_all(cfg.show_seconds);
        let workspace = active_window_summary(std::process::id());
        let (_, lang_code) = input_language_status(tracker.last_app_window());
        let shell_hook_msg = RegisterWindowMessageW(wide("SHELLHOOK").as_ptr());
        let taskbar_created_msg = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());

        let h_host = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            wide("gp_taskbar_host").as_ptr(),
            wide("gp_taskbar_host").as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hi,
            null(),
        );
        if !h_host.is_null() {
            let _ = ChangeWindowMessageFilterEx(h_host, WM_CLOSE, MSGFLT_ALLOW, null_mut());
            let _ = ChangeWindowMessageFilterEx(h_host, WM_APP_COUCOU_EXPANDED, MSGFLT_ALLOW, null_mut());
            let _ = ChangeWindowMessageFilterEx(h_host, WM_APP_OPEN_SEARCH, MSGFLT_ALLOW, null_mut());
            let _ = ChangeWindowMessageFilterEx(h_host, WM_APP_COUCOU_RECT, MSGFLT_ALLOW, null_mut());
            if taskbar_created_msg != 0 {
                let _ = ChangeWindowMessageFilterEx(h_host, taskbar_created_msg, MSGFLT_ALLOW, null_mut());
            }
            if !smoke_test {
                RegisterShellHookWindow(h_host);
            }
        }
        // Hidden topmost Shell_TrayWnd host so background apps (Steam, Discord, Docker, etc.)
        // register their notification-area icons with us via Shell_NotifyIconW (WM_COPYDATA).
        let h_tray_host = if !smoke_test {
            let th = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                wide("Shell_TrayWnd").as_ptr(),
                null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                hi,
                null(),
            );
            if !th.is_null() {
                let _ = ChangeWindowMessageFilterEx(th, WM_COPYDATA, MSGFLT_ALLOW, null_mut());
                SetWindowPos(th, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
            th
        } else {
            null_mut()
        };
        let h_tray_fly = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            wide("gp_taskbar_tray_fly").as_ptr(),
            wide("GP Tray Overflow").as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            null_mut(),
            null_mut(),
            hi,
            null(),
        );
        let h_coucou_hub = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            wide("gp_taskbar_coucou_hub").as_ptr(),
            wide("Coucou Hub").as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            null_mut(),
            null_mut(),
            hi,
            null(),
        );
        let h_search_bar = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            wide("gp_taskbar_search_bar").as_ptr(),
            wide("GP Search Bar").as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            null_mut(),
            null_mut(),
            hi,
            null(),
        );
        let h_search_panel = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            wide("gp_taskbar_search_panel").as_ptr(),
            wide("GP Search Results").as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            null_mut(),
            null_mut(),
            hi,
            null(),
        );
        let h_drag_ghost = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
            wide("gp_taskbar_tip").as_ptr(),
            wide("GP Drag Ghost").as_ptr(),
            WS_POPUP,
            0,
            0,
            32,
            32,
            null_mut(),
            null_mut(),
            hi,
            null(),
        );
        let h_popup = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            wide("gp_taskbar_popup").as_ptr(),
            wide("GP Settings").as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            null_mut(),
            null_mut(),
            hi,
            null(),
        );
        let h_tip = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            wide("gp_taskbar_tip").as_ptr(),
            wide("GP Tip").as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            null_mut(),
            null_mut(),
            hi,
            null(),
        );
        // Non-layered WS_POPUP so DwmRegisterThumbnail composites live 60fps window previews cleanly
        let h_preview = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            wide("gp_taskbar_preview").as_ptr(),
            wide("GP Preview").as_ptr(),
            WS_POPUP,
            0,
            0,
            10,
            10,
            null_mut(),
            null_mut(),
            hi,
            null(),
        );
        let pop_mat = if pal.transparent { BackdropKind::Acrylic } else { BackdropKind::Solid };
        // Frameless per-pixel ARGB layered surfaces (strips DWM NC-shadow, rounded clip,
        // 1px hardware border, blur-behind, and DWM show/hide transitions) so morphs,
        // dynamic result-panel resizes, and close fades NEVER leave ghost frame residues!
        for hw in [h_tray_fly, h_coucou_hub, h_search_bar, h_search_panel, h_popup] {
            if !hw.is_null() {
                apply_frameless_layered(hw, pal.is_light);
            }
        }
        if !h_tip.is_null() {
            apply_backdrop(h_tip, pal.is_light, pop_mat, pal.border_ref);
        }
        if !h_preview.is_null() {
            apply_backdrop(h_preview, pal.is_light, BackdropKind::Solid, pal.border_ref);
        }

        if !smoke_test {
            hide_all_stock_taskbars();
        }

        telem.host_hwnd.store(h_host as isize, Relaxed);
        coucou.host_hwnd.store(h_host as isize, Relaxed);
        if !h_host.is_null() {
            pacer.bind_and_start(h_host, WM_APP_ANIM_TICK);
        }

        let state = AppState {
            hi,
            cfg,
            sys,
            pal,
            pins,
            tracker,
            all_items,
            icons,
            tray,
            telem: Arc::clone(&telem),
            coucou: Arc::clone(&coucou),
            pacer: Arc::clone(&pacer),
            search,
            monitors: Vec::new(),
            hwnd_host: h_host,
            hwnd_tray_host: h_tray_host,
            hwnd_tray_fly: h_tray_fly,
            hwnd_coucou_hub: h_coucou_hub,
            hwnd_search_bar: h_search_bar,
            hwnd_search_panel: h_search_panel,
            hwnd_drag_ghost: h_drag_ghost,
            hwnd_popup: h_popup,
            hwnd_tip: h_tip,
            hwnd_preview: h_preview,
            surf_tray_fly: Surface::new(),
            surf_coucou_hub: Surface::new(),
            surf_search_bar: Surface::new(),
            surf_search_panel: Surface::new(),
            surf_drag_ghost: Surface::new(),
            surf_popup: Surface::new(),
            surf_tip: Surface::new(),
            surf_preview: Surface::new(),
            ui_text_ren: TextRenderer::new(1.0),
            popup_rect: RECT { left: 0, top: 0, right: 10, bottom: 10 },
            tip_rect: RECT { left: 0, top: 0, right: 10, bottom: 10 },
            tray_fly_open: false,
            tray_fly_layout: None,
            tray_fly_hover: None,
            tray_fly_mon: 0,
            tray_fly_spring: SpringVal::new(0.0),
            tray_drag: None,
            coucou_hub_open: false,
            coucou_hub_layout: None,
            coucou_hub_hover: None,
            coucou_hub_mon: 0,
            coucou_hub_spring: SpringVal::new(0.0),
            coucou_seen_notice_ms: 0,
            coucou_anim_tick: 0,
            coucou_launching: false,
            coucou_live: CoucouLiveState::default(),
            search_open: false,
            search_mon: 0,
            search_query: String::new(),
            search_scope: SearchScope::All,
            search_results: Vec::new(),
            search_selected: 0,
            search_bar_layout: None,
            search_panel_layout: None,
            search_bar_hover: SearchBarHit::None,
            search_panel_hover: SearchPanelHit::None,
            search_spring: SpringVal::new(0.0),
            search_panel_spring: SpringVal::new(0.0),
            search_caret_on: true,
            hot: None,
            pressed: None,
            drag: None,
            custom_order,
            tracking_hwnd: null_mut(),
            popup_open: false,
            popup_mon: 0,
            popup_spring: SpringVal::new(0.0),
            menu_open: false,
            start_menu_open: false,
            start_menu_arrived: false,
            win_key_override_until: 0,
            popup_hover: None,
            tip_text: String::new(),
            tip_target: None,
            tip_last_closed_ms: 0,
            preview_open: false,
            preview_mon: 0,
            preview_layout: None,
            preview_thumbs: Vec::new(),
            preview_icon_key: String::new(),
            preview_target: None,
            preview_hover: PreviewHit::None,
            preview_tracking: false,
            preview_last_closed_ms: 0,
            workspace,
            lang_code,
            time_str,
            date_str,
            split_clock_str,
            primary_work_reserved: None,
            shell_hook_msg,
            taskbar_created_msg,
            event_dirty: false,
            smoke_test,
            total_paints: 0,
            ulw_ok: 0,
        };

        APP.with(|cell| *cell.borrow_mut() = Some(state));

        rebuild_monitors_safe();
        if !smoke_test {
            // Ask every already-running background app to register its tray icon with our Shell_TrayWnd
            broadcast_taskbar_created();
        }
        if !h_host.is_null() {
            SetTimer(h_host, TIMER_CLOCK, 1000, None);
            if smoke_test {
                SetTimer(h_host, TIMER_SMOKE_DONE, 1200, None);
            }
        }
        spawn_poller(telem, WM_APP_TELEM_CHANGED);
        if !smoke_test {
            spawn_coucou_poller(coucou, WM_APP_COUCOU_CHANGED);
            with_app(|st| st.search.spawn_indexer());
        }

        let mut hooks: Vec<HWINEVENTHOOK> = Vec::new();
        for (a, b) in [
            (0x0003, 0x0003), // EVENT_SYSTEM_FOREGROUND
            (0x0016, 0x0017), // EVENT_SYSTEM_MINIMIZESTART..END
            (0x8000, 0x8003), // OBJECT_CREATE..HIDE
            (0x800C, 0x800C), // OBJECT_NAMECHANGE
            (0x8017, 0x8018), // OBJECT_CLOAKED..UNCLOAKED
        ] {
            let hk = SetWinEventHook(a, b, null_mut(), Some(win_event_hook), 0, 0, 2);
            if !hk.is_null() {
                hooks.push(hk);
            }
        }

        let kb_hook = if !smoke_test && !h_host.is_null() {
            KB_HOST_HWND.store(h_host as isize, Relaxed);
            WIN_KEY_DOWN.store(false, Relaxed);
            WIN_KEY_COMBO.store(false, Relaxed);
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(low_level_keyboard_proc), hi, 0)
        } else {
            null_mut()
        };

        let mut msg: MSG = zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        if !kb_hook.is_null() {
            UnhookWindowsHookEx(kb_hook);
            KB_HOST_HWND.store(0, Relaxed);
        }
        for hk in hooks {
            UnhookWinEvent(hk);
        }

        close_preview_safe();
        close_tray_fly_safe();
        close_coucou_hub_safe();
        if !h_host.is_null() && !smoke_test {
            DeregisterShellHookWindow(h_host);
        }
        let (old_mons, detach_coucou) = with_app(|st| {
            st.pacer.shutdown();
            st.telem.stop.store(true, Relaxed);
            st.coucou.stop.store(true, Relaxed);
            let detach = !st.smoke_test && st.coucou.snapshot().running;
            st.primary_work_reserved = None;
            st.tray.clear();
            (std::mem::take(&mut st.monitors), detach)
        })
        .unwrap_or_else(|| (Vec::new(), false));
        if detach_coucou {
            let _ = send_coucou_json_sync(r#"{"taskbar_cmd":"dock_detach"}"#);
        }
        for mut m in old_mons {
            m.destroy_all();
        }
        if !h_tray_host.is_null() {
            DestroyWindow(h_tray_host);
        }
        if !h_tray_fly.is_null() {
            DestroyWindow(h_tray_fly);
        }
        if !h_coucou_hub.is_null() {
            DestroyWindow(h_coucou_hub);
        }
        if !h_search_bar.is_null() {
            DestroyWindow(h_search_bar);
        }
        if !h_search_panel.is_null() {
            DestroyWindow(h_search_panel);
        }
        if !h_drag_ghost.is_null() {
            DestroyWindow(h_drag_ghost);
        }
        if !h_popup.is_null() {
            DestroyWindow(h_popup);
        }
        if !h_tip.is_null() {
            DestroyWindow(h_tip);
        }
        if !h_preview.is_null() {
            DestroyWindow(h_preview);
        }
        APP.with(|cell| *cell.borrow_mut() = None);

        if !smoke_test {
            restore_system();
        }
        GdiplusShutdown(gdi_tok);
        CloseHandle(mutex);
    }
}

unsafe fn register_classes(hi: HINSTANCE) {
    let classes: [(&str, WNDPROC); 11] = [
        ("gp_taskbar_host", Some(host_proc)),
        ("Shell_TrayWnd", Some(tray_host_proc)),
        ("gp_taskbar_tray_fly", Some(tray_fly_proc)),
        ("gp_taskbar_coucou_hub", Some(coucou_hub_proc)),
        ("gp_taskbar_search_bar", Some(search_bar_proc)),
        ("gp_taskbar_search_panel", Some(search_panel_proc)),
        ("gp_taskbar_surface", Some(surface_proc)),
        ("gp_taskbar_popup", Some(popup_proc)),
        ("gp_taskbar_tip", Some(tip_proc)),
        ("gp_taskbar_preview", Some(preview_proc)),
        ("gp_taskbar_sensor", Some(sensor_proc)),
    ];
    for (name, proc) in classes {
        let w = wide(name);
        let wc = WNDCLASSW {
            lpfnWndProc: proc,
            hInstance: hi,
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            lpszClassName: w.as_ptr(),
            ..zeroed()
        };
        RegisterClassW(&wc);
    }
}

unsafe fn format_clock_all(show_seconds: bool) -> (String, String, String) {
    let mut t: SYSTEMTIME = zeroed();
    GetLocalTime(&mut t);
    let (mut tb, mut db) = ([0u16; 32], [0u16; 32]);
    let flags = if show_seconds { 0 } else { 2 }; // TIME_NOSECONDS = 2
    let tn = GetTimeFormatW(0x0400, flags, &t, null(), tb.as_mut_ptr(), 32).max(1) as usize - 1;
    let dn = GetDateFormatW(0x0400, 1, &t, null(), db.as_mut_ptr(), 32).max(1) as usize - 1;
    let time_s = String::from_utf16_lossy(&tb[..tn]);
    let date_s = String::from_utf16_lossy(&db[..dn]);
    let dow = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"]
        .get(t.wDayOfWeek as usize)
        .copied()
        .unwrap_or("");
    let mon = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
        .get(t.wMonth.saturating_sub(1) as usize)
        .copied()
        .unwrap_or("");
    let split_s = format!("{dow}, {mon} {}  ·  {time_s}", t.wDay);
    (time_s, date_s, split_s)
}

unsafe fn restore_system() {
    let mut full = RECT {
        left: 0,
        top: 0,
        right: GetSystemMetrics(SM_CXSCREEN),
        bottom: GetSystemMetrics(SM_CYSCREEN),
    };
    SystemParametersInfoW(SPI_SETWORKAREA, 0, &mut full as *mut _ as *mut _, 0);
    PostMessageW(HWND_BROADCAST, WM_SETTINGCHANGE, SPI_SETWORKAREA as usize, 0);
    restore_all_stock_taskbars();
    broadcast_taskbar_created();
}

impl AppState {
    fn effective_mm_mode(&self) -> (bool, u32) {
        match self.cfg.multimon {
            MultiMonPref::System => (self.sys.mm_enabled, self.sys.mm_mode),
            MultiMonPref::AllDisplays => (true, 0),
            MultiMonPref::PrimaryOnly => (false, 0),
        }
    }

    fn host_hwnd(&self) -> HWND {
        self.hwnd_host
    }

    /// True when Coucou (Mochi) should stay in universal floating rounded-pill docked mode
    /// (`Win12Split`, `UnifiedTop`, and `UnifiedBottom` all keep the exact same rounded floating
    /// Win12 pill shape; in Unified modes it sits on the free opposite edge with `own_wake: true`).
    fn coucou_container_active(&self) -> bool {
        !self.smoke_test && self.cfg.show_coucou && self.coucou.snapshot().running
    }

    fn coucou_pill_label(&self) -> String {
        let snap = self.coucou.snapshot();
        if !snap.running && self.coucou_launching {
            "Starting…".into()
        } else {
            snap.pill_summary().0
        }
    }

    fn coucou_span_for(&self, hmon: isize) -> Option<(i32, i32)> {
        if self.coucou_live.mode == 1 && self.coucou_live.hmon == hmon && self.coucou_live.right > self.coucou_live.left {
            Some((self.coucou_live.left, self.coucou_live.right))
        } else {
            None
        }
    }

    fn coucou_full_covering(&self, hmon: isize) -> bool {
        self.coucou_live.mode == 2 && self.coucou_live.hmon == hmon
    }
}

fn coucou_dock_cmd_payload(cfg: &Config, cmd: &str) -> String {
    let (edge, own_wake) = match cfg.layout {
        LayoutMode::Win12Split => (if cfg.dock_edge == DockEdge::Bottom { "top" } else { "bottom" }, false),
        LayoutMode::UnifiedTop => ("bottom", true),
        LayoutMode::UnifiedBottom => ("top", true),
    };
    format!(r#"{{"taskbar_cmd":"{cmd}","edge":"{edge}","own_wake":{own_wake}}}"#)
}

unsafe fn coucou_window_hwnd() -> HWND {
    FindWindowW(null(), wide("Coucou").as_ptr())
}

/// Ensures `hwnd_tc` stays strictly behind Coucou's live pill window in Z-order whenever
/// Coucou's compact pill (`mode == 1`) is docked inside the unified center bar.
unsafe fn keep_tc_behind_coucou_safe() {
    let tc_hw = with_app(|st| {
        if st.coucou_live.mode != 1 {
            return null_mut();
        }
        st.monitors
            .iter()
            .find(|m| m.spec.hmon == st.coucou_live.hmon)
            .map_or(null_mut(), |m| m.hwnd_tc)
    })
    .unwrap_or(null_mut());
    if tc_hw.is_null() || IsWindowVisible(tc_hw) == 0 {
        return;
    }
    let coucou_hw = coucou_window_hwnd();
    if !coucou_hw.is_null() {
        SetWindowPos(tc_hw, coucou_hw, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
    }
}

/// True when Coucou's real island window is currently visible & non-collapsed (height > 20px).
unsafe fn coucou_window_visible() -> bool {
    let hw = coucou_window_hwnd();
    if hw.is_null() || IsWindowVisible(hw) == 0 {
        return false;
    }
    let mut r: RECT = zeroed();
    GetWindowRect(hw, &mut r) != 0 && (r.bottom - r.top) > 20
}

/// Reacts to Coucou's live visual state (`WM_APP_COUCOU_RECT`):
///   - `mode == 0`: Coucou hidden/collapsed -> restore centered `[Search | Calendar | Mochi]`
///   - `mode == 1`: Coucou live horizontal pill (music/agents) -> expand `TopCenter` into ONE
///                  unified dock bar wrapping `[Search | Calendar | <live Coucou bay>]` placed
///                  strictly behind Coucou in Z-order, waking the bottom bar with Coucou!
///   - `mode == 2`: Coucou full interactive panel (user clicked Mochi) -> step `hwnd_tc` aside
///                  so Mochi fills the entire bottom-middle cleanly without overlap.
unsafe fn apply_coucou_live_state_safe(new_live: CoucouLiveState) {
    let mut to_hide: Vec<HWND> = Vec::new();
    let mut to_show: Vec<(usize, HWND, bool)> = Vec::new();
    let mut to_repaint: Vec<usize> = Vec::new();
    let mut wake_status_mons: Vec<usize> = Vec::new();
    let mut sched_status_hide: HWND = null_mut();
    let mut keep_search_on_top: Option<(HWND, HWND)> = None;
    let mut hide_coucou_for_fs = false;
    with_app(|st| {
        if st.smoke_test {
            return;
        }
        let prev = st.coucou_live;
        st.coucou_live = new_live;
        if st.cfg.layout == LayoutMode::Win12Split && st.cfg.gaming_mode != GamingModePref::Off && new_live.mode != 0 {
            let own_pid = std::process::id();
            let target_fs = st.monitors.iter().any(|m| {
                m.spec.hmon == new_live.hmon && (m.fullscreen || detect_fullscreen_on_monitor(m.spec.hmon, own_pid))
            });
            if target_fs {
                hide_coucou_for_fs = true;
                st.coucou_live = CoucouLiveState::default();
                return;
            }
        }
        // If Search is currently open on the same edge (Win12Split) and Coucou tried to wake up,
        // keep Search strictly on top and politely tuck Coucou back away so it NEVER covers Search!
        if st.cfg.layout == LayoutMode::Win12Split && st.search_open && new_live.mode != 0 {
            keep_search_on_top = Some((st.hwnd_search_bar, st.hwnd_search_panel));
            return;
        }
        if st.cfg.layout != LayoutMode::Win12Split {
            return;
        }
        let telem = st.telem.snapshot();
        let coucou_label = st.coucou_pill_label();
        let tray_bar_count = st.tray.bar_indices(st.cfg.tray_mode.limit()).len();
        let has_tray_chevron = !st.tray.all_visible_indices().is_empty();

        for idx in 0..st.monitors.len() {
            let hmon = st.monitors[idx].spec.hmon;
            let hw = st.monitors[idx].hwnd_tc;
            if hw.is_null() {
                continue;
            }
            let search_covering = st.search_open && st.search_mon == idx;
            let covered = st.coucou_full_covering(hmon);
            if covered {
                to_hide.push(hw);
                continue;
            }
            let span = st.coucou_span_for(hmon);
            let prev_tc = st.monitors[idx].lay.top_center;
            let prev_has_coucou = st.monitors[idx].lay.coucou_rect.is_some();
            let primary = st.monitors[idx].spec.primary;
            st.monitors[idx].lay = compute_monitor_layout(
                &st.monitors[idx].spec,
                &st.cfg,
                &st.sys,
                st.monitors[idx].items.len(),
                if primary { tray_bar_count } else { 0 },
                primary && has_tray_chevron,
                primary && !st.lang_code.is_empty(),
                telem.bat_pct >= 0,
                telem,
                &st.workspace,
                &st.split_clock_str,
                &coucou_label,
                span,
            );
            if let Some(tc) = st.monitors[idx].lay.top_center {
                let prev_w = prev_tc.map_or(0, |r| r.right - r.left);
                let new_w = tc.right - tc.left;
                let width_changed = prev_w != new_w || prev_has_coucou != st.monitors[idx].lay.coucou_rect.is_some();
                if (prev.mode == 2 && prev.hmon == hmon) || (prev.mode == 0 && new_live.mode == 1) {
                    st.monitors[idx].tc_x_spring.jump(tc.left as f32);
                } else {
                    st.monitors[idx].tc_x_spring.spring_to(tc.left as f32, MOTION_FOLLOW);
                    st.pacer.wake();
                }
                if width_changed {
                    to_repaint.push(idx);
                }
            }
            // When Coucou's live pill wakes on this monitor while status_autohide is enabled,
            // glide the bottom bar out with it so Search + Calendar stay fused inside the dock!
            if span.is_some()
                && !st.monitors[idx].fullscreen
                && !search_covering
                && st.cfg.status_autohide
                && st.monitors[idx].status_spring.target <= 0.5
            {
                wake_status_mons.push(idx);
            }
            if !st.monitors[idx].fullscreen
                && !search_covering
                && (span.is_some() || !st.cfg.status_autohide || st.monitors[idx].status_spring.value > 0.5)
            {
                to_show.push((idx, hw, span.is_some()));
            }
        }
        // If Coucou just hid back to mode 0 and status_autohide is active with cursor off-strip,
        // gracefully schedule the bottom bar to tuck back away.
        if prev.mode != 0 && new_live.mode == 0 && st.cfg.status_autohide && !st.search_open && !st.popup_open {
            sched_status_hide = st.host_hwnd();
        }
    });
    if hide_coucou_for_fs {
        send_coucou_json_async(r#"{"taskbar_cmd":"dock_hide"}"#.to_string());
        sync_fullscreen_state_safe();
        return;
    }
    if let Some((hb, hp)) = keep_search_on_top {
        send_coucou_json_async(r#"{"taskbar_cmd":"dock_hide"}"#.to_string());
        if !hb.is_null() {
            SetWindowPos(hb, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        if !hp.is_null() {
            SetWindowPos(hp, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        return;
    }
    for hw in to_hide {
        if IsWindowVisible(hw) != 0 {
            ShowWindow(hw, SW_HIDE);
        }
    }
    for idx in wake_status_mons {
        start_status_anim_safe(idx, 1.0);
    }
    for idx in to_repaint {
        present_bar_safe(idx, SurfaceKind::TopCenter);
    }
    let coucou_hw = coucou_window_hwnd();
    for (idx, hw, behind_coucou) in to_show {
        if IsWindowVisible(hw) == 0 {
            present_bar_safe(idx, SurfaceKind::TopCenter);
        }
        let after = if behind_coucou && !coucou_hw.is_null() {
            coucou_hw
        } else {
            HWND_TOPMOST
        };
        SetWindowPos(
            hw,
            after,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
    if !sched_status_hide.is_null() {
        SetTimer(sched_status_hide, TIMER_STATUS_HIDE, 750, None);
    }
}

/// True when `pt` is within `pad` pixels of `r`.
fn pt_near_rect(pt: POINT, r: RECT, pad: i32) -> bool {
    pt.x >= r.left - pad && pt.x <= r.right + pad && pt.y >= r.top - pad && pt.y <= r.bottom + pad
}

/// Checks real cursor position against a monitor's status bar pills, live Coucou region, and
/// edge strip so stale hover flags never leave the bar stuck while hovering Mochi never kills it!
fn cursor_on_status_strip(pt: POINT, m: &MonWindows, cfg: &Config, coucou_live: CoucouLiveState) -> bool {
    let pad = sc(10, m.spec.scale);
    for ro in [m.lay.top_left, m.lay.top_center, m.lay.top_right] {
        if let Some(r) = ro
            && pt_near_rect(pt, r, pad)
        {
            return true;
        }
    }
    if coucou_live.mode != 0 && coucou_live.hmon == m.spec.hmon {
        let h_band = sc(if coucou_live.mode == 2 { 380 } else { 56 }, m.spec.scale);
        let in_x = pt.x >= coucou_live.left - sc(20, m.spec.scale) && pt.x <= coucou_live.right + sc(20, m.spec.scale);
        let in_y = if cfg.dock_edge == DockEdge::Bottom {
            pt.y >= m.spec.rect.top && pt.y <= m.spec.rect.top + h_band
        } else {
            pt.y >= m.spec.rect.bottom - h_band && pt.y <= m.spec.rect.bottom
        };
        if in_x && in_y {
            return true;
        }
    }
    let edge_band = sc(6, m.spec.scale);
    let in_x = pt.x >= m.spec.rect.left && pt.x < m.spec.rect.right;
    if !in_x {
        return false;
    }
    if cfg.dock_edge == DockEdge::Bottom {
        pt.y >= m.spec.rect.top && pt.y <= m.spec.rect.top + edge_band
    } else {
        pt.y >= m.spec.rect.bottom - edge_band && pt.y <= m.spec.rect.bottom
    }
}

fn cursor_on_dock_strip(pt: POINT, m: &MonWindows, cfg: &Config) -> bool {
    let pad = sc(10, m.spec.scale);
    if m.lay.dock.is_some_and(|r| pt_near_rect(pt, r, pad)) {
        return true;
    }
    if cfg.layout != LayoutMode::Win12Split {
        for ro in [m.lay.top_left, m.lay.top_right] {
            if let Some(r) = ro
                && pt_near_rect(pt, r, pad)
            {
                return true;
            }
        }
        let edge_band = sc(6, m.spec.scale);
        let in_x = pt.x >= m.spec.rect.left && pt.x < m.spec.rect.right;
        if in_x {
            if cfg.layout == LayoutMode::UnifiedTop {
                return pt.y >= m.spec.rect.top && pt.y <= m.spec.rect.top + edge_band;
            } else {
                return pt.y >= m.spec.rect.bottom - edge_band && pt.y <= m.spec.rect.bottom;
            }
        }
    }
    false
}

/// Synchronizes container-docked mode with the real Coucou process without ever hiding `hwnd_tc`.
unsafe fn sync_coucou_center_handoff_safe() {
    let payload = with_app(|st| {
        if st.smoke_test {
            return None;
        }
        let running = st.coucou.snapshot().running;
        if !running {
            return None;
        }
        if st.coucou_container_active() {
            if st.coucou_launching {
                st.coucou_launching = false;
                Some(coucou_dock_cmd_payload(&st.cfg, "dock_expand"))
            } else {
                Some(coucou_dock_cmd_payload(&st.cfg, "dock_sync"))
            }
        } else {
            Some(r#"{"taskbar_cmd":"dock_detach"}"#.to_string())
        }
    })
    .flatten();

    if let Some(p) = payload {
        send_coucou_json_async(p);
    }
}

unsafe fn rebuild_monitors_safe() {
    struct BuildSnap {
        hi: HINSTANCE,
        cfg: Config,
        sys: SysPrefs,
        pal: Palette,
        all_items: Vec<AppItem>,
        telem_snap: telemetry::TelemetrySnapshot,
        workspace: String,
        split_clock_str: String,
        coucou_label: String,
        tray_bar_count: usize,
        has_tray_chevron: bool,
        has_lang: bool,
        mm_on: bool,
        mm_mode: u32,
        smoke_test: bool,
        was_reserved: bool,
        old_monitors: Vec<MonWindows>,
    }

    let Some(snap) = with_app(|st| {
        st.hot = None;
        st.pressed = None;
        st.tip_target = None;
        st.tracking_hwnd = null_mut();
        if st.cfg.layout != LayoutMode::Win12Split {
            st.coucou_live = CoucouLiveState::default();
        }
        let (mm_on, mm_mode) = st.effective_mm_mode();
        let tray_limit = st.cfg.tray_mode.limit();
        let tray_bar_count = st.tray.bar_indices(tray_limit).len();
        let has_tray_chevron = !st.tray.all_visible_indices().is_empty();
        let (coucou_label, _) = st.coucou.snapshot().pill_summary();
        BuildSnap {
            hi: st.hi,
            cfg: st.cfg.clone(),
            sys: st.sys,
            pal: st.pal,
            all_items: st.all_items.clone(),
            telem_snap: st.telem.snapshot(),
            workspace: st.workspace.clone(),
            split_clock_str: st.split_clock_str.clone(),
            coucou_label,
            tray_bar_count,
            has_tray_chevron,
            has_lang: !st.lang_code.is_empty(),
            mm_on,
            mm_mode,
            smoke_test: st.smoke_test,
            was_reserved: st.primary_work_reserved.take().is_some(),
            old_monitors: std::mem::take(&mut st.monitors),
        }
    }) else {
        return;
    };

    for mut m in snap.old_monitors {
        m.destroy_all();
    }
    if !snap.smoke_test && snap.was_reserved {
        let full = RECT {
            left: 0,
            top: 0,
            right: GetSystemMetrics(SM_CXSCREEN),
            bottom: GetSystemMetrics(SM_CYSCREEN),
        };
        set_work_area_if_needed(full);
    }

    let specs = enumerate_monitors();
    let cls = wide("gp_taskbar_surface");
    let scls = wide("gp_taskbar_sensor");
    let ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE;
    let mut new_monitors = Vec::new();
    let mut primary_scale = None;
    let mut bars_to_show: Vec<HWND> = Vec::new();
    let mut sensors_to_show: Vec<HWND> = Vec::new();

    for spec in specs {
        if !spec.primary && !snap.mm_on {
            continue;
        }
        if spec.primary {
            primary_scale = Some(spec.scale);
        }
        let items = filter_items_for_monitor(&snap.all_items, &spec, snap.mm_mode);
        let coucou_live = with_app(|st| st.coucou_live).unwrap_or_default();
        let pill_span = if coucou_live.mode == 1 && coucou_live.hmon == spec.hmon && coucou_live.right > coucou_live.left {
            Some((coucou_live.left, coucou_live.right))
        } else {
            None
        };
        let lay = compute_monitor_layout(
            &spec,
            &snap.cfg,
            &snap.sys,
            items.len(),
            if spec.primary { snap.tray_bar_count } else { 0 },
            spec.primary && snap.has_tray_chevron,
            spec.primary && snap.has_lang,
            snap.telem_snap.bat_pct >= 0,
            snap.telem_snap,
            &snap.workspace,
            &snap.split_clock_str,
            &snap.coucou_label,
            pill_span,
        );

        let mk = |r: Option<RECT>, mat: BackdropKind| -> HWND {
            let Some(rc) = r else { return null_mut() };
            let hw = CreateWindowExW(
                ex,
                cls.as_ptr(),
                null(),
                WS_POPUP,
                rc.left,
                rc.top,
                (rc.right - rc.left).max(1),
                (rc.bottom - rc.top).max(1),
                null_mut(),
                null_mut(),
                snap.hi,
                null(),
            );
            if hw.is_null() {
                return null_mut();
            }
            let real_mat = if snap.pal.transparent { mat } else { BackdropKind::Solid };
            apply_backdrop(hw, snap.pal.is_light, real_mat, snap.pal.border_ref);
            hw
        };

        let hwnd_tl = mk(lay.top_left, BackdropKind::Mica);
        let hwnd_tc = mk(lay.top_center, BackdropKind::Mica);
        let hwnd_tr = mk(lay.top_right, BackdropKind::Mica);
        let hwnd_dock = mk(lay.dock, BackdropKind::Acrylic);
        if !hwnd_dock.is_null() {
            DragAcceptFiles(hwnd_dock, 1);
            for m in [WM_DROPFILES, 0x0049, WM_COPYDATA] {
                let _ = ChangeWindowMessageFilterEx(hwnd_dock, m, MSGFLT_ALLOW, null_mut());
            }
        }

        let hwnd_sensor = CreateWindowExW(
            ex,
            scls.as_ptr(),
            null(),
            WS_POPUP,
            spec.rect.left,
            spec.rect.top,
            (spec.rect.right - spec.rect.left).max(1),
            3,
            null_mut(),
            null_mut(),
            snap.hi,
            null(),
        );
        if !hwnd_sensor.is_null() {
            SetLayeredWindowAttributes(hwnd_sensor, 0, 1, LWA_ALPHA);
        }

        let hwnd_status_sensor = CreateWindowExW(
            ex,
            scls.as_ptr(),
            null(),
            WS_POPUP,
            spec.rect.left,
            spec.rect.bottom - 4,
            (spec.rect.right - spec.rect.left).max(1),
            4,
            null_mut(),
            null_mut(),
            snap.hi,
            null(),
        );
        if !hwnd_status_sensor.is_null() {
            SetLayeredWindowAttributes(hwnd_status_sensor, 0, 1, LWA_ALPHA);
        }

        let keep_visible_for_popup = with_app(|st| st.popup_open).unwrap_or(false);
        let status_split_auto = snap.cfg.layout == LayoutMode::Win12Split
            && snap.cfg.status_autohide
            && !snap.smoke_test
            && !keep_visible_for_popup;
        let dock_start_hidden = snap.cfg.autohide && !snap.smoke_test && !keep_visible_for_popup;
        let init_p = if dock_start_hidden { 0.0 } else { 1.0 };
        let init_status_p = if status_split_auto {
            0.0
        } else if snap.cfg.layout == LayoutMode::Win12Split {
            1.0
        } else {
            init_p
        };
        let init_tc_x = lay.top_center.map_or(0.0, |r| r.left as f32);
        let mw = MonWindows {
            spec,
            lay,
            items,
            hwnd_tl,
            hwnd_tc,
            hwnd_tr,
            hwnd_dock,
            hwnd_sensor,
            hwnd_status_sensor,
            surf_tl: Surface::new(),
            surf_tc: Surface::new(),
            surf_tr: Surface::new(),
            surf_dock: Surface::new(),
            text_ren: TextRenderer::new(spec.scale),
            fullscreen: false,
            anim_spring: SpringVal::new(init_p),
            mouse_inside: false,
            status_spring: SpringVal::new(init_status_p),
            status_mouse_inside: false,
            tc_x_spring: SpringVal::new(init_tc_x),
            paints: 0,
        };
        if snap.cfg.autohide && !mw.hwnd_sensor.is_null() {
            position_sensor(&mw, &snap.cfg);
            if dock_start_hidden {
                sensors_to_show.push(mw.hwnd_sensor);
            }
        }
        if !dock_start_hidden && !mw.hwnd_dock.is_null() {
            bars_to_show.push(mw.hwnd_dock);
        }
        if status_split_auto {
            if !mw.hwnd_status_sensor.is_null() {
                position_status_sensor(&mw, &snap.cfg);
                sensors_to_show.push(mw.hwnd_status_sensor);
            }
        } else if !dock_start_hidden || snap.cfg.layout == LayoutMode::Win12Split {
            for hw in mw.status_bars() {
                if !hw.is_null() {
                    bars_to_show.push(hw);
                }
            }
        }
        new_monitors.push(mw);
    }

    with_app(|st| {
        if let Some(sc_val) = primary_scale {
            st.ui_text_ren = TextRenderer::new(sc_val);
            st.tray.ensure_icon_size(sc(16, sc_val));
        }
        st.monitors = new_monitors;
    });

    present_all_safe();

    for hw in sensors_to_show {
        ShowWindow(hw, SW_SHOWNOACTIVATE);
    }
    for hw in bars_to_show {
        SetWindowPos(hw, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
    sync_primary_work_area_safe();
    sync_coucou_center_handoff_safe();
    sync_fullscreen_state_safe();
}

unsafe fn sync_primary_work_area_safe() {
    enum WorkAction {
        None,
        Release,
        Reserve(RECT),
    }
    let act = with_app(|st| {
        if st.smoke_test {
            return WorkAction::None;
        }
        let Some(m0) = st.monitors.first() else { return WorkAction::None };
        if st.cfg.autohide || m0.fullscreen {
            if st.primary_work_reserved.take().is_some() {
                WorkAction::Release
            } else {
                WorkAction::None
            }
        } else {
            let mut want = m0.spec.rect;
            want.top += m0.lay.work_top_reserve;
            want.bottom -= m0.lay.work_bottom_reserve;
            st.primary_work_reserved = Some(want);
            WorkAction::Reserve(want)
        }
    })
    .unwrap_or(WorkAction::None);

    match act {
        WorkAction::None => {}
        WorkAction::Release => {
            let full = RECT {
                left: 0,
                top: 0,
                right: GetSystemMetrics(SM_CXSCREEN),
                bottom: GetSystemMetrics(SM_CYSCREEN),
            };
            set_work_area_if_needed(full);
        }
        WorkAction::Reserve(want) => {
            set_work_area_if_needed(want);
        }
    }
}

#[inline]
fn spring_alpha(val: f32) -> u8 {
    ((val.clamp(0.0, 1.0) * 255.0).round() as i32).clamp(0, 255) as u8
}

/// Windows 11 taskbar auto-hide opacity: stays 100% solid (`255`) while sliding cleanly behind
/// the monitor bezel (matching native Windows 11 taskbar auto-hide with zero DWM alpha artifacts!).
#[inline]
fn bar_slide_alpha(_val: f32) -> u8 {
    255
}

fn anim_dy_for(r: RECT, mon_rect: RECT, scale: f32, spring_val: f32) -> i32 {
    let mid_y = (mon_rect.top + mon_rect.bottom) / 2;
    if r.top < mid_y {
        let travel = ((r.bottom - mon_rect.top) + sc(10, scale)).max(sc(48, scale)) as f32;
        -(((1.0 - spring_val) * travel).round() as i32)
    } else {
        let travel = ((mon_rect.bottom - r.top) + sc(10, scale)).max(sc(48, scale)) as f32;
        ((1.0 - spring_val) * travel).round() as i32
    }
}

fn flyout_slide_dy(r: RECT, mon_rect: RECT, scale: f32, spring_val: f32, px: i32) -> i32 {
    let slide = ((1.0 - spring_val) * sc(px, scale) as f32).round() as i32;
    if r.top < (mon_rect.top + mon_rect.bottom) / 2 { -slide } else { slide }
}

unsafe fn present_bar_safe(idx: usize, kind: SurfaceKind) {
    with_app(|st| {
        if idx >= st.monitors.len() || st.monitors[idx].fullscreen {
            return;
        }
        let host = st.hwnd_host;
        let telem = st.telem.snapshot();
        let hot = st.hot.filter(|&(mi, sk, _)| mi == idx && sk == kind).map_or(HitZone::None, |x| x.2);
        let pressed = st.pressed.filter(|&(mi, sk, _)| mi == idx && sk == kind).map_or(HitZone::None, |x| x.2);
        let pal = st.pal;
        let time_s = st.time_str.clone();
        let date_s = st.date_str.clone();
        let split_s = st.split_clock_str.clone();
        let ws_s = st.workspace.clone();

        let (hwnd, rect_opt) = {
            let m = &st.monitors[idx];
            match kind {
                SurfaceKind::TopLeft => (m.hwnd_tl, m.lay.top_left),
                SurfaceKind::TopCenter => (m.hwnd_tc, m.lay.top_center),
                SurfaceKind::TopRight => (m.hwnd_tr, m.lay.top_right),
                SurfaceKind::Dock => (m.hwnd_dock, m.lay.dock),
            }
        };
        let Some(r) = rect_opt else { return };
        if hwnd.is_null() {
            return;
        }
        let w = (r.right - r.left).max(1);
        let h = (r.bottom - r.top).max(1);
        let p_val = if kind == SurfaceKind::Dock || st.cfg.layout != LayoutMode::Win12Split {
            st.monitors[idx].anim_spring.value
        } else {
            st.monitors[idx].status_spring.value
        };
        let dy = anim_dy_for(r, st.monitors[idx].spec.rect, st.monitors[idx].spec.scale, p_val);
        let alpha = bar_slide_alpha(p_val);
        let tc_x = st.monitors[idx].tc_x_spring.value.round() as i32;

        let drag_vis = st.drag.filter(|d| d.active && d.mon_idx == idx).map(|d| DragVisual {
            idx: d.cur_idx,
            cur_x: d.cur_x,
        });
        let tray_bmps: Vec<Option<Arc<IconBitmap>>> = if kind == SurfaceKind::TopRight && st.monitors[idx].spec.primary {
            st.tray
                .bar_indices(st.cfg.tray_mode.limit())
                .into_iter()
                .map(|ti| st.tray.icons.get(ti).and_then(|x| x.bitmap.clone()))
                .collect()
        } else {
            Vec::new()
        };
        let tray_fly_open = st.tray_fly_open;
        let tray_drop_slot = st
            .tray_drag
            .as_ref()
            .filter(|d| d.active && matches!(d.origin, TrayDragOrigin::FromFlyout { .. }))
            .and_then(|d| d.over_bar_slot);
        let tray_drag_from_slot = st
            .tray_drag
            .as_ref()
            .filter(|d| d.active && d.mon_idx == idx)
            .and_then(|d| match d.origin {
                TrayDragOrigin::FromBar { slot } => Some(slot),
                TrayDragOrigin::FromFlyout { .. } => None,
            });
        let tray_hide_hover = st
            .tray_drag
            .as_ref()
            .is_some_and(|d| d.active && matches!(d.origin, TrayDragOrigin::FromBar { .. }) && d.over_hide);
        let coucou_snap = st.coucou.snapshot();
        let coucou_hub_open = st.coucou_hub_open;
        let has_unread_notice = coucou_snap.latest_notice_ms > st.coucou_seen_notice_ms;
        let anim_tick = st.coucou_anim_tick;
        let lang_code = st.lang_code.clone();
        let bar_at_top = r.top < (st.monitors[idx].spec.rect.top + st.monitors[idx].spec.rect.bottom) / 2;
        let icons = &st.icons;
        let m = &mut st.monitors[idx];
        let presented = match kind {
            SurfaceKind::TopLeft => {
                if m.surf_tl.ensure_dims(w, h) {
                    paint_top_left(&mut m.surf_tl, &m.text_ren, &m.lay, &pal, telem, &ws_s, hot, pressed, m.spec.scale);
                    present_layered_alpha(hwnd, r.left, r.top + dy, &m.surf_tl, alpha)
                } else {
                    false
                }
            }
            SurfaceKind::TopCenter => {
                if m.surf_tc.ensure_dims(w, h) {
                    paint_top_center(
                        &mut m.surf_tc,
                        &m.text_ren,
                        &m.lay,
                        &pal,
                        &split_s,
                        &coucou_snap,
                        coucou_hub_open,
                        has_unread_notice,
                        anim_tick,
                        hot,
                        pressed,
                        m.spec.scale,
                    );
                    present_layered_alpha(hwnd, tc_x, r.top + dy, &m.surf_tc, alpha)
                } else {
                    false
                }
            }
            SurfaceKind::TopRight => {
                if m.surf_tr.ensure_dims(w, h) {
                    paint_top_right(
                        &mut m.surf_tr,
                        &m.text_ren,
                        &m.lay,
                        &pal,
                        telem,
                        &tray_bmps,
                        tray_fly_open,
                        tray_drop_slot,
                        tray_drag_from_slot,
                        tray_hide_hover,
                        &coucou_snap,
                        coucou_hub_open,
                        has_unread_notice,
                        anim_tick,
                        bar_at_top,
                        &lang_code,
                        &time_s,
                        &date_s,
                        hot,
                        pressed,
                        m.spec.scale,
                    );
                    present_layered_alpha(hwnd, r.left, r.top + dy, &m.surf_tr, alpha)
                } else {
                    false
                }
            }
            SurfaceKind::Dock => {
                if m.surf_dock.ensure_dims(w, h) {
                    paint_dock(
                        &mut m.surf_dock,
                        &m.text_ren,
                        &m.lay,
                        &pal,
                        &m.items,
                        hot,
                        pressed,
                        drag_vis,
                        icons,
                        host,
                        WM_APP_ICON_READY,
                        m.spec.scale,
                    );
                    present_layered_alpha(hwnd, r.left, r.top + dy, &m.surf_dock, alpha)
                } else {
                    false
                }
            }
        };
        m.paints += 1;
        st.total_paints += 1;
        if presented {
            st.ulw_ok += 1;
        }
    });
}

unsafe fn present_bar_hwnd_safe(hwnd: HWND) {
    if hwnd.is_null() {
        return;
    }
    if let Some((idx, kind)) = with_app(|st| find_owner(st, hwnd)).flatten() {
        present_bar_safe(idx, kind);
    }
}

unsafe fn present_all_safe() {
    let count = with_app(|st| st.monitors.len()).unwrap_or(0);
    for idx in 0..count {
        for kind in [SurfaceKind::TopLeft, SurfaceKind::TopCenter, SurfaceKind::TopRight, SurfaceKind::Dock] {
            present_bar_safe(idx, kind);
        }
    }
}

unsafe fn present_popup_safe() {
    with_app(|st| {
        if st.hwnd_popup.is_null() {
            return;
        }
        let r = st.popup_rect;
        let w = (r.right - r.left).max(1);
        let h = (r.bottom - r.top).max(1);
        let mrect = st
            .monitors
            .get(st.popup_mon)
            .or_else(|| st.monitors.first())
            .map_or(RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, |m| m.spec.rect);
        let scale = st
            .monitors
            .get(st.popup_mon)
            .or_else(|| st.monitors.first())
            .map_or(1.0, |m| m.spec.scale);
        let p = st.popup_spring.value;
        let dy = flyout_slide_dy(r, mrect, scale, p, 14);
        let alpha = spring_alpha(p);
        if st.surf_popup.ensure_dims(w, h) {
            paint_popup(&mut st.surf_popup, &st.ui_text_ren, &st.cfg, &st.pal, st.popup_hover, scale);
            let _ = present_layered_alpha(st.hwnd_popup, r.left, r.top + dy, &st.surf_popup, alpha);
        }
    });
}

unsafe fn present_tip_safe() {
    with_app(|st| {
        if st.hwnd_tip.is_null() {
            return;
        }
        let r = st.tip_rect;
        let w = (r.right - r.left).max(1);
        let h = (r.bottom - r.top).max(1);
        let scale = st.monitors.first().map_or(1.0, |m| m.spec.scale);
        if st.surf_tip.ensure_dims(w, h) {
            paint_tooltip(&mut st.surf_tip, &st.ui_text_ren, &st.pal, &st.tip_text, scale);
            let _ = present_layered(st.hwnd_tip, r.left, r.top, &st.surf_tip);
        }
    });
}

unsafe fn refresh_windows_all_safe() {
    let mut to_present: Vec<(usize, SurfaceKind)> = Vec::new();
    with_app(|st| {
        if st.drag.is_some_and(|d| d.active) {
            return;
        }
        st.all_items = st.tracker.build_items(&st.pins);
        apply_custom_order(&mut st.all_items, &st.custom_order);
        let ws = active_window_summary(std::process::id());
        let ws_changed = ws != st.workspace;
        if ws_changed {
            st.workspace = ws;
        }
        let (_, new_lang) = input_language_status(st.tracker.last_app_window());
        let lang_changed = new_lang != st.lang_code;
        if lang_changed {
            st.lang_code = new_lang;
        }
        let (_, mm_mode) = st.effective_mm_mode();
        let telem = st.telem.snapshot();
        let (coucou_label, _) = st.coucou.snapshot().pill_summary();
        let tray_bar_count = st.tray.bar_indices(st.cfg.tray_mode.limit()).len();
        let has_tray_chevron = !st.tray.all_visible_indices().is_empty();
        for idx in 0..st.monitors.len() {
            let new_items = filter_items_for_monitor(&st.all_items, &st.monitors[idx].spec, mm_mode);
            let count_changed = new_items.len() != st.monitors[idx].items.len();
            st.monitors[idx].items = new_items;
            let primary = st.monitors[idx].spec.primary;
            if count_changed || (primary && (ws_changed || lang_changed)) {
                let pill_span = st.coucou_span_for(st.monitors[idx].spec.hmon);
                st.monitors[idx].lay = compute_monitor_layout(
                    &st.monitors[idx].spec,
                    &st.cfg,
                    &st.sys,
                    st.monitors[idx].items.len(),
                    if primary { tray_bar_count } else { 0 },
                    primary && has_tray_chevron,
                    primary && !st.lang_code.is_empty(),
                    telem.bat_pct >= 0,
                    telem,
                    &st.workspace,
                    &st.split_clock_str,
                    &coucou_label,
                    pill_span,
                );
                if let Some(tc) = st.monitors[idx].lay.top_center {
                    st.monitors[idx].tc_x_spring.spring_to(tc.left as f32, MOTION_FOLLOW);
                    st.pacer.wake();
                }
                if primary && (ws_changed || lang_changed) {
                    to_present.push((idx, SurfaceKind::TopCenter));
                    to_present.push((idx, SurfaceKind::TopRight));
                }
            }
            if !st.monitors[idx].hwnd_dock.is_null() {
                to_present.push((idx, SurfaceKind::Dock));
            }
            if primary && ws_changed && !st.monitors[idx].hwnd_tl.is_null() {
                to_present.push((idx, SurfaceKind::TopLeft));
            }
        }
    });
    for (idx, kind) in to_present {
        present_bar_safe(idx, kind);
    }
}

unsafe fn refresh_tray_layout_safe() {
    let mut to_present: Vec<(usize, SurfaceKind)> = Vec::new();
    let mut refresh_fly = false;
    with_app(|st| {
        let telem = st.telem.snapshot();
        let (coucou_label, _) = st.coucou.snapshot().pill_summary();
        let tray_bar_count = st.tray.bar_indices(st.cfg.tray_mode.limit()).len();
        let has_tray_chevron = !st.tray.all_visible_indices().is_empty();
        for idx in 0..st.monitors.len() {
            if !st.monitors[idx].spec.primary {
                continue;
            }
            let prev_slots = st.monitors[idx].lay.tray_rects.len();
            let prev_chev = st.monitors[idx].lay.tray_chevron_rect.is_some();
            let geom_changed = prev_slots != tray_bar_count || prev_chev != (has_tray_chevron || tray_bar_count > 0);
            if geom_changed {
                let pill_span = st.coucou_span_for(st.monitors[idx].spec.hmon);
                st.monitors[idx].lay = compute_monitor_layout(
                    &st.monitors[idx].spec,
                    &st.cfg,
                    &st.sys,
                    st.monitors[idx].items.len(),
                    tray_bar_count,
                    has_tray_chevron,
                    !st.lang_code.is_empty(),
                    telem.bat_pct >= 0,
                    telem,
                    &st.workspace,
                    &st.split_clock_str,
                    &coucou_label,
                    pill_span,
                );
                if let Some(tc) = st.monitors[idx].lay.top_center {
                    st.monitors[idx].tc_x_spring.spring_to(tc.left as f32, MOTION_FOLLOW);
                    st.pacer.wake();
                }
            }
            to_present.push((idx, SurfaceKind::TopRight));
            if geom_changed {
                if !st.monitors[idx].hwnd_tc.is_null() {
                    to_present.push((idx, SurfaceKind::TopCenter));
                }
                if st.cfg.layout != LayoutMode::Win12Split && !st.monitors[idx].hwnd_dock.is_null() {
                    to_present.push((idx, SurfaceKind::Dock));
                }
            }
        }
        // Always rebuild tray_fly_layout from live tray.icons so animated icons inside the hidden
        // overflow flyout update their bitmaps in real time!
        if st.tray_fly_open {
            let mon_idx = st.tray_fly_mon;
            let (cx, _, base) = st
                .monitors
                .get(mon_idx)
                .map(|m| zone_anchor(SurfaceKind::TopRight, &m.lay, HitZone::TrayChevron))
                .unwrap_or((0, 0, RECT { left: 0, top: 0, right: 0, bottom: 0 }));
            rebuild_tray_fly_layout(st, mon_idx, cx, base);
            if let Some(ref lay) = st.tray_fly_layout
                && let Some(h) = st.tray_fly_hover
                && h >= lay.cells.len()
            {
                st.tray_fly_hover = None;
            }
            refresh_fly = true;
        }
    });
    for (idx, kind) in to_present {
        present_bar_safe(idx, kind);
    }
    if refresh_fly {
        present_tray_fly_safe();
    }
}

unsafe fn handle_system_prefs_changed() {
    let next = read_system_prefs();
    enum PrefAction {
        None,
        Rebuild,
        RepaintAll,
    }
    let act = with_app(|st| {
        if next == st.sys {
            return PrefAction::None;
        }
        let mm_changed = (next.mm_enabled, next.mm_mode) != (st.sys.mm_enabled, st.sys.mm_mode);
        let align_changed = next.align_center != st.sys.align_center;
        st.sys = next;
        st.pal = fluent_palette(&st.cfg, &st.sys);
        if mm_changed {
            return PrefAction::Rebuild;
        }
        if align_changed {
            let telem = st.telem.snapshot();
            let (coucou_label, _) = st.coucou.snapshot().pill_summary();
            let tray_bar_count = st.tray.bar_indices(st.cfg.tray_mode.limit()).len();
            let has_tray_chevron = !st.tray.all_visible_indices().is_empty();
            let coucou_live = st.coucou_live;
            for m in &mut st.monitors {
                let pill_span = if coucou_live.mode == 1 && coucou_live.hmon == m.spec.hmon && coucou_live.right > coucou_live.left {
                    Some((coucou_live.left, coucou_live.right))
                } else {
                    None
                };
                m.lay = compute_monitor_layout(
                    &m.spec,
                    &st.cfg,
                    &st.sys,
                    m.items.len(),
                    if m.spec.primary { tray_bar_count } else { 0 },
                    m.spec.primary && has_tray_chevron,
                    m.spec.primary && !st.lang_code.is_empty(),
                    telem.bat_pct >= 0,
                    telem,
                    &st.workspace,
                    &st.split_clock_str,
                    &coucou_label,
                    pill_span,
                );
                if let Some(tc) = m.lay.top_center {
                    m.tc_x_spring.jump(tc.left as f32);
                }
            }
        }
        PrefAction::RepaintAll
    })
    .unwrap_or(PrefAction::None);

    match act {
        PrefAction::None => {}
        PrefAction::Rebuild => rebuild_monitors_safe(),
        PrefAction::RepaintAll => reapply_backdrops_safe(),
    }
}

unsafe fn reapply_backdrops_safe() {
    struct Target {
        hwnd: HWND,
        mat: BackdropKind,
    }
    let Some((is_light, border_ref, pop_open, fly_open, hub_open, frameless, targets)) = with_app(|st| {
        st.pal = fluent_palette(&st.cfg, &st.sys);
        let pop_mat = if st.pal.transparent { BackdropKind::Acrylic } else { BackdropKind::Solid };
        let mica = if st.pal.transparent { BackdropKind::Mica } else { BackdropKind::Solid };
        let acryl = if st.pal.transparent { BackdropKind::Acrylic } else { BackdropKind::Solid };
        let frameless = [st.hwnd_tray_fly, st.hwnd_coucou_hub, st.hwnd_search_bar, st.hwnd_search_panel, st.hwnd_popup];
        let mut t = vec![
            Target { hwnd: st.hwnd_tip, mat: pop_mat },
            Target { hwnd: st.hwnd_preview, mat: BackdropKind::Solid },
        ];
        for m in &st.monitors {
            for hw in [m.hwnd_tl, m.hwnd_tc, m.hwnd_tr] {
                if !hw.is_null() {
                    t.push(Target { hwnd: hw, mat: mica });
                }
            }
            if !m.hwnd_dock.is_null() {
                t.push(Target { hwnd: m.hwnd_dock, mat: acryl });
            }
        }
        (st.pal.is_light, st.pal.border_ref, st.popup_open, st.tray_fly_open, st.coucou_hub_open, frameless, t)
    }) else {
        return;
    };

    for hw in frameless {
        if !hw.is_null() {
            apply_frameless_layered(hw, is_light);
        }
    }
    for t in targets {
        if !t.hwnd.is_null() {
            apply_backdrop(t.hwnd, is_light, t.mat, border_ref);
        }
    }
    present_all_safe();
    if pop_open {
        present_popup_safe();
    }
    if fly_open {
        present_tray_fly_safe();
    }
    if hub_open {
        present_coucou_hub_safe();
    }
}

unsafe fn toggle_popup_safe(mon_idx: usize, anchor_rect: RECT) {
    close_coucou_hub_safe();
    enum PopAct {
        None,
        Close,
        Open(HWND),
    }
    let act = with_app(|st| {
        if st.popup_open {
            st.popup_open = false;
            st.popup_hover = None;
            st.popup_spring.spring_to(0.0, MOTION_COLLAPSE);
            st.pacer.wake();
            return PopAct::Close;
        }
        let scale = st.monitors.get(mon_idx).map_or(1.0, |m| m.spec.scale);
        let mrect = st.monitors.get(mon_idx).map_or(RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, |m| m.spec.rect);
        let s = |v: i32| sc(v, scale);
        let play = compute_popup_layout(&st.cfg, scale);
        let pw = play.w;
        let ph = play.h;
        let px = (anchor_rect.right - pw).clamp(mrect.left + s(8), (mrect.right - pw - s(8)).max(mrect.left + s(8)));
        let py = if anchor_rect.top < (mrect.top + mrect.bottom) / 2 {
            anchor_rect.bottom + s(8)
        } else {
            anchor_rect.top - ph - s(8)
        }
        .clamp(mrect.top + s(8), (mrect.bottom - ph - s(8)).max(mrect.top + s(8)));
        st.popup_rect = RECT { left: px, top: py, right: px + pw, bottom: py + ph };
        st.popup_mon = mon_idx;
        st.popup_open = true;
        st.popup_hover = None;
        if !st.popup_spring.animating() {
            st.popup_spring.jump(0.0);
        }
        st.popup_spring.spring_to(1.0, MOTION_EXPAND);
        st.pacer.wake();
        PopAct::Open(st.hwnd_popup)
    })
    .unwrap_or(PopAct::None);

    match act {
        PopAct::None | PopAct::Close => {}
        PopAct::Open(hw) => {
            if !hw.is_null() {
                present_popup_safe();
                SetWindowPos(hw, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
                SetForegroundWindow(hw);
            }
        }
    }
}

unsafe fn close_popup_safe() {
    let (host, schedule_status_hide, schedule_dock_hide) = with_app(|st| {
        if st.popup_open {
            st.popup_open = false;
            st.popup_hover = None;
            st.popup_spring.spring_to(0.0, MOTION_COLLAPSE);
            st.pacer.wake();
            let sched_status = st.cfg.layout == LayoutMode::Win12Split
                && st.cfg.status_autohide
                && st.coucou_live.mode == 0
                && !st.menu_open
                && !st.tray_fly_open
                && !st.coucou_hub_open
                && st.monitors.iter().all(|m| !m.status_mouse_inside);
            let sched_dock = st.cfg.autohide
                && !st.menu_open
                && !st.tray_fly_open
                && !st.coucou_hub_open
                && !st.preview_open
                && st.monitors.iter().all(|m| !m.mouse_inside);
            (st.host_hwnd(), sched_status, sched_dock)
        } else {
            (null_mut(), false, false)
        }
    })
    .unwrap_or((null_mut(), false, false));
    if schedule_status_hide && !host.is_null() {
        SetTimer(host, TIMER_STATUS_HIDE, 700, None);
    }
    if schedule_dock_hide && !host.is_null() {
        SetTimer(host, TIMER_AUTOHIDE_HOLD, 650, None);
    }
}

unsafe fn present_tray_fly_safe() {
    with_app(|st| {
        if st.hwnd_tray_fly.is_null() || (!st.tray_fly_open && !st.tray_fly_spring.animating()) {
            return;
        }
        let Some(ref lay) = st.tray_fly_layout else { return };
        let r = lay.win_rect;
        let w = (r.right - r.left).max(1);
        let h = (r.bottom - r.top).max(1);
        let mrect = st
            .monitors
            .get(st.tray_fly_mon)
            .map_or(RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, |m| m.spec.rect);
        let scale = st.monitors.get(st.tray_fly_mon).map_or(1.0, |m| m.spec.scale);
        let p = st.tray_fly_spring.value;
        let dy = flyout_slide_dy(r, mrect, scale, p, 14);
        let alpha = spring_alpha(p);
        let dragging_cell = st
            .tray_drag
            .as_ref()
            .filter(|d| d.active)
            .and_then(|d| match d.origin {
                TrayDragOrigin::FromFlyout { cell_idx } => Some(cell_idx),
                TrayDragOrigin::FromBar { .. } => None,
            });
        let drop_to_hide_active = st
            .tray_drag
            .as_ref()
            .is_some_and(|d| d.active && matches!(d.origin, TrayDragOrigin::FromBar { .. }) && d.over_hide);
        if st.surf_tray_fly.ensure_dims(w, h) {
            // Paint crisp full-alpha content ONCE; VSync slide+fade happens purely on GPU via `SourceConstantAlpha`.
            paint_tray_flyout(
                &mut st.surf_tray_fly,
                &st.ui_text_ren,
                lay,
                &st.pal,
                st.tray_fly_hover,
                dragging_cell,
                drop_to_hide_active,
                1.0,
                scale,
            );
            let _ = present_layered_alpha(st.hwnd_tray_fly, r.left, r.top + dy, &st.surf_tray_fly, alpha);
        }
    });
}

unsafe fn update_drag_ghost_safe(pt: POINT, bmp: Option<&IconBitmap>, highlight: bool, scale: f32) {
    let hw = with_app(|st| {
        if st.hwnd_drag_ghost.is_null() {
            return null_mut();
        }
        let sz = sc(32, scale).max(24);
        if st.surf_drag_ghost.ensure_dims(sz, sz) {
            paint_drag_ghost(&mut st.surf_drag_ghost, &st.pal, bmp, highlight, scale);
            let _ = present_layered(st.hwnd_drag_ghost, pt.x - sz / 2, pt.y - sz / 2, &st.surf_drag_ghost);
        }
        st.hwnd_drag_ghost
    })
    .unwrap_or(null_mut());
    if !hw.is_null() && IsWindowVisible(hw) == 0 {
        SetWindowPos(hw, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
}

unsafe fn hide_drag_ghost_safe() {
    let hw = with_app(|st| st.hwnd_drag_ghost).unwrap_or(null_mut());
    if !hw.is_null() && IsWindowVisible(hw) != 0 {
        ShowWindow(hw, SW_HIDE);
    }
}

unsafe fn rebuild_tray_fly_layout(st: &mut AppState, mon_idx: usize, anchor_cx: i32, bar_rect: RECT) {
    let scale = st.monitors.get(mon_idx).map_or(1.0, |m| m.spec.scale);
    let mrect = st
        .monitors
        .get(mon_idx)
        .map_or(RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, |m| m.spec.rect);
    let limit = st.cfg.tray_mode.limit();
    let over = st.tray.overflow_indices(limit);
    let entries = over
        .into_iter()
        .filter_map(|ti| {
            let x = st.tray.icons.get(ti)?;
            Some((ti, x.tip.clone(), x.key.clone(), false, x.bitmap.clone()))
        })
        .collect();
    st.tray_fly_layout = Some(compute_tray_flyout_layout(mrect, anchor_cx, bar_rect, entries, scale));
}

/// Opens the tray overflow flyout without stealing focus/capture (used as drop target while dragging from TopRight).
unsafe fn open_tray_fly_for_drop_safe(mon_idx: usize) {
    let hw = with_app(|st| {
        if st.tray_fly_open {
            return null_mut();
        }
        let (cx, _, base) = st
            .monitors
            .get(mon_idx)
            .map(|m| zone_anchor(SurfaceKind::TopRight, &m.lay, HitZone::TrayChevron))
            .unwrap_or((0, 0, RECT { left: 0, top: 0, right: 0, bottom: 0 }));
        st.tray_fly_mon = mon_idx;
        st.tray_fly_hover = None;
        rebuild_tray_fly_layout(st, mon_idx, cx, base);
        st.tray_fly_open = true;
        if !st.tray_fly_spring.animating() {
            st.tray_fly_spring.jump(0.0);
        }
        st.tray_fly_spring.spring_to(1.0, MOTION_EXPAND);
        st.pacer.wake();
        st.hwnd_tray_fly
    })
    .unwrap_or(null_mut());
    if !hw.is_null() {
        present_tray_fly_safe();
        SetWindowPos(hw, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
}

unsafe fn toggle_tray_fly_safe(mon_idx: usize, anchor_cx: i32, bar_rect: RECT) {
    close_popup_safe();
    close_coucou_hub_safe();
    hide_tooltip_safe();
    enum FlyAct {
        None,
        Close(usize),
        Open(HWND, usize),
    }
    let act = with_app(|st| {
        if st.tray_fly_open {
            st.tray_fly_open = false;
            st.tray_fly_hover = None;
            st.tray_fly_spring.spring_to(0.0, MOTION_COLLAPSE);
            st.pacer.wake();
            return FlyAct::Close(st.tray_fly_mon);
        }
        st.tray_fly_mon = mon_idx;
        st.tray_fly_hover = None;
        rebuild_tray_fly_layout(st, mon_idx, anchor_cx, bar_rect);
        st.tray_fly_open = true;
        if !st.tray_fly_spring.animating() {
            st.tray_fly_spring.jump(0.0);
        }
        st.tray_fly_spring.spring_to(1.0, MOTION_EXPAND);
        st.pacer.wake();
        FlyAct::Open(st.hwnd_tray_fly, mon_idx)
    })
    .unwrap_or(FlyAct::None);

    match act {
        FlyAct::None => {}
        FlyAct::Close(mi) => {
            present_bar_safe(mi, SurfaceKind::TopRight);
        }
        FlyAct::Open(hw, mi) => {
            present_bar_safe(mi, SurfaceKind::TopRight);
            if !hw.is_null() {
                present_tray_fly_safe();
                SetWindowPos(hw, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
                SetForegroundWindow(hw);
            }
        }
    }
}

unsafe fn close_tray_fly_safe() {
    let (was_open, host, mi, schedule_status_hide, schedule_dock_hide) = with_app(|st| {
        if st.tray_fly_open {
            st.tray_fly_open = false;
            st.tray_fly_hover = None;
            st.tray_fly_spring.spring_to(0.0, MOTION_COLLAPSE);
            st.pacer.wake();
            let sched_status = st.cfg.layout == LayoutMode::Win12Split
                && st.cfg.status_autohide
                && !st.menu_open
                && !st.popup_open
                && !st.coucou_hub_open
                && st.tray_drag.is_none()
                && st.monitors.iter().all(|m| !m.status_mouse_inside);
            let sched_dock = st.cfg.autohide
                && !st.menu_open
                && !st.popup_open
                && !st.preview_open
                && !st.coucou_hub_open
                && st.tray_drag.is_none()
                && st.monitors.iter().all(|m| !m.mouse_inside);
            (true, st.host_hwnd(), st.tray_fly_mon, sched_status, sched_dock)
        } else {
            (false, null_mut(), 0, false, false)
        }
    })
    .unwrap_or((false, null_mut(), 0, false, false));

    if was_open {
        present_bar_safe(mi, SurfaceKind::TopRight);
    }
    if schedule_status_hide && !host.is_null() {
        SetTimer(host, TIMER_STATUS_HIDE, 700, None);
    }
    if schedule_dock_hide && !host.is_null() {
        SetTimer(host, TIMER_AUTOHIDE_HOLD, 650, None);
    }
}

unsafe fn present_coucou_hub_safe() {
    with_app(|st| {
        if st.hwnd_coucou_hub.is_null() || (!st.coucou_hub_open && !st.coucou_hub_spring.animating()) {
            return;
        }
        let Some(ref lay) = st.coucou_hub_layout else { return };
        let r = lay.win_rect;
        let w = (r.right - r.left).max(1);
        let h = (r.bottom - r.top).max(1);
        let mrect = st
            .monitors
            .get(st.coucou_hub_mon)
            .or_else(|| st.monitors.first())
            .map_or(RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, |m| m.spec.rect);
        let scale = st
            .monitors
            .get(st.coucou_hub_mon)
            .or_else(|| st.monitors.first())
            .map_or(1.0, |m| m.spec.scale);
        let p = st.coucou_hub_spring.value;
        let dy = flyout_slide_dy(r, mrect, scale, p, 14);
        let alpha = spring_alpha(p);
        let snap = st.coucou.snapshot();
        if st.surf_coucou_hub.ensure_dims(w, h) {
            paint_coucou_hub(
                &mut st.surf_coucou_hub,
                &st.ui_text_ren,
                lay,
                &snap,
                &st.pal,
                st.coucou_hub_hover,
                st.coucou_anim_tick,
                scale,
            );
            let _ = present_layered_alpha(st.hwnd_coucou_hub, r.left, r.top + dy, &st.surf_coucou_hub, alpha);
        }
    });
}

unsafe fn rebuild_coucou_hub_layout(st: &mut AppState, mon_idx: usize, anchor_cx: i32, bar_rect: RECT) {
    let scale = st.monitors.get(mon_idx).map_or(1.0, |m| m.spec.scale);
    let mrect = st
        .monitors
        .get(mon_idx)
        .map_or(RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, |m| m.spec.rect);
    let snap = st.coucou.snapshot();
    st.coucou_hub_layout = Some(compute_coucou_hub_layout(mrect, anchor_cx, bar_rect, &snap, scale));
}

unsafe fn toggle_coucou_hub_safe(mon_idx: usize, anchor_cx: i32, bar_rect: RECT) {
    close_popup_safe();
    close_tray_fly_safe();
    hide_tooltip_safe();
    enum HubAct {
        None,
        Close(usize),
        Open(HWND, usize),
    }
    let act = with_app(|st| {
        if st.coucou_hub_open {
            st.coucou_hub_open = false;
            st.coucou_hub_hover = None;
            st.coucou_hub_spring.spring_to(0.0, MOTION_COLLAPSE);
            st.pacer.wake();
            return HubAct::Close(st.coucou_hub_mon);
        }
        let snap = st.coucou.snapshot();
        if snap.latest_notice_ms > st.coucou_seen_notice_ms {
            st.coucou_seen_notice_ms = snap.latest_notice_ms;
        }
        st.coucou_hub_mon = mon_idx;
        st.coucou_hub_hover = None;
        rebuild_coucou_hub_layout(st, mon_idx, anchor_cx, bar_rect);
        st.coucou_hub_open = true;
        if !st.coucou_hub_spring.animating() {
            st.coucou_hub_spring.jump(0.0);
        }
        st.coucou_hub_spring.spring_to(1.0, MOTION_EXPAND);
        st.pacer.wake();
        HubAct::Open(st.hwnd_coucou_hub, mon_idx)
    })
    .unwrap_or(HubAct::None);

    match act {
        HubAct::None => {}
        HubAct::Close(mi) => {
            present_bar_safe(mi, SurfaceKind::TopCenter);
            present_bar_safe(mi, SurfaceKind::TopRight);
        }
        HubAct::Open(hw, mi) => {
            present_bar_safe(mi, SurfaceKind::TopCenter);
            present_bar_safe(mi, SurfaceKind::TopRight);
            if !hw.is_null() {
                present_coucou_hub_safe();
                SetWindowPos(hw, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
                SetForegroundWindow(hw);
            }
        }
    }
}

unsafe fn close_coucou_hub_safe() {
    let (was_open, host, mi, schedule_status_hide, schedule_dock_hide) = with_app(|st| {
        if st.coucou_hub_open {
            st.coucou_hub_open = false;
            st.coucou_hub_hover = None;
            st.coucou_hub_spring.spring_to(0.0, MOTION_COLLAPSE);
            st.pacer.wake();
            let sched_status = st.cfg.layout == LayoutMode::Win12Split
                && st.cfg.status_autohide
                && !st.menu_open
                && !st.popup_open
                && !st.tray_fly_open
                && st.tray_drag.is_none()
                && st.monitors.iter().all(|m| !m.status_mouse_inside);
            let sched_dock = st.cfg.autohide
                && !st.menu_open
                && !st.popup_open
                && !st.preview_open
                && !st.tray_fly_open
                && st.tray_drag.is_none()
                && st.monitors.iter().all(|m| !m.mouse_inside);
            (true, st.host_hwnd(), st.coucou_hub_mon, sched_status, sched_dock)
        } else {
            (false, null_mut(), 0, false, false)
        }
    })
    .unwrap_or((false, null_mut(), 0, false, false));

    if was_open {
        present_bar_safe(mi, SurfaceKind::TopCenter);
        present_bar_safe(mi, SurfaceKind::TopRight);
    }
    if schedule_status_hide && !host.is_null() {
        SetTimer(host, TIMER_STATUS_HIDE, 700, None);
    }
    if schedule_dock_hide && !host.is_null() {
        SetTimer(host, TIMER_AUTOHIDE_HOLD, 650, None);
    }
}

unsafe extern "system" fn coucou_hub_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &ps);
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            let inv = with_app(|st| {
                let hit = st
                    .coucou_hub_layout
                    .as_ref()
                    .and_then(|l| hit_test_coucou_hub(l, x, y));
                if st.coucou_hub_hover != hit {
                    st.coucou_hub_hover = hit;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
            if inv {
                present_coucou_hub_safe();
            }
            0
        }
        WM_LBUTTONUP => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            let act = with_app(|st| {
                if !st.coucou_hub_open {
                    return None;
                }
                st.coucou_hub_layout
                    .as_ref()
                    .and_then(|l| hit_test_coucou_hub(l, x, y))
            })
            .flatten();
            if let Some(action) = act {
                match action {
                    CoucouHubAction::OpenIsland => {
                        close_coucou_hub_safe();
                        send_coucou_cmd_async("open");
                    }
                    CoucouHubAction::AskMochi => {
                        close_coucou_hub_safe();
                        send_coucou_cmd_async("prompt");
                    }
                    CoucouHubAction::OpenSettings => {
                        close_coucou_hub_safe();
                        send_coucou_cmd_async("settings");
                    }
                    CoucouHubAction::OpenFocus => {
                        close_coucou_hub_safe();
                        send_coucou_cmd_async("focus");
                    }
                    CoucouHubAction::OpenWater => {
                        close_coucou_hub_safe();
                        send_coucou_cmd_async("overview");
                    }
                    CoucouHubAction::OpenTodos => {
                        close_coucou_hub_safe();
                        send_coucou_cmd_async("todo");
                    }
                    CoucouHubAction::TogglePause => {
                        close_coucou_hub_safe();
                        send_coucou_cmd_async("pause");
                    }
                    CoucouHubAction::MediaPrev => send_chord(&[0xB1]),
                    CoucouHubAction::MediaPlayPause => send_chord(&[0xB3]),
                    CoucouHubAction::MediaNext => send_chord(&[0xB0]),
                    CoucouHubAction::LaunchCoucou => {
                        close_coucou_hub_safe();
                        launch_coucou();
                    }
                }
            }
            0
        }
        WM_ACTIVATE if (wp & 0xFFFF) == 0 => {
            close_coucou_hub_safe();
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn present_search_bar_safe() {
    with_app(|st| {
        if st.hwnd_search_bar.is_null() || !st.search_open {
            return;
        }
        let Some(ref lay) = st.search_bar_layout else { return };
        // Fixed full-width transparent canvas: zero HWND resize and zero DIB reallocation per frame!
        let r = lay.full_rect;
        let w = (r.right - r.left).max(1);
        let h = (r.bottom - r.top).max(1);
        let scale = st
            .monitors
            .get(st.search_mon)
            .or_else(|| st.monitors.first())
            .map_or(1.0, |m| m.spec.scale);
        if st.surf_search_bar.ensure_dims(w, h) {
            paint_search_bar(
                &mut st.surf_search_bar,
                &st.ui_text_ren,
                lay,
                &st.pal,
                &st.search_query,
                st.search_scope,
                st.search_caret_on,
                st.search_bar_hover,
                st.search_spring.value,
                scale,
            );
            let _ = present_layered(st.hwnd_search_bar, r.left, r.top, &st.surf_search_bar);
        }
    });
}

unsafe fn present_search_panel_safe() {
    with_app(|st| {
        if st.hwnd_search_panel.is_null() || !st.search_open || st.search_spring.target <= 0.5 {
            return;
        }
        let Some(ref lay) = st.search_panel_layout else { return };
        let r = lay.win_rect;
        let w = (r.right - r.left).max(1);
        let h = (r.bottom - r.top).max(1);
        let mrect = st
            .monitors
            .get(st.search_mon)
            .map_or(RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, |m| m.spec.rect);
        let scale = st
            .monitors
            .get(st.search_mon)
            .or_else(|| st.monitors.first())
            .map_or(1.0, |m| m.spec.scale);
        let p = st.search_panel_spring.value;
        let dy = flyout_slide_dy(r, mrect, scale, p, 14);
        let alpha = spring_alpha(p);
        let host = st.hwnd_host;
        if st.surf_search_panel.ensure_dims(w, h) {
            paint_search_panel(
                &mut st.surf_search_panel,
                &st.ui_text_ren,
                lay,
                &st.pal,
                &st.search_query,
                st.search_scope,
                &st.search_results,
                st.search_selected,
                st.search_panel_hover,
                &st.icons,
                host,
                WM_APP_ICON_READY,
                scale,
            );
            let _ = present_layered_alpha(st.hwnd_search_panel, r.left, r.top + dy, &st.surf_search_panel, alpha);
        }
    });
}

unsafe fn refresh_search_results_in_app(st: &mut AppState) {
    let results = st.search.query(&st.search_query, st.search_scope, &st.all_items, 8);
    if st.search_selected >= results.len() {
        st.search_selected = 0;
    }
    st.search_results = results;
    if let Some(ref bl) = st.search_bar_layout
        && let Some(m) = st.monitors.get(st.search_mon).or_else(|| st.monitors.first())
    {
        st.search_panel_layout = Some(compute_search_panel_layout(
            &m.spec,
            bl.full_rect,
            st.search_results.len(),
        ));
    }
}

unsafe fn toggle_search_bar_safe(mon_idx: usize) {
    let is_opening = with_app(|st| st.search_open && st.search_spring.target > 0.5).unwrap_or(false);
    if is_opening {
        close_search_bar_safe();
    } else {
        open_search_bar_safe(mon_idx);
    }
}

unsafe fn open_search_bar_safe(mon_idx: usize) {
    close_popup_safe();
    close_tray_fly_safe();
    close_coucou_hub_safe();
    close_preview_safe();
    hide_tooltip_safe();

    let prep = with_app(|st| {
        let mi = if mon_idx < st.monitors.len() { mon_idx } else { 0 };
        let m = st.monitors.get(mi)?;
        let bl = compute_search_bar_layout(&m.spec, &m.lay);
        let hwnd_replaced = if st.cfg.layout == LayoutMode::Win12Split { m.hwnd_tc } else { m.hwnd_dock };
        let was_open = st.search_open;
        st.search_mon = mi;
        st.search_query.clear();
        st.search_scope = SearchScope::All;
        st.search_selected = 0;
        st.search_bar_hover = SearchBarHit::None;
        st.search_panel_hover = SearchPanelHit::None;
        st.search_bar_layout = Some(bl);
        refresh_search_results_in_app(st);
        st.search_open = true;
        if !was_open {
            st.search_spring.jump(0.0);
            st.search_panel_spring.jump(0.0);
        }
        st.search_spring.spring_to(1.0, MOTION_EXPAND);
        st.search_panel_spring.spring_to(1.0, MOTION_EXPAND);
        st.pacer.wake();
        st.search_caret_on = true;
        let hide_coucou = st.cfg.layout == LayoutMode::Win12Split && st.coucou.snapshot().running;
        Some((st.hwnd_search_bar, st.hwnd_search_panel, st.host_hwnd(), hwnd_replaced, hide_coucou))
    })
    .flatten();

    let Some((hb, hp, host, hwnd_replaced, hide_coucou)) = prep else { return };
    if hide_coucou {
        send_coucou_json_async(r#"{"taskbar_cmd":"dock_hide"}"#.to_string());
    }
    if !host.is_null() {
        KillTimer(host, TIMER_STATUS_HIDE);
        KillTimer(host, TIMER_AUTOHIDE_HOLD);
    }
    present_search_bar_safe();
    present_search_panel_safe();
    if !hb.is_null() {
        SetWindowPos(hb, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
        // Use our proven AttachThreadInput + AllowSetForegroundWindow helper so the search bar
        // ALWAYS receives immediate keyboard focus even though the taskbar pill is WS_EX_NOACTIVATE!
        bring_to_front(hb);
        SetFocus(hb);
    }
    if !hp.is_null() {
        SetWindowPos(hp, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
    }
    if !hwnd_replaced.is_null() && IsWindowVisible(hwnd_replaced) != 0 {
        ShowWindow(hwnd_replaced, SW_HIDE);
    }
    if !host.is_null() {
        SetTimer(host, TIMER_SEARCH_CARET, 530, None);
    }
}

unsafe fn close_search_bar_safe() {
    let host = with_app(|st| {
        if !st.search_open || st.search_spring.target <= 0.0 {
            return null_mut();
        }
        st.search_spring.spring_to(0.0, MOTION_COLLAPSE);
        st.search_panel_spring.spring_to(0.0, MOTION_COLLAPSE);
        st.pacer.wake();
        st.host_hwnd()
    })
    .unwrap_or(null_mut());

    if !host.is_null() {
        KillTimer(host, TIMER_SEARCH_CARET);
    }
}

unsafe extern "system" fn search_bar_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &ps);
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            let inv = with_app(|st| {
                let hit = st
                    .search_bar_layout
                    .as_ref()
                    .map_or(SearchBarHit::None, |l| hit_test_search_bar(l, x, y));
                if st.search_bar_hover != hit {
                    st.search_bar_hover = hit;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
            if inv {
                present_search_bar_safe();
            }
            0
        }
        WM_LBUTTONUP => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            let hit = with_app(|st| {
                st.search_bar_layout
                    .as_ref()
                    .map_or(SearchBarHit::None, |l| hit_test_search_bar(l, x, y))
            })
            .unwrap_or(SearchBarHit::None);
            match hit {
                SearchBarHit::Clear => {
                    let was_empty = with_app(|st| {
                        if st.search_query.is_empty() {
                            true
                        } else {
                            st.search_query.clear();
                            st.search_selected = 0;
                            refresh_search_results_in_app(st);
                            false
                        }
                    })
                    .unwrap_or(true);
                    if was_empty {
                        close_search_bar_safe();
                    } else {
                        present_search_bar_safe();
                        present_search_panel_safe();
                    }
                }
                SearchBarHit::Scope => {
                    with_app(|st| {
                        st.search_scope = st.search_scope.next();
                        st.search_selected = 0;
                        refresh_search_results_in_app(st);
                    });
                    present_search_bar_safe();
                    present_search_panel_safe();
                }
                SearchBarHit::Input | SearchBarHit::None => {
                    SetFocus(hwnd);
                }
            }
            0
        }
        WM_KEYDOWN => {
            let vk = wp as u16;
            if vk == VK_ESCAPE {
                close_search_bar_safe();
                return 0;
            }
            if vk == VK_DOWN || vk == VK_UP {
                let changed = with_app(|st| {
                    let n = st.search_results.len();
                    if n == 0 {
                        return false;
                    }
                    st.search_selected = if vk == VK_DOWN {
                        (st.search_selected + 1) % n
                    } else {
                        (st.search_selected + n - 1) % n
                    };
                    true
                })
                .unwrap_or(false);
                if changed {
                    present_search_panel_safe();
                }
                return 0;
            }
            if vk == VK_TAB {
                with_app(|st| {
                    st.search_scope = st.search_scope.next();
                    st.search_selected = 0;
                    refresh_search_results_in_app(st);
                });
                present_search_bar_safe();
                present_search_panel_safe();
                return 0;
            }
            if vk == VK_RETURN {
                let as_admin = GetKeyState(VK_CONTROL as i32) < 0;
                let chosen = with_app(|st| st.search_results.get(st.search_selected).cloned()).flatten();
                close_search_bar_safe();
                if let Some(res) = chosen {
                    execute_result(&res, as_admin);
                }
                return 0;
            }
            0
        }
        WM_CHAR => {
            let code = wp as u32;
            let mut changed = false;
            if code == 0x16 {
                // Ctrl+V paste
                if let Some(clip) = paste_from_clipboard() {
                    with_app(|st| {
                        for ch in clip.chars() {
                            if st.search_query.chars().count() >= 128 {
                                break;
                            }
                            st.search_query.push(ch);
                        }
                        st.search_selected = 0;
                        st.search_caret_on = true;
                        refresh_search_results_in_app(st);
                    });
                    changed = true;
                }
            } else if code == 0x08 {
                // Backspace (Ctrl+Backspace clears last word)
                let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
                changed = with_app(|st| {
                    if st.search_query.is_empty() {
                        return false;
                    }
                    if ctrl {
                        let trimmed = st.search_query.trim_end();
                        let cut = trimmed.rfind(' ').map_or(0, |i| i + 1);
                        st.search_query.truncate(cut);
                    } else {
                        st.search_query.pop();
                    }
                    st.search_selected = 0;
                    st.search_caret_on = true;
                    refresh_search_results_in_app(st);
                    true
                })
                .unwrap_or(false);
            } else if code >= 0x20 && code != 0x7F && let Some(ch) = char::from_u32(code) {
                changed = with_app(|st| {
                    if st.search_query.chars().count() >= 128 {
                        return false;
                    }
                    st.search_query.push(ch);
                    st.search_selected = 0;
                    st.search_caret_on = true;
                    refresh_search_results_in_app(st);
                    true
                })
                .unwrap_or(false);
            }
            if changed {
                present_search_bar_safe();
                present_search_panel_safe();
            }
            0
        }
        WM_ACTIVATE if (wp & 0xFFFF) == 0 => {
            let other = lp as HWND;
            let is_panel = with_app(|st| !other.is_null() && other == st.hwnd_search_panel).unwrap_or(false);
            if !is_panel {
                close_search_bar_safe();
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn search_panel_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &ps);
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            let inv = with_app(|st| {
                let hit = st
                    .search_panel_layout
                    .as_ref()
                    .map_or(SearchPanelHit::None, |l| hit_test_search_panel(l, x, y));
                let mut dirty = false;
                if st.search_panel_hover != hit {
                    st.search_panel_hover = hit;
                    dirty = true;
                }
                if let SearchPanelHit::Row(i) = hit
                    && st.search_selected != i
                {
                    st.search_selected = i;
                    dirty = true;
                }
                dirty
            })
            .unwrap_or(false);
            if inv {
                present_search_panel_safe();
            }
            0
        }
        WM_LBUTTONUP => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            let as_admin = GetKeyState(VK_CONTROL as i32) < 0;
            enum PanelAct {
                None,
                SetScope(SearchScope),
                Launch(SearchResult),
            }
            let act = with_app(|st| {
                let lay = st.search_panel_layout.as_ref()?;
                match hit_test_search_panel(lay, x, y) {
                    SearchPanelHit::Scope(sc) => Some(PanelAct::SetScope(sc)),
                    SearchPanelHit::Row(i) => st.search_results.get(i).cloned().map(PanelAct::Launch),
                    SearchPanelHit::None => None,
                }
            })
            .flatten()
            .unwrap_or(PanelAct::None);

            match act {
                PanelAct::None => {}
                PanelAct::SetScope(sc) => {
                    with_app(|st| {
                        st.search_scope = sc;
                        st.search_selected = 0;
                        refresh_search_results_in_app(st);
                    });
                    present_search_bar_safe();
                    present_search_panel_safe();
                }
                PanelAct::Launch(res) => {
                    close_search_bar_safe();
                    execute_result(&res, as_admin);
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn show_tooltip_safe(mon_idx: usize, kind: SurfaceKind, zone: HitZone) {
    enum TipOp {
        None,
        Hide,
        Show(HWND),
    }
    let op = with_app(|st| {
        let Some(m) = st.monitors.get(mon_idx) else { return TipOp::None };
        if m.fullscreen {
            return TipOp::Hide;
        }
        let text = match zone {
            HitZone::Workspace => format!("{}  ·  Click: GP PC Boost UI  ·  Right-click: Task Manager", st.workspace),
            HitZone::Gear => "Taskbar Settings".into(),
            HitZone::Start => "Start (Win)  ·  Right-click: Quick Link Menu (Win+X)".into(),
            HitZone::Search => "Search".into(),
            HitZone::Coucou => st.coucou.snapshot().tooltip_text(),
            HitZone::TaskView => "Task View (Win+Tab)".into(),
            HitZone::App(i) => m.items.get(i).map(|it| it.label.clone()).unwrap_or_default(),
            HitZone::TrayChevron => {
                let over = st.tray.overflow_indices(st.cfg.tray_mode.limit()).len();
                if over > 0 {
                    format!("Show hidden icons ({over})")
                } else {
                    "System tray icons".into()
                }
            }
            HitZone::TrayIcon(slot) => {
                let bar = st.tray.bar_indices(st.cfg.tray_mode.limit());
                bar.get(slot)
                    .and_then(|&ti| st.tray.icons.get(ti))
                    .map(|x| if x.tip.is_empty() { x.key.clone() } else { x.tip.clone() })
                    .unwrap_or_default()
            }
            HitZone::InputLang => format!("Keyboard layout: {}  ·  Click: switch (Win+Space)", st.lang_code),
            HitZone::Network => if st.telem.online.load(Relaxed) { "Network: Connected".into() } else { "Network: Offline".into() },
            HitZone::Volume => "Volume  ·  Scroll: adjust  ·  Middle-click: mute  ·  Click: Quick Settings".into(),
            HitZone::Battery => format!("Battery {}%", st.telem.bat_pct.load(Relaxed)),
            HitZone::Clock => format!("{}  ·  Click: Calendar & Notifications", st.split_clock_str),
            HitZone::ShowDesktop => "Show Desktop (Win+D)".into(),
            HitZone::None => String::new(),
        };
        if text.is_empty() || st.popup_open || st.tray_fly_open || st.coucou_hub_open {
            return TipOp::Hide;
        }
        let s = |v: i32| sc(v, m.spec.scale);
        let tw = ((text.chars().count() as i32) * s(6) + s(24)).clamp(s(84), s(340));
        let th = s(28);
        let (cx, _, base) = zone_anchor(kind, &m.lay, zone);
        let x = (cx - tw / 2).clamp(m.spec.rect.left + s(8), m.spec.rect.right - tw - s(8));
        let y = if base.top < (m.spec.rect.top + m.spec.rect.bottom) / 2 {
            base.bottom + s(6)
        } else {
            base.top - th - s(6)
        };
        st.tip_text = text;
        st.tip_target = Some((mon_idx, kind, zone));
        st.tip_rect = RECT { left: x, top: y, right: x + tw, bottom: y + th };
        TipOp::Show(st.hwnd_tip)
    })
    .unwrap_or(TipOp::None);

    match op {
        TipOp::None => {}
        TipOp::Hide => hide_tooltip_safe(),
        TipOp::Show(hw) => {
            if !hw.is_null() {
                present_tip_safe();
                SetWindowPos(hw, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
            }
        }
    }
}

unsafe fn show_preview_safe(mon_idx: usize, app_idx: usize) {
    hide_tooltip_safe();
    let prep = with_app(|st| {
        if st.popup_open || st.drag.is_some_and(|d| d.active) {
            return None;
        }
        let m = st.monitors.get(mon_idx)?;
        if m.fullscreen {
            return None;
        }
        let item = m.items.get(app_idx)?;
        if item.wins.is_empty() {
            return None;
        }
        let mut entries: Vec<(HWND, String)> = Vec::new();
        for &w in item.wins.iter().take(5) {
            if IsWindow(w) != 0 {
                let t = window_title(w);
                let title = if t.is_empty() { item.label.clone() } else { t };
                entries.push((w, title));
            }
        }
        if entries.is_empty() {
            return None;
        }
        let (anchor_cx, _, dock_rect) = zone_anchor(SurfaceKind::Dock, &m.lay, HitZone::App(app_idx));
        let layout = compute_preview_layout(m.spec.rect, anchor_cx, dock_rect, &entries, m.spec.scale);
        let old_thumbs = std::mem::take(&mut st.preview_thumbs);
        st.preview_layout = Some(layout.clone());
        st.preview_icon_key = item.icon_key.clone();
        st.preview_target = Some((mon_idx, app_idx));
        st.preview_mon = mon_idx;
        st.preview_hover = PreviewHit::None;
        st.preview_open = true;
        Some((st.host_hwnd(), st.hwnd_preview, layout, old_thumbs))
    })
    .flatten();

    let Some((host, hw, layout, old_thumbs)) = prep else {
        close_preview_safe();
        return;
    };
    if !host.is_null() {
        KillTimer(host, TIMER_PREVIEW_HIDE);
        KillTimer(host, TIMER_PREVIEW_SHOW);
        KillTimer(host, TIMER_AUTOHIDE_HOLD);
    }
    for th in old_thumbs {
        let _ = DwmUnregisterThumbnail(th);
    }
    if hw.is_null() {
        return;
    }
    let wr = layout.win_rect;
    SetWindowPos(
        hw,
        HWND_TOPMOST,
        wr.left,
        wr.top,
        (wr.right - wr.left).max(1),
        (wr.bottom - wr.top).max(1),
        SWP_NOACTIVATE | SWP_SHOWWINDOW,
    );
    InvalidateRect(hw, null(), 0);

    let mut new_thumbs = Vec::new();
    for card in &layout.cards {
        if IsIconic(card.hwnd) != 0 {
            // Minimized windows don't render live DWM frames; let the recessed well show the crisp app icon
            continue;
        }
        let mut hthumb: isize = 0;
        if DwmRegisterThumbnail(hw, card.hwnd, &mut hthumb) == 0 && hthumb != 0 {
            let mut src_sz = SIZE { cx: 0, cy: 0 };
            let dest = if DwmQueryThumbnailSourceSize(hthumb, &mut src_sz) == 0 && src_sz.cx > 0 && src_sz.cy > 0 {
                let avail_w = (card.thumb_rect.right - card.thumb_rect.left).max(1) as f32;
                let avail_h = (card.thumb_rect.bottom - card.thumb_rect.top).max(1) as f32;
                let k = (avail_w / src_sz.cx as f32).min(avail_h / src_sz.cy as f32).min(1.0);
                let dw = (src_sz.cx as f32 * k).round().max(1.0) as i32;
                let dh = (src_sz.cy as f32 * k).round().max(1.0) as i32;
                let dx = card.thumb_rect.left + ((card.thumb_rect.right - card.thumb_rect.left) - dw) / 2;
                let dy = card.thumb_rect.top + ((card.thumb_rect.bottom - card.thumb_rect.top) - dh) / 2;
                RECT { left: dx, top: dy, right: dx + dw, bottom: dy + dh }
            } else {
                card.thumb_rect
            };
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: dest,
                rcSource: RECT { left: 0, top: 0, right: 0, bottom: 0 },
                opacity: 255,
                fVisible: 1,
                fSourceClientAreaOnly: 0,
            };
            let _ = DwmUpdateThumbnailProperties(hthumb, &props);
            new_thumbs.push(hthumb);
        }
    }
    with_app(|st| {
        st.preview_thumbs = new_thumbs;
    });
}

unsafe fn close_preview_safe() {
    let (host, hw, old_thumbs, rehide_dock) = with_app(|st| {
        let thumbs = std::mem::take(&mut st.preview_thumbs);
        let was_open = st.preview_open;
        st.preview_open = false;
        st.preview_hover = PreviewHit::None;
        st.preview_target = None;
        st.preview_layout = None;
        st.preview_tracking = false;
        if was_open {
            st.preview_last_closed_ms = GetTickCount64();
        }
        let rd = was_open
            && st.cfg.autohide
            && !st.popup_open
            && !st.menu_open
            && !st.tray_fly_open
            && !st.coucou_hub_open
            && st.monitors.iter().all(|m| !m.mouse_inside);
        (st.host_hwnd(), st.hwnd_preview, thumbs, rd)
    })
    .unwrap_or((null_mut(), null_mut(), Vec::new(), false));
    if !host.is_null() {
        KillTimer(host, TIMER_PREVIEW_SHOW);
        KillTimer(host, TIMER_PREVIEW_HIDE);
        if rehide_dock {
            SetTimer(host, TIMER_AUTOHIDE_HOLD, 650, None);
        }
    }
    for th in old_thumbs {
        let _ = DwmUnregisterThumbnail(th);
    }
    if !hw.is_null() && IsWindowVisible(hw) != 0 {
        ShowWindow(hw, SW_HIDE);
    }
}

unsafe fn hide_tooltip_safe() {
    let (host, tip) = with_app(|st| {
        st.tip_last_closed_ms = GetTickCount64();
        (st.host_hwnd(), st.hwnd_tip)
    })
    .unwrap_or((null_mut(), null_mut()));
    if !host.is_null() {
        KillTimer(host, TIMER_TIP);
    }
    if !tip.is_null() && IsWindowVisible(tip) != 0 {
        ShowWindow(tip, SW_HIDE);
    }
}

unsafe fn start_mon_anim_safe(mon_idx: usize, target: f32) {
    let (show_bars, sensor_to_hide) = with_app(|st| {
        let split = st.cfg.layout == LayoutMode::Win12Split;
        let Some(m) = st.monitors.get_mut(mon_idx) else {
            return (Vec::new(), null_mut());
        };
        // Native Windows Gaming Mode: NEVER pop/animate a dock or unified bar on a fullscreen monitor!
        if target > 0.0 && m.fullscreen {
            return (Vec::new(), null_mut());
        }
        if (m.anim_spring.target - target).abs() < 0.01 && !m.anim_spring.animating() {
            return (Vec::new(), null_mut());
        }
        let preset = if target > 0.5 { MOTION_PEEK } else { MOTION_HIDE };
        m.anim_spring.spring_to(target, preset);
        st.pacer.wake();
        let mut out = Vec::new();
        let mut hide_sensor = null_mut();
        if target > 0.0 {
            hide_sensor = m.hwnd_sensor;
            let p = m.anim_spring.value;
            let a = bar_slide_alpha(p);
            let tc_x = m.tc_x_spring.value.round() as i32;
            let push_one = |out: &mut Vec<(HWND, i32, i32, u8)>, hw: HWND, ro: Option<RECT>, x_override: Option<i32>| {
                if !hw.is_null()
                    && let Some(r) = ro
                {
                    let dy = anim_dy_for(r, m.spec.rect, m.spec.scale, p);
                    out.push((hw, x_override.unwrap_or(r.left), r.top + dy, a));
                }
            };
            push_one(&mut out, m.hwnd_dock, m.lay.dock, None);
            if !split {
                push_one(&mut out, m.hwnd_tl, m.lay.top_left, None);
                push_one(&mut out, m.hwnd_tc, m.lay.top_center, Some(tc_x));
                push_one(&mut out, m.hwnd_tr, m.lay.top_right, None);
            }
        }
        (out, hide_sensor)
    })
    .unwrap_or((Vec::new(), null_mut()));

    if !sensor_to_hide.is_null() && IsWindowVisible(sensor_to_hide) != 0 {
        ShowWindow(sensor_to_hide, SW_HIDE);
    }
    for (hw, x, y, a) in show_bars {
        if IsWindowVisible(hw) == 0 {
            move_layered_alpha(hw, x, y, a);
            SetWindowPos(hw, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }
}

unsafe fn start_status_anim_safe(mon_idx: usize, target: f32) {
    let (show_bars, sensor_to_hide, host, coucou_msg) = with_app(|st| {
        let host = st.host_hwnd();
        let docked = st.coucou_container_active();
        let Some(m) = st.monitors.get_mut(mon_idx) else {
            return (Vec::new(), null_mut(), null_mut(), None);
        };
        // Native Windows Gaming Mode: NEVER pop/animate status modules on a fullscreen monitor!
        if target > 0.0 && m.fullscreen {
            return (Vec::new(), null_mut(), null_mut(), None);
        }
        if (m.status_spring.target - target).abs() < 0.01 && !m.status_spring.animating() {
            return (Vec::new(), null_mut(), null_mut(), None);
        }
        let preset = if target > 0.5 { MOTION_PEEK } else { MOTION_HIDE };
        m.status_spring.spring_to(target, preset);
        st.pacer.wake();
        // Only tell Coucou to hide when Coucou is NOT actively expanded by the user (`mode != 2`)
        // AND the user isn't hovering Coucou.
        let cmsg = if docked && m.spec.primary && target <= 0.5 && st.coucou_live.mode == 0 {
            Some(r#"{"taskbar_cmd":"dock_hide"}"#.to_string())
        } else {
            None
        };
        if target > 0.0 {
            let p = m.status_spring.value;
            let a = bar_slide_alpha(p);
            let tc_x = m.tc_x_spring.value.round() as i32;
            let behind_coucou = st.coucou_live.mode == 1 && st.coucou_live.hmon == m.spec.hmon;
            let mut out = Vec::new();
            let tc_blocked = (st.coucou_live.mode == 2 && st.coucou_live.hmon == m.spec.hmon)
                || (st.search_open && st.search_mon == mon_idx);
            let pairs = [
                (m.hwnd_tl, m.lay.top_left, None, false),
                (if tc_blocked { null_mut() } else { m.hwnd_tc }, m.lay.top_center, Some(tc_x), behind_coucou),
                (m.hwnd_tr, m.lay.top_right, None, false),
            ];
            for (hw, ro, x_ov, behind) in pairs {
                if !hw.is_null()
                    && let Some(r) = ro
                {
                    let dy = anim_dy_for(r, m.spec.rect, m.spec.scale, p);
                    out.push((hw, x_ov.unwrap_or(r.left), r.top + dy, a, behind));
                }
            }
            (out, m.hwnd_status_sensor, host, cmsg)
        } else {
            (Vec::new(), null_mut(), host, cmsg)
        }
    })
    .unwrap_or((Vec::new(), null_mut(), null_mut(), None));

    if let Some(payload) = coucou_msg {
        send_coucou_json_async(payload);
    }
    if !sensor_to_hide.is_null() {
        ShowWindow(sensor_to_hide, SW_HIDE);
    }
    let coucou_hw = coucou_window_hwnd();
    for (hw, x, y, a, behind) in show_bars {
        if IsWindowVisible(hw) == 0 {
            move_layered_alpha(hw, x, y, a);
            let after = if behind && !coucou_hw.is_null() { coucou_hw } else { HWND_TOPMOST };
            SetWindowPos(hw, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
    }
    if !host.is_null() {
        KillTimer(host, TIMER_STATUS_HIDE);
    }
}

/// Hardware-VSync animation frame step (driven by `VsyncPacer` `DwmFlush()` at 60–240Hz).
/// Every sliding bar, flyout, popup, and search results panel animates purely on the DWM GPU
/// compositor via `move_layered_alpha` (zero GDI redraws); the Search Bar morphs inside its
/// fixed pre-allocated canvas with zero DIB reallocation or HWND resizing.
unsafe fn step_vsync_springs_safe() {
    struct MonStep {
        tl: HWND,
        tc: HWND,
        tr: HWND,
        dock: HWND,
        dock_sensor: HWND,
        status_sensor: HWND,
        lay: MonitorLayout,
        rect: RECT,
        scale: f32,
        dock_p: f32,
        status_p: f32,
        tc_x: i32,
        dock_moved: bool,
        status_moved: bool,
        tc_moved: bool,
        hide_dock: bool,
        hide_status: bool,
        rearm_dock_sensor: bool,
        rearm_status_sensor: bool,
    }
    struct FlyStep {
        hwnd: HWND,
        x: i32,
        y: i32,
        alpha: u8,
        hide: bool,
    }
    struct SearchClose {
        hb: HWND,
        hp: HWND,
        host: HWND,
        restore_bar: Option<(usize, SurfaceKind, HWND)>,
        sched_status: bool,
        sched_dock: bool,
    }

    let Some((mon_steps, fly_steps, paint_search_bar, search_close)) = with_app(|st| {
        let dt = st.pacer.take_dt();
        let split = st.cfg.layout == LayoutMode::Win12Split;
        let dock_auto = st.cfg.autohide && !st.smoke_test;
        let status_auto = split && st.cfg.status_autohide && !st.smoke_test;
        let mut any_running = false;
        let mut mon_steps = Vec::new();
        let mut fly_steps = Vec::new();

        // 1. Per-monitor Dock, Bottom/Top Status Strip & Companion TopCenter X springs
        for m in &mut st.monitors {
            let mut dock_moved = false;
            let mut status_moved = false;
            let mut tc_moved = false;
            let mut hide_dock = false;
            let mut hide_status = false;
            let mut rearm_dock_sensor = false;
            let mut rearm_status_sensor = false;

            if m.anim_spring.step(dt) {
                dock_moved = true;
                if m.anim_spring.animating() {
                    any_running = true;
                } else if m.anim_spring.value <= 0.01 {
                    hide_dock = true;
                    if !split {
                        hide_status = true;
                    }
                    if dock_auto && !m.fullscreen {
                        rearm_dock_sensor = true;
                    }
                }
            }
            if split && m.status_spring.step(dt) {
                status_moved = true;
                if m.status_spring.animating() {
                    any_running = true;
                } else if m.status_spring.value <= 0.01 {
                    hide_status = true;
                    if status_auto && !m.fullscreen {
                        rearm_status_sensor = true;
                    }
                }
            }
            if m.tc_x_spring.step(dt) {
                tc_moved = true;
                if m.tc_x_spring.animating() {
                    any_running = true;
                }
            }
            if dock_moved || status_moved || tc_moved {
                mon_steps.push(MonStep {
                    tl: m.hwnd_tl,
                    tc: m.hwnd_tc,
                    tr: m.hwnd_tr,
                    dock: m.hwnd_dock,
                    dock_sensor: m.hwnd_sensor,
                    status_sensor: m.hwnd_status_sensor,
                    lay: m.lay.clone(),
                    rect: m.spec.rect,
                    scale: m.spec.scale,
                    dock_p: m.anim_spring.value,
                    status_p: if split { m.status_spring.value } else { m.anim_spring.value },
                    tc_x: m.tc_x_spring.value.round() as i32,
                    dock_moved,
                    status_moved: if split { status_moved } else { dock_moved },
                    tc_moved,
                    hide_dock,
                    hide_status,
                    rearm_dock_sensor,
                    rearm_status_sensor,
                });
            }
        }

        // 2. Taskbar Settings Popup spring (GPU slide + fade)
        if st.popup_spring.step(dt) {
            if st.popup_spring.animating() {
                any_running = true;
            }
            let r = st.popup_rect;
            let (mrect, scale) = st
                .monitors
                .get(st.popup_mon)
                .or_else(|| st.monitors.first())
                .map_or((RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, 1.0), |m| (m.spec.rect, m.spec.scale));
            let p = st.popup_spring.value;
            let dy = flyout_slide_dy(r, mrect, scale, p, 14);
            let hide = !st.popup_open && !st.popup_spring.animating() && p <= 0.01;
            if !st.hwnd_popup.is_null() {
                fly_steps.push(FlyStep { hwnd: st.hwnd_popup, x: r.left, y: r.top + dy, alpha: spring_alpha(p), hide });
            }
        }

        // 3. Tray Overflow Flyout spring (GPU slide + fade, zero GDI redraw per frame)
        if st.tray_fly_spring.step(dt) {
            if st.tray_fly_spring.animating() {
                any_running = true;
            }
            let p = st.tray_fly_spring.value;
            let hide = !st.tray_fly_open && !st.tray_fly_spring.animating() && p <= 0.01;
            if let Some(ref lay) = st.tray_fly_layout
                && !st.hwnd_tray_fly.is_null()
            {
                let r = lay.win_rect;
                let (mrect, scale) = st
                    .monitors
                    .get(st.tray_fly_mon)
                    .or_else(|| st.monitors.first())
                    .map_or((RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, 1.0), |m| (m.spec.rect, m.spec.scale));
                let dy = flyout_slide_dy(r, mrect, scale, p, 14);
                fly_steps.push(FlyStep { hwnd: st.hwnd_tray_fly, x: r.left, y: r.top + dy, alpha: spring_alpha(p), hide });
            }
            if hide {
                st.tray_fly_layout = None;
            }
        }

        // 4. Coucou Hub Flyout spring (GPU slide + fade)
        if st.coucou_hub_spring.step(dt) {
            if st.coucou_hub_spring.animating() {
                any_running = true;
            }
            let p = st.coucou_hub_spring.value;
            let hide = !st.coucou_hub_open && !st.coucou_hub_spring.animating() && p <= 0.01;
            if let Some(ref lay) = st.coucou_hub_layout
                && !st.hwnd_coucou_hub.is_null()
            {
                let r = lay.win_rect;
                let (mrect, scale) = st
                    .monitors
                    .get(st.coucou_hub_mon)
                    .or_else(|| st.monitors.first())
                    .map_or((RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, 1.0), |m| (m.spec.rect, m.spec.scale));
                let dy = flyout_slide_dy(r, mrect, scale, p, 14);
                fly_steps.push(FlyStep { hwnd: st.hwnd_coucou_hub, x: r.left, y: r.top + dy, alpha: spring_alpha(p), hide });
            }
            if hide {
                st.coucou_hub_layout = None;
            }
        }

        // 5. Native Search Bar pill morph + Search Results Panel GPU slide+fade
        let mut paint_search_bar = false;
        let mut search_close = None;
        if st.search_spring.step(dt) {
            if st.search_spring.target <= 0.0 && st.search_spring.value <= 0.03 {
                st.search_spring.jump(0.0);
            } else if st.search_spring.animating() {
                any_running = true;
            }
            if st.search_open && st.search_spring.value > 0.03 {
                paint_search_bar = true;
            }
        }
        if st.search_panel_spring.step(dt) {
            if st.search_panel_spring.target <= 0.0 && st.search_panel_spring.value <= 0.03 {
                st.search_panel_spring.jump(0.0);
            } else if st.search_panel_spring.animating() {
                any_running = true;
            }
            if st.search_open
                && let Some(ref lay) = st.search_panel_layout
                && !st.hwnd_search_panel.is_null()
            {
                let r = lay.win_rect;
                let (mrect, scale) = st
                    .monitors
                    .get(st.search_mon)
                    .or_else(|| st.monitors.first())
                    .map_or((RECT { left: 0, top: 0, right: 1920, bottom: 1080 }, 1.0), |m| (m.spec.rect, m.spec.scale));
                let p = st.search_panel_spring.value;
                let dy = flyout_slide_dy(r, mrect, scale, p, 14);
                let hide = st.search_panel_spring.target <= 0.0 && p <= 0.03;
                fly_steps.push(FlyStep { hwnd: st.hwnd_search_panel, x: r.left, y: r.top + dy, alpha: spring_alpha(p), hide });
            }
        }

        if st.search_open
            && st.search_spring.target <= 0.0
            && !st.search_spring.animating()
            && !st.search_panel_spring.animating()
        {
            st.search_open = false;
            paint_search_bar = false;
            let mut pt: POINT = zeroed();
            let have_pt = GetCursorPos(&mut pt) != 0;
            let on_status = have_pt && st.monitors.iter().any(|m| cursor_on_status_strip(pt, m, &st.cfg, st.coucou_live));
            let on_dock = have_pt && st.monitors.iter().any(|m| cursor_on_dock_strip(pt, m, &st.cfg));
            let sched_status = st.cfg.layout == LayoutMode::Win12Split
                && st.cfg.status_autohide
                && !st.popup_open
                && !st.menu_open
                && !st.tray_fly_open
                && !on_status;
            let sched_dock = st.cfg.autohide && !st.popup_open && !st.menu_open && !on_dock;
            let restore_bar = st.monitors.get(st.search_mon).and_then(|m| {
                if m.fullscreen {
                    return None;
                }
                if st.cfg.layout == LayoutMode::Win12Split {
                    if !m.hwnd_tc.is_null()
                        && !st.coucou_full_covering(m.spec.hmon)
                        && (!st.cfg.status_autohide || m.status_spring.value > 0.5)
                    {
                        Some((st.search_mon, SurfaceKind::TopCenter, m.hwnd_tc))
                    } else {
                        None
                    }
                } else if !m.hwnd_dock.is_null() && (!st.cfg.autohide || m.anim_spring.value > 0.5) {
                    Some((st.search_mon, SurfaceKind::Dock, m.hwnd_dock))
                } else {
                    None
                }
            });
            search_close = Some(SearchClose {
                hb: st.hwnd_search_bar,
                hp: st.hwnd_search_panel,
                host: st.host_hwnd(),
                restore_bar,
                sched_status,
                sched_dock,
            });
        }

        if !any_running {
            st.pacer.sleep();
        }
        (mon_steps, fly_steps, paint_search_bar, search_close)
    }) else {
        return;
    };

    for stp in mon_steps {
        if stp.dock_moved
            && let Some(r) = stp.lay.dock
            && !stp.dock.is_null()
        {
            let dy = anim_dy_for(r, stp.rect, stp.scale, stp.dock_p);
            move_layered_alpha(stp.dock, r.left, r.top + dy, bar_slide_alpha(stp.dock_p));
        }
        if stp.status_moved || stp.tc_moved {
            let a = bar_slide_alpha(stp.status_p);
            if stp.status_moved {
                for (hw, rect_opt) in [(stp.tl, stp.lay.top_left), (stp.tr, stp.lay.top_right)] {
                    if let Some(r) = rect_opt
                        && !hw.is_null()
                    {
                        let dy = anim_dy_for(r, stp.rect, stp.scale, stp.status_p);
                        move_layered_alpha(hw, r.left, r.top + dy, a);
                    }
                }
            }
            if let Some(r) = stp.lay.top_center
                && !stp.tc.is_null()
            {
                let dy = anim_dy_for(r, stp.rect, stp.scale, stp.status_p);
                move_layered_alpha(stp.tc, stp.tc_x, r.top + dy, a);
            }
        }
        if stp.hide_dock && !stp.dock.is_null() {
            ShowWindow(stp.dock, SW_HIDE);
        }
        if stp.hide_status {
            for hw in [stp.tl, stp.tc, stp.tr] {
                if !hw.is_null() {
                    ShowWindow(hw, SW_HIDE);
                }
            }
        }
        if stp.rearm_dock_sensor && !stp.dock_sensor.is_null() {
            ShowWindow(stp.dock_sensor, SW_SHOWNOACTIVATE);
        }
        if stp.rearm_status_sensor && !stp.status_sensor.is_null() {
            ShowWindow(stp.status_sensor, SW_SHOWNOACTIVATE);
        }
    }

    for f in fly_steps {
        move_layered_alpha(f.hwnd, f.x, f.y, if f.hide { 0 } else { f.alpha });
        if f.hide && IsWindowVisible(f.hwnd) != 0 {
            ShowWindow(f.hwnd, SW_HIDE);
        }
    }

    if paint_search_bar {
        present_search_bar_safe();
    }

    if let Some(sc_end) = search_close {
        if let Some((mi, kind, hw)) = sc_end.restore_bar {
            present_bar_safe(mi, kind);
            let coucou_hw = coucou_window_hwnd();
            let behind = kind == SurfaceKind::TopCenter
                && with_app(|st| {
                    st.monitors.get(mi).is_some_and(|m| st.coucou_live.mode == 1 && st.coucou_live.hmon == m.spec.hmon)
                })
                .unwrap_or(false);
            let after = if behind && !coucou_hw.is_null() { coucou_hw } else { HWND_TOPMOST };
            SetWindowPos(hw, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW);
        }
        if !sc_end.hb.is_null() {
            if let Some(r) = with_app(|st| st.search_bar_layout.as_ref().map(|l| l.full_rect)).flatten() {
                move_layered_alpha(sc_end.hb, r.left, r.top, 0);
            }
            ShowWindow(sc_end.hb, SW_HIDE);
        }
        if !sc_end.hp.is_null() {
            if let Some(r) = with_app(|st| st.search_panel_layout.as_ref().map(|l| l.win_rect)).flatten() {
                move_layered_alpha(sc_end.hp, r.left, r.top, 0);
            }
            ShowWindow(sc_end.hp, SW_HIDE);
        }
        if !sc_end.host.is_null() {
            KillTimer(sc_end.host, TIMER_SEARCH_CARET);
            if sc_end.sched_status {
                SetTimer(sc_end.host, TIMER_STATUS_HIDE, 450, None);
            }
            if sc_end.sched_dock {
                SetTimer(sc_end.host, TIMER_AUTOHIDE_HOLD, 450, None);
            }
        }
    }
}

unsafe fn write_smoke_report() {
    let dir = env::temp_dir();
    let report = with_app(|st| {
        let mut out = format!(
            "monitors={}\npaints={}\nulw_ok={}\npins={}\nicons_loaded={}\ntray_icons={}\ntray_host_ok={}\n",
            st.monitors.len(),
            st.total_paints,
            st.ulw_ok,
            st.pins.len(),
            st.icons.loaded_count(),
            st.tray.icons.len(),
            !st.hwnd_tray_host.is_null(),
        );
        for (pi, p) in st.pins.iter().enumerate() {
            let kind = if let Some(a) = &p.aumid {
                format!("msix:{a}")
            } else {
                "lnk".into()
            };
            let has_icon = st.icons.get_cached(&p.key).is_some();
            out.push_str(&format!("pin[{pi}]: label='{}' kind={} cached_icon={}\n", p.label, kind, has_icon));
        }
        for (i, m) in st.monitors.iter().enumerate() {
            let r = m.spec.rect;
            let fmt_r = |ro: Option<RECT>| -> String {
                match ro {
                    Some(x) => format!("({},{},{},{})", x.left, x.top, x.right, x.bottom),
                    None => "none".into(),
                }
            };
            out.push_str(&format!(
                "mon[{i}]: primary={} rect=({},{},{},{}) dock={} top_left={} top_center={} top_right={} paints={}\n",
                m.spec.primary,
                r.left,
                r.top,
                r.right,
                r.bottom,
                fmt_r(m.lay.dock),
                fmt_r(m.lay.top_left),
                fmt_r(m.lay.top_center),
                fmt_r(m.lay.top_right),
                m.paints
            ));
            if m.surf_dock.w > 0 {
                m.surf_dock.save_bmp(&dir.join(format!("gp-smoke-mon{i}-dock.bmp")));
            }
            if m.surf_tl.w > 0 {
                m.surf_tl.save_bmp(&dir.join(format!("gp-smoke-mon{i}-tl.bmp")));
            }
            if m.surf_tc.w > 0 {
                m.surf_tc.save_bmp(&dir.join(format!("gp-smoke-mon{i}-tc.bmp")));
            }
            if m.surf_tr.w > 0 {
                m.surf_tr.save_bmp(&dir.join(format!("gp-smoke-mon{i}-tr.bmp")));
            }
        }
        out
    })
    .unwrap_or_else(|| "monitors=0\npaints=0\nulw_ok=0\n".into());

    let path = dir.join("gp-taskbar-smoke.txt");
    let _ = std::fs::write(path, report);
}

unsafe fn position_sensor(m: &MonWindows, cfg: &Config) {
    let y = match cfg.layout {
        LayoutMode::UnifiedBottom => m.spec.rect.bottom - 3,
        LayoutMode::UnifiedTop => m.spec.rect.top,
        LayoutMode::Win12Split => {
            if cfg.dock_edge == DockEdge::Bottom { m.spec.rect.bottom - 3 } else { m.spec.rect.top }
        }
    };
    SetWindowPos(
        m.hwnd_sensor,
        HWND_TOPMOST,
        m.spec.rect.left,
        y,
        (m.spec.rect.right - m.spec.rect.left).max(1),
        3,
        SWP_NOACTIVATE | SWP_NOSENDCHANGING,
    );
}

unsafe fn position_status_sensor(m: &MonWindows, cfg: &Config) {
    let y = if cfg.dock_edge == DockEdge::Bottom {
        m.spec.rect.top
    } else {
        m.spec.rect.bottom - 4
    };
    SetWindowPos(
        m.hwnd_status_sensor,
        HWND_TOPMOST,
        m.spec.rect.left,
        y,
        (m.spec.rect.right - m.spec.rect.left).max(1),
        4,
        SWP_NOACTIVATE | SWP_NOSENDCHANGING,
    );
}

unsafe fn run_taskbar_context_menu_safe(owner: HWND, mon_idx: usize, anchor_rect: RECT, screen_x: i32, screen_y: i32) {
    let (host, dock_sz, dock_ah, status_ah, gaming_on, show_prev, show_sec) = with_app(|st| {
        st.menu_open = true;
        (
            st.host_hwnd(),
            st.cfg.dock_size,
            st.cfg.autohide,
            st.cfg.status_autohide,
            st.cfg.gaming_mode != GamingModePref::Off,
            st.cfg.show_previews,
            st.cfg.show_seconds,
        )
    })
    .unwrap_or((null_mut(), DockSize::Compact, false, true, true, true, false));
    if !host.is_null() {
        KillTimer(host, TIMER_AUTOHIDE_HOLD);
        KillTimer(host, TIMER_STATUS_HIDE);
    }

    let cmd = show_taskbar_context_menu(owner, screen_x, screen_y, dock_sz, dock_ah, status_ah, gaming_on, show_prev, show_sec);

    let (rehide_host, rehide_status) = with_app(|st| {
        st.menu_open = false;
        let rs = st.cfg.layout == LayoutMode::Win12Split
            && st.cfg.status_autohide
            && !st.popup_open
            && !st.tray_fly_open
            && !st.coucou_hub_open
            && st.monitors.iter().all(|m| !m.status_mouse_inside);
        (st.host_hwnd(), rs)
    })
    .unwrap_or((null_mut(), false));

    match cmd {
        TaskbarMenuCmd::None => {}
        TaskbarMenuCmd::ShowDesktop => send_chord(&[0x5B, b'D' as u16]),
        TaskbarMenuCmd::TaskManager => launch_task_manager(),
        TaskbarMenuCmd::RunDialog => send_chord(&[0x5B, b'R' as u16]),
        TaskbarMenuCmd::SetDockSize(sz) => {
            with_app(|st| {
                st.cfg.dock_size = sz;
                let _ = save_config(&st.cfg);
            });
            rebuild_monitors_safe();
            return;
        }
        TaskbarMenuCmd::ToggleStatusAutoHide => {
            with_app(|st| {
                st.cfg.status_autohide = !st.cfg.status_autohide;
                let _ = save_config(&st.cfg);
            });
            rebuild_monitors_safe();
            return;
        }
        TaskbarMenuCmd::ToggleDockAutoHide => {
            with_app(|st| {
                st.cfg.autohide = !st.cfg.autohide;
                let _ = save_config(&st.cfg);
            });
            rebuild_monitors_safe();
            return;
        }
        TaskbarMenuCmd::ToggleGamingMode => {
            with_app(|st| {
                st.cfg.gaming_mode = st.cfg.gaming_mode.toggle();
                let _ = save_config(&st.cfg);
            });
            sync_fullscreen_state_safe();
        }
        TaskbarMenuCmd::TogglePreviews => {
            with_app(|st| {
                st.cfg.show_previews = !st.cfg.show_previews;
                let _ = save_config(&st.cfg);
            });
            close_preview_safe();
        }
        TaskbarMenuCmd::ToggleClockSeconds => {
            let show_sec = with_app(|st| {
                st.cfg.show_seconds = !st.cfg.show_seconds;
                let _ = save_config(&st.cfg);
                st.cfg.show_seconds
            })
            .unwrap_or(false);
            let (t, d, sp) = format_clock_all(show_sec);
            with_app(|st| {
                st.time_str = t;
                st.date_str = d;
                st.split_clock_str = sp;
            });
            rebuild_monitors_safe();
            return;
        }
        TaskbarMenuCmd::OpenSettingsPopup => {
            toggle_popup_safe(mon_idx, anchor_rect);
            return;
        }
        TaskbarMenuCmd::WindowsTaskbarSettings => launch_path("ms-settings:taskbar"),
        TaskbarMenuCmd::RefreshPins => {
            let fresh_pins = load_pins();
            with_app(|st| st.pins = fresh_pins);
            refresh_windows_all_safe();
            broadcast_taskbar_created();
        }
        TaskbarMenuCmd::OpenBoostUi => launch_boost_ui(),
        TaskbarMenuCmd::RestoreStock => {
            if !rehide_host.is_null() {
                PostMessageW(rehide_host, WM_CLOSE, 0, 0);
            }
            return;
        }
    }
    if rehide_status && !rehide_host.is_null() {
        SetTimer(rehide_host, TIMER_STATUS_HIDE, 700, None);
    }
}

unsafe fn set_work_area_if_needed(mut want: RECT) {
    let mut cur: RECT = zeroed();
    SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut cur as *mut _ as *mut _, 0);
    if (cur.left, cur.top, cur.right, cur.bottom) != (want.left, want.top, want.right, want.bottom) {
        SystemParametersInfoW(SPI_SETWORKAREA, 0, &mut want as *mut _ as *mut _, 0);
        PostMessageW(HWND_BROADCAST, WM_SETTINGCHANGE, SPI_SETWORKAREA as usize, 0);
        EnumWindows(Some(trim_primary_maximized), &want as *const RECT as LPARAM);
    }
}

unsafe extern "system" fn trim_primary_maximized(hwnd: HWND, lp: LPARAM) -> i32 {
    let want = &*(lp as *const RECT);
    let primary = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
    if IsWindowVisible(hwnd) == 0
        || IsZoomed(hwnd) == 0
        || MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) != primary
    {
        return 1;
    }
    let mut r: RECT = zeroed();
    GetWindowRect(hwnd, &mut r);
    let border = GetSystemMetrics(SM_CXFRAME) + GetSystemMetrics(SM_CXPADDEDBORDER);
    let clamped = RECT {
        left: r.left.max(want.left - border),
        top: r.top.max(want.top - border),
        right: r.right.min(want.right + border),
        bottom: r.bottom.min(want.bottom + border),
    };
    if (clamped.left, clamped.top, clamped.right, clamped.bottom) != (r.left, r.top, r.right, r.bottom)
        && clamped.right > clamped.left
        && clamped.bottom > clamped.top
    {
        SetWindowPos(
            hwnd,
            null_mut(),
            clamped.left,
            clamped.top,
            clamped.right - clamped.left,
            clamped.bottom - clamped.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    1
}

/// Shell / desktop / system-flyout window classes that must NEVER trigger fullscreen Gaming Mode.
fn is_excluded_fullscreen_class(cls: &str) -> bool {
    matches!(
        cls,
        "Progman"
            | "WorkerW"
            | "Shell_TrayWnd"
            | "Shell_SecondaryTrayWnd"
            | "RustExplorer"
            | "Windows.UI.Core.CoreWindow"
            | "NotifyIconOverflowWindow"
            | "TaskListThumbnailWnd"
            | "MultitaskingViewFrame"
            | "XamlExplorerHostIslandWindow"
            | "ForegroundStaging"
            | "ApplicationManager_DesktopShellWindow"
    )
}

/// Pure classifier distinguishing genuine exclusive/borderless fullscreen games & videos
/// from ordinary maximized or windowed desktop applications (even when Explorer's taskbar is hidden).
fn classify_fullscreen_window(
    mon: RECT,
    win_rect: RECT,
    style: u32,
    is_zoomed: bool,
    quns_exclusive: bool,
) -> bool {
    if quns_exclusive {
        return true;
    }
    // Allow 1px tolerance for fractional DPI rounding (125% / 150% displays).
    let covers_monitor = win_rect.left <= mon.left + 1
        && win_rect.top <= mon.top + 1
        && win_rect.right >= mon.right - 1
        && win_rect.bottom >= mon.bottom - 1;
    if !covers_monitor {
        return false;
    }
    let has_caption = (style & WS_CAPTION) == WS_CAPTION;
    let has_thickframe = (style & WS_THICKFRAME) != 0;
    if is_zoomed {
        // Normal maximized desktop apps (browsers, IDEs, Discord, Explorer) always keep WS_CAPTION
        // and/or WS_THICKFRAME. Only a captionless, non-resizable borderless fullscreen app qualifies.
        !has_caption && !has_thickframe
    } else if !has_caption {
        // Standard borderless fullscreen game (UE5/Unity/DXGI/SDL/GLFW WS_POPUP) or F11 fullscreen video.
        true
    } else {
        // Legacy captioned exclusive/fullscreen window without resize borders matching exact monitor bounds.
        !has_thickframe
            && win_rect.left == mon.left
            && win_rect.top == mon.top
            && win_rect.right == mon.right
            && win_rect.bottom == mon.bottom
    }
}

unsafe fn detect_fullscreen_on_monitor(target_hmon: isize, own_pid: u32) -> bool {
    let fg = GetForegroundWindow();
    if fg.is_null() || IsWindowVisible(fg) == 0 || IsIconic(fg) != 0 {
        return false;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(fg, &mut pid);
    if pid == own_pid || fg == coucou_window_hwnd() {
        return false;
    }
    let mut cloaked = 0u32;
    if DwmGetWindowAttribute(fg, DWMWA_CLOAKED as u32, &mut cloaked as *mut _ as *mut _, size_of::<u32>() as u32) == 0
        && cloaked != 0
    {
        return false;
    }
    let mon = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
    if mon as isize != target_hmon {
        return false;
    }
    let mut cls = [0u16; 64];
    let n = GetClassNameW(fg, cls.as_mut_ptr(), 64).max(0) as usize;
    if is_excluded_fullscreen_class(&String::from_utf16_lossy(&cls[..n])) {
        return false;
    }
    let mut quns = 0i32;
    let quns_exclusive = SHQueryUserNotificationState(&mut quns) == 0
        && (quns == QUNS_RUNNING_D3D_FULL_SCREEN || quns == QUNS_PRESENTATION_MODE);
    let mut r: RECT = zeroed();
    if GetWindowRect(fg, &mut r) == 0 {
        return quns_exclusive;
    }
    let mut mi: MONITORINFO = zeroed();
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(mon, &mut mi) == 0 {
        return quns_exclusive;
    }
    let style = GetWindowLongW(fg, GWL_STYLE) as u32;
    classify_fullscreen_window(mi.rcMonitor, r, style, IsZoomed(fg) != 0, quns_exclusive)
}

/// Ultra-lightweight low-level keyboard hook that observes solo `Win` key taps
/// (never blocks or alters any key) so pressing the Windows key ALWAYS makes the
/// taskbar appear immediately — even during exclusive/borderless fullscreen games
/// or when auto-hide is enabled.
unsafe extern "system" fn low_level_keyboard_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && lp != 0 {
        let kb = &*(lp as *const KBDLLHOOKSTRUCT);
        let vk = kb.vkCode as u16;
        let is_down = wp == WM_KEYDOWN as usize || wp == WM_SYSKEYDOWN as usize;
        let is_up = wp == WM_KEYUP as usize || wp == WM_SYSKEYUP as usize;
        if vk == VK_LWIN || vk == VK_RWIN {
            if is_down {
                if !WIN_KEY_DOWN.swap(true, Relaxed) {
                    WIN_KEY_COMBO.store(false, Relaxed);
                }
            } else if is_up && WIN_KEY_DOWN.swap(false, Relaxed) && !WIN_KEY_COMBO.swap(false, Relaxed) {
                let host = KB_HOST_HWND.load(Relaxed) as HWND;
                if !host.is_null() {
                    PostMessageW(host, WM_APP_WIN_KEY_TAP, 0, 0);
                }
            }
        } else if is_down && WIN_KEY_DOWN.load(Relaxed) {
            WIN_KEY_COMBO.store(true, Relaxed);
        }
    }
    CallNextHookEx(null_mut(), code, wp, lp)
}

/// Pure helper matching Windows 10/11 Start Menu, Search, Calendar, and Quick Settings host EXEs.
fn is_windows_shell_flyout_exe(exe_path: &str) -> bool {
    let base = exe_path
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(exe_path)
        .to_ascii_lowercase();
    matches!(
        base.as_str(),
        "startmenuexperiencehost.exe"
            | "searchhost.exe"
            | "searchapp.exe"
            | "shellexperiencehost.exe"
            | "explorer.exe"
    )
}

/// True when `hwnd` is a Windows Start Menu, Search, Calendar/Notification Center, or Quick Settings flyout.
unsafe fn is_windows_shell_flyout(hwnd: HWND) -> bool {
    if hwnd.is_null() || IsWindowVisible(hwnd) == 0 {
        return false;
    }
    let mut cls = [0u16; 64];
    let n = GetClassNameW(hwnd, cls.as_mut_ptr(), 64).max(0) as usize;
    let cls_s = String::from_utf16_lossy(&cls[..n]);
    if cls_s == "XamlExplorerHostIslandWindow" || cls_s == "Shell_LightDismissOverlay" {
        return true;
    }
    if cls_s != "Windows.UI.Core.CoreWindow" {
        return false;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid != 0 {
        let hp = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if !hp.is_null() {
            let mut buf = [0u16; 512];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(hp, 0, buf.as_mut_ptr(), &mut len) != 0;
            CloseHandle(hp);
            if ok && len > 0 {
                let path = String::from_utf16_lossy(&buf[..len as usize]);
                return is_windows_shell_flyout_exe(&path);
            }
        }
    }
    // Fallback if OpenProcess failed: inspect window title for known shell flyout names.
    let title = window_title(hwnd);
    matches!(
        title.as_str(),
        "Start" | "Search" | "Notification Center" | "Quick settings" | "Windows Shell Experience Host" | "Jump List"
    )
}

/// Force-reveals the taskbar (Dock + Status Strip) on top of everything when the user presses
/// the Windows key or opens Start Menu / Calendar / Quick Settings — even during fullscreen games
/// or when auto-hide is enabled.
unsafe fn reveal_taskbar_for_start_safe(target_hmon: Option<isize>) {
    struct RevealMon {
        idx: usize,
        split: bool,
        topmost_bars: Vec<HWND>,
        sensors_to_hide: Vec<HWND>,
    }
    let mut host_to_quiet: HWND = null_mut();
    let mut primary_fs_cleared = false;
    let mut reveal_list: Vec<RevealMon> = Vec::new();
    with_app(|st| {
        if st.smoke_test {
            return;
        }
        host_to_quiet = st.hwnd_host;
        let split = st.cfg.layout == LayoutMode::Win12Split;
        for (idx, m) in st.monitors.iter_mut().enumerate() {
            let match_mon = target_hmon.is_none_or(|h| m.spec.hmon == h || m.spec.primary);
            if !match_mon {
                continue;
            }
            if m.fullscreen {
                m.fullscreen = false;
                if idx == 0 {
                    primary_fs_cleared = true;
                }
            }
            let mut topmost_bars = Vec::new();
            for hw in [m.hwnd_dock, m.hwnd_tl, m.hwnd_tc, m.hwnd_tr] {
                if !hw.is_null() {
                    topmost_bars.push(hw);
                }
            }
            let mut sensors_to_hide = Vec::new();
            for hw in [m.hwnd_sensor, m.hwnd_status_sensor] {
                if !hw.is_null() {
                    sensors_to_hide.push(hw);
                }
            }
            reveal_list.push(RevealMon { idx, split, topmost_bars, sensors_to_hide });
        }
    });
    if !host_to_quiet.is_null() {
        KillTimer(host_to_quiet, TIMER_AUTOHIDE_HOLD);
        KillTimer(host_to_quiet, TIMER_STATUS_HIDE);
    }
    if primary_fs_cleared {
        sync_primary_work_area_safe();
    }
    for rm in reveal_list {
        for hw in rm.sensors_to_hide {
            if IsWindowVisible(hw) != 0 {
                ShowWindow(hw, SW_HIDE);
            }
        }
        present_bar_safe(rm.idx, SurfaceKind::Dock);
        present_bar_safe(rm.idx, SurfaceKind::TopLeft);
        present_bar_safe(rm.idx, SurfaceKind::TopCenter);
        present_bar_safe(rm.idx, SurfaceKind::TopRight);
        start_mon_anim_safe(rm.idx, 1.0);
        if rm.split {
            start_status_anim_safe(rm.idx, 1.0);
        }
        // Re-assert HWND_TOPMOST even if the bar was already at 1.0 so it always rises cleanly above fullscreen games.
        for hw in rm.topmost_bars {
            if IsWindowVisible(hw) != 0 {
                SetWindowPos(hw, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
    }
}

/// Called immediately when the user taps the Windows key (`VK_LWIN` / `VK_RWIN`).
unsafe fn handle_win_key_tap_safe() {
    let fg_is_start = is_windows_shell_flyout(GetForegroundWindow());
    let should_reveal = with_app(|st| {
        if st.smoke_test {
            return false;
        }
        // If Start Menu is already open, this second Win tap is dismissing Start — let foreground
        // handoff cleanly re-engage auto-hide / Gaming Mode instead of holding it open.
        if st.start_menu_open || fg_is_start {
            st.win_key_override_until = 0;
            return false;
        }
        st.start_menu_open = true;
        st.start_menu_arrived = false;
        st.win_key_override_until = GetTickCount64().saturating_add(1600);
        true
    })
    .unwrap_or(false);
    if should_reveal {
        reveal_taskbar_for_start_safe(None);
    }
}

/// Synchronizes per-monitor Native Windows Fullscreen Gaming Mode immediately on foreground/shell
/// changes and clock sweeps:
/// - While Start Menu / Windows shell flyout is active (or right after a `Win` key tap):
///   ALWAYS keeps the taskbar revealed on top — never suppressed by Gaming Mode or auto-hide!
/// - Entering fullscreen: instantly suppresses ALL taskbar surfaces (dock + status islands),
///   hides BOTH edge sensors so edge-hovers/clicks NEVER wake or pop anything, closes active
///   overlays on that monitor, and releases reserved work area.
/// - Leaving fullscreen: restores reserved work area, re-arms edge sensors (when auto-hide is on),
///   and glides non-auto-hide bars cleanly back in.
unsafe fn sync_fullscreen_state_safe() {
    let fg = GetForegroundWindow();
    let fg_is_shell_flyout = is_windows_shell_flyout(fg);
    let fg_hmon = if fg_is_shell_flyout && !fg.is_null() {
        Some(MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST) as isize)
    } else {
        None
    };
    let now_ms = GetTickCount64();

    enum StartPhase {
        Ignore,
        RevealFlyout(Option<isize>),
        HoldOpen,
        JustClosed { host: HWND, sched_dock: bool, sched_status: bool },
        Normal,
    }

    let Some((smoke, gaming_on, start_phase, mons)) = with_app(|st| {
        let phase = if st.smoke_test {
            StartPhase::Ignore
        } else if fg_is_shell_flyout {
            let was_open = st.start_menu_open;
            st.start_menu_open = true;
            st.start_menu_arrived = true;
            st.win_key_override_until = 0;
            if !was_open {
                StartPhase::RevealFlyout(fg_hmon)
            } else {
                StartPhase::HoldOpen
            }
        } else if now_ms < st.win_key_override_until {
            st.start_menu_open = true;
            StartPhase::HoldOpen
        } else if st.start_menu_open {
            st.start_menu_open = false;
            st.start_menu_arrived = false;
            st.win_key_override_until = 0;
            let sched_dock = st.cfg.autohide
                && !st.popup_open
                && !st.menu_open
                && !st.search_open
                && !st.preview_open
                && st.monitors.iter().all(|m| !m.mouse_inside);
            let sched_status = st.cfg.layout == LayoutMode::Win12Split
                && st.cfg.status_autohide
                && st.coucou_live.mode == 0
                && !st.popup_open
                && !st.menu_open
                && !st.search_open
                && !st.tray_fly_open
                && !st.coucou_hub_open
                && st.monitors.iter().all(|m| !m.status_mouse_inside);
            StartPhase::JustClosed { host: st.hwnd_host, sched_dock, sched_status }
        } else {
            StartPhase::Normal
        };
        let v: Vec<(usize, isize, bool)> = st
            .monitors
            .iter()
            .enumerate()
            .map(|(i, m)| (i, m.spec.hmon, m.fullscreen))
            .collect();
        (st.smoke_test, st.cfg.gaming_mode != GamingModePref::Off, phase, v)
    }) else {
        return;
    };
    if smoke || mons.is_empty() {
        return;
    }

    match start_phase {
        StartPhase::Ignore => return,
        StartPhase::RevealFlyout(hmon) => {
            reveal_taskbar_for_start_safe(hmon);
            return;
        }
        StartPhase::HoldOpen => {
            return;
        }
        StartPhase::JustClosed { host, sched_dock, sched_status } => {
            if !host.is_null() {
                if sched_dock {
                    SetTimer(host, TIMER_AUTOHIDE_HOLD, 350, None);
                }
                if sched_status {
                    SetTimer(host, TIMER_STATUS_HIDE, 350, None);
                }
            }
        }
        StartPhase::Normal => {}
    }

    let own_pid = std::process::id();
    let mut changes: Vec<(usize, bool)> = Vec::new();
    let mut any_fs_active = false;
    for (idx, hmon, prev_fs) in mons {
        let fs = gaming_on && detect_fullscreen_on_monitor(hmon, own_pid);
        if fs {
            any_fs_active = true;
        }
        if fs != prev_fs {
            changes.push((idx, fs));
        }
    }

    // Fast path: no state transition, just ensure no stray bar/sensor leaked onto a fullscreen monitor.
    if changes.is_empty() {
        if any_fs_active {
            let stray: Vec<HWND> = with_app(|st| {
                let mut out = Vec::new();
                for m in &st.monitors {
                    if m.fullscreen {
                        for hw in [m.hwnd_dock, m.hwnd_tl, m.hwnd_tc, m.hwnd_tr, m.hwnd_sensor, m.hwnd_status_sensor] {
                            if !hw.is_null() && IsWindowVisible(hw) != 0 {
                                out.push(hw);
                            }
                        }
                    }
                }
                out
            })
            .unwrap_or_default();
            for hw in stray {
                ShowWindow(hw, SW_HIDE);
            }
        }
        return;
    }

    struct LeaveRestore {
        idx: usize,
        split: bool,
        restore_dock: bool,
        restore_status: bool,
        rearm_dock_sensor: HWND,
        rearm_status_sensor: HWND,
    }

    let mut primary_fs_changed = false;
    let mut hide_hwnds: Vec<HWND> = Vec::new();
    let mut leave_list: Vec<LeaveRestore> = Vec::new();
    let mut host_to_quiet: HWND = null_mut();
    let mut close_search = false;
    let mut close_popup = false;
    let mut close_tray_fly = false;
    let mut close_coucou_hub = false;
    let mut close_preview = false;
    let mut hide_tip = false;
    let mut hide_coucou = false;

    with_app(|st| {
        let split = st.cfg.layout == LayoutMode::Win12Split;
        let docked_coucou = st.coucou_container_active();
        for (idx, fs) in changes {
            let Some(m) = st.monitors.get_mut(idx) else { continue };
            m.fullscreen = fs;
            if idx == 0 {
                primary_fs_changed = true;
            }
            if fs {
                host_to_quiet = st.hwnd_host;
                m.anim_spring.jump(0.0);
                m.status_spring.jump(0.0);
                m.mouse_inside = false;
                m.status_mouse_inside = false;
                for hw in [m.hwnd_dock, m.hwnd_tl, m.hwnd_tc, m.hwnd_tr, m.hwnd_sensor, m.hwnd_status_sensor] {
                    if !hw.is_null() {
                        hide_hwnds.push(hw);
                    }
                }
                if docked_coucou && m.spec.primary {
                    hide_coucou = true;
                    if st.coucou_live.hmon == m.spec.hmon {
                        st.coucou_live = CoucouLiveState::default();
                    }
                }
                if st.hot.is_some_and(|(mi, _, _)| mi == idx) {
                    st.hot = None;
                }
                if st.pressed.is_some_and(|(mi, _, _)| mi == idx) {
                    st.pressed = None;
                }
                if st.search_open && st.search_mon == idx {
                    close_search = true;
                    st.search_open = false;
                    st.search_spring.jump(0.0);
                    st.search_panel_spring.jump(0.0);
                    if !st.hwnd_search_bar.is_null() {
                        hide_hwnds.push(st.hwnd_search_bar);
                    }
                    if !st.hwnd_search_panel.is_null() {
                        hide_hwnds.push(st.hwnd_search_panel);
                    }
                }
                if st.popup_open && st.popup_mon == idx {
                    close_popup = true;
                }
                if st.tray_fly_open && st.tray_fly_mon == idx {
                    close_tray_fly = true;
                }
                if st.coucou_hub_open && st.coucou_hub_mon == idx {
                    close_coucou_hub = true;
                }
                if st.preview_open && st.preview_mon == idx {
                    close_preview = true;
                }
                hide_tip = true;
            } else {
                let restore_dock = !st.cfg.autohide;
                let restore_status = split && !st.cfg.status_autohide;
                let rearm_dock_sensor = if st.cfg.autohide { m.hwnd_sensor } else { null_mut() };
                let rearm_status_sensor = if split && st.cfg.status_autohide {
                    m.hwnd_status_sensor
                } else {
                    null_mut()
                };
                leave_list.push(LeaveRestore {
                    idx,
                    split,
                    restore_dock,
                    restore_status,
                    rearm_dock_sensor,
                    rearm_status_sensor,
                });
            }
        }
    });

    if !host_to_quiet.is_null() {
        KillTimer(host_to_quiet, TIMER_AUTOHIDE_HOLD);
        KillTimer(host_to_quiet, TIMER_STATUS_HIDE);
        KillTimer(host_to_quiet, TIMER_TIP);
        KillTimer(host_to_quiet, TIMER_PREVIEW_SHOW);
        KillTimer(host_to_quiet, TIMER_PREVIEW_HIDE);
        if close_search {
            KillTimer(host_to_quiet, TIMER_SEARCH_CARET);
        }
    }
    if hide_tip {
        hide_tooltip_safe();
    }
    if close_preview {
        close_preview_safe();
    }
    if close_popup {
        close_popup_safe();
    }
    if close_tray_fly {
        close_tray_fly_safe();
    }
    if close_coucou_hub {
        close_coucou_hub_safe();
    }
    for hw in hide_hwnds {
        if IsWindowVisible(hw) != 0 {
            ShowWindow(hw, SW_HIDE);
        }
    }
    if hide_coucou {
        send_coucou_json_async(r#"{"taskbar_cmd":"dock_hide"}"#.to_string());
    }
    if primary_fs_changed {
        sync_primary_work_area_safe();
    }
    for lr in leave_list {
        if !lr.rearm_dock_sensor.is_null() {
            ShowWindow(lr.rearm_dock_sensor, SW_SHOWNOACTIVATE);
        }
        if !lr.rearm_status_sensor.is_null() {
            ShowWindow(lr.rearm_status_sensor, SW_SHOWNOACTIVATE);
        }
        if lr.restore_dock {
            present_bar_safe(lr.idx, SurfaceKind::Dock);
            if !lr.split {
                present_bar_safe(lr.idx, SurfaceKind::TopLeft);
                present_bar_safe(lr.idx, SurfaceKind::TopCenter);
                present_bar_safe(lr.idx, SurfaceKind::TopRight);
            }
            start_mon_anim_safe(lr.idx, 1.0);
        }
        if lr.restore_status {
            present_bar_safe(lr.idx, SurfaceKind::TopLeft);
            present_bar_safe(lr.idx, SurfaceKind::TopCenter);
            present_bar_safe(lr.idx, SurfaceKind::TopRight);
            start_status_anim_safe(lr.idx, 1.0);
        }
    }
}

unsafe extern "system" fn win_event_hook(
    _: windows_sys::Win32::UI::Accessibility::HWINEVENTHOOK,
    ev: u32,
    w: HWND,
    id: i32,
    child: i32,
    _: u32,
    _: u32,
) {
    if id != 0 || child != 0 || w.is_null() {
        return;
    }
    with_app(|st| {
        if ev == 0x0003 {
            // EVENT_SYSTEM_FOREGROUND: stamp MRU immediately so fast clicks always know the last active window
            st.tracker.note_active(w);
            let fg_is_shell_flyout = is_windows_shell_flyout(w);
            if !fg_is_shell_flyout && st.start_menu_arrived {
                st.win_key_override_until = 0;
            }
            // If user clicked another window while an auto-hiding bar was open and the pointer is
            // no longer over the bar, clear stale hover flags and hide it right away!
            // Never auto-hide when Start Menu / Calendar / Quick Settings took foreground!
            let mut pt: POINT = zeroed();
            if !fg_is_shell_flyout && !st.start_menu_open && GetCursorPos(&mut pt) != 0 {
                let host = st.host_hwnd();
                if !host.is_null()
                    && !st.popup_open
                    && !st.menu_open
                    && !st.tray_fly_open
                    && !st.coucou_hub_open
                    && !st.search_open
                    && st.tray_drag.is_none()
                {
                    let mut own_pid = 0u32;
                    GetWindowThreadProcessId(w, &mut own_pid);
                    if own_pid != std::process::id() {
                        if st.cfg.layout == LayoutMode::Win12Split && st.cfg.status_autohide && st.coucou_live.mode == 0 {
                            let mut need_hide = false;
                            let coucou_live = st.coucou_live;
                            for m in &mut st.monitors {
                                if m.status_spring.target > 0.5 && !cursor_on_status_strip(pt, m, &st.cfg, coucou_live) {
                                    m.status_mouse_inside = false;
                                    need_hide = true;
                                }
                            }
                            if need_hide {
                                SetTimer(host, TIMER_STATUS_HIDE, 140, None);
                            }
                        }
                        if st.cfg.autohide && !st.preview_open {
                            let mut need_hide = false;
                            for m in &mut st.monitors {
                                if m.anim_spring.target > 0.5 && !cursor_on_dock_strip(pt, m, &st.cfg) {
                                    m.mouse_inside = false;
                                    need_hide = true;
                                }
                            }
                            if need_hide {
                                SetTimer(host, TIMER_AUTOHIDE_HOLD, 160, None);
                            }
                        }
                    }
                }
            }
        }
        if !st.event_dirty {
            let host = st.host_hwnd();
            if !host.is_null() {
                st.event_dirty = true;
                SetTimer(host, TIMER_EVENTS, 40, None);
            }
        }
    });
    if ev == 0x0003 {
        sync_fullscreen_state_safe();
        keep_tc_behind_coucou_safe();
    }
}

unsafe extern "system" fn tray_host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA {
        let cds = &*(lp as *const CopyDataStruct);
        if cds.dw_data == 1 && cds.cb_data >= 960 && !cds.lp_data.is_null() {
            let bytes = std::slice::from_raw_parts(cds.lp_data, cds.cb_data as usize);
            let changed = with_app(|st| {
                let scale = st.monitors.first().map_or(1.0, |m| m.spec.scale);
                st.tray.handle_notify_data(bytes, sc(16, scale))
            })
            .unwrap_or(false);
            if changed {
                refresh_tray_layout_safe();
            }
            return 1;
        }
        // Forward non-tray WM_COPYDATA (e.g. SHAppBarMessage) to Explorer's real Shell_TrayWnd
        let other = find_other_shell_tray(hwnd);
        if !other.is_null() {
            let mut res = 0usize;
            SendMessageTimeoutW(other, msg, wp, lp, SMTO_ABORTIFHUNG, 1500, &mut res);
            return res as LRESULT;
        }
        return 0;
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe extern "system" fn tray_fly_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &ps);
            0
        }
        WM_LBUTTONDOWN => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            let mut pt = POINT { x, y };
            ClientToScreen(hwnd, &mut pt);
            let captured = with_app(|st| {
                let lay = st.tray_fly_layout.as_ref()?;
                let cell_idx = hit_test_tray_flyout(lay, x, y)?;
                let cell = lay.cells.get(cell_idx)?;
                st.tray_drag = Some(TrayDragState {
                    origin: TrayDragOrigin::FromFlyout { cell_idx },
                    mon_idx: st.tray_fly_mon,
                    key: cell.key.clone(),
                    bitmap: cell.bitmap.clone(),
                    start_pt: pt,
                    cur_pt: pt,
                    active: false,
                    over_bar_slot: None,
                    over_hide: false,
                });
                Some(())
            })
            .flatten()
            .is_some();
            if captured {
                SetCapture(hwnd);
            }
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            let mut pt = POINT { x, y };
            ClientToScreen(hwnd, &mut pt);

            enum DragMove {
                NotDragging,
                Update {
                    mon_idx: usize,
                    pt: POINT,
                    bmp: Option<Arc<IconBitmap>>,
                    highlight: bool,
                    scale: f32,
                    repaint_bar: bool,
                    repaint_fly: bool,
                },
            }

            let step = with_app(|st| {
                let Some(mut d) = st.tray_drag.take() else {
                    return DragMove::NotDragging;
                };
                if !matches!(d.origin, TrayDragOrigin::FromFlyout { .. }) {
                    st.tray_drag = Some(d);
                    return DragMove::NotDragging;
                }
                let mon_idx = d.mon_idx;
                let scale = st.monitors.get(mon_idx).map_or(1.0, |m| m.spec.scale);
                let mut repaint_fly = false;
                if !d.active {
                    let dx = (pt.x - d.start_pt.x).abs();
                    let dy = (pt.y - d.start_pt.y).abs();
                    if dx.max(dy) >= sc(6, scale) {
                        d.active = true;
                        repaint_fly = true;
                    }
                }
                d.cur_pt = pt;
                if !d.active {
                    st.tray_drag = Some(d);
                    return DragMove::NotDragging;
                }
                let prev_slot = d.over_bar_slot;
                let mut over_slot = None;
                if let Some(m) = st.monitors.get(mon_idx)
                    && let Some(tr) = m.lay.top_right
                {
                    let pad = sc(24, scale);
                    if pt.x >= tr.left - pad && pt.x <= tr.right + pad && pt.y >= tr.top - pad && pt.y <= tr.bottom + pad {
                        let lx = pt.x - tr.left;
                        let mut slot = m.lay.tray_rects.len();
                        for (i, r) in m.lay.tray_rects.iter().enumerate() {
                            if lx < (r.left + r.right) / 2 {
                                slot = i;
                                break;
                            }
                        }
                        over_slot = Some(slot);
                    }
                }
                d.over_bar_slot = over_slot;
                let bmp = d.bitmap.clone();
                let highlight = over_slot.is_some();
                st.tray_drag = Some(d);
                DragMove::Update {
                    mon_idx,
                    pt,
                    bmp,
                    highlight,
                    scale,
                    repaint_bar: prev_slot != over_slot || repaint_fly,
                    repaint_fly,
                }
            })
            .unwrap_or(DragMove::NotDragging);

            if let DragMove::Update { mon_idx, pt, bmp, highlight, scale, repaint_bar, repaint_fly } = step {
                update_drag_ghost_safe(pt, bmp.as_deref(), highlight, scale);
                if repaint_bar {
                    present_bar_safe(mon_idx, SurfaceKind::TopRight);
                }
                if repaint_fly {
                    present_tray_fly_safe();
                }
                return 0;
            }

            let inv = with_app(|st| {
                let hit = st
                    .tray_fly_layout
                    .as_ref()
                    .and_then(|l| hit_test_tray_flyout(l, x, y));
                if st.tray_fly_hover != hit {
                    st.tray_fly_hover = hit;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
            if inv {
                present_tray_fly_safe();
            }
            0
        }
        WM_CAPTURECHANGED => {
            let mon_opt = with_app(|st| {
                if st.tray_drag.as_ref().is_some_and(|d| matches!(d.origin, TrayDragOrigin::FromFlyout { .. })) {
                    let d = st.tray_drag.take()?;
                    Some(d.mon_idx)
                } else {
                    None
                }
            })
            .flatten();
            if let Some(mi) = mon_opt {
                hide_drag_ghost_safe();
                present_bar_safe(mi, SurfaceKind::TopRight);
                present_tray_fly_safe();
            }
            0
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            enum FlyClick {
                None,
                DropToBar { key: String, slot: Option<usize>, mon_idx: usize },
                CancelDrag { mon_idx: usize },
                Notify { tray_idx: usize, left: bool },
                TogglePin { key: String, on_bar: bool, mon_idx: usize },
            }
            // IMPORTANT: Consume `tray_drag` BEFORE calling `ReleaseCapture()`!
            let act = with_app(|st| {
                if msg == WM_LBUTTONUP
                    && let Some(d) = st.tray_drag.take()
                    && matches!(d.origin, TrayDragOrigin::FromFlyout { .. })
                    && d.active
                {
                    if d.over_bar_slot.is_some() {
                        return Some(FlyClick::DropToBar {
                            key: d.key,
                            slot: d.over_bar_slot,
                            mon_idx: d.mon_idx,
                        });
                    } else {
                        return Some(FlyClick::CancelDrag { mon_idx: d.mon_idx });
                    }
                }
                let lay = st.tray_fly_layout.as_ref()?;
                let cell_idx = hit_test_tray_flyout(lay, x, y)?;
                let cell = lay.cells.get(cell_idx)?;
                if msg == WM_MBUTTONUP {
                    Some(FlyClick::TogglePin {
                        key: cell.key.clone(),
                        on_bar: cell.on_bar,
                        mon_idx: st.tray_fly_mon,
                    })
                } else {
                    Some(FlyClick::Notify {
                        tray_idx: cell.tray_idx,
                        left: msg == WM_LBUTTONUP,
                    })
                }
            })
            .flatten()
            .unwrap_or(FlyClick::None);

            if GetCapture() == hwnd {
                ReleaseCapture();
            }
            hide_drag_ghost_safe();

            match act {
                FlyClick::None => {}
                FlyClick::DropToBar { key, slot, mon_idx } => {
                    with_app(|st| {
                        let limit = st.cfg.tray_mode.limit();
                        st.tray.pin_on_bar_at(&key, limit, slot);
                        st.tray_fly_hover = None;
                        let _ = mon_idx;
                    });
                    refresh_tray_layout_safe();
                }
                FlyClick::CancelDrag { mon_idx } => {
                    present_bar_safe(mon_idx, SurfaceKind::TopRight);
                    present_tray_fly_safe();
                }
                FlyClick::Notify { tray_idx, left } => {
                    close_tray_fly_safe();
                    with_app(|st| st.tray.dispatch_click(tray_idx, left));
                }
                FlyClick::TogglePin { key, on_bar, mon_idx } => {
                    with_app(|st| {
                        let limit = st.cfg.tray_mode.limit();
                        st.tray.toggle_pin_on_bar(&key, on_bar, limit);
                        st.tray_fly_hover = None;
                        let _ = mon_idx;
                    });
                    refresh_tray_layout_safe();
                }
            }
            0
        }
        WM_ACTIVATE if (wp & 0xFFFF) == 0 => {
            let dragging = with_app(|st| st.tray_drag.is_some()).unwrap_or(false);
            if !dragging {
                close_tray_fly_safe();
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn find_owner(st: &AppState, hwnd: HWND) -> Option<(usize, SurfaceKind)> {
    for (idx, m) in st.monitors.iter().enumerate() {
        if let Some(k) = m.kind_for(hwnd) {
            return Some((idx, k));
        }
    }
    None
}

unsafe extern "system" fn host_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let (sh_msg, tb_msg) = with_app(|st| (st.shell_hook_msg, st.taskbar_created_msg)).unwrap_or((0, 0));
    if msg != 0 && msg == sh_msg {
        let target = lp as HWND;
        let is_flash = wp == 0x8006; // HSHELL_FLASH
        let is_clear = wp == 6 || wp == 4 || wp == 0x8004; // HSHELL_REDRAW | HSHELL_WINDOWACTIVATED | HSHELL_RUDEAPPACTIVATED
        if is_flash || is_clear {
            with_app(|st| {
                st.tracker.note_flash(target, is_flash);
                if (wp == 4 || wp == 0x8004) && !target.is_null() {
                    st.tracker.note_active(target);
                }
                if !st.event_dirty {
                    st.event_dirty = true;
                    SetTimer(hwnd, TIMER_EVENTS, 40, None);
                }
            });
            if wp == 4 || wp == 0x8004 {
                // HSHELL_WINDOWACTIVATED / HSHELL_RUDEAPPACTIVATED (fullscreen transition!)
                sync_fullscreen_state_safe();
            }
        }
        return 0;
    }
    if msg != 0 && msg == tb_msg {
        let (smoke, th) = with_app(|st| (st.smoke_test, st.hwnd_tray_host)).unwrap_or((false, null_mut()));
        if !smoke {
            hide_all_stock_taskbars();
            if !th.is_null() {
                SetWindowPos(th, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        return 0;
    }

    match msg {
        WM_APP_ICON_READY => {
            let (count, search_open) = with_app(|st| (st.monitors.len(), st.search_open)).unwrap_or((0, false));
            for idx in 0..count {
                present_bar_safe(idx, SurfaceKind::Dock);
            }
            if search_open {
                present_search_panel_safe();
            }
            0
        }
        WM_APP_TELEM_CHANGED => {
            let count = with_app(|st| st.monitors.len()).unwrap_or(0);
            for idx in 0..count {
                present_bar_safe(idx, SurfaceKind::TopLeft);
                present_bar_safe(idx, SurfaceKind::TopRight);
            }
            0
        }
        WM_APP_COUCOU_CHANGED => {
            sync_coucou_center_handoff_safe();
            let mut to_present: Vec<(usize, SurfaceKind)> = Vec::new();
            let mut refresh_hub = false;
            with_app(|st| {
                let telem = st.telem.snapshot();
                let coucou_label = st.coucou_pill_label();
                let tray_bar_count = st.tray.bar_indices(st.cfg.tray_mode.limit()).len();
                let has_tray_chevron = !st.tray.all_visible_indices().is_empty();
                for idx in 0..st.monitors.len() {
                    if !st.monitors[idx].spec.primary {
                        continue;
                    }
                    let pill_span = st.coucou_span_for(st.monitors[idx].spec.hmon);
                    st.monitors[idx].lay = compute_monitor_layout(
                        &st.monitors[idx].spec,
                        &st.cfg,
                        &st.sys,
                        st.monitors[idx].items.len(),
                        tray_bar_count,
                        has_tray_chevron,
                        !st.lang_code.is_empty(),
                        telem.bat_pct >= 0,
                        telem,
                        &st.workspace,
                        &st.split_clock_str,
                        &coucou_label,
                        pill_span,
                    );
                    if let Some(tc) = st.monitors[idx].lay.top_center {
                        st.monitors[idx].tc_x_spring.spring_to(tc.left as f32, MOTION_FOLLOW);
                        st.pacer.wake();
                    }
                    if !st.monitors[idx].hwnd_tc.is_null() {
                        to_present.push((idx, SurfaceKind::TopCenter));
                    }
                    if st.cfg.layout != LayoutMode::Win12Split && !st.monitors[idx].hwnd_tr.is_null() {
                        to_present.push((idx, SurfaceKind::TopRight));
                    }
                }
                if st.coucou_hub_open {
                    let mi = st.coucou_hub_mon;
                    let kind = if st.cfg.layout == LayoutMode::Win12Split {
                        SurfaceKind::TopCenter
                    } else {
                        SurfaceKind::TopRight
                    };
                    let (cx, _, base) = st
                        .monitors
                        .get(mi)
                        .map(|m| zone_anchor(kind, &m.lay, HitZone::Coucou))
                        .unwrap_or((0, 0, RECT { left: 0, top: 0, right: 0, bottom: 0 }));
                    rebuild_coucou_hub_layout(st, mi, cx, base);
                    refresh_hub = true;
                }
            });
            for (idx, kind) in to_present {
                present_bar_safe(idx, kind);
            }
            if refresh_hub {
                present_coucou_hub_safe();
            }
            0
        }
        WM_APP_COUCOU_EXPANDED => {
            // Back-compat fallback for Coucou <= 0.1.1.53 (only sends binary expanded/collapsed)
            let expanded = wp != 0;
            let live = if expanded && lp != 0 {
                CoucouLiveState { mode: 2, left: 0, right: 0, hmon: lp }
            } else {
                CoucouLiveState::default()
            };
            apply_coucou_live_state_safe(live);
            0
        }
        WM_APP_COUCOU_RECT => {
            let mode = ((wp >> 32) & 0xFF) as u8;
            let raw = lp as u64;
            let left = (raw & 0xFFFF_FFFF) as u32 as i32;
            let right = ((raw >> 32) & 0xFFFF_FFFF) as u32 as i32;
            let live = if mode == 0 {
                CoucouLiveState::default()
            } else {
                let cx = (left + right) / 2;
                let hmon = with_app(|st| {
                    st.monitors
                        .iter()
                        .find(|m| cx >= m.spec.rect.left && cx < m.spec.rect.right)
                        .or_else(|| st.monitors.first())
                        .map_or(0, |m| m.spec.hmon)
                })
                .unwrap_or(0);
                CoucouLiveState { mode, left, right, hmon }
            };
            apply_coucou_live_state_safe(live);
            0
        }
        WM_APP_OPEN_SEARCH => {
            toggle_search_bar_safe(0);
            0
        }
        WM_APP_ANIM_TICK => {
            step_vsync_springs_safe();
            0
        }
        WM_APP_WIN_KEY_TAP => {
            handle_win_key_tap_safe();
            0
        }
        WM_TIMER => {
            match wp {
                TIMER_CLOCK => {
                    let (smoke, show_sec) = with_app(|st| (st.smoke_test, st.cfg.show_seconds)).unwrap_or((false, false));
                    if !smoke {
                        hide_all_stock_taskbars();
                    }
                    let (t, d, sp) = format_clock_all(show_sec);
                    sync_fullscreen_state_safe();

                    let mut clock_redraw_count = 0usize;
                    let mut coucou_tick_redraw: Option<(usize, SurfaceKind, bool)> = None;
                    with_app(|st| {
                        st.coucou_anim_tick = st.coucou_anim_tick.wrapping_add(1);
                        let clock_changed = t != st.time_str || d != st.date_str || sp != st.split_clock_str;
                        if clock_changed {
                            st.time_str = t;
                            st.date_str = d;
                            st.split_clock_str = sp;
                            clock_redraw_count = st.monitors.len();
                        } else if st.cfg.show_coucou
                            && let Some(pi) = st.monitors.iter().position(|m| m.spec.primary)
                        {
                            let kind = if st.cfg.layout == LayoutMode::Win12Split {
                                SurfaceKind::TopCenter
                            } else {
                                SurfaceKind::TopRight
                            };
                            coucou_tick_redraw = Some((pi, kind, st.coucou_hub_open));
                        }
                    });
                    let tray_pruned = with_app(|st| st.tray.prune_dead()).unwrap_or(false);
                    if tray_pruned {
                        refresh_tray_layout_safe();
                    }
                    for idx in 0..clock_redraw_count {
                        present_bar_safe(idx, SurfaceKind::TopCenter);
                        present_bar_safe(idx, SurfaceKind::TopRight);
                    }
                    if let Some((pi, kind, hub_open)) = coucou_tick_redraw {
                        present_bar_safe(pi, kind);
                        if hub_open {
                            present_coucou_hub_safe();
                        }
                    }
                    // Safety sweep #1: if Coucou exited or collapsed silently, restore normal centered TopCenter;
                    // while Coucou's live pill is active (`mode == 1`), keep TopCenter strictly behind it in Z-order.
                    if !smoke {
                        let live_mode = with_app(|st| st.coucou_live.mode).unwrap_or(0);
                        if live_mode != 0 && !coucou_window_visible() {
                            apply_coucou_live_state_safe(CoucouLiveState::default());
                        } else if live_mode == 1 {
                            keep_tc_behind_coucou_safe();
                        }
                    }
                    // Safety sweep #2: auto-hide any stuck status strip or dock when cursor has left it
                    let mut pt: POINT = zeroed();
                    if !smoke && GetCursorPos(&mut pt) != 0 {
                        let (hide_status, hide_dock) = with_app(|st| {
                            if st.popup_open
                                || st.menu_open
                                || st.start_menu_open
                                || st.tray_fly_open
                                || st.coucou_hub_open
                                || st.search_open
                                || st.tray_drag.is_some()
                            {
                                return (Vec::new(), Vec::new());
                            }
                            let mut hs = Vec::new();
                            let mut hd = Vec::new();
                            if st.cfg.layout == LayoutMode::Win12Split && st.cfg.status_autohide && st.coucou_live.mode == 0 {
                                let coucou_live = st.coucou_live;
                                for (i, m) in st.monitors.iter_mut().enumerate() {
                                    if m.status_spring.target > 0.5 && !cursor_on_status_strip(pt, m, &st.cfg, coucou_live) {
                                        m.status_mouse_inside = false;
                                        hs.push(i);
                                    }
                                }
                            }
                            if st.cfg.autohide && !st.preview_open {
                                for (i, m) in st.monitors.iter_mut().enumerate() {
                                    if m.anim_spring.target > 0.5 && !cursor_on_dock_strip(pt, m, &st.cfg) {
                                        m.mouse_inside = false;
                                        hd.push(i);
                                    }
                                }
                            }
                            (hs, hd)
                        })
                        .unwrap_or_default();
                        for idx in hide_status {
                            start_status_anim_safe(idx, 0.0);
                        }
                        for idx in hide_dock {
                            start_mon_anim_safe(idx, 0.0);
                        }
                    }
                }
                TIMER_EVENTS => {
                    KillTimer(hwnd, TIMER_EVENTS);
                    with_app(|st| st.event_dirty = false);
                    refresh_windows_all_safe();
                    sync_fullscreen_state_safe();
                }
                TIMER_TIP => {
                    KillTimer(hwnd, TIMER_TIP);
                    let target = with_app(|st| {
                        if let Some((ti, tk, tz)) = st.tip_target
                            && st.hot == Some((ti, tk, tz))
                            && tz != HitZone::None
                        {
                            Some((ti, tk, tz))
                        } else {
                            None
                        }
                    })
                    .flatten();
                    if let Some((ti, tk, tz)) = target {
                        show_tooltip_safe(ti, tk, tz);
                    }
                }
                TIMER_AUTOHIDE_HOLD => {
                    KillTimer(hwnd, TIMER_AUTOHIDE_HOLD);
                    let hide_idxs = with_app(|st| {
                        if st.cfg.autohide
                            && !st.popup_open
                            && !st.menu_open
                            && !st.start_menu_open
                            && !st.preview_open
                            && !st.tray_fly_open
                            && !st.coucou_hub_open
                            && !st.search_open
                            && st.tray_drag.is_none()
                        {
                            st.monitors
                                .iter()
                                .enumerate()
                                .filter(|(_, m)| !m.mouse_inside)
                                .map(|(i, _)| i)
                                .collect::<Vec<usize>>()
                        } else {
                            Vec::new()
                        }
                    })
                    .unwrap_or_default();
                    for idx in hide_idxs {
                        start_mon_anim_safe(idx, 0.0);
                    }
                }
                TIMER_STATUS_HIDE => {
                    KillTimer(hwnd, TIMER_STATUS_HIDE);
                    let hide_idxs = with_app(|st| {
                        if st.cfg.layout == LayoutMode::Win12Split
                            && st.cfg.status_autohide
                            && st.coucou_live.mode == 0
                            && !st.popup_open
                            && !st.menu_open
                            && !st.start_menu_open
                            && !st.tray_fly_open
                            && !st.coucou_hub_open
                            && !st.search_open
                            && st.tray_drag.is_none()
                        {
                            st.monitors
                                .iter()
                                .enumerate()
                                .filter(|(_, m)| !m.status_mouse_inside)
                                .map(|(i, _)| i)
                                .collect::<Vec<usize>>()
                        } else {
                            Vec::new()
                        }
                    })
                    .unwrap_or_default();
                    for idx in hide_idxs {
                        start_status_anim_safe(idx, 0.0);
                    }
                }
                TIMER_SEARCH_CARET => {
                    let open = with_app(|st| {
                        if st.search_open && st.search_spring.target > 0.5 {
                            st.search_caret_on = !st.search_caret_on;
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                    if open {
                        present_search_bar_safe();
                    } else {
                        KillTimer(hwnd, TIMER_SEARCH_CARET);
                    }
                }
                TIMER_PREVIEW_SHOW => {
                    KillTimer(hwnd, TIMER_PREVIEW_SHOW);
                    let target = with_app(|st| {
                        let (mi, ai) = st.preview_target?;
                        if st.hot == Some((mi, SurfaceKind::Dock, HitZone::App(ai))) {
                            Some((mi, ai))
                        } else {
                            None
                        }
                    })
                    .flatten();
                    if let Some((mi, ai)) = target {
                        show_preview_safe(mi, ai);
                    }
                }
                TIMER_PREVIEW_HIDE => {
                    KillTimer(hwnd, TIMER_PREVIEW_HIDE);
                    let keep = with_app(|st| {
                        st.preview_tracking
                            || matches!(st.hot, Some((_, SurfaceKind::Dock, HitZone::App(_))))
                    })
                    .unwrap_or(false);
                    if !keep {
                        close_preview_safe();
                    }
                }
                TIMER_SMOKE_DONE => {
                    KillTimer(hwnd, TIMER_SMOKE_DONE);
                    write_smoke_report();
                    PostMessageW(hwnd, WM_CLOSE, 0, 0);
                }
                _ => {}
            }
            0
        }
        WM_DISPLAYCHANGE => {
            rebuild_monitors_safe();
            0
        }
        WM_SETTINGCHANGE | WM_DWMCOLORIZATIONCOLORCHANGED => {
            handle_system_prefs_changed();
            0
        }
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn surface_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_WINDOWPOSCHANGING => {
            let wp_ptr = lp as *mut WINDOWPOS;
            if !wp_ptr.is_null() {
                let is_tc_live = with_app(|st| {
                    find_owner(st, hwnd).is_some_and(|(idx, kind)| {
                        kind == SurfaceKind::TopCenter
                            && st.coucou_live.mode == 1
                            && st.coucou_live.hmon == st.monitors[idx].spec.hmon
                    })
                })
                .unwrap_or(false);
                if is_tc_live {
                    let coucou_hw = coucou_window_hwnd();
                    if !coucou_hw.is_null() {
                        (*wp_ptr).hwndInsertAfter = coucou_hw;
                        (*wp_ptr).flags &= !SWP_NOZORDER;
                    }
                }
            }
            0
        }
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &ps);
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            // 1. Live drag-to-reorder on the Dock
            enum DragStep {
                NotDragging,
                Moved { mon_idx: usize, just_started: bool },
            }
            let drag_step = with_app(|st| {
                let Some(mut d) = st.drag else { return DragStep::NotDragging };
                let Some((idx, SurfaceKind::Dock)) = find_owner(st, hwnd) else {
                    return DragStep::NotDragging;
                };
                if idx != d.mon_idx {
                    return DragStep::NotDragging;
                }
                let scale = st.monitors[idx].spec.scale;
                let mut just_started = false;
                if !d.active && (x - d.start_x).abs() >= sc(7, scale) {
                    d.active = true;
                    just_started = true;
                    st.pressed = None;
                }
                d.cur_x = x;
                if d.active {
                    let count = st.monitors[idx].items.len();
                    if count > 1 {
                        let lay = &st.monitors[idx].lay;
                        let step = (lay.btn_w + lay.btn_gap).max(1);
                        let target = ((x - lay.apps_x0) / step).clamp(0, count as i32 - 1) as usize;
                        if target != d.cur_idx && d.cur_idx < count {
                            let moved = st.monitors[idx].items.remove(d.cur_idx);
                            st.monitors[idx].items.insert(target, moved);
                            d.cur_idx = target;
                            // Sync custom_order from this monitor's new sequence + remaining global items
                            let mut order: Vec<String> =
                                st.monitors[idx].items.iter().map(|it| it.icon_key.clone()).collect();
                            for it in &st.all_items {
                                if !order.contains(&it.icon_key) {
                                    order.push(it.icon_key.clone());
                                }
                            }
                            st.custom_order = order;
                            apply_custom_order(&mut st.all_items, &st.custom_order);
                        }
                    }
                    st.drag = Some(d);
                    return DragStep::Moved { mon_idx: idx, just_started };
                }
                st.drag = Some(d);
                DragStep::NotDragging
            })
            .unwrap_or(DragStep::NotDragging);

            if let DragStep::Moved { mon_idx, just_started } = drag_step {
                if just_started {
                    hide_tooltip_safe();
                    close_preview_safe();
                }
                present_bar_safe(mon_idx, SurfaceKind::Dock);
                return 0;
            }

            // 1b. Live tray icon drag on TopRight (reorder on main bar OR drag off/to chevron to hide!)
            enum BarTrayDrag {
                NotDragging,
                Moved {
                    mon_idx: usize,
                    pt: POINT,
                    bmp: Option<Arc<IconBitmap>>,
                    over_hide: bool,
                    open_fly_for_drop: bool,
                    scale: f32,
                    just_started: bool,
                    repaint_bar: bool,
                    repaint_fly: bool,
                },
            }
            let mut scr_pt = POINT { x, y };
            ClientToScreen(hwnd, &mut scr_pt);
            let bar_td = with_app(|st| {
                let Some(mut d) = st.tray_drag.take() else {
                    return BarTrayDrag::NotDragging;
                };
                let TrayDragOrigin::FromBar { mut slot } = d.origin else {
                    st.tray_drag = Some(d);
                    return BarTrayDrag::NotDragging;
                };
                let Some((idx, SurfaceKind::TopRight)) = find_owner(st, hwnd) else {
                    st.tray_drag = Some(d);
                    return BarTrayDrag::NotDragging;
                };
                let scale = st.monitors[idx].spec.scale;
                let mut just_started = false;
                if !d.active {
                    let dx = (scr_pt.x - d.start_pt.x).abs();
                    let dy = (scr_pt.y - d.start_pt.y).abs();
                    if dx.max(dy) >= sc(6, scale) {
                        d.active = true;
                        just_started = true;
                        st.pressed = None;
                    }
                }
                d.cur_pt = scr_pt;
                if !d.active {
                    st.tray_drag = Some(d);
                    return BarTrayDrag::NotDragging;
                }
                let prev_hide = d.over_hide;
                let tr = st.monitors[idx].lay.top_right.unwrap_or(st.monitors[idx].spec.rect);
                let in_chev = st.monitors[idx].lay.tray_chevron_rect.is_some_and(|rc| {
                    x >= rc.left - sc(4, scale) && x <= rc.right + sc(4, scale) && y >= rc.top - sc(8, scale) && y <= rc.bottom + sc(8, scale)
                });
                let in_fly = st.tray_fly_open
                    && st.tray_fly_layout.as_ref().is_some_and(|fl| {
                        let wr = fl.win_rect;
                        scr_pt.x >= wr.left && scr_pt.x <= wr.right && scr_pt.y >= wr.top && scr_pt.y <= wr.bottom
                    });
                let off_vertically = scr_pt.y < tr.top - sc(18, scale) || scr_pt.y > tr.bottom + sc(18, scale);
                let over_hide = in_chev || in_fly || off_vertically;
                d.over_hide = over_hide;
                let mut reordered = false;
                if !over_hide && !st.monitors[idx].lay.tray_rects.is_empty() {
                    let mut target = slot;
                    for (i, r) in st.monitors[idx].lay.tray_rects.iter().enumerate() {
                        if x >= r.left - sc(2, scale) && x <= r.right + sc(2, scale) {
                            target = i;
                            break;
                        }
                    }
                    let limit = st.cfg.tray_mode.limit();
                    if target != slot && st.tray.reorder_on_bar(slot, target, limit) {
                        slot = target;
                        d.origin = TrayDragOrigin::FromBar { slot };
                        reordered = true;
                    }
                }
                let open_fly_for_drop = over_hide && !st.tray_fly_open;
                let bmp = d.bitmap.clone();
                let repaint_fly = st.tray_fly_open && (prev_hide != over_hide || just_started);
                st.tray_drag = Some(d);
                BarTrayDrag::Moved {
                    mon_idx: idx,
                    pt: scr_pt,
                    bmp,
                    over_hide,
                    open_fly_for_drop,
                    scale,
                    just_started,
                    repaint_bar: just_started || reordered || prev_hide != over_hide,
                    repaint_fly,
                }
            })
            .unwrap_or(BarTrayDrag::NotDragging);

            if let BarTrayDrag::Moved {
                mon_idx,
                pt,
                bmp,
                over_hide,
                open_fly_for_drop,
                scale,
                just_started,
                repaint_bar,
                repaint_fly,
            } = bar_td
            {
                if just_started {
                    hide_tooltip_safe();
                }
                if open_fly_for_drop {
                    open_tray_fly_for_drop_safe(mon_idx);
                }
                update_drag_ghost_safe(pt, bmp.as_deref(), over_hide, scale);
                if repaint_bar {
                    present_bar_safe(mon_idx, SurfaceKind::TopRight);
                }
                if repaint_fly {
                    present_tray_fly_safe();
                }
                return 0;
            }

            // 2. Normal hover, tooltip & live DWM window preview
            enum HoverAct {
                None,
                HideAll,
                TipNow(usize, SurfaceKind, HitZone),
                TipDelay,
                PreviewNow(usize, usize),
                PreviewDelay,
            }
            let (host, need_track, inv_prev, inv_cur, hover_act) = with_app(|st| {
                let Some((idx, kind)) = find_owner(st, hwnd) else {
                    return (null_mut(), false, null_mut(), false, HoverAct::None);
                };
                if kind == SurfaceKind::Dock || st.cfg.layout != LayoutMode::Win12Split {
                    st.monitors[idx].mouse_inside = true;
                } else {
                    st.monitors[idx].status_mouse_inside = true;
                }
                let host = st.host_hwnd();
                let need_track = if st.tracking_hwnd != hwnd {
                    st.tracking_hwnd = hwnd;
                    true
                } else {
                    false
                };
                let z = hit_test(kind, &st.monitors[idx].lay, x, y, st.monitors[idx].items.len());
                let cur = Some((idx, kind, z));
                let mut inv_prev = null_mut();
                let mut inv_cur = false;
                let mut hover_act = HoverAct::None;
                if st.hot != cur {
                    let prev = st.hot;
                    st.hot = cur;
                    if let Some((pi, pk, _)) = prev
                        && (pi, pk) != (idx, kind)
                        && let Some(pm) = st.monitors.get(pi)
                    {
                        inv_prev = match pk {
                            SurfaceKind::TopLeft => pm.hwnd_tl,
                            SurfaceKind::TopCenter => pm.hwnd_tc,
                            SurfaceKind::TopRight => pm.hwnd_tr,
                            SurfaceKind::Dock => pm.hwnd_dock,
                        };
                    }
                    inv_cur = true;
                    let tip_active = (!st.hwnd_tip.is_null() && IsWindowVisible(st.hwnd_tip) != 0)
                        || GetTickCount64().saturating_sub(st.tip_last_closed_ms) < 350;
                    let prev_active = st.preview_open
                        || GetTickCount64().saturating_sub(st.preview_last_closed_ms) < 350;

                    if z == HitZone::None {
                        hover_act = HoverAct::HideAll;
                    } else if st.cfg.show_previews
                        && kind == SurfaceKind::Dock
                        && let HitZone::App(ai) = z
                        && st.monitors[idx].items.get(ai).is_some_and(|it| !it.wins.is_empty())
                    {
                        st.preview_target = Some((idx, ai));
                        hover_act = if prev_active {
                            HoverAct::PreviewNow(idx, ai)
                        } else {
                            HoverAct::PreviewDelay
                        };
                    } else if tip_active {
                        hover_act = HoverAct::TipNow(idx, kind, z);
                    } else {
                        st.tip_target = cur;
                        hover_act = HoverAct::TipDelay;
                    }
                }
                (host, need_track, inv_prev, inv_cur, hover_act)
            })
            .unwrap_or((null_mut(), false, null_mut(), false, HoverAct::None));

            if !host.is_null() {
                KillTimer(host, TIMER_AUTOHIDE_HOLD);
                KillTimer(host, TIMER_STATUS_HIDE);
                if matches!(hover_act, HoverAct::PreviewNow(_, _) | HoverAct::PreviewDelay) {
                    KillTimer(host, TIMER_PREVIEW_HIDE);
                }
            }
            if need_track {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tme);
            }
            if !inv_prev.is_null() {
                present_bar_hwnd_safe(inv_prev);
            }
            if inv_cur {
                present_bar_hwnd_safe(hwnd);
            }
            match hover_act {
                HoverAct::None => {}
                HoverAct::HideAll => {
                    hide_tooltip_safe();
                    if !host.is_null() {
                        SetTimer(host, TIMER_PREVIEW_HIDE, 240, None);
                    }
                }
                HoverAct::TipNow(ti, tk, tz) => {
                    close_preview_safe();
                    show_tooltip_safe(ti, tk, tz);
                }
                HoverAct::TipDelay => {
                    hide_tooltip_safe();
                    if !host.is_null() {
                        SetTimer(host, TIMER_PREVIEW_HIDE, 220, None);
                        SetTimer(host, TIMER_TIP, 380, None);
                    }
                }
                HoverAct::PreviewNow(mi, ai) => show_preview_safe(mi, ai),
                HoverAct::PreviewDelay => {
                    hide_tooltip_safe();
                    if !host.is_null() {
                        SetTimer(host, TIMER_PREVIEW_SHOW, 260, None);
                    }
                }
            }
            0
        }
        WM_MOUSELEAVE_EVT => {
            let (inv, autohide_host, status_hide_host, preview_host) = with_app(|st| {
                if st.tracking_hwnd == hwnd {
                    st.tracking_hwnd = null_mut();
                }
                let Some((idx, kind)) = find_owner(st, hwnd) else {
                    return (false, null_mut(), null_mut(), null_mut());
                };
                if kind == SurfaceKind::Dock || st.cfg.layout != LayoutMode::Win12Split {
                    st.monitors[idx].mouse_inside = false;
                } else {
                    st.monitors[idx].status_mouse_inside = false;
                }
                let mut inv = false;
                if st.hot.is_some_and(|(hi, hk, _)| hi == idx && hk == kind) {
                    st.hot = None;
                    st.pressed = None;
                    inv = true;
                }
                let ah = if st.cfg.autohide && !st.popup_open && !st.menu_open && !st.coucou_hub_open && !st.search_open {
                    st.host_hwnd()
                } else {
                    null_mut()
                };
                let sh = if kind != SurfaceKind::Dock
                    && st.cfg.layout == LayoutMode::Win12Split
                    && st.cfg.status_autohide
                    && !st.popup_open
                    && !st.menu_open
                    && !st.coucou_hub_open
                    && !st.search_open
                {
                    st.host_hwnd()
                } else {
                    null_mut()
                };
                let ph = if st.preview_open || st.preview_target.is_some() { st.host_hwnd() } else { null_mut() };
                (inv, ah, sh, ph)
            })
            .unwrap_or((false, null_mut(), null_mut(), null_mut()));
            if inv {
                hide_tooltip_safe();
                present_bar_hwnd_safe(hwnd);
            }
            if !preview_host.is_null() {
                KillTimer(preview_host, TIMER_PREVIEW_SHOW);
                SetTimer(preview_host, TIMER_PREVIEW_HIDE, 280, None);
            }
            if !autohide_host.is_null() {
                SetTimer(autohide_host, TIMER_AUTOHIDE_HOLD, 650, None);
            }
            if !status_hide_host.is_null() {
                SetTimer(status_hide_host, TIMER_STATUS_HIDE, 700, None);
            }
            0
        }
        WM_MOUSEWHEEL => {
            let is_tr = with_app(|st| matches!(find_owner(st, hwnd), Some((_, SurfaceKind::TopRight)))).unwrap_or(false);
            if is_tr {
                let delta = (wp >> 16) as i16;
                send_chord(&[if delta > 0 { 0xAF } else { 0xAE }]);
            }
            0
        }
        WM_LBUTTONDOWN => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            let mut scr_pt = POINT { x, y };
            ClientToScreen(hwnd, &mut scr_pt);
            hide_tooltip_safe();
            close_preview_safe();
            let capture = with_app(|st| {
                if let Some((idx, kind)) = find_owner(st, hwnd) {
                    let z = hit_test(kind, &st.monitors[idx].lay, x, y, st.monitors[idx].items.len());
                    if z != HitZone::None {
                        st.pressed = Some((idx, kind, z));
                        if kind == SurfaceKind::Dock
                            && let HitZone::App(ai) = z
                        {
                            st.drag = Some(DragState {
                                mon_idx: idx,
                                start_x: x,
                                cur_x: x,
                                cur_idx: ai,
                                active: false,
                            });
                        } else if kind == SurfaceKind::TopRight
                            && let HitZone::TrayIcon(slot) = z
                        {
                            let bar = st.tray.bar_indices(st.cfg.tray_mode.limit());
                            if let Some(&ti) = bar.get(slot)
                                && let Some(entry) = st.tray.icons.get(ti)
                            {
                                st.tray_drag = Some(TrayDragState {
                                    origin: TrayDragOrigin::FromBar { slot },
                                    mon_idx: idx,
                                    key: entry.key.clone(),
                                    bitmap: entry.bitmap.clone(),
                                    start_pt: scr_pt,
                                    cur_pt: scr_pt,
                                    active: false,
                                    over_bar_slot: None,
                                    over_hide: false,
                                });
                            }
                        }
                        return true;
                    }
                }
                false
            })
            .unwrap_or(false);
            if capture {
                SetCapture(hwnd);
                present_bar_hwnd_safe(hwnd);
            }
            0
        }
        WM_CAPTURECHANGED => {
            let (order_to_save, had_tray_drag) = with_app(|st| {
                let was_active = st.drag.take().is_some_and(|d| d.active);
                let td = st
                    .tray_drag
                    .take()
                    .is_some_and(|d| d.active && matches!(d.origin, TrayDragOrigin::FromBar { .. }));
                st.pressed = None;
                (was_active.then(|| st.custom_order.clone()), td)
            })
            .unwrap_or((None, false));
            if had_tray_drag {
                hide_drag_ghost_safe();
                refresh_tray_layout_safe();
            }
            if let Some(ord) = order_to_save {
                save_custom_order(&ord);
                present_all_safe();
            }
            0
        }
        WM_LBUTTONUP => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            enum TrayDragEnd {
                None,
                HideToOverflow { key: String },
                SaveReorder,
            }
            enum ClickAct {
                None,
                ClosePopup,
                TogglePopup(usize, RECT),
                ToggleTrayFly(usize, i32, RECT),
                ToggleCoucouHub(usize, i32, RECT),
                TrayIcon(usize),
                InputLang,
                Workspace,
                Start,
                Search(usize),
                TaskView,
                App(AppItem),
                QuickSettings,
                Clock,
                ShowDesktop,
            }
            // IMPORTANT: Consume `st.drag`, `st.tray_drag`, and `st.pressed` BEFORE calling `ReleaseCapture()`,
            // because `ReleaseCapture()` synchronously dispatches `WM_CAPTURECHANGED` which clears `st.pressed`!
            let (drag_committed, tray_drag_end, act) = with_app(|st| {
                let was_active = st.drag.take().is_some_and(|d| d.active);
                let td = st.tray_drag.take();
                let pressed = st.pressed.take();
                if was_active {
                    return (Some(st.custom_order.clone()), TrayDragEnd::None, ClickAct::None);
                }
                if let Some(d) = td
                    && d.active
                    && matches!(d.origin, TrayDragOrigin::FromBar { .. })
                {
                    let end = if d.over_hide {
                        TrayDragEnd::HideToOverflow { key: d.key }
                    } else {
                        TrayDragEnd::SaveReorder
                    };
                    return (None, end, ClickAct::None);
                }
                let Some((idx, kind)) = find_owner(st, hwnd) else {
                    return (None, TrayDragEnd::None, ClickAct::None);
                };
                let z = hit_test(kind, &st.monitors[idx].lay, x, y, st.monitors[idx].items.len());
                if pressed != Some((idx, kind, z)) {
                    return (None, TrayDragEnd::None, ClickAct::None);
                }
                let click = match z {
                    HitZone::Gear => {
                        let rect = st.monitors[idx].lay.rect_for(kind).unwrap_or(st.monitors[idx].spec.rect);
                        ClickAct::TogglePopup(idx, rect)
                    }
                    HitZone::TrayChevron => {
                        let (cx, _, base) = zone_anchor(kind, &st.monitors[idx].lay, HitZone::TrayChevron);
                        ClickAct::ToggleTrayFly(idx, cx, base)
                    }
                    HitZone::Coucou => {
                        let (cx, _, base) = zone_anchor(kind, &st.monitors[idx].lay, HitZone::Coucou);
                        ClickAct::ToggleCoucouHub(idx, cx, base)
                    }
                    HitZone::TrayIcon(slot) => {
                        let bar = st.tray.bar_indices(st.cfg.tray_mode.limit());
                        bar.get(slot).copied().map_or(ClickAct::None, ClickAct::TrayIcon)
                    }
                    HitZone::InputLang => ClickAct::InputLang,
                    HitZone::Workspace => ClickAct::Workspace,
                    HitZone::Start => ClickAct::Start,
                    HitZone::Search => ClickAct::Search(idx),
                    HitZone::TaskView => ClickAct::TaskView,
                    HitZone::App(i) => st.monitors[idx].items.get(i).cloned().map_or(ClickAct::None, ClickAct::App),
                    HitZone::Network | HitZone::Volume | HitZone::Battery => ClickAct::QuickSettings,
                    HitZone::Clock => ClickAct::Clock,
                    HitZone::ShowDesktop => ClickAct::ShowDesktop,
                    HitZone::None => ClickAct::ClosePopup,
                };
                (None, TrayDragEnd::None, click)
            })
            .unwrap_or((None, TrayDragEnd::None, ClickAct::None));

            if GetCapture() == hwnd {
                ReleaseCapture();
            }
            hide_drag_ghost_safe();
            if let Some(ord) = drag_committed {
                save_custom_order(&ord);
                present_all_safe();
                return 0;
            }
            match tray_drag_end {
                TrayDragEnd::HideToOverflow { key } => {
                    with_app(|st| {
                        let limit = st.cfg.tray_mode.limit();
                        st.tray.hide_to_overflow(&key, limit);
                    });
                    refresh_tray_layout_safe();
                    return 0;
                }
                TrayDragEnd::SaveReorder => {
                    with_app(|st| st.tray.save_prefs());
                    refresh_tray_layout_safe();
                    return 0;
                }
                TrayDragEnd::None => {}
            }
            present_bar_hwnd_safe(hwnd);

            match act {
                ClickAct::None => {}
                ClickAct::ClosePopup => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                }
                ClickAct::TogglePopup(idx, rect) => {
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    toggle_popup_safe(idx, rect);
                }
                ClickAct::ToggleTrayFly(idx, cx, rect) => toggle_tray_fly_safe(idx, cx, rect),
                ClickAct::ToggleCoucouHub(idx, cx, rect) => {
                    // Left-clicking the Coucou module slot expands/toggles the REAL Coucou process
                    // docked right over our center container (or launches & waits for it if offline).
                    // Shift+Click still opens the quick native Hub if desired.
                    let shift = GetKeyState(VK_SHIFT as i32) < 0;
                    if shift {
                        toggle_coucou_hub_safe(idx, cx, rect);
                    } else {
                        close_popup_safe();
                        close_tray_fly_safe();
                        close_coucou_hub_safe();
                        let action = with_app(|st| {
                            let snap = st.coucou.snapshot();
                            if snap.latest_notice_ms > st.coucou_seen_notice_ms {
                                st.coucou_seen_notice_ms = snap.latest_notice_ms;
                            }
                            if snap.running {
                                Some(coucou_dock_cmd_payload(&st.cfg, "dock_toggle"))
                            } else {
                                st.coucou_launching = true;
                                None
                            }
                        })
                        .flatten();
                        present_bar_safe(idx, SurfaceKind::TopCenter);
                        present_bar_safe(idx, SurfaceKind::TopRight);
                        if let Some(payload) = action {
                            send_coucou_json_async(payload);
                        } else {
                            launch_coucou();
                        }
                    }
                }
                ClickAct::TrayIcon(tray_idx) => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    with_app(|st| st.tray.dispatch_click(tray_idx, true));
                }
                ClickAct::InputLang => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    send_chord(&[0x5B, 0x20]);
                }
                ClickAct::Workspace => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    launch_boost_ui();
                }
                ClickAct::Start => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    send_chord(&[0x5B]);
                }
                ClickAct::Search(idx) => {
                    toggle_search_bar_safe(idx);
                }
                ClickAct::TaskView => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    send_chord(&[0x5B, 0x09]);
                }
                ClickAct::App(item) => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    let shift = GetKeyState(VK_SHIFT as i32) < 0;
                    let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
                    if shift && ctrl {
                        launch_as_admin(&item);
                    } else if shift {
                        launch_new_instance(&item);
                    } else {
                        activate_or_toggle(&item);
                    }
                }
                ClickAct::QuickSettings => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    send_chord(&[0x5B, b'A' as u16]);
                }
                ClickAct::Clock => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    send_chord(&[0x5B, b'N' as u16]);
                    keep_tc_behind_coucou_safe();
                }
                ClickAct::ShowDesktop => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    send_chord(&[0x5B, b'D' as u16]);
                }
            }
            0
        }
        WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            hide_tooltip_safe();
            close_preview_safe();
            enum RAct {
                None,
                TaskMgr,
                WinX,
                MuteToggle,
                OpenUri(&'static str),
                CoucouMiddle,
                CoucouRight(Option<usize>),
                TrayRightClick(usize),
                TrayTogglePin(String),
                NewInstance(AppItem),
                AppMenu(AppItem),
                TaskbarMenu(usize, RECT),
            }
            let act = with_app(|st| {
                let Some((idx, kind)) = find_owner(st, hwnd) else { return RAct::None };
                let z = hit_test(kind, &st.monitors[idx].lay, x, y, st.monitors[idx].items.len());
                let rect = st.monitors[idx].lay.rect_for(kind).unwrap_or(st.monitors[idx].spec.rect);
                match z {
                    HitZone::Workspace if msg == WM_RBUTTONDOWN => RAct::TaskMgr,
                    HitZone::Start if msg == WM_RBUTTONDOWN => RAct::WinX,
                    HitZone::Coucou if msg == WM_MBUTTONDOWN => RAct::CoucouMiddle,
                    HitZone::Coucou if msg == WM_RBUTTONDOWN => RAct::CoucouRight(st.tray.find_by_key("coucou.exe")),
                    HitZone::TrayChevron if msg == WM_RBUTTONDOWN => RAct::OpenUri("ms-settings:taskbar"),
                    HitZone::TrayIcon(slot) => {
                        let bar = st.tray.bar_indices(st.cfg.tray_mode.limit());
                        if let Some(&ti) = bar.get(slot)
                            && let Some(entry) = st.tray.icons.get(ti)
                        {
                            if msg == WM_MBUTTONDOWN {
                                RAct::TrayTogglePin(entry.key.clone())
                            } else {
                                RAct::TrayRightClick(ti)
                            }
                        } else {
                            RAct::None
                        }
                    }
                    HitZone::InputLang if msg == WM_RBUTTONDOWN => RAct::OpenUri("ms-settings:regionlanguage"),
                    HitZone::Network if msg == WM_RBUTTONDOWN => RAct::OpenUri("ms-settings:network"),
                    HitZone::Volume if msg == WM_MBUTTONDOWN => RAct::MuteToggle,
                    HitZone::Volume if msg == WM_RBUTTONDOWN => RAct::OpenUri("ms-settings:apps-volume"),
                    HitZone::Battery if msg == WM_RBUTTONDOWN => RAct::OpenUri("ms-settings:powersleep"),
                    HitZone::Clock if msg == WM_RBUTTONDOWN => RAct::OpenUri("ms-settings:dateandtime"),
                    HitZone::App(i) => {
                        if let Some(item) = st.monitors[idx].items.get(i).cloned() {
                            if msg == WM_MBUTTONDOWN { RAct::NewInstance(item) } else { RAct::AppMenu(item) }
                        } else {
                            RAct::None
                        }
                    }
                    _ if msg == WM_RBUTTONDOWN => RAct::TaskbarMenu(idx, rect),
                    _ => RAct::None,
                }
            })
            .unwrap_or(RAct::None);
            match act {
                RAct::None => {}
                RAct::TaskMgr => launch_task_manager(),
                RAct::WinX => send_chord(&[0x5B, b'X' as u16]),
                RAct::MuteToggle => send_chord(&[0xAD]),
                RAct::OpenUri(uri) => launch_path(uri),
                RAct::CoucouMiddle => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    let snap = with_app(|st| st.coucou.snapshot()).unwrap_or_default();
                    if snap.media_playing || !snap.media_title.is_empty() {
                        send_chord(&[0xB3]);
                    } else if snap.running {
                        send_coucou_cmd_async("toggle");
                    } else {
                        launch_coucou();
                    }
                }
                RAct::CoucouRight(tray_idx_opt) => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    if let Some(ti) = tray_idx_opt {
                        with_app(|st| st.tray.dispatch_click(ti, false));
                    } else {
                        let running = with_app(|st| st.coucou.snapshot().running).unwrap_or(false);
                        if running {
                            send_coucou_cmd_async("settings");
                        } else {
                            launch_coucou();
                        }
                    }
                }
                RAct::TrayRightClick(ti) => {
                    close_popup_safe();
                    close_tray_fly_safe();
                    close_coucou_hub_safe();
                    with_app(|st| st.tray.dispatch_click(ti, false));
                }
                RAct::TrayTogglePin(key) => {
                    with_app(|st| {
                        let limit = st.cfg.tray_mode.limit();
                        st.tray.toggle_pin_on_bar(&key, true, limit);
                    });
                    refresh_tray_layout_safe();
                }
                RAct::NewInstance(item) => launch_new_instance(&item),
                RAct::AppMenu(item) => {
                    let host = with_app(|st| {
                        st.menu_open = true;
                        st.host_hwnd()
                    })
                    .unwrap_or(null_mut());
                    if !host.is_null() {
                        KillTimer(host, TIMER_AUTOHIDE_HOLD);
                        KillTimer(host, TIMER_STATUS_HIDE);
                    }
                    let mut pt = POINT { x, y };
                    ClientToScreen(hwnd, &mut pt);
                    let pins_changed = show_app_context_menu(hwnd, pt.x, pt.y, &item);
                    let rehide = with_app(|st| {
                        st.menu_open = false;
                        st.cfg.autohide && !st.popup_open && st.monitors.iter().all(|m| !m.mouse_inside)
                    })
                    .unwrap_or(false);
                    if pins_changed {
                        let fresh = load_pins();
                        with_app(|st| st.pins = fresh);
                        refresh_windows_all_safe();
                    }
                    if rehide && !host.is_null() {
                        SetTimer(host, TIMER_AUTOHIDE_HOLD, 650, None);
                    }
                }
                RAct::TaskbarMenu(idx, rect) => {
                    let mut pt = POINT { x, y };
                    ClientToScreen(hwnd, &mut pt);
                    run_taskbar_context_menu_safe(hwnd, idx, rect, pt.x, pt.y);
                }
            }
            0
        }
        WM_DROPFILES => {
            let hdrop = wp as HDROP;
            let count = DragQueryFileW(hdrop, u32::MAX, null_mut(), 0);
            let mut pinned_any = false;
            for i in 0..count {
                let len = DragQueryFileW(hdrop, i, null_mut(), 0);
                if len > 0 {
                    let mut buf = vec![0u16; (len + 1) as usize];
                    if DragQueryFileW(hdrop, i, buf.as_mut_ptr(), len + 1) > 0 {
                        let p = PathBuf::from(String::from_utf16_lossy(&buf[..len as usize]));
                        if pin_dropped_file(&p) {
                            pinned_any = true;
                        }
                    }
                }
            }
            DragFinish(hdrop);
            if pinned_any {
                let fresh = load_pins();
                with_app(|st| st.pins = fresh);
                refresh_windows_all_safe();
            }
            0
        }
        WM_APP_ICON_READY => {
            present_bar_hwnd_safe(hwnd);
            0
        }
        WM_DESTROY => 0,
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn popup_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &ps);
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            let inv = with_app(|st| {
                if !st.popup_open {
                    return false;
                }
                let scale = st
                    .monitors
                    .get(st.popup_mon)
                    .or_else(|| st.monitors.first())
                    .map_or(1.0, |m| m.spec.scale);
                let lay = compute_popup_layout(&st.cfg, scale);
                let hit = hit_test_popup(&lay, x, y);
                if st.popup_hover != hit {
                    st.popup_hover = hit;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);
            if inv {
                present_popup_safe();
            }
            0
        }
        WM_LBUTTONDOWN => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            enum PopClick {
                None,
                RebuildKeepOpen,
                ReapplyBackdrops,
                RefreshAll,
                WinSettings,
                LaunchUi,
                Quit(HWND),
            }
            let act = with_app(|st| {
                if !st.popup_open {
                    return PopClick::None;
                }
                let scale = st
                    .monitors
                    .get(st.popup_mon)
                    .or_else(|| st.monitors.first())
                    .map_or(1.0, |m| m.spec.scale);
                let lay = compute_popup_layout(&st.cfg, scale);
                let Some(action) = hit_test_popup(&lay, x, y) else {
                    return PopClick::None;
                };
                match action {
                    PopupAction::TaskbarSize => {
                        st.cfg.dock_size = st.cfg.dock_size.next();
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::Layout => {
                        st.cfg.layout = match st.cfg.layout {
                            LayoutMode::Win12Split => LayoutMode::UnifiedTop,
                            LayoutMode::UnifiedTop => LayoutMode::UnifiedBottom,
                            LayoutMode::UnifiedBottom => LayoutMode::Win12Split,
                        };
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::DockEdge => {
                        st.cfg.dock_edge = if st.cfg.dock_edge == DockEdge::Bottom { DockEdge::Top } else { DockEdge::Bottom };
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::Alignment => {
                        st.cfg.align = match st.cfg.align {
                            AlignPref::System => AlignPref::Center,
                            AlignPref::Center => AlignPref::Left,
                            AlignPref::Left => AlignPref::System,
                        };
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::Displays => {
                        st.cfg.multimon = match st.cfg.multimon {
                            MultiMonPref::System => MultiMonPref::AllDisplays,
                            MultiMonPref::AllDisplays => MultiMonPref::PrimaryOnly,
                            MultiMonPref::PrimaryOnly => MultiMonPref::System,
                        };
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::Theme => {
                        st.cfg.theme = match st.cfg.theme {
                            ThemePref::System => ThemePref::Dark,
                            ThemePref::Dark => ThemePref::Light,
                            ThemePref::Light => ThemePref::System,
                        };
                        let _ = save_config(&st.cfg);
                        PopClick::ReapplyBackdrops
                    }
                    PopupAction::Glass => {
                        st.cfg.glass = st.cfg.glass.next();
                        let _ = save_config(&st.cfg);
                        PopClick::ReapplyBackdrops
                    }
                    PopupAction::AutoHideDock => {
                        st.cfg.autohide = !st.cfg.autohide;
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::BottomModules => {
                        st.cfg.status_autohide = !st.cfg.status_autohide;
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::GamingMode => {
                        st.cfg.gaming_mode = st.cfg.gaming_mode.toggle();
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::TrayMode => {
                        st.cfg.tray_mode = st.cfg.tray_mode.next();
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::ShowPreviews => {
                        st.cfg.show_previews = !st.cfg.show_previews;
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::ShowSeconds => {
                        st.cfg.show_seconds = !st.cfg.show_seconds;
                        let _ = save_config(&st.cfg);
                        let (t, d, sp) = format_clock_all(st.cfg.show_seconds);
                        st.time_str = t;
                        st.date_str = d;
                        st.split_clock_str = sp;
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::ShowSearch => {
                        st.cfg.show_search = !st.cfg.show_search;
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::ShowTaskView => {
                        st.cfg.show_taskview = !st.cfg.show_taskview;
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::ShowTelemetry => {
                        st.cfg.show_telemetry = !st.cfg.show_telemetry;
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::ShowDesktopBtn => {
                        st.cfg.show_desktop_btn = !st.cfg.show_desktop_btn;
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::ShowCoucou => {
                        st.cfg.show_coucou = !st.cfg.show_coucou;
                        let _ = save_config(&st.cfg);
                        PopClick::RebuildKeepOpen
                    }
                    PopupAction::RefreshAll => PopClick::RefreshAll,
                    PopupAction::WinTaskbarSettings => PopClick::WinSettings,
                    PopupAction::OpenBoostUi => PopClick::LaunchUi,
                    PopupAction::RestoreStock => PopClick::Quit(st.host_hwnd()),
                }
            })
            .unwrap_or(PopClick::None);

            match act {
                PopClick::None => {}
                PopClick::RebuildKeepOpen => {
                    close_preview_safe();
                    rebuild_monitors_safe();
                    // Re-anchor the open Settings popup cleanly to the new TopRight position (e.g. when
                    // switching between Win12Split, UnifiedTop, UnifiedBottom, or DockEdge).
                    with_app(|st| {
                        let mi = st.popup_mon.min(st.monitors.len().saturating_sub(1));
                        if let Some(m) = st.monitors.get(mi) {
                            let scale = m.spec.scale;
                            let mrect = m.spec.rect;
                            let anchor = m.lay.top_right.or(m.lay.dock).unwrap_or(mrect);
                            let s = |v: i32| sc(v, scale);
                            let play = compute_popup_layout(&st.cfg, scale);
                            let pw = play.w;
                            let ph = play.h;
                            let px = (anchor.right - pw)
                                .clamp(mrect.left + s(8), (mrect.right - pw - s(8)).max(mrect.left + s(8)));
                            let py = if anchor.top < (mrect.top + mrect.bottom) / 2 {
                                anchor.bottom + s(8)
                            } else {
                                anchor.top - ph - s(8)
                            }
                            .clamp(mrect.top + s(8), (mrect.bottom - ph - s(8)).max(mrect.top + s(8)));
                            st.popup_rect = RECT { left: px, top: py, right: px + pw, bottom: py + ph };
                        }
                    });
                    present_popup_safe();
                    SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_SHOWWINDOW);
                }
                PopClick::ReapplyBackdrops => reapply_backdrops_safe(),
                PopClick::RefreshAll => {
                    let fresh = load_pins();
                    with_app(|st| st.pins = fresh);
                    refresh_windows_all_safe();
                    broadcast_taskbar_created();
                    present_popup_safe();
                }
                PopClick::WinSettings => {
                    close_popup_safe();
                    launch_path("ms-settings:taskbar");
                }
                PopClick::LaunchUi => {
                    close_popup_safe();
                    launch_boost_ui();
                }
                PopClick::Quit(host) => {
                    close_popup_safe();
                    if !host.is_null() {
                        PostMessageW(host, WM_CLOSE, 0, 0);
                    }
                }
            }
            0
        }
        WM_ACTIVATE if (wp & 0xFFFF) == 0 => {
            close_popup_safe();
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn tip_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            BeginPaint(hwnd, &mut ps);
            EndPaint(hwnd, &ps);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn preview_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            let wdc = BeginPaint(hwnd, &mut ps);
            let mut cr: RECT = zeroed();
            GetClientRect(hwnd, &mut cr);
            let (w, h) = (cr.right.max(1), cr.bottom.max(1));
            with_app(|st| {
                let Some(ref lay) = st.preview_layout else { return };
                let scale = st
                    .monitors
                    .get(st.preview_mon)
                    .or_else(|| st.monitors.first())
                    .map_or(1.0, |m| m.spec.scale);
                let icon_bmp = st.icons.get_cached(&st.preview_icon_key);
                if st.surf_preview.ensure_size(wdc, w, h) {
                    paint_preview(
                        &mut st.surf_preview,
                        &st.ui_text_ren,
                        lay,
                        &st.pal,
                        icon_bmp.as_deref(),
                        st.preview_hover,
                        scale,
                    );
                    BitBlt(wdc, 0, 0, w, h, st.surf_preview.dc, 0, 0, SRCCOPY);
                }
            });
            EndPaint(hwnd, &ps);
            0
        }
        WM_MOUSEMOVE => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            let (host, need_track, inv) = with_app(|st| {
                let host = st.host_hwnd();
                let need_track = if !st.preview_tracking {
                    st.preview_tracking = true;
                    true
                } else {
                    false
                };
                let hit = st
                    .preview_layout
                    .as_ref()
                    .map_or(PreviewHit::None, |l| hit_test_preview(l, x, y));
                let inv = if st.preview_hover != hit {
                    st.preview_hover = hit;
                    true
                } else {
                    false
                };
                (host, need_track, inv)
            })
            .unwrap_or((null_mut(), false, false));
            if !host.is_null() {
                KillTimer(host, TIMER_PREVIEW_HIDE);
            }
            if need_track {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tme);
            }
            if inv {
                InvalidateRect(hwnd, null(), 0);
            }
            0
        }
        WM_MOUSELEAVE_EVT => {
            let host = with_app(|st| {
                st.preview_tracking = false;
                st.preview_hover = PreviewHit::None;
                st.host_hwnd()
            })
            .unwrap_or(null_mut());
            InvalidateRect(hwnd, null(), 0);
            if !host.is_null() {
                SetTimer(host, TIMER_PREVIEW_HIDE, 260, None);
            }
            0
        }
        WM_LBUTTONUP | WM_MBUTTONUP | WM_RBUTTONUP => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            enum PrevAct {
                None,
                Focus(HWND),
                Close(HWND),
                ContextMenu(HWND),
            }
            let act = with_app(|st| {
                let Some(ref lay) = st.preview_layout else { return PrevAct::None };
                match (msg, hit_test_preview(lay, x, y)) {
                    (WM_LBUTTONUP, PreviewHit::Card(i)) => lay.cards.get(i).map_or(PrevAct::None, |c| PrevAct::Focus(c.hwnd)),
                    (WM_LBUTTONUP, PreviewHit::Close(i)) => lay.cards.get(i).map_or(PrevAct::None, |c| PrevAct::Close(c.hwnd)),
                    (WM_MBUTTONUP, PreviewHit::Card(i) | PreviewHit::Close(i)) => {
                        lay.cards.get(i).map_or(PrevAct::None, |c| PrevAct::Close(c.hwnd))
                    }
                    (WM_RBUTTONUP, PreviewHit::Card(i) | PreviewHit::Close(i)) => {
                        lay.cards.get(i).map_or(PrevAct::None, |c| PrevAct::ContextMenu(c.hwnd))
                    }
                    _ => PrevAct::None,
                }
            })
            .unwrap_or(PrevAct::None);

            match act {
                PrevAct::None => {}
                PrevAct::Focus(w) => {
                    close_preview_safe();
                    bring_to_front(w);
                }
                PrevAct::Close(w) => {
                    close_window_graceful(w);
                    close_preview_safe();
                }
                PrevAct::ContextMenu(w) => {
                    let mut pt = POINT { x, y };
                    ClientToScreen(hwnd, &mut pt);
                    let host = with_app(|st| {
                        st.menu_open = true;
                        st.host_hwnd()
                    })
                    .unwrap_or(null_mut());
                    if !host.is_null() {
                        KillTimer(host, TIMER_PREVIEW_HIDE);
                        KillTimer(host, TIMER_AUTOHIDE_HOLD);
                    }
                    show_window_context_menu(hwnd, pt.x, pt.y, w);
                    with_app(|st| st.menu_open = false);
                    close_preview_safe();
                }
            }
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn sensor_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            enum SensorHit {
                None,
                Dock(usize, isize, bool, bool, HWND),
                Status(usize, isize, bool, bool, HWND),
            }
            let hit = with_app(|st| {
                let host = st.host_hwnd();
                let gaming_on = st.cfg.gaming_mode != GamingModePref::Off;
                if let Some(idx) = st.monitors.iter().position(|m| m.hwnd_sensor == hwnd)
                    && st.cfg.autohide
                {
                    let m = &st.monitors[idx];
                    return SensorHit::Dock(idx, m.spec.hmon, m.fullscreen, gaming_on, host);
                }
                if let Some(idx) = st.monitors.iter().position(|m| m.hwnd_status_sensor == hwnd)
                    && st.cfg.layout == LayoutMode::Win12Split
                    && st.cfg.status_autohide
                {
                    let m = &st.monitors[idx];
                    return SensorHit::Status(idx, m.spec.hmon, m.fullscreen, gaming_on, host);
                }
                SensorHit::None
            })
            .unwrap_or(SensorHit::None);
            match hit {
                SensorHit::None => {}
                SensorHit::Dock(idx, hmon, cached_fs, gaming_on, host) => {
                    // Live fullscreen double-check: never pop even 1ms after a game goes fullscreen!
                    if gaming_on && (cached_fs || detect_fullscreen_on_monitor(hmon, std::process::id())) {
                        sync_fullscreen_state_safe();
                        return 0;
                    }
                    start_mon_anim_safe(idx, 1.0);
                    if !host.is_null() {
                        SetTimer(host, TIMER_AUTOHIDE_HOLD, 1800, None);
                    }
                }
                SensorHit::Status(idx, hmon, cached_fs, gaming_on, host) => {
                    if gaming_on && (cached_fs || detect_fullscreen_on_monitor(hmon, std::process::id())) {
                        sync_fullscreen_state_safe();
                        return 0;
                    }
                    start_status_anim_safe(idx, 1.0);
                    if !host.is_null() {
                        SetTimer(host, TIMER_STATUS_HIDE, 1800, None);
                    }
                }
            }
            return 0;
        }
        WM_RBUTTONDOWN => {
            let (x, y) = ((lp & 0xFFFF) as i16 as i32, (lp >> 16) as i16 as i32);
            let target = with_app(|st| {
                let idx = st
                    .monitors
                    .iter()
                    .position(|m| m.hwnd_status_sensor == hwnd || m.hwnd_sensor == hwnd)?;
                let m = &st.monitors[idx];
                let is_status = m.hwnd_status_sensor == hwnd;
                let anchor = m.lay.top_right.or(m.lay.dock).unwrap_or(m.spec.rect);
                Some((idx, m.spec.hmon, m.fullscreen, st.cfg.gaming_mode != GamingModePref::Off, is_status, anchor))
            })
            .flatten();
            if let Some((idx, hmon, cached_fs, gaming_on, is_status, anchor)) = target {
                if gaming_on && (cached_fs || detect_fullscreen_on_monitor(hmon, std::process::id())) {
                    sync_fullscreen_state_safe();
                    return 0;
                }
                if is_status {
                    start_status_anim_safe(idx, 1.0);
                } else {
                    start_mon_anim_safe(idx, 1.0);
                }
                let mut pt = POINT { x, y };
                ClientToScreen(hwnd, &mut pt);
                run_taskbar_context_menu_safe(hwnd, idx, anchor, pt.x, pt.y);
            }
            return 0;
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe fn launch_boost_ui() {
    let mut candidates = Vec::new();
    if let Ok(exe) = env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("gp-pc-boost-ui.exe"));
    }
    candidates.push(PathBuf::from(r"D:\Project\gp-pc-boost\target\release\gp-pc-boost-ui.exe"));
    for c in candidates {
        if c.is_file() {
            launch_path(&c.to_string_lossy());
            return;
        }
    }
}

unsafe fn launch_task_manager() {
    let sys = env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let tm = PathBuf::from(sys).join(r"System32\taskmgr.exe");
    launch_path(&tm.to_string_lossy());
}

#[cfg(test)]
mod gaming_tests {
    use super::*;

    const MON: RECT = RECT { left: 0, top: 0, right: 1920, bottom: 1080 };

    #[test]
    fn fullscreen_classifier_matches_native_windows_gaming_mode() {
        // 1. Exclusive DirectX / Vulkan fullscreen (QUNS_RUNNING_D3D_FULL_SCREEN)
        assert!(classify_fullscreen_window(MON, MON, WS_POPUP, false, true));

        // 2. Borderless fullscreen game (UE5 / Unity / DXGI / F11 video): WS_POPUP covering monitor
        assert!(classify_fullscreen_window(MON, MON, WS_POPUP, false, false));

        // 3. Borderless fullscreen with 1px DPI rounding tolerance
        let tol = RECT { left: 1, top: 0, right: 1919, bottom: 1080 };
        assert!(classify_fullscreen_window(MON, tol, WS_POPUP, false, false));

        // 4. Borderless fullscreen game that also sets WS_MAXIMIZE (no caption, no thickframe)
        assert!(classify_fullscreen_window(MON, MON, WS_POPUP | WS_MAXIMIZE, true, false));

        // 5. Normal maximized desktop app (Edge, VS Code, Explorer) MUST NOT trigger gaming mode
        let normal_max = RECT { left: -8, top: -8, right: 1928, bottom: 1088 };
        assert!(!classify_fullscreen_window(
            MON,
            normal_max,
            WS_OVERLAPPEDWINDOW | WS_MAXIMIZE,
            true,
            false
        ));

        // 6. Custom-chrome frameless maximized Electron/Chrome app (no caption, keeps WS_THICKFRAME)
        assert!(!classify_fullscreen_window(
            MON,
            MON,
            WS_POPUP | WS_THICKFRAME | WS_MAXIMIZE,
            true,
            false
        ));

        // 7. Windowed game (not covering full monitor) MUST NOT trigger gaming mode
        let windowed = RECT { left: 320, top: 180, right: 1600, bottom: 900 };
        assert!(!classify_fullscreen_window(MON, windowed, WS_POPUP, false, false));
        assert!(!classify_fullscreen_window(MON, windowed, WS_OVERLAPPEDWINDOW, false, false));

        // 8. Shell / Desktop / Start / Notification classes are always excluded
        assert!(is_excluded_fullscreen_class("Progman"));
        assert!(is_excluded_fullscreen_class("WorkerW"));
        assert!(is_excluded_fullscreen_class("Shell_TrayWnd"));
        assert!(is_excluded_fullscreen_class("Windows.UI.Core.CoreWindow"));
        assert!(!is_excluded_fullscreen_class("UnrealWindow"));
        assert!(!is_excluded_fullscreen_class("UnityWndClass"));

        // 9. Windows Start Menu / Search / Calendar / Quick Settings host detection
        assert!(is_windows_shell_flyout_exe(
            r"C:\Windows\SystemApps\Microsoft.Windows.StartMenuExperienceHost_cw5n1h2txyewy\StartMenuExperienceHost.exe"
        ));
        assert!(is_windows_shell_flyout_exe(r"C:\Windows\SystemApps\ShellExperienceHost\ShellExperienceHost.exe"));
        assert!(is_windows_shell_flyout_exe(r"C:\Windows\SystemApps\SearchHost\SearchHost.exe"));
        assert!(!is_windows_shell_flyout_exe(r"C:\Games\Tekken8\Polaris-Win64-Shipping.exe"));
    }
}
