//! Corral's independent colour treatment, from camouflage-spec sections 3, 4 and 6.
//! Pixel buffers contain premultiplied BGRA (little-endian ARGB words).

pub type Rgb = [u8; 3];
pub mod runtime;

#[derive(Clone, Debug)]
pub struct Palette {
    // The two end colours of the gradient (spec 3.1); the engine reads `lut`, tests read these.
    #[allow(dead_code)]
    pub shadow: Rgb,
    #[allow(dead_code)]
    pub highlight: Rgb,
    pub lut: [Rgb; 256],
    pub hash: u64,
}

fn decode(v: u8) -> f32 {
    let v = v as f32 / 255.0;
    if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

fn encode(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    let v = if v <= 0.0031308 { 12.92 * v } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    (v * 255.0 + 0.5).floor() as u8
}

// camouflage-spec 3.2: published Oklab sRGB matrices (2021 precision update).
fn linear_to_lab(c: [f32; 3]) -> [f32; 3] {
    let l = (0.4122214708*c[0] + 0.5363325363*c[1] + 0.0514459929*c[2]).cbrt();
    let m = (0.2119034982*c[0] + 0.6806995451*c[1] + 0.1073969566*c[2]).cbrt();
    let s = (0.0883024619*c[0] + 0.2817188376*c[1] + 0.6299787005*c[2]).cbrt();
    [0.2104542553*l + 0.7936177850*m - 0.0040720468*s,
     1.9779984951*l - 2.4285922050*m + 0.4505937099*s,
     0.0259040371*l + 0.7827717662*m - 0.8086757660*s]
}

fn lab_to_linear(c: [f32; 3]) -> [f32; 3] {
    let l = (c[0] + 0.3963377774*c[1] + 0.2158037573*c[2]).powi(3);
    let m = (c[0] - 0.1055613458*c[1] - 0.0638541728*c[2]).powi(3);
    let s = (c[0] - 0.0894841775*c[1] - 1.2914855480*c[2]).powi(3);
    [4.0767416621*l - 3.3077115913*m + 0.2309699292*s,
     -1.2684380046*l + 2.6097574011*m - 0.3413193965*s,
     -0.0041960863*l - 0.7034186147*m + 1.7076147010*s]
}

fn rgb_to_lab(c: Rgb) -> [f32; 3] { linear_to_lab(c.map(decode)) }
fn lab_to_rgb(c: [f32; 3]) -> Rgb { lab_to_linear(c).map(encode) }
fn mean(labs: &[[f32; 3]]) -> [f32; 3] {
    if labs.is_empty() { return rgb_to_lab([128; 3]); }
    let mut sum = [0.0; 3];
    for c in labs { for i in 0..3 { sum[i] += c[i]; } }
    sum.map(|v| v / labs.len() as f32)
}

fn in_gamut(c: [f32; 3]) -> bool {
    lab_to_linear(c).iter().all(|&v| (0.0..=1.0).contains(&v))
}

fn gamut(mut c: [f32; 3]) -> [f32; 3] {
    if in_gamut(c) { return c; }
    let (a, b) = (c[1], c[2]);
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..12 {
        let mid = (lo + hi) * 0.5;
        if in_gamut([c[0], a * mid, b * mid]) { lo = mid; } else { hi = mid; }
    }
    c[1] = a * lo;
    c[2] = b * lo;
    c
}

fn guarded(mut shadow: [f32; 3], mut highlight: [f32; 3], cap: bool) -> Palette {
    if cap {
        for c in [&mut shadow, &mut highlight] {
            let chroma = c[1].hypot(c[2]);
            if chroma > 0.15 { c[1] *= 0.15 / chroma; c[2] *= 0.15 / chroma; }
        }
    }
    if highlight[0] - shadow[0] < 0.35 {
        let middle = (shadow[0] + highlight[0]) * 0.5;
        shadow[0] = middle - 0.175;
        highlight[0] = middle + 0.175;
    }
    if shadow[0] < 0.10 {
        highlight[0] += 0.10 - shadow[0]; shadow[0] = 0.10;
    }
    if highlight[0] > 0.97 {
        shadow[0] -= highlight[0] - 0.97; highlight[0] = 0.97;
    }
    shadow = gamut(shadow);
    highlight = gamut(highlight);
    let lut = std::array::from_fn(|i| {
        let t = i as f32 / 255.0;
        lab_to_rgb(std::array::from_fn(|j| shadow[j] * (1.0 - t) + highlight[j] * t))
    });
    // Deterministic content identity. Runtime cache keys also include all spec 4 inputs.
    let mut hash = 0xcbf29ce484222325u64;
    for byte in lut.iter().flatten() {
        hash = (hash ^ *byte as u64).wrapping_mul(0x100000001b3);
    }
    Palette { shadow: lut[0], highlight: lut[255], lut, hash }
}

/// camouflage-spec 3.2: trimmed lightness bands, with a mean fallback below 16 samples.
pub fn derive_wallpaper_palette(samples: &[Rgb]) -> Palette {
    let mut labs: Vec<_> = samples.iter().map(|&v| rgb_to_lab(v)).collect();
    let average = mean(&labs);
    let (shadow, highlight) = if labs.len() < 16 { (average, average) } else {
        labs.sort_by(|a, b| a[0].total_cmp(&b[0]));
        let n = labs.len();
        (mean(&labs[n * 5 / 100..(n * 30).div_ceil(100)]),
         mean(&labs[n * 70 / 100..(n * 95).div_ceil(100)]))
    };
    guarded(shadow, highlight, true)
}

/// camouflage-spec 3.4: chosen colour has no wallpaper chroma cap.
pub fn derive_tint_palette(colour: Rgb) -> Palette {
    let c = rgb_to_lab(colour);
    guarded([c[0] - 0.25, c[1], c[2]], [c[0] + 0.25, c[1], c[2]], false)
}

fn prem(c: u32, alpha: u32) -> u32 { (c * alpha + 127) / 255 }
/// Exact n / 100 for n <= 25550 (255 * 100 + 50) as multiply and shift; checked exhaustively in tests.
fn div100(n: u32) -> u32 { (n * 5243) >> 19 }
fn mix(a: u32, b: u32, strength: u32) -> u32 { div100(a * (100 - strength) + b * strength + 50) }

/// camouflage-spec 3.3. Strength is percent, not the persisted 0..255 byte.
pub fn apply_duotone(palette: &Palette, strength: u8, pixels: &mut [u32]) {
    let strength = strength.min(100) as u32;
    if strength == 0 { return; }
    for pixel in pixels {
        let alpha = *pixel >> 24;
        if alpha == 0 { continue; }
        let original = [(*pixel >> 16) & 255, (*pixel >> 8) & 255, *pixel & 255];
        let c = original.map(|v| if alpha == 255 { v } else { ((v * 255 + alpha / 2) / alpha).min(255) });
        let brightness = (54*c[0] + 183*c[1] + 19*c[2] + 128) >> 8;
        let mapped = palette.lut[brightness as usize];
        if strength == 100 && alpha == 255 {
            *pixel = 0xff000000 | (mapped[0] as u32) << 16 | (mapped[1] as u32) << 8 | mapped[2] as u32;
            continue;
        }
        let out = std::array::from_fn::<_, 3, _>(|i| mix(original[i], prem(mapped[i] as u32, alpha), strength));
        *pixel = (alpha << 24) | (out[0] << 16) | (out[1] << 8) | out[2];
    }
}

/// Mean colour of a set of samples, averaged in Oklab (spec 3.5 backdrop).
pub fn mean_color(samples: &[Rgb]) -> Rgb {
    let labs: Vec<_> = samples.iter().copied().map(rgb_to_lab).collect();
    lab_to_rgb(mean(&labs))
}

/// `apply_duotone` for a premultiplied BGRA byte buffer of any alignment.
pub fn apply_duotone_bytes(palette: &Palette, strength: u8, bytes: &mut [u8]) {
    if strength == 0 { return; }
    for chunk in bytes.chunks_exact_mut(4) {
        let mut pixel = [u32::from_le_bytes(chunk.try_into().unwrap())];
        apply_duotone(palette, strength, &mut pixel);
        chunk.copy_from_slice(&pixel[0].to_le_bytes());
    }
}

/// camouflage-spec 3.5: straight ARGB colour in/out; alpha is preserved exactly.
pub fn recolor_color(palette: &Palette, strength: u8, colour: u32) -> u32 {
    if strength == 0 || colour >> 24 == 0 { return colour; }
    // Apply to opaque straight RGB then restore alpha, avoiding low-alpha quantisation.
    let mut pixel = [colour | 0xff000000];
    apply_duotone(palette, strength, &mut pixel);
    (colour & 0xff000000) | (pixel[0] & 0xffffff)
}

/// Spec 3.5: choose the higher WCAG contrast, without recolouring glyphs.
pub fn auto_text_color(backdrop: Rgb) -> u32 {
    fn luminance(c: Rgb) -> f32 { let c = c.map(decode); 0.2126*c[0] + 0.7152*c[1] + 0.0722*c[2] }
    let background = luminance(backdrop);
    let dark = luminance([0x1b, 0x1d, 0x22]);
    let white_contrast = 1.05 / (background + 0.05);
    let dark_contrast = (background.max(dark) + 0.05) / (background.min(dark) + 0.05);
    if white_contrast >= dark_contrast { 0xffffffff } else { 0xff1b1d22 }
}

/// Spec 3.5: encoded backdrop after the Corral's see-through amount (255 = fully clear).
pub fn composite_backdrop(background: Rgb, wallpaper_mean: Rgb, see_through: u8) -> Rgb {
    std::array::from_fn(|i| ((background[i] as u32 * (255 - see_through as u32)
        + wallpaper_mean[i] as u32 * see_through as u32 + 127) / 255) as u8)
}

/// Spec 4: one cheap lerp between an icon and its cached full-strength result, for any strength or
/// hover-fade step. Both buffers are premultiplied BGRA bytes with equal alpha, so the alpha byte
/// passes through exactly and the result equals `apply_duotone` on the unprocessed pixels.
pub fn lerp_bytes(original: &[u8], processed: &[u8], amount: u8, output: &mut [u8]) {
    assert_eq!(original.len(), processed.len());
    assert_eq!(original.len(), output.len());
    let take = amount.min(100) as u32;
    let keep = 100 - take;
    for ((&o, &p), out) in original.iter().zip(processed).zip(output) {
        *out = div100(o as u32 * keep + p as u32 * take + 50) as u8;
    }
}

/// Spec 3.2: at most 32x32 centre samples, each a linear-light mean of up to 4x4 pixels.
pub fn sample_rect(wallpaper: &crate::tint::Wallpaper, rect: [i32; 4], grid: usize) -> Vec<Rgb> {
    if let Some(c) = wallpaper.solid {
        return vec![[c as u8, (c >> 8) as u8, (c >> 16) as u8]];
    }
    let (width, height) = (wallpaper.width.max(0) as i64, wallpaper.height.max(0) as i64);
    if width == 0 || height == 0 || wallpaper.bgra.len() < (width * height * 4) as usize { return vec![[128; 3]]; }
    let x0 = rect[0] as i64 - wallpaper.origin.0 as i64;
    let y0 = rect[1] as i64 - wallpaper.origin.1 as i64;
    let x1 = rect[2] as i64 - wallpaper.origin.0 as i64;
    let y1 = rect[3] as i64 - wallpaper.origin.1 as i64;
    let (mut left, mut top, mut right, mut bottom) = (x0.max(0), y0.max(0), x1.min(width), y1.min(height));
    if left >= right || top >= bottom {
        // No monitor topology is stored in Wallpaper: nearest captured pixel is deterministic.
        left = ((x0 + x1) / 2).clamp(0, width - 1);
        top = ((y0 + y1) / 2).clamp(0, height - 1);
        right = left + 1; bottom = top + 1;
    }
    let grid = grid.clamp(1, 32) as i64;
    let (columns, rows) = ((right - left).min(grid), (bottom - top).min(grid));
    let mut samples = Vec::with_capacity((columns * rows) as usize);
    for row in 0..rows { for col in 0..columns {
        let cell_left = left + col * (right - left) / columns;
        let cell_right = left + (col + 1) * (right - left) / columns;
        let cell_top = top + row * (bottom - top) / rows;
        let cell_bottom = top + (row + 1) * (bottom - top) / rows;
        let sw = (cell_right - cell_left).min(4);
        let sh = (cell_bottom - cell_top).min(4);
        let sx = (cell_left + cell_right - sw) / 2;
        let sy = (cell_top + cell_bottom - sh) / 2;
        let mut sum = [0.0; 3];
        for y in sy..sy + sh { for x in sx..sx + sw {
            let p = ((y * width + x) * 4) as usize;
            sum[0] += decode(wallpaper.bgra[p + 2]);
            sum[1] += decode(wallpaper.bgra[p + 1]);
            sum[2] += decode(wallpaper.bgra[p]);
        }}
        samples.push(sum.map(|v| encode(v / (sw * sh) as f32)));
    }}
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channels(c: u32) -> [u32; 3] { [(c >> 16) & 255, (c >> 8) & 255, c & 255] }
    fn wp(origin: (i32, i32), width: i32, height: i32, f: impl Fn(i32, i32) -> Rgb) -> crate::tint::Wallpaper {
        let mut bgra = Vec::new();
        for y in 0..height { for x in 0..width {
            let [r, g, b] = f(x, y); bgra.extend_from_slice(&[b, g, r, 255]);
        }}
        crate::tint::Wallpaper { origin, width, height, bgra, solid: None }
    }

    #[test]
    fn strength_zero_is_bit_identical_and_cache_lerp() {
        let palette = derive_wallpaper_palette(&[[80, 140, 200]; 16]);
        let original = [0, 0x00010203, 0x01010000, 0x80643a71, 0xffff0000];
        let mut actual = original;
        apply_duotone(&palette, 0, &mut actual);
        assert_eq!(actual, original);
        let bytes: Vec<u8> = original.iter().flat_map(|p| p.to_le_bytes()).collect();
        let mut output = vec![0; bytes.len()];
        lerp_bytes(&bytes, &vec![0; bytes.len()], 0, &mut output);
        assert_eq!(output, bytes);
    }

    #[test]
    fn alpha_and_premultiplied_bounds_survive_all_strengths() {
        let palette = derive_tint_palette([200, 150, 30]);
        for strength in 0..=100 {
            for alpha in [0u32, 1, 128, 255] {
                let mut pixels = [(alpha << 24) | (alpha << 16) | ((alpha / 2) << 8)];
                apply_duotone(&palette, strength, &mut pixels);
                assert_eq!(pixels[0] >> 24, alpha);
                assert!(channels(pixels[0]).iter().all(|&c| c <= alpha));
                if alpha == 0 { assert_eq!(pixels[0], 0); }
            }
        }
    }

    #[test]
    fn full_duotone_equals_lut_after_unpremultiplication() {
        let palette = derive_wallpaper_palette(&[[40, 120, 180]; 64]);
        for alpha in [1, 128, 255] { for value in 0..=alpha {
            let original = (alpha << 24) | (value << 16) | (value << 8) | value;
            let index = ((value * 255 + alpha / 2) / alpha).min(255) as usize;
            let expected = palette.lut[index].map(|c| prem(c as u32, alpha));
            let mut pixels = [original];
            apply_duotone(&palette, 100, &mut pixels);
            assert_eq!(channels(pixels[0]), expected);
        }}
    }

    #[test]
    fn half_strength_is_rounded_midpoint() {
        let palette = derive_tint_palette([100, 170, 30]);
        let original = [0xff35210a, 0x80403010, 0x01010000];
        let mut full = original;
        let mut half = original;
        apply_duotone(&palette, 100, &mut full);
        apply_duotone(&palette, 50, &mut half);
        for i in 0..original.len() { for c in 0..3 {
            let (a, b, mid) = (channels(original[i])[c], channels(full[i])[c], channels(half[i])[c]);
            assert!((a.min(b)..=a.max(b)).contains(&mid));
            assert_eq!(mid, (a + b + 1) / 2);
        }}
    }

    #[test]
    fn endpoints_and_grey_ramp_keep_order() {
        let palette = derive_tint_palette([160, 100, 60]);
        let mut pixels: Vec<_> = (0..=255u32).map(|v| 0xff000000 | v * 0x10101).collect();
        apply_duotone(&palette, 100, &mut pixels);
        assert_eq!(channels(pixels[0]), palette.shadow.map(u32::from));
        assert_eq!(channels(pixels[255]), palette.highlight.map(u32::from));
        let labs: Vec<_> = pixels.iter().map(|&p| rgb_to_lab(channels(p).map(|v| v as u8))).collect();
        assert!(labs.windows(2).all(|pair| pair[1][0] + 0.002 >= pair[0][0]));
        let mut identity = palette.clone();
        identity.lut = std::array::from_fn(|i| [i as u8; 3]);
        let mut pixels: Vec<_> = (0..=255u32).map(|v| 0xff000000 | v * 0x10101).collect();
        let original = pixels.clone();
        apply_duotone(&identity, 100, &mut pixels);
        assert_eq!(pixels, original);
    }

    #[test]
    fn flat_wallpaper_guards_and_documented_neutral_targets() {
        for c in [[0; 3], [255; 3], [128; 3]] {
            let p = derive_wallpaper_palette(&[c; 1024]);
            let (s, h) = (rgb_to_lab(p.shadow), rgb_to_lab(p.highlight));
            assert!(h[0] - s[0] >= 0.345, "{c:?}: {s:?}, {h:?}");
            assert!(s[0] >= 0.095 && h[0] <= 0.975);
            assert!(p.shadow.iter().all(|v| *v == p.shadow[0]));
            assert!(p.highlight.iter().all(|v| *v == p.highlight[0]));
            let expected = match c[0] { 0 => Some((3u8, 85u8)), 255 => Some((134, 245)), _ => None };
            if let Some((s, h)) = expected {
                assert!(p.shadow[0].abs_diff(s) <= 2);
                assert!(p.highlight[0].abs_diff(h) <= 2);
            }
        }
    }

    #[test]
    fn saturated_wallpaper_keeps_hue_and_caps_chroma() {
        for colour in [[255, 0, 0], [0, 255, 0], [0, 0, 255]] {
            let p = derive_wallpaper_palette(&[colour; 16]);
            let original = rgb_to_lab(colour);
            let hue = original[2].atan2(original[1]);
            for c in [p.shadow, p.highlight] {
                let c = rgb_to_lab(c);
                assert!(c[1].hypot(c[2]) <= 0.155);
                let error = (c[2].atan2(c[1]) - hue).sin().abs().asin().to_degrees();
                assert!(error < 3.0, "hue changed by {error}");
                assert!(lab_to_linear(c).iter().all(|&v| (-0.00001..=1.00001).contains(&v)));
            }
        }
    }

    #[test]
    fn two_tone_and_small_sample_fallback() {
        let mut samples = vec![[0; 3]; 512]; samples.extend(vec![[255; 3]; 512]);
        let p = derive_wallpaper_palette(&samples);
        assert!(p.shadow[0] < 10 && p.highlight[0] > 240);
        let one = derive_wallpaper_palette(&[[85, 105, 125]]);
        for count in 0..16 {
            let p = derive_wallpaper_palette(&vec![[85, 105, 125]; count]);
            if count > 0 { assert_eq!(p.hash, one.hash); }
            assert!(rgb_to_lab(p.highlight)[0] - rgb_to_lab(p.shadow)[0] >= 0.345);
        }
    }

    #[test]
    fn tint_midpoint_and_extreme_range() {
        for colour in [[130, 110, 90], [80, 130, 150], [140; 3]] {
            let p = derive_tint_palette(colour);
            let target = rgb_to_lab(colour);
            let actual = rgb_to_lab(p.lut[128]);
            assert!((0.35..=0.72).contains(&target[0]));
            let distance = actual.iter().zip(target).map(|(&a, b)| (a - b).powi(2)).sum::<f32>().sqrt();
            assert!(distance < 0.03, "midpoint distance {distance}");
        }
        for colour in [[0; 3], [255; 3]] {
            let p = derive_tint_palette(colour);
            assert!(rgb_to_lab(p.shadow)[0] >= 0.095);
            assert!(rgb_to_lab(p.highlight)[0] <= 0.975);
        }
    }

    #[test]
    fn straight_surface_colour_preserves_alpha_and_low_alpha_precision() {
        let p = derive_tint_palette([120, 70, 180]);
        for alpha in [0, 1, 128, 255] {
            let colour = (alpha << 24) | 0x4282c2;
            assert_eq!(recolor_color(&p, 0, colour), colour);
            let mapped = recolor_color(&p, 100, colour);
            assert_eq!(mapped >> 24, alpha);
            if alpha > 0 {
                let opaque = recolor_color(&p, 100, 0xff4282c2);
                assert_eq!(mapped & 0xffffff, opaque & 0xffffff);
            }
        }
    }

    #[test]
    fn geometry_clips_and_converts_negative_virtual_origin() {
        let wallpaper = wp((-1920, -200), 1920, 100, |x, _| if x < 960 { [210, 30, 90] } else { [30, 170, 70] });
        assert_eq!(sample_rect(&wallpaper, [-1500, -180, -1400, -150], 32), vec![[210, 30, 90]; 960]);
        let clipped = sample_rect(&wallpaper, [-2000, -230, -1800, -150], 32);
        assert_eq!(clipped, sample_rect(&wallpaper, [-1920, -200, -1800, -150], 32));
        assert_eq!(sample_rect(&wallpaper, [-2300, -1000, -2200, -900], 32), vec![[210, 30, 90]]);
        let spanning = sample_rect(&wallpaper, [-1920, -200, 0, -100], 32);
        assert_eq!(spanning.len(), 1024);
        assert!(spanning.contains(&[210, 30, 90]) && spanning.contains(&[30, 170, 70]));
    }

    #[test]
    fn cell_centres_are_linear_light_means_and_small_grids_are_bounded() {
        let wallpaper = wp((0, 0), 4, 4, |x, _| if x % 2 == 0 { [0; 3] } else { [255; 3] });
        assert_eq!(sample_rect(&wallpaper, [0, 0, 4, 4], 1), vec![[188; 3]]);
        let tiny = wp((0, 0), 1, 1, |_, _| [20, 40, 60]);
        assert_eq!(sample_rect(&tiny, [0, 0, 1, 1], 32), vec![[20, 40, 60]]);
        assert_eq!(sample_rect(&wallpaper, [0, 0, 4, 4], usize::MAX).len(), 16);
    }

    #[test]
    fn division_by_one_hundred_is_exact_over_the_whole_mix_range() {
        for n in 0..=25550u32 { assert_eq!(div100(n), n / 100, "{n}"); }
    }

    #[test]
    fn auto_labels_choose_readable_text_and_backdrop_composition() {
        assert_eq!(auto_text_color([0; 3]), 0xffffffff);
        assert_eq!(auto_text_color([255; 3]), 0xff1b1d22);
        assert_eq!(auto_text_color([128; 3]), 0xff1b1d22);
        assert_eq!(composite_backdrop([0; 3], [255; 3], 128), [128; 3]);
        for background in [[0; 3], [128; 3], [255; 3]] {
            let foreground = channels(auto_text_color(background)).map(|v| v as u8);
            let lum = |c: Rgb| { let c = c.map(decode); 0.2126*c[0] + 0.7152*c[1] + 0.0722*c[2] };
            let (a, b) = (lum(background), lum(foreground));
            let actual = (a.max(b) + 0.05) / (a.min(b) + 0.05);
            let alternatives = [[255; 3], [0x1b, 0x1d, 0x22]].map(|c| {
                let foreground = lum(c); (foreground.max(a) + 0.05) / (foreground.min(a) + 0.05)
            });
            assert!((actual - alternatives[0].max(alternatives[1])).abs() < 0.0001);
            if alternatives.iter().any(|&contrast| contrast >= 4.5) { assert!(actual >= 4.5); }
        }
    }

    #[test]
    fn content_hash_is_stable_and_changes_with_palette() {
        let a = derive_tint_palette([100, 140, 180]);
        assert_eq!(a.hash, derive_tint_palette([100, 140, 180]).hash);
        assert_ne!(a.hash, derive_tint_palette([180, 140, 100]).hash);
    }

    #[test]
    fn performance_smoke_logs_spec_four_budgets() {
        use std::{hint::black_box, time::Instant};
        let samples: Vec<_> = (0..1024).map(|i| [(i % 256) as u8, ((i * 7) % 256) as u8, ((i * 13) % 256) as u8]).collect();
        let start = Instant::now();
        for _ in 0..100 { black_box(derive_wallpaper_palette(black_box(&samples))); }
        let palette_time = start.elapsed() / 100;
        let palette = derive_wallpaper_palette(&samples);
        let mut icon = vec![0xff5070c0; 48 * 48];
        let start = Instant::now();
        for _ in 0..1000 { apply_duotone(black_box(&palette), 100, black_box(&mut icon)); }
        let icon_time = start.elapsed() / 1000;
        let original: Vec<u8> = icon.iter().flat_map(|p| p.to_le_bytes()).collect();
        let processed = original.clone();
        let mut out = vec![0u8; original.len()];
        let start = Instant::now();
        for _ in 0..1000 { lerp_bytes(black_box(&original), black_box(&processed), 60, black_box(&mut out)); }
        let lerp_time = start.elapsed() / 1000;
        eprintln!("camouflage-spec 4 performance (informational): palette={palette_time:?} (<=1ms); 48x48={icon_time:?} (<=15us); cached lerp={lerp_time:?} (<=5us)");
    }

    fn synthetic_wallpaper(kind: usize, x: usize, y: usize) -> Rgb {
        match kind {
            0 => [12 + (x % 20) as u8, 18 + (y % 20) as u8, 35],
            1 => [235 + (x % 20) as u8, 230 + (y % 25) as u8, 220],
            2 => [128; 3],
            3 => [[230, 20, 50], [20, 220, 80], [20, 60, 245]][((x + y) / 40) % 3],
            4 => {
                let noise = ((x * 1973 + y * 9277 + x * y * 31) % 53) as u8;
                let wave = (((x as f32 / 15.0).sin() + (y as f32 / 23.0).cos()) * 30.0 + 80.0) as u8;
                [wave.saturating_add(noise), 70 + noise, 45 + noise / 2]
            },
            _ => [180 + (x % 65) as u8, 190 + (y % 60) as u8, 225],
        }
    }

    fn stock_icons() -> Vec<Vec<u32>> {
        use windows::Win32::UI::{Shell::*, WindowsAndMessaging::DestroyIcon};
        let ids = [SIID_DOCNOASSOC, SIID_DOCASSOC, SIID_APPLICATION, SIID_FOLDER,
            SIID_FOLDEROPEN, SIID_DRIVEFIXED, SIID_DRIVENET, SIID_WORLD,
            SIID_PRINTER, SIID_AUDIOFILES, SIID_IMAGEFILES, SIID_VIDEOFILES];
        ids.into_iter().map(|id| {
            let mut info = SHSTOCKICONINFO { cbSize: std::mem::size_of::<SHSTOCKICONINFO>() as u32, ..Default::default() };
            // Read-only stock resources; never enumerates the user's files or opens a window.
            unsafe { SHGetStockIconInfo(id, SHGSI_ICON | SHGSI_LARGEICON, &mut info) }.unwrap();
            let bitmap = crate::shell::icon_to_bgra(info.hIcon, 48);
            unsafe { let _ = DestroyIcon(info.hIcon); }
            bitmap.unwrap().bgra_premultiplied.chunks_exact(4)
                .map(|pixel| u32::from_le_bytes(pixel.try_into().unwrap())).collect()
        }).collect()
    }

    // Minimal PNG writer using stored DEFLATE blocks. This keeps the pure QA test
    // independent of COM and graphics drivers (camouflage-spec 3.7).
    fn write_png(path: &std::path::Path, width: usize, height: usize, rgb: &[Rgb]) {
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(kind); out.extend_from_slice(data);
            let mut crc = !0u32;
            for &byte in kind.iter().chain(data) {
                crc ^= byte as u32;
                for _ in 0..8 { crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1)); }
            }
            out.extend_from_slice(&(!crc).to_be_bytes());
        }
        let mut scanlines = Vec::with_capacity((width * 3 + 1) * height);
        for row in rgb.chunks_exact(width) { scanlines.push(0); for pixel in row { scanlines.extend_from_slice(pixel); } }
        let mut zlib = vec![0x78, 0x01];
        let mut blocks = scanlines.chunks(65535).peekable();
        while let Some(block) = blocks.next() {
            zlib.push(if blocks.peek().is_none() { 1 } else { 0 });
            zlib.extend_from_slice(&(block.len() as u16).to_le_bytes());
            zlib.extend_from_slice(&(!(block.len() as u16)).to_le_bytes());
            zlib.extend_from_slice(block);
        }
        let (mut s1, mut s2) = (1u32, 0u32);
        for byte in scanlines { s1 = (s1 + byte as u32) % 65521; s2 = (s2 + s1) % 65521; }
        zlib.extend_from_slice(&((s2 << 16) | s1).to_be_bytes());
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut header = Vec::new(); header.extend_from_slice(&(width as u32).to_be_bytes()); header.extend_from_slice(&(height as u32).to_be_bytes());
        header.extend_from_slice(&[8, 2, 0, 0, 0]);
        chunk(&mut png, b"IHDR", &header); chunk(&mut png, b"IDAT", &zlib); chunk(&mut png, b"IEND", &[]);
        std::fs::write(path, png).unwrap();
    }

    #[test]
    #[ignore = "writes offline PNG fixtures to target/camouflage-contact-sheet"]
    fn contact_sheet() {
        let directory = std::path::Path::new("target/camouflage-contact-sheet");
        std::fs::create_dir_all(directory).unwrap();
        let names = ["dark", "bright", "flat-grey", "saturated", "busy-procedural", "pastel"];
        let strengths = [25, 50, 75, 100];
        let icons = stock_icons();
        let (width, height) = (256, 192);
        let mut sheet = vec![[0; 3]; width * 4 * height * 6];
        for (wallpaper, name) in names.iter().enumerate() {
            let fixture = wp((0, 0), width as i32, height as i32, |x, y| synthetic_wallpaper(wallpaper, x as usize, y as usize));
            let palette = derive_wallpaper_palette(&sample_rect(&fixture, [0, 0, width as i32, height as i32], 32));
            for (column, &strength) in strengths.iter().enumerate() {
                let mut rgb: Vec<_> = (0..width * height).map(|i| synthetic_wallpaper(wallpaper, i % width, i / width)).collect();
                for kind in 0..12 {
                    let mut icon = icons[kind].clone();
                    apply_duotone(&palette, strength, &mut icon);
                    for y in 0..48 { for x in 0..48 {
                        let p = icon[y * 48 + x];
                        if p >> 24 != 0 {
                            let index = (kind / 4 * 64 + y + 8) * width + kind % 4 * 64 + x + 8;
                            let backdrop = rgb[index];
                            rgb[index] = std::array::from_fn(|c| (channels(p)[c]
                                + (backdrop[c] as u32 * (255 - (p >> 24)) + 127) / 255).min(255) as u8);
                        }
                    }}
                }
                let file = directory.join(format!("{wallpaper}-{name}-duotone-{strength:03}.png"));
                write_png(&file, width, height, &rgb);
                for y in 0..height { let start = (wallpaper * height + y) * width * 4 + column * width;
                    sheet[start..start + width].copy_from_slice(&rgb[y * width..(y + 1) * width]); }
            }
        }
        write_png(&directory.join("contact-sheet.png"), width * 4, height * 6, &sheet);
        std::fs::write(directory.join("README.md"), "# Camouflage offline comparison\n\nRows: dark, bright, flat grey, saturated, busy procedural, pastel; duotone only. Columns: 25, 50, 75, 100 percent. 24 individual PNGs plus contact-sheet.png.\n\nAt 50 percent: are all icons recognisable at arm's length? Does the Corral feel part of its wallpaper? Can folder/document/app still be distinguished by colour, or only by shape? Compare alpha edges, contrast, warmth and colour identity.\n\nTwelve real Windows stock icons, in row-major order: unassociated document, associated document, application, folder, open folder, fixed drive, network drive, world, printer, audio files, image files, video files. Read-only SHGetStockIconInfo handles are drawn into owned memory DIBs and destroyed; no user files, windows or desktop are touched.\n\nLIMITATION: the busy wallpaper is procedural, not a photo; all wallpapers are synthetic. Product comparison on the user's wallpaper remains a live checklist item.\n").unwrap();
        eprintln!("wrote {}", directory.display());
    }
}
