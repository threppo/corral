//! Shell namespace access: enumerate desktop/folder items, fetch icons, invoke the default verb.
//!
//! Items are identified by their Shell parsing name (`SIGDN_DESKTOPABSOLUTEPARSING`), e.g.
//! `C:\Users\X\Desktop\a.txt` or `::{645FF040-5081-101B-9F08-00AA002F954E}` (Recycle Bin).
//!
//! COM must be initialized by the caller on the calling thread (apartment-threaded,
//! `COINIT_APARTMENTTHREADED`); nothing here initializes it. Nothing in this module modifies,
//! moves or deletes files.
#![allow(dead_code)]

use std::ffi::c_void;
use std::mem::size_of;

use windows::Win32::Foundation::{HWND, SIZE};
use windows::Win32::Graphics::Gdi::{
    BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW, HBITMAP,
    ReleaseDC,
};
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx};
use windows::Win32::System::SystemServices::{SFGAO_FLAGS, SFGAO_FOLDER, SFGAO_LINK, SFGAO_STREAM};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_SFObject, IEnumIDList, IShellFolder, IShellItem, IShellItemImageFactory, SHCONTF_FOLDERS, SHCONTF_INCLUDEHIDDEN,
    SHCONTF_NONFOLDERS, SHCreateItemFromParsingName, SHCreateItemWithParent, SHGetDesktopFolder, SHGetSetSettings,
    SHParseDisplayName, SHELLEXECUTEINFOW, SHELLSTATEA, ShellExecuteExW, SIGDN_DESKTOPABSOLUTEPARSING,
    SIGDN_NORMALDISPLAY, SIIGBF_ICONONLY, SSF_SHOWALLOBJECTS,
    SEE_MASK_INVOKEIDLIST,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// One Shell namespace item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellItem {
    /// Absolute parsing name (file path or `::{GUID}` for virtual items).
    pub parsing_name: String,
    /// Display name as Explorer shows it (honours "hide extensions").
    pub display_name: String,
    /// True for folders that open in place (zip-like stream folders count as files).
    pub is_folder: bool,
    /// True for shortcuts (`SFGAO_LINK`).
    pub is_link: bool,
    /// Raw `SFGAO_*` bits queried for the item.
    pub attributes: u32,
    /// Last-write time as Unix seconds, for file-system items.
    pub modified_unix: Option<i64>,
    /// Size in bytes, for file-system files.
    pub size: Option<u64>,
}

/// Premultiplied top-down BGRA pixels.
#[derive(Clone, Debug)]
pub struct IconBitmap {
    pub width: u32,
    pub height: u32,
    pub bgra_premultiplied: Vec<u8>,
    /// Per-item bitmap cache identity; settings refresh compares the full raw policy.
    pub cache_key: Option<crate::icon_overlay::CacheKey>,
}

const QUERY_ATTRS: SFGAO_FLAGS = SFGAO_FLAGS(SFGAO_FOLDER.0 | SFGAO_LINK.0 | SFGAO_STREAM.0);

/// The merged desktop view (user + public desktop + virtual items) as Explorer shows it.
pub fn enumerate_desktop() -> windows::core::Result<Vec<ShellItem>> {
    let folder = unsafe { SHGetDesktopFolder()? };
    enumerate_shell_folder(&folder)
}

/// Items of the folder at `path` (any parsing name works). Errors if the path does not exist.
pub fn enumerate_folder(path: &str) -> windows::core::Result<Vec<ShellItem>> {
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(&HSTRING::from(path), None::<&IBindCtx>)? };
    let folder: IShellFolder = unsafe { item.BindToHandler(None::<&IBindCtx>, &BHID_SFObject)? };
    enumerate_shell_folder(&folder)
}

/// Whether the user's Explorer setting shows hidden files (best effort; false on failure).
fn show_hidden() -> bool {
    let mut state = SHELLSTATEA::default();
    unsafe { SHGetSetSettings(Some(&mut state), SSF_SHOWALLOBJECTS, false) };
    // `fShowAllObjects` is the lowest bit of the SHELLSTATE bitfield.
    state._bitfield1 & 1 != 0
}

fn enumerate_shell_folder(folder: &IShellFolder) -> windows::core::Result<Vec<ShellItem>> {
    let mut flags = (SHCONTF_FOLDERS.0 | SHCONTF_NONFOLDERS.0) as u32;
    if show_hidden() {
        flags |= SHCONTF_INCLUDEHIDDEN.0 as u32;
    }
    let mut enumerator: Option<IEnumIDList> = None;
    unsafe { folder.EnumObjects(HWND::default(), flags, &mut enumerator).ok()? };
    let Some(enumerator) = enumerator else { return Ok(Vec::new()) };

    let mut items = Vec::new();
    loop {
        let mut pidl: [*mut ITEMIDLIST; 1] = [std::ptr::null_mut()];
        let mut fetched = 0u32;
        let hr = unsafe { enumerator.Next(&mut pidl, Some(&mut fetched)) };
        if hr.0 != 0 || fetched == 0 || pidl[0].is_null() {
            break; // S_FALSE (end) or failure
        }
        let built = item_from_pidl(folder, pidl[0]);
        unsafe { CoTaskMemFree(Some(pidl[0] as *const c_void)) };
        if let Ok(item) = built {
            items.push(item);
        }
    }
    Ok(items)
}

fn item_from_pidl(folder: &IShellFolder, pidl: *const ITEMIDLIST) -> windows::core::Result<ShellItem> {
    let item: IShellItem = unsafe { SHCreateItemWithParent(None, folder, pidl)? };
    let parsing_name = display_name(&item, SIGDN_DESKTOPABSOLUTEPARSING)?;
    let display = display_name(&item, SIGDN_NORMALDISPLAY).unwrap_or_else(|_| parsing_name.clone());
    let attributes = unsafe { item.GetAttributes(QUERY_ATTRS) }.map(|a| a.0).unwrap_or(0);
    let is_folder = attributes & SFGAO_FOLDER.0 != 0 && attributes & SFGAO_STREAM.0 == 0;
    let is_link = attributes & SFGAO_LINK.0 != 0;
    let (modified_unix, size) = file_meta(&parsing_name, is_folder);
    Ok(ShellItem { parsing_name, display_name: display, is_folder, is_link, attributes, modified_unix, size })
}

fn display_name(item: &IShellItem, kind: windows::Win32::UI::Shell::SIGDN) -> windows::core::Result<String> {
    let name: PWSTR = unsafe { item.GetDisplayName(kind)? };
    let text = unsafe { name.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(name.0 as *const c_void)) };
    Ok(text)
}

/// Modified time and size from the file system; `(None, None)` for virtual items.
fn file_meta(parsing_name: &str, is_folder: bool) -> (Option<i64>, Option<u64>) {
    if parsing_name.starts_with("::") {
        return (None, None);
    }
    let Ok(meta) = std::fs::metadata(parsing_name) else { return (None, None) };
    let modified = meta.modified().ok().map(|t| match t.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    });
    (modified, if is_folder || meta.is_dir() { None } else { Some(meta.len()) })
}

/// Desktop-style icon (with shortcut/cloud overlays where the Shell provides them) at `size_px`.
/// The result may differ slightly from `size_px` if the Shell returns a different size.
pub fn icon_for(parsing_name: &str, size_px: u32) -> windows::core::Result<IconBitmap> {
    let item: IShellItem =
        unsafe { SHCreateItemFromParsingName(&HSTRING::from(parsing_name), None::<&IBindCtx>)? };
    let factory: IShellItemImageFactory = windows::core::Interface::cast(&item)?;
    let size = SIZE { cx: size_px as i32, cy: size_px as i32 };
    let hbitmap = unsafe { factory.GetImage(size, SIIGBF_ICONONLY)? };
    let result = bitmap_to_bgra(hbitmap);
    unsafe {
        let _ = DeleteObject(hbitmap);
    }
    result
}

/// Read saved shortcut data only. No Resolve, Save or target existence/network probe.
/// Missing/advertised links with no usable stored path remain Unknown and keep their arrow.
fn link_class(item: &ShellItem) -> crate::icon_overlay::LinkClass {
    use crate::icon_overlay::classify;
    use windows::core::Interface;
    use windows::Win32::Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, WIN32_FIND_DATAW};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, STGM_READ};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink, SLGP_RAWPATH};
    if !std::path::Path::new(&item.parsing_name).extension().is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
        return classify(&item.parsing_name, item.is_link, None, false);
    }
    let saved = (|| -> windows::core::Result<(String, bool)> {
        unsafe {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let persist: IPersistFile = link.cast()?;
            persist.Load(&HSTRING::from(item.parsing_name.as_str()), STGM_READ)?;
            let mut path = vec![0u16; 32768];
            let mut data = WIN32_FIND_DATAW::default();
            link.GetPath(&mut path, &mut data, SLGP_RAWPATH.0 as u32)?;
            let len = path.iter().position(|&c| c == 0).unwrap_or(path.len());
            Ok((String::from_utf16_lossy(&path[..len]), data.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0))
        }
    })();
    match saved {
        Ok((target, folder)) => classify(&item.parsing_name, item.is_link, Some(&target), folder),
        Err(_) => classify(&item.parsing_name, item.is_link, None, false),
    }
}

/// Corral-owned extraction: keep the image-factory route for non-links. For links,
/// fetch the Shell base image and its independently identified overlay, even with hiding off
/// (image-factory output alone does not guarantee that toggling can show the arrow again).
/// No bitmap-corner erasure, Explorer messages, global icon replacement or injection.
/// If extraction/arrow identification fails, preserve the plain bitmap rather than erase status.
pub fn icon_for_policy(item: &ShellItem, size_px: u32, policy: crate::icon_overlay::Policy) -> windows::core::Result<IconBitmap> {
    let class = link_class(item);
    let mut bitmap = if class != crate::icon_overlay::LinkClass::NonLink {
        match icon_from_image_list(&item.parsing_name, size_px, policy, class) {
            Ok(bitmap) => bitmap,
            Err(error) => {
                eprintln!("corral: shortcut overlay extraction retained Shell image for {}: {error}", item.parsing_name);
                icon_for(&item.parsing_name, size_px)?
            }
        }
    } else {
        icon_for(&item.parsing_name, size_px)?
    };
    bitmap.cache_key = Some(crate::icon_overlay::CacheKey::new(&item.parsing_name, size_px, policy));
    Ok(bitmap)
}

fn icon_from_image_list(name: &str, size: u32, policy: crate::icon_overlay::Policy, class: crate::icon_overlay::LinkClass) -> windows::core::Result<IconBitmap> {
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::UI::Controls::{IImageList, ILD_TRANSPARENT};
    use windows::Win32::UI::Shell::{SHFILEINFOW, SHGetFileInfoW, SHGetImageList, SHGetIconOverlayIndexW,
        SHGFI_ICON, SHGFI_SYSICONINDEX, SHGFI_OVERLAYINDEX, SHGFI_ADDOVERLAYS,
        SHIL_SMALL, SHIL_LARGE, SHIL_EXTRALARGE, SHIL_JUMBO, IDO_SHGIOI_LINK};
    use windows::Win32::UI::WindowsAndMessaging::DestroyIcon;
    // SHGFI_OVERLAYINDEX requires SHGFI_ICON. The returned composite HICON is ours to destroy;
    // the system image list is read-only and only the low 24 bits are the base image index.
    let mut info = SHFILEINFOW::default();
    let got = unsafe { SHGetFileInfoW(&HSTRING::from(name), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info),
        size_of::<SHFILEINFOW>() as u32, SHGFI_ICON | SHGFI_SYSICONINDEX | SHGFI_OVERLAYINDEX | SHGFI_ADDOVERLAYS) };
    if !info.hIcon.is_invalid() { unsafe { let _ = DestroyIcon(info.hIcon); } }
    if got == 0 { return Err(windows::core::Error::from_win32()); }
    let selected = (info.iIcon as u32) >> 24;
    let shortcut = unsafe { SHGetIconOverlayIndexW(PCWSTR::null(), IDO_SHGIOI_LINK as i32) };
    // An invalid/unknown overlay slot must not cause a silent loss of another status overlay.
    if selected > 15 || !(1..=15).contains(&shortcut) {
        return Err(windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL));
    }
    let overlay = policy.overlay(class, selected, Some(shortcut as u32));
    // LARGE/SMALL/EXTRALARGE can be DPI-scaled. Choose by actual dimensions rather than
    // assuming 16/32/48 device pixels (e.g. a 32-DIP icon is 80 physical pixels at 250%).
    let mut candidates = Vec::new();
    for kind in [SHIL_SMALL, SHIL_LARGE, SHIL_EXTRALARGE, SHIL_JUMBO] {
        if let Ok(list) = unsafe { SHGetImageList::<IImageList>(kind as i32) } {
            let (mut width, mut height) = (0, 0);
            if unsafe { list.GetIconSize(&mut width, &mut height) }.is_ok() && width > 0 && height > 0 {
                let width = width as u32;
                candidates.push(((width < size, width.abs_diff(size)), width, list));
            }
        }
    }
    let (native, list) = candidates.into_iter().min_by_key(|(score, _, _)| *score).map(|(_, native, list)| (native, list))
        .ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
    // INDEXTOOVERLAYMASK: the selected overlay occupies bits 8..11 of the draw flags.
    let icon = unsafe { list.GetIcon(info.iIcon & 0x00ff_ffff, ILD_TRANSPARENT.0 | (overlay << 8))? };
    // Render at the list's own size and resample here: DrawIconEx scales a 256 px jumbo icon down to an
    // arbitrary size by point sampling, which leaves single aliased rim pixels that the global Tint
    // (mid-grey boost) turns into bright specks (observed 2026-10-15, AIOStreams.lnk at icon_dip 53).
    let result = icon_to_bgra(icon, native).map(|bitmap| resize_icon(bitmap, size));
    unsafe { let _ = DestroyIcon(icon); }
    result
}

/// Resamples a premultiplied BGRA icon to `size` x `size`: an area (box) filter when shrinking, so
/// every source pixel contributes in proportion to its coverage, and bilinear when enlarging.
/// Averaging premultiplied channels keeps `colour <= alpha`.
fn resize_icon(bitmap: IconBitmap, size: u32) -> IconBitmap {
    let size = size.clamp(1, 1024);
    if bitmap.width == size && bitmap.height == size {
        return bitmap;
    }
    let pixels = resample_premultiplied(&bitmap.bgra_premultiplied, bitmap.width as usize, bitmap.height as usize, size as usize, size as usize);
    IconBitmap { width: size, height: size, bgra_premultiplied: pixels, cache_key: bitmap.cache_key }
}

/// Weights of the source samples that make up each destination sample along one axis:
/// `(first source index, weights)`; the weights sum to 1.
fn axis_weights(src: usize, dst: usize) -> Vec<(usize, Vec<f32>)> {
    let ratio = src as f32 / dst as f32;
    (0..dst).map(|d| {
        if ratio > 1.0 {
            // box: the interval [d*ratio, (d+1)*ratio) of the source, weighted by overlap
            let (lo, hi) = (d as f32 * ratio, (d as f32 + 1.0) * ratio);
            let first = lo.floor() as usize;
            let last = ((hi.ceil() as usize).max(first + 1)).min(src);
            let weights: Vec<f32> = (first..last).map(|i| ((i as f32 + 1.0).min(hi) - (i as f32).max(lo)).max(0.0)).collect();
            let sum: f32 = weights.iter().sum();
            (first, weights.into_iter().map(|w| w / sum).collect())
        } else {
            // bilinear on pixel centres
            let centre = ((d as f32 + 0.5) * ratio - 0.5).clamp(0.0, (src - 1) as f32);
            let first = (centre.floor() as usize).min(src - 1);
            let t = centre - first as f32;
            if first + 1 < src && t > 0.0 { (first, vec![1.0 - t, t]) } else { (first, vec![1.0]) }
        }
    }).collect()
}

fn resample_premultiplied(src: &[u8], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let (xs, ys) = (axis_weights(sw, dw), axis_weights(sh, dh));
    // horizontal pass into f32, then vertical
    let mut mid = vec![0f32; dw * sh * 4];
    for y in 0..sh {
        for (dx, (first, weights)) in xs.iter().enumerate() {
            for (k, w) in weights.iter().enumerate() {
                let p = &src[(y * sw + first + k) * 4..][..4];
                for c in 0..4 {
                    mid[(y * dw + dx) * 4 + c] += p[c] as f32 * w;
                }
            }
        }
    }
    let mut out = vec![0u8; dw * dh * 4];
    for (dy, (first, weights)) in ys.iter().enumerate() {
        for dx in 0..dw {
            let mut acc = [0f32; 4];
            for (k, w) in weights.iter().enumerate() {
                for c in 0..4 {
                    acc[c] += mid[((first + k) * dw + dx) * 4 + c] * w;
                }
            }
            let alpha = (acc[3] + 0.5).floor().clamp(0.0, 255.0) as u8;
            let o = &mut out[(dy * dw + dx) * 4..][..4];
            for c in 0..3 {
                // premultiplied invariant: a rounded colour never exceeds the rounded alpha
                o[c] = ((acc[c] + 0.5).floor().clamp(0.0, 255.0) as u8).min(alpha);
            }
            o[3] = alpha;
        }
    }
    out
}

/// Render the owned HICON over black and white to recover alpha, including legacy mask icons.
/// DrawIconEx handles both the image-list overlay and icon mask; no layer is painted heuristically.
pub(crate) fn icon_to_bgra(icon: windows::Win32::UI::WindowsAndMessaging::HICON, size: u32) -> windows::core::Result<IconBitmap> {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Graphics::Gdi::{CreateCompatibleDC, CreateDIBSection, DeleteDC, GdiFlush, HBRUSH, HGDIOBJ, SelectObject};
    use windows::Win32::UI::WindowsAndMessaging::{DrawIconEx, DI_NORMAL};
    let size = size.clamp(1, 1024);
    let mut info = BITMAPINFO::default();
    info.bmiHeader = BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: size as i32, biHeight: -(size as i32), biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() };
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() { return Err(windows::core::Error::from_win32()); }
        let mut bits = std::ptr::null_mut();
        let bitmap = match CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, HANDLE::default(), 0) {
            Ok(bitmap) => bitmap,
            Err(e) => { let _ = DeleteDC(dc); return Err(e); }
        };
        let previous = SelectObject(dc, HGDIOBJ(bitmap.0));
        if bits.is_null() || previous.is_invalid() || previous.0 as isize == -1 {
            let _ = DeleteDC(dc);
            let _ = DeleteObject(bitmap);
            return Err(windows::core::Error::from_win32());
        }
        let result = (|| {
            let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, (size * size * 4) as usize);
            pixels.fill(0);
            DrawIconEx(dc, 0, 0, icon, size as i32, size as i32, 0, HBRUSH::default(), DI_NORMAL)?;
            let _ = GdiFlush();
            let black = pixels.to_vec();
            pixels.fill(255);
            DrawIconEx(dc, 0, 0, icon, size as i32, size as i32, 0, HBRUSH::default(), DI_NORMAL)?;
            let _ = GdiFlush();
            let bgra_premultiplied = alpha_from_backgrounds(&black, pixels);
            Ok(IconBitmap { width: size, height: size, bgra_premultiplied, cache_key: None })
        })();
        let _ = SelectObject(dc, previous);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(dc);
        result
    }
}

fn alpha_from_backgrounds(black: &[u8], white: &[u8]) -> Vec<u8> {
    let mut result = black.to_vec();
    for (b, w) in result.chunks_exact_mut(4).zip(white.chunks_exact(4)) {
        let alpha = 255 - (0..3).map(|c| w[c].saturating_sub(b[c])).max().unwrap_or(255);
        b[3] = alpha;
        for channel in &mut b[..3] { *channel = (*channel).min(alpha); }
    }
    result
}

/// Converts an HBITMAP to top-down premultiplied BGRA. Bitmaps without any alpha are treated as
/// opaque; bitmaps whose colour exceeds alpha are premultiplied here.
fn bitmap_to_bgra(hbitmap: HBITMAP) -> windows::core::Result<IconBitmap> {
    let mut bm = BITMAP::default();
    let got = unsafe { GetObjectW(hbitmap, size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void)) };
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
        return Err(windows::core::Error::from_win32());
    }
    let (width, height) = (bm.bmWidth as u32, bm.bmHeight.unsigned_abs());
    let mut info = BITMAPINFO::default();
    info.bmiHeader = BITMAPINFOHEADER {
        biSize: size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width as i32,
        biHeight: -(height as i32), // negative: top-down
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let lines = unsafe {
        let dc = GetDC(None);
        let n = GetDIBits(dc, hbitmap, 0, height, Some(pixels.as_mut_ptr() as *mut c_void), &mut info, DIB_RGB_COLORS);
        ReleaseDC(None, dc);
        n
    };
    if lines == 0 {
        return Err(windows::core::Error::from_win32());
    }
    normalize_alpha(&mut pixels);
    Ok(IconBitmap { width, height, bgra_premultiplied: pixels, cache_key: None })
}

fn normalize_alpha(pixels: &mut [u8]) {
    if pixels.chunks_exact(4).all(|p| p[3] == 0) {
        for p in pixels.chunks_exact_mut(4) {
            p[3] = 255;
        }
        return;
    }
    let needs_premultiply = pixels.chunks_exact(4).any(|p| p[0] > p[3] || p[1] > p[3] || p[2] > p[3]);
    if needs_premultiply {
        for p in pixels.chunks_exact_mut(4) {
            let a = p[3] as u32;
            for c in &mut p[..3] {
                *c = ((*c as u32 * a + 127) / 255) as u8;
            }
        }
    }
}

/// Invokes the item's default verb (as a double-click would) via `ShellExecuteExW`.
/// Returns once the launch has been handed off; it does not wait for the launched program.
pub fn open_item(parsing_name: &str) -> windows::core::Result<()> {
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    unsafe { SHParseDisplayName(&HSTRING::from(parsing_name), None::<&IBindCtx>, &mut pidl, 0, None)? };
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_INVOKEIDLIST,
        lpIDList: pidl as *mut c_void,
        lpVerb: PCWSTR::null(),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    let result = unsafe { ShellExecuteExW(&mut info) };
    unsafe { CoTaskMemFree(Some(pidl as *const c_void)) };
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

    fn init_com() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
    }

    #[test]
    fn q20_alpha_retains_transparent_mask_and_premultiplied_pixels() {
        let black = [0, 0, 0, 0, 20, 30, 40, 0, 30, 60, 100, 128];
        let white = [255, 255, 255, 0, 20, 30, 40, 0, 157, 187, 227, 0];
        assert_eq!(alpha_from_backgrounds(&black, &white),
            [0, 0, 0, 0, 20, 30, 40, 255, 30, 60, 100, 128]);
    }

    /// Synthetic icon at `n` px: black rounded square (premultiplied 7), a 1 px mid-grey rim (61), transparent outside.
    fn rimmed_square(n: usize) -> Vec<u8> {
        let mut px = vec![0u8; n * n * 4];
        let r = n as f32 * 0.16;
        for y in 0..n {
            for x in 0..n {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let (cx, cy) = (fx.min(n as f32 - fx), fy.min(n as f32 - fy));
                let d = if cx < r && cy < r { ((r - cx).powi(2) + (r - cy).powi(2)).sqrt() - r } else { -cx.min(cy).min(r) };
                let p = if d > 0.0 { [0, 0, 0, 0] } else if d > -1.0 { [61, 61, 61, 255] } else { [7, 7, 7, 255] };
                px[(y * n + x) * 4..][..4].copy_from_slice(&p);
            }
        }
        px
    }

    /// What `DrawIconEx` effectively did before: point sampling.
    fn nearest(src: &[u8], sw: usize, dw: usize) -> Vec<u8> {
        let mut out = vec![0u8; dw * dw * 4];
        for y in 0..dw {
            for x in 0..dw {
                let (sx, sy) = (x * sw / dw, y * sw / dw);
                out[(y * dw + x) * 4..][..4].copy_from_slice(&src[(sy * sw + sx) * 4..][..4]);
            }
        }
        out
    }

    #[test]
    fn box_resample_keeps_flat_areas_and_the_premultiplied_invariant() {
        let big = rimmed_square(256);
        for dst in [32usize, 53, 80, 132, 255] {
            let small = resample_premultiplied(&big, 256, 256, dst, dst);
            assert_eq!(small.len(), dst * dst * 4);
            let mid = &small[((dst / 2) * dst + dst / 2) * 4..][..4];
            assert_eq!(mid, [7, 7, 7, 255], "flat interior unchanged at {dst}");
            assert_eq!(&small[..4], [0, 0, 0, 0], "transparent corner unchanged at {dst}");
            assert!(small.chunks_exact(4).all(|p| p[..3].iter().all(|&c| c <= p[3])), "colour above alpha at {dst}");
        }
        // an exact same-size resample is the identity
        assert_eq!(resample_premultiplied(&big, 256, 256, 256, 256), big);
    }

    #[test]
    fn upscale_is_bilinear_and_stays_valid() {
        let small = rimmed_square(24);
        let big = resample_premultiplied(&small, 24, 24, 53, 53);
        assert!(big.chunks_exact(4).all(|p| p[..3].iter().all(|&c| c <= p[3])));
        let mid = &big[((53 / 2) * 53 + 53 / 2) * 4..][..4];
        assert_eq!(mid, [7, 7, 7, 255]);
    }

    /// The AIOStreams case: a black rounded icon with a thin grey rim, tinted, shown at an icon size
    /// the system image lists do not offer (icon_dip 53). Point sampling leaves isolated opaque rim
    /// pixels that the tint boosts; the area filter spreads the rim over its neighbours.

    #[test]
    fn desktop_items_round_trip() {
        init_com();
        let items = enumerate_desktop().expect("enumerate desktop");
        assert!(!items.is_empty(), "desktop should have at least one item");
        for item in &items {
            let parsed: windows::core::Result<IShellItem> =
                unsafe { SHCreateItemFromParsingName(&HSTRING::from(item.parsing_name.as_str()), None::<&IBindCtx>) };
            assert!(parsed.is_ok(), "does not round-trip: {}", item.parsing_name);
        }
    }

    #[test]
    fn icon_has_alpha() {
        init_com();
        let items = enumerate_desktop().unwrap();
        let icon = icon_for(&items[0].parsing_name, 48).expect("icon");
        assert_eq!((icon.width, icon.height), (48, 48));
        assert_eq!(icon.bgra_premultiplied.len(), 48 * 48 * 4);
        assert!(icon.bgra_premultiplied.chunks_exact(4).any(|p| p[3] != 0));
    }

    #[test]
    fn folder_enumeration_lists_exact_files() {
        init_com();
        let dir = std::env::temp_dir().join(format!("corral-shell-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("one.txt"), b"1").unwrap();
        std::fs::write(dir.join("two.txt"), b"22").unwrap();
        let result = enumerate_folder(dir.to_str().unwrap());
        let _ = std::fs::remove_dir_all(&dir);
        let mut items = result.expect("enumerate folder");
        items.sort_by(|a, b| a.parsing_name.cmp(&b.parsing_name));
        assert_eq!(items.len(), 2);
        assert!(items[0].parsing_name.ends_with("one.txt"));
        assert!(items[1].parsing_name.ends_with("two.txt"));
        assert_eq!(items[1].size, Some(2));
        assert!(!items[0].is_folder && items[0].modified_unix.is_some());
    }

    #[test]
    fn missing_folder_is_error() {
        init_com();
        assert!(enumerate_folder(r"C:\definitely\not\here\corral-xyz").is_err());
    }
}

/// Shell type name of an item ("Text Document", "File folder"; `SHGetFileInfoW(SHGFI_TYPENAME)`),
/// the "Item type" sort key. Virtual items (`::{GUID}`) go through their pidl.
pub fn type_name_of(parsing_name: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_PIDL, SHGFI_TYPENAME, SHGetFileInfoW};
    let mut info = SHFILEINFOW::default();
    let ok = if parsing_name.starts_with("::") {
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        if unsafe { SHParseDisplayName(&HSTRING::from(parsing_name), None::<&IBindCtx>, &mut pidl, 0, None) }.is_err() || pidl.is_null() {
            return None;
        }
        let ok = unsafe {
            SHGetFileInfoW(windows::core::PCWSTR(pidl as *const u16), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info), size_of::<SHFILEINFOW>() as u32, SHGFI_TYPENAME | SHGFI_PIDL)
        };
        unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(pidl as *const _)) };
        ok
    } else {
        unsafe { SHGetFileInfoW(&HSTRING::from(parsing_name), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info), size_of::<SHFILEINFOW>() as u32, SHGFI_TYPENAME) }
    };
    if ok == 0 {
        return None;
    }
    let len = info.szTypeName.iter().position(|&c| c == 0).unwrap_or(info.szTypeName.len());
    Some(String::from_utf16_lossy(&info.szTypeName[..len]))
}

/// Shell display name of a path (`SHGetFileInfoW(SHGFI_DISPLAYNAME)`), used for
/// portal titles.
pub fn display_name_of(path: &str) -> Option<String> {
    use windows::Win32::Storage::FileSystem::FILE_FLAGS_AND_ATTRIBUTES;
    use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_DISPLAYNAME, SHGetFileInfoW};
    let mut info = SHFILEINFOW::default();
    let ok = unsafe { SHGetFileInfoW(&HSTRING::from(path), FILE_FLAGS_AND_ATTRIBUTES(0), Some(&mut info), size_of::<SHFILEINFOW>() as u32, SHGFI_DISPLAYNAME) };
    if ok == 0 {
        return None;
    }
    let len = info.szDisplayName.iter().position(|&c| c == 0).unwrap_or(info.szDisplayName.len());
    let name = String::from_utf16_lossy(&info.szDisplayName[..len]);
    (!name.is_empty()).then_some(name)
}
