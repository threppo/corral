//! Per-monitor build with cache keys (wallpaper-blur-spec 6). Pure given a [`Decoder`].
//!
//! S key: (source identity or colour, background colour, mode, canvas rect, virtual rect for Span, k).
//! B key: (S key, sigma_w rounded to 1/16). A blur-step change with the same `k` reuses S and reruns
//! only the blur; a `k` change decodes again.

use super::blur;
use super::level::{Level, Scaled, compose_working};
use super::mapping;
use super::placement::{Mode, RectF64, decode_plan, placement};
use super::source::{Decoder, SourceId, exceeds_limit};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SKey {
    pub source: SourceId,
    pub background: u32,
    pub mode: Mode,
    pub canvas: [i32; 4],
    /// Only Span depends on the other monitors' arrangement.
    pub virtual_rect: Option<[i32; 4]>,
    pub k: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BKey {
    pub s: SKey,
    pub sigma16: u32,
}

/// Everything one monitor's build depends on.
#[derive(Clone, Debug)]
pub struct MonitorInput {
    pub canvas: [i32; 4],
    pub dpi: u32,
    pub source: SourceId,
    pub background: u32,
    pub mode: Mode,
    pub virtual_rect: [i32; 4],
    /// Effective blur radius `R`; 0 builds no blurred copy.
    pub radius: f32,
    /// An unblurred copy at `SHARP_K` is wanted (Camouflage without blur, or a preview).
    pub want_sharp: bool,
}

impl MonitorInput {
    pub fn s_key(&self, k: u32) -> SKey {
        SKey {
            source: self.source.clone(),
            background: self.background,
            mode: self.mode,
            canvas: self.canvas,
            virtual_rect: (self.mode == Mode::Span).then_some(self.virtual_rect),
            k,
        }
    }
}

/// Cached results of one monitor.
#[derive(Default)]
pub struct MonitorCache {
    pub s: Option<(SKey, Arc<Level>)>,
    pub b: Option<(BKey, Arc<Level>)>,
    /// Unblurred copy at `SHARP_K`, kept only when it differs from `s`.
    pub sharp: Option<(SKey, Arc<Level>)>,
}

/// What was built for one monitor.
pub struct Built {
    /// The blurred copy when `radius > 0`.
    pub blurred: Option<Arc<Level>>,
    /// The unblurred copy used for blurring (when `radius > 0`) or sampling.
    pub s: Option<Arc<Level>>,
    /// The unblurred copy at `SHARP_K` (same as `s` when the plan's k is `SHARP_K`).
    pub sharp: Option<Arc<Level>>,
    /// Decodes that ran for this call (the cache tests count them).
    #[allow(dead_code)]
    pub decodes: u32,
    /// A read failed just now and was not cached: ask for another pass soon.
    pub retry: bool,
}

fn canvas_size(c: [i32; 4]) -> (i32, i32) {
    ((c[2] - c[0]).max(1), (c[3] - c[1]).max(1))
}

/// Result of building S for one key.
pub enum Outcome {
    /// A level to cache under the key: the decoded picture, or a flat colour for a picture that can
    /// never be shown (empty or over the size limit), which is as stable as the file itself.
    Level(Arc<Level>, bool),
    /// The file could not be read or decoded just now (locked, half-written). Never cached under the
    /// file's key, so the next pass tries again.
    Transient(String),
}

/// Build the working image S for `key` from its source (wallpaper-blur-spec 3.1 step 3). Returns the
/// outcome and whether a decode ran.
pub fn build_s(dec: &dyn Decoder, key: &SKey) -> Outcome {
    let k = key.k;
    let path = match &key.source {
        SourceId::Solid(c) => return Outcome::Level(Arc::new(Level::solid(*c, k)), false),
        SourceId::File { path, .. } => path,
    };
    let (iw, ih) = match dec.dimensions(path) {
        Ok(d) => d,
        Err(e) => return Outcome::Transient(e),
    };
    if iw == 0 || ih == 0 || exceeds_limit(iw, ih) {
        return Outcome::Level(Arc::new(Level::solid(key.background, k)), false);
    }
    let (cw, ch) = canvas_size(key.canvas);
    let canvas = RectF64::new(key.canvas[0] as f64, key.canvas[1] as f64, cw as f64, ch as f64);
    let virt = key.virtual_rect.map_or(canvas, |v| {
        RectF64::new(v[0] as f64, v[1] as f64, (v[2] - v[0]).max(1) as f64, (v[3] - v[1]).max(1) as f64)
    });
    let p = placement(key.mode, (iw, ih), canvas, virt);
    let plan = decode_plan(key.mode, (iw, ih), canvas, virt, k);
    let (sw, sh) = plan.out;
    let pixels = match dec.scaled(path, plan.region, sw, sh) {
        Ok(p) if p.len() >= sw as usize * sh as usize * 4 => p,
        Ok(_) => return Outcome::Transient("decoder returned too few pixels".into()),
        Err(e) => return Outcome::Transient(e),
    };
    let scaled = Scaled { w: sw as usize, h: sh as usize, bgra: pixels, region: plan.region };
    let size = mapping::working_size(cw, ch, k);
    let bgrx = compose_working(&p, (iw, ih), &scaled, key.background, (cw, ch), k, size);
    Outcome::Level(Arc::new(Level::new(size.0, size.1, k, bgrx, None)), true)
}

/// Blur S into B. A flat colour blurs to itself (wallpaper-blur-spec 3.1).
pub fn build_b(s: &Level, sigma_w: f32) -> Arc<Level> {
    if s.solid.is_some() {
        return Arc::new(Level::new(s.w, s.h, s.k, s.bgrx.clone(), s.solid));
    }
    Arc::new(Level::new(s.w, s.h, s.k, blur::blur_bgrx(&s.bgrx, s.w, s.h, sigma_w), None))
}

/// Bring a monitor's cache up to date for `input`, reusing whatever the keys allow. A transient read
/// failure keeps the previous S and B of the monitor (the flat colour only when there is none) and is
/// reported through [`Built::retry`] instead of being cached.
pub fn build_monitor(
    cache: &mut MonitorCache,
    input: &MonitorInput,
    dec: &dyn Decoder,
    failures: &mut dyn FnMut(&SourceId, &str),
) -> Built {
    let mut decodes = 0;
    let mut retry = false;
    let mut fetch = |slot: &mut Option<(SKey, Arc<Level>)>, key: SKey, decodes: &mut u32, retry: &mut bool| -> (Arc<Level>, bool) {
        if let Some((k, level)) = slot.as_ref() {
            if *k == key {
                return (level.clone(), false);
            }
        }
        match build_s(dec, &key) {
            Outcome::Level(level, decoded) => {
                *decodes += decoded as u32;
                *slot = Some((key, level.clone()));
                (level, false)
            }
            Outcome::Transient(reason) => {
                failures(&key.source, &reason);
                *retry = true;
                match slot.as_ref() {
                    Some((_, previous)) => (previous.clone(), true),
                    None => (Arc::new(Level::solid(key.background, key.k)), true),
                }
            }
        }
    };

    let plan = (input.radius > 0.0).then(|| mapping::plan(input.radius, input.dpi));
    let primary_k = plan.map_or(mapping::SHARP_K, |p| p.k);
    let need_primary = plan.is_some() || input.want_sharp;

    let (s, s_failed) = match need_primary.then(|| fetch(&mut cache.s, input.s_key(primary_k), &mut decodes, &mut retry)) {
        Some((level, failed)) => (Some(level), failed),
        None => {
            cache.s = None;
            (None, false)
        }
    };

    let mut blurred = None;
    if let (Some(plan), Some(s)) = (plan, s.as_ref()) {
        if s_failed {
            // Keep the previous blurred copy; with none, blur whatever S stands in.
            blurred = cache.b.as_ref().map(|(_, l)| l.clone()).or_else(|| {
                let sigma_w = mapping::sigma_px(input.radius, input.dpi) / s.k.max(1) as f32;
                Some(build_b(s, sigma_w))
            });
        } else {
            let key = BKey { s: input.s_key(plan.k), sigma16: mapping::sigma_key(plan.sigma_w) };
            match cache.b.as_ref() {
                Some((k, level)) if *k == key => blurred = Some(level.clone()),
                _ => {
                    let level = build_b(s, plan.sigma_w);
                    cache.b = Some((key, level.clone()));
                    blurred = Some(level);
                }
            }
        }
    } else {
        cache.b = None;
    }

    // The unblurred SHARP_K copy for samples and previews; shares S when the plan already uses SHARP_K.
    let sharp = if !input.want_sharp {
        cache.sharp = None;
        None
    } else if primary_k == mapping::SHARP_K {
        cache.sharp = None;
        s.clone()
    } else {
        Some(fetch(&mut cache.sharp, input.s_key(mapping::SHARP_K), &mut decodes, &mut retry).0)
    };

    Built { blurred, s, sharp, decodes, retry }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::cell::Cell;

    /// A synthetic picture source: every path is a left-dark/right-light gradient of a fixed size.
    pub struct Fake {
        pub size: (u32, u32),
        pub decodes: Cell<u32>,
        /// The header cannot be read (a locked file).
        pub fail: Cell<bool>,
        /// Reading the pixels fails.
        pub fail_pixels: Cell<bool>,
        /// Region and size of the last pixel request.
        pub last: Cell<((u32, u32, u32, u32), u32, u32)>,
    }

    impl Fake {
        pub fn new(w: u32, h: u32) -> Fake {
            Fake { size: (w, h), decodes: Cell::new(0), fail: Cell::new(false), fail_pixels: Cell::new(false), last: Cell::new(((0, 0, 0, 0), 0, 0)) }
        }
    }

    impl Decoder for Fake {
        fn dimensions(&self, _path: &str) -> Result<(u32, u32), String> {
            if self.fail.get() { Err("unreadable".into()) } else { Ok(self.size) }
        }
        fn scaled(&self, _path: &str, region: (u32, u32, u32, u32), w: u32, h: u32) -> Result<Vec<u8>, String> {
            self.decodes.set(self.decodes.get() + 1);
            self.last.set((region, w, h));
            if self.fail_pixels.get() {
                return Err("locked".into());
            }
            Ok((0..w * h).flat_map(|i| {
                let x = (i % w) as f32 / w.max(1) as f32;
                let v = (x * 255.0) as u8;
                [v, v, v, 255]
            }).collect())
        }
    }

    pub fn input() -> MonitorInput {
        MonitorInput {
            canvas: [0, 0, 1920, 1080],
            dpi: 96,
            source: SourceId::File { path: "a.jpg".into(), mtime: 1, size: 2 },
            background: 0x000000,
            mode: Mode::Fill,
            virtual_rect: [0, 0, 1920, 1080],
            radius: 60.0,
            want_sharp: false,
        }
    }

    fn run(cache: &mut MonitorCache, i: &MonitorInput, dec: &Fake) -> Built {
        build_monitor(cache, i, dec, &mut |_, _| {})
    }

    #[test]
    fn identical_inputs_give_identical_keys() {
        let a = input();
        let b = input();
        assert_eq!(a.s_key(8), b.s_key(8));
        assert_eq!(BKey { s: a.s_key(8), sigma16: 120 }, BKey { s: b.s_key(8), sigma16: 120 });
    }

    #[test]
    fn each_trigger_changes_the_right_key() {
        let base = input();
        let k = base.s_key(8);
        let mut changed = |f: &dyn Fn(&mut MonitorInput)| {
            let mut i = base.clone();
            f(&mut i);
            i.s_key(8) != k
        };
        assert!(changed(&|i| i.source = SourceId::File { path: "a.jpg".into(), mtime: 9, size: 2 }), "wallpaper changed on disk");
        assert!(changed(&|i| i.source = SourceId::File { path: "b.jpg".into(), mtime: 1, size: 2 }), "slideshow moved on");
        assert!(changed(&|i| i.background = 0xffffff), "background colour");
        assert!(changed(&|i| i.mode = Mode::Fit), "fit mode");
        assert!(changed(&|i| i.canvas = [0, 0, 2560, 1440]), "resolution");
        assert!(changed(&|i| i.canvas = [10, 0, 1930, 1080]), "monitor moved");
        // Not part of S: radius, DPI (only through k) and the virtual rect outside Span.
        assert!(!changed(&|i| i.radius = 120.0));
        assert!(!changed(&|i| i.dpi = 144));
        assert!(!changed(&|i| i.virtual_rect = [-1920, 0, 1920, 1080]));
        let mut span = base.clone();
        span.mode = Mode::Span;
        let before = span.s_key(8);
        span.virtual_rect = [-1920, 0, 1920, 1080];
        assert_ne!(before, span.s_key(8), "Span depends on the virtual rect");
        assert_ne!(base.s_key(4), base.s_key(8), "k");
    }

    #[test]
    fn a_blur_step_with_the_same_k_reuses_s_and_runs_no_decode() {
        let dec = Fake::new(1600, 900);
        let mut cache = MonitorCache::default();
        let mut i = input();
        i.radius = 60.0; // Medium: k 8
        let first = run(&mut cache, &i, &dec);
        assert_eq!(first.decodes, 1);
        assert_eq!(dec.decodes.get(), 1);
        i.radius = 120.0; // Strong: k 8 as well, different sigma_w
        let second = run(&mut cache, &i, &dec);
        assert_eq!(second.decodes, 0, "same k: no decode");
        assert_eq!(dec.decodes.get(), 1);
        assert!(Arc::ptr_eq(first.s.as_ref().unwrap(), second.s.as_ref().unwrap()), "S reused");
        assert!(!Arc::ptr_eq(first.blurred.as_ref().unwrap(), second.blurred.as_ref().unwrap()), "B rebuilt");
        // The same step again changes nothing.
        let third = run(&mut cache, &i, &dec);
        assert!(Arc::ptr_eq(second.blurred.as_ref().unwrap(), third.blurred.as_ref().unwrap()));
        assert_eq!(dec.decodes.get(), 1);
        // A k change decodes again (Soft is k 4 at 96 dpi).
        i.radius = 30.0;
        let fourth = run(&mut cache, &i, &dec);
        assert_eq!(fourth.decodes, 1);
        assert_eq!(fourth.s.as_ref().unwrap().k, 4);
    }

    #[test]
    fn a_blur_step_changes_only_the_b_key_when_k_stays() {
        let i = input();
        let (a, b) = (mapping::plan(60.0, 96), mapping::plan(120.0, 96));
        assert_eq!(a.k, b.k);
        let key = |p: mapping::Plan| BKey { s: i.s_key(p.k), sigma16: mapping::sigma_key(p.sigma_w) };
        assert_eq!(key(a).s, key(b).s, "S key unchanged");
        assert_ne!(key(a), key(b), "B key follows sigma_w");
        let c = mapping::plan(30.0, 96);
        assert_ne!(key(a).s, BKey { s: i.s_key(c.k), sigma16: 0 }.s, "a k change moves the S key");
    }

    #[test]
    fn dpi_change_moves_k_and_rebuilds_s() {
        let dec = Fake::new(800, 600);
        let mut cache = MonitorCache::default();
        let mut i = input();
        i.radius = 15.0;
        let a = run(&mut cache, &i, &dec);
        assert_eq!(a.s.as_ref().unwrap().k, 2);
        i.dpi = 192;
        let b = run(&mut cache, &i, &dec);
        assert_eq!(b.s.as_ref().unwrap().k, 4);
        assert_eq!(dec.decodes.get(), 2);
    }

    #[test]
    fn radius_zero_builds_nothing_unless_a_sharp_copy_is_wanted() {
        let dec = Fake::new(800, 600);
        let mut cache = MonitorCache::default();
        let mut i = input();
        i.radius = 0.0;
        let none = run(&mut cache, &i, &dec);
        assert!(none.blurred.is_none() && none.s.is_none() && none.sharp.is_none());
        assert_eq!(dec.decodes.get(), 0, "laziness: nothing is decoded");
        i.want_sharp = true;
        let sharp = run(&mut cache, &i, &dec);
        assert!(sharp.blurred.is_none());
        assert_eq!(sharp.sharp.as_ref().unwrap().k, 4);
        assert_eq!((sharp.sharp.as_ref().unwrap().w, sharp.sharp.as_ref().unwrap().h), (480, 270));
    }

    #[test]
    fn sharp_copy_next_to_a_blur_with_another_k_is_separate() {
        let dec = Fake::new(800, 600);
        let mut cache = MonitorCache::default();
        let mut i = input();
        i.radius = 60.0; // k 8
        i.want_sharp = true;
        let built = run(&mut cache, &i, &dec);
        assert_eq!(built.s.as_ref().unwrap().k, 8);
        assert_eq!(built.sharp.as_ref().unwrap().k, 4);
        i.radius = 30.0; // k 4: S doubles as the sharp copy
        let built = run(&mut cache, &i, &dec);
        assert!(Arc::ptr_eq(built.s.as_ref().unwrap(), built.sharp.as_ref().unwrap()));
    }

    #[test]
    fn unreadable_oversized_and_solid_sources_give_flat_colour() {
        let mut fails = Vec::new();
        let mut i = input();
        i.background = 0x00aabbcc;
        let dec = Fake::new(800, 600);
        dec.fail.set(true);
        let mut cache = MonitorCache::default();
        let built = build_monitor(&mut cache, &i, &dec, &mut |s, why| fails.push((s.clone(), why.to_string())));
        assert_eq!(built.s.as_ref().unwrap().solid, Some(0x00aabbcc));
        assert_eq!(built.blurred.as_ref().unwrap().solid, Some(0x00aabbcc));
        assert_eq!(fails.len(), 1);

        // A 129-megapixel header.
        let big = Fake::new(12_900, 10_000);
        let mut cache = MonitorCache::default();
        let built = run(&mut cache, &i, &big);
        assert_eq!(built.s.as_ref().unwrap().solid, Some(0x00aabbcc));
        assert_eq!(big.decodes.get(), 0, "never decoded");

        // A flat colour source blurs to itself within 1 level.
        let mut i = input();
        i.source = SourceId::Solid(0x00336699);
        let mut cache = MonitorCache::default();
        let built = run(&mut cache, &i, &Fake::new(10, 10));
        let b = built.blurred.unwrap();
        assert_eq!(&b.bgrx[..3], &[0x33, 0x66, 0x99], "BGR of COLORREF 0x00336699");
    }

    #[test]
    fn budget_smoke_blur_per_step_on_a_4k_canvas() {
        // wallpaper-blur-spec 7 (logged, not a gate): linearise + blur + encode at each step, 4K at 96 dpi.
        for radius in [15.0f32, 30.0, 60.0, 120.0] {
            let p = mapping::plan(radius, 96);
            let (w, h) = mapping::working_size(3840, 2160, p.k);
            let px: Vec<u8> = (0..w * h).flat_map(|i| [(i * 3) as u8, (i / 5) as u8, (i / w) as u8, 255]).collect();
            let s = Level::new(w, h, p.k, px, None);
            let started = std::time::Instant::now();
            let b = build_b(&s, p.sigma_w);
            eprintln!("blur R {radius}: k {} {}x{} sigma_w {} in {:?}", p.k, w, h, p.sigma_w, started.elapsed());
            assert_eq!(b.bgrx.len(), s.bgrx.len());
        }
    }

    #[test]
    fn a_failed_read_keeps_the_previous_images_and_is_never_cached() {
        let dec = Fake::new(800, 600);
        let mut cache = MonitorCache::default();
        let mut i = input();
        let good = run(&mut cache, &i, &dec);
        assert!(!good.retry);
        // A new picture arrives but cannot be read yet (the file is still locked).
        i.source = SourceId::File { path: "b.jpg".into(), mtime: 5, size: 6 };
        dec.fail_pixels.set(true);
        let busy = run(&mut cache, &i, &dec);
        assert!(busy.retry, "the caller must ask again");
        assert!(Arc::ptr_eq(busy.s.as_ref().unwrap(), good.s.as_ref().unwrap()), "previous S stays");
        assert!(Arc::ptr_eq(busy.blurred.as_ref().unwrap(), good.blurred.as_ref().unwrap()), "previous B stays");
        assert_eq!(cache.s.as_ref().unwrap().0.source, SourceId::File { path: "a.jpg".into(), mtime: 1, size: 2 }, "failure not cached under the new key");
        // The lock is gone: the next pass decodes the new picture.
        dec.fail_pixels.set(false);
        let before = dec.decodes.get();
        let fine = run(&mut cache, &i, &dec);
        assert!(!fine.retry);
        assert_eq!(dec.decodes.get(), before + 1, "it really retried");
        assert!(!Arc::ptr_eq(fine.s.as_ref().unwrap(), good.s.as_ref().unwrap()));
        assert_eq!(cache.s.as_ref().unwrap().0.source, i.source);
    }

    #[test]
    fn a_first_failure_shows_the_colour_but_recovers() {
        let dec = Fake::new(800, 600);
        dec.fail.set(true);
        let mut cache = MonitorCache::default();
        let mut i = input();
        i.background = 0x00112233;
        let first = run(&mut cache, &i, &dec);
        assert!(first.retry);
        assert_eq!(first.s.as_ref().unwrap().solid, Some(0x00112233));
        assert!(cache.s.is_none(), "the colour is not stored under the file's key");
        dec.fail.set(false);
        let second = run(&mut cache, &i, &dec);
        assert!(!second.retry);
        assert_eq!(second.s.as_ref().unwrap().solid, None, "the real picture now");
    }

    #[test]
    fn huge_center_and_tile_pictures_decode_only_what_shows() {
        // 120 megapixels with a tiny radius (k 1): never more than the canvas worth of pixels is requested.
        for mode in [Mode::Center, Mode::Tile, Mode::Fill] {
            let dec = Fake::new(12_000, 10_000);
            let mut cache = MonitorCache::default();
            let mut i = input();
            i.mode = mode;
            i.radius = 1.0;
            i.canvas = [0, 0, 3840, 2160];
            let built = run(&mut cache, &i, &dec);
            assert_eq!(built.s.as_ref().unwrap().k, 1);
            let (region, w, h) = dec.last.get();
            assert!(w as u64 * h as u64 <= 3840 * 2160 + 4000, "{mode:?}: {w}x{h}");
            if matches!(mode, Mode::Center | Mode::Tile) {
                // Native-size modes read no more of the picture than the canvas shows.
                assert!(region.2 as u64 * region.3 as u64 <= 3840 * 2160 + 4000, "{mode:?}: region {region:?}");
            }
        }
    }

    #[test]
    fn blurred_copy_is_smooth_where_s_is_sharp() {
        // A hard black/white edge picture: S keeps the edge, B does not.
        struct Edge;
        impl Decoder for Edge {
            fn dimensions(&self, _: &str) -> Result<(u32, u32), String> { Ok((64, 64)) }
            fn scaled(&self, _: &str, _: (u32, u32, u32, u32), w: u32, h: u32) -> Result<Vec<u8>, String> {
                Ok((0..w * h).flat_map(|i| if (i % w) < w / 2 { [0, 0, 0, 255] } else { [255, 255, 255, 255] }).collect())
            }
        }
        let mut i = input();
        i.canvas = [0, 0, 512, 128];
        i.mode = Mode::Stretch;
        i.radius = 60.0;
        let mut cache = MonitorCache::default();
        let built = build_monitor(&mut cache, &i, &Edge, &mut |_, _| {});
        let (s, b) = (built.s.unwrap(), built.blurred.unwrap());
        let row = |l: &Level, x: usize| l.bgrx[(l.h / 2 * l.w + x) * 4];
        let mid = s.w / 2;
        assert_eq!(row(&s, mid - 1), 0);
        assert_eq!(row(&s, mid), 255);
        assert!(row(&b, mid - 1) > 20 && row(&b, mid - 1) < 235, "B at the edge: {}", row(&b, mid - 1));
    }
}
