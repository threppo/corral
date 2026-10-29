//! Drawing and sampling the working-size images (wallpaper-blur-spec 4, 5, 8). Pure: layers are plain
//! borrowed pixel buffers, so it runs without a window.

use super::blur::{decode_level, encode_level};
use super::monitors::monitor_index_for;
use super::noise;

/// Two neighbouring image indices along one axis and the weight of the second.
type Axis = (usize, usize, f32);

/// One monitor's working image positioned on the virtual screen.
#[derive(Clone, Copy, Debug)]
pub struct Layer<'a> {
    /// Monitor canvas `[left, top, right, bottom]` in virtual-screen physical pixels.
    pub rect: [i32; 4],
    /// Working divisor: one image pixel covers `k` canvas pixels.
    pub k: u32,
    pub w: usize,
    pub h: usize,
    /// BGRX.
    pub px: &'a [u8],
    pub solid: Option<u32>,
}

impl<'a> Layer<'a> {
    /// A 1:1 pixel layer (previews and tests).
    pub fn flat(origin: (i32, i32), w: usize, h: usize, px: &'a [u8], solid: Option<u32>) -> Layer<'a> {
        Layer { rect: [origin.0, origin.1, origin.0 + w as i32, origin.1 + h as i32], k: 1, w, h, px, solid }
    }

    fn texel(&self, x: usize, y: usize) -> &[u8] {
        &self.px[(y * self.w + x) * 4..][..4]
    }

    /// Image-space coordinate of the continuous screen coordinate `s` along one axis.
    fn map(&self, s: f32, origin: i32, len: usize) -> Axis {
        let u = ((s - origin as f32) / self.k as f32 - 0.5).clamp(0.0, (len - 1) as f32);
        let i0 = u.floor() as usize;
        (i0, (i0 + 1).min(len - 1), u - i0 as f32)
    }

    /// Bilinear colour (RGB, sRGB levels 0..255) at the continuous screen position `(sx, sy)`, where a
    /// pixel centre is `x + 0.5` (`D2D1_INTERPOLATION_MODE_LINEAR` behaviour; wallpaper-blur-spec 4).
    pub fn bilinear(&self, sx: f32, sy: f32) -> [f32; 3] {
        if let Some(c) = self.solid {
            return [(c & 255) as f32, ((c >> 8) & 255) as f32, ((c >> 16) & 255) as f32];
        }
        if self.w == 0 || self.h == 0 || self.px.len() < self.w * self.h * 4 {
            return [128.0; 3];
        }
        self.at_mapped(self.map(sx, self.rect[0], self.w), self.map(sy, self.rect[1], self.h))
    }

    fn valid(&self) -> bool {
        self.solid.is_none() && self.w > 0 && self.h > 0 && self.px.len() >= self.w * self.h * 4
    }

    /// Bilinear colour from already mapped axes (the drawing loop maps each column and row once).
    fn at_mapped(&self, (x0, x1, fx): Axis, (y0, y1, fy): Axis) -> [f32; 3] {
        let (a, b, c, d) = (self.texel(x0, y0), self.texel(x1, y0), self.texel(x0, y1), self.texel(x1, y1));
        let mut out = [0.0; 3];
        for (i, channel) in [2usize, 1, 0].into_iter().enumerate() {
            let top = a[channel] as f32 + (b[channel] as f32 - a[channel] as f32) * fx;
            let bottom = c[channel] as f32 + (d[channel] as f32 - c[channel] as f32) * fx;
            out[i] = top + (bottom - top) * fy;
        }
        out
    }

    /// The same bilinear read decoded to linear light (RGB, 0..1). Interpolating the sRGB levels first
    /// matches what the drawing shows on screen, so Camouflage samples what the user sees.
    fn linear(&self, sx: f32, sy: f32) -> [f32; 3] {
        self.bilinear(sx, sy).map(decode_fraction)
    }
}

/// Linear light of a fractional sRGB level (0..255), interpolating the table between whole levels.
fn decode_fraction(level: f32) -> f32 {
    let v = level.clamp(0.0, 255.0);
    let i = (v.floor() as usize).min(254);
    let f = v - i as f32;
    decode_level(i as u8) + (decode_level(i as u8 + 1) - decode_level(i as u8)) * f
}

/// The layers to read and, during a wallpaper cross-fade, the layers being faded away from and the
/// weight of the new ones.
#[derive(Clone, Copy, Debug)]
pub struct Blend<'a> {
    pub layers: &'a [Layer<'a>],
    pub from: Option<(&'a [Layer<'a>], f32)>,
}

impl<'a> Blend<'a> {
    pub fn new(layers: &'a [Layer<'a>]) -> Blend<'a> {
        Blend { layers, from: None }
    }

    fn rects(&self) -> Vec<[i32; 4]> {
        self.layers.iter().map(|l| l.rect).collect()
    }

    /// Colour under screen pixel `(x, y)`: the layer of its monitor, mixed with the outgoing one.
    fn colour(&self, index: usize, sx: f32, sy: f32) -> [f32; 3] {
        let layer = &self.layers[index];
        let new = layer.bilinear(sx, sy);
        match self.from {
            Some((old, weight)) if weight < 1.0 => match old.iter().find(|l| l.rect == layer.rect) {
                Some(o) => {
                    let old = o.bilinear(sx, sy);
                    [0, 1, 2].map(|i| old[i] + (new[i] - old[i]) * weight)
                }
                None => new,
            },
            _ => new,
        }
    }
}

/// Draw the backdrop under an already painted Corral surface (premultiplied BGRA `pixels`, `w x h`,
/// whose top-left sits at screen position `origin`): the blurred wallpaper at opacity
/// `coverage x fade x strength`, then the noise at the same opacity (wallpaper-blur-spec 5, 8). The
/// surface's own pixels end up on top, so the body fill sits over the frosted backdrop.
/// `mask` is the rounded-shape coverage in its alpha byte, same size as `pixels`.
pub fn composite_under(pixels: &mut [u8], mask: &[u8], w: usize, h: usize, origin: (i32, i32), fade: u8, strength: f64, blend: &Blend, with_noise: bool) {
    if w == 0 || h == 0 || blend.layers.is_empty() || fade == 0 || strength <= 0.0 {
        return;
    }
    if pixels.len() < w * h * 4 || mask.len() < w * h * 4 {
        return;
    }
    let strength = strength.clamp(0.0, 1.0) as f32;
    let fade = fade as f32 / 255.0;
    let rects = blend.rects();
    let texels = noise::table();
    let mut last = 0usize;
    // Without a cross-fade every column and row is mapped once per layer instead of once per pixel.
    let fast = blend.from.is_none();
    let mut columns: Vec<Option<Vec<Axis>>> = vec![None; blend.layers.len()];
    let mut rows: Vec<Option<Axis>> = vec![None; blend.layers.len()];
    for y in 0..h {
        let sy = origin.1 + y as i32;
        rows.iter_mut().for_each(|r| *r = None);
        for x in 0..w {
            let i = (y * w + x) * 4;
            let coverage = mask[i + 3];
            let below = pixels[i + 3];
            if coverage == 0 || below == 255 {
                continue;
            }
            let sx = origin.0 + x as i32;
            let r = &rects[last];
            if !(sx >= r[0] && sx < r[2] && sy >= r[1] && sy < r[3]) {
                last = monitor_index_for(&rects, sx, sy).unwrap_or(0);
            }
            let layer = &blend.layers[last];
            let colour = if fast && layer.valid() {
                let cols = columns[last].get_or_insert_with(|| {
                    (0..w).map(|c| layer.map((origin.0 + c as i32) as f32 + 0.5, layer.rect[0], layer.w)).collect()
                });
                let ym = *rows[last].get_or_insert_with(|| layer.map(sy as f32 + 0.5, layer.rect[1], layer.h));
                layer.at_mapped(cols[x], ym)
            } else {
                blend.colour(last, sx as f32 + 0.5, sy as f32 + 0.5)
            };
            let a = coverage as f32 / 255.0 * fade * strength;
            let n = if with_noise {
                let r = &rects[last];
                let t = texels[noise::texel_index(sx, sy, r[0], r[1])];
                (t.white, t.alpha as f32 / 255.0 * a)
            } else {
                (false, 0.0)
            };
            let keep = 1.0 - n.1;
            let white = if n.0 { n.1 } else { 0.0 };
            let alpha = n.1 + a * keep;
            let hole = 1.0 - below as f32 / 255.0;
            // BGRA premultiplied: backdrop (premultiplied) goes under the existing pixel.
            for (slot, channel) in [(0usize, 2usize), (1, 1), (2, 0)] {
                let backdrop = colour[channel] / 255.0 * a * keep + white;
                let v = pixels[i + slot] as f32 + backdrop * 255.0 * hole;
                pixels[i + slot] = (v + 0.5).min(255.0) as u8;
            }
            pixels[i + 3] = (below as f32 + alpha * 255.0 * hole + 0.5).min(255.0) as u8;
        }
    }
}

/// Spec 8: at most `grid x grid` cell samples over `rect` (screen pixels), each the linear-light mean
/// of up to 4 x 4 taps read from the working pixels around the cell centre with bilinear filtering.
/// A rect partly outside the monitors is clipped to their bounding box; one fully outside takes the
/// nearest pixel. Monitor origins are subtracted by the layers themselves. Noise is never included.
pub fn sample_rect(layers: &[Layer], rect: [i32; 4], grid: usize) -> Vec<[u8; 3]> {
    if layers.is_empty() {
        return vec![[128; 3]];
    }
    if let Some(c) = layers.first().and_then(|l| l.solid).filter(|_| layers.iter().all(|l| l.solid == layers[0].solid)) {
        return vec![[c as u8, (c >> 8) as u8, (c >> 16) as u8]];
    }
    let bounds = layers.iter().fold(layers[0].rect, |a, l| {
        [a[0].min(l.rect[0]), a[1].min(l.rect[1]), a[2].max(l.rect[2]), a[3].max(l.rect[3])]
    });
    let rects: Vec<[i32; 4]> = layers.iter().map(|l| l.rect).collect();
    let (mut left, mut top, mut right, mut bottom) =
        (rect[0].max(bounds[0]) as i64, rect[1].max(bounds[1]) as i64, rect[2].min(bounds[2]) as i64, rect[3].min(bounds[3]) as i64);
    if left >= right || top >= bottom {
        left = (((rect[0] as i64 + rect[2] as i64) / 2).clamp(bounds[0] as i64, bounds[2] as i64 - 1)).max(bounds[0] as i64);
        top = (((rect[1] as i64 + rect[3] as i64) / 2).clamp(bounds[1] as i64, bounds[3] as i64 - 1)).max(bounds[1] as i64);
        right = left + 1;
        bottom = top + 1;
    }
    let grid = grid.clamp(1, 32) as i64;
    let (columns, rows) = ((right - left).min(grid), (bottom - top).min(grid));
    let mut samples = Vec::with_capacity((columns * rows) as usize);
    for row in 0..rows {
        for col in 0..columns {
            let cell_left = left + col * (right - left) / columns;
            let cell_right = left + (col + 1) * (right - left) / columns;
            let cell_top = top + row * (bottom - top) / rows;
            let cell_bottom = top + (row + 1) * (bottom - top) / rows;
            let sw = (cell_right - cell_left).min(4);
            let sh = (cell_bottom - cell_top).min(4);
            let sx = (cell_left + cell_right - sw) / 2;
            let sy = (cell_top + cell_bottom - sh) / 2;
            let centre = (sx + sw / 2, sy + sh / 2);
            let layer = &layers[monitor_index_for(&rects, centre.0 as i32, centre.1 as i32).unwrap_or(0)];
            let mut sum = [0.0f32; 3];
            for y in sy..sy + sh {
                for x in sx..sx + sw {
                    let c = layer.linear(x as f32 + 0.5, y as f32 + 0.5);
                    for k in 0..3 {
                        sum[k] += c[k];
                    }
                }
            }
            let n = (sw * sh) as f32;
            samples.push(sum.map(|v| encode_level(v / n)));
        }
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_px(w: usize, h: usize, f: impl Fn(usize, usize) -> u8) -> Vec<u8> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).flat_map(|(x, y)| {
            let v = f(x, y);
            [v, v, v, 255]
        }).collect()
    }

    fn layer<'a>(rect: [i32; 4], k: u32, w: usize, h: usize, px: &'a [u8]) -> Layer<'a> {
        Layer { rect, k, w, h, px, solid: None }
    }

    #[test]
    fn bilinear_reads_pixel_centres_exactly_and_interpolates_between() {
        let px = flat_px(2, 1, |x, _| if x == 0 { 0 } else { 200 });
        let l = layer([0, 0, 8, 4], 4, 2, 1, &px);
        // Working pixel 0 covers screen x 0..4 (centre 2), pixel 1 covers 4..8 (centre 6).
        assert_eq!(l.bilinear(2.0, 2.0)[0], 0.0);
        assert_eq!(l.bilinear(6.0, 2.0)[0], 200.0);
        assert!((l.bilinear(4.0, 2.0)[0] - 100.0).abs() < 1e-4);
        assert_eq!(l.bilinear(-50.0, 2.0)[0], 0.0, "clamped at the border");
        assert_eq!(l.bilinear(500.0, 2.0)[0], 200.0);
    }

    #[test]
    fn negative_origin_is_subtracted() {
        let px = flat_px(2, 1, |x, _| if x == 0 { 10 } else { 250 });
        let l = layer([-1920, -200, -1912, -196], 4, 2, 1, &px);
        assert_eq!(l.bilinear(-1918.0, -198.0)[0], 10.0);
        assert_eq!(l.bilinear(-1914.0, -198.0)[0], 250.0);
    }

    fn surface(w: usize, h: usize, rgba: [u8; 4]) -> Vec<u8> {
        (0..w * h).flat_map(|_| rgba).collect()
    }

    #[test]
    fn transparent_body_shows_the_backdrop_where_the_mask_covers() {
        let px = flat_px(4, 4, |_, _| 100);
        let l = [layer([0, 0, 4, 4], 1, 4, 4, &px)];
        let mut pixels = surface(4, 4, [0; 4]);
        let mut mask = surface(4, 4, [0; 4]);
        for y in 0..4 { for x in 0..2 { mask[(y * 4 + x) * 4 + 3] = 255; } }
        composite_under(&mut pixels, &mask, 4, 4, (0, 0), 255, 1.0, &Blend::new(&l), false);
        assert_eq!(&pixels[..4], &[100, 100, 100, 255]);
        assert_eq!(&pixels[2 * 4..2 * 4 + 4], &[0, 0, 0, 0], "outside the mask nothing is drawn");
    }

    #[test]
    fn opaque_body_is_untouched_and_half_body_lets_backdrop_through() {
        let px = flat_px(2, 2, |_, _| 200);
        let l = [layer([0, 0, 2, 2], 1, 2, 2, &px)];
        let mask = surface(2, 2, [255; 4]);
        let mut opaque = surface(2, 2, [10, 20, 30, 255]);
        composite_under(&mut opaque, &mask, 2, 2, (0, 0), 255, 1.0, &Blend::new(&l), true);
        assert_eq!(&opaque[..4], &[10, 20, 30, 255]);
        // Premultiplied half-alpha black over 200 grey: 0 + 200 * (1 - 0.5).
        let mut half = surface(2, 2, [0, 0, 0, 128]);
        composite_under(&mut half, &mask, 2, 2, (0, 0), 255, 1.0, &Blend::new(&l), false);
        assert!((half[0] as i32 - 100).abs() <= 1 && half[3] == 255, "{:?}", &half[..4]);
    }

    #[test]
    fn fade_scales_the_backdrop_and_zero_draws_nothing() {
        let px = flat_px(2, 2, |_, _| 200);
        let l = [layer([0, 0, 2, 2], 1, 2, 2, &px)];
        let mask = surface(2, 2, [255; 4]);
        let mut p = surface(2, 2, [0; 4]);
        composite_under(&mut p, &mask, 2, 2, (0, 0), 0, 1.0, &Blend::new(&l), true);
        assert!(p.iter().all(|&b| b == 0));
        composite_under(&mut p, &mask, 2, 2, (0, 0), 128, 1.0, &Blend::new(&l), false);
        assert_eq!(p[3], 128);
        assert!((p[0] as i32 - 100).abs() <= 1, "premultiplied: 200 x 0.5");
    }

    #[test]
    fn corral_straddling_two_monitors_draws_each_part_from_its_own_image() {
        let left = flat_px(2, 2, |_, _| 40);
        let right = flat_px(2, 2, |_, _| 220);
        let l = [layer([0, 0, 4, 4], 2, 2, 2, &left), layer([4, 0, 8, 4], 2, 2, 2, &right)];
        let mut p = surface(4, 1, [0; 4]);
        let mask = surface(4, 1, [255; 4]);
        composite_under(&mut p, &mask, 4, 1, (2, 0), 255, 1.0, &Blend::new(&l), false);
        assert_eq!(p[0], 40);
        assert_eq!(p[4], 40);
        assert_eq!(p[8], 220, "split at the seam");
        assert_eq!(p[12], 220);
    }

    #[test]
    fn cross_fade_mixes_old_and_new_per_monitor() {
        let new = flat_px(2, 2, |_, _| 200);
        let old = flat_px(2, 2, |_, _| 100);
        let now = [layer([0, 0, 2, 2], 1, 2, 2, &new)];
        let before = [layer([0, 0, 2, 2], 1, 2, 2, &old)];
        let mut p = surface(2, 2, [0; 4]);
        let mask = surface(2, 2, [255; 4]);
        let blend = Blend { layers: &now, from: Some((&before, 0.25)) };
        composite_under(&mut p, &mask, 2, 2, (0, 0), 255, 1.0, &blend, false);
        assert_eq!(p[0], 125);
    }

    #[test]
    fn column_and_row_caching_gives_the_same_pixels_as_the_per_pixel_read() {
        let px = flat_px(37, 23, |x, y| (x * 7 + y * 11) as u8);
        let l = [layer([-40, -10, 100, 80], 4, 37, 23, &px)];
        let mask = surface(61, 29, [255; 4]);
        let mut fast = surface(61, 29, [0; 4]);
        let mut slow = surface(61, 29, [0; 4]);
        composite_under(&mut fast, &mask, 61, 29, (-30, -5), 255, 1.0, &Blend::new(&l), true);
        let slow_blend = Blend { layers: &l, from: Some((&[], 1.0)) };
        composite_under(&mut slow, &mask, 61, 29, (-30, -5), 255, 1.0, &slow_blend, true);
        assert_eq!(fast, slow);
    }

    #[test]
    fn noise_is_anchored_to_the_monitor_not_the_corral() {
        let px = flat_px(512, 8, |_, _| 128);
        let l = [layer([0, 0, 512, 8], 1, 512, 8, &px)];
        let mask = surface(64, 1, [255; 4]);
        let mut at_100 = surface(64, 1, [0; 4]);
        let mut at_132 = surface(64, 1, [0; 4]);
        composite_under(&mut at_100, &mask, 64, 1, (100, 0), 255, 1.0, &Blend::new(&l), true);
        composite_under(&mut at_132, &mask, 64, 1, (132, 0), 255, 1.0, &Blend::new(&l), true);
        // Screen pixel 140 is surface x 40 in the first and x 8 in the second; both equal.
        assert_eq!(&at_100[40 * 4..40 * 4 + 4], &at_132[8 * 4..8 * 4 + 4]);
        let flat: Vec<u8> = at_100.chunks_exact(4).map(|p| p[0]).collect();
        assert!(flat.iter().any(|&v| v != 128), "noise moves some levels");
        assert!(flat.iter().all(|&v| (125..=131).contains(&v)), "at most a few levels: {flat:?}");
    }

    #[test]
    fn noise_breaks_up_the_bands_of_a_dark_ramp() {
        // wallpaper-blur-spec 9.6: a 1000 x 1 ramp 100..120 had bands of 50 px; with noise no run exceeds 12.
        let ramp = flat_px(1000, 1, |x, _| (100.0 + 20.0 * x as f32 / 1000.0).floor() as u8);
        let l = [layer([0, 0, 1000, 1], 1, 1000, 1, &ramp)];
        let longest = |with_noise: bool| {
            let mut p = surface(1000, 1, [0; 4]);
            let mask = surface(1000, 1, [255; 4]);
            composite_under(&mut p, &mask, 1000, 1, (0, 0), 255, 1.0, &Blend::new(&l), with_noise);
            let (mut best, mut run, mut last) = (0, 0, 256u32);
            for px in p.chunks_exact(4) {
                if px[0] as u32 == last { run += 1 } else { run = 1; last = px[0] as u32 }
                best = best.max(run);
            }
            best
        };
        assert!(longest(false) >= 49, "without noise the bands are 50 px");
        assert!(longest(true) <= 12, "longest run with noise {}", longest(true));
    }

    #[test]
    fn noise_stays_within_its_alpha_at_the_extremes_and_is_about_three_levels_over_grey() {
        // wallpaper-blur-spec 5: texel alpha is at most 6 of 255. The like-coloured texels vanish at
        // pure black or white; the opposite ones move a pixel by at most 6 levels, never past 0 or 255.
        for (level, range) in [(0u8, 0..=6u8), (255, 249..=255), (128, 125..=131)] {
            let px = flat_px(256, 4, |_, _| level);
            let l = [layer([0, 0, 256, 4], 1, 256, 4, &px)];
            let mut p = surface(256, 4, [0; 4]);
            let mask = surface(256, 4, [255; 4]);
            composite_under(&mut p, &mask, 256, 4, (0, 0), 255, 1.0, &Blend::new(&l), true);
            assert!(p.chunks_exact(4).all(|q| range.contains(&q[0]) && q[3] == 255), "level {level}");
        }
    }

    #[test]
    fn sampling_a_flat_image_returns_the_flat_colour() {
        let px = flat_px(10, 10, |_, _| 77);
        let l = [layer([0, 0, 40, 40], 4, 10, 10, &px)];
        for s in sample_rect(&l, [3, 3, 30, 30], 8) {
            assert!(s.iter().all(|&c| (c as i32 - 77).abs() <= 1), "{s:?}");
        }
        let solid = [Layer { solid: Some(0x0030_2010), ..layer([0, 0, 40, 40], 4, 1, 1, &[0, 0, 0, 255]) }];
        assert_eq!(sample_rect(&solid, [0, 0, 10, 10], 8), vec![[0x10, 0x20, 0x30]]);
        assert_eq!(sample_rect(&[], [0, 0, 10, 10], 8), vec![[128; 3]]);
    }

    /// Expand a layer to the full canvas by its own bilinear read (the "full-size computation").
    fn expand(l: &Layer) -> Vec<u8> {
        let (cw, ch) = ((l.rect[2] - l.rect[0]) as usize, (l.rect[3] - l.rect[1]) as usize);
        let mut out = Vec::with_capacity(cw * ch * 4);
        for y in 0..ch {
            for x in 0..cw {
                let c = l.bilinear((l.rect[0] + x as i32) as f32 + 0.5, (l.rect[1] + y as i32) as f32 + 0.5);
                out.extend_from_slice(&[c[2].round() as u8, c[1].round() as u8, c[0].round() as u8, 255]);
            }
        }
        out
    }

    #[test]
    fn working_size_sampling_matches_the_full_size_computation() {
        // Half black / half white at k 4 on a negative-origin monitor, plus a second monitor at k 2
        // (mixed DPI); rects partly outside and across the seam.
        let a = flat_px(20, 10, |x, _| if x < 10 { 0 } else { 255 });
        let b = flat_px(30, 15, |x, y| if (x + y) % 2 == 0 { 255 } else { 0 });
        let layers = [layer([-80, -20, 0, 20], 4, 20, 10, &a), layer([0, 0, 60, 30], 2, 30, 15, &b)];
        let full_a = expand(&layers[0]);
        let full_b = expand(&layers[1]);
        let full = [Layer::flat((-80, -20), 80, 40, &full_a, None), Layer::flat((0, 0), 60, 30, &full_b, None)];
        for rect in [[-100, -30, -20, 30], [-60, -10, 40, 25], [10, 5, 59, 29], [-4, -4, 4, 4], [200, 200, 210, 210]] {
            let got = sample_rect(&layers, rect, 8);
            let want = sample_rect(&full, rect, 8);
            assert_eq!(got.len(), want.len());
            for (g, w) in got.iter().zip(&want) {
                for c in 0..3 {
                    assert!((g[c] as i32 - w[c] as i32).abs() <= 1, "rect {rect:?}: {g:?} vs {w:?}");
                }
            }
        }
    }

    #[test]
    fn budget_smoke_compose_a_corral() {
        // wallpaper-blur-spec 7: a 600 x 400 Corral over a k 4 image.
        let px = flat_px(960, 540, |x, y| (x + y) as u8);
        let l = [layer([0, 0, 3840, 2160], 4, 960, 540, &px)];
        let mut p = surface(600, 400, [0; 4]);
        let mask = surface(600, 400, [255; 4]);
        let started = std::time::Instant::now();
        composite_under(&mut p, &mask, 600, 400, (1000, 700), 255, 1.0, &Blend::new(&l), true);
        eprintln!("compose 600x400 backdrop + noise: {:?}", started.elapsed());
        assert!(p[3] == 255);
    }
}
