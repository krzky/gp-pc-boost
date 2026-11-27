//! Async off-thread icon extraction (Win32 .exe/.ico/.lnk + MSIX/UWP AppxManifest PNGs + live HWND).
//! The UI thread never blocks on disk I/O, COM shortcuts, or cross-process WM_GETICON.

use std::{
    collections::{HashMap, HashSet},
    env, fs,
    mem::{size_of, zeroed},
    path::{Path, PathBuf},
    ptr::null_mut,
    slice,
    sync::{
        Arc, Mutex,
        mpsc::{self, Sender},
    },
    thread,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{
        Gdi::*,
        GdiPlus::{
            BitmapData, GdipBitmapLockBits, GdipBitmapUnlockBits, GdipCreateBitmapFromFile,
            GdipDisposeImage, GdipGetImageHeight, GdipGetImageWidth, GpBitmap, GpImage,
            ImageLockModeRead, Rect as GpRect,
        },
    },
    System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW,
        RegGetValueW, RegOpenKeyExW,
    },
    UI::{
        Shell::{ExtractIconExW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGetFileInfoW},
        WindowsAndMessaging::*,
    },
};

const PIXEL_FORMAT_32BPP_PARGB: i32 = 0x000E_200B;

#[derive(Clone)]
pub struct IconBitmap {
    pub w: i32,
    pub h: i32,
    pub pixels: Vec<u32>, // premultiplied 0xAARRGGBB
}

#[derive(Clone, Debug)]
pub enum IconRequest {
    App {
        key: String,
        path: String,
        fallback_hwnd: usize,
    },
}

impl IconRequest {
    fn key(&self) -> &str {
        match self {
            Self::App { key, .. } => key,
        }
    }
}

pub struct IconCache {
    map: Arc<Mutex<HashMap<String, Arc<IconBitmap>>>>,
    pending: Arc<Mutex<HashSet<String>>>,
    tx: Sender<(IconRequest, i32, isize, u32)>,
}

#[link(name = "ole32")]
unsafe extern "system" {
    fn CoInitializeEx(pv_reserved: *const core::ffi::c_void, dw_coinit: u32) -> i32;
}

impl IconCache {
    pub fn new() -> Self {
        let map = Arc::new(Mutex::new(HashMap::new()));
        let pending = Arc::new(Mutex::new(HashSet::new()));
        let (tx, rx) = mpsc::channel::<(IconRequest, i32, isize, u32)>();

        let map_w = Arc::clone(&map);
        let pend_w = Arc::clone(&pending);
        thread::spawn(move || unsafe {
            let _ = CoInitializeEx(core::ptr::null(), 0x2); // COINIT_APARTMENTTHREADED
            while let Ok((req, target_px, notify_hwnd, notify_msg)) = rx.recv() {
                let cache_key = format!("{}@{target_px}", req.key());
                let bmp = match &req {
                    IconRequest::App { path, fallback_hwnd, .. } => {
                        let from_path = if !path.is_empty() {
                            load_path_icon(path, target_px)
                        } else {
                            None
                        };
                        from_path.or_else(|| {
                            if *fallback_hwnd != 0 {
                                load_hwnd_icon(*fallback_hwnd as HWND, target_px)
                            } else {
                                None
                            }
                        })
                    }
                };
                if let Some(bmp) = bmp {
                    map_w.lock().unwrap().insert(cache_key.clone(), Arc::new(bmp));
                    if notify_hwnd != 0 {
                        PostMessageW(notify_hwnd as HWND, notify_msg, 0, 0);
                    }
                }
                pend_w.lock().unwrap().remove(&cache_key);
            }
        });

        Self { map, pending, tx }
    }

    pub fn loaded_count(&self) -> usize {
        self.map.lock().unwrap().len()
    }

    pub fn get_cached(&self, key: &str) -> Option<Arc<IconBitmap>> {
        let guard = self.map.lock().unwrap();
        if let Some(hit) = guard.get(key) {
            return Some(Arc::clone(hit));
        }
        let prefix = format!("{key}@");
        guard
            .iter()
            .find(|(k, _)| k.starts_with(&prefix))
            .map(|(_, v)| Arc::clone(v))
    }

    pub fn get_or_request(
        &self,
        req: IconRequest,
        target_px: i32,
        notify_hwnd: HWND,
        notify_msg: u32,
    ) -> Option<Arc<IconBitmap>> {
        let cache_key = format!("{}@{target_px}", req.key());
        if let Some(hit) = self.map.lock().unwrap().get(&cache_key) {
            return Some(Arc::clone(hit));
        }
        let mut pend = self.pending.lock().unwrap();
        if pend.insert(cache_key) {
            let _ = self.tx.send((req, target_px, notify_hwnd as isize, notify_msg));
        }
        None
    }
}

impl Default for IconCache {
    fn default() -> Self {
        Self::new()
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

#[derive(Clone, Debug, Default)]
pub struct LnkInfo {
    pub target_exe: Option<String>,
    pub icon_location: Option<String>,
    pub icon_index: i32,
}

/// Pure-Rust MS-SHLLINK reader extracting both target path and explicit IconLocation/IconIndex.
pub fn parse_lnk_info(bytes: &[u8]) -> Option<LnkInfo> {
    if bytes.len() < 76 {
        return None;
    }
    let header_size = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    if header_size != 0x4C {
        return None;
    }
    let flags = u32::from_le_bytes(bytes[20..24].try_into().ok()?);
    let icon_index = i32::from_le_bytes(bytes[56..60].try_into().ok()?);
    let mut off = 76usize;
    let mut info_out = LnkInfo {
        target_exe: None,
        icon_location: None,
        icon_index,
    };

    // HasLinkTargetIDList (bit 0)
    if (flags & 0x01) != 0 {
        if off + 2 > bytes.len() {
            return None;
        }
        let id_sz = u16::from_le_bytes(bytes[off..off + 2].try_into().ok()?) as usize;
        off = off.checked_add(2 + id_sz)?;
    }

    // HasLinkInfo (bit 1)
    if (flags & 0x02) != 0 && off + 4 <= bytes.len() {
        let info_sz = u32::from_le_bytes(bytes[off..off + 4].try_into().ok()?) as usize;
        if info_sz >= 28 && off + info_sz <= bytes.len() {
            let info = &bytes[off..off + info_sz];
            let hdr_sz = u32::from_le_bytes(info[4..8].try_into().ok()?) as usize;
            let info_flags = u32::from_le_bytes(info[8..12].try_into().ok()?);
            if (info_flags & 0x01) != 0 {
                if hdr_sz >= 0x24 && info.len() >= 32 {
                    let u_off = u32::from_le_bytes(info[28..32].try_into().ok()?) as usize;
                    if let Some(s) = read_utf16_nul(info, u_off)
                        && !s.is_empty()
                    {
                        info_out.target_exe = Some(s);
                    }
                }
                if info_out.target_exe.is_none() {
                    let a_off = u32::from_le_bytes(info[16..20].try_into().ok()?) as usize;
                    if let Some(s) = read_ascii_nul(info, a_off)
                        && !s.is_empty()
                    {
                        info_out.target_exe = Some(s);
                    }
                }
            }
        }
        off = off.checked_add(info_sz)?;
    }

    // StringData: bit 2 Name, bit 3 RelPath, bit 4 WorkingDir, bit 5 Args, bit 6 IconLocation, bit 7 IsUnicode
    let is_unicode = (flags & 0x80) != 0;
    for bit in [0x04u32, 0x08, 0x10, 0x20, 0x40] {
        if (flags & bit) != 0 {
            if off + 2 > bytes.len() {
                break;
            }
            let count = u16::from_le_bytes(bytes[off..off + 2].try_into().ok()?) as usize;
            off += 2;
            let byte_len = if is_unicode { count * 2 } else { count };
            if off + byte_len > bytes.len() {
                break;
            }
            if bit == 0x40 {
                let s = if is_unicode {
                    let u: Vec<u16> = bytes[off..off + byte_len]
                        .chunks_exact(2)
                        .map(|c| u16::from_le_bytes([c[0], c[1]]))
                        .collect();
                    String::from_utf16_lossy(&u)
                } else {
                    String::from_utf8_lossy(&bytes[off..off + byte_len]).into_owned()
                };
                let trimmed = s.trim();
                if !trimmed.is_empty() {
                    info_out.icon_location = Some(expand_env_vars(trimmed));
                }
            }
            off += byte_len;
        }
    }
    Some(info_out)
}

/// Pure-Rust MS-SHLLINK target path reader (zero COM calls).
pub fn parse_lnk_target(bytes: &[u8]) -> Option<String> {
    let info = parse_lnk_info(bytes)?;
    info.target_exe.or(info.icon_location)
}

pub fn expand_env_vars(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        if let Some(end) = after.find('%') {
            let var = &after[..end];
            if !var.is_empty()
                && let Ok(val) = env::var(var)
            {
                out.push_str(&val);
            } else {
                out.push('%');
                out.push_str(var);
                out.push('%');
            }
            rest = &after[end + 1..];
        } else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        }
    }
    out.push_str(rest);
    out
}

#[derive(Clone, Debug)]
pub struct MsixAppInfo {
    pub display_name: Option<String>,
    pub logo_path: Option<PathBuf>,
}

/// Derives MSIX PackageFamilyName (`Name_PublisherId`) from PackageFullName (`Name_Ver_Arch_Res_PubId`).
pub fn package_family_from_full(full_name: &str) -> Option<String> {
    let first_us = full_name.find('_')?;
    let last_us = full_name.rfind('_')?;
    if last_us <= first_us + 1 {
        return None;
    }
    Some(format!("{}_{}", &full_name[..first_us], &full_name[last_us + 1..]))
}

/// Maps lowercase `PackageFamilyName` -> `PackageRootFolder` for all installed user MSIX packages.
pub unsafe fn resolve_msix_packages() -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let subkey = wide(
        r"Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppModel\Repository\Packages",
    );
    let mut hkey: HKEY = null_mut();
    if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey) != 0 {
        return out;
    }
    let val_name = wide("PackageRootFolder");
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
        let full_name = String::from_utf16_lossy(&name_buf[..name_len as usize]);
        let Some(family) = package_family_from_full(&full_name) else {
            continue;
        };
        let mut child_w = name_buf[..name_len as usize].to_vec();
        child_w.push(0);
        let mut root_buf = [0u16; 520];
        let mut cb = (root_buf.len() * 2) as u32;
        if RegGetValueW(
            hkey,
            child_w.as_ptr(),
            val_name.as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            root_buf.as_mut_ptr() as *mut _,
            &mut cb,
        ) == 0
            && cb >= 2
        {
            let chars = (cb as usize / 2).saturating_sub(1);
            let root = PathBuf::from(String::from_utf16_lossy(&root_buf[..chars]));
            if root.is_dir() {
                out.insert(family.to_ascii_lowercase(), root);
            }
        }
    }
    RegCloseKey(hkey);
    out
}

/// Finds the MSIX package root folder containing `AppxManifest.xml` for an exe inside `WindowsApps`.
pub fn find_windowsapps_root(exe_path: &str) -> Option<PathBuf> {
    let lower = exe_path.to_ascii_lowercase();
    if !lower.contains("\\windowsapps\\") {
        return None;
    }
    let mut cur = Path::new(exe_path).parent()?;
    for _ in 0..6 {
        if cur.join("AppxManifest.xml").is_file() {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
    None
}

/// Parses `<root>\AppxManifest.xml` for display name and best unplated taskbar icon PNG.
pub fn msix_manifest_info(root: &Path, app_id: Option<&str>) -> Option<MsixAppInfo> {
    let xml = fs::read_to_string(root.join("AppxManifest.xml")).ok()?;
    let (display_name, rel_logo) = extract_manifest_logo_and_name(&xml, app_id);
    let logo_path = rel_logo.and_then(|rel| find_best_msix_asset(root, &rel));
    Some(MsixAppInfo { display_name, logo_path })
}

pub fn extract_manifest_logo_and_name(xml: &str, app_id: Option<&str>) -> (Option<String>, Option<String>) {
    // Narrow to the matching <Application Id="..."> block if present
    let section = app_id
        .and_then(|id| {
            let pat = format!("Id=\"{id}\"");
            xml.find(&pat).map(|pos| {
                let tail = &xml[pos..];
                let end = tail.find("</Application>").unwrap_or(tail.len());
                &tail[..end]
            })
        })
        .unwrap_or(xml);

    let pick_attr = |hay: &str, attr: &str| -> Option<String> {
        let pat = format!("{attr}=\"");
        let start = hay.find(&pat)? + pat.len();
        let end = hay[start..].find('"')?;
        let val = hay[start..start + end].trim();
        if val.is_empty() { None } else { Some(val.to_string()) }
    };
    let pick_tag = |hay: &str, tag: &str| -> Option<String> {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start = hay.find(&open)? + open.len();
        let end = hay[start..].find(&close)?;
        let val = hay[start..start + end].trim();
        if val.is_empty() { None } else { Some(val.to_string()) }
    };

    let raw_name = pick_attr(section, "DisplayName")
        .or_else(|| pick_tag(xml, "DisplayName"))
        .filter(|n| !n.to_ascii_lowercase().starts_with("ms-resource:"));

    let rel_logo = pick_attr(section, "Square44x44Logo")
        .or_else(|| pick_attr(xml, "Square44x44Logo"))
        .or_else(|| pick_attr(section, "Square150x150Logo"))
        .or_else(|| pick_tag(xml, "Logo"));

    (raw_name, rel_logo)
}

pub fn score_msix_asset(file_name_lower: &str, stem_lower: &str) -> Option<i32> {
    if !(file_name_lower.ends_with(".png") || file_name_lower.ends_with(".ico")) {
        return None;
    }
    let no_ext = file_name_lower.rsplit_once('.')?.0;
    if no_ext != stem_lower && !no_ext.starts_with(&format!("{stem_lower}.")) {
        return None;
    }
    if no_ext.contains("contrast-white") || no_ext.contains("contrast-black") {
        return None;
    }
    let mut score = 10;
    if no_ext.contains("altform-unplated") {
        score += 220;
    } else if no_ext.contains("altform-lightunplated") {
        score += 120;
    } else if no_ext.contains("altform-colorful_theme-dark") {
        score += 160;
    }
    if no_ext.contains("targetsize-48") {
        score += 110;
    } else if no_ext.contains("targetsize-44") || no_ext.contains("targetsize-36") {
        score += 105;
    } else if no_ext.contains("targetsize-32") {
        score += 100;
    } else if no_ext.contains("targetsize-64") || no_ext.contains("targetsize-96") {
        score += 95;
    } else if no_ext.contains("targetsize-24") {
        score += 85;
    } else if no_ext.contains("scale-200") {
        score += 65;
    } else if no_ext.contains("scale-150") {
        score += 55;
    } else if no_ext.contains("scale-125") {
        score += 45;
    } else if no_ext.contains("scale-100") || no_ext == stem_lower {
        score += 35;
    }
    Some(score)
}

pub fn find_best_msix_asset(root: &Path, rel_logo: &str) -> Option<PathBuf> {
    let rel_norm = rel_logo.replace('/', "\\");
    let full = root.join(&rel_norm);
    let dir = full.parent()?;
    let stem = full.file_stem()?.to_string_lossy().to_ascii_lowercase();
    let mut best: Option<(i32, PathBuf)> = None;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let fname = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if let Some(sc) = score_msix_asset(&fname, &stem)
                && best.as_ref().is_none_or(|(bs, _)| sc > *bs)
            {
                best = Some((sc, p));
            }
        }
    }
    best.map(|(_, p)| p).or_else(|| full.is_file().then_some(full))
}

unsafe fn load_image_file_icon(path: &Path, target_px: i32) -> Option<IconBitmap> {
    if !path.is_file() || target_px <= 0 {
        return None;
    }
    let wpath = wide(&path.to_string_lossy());
    let mut bmp: *mut GpBitmap = null_mut();
    if GdipCreateBitmapFromFile(wpath.as_ptr(), &mut bmp) != 0 || bmp.is_null() {
        return None;
    }
    let mut w = 0u32;
    let mut h = 0u32;
    let ok_dims = GdipGetImageWidth(bmp as *mut GpImage, &mut w) == 0
        && GdipGetImageHeight(bmp as *mut GpImage, &mut h) == 0
        && w > 0
        && h > 0;
    if !ok_dims {
        GdipDisposeImage(bmp as *mut GpImage);
        return None;
    }
    let rect = GpRect { X: 0, Y: 0, Width: w as i32, Height: h as i32 };
    let mut data: BitmapData = zeroed();
    let mut result = None;
    if GdipBitmapLockBits(bmp, &rect, ImageLockModeRead as u32, PIXEL_FORMAT_32BPP_PARGB, &mut data) == 0
        && !data.Scan0.is_null()
    {
        let stride = data.Stride;
        let mut pixels = vec![0u32; (w * h) as usize];
        for y in 0..h as i32 {
            let row_ptr = if stride >= 0 {
                (data.Scan0 as *const u8).offset((y * stride) as isize)
            } else {
                (data.Scan0 as *const u8).offset(((h as i32 - 1 - y) * (-stride)) as isize)
            };
            let row = slice::from_raw_parts(row_ptr as *const u32, w as usize);
            pixels[(y as usize * w as usize)..((y as usize + 1) * w as usize)].copy_from_slice(row);
        }
        GdipBitmapUnlockBits(bmp, &mut data);
        result = Some(downsample_premul(w as i32, h as i32, &pixels, target_px));
    }
    GdipDisposeImage(bmp as *mut GpImage);
    result
}

unsafe fn extract_icon_from_binary(path: &str, index: i32, target_px: i32) -> Option<IconBitmap> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".bmp") {
        return load_image_file_icon(Path::new(path), target_px);
    }
    let w = wide(path);
    if lower.ends_with(".ico") {
        let h = LoadImageW(null_mut(), w.as_ptr(), IMAGE_ICON, 48, 48, LR_LOADFROMFILE) as HICON;
        if !h.is_null() {
            let bmp = extract_hicon_bitmap(h, target_px);
            DestroyIcon(h);
            if bmp.is_some() {
                return bmp;
            }
        }
        if let Some(bmp) = load_image_file_icon(Path::new(path), target_px) {
            return Some(bmp);
        }
    }
    let mut big: HICON = null_mut();
    if ExtractIconExW(w.as_ptr(), index, &mut big, null_mut(), 1) > 0 && !big.is_null() {
        let bmp = extract_hicon_bitmap(big, target_px);
        DestroyIcon(big);
        if bmp.is_some() {
            return bmp;
        }
    }
    None
}

fn read_ascii_nul(buf: &[u8], off: usize) -> Option<String> {
    if off >= buf.len() {
        return None;
    }
    let slice = &buf[off..];
    let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
    Some(String::from_utf8_lossy(&slice[..end]).into_owned())
}

fn read_utf16_nul(buf: &[u8], off: usize) -> Option<String> {
    if off >= buf.len() {
        return None;
    }
    let mut units = Vec::new();
    for c in buf[off..].chunks_exact(2) {
        let u = u16::from_le_bytes([c[0], c[1]]);
        if u == 0 {
            break;
        }
        units.push(u);
    }
    Some(String::from_utf16_lossy(&units))
}

unsafe fn load_path_icon(path: &str, target_px: i32) -> Option<IconBitmap> {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".bmp") {
        return load_image_file_icon(Path::new(path), target_px);
    }
    if lower.ends_with(".ico") {
        return extract_icon_from_binary(path, 0, target_px);
    }
    if lower.ends_with(".lnk") {
        if let Ok(bytes) = fs::read(path)
            && let Some(info) = parse_lnk_info(&bytes)
        {
            if let Some(icon_loc) = &info.icon_location
                && fs::metadata(icon_loc).is_ok()
                && let Some(bmp) = extract_icon_from_binary(icon_loc, info.icon_index, target_px)
            {
                return Some(bmp);
            }
            if let Some(target) = &info.target_exe
                && fs::metadata(target).is_ok()
            {
                if let Some(root) = find_windowsapps_root(target)
                    && let Some(msix) = msix_manifest_info(&root, None)
                    && let Some(logo) = msix.logo_path
                    && let Some(bmp) = load_image_file_icon(&logo, target_px)
                {
                    return Some(bmp);
                }
                if let Some(bmp) = extract_icon_from_binary(target, 0, target_px) {
                    return Some(bmp);
                }
            }
        }
    } else if let Some(root) = find_windowsapps_root(path) {
        // MSIX packaged exe: prefer crisp unplated AppxManifest PNG, fallback to exe icon resource
        if let Some(msix) = msix_manifest_info(&root, None)
            && let Some(logo) = msix.logo_path
            && let Some(bmp) = load_image_file_icon(&logo, target_px)
        {
            return Some(bmp);
        }
        if let Some(bmp) = extract_icon_from_binary(path, 0, target_px) {
            return Some(bmp);
        }
    } else if let Some(bmp) = extract_icon_from_binary(path, 0, target_px) {
        return Some(bmp);
    }

    let mut sfi: SHFILEINFOW = zeroed();
    let wl = wide(path);
    if SHGetFileInfoW(wl.as_ptr(), 0, &mut sfi, size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_LARGEICON) != 0
        && !sfi.hIcon.is_null()
    {
        let bmp = extract_hicon_bitmap(sfi.hIcon, target_px);
        DestroyIcon(sfi.hIcon);
        if bmp.is_some() {
            return bmp;
        }
    }
    None
}

unsafe fn load_hwnd_icon(hwnd: HWND, target_px: i32) -> Option<IconBitmap> {
    if IsWindow(hwnd) == 0 {
        return None;
    }
    for which in [1usize, 2usize, 0usize] {
        let mut res = 0usize;
        if SendMessageTimeoutW(hwnd, WM_GETICON, which, 0, SMTO_ABORTIFHUNG, 15, &mut res) != 0
            && res != 0
            && let Some(bmp) = extract_hicon_bitmap(res as HICON, target_px)
        {
            return Some(bmp);
        }
    }
    let cls = GetClassLongPtrW(hwnd, GCLP_HICON);
    let hicon = (if cls != 0 { cls } else { GetClassLongPtrW(hwnd, GCLP_HICONSM) }) as HICON;
    if !hicon.is_null() {
        return extract_hicon_bitmap(hicon, target_px);
    }
    None
}

/// Converts an `HICON` into a clean premultiplied ARGB bitmap resized to `target_px` × `target_px`.
pub unsafe fn extract_hicon_bitmap(hicon: HICON, target_px: i32) -> Option<IconBitmap> {
    if hicon.is_null() || target_px <= 0 {
        return None;
    }
    let mut ii: ICONINFO = zeroed();
    if GetIconInfo(hicon, &mut ii) == 0 {
        return None;
    }
    let result = read_icon_pixels(&ii, target_px);
    if !ii.hbmMask.is_null() {
        DeleteObject(ii.hbmMask as _);
    }
    if !ii.hbmColor.is_null() {
        DeleteObject(ii.hbmColor as _);
    }
    result
}

unsafe fn read_icon_pixels(ii: &ICONINFO, target_px: i32) -> Option<IconBitmap> {
    let dc = GetDC(null_mut());
    if dc.is_null() {
        return None;
    }
    let mut bm: BITMAP = zeroed();
    let src_bmp = if !ii.hbmColor.is_null() { ii.hbmColor } else { ii.hbmMask };
    if GetObjectW(src_bmp as _, size_of::<BITMAP>() as i32, &mut bm as *mut _ as *mut _) == 0 {
        ReleaseDC(null_mut(), dc);
        return None;
    }
    let w = bm.bmWidth.max(1);
    let h = if ii.hbmColor.is_null() { (bm.bmHeight / 2).max(1) } else { bm.bmHeight.max(1) };

    let mut bmi: BITMAPINFO = zeroed();
    bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w;
    bmi.bmiHeader.biHeight = -h; // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;

    let mut color_px = vec![0u32; (w * h) as usize];
    if !ii.hbmColor.is_null() {
        if GetDIBits(dc, ii.hbmColor, 0, h as u32, color_px.as_mut_ptr() as *mut _, &mut bmi, DIB_RGB_COLORS) == 0 {
            ReleaseDC(null_mut(), dc);
            return None;
        }
    }

    let has_alpha = color_px.iter().any(|&p| (p >> 24) != 0);
    if !has_alpha && !ii.hbmMask.is_null() {
        let mask_h = if ii.hbmColor.is_null() { h * 2 } else { h };
        let mut mask_bmi = bmi;
        mask_bmi.bmiHeader.biHeight = -mask_h;
        let mut mask_px = vec![0u32; (w * mask_h) as usize];
        if GetDIBits(dc, ii.hbmMask, 0, mask_h as u32, mask_px.as_mut_ptr() as *mut _, &mut mask_bmi, DIB_RGB_COLORS) != 0 {
            for i in 0..(w * h) as usize {
                let m = mask_px[i] & 0xFF_FFFF;
                if ii.hbmColor.is_null() {
                    let xor = mask_px[i + (w * h) as usize] & 0xFF_FFFF;
                    color_px[i] = if m == 0 { 0xFF00_0000 | xor } else { 0 };
                } else {
                    color_px[i] = if m == 0 { 0xFF00_0000 | (color_px[i] & 0xFF_FFFF) } else { 0 };
                }
            }
        }
    } else if !has_alpha {
        for p in &mut color_px {
            *p |= 0xFF00_0000;
        }
    }
    ReleaseDC(null_mut(), dc);

    let premul_src: Vec<u32> = color_px.into_iter().map(premultiply).collect();
    Some(downsample_premul(w, h, &premul_src, target_px))
}

#[inline]
pub fn premultiply(argb: u32) -> u32 {
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

pub fn downsample_premul(sw: i32, sh: i32, src: &[u32], dst_sz: i32) -> IconBitmap {
    if sw == dst_sz && sh == dst_sz {
        return IconBitmap {
            w: dst_sz,
            h: dst_sz,
            pixels: src.to_vec(),
        };
    }
    let mut out = vec![0u32; (dst_sz * dst_sz) as usize];
    for dy in 0..dst_sz {
        let sy0 = (dy as f32 * sh as f32) / dst_sz as f32;
        let sy1 = ((dy + 1) as f32 * sh as f32) / dst_sz as f32;
        let iy0 = (sy0.floor() as i32).clamp(0, sh - 1);
        let iy1 = ((sy1.ceil() as i32) - 1).clamp(iy0, sh - 1);
        for dx in 0..dst_sz {
            let sx0 = (dx as f32 * sw as f32) / dst_sz as f32;
            let sx1 = ((dx + 1) as f32 * sw as f32) / dst_sz as f32;
            let ix0 = (sx0.floor() as i32).clamp(0, sw - 1);
            let ix1 = ((sx1.ceil() as i32) - 1).clamp(ix0, sw - 1);
            let (mut sa, mut sr, mut sg, mut sb, mut wsum) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
            for y in iy0..=iy1 {
                let wy = (sy1.min((y + 1) as f32) - sy0.max(y as f32)).max(0.0);
                for x in ix0..=ix1 {
                    let wx = (sx1.min((x + 1) as f32) - sx0.max(x as f32)).max(0.0);
                    let wt = wx * wy;
                    let p = src[(y * sw + x) as usize];
                    sa += ((p >> 24) & 0xFF) as f32 * wt;
                    sr += ((p >> 16) & 0xFF) as f32 * wt;
                    sg += ((p >> 8) & 0xFF) as f32 * wt;
                    sb += (p & 0xFF) as f32 * wt;
                    wsum += wt;
                }
            }
            if wsum > 0.0 {
                let a = (sa / wsum).round().clamp(0.0, 255.0) as u32;
                let r = (sr / wsum).round().clamp(0.0, 255.0) as u32;
                let g = (sg / wsum).round().clamp(0.0, 255.0) as u32;
                let b = (sb / wsum).round().clamp(0.0, 255.0) as u32;
                out[(dy * dst_sz + dx) as usize] = (a << 24) | (r << 16) | (g << 8) | b;
            }
        }
    }
    IconBitmap {
        w: dst_sz,
        h: dst_sz,
        pixels: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiply_and_downsample_preserve_channels() {
        assert_eq!(premultiply(0x80FF_8000), 0x8080_4000);
        let src = vec![0xFF40_80C0u32; 4 * 4];
        let small = downsample_premul(4, 4, &src, 2);
        assert_eq!(small.w, 2);
        assert_eq!(small.h, 2);
        assert_eq!(small.pixels, vec![0xFF40_80C0u32; 4]);
    }

    #[test]
    fn lnk_parser_rejects_non_shelllink_and_reads_local_base_path() {
        assert!(parse_lnk_target(b"not a lnk").is_none());
        let mut blob = vec![0u8; 76];
        blob[0..4].copy_from_slice(&0x4Cu32.to_le_bytes());
        blob[20..24].copy_from_slice(&0x02u32.to_le_bytes()); // HasLinkInfo
        let path_bytes = b"C:\\Windows\\notepad.exe\0";
        let info_hdr = 28u32;
        let info_sz = info_hdr as usize + path_bytes.len();
        let mut info = vec![0u8; info_sz];
        info[0..4].copy_from_slice(&(info_sz as u32).to_le_bytes());
        info[4..8].copy_from_slice(&info_hdr.to_le_bytes());
        info[8..12].copy_from_slice(&1u32.to_le_bytes()); // VolumeIDAndLocalBasePath
        info[16..20].copy_from_slice(&info_hdr.to_le_bytes());
        info[28..].copy_from_slice(path_bytes);
        blob.extend_from_slice(&info);
        assert_eq!(parse_lnk_target(&blob).as_deref(), Some("C:\\Windows\\notepad.exe"));
    }

    #[test]
    fn msix_family_manifest_and_asset_scoring_work() {
        assert_eq!(
            package_family_from_full("Claude_6.28899.6.0_x64__pzs8sxrjxfjjc").as_deref(),
            Some("Claude_pzs8sxrjxfjjc")
        );
        let xml = r#"<Package><Applications><Application Id="Claude"><uap:VisualElements DisplayName="Claude" Square44x44Logo="Assets\Square44x44Logo.png"/></Application></Applications></Package>"#;
        let (name, logo) = extract_manifest_logo_and_name(xml, Some("Claude"));
        assert_eq!(name.as_deref(), Some("Claude"));
        assert_eq!(logo.as_deref(), Some("Assets\\Square44x44Logo.png"));
        let s_unplated = score_msix_asset("square44x44logo.altform-unplated_targetsize-48.png", "square44x44logo").unwrap();
        let s_plain = score_msix_asset("square44x44logo.png", "square44x44logo").unwrap();
        assert!(s_unplated > s_plain);
        assert!(score_msix_asset("square44x44logo.contrast-white.png", "square44x44logo").is_none());
    }
}
