//! Where each monitor's picture comes from (wallpaper-blur-spec 3.1): the public desktop-wallpaper
//! COM interface, then the system-wide wallpaper path, then a flat colour. Decoding is by WIC,
//! scaled straight to the working size so a large picture never exists at full size in memory.
//! Nothing here captures the screen.

use super::placement::Mode;

/// Pictures above this many pixels are not decoded (wallpaper-blur-spec 3.1).
pub const MAX_PIXELS: u64 = 128_000_000;

pub fn exceeds_limit(w: u32, h: u32) -> bool {
    w as u64 * h as u64 > MAX_PIXELS
}

/// Identity of a source for the cache key: a file by path, modification time and size, or a colour.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum SourceId {
    File { path: String, mtime: u64, size: u64 },
    Solid(u32),
}

/// (modification time in nanoseconds since the epoch, size in bytes) of a readable file.
pub fn file_stamp(path: &str) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as u64);
    Some((mtime, meta.len()))
}

/// First usable path in order (the monitor's own, then the system-wide one), else the flat colour
/// (wallpaper-blur-spec 3.1). `probe` returns the file stamp of a path that can be read.
pub fn pick_source(monitor_path: &str, system_path: &str, background: u32, probe: &dyn Fn(&str) -> Option<(u64, u64)>) -> SourceId {
    for path in [monitor_path, system_path] {
        if path.is_empty() {
            continue;
        }
        if let Some((mtime, size)) = probe(path) {
            return SourceId::File { path: path.to_string(), mtime, size };
        }
    }
    SourceId::Solid(background)
}

/// Pair each monitor with the wallpaper interface's entry for the same rectangle: exact match first,
/// else the entry overlapping most (wallpaper-blur-spec 3.1 "joined to the monitor"). `entries` are
/// the interface's monitor rectangles; the result holds the entry index for each monitor.
pub fn join_monitors(entries: &[[i32; 4]], monitors: &[[i32; 4]]) -> Vec<Option<usize>> {
    let area = |a: &[i32; 4], b: &[i32; 4]| -> i64 {
        let w = (a[2].min(b[2]) as i64 - a[0].max(b[0]) as i64).max(0);
        let h = (a[3].min(b[3]) as i64 - a[1].max(b[1]) as i64).max(0);
        w * h
    };
    let mut taken = vec![false; entries.len()];
    let mut result = vec![None; monitors.len()];
    for (m, rect) in monitors.iter().enumerate() {
        if let Some(e) = entries.iter().enumerate().position(|(i, r)| !taken[i] && r == rect) {
            taken[e] = true;
            result[m] = Some(e);
        }
    }
    for (m, rect) in monitors.iter().enumerate() {
        if result[m].is_some() {
            continue;
        }
        let best = entries
            .iter()
            .enumerate()
            .filter(|(i, _)| !taken[*i])
            .map(|(i, r)| (i, area(r, rect)))
            .filter(|(_, a)| *a > 0)
            .max_by_key(|(_, a)| *a);
        if let Some((e, _)) = best {
            taken[e] = true;
            result[m] = Some(e);
        }
    }
    result
}

/// What the desktop reports (wallpaper-blur-spec 3.1).
#[derive(Clone, Debug, PartialEq)]
pub struct DesktopQuery {
    /// Per monitor, parallel to the monitor list; empty when the interface has no usable path.
    pub paths: Vec<String>,
    /// The single fit mode of the whole desktop.
    pub mode: Mode,
    /// `COLORREF` of the desktop background.
    pub background: u32,
    /// True while a slideshow is running (polling is on only then).
    pub slideshow: bool,
    /// `SystemParametersInfoW(SPI_GETDESKWALLPAPER)`: one picture for every monitor.
    pub system_path: String,
}

/// Decoder seam: tests supply synthetic pictures, the engine supplies WIC.
pub trait Decoder {
    /// Picture size in pixels (header only).
    fn dimensions(&self, path: &str) -> Result<(u32, u32), String>;
    /// The `region` (`x, y, w, h` in picture pixels) scaled to `w x h`, straight-alpha BGRA.
    fn scaled(&self, path: &str, region: (u32, u32, u32, u32), w: u32, h: u32) -> Result<Vec<u8>, String>;
}

// ---- Windows --------------------------------------------------------------------------------------

use windows::Win32::Foundation::GENERIC_READ;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, IWICBitmapFrameDecode, IWICBitmapSource, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx};
use windows::core::{HSTRING, Interface, PCWSTR};

/// WIC decoding (JPEG, PNG, BMP, TIFF, installed codecs; GIF uses its first frame).
/// The calling thread must have COM initialised.
pub struct WicDecoder {
    factory: IWICImagingFactory,
}

impl WicDecoder {
    pub fn new() -> Result<WicDecoder, String> {
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.map_err(|e| e.to_string())?;
        Ok(WicDecoder { factory })
    }

    fn frame(&self, path: &str) -> Result<IWICBitmapFrameDecode, String> {
        unsafe {
            let decoder = self
                .factory
                .CreateDecoderFromFilename(&HSTRING::from(path), None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
                .map_err(|e| e.to_string())?;
            decoder.GetFrame(0).map_err(|e| e.to_string())
        }
    }
}

impl Decoder for WicDecoder {
    fn dimensions(&self, path: &str) -> Result<(u32, u32), String> {
        let frame = self.frame(path)?;
        let (mut w, mut h) = (0u32, 0u32);
        unsafe { frame.GetSize(&mut w, &mut h) }.map_err(|e| e.to_string())?;
        Ok((w, h))
    }

    fn scaled(&self, path: &str, region: (u32, u32, u32, u32), w: u32, h: u32) -> Result<Vec<u8>, String> {
        let (w, h) = (w.max(1), h.max(1));
        let frame = self.frame(path)?;
        unsafe {
            let (mut fw, mut fh) = (0u32, 0u32);
            frame.GetSize(&mut fw, &mut fh).map_err(|e| e.to_string())?;
            let mut source: IWICBitmapSource = frame.cast().map_err(|e| e.to_string())?;
            // Clip first, so only the visible part is scaled (and, for tiled formats, decoded).
            if region != (0, 0, fw, fh) {
                let clipper = self.factory.CreateBitmapClipper().map_err(|e| e.to_string())?;
                let rect = WICRect { X: region.0 as i32, Y: region.1 as i32, Width: region.2 as i32, Height: region.3 as i32 };
                clipper.Initialize(&source, &rect).map_err(|e| e.to_string())?;
                source = clipper.cast().map_err(|e| e.to_string())?;
            }
            let scaler = self.factory.CreateBitmapScaler().map_err(|e| e.to_string())?;
            scaler.Initialize(&source, w, h, WICBitmapInterpolationModeFant).map_err(|e| e.to_string())?;
            let convert = self.factory.CreateFormatConverter().map_err(|e| e.to_string())?;
            let scaled: IWICBitmapSource = scaler.cast().map_err(|e| e.to_string())?;
            convert
                .Initialize(&scaled, &GUID_WICPixelFormat32bppBGRA, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)
                .map_err(|e| e.to_string())?;
            let mut out = vec![0u8; w as usize * h as usize * 4];
            convert.CopyPixels(std::ptr::null(), w * 4, &mut out).map_err(|e| e.to_string())?;
            Ok(out)
        }
    }
}

/// Initialise COM for the calling thread as a single-threaded apartment (needed by the wallpaper
/// interface). Safe to call repeatedly.
pub fn init_apartment() {
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
}

/// Ask the desktop for each monitor's picture, the fit mode, the background colour and the slideshow
/// state (wallpaper-blur-spec 3.1). Failures degrade field by field: the system-wide path and the
/// `COLOR_DESKTOP` colour are always filled in.
pub fn query_desktop(monitors: &[[i32; 4]]) -> DesktopQuery {
    use windows::Win32::Graphics::Gdi::{COLOR_DESKTOP, GetSysColor};
    use windows::Win32::UI::Shell::{DSS_SLIDESHOW, DesktopWallpaper, IDesktopWallpaper};

    let system_path = system_wallpaper_path();
    let sys_color = unsafe { GetSysColor(COLOR_DESKTOP) };
    // The fit mode the registry holds is the fallback when the COM interface cannot answer.
    let mut query = DesktopQuery { paths: vec![String::new(); monitors.len()], mode: registry_mode(), background: sys_color, slideshow: false, system_path };
    unsafe {
        // The wallpaper class is registered for an out-of-process server only.
        let Ok(wallpaper) = CoCreateInstance::<_, IDesktopWallpaper>(&DesktopWallpaper, None, CLSCTX_LOCAL_SERVER) else {
            return query;
        };
        if let Ok(color) = wallpaper.GetBackgroundColor() {
            query.background = color.0;
        }
        if let Ok(position) = wallpaper.GetPosition() {
            query.mode = Mode::from_position(position.0);
        }
        query.slideshow = wallpaper.GetStatus().is_ok_and(|s| (s.0 & DSS_SLIDESHOW.0) != 0);
        let count = wallpaper.GetMonitorDevicePathCount().unwrap_or(0);
        let mut ids = Vec::new();
        let mut rects = Vec::new();
        for i in 0..count {
            let Ok(id) = wallpaper.GetMonitorDevicePathAt(i) else { continue };
            let id_text = take_pwstr(id);
            let id_h = HSTRING::from(id_text.as_str());
            let Ok(rect) = wallpaper.GetMonitorRECT(PCWSTR(id_h.as_ptr())) else { continue };
            ids.push(id_text);
            rects.push([rect.left, rect.top, rect.right, rect.bottom]);
        }
        for (m, entry) in join_monitors(&rects, monitors).into_iter().enumerate() {
            let Some(e) = entry else { continue };
            let id_h = HSTRING::from(ids[e].as_str());
            if let Ok(path) = wallpaper.GetWallpaper(PCWSTR(id_h.as_ptr())) {
                query.paths[m] = take_pwstr(path);
            }
        }
    }
    query
}

/// Fit mode from the `WallpaperStyle` and `TileWallpaper` values of the desktop settings:
/// tile wins, then 0 Center, 2 Stretch, 6 Fit, 10 Fill, 22 Span; anything else counts as Fill.
pub fn mode_from_style(style: &str, tile: &str) -> Mode {
    if tile.trim() == "1" {
        return Mode::Tile;
    }
    match style.trim() {
        "0" => Mode::Center,
        "2" => Mode::Stretch,
        "6" => Mode::Fit,
        "22" => Mode::Span,
        _ => Mode::Fill,
    }
}

fn desktop_string(name: windows::core::PCWSTR) -> String {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    let mut buffer = [0u16; 16];
    let mut size = (buffer.len() * 2) as u32;
    let ok = unsafe {
        RegGetValueW(HKEY_CURRENT_USER, windows::core::w!(r"Control Panel\Desktop"), name, RRF_RT_REG_SZ, None, Some(buffer.as_mut_ptr().cast()), Some(&mut size))
    };
    if ok.is_err() {
        return String::new();
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

/// Read-only registry fallback for the fit mode (wallpaper-blur-spec 3.2).
fn registry_mode() -> Mode {
    use windows::core::w;
    mode_from_style(&desktop_string(w!("WallpaperStyle")), &desktop_string(w!("TileWallpaper")))
}

/// Copy a COM-allocated wide string and free it.
unsafe fn take_pwstr(p: windows::core::PWSTR) -> String {
    if p.is_null() {
        return String::new();
    }
    let text = unsafe { p.to_string() }.unwrap_or_default();
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _)) };
    text
}

/// `SPI_GETDESKWALLPAPER` into a `MAX_PATH` buffer (wallpaper-blur-spec 3.1 step 2).
pub fn system_wallpaper_path() -> String {
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
    let mut buffer = [0u16; 260];
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETDESKWALLPAPER, buffer.len() as u32, Some(buffer.as_mut_ptr().cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
    }
    .is_ok();
    if !ok {
        return String::new();
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..len])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_of<'a>(existing: &'a [&'a str]) -> impl Fn(&str) -> Option<(u64, u64)> + 'a {
        move |p| existing.contains(&p).then_some((11, 22))
    }

    #[test]
    fn empty_monitor_path_falls_back_to_the_system_path_then_to_solid() {
        let probe = probe_of(&["C:\\pics\\a.jpg"]);
        assert_eq!(
            pick_source("", "C:\\pics\\a.jpg", 0x123456, &probe),
            SourceId::File { path: "C:\\pics\\a.jpg".into(), mtime: 11, size: 22 }
        );
        assert_eq!(pick_source("", "", 0x123456, &probe), SourceId::Solid(0x123456));
        assert_eq!(pick_source("", "C:\\missing.jpg", 7, &probe), SourceId::Solid(7), "an unreadable file is not usable");
        // The monitor's own path wins over the system path.
        let probe = probe_of(&["m.jpg", "s.jpg"]);
        assert!(matches!(pick_source("m.jpg", "s.jpg", 0, &probe), SourceId::File { path, .. } if path == "m.jpg"));
        // Its own path unreadable: the system path is used.
        let probe = probe_of(&["s.jpg"]);
        assert!(matches!(pick_source("m.jpg", "s.jpg", 0, &probe), SourceId::File { path, .. } if path == "s.jpg"));
    }

    #[test]
    fn registry_style_values_map_to_fit_modes() {
        assert_eq!(mode_from_style("10", "0"), Mode::Fill);
        assert_eq!(mode_from_style("6", "0"), Mode::Fit);
        assert_eq!(mode_from_style("2", "0"), Mode::Stretch);
        assert_eq!(mode_from_style("0", "0"), Mode::Center);
        assert_eq!(mode_from_style("0", "1"), Mode::Tile);
        assert_eq!(mode_from_style("22", "0"), Mode::Span);
        assert_eq!(mode_from_style("", ""), Mode::Fill);
    }

    #[test]
    fn size_limit_is_128_megapixels() {
        assert!(!exceeds_limit(16_000, 8_000));
        assert!(exceeds_limit(12_900, 10_000), "129 megapixels");
        assert!(!exceeds_limit(0, 0));
    }

    #[test]
    fn join_prefers_exact_rectangles_then_overlap() {
        let monitors = [[0, 0, 1920, 1080], [-1920, -200, 0, 1000]];
        let entries = [[-1920, -200, 0, 1000], [0, 0, 1920, 1080]];
        assert_eq!(join_monitors(&entries, &monitors), vec![Some(1), Some(0)]);
        // Slightly different rectangles (virtualised coordinates): the largest overlap wins.
        let entries = [[0, 0, 1900, 1060]];
        assert_eq!(join_monitors(&entries, &monitors), vec![Some(0), None]);
        // An entry is used at most once.
        let entries = [[0, 0, 1920, 1080]];
        assert_eq!(join_monitors(&entries, &[[0, 0, 1920, 1080], [0, 0, 1920, 1080]]), vec![Some(0), None]);
    }

    #[test]
    fn file_stamp_of_a_missing_file_or_a_directory_is_none() {
        assert_eq!(file_stamp("Z:\\definitely\\missing.jpg"), None);
        assert_eq!(file_stamp(env!("CARGO_MANIFEST_DIR")), None);
        assert!(file_stamp(concat!(env!("CARGO_MANIFEST_DIR"), "\\Cargo.toml")).is_some());
    }

    #[test]
    fn wic_decodes_and_scales_a_small_bmp() {
        // A real WIC round trip on a 4 x 2 24-bit BMP written under the build directory.
        init_apartment();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("tint-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("two-tone.bmp");
        let (w, h) = (4u32, 2u32);
        let row = ((w * 3 + 3) / 4 * 4) as usize;
        let mut pixels = vec![0u8; row * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let c: [u8; 3] = if x < 2 { [0, 0, 255] } else { [255, 0, 0] }; // BGR: red | blue
                pixels[y * row + x * 3..][..3].copy_from_slice(&c);
            }
        }
        let mut file = Vec::new();
        file.extend_from_slice(b"BM");
        file.extend_from_slice(&((54 + pixels.len()) as u32).to_le_bytes());
        file.extend_from_slice(&[0; 4]);
        file.extend_from_slice(&54u32.to_le_bytes());
        file.extend_from_slice(&40u32.to_le_bytes());
        file.extend_from_slice(&(w as i32).to_le_bytes());
        file.extend_from_slice(&(h as i32).to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&24u16.to_le_bytes());
        file.extend_from_slice(&[0; 24]);
        file.extend_from_slice(&pixels);
        std::fs::write(&path, file).unwrap();
        let path = path.to_string_lossy().to_string();

        let wic = WicDecoder::new().expect("WIC factory");
        assert_eq!(wic.dimensions(&path), Ok((4, 2)));
        let out = wic.scaled(&path, (0, 0, 4, 2), 8, 4).unwrap();
        assert_eq!(out.len(), 8 * 4 * 4);
        assert_eq!(&out[..4], &[0, 0, 255, 255], "left half is red (BGRA)");
        assert_eq!(&out[(7 * 4)..(7 * 4) + 4], &[255, 0, 0, 255], "right half is blue");
        // Only the right half: all blue, at its own size.
        let right = wic.scaled(&path, (2, 0, 2, 2), 2, 2).unwrap();
        assert!(right.chunks_exact(4).all(|p| p == [255, 0, 0, 255]), "clipped region: {right:?}");
        assert!(wic.dimensions("Z:\\missing.bmp").is_err());
    }
}
