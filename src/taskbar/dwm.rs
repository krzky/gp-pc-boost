//! DWM Fluent 2 materials (Mica / Acrylic / Solid) + 32-bit premultiplied DIB rasterizer + AA text renderer.

use crate::taskbar::icons::IconBitmap;
use std::{
    env, fs,
    mem::{size_of, zeroed},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    slice,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{
        Dwm::{
            DWM_SYSTEMBACKDROP_TYPE, DWM_WINDOW_CORNER_PREFERENCE, DWMNCRP_DISABLED,
            DWMSBT_MAINWINDOW, DWMSBT_NONE, DWMSBT_TRANSIENTWINDOW, DWMWA_BORDER_COLOR,
            DWMWA_NCRENDERING_POLICY, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_TRANSITIONS_FORCEDISABLED,
            DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
            DWMWCP_ROUND, DwmSetWindowAttribute,
        },
        Gdi::*,
        GdiPlus::{
            FlushIntentionSync, GdipCreateBitmapFromScan0, GdipCreateFont,
            GdipCreateFontFamilyFromName, GdipCreateSolidFill, GdipCreateStringFormat,
            GdipDeleteBrush, GdipDeleteFont, GdipDeleteFontFamily, GdipDeleteGraphics,
            GdipDeleteStringFormat, GdipDisposeImage, GdipDrawString, GdipFlush,
            GdipGetImageGraphicsContext, GdipSetSolidFillColor, GdipSetStringFormatAlign,
            GdipSetStringFormatLineAlign, GdipSetStringFormatTrimming, GdipSetTextRenderingHint,
            GpBitmap, GpBrush, GpFont, GpFontFamily, GpGraphics, GpImage, GpSolidFill,
            GpStringFormat, RectF, StringAlignmentCenter, StringAlignmentFar, StringAlignmentNear,
            StringTrimmingEllipsisCharacter, TextRenderingHintAntiAliasGridFit, UnitPixel,
        },
    },
    System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
    UI::WindowsAndMessaging::{ULW_ALPHA, UpdateLayeredWindow},
};

const PIXEL_FORMAT_32BPP_PARGB: i32 = 0x000E_200B;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BackdropKind {
    Mica,    // Long-lived top status pills
    Acrylic, // Floating App Dock, popovers, tooltips
    Solid,   // Fallback when system transparency is disabled
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FontSlot {
    Regular = 0,
    SemiBold = 1,
    Small = 2,
    Glyph = 3,
    GlyphSmall = 4,
}

#[repr(C)]
struct AccentPolicy {
    accent_state: i32,
    accent_flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct WinCompAttrData {
    attrib: u32,
    pv_data: *mut AccentPolicy,
    cb_data: usize,
}

pub unsafe fn apply_backdrop(hwnd: HWND, is_light: bool, kind: BackdropKind, border_colorref: u32) {
    let dark: i32 = if is_light { 0 } else { 1 };
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
        &dark as *const _ as *const _,
        size_of::<i32>() as u32,
    );

    let corners: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE as u32,
        &corners as *const _ as *const _,
        size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
    );

    let backdrop: DWM_SYSTEMBACKDROP_TYPE = match kind {
        BackdropKind::Mica => DWMSBT_MAINWINDOW,
        BackdropKind::Acrylic => DWMSBT_TRANSIENTWINDOW,
        BackdropKind::Solid => DWMSBT_NONE,
    };
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_SYSTEMBACKDROP_TYPE as u32,
        &backdrop as *const _ as *const _,
        size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
    );

    let u32_mod = GetModuleHandleW(wide("user32.dll").as_ptr());
    if !u32_mod.is_null()
        && let Some(proc) = GetProcAddress(u32_mod, c"SetWindowCompositionAttribute".as_ptr() as *const u8)
    {
        type SetWcaFn = unsafe extern "system" fn(HWND, *mut WinCompAttrData) -> i32;
        let set_wca: SetWcaFn = std::mem::transmute(proc);
        let mut policy = AccentPolicy {
            accent_state: if kind == BackdropKind::Solid { 0 } else { 3 }, // ACCENT_ENABLE_BLURBEHIND
            accent_flags: 2,
            gradient_color: 0,
            animation_id: 0,
        };
        let mut data = WinCompAttrData {
            attrib: 19,
            pv_data: &mut policy,
            cb_data: size_of::<AccentPolicy>(),
        };
        let _ = set_wca(hwnd, &mut data);
    }

    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_BORDER_COLOR as u32,
        &border_colorref as *const _ as *const _,
        size_of::<u32>() as u32,
    );
    // Disable Windows 11 DWM's built-in show/hide ghost fade transition so our VSync spring
    // animations own 100% of motion with zero leftover ghost frames!
    let disable_trans: i32 = 1;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_TRANSITIONS_FORCEDISABLED as u32,
        &disable_trans as *const _ as *const _,
        size_of::<i32>() as u32,
    );
}

/// Strips DWM backdrop blur, non-client drop shadow, rounded-frame clip, hardware 1px border,
/// and DWM show/hide transitions so a per-pixel alpha `WS_EX_LAYERED` window (Search Bar,
/// dynamic-height Search Results Panel, flyouts) composites purely from its premultiplied ARGB
/// bitmap with ZERO ghost shadow/border/fade residue during morphs, resizes, or close!
pub unsafe fn apply_frameless_layered(hwnd: HWND, is_light: bool) {
    let dark: i32 = if is_light { 0 } else { 1 };
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
        &dark as *const _ as *const _,
        size_of::<i32>() as u32,
    );
    let nc_off: i32 = DWMNCRP_DISABLED;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_NCRENDERING_POLICY as u32,
        &nc_off as *const _ as *const _,
        size_of::<i32>() as u32,
    );
    let disable_trans: i32 = 1;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_TRANSITIONS_FORCEDISABLED as u32,
        &disable_trans as *const _ as *const _,
        size_of::<i32>() as u32,
    );
    let corners: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_DONOTROUND;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_WINDOW_CORNER_PREFERENCE as u32,
        &corners as *const _ as *const _,
        size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
    );
    let backdrop: DWM_SYSTEMBACKDROP_TYPE = DWMSBT_NONE;
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_SYSTEMBACKDROP_TYPE as u32,
        &backdrop as *const _ as *const _,
        size_of::<DWM_SYSTEMBACKDROP_TYPE>() as u32,
    );
    let no_border: u32 = 0xFFFF_FFFE; // DWMWA_COLOR_NONE
    let _ = DwmSetWindowAttribute(
        hwnd,
        DWMWA_BORDER_COLOR as u32,
        &no_border as *const _ as *const _,
        size_of::<u32>() as u32,
    );
    let u32_mod = GetModuleHandleW(wide("user32.dll").as_ptr());
    if !u32_mod.is_null()
        && let Some(proc) = GetProcAddress(u32_mod, c"SetWindowCompositionAttribute".as_ptr() as *const u8)
    {
        type SetWcaFn = unsafe extern "system" fn(HWND, *mut WinCompAttrData) -> i32;
        let set_wca: SetWcaFn = std::mem::transmute(proc);
        let mut policy = AccentPolicy {
            accent_state: 0, // ACCENT_DISABLED
            accent_flags: 0,
            gradient_color: 0,
            animation_id: 0,
        };
        let mut data = WinCompAttrData {
            attrib: 19,
            pv_data: &mut policy,
            cb_data: size_of::<AccentPolicy>(),
        };
        let _ = set_wca(hwnd, &mut data);
    }
}

/// Atomically pushes a 32-bit premultiplied ARGB `Surface` + screen position + size to DWM.
pub unsafe fn present_layered(hwnd: HWND, x: i32, y: i32, surf: &Surface) -> bool {
    present_layered_alpha(hwnd, x, y, surf, 255)
}

/// Atomically pushes a 32-bit premultiplied ARGB `Surface` with GPU-composited `SourceConstantAlpha`.
pub unsafe fn present_layered_alpha(hwnd: HWND, x: i32, y: i32, surf: &Surface, alpha: u8) -> bool {
    if hwnd.is_null() || surf.dc.is_null() || surf.w <= 0 || surf.h <= 0 {
        return false;
    }
    let pt_dst = POINT { x, y };
    let pt_src = POINT { x: 0, y: 0 };
    let sz = SIZE { cx: surf.w, cy: surf.h };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };
    UpdateLayeredWindow(hwnd, null_mut(), &pt_dst, &sz, surf.dc, &pt_src, 0, &blend, ULW_ALPHA) != 0
}

/// Moves an already-rendered `WS_EX_LAYERED` surface without re-uploading pixels.
pub unsafe fn move_layered(hwnd: HWND, x: i32, y: i32) {
    if !hwnd.is_null() {
        let pt_dst = POINT { x, y };
        let _ = UpdateLayeredWindow(hwnd, null_mut(), &pt_dst, null(), null_mut(), null(), 0, null(), 0);
    }
}

/// Moves AND fades an already-rendered `WS_EX_LAYERED` surface purely on the DWM GPU compositor
/// (zero DIB redraws, zero pixel uploads — silky 144Hz slide+fade!).
pub unsafe fn move_layered_alpha(hwnd: HWND, x: i32, y: i32, alpha: u8) {
    if !hwnd.is_null() {
        let pt_dst = POINT { x, y };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(hwnd, null_mut(), &pt_dst, null(), null_mut(), null(), 0, &blend, ULW_ALPHA);
    }
}

#[inline]
pub fn premul(argb: u32) -> u32 {
    let a = (argb >> 24) & 0xFF;
    if a == 255 {
        return argb;
    }
    if a == 0 {
        return 0;
    }
    let r = (((argb >> 16) & 0xFF) * a + 127) / 255;
    let g = (((argb >> 8) & 0xFF) * a + 127) / 255;
    let b = ((argb & 0xFF) * a + 127) / 255;
    (a << 24) | (r << 16) | (g << 8) | b
}

#[inline]
pub fn blend_over(dst: u32, src_premul: u32) -> u32 {
    let sa = (src_premul >> 24) & 0xFF;
    if sa == 255 {
        return src_premul;
    }
    if sa == 0 {
        return dst;
    }
    let inv = 255 - sa;
    let da = (dst >> 24) & 0xFF;
    let dr = (dst >> 16) & 0xFF;
    let dg = (dst >> 8) & 0xFF;
    let db = dst & 0xFF;

    let sr = (src_premul >> 16) & 0xFF;
    let sg = (src_premul >> 8) & 0xFF;
    let sb = src_premul & 0xFF;

    let a = (sa + (da * inv + 127) / 255).min(255);
    let r = (sr + (dr * inv + 127) / 255).min(255);
    let g = (sg + (dg * inv + 127) / 255).min(255);
    let b = (sb + (db * inv + 127) / 255).min(255);
    (a << 24) | (r << 16) | (g << 8) | b
}

#[inline]
fn scale_alpha_premul(src_premul: u32, cov: f32) -> u32 {
    let c = (cov.clamp(0.0, 1.0) * 255.0).round() as u32;
    if c == 255 {
        return src_premul;
    }
    if c == 0 {
        return 0;
    }
    let a = (((src_premul >> 24) & 0xFF) * c + 127) / 255;
    let r = (((src_premul >> 16) & 0xFF) * c + 127) / 255;
    let g = (((src_premul >> 8) & 0xFF) * c + 127) / 255;
    let b = ((src_premul & 0xFF) * c + 127) / 255;
    (a << 24) | (r << 16) | (g << 8) | b
}

/// Persistent 32-bit top-down ARGB DIB backbuffer for zero-allocation repaints.
pub struct Surface {
    pub dc: HDC,
    bmp: HBITMAP,
    bits: *mut u32,
    pub w: i32,
    pub h: i32,
}

impl Default for Surface {
    fn default() -> Self {
        Self::new()
    }
}

impl Surface {
    pub fn new() -> Self {
        Self {
            dc: null_mut(),
            bmp: null_mut(),
            bits: null_mut(),
            w: 0,
            h: 0,
        }
    }

    pub unsafe fn ensure_dims(&mut self, w: i32, h: i32) -> bool {
        let (w, h) = (w.max(1), h.max(1));
        if !self.dc.is_null() && !self.bmp.is_null() && self.w == w && self.h == h {
            return true;
        }
        let sdc = GetDC(null_mut());
        let ok = self.ensure_size(sdc, w, h);
        if !sdc.is_null() {
            ReleaseDC(null_mut(), sdc);
        }
        ok
    }

    pub unsafe fn ensure_size(&mut self, wdc: HDC, w: i32, h: i32) -> bool {
        let (w, h) = (w.max(1), h.max(1));
        if !self.dc.is_null() && !self.bmp.is_null() && self.w == w && self.h == h {
            return true;
        }
        if self.dc.is_null() {
            self.dc = CreateCompatibleDC(wdc);
            if self.dc.is_null() {
                return false;
            }
        }
        if !self.bmp.is_null() {
            DeleteObject(self.bmp as _);
            self.bmp = null_mut();
            self.bits = null_mut();
        }
        let mut bmi: BITMAPINFO = zeroed();
        bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.bmiHeader.biWidth = w;
        bmi.bmiHeader.biHeight = -h;
        bmi.bmiHeader.biPlanes = 1;
        bmi.bmiHeader.biBitCount = 32;
        bmi.bmiHeader.biCompression = BI_RGB;

        let mut raw = null_mut();
        let bmp = CreateDIBSection(self.dc, &bmi, DIB_RGB_COLORS, &mut raw, null_mut(), 0);
        if bmp.is_null() || raw.is_null() {
            return false;
        }
        SelectObject(self.dc, bmp as _);
        self.bmp = bmp;
        self.bits = raw as *mut u32;
        self.w = w;
        self.h = h;
        true
    }

    unsafe fn pixels_mut(&mut self) -> &mut [u32] {
        if self.bits.is_null() || self.w <= 0 || self.h <= 0 {
            &mut []
        } else {
            slice::from_raw_parts_mut(self.bits, (self.w * self.h) as usize)
        }
    }

    pub unsafe fn clear_transparent(&mut self) {
        self.pixels_mut().fill(0);
    }

    pub unsafe fn clear_island(&mut self, rad: f32, fill_argb: u32, border_argb: u32, top_sheen_argb: u32) {
        self.pixels_mut().fill(0);
        let w = self.w;
        let h = self.h;
        self.fill_round_rect(0, 0, w, h, rad, fill_argb);
        self.stroke_round_rect(0, 0, w, h, rad, 1.0, border_argb);
        let inset = (rad.ceil() as i32).max(4);
        if w > inset * 2 && h > 4 && (top_sheen_argb >> 24) > 0 {
            self.fill_rect(inset, 1, w - inset * 2, 1, top_sheen_argb);
        }
    }

    pub unsafe fn clear_glass(&mut self, argb_tint: u32, top_sheen_argb: u32) {
        self.pixels_mut().fill(premul(argb_tint));
        if self.w > 4 && self.h > 2 && (top_sheen_argb >> 24) > 0 {
            self.fill_rect(2, 0, self.w - 4, 1, top_sheen_argb);
        }
    }

    pub unsafe fn clear(&mut self, argb_tint: u32) {
        self.pixels_mut().fill(premul(argb_tint));
    }

    pub unsafe fn stroke_round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, rad: f32, thickness: f32, argb: u32) {
        let src = premul(argb);
        if src == 0 || w <= 2 || h <= 2 || thickness <= 0.0 {
            return;
        }
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + w).min(self.w);
        let y1 = (y + h).min(self.h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let hw = w as f32 * 0.5;
        let hh = h as f32 * 0.5;
        let r = (rad - thickness * 0.5).clamp(0.0, (hw - thickness * 0.5).min(hh - thickness * 0.5));
        let cx = x as f32 + hw;
        let cy = y as f32 + hh;
        let bx = (hw - thickness * 0.5) - r;
        let by = (hh - thickness * 0.5) - r;
        let half_t = thickness * 0.5;
        let stride = self.w;
        let buf = self.pixels_mut();

        for yy in y0..y1 {
            let py = yy as f32 + 0.5;
            let dy = (py - cy).abs() - by;
            let row = (yy * stride) as usize;
            for xx in x0..x1 {
                let px = xx as f32 + 0.5;
                let dx = (px - cx).abs() - bx;
                let dist = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - r;
                let ring_cov = (half_t + 0.5 - dist.abs()).clamp(0.0, 1.0);
                if ring_cov > 0.0 {
                    let idx = row + xx as usize;
                    buf[idx] = blend_over(buf[idx], scale_alpha_premul(src, ring_cov));
                }
            }
        }
    }

    pub unsafe fn save_bmp(&self, path: &Path) {
        if self.bits.is_null() || self.w <= 0 || self.h <= 0 {
            return;
        }
        let px_bytes = (self.w * self.h * 4) as u32;
        let file_size = 54 + px_bytes;
        let mut out = Vec::with_capacity(file_size as usize);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&file_size.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&self.w.to_le_bytes());
        out.extend_from_slice(&(-self.h).to_le_bytes()); // top-down
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&px_bytes.to_le_bytes());
        out.extend_from_slice(&2835i32.to_le_bytes());
        out.extend_from_slice(&2835i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        let px = slice::from_raw_parts(self.bits as *const u8, px_bytes as usize);
        out.extend_from_slice(px);
        let _ = fs::write(path, out);
    }

    pub unsafe fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, argb: u32) {
        let p = premul(argb);
        if p == 0 {
            return;
        }
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + w).min(self.w);
        let y1 = (y + h).min(self.h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let stride = self.w;
        let buf = self.pixels_mut();
        for yy in y0..y1 {
            let row = (yy * stride) as usize;
            for xx in x0..x1 {
                let idx = row + xx as usize;
                buf[idx] = blend_over(buf[idx], p);
            }
        }
    }

    /// Analytic signed-distance-field anti-aliased rounded rectangle in premultiplied ARGB.
    pub unsafe fn fill_round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, rad: f32, argb: u32) {
        let src = premul(argb);
        if src == 0 || w <= 0 || h <= 0 {
            return;
        }
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + w).min(self.w);
        let y1 = (y + h).min(self.h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let hw = w as f32 * 0.5;
        let hh = h as f32 * 0.5;
        let r = rad.clamp(0.0, hw.min(hh));
        let cx = x as f32 + hw;
        let cy = y as f32 + hh;
        let bx = hw - r;
        let by = hh - r;
        let stride = self.w;
        let buf = self.pixels_mut();

        for yy in y0..y1 {
            let py = yy as f32 + 0.5;
            let dy = (py - cy).abs() - by;
            let row = (yy * stride) as usize;
            for xx in x0..x1 {
                let px = xx as f32 + 0.5;
                let dx = (px - cx).abs() - bx;
                let dist = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - r;
                let cov = (0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let s = scale_alpha_premul(src, cov);
                    let idx = row + xx as usize;
                    buf[idx] = blend_over(buf[idx], s);
                }
            }
        }
    }

    pub unsafe fn fill_circle(&mut self, cx: f32, cy: f32, r: f32, argb: u32) {
        let src = premul(argb);
        if src == 0 || r <= 0.0 {
            return;
        }
        let x0 = ((cx - r - 1.0).floor() as i32).max(0);
        let y0 = ((cy - r - 1.0).floor() as i32).max(0);
        let x1 = ((cx + r + 1.0).ceil() as i32).min(self.w);
        let y1 = ((cy + r + 1.0).ceil() as i32).min(self.h);
        let stride = self.w;
        let buf = self.pixels_mut();
        for yy in y0..y1 {
            let py = yy as f32 + 0.5;
            let dy = py - cy;
            let row = (yy * stride) as usize;
            for xx in x0..x1 {
                let px = xx as f32 + 0.5;
                let dist = (px - cx).hypot(dy) - r;
                let cov = (0.5 - dist).clamp(0.0, 1.0);
                if cov > 0.0 {
                    let idx = row + xx as usize;
                    buf[idx] = blend_over(buf[idx], scale_alpha_premul(src, cov));
                }
            }
        }
    }

    pub unsafe fn blit_icon(&mut self, x: i32, y: i32, icon: &IconBitmap, alpha: f32) {
        if icon.w <= 0 || icon.h <= 0 || alpha <= 0.0 {
            return;
        }
        let x0 = x.max(0);
        let y0 = y.max(0);
        let x1 = (x + icon.w).min(self.w);
        let y1 = (y + icon.h).min(self.h);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let stride = self.w;
        let buf = self.pixels_mut();
        for yy in y0..y1 {
            let sy = yy - y;
            let srow = (sy * icon.w) as usize;
            let drow = (yy * stride) as usize;
            for xx in x0..x1 {
                let sx = xx - x;
                let px = icon.pixels[srow + sx as usize];
                if px != 0 {
                    let s = if alpha >= 0.999 { px } else { scale_alpha_premul(px, alpha) };
                    let idx = drow + xx as usize;
                    buf[idx] = blend_over(buf[idx], s);
                }
            }
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            if !self.bmp.is_null() {
                DeleteObject(self.bmp as _);
            }
            if !self.dc.is_null() {
                DeleteDC(self.dc);
            }
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

fn font_exists(name: &str) -> bool {
    env::var_os("WINDIR").is_some_and(|w| PathBuf::from(w).join("Fonts").join(name).is_file())
}

/// Cached GDI+ anti-aliased text & glyph renderer writing crisp premultiplied alpha onto `Surface.dc`.
pub struct TextRenderer {
    text_family: *mut GpFontFamily,
    icon_family: *mut GpFontFamily,
    fonts: [*mut GpFont; 5],
    fmt_left: *mut GpStringFormat,
    fmt_center: *mut GpStringFormat,
    fmt_right: *mut GpStringFormat,
    brush: *mut GpSolidFill,
}

impl TextRenderer {
    pub unsafe fn new(scale: f32) -> Self {
        let text_face = if font_exists("SegUIVar.ttf") {
            "Segoe UI Variable Text"
        } else {
            "Segoe UI"
        };
        let icon_face = if font_exists("SegoeIcons.ttf") {
            "Segoe Fluent Icons"
        } else {
            "Segoe MDL2 Assets"
        };

        let mut text_family = null_mut();
        GdipCreateFontFamilyFromName(wide(text_face).as_ptr(), null_mut(), &mut text_family);
        if text_family.is_null() {
            GdipCreateFontFamilyFromName(wide("Segoe UI").as_ptr(), null_mut(), &mut text_family);
        }

        let mut icon_family = null_mut();
        GdipCreateFontFamilyFromName(wide(icon_face).as_ptr(), null_mut(), &mut icon_family);
        if icon_family.is_null() {
            icon_family = text_family;
        }

        let mk_font = |fam: *mut GpFontFamily, px: f32, style: i32| -> *mut GpFont {
            let mut f = null_mut();
            if !fam.is_null() {
                GdipCreateFont(fam, px * scale, style, UnitPixel, &mut f);
            }
            f
        };

        let fonts = [
            mk_font(text_family, 13.0, 0),
            mk_font(text_family, 13.0, 1),
            mk_font(text_family, 11.5, 0),
            mk_font(icon_family, 16.0, 0),
            mk_font(icon_family, 13.5, 0),
        ];

        let mk_fmt = |align: i32| -> *mut GpStringFormat {
            let mut fmt = null_mut();
            GdipCreateStringFormat(0x0000_1000, 0, &mut fmt);
            if !fmt.is_null() {
                GdipSetStringFormatAlign(fmt, align);
                GdipSetStringFormatLineAlign(fmt, StringAlignmentCenter);
                GdipSetStringFormatTrimming(fmt, StringTrimmingEllipsisCharacter);
            }
            fmt
        };

        let mut brush = null_mut();
        GdipCreateSolidFill(0xFFFF_FFFF, &mut brush);

        Self {
            text_family,
            icon_family,
            fonts,
            fmt_left: mk_fmt(StringAlignmentNear),
            fmt_center: mk_fmt(StringAlignmentCenter),
            fmt_right: mk_fmt(StringAlignmentFar),
            brush,
        }
    }

    pub unsafe fn draw_text(
        &self,
        surf: &mut Surface,
        slot: FontSlot,
        s: &str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        argb: u32,
        align: TextAlign,
    ) {
        let font = self.fonts[slot as usize];
        if font.is_null()
            || self.brush.is_null()
            || surf.bits.is_null()
            || surf.w <= 0
            || surf.h <= 0
            || s.is_empty()
            || w <= 0.0
            || h <= 0.0
        {
            return;
        }
        let mut gbmp: *mut GpBitmap = null_mut();
        if GdipCreateBitmapFromScan0(
            surf.w,
            surf.h,
            surf.w * 4,
            PIXEL_FORMAT_32BPP_PARGB,
            surf.bits as *mut u8,
            &mut gbmp,
        ) != 0
            || gbmp.is_null()
        {
            return;
        }
        let mut g: *mut GpGraphics = null_mut();
        if GdipGetImageGraphicsContext(gbmp as *mut GpImage, &mut g) != 0 || g.is_null() {
            GdipDisposeImage(gbmp as *mut GpImage);
            return;
        }
        GdipSetTextRenderingHint(g, TextRenderingHintAntiAliasGridFit);
        GdipSetSolidFillColor(self.brush, argb);
        let fmt = match align {
            TextAlign::Left => self.fmt_left,
            TextAlign::Center => self.fmt_center,
            TextAlign::Right => self.fmt_right,
        };
        let rect = RectF {
            X: x,
            Y: y,
            Width: w,
            Height: h,
        };
        let wtext: Vec<u16> = s.encode_utf16().collect();
        GdipDrawString(
            g,
            wtext.as_ptr(),
            wtext.len() as i32,
            font,
            &rect,
            fmt,
            self.brush as *const GpBrush,
        );
        GdipFlush(g, FlushIntentionSync);
        GdipDeleteGraphics(g);
        GdipDisposeImage(gbmp as *mut GpImage);
    }
}

impl Drop for TextRenderer {
    fn drop(&mut self) {
        unsafe {
            for f in self.fonts {
                if !f.is_null() {
                    GdipDeleteFont(f);
                }
            }
            for fmt in [self.fmt_left, self.fmt_center, self.fmt_right] {
                if !fmt.is_null() {
                    GdipDeleteStringFormat(fmt);
                }
            }
            if !self.brush.is_null() {
                GdipDeleteBrush(self.brush as *mut GpBrush);
            }
            if !self.icon_family.is_null() && self.icon_family != self.text_family {
                GdipDeleteFontFamily(self.icon_family);
            }
            if !self.text_family.is_null() {
                GdipDeleteFontFamily(self.text_family);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premul_and_blend_over_math() {
        assert_eq!(premul(0x00FF_FFFF), 0);
        assert_eq!(premul(0xFFFF_8000), 0xFFFF_8000);
        assert_eq!(premul(0x80FF_0000), 0x8080_0000);
        assert_eq!(blend_over(0xFF00_0000, 0xFFFF_FFFF), 0xFFFF_FFFF);
        assert_eq!(blend_over(0xFF20_4060, 0), 0xFF20_4060);
    }

    #[test]
    fn pargb_text_preserves_alpha_without_punching_holes() {
        unsafe {
            use windows_sys::Win32::Graphics::GdiPlus::{
                GdiplusShutdown, GdiplusStartup, GdiplusStartupInput,
            };
            let mut tok = 0usize;
            let mut inp: GdiplusStartupInput = zeroed();
            inp.GdiplusVersion = 1;
            GdiplusStartup(&mut tok, &inp, null_mut());

            let screen_dc = GetDC(null_mut());
            let mut surf = Surface::new();
            assert!(surf.ensure_size(screen_dc, 180, 36));
            surf.clear_glass(0xCC16_1922, 0x20FF_FFFF);
            let tr = TextRenderer::new(1.0);
            tr.draw_text(&mut surf, FontSlot::SemiBold, "Thu, Nov 26 · 21:40", 4.0, 0.0, 172.0, 36.0, 0xFFF5_F6F8, TextAlign::Center);

            let px = surf.pixels_mut();
            let min_alpha = px.iter().map(|p| (p >> 24) & 0xFF).min().unwrap_or(0);
            let max_alpha = px.iter().map(|p| (p >> 24) & 0xFF).max().unwrap_or(0);
            assert!(min_alpha >= 0xC8, "text rendering must never drop alpha below glass tint (got 0x{min_alpha:02X})");
            assert!(max_alpha >= 0xF0, "text glyph strokes must raise alpha near opaque (got 0x{max_alpha:02X})");

            drop(tr);
            drop(surf);
            ReleaseDC(null_mut(), screen_dc);
            GdiplusShutdown(tok);
        }
    }
}
