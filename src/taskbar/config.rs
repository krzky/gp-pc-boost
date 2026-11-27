//! Windows 12 taskbar configuration stored in %APPDATA%\gp-pc-boost\taskbar.ini.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayoutMode {
    Win12Split,    // Top/bottom status strip + floating app dock on opposite edge
    UnifiedTop,    // Single-edge 3-island bar at top
    UnifiedBottom, // Single-edge 3-island bar at bottom
}

impl LayoutMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Win12Split => "win12_split",
            Self::UnifiedTop => "unified_top",
            Self::UnifiedBottom => "unified_bottom",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Win12Split => "Win12 Split",
            Self::UnifiedTop => "Unified Top",
            Self::UnifiedBottom => "Unified Bottom",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "win12_split" | "split" => Some(Self::Win12Split),
            "unified_top" | "top" => Some(Self::UnifiedTop),
            "unified_bottom" | "bottom" => Some(Self::UnifiedBottom),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DockEdge {
    Bottom,
    Top,
}

impl DockEdge {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bottom => "bottom",
            Self::Top => "top",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Bottom => "Bottom",
            Self::Top => "Top",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bottom" => Some(Self::Bottom),
            "top" => Some(Self::Top),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DockSize {
    Compact,  // 34px — matches the sleek right-corner control strip
    Small,    // 40px — classic Windows small taskbar
    Standard, // 48px — default Windows 11/12 height
    Large,    // 56px — roomy floating dock
}

impl DockSize {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Small => "small",
            Self::Standard => "standard",
            Self::Large => "large",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Compact => "Compact (34px)",
            Self::Small => "Small (40px)",
            Self::Standard => "Standard (48px)",
            Self::Large => "Large (56px)",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Compact => Self::Small,
            Self::Small => Self::Standard,
            Self::Standard => Self::Large,
            Self::Large => Self::Compact,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "compact" | "xs" | "34" => Some(Self::Compact),
            "small" | "sm" | "40" => Some(Self::Small),
            "standard" | "medium" | "md" | "normal" | "48" => Some(Self::Standard),
            "large" | "lg" | "56" => Some(Self::Large),
            _ => None,
        }
    }

    /// Returns `(primary_dock_h, secondary_dock_h, unified_h, max_btn_w, icon_px, pad_x, btn_gap)`.
    pub fn metrics(self) -> (i32, i32, i32, i32, i32, i32, i32) {
        match self {
            Self::Compact => (34, 32, 36, 30, 18, 6, 3),
            Self::Small => (40, 38, 40, 35, 20, 7, 4),
            Self::Standard => (48, 44, 48, 40, 24, 8, 5),
            Self::Large => (56, 52, 54, 46, 28, 9, 5),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AlignPref {
    System,
    Center,
    Left,
}

impl AlignPref {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Center => "center",
            Self::Left => "left",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Center => "Center",
            Self::Left => "Left",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" => Some(Self::System),
            "center" => Some(Self::Center),
            "left" => Some(Self::Left),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MultiMonPref {
    System,
    AllDisplays,
    PrimaryOnly,
}

impl MultiMonPref {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::AllDisplays => "all",
            Self::PrimaryOnly => "primary",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::AllDisplays => "All Displays",
            Self::PrimaryOnly => "Primary Only",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" => Some(Self::System),
            "all" | "all_displays" => Some(Self::AllDisplays),
            "primary" | "primary_only" => Some(Self::PrimaryOnly),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ThemePref {
    System,
    Dark,
    Light,
}

impl ThemePref {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" => Some(Self::System),
            "dark" | "dark_acrylic" | "midnight" => Some(Self::Dark),
            "light" | "light_frost" => Some(Self::Light),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlassPref {
    System,
    Frosted,
    Solid,
}

impl GlassPref {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Frosted => "frosted",
            Self::Solid => "solid",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Frosted => "Frosted Glass",
            Self::Solid => "Solid",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::System => Self::Frosted,
            Self::Frosted => Self::Solid,
            Self::Solid => Self::System,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "system" => Some(Self::System),
            "frosted" | "glass" | "acrylic" | "mica" => Some(Self::Frosted),
            "solid" | "opaque" => Some(Self::Solid),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayBarMode {
    AutoSix,     // Up to 6 active tray icons beside the control area
    CompactThree,// Up to 3 active tray icons
    AllTen,      // Up to 10 active tray icons
    ChevronOnly, // 0 directly on bar; all icons in the ^ overflow flyout
}

impl TrayBarMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AutoSix => "auto6",
            Self::CompactThree => "compact3",
            Self::AllTen => "all10",
            Self::ChevronOnly => "chevron",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::AutoSix => "Auto (6)",
            Self::CompactThree => "Compact (3)",
            Self::AllTen => "Expanded (10)",
            Self::ChevronOnly => "Chevron Only",
        }
    }

    pub fn limit(self) -> usize {
        match self {
            Self::AutoSix => 6,
            Self::CompactThree => 3,
            Self::AllTen => 10,
            Self::ChevronOnly => 0,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::AutoSix => Self::CompactThree,
            Self::CompactThree => Self::AllTen,
            Self::AllTen => Self::ChevronOnly,
            Self::ChevronOnly => Self::AutoSix,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto6" | "auto" | "6" => Some(Self::AutoSix),
            "compact3" | "compact" | "3" => Some(Self::CompactThree),
            "all10" | "all" | "10" => Some(Self::AllTen),
            "chevron" | "none" | "0" => Some(Self::ChevronOnly),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GamingModePref {
    Auto, // Native Windows fullscreen suppression (exclusive & borderless fullscreen only)
    Off,  // Always allow taskbar over fullscreen apps
}

impl GamingModePref {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Auto => "Fullscreen",
            Self::Off => "Off",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Auto => Self::Off,
            Self::Off => Self::Auto,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "fullscreen" | "on" | "1" | "true" => Some(Self::Auto),
            "off" | "0" | "false" => Some(Self::Off),
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Config {
    pub layout: LayoutMode,
    pub dock_edge: DockEdge,
    pub dock_size: DockSize,
    pub align: AlignPref,
    pub multimon: MultiMonPref,
    pub theme: ThemePref,
    pub glass: GlassPref,
    pub autohide: bool,
    pub status_autohide: bool,
    pub gaming_mode: GamingModePref,
    pub tray_mode: TrayBarMode,
    pub show_previews: bool,
    pub show_seconds: bool,
    pub show_search: bool,
    pub show_coucou: bool,
    pub show_taskview: bool,
    pub show_telemetry: bool,
    pub show_desktop_btn: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            layout: LayoutMode::Win12Split,
            dock_edge: DockEdge::Top,
            dock_size: DockSize::Compact,
            align: AlignPref::System,
            multimon: MultiMonPref::System,
            theme: ThemePref::System,
            glass: GlassPref::System,
            autohide: false,
            status_autohide: true,
            gaming_mode: GamingModePref::Auto,
            tray_mode: TrayBarMode::AutoSix,
            show_previews: true,
            show_seconds: false,
            show_search: true,
            show_coucou: true,
            show_taskview: true,
            show_telemetry: true,
            show_desktop_btn: true,
        }
    }
}

fn parse_bool(v: &str) -> bool {
    matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

impl Config {
    pub fn to_ini(&self) -> String {
        format!(
            "layout={}\ndock_edge={}\ndock_size={}\nalign={}\nmultimon={}\ntheme={}\nglass={}\nautohide={}\nstatus_autohide={}\ngaming_mode={}\ntray_mode={}\nshow_previews={}\nshow_seconds={}\nshow_search={}\nshow_coucou={}\nshow_taskview={}\nshow_telemetry={}\nshow_desktop_btn={}\n",
            self.layout.as_str(),
            self.dock_edge.as_str(),
            self.dock_size.as_str(),
            self.align.as_str(),
            self.multimon.as_str(),
            self.theme.as_str(),
            self.glass.as_str(),
            if self.autohide { 1 } else { 0 },
            if self.status_autohide { 1 } else { 0 },
            self.gaming_mode.as_str(),
            self.tray_mode.as_str(),
            if self.show_previews { 1 } else { 0 },
            if self.show_seconds { 1 } else { 0 },
            if self.show_search { 1 } else { 0 },
            if self.show_coucou { 1 } else { 0 },
            if self.show_taskview { 1 } else { 0 },
            if self.show_telemetry { 1 } else { 0 },
            if self.show_desktop_btn { 1 } else { 0 },
        )
    }

    pub fn from_ini(text: &str) -> Self {
        let mut cfg = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k.trim().to_ascii_lowercase().as_str() {
                "layout" => {
                    if let Some(m) = LayoutMode::parse(v) {
                        cfg.layout = m;
                    }
                }
                "dock_edge" => {
                    if let Some(e) = DockEdge::parse(v) {
                        cfg.dock_edge = e;
                    }
                }
                "dock_size" | "size" => {
                    if let Some(sz) = DockSize::parse(v) {
                        cfg.dock_size = sz;
                    }
                }
                "align" => {
                    if let Some(a) = AlignPref::parse(v) {
                        cfg.align = a;
                    }
                }
                "multimon" => {
                    if let Some(m) = MultiMonPref::parse(v) {
                        cfg.multimon = m;
                    }
                }
                "theme" => {
                    if let Some(t) = ThemePref::parse(v) {
                        cfg.theme = t;
                    }
                }
                "glass" => {
                    if let Some(g) = GlassPref::parse(v) {
                        cfg.glass = g;
                    }
                }
                "autohide" => cfg.autohide = parse_bool(v),
                "status_autohide" => cfg.status_autohide = parse_bool(v),
                "gaming_mode" => {
                    if let Some(gm) = GamingModePref::parse(v) {
                        cfg.gaming_mode = gm;
                    }
                }
                "tray_mode" => {
                    if let Some(tm) = TrayBarMode::parse(v) {
                        cfg.tray_mode = tm;
                    }
                }
                "show_previews" => cfg.show_previews = parse_bool(v),
                "show_seconds" => cfg.show_seconds = parse_bool(v),
                "show_search" => cfg.show_search = parse_bool(v),
                "show_coucou" => cfg.show_coucou = parse_bool(v),
                "show_taskview" => cfg.show_taskview = parse_bool(v),
                "show_telemetry" => cfg.show_telemetry = parse_bool(v),
                "show_desktop_btn" => cfg.show_desktop_btn = parse_bool(v),
                _ => {}
            }
        }
        cfg
    }
}

pub fn config_path() -> PathBuf {
    let base = env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(env::temp_dir);
    base.join("gp-pc-boost").join("taskbar.ini")
}

pub fn load_config() -> Config {
    load_config_from(&config_path())
}

pub fn load_config_from(path: &Path) -> Config {
    fs::read_to_string(path)
        .ok()
        .map(|text| Config::from_ini(&text))
        .unwrap_or_default()
}

pub fn save_config(cfg: &Config) -> Result<(), String> {
    save_config_to(&config_path(), cfg)
}

pub fn save_config_to(path: &Path, cfg: &Config) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create config dir: {e}"))?;
    }
    fs::write(path, cfg.to_ini()).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ini_round_trip_and_system_defaults() {
        let def = Config::default();
        assert_eq!(def.layout, LayoutMode::Win12Split);
        assert_eq!(def.dock_edge, DockEdge::Top);
        assert_eq!(def.dock_size, DockSize::Compact);
        assert_eq!(def.align, AlignPref::System);
        assert_eq!(def.multimon, MultiMonPref::System);
        assert_eq!(def.theme, ThemePref::System);
        assert_eq!(def.glass, GlassPref::System);
        assert!(!def.autohide);
        assert!(def.status_autohide);
        assert_eq!(def.gaming_mode, GamingModePref::Auto);
        assert_eq!(def.tray_mode, TrayBarMode::AutoSix);
        assert!(def.show_previews);
        assert!(!def.show_seconds);

        let custom = Config {
            layout: LayoutMode::UnifiedTop,
            dock_edge: DockEdge::Bottom,
            dock_size: DockSize::Small,
            align: AlignPref::Left,
            multimon: MultiMonPref::PrimaryOnly,
            theme: ThemePref::Light,
            glass: GlassPref::Solid,
            autohide: true,
            status_autohide: false,
            gaming_mode: GamingModePref::Off,
            tray_mode: TrayBarMode::CompactThree,
            show_previews: false,
            show_seconds: true,
            show_search: false,
            show_coucou: false,
            show_taskview: false,
            show_telemetry: false,
            show_desktop_btn: false,
        };
        assert_eq!(Config::from_ini(&custom.to_ini()), custom);
        assert_eq!(GamingModePref::parse("fullscreen"), Some(GamingModePref::Auto));
        assert_eq!(GamingModePref::parse("off"), Some(GamingModePref::Off));
    }
}
