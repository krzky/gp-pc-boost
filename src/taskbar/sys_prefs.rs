//! Live Windows 11/12 system preferences read directly from HKCU registry (< 50 us, zero subprocesses).

use std::ptr::null_mut;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SysPrefs {
    pub mm_enabled: bool,
    pub mm_mode: u32,       // 0 = all taskbars, 1 = main + open window, 2 = where open
    pub align_center: bool, // TaskbarAl: 1 = Center, 0 = Left
    pub light_theme: bool,  // SystemUsesLightTheme == 1
    pub transparency: bool, // EnableTransparency == 1
    pub accent_argb: u32,   // 0xFFRRGGBB contrast-adjusted system accent
}

impl Default for SysPrefs {
    fn default() -> Self {
        Self {
            mm_enabled: true,
            mm_mode: 0,
            align_center: true,
            light_theme: false,
            transparency: true,
            accent_argb: 0xFF38_BDF8,
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

unsafe fn read_hkcu_dword(subkey: &str, value: &str) -> Option<u32> {
    let mut data: u32 = 0;
    let mut size: u32 = std::mem::size_of::<u32>() as u32;
    let status = RegGetValueW(
        HKEY_CURRENT_USER as HKEY,
        wide(subkey).as_ptr(),
        wide(value).as_ptr(),
        RRF_RT_REG_DWORD,
        null_mut(),
        &mut data as *mut _ as *mut _,
        &mut size,
    );
    (status == 0).then_some(data)
}

/// Converts Windows `AccentColor` DWORD (`0xAABBGGRR` COLORREF/ABGR) into opaque `0xFFRRGGBB`.
pub fn abgr_to_argb(abgr: u32) -> u32 {
    let r = abgr & 0xFF;
    let g = (abgr >> 8) & 0xFF;
    let b = (abgr >> 16) & 0xFF;
    0xFF00_0000 | (r << 16) | (g << 8) | b
}

/// Ensures system accent keeps crisp readability & Fluent pop on dark vs light glass.
pub fn contrast_safe_accent(argb: u32, is_light: bool) -> u32 {
    let r = ((argb >> 16) & 0xFF) as f32;
    let g = ((argb >> 8) & 0xFF) as f32;
    let b = (argb & 0xFF) as f32;
    // Relative perceived luminance (0..255 scale)
    let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;

    let (nr, ng, nb) = if !is_light && lum < 115.0 {
        // Lift overly dark accents (like deep navy 0x0C00D0) toward bright Fluent tint
        let t = ((130.0 - lum) / 130.0).clamp(0.28, 0.62);
        (
            (r + (255.0 - r) * t).round().clamp(0.0, 255.0) as u32,
            (g + (255.0 - g) * t).round().clamp(0.0, 255.0) as u32,
            (b + (255.0 - b) * t).round().clamp(0.0, 255.0) as u32,
        )
    } else if is_light && lum > 155.0 {
        // Deepen overly bright pastel accents on light frost so they keep contrast
        let t = ((lum - 120.0) / 135.0).clamp(0.25, 0.55);
        (
            (r * (1.0 - t)).round().clamp(0.0, 255.0) as u32,
            (g * (1.0 - t)).round().clamp(0.0, 255.0) as u32,
            (b * (1.0 - t)).round().clamp(0.0, 255.0) as u32,
        )
    } else {
        (r as u32, g as u32, b as u32)
    };
    0xFF00_0000 | (nr << 16) | (ng << 8) | nb
}

pub fn read_system_prefs() -> SysPrefs {
    unsafe {
        let adv = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced";
        let pers = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
        let dwm = r"Software\Microsoft\Windows\DWM";

        let mm_enabled = read_hkcu_dword(adv, "MMTaskbarEnabled").map_or(true, |v| v != 0);
        let mm_mode = read_hkcu_dword(adv, "MMTaskbarMode").unwrap_or(0).min(2);
        let align_center = read_hkcu_dword(adv, "TaskbarAl").map_or(true, |v| v != 0);
        let light_theme = read_hkcu_dword(pers, "SystemUsesLightTheme").is_some_and(|v| v != 0);
        let transparency = read_hkcu_dword(pers, "EnableTransparency").map_or(true, |v| v != 0);
        let raw_accent = read_hkcu_dword(dwm, "AccentColor").map(abgr_to_argb).unwrap_or(0xFF00_78D4);
        let accent_argb = contrast_safe_accent(raw_accent, light_theme);

        SysPrefs {
            mm_enabled,
            mm_mode,
            align_center,
            light_theme,
            transparency,
            accent_argb,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abgr_converts_to_argb() {
        // Live Windows AccentColor observed: 0xFF0C00D0 => R=D0, G=00, B=0C
        assert_eq!(abgr_to_argb(0xFF0C_00D0), 0xFFD0_000C);
        assert_eq!(abgr_to_argb(0xFF38_BDF8), 0xFFF8_BD38);
    }

    #[test]
    fn dark_and_light_accents_lift_or_deepen_for_contrast() {
        let dark_lifted = contrast_safe_accent(0xFFD0_000C, false);
        let lr = ((dark_lifted >> 16) & 0xFF) as f32;
        let lg = ((dark_lifted >> 8) & 0xFF) as f32;
        let lb = (dark_lifted & 0xFF) as f32;
        assert!(0.2126 * lr + 0.7152 * lg + 0.0722 * lb > 100.0);

        let light_deep = contrast_safe_accent(0xFFE0_F4FF, true);
        let dr = ((light_deep >> 16) & 0xFF) as f32;
        let dg = ((light_deep >> 8) & 0xFF) as f32;
        let db = (light_deep & 0xFF) as f32;
        assert!(0.2126 * dr + 0.7152 * dg + 0.0722 * db < 170.0);
    }
}
