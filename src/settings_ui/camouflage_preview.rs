//! Corral extra: primary-monitor desktop-layer settings preview. No screen DC capture.
//! The background and foreground are recoloured separately, like app/paint.rs.
use super::{PageLay, Theme, scf};
use crate::render::{Align, RectF, Rgba, Surface, TextStyle};
use crate::settings::Settings;
use crate::{camouflage, group_paint, tint};
use super::sample_corral::{color_argb, color_rgb, wallpaper_palette, apply_pixels, auto_text_color, mapped_color};
use std::cell::RefCell;
use std::rc::Rc;
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::Result;

/// Each registered thumbnail belongs to the settings window and is released on destroy.
struct Thumbnail {
    source: isize,
    handle: isize,
}
impl Drop for Thumbnail {
    fn drop(&mut self) {
        unsafe {
            let _ = DwmUnregisterThumbnail(self.handle);
        }
    }
}
#[derive(Default)]
struct Live {
    destination: isize,
    thumbnails: Vec<Thumbnail>,
    source_order: Vec<isize>,
    visible: bool,
    desktop: bool,
    real: bool,
}
thread_local! { static LIVE: RefCell<Live> = RefCell::new(Live::default()); }

thread_local! {
    /// Paints so far that found no wallpaper picture ready (0 = the picture is cached or given up on).
    static MISSING: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}
/// Paints that may retry a missing picture before the desktop colour is accepted.
const PICTURE_RETRIES: u8 = 30;

/// Page changes hide composition without discarding the expensive capture or registrations.
pub(super) fn leave() {
    LIVE.with(|live| {
        let mut live = live.borrow_mut();
        if !live.visible {
            return;
        }
        for thumbnail in &live.thumbnails {
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_VISIBLE,
                fVisible: BOOL(0),
                ..Default::default()
            };
            unsafe {
                let _ = DwmUpdateThumbnailProperties(thumbnail.handle, &props);
            }
        }
        live.visible = false;
    });
}

/// Also used for display/DPI changes: the next selected-page paint rebuilds once.
pub(super) fn reset() {
    tint::set_preview_visible(false);
    MISSING.set(0);
    LIVE.with(|live| *live.borrow_mut() = Live::default());
    CACHE.with(|cache| *cache.borrow_mut() = None);
    ASPECT.with(|aspect| aspect.set(None));
}

fn class(hwnd: HWND) -> String {
    let mut text = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut text) }.max(0) as usize;
    String::from_utf16_lossy(&text[..n])
}
unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
    unsafe {
        (&mut *(data.0 as *mut Vec<HWND>)).push(hwnd);
    }
    BOOL(1)
}
fn pid(hwnd: HWND) -> u32 {
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    pid
}
fn engine_class(name: &str) -> bool {
    matches!(name, "CorralGroup" | "CorralSurface")
}
fn wallpaper_process(hwnd: HWND) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::*;
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid(hwnd)) else {
            return false;
        };
        let mut path = [0u16; 1024];
        let mut len = path.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            windows::core::PWSTR(path.as_mut_ptr()),
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(handle);
        ok && matches!(
            String::from_utf16_lossy(&path[..len as usize])
                .rsplit(['\\', '/'])
                .next()
                .unwrap_or("")
                .to_ascii_lowercase()
                .as_str(),
            "wallpaper32.exe"
                | "wallpaper64.exe"
                | "lively.wallpaper.wpe.exe"
                | "lively.player.mpv.exe"
        )
    }
}
fn desktop_safe(hwnd: HWND, shell_pid: u32, engines: &[u32]) -> bool {
    let mut children = Vec::new();
    unsafe {
        let _ = EnumChildWindows(
            hwnd,
            Some(collect),
            LPARAM(&mut children as *mut _ as isize),
        );
    }
    children
        .iter()
        .any(|&h| class(h) == "SHELLDLL_DefView" || wallpaper_process(h))
        && children.into_iter().all(|h| {
            let process = pid(h);
            engines.contains(&process)
                || wallpaper_process(h)
                || (process == shell_pid
                    && matches!(
                        class(h).as_str(),
                        "WorkerW"
                            | "Progman"
                            | "SHELLDLL_DefView"
                            | "SysListView32"
                            | "SysHeader32"
                            | "tooltips_class32"
                            | "DirectUIHWND"
                    ))
        })
}
/// Only shell-owned desktop roots and windows from a verified Corral engine process.
/// The settings process itself is never eligible. EnumWindows returns front-to-back.
fn sources() -> Vec<(HWND, bool)> {
    let mut roots = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut roots as *mut _ as isize));
    }
    let shell = unsafe { GetShellWindow() };
    let shell_pid = pid(shell);
    let engines: Vec<u32> = roots
        .iter()
        .filter(|&&h| class(h) == "CorralMessage")
        .map(|&h| pid(h))
        .filter(|&p| p != 0)
        .collect();
    let mut result: Vec<_> = roots
        .into_iter()
        .rev()
        .filter_map(|h| {
            if !unsafe { IsWindowVisible(h) }.as_bool() {
                return None;
            }
            let name = class(h);
            let process = pid(h);
            let desktop = shell_pid != 0
                && process == shell_pid
                && matches!(name.as_str(), "Progman" | "WorkerW");
            if desktop && desktop_safe(h, shell_pid, &engines) {
                Some((h, true))
            } else if engine_class(&name) && engines.contains(&process) {
                Some((h, false))
            } else {
                None
            }
        })
        .collect();
    result.sort_by_key(|(_, desktop)| !*desktop);
    result
}

/// Snapshot fallback uses only the engine's own allowlisted window surfaces, never screen pixels.
/// A single bounded worker prevents foreign PrintWindow from stalling the settings thread forever.
fn surface_snapshots() -> Vec<tint::Wallpaper> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
    if IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return Vec::new();
    }
    let windows: Vec<_> = sources()
        .into_iter()
        .filter(|(_, desktop)| !desktop)
        .map(|(h, _)| (h.0 as isize, pid(h), class(h)))
        .collect();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::Builder::new()
        .name("settings-corral-snapshot".into())
        .spawn(move || {
            struct Flight;
            impl Drop for Flight {
                fn drop(&mut self) {
                    IN_FLIGHT.store(false, Ordering::Release);
                }
            }
            let _flight = Flight;
            use windows::Win32::Graphics::Gdi::*;
            use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
            use windows::Win32::UI::HiDpi::{
                DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
            };
            let old_dpi =
                unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
            let mut images = Vec::new();
            for (raw, process, name) in windows {
                let hwnd = HWND(raw as *mut _);
                if pid(hwnd) != process || class(hwnd) != name {
                    continue;
                }
                unsafe {
                    let mut r = RECT::default();
                    if GetWindowRect(hwnd, &mut r).is_err() {
                        continue;
                    }
                    let (w, h) = (r.right - r.left, r.bottom - r.top);
                    if w <= 0 || h <= 0 || w as i64 * h as i64 > 16_777_216 {
                        continue;
                    }
                    let dc = CreateCompatibleDC(None);
                    if dc.0.is_null() {
                        continue;
                    }
                    let info = BITMAPINFO {
                        bmiHeader: BITMAPINFOHEADER {
                            biSize: size_of::<BITMAPINFOHEADER>() as u32,
                            biWidth: w,
                            biHeight: -h,
                            biPlanes: 1,
                            biBitCount: 32,
                            biCompression: BI_RGB.0,
                            ..Default::default()
                        },
                        ..Default::default()
                    };
                    let mut bits = std::ptr::null_mut();
                    let bitmap = CreateDIBSection(dc, &info, DIB_RGB_COLORS, &mut bits, None, 0);
                    if let Ok(bitmap) = bitmap {
                        if !bits.is_null() {
                            let old = SelectObject(dc, bitmap);
                            let len = w as usize * h as usize * 4;
                            std::ptr::write_bytes(bits, 0, len);
                            let ok = PrintWindow(hwnd, dc, PRINT_WINDOW_FLAGS(2)).as_bool();
                            let _ = GdiFlush();
                            let mut bgra =
                                std::slice::from_raw_parts(bits as *const u8, len).to_vec();
                            SelectObject(dc, old);
                            if ok
                                && pid(hwnd) == process
                                && class(hwnd) == name
                                && bgra.chunks_exact(4).any(|p| p[..3] != [0, 0, 0])
                            {
                                for pixel in bgra.chunks_exact_mut(4) {
                                    // PrintWindow commonly discards alpha; captured content remains visible.
                                    if pixel[3] == 0 && pixel[..3] != [0, 0, 0] {
                                        pixel[3] = 255;
                                    }
                                }
                                images.push(tint::Wallpaper {
                                    origin: (r.left, r.top),
                                    width: w,
                                    height: h,
                                    bgra,
                                    solid: None,
                                });
                            }
                        }
                        let _ = DeleteObject(bitmap);
                    }
                    let _ = DeleteDC(dc);
                }
            }
            unsafe {
                SetThreadDpiAwarenessContext(old_dpi);
            }
            let _ = tx.send(images);
        });
    if worker.is_err() {
        IN_FLIGHT.store(false, Ordering::Release);
        return Vec::new();
    }
    rx.recv_timeout(std::time::Duration::from_millis(300))
        .unwrap_or_default()
}
fn composite_surface(dst: &mut tint::Wallpaper, src: &tint::Wallpaper) {
    composite_surface_scaled(dst, src, 1);
}
/// Blend a window snapshot (physical pixels) onto a preview image that holds one pixel per `k x k`
/// screen pixels (wallpaper-blur-spec 8: the preview works at a reduced scale).
fn composite_surface_scaled(dst: &mut tint::Wallpaper, src: &tint::Wallpaper, k: i32) {
    let k = k.max(1);
    for dy in 0..dst.height {
        let y = dst.origin.1 + dy * k + k / 2 - src.origin.1;
        if y < 0 || y >= src.height {
            continue;
        }
        for dx in 0..dst.width {
            let x = dst.origin.0 + dx * k + k / 2 - src.origin.0;
            if x < 0 || x >= src.width {
                continue;
            }
            let from = (y as usize * src.width as usize + x as usize) * 4;
            let to = (dy as usize * dst.width as usize + dx as usize) * 4;
            let a = src.bgra[from + 3] as u32;
            for c in 0..3 {
                dst.bgra[to + c] = (src.bgra[from + c] as u32
                    + (dst.bgra[to + c] as u32 * (255 - a) + 127) / 255)
                    .min(255) as u8;
            }
            dst.bgra[to + 3] = 255;
        }
    }
}

/// Screen/source rectangles and destination are all physical pixels. Source coordinates are
/// relative to the HWND, never relative to the primary or virtual monitor origin.
fn mapping(primary: RECT, window: RECT, preview: RECT) -> Option<(RECT, RECT)> {
    let clipped = RECT {
        left: primary.left.max(window.left),
        top: primary.top.max(window.top),
        right: primary.right.min(window.right),
        bottom: primary.bottom.min(window.bottom),
    };
    if clipped.right <= clipped.left || clipped.bottom <= clipped.top {
        return None;
    }
    let area = fit(
        primary.right - primary.left,
        primary.bottom - primary.top,
        (preview.right - preview.left).max(1) as u32,
        (preview.bottom - preview.top).max(1) as u32,
    );
    let scale = (area.right - area.left) / (primary.right - primary.left).max(1) as f32;
    let x = |v: i32| preview.left + (area.left + (v - primary.left) as f32 * scale).round() as i32;
    let y = |v: i32| preview.top + (area.top + (v - primary.top) as f32 * scale).round() as i32;
    Some((
        RECT {
            left: clipped.left - window.left,
            top: clipped.top - window.top,
            right: clipped.right - window.left,
            bottom: clipped.bottom - window.top,
        },
        RECT {
            left: x(clipped.left),
            top: y(clipped.top),
            right: x(clipped.right),
            bottom: y(clipped.bottom),
        },
    ))
}

/// Crop the thumbnail source by the same amount as its offscreen destination.
/// DWM does not inherit the parent's Direct2D clip when a tall page scrolls.
fn clip_mapping(source: RECT, dest: RECT, viewport: RECT) -> Option<(RECT, RECT)> {
    let clipped = RECT {
        left: dest.left.max(viewport.left),
        top: dest.top.max(viewport.top),
        right: dest.right.min(viewport.right),
        bottom: dest.bottom.min(viewport.bottom),
    };
    if clipped.right <= clipped.left
        || clipped.bottom <= clipped.top
        || dest.right <= dest.left
        || dest.bottom <= dest.top
    {
        return None;
    }
    let x = |v: i32| {
        source.left
            + ((v - dest.left) as f64 * (source.right - source.left) as f64
                / (dest.right - dest.left) as f64)
                .round() as i32
    };
    let y = |v: i32| {
        source.top
            + ((v - dest.top) as f64 * (source.bottom - source.top) as f64
                / (dest.bottom - dest.top) as f64)
                .round() as i32
    };
    let source = RECT {
        left: x(clipped.left),
        top: y(clipped.top),
        right: x(clipped.right),
        bottom: y(clipped.bottom),
    };
    (source.right > source.left && source.bottom > source.top).then_some((source, clipped))
}

/// Update only on normal settings paints/resize/page changes. DWM then updates content itself;
/// no capture timer and no screen DC (so covering applications never enter the preview).
pub(super) fn sync(hwnd: HWND, layout: Option<&PageLay>, _dpi: u32) {
    let Some(layout) = layout else {
        leave();
        return;
    };
    let Some(section) = layout.sections.first() else {
        leave();
        return;
    };
    let Some((left, top, right, bottom)) = tint::primary_rect() else {
        leave();
        return;
    };
    let primary = RECT {
        left,
        top,
        right,
        bottom,
    };
    let Some(box_rect) = layout.preview else {
        leave();
        return;
    };
    let _ = section;
    let preview = RECT {
        left: box_rect.left.round() as i32,
        top: box_rect.top.round() as i32,
        right: box_rect.right.round() as i32,
        bottom: box_rect.bottom.round() as i32,
    };
    let mut viewport = RECT::default();
    if unsafe { GetClientRect(hwnd, &mut viewport) }.is_err() {
        leave();
        return;
    }
    LIVE.with(|cell| {
        let mut live = cell.borrow_mut();
        if live.destination != hwnd.0 as isize {
            *live = Live {
                destination: hwnd.0 as isize,
                ..Default::default()
            };
        }
        let previous = (live.desktop, live.real);
        let sources = sources();
        let order: Vec<_> = sources
            .iter()
            .map(|(source, _)| source.0 as isize)
            .collect();
        if live.source_order != order {
            // Register back-to-front again if a desktop host is recreated; a newly registered
            // background must never cover an older Corral thumbnail.
            live.thumbnails.clear();
            live.source_order = order;
        }
        live.thumbnails
            .retain(|t| sources.iter().any(|(s, _)| s.0 as isize == t.source));
        live.visible = true;
        live.desktop = false;
        live.real = false;
        for (source, desktop) in sources {
            let mut rect = RECT::default();
            if unsafe { GetWindowRect(source, &mut rect) }.is_err() {
                continue;
            }
            let Some((src, dst)) =
                mapping(primary, rect, preview).and_then(|(s, d)| clip_mapping(s, d, viewport))
            else {
                if let Some(t) = live
                    .thumbnails
                    .iter()
                    .find(|t| t.source == source.0 as isize)
                {
                    let props = DWM_THUMBNAIL_PROPERTIES {
                        dwFlags: DWM_TNP_VISIBLE,
                        fVisible: BOOL(0),
                        ..Default::default()
                    };
                    unsafe {
                        let _ = DwmUpdateThumbnailProperties(t.handle, &props);
                    }
                }
                continue;
            };
            let at = if let Some(at) = live
                .thumbnails
                .iter()
                .position(|t| t.source == source.0 as isize)
            {
                at
            } else {
                let Ok(handle) = (unsafe { DwmRegisterThumbnail(hwnd, source) }) else {
                    continue;
                };
                live.thumbnails.push(Thumbnail {
                    source: source.0 as isize,
                    handle,
                });
                live.thumbnails.len() - 1
            };
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION
                    | DWM_TNP_RECTSOURCE
                    | DWM_TNP_OPACITY
                    | DWM_TNP_VISIBLE
                    | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: dst,
                rcSource: src,
                opacity: 255,
                fVisible: BOOL(1),
                fSourceClientAreaOnly: BOOL(0),
            };
            if unsafe { DwmUpdateThumbnailProperties(live.thumbnails[at].handle, &props) }.is_ok() {
                if desktop {
                    live.desktop = true;
                } else {
                    live.real = true;
                }
            }
        }
        if previous != (live.desktop, live.real) {
            // Registration happens after BitBlt. Repaint once to remove samples from letterbox
            // margins and refresh the notice when a real surface first becomes available.
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
            }
        }
    });
}

thread_local! { static ASPECT: std::cell::Cell<Option<(i32,i32)>> = const { std::cell::Cell::new(None) }; }
/// Primary monitor size (physical pixels); 16:9 when unavailable.
pub(super) fn monitor_aspect() -> (i32, i32) {
    ASPECT.with(|aspect| {
        if let Some(value) = aspect.get() {
            return value;
        }
        let value = match tint::primary_rect() {
            Some((l, t, r, b)) if r > l && b > t => (r - l, b - t),
            _ => (16, 9),
        };
        aspect.set(Some(value));
        value
    })
}
/// Preview box (px) for `avail_w` content width in a `client_h` tall client: monitor aspect, at most
/// ~45 % of the client height (then the width shrinks to keep the aspect; the caller centres it).
pub(super) fn box_size(avail_w: f32, client_h: f32, monitor: (i32, i32), dpi: u32) -> (f32, f32) {
    let ratio = monitor.1.max(1) as f32 / monitor.0.max(1) as f32;
    let min_h = scf(60.0, dpi);
    let cap = (client_h * 0.45).max(min_h);
    let avail_w = avail_w.max(1.0);
    let mut w = avail_w;
    let mut h = w * ratio;
    if h > cap {
        h = cap;
        w = (h / ratio).min(avail_w);
    }
    (w.floor().max(1.0), h.floor().max(1.0))
}
pub(super) const NOTICE_H: f32 = 34.0;

struct Preview {
    source: Rc<tint::Wallpaper>,
    sample: Rc<tint::Wallpaper>,
    matte: u32,
    fallback: bool,
    desktop_layer: bool,
    raster: Option<(String, u32, u32, Vec<u8>)>,
}
thread_local! { static CACHE: RefCell<Option<Preview>> = const { RefCell::new(None) }; }

/// Acquire once per settings-window lifetime, then reuse on subsequent page entries.
/// Tests inject synthetic wallpaper and never need to capture desktop content.
pub(super) fn enter() {
    if CACHE.with(|cache| cache.borrow().is_some()) && MISSING.get() == 0 {
        return;
    }
    let matte = unsafe {
        windows::Win32::Graphics::Gdi::GetSysColor(windows::Win32::Graphics::Gdi::COLOR_BACKGROUND)
    };
    // wallpaper-blur-spec 8: the picture Windows reports for the primary monitor, at the preview
    // scale, plus the engine's own window snapshots; never a screen capture.
    let picture = tint::primary_preview();
    // No picture yet: show the desktop colour now and try again on the next paint (a few times).
    MISSING.set(if picture.is_some() || MISSING.get() >= PICTURE_RETRIES { 0 } else { MISSING.get() + 1 });
    let (source, desktop_layer) = picture
        .map(|mut image| {
            let k = tint::primary_rect()
                .map_or(1, |(l, _, r, _)| ((r - l) as f32 / image.width.max(1) as f32).round().max(1.0) as i32);
            let surfaces = surface_snapshots();
            for surface in &surfaces {
                composite_surface_scaled(&mut image, surface, k);
            }
            if !surfaces.is_empty() {
                image.solid = None;
            }
            (Rc::new(image), !surfaces.is_empty())
        })
        .unwrap_or_else(|| {
            // No picture in time (or no primary monitor): the desktop colour, never the multi-monitor source.
            (
                Rc::new(tint::Wallpaper {
                    origin: (0, 0),
                    width: 1,
                    height: 1,
                    bgra: color_argb(matte).to_le_bytes().to_vec(),
                    solid: Some(matte),
                }),
                false,
            )
        });
    let fallback = source.solid.is_some() || source.width <= 0 || source.height <= 0;
    let source = if fallback {
        let colour = matte;
        Rc::new(tint::Wallpaper {
            origin: (0, 0),
            width: 1,
            height: 1,
            bgra: color_argb(colour).to_le_bytes().to_vec(),
            solid: Some(colour),
        })
    } else {
        source
    };
    let sample = if fallback { source.clone() } else { blurred_sample(&source) };
    CACHE.with(|c| {
        *c.borrow_mut() = Some(Preview {
            source,
            sample,
            matte,
            fallback,
            desktop_layer,
            raster: None,
        })
    });
}

/// The picture Camouflage samples in the preview: the same blur as the real wallpaper, with the
/// sigma scaled by the preview scale (wallpaper-blur-spec 8), or the picture itself when blur is off.
fn blurred_sample(source: &Rc<tint::Wallpaper>) -> Rc<tint::Wallpaper> {
    tint::blur_radius()
        .and_then(|radius| tint::sample_blur(source, radius))
        .map_or_else(|| source.clone(), Rc::new)
}

/// Refresh only the sampled source after a blur gesture; retain the desktop frame
/// and DWM registrations. No recapture or polling on slider movement.
pub(super) fn refresh_blur_sample() {
    CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        let Some(preview) = cache.as_mut() else {
            return;
        };
        if !preview.fallback {
            preview.sample = blurred_sample(&preview.source);
        }
        preview.raster = None;
    });
}

/// Fit without cropping or stretching. Letterboxing uses the desktop colour.
fn fit(sw: i32, sh: i32, w: u32, h: u32) -> RectF {
    let ratio = (w as f32 / sw.max(1) as f32).min(h as f32 / sh.max(1) as f32);
    let (fw, fh) = (sw.max(1) as f32 * ratio, sh.max(1) as f32 * ratio);
    RectF::new(
        (w as f32 - fw) / 2.0,
        (h as f32 - fh) / 2.0,
        (w as f32 + fw) / 2.0,
        (h as f32 + fh) / 2.0,
    )
}
fn wallpaper_scaled(source: &tint::Wallpaper, w: u32, h: u32, matte: u32) -> tint::Wallpaper {
    let area = fit(source.width, source.height, w, h);
    let solid = source.solid.unwrap_or(matte);
    let mut bgra = vec![0; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let p = if source.solid.is_none()
                && (x as f32) >= area.left
                && (x as f32) < area.right
                && (y as f32) >= area.top
                && (y as f32) < area.bottom
            {
                let sx = (((x as f32 - area.left) / (area.right - area.left)) * source.width as f32)
                    as i32;
                let sy = (((y as f32 - area.top) / (area.bottom - area.top)) * source.height as f32)
                    as i32;
                source
                    .pixel(
                        sx.clamp(0, source.width - 1),
                        sy.clamp(0, source.height - 1),
                    )
                    .unwrap_or(color_argb(solid))
                    | 0xff00_0000
            } else {
                color_argb(solid)
            };
            bgra[((y * w + x) * 4) as usize..][..4].copy_from_slice(&p.to_le_bytes());
        }
    }
    tint::Wallpaper {
        origin: (0, 0),
        width: w as i32,
        height: h as i32,
        bgra,
        solid: None,
    }
}
fn rgba(c: u32, a: u8) -> Rgba {
    Rgba::new(c as u8, (c >> 8) as u8, (c >> 16) as u8, a)
}
/// camouflage-spec 3.1/3.5: independently sampled Corrals; only icon and background pixels map.
fn render(
    source: &tint::Wallpaper,
    sample: &tint::Wallpaper,
    matte: u32,
    settings: &Settings,
    w: u32,
    h: u32,
    dpi: u32,
) -> Result<Vec<u8>> {
    let wallpaper = wallpaper_scaled(source,w,h,matte);
    let sampled = wallpaper_scaled(sample,w,h,matte);
    let mut output = Surface::new(w,h)?;
    output.pixels_mut().copy_from_slice(&wallpaper.bgra);
    let m = group_paint::metrics(dpi);
    let pad = scf(16.0,dpi);
    let gap = scf(18.0,dpi);
    let fw = ((w as f32 - 2.0*pad-gap)/2.0).max(1.0);
    let top = scf(26.0,dpi);
    let bottom = (h as f32-scf(22.0,dpi)).max(top+1.0);
    let title_h = m.title_h as f32;
    let strength = super::look_pages::blend_percent(settings.desktop_blend_icon_opacity) as u8;
    let chosen = if settings.desktop_tint_icons_color_source == "corral" {settings.color} else {settings.desktop_tint_icons_color};
    for (i,title) in ["Work","Personal"].iter().enumerate() {
        let x = pad + i as f32*(fw+gap);
        let rect = [x.round() as i32,top.round() as i32,(x+fw).round() as i32,bottom.round() as i32];
        let palette = if settings.desktop_blend_icons && !settings.desktop_blend_uses_tint_icons_color {
            wallpaper_palette(&sampled,rect)
        } else {camouflage::derive_tint_palette(color_rgb(chosen))};
        let alpha = group_paint::see_through_alpha(settings.transparency_factor as i64);
        // Item labels read the body alone; the title text is decided by the painter (corral-painting-spec 4.4).
        let text_color=if settings.text_color_auto {auto_text_color(settings,&sampled,rect,&palette,strength,settings.color,alpha)} else {settings.text_color};
        let rendered_color=if settings.desktop_blend_icons && settings.desktop_blend_corral_backgrounds {mapped_color(&palette,strength,settings.color)}else{settings.color};
        let outer = crate::geometry::Rect::new(rect[0],rect[1],rect[2],rect[3]);
        let strip = crate::geometry::Rect::new(outer.left,outer.top,outer.right,(outer.top+m.title_h).min(outer.bottom));
        let weight = group_paint::title_weight(None);
        let text_w = group_paint::measure_title(title, dpi, weight, false);
        let buttons = settings.use_titlebar_controls;
        let paint = group_paint::PaintInputs {
            outer, strip: Some(strip), strip_above: true, folded: false, dpi, square_edges: settings.square_edges,
            body_rgb: group_paint::colorref_rgb(rendered_color), fill_alpha: alpha, shade: settings.darken_titlebars,
            outline: settings.outline, fade: 1.0, reveal: 1.0, strip_hover: 0.0,
            text: if settings.text_color_auto { group_paint::TextMode::Auto } else { group_paint::TextMode::Custom(group_paint::colorref_rgb(settings.text_color)) },
            backdrop_mean: camouflage::mean_color(&camouflage::sample_rect(&sampled, rect, 32)),
            title, weight, italic: false,
            layout: group_paint::layout_title(&group_paint::TitleLayoutInput { strip, dpi,
                want: group_paint::ButtonSet { menu: buttons, up: false, close: buttons }, folder: false, text_w }),
            menu: group_paint::ButtonState::Rest, up: group_paint::ButtonState::Rest, close: group_paint::ButtonState::Rest,
            buttons: 0.0, tabs: None, high_contrast: group_paint::high_contrast(),
        };
        let mut body=Surface::new(w,h)?;
        body.draw(|ctx| {
            ctx.clear(Rgba::TRANSPARENT);
            group_paint::paint_fills(&mut group_paint::CtxSink(ctx), &paint)
        })?;

        if tint::blur_gate(settings.background_blur_enabled,settings.background_blur_radius).is_some() && paint.high_contrast.is_none() {
            let mut mask=Surface::new(w,h)?;
            let radius = paint.radius();
            mask.draw(|ctx| {ctx.clear(Rgba::TRANSPARENT);ctx.fill_corner_shape(RectF::new(x,top,x+fw,bottom),[radius;4],Rgba::new(255,255,255,255))})?;
            tint::composite_backdrop_from(body.pixels_mut(),mask.pixels(),w as usize,h as usize,(0,0),255,1.0,&sampled);
        }
        let mut icons=Surface::new(w,h)?;
        icons.draw(|ctx| {
            ctx.clear(Rgba::TRANSPARENT);
            for j in 0..2 {
                let cx=x+fw*(j as f32+0.5)/2.0;
                let y=top+title_h+scf(20.0,dpi);
                let size=scf(27.0,dpi);
                ctx.fill_rounded_rect(RectF::new(cx-size/2.0,y,cx+size/2.0,y+size),scf(2.0,dpi),if j==0{Rgba::new(225,235,250,255)}else{Rgba::new(240,190,65,255)})?;
            }
            Ok(())
        })?;
        if settings.desktop_blend_icons || settings.desktop_tint_icons {
            apply_pixels(&palette,if settings.desktop_blend_icons{strength}else{100},icons.pixels_mut());
        }
        output.draw(|ctx| {
            let dest=RectF::new(0.0,0.0,w as f32,h as f32);
            ctx.draw_bitmap(body.pixels(),w,h,dest,1.0)?;
            ctx.draw_bitmap(icons.pixels(),w,h,dest,1.0)?;
            for (j,label) in ["Document","Folder"].iter().enumerate() {
                let cx=x+fw*(j as f32+0.5)/2.0;
                let y=top+title_h+scf(50.0,dpi);
                ctx.draw_text(label,RectF::new(cx-fw/4.0,y,cx+fw/4.0,y+scf(30.0,dpi)),&TextStyle{family:crate::style::FONT,size_px:scf(12.0,dpi),weight:400,color:rgba(if settings.text_color_auto{text_color}else{0x00ff_ffff},255),align:Align::Center,shadow:Some((Rgba::new(0,0,0,160),1.0,1.0))})?;
            }
            group_paint::paint_chrome(&mut group_paint::CtxSink(ctx), &paint)
        })?;
    }
    Ok(output.pixels().to_vec())
}

pub(super) fn draw(
    ctx: &crate::render::Ctx,
    lay: &PageLay,
    settings: &Settings,
    theme: &Theme,
    dpi: u32,
) -> Result<()> {
    let Some(rect) = lay.preview else {
        return Ok(());
    };
    draw_rect(ctx, rect, dpi, theme, settings)
}
/// Rectangle-based U3 renderer. Desktop capture and DWM composition are unchanged.
pub(super) fn draw_rect(
    ctx: &crate::render::Ctx,
    rect: RectF,
    dpi: u32,
    theme: &Theme,
    settings: &Settings,
) -> Result<()> {
    let (w, h) = (
        (rect.right - rect.left).round().max(1.0) as u32,
        (rect.bottom - rect.top).round().max(1.0) as u32,
    );
    CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        // Offscreen callers have no page-entry side effects and never read private wallpaper.
        let preview = cache.get_or_insert_with(|| Preview {
            source: Rc::new(tint::Wallpaper {
                origin: (0, 0),
                width: 1,
                height: 1,
                bgra: vec![32, 32, 32, 255],
                solid: Some(0x202020),
            }),
            sample: Rc::new(tint::Wallpaper {
                origin: (0, 0),
                width: 1,
                height: 1,
                bgra: vec![32, 32, 32, 255],
                solid: Some(0x202020),
            }),
            matte: 0x202020,
            fallback: true,
            desktop_layer: false,
            raster: None,
        });
        let key = format!(
            "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{:?}",
            LIVE.with(|live| {
                let live = live.borrow();
                live.desktop || live.real
            }),
            dpi,
            settings.desktop_blend_icons,
            settings.color,
            settings.transparency_factor,
            settings.text_color,
            settings.darken_titlebars,
            settings.square_edges,
            settings.desktop_blend_icon_opacity,
            w,
            (settings.desktop_blend_corral_backgrounds,settings.desktop_blend_uses_tint_icons_color,settings.desktop_tint_icons,settings.desktop_tint_icons_color,settings.desktop_tint_icons_color_source.clone(),settings.text_color_auto)
        );
        if !preview
            .raster
            .as_ref()
            .is_some_and(|(k, rw, rh, _)| k == &key && *rw == w && *rh == h)
        {
            preview.raster = Some((
                key,
                w,
                h,
                if preview.desktop_layer
                    || LIVE.with(|live| {
                        let live = live.borrow();
                        live.desktop || live.real
                    })
                {
                    wallpaper_scaled(&preview.source, w, h, preview.matte).bgra
                } else {
                    render(
                        &preview.source,
                        &preview.sample,
                        preview.matte,
                        settings,
                        w,
                        h,
                        dpi,
                    )?
                },
            ));
            // Only layout masking changes: capture selection, recolouring and DWM remain intact.
            let mut mask = Surface::new(w, h)?;
            mask.draw(|ctx| {
                ctx.clear(Rgba::TRANSPARENT);
                ctx.fill_rounded_rect(
                    RectF::new(0.0, 0.0, w as f32, h as f32),
                    scf(8.0, dpi),
                    Rgba::new(255, 255, 255, 255),
                )
            })?;
            for (pixel, coverage) in preview
                .raster
                .as_mut()
                .unwrap()
                .3
                .chunks_exact_mut(4)
                .zip(mask.pixels().chunks_exact(4))
            {
                for channel in pixel {
                    *channel = ((*channel as u32 * coverage[3] as u32 + 127) / 255) as u8;
                }
            }
        }
        ctx.draw_bitmap(&preview.raster.as_ref().unwrap().3, w, h, rect, 1.0)?;
        ctx.stroke_rounded_rect(
            rect,
            scf(8.0, dpi),
            scf(1.0, dpi),
            super::kit::theme::Theme::new(theme.light).border,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scrolling_clips_live_destinations_and_their_sources_together() {
        let s = RECT {
            left: 100,
            top: 200,
            right: 900,
            bottom: 600,
        };
        let d = RECT {
            left: 300,
            top: -100,
            right: 700,
            bottom: 100,
        };
        let v = RECT {
            left: 0,
            top: 0,
            right: 760,
            bottom: 520,
        };
        let (source, dest) = clip_mapping(s, d, v).unwrap();
        assert_eq!(
            (dest.left, dest.top, dest.right, dest.bottom),
            (300, 0, 700, 100)
        );
        assert_eq!(
            (source.left, source.top, source.right, source.bottom),
            (100, 400, 900, 600)
        );
        assert!(
            clip_mapping(
                s,
                RECT {
                    left: 300,
                    top: -300,
                    right: 700,
                    bottom: -100
                },
                v
            )
            .is_none()
        );
    }
    #[test]
    fn q33k_preview_blur_supplies_a_backdrop_even_at_full_see_through() {
        let source = fixture();
        let sample = tint::Wallpaper {
            origin: (0, 0),
            width: 1,
            height: 1,
            bgra: vec![80, 120, 160, 255],
            solid: Some(0x5078a0),
        };
        let mut s = Settings::default();
        s.desktop_blend_icons = true;
        // Most see-through (corral-painting-spec 3.3: stored 0 gives body alpha 20 of 255, the floor).
        s.transparency_factor = 0;
        s.darken_titlebars = 0;
        s.background_blur_enabled = false;
        let sharp = render(&source, &sample, 0, &s, 560, 225, 96).unwrap();
        s.background_blur_enabled = true;
        s.background_blur_radius = 60.0;
        let blur = render(&source, &sample, 0, &s, 560, 225, 96).unwrap();
        let inside = (185 * 560 + 25) * 4;
        // the black body at alpha 20 lies over the backdrop: backdrop * 235 / 255
        for (got, want) in blur[inside..inside + 4].iter().zip([74i32, 111, 147, 255]) {
            assert!((*got as i32 - want).abs() <= 1, "blurred backdrop shows through: {:?}", &blur[inside..inside + 4]);
        }
        assert_ne!(&sharp[inside..inside + 4], &blur[inside..inside + 4]);
        assert_eq!(
            &sharp[..4],
            &blur[..4],
            "wallpaper outside the sample Corral stays sharp"
        );
        s.background_blur_radius = 0.0;
        assert_eq!(
            render(&source, &sample, 0, &s, 560, 225, 96).unwrap(),
            sharp
        );
    }
    #[test]
    fn page_switch_reuses_capture_until_window_or_display_reset() {
        reset();
        let source = Rc::new(fixture());
        let weak = Rc::downgrade(&source);
        CACHE.with(|cache| {
            *cache.borrow_mut() = Some(Preview {
                source: source.clone(),
                sample: source.clone(),
                matte: 0x123456,
                fallback: false,
                desktop_layer: true,
                raster: None,
            })
        });
        drop(source);
        LIVE.with(|live| {
            let mut live = live.borrow_mut();
            live.visible = true;
            live.desktop = true;
            live.source_order = vec![123];
        });
        for _ in 0..3 {
            leave();
            assert!(LIVE.with(|live| !live.borrow().visible));
            enter();
            assert!(CACHE.with(|cache| Rc::ptr_eq(
                &cache.borrow().as_ref().unwrap().source,
                &weak.upgrade().unwrap()
            )));
            assert!(LIVE.with(|live| live.borrow().source_order == [123] && live.borrow().desktop));
        }
        reset();
        assert!(
            weak.upgrade().is_none(),
            "window close releases captured image storage"
        );
        assert!(CACHE.with(|cache| cache.borrow().is_none()));
        assert!(LIVE.with(|live| live.borrow().source_order.is_empty() && !live.borrow().desktop));
    }
    #[test]
    fn primary_mapping_uses_physical_coordinates_across_negative_secondary_and_dpi() {
        let primary = RECT {
            left: 0,
            top: 0,
            right: 2560,
            bottom: 1440,
        };
        let virtual_host = RECT {
            left: -3840,
            top: 0,
            right: 2560,
            bottom: 2160,
        };
        let secondary = RECT {
            left: -3840,
            top: 0,
            right: 0,
            bottom: 2160,
        };
        for factor in [1, 2, 3] {
            let preview = RECT {
                left: 20 * factor,
                top: 30 * factor,
                right: 660 * factor,
                bottom: 430 * factor,
            };
            let (source, dest) = mapping(primary, virtual_host, preview).unwrap();
            assert_eq!(
                (source.left, source.top, source.right, source.bottom),
                (3840, 0, 6400, 1440)
            );
            assert_eq!(
                (dest.left, dest.top, dest.right, dest.bottom),
                (20 * factor, 50 * factor, 660 * factor, 410 * factor)
            );
            assert!(mapping(primary, secondary, preview).is_none());
            let corral = RECT {
                left: -100,
                top: 400,
                right: 300,
                bottom: 800,
            };
            let (source, dest) = mapping(primary, corral, preview).unwrap();
            assert_eq!((source.left, source.right), (100, 400));
            assert_eq!(dest.left, 20 * factor);
            assert_eq!(dest.top, 150 * factor);
            assert_eq!(dest.bottom, 250 * factor);
        }
    }
    #[test]
    fn real_surface_composite_clips_and_preserves_transparent_portal_holes() {
        let mut dst = tint::Wallpaper {
            origin: (0, 0),
            width: 2,
            height: 1,
            bgra: [10, 20, 30, 255].repeat(2),
            solid: None,
        };
        let src = tint::Wallpaper {
            origin: (-1, 0),
            width: 3,
            height: 1,
            bgra: vec![255, 0, 0, 255, 0, 0, 0, 0, 50, 10, 0, 128],
            solid: None,
        };
        composite_surface(&mut dst, &src);
        assert_eq!(&dst.bgra[..4], &[10, 20, 30, 255]);
        assert_eq!(&dst.bgra[4..], &[55, 20, 15, 255]);
    }
    #[test]
    fn preview_source_class_policy_excludes_settings_and_other_apps() {
        assert!(engine_class("CorralGroup"));
        assert!(engine_class("CorralSurface"));
        for name in [
            "CorralSettings",
            "CorralPreview",
            "CabinetWClass",
            "CorralGhost",
        ] {
            assert!(!engine_class(name));
        }
    }
    fn fixture() -> tint::Wallpaper {
        let mut bgra = vec![0; 320 * 180 * 4];
        for y in 0..180 {
            for x in 0..320 {
                let p = (y * 320 + x) * 4;
                bgra[p..p + 4].copy_from_slice(&[x as u8, (y + 40) as u8, 190, 255]);
            }
        }
        tint::Wallpaper {
            origin: (-320, -180),
            width: 320,
            height: 180,
            bgra,
            solid: None,
        }
    }
    #[test]
    fn fit_keeps_aspect_and_samples_solid_or_missing_sources() {
        let r = fit(400, 200, 200, 200);
        assert_eq!(
            (r.left, r.top, r.right, r.bottom),
            (0.0, 50.0, 200.0, 150.0)
        );
        let source = tint::Wallpaper {
            origin: (0, 0),
            width: 0,
            height: 0,
            bgra: vec![],
            solid: Some(0x123456),
        };
        let wp = wallpaper_scaled(&source, 20, 30, 0x987654);
        assert!(
            wp.bgra
                .chunks_exact(4)
                .all(|p| p == [0x12, 0x34, 0x56, 255])
        );
        let image = wallpaper_scaled(&fixture(), 200, 200, 0x987654);
        assert_eq!(image.pixel(0, 0), Some(0xff547698));
        assert_ne!(image.pixel(100, 100), image.pixel(0, 0));
    }
    #[test]
    fn recolouring_samples_wallpaper_under_corrals_without_changing_wallpaper() {
        let source = fixture();
        let red = tint::Wallpaper {
            origin: (0, 0),
            width: 1,
            height: 1,
            bgra: vec![0, 0, 255, 255],
            solid: Some(0x0000ff),
        };
        let blue = tint::Wallpaper {
            origin: (0, 0),
            width: 1,
            height: 1,
            bgra: vec![255, 0, 0, 255],
            solid: Some(0xff0000),
        };
        let mut settings = Settings::default();
        let plain = render(&source, &red, 0x123456, &settings, 560, 225, 96).unwrap();
        assert_eq!(
            plain,
            render(&source, &blue, 0x123456, &settings, 560, 225, 96).unwrap()
        );
        settings.desktop_blend_icons = true;
        settings.desktop_blend_corral_backgrounds = true;
        let a = render(&source, &red, 0x123456, &settings, 560, 225, 96).unwrap();
        let b = render(&source, &blue, 0x123456, &settings, 560, 225, 96).unwrap();
        let body = (185 * 560 + 25) * 4;
        assert_ne!(&a[body..body + 4], &b[body..body + 4]);
        assert_eq!(&a[..4], &b[..4]);
        assert_eq!(
            &a[..4],
            &plain[..4],
            "outside the Corral the wallpaper stays sharp"
        );
    }
    #[test]
    fn live_preview_raster_toggle_cache_and_geometry_both_themes_all_dpis() {
        for dpi in [96, 120, 144, 240] {
            for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
                CACHE.with(|c| {
                    *c.borrow_mut() = Some(Preview {
                        source: Rc::new(fixture()),
                        sample: Rc::new(fixture()),
                        matte: 0x345678,
                        fallback: false,
                        desktop_layer: false,
                        raster: None,
                    })
                });
                let mut settings = Settings::default();
                settings.desktop_blend_corral_backgrounds = true;
                let view = super::super::View {
                    page: super::super::Page::Camouflage,
                    hover: None,
                    scroll: 0,
                };
                let mut surface = Surface::new(
                    super::super::sc(828.0, dpi) as u32,
                    super::super::sc(655.0, dpi) as u32,
                )
                .unwrap();
                super::super::render_settings_state(&mut surface, &view, &settings, &theme, dpi)
                    .unwrap();
                let before = surface.pixels().to_vec();
                let ptr = CACHE.with(|c| {
                    c.borrow()
                        .as_ref()
                        .unwrap()
                        .raster
                        .as_ref()
                        .unwrap()
                        .3
                        .as_ptr()
                });
                super::super::render_settings_state(&mut surface, &view, &settings, &theme, dpi)
                    .unwrap();
                assert_eq!(
                    ptr,
                    CACHE.with(|c| c
                        .borrow()
                        .as_ref()
                        .unwrap()
                        .raster
                        .as_ref()
                        .unwrap()
                        .3
                        .as_ptr())
                );
                assert_eq!(before, surface.pixels());
                settings.desktop_blend_icons = true;
                super::super::render_settings_state(&mut surface, &view, &settings, &theme, dpi)
                    .unwrap();
                assert_ne!(before, surface.pixels(), "toggle dpi {dpi}");
                let lay = super::super::layout_page(
                    view.page,
                    &settings,
                    dpi,
                    surface.width() as f32,
                    surface.height() as f32,
                    0,
                )
                .unwrap();
                let pb = lay.preview.unwrap();
                let (pw, ph) = (pb.right - pb.left, pb.bottom - pb.top);
                let fw = (pw - 2.0 * scf(16.0, dpi) - scf(18.0, dpi)) / 2.0;
                // Sample inside an empty part of each Corral, away from its title/icons/checkbox.
                for corral in 0..2 {
                    let x = (pb.left
                        + scf(16.0, dpi)
                        + corral as f32 * (fw + scf(18.0, dpi))
                        + scf(8.0, dpi)) as usize;
                    let y = (pb.top + ph - scf(22.0 + 8.0, dpi)) as usize;
                    let at = (y * surface.width() as usize + x) * 4;
                    assert_ne!(
                        &before[at..at + 4],
                        &surface.pixels()[at..at + 4],
                        "body recolour {dpi} {corral}"
                    );
                }
                let lay = super::super::layout_page(
                    view.page,
                    &settings,
                    dpi,
                    surface.width() as f32,
                    surface.height() as f32,
                    0,
                )
                .unwrap();
                let (_, checkbox) = lay
                    .master
                    .expect("Camouflage master switch lives in the page header");
                assert_eq!(
                    super::super::hit_test(
                        &settings,
                        view.page,
                        (
                            checkbox.left as i32 + 3,
                            ((checkbox.top + checkbox.bottom) / 2.0) as i32
                        ),
                        dpi,
                        surface.width() as i32,
                        surface.height() as i32,
                        0
                    )
                    .unwrap(),
                    super::super::Hit::Toggle(crate::settings::Field::BlendIcons)
                );
                std::fs::create_dir_all("target/camouflage-ui-qa/preview").unwrap();
                surface
                    .save_png(&std::path::PathBuf::from(format!(
                        "target/camouflage-ui-qa/preview/camouflage-{name}-{dpi}.png"
                    )))
                    .unwrap();
            }
        }
        CACHE.with(|c| *c.borrow_mut() = None);
    }
}

#[cfg(test)]
mod box_size_tests {
    use super::box_size;
    const QHD: (i32, i32) = (2560, 1440);
    fn aspect(b: (f32, f32)) -> f32 {
        b.1 / b.0
    }
    #[test]
    fn normal_window_uses_full_width_at_monitor_aspect() {
        let b = box_size(700.0, 1200.0, QHD, 96);
        assert_eq!(b.0, 700.0);
        assert!((aspect(b) - 0.5625).abs() < 0.01, "{b:?}");
    }
    #[test]
    fn maximised_wide_window_is_capped_and_keeps_aspect() {
        let b = box_size(2200.0, 1000.0, QHD, 96);
        assert_eq!(b.1, 450.0);
        assert!(b.0 < 2200.0 && (aspect(b) - 0.5625).abs() < 0.01, "{b:?}");
    }
    #[test]
    fn narrow_window_shrinks_with_width_and_never_exceeds_it() {
        let b = box_size(240.0, 900.0, QHD, 96);
        assert_eq!(b.0, 240.0);
        assert!(b.1 < 405.0);
        let tiny = box_size(0.0, 0.0, QHD, 96);
        assert!(tiny.0 >= 1.0 && tiny.1 >= 1.0);
    }
    #[test]
    fn mixed_dpi_scales_proportionally_and_portrait_monitors_work() {
        let a = box_size(700.0, 1200.0, QHD, 96);
        let b = box_size(1750.0, 3000.0, QHD, 240);
        assert!(
            (b.0 / a.0 - 2.5).abs() < 0.01 && (b.1 / a.1 - 2.5).abs() < 0.02,
            "{a:?} {b:?}"
        );
        let p = box_size(1000.0, 800.0, (1080, 1920), 96);
        assert_eq!(p.1, 360.0);
        assert!(p.0 < 1000.0);
    }
}

#[cfg(test)]
mod q33l_preview_tests {
    use super::*;
    #[test]
    fn blur_backing_works_with_camouflage_off_and_leaves_wallpaper_outside_unchanged() {
        let source = tint::Wallpaper {
            origin: (0, 0),
            width: 1,
            height: 1,
            bgra: vec![20, 40, 180, 255],
            solid: Some(0xb42814),
        };
        let sample = tint::Wallpaper {
            origin: (0, 0),
            width: 1,
            height: 1,
            bgra: vec![180, 40, 20, 255],
            solid: Some(0x1428b4),
        };
        let mut s = Settings::default();
        s.desktop_blend_icons = false;
        let sharp = render(&source, &sample, 0, &s, 640, 240, 96).unwrap();
        s.background_blur_enabled = true;
        s.background_blur_radius = 60.0;
        let blurred = render(&source, &sample, 0, &s, 640, 240, 96).unwrap();
        let pixel =
            |b: &[u8], x: usize, y: usize| b[(y * 640 + x) * 4..(y * 640 + x + 1) * 4].to_vec();
        assert_eq!(pixel(&sharp, 0, 0), pixel(&blurred, 0, 0));
        assert_ne!(pixel(&sharp, 30, 160), pixel(&blurred, 30, 160));
    }
}
