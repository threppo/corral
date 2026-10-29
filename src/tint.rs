//! Wallpaper blur (wallpaper-blur-spec): a blurred, icon-free copy of each monitor's wallpaper that
//! Corral bodies draw as frosted glass and Camouflage samples. The picture comes from the wallpaper
//! source Windows reports, never from a screen capture; blur runs on the CPU at a reduced working size
//! on one background worker. This file is the facade the rest of the app calls; the parts are in
//! `tint/`.

mod blur;
mod cache;
mod composite;
mod crossfade;
mod engine;
mod host;
mod level;
mod mapping;
mod monitors;
mod noise;
mod placement;
mod source;

use std::sync::Arc;
use std::time::Duration;

pub use host::live_wallpaper_host;
pub use monitors::primary_rect;

/// A plain pixel image positioned on the virtual screen: the Personalization sample, the Camouflage
/// preview and the tests use these at 1:1 scale.
pub struct Wallpaper {
    /// Virtual-screen origin of the first pixel.
    pub origin: (i32, i32),
    pub width: i32,
    pub height: i32,
    /// Opaque BGRA.
    pub bgra: Vec<u8>,
    /// `Some(COLORREF)` when there is no picture: a flat colour.
    pub solid: Option<u32>,
}

impl Wallpaper {
    /// The pixel at `(x, y)` as `0xAARRGGBB`, or `None` outside the image.
    pub fn pixel(&self, x: i32, y: i32) -> Option<u32> {
        if x < 0 || y < 0 || x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        self.bgra.get(i..i + 4).map(|p| u32::from_le_bytes([p[0], p[1], p[2], p[3]]))
    }

    fn layer(&self) -> composite::Layer<'_> {
        composite::Layer::flat(self.origin, self.width.max(0) as usize, self.height.max(0) as usize, &self.bgra, self.solid)
    }

    fn from_level(origin: (i32, i32), level: &level::Level) -> Wallpaper {
        Wallpaper { origin, width: level.w as i32, height: level.h as i32, bgra: level.bgrx.clone(), solid: level.solid }
    }
}

// ---- settings --------------------------------------------------------------------------------------

/// Blur settings as the engine gates them: the effective radius when the toggle is on and the stored
/// radius is above zero (wallpaper-blur-spec 2).
pub fn blur_gate(enabled: bool, radius: f32) -> Option<f32> {
    let r = mapping::effective_radius(Some(enabled), radius);
    (r > 0.0).then_some(r)
}

/// Set from the loaded settings and from the Blur controls (`BackgroundBlurEnabled`,
/// `BackgroundBlurRadius`); starts or releases the wallpaper work as needed.
pub fn set_blur(enabled: bool, radius: f32) {
    engine::set_radius(mapping::effective_radius(Some(enabled), radius));
}

/// The effective radius when blur is on.
pub fn blur_radius() -> Option<f32> {
    let r = engine::radius();
    (r > 0.0).then_some(r)
}

pub fn blur_enabled() -> bool {
    blur_radius().is_some()
}

/// Content generation of the wallpaper images: moves only when a rebuilt image differs
/// (wallpaper-blur-spec 6). Camouflage keys its palettes on it.
pub fn wallpaper_generation() -> u32 {
    engine::generation()
}

/// The wallpaper may have changed: look again after the debounce. Cheap when nothing is needed or
/// nothing changed (an identical re-read changes nothing).
pub fn invalidate_wallpaper() {
    engine::request(engine::DEBOUNCE_MS);
}

/// The window that receives `WM_READY` (lparam 0) when a new snapshot is ready to draw.
pub fn set_notify_window(hwnd: isize) {
    engine::set_notify_window(hwnd);
}

/// Called once by the settings window process: its sample blurs its own preview picture, so the
/// engine never builds the full blurred copies there.
pub fn use_for_settings_only() {
    engine::set_settings_only();
}

/// Camouflage no longer samples the wallpaper (releases what only it needed).
#[allow(dead_code)]
pub fn set_camouflage_wanted(wanted: bool) {
    engine::set_camouflage_wanted(wanted);
}

/// The Camouflage preview or Personalization sample opened (true) or closed (false).
pub fn set_preview_visible(visible: bool) {
    engine::set_preview_visible(visible);
}

// ---- Camouflage sampling ---------------------------------------------------------------------------

/// What Camouflage samples (wallpaper-blur-spec 8): the blurred copy when the radius is above zero,
/// otherwise the unblurred working copy. Empty until the first snapshot is ready.
pub struct Backdrop {
    snapshot: Option<Arc<engine::Snapshot>>,
}

impl Backdrop {
    /// Spec 8: at most `grid x grid` samples over `rect` (screen pixels), each a linear-light mean of
    /// 4 x 4 taps around the cell centre. Without data a neutral grey comes back and the next ready
    /// snapshot moves the generation.
    pub fn sample_rect(&self, rect: [i32; 4], grid: usize) -> Vec<crate::camouflage::Rgb> {
        match &self.snapshot {
            Some(snapshot) => composite::sample_rect(&snapshot.sample_layers(), rect, grid),
            None => vec![[128; 3]],
        }
    }
}

/// The wallpaper source for Camouflage. Asking marks Camouflage as a user of the wallpaper.
pub fn camouflage_source() -> Backdrop {
    engine::set_camouflage_wanted(true);
    backdrop_source()
}

/// A scrollbar starts (true) or stops (false) sampling the wallpaper behind its Corral; the engine
/// keeps the unblurred copy only while someone needs it.
pub fn set_scrollbar_user(id: u64, wanted: bool) {
    engine::set_scrollbar_user(id, wanted);
}

/// The current wallpaper source without marking a user; callers register their need first
/// ([`set_scrollbar_user`], [`set_blur`]).
pub fn backdrop_source() -> Backdrop {
    let snapshot = engine::current();
    if snapshot.is_none() {
        engine::ensure();
    }
    Backdrop { snapshot }
}

// ---- drawing ---------------------------------------------------------------------------------------

/// Draw the frosted backdrop under an already painted Corral surface: B for the outer rect at opacity
/// `fade`, then the noise at the same `fade` (wallpaper-blur-spec 8). `origin` is the surface's
/// top-left in virtual-screen physical pixels; `mask` carries the rounded-shape coverage in alpha.
/// The caller skips this in high contrast.
pub fn composite_blurred_backdrop(pixels: &mut [u8], mask: &[u8], w: usize, h: usize, origin: (i32, i32), fade: u8, strength: f64) {
    if !blur_enabled() || strength <= 0.0 || fade == 0 {
        return;
    }
    let drawn = engine::with_blur_blend(|blend| composite::composite_under(pixels, mask, w, h, origin, fade, strength, blend, true));
    if drawn.is_none() {
        engine::ensure();
    }
}

/// As [`composite_blurred_backdrop`] over a plain image (the Personalization sample draws the same
/// backdrop, noise included).
pub(crate) fn composite_backdrop_from(pixels: &mut [u8], mask: &[u8], w: usize, h: usize, origin: (i32, i32), fade: u8, strength: f64, wallpaper: &Wallpaper) {
    let layers = [wallpaper.layer()];
    composite::composite_under(pixels, mask, w, h, origin, fade, strength, &composite::Blend::new(&layers), true);
}

// ---- Personalization sample and Camouflage preview --------------------------------------------------

/// How long the settings window waits, once, for the first picture of the primary monitor. Later
/// calls only look; the engine posts a repaint when the picture is ready.
const PREVIEW_WAIT: Duration = Duration::from_millis(300);

/// The primary monitor's unblurred picture at the preview working scale (a quarter of the canvas),
/// without icons or windows. `None` when it cannot be built in time.
pub fn primary_preview() -> Option<Wallpaper> {
    static WAITED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    engine::set_preview_visible(true);
    let wait = if WAITED.swap(true, std::sync::atomic::Ordering::Relaxed) { Duration::ZERO } else { PREVIEW_WAIT };
    let snapshot = engine::wait_for(wait, &|s| s.monitors.iter().any(|m| m.primary && m.sharp.is_some()))?;
    let monitor = snapshot.monitors.iter().find(|m| m.primary && m.sharp.is_some())?;
    let level = monitor.sharp.as_ref()?;
    Some(Wallpaper::from_level((monitor.rect[0], monitor.rect[1]), level))
}

/// Blur a preview-scale picture of the primary monitor as the real wallpaper would be blurred: the
/// monitor's sigma in pixels, scaled by the picture's scale (wallpaper-blur-spec 8). A flat colour
/// blurs to itself.
pub fn sample_blur(sharp: &Wallpaper, radius: f32) -> Option<Wallpaper> {
    if sharp.solid.is_some() || sharp.width <= 0 || sharp.height <= 0 || radius <= 0.0 {
        return None;
    }
    let (canvas_w, dpi) = monitors::enumerate()
        .iter()
        .find(|m| m.primary)
        .map_or((sharp.width, 96), |m| (m.width().max(1), m.dpi));
    let scale = (sharp.width as f32 / canvas_w as f32).min(1.0);
    let sigma_w = mapping::sigma_px(radius, dpi) * scale;
    let bgra = blur::blur_bgrx(&sharp.bgra, sharp.width as usize, sharp.height as usize, sigma_w);
    Some(Wallpaper { origin: sharp.origin, width: sharp.width, height: sharp.height, bgra, solid: None })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_gate_needs_toggle_and_positive_radius() {
        assert_eq!(blur_gate(true, 60.0), Some(60.0));
        assert_eq!(blur_gate(false, 60.0), None);
        assert_eq!(blur_gate(true, 0.0), None);
        assert_eq!(blur_gate(true, f32::NAN), None);
        assert_eq!(blur_gate(true, 300.0), Some(255.0));
    }

    #[test]
    fn pixel_reads_argb_and_stays_inside() {
        let w = Wallpaper { origin: (0, 0), width: 2, height: 1, bgra: vec![1, 2, 3, 255, 4, 5, 6, 255], solid: None };
        assert_eq!(w.pixel(1, 0), Some(0xff06_0504));
        assert_eq!(w.pixel(2, 0), None);
        assert_eq!(w.pixel(-1, 0), None);
        assert_eq!(w.pixel(0, 1), None);
    }

    #[test]
    fn sample_blur_keeps_flat_colour_and_smooths_an_edge() {
        let flat = Wallpaper { origin: (0, 0), width: 1, height: 1, bgra: vec![9, 9, 9, 255], solid: Some(0x090909) };
        assert!(sample_blur(&flat, 60.0).is_none());
        let (w, h) = (64usize, 8usize);
        let bgra: Vec<u8> = (0..w * h).flat_map(|i| if i % w < w / 2 { [0, 0, 0, 255] } else { [255, 255, 255, 255] }).collect();
        let edge = Wallpaper { origin: (0, 0), width: w as i32, height: h as i32, bgra, solid: None };
        let blurred = sample_blur(&edge, 120.0).unwrap();
        let at = |x: i32| blurred.pixel(x, 4).unwrap() & 255;
        assert!(at(31) > 20 && at(31) < 235, "edge softened: {}", at(31));
        assert!(at(0) <= at(31) && at(31) <= at(32) && at(32) <= at(63), "monotone across the edge");
        assert!(sample_blur(&edge, 0.0).is_none());
    }

    #[test]
    fn backdrop_without_data_is_neutral_grey() {
        let b = Backdrop { snapshot: None };
        assert_eq!(b.sample_rect([0, 0, 100, 100], 8), vec![[128; 3]]);
    }

    #[test]
    fn plain_images_draw_as_a_backdrop() {
        let w = Wallpaper { origin: (0, 0), width: 2, height: 2, bgra: vec![60; 16], solid: None };
        let mut pixels = vec![0u8; 16];
        let mask = vec![255u8; 16];
        composite_backdrop_from(&mut pixels, &mask, 2, 2, (0, 0), 255, 1.0, &w);
        assert!(pixels.chunks_exact(4).all(|p| (54..=66).contains(&p[0]) && p[3] == 255), "{pixels:?}");
    }
}
