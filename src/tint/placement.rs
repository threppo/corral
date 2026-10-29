//! Where the wallpaper picture lands on one monitor's canvas, per fit mode (wallpaper-blur-spec 3.2).
//! Pure: no Windows types.

/// A rectangle in pixels (floating point so scaled placements stay exact).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RectF64 {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl RectF64 {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }
    fn intersect(&self, o: &RectF64) -> Option<RectF64> {
        let (l, t) = (self.x.max(o.x), self.y.max(o.y));
        let (r, b) = (self.right().min(o.right()), self.bottom().min(o.bottom()));
        (r > l && b > t).then(|| RectF64::new(l, t, r - l, b - t))
    }
}

/// The desktop's single fit mode (`IDesktopWallpaper::GetPosition`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Center,
    Tile,
    Stretch,
    Fit,
    Fill,
    Span,
}

impl Mode {
    /// From the numeric `DESKTOP_WALLPAPER_POSITION`; unknown values count as Fill.
    pub fn from_position(value: i32) -> Mode {
        match value {
            0 => Mode::Center,
            1 => Mode::Tile,
            2 => Mode::Stretch,
            3 => Mode::Fit,
            5 => Mode::Span,
            _ => Mode::Fill,
        }
    }
}

/// Result of [`placement`]: the part of the image that shows (`src`, image pixels) and where it lands
/// (`dest`, canvas pixels with the canvas top-left at 0,0). Outside `dest` the canvas shows the
/// background colour. `tiled` means the image repeats from the canvas top-left and `src`/`dest` are
/// the whole image and the whole canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub src: RectF64,
    pub dest: RectF64,
    pub tiled: bool,
}

impl Placement {
    /// Image coordinate under canvas coordinate `(cx, cy)`, or `None` where the background shows.
    pub fn image_at(&self, cx: f64, cy: f64, image: (u32, u32)) -> Option<(f64, f64)> {
        if self.tiled {
            let (iw, ih) = (image.0.max(1) as f64, image.1.max(1) as f64);
            return Some((cx.rem_euclid(iw), cy.rem_euclid(ih)));
        }
        let d = &self.dest;
        if cx < d.x || cy < d.y || cx >= d.right() || cy >= d.bottom() || d.w <= 0.0 || d.h <= 0.0 {
            return None;
        }
        Some((self.src.x + (cx - d.x) * self.src.w / d.w, self.src.y + (cy - d.y) * self.src.h / d.h))
    }
}

/// Placement of an `image` (w, h) on a canvas (wallpaper-blur-spec 3.2).
///
/// `canvas` is the monitor's rectangle in virtual-screen physical pixels and `virtual_rect` the bounding
/// rectangle of all monitors; both matter only for Span, where one Fill-style placement covers the
/// virtual rectangle and each monitor takes its own crop.
pub fn placement(mode: Mode, image: (u32, u32), canvas: RectF64, virtual_rect: RectF64) -> Placement {
    let (iw, ih) = (image.0.max(1) as f64, image.1.max(1) as f64);
    let whole = RectF64::new(0.0, 0.0, iw, ih);
    let local = RectF64::new(0.0, 0.0, canvas.w, canvas.h);
    match mode {
        Mode::Stretch => Placement { src: whole, dest: local, tiled: false },
        Mode::Tile => Placement { src: whole, dest: local, tiled: true },
        Mode::Fill => {
            let s = (canvas.w / iw).max(canvas.h / ih);
            let (sw, sh) = (canvas.w / s, canvas.h / s);
            Placement { src: RectF64::new((iw - sw) / 2.0, (ih - sh) / 2.0, sw, sh), dest: local, tiled: false }
        }
        Mode::Fit => {
            let s = (canvas.w / iw).min(canvas.h / ih);
            let (dw, dh) = (iw * s, ih * s);
            Placement {
                src: whole,
                dest: RectF64::new((canvas.w - dw) / 2.0, (canvas.h - dh) / 2.0, dw, dh),
                tiled: false,
            }
        }
        Mode::Center => {
            let dest = RectF64::new((canvas.w - iw) / 2.0, (canvas.h - ih) / 2.0, iw, ih);
            clip_to_canvas(dest, 1.0, local)
        }
        Mode::Span => {
            let v = virtual_rect;
            let s = (v.w / iw).max(v.h / ih);
            let (sw, sh) = (v.w / s, v.h / s);
            // The cover crop of the whole virtual rectangle, in image pixels.
            let (cx, cy) = ((iw - sw) / 2.0, (ih - sh) / 2.0);
            // This monitor's part of it.
            let src = RectF64::new(cx + (canvas.x - v.x) / s, cy + (canvas.y - v.y) / s, canvas.w / s, canvas.h / s);
            Placement { src, dest: local, tiled: false }
        }
    }
}

/// Clip a native-size placement (`scale` canvas pixels per image pixel) to the canvas.
fn clip_to_canvas(dest: RectF64, scale: f64, canvas: RectF64) -> Placement {
    match dest.intersect(&canvas) {
        Some(visible) => Placement {
            src: RectF64::new((visible.x - dest.x) / scale, (visible.y - dest.y) / scale, visible.w / scale, visible.h / scale),
            dest: visible,
            tiled: false,
        },
        None => Placement { src: RectF64::new(0.0, 0.0, 0.0, 0.0), dest: RectF64::new(0.0, 0.0, 0.0, 0.0), tiled: false },
    }
}

/// What to decode: the part of the picture that can show on the canvas (`region`, whole image pixels
/// `x, y, w, h`) and the size to scale it to so one pixel of the result is one working pixel. Only the
/// visible part is ever decoded, so a very large Center, Tile, Fill or Span picture never exists at
/// full size in memory (wallpaper-blur-spec 3.1). `k` is the working divisor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodePlan {
    pub region: (u32, u32, u32, u32),
    pub out: (u32, u32),
}

pub fn decode_plan(mode: Mode, image: (u32, u32), canvas: RectF64, virtual_rect: RectF64, k: u32) -> DecodePlan {
    let p = placement(mode, image, canvas, virtual_rect);
    let k = k.max(1) as f64;
    let (iw, ih) = (image.0.max(1), image.1.max(1));
    // Visible source rectangle in image pixels and the canvas pixels per image pixel.
    let (src, sx, sy) = if p.tiled {
        // Tiles repeat from the canvas top-left: only min(image, canvas) per axis is ever seen.
        let w = (iw as f64).min(canvas.w.ceil().max(1.0));
        let h = (ih as f64).min(canvas.h.ceil().max(1.0));
        (RectF64::new(0.0, 0.0, w, h), 1.0, 1.0)
    } else if p.src.w > 0.0 && p.src.h > 0.0 && p.dest.w > 0.0 && p.dest.h > 0.0 {
        (p.src, p.dest.w / p.src.w, p.dest.h / p.src.h)
    } else {
        (RectF64::new(0.0, 0.0, iw as f64, ih as f64), 1.0, 1.0)
    };
    // Round outward to whole pixels, inside the image.
    let x0 = (src.x.floor().max(0.0) as u32).min(iw - 1);
    let y0 = (src.y.floor().max(0.0) as u32).min(ih - 1);
    let x1 = (src.right().ceil().min(iw as f64) as u32).max(x0 + 1);
    let y1 = (src.bottom().ceil().min(ih as f64) as u32).max(y0 + 1);
    let (rw, rh) = (x1 - x0, y1 - y0);
    let f = |len: u32, s: f64| ((len as f64 * s / k).round().max(1.0)).min(u32::MAX as f64 / 2.0) as u32;
    DecodePlan { region: (x0, y0, rw, rh), out: (f(rw, sx), f(rh, sy)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;
    fn canvas(w: f64, h: f64) -> RectF64 {
        RectF64::new(0.0, 0.0, w, h)
    }
    fn close(a: RectF64, b: RectF64) -> bool {
        (a.x - b.x).abs() < EPS && (a.y - b.y).abs() < EPS && (a.w - b.w).abs() < EPS && (a.h - b.h).abs() < EPS
    }

    const IMAGES: [(u32, u32); 2] = [(1920, 1080), (1600, 1200)];
    const CANVASES: [(f64, f64); 4] = [(1920.0, 1200.0), (3440.0, 1440.0), (1080.0, 1920.0), (2560.0, 1440.0)];

    #[test]
    fn fill_covers_the_canvas_with_a_centred_crop_and_keeps_the_aspect() {
        for image in IMAGES {
            for (w, h) in CANVASES {
                let p = placement(Mode::Fill, image, canvas(w, h), canvas(w, h));
                assert!(close(p.dest, canvas(w, h)));
                assert!(p.src.x >= -EPS && p.src.y >= -EPS);
                assert!(p.src.right() <= image.0 as f64 + EPS && p.src.bottom() <= image.1 as f64 + EPS);
                assert!((p.src.w / p.src.h - w / h).abs() < 1e-9, "aspect");
                // Centred and touching at least one pair of edges.
                assert!((p.src.x - (image.0 as f64 - p.src.right())).abs() < 1e-6);
                assert!((p.src.y - (image.1 as f64 - p.src.bottom())).abs() < 1e-6);
                assert!(p.src.x.abs() < 1e-6 || p.src.y.abs() < 1e-6);
            }
        }
    }

    #[test]
    fn fit_contains_the_image_with_bars_and_keeps_the_aspect() {
        for image in IMAGES {
            for (w, h) in CANVASES {
                let p = placement(Mode::Fit, image, canvas(w, h), canvas(w, h));
                assert_eq!(p.src, RectF64::new(0.0, 0.0, image.0 as f64, image.1 as f64));
                assert!(p.dest.x >= -EPS && p.dest.y >= -EPS && p.dest.right() <= w + EPS && p.dest.bottom() <= h + EPS);
                assert!((p.dest.w / p.dest.h - image.0 as f64 / image.1 as f64).abs() < 1e-9);
                assert!((p.dest.x - (w - p.dest.right())).abs() < 1e-6 && (p.dest.y - (h - p.dest.bottom())).abs() < 1e-6);
                assert!((p.dest.w - w).abs() < 1e-6 || (p.dest.h - h).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn stretch_maps_the_whole_image_to_the_whole_canvas() {
        let p = placement(Mode::Stretch, (1600, 1200), canvas(3440.0, 1440.0), canvas(3440.0, 1440.0));
        assert_eq!(p.src, RectF64::new(0.0, 0.0, 1600.0, 1200.0));
        assert_eq!(p.dest, canvas(3440.0, 1440.0));
    }

    #[test]
    fn center_is_native_size_centred_and_clipped() {
        let p = placement(Mode::Center, (800, 600), canvas(1920.0, 1080.0), canvas(1920.0, 1080.0));
        assert_eq!(p.dest, RectF64::new(560.0, 240.0, 800.0, 600.0));
        assert_eq!(p.src, RectF64::new(0.0, 0.0, 800.0, 600.0));
        // Larger than the canvas: the middle of the image shows.
        let p = placement(Mode::Center, (3000, 2000), canvas(1920.0, 1080.0), canvas(1920.0, 1080.0));
        assert_eq!(p.dest, canvas(1920.0, 1080.0));
        assert_eq!(p.src, RectF64::new(540.0, 460.0, 1920.0, 1080.0));
    }

    #[test]
    fn tile_repeats_from_the_canvas_top_left() {
        let p = placement(Mode::Tile, (100, 50), canvas(1920.0, 1080.0), canvas(1920.0, 1080.0));
        assert!(p.tiled);
        assert_eq!(p.image_at(0.0, 0.0, (100, 50)), Some((0.0, 0.0)));
        assert_eq!(p.image_at(250.0, 120.0, (100, 50)), Some((50.0, 20.0)));
    }

    #[test]
    fn image_at_maps_through_dest_and_src_and_leaves_bars_empty() {
        let p = placement(Mode::Fit, (1600, 1200), canvas(1920.0, 1080.0), canvas(1920.0, 1080.0));
        assert_eq!(p.image_at(10.0, 500.0, (1600, 1200)), None, "left bar");
        let (u, v) = p.image_at(960.0, 540.0, (1600, 1200)).unwrap();
        assert!((u - 800.0).abs() < 1e-6 && (v - 600.0).abs() < 1e-6);
    }

    #[test]
    fn span_crops_abut_exactly_and_union_equals_the_cover_placement() {
        // Monitors at (0,0), (-1920,-200) and (0,-1080), differently sized.
        let monitors = [
            RectF64::new(0.0, 0.0, 1920.0, 1080.0),
            RectF64::new(-1920.0, -200.0, 1920.0, 1200.0),
            RectF64::new(0.0, -1080.0, 2560.0, 1080.0),
        ];
        let left = monitors.iter().map(|m| m.x).fold(f64::MAX, f64::min);
        let top = monitors.iter().map(|m| m.y).fold(f64::MAX, f64::min);
        let right = monitors.iter().map(|m| m.right()).fold(f64::MIN, f64::max);
        let bottom = monitors.iter().map(|m| m.bottom()).fold(f64::MIN, f64::max);
        let virt = RectF64::new(left, top, right - left, bottom - top);
        for image in IMAGES {
            let cover = placement(Mode::Fill, image, canvas(virt.w, virt.h), canvas(virt.w, virt.h)).src;
            let crops: Vec<RectF64> = monitors.iter().map(|m| placement(Mode::Span, image, *m, virt).src).collect();
            // Each crop equals the cover crop's matching sub-rectangle.
            let s = cover.w / virt.w;
            for (m, c) in monitors.iter().zip(&crops) {
                let want = RectF64::new(cover.x + (m.x - virt.x) * s, cover.y + (m.y - virt.y) * s, m.w * s, m.h * s);
                assert!(close(*c, want), "{c:?} vs {want:?}");
            }
            // Neighbours abut: monitor 1 ends where monitor 0 starts, monitor 2 ends where monitor 0 starts.
            assert!((crops[1].right() - crops[0].x).abs() < 1e-6);
            assert!((crops[2].bottom() - (cover.y + (0.0 - virt.y) * s)).abs() < 1e-6);
            // Union of the crops stays inside the cover crop and touches its corners.
            let ux = crops.iter().map(|c| c.x).fold(f64::MAX, f64::min);
            let uy = crops.iter().map(|c| c.y).fold(f64::MAX, f64::min);
            let ur = crops.iter().map(|c| c.right()).fold(f64::MIN, f64::max);
            let ub = crops.iter().map(|c| c.bottom()).fold(f64::MIN, f64::max);
            assert!(close(RectF64::new(ux, uy, ur - ux, ub - uy), cover), "union equals the cover placement");
        }
    }

    #[test]
    fn decode_plan_lands_on_the_working_canvas() {
        // Fill of 4:3 on a 16:9 canvas at k 4: the visible band of the picture, scaled to 960 x 540.
        let c = canvas(3840.0, 2160.0);
        let plan = decode_plan(Mode::Fill, (1600, 1200), c, c, 4);
        assert_eq!(plan.region, (0, 150, 1600, 900));
        assert_eq!(plan.out, (960, 540));
        assert_eq!(decode_plan(Mode::Stretch, (1600, 1200), c, c, 4).out, (960, 540));
        assert_eq!(decode_plan(Mode::Stretch, (1600, 1200), c, c, 4).region, (0, 0, 1600, 1200));
        let center = decode_plan(Mode::Center, (800, 600), c, c, 4);
        assert_eq!((center.region, center.out), ((0, 0, 800, 600), (200, 150)));
    }

    #[test]
    fn huge_pictures_are_decoded_only_where_they_show() {
        // 120 megapixels at k 1 on a 4K canvas: never more than the canvas worth of pixels.
        let c = canvas(3840.0, 2160.0);
        let image = (12_000, 10_000);
        for mode in [Mode::Center, Mode::Tile, Mode::Fill, Mode::Fit, Mode::Stretch] {
            let plan = decode_plan(mode, image, c, c, 1);
            assert!(plan.out.0 as u64 * plan.out.1 as u64 <= 3840 * 2160 + 4000, "{mode:?}: {:?}", plan.out);
            assert!(plan.region.2 <= image.0 && plan.region.3 <= image.1);
        }
        let center = decode_plan(Mode::Center, image, c, c, 1);
        assert_eq!(center.region, (4080, 3920, 3840, 2160), "the middle of the picture");
        let tile = decode_plan(Mode::Tile, image, c, c, 1);
        assert_eq!(tile.region, (0, 0, 3840, 2160), "top-left canvas worth");
        // A small tile is decoded whole.
        assert_eq!(decode_plan(Mode::Tile, (100, 50), c, c, 1).region, (0, 0, 100, 50));
    }

    #[test]
    fn span_decodes_only_the_monitors_crop() {
        let virt = RectF64::new(-1920.0, 0.0, 3840.0, 1080.0);
        let right = RectF64::new(0.0, 0.0, 1920.0, 1080.0);
        let plan = decode_plan(Mode::Span, (7680, 2160), right, virt, 2);
        assert_eq!(plan.region, (3840, 0, 3840, 2160));
        assert_eq!(plan.out, (960, 540));
    }

    #[test]
    fn from_position_covers_every_value() {
        assert_eq!(Mode::from_position(0), Mode::Center);
        assert_eq!(Mode::from_position(1), Mode::Tile);
        assert_eq!(Mode::from_position(2), Mode::Stretch);
        assert_eq!(Mode::from_position(3), Mode::Fit);
        assert_eq!(Mode::from_position(4), Mode::Fill);
        assert_eq!(Mode::from_position(5), Mode::Span);
        assert_eq!(Mode::from_position(99), Mode::Fill);
    }
}
