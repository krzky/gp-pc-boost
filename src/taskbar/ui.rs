//! Fluent 2 Windows 12 Split-Plane layout, system-accent palette, rendering & hit-testing.

use crate::taskbar::{
    config::{AlignPref, Config, DockEdge, GamingModePref, GlassPref, LayoutMode, ThemePref},
    coucou::{CoucouMood, CoucouSnapshot},
    dwm::{FontSlot, Surface, TextAlign, TextRenderer},
    icons::{IconBitmap, IconCache, IconRequest},
    search::{SearchKind, SearchResult, SearchScope},
    sys_prefs::SysPrefs,
    telemetry::TelemetrySnapshot,
    windows::{AppItem, MonitorSpec},
};
use std::sync::Arc;
use windows_sys::Win32::Foundation::{HWND, RECT};

pub const TOP_STRIP_H: i32 = 34;
pub const TOP_MINI_H: i32 = 30;
pub const DOCK_H: i32 = 52;
pub const DOCK_SEC_H: i32 = 48;
pub const UNIFIED_H: i32 = 48;
pub const EDGE_GAP: i32 = 8;
pub const DOCK_GAP: i32 = 10;
pub const ISLAND_GAP: i32 = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SurfaceKind {
    TopLeft,
    TopCenter,
    TopRight,
    Dock,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HitZone {
    None,
    Workspace,
    Gear,
    Start,
    Search,
    Coucou,
    TaskView,
    App(usize),
    TrayChevron,
    TrayIcon(usize),
    InputLang,
    Network,
    Volume,
    Battery,
    Clock,
    ShowDesktop,
}

#[derive(Clone, Copy)]
pub struct Palette {
    pub is_light: bool,
    pub transparent: bool,
    pub mica_tint: u32,     // status plane frosted tint
    pub acrylic_tint: u32,  // dock / popup / tooltip frosted tint
    pub border_argb: u32,   // 1px anti-aliased pill outline
    pub top_sheen: u32,     // 1px Fluent 2 specular top highlight
    pub hover_pill: u32,
    pub press_pill: u32,
    pub active_pill: u32,
    pub accent: u32,
    pub start_tiles: [u32; 4],
    pub text: u32,
    pub dim: u32,
    pub divider: u32,
    pub border_ref: u32, // COLORREF 0x00BBGGRR
    pub good: u32,
    pub warn: u32,
}

pub fn fluent_palette(cfg: &Config, sys: &SysPrefs) -> Palette {
    let is_light = match cfg.theme {
        ThemePref::System => sys.light_theme,
        ThemePref::Dark => false,
        ThemePref::Light => true,
    };
    let accent = sys.accent_argb;
    let accent_soft = tint_with_white(accent, if is_light { 0.10 } else { 0.28 });
    let accent_deep = darken(accent, if is_light { 0.18 } else { 0.10 });
    let transparent = match cfg.glass {
        GlassPref::System => sys.transparency,
        GlassPref::Frosted => true,
        GlassPref::Solid => false,
    };

    if is_light {
        Palette {
            is_light: true,
            transparent,
            mica_tint: if transparent { 0xE6F3_F5FA } else { 0xFFF3_F4F6 },
            acrylic_tint: if transparent { 0xEEF8_FAFE } else { 0xFFF8_FAFC },
            border_argb: 0x2400_0000,
            top_sheen: 0x7AFF_FFFF,
            hover_pill: 0x1600_0000,
            press_pill: 0x2200_0000,
            active_pill: 0x2000_0000,
            accent,
            start_tiles: [accent_soft, accent, accent, accent_deep],
            text: 0xFF16_181D,
            dim: 0xFF52_5866,
            divider: 0x1E00_0000,
            border_ref: 0x00D8_D2CC,
            good: 0xFF0F_7B0F,
            warn: 0xFF9D_5D00,
        }
    } else {
        Palette {
            is_light: false,
            transparent,
            mica_tint: if transparent { 0xDE15_1822 } else { 0xFF1F_2128 },
            acrylic_tint: if transparent { 0xE811_141D } else { 0xFF1C_1E24 },
            border_argb: 0x30FF_FFFF,
            top_sheen: 0x28FF_FFFF,
            hover_pill: 0x1EFF_FFFF,
            press_pill: 0x2AFF_FFFF,
            active_pill: 0x28FF_FFFF,
            accent,
            start_tiles: [accent_soft, accent, accent, accent_deep],
            text: 0xFFF6_F7FA,
            dim: 0xFFB8_BECD,
            divider: 0x24FF_FFFF,
            border_ref: 0x004A_443E,
            good: 0xFF6C_CB5F,
            warn: 0xFFFC_E100,
        }
    }
}

fn tint_with_white(argb: u32, t: f32) -> u32 {
    let blend = |c: u32| (c as f32 + (255.0 - c as f32) * t).round().clamp(0.0, 255.0) as u32;
    0xFF00_0000
        | (blend((argb >> 16) & 0xFF) << 16)
        | (blend((argb >> 8) & 0xFF) << 8)
        | blend(argb & 0xFF)
}

fn darken(argb: u32, t: f32) -> u32 {
    let scale = |c: u32| (c as f32 * (1.0 - t)).round().clamp(0.0, 255.0) as u32;
    0xFF00_0000
        | (scale((argb >> 16) & 0xFF) << 16)
        | (scale((argb >> 8) & 0xFF) << 8)
        | scale(argb & 0xFF)
}

#[inline]
pub fn sc(v: i32, scale: f32) -> i32 {
    (v as f32 * scale).round() as i32
}

#[derive(Clone)]
pub struct MonitorLayout {
    pub top_left: Option<RECT>,
    pub top_center: Option<RECT>,
    pub top_right: Option<RECT>,
    pub dock: Option<RECT>,

    // TopLeft internal zones
    pub workspace_rect: RECT,
    // TopCenter internal zones
    pub clock_rect: RECT,
    pub search_rect: Option<RECT>,
    pub coucou_rect: Option<RECT>,
    // TopRight internal zones
    pub tray_chevron_rect: Option<RECT>,
    pub tray_rects: Vec<RECT>,
    pub tray_div_x: Option<i32>,
    pub net_rect: RECT,
    pub vol_rect: RECT,
    pub bat_rect: Option<RECT>,
    pub lang_rect: Option<RECT>,
    pub gear_rect: RECT,
    pub desk_rect: RECT,

    // Dock internal layout
    pub dock_pad: i32,
    pub dock_h: i32,
    pub btn_w: i32,
    pub btn_gap: i32,
    pub icon_px: i32,
    pub dock_div_x: i32,
    pub apps_x0: i32,
    pub has_search_in_dock: bool,
    pub has_taskview_in_dock: bool,
    pub show_telemetry: bool,
    pub work_top_reserve: i32,
    pub work_bottom_reserve: i32,
}

impl MonitorLayout {
    pub fn rect_for(&self, kind: SurfaceKind) -> Option<RECT> {
        match kind {
            SurfaceKind::TopLeft => self.top_left,
            SurfaceKind::TopCenter => self.top_center,
            SurfaceKind::TopRight => self.top_right,
            SurfaceKind::Dock => self.dock,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn compute_monitor_layout(
    mon: &MonitorSpec,
    cfg: &Config,
    sys: &SysPrefs,
    app_count: usize,
    tray_bar_count: usize,
    has_tray_chevron: bool,
    has_lang: bool,
    has_battery: bool,
    telem: TelemetrySnapshot,
    workspace_label: &str,
    clock_text: &str,
    coucou_label: &str,
    coucou_live_span: Option<(i32, i32)>,
) -> MonitorLayout {
    let s = |v: i32| sc(v, mon.scale);
    let m = mon.rect;
    let sw = (m.right - m.left).max(480);
    let margin = s(12);
    let (dock_prim_h, dock_sec_h, unified_h, max_bw_cfg, icon_px_cfg, pad_cfg, gap_cfg) = cfg.dock_size.metrics();
    let center_align = match cfg.align {
        AlignPref::System => sys.align_center,
        AlignPref::Center => true,
        AlignPref::Left => false,
    };

    match cfg.layout {
        LayoutMode::Win12Split => {
            if mon.primary {
                // Primary Split Plane: App Dock sits on `cfg.dock_edge`, Status Plane sits on the opposite edge
                let th = s(TOP_STRIP_H);
                let tgap = s(EDGE_GAP);
                let dock_h = s(dock_prim_h);
                let dgap = s(if dock_prim_h <= 36 { 8 } else { DOCK_GAP });
                let status_res = if cfg.status_autohide { 0 } else { tgap + th + s(4) };
                let (status_y, dock_y, work_top_reserve, work_bottom_reserve) = match cfg.dock_edge {
                    DockEdge::Top => (
                        m.bottom - th - tgap,
                        m.top + dgap,
                        dgap + dock_h + s(4),
                        status_res,
                    ),
                    DockEdge::Bottom => (
                        m.top + tgap,
                        m.bottom - dock_h - dgap,
                        status_res,
                        dgap + dock_h + s(4),
                    ),
                };

                // Status Left: subtle dot + active workspace/(optional telemetry)
                let ws_chars = workspace_label.chars().count().clamp(8, 24) as i32;
                let cpu_ram_w = if cfg.show_telemetry { s(124) } else { 0 };
                let min_tl = if cfg.show_telemetry { s(220) } else { s(120) };
                let tl_w = (s(32) + ws_chars * s(7) + cpu_ram_w).clamp(min_tl, (sw / 3).max(min_tl + s(10)));
                let top_left = RECT {
                    left: m.left + margin,
                    top: status_y,
                    right: m.left + margin + tl_w,
                    bottom: status_y + th,
                };
                let workspace_rect = RECT {
                    left: s(4),
                    top: s(3),
                    right: tl_w - s(4),
                    bottom: th - s(3),
                };

                // Status Right: [(^ chevron) (active small tray icons...) | Net | Vol | (Bat) | (Lang) | Gear | (ShowDesktop)]
                let mut rx = s(6);
                let tray_chevron_rect = if has_tray_chevron || tray_bar_count > 0 {
                    let r = RECT { left: rx, top: s(3), right: rx + s(22), bottom: th - s(3) };
                    rx += s(24);
                    Some(r)
                } else {
                    None
                };
                let max_tray_slots = ((sw / 3 - s(120)) / s(26)).clamp(0, 14) as usize;
                let shown_tray = tray_bar_count.min(max_tray_slots);
                let mut tray_rects = Vec::with_capacity(shown_tray);
                for _ in 0..shown_tray {
                    tray_rects.push(RECT { left: rx, top: s(3), right: rx + s(24), bottom: th - s(3) });
                    rx += s(26);
                }
                let tray_div_x = if tray_chevron_rect.is_some() || !tray_rects.is_empty() {
                    let dx = rx + s(2);
                    rx += s(8);
                    Some(dx)
                } else {
                    None
                };

                let net_rect = RECT { left: rx, top: s(3), right: rx + s(28), bottom: th - s(3) };
                rx += s(30);
                let vol_rect = RECT { left: rx, top: s(3), right: rx + s(28), bottom: th - s(3) };
                rx += s(30);
                let bat_rect = if has_battery {
                    let r = RECT { left: rx, top: s(3), right: rx + s(40), bottom: th - s(3) };
                    rx += s(42);
                    Some(r)
                } else {
                    let _ = telem;
                    None
                };
                let lang_rect = if has_lang {
                    let r = RECT { left: rx, top: s(3), right: rx + s(34), bottom: th - s(3) };
                    rx += s(36);
                    Some(r)
                } else {
                    None
                };
                rx += s(6);
                let gear_rect = RECT { left: rx, top: s(3), right: rx + s(28), bottom: th - s(3) };
                rx += s(31);
                let (desk_rect, tr_w) = if cfg.show_desktop_btn {
                    let dr = RECT { left: rx, top: s(3), right: rx + s(14), bottom: th - s(3) };
                    (dr, rx + s(20))
                } else {
                    (RECT { left: 0, top: 0, right: 0, bottom: 0 }, rx + s(5))
                };
                let top_right = RECT {
                    left: m.right - margin - tr_w,
                    top: status_y,
                    right: m.right - margin,
                    bottom: status_y + th,
                };

                // Status Center: ONE unified middle dock capsule
                //   - Idle: `[ 🔍 Search | Calendar/Clock | Mochi ]` centered in the middle.
                //   - Live Coucou pill (`coucou_live_span == Some((pl, pr))`):
                //     `[ 🔍 Search | <live Coucou pill centered in bay> | Calendar/Clock ]`
                //     Search flanks the left and Calendar moves to the right of Coucou inside ONE
                //     continuous rounded glass bar so the whole middle section stays balanced & centered!
                let c_chars = clock_text.chars().count().clamp(14, 30) as i32;
                let clock_w = (c_chars * s(8) + s(20)).clamp(s(168), s(264));
                let search_w = if cfg.show_search { s(76) } else { 0 };
                let show_static_coucou = cfg.show_coucou && coucou_live_span.is_none();
                let coucou_chars = coucou_label.chars().count().clamp(5, 22) as i32;
                let max_center_avail = (top_right.left - top_left.right - 2 * s(ISLAND_GAP)).max(s(180));
                let tc_min = top_left.right + s(ISLAND_GAP);
                let tc_right_max = top_right.left - s(ISLAND_GAP);
                let (tc_x, tc_right, coucou_w) = if let Some((pill_l, pill_r)) = coucou_live_span {
                    let left_wing = if cfg.show_search { s(4) + search_w + s(8) } else { s(4) };
                    let right_wing = s(8) + clock_w + s(4);
                    let x = (pill_l - left_wing).clamp(tc_min, pill_l.max(tc_min));
                    let r = (pill_r + right_wing).clamp((pill_r + s(40)).min(tc_right_max), tc_right_max.max(x + s(160)));
                    (x, r, (pill_r - pill_l).max(0))
                } else {
                    let cw = if show_static_coucou {
                        (s(36) + coucou_chars * s(7))
                            .clamp(s(86), s(188))
                            .min((max_center_avail - search_w - clock_w - s(24)).max(s(76)))
                    } else {
                        0
                    };
                    let mut w = s(8) + clock_w;
                    if cfg.show_search {
                        w += search_w + s(8);
                    }
                    if show_static_coucou {
                        w += cw + s(8);
                    }
                    w = w.min(max_center_avail);
                    let tc_max = (tc_right_max - w).max(tc_min);
                    let x = (m.left + (sw - w) / 2).clamp(tc_min, tc_max);
                    let r = (x + w).min(top_right.left - s(6));
                    (x, r, cw)
                };
                let tc_actual_w = (tc_right - tc_x).max(s(120));
                let top_center = RECT {
                    left: tc_x,
                    top: status_y,
                    right: tc_right,
                    bottom: status_y + th,
                };
                let mut cx_in = s(4);
                let search_rect = if cfg.show_search {
                    let r = RECT {
                        left: cx_in,
                        top: s(3),
                        right: cx_in + search_w,
                        bottom: th - s(3),
                    };
                    cx_in += search_w + s(8);
                    Some(r)
                } else {
                    None
                };
                let (clock_left, clock_right) = if let Some((_, pill_r)) = coucou_live_span {
                    // Calendar moves to the right wing of the unified bar so Coucou sits dead-center!
                    let cl = ((pill_r - tc_x) + s(6)).clamp(cx_in, (tc_actual_w - s(84)).max(cx_in));
                    (cl, tc_actual_w - s(4))
                } else if show_static_coucou {
                    (cx_in, (tc_actual_w - coucou_w - s(10)).max(cx_in + s(80)))
                } else {
                    (cx_in, tc_actual_w - s(4))
                };
                let clock_rect = RECT {
                    left: clock_left,
                    top: s(3),
                    right: clock_right,
                    bottom: th - s(3),
                };
                let coucou_rect = if show_static_coucou && tc_actual_w - (clock_right + s(8)) >= s(54) {
                    Some(RECT {
                        left: clock_right + s(6),
                        top: s(3),
                        right: tc_actual_w - s(4),
                        bottom: th - s(3),
                    })
                } else {
                    None
                };

                // Floating App Dock: [Start | (TaskView) | div | Apps...]
                let dock_pad = s(pad_cfg);
                let fixed_slots = 1 + i32::from(cfg.show_taskview);
                let total_slots = fixed_slots + app_count as i32;
                let mut btn_gap = s(gap_cfg);
                let div_extra = s(10);
                let max_dock_w = (sw - 2 * margin).max(s(180));
                let usable = (max_dock_w - 2 * dock_pad - div_extra).max(s(110));
                let raw_bw = ((usable + btn_gap) / total_slots.max(1)) - btn_gap;
                let min_bw = s(22);
                let btn_w = raw_bw.clamp(min_bw, s(max_bw_cfg));
                if raw_bw < min_bw {
                    btn_gap = s(2);
                }
                let content_w = total_slots * btn_w + (total_slots - 1).max(0) * btn_gap + div_extra;
                let dock_w = (2 * dock_pad + content_w).min(max_dock_w);
                let dock_x = if center_align {
                    m.left + (sw - dock_w) / 2
                } else {
                    m.left + margin
                };
                let dock = RECT {
                    left: dock_x,
                    top: dock_y,
                    right: dock_x + dock_w,
                    bottom: dock_y + dock_h,
                };
                let dock_div_x = dock_pad + fixed_slots * btn_w + (fixed_slots - 1) * btn_gap + s(4);
                let apps_x0 = dock_div_x + s(6);

                MonitorLayout {
                    top_left: Some(top_left),
                    top_center: Some(top_center),
                    top_right: Some(top_right),
                    dock: Some(dock),
                    workspace_rect,
                    clock_rect,
                    search_rect,
                    coucou_rect,
                    tray_chevron_rect,
                    tray_rects,
                    tray_div_x,
                    net_rect,
                    vol_rect,
                    bat_rect,
                    lang_rect,
                    gear_rect,
                    desk_rect,
                    dock_pad,
                    dock_h,
                    btn_w,
                    btn_gap,
                    icon_px: s(icon_px_cfg).clamp(14, (btn_w - s(6)).max(14)),
                    dock_div_x,
                    apps_x0,
                    has_search_in_dock: false,
                    has_taskview_in_dock: cfg.show_taskview,
                    show_telemetry: cfg.show_telemetry,
                    work_top_reserve,
                    work_bottom_reserve,
                }
            } else {
                // Secondary monitor: Dock on `cfg.dock_edge`, Mini Clock on opposite edge
                let th = s(TOP_MINI_H);
                let tgap = s(EDGE_GAP);
                let dock_h = s(dock_sec_h);
                let dgap = s(if dock_sec_h <= 34 { 8 } else { DOCK_GAP });
                let (clock_y, dock_y, work_top_reserve, work_bottom_reserve) = match cfg.dock_edge {
                    DockEdge::Top => (m.bottom - th - tgap, m.top + dgap, dgap + dock_h + s(4), 0),
                    DockEdge::Bottom => (m.top + tgap, m.bottom - dock_h - dgap, 0, dgap + dock_h + s(4)),
                };
                let c_chars = clock_text.chars().count().clamp(14, 30) as i32;
                let tc_w = (c_chars * s(8) + s(28)).clamp(s(176), s(268)).min(sw - 2 * margin);
                let tc_x = m.left + (sw - tc_w) / 2;
                let top_center = RECT {
                    left: tc_x,
                    top: clock_y,
                    right: tc_x + tc_w,
                    bottom: clock_y + th,
                };
                let clock_rect = RECT { left: s(6), top: s(2), right: tc_w - s(6), bottom: th - s(2) };

                let dock_pad = s(pad_cfg);
                let fixed_slots = 1; // Start button on secondary dock
                let total_slots = fixed_slots + app_count as i32;
                let mut btn_gap = s(gap_cfg);
                let div_extra = if app_count > 0 { s(10) } else { 0 };
                let max_dock_w = (sw - 2 * margin).max(s(120));
                let usable = (max_dock_w - 2 * dock_pad - div_extra).max(s(70));
                let raw_bw = ((usable + btn_gap) / total_slots.max(1)) - btn_gap;
                let btn_w = raw_bw.clamp(s(20), s((max_bw_cfg - 2).max(28)));
                if raw_bw < s(20) {
                    btn_gap = s(2);
                }
                let content_w = total_slots * btn_w + (total_slots - 1).max(0) * btn_gap + div_extra;
                let dock_w = (2 * dock_pad + content_w).min(max_dock_w);
                let dock_x = if center_align {
                    m.left + (sw - dock_w) / 2
                } else {
                    m.left + margin
                };
                let dock = RECT {
                    left: dock_x,
                    top: dock_y,
                    right: dock_x + dock_w,
                    bottom: dock_y + dock_h,
                };
                let dock_div_x = dock_pad + btn_w + s(4);
                let apps_x0 = if app_count > 0 { dock_div_x + s(6) } else { dock_pad + btn_w };

                MonitorLayout {
                    top_left: None,
                    top_center: Some(top_center),
                    top_right: None,
                    dock: Some(dock),
                    workspace_rect: RECT { left: 0, top: 0, right: 0, bottom: 0 },
                    clock_rect,
                    search_rect: None,
                    coucou_rect: None,
                    tray_chevron_rect: None,
                    tray_rects: Vec::new(),
                    tray_div_x: None,
                    net_rect: RECT { left: 0, top: 0, right: 0, bottom: 0 },
                    vol_rect: RECT { left: 0, top: 0, right: 0, bottom: 0 },
                    bat_rect: None,
                    lang_rect: None,
                    gear_rect: RECT { left: 0, top: 0, right: 0, bottom: 0 },
                    desk_rect: RECT { left: 0, top: 0, right: 0, bottom: 0 },
                    dock_pad,
                    dock_h,
                    btn_w,
                    btn_gap,
                    icon_px: s((icon_px_cfg - 2).max(16)).clamp(14, (btn_w - s(6)).max(14)),
                    dock_div_x,
                    apps_x0,
                    has_search_in_dock: false,
                    has_taskview_in_dock: false,
                    show_telemetry: cfg.show_telemetry,
                    work_top_reserve,
                    work_bottom_reserve,
                }
            }
        }
        LayoutMode::UnifiedTop | LayoutMode::UnifiedBottom => {
            // Single-edge 3-island bar
            let h = s(unified_h);
            let gap = s(EDGE_GAP);
            let at_top = cfg.layout == LayoutMode::UnifiedTop;
            let y = if at_top { m.top + gap } else { m.bottom - h - gap };
            let vpad = if h <= s(38) { s(3) } else { s(4) };

            let (
                top_left,
                top_right,
                workspace_rect,
                tray_chevron_rect,
                tray_rects,
                tray_div_x,
                net_rect,
                vol_rect,
                bat_rect,
                lang_rect,
                coucou_rect,
                clock_rect,
                gear_rect,
                desk_rect,
            ) = if mon.primary {
                let tl_w = if cfg.show_telemetry { s(172) } else { s(126) };
                let tl = RECT { left: m.left + margin, top: y, right: m.left + margin + tl_w, bottom: y + h };
                let ws = RECT { left: s(4), top: vpad, right: tl_w - s(4), bottom: h - vpad };

                let mut rx = s(6);
                let tchev = if has_tray_chevron || tray_bar_count > 0 {
                    let r = RECT { left: rx, top: vpad, right: rx + s(22), bottom: h - vpad };
                    rx += s(24);
                    Some(r)
                } else {
                    None
                };
                let max_tray_slots = ((sw / 4 - s(90)) / s(26)).clamp(0, 8) as usize;
                let shown_tray = tray_bar_count.min(max_tray_slots);
                let mut trects = Vec::with_capacity(shown_tray);
                for _ in 0..shown_tray {
                    trects.push(RECT { left: rx, top: vpad, right: rx + s(24), bottom: h - vpad });
                    rx += s(26);
                }
                let tdiv = if tchev.is_some() || !trects.is_empty() {
                    let dx = rx + s(2);
                    rx += s(8);
                    Some(dx)
                } else {
                    None
                };

                let nr = RECT { left: rx, top: vpad, right: rx + s(28), bottom: h - vpad };
                rx += s(29);
                let vr = RECT { left: rx, top: vpad, right: rx + s(28), bottom: h - vpad };
                rx += s(29);
                let br = if has_battery {
                    let r = RECT { left: rx, top: vpad, right: rx + s(38), bottom: h - vpad };
                    rx += s(40);
                    Some(r)
                } else {
                    None
                };
                let lr = if has_lang {
                    let r = RECT { left: rx, top: vpad, right: rx + s(34), bottom: h - vpad };
                    rx += s(36);
                    Some(r)
                } else {
                    None
                };
                rx += s(4);
                let couc_r = if cfg.show_coucou {
                    let r = RECT { left: rx, top: vpad, right: rx + s(28), bottom: h - vpad };
                    rx += s(30);
                    Some(r)
                } else {
                    None
                };
                let clock_w = if cfg.show_seconds { s(102) } else { s(82) };
                let cr = RECT { left: rx, top: vpad, right: rx + clock_w, bottom: h - vpad };
                rx += clock_w + s(2);
                let gr = RECT { left: rx, top: vpad, right: rx + s(28), bottom: h - vpad };
                rx += s(31);
                let (dr, tr_w) = if cfg.show_desktop_btn {
                    let r = RECT { left: rx, top: vpad + s(2), right: rx + s(8), bottom: h - vpad - s(2) };
                    (r, rx + s(12))
                } else {
                    (RECT { left: 0, top: 0, right: 0, bottom: 0 }, rx + s(4))
                };
                let tr = RECT { left: m.right - margin - tr_w, top: y, right: m.right - margin, bottom: y + h };
                (Some(tl), Some(tr), ws, tchev, trects, tdiv, nr, vr, br, lr, couc_r, cr, gr, dr)
            } else {
                let tr_w = if cfg.show_seconds { s(108) } else { s(96) };
                let tr = RECT { left: m.right - margin - tr_w, top: y, right: m.right - margin, bottom: y + h };
                let cr = RECT { left: s(6), top: vpad, right: tr_w - s(6), bottom: h - vpad };
                let zero = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                (None, Some(tr), zero, None, Vec::new(), None, zero, zero, None, None, None, cr, zero, zero)
            };

            let min_x = top_left.map_or(m.left + margin, |r| r.right + s(ISLAND_GAP));
            let max_x = top_right.map_or(m.right - margin, |r| r.left - s(ISLAND_GAP));
            let avail = (max_x - min_x).max(s(140));
            let dock_pad = s(pad_cfg);
            let has_search_in_dock = mon.primary && cfg.show_search;
            let has_taskview_in_dock = mon.primary && cfg.show_taskview;
            let fixed_slots = 1 + i32::from(has_search_in_dock) + i32::from(has_taskview_in_dock);
            let total_slots = fixed_slots + app_count as i32;
            let mut btn_gap = s(gap_cfg);
            let div_extra = if app_count > 0 || fixed_slots > 1 { s(10) } else { 0 };
            let usable = (avail - 2 * dock_pad - div_extra).max(s(80));
            let raw_bw = ((usable + btn_gap) / total_slots.max(1)) - btn_gap;
            let btn_w = raw_bw.clamp(s(22), s(max_bw_cfg));
            if raw_bw < s(22) {
                btn_gap = s(2);
            }
            let content_w = total_slots * btn_w + (total_slots - 1).max(0) * btn_gap + div_extra;
            let dock_w = (2 * dock_pad + content_w).min(avail);
            let dock_x = if center_align {
                (m.left + (sw - dock_w) / 2).clamp(min_x, (max_x - dock_w).max(min_x))
            } else {
                min_x
            };
            let dock = RECT { left: dock_x, top: y, right: dock_x + dock_w, bottom: y + h };
            let dock_div_x = dock_pad + fixed_slots * btn_w + (fixed_slots - 1) * btn_gap + s(4);
            let apps_x0 = dock_div_x + s(6);
            let reserve = gap + h + s(4);

            MonitorLayout {
                top_left,
                top_center: None,
                top_right,
                dock: Some(dock),
                workspace_rect,
                clock_rect,
                search_rect: None,
                coucou_rect,
                tray_chevron_rect,
                tray_rects,
                tray_div_x,
                net_rect,
                vol_rect,
                bat_rect,
                lang_rect,
                gear_rect,
                desk_rect,
                dock_pad,
                dock_h: h,
                btn_w,
                btn_gap,
                icon_px: s(icon_px_cfg).clamp(14, (btn_w - s(6)).max(14)),
                dock_div_x,
                apps_x0,
                has_search_in_dock,
                has_taskview_in_dock,
                show_telemetry: cfg.show_telemetry,
                work_top_reserve: if at_top { reserve } else { 0 },
                work_bottom_reserve: if at_top { 0 } else { reserve },
            }
        }
    }
}

pub fn hit_test(
    kind: SurfaceKind,
    lay: &MonitorLayout,
    x: i32,
    y: i32,
    app_count: usize,
) -> HitZone {
    let in_r = |r: RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
    match kind {
        SurfaceKind::TopLeft => {
            if in_r(lay.workspace_rect) { HitZone::Workspace } else { HitZone::None }
        }
        SurfaceKind::TopCenter => {
            if lay.coucou_rect.is_some_and(in_r) {
                HitZone::Coucou
            } else if in_r(lay.clock_rect) {
                HitZone::Clock
            } else if lay.search_rect.is_some_and(in_r) {
                HitZone::Search
            } else {
                HitZone::None
            }
        }
        SurfaceKind::TopRight => {
            if lay.tray_chevron_rect.is_some_and(in_r) {
                return HitZone::TrayChevron;
            }
            for (ti, &tr) in lay.tray_rects.iter().enumerate() {
                if in_r(tr) {
                    return HitZone::TrayIcon(ti);
                }
            }
            if lay.net_rect.right > lay.net_rect.left && in_r(lay.net_rect) {
                HitZone::Network
            } else if lay.vol_rect.right > lay.vol_rect.left && in_r(lay.vol_rect) {
                HitZone::Volume
            } else if lay.bat_rect.is_some_and(in_r) {
                HitZone::Battery
            } else if lay.lang_rect.is_some_and(in_r) {
                HitZone::InputLang
            } else if lay.top_center.is_none() && lay.coucou_rect.is_some_and(in_r) {
                HitZone::Coucou
            } else if lay.gear_rect.right > lay.gear_rect.left && in_r(lay.gear_rect) {
                HitZone::Gear
            } else if lay.desk_rect.right > lay.desk_rect.left && in_r(lay.desk_rect) {
                HitZone::ShowDesktop
            } else if lay.top_center.is_none() && in_r(lay.clock_rect) {
                HitZone::Clock
            } else {
                HitZone::None
            }
        }
        SurfaceKind::Dock => {
            if y < 0 || y >= lay.dock_h {
                return HitZone::None;
            }
            let mut slot_zones = vec![HitZone::Start];
            if lay.has_search_in_dock {
                slot_zones.push(HitZone::Search);
            }
            if lay.has_taskview_in_dock {
                slot_zones.push(HitZone::TaskView);
            }
            for (idx, z) in slot_zones.into_iter().enumerate() {
                let sx = lay.dock_pad + idx as i32 * (lay.btn_w + lay.btn_gap);
                if x >= sx && x < sx + lay.btn_w {
                    return z;
                }
            }
            let dx = x - lay.apps_x0;
            if dx >= 0 {
                let step = lay.btn_w + lay.btn_gap;
                let idx = dx / step;
                if dx % step < lay.btn_w && (idx as usize) < app_count {
                    return HitZone::App(idx as usize);
                }
            }
            HitZone::None
        }
    }
}

pub fn zone_anchor(kind: SurfaceKind, lay: &MonitorLayout, zone: HitZone) -> (i32, i32, RECT) {
    let base = lay.rect_for(kind).unwrap_or(RECT { left: 0, top: 0, right: 0, bottom: 0 });
    let cx = match (kind, zone) {
        (SurfaceKind::TopLeft, HitZone::Workspace) => base.left + (base.right - base.left) / 2,
        (SurfaceKind::TopCenter, HitZone::Clock) => base.left + (lay.clock_rect.left + lay.clock_rect.right) / 2,
        (SurfaceKind::TopCenter, HitZone::Search) => base.left + lay.search_rect.map_or(0, |r| (r.left + r.right) / 2),
        (SurfaceKind::TopCenter, HitZone::Coucou) => base.left + lay.coucou_rect.map_or(0, |r| (r.left + r.right) / 2),
        (SurfaceKind::TopRight, HitZone::Coucou) => base.left + lay.coucou_rect.map_or(0, |r| (r.left + r.right) / 2),
        (SurfaceKind::TopRight, HitZone::TrayChevron) => base.left + lay.tray_chevron_rect.map_or(0, |r| (r.left + r.right) / 2),
        (SurfaceKind::TopRight, HitZone::TrayIcon(i)) => base.left + lay.tray_rects.get(i).map_or(0, |r| (r.left + r.right) / 2),
        (SurfaceKind::TopRight, HitZone::InputLang) => base.left + lay.lang_rect.map_or(0, |r| (r.left + r.right) / 2),
        (SurfaceKind::TopRight, HitZone::Network) => base.left + (lay.net_rect.left + lay.net_rect.right) / 2,
        (SurfaceKind::TopRight, HitZone::Volume) => base.left + (lay.vol_rect.left + lay.vol_rect.right) / 2,
        (SurfaceKind::TopRight, HitZone::Battery) => base.left + lay.bat_rect.map_or(0, |r| (r.left + r.right) / 2),
        (SurfaceKind::TopRight, HitZone::Gear) => base.left + (lay.gear_rect.left + lay.gear_rect.right) / 2,
        (SurfaceKind::TopRight, HitZone::Clock) => base.left + (lay.clock_rect.left + lay.clock_rect.right) / 2,
        (SurfaceKind::TopRight, HitZone::ShowDesktop) => base.left + (lay.desk_rect.left + lay.desk_rect.right) / 2,
        (SurfaceKind::Dock, HitZone::Start) => base.left + lay.dock_pad + lay.btn_w / 2,
        (SurfaceKind::Dock, HitZone::Search) => base.left + lay.dock_pad + (lay.btn_w + lay.btn_gap) + lay.btn_w / 2,
        (SurfaceKind::Dock, HitZone::TaskView) => {
            let slot = if lay.has_search_in_dock { 2 } else { 1 };
            base.left + lay.dock_pad + slot * (lay.btn_w + lay.btn_gap) + lay.btn_w / 2
        }
        (SurfaceKind::Dock, HitZone::App(i)) => {
            base.left + lay.apps_x0 + i as i32 * (lay.btn_w + lay.btn_gap) + lay.btn_w / 2
        }
        _ => base.left + (base.right - base.left) / 2,
    };
    (cx, base.top, base)
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn paint_top_left(
    surf: &mut Surface,
    text_ren: &TextRenderer,
    lay: &MonitorLayout,
    p: &Palette,
    telem: TelemetrySnapshot,
    workspace: &str,
    hot: HitZone,
    pressed: HitZone,
    scale: f32,
) {
    surf.clear_island(8.0 * scale, p.mica_tint, p.border_argb, p.top_sheen);
    let r = lay.workspace_rect;
    if hot == HitZone::Workspace {
        let fill = if pressed == HitZone::Workspace { p.press_pill } else { p.hover_pill };
        surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, fill);
    }
    let s = |v: i32| sc(v, scale);
    surf.fill_circle(r.left as f32 + 10.0 * scale, surf.h as f32 * 0.5, 3.2 * scale, p.accent);
    if lay.show_telemetry {
        let stat = format!("{}% CPU · {}% RAM", telem.cpu_pct, telem.ram_pct);
        let stat_w = s(118).min((r.right - r.left) * 3 / 5);
        let ws_w = (r.right - r.left - s(32) - stat_w).max(s(32));
        surf.fill_rect(r.right - stat_w - s(10), s(9), 1, surf.h - s(18), p.divider);
        text_ren.draw_text(
            surf,
            FontSlot::SemiBold,
            workspace,
            (r.left + s(18)) as f32,
            r.top as f32,
            ws_w as f32,
            (r.bottom - r.top) as f32,
            p.text,
            TextAlign::Left,
        );
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            &stat,
            (r.right - stat_w - s(6)) as f32,
            r.top as f32,
            stat_w as f32,
            (r.bottom - r.top) as f32,
            p.dim,
            TextAlign::Right,
        );
    } else {
        let ws_w = (r.right - r.left - s(24)).max(s(32));
        text_ren.draw_text(
            surf,
            FontSlot::SemiBold,
            workspace,
            (r.left + s(18)) as f32,
            r.top as f32,
            ws_w as f32,
            (r.bottom - r.top) as f32,
            p.text,
            TextAlign::Left,
        );
    }
}

/// Draws Mochi's signature rounded squircle face with expressive eyes and mood glow.
unsafe fn draw_mochi_emblem(
    surf: &mut Surface,
    x: i32,
    y: i32,
    sz: i32,
    mood: CoucouMood,
    has_unread: bool,
    pulse_tick: u32,
    p: &Palette,
    scale: f32,
) {
    let (body_col, border_col, eye_col) = match mood {
        CoucouMood::Offline => (
            if p.is_light { 0x3000_0000 } else { 0x30FF_FFFF },
            p.divider,
            p.dim,
        ),
        CoucouMood::Ready => (0xFF28_244A, 0xFF81_8CF8, 0xFFF8_FAFC),
        CoucouMood::Music => (0xFF13_342B, 0xFF34_D399, 0xFFA7_F3D0),
        CoucouMood::Focus => (0xFF34_204E, 0xFFC0_84FC, 0xFFF5_D0FE),
        CoucouMood::Water => (0xFF10_3248, 0xFF38_BDF8, 0xFFBA_E6FD),
    };
    let rad = (sz as f32 * 0.34).max(3.5 * scale);
    surf.fill_round_rect(x, y, sz, sz, rad, body_col);
    surf.stroke_round_rect(x, y, sz, sz, rad, (1.1 * scale).max(1.0), border_col);

    // Expressive Mochi eyes (periodic subtle blink every ~16 ticks)
    let blink = (pulse_tick % 19) == 0 && mood != CoucouMood::Offline;
    let eye_w = (sz as f32 * 0.14).max(2.0).round() as i32;
    let eye_h = if blink { 1 } else { (sz as f32 * 0.24).max(2.0).round() as i32 };
    let eye_y = y + (sz - eye_h) / 2;
    let lx = x + (sz as f32 * 0.28).round() as i32 - eye_w / 2;
    let rx = x + (sz as f32 * 0.72).round() as i32 - eye_w / 2;
    surf.fill_round_rect(lx, eye_y, eye_w, eye_h, eye_w as f32 * 0.5, eye_col);
    surf.fill_round_rect(rx, eye_y, eye_w, eye_h, eye_w as f32 * 0.5, eye_col);

    if has_unread {
        surf.fill_circle(
            (x + sz - sc(1, scale)) as f32,
            (y + sc(1, scale)) as f32,
            (2.4 * scale).max(2.0),
            0xFFFB_BF24,
        );
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn paint_top_center(
    surf: &mut Surface,
    text_ren: &TextRenderer,
    lay: &MonitorLayout,
    p: &Palette,
    clock_str: &str,
    coucou: &CoucouSnapshot,
    coucou_hub_open: bool,
    has_unread_notice: bool,
    anim_tick: u32,
    hot: HitZone,
    pressed: HitZone,
    scale: f32,
) {
    surf.clear_island(8.0 * scale, p.mica_tint, p.border_argb, p.top_sheen);
    let s = |v: i32| sc(v, scale);
    if let Some(rs) = lay.search_rect {
        let fill = if hot == HitZone::Search {
            if pressed == HitZone::Search { p.press_pill } else { p.hover_pill }
        } else {
            0x10FF_FFFF
        };
        surf.fill_round_rect(rs.left, rs.top, rs.right - rs.left, rs.bottom - rs.top, 6.0 * scale, fill);
        text_ren.draw_text(
            surf,
            FontSlot::GlyphSmall,
            "\u{E721}",
            (rs.left + s(6)) as f32,
            rs.top as f32,
            s(18) as f32,
            (rs.bottom - rs.top) as f32,
            if hot == HitZone::Search { p.accent } else { p.dim },
            TextAlign::Center,
        );
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            "Search",
            (rs.left + s(25)) as f32,
            rs.top as f32,
            (rs.right - rs.left - s(29)).max(s(20)) as f32,
            (rs.bottom - rs.top) as f32,
            if hot == HitZone::Search { p.text } else { p.dim },
            TextAlign::Left,
        );
        surf.fill_rect(rs.right + s(3), s(8), 1, surf.h - s(16), p.divider);
    }

    let rc = lay.clock_rect;
    if hot == HitZone::Clock {
        let fill = if pressed == HitZone::Clock { p.press_pill } else { p.hover_pill };
        surf.fill_round_rect(rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top, 6.0 * scale, fill);
    }
    text_ren.draw_text(
        surf,
        FontSlot::SemiBold,
        clock_str,
        rc.left as f32 + 4.0 * scale,
        rc.top as f32,
        (rc.right - rc.left) as f32 - 8.0 * scale,
        (rc.bottom - rc.top) as f32,
        p.text,
        TextAlign::Center,
    );

    // Dedicated Coucou Assistant (Mochi) bar directly beside the Calendar/Clock button
    if let Some(rm) = lay.coucou_rect {
        surf.fill_rect(rm.left - s(3), s(8), 1, surf.h - s(16), p.divider);
        let (label, mood) = coucou.pill_summary();
        let pill_fill = if pressed == HitZone::Coucou {
            p.press_pill
        } else if coucou_hub_open {
            p.active_pill
        } else if hot == HitZone::Coucou {
            p.hover_pill
        } else {
            match mood {
                CoucouMood::Music => 0x1A34_D399,
                CoucouMood::Focus => 0x1EC0_84FC,
                CoucouMood::Water => 0x1A38_BDF8,
                CoucouMood::Ready => 0x1681_8CF8,
                CoucouMood::Offline => 0x0C88_8888,
            }
        };
        surf.fill_round_rect(rm.left, rm.top, rm.right - rm.left, rm.bottom - rm.top, 6.0 * scale, pill_fill);
        if coucou_hub_open {
            surf.stroke_round_rect(rm.left, rm.top, rm.right - rm.left, rm.bottom - rm.top, 6.0 * scale, 1.0 * scale, 0xFF81_8CF8);
        }

        let emb = s(18);
        let ex = rm.left + s(6);
        let ey = rm.top + ((rm.bottom - rm.top) - emb) / 2;
        draw_mochi_emblem(surf, ex, ey, emb, mood, has_unread_notice, anim_tick, p, scale);

        let mut tx = ex + emb + s(6);
        if mood == CoucouMood::Music {
            // 3 crisp mini equalizer bars when music is actively playing
            let eq_h = [
                s(5 + ((anim_tick % 3) as i32 * 2)),
                s(9 - ((anim_tick % 2) as i32 * 3)),
                s(6 + (((anim_tick + 1) % 3) as i32 * 2)),
            ];
            let base_y = rm.bottom - s(7);
            for (bi, bh) in eq_h.into_iter().enumerate() {
                surf.fill_round_rect(tx + bi as i32 * s(3), base_y - bh, s(2).max(2), bh, 1.0, 0xFF34_D399);
            }
            tx += s(11);
        }

        let tw = (rm.right - tx - s(6)).max(s(16));
        let tcol = match mood {
            CoucouMood::Offline => p.dim,
            CoucouMood::Music => if p.is_light { 0xFF04_7857 } else { 0xFFA7_F3D0 },
            CoucouMood::Focus => if p.is_light { 0xFF6D_28D9 } else { 0xFFE9_D5FF },
            CoucouMood::Water => if p.is_light { 0xFF03_69A1 } else { 0xFFBA_E6FD },
            CoucouMood::Ready => p.text,
        };
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            &label,
            tx as f32,
            rm.top as f32,
            tw as f32,
            (rm.bottom - rm.top) as f32,
            tcol,
            TextAlign::Left,
        );
    } else if rc.left > lay.search_rect.map_or(s(12), |sr| sr.right + s(32)) {
        // Unified live-Coucou dock bay: draw clean divider between the centered live Mochi pill and Calendar on the right
        surf.fill_rect(rc.left - s(3), s(8), 1, surf.h - s(16), p.divider);
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn paint_top_right(
    surf: &mut Surface,
    text_ren: &TextRenderer,
    lay: &MonitorLayout,
    p: &Palette,
    telem: TelemetrySnapshot,
    tray_bmps: &[Option<Arc<IconBitmap>>],
    tray_fly_open: bool,
    tray_drop_slot: Option<usize>,
    tray_drag_from_slot: Option<usize>,
    tray_hide_hover: bool,
    coucou: &CoucouSnapshot,
    coucou_hub_open: bool,
    has_unread_notice: bool,
    anim_tick: u32,
    bar_at_top: bool,
    lang_code: &str,
    time_str: &str,
    date_str: &str,
    hot: HitZone,
    pressed: HitZone,
    scale: f32,
) {
    surf.clear_island(8.0 * scale, p.mica_tint, p.border_argb, p.top_sheen);
    let s = |v: i32| sc(v, scale);

    // Highlight tray drop zone when dragging an icon out of the overflow flyout onto the main bar
    if tray_drop_slot.is_some() {
        let z_left = lay
            .tray_rects
            .first()
            .map(|r| r.left - s(2))
            .or_else(|| lay.tray_chevron_rect.map(|r| r.right + s(2)))
            .unwrap_or(s(4));
        let z_right = lay
            .tray_div_x
            .map(|dx| dx - s(2))
            .unwrap_or(lay.net_rect.left - s(2));
        if z_right > z_left + s(4) {
            surf.fill_round_rect(z_left, s(2), z_right - z_left, surf.h - s(4), 6.0 * scale, p.active_pill);
            surf.stroke_round_rect(z_left, s(2), z_right - z_left, surf.h - s(4), 6.0 * scale, 1.2 * scale, p.accent);
        }
    }

    // Notification area overflow chevron (^) beside active small tray icons
    if let Some(rc) = lay.tray_chevron_rect {
        if hot == HitZone::TrayChevron || tray_fly_open || tray_hide_hover {
            let f = if tray_hide_hover {
                p.active_pill
            } else if pressed == HitZone::TrayChevron {
                p.press_pill
            } else if tray_fly_open {
                p.active_pill
            } else {
                p.hover_pill
            };
            surf.fill_round_rect(rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top, 6.0 * scale, f);
            if tray_hide_hover {
                surf.stroke_round_rect(rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top, 6.0 * scale, 1.2 * scale, p.accent);
            }
        }
        // Chevron points away from screen edge, flips when overflow flyout is open
        let glyph = if bar_at_top ^ tray_fly_open { "\u{E70D}" } else { "\u{E70E}" };
        text_ren.draw_text(
            surf,
            FontSlot::GlyphSmall,
            glyph,
            rc.left as f32,
            rc.top as f32,
            (rc.right - rc.left) as f32,
            (rc.bottom - rc.top) as f32,
            if tray_hide_hover { p.accent } else if hot == HitZone::TrayChevron || tray_fly_open { p.text } else { p.dim },
            TextAlign::Center,
        );
    }

    // Active background program small icons directly beside the control menu bar
    for (ti, &r) in lay.tray_rects.iter().enumerate() {
        let z = HitZone::TrayIcon(ti);
        let is_being_dragged = tray_drag_from_slot == Some(ti);
        if is_being_dragged {
            surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, p.active_pill);
            surf.stroke_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, 1.0 * scale, p.accent);
        } else if hot == z {
            let f = if pressed == z { p.press_pill } else { p.hover_pill };
            surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, f);
        }
        let icon_alpha = if is_being_dragged { 0.35 } else { 1.0 };
        if let Some(Some(bmp)) = tray_bmps.get(ti) {
            let ix = r.left + ((r.right - r.left) - bmp.w) / 2;
            let iy = r.top + ((r.bottom - r.top) - bmp.h) / 2;
            surf.blit_icon(ix, iy, bmp, icon_alpha);
        } else {
            surf.fill_circle(
                ((r.left + r.right) / 2) as f32,
                ((r.top + r.bottom) / 2) as f32,
                3.5 * scale,
                p.dim,
            );
        }
    }

    // Insertion bar when dragging a hidden icon onto a specific slot in TopRight
    if let Some(slot) = tray_drop_slot {
        let bar_x = if let Some(r) = lay.tray_rects.get(slot) {
            r.left - s(2)
        } else if let Some(last) = lay.tray_rects.last() {
            last.right + s(1)
        } else if let Some(rc) = lay.tray_chevron_rect {
            rc.right + s(3)
        } else {
            s(6)
        };
        surf.fill_round_rect(bar_x, s(6), s(2).max(2), surf.h - s(12), 1.0 * scale, p.accent);
    }

    if let Some(dx) = lay.tray_div_x {
        surf.fill_rect(dx, s(8), 1, surf.h - s(16), p.divider);
    }

    if lay.net_rect.right > lay.net_rect.left {
        let r = lay.net_rect;
        if hot == HitZone::Network {
            let f = if pressed == HitZone::Network { p.press_pill } else { p.hover_pill };
            surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, f);
        }
        text_ren.draw_text(
            surf,
            FontSlot::GlyphSmall,
            if telem.online { "\u{E701}" } else { "\u{F384}" },
            r.left as f32,
            r.top as f32,
            (r.right - r.left) as f32,
            (r.bottom - r.top) as f32,
            if telem.online { p.text } else { p.warn },
            TextAlign::Center,
        );
    }

    if lay.vol_rect.right > lay.vol_rect.left {
        let r = lay.vol_rect;
        if hot == HitZone::Volume {
            let f = if pressed == HitZone::Volume { p.press_pill } else { p.hover_pill };
            surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, f);
        }
        text_ren.draw_text(
            surf,
            FontSlot::GlyphSmall,
            "\u{E994}",
            r.left as f32,
            r.top as f32,
            (r.right - r.left) as f32,
            (r.bottom - r.top) as f32,
            p.text,
            TextAlign::Center,
        );
    }

    if let Some(rb) = lay.bat_rect {
        if hot == HitZone::Battery {
            let f = if pressed == HitZone::Battery { p.press_pill } else { p.hover_pill };
            surf.fill_round_rect(rb.left, rb.top, rb.right - rb.left, rb.bottom - rb.top, 6.0 * scale, f);
        }
        let bcol = if telem.on_ac { p.good } else if telem.bat_pct <= 15 { p.warn } else { p.text };
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            &format!("{}%", telem.bat_pct),
            rb.left as f32,
            rb.top as f32,
            (rb.right - rb.left) as f32,
            (rb.bottom - rb.top) as f32,
            bcol,
            TextAlign::Center,
        );
    }

    if let Some(rl) = lay.lang_rect
        && !lang_code.is_empty()
    {
        if hot == HitZone::InputLang {
            let f = if pressed == HitZone::InputLang { p.press_pill } else { p.hover_pill };
            surf.fill_round_rect(rl.left, rl.top, rl.right - rl.left, rl.bottom - rl.top, 6.0 * scale, f);
        }
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            lang_code,
            rl.left as f32,
            rl.top as f32,
            (rl.right - rl.left) as f32,
            (rl.bottom - rl.top) as f32,
            p.text,
            TextAlign::Center,
        );
    }

    // Unified modes render compact Coucou emblem beside clock inside TopRight
    if lay.top_center.is_none()
        && let Some(rm) = lay.coucou_rect
    {
        if hot == HitZone::Coucou || coucou_hub_open {
            let f = if pressed == HitZone::Coucou {
                p.press_pill
            } else if coucou_hub_open {
                p.active_pill
            } else {
                p.hover_pill
            };
            surf.fill_round_rect(rm.left, rm.top, rm.right - rm.left, rm.bottom - rm.top, 6.0 * scale, f);
        }
        let (_, mood) = coucou.pill_summary();
        let emb = s(18);
        let ex = rm.left + ((rm.right - rm.left) - emb) / 2;
        let ey = rm.top + ((rm.bottom - rm.top) - emb) / 2;
        draw_mochi_emblem(surf, ex, ey, emb, mood, has_unread_notice, anim_tick, p, scale);
    }

    // Unified modes render clock inside TopRight; Split renders clock in TopCenter
    if lay.top_center.is_none() && lay.clock_rect.right > lay.clock_rect.left {
        let rc = lay.clock_rect;
        if hot == HitZone::Clock {
            let f = if pressed == HitZone::Clock { p.press_pill } else { p.hover_pill };
            surf.fill_round_rect(rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top, 6.0 * scale, f);
        }
        if surf.h <= s(38) {
            // Compact unified bar: crisp single-line time so text never crowds vertically
            text_ren.draw_text(
                surf,
                FontSlot::SemiBold,
                time_str,
                rc.left as f32,
                rc.top as f32,
                (rc.right - rc.left) as f32,
                (rc.bottom - rc.top) as f32,
                p.text,
                TextAlign::Center,
            );
        } else {
            let mid = (rc.top + rc.bottom) / 2;
            text_ren.draw_text(
                surf,
                FontSlot::SemiBold,
                time_str,
                rc.left as f32,
                rc.top as f32,
                (rc.right - rc.left) as f32,
                (mid - rc.top + s(2)) as f32,
                p.text,
                TextAlign::Center,
            );
            text_ren.draw_text(
                surf,
                FontSlot::Small,
                date_str,
                rc.left as f32,
                (mid - s(1)) as f32,
                (rc.right - rc.left) as f32,
                (rc.bottom - mid + s(1)) as f32,
                p.dim,
                TextAlign::Center,
            );
        }
    }

    if lay.gear_rect.right > lay.gear_rect.left {
        surf.fill_rect(lay.gear_rect.left - s(4), s(8), 1, surf.h - s(16), p.divider);
        let rg = lay.gear_rect;
        if hot == HitZone::Gear {
            let f = if pressed == HitZone::Gear { p.press_pill } else { p.hover_pill };
            surf.fill_round_rect(rg.left, rg.top, rg.right - rg.left, rg.bottom - rg.top, 6.0 * scale, f);
        }
        text_ren.draw_text(
            surf,
            FontSlot::GlyphSmall,
            "\u{E713}",
            rg.left as f32,
            rg.top as f32,
            (rg.right - rg.left) as f32,
            (rg.bottom - rg.top) as f32,
            if hot == HitZone::Gear { p.text } else { p.dim },
            TextAlign::Center,
        );
    }

    if lay.desk_rect.right > lay.desk_rect.left {
        let rd = lay.desk_rect;
        if hot == HitZone::ShowDesktop {
            let f = if pressed == HitZone::ShowDesktop { p.press_pill } else { p.hover_pill };
            surf.fill_round_rect(rd.left, rd.top, rd.right - rd.left, rd.bottom - rd.top, 5.0 * scale, f);
        }
        let col = if hot == HitZone::ShowDesktop { p.accent } else { p.divider };
        let bar_x = rd.left + (rd.right - rd.left - s(3)) / 2;
        surf.fill_round_rect(bar_x, rd.top + s(5), s(3), rd.bottom - rd.top - s(10), 1.5 * scale, col);
    }
}

#[derive(Clone, Copy)]
pub struct DragVisual {
    pub idx: usize,
    pub cur_x: i32,
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn paint_dock(
    surf: &mut Surface,
    text_ren: &TextRenderer,
    lay: &MonitorLayout,
    p: &Palette,
    items: &[AppItem],
    hot: HitZone,
    pressed: HitZone,
    drag: Option<DragVisual>,
    icons: &IconCache,
    notify_hwnd: HWND,
    notify_msg: u32,
    scale: f32,
) {
    surf.clear_island(8.0 * scale, p.acrylic_tint, p.border_argb, p.top_sheen);
    let s = |v: i32| sc(v, scale);
    let compact = lay.dock_h <= s(38);
    let top = if compact { s(3) } else { s(4) };
    let bh = lay.dock_h - top * 2;
    let mid_y = lay.dock_h / 2;
    let icon_sz = lay.icon_px.clamp(14, (lay.btn_w - s(6)).max(14));
    let pill_rad = if compact { 6.0 * scale } else { 8.0 * scale };
    let glyph_slot = if compact { FontSlot::GlyphSmall } else { FontSlot::Glyph };

    // Fixed utility slots (Start + optional Search + optional TaskView)
    let mut fixed = vec![HitZone::Start];
    if lay.has_search_in_dock {
        fixed.push(HitZone::Search);
    }
    if lay.has_taskview_in_dock {
        fixed.push(HitZone::TaskView);
    }
    for (idx, z) in fixed.iter().enumerate() {
        let bx = lay.dock_pad + idx as i32 * (lay.btn_w + lay.btn_gap);
        let is_hot = hot == *z;
        let is_press = pressed == *z;
        if is_hot {
            let fill = if is_press { p.press_pill } else { p.hover_pill };
            surf.fill_round_rect(bx, top, lay.btn_w, bh, pill_rad, fill);
        }
        match z {
            HitZone::Start => {
                let base_tile = (icon_sz / 3).max(s(5));
                let tile = if is_press { (base_tile - s(1)).max(s(4)) } else if is_hot { base_tile + s(1) } else { base_tile };
                let tgap = s(2).max(1);
                let cx = bx + lay.btn_w / 2;
                for (k, (dx, dy)) in [(-1, -1), (0, -1), (-1, 0), (0, 0)].into_iter().enumerate() {
                    let tx = cx + if dx < 0 { -tile - tgap / 2 } else { tgap / 2 };
                    let ty = mid_y + if dy < 0 { -tile - tgap / 2 } else { tgap / 2 };
                    surf.fill_round_rect(tx, ty, tile, tile, (1.5 * scale).max(1.0), p.start_tiles[k]);
                }
            }
            HitZone::Search => {
                text_ren.draw_text(
                    surf,
                    glyph_slot,
                    "\u{E721}",
                    bx as f32,
                    top as f32,
                    lay.btn_w as f32,
                    bh as f32,
                    if is_hot { p.text } else { p.dim },
                    TextAlign::Center,
                );
            }
            HitZone::TaskView => {
                text_ren.draw_text(
                    surf,
                    glyph_slot,
                    "\u{E7C4}",
                    bx as f32,
                    top as f32,
                    lay.btn_w as f32,
                    bh as f32,
                    if is_hot { p.text } else { p.dim },
                    TextAlign::Center,
                );
            }
            _ => {}
        }
    }

    if !items.is_empty() || fixed.len() > 1 {
        let div_pad = if compact { s(8) } else { s(12) };
        surf.fill_rect(lay.dock_div_x, div_pad, 1, (lay.dock_h - div_pad * 2).max(8), p.divider);
    }

    // App icons (draw dragged slot last on top so it glides smoothly over neighbors)
    let draw_app_slot = |surf: &mut Surface, i: usize, item: &AppItem, ax: i32, is_dragged: bool| {
        let z = HitZone::App(i);
        let is_hot = hot == z;
        let is_press = pressed == z;
        if is_dragged {
            surf.fill_round_rect(ax, top, lay.btn_w, bh, pill_rad, p.active_pill);
            surf.stroke_round_rect(ax, top, lay.btn_w, bh, pill_rad, 1.2 * scale, p.accent);
        } else if is_press {
            surf.fill_round_rect(ax, top, lay.btn_w, bh, pill_rad, p.press_pill);
        } else if item.flashing {
            surf.fill_round_rect(ax, top, lay.btn_w, bh, pill_rad, 0x38FF_8C00);
        } else if item.active {
            surf.fill_round_rect(ax, top, lay.btn_w, bh, pill_rad, p.active_pill);
        } else if is_hot {
            surf.fill_round_rect(ax, top, lay.btn_w, bh, pill_rad, p.hover_pill);
        }

        let fallback_hwnd = item.wins.first().copied().map_or(0usize, |w| w as usize);
        let bmp: Option<Arc<IconBitmap>> = if !item.icon_path.is_empty() || fallback_hwnd != 0 {
            icons.get_or_request(
                IconRequest::App {
                    key: item.icon_key.clone(),
                    path: item.icon_path.clone(),
                    fallback_hwnd,
                },
                icon_sz,
                notify_hwnd,
                notify_msg,
            )
        } else {
            None
        };

        let y_bias = if compact { s(1) } else { s(2) };
        if let Some(bmp) = bmp {
            let press_y = if is_press && !is_dragged { s(1) } else if is_dragged { -s(1) } else { 0 };
            let ix = ax + (lay.btn_w - bmp.w) / 2;
            let iy = mid_y - bmp.h / 2 - y_bias + press_y;
            surf.blit_icon(ix, iy, &bmp, 1.0);
        } else {
            // Crisp monogram badge for apps without an icon resource
            let badge = icon_sz.clamp(s(16), s(24));
            let bx = ax + (lay.btn_w - badge) / 2;
            let by = mid_y - badge / 2 - y_bias;
            let badge_fill = if p.is_light { 0x2200_0000 } else { 0x24FF_FFFF };
            surf.fill_round_rect(bx, by, badge, badge, 5.0 * scale, badge_fill);
            let ch = item.label.chars().next().unwrap_or('?').to_ascii_uppercase().to_string();
            text_ren.draw_text(
                surf,
                FontSlot::SemiBold,
                &ch,
                bx as f32,
                by as f32,
                badge as f32,
                badge as f32,
                p.text,
                TextAlign::Center,
            );
        }

        if !item.wins.is_empty() {
            let pill_h = if compact { s(2).max(2) } else { s(3) };
            let pill_w = if item.active || item.flashing {
                if compact { s(12) } else { s(14) }
            } else {
                s(6)
            };
            let pill_col = if item.flashing {
                0xFFFF_8C00
            } else if item.active {
                p.accent
            } else if item.minimized {
                (p.dim & 0x00FF_FFFF) | 0x7700_0000
            } else {
                p.dim
            };
            let py = top + bh - pill_h;
            surf.fill_round_rect(ax + (lay.btn_w - pill_w) / 2, py, pill_w, pill_h, pill_h as f32 * 0.5, pill_col);
            if item.wins.len() > 1 {
                surf.fill_round_rect(ax + (lay.btn_w + pill_w) / 2 + s(2), py, pill_h, pill_h, pill_h as f32 * 0.5, pill_col);
            }
        }
    };

    for (i, item) in items.iter().enumerate() {
        if drag.is_some_and(|d| d.idx == i) {
            continue;
        }
        let ax = lay.apps_x0 + i as i32 * (lay.btn_w + lay.btn_gap);
        draw_app_slot(surf, i, item, ax, false);
    }
    if let Some(d) = drag
        && let Some(item) = items.get(d.idx)
    {
        let min_x = lay.apps_x0;
        let max_x = (surf.w - lay.dock_pad - lay.btn_w).max(min_x);
        let ax = (d.cur_x - lay.btn_w / 2).clamp(min_x, max_x);
        draw_app_slot(surf, d.idx, item, ax, true);
    }
}

pub const POPUP_W: i32 = 556;
pub const POPUP_HEAD: i32 = 40;
pub const POPUP_SEC_H: i32 = 22;
pub const POPUP_ROW: i32 = 30;
pub const POPUP_FOOT_H: i32 = 40;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PopupAction {
    TaskbarSize,
    Layout,
    DockEdge,
    Alignment,
    Displays,
    Theme,
    Glass,
    AutoHideDock,
    BottomModules,
    GamingMode,
    TrayMode,
    ShowPreviews,
    ShowSeconds,
    ShowSearch,
    ShowCoucou,
    ShowTaskView,
    ShowTelemetry,
    ShowDesktopBtn,
    RefreshAll,
    WinTaskbarSettings,
    OpenBoostUi,
    RestoreStock,
}

pub struct PopupItem {
    pub action: PopupAction,
    pub label: &'static str,
    pub value: String,
    pub accent: bool,
    pub danger: bool,
    pub rect: RECT,
}

pub struct PopupLayout {
    pub w: i32,
    pub h: i32,
    pub left_hdr: RECT,
    pub right_hdr: RECT,
    pub col_div_x: i32,
    pub items: Vec<PopupItem>,
}

pub fn compute_popup_layout(cfg: &Config, scale: f32) -> PopupLayout {
    let s = |v: i32| sc(v, scale);
    let w = s(POPUP_W);
    let pad = s(12);
    let col_gap = s(16);
    let col_w = (w - 2 * pad - col_gap) / 2;
    let left_x = pad;
    let right_x = pad + col_w + col_gap;
    let col_div_x = pad + col_w + col_gap / 2;

    let left_rows: [(PopupAction, &'static str, String, bool); 10] = [
        (PopupAction::TaskbarSize, "Taskbar Size", cfg.dock_size.display_name().into(), true),
        (PopupAction::Layout, "Layout", cfg.layout.display_name().into(), true),
        (PopupAction::DockEdge, "Dock Edge", cfg.dock_edge.display_name().into(), false),
        (PopupAction::Alignment, "Alignment", cfg.align.display_name().into(), false),
        (PopupAction::Displays, "Displays", cfg.multimon.display_name().into(), false),
        (PopupAction::Theme, "Theme", cfg.theme.display_name().into(), false),
        (PopupAction::Glass, "Glass Style", cfg.glass.display_name().into(), false),
        (PopupAction::AutoHideDock, "Auto-Hide Dock", if cfg.autohide { "On" } else { "Off" }.into(), cfg.autohide),
        (
            PopupAction::BottomModules,
            "Status Strip",
            if cfg.status_autohide { "Hover Edge" } else { "Always On" }.into(),
            cfg.status_autohide,
        ),
        (
            PopupAction::GamingMode,
            "Gaming Mode",
            cfg.gaming_mode.display_name().into(),
            cfg.gaming_mode != GamingModePref::Off,
        ),
    ];

    let right_rows: [(PopupAction, &'static str, String, bool); 10] = [
        (PopupAction::ShowCoucou, "Coucou Assistant", if cfg.show_coucou { "On" } else { "Off" }.into(), cfg.show_coucou),
        (PopupAction::TrayMode, "Tray Icons on Bar", cfg.tray_mode.display_name().into(), true),
        (PopupAction::ShowPreviews, "Window Previews", if cfg.show_previews { "On" } else { "Off" }.into(), cfg.show_previews),
        (PopupAction::ShowSeconds, "Clock Seconds", if cfg.show_seconds { "On" } else { "Off" }.into(), cfg.show_seconds),
        (PopupAction::ShowSearch, "Search Module", if cfg.show_search { "On" } else { "Off" }.into(), cfg.show_search),
        (PopupAction::ShowTaskView, "Task View Button", if cfg.show_taskview { "On" } else { "Off" }.into(), cfg.show_taskview),
        (PopupAction::ShowTelemetry, "CPU / RAM Stats", if cfg.show_telemetry { "On" } else { "Off" }.into(), cfg.show_telemetry),
        (PopupAction::ShowDesktopBtn, "Show Desktop Strip", if cfg.show_desktop_btn { "On" } else { "Off" }.into(), cfg.show_desktop_btn),
        (PopupAction::RefreshAll, "Refresh Pins & Tray", "↻".into(), false),
        (PopupAction::WinTaskbarSettings, "Windows Settings", "›".into(), false),
    ];

    let hdr_y = s(POPUP_HEAD + 4);
    let left_hdr = RECT { left: left_x + s(6), top: hdr_y, right: left_x + col_w - s(6), bottom: hdr_y + s(POPUP_SEC_H) };
    let right_hdr = RECT { left: right_x + s(6), top: hdr_y, right: right_x + col_w - s(6), bottom: hdr_y + s(POPUP_SEC_H) };
    let rows_y0 = hdr_y + s(POPUP_SEC_H + 2);
    let rh = s(POPUP_ROW - 2);

    let mut items = Vec::with_capacity(21);
    for (idx, (action, label, value, accent)) in left_rows.into_iter().enumerate() {
        let ry = rows_y0 + idx as i32 * s(POPUP_ROW);
        items.push(PopupItem {
            action,
            label,
            value,
            accent,
            danger: false,
            rect: RECT { left: left_x, top: ry, right: left_x + col_w, bottom: ry + rh },
        });
    }
    for (idx, (action, label, value, accent)) in right_rows.into_iter().enumerate() {
        let ry = rows_y0 + idx as i32 * s(POPUP_ROW);
        items.push(PopupItem {
            action,
            label,
            value,
            accent,
            danger: false,
            rect: RECT { left: right_x, top: ry, right: right_x + col_w, bottom: ry + rh },
        });
    }

    let foot_y = rows_y0 + 10 * s(POPUP_ROW) + s(10);
    let foot_h = s(POPUP_FOOT_H - 8);
    items.push(PopupItem {
        action: PopupAction::OpenBoostUi,
        label: "Open GP PC Boost UI",
        value: "›".into(),
        accent: false,
        danger: false,
        rect: RECT { left: left_x, top: foot_y, right: left_x + col_w, bottom: foot_y + foot_h },
    });
    items.push(PopupItem {
        action: PopupAction::RestoreStock,
        label: "Restore Stock Taskbar",
        value: "Exit".into(),
        accent: false,
        danger: true,
        rect: RECT { left: right_x, top: foot_y, right: right_x + col_w, bottom: foot_y + foot_h },
    });

    let h = foot_y + foot_h + s(12);
    PopupLayout { w, h, left_hdr, right_hdr, col_div_x, items }
}

pub fn hit_test_popup(lay: &PopupLayout, x: i32, y: i32) -> Option<PopupAction> {
    lay.items
        .iter()
        .find(|it| x >= it.rect.left && x < it.rect.right && y >= it.rect.top && y < it.rect.bottom)
        .map(|it| it.action)
}

pub unsafe fn paint_popup(
    surf: &mut Surface,
    text_ren: &TextRenderer,
    cfg: &Config,
    p: &Palette,
    hover_act: Option<PopupAction>,
    scale: f32,
) {
    surf.clear_island(8.0 * scale, p.acrylic_tint, p.border_argb, p.top_sheen);
    let lay = compute_popup_layout(cfg, scale);
    let s = |v: i32| sc(v, scale);
    let w = surf.w;

    text_ren.draw_text(
        surf,
        FontSlot::SemiBold,
        "Taskbar Settings",
        s(16) as f32,
        s(6) as f32,
        s(180) as f32,
        s(POPUP_HEAD - 8) as f32,
        p.text,
        TextAlign::Left,
    );
    let summary = format!("{}  ·  {}", cfg.layout.display_name(), cfg.dock_size.display_name());
    text_ren.draw_text(
        surf,
        FontSlot::Small,
        &summary,
        s(200) as f32,
        s(6) as f32,
        (w - s(216)) as f32,
        s(POPUP_HEAD - 8) as f32,
        p.dim,
        TextAlign::Right,
    );
    surf.fill_rect(s(12), s(POPUP_HEAD - 2), w - s(24), 1, p.divider);

    text_ren.draw_text(
        surf,
        FontSlot::Small,
        "APPEARANCE & BEHAVIOR",
        lay.left_hdr.left as f32,
        lay.left_hdr.top as f32,
        (lay.left_hdr.right - lay.left_hdr.left) as f32,
        (lay.left_hdr.bottom - lay.left_hdr.top) as f32,
        p.dim,
        TextAlign::Left,
    );
    text_ren.draw_text(
        surf,
        FontSlot::Small,
        "MODULES & SYSTEM",
        lay.right_hdr.left as f32,
        lay.right_hdr.top as f32,
        (lay.right_hdr.right - lay.right_hdr.left) as f32,
        (lay.right_hdr.bottom - lay.right_hdr.top) as f32,
        p.dim,
        TextAlign::Left,
    );

    // Subtle vertical divider between the two settings columns
    let div_y0 = lay.left_hdr.top + s(4);
    let div_y1 = lay.items[18].rect.bottom;
    surf.fill_rect(lay.col_div_x, div_y0, 1, (div_y1 - div_y0).max(1), p.divider);
    surf.fill_rect(s(12), div_y1 + s(5), w - s(24), 1, p.divider);

    for item in &lay.items {
        let r = item.rect;
        let is_hot = hover_act == Some(item.action);
        let is_footer = matches!(item.action, PopupAction::OpenBoostUi | PopupAction::RestoreStock);
        if is_hot {
            let fill = if item.danger { 0x30F8_7171 } else { p.hover_pill };
            surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, fill);
        } else if is_footer {
            let base = if item.danger { 0x16F8_7171 } else if p.is_light { 0x0C00_0000 } else { 0x10FF_FFFF };
            surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, base);
        }

        let col = if item.danger { 0xFFF8_7171 } else { p.text };
        let rw = r.right - r.left;
        let val_w = s(96).min(rw / 2);
        text_ren.draw_text(
            surf,
            FontSlot::Regular,
            item.label,
            (r.left + s(8)) as f32,
            r.top as f32,
            (rw - val_w - s(12)).max(s(40)) as f32,
            (r.bottom - r.top) as f32,
            col,
            TextAlign::Left,
        );
        let vcol = if item.danger {
            0xFFF8_7171
        } else if item.accent {
            p.accent
        } else {
            p.dim
        };
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            &item.value,
            (r.right - val_w - s(8)) as f32,
            r.top as f32,
            val_w as f32,
            (r.bottom - r.top) as f32,
            vcol,
            TextAlign::Right,
        );
    }
}

pub unsafe fn paint_tooltip(surf: &mut Surface, text_ren: &TextRenderer, p: &Palette, msg: &str, scale: f32) {
    surf.clear_island(8.0 * scale, p.acrylic_tint, p.border_argb, p.top_sheen);
    let pad = sc(8, scale) as f32;
    text_ren.draw_text(
        surf,
        FontSlot::Small,
        msg,
        pad,
        0.0,
        (surf.w as f32 - 2.0 * pad).max(1.0),
        surf.h as f32,
        p.text,
        TextAlign::Center,
    );
}

#[derive(Clone)]
pub struct PreviewCard {
    pub hwnd: HWND,
    pub title: String,
    pub card_rect: RECT,
    pub close_rect: RECT,
    pub thumb_rect: RECT,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PreviewHit {
    None,
    Card(usize),
    Close(usize),
}

#[derive(Clone)]
pub struct PreviewLayout {
    pub win_rect: RECT,
    pub cards: Vec<PreviewCard>,
}

pub fn compute_preview_layout(
    mon_rect: RECT,
    anchor_cx: i32,
    dock_rect: RECT,
    entries: &[(HWND, String)],
    scale: f32,
) -> PreviewLayout {
    let s = |v: i32| sc(v, scale);
    let count = entries.len().clamp(1, 5) as i32;
    let pad = s(8);
    let gap = s(8);
    let max_total = (mon_rect.right - mon_rect.left - s(24)).max(s(220));
    let raw_cw = ((max_total - 2 * pad - (count - 1) * gap) / count).clamp(s(170), s(232));
    let card_h = s(158);
    let head_h = s(28);
    let total_w = 2 * pad + count * raw_cw + (count - 1) * gap;
    let total_h = 2 * pad + card_h;

    let x = (anchor_cx - total_w / 2).clamp(mon_rect.left + s(8), (mon_rect.right - total_w - s(8)).max(mon_rect.left + s(8)));
    let y = if dock_rect.top < (mon_rect.top + mon_rect.bottom) / 2 {
        dock_rect.bottom + s(8)
    } else {
        dock_rect.top - total_h - s(8)
    };

    let mut cards = Vec::with_capacity(count as usize);
    for (i, (hw, title)) in entries.iter().take(5).enumerate() {
        let cx = pad + i as i32 * (raw_cw + gap);
        let cy = pad;
        let card_rect = RECT { left: cx, top: cy, right: cx + raw_cw, bottom: cy + card_h };
        let close_rect = RECT {
            left: cx + raw_cw - s(24),
            top: cy + s(4),
            right: cx + raw_cw - s(4),
            bottom: cy + s(24),
        };
        let thumb_rect = RECT {
            left: cx + s(6),
            top: cy + head_h + s(2),
            right: cx + raw_cw - s(6),
            bottom: cy + card_h - s(6),
        };
        cards.push(PreviewCard {
            hwnd: *hw,
            title: title.clone(),
            card_rect,
            close_rect,
            thumb_rect,
        });
    }
    PreviewLayout {
        win_rect: RECT { left: x, top: y, right: x + total_w, bottom: y + total_h },
        cards,
    }
}

pub fn hit_test_preview(lay: &PreviewLayout, x: i32, y: i32) -> PreviewHit {
    let in_r = |r: RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
    for (i, c) in lay.cards.iter().enumerate() {
        if in_r(c.close_rect) {
            return PreviewHit::Close(i);
        }
        if in_r(c.card_rect) {
            return PreviewHit::Card(i);
        }
    }
    PreviewHit::None
}

pub unsafe fn paint_preview(
    surf: &mut Surface,
    text_ren: &TextRenderer,
    lay: &PreviewLayout,
    p: &Palette,
    icon_bmp: Option<&IconBitmap>,
    hover: PreviewHit,
    scale: f32,
) {
    // Opaque Fluent 2 surface so DwmRegisterThumbnail composites cleanly at 60fps on top
    let bg = if p.is_light { 0xFFF3_F5FA } else { 0xFF19_1C25 };
    let card_bg = if p.is_light { 0xFFFF_FFFF } else { 0xFF21_2531 };
    let well_bg = if p.is_light { 0xFFE6_EAF2 } else { 0xFF0E_1017 };
    surf.clear_glass(bg, p.top_sheen);
    let s = |v: i32| sc(v, scale);

    for (i, c) in lay.cards.iter().enumerate() {
        let r = c.card_rect;
        let is_card_hot = matches!(hover, PreviewHit::Card(hi) | PreviewHit::Close(hi) if hi == i);
        let fill = if is_card_hot {
            if p.is_light { 0xFFFA_FBFE } else { 0xFF27_2C3A }
        } else {
            card_bg
        };
        surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 7.0 * scale, fill);
        let border = if is_card_hot { p.accent } else { p.border_argb };
        surf.stroke_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 7.0 * scale, 1.0, border);

        // Header: mini icon (or accent dot) + full window title + close X
        let mut tx = r.left + s(8);
        if let Some(bmp) = icon_bmp {
            let iy = r.top + (s(28) - bmp.h).max(0) / 2;
            surf.blit_icon(tx, iy, bmp, 1.0);
            tx += bmp.w + s(6);
        } else {
            surf.fill_circle(tx as f32 + 5.0 * scale, (r.top + s(14)) as f32, 3.5 * scale, p.accent);
            tx += s(14);
        }
        let tw = (c.close_rect.left - tx - s(4)).max(s(24));
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            &c.title,
            tx as f32,
            (r.top + s(2)) as f32,
            tw as f32,
            s(24) as f32,
            p.text,
            TextAlign::Left,
        );

        let cr = c.close_rect;
        let close_hot = hover == PreviewHit::Close(i);
        if close_hot {
            surf.fill_round_rect(cr.left, cr.top, cr.right - cr.left, cr.bottom - cr.top, 4.0 * scale, 0xFFE8_1123);
        }
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            "×",
            cr.left as f32,
            cr.top as f32,
            (cr.right - cr.left) as f32,
            (cr.bottom - cr.top) as f32,
            if close_hot { 0xFFFF_FFFF } else { p.dim },
            TextAlign::Center,
        );

        // Recessed thumbnail well (live DWM thumbnail composites directly inside this rect;
        // minimized or non-composited windows fall back cleanly to the app icon + title)
        let tr = c.thumb_rect;
        surf.fill_round_rect(tr.left, tr.top, tr.right - tr.left, tr.bottom - tr.top, 5.0 * scale, well_bg);
        if let Some(bmp) = icon_bmp {
            let ix = tr.left + ((tr.right - tr.left) - bmp.w) / 2;
            let iy = tr.top + ((tr.bottom - tr.top) - bmp.h) / 2 - s(8);
            surf.blit_icon(ix, iy, bmp, 0.9);
            text_ren.draw_text(
                surf,
                FontSlot::Small,
                &c.title,
                (tr.left + s(8)) as f32,
                (iy + bmp.h + s(4)) as f32,
                (tr.right - tr.left - s(16)).max(s(20)) as f32,
                s(20) as f32,
                p.dim,
                TextAlign::Center,
            );
        } else {
            text_ren.draw_text(
                surf,
                FontSlot::Small,
                &c.title,
                (tr.left + s(8)) as f32,
                tr.top as f32,
                (tr.right - tr.left - s(16)).max(s(20)) as f32,
                (tr.bottom - tr.top) as f32,
                p.dim,
                TextAlign::Center,
            );
        }
    }
}

#[derive(Clone)]
pub struct TrayFlyoutCell {
    pub tray_idx: usize,
    pub rect: RECT,
    pub tip: String,
    pub key: String,
    pub on_bar: bool,
    pub bitmap: Option<Arc<IconBitmap>>,
}

#[derive(Clone)]
pub struct TrayFlyoutLayout {
    pub win_rect: RECT,
    pub cells: Vec<TrayFlyoutCell>,
    pub footer_rect: RECT,
}

pub fn compute_tray_flyout_layout(
    mon_rect: RECT,
    anchor_cx: i32,
    bar_rect: RECT,
    entries: Vec<(usize, String, String, bool, Option<Arc<IconBitmap>>)>,
    scale: f32,
) -> TrayFlyoutLayout {
    let s = |v: i32| sc(v, scale);
    let pad = s(10);
    let cell = s(38);
    let gap = s(4);
    let footer_h = s(24);
    let count = entries.len();
    let cols = count.clamp(1, 5);
    let rows = if count == 0 { 1 } else { count.div_ceil(cols) };
    let grid_w = (cols as i32 * cell + (cols as i32 - 1).max(0) * gap).max(s(172));
    let grid_h = rows as i32 * cell + (rows as i32 - 1).max(0) * gap;
    let total_w = 2 * pad + grid_w;
    let total_h = 2 * pad + grid_h + s(6) + footer_h;

    let x = (anchor_cx - total_w / 2)
        .clamp(mon_rect.left + s(8), (mon_rect.right - total_w - s(8)).max(mon_rect.left + s(8)));
    let y = if bar_rect.top < (mon_rect.top + mon_rect.bottom) / 2 {
        bar_rect.bottom + s(8)
    } else {
        bar_rect.top - total_h - s(8)
    };

    let mut cells = Vec::with_capacity(count);
    for (i, (tray_idx, tip, key, on_bar, bitmap)) in entries.into_iter().enumerate() {
        let col = (i % cols) as i32;
        let row = (i / cols) as i32;
        let cx = pad + col * (cell + gap);
        let cy = pad + row * (cell + gap);
        cells.push(TrayFlyoutCell {
            tray_idx,
            rect: RECT { left: cx, top: cy, right: cx + cell, bottom: cy + cell },
            tip,
            key,
            on_bar,
            bitmap,
        });
    }
    let footer_rect = RECT {
        left: pad,
        top: pad + grid_h + s(4),
        right: total_w - pad,
        bottom: total_h - pad / 2,
    };
    TrayFlyoutLayout {
        win_rect: RECT { left: x, top: y, right: x + total_w, bottom: y + total_h },
        cells,
        footer_rect,
    }
}

pub fn hit_test_tray_flyout(lay: &TrayFlyoutLayout, x: i32, y: i32) -> Option<usize> {
    lay.cells
        .iter()
        .position(|c| x >= c.rect.left && x < c.rect.right && y >= c.rect.top && y < c.rect.bottom)
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn paint_tray_flyout(
    surf: &mut Surface,
    text_ren: &TextRenderer,
    lay: &TrayFlyoutLayout,
    p: &Palette,
    hover: Option<usize>,
    dragging_cell: Option<usize>,
    drop_to_hide_active: bool,
    anim_p: f32,
    scale: f32,
) {
    let border = if drop_to_hide_active { p.accent } else { p.border_argb };
    surf.clear_island(8.0 * scale, p.acrylic_tint, border, p.top_sheen);
    let s = |v: i32| sc(v, scale);
    if drop_to_hide_active {
        surf.stroke_round_rect(1, 1, surf.w - 2, surf.h - 2, 7.5 * scale, 1.4 * scale, p.accent);
    }

    let p_clamped = anim_p.clamp(0.0, 1.0);

    if lay.cells.is_empty() {
        let msg = if drop_to_hide_active {
            "Drop here to hide icon"
        } else {
            "No hidden icons · drag from bar to hide"
        };
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            msg,
            s(10) as f32,
            s(10) as f32,
            (surf.w - s(20)) as f32,
            s(36) as f32,
            if drop_to_hide_active { p.accent } else { p.dim },
            TextAlign::Center,
        );
    } else {
        for (i, cell) in lay.cells.iter().enumerate() {
            let r = cell.rect;
            let is_dragged = dragging_cell == Some(i);
            if is_dragged {
                surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, p.active_pill);
                surf.stroke_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, 1.0 * scale, p.accent);
            } else if hover == Some(i) {
                surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, p.hover_pill);
            }
            // Staggered ease-in alpha & micro-lift per icon on flyout open
            let stagger = ((p_clamped * 1.4 - (i.min(8) as f32) * 0.05).clamp(0.0, 1.0)).powf(0.8);
            let lift_y = ((1.0 - stagger) * s(5) as f32).round() as i32;
            let alpha = if is_dragged { 0.3 } else { stagger.max(0.15) };
            if let Some(ref bmp) = cell.bitmap {
                let ix = r.left + ((r.right - r.left) - bmp.w) / 2;
                let iy = r.top + ((r.bottom - r.top) - bmp.h) / 2 + lift_y;
                surf.blit_icon(ix, iy, bmp, alpha);
            } else {
                surf.fill_circle(
                    ((r.left + r.right) / 2) as f32,
                    ((r.top + r.bottom) / 2 + lift_y) as f32,
                    4.0 * scale,
                    p.dim,
                );
            }
            if cell.on_bar {
                surf.fill_circle(
                    ((r.left + r.right) / 2) as f32,
                    (r.bottom - s(4)) as f32,
                    1.8 * scale,
                    p.accent,
                );
            }
        }
    }

    surf.fill_rect(s(10), lay.footer_rect.top - s(2), surf.w - s(20), 1, p.divider);
    let footer_text = if drop_to_hide_active {
        "Release to hide icon in overflow".into()
    } else if let Some(cell) = hover.and_then(|i| lay.cells.get(i)) {
        let name = if cell.tip.is_empty() { cell.key.as_str() } else { cell.tip.as_str() };
        format!("{name} · Drag to bar to pin")
    } else {
        "Drag to bar to pin · Drag off bar to hide".into()
    };
    text_ren.draw_text(
        surf,
        FontSlot::Small,
        &footer_text,
        lay.footer_rect.left as f32,
        lay.footer_rect.top as f32,
        (lay.footer_rect.right - lay.footer_rect.left) as f32,
        (lay.footer_rect.bottom - lay.footer_rect.top) as f32,
        if drop_to_hide_active { p.accent } else { p.dim },
        TextAlign::Left,
    );
}

/// Renders the floating Fluent 2 ghost pill that follows the cursor while dragging a tray icon.
pub unsafe fn paint_drag_ghost(
    surf: &mut Surface,
    p: &Palette,
    bmp: Option<&IconBitmap>,
    highlight: bool,
    scale: f32,
) {
    let border = if highlight { p.accent } else { p.border_argb };
    surf.clear_island(8.0 * scale, p.acrylic_tint, border, p.top_sheen);
    if highlight {
        surf.stroke_round_rect(1, 1, surf.w - 2, surf.h - 2, 7.0 * scale, 1.3 * scale, p.accent);
    }
    if let Some(bmp) = bmp {
        let ix = (surf.w - bmp.w) / 2;
        let iy = (surf.h - bmp.h) / 2;
        surf.blit_icon(ix, iy, bmp, 1.0);
    } else {
        surf.fill_circle(surf.w as f32 * 0.5, surf.h as f32 * 0.5, 4.5 * scale, p.accent);
    }
}

// ---------- Fluent 2 Coucou Smart Hub Flyout (anchored to the center Coucou bar) ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CoucouHubAction {
    OpenIsland,
    AskMochi,
    OpenSettings,
    MediaPrev,
    MediaPlayPause,
    MediaNext,
    OpenFocus,
    OpenWater,
    OpenTodos,
    TogglePause,
    LaunchCoucou,
}

#[derive(Clone)]
pub struct CoucouHubButton {
    pub action: CoucouHubAction,
    pub rect: RECT,
}

#[derive(Clone)]
pub struct CoucouHubLayout {
    pub win_rect: RECT,
    pub media_card: Option<RECT>,
    pub tiles: [RECT; 3], // Focus, Water, To-Dos
    pub todos_box: Option<RECT>,
    pub notices_box: Option<RECT>,
    pub buttons: Vec<CoucouHubButton>,
}

pub fn compute_coucou_hub_layout(
    mon_rect: RECT,
    anchor_cx: i32,
    bar_rect: RECT,
    snap: &CoucouSnapshot,
    scale: f32,
) -> CoucouHubLayout {
    let s = |v: i32| sc(v, scale);
    let w = s(364);
    let pad = s(12);
    let inner_w = w - 2 * pad;
    let mut y = pad;

    let mut buttons = Vec::new();

    // Header row (height 34): Mochi emblem + title on left; Settings gear on right
    let gear_r = RECT { left: w - pad - s(28), top: y + s(2), right: w - pad, bottom: y + s(30) };
    buttons.push(CoucouHubButton { action: CoucouHubAction::OpenSettings, rect: gear_r });
    y += s(40);

    // Now Playing card (always shown when media metadata exists)
    let media_card = if !snap.media_title.is_empty() {
        let mh = s(52);
        let r = RECT { left: pad, top: y, right: pad + inner_w, bottom: y + mh };
        let btn_sz = s(26);
        let by = r.top + (mh - btn_sz) / 2;
        let next_r = RECT { left: r.right - s(8) - btn_sz, top: by, right: r.right - s(8), bottom: by + btn_sz };
        let play_r = RECT { left: next_r.left - s(4) - btn_sz, top: by, right: next_r.left - s(4), bottom: by + btn_sz };
        let prev_r = RECT { left: play_r.left - s(4) - btn_sz, top: by, right: play_r.left - s(4), bottom: by + btn_sz };
        buttons.push(CoucouHubButton { action: CoucouHubAction::MediaPrev, rect: prev_r });
        buttons.push(CoucouHubButton { action: CoucouHubAction::MediaPlayPause, rect: play_r });
        buttons.push(CoucouHubButton { action: CoucouHubAction::MediaNext, rect: next_r });
        y += mh + s(8);
        Some(r)
    } else {
        None
    };

    // 3 compact live widget tiles: Focus | Water | To-Dos
    let tgap = s(6);
    let tw = (inner_w - 2 * tgap) / 3;
    let th = s(46);
    let t0 = RECT { left: pad, top: y, right: pad + tw, bottom: y + th };
    let t1 = RECT { left: t0.right + tgap, top: y, right: t0.right + tgap + tw, bottom: y + th };
    let t2 = RECT { left: t1.right + tgap, top: y, right: pad + inner_w, bottom: y + th };
    buttons.push(CoucouHubButton { action: CoucouHubAction::OpenFocus, rect: t0 });
    buttons.push(CoucouHubButton { action: CoucouHubAction::OpenWater, rect: t1 });
    buttons.push(CoucouHubButton { action: CoucouHubAction::OpenTodos, rect: t2 });
    y += th + s(8);

    // Top pending To-Dos (up to 2)
    let todos_box = if !snap.top_todos.is_empty() {
        let rows = snap.top_todos.len().min(2) as i32;
        let h = s(22) + rows * s(20) + s(6);
        let r = RECT { left: pad, top: y, right: pad + inner_w, bottom: y + h };
        y += h + s(8);
        Some(r)
    } else {
        None
    };

    // Recent captured notifications (up to 3)
    let notices_box = if !snap.recent_notices.is_empty() {
        let rows = snap.recent_notices.len().min(3) as i32;
        let h = s(22) + rows * s(22) + s(6);
        let r = RECT { left: pad, top: y, right: pad + inner_w, bottom: y + h };
        y += h + s(8);
        Some(r)
    } else {
        None
    };

    // Footer action bar
    let fh = s(32);
    if snap.running {
        let fgap = s(6);
        let bw = (inner_w - 2 * fgap) / 3;
        let b0 = RECT { left: pad, top: y, right: pad + bw, bottom: y + fh };
        let b1 = RECT { left: b0.right + fgap, top: y, right: b0.right + fgap + bw, bottom: y + fh };
        let b2 = RECT { left: b1.right + fgap, top: y, right: pad + inner_w, bottom: y + fh };
        buttons.push(CoucouHubButton { action: CoucouHubAction::OpenIsland, rect: b0 });
        buttons.push(CoucouHubButton { action: CoucouHubAction::AskMochi, rect: b1 });
        buttons.push(CoucouHubButton { action: CoucouHubAction::TogglePause, rect: b2 });
    } else {
        let b0 = RECT { left: pad, top: y, right: pad + inner_w, bottom: y + fh };
        buttons.push(CoucouHubButton { action: CoucouHubAction::LaunchCoucou, rect: b0 });
    }
    let total_h = y + fh + pad;

    let x = (anchor_cx - w / 2)
        .clamp(mon_rect.left + s(8), (mon_rect.right - w - s(8)).max(mon_rect.left + s(8)));
    let wy = if bar_rect.top < (mon_rect.top + mon_rect.bottom) / 2 {
        bar_rect.bottom + s(8)
    } else {
        bar_rect.top - total_h - s(8)
    }
    .clamp(mon_rect.top + s(8), (mon_rect.bottom - total_h - s(8)).max(mon_rect.top + s(8)));

    CoucouHubLayout {
        win_rect: RECT { left: x, top: wy, right: x + w, bottom: wy + total_h },
        media_card,
        tiles: [t0, t1, t2],
        todos_box,
        notices_box,
        buttons,
    }
}

pub fn hit_test_coucou_hub(lay: &CoucouHubLayout, x: i32, y: i32) -> Option<CoucouHubAction> {
    lay.buttons
        .iter()
        .find(|b| x >= b.rect.left && x < b.rect.right && y >= b.rect.top && y < b.rect.bottom)
        .map(|b| b.action)
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn paint_coucou_hub(
    surf: &mut Surface,
    text_ren: &TextRenderer,
    lay: &CoucouHubLayout,
    snap: &CoucouSnapshot,
    p: &Palette,
    hover: Option<CoucouHubAction>,
    anim_tick: u32,
    scale: f32,
) {
    let glass_bg = if p.is_light { 0xF5F8_FAFE } else { 0xF312_151E };
    surf.clear_island(10.0 * scale, glass_bg, p.border_argb, p.top_sheen);
    let s = |v: i32| sc(v, scale);
    let pad = s(12);
    let card_fill = if p.is_light { 0x1000_0000 } else { 0x12FF_FFFF };
    let (_, mood) = snap.pill_summary();

    // Header
    draw_mochi_emblem(surf, pad, pad + s(3), s(26), mood, false, anim_tick, p, scale);
    text_ren.draw_text(
        surf,
        FontSlot::SemiBold,
        "Coucou · Mochi",
        (pad + s(34)) as f32,
        pad as f32,
        s(180) as f32,
        s(18) as f32,
        p.text,
        TextAlign::Left,
    );
    let status_line = if snap.running { "Assistant Online · Center Hub" } else { "Assistant Offline · Click below to launch" };
    text_ren.draw_text(
        surf,
        FontSlot::Small,
        status_line,
        (pad + s(34)) as f32,
        (pad + s(16)) as f32,
        s(240) as f32,
        s(16) as f32,
        if snap.running { 0xFF81_8CF8 } else { p.dim },
        TextAlign::Left,
    );
    if let Some(gb) = lay.buttons.iter().find(|b| b.action == CoucouHubAction::OpenSettings) {
        let r = gb.rect;
        if hover == Some(CoucouHubAction::OpenSettings) {
            surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, p.hover_pill);
        }
        text_ren.draw_text(
            surf,
            FontSlot::GlyphSmall,
            "\u{E713}",
            r.left as f32,
            r.top as f32,
            (r.right - r.left) as f32,
            (r.bottom - r.top) as f32,
            p.dim,
            TextAlign::Center,
        );
    }
    surf.fill_rect(pad, pad + s(36), surf.w - 2 * pad, 1, p.divider);

    // Now Playing card
    if let Some(mr) = lay.media_card {
        surf.fill_round_rect(mr.left, mr.top, mr.right - mr.left, mr.bottom - mr.top, 8.0 * scale, card_fill);
        let dot_col = if snap.media_playing { 0xFF34_D399 } else { p.dim };
        surf.fill_circle((mr.left + s(12)) as f32, (mr.top + (mr.bottom - mr.top) / 2) as f32, 3.5 * scale, dot_col);
        let tx = mr.left + s(22);
        let tw = (mr.right - tx - s(98)).max(s(60));
        text_ren.draw_text(
            surf,
            FontSlot::SemiBold,
            &snap.media_title,
            tx as f32,
            (mr.top + s(7)) as f32,
            tw as f32,
            s(18) as f32,
            p.text,
            TextAlign::Left,
        );
        let sub = if snap.media_artist.is_empty() {
            snap.media_player.clone()
        } else if snap.media_player.is_empty() {
            snap.media_artist.clone()
        } else {
            format!("{} · {}", snap.media_artist, snap.media_player)
        };
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            &sub,
            tx as f32,
            (mr.top + s(25)) as f32,
            tw as f32,
            s(18) as f32,
            p.dim,
            TextAlign::Left,
        );
        for (act, glyph) in [
            (CoucouHubAction::MediaPrev, "\u{E892}"),
            (CoucouHubAction::MediaPlayPause, if snap.media_playing { "\u{E769}" } else { "\u{E768}" }),
            (CoucouHubAction::MediaNext, "\u{E893}"),
        ] {
            if let Some(btn) = lay.buttons.iter().find(|b| b.action == act) {
                let r = btn.rect;
                let fill = if hover == Some(act) {
                    p.hover_pill
                } else if act == CoucouHubAction::MediaPlayPause {
                    0x2634_D399
                } else {
                    0x10FF_FFFF
                };
                surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, fill);
                text_ren.draw_text(
                    surf,
                    FontSlot::GlyphSmall,
                    glyph,
                    r.left as f32,
                    r.top as f32,
                    (r.right - r.left) as f32,
                    (r.bottom - r.top) as f32,
                    if act == CoucouHubAction::MediaPlayPause { 0xFF34_D399 } else { p.text },
                    TextAlign::Center,
                );
            }
        }
    }

    // 3 live widget summary tiles: Focus | Water | To-Dos
    let focus_val = if snap.focus_phase != "idle" && !snap.focus_phase.is_empty() && snap.focus_remaining_sec > 0 {
        format!("{:02}:{:02}", snap.focus_remaining_sec / 60, snap.focus_remaining_sec % 60)
    } else {
        "Idle".into()
    };
    let water_val = if snap.water_due {
        format!("{} · Drink!", snap.water_glasses)
    } else {
        format!("{} glasses", snap.water_glasses)
    };
    let todo_val = format!("{} open", snap.open_todos);
    let tile_specs = [
        (CoucouHubAction::OpenFocus, "Focus", focus_val, 0xFFC0_84FC),
        (CoucouHubAction::OpenWater, "Hydration", water_val, 0xFF38_BDF8),
        (CoucouHubAction::OpenTodos, "To-Dos", todo_val, 0xFFFA_CC15),
    ];
    for (idx, (act, title, val, dot)) in tile_specs.into_iter().enumerate() {
        let r = lay.tiles[idx];
        let fill = if hover == Some(act) { p.hover_pill } else { card_fill };
        surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 7.0 * scale, fill);
        surf.fill_circle((r.left + s(10)) as f32, (r.top + s(13)) as f32, 2.5 * scale, dot);
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            title,
            (r.left + s(17)) as f32,
            (r.top + s(4)) as f32,
            (r.right - r.left - s(22)) as f32,
            s(16) as f32,
            p.dim,
            TextAlign::Left,
        );
        text_ren.draw_text(
            surf,
            FontSlot::SemiBold,
            &val,
            (r.left + s(10)) as f32,
            (r.top + s(21)) as f32,
            (r.right - r.left - s(16)) as f32,
            s(20) as f32,
            p.text,
            TextAlign::Left,
        );
    }

    // Top pending To-Dos
    if let Some(tr) = lay.todos_box {
        surf.fill_round_rect(tr.left, tr.top, tr.right - tr.left, tr.bottom - tr.top, 7.0 * scale, card_fill);
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            "PENDING TO-DOS",
            (tr.left + s(10)) as f32,
            (tr.top + s(4)) as f32,
            (tr.right - tr.left - s(20)) as f32,
            s(16) as f32,
            p.dim,
            TextAlign::Left,
        );
        for (i, td) in snap.top_todos.iter().take(2).enumerate() {
            let ry = tr.top + s(22) + i as i32 * s(20);
            let col = match td.color.as_str() {
                "red" => 0xFFF8_7171,
                "amber" => 0xFFFB_BF24,
                "blue" => 0xFF60_A5FA,
                "green" => 0xFF34_D399,
                "purple" => 0xFFC0_84FC,
                _ => p.accent,
            };
            surf.fill_circle((tr.left + s(12)) as f32, (ry + s(9)) as f32, 2.5 * scale, col);
            text_ren.draw_text(
                surf,
                FontSlot::Small,
                &td.text,
                (tr.left + s(20)) as f32,
                ry as f32,
                (tr.right - tr.left - s(28)) as f32,
                s(18) as f32,
                p.text,
                TextAlign::Left,
            );
        }
    }

    // Recent captured notifications
    if let Some(nr) = lay.notices_box {
        surf.fill_round_rect(nr.left, nr.top, nr.right - nr.left, nr.bottom - nr.top, 7.0 * scale, card_fill);
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            "RECENT ALERTS",
            (nr.left + s(10)) as f32,
            (nr.top + s(4)) as f32,
            (nr.right - nr.left - s(20)) as f32,
            s(16) as f32,
            p.dim,
            TextAlign::Left,
        );
        for (i, n) in snap.recent_notices.iter().take(3).enumerate() {
            let ry = nr.top + s(22) + i as i32 * s(22);
            let line = if n.title.is_empty() {
                format!("{}: {}", n.app, n.body)
            } else {
                format!("{} · {}", n.app, n.title)
            };
            text_ren.draw_text(
                surf,
                FontSlot::Small,
                &line,
                (nr.left + s(10)) as f32,
                ry as f32,
                (nr.right - nr.left - s(18)) as f32,
                s(20) as f32,
                p.text,
                TextAlign::Left,
            );
        }
    }

    // Footer buttons
    for btn in &lay.buttons {
        let label = match btn.action {
            CoucouHubAction::OpenIsland => "Open Island",
            CoucouHubAction::AskMochi => "Ask Mochi",
            CoucouHubAction::TogglePause => "Pause / Wake",
            CoucouHubAction::LaunchCoucou => "Launch Coucou Assistant",
            _ => continue,
        };
        let r = btn.rect;
        let primary = matches!(btn.action, CoucouHubAction::OpenIsland | CoucouHubAction::LaunchCoucou);
        let fill = if hover == Some(btn.action) {
            if primary { 0xFF63_66F1 } else { p.hover_pill }
        } else if primary {
            0xCC4F_46E5
        } else {
            card_fill
        };
        surf.fill_round_rect(r.left, r.top, r.right - r.left, r.bottom - r.top, 6.0 * scale, fill);
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            label,
            r.left as f32,
            r.top as f32,
            (r.right - r.left) as f32,
            (r.bottom - r.top) as f32,
            if primary { 0xFFFF_FFFF } else { p.text },
            TextAlign::Center,
        );
    }
}

// ---------- Native Fluent 2 Animated Search Bar & Results Panel ----------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchBarHit {
    None,
    Input,
    Scope,
    Clear,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SearchPanelHit {
    None,
    Scope(SearchScope),
    Row(usize),
}

#[derive(Clone)]
pub struct SearchBarLayout {
    pub start_rect: RECT, // Screen rect of the Search button inside TopCenter (morph start)
    pub full_rect: RECT,  // Expanded middle bottom topbar rect (morph end)
    pub scope_rect: RECT, // Client-local rect inside full_rect
    pub clear_rect: RECT, // Client-local rect inside full_rect
}

#[derive(Clone)]
pub struct SearchPanelLayout {
    pub win_rect: RECT,
    pub tab_rects: [(SearchScope, RECT); 4],
    pub row_rects: Vec<RECT>,
}

/// Computes the morphing start/end rects for the bottom-center Search Bar so it animates
/// smoothly out of the Search pill to fill the middle bottom topbar between `TopLeft` and `TopRight`.
pub fn compute_search_bar_layout(mon: &MonitorSpec, lay: &MonitorLayout) -> SearchBarLayout {
    let s = |v: i32| sc(v, mon.scale);
    let m = mon.rect;
    let sw = (m.right - m.left).max(480);

    let (base_top, base_h, start_rect) = if let Some(tc) = lay.top_center {
        let sr = lay.search_rect.map_or(tc, |r| RECT {
            left: tc.left + r.left,
            top: tc.top + r.top,
            right: tc.left + r.right,
            bottom: tc.top + r.bottom,
        });
        (tc.top, (tc.bottom - tc.top).max(s(TOP_STRIP_H)), sr)
    } else if let Some(dk) = lay.dock {
        let sx = dk.left + lay.dock_pad + (lay.btn_w + lay.btn_gap);
        let sr = RECT {
            left: sx,
            top: dk.top + s(3),
            right: sx + lay.btn_w,
            bottom: dk.bottom - s(3),
        };
        let dh = (dk.bottom - dk.top).max(s(TOP_STRIP_H));
        (dk.top, dh, sr)
    } else {
        let th = s(TOP_STRIP_H);
        let y = m.bottom - th - s(EDGE_GAP);
        let r = RECT { left: m.left + sw / 2 - s(40), top: y, right: m.left + sw / 2 + s(40), bottom: y + th };
        (y, th, r)
    };

    // Fill the middle space cleanly between TopLeft and TopRight (clamped up to 620px on wide screens)
    let min_left = lay
        .top_left
        .map_or(m.left + s(16), |tl| tl.right + s(ISLAND_GAP));
    let max_right = lay
        .top_right
        .map_or(m.right - s(16), |tr| tr.left - s(ISLAND_GAP));
    let avail = (max_right - min_left).max(s(260));
    let want_w = avail.min(s(620));
    let cx = m.left + sw / 2;
    let fl = (cx - want_w / 2).clamp(min_left, (max_right - want_w).max(min_left));
    let full_rect = RECT {
        left: fl,
        top: base_top,
        right: fl + want_w,
        bottom: base_top + base_h,
    };

    let clear_w = s(26);
    let scope_w = s(64);
    let clear_rect = RECT {
        left: want_w - s(4) - clear_w,
        top: s(4),
        right: want_w - s(4),
        bottom: base_h - s(4),
    };
    let scope_rect = RECT {
        left: clear_rect.left - s(4) - scope_w,
        top: s(4),
        right: clear_rect.left - s(4),
        bottom: base_h - s(4),
    };
    SearchBarLayout { start_rect, full_rect, scope_rect, clear_rect }
}

pub fn interpolate_rect(a: RECT, b: RECT, t: f32) -> RECT {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: i32, y: i32| (x as f32 + (y - x) as f32 * t).round() as i32;
    RECT {
        left: lerp(a.left, b.left),
        top: lerp(a.top, b.top),
        right: lerp(a.right, b.right),
        bottom: lerp(a.bottom, b.bottom),
    }
}

pub fn hit_test_search_bar(lay: &SearchBarLayout, x: i32, y: i32) -> SearchBarHit {
    let in_r = |r: RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
    if in_r(lay.clear_rect) {
        SearchBarHit::Clear
    } else if in_r(lay.scope_rect) {
        SearchBarHit::Scope
    } else {
        SearchBarHit::Input
    }
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn paint_search_bar(
    surf: &mut Surface,
    text_ren: &TextRenderer,
    lay: &SearchBarLayout,
    p: &Palette,
    query: &str,
    scope: SearchScope,
    caret_on: bool,
    hover: SearchBarHit,
    spring_p: f32,
    scale: f32,
) {
    // Fixed full-width transparent canvas: morphs the pill smoothly inside without resizing the HWND
    // or reallocating the GDI DIB section on every VSync frame!
    surf.clear_transparent();
    let s = |v: i32| sc(v, scale);
    let w = surf.w;
    let h = surf.h;
    let t = spring_p.clamp(0.0, 1.04);

    // Local start pill bounds inside `full_rect`
    let start_l = (lay.start_rect.left - lay.full_rect.left).clamp(0, w - s(48)) as f32;
    let start_r = (lay.start_rect.right - lay.full_rect.left).clamp(start_l as i32 + s(48), w) as f32;
    let cur_l = ((start_l * (1.0 - t)).round() as i32).clamp(0, w - s(48));
    let cur_r = ((start_r + (w as f32 - start_r) * t).round() as i32).clamp(cur_l + s(48), w);
    let pw = (cur_r - cur_l).max(s(48));

    let rad = 8.0 * scale;
    let fade = (spring_p / 0.18).clamp(0.0, 1.0);
    let mul_a = |argb: u32, k: f32| -> u32 {
        let a = (((argb >> 24) & 0xFF) as f32 * k.clamp(0.0, 1.0)).round() as u32;
        (a << 24) | (argb & 0x00FF_FFFF)
    };

    // Morphing Fluent 2 rounded pill body + 1px anti-aliased border + top specular sheen
    let glass_bg = if p.is_light { 0xF5F8_FAFE } else { 0xF212_151E };
    surf.fill_round_rect(cur_l, 0, pw, h, rad, mul_a(glass_bg, fade));
    surf.stroke_round_rect(cur_l, 0, pw, h, rad, 1.0, mul_a(p.border_argb, fade));
    let inset = (rad.ceil() as i32).max(4);
    if pw > inset * 2 && h > 4 && (p.top_sheen >> 24) > 0 {
        surf.fill_rect(cur_l + inset, 1, pw - inset * 2, 1, mul_a(p.top_sheen, fade));
    }

    // Signature Fluent 2 bottom accent focus line growing smoothly with the pill
    let line_w = ((pw - s(20)).max(s(12)) as f32 * spring_p.clamp(0.0, 1.0)).round() as i32;
    let line_x = cur_l + (pw - line_w) / 2;
    if line_w > 2 {
        surf.fill_round_rect(line_x, h - s(2).max(2), line_w, s(2).max(2), 1.0, mul_a(p.accent, fade));
    }

    // Glowing Search magnifier glyph anchored to the left edge of the morphing pill
    text_ren.draw_text(
        surf,
        FontSlot::GlyphSmall,
        "\u{E721}",
        (cur_l + s(8)) as f32,
        0.0,
        s(24) as f32,
        h as f32,
        mul_a(p.accent, fade),
        TextAlign::Center,
    );

    let text_fade = ((spring_p - 0.28) / 0.45).clamp(0.0, 1.0);
    if text_fade <= 0.02 || pw < s(150) {
        return;
    }

    let right_limit = if spring_p > 0.72 {
        (lay.scope_rect.left - s(8)).min(cur_r - s(10))
    } else {
        cur_r - s(12)
    };
    let tx = cur_l + s(36);
    let tw = (right_limit - tx).max(s(40));

    if query.is_empty() {
        let ph = if caret_on {
            "|  Search apps, settings, files, commands, or calculate…"
        } else {
            "   Search apps, settings, files, commands, or calculate…"
        };
        text_ren.draw_text(
            surf,
            FontSlot::Regular,
            ph,
            tx as f32,
            0.0,
            tw as f32,
            h as f32,
            mul_a(p.dim, text_fade),
            TextAlign::Left,
        );
    } else {
        let shown = if caret_on {
            format!("{query}|")
        } else {
            query.to_string()
        };
        text_ren.draw_text(
            surf,
            FontSlot::SemiBold,
            &shown,
            tx as f32,
            0.0,
            tw as f32,
            h as f32,
            mul_a(p.text, text_fade),
            TextAlign::Left,
        );
    }

    // Fade in right-side scope chip & clear/close button smoothly as pill nears full width
    let controls_fade = ((spring_p - 0.62) / 0.32).clamp(0.0, 1.0);
    if controls_fade > 0.02 && cur_r >= lay.clear_rect.right {
        let sr = lay.scope_rect;
        let sfill = if hover == SearchBarHit::Scope { p.hover_pill } else { p.active_pill };
        surf.fill_round_rect(
            sr.left,
            sr.top,
            sr.right - sr.left,
            sr.bottom - sr.top,
            6.0 * scale,
            mul_a(sfill, controls_fade),
        );
        text_ren.draw_text(
            surf,
            FontSlot::Small,
            scope.label(),
            sr.left as f32,
            sr.top as f32,
            (sr.right - sr.left) as f32,
            (sr.bottom - sr.top) as f32,
            mul_a(p.accent, controls_fade),
            TextAlign::Center,
        );

        let cr = lay.clear_rect;
        if hover == SearchBarHit::Clear {
            surf.fill_round_rect(
                cr.left,
                cr.top,
                cr.right - cr.left,
                cr.bottom - cr.top,
                6.0 * scale,
                mul_a(p.hover_pill, controls_fade),
            );
        }
        text_ren.draw_text(
            surf,
            FontSlot::GlyphSmall,
            "\u{E711}",
            cr.left as f32,
            cr.top as f32,
            (cr.right - cr.left) as f32,
            (cr.bottom - cr.top) as f32,
            mul_a(if hover == SearchBarHit::Clear { p.text } else { p.dim }, controls_fade),
            TextAlign::Center,
        );
    }
}

pub fn compute_search_panel_layout(
    mon: &MonitorSpec,
    bar_rect: RECT,
    result_count: usize,
) -> SearchPanelLayout {
    let s = |v: i32| sc(v, mon.scale);
    let w = (bar_rect.right - bar_rect.left).max(s(320));
    let pad = s(10);
    let tab_h = s(28);
    let row_h = s(44);
    let row_gap = s(2);
    let footer_h = s(24);

    let shown = result_count.clamp(1, 8);
    let list_top = pad + tab_h + s(8);
    let mut row_rects = Vec::with_capacity(result_count.min(8));
    for i in 0..result_count.min(8) {
        let ry = list_top + i as i32 * (row_h + row_gap);
        row_rects.push(RECT {
            left: pad,
            top: ry,
            right: w - pad,
            bottom: ry + row_h,
        });
    }
    let total_h = list_top + shown as i32 * (row_h + row_gap) + s(6) + footer_h + pad / 2;

    let mut tab_rects = [(SearchScope::All, RECT { left: 0, top: 0, right: 0, bottom: 0 }); 4];
    let mut tx = pad;
    for (i, sc_kind) in SearchScope::ALL.into_iter().enumerate() {
        let tw = s(62);
        tab_rects[i] = (
            sc_kind,
            RECT { left: tx, top: pad, right: tx + tw, bottom: pad + tab_h },
        );
        tx += tw + s(6);
    }

    let m = mon.rect;
    let x = bar_rect
        .left
        .clamp(m.left + s(8), (m.right - w - s(8)).max(m.left + s(8)));
    let wy = if bar_rect.top < (m.top + m.bottom) / 2 {
        bar_rect.bottom + s(8)
    } else {
        bar_rect.top - total_h - s(8)
    }
    .clamp(m.top + s(8), (m.bottom - total_h - s(8)).max(m.top + s(8)));

    SearchPanelLayout {
        win_rect: RECT { left: x, top: wy, right: x + w, bottom: wy + total_h },
        tab_rects,
        row_rects,
    }
}

pub fn hit_test_search_panel(lay: &SearchPanelLayout, x: i32, y: i32) -> SearchPanelHit {
    for &(scope, r) in &lay.tab_rects {
        if x >= r.left && x < r.right && y >= r.top && y < r.bottom {
            return SearchPanelHit::Scope(scope);
        }
    }
    for (i, &r) in lay.row_rects.iter().enumerate() {
        if x >= r.left && x < r.right && y >= r.top && y < r.bottom {
            return SearchPanelHit::Row(i);
        }
    }
    SearchPanelHit::None
}

#[allow(clippy::too_many_arguments)]
pub unsafe fn paint_search_panel(
    surf: &mut Surface,
    text_ren: &TextRenderer,
    lay: &SearchPanelLayout,
    p: &Palette,
    query: &str,
    scope: SearchScope,
    results: &[SearchResult],
    selected: usize,
    hover: SearchPanelHit,
    icons: &IconCache,
    notify_hwnd: HWND,
    notify_msg: u32,
    scale: f32,
) {
    let glass_bg = if p.is_light { 0xF5F8_FAFE } else { 0xF312_151E };
    surf.clear_island(10.0 * scale, glass_bg, p.border_argb, p.top_sheen);
    let s = |v: i32| sc(v, scale);
    let pad = s(10);

    // Top scope tabs: All | Apps | Settings | Files
    for &(tab_scope, r) in &lay.tab_rects {
        let active = tab_scope == scope;
        let is_hov = hover == SearchPanelHit::Scope(tab_scope);
        if active || is_hov {
            surf.fill_round_rect(
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                6.0 * scale,
                if active { p.active_pill } else { p.hover_pill },
            );
        }
        text_ren.draw_text(
            surf,
            if active { FontSlot::SemiBold } else { FontSlot::Small },
            tab_scope.label(),
            r.left as f32,
            r.top as f32,
            (r.right - r.left) as f32,
            (r.bottom - r.top) as f32,
            if active { p.text } else { p.dim },
            TextAlign::Center,
        );
        if active {
            let uw = s(22);
            surf.fill_round_rect(
                (r.left + r.right - uw) / 2,
                r.bottom - s(3),
                uw,
                s(2).max(2),
                1.0,
                p.accent,
            );
        }
    }

    // Header hint on right of tab strip
    let hdr_hint = if query.trim().is_empty() {
        "Quick Access & Apps"
    } else {
        "Best Matches"
    };
    text_ren.draw_text(
        surf,
        FontSlot::Small,
        hdr_hint,
        (surf.w - pad - s(170)) as f32,
        pad as f32,
        s(170) as f32,
        s(28) as f32,
        p.dim,
        TextAlign::Right,
    );
    surf.fill_rect(pad, pad + s(32), surf.w - 2 * pad, 1, p.divider);

    if results.is_empty() {
        text_ren.draw_text(
            surf,
            FontSlot::Regular,
            "No local matches — press Enter to search the web",
            pad as f32,
            (pad + s(44)) as f32,
            (surf.w - 2 * pad) as f32,
            s(32) as f32,
            p.dim,
            TextAlign::Center,
        );
    } else {
        let icon_px = s(22);
        for (i, (res, &r)) in results.iter().zip(lay.row_rects.iter()).enumerate() {
            let is_sel = i == selected;
            let is_hov = hover == SearchPanelHit::Row(i);
            if is_sel || is_hov {
                surf.fill_round_rect(
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    7.0 * scale,
                    if is_sel { p.active_pill } else { p.hover_pill },
                );
            }
            if is_sel {
                surf.fill_round_rect(
                    r.left + s(2),
                    r.top + s(9),
                    s(3).max(3),
                    (r.bottom - r.top) - s(18),
                    1.5 * scale,
                    p.accent,
                );
            }

            // Icon well (real cached app icon OR crisp Fluent 2 category badge)
            let well = s(30);
            let wx = r.left + s(10);
            let wy = r.top + ((r.bottom - r.top) - well) / 2;
            let bmp = if !res.icon_key.is_empty() && (!res.icon_path.is_empty() || res.hwnd != 0) {
                icons.get_or_request(
                    IconRequest::App {
                        key: res.icon_key.clone(),
                        path: res.icon_path.clone(),
                        fallback_hwnd: res.hwnd,
                    },
                    icon_px,
                    notify_hwnd,
                    notify_msg,
                )
            } else {
                None
            };
            if let Some(b) = bmp {
                let ix = wx + (well - b.w) / 2;
                let iy = wy + (well - b.h) / 2;
                surf.blit_icon(ix, iy, &b, 1.0);
            } else {
                let (bg, fg) = match res.kind {
                    SearchKind::Calc => (0x2634_D399, 0xFF34_D399),
                    SearchKind::Setting => (0x2660_A5FA, 0xFF60_A5FA),
                    SearchKind::Project => (0x26C0_84FC, 0xFFC0_84FC),
                    SearchKind::Folder => (0x26FA_CC15, 0xFFFA_CC15),
                    SearchKind::Web => (0x2638_BDF8, 0xFF38_BDF8),
                    _ => (0x1CFF_FFFF, p.text),
                };
                surf.fill_round_rect(wx, wy, well, well, 7.0 * scale, bg);
                text_ren.draw_text(
                    surf,
                    FontSlot::GlyphSmall,
                    res.kind.glyph(),
                    wx as f32,
                    wy as f32,
                    well as f32,
                    well as f32,
                    fg,
                    TextAlign::Center,
                );
            }

            // Right category badge pill
            let badge_w = s(54);
            let badge_h = s(20);
            let bx = r.right - s(10) - badge_w;
            let by = r.top + ((r.bottom - r.top) - badge_h) / 2;
            surf.fill_round_rect(
                bx,
                by,
                badge_w,
                badge_h,
                5.0 * scale,
                if p.is_light { 0x1000_0000 } else { 0x14FF_FFFF },
            );
            text_ren.draw_text(
                surf,
                FontSlot::Small,
                res.kind.badge(),
                bx as f32,
                by as f32,
                badge_w as f32,
                badge_h as f32,
                if is_sel { p.accent } else { p.dim },
                TextAlign::Center,
            );

            // Title & subtitle
            let tx = wx + well + s(10);
            let tw = (bx - tx - s(8)).max(s(60));
            text_ren.draw_text(
                surf,
                FontSlot::SemiBold,
                &res.title,
                tx as f32,
                (r.top + s(4)) as f32,
                tw as f32,
                s(19) as f32,
                p.text,
                TextAlign::Left,
            );
            text_ren.draw_text(
                surf,
                FontSlot::Small,
                &res.subtitle,
                tx as f32,
                (r.top + s(23)) as f32,
                tw as f32,
                s(17) as f32,
                p.dim,
                TextAlign::Left,
            );
        }
    }

    // Bottom keyboard shortcut bar
    let fy = surf.h - s(26);
    surf.fill_rect(pad, fy - s(2), surf.w - 2 * pad, 1, p.divider);
    text_ren.draw_text(
        surf,
        FontSlot::Small,
        "↑↓ Navigate  ·  ↵ Open  ·  Ctrl+↵ Admin  ·  Tab Filter  ·  Esc Close",
        pad as f32,
        fy as f32,
        (surf.w - 2 * pad) as f32,
        s(22) as f32,
        p.dim,
        TextAlign::Center,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_telem() -> TelemetrySnapshot {
        TelemetrySnapshot { cpu_pct: 12, ram_pct: 48, online: true, bat_pct: -1, on_ac: true }
    }

    #[test]
    fn split_and_unified_layouts_fit_landscape_and_portrait_monitors() {
        let mon1 = MonitorSpec {
            hmon: 1,
            rect: RECT { left: 0, top: 0, right: 1920, bottom: 1080 },
            primary: true,
            scale: 1.0,
        };
        let mon2 = MonitorSpec {
            hmon: 2,
            rect: RECT { left: 1920, top: -464, right: 3000, bottom: 1456 },
            primary: false,
            scale: 1.0,
        };
        let sys = SysPrefs::default();

        for mode in [LayoutMode::Win12Split, LayoutMode::UnifiedTop, LayoutMode::UnifiedBottom] {
            for edge in [DockEdge::Top, DockEdge::Bottom] {
                let cfg = Config { layout: mode, dock_edge: edge, ..Config::default() };
                for mon in [mon1, mon2] {
                    let lay = compute_monitor_layout(&mon, &cfg, &sys, 8, 4, true, false, false, test_telem(), "Editor", "21:10", "Mochi", None);
                    if let (Some(tl), Some(tc), Some(tr)) = (lay.top_left, lay.top_center, lay.top_right) {
                        assert!(tl.right < tc.left, "tl {:?} >= tc {:?}", (tl.left, tl.right), (tc.left, tc.right));
                        assert!(tc.right < tr.left, "tc {:?} >= tr {:?}", (tc.left, tc.right), (tr.left, tr.right));
                        if mon.primary && cfg.show_coucou {
                            let cr = lay.coucou_rect.expect("primary Win12Split must place Coucou beside Calendar");
                            assert!(cr.left > lay.clock_rect.right, "Coucou must sit beside clock without overlapping");
                        }
                    }
                    if let Some(dk) = lay.dock {
                        assert!(dk.left >= mon.rect.left && dk.right <= mon.rect.right);
                        assert!(dk.top >= mon.rect.top && dk.bottom <= mon.rect.bottom);
                        if mode == LayoutMode::Win12Split {
                            let mid_y = (mon.rect.top + mon.rect.bottom) / 2;
                            if edge == DockEdge::Top {
                                assert!(dk.top < mid_y, "DockEdge::Top must place dock in top half");
                                assert!(lay.top_center.unwrap().top > mid_y, "DockEdge::Top must place status in bottom half");
                            } else {
                                assert!(dk.top > mid_y, "DockEdge::Bottom must place dock in bottom half");
                                assert!(lay.top_center.unwrap().top < mid_y, "DockEdge::Bottom must place status in top half");
                            }
                            if mon.primary {
                                // Split mode must never hit-test TopCenter's clock_rect inside TopRight
                                let cx = (lay.clock_rect.left + lay.clock_rect.right) / 2;
                                let cy = (lay.clock_rect.top + lay.clock_rect.bottom) / 2;
                                assert_ne!(hit_test(SurfaceKind::TopRight, &lay, cx, cy, 8), HitZone::Clock);
                            }
                        }
                    }
                }
            }
        }

        // Unified centered live-Coucou dock bay: Search flanks the left of Coucou (742..1178)
        // and Calendar moves to the right of Coucou inside ONE balanced continuous bar!
        let cfg = Config { layout: LayoutMode::Win12Split, dock_edge: DockEdge::Top, ..Config::default() };
        let comp = compute_monitor_layout(&mon1, &cfg, &sys, 8, 4, true, false, false, test_telem(), "Editor", "Fri, Nov 27 · 7:56 PM", "Mochi", Some((742, 1178)));
        let tc = comp.top_center.expect("unified TopCenter must stay visible");
        let sr = comp.search_rect.expect("Search button must stay visible on left of Coucou");
        assert!(tc.left < 742 && tc.right > 1178, "TopCenter must span Search + Coucou + Calendar: {:?}", (tc.left, tc.right));
        assert!(tc.left + sr.right <= 742, "Search must sit left of Coucou bay");
        assert!(tc.left + comp.clock_rect.left >= 1178, "Calendar must move to the right of Coucou bay");
        assert!(comp.coucou_rect.is_none(), "Duplicate static Mochi button should hide while live Mochi pill occupies the bay");
    }
}
