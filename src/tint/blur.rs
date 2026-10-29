//! Linear-light three-box Gaussian approximation on the working size (wallpaper-blur-spec 4). Pure CPU,
//! no GPU effect: the result is the same on every machine and one function serves the engine, the
//! live sample and the tests.

use std::sync::OnceLock;

/// One horizontal or vertical pass: a box of odd width `2 ra + 1`, optionally blended with a box of
/// width `2 rb + 1` by weight `t` (see [`kernels`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pass {
    pub ra: usize,
    pub rb: usize,
    pub t: f32,
}

/// Variance of a box of odd width `w` (in samples squared).
fn box_variance(w: usize) -> f64 {
    ((w * w) as f64 - 1.0) / 12.0
}

/// The three passes whose summed variance equals `sigma_w^2` (wallpaper-blur-spec 4.3).
///
/// [inferred] The spec's whole-number box widths (`wl`, `wu = wl + 2`) cannot reach 3 % of the target
/// variance for small sigma (sigma 1 would land 33 % short), so the pass that straddles the target is a
/// blend of the two neighbouring box widths: a blend of zero-mean kernels has the blended variance, so
/// the total is exact. Every other pass is a plain box of width `wl` or `wu`.
pub fn kernels(sigma_w: f32) -> [Pass; 3] {
    let target = (sigma_w.max(0.0) as f64).powi(2);
    // Largest odd width whose three-fold variance does not exceed the target.
    let mut wl = 1usize;
    while 3.0 * box_variance(wl + 2) <= target && wl < 4001 {
        wl += 2;
    }
    let wu = wl + 2;
    let step = box_variance(wu) - box_variance(wl);
    let extra = (target - 3.0 * box_variance(wl)).max(0.0);
    let upgrades = ((extra / step).floor() as usize).min(3);
    let frac = if upgrades >= 3 { 0.0 } else { ((extra - upgrades as f64 * step) / step).clamp(0.0, 1.0) };
    let (rl, ru) = ((wl - 1) / 2, (wu - 1) / 2);
    let mut passes = [Pass { ra: rl, rb: rl, t: 0.0 }; 3];
    for pass in passes.iter_mut().take(upgrades) {
        *pass = Pass { ra: ru, rb: ru, t: 0.0 };
    }
    if upgrades < 3 && frac > 0.0 {
        passes[upgrades] = Pass { ra: rl, rb: ru, t: frac as f32 };
    }
    passes
}

/// Summed variance of the three passes (the tests compare it with `sigma_w^2`).
#[cfg(test)]
pub fn kernel_variance(passes: &[Pass; 3]) -> f64 {
    passes
        .iter()
        .map(|p| {
            let (a, b) = (box_variance(2 * p.ra + 1), box_variance(2 * p.rb + 1));
            (1.0 - p.t as f64) * a + p.t as f64 * b
        })
        .sum()
}

/// One horizontal pass over `rows` rows of `w` samples; edges repeat the border sample.
fn pass_rows(src: &[f32], dst: &mut [f32], w: usize, pass: Pass) {
    if w == 0 {
        return;
    }
    let last = w - 1;
    let mix = pass.t > 0.0 && pass.rb != pass.ra;
    for (row_in, row_out) in src.chunks_exact(w).zip(dst.chunks_exact_mut(w)) {
        let (mut a, mut b) = (window_sum(row_in, pass.ra), if mix { window_sum(row_in, pass.rb) } else { 0.0 });
        let (wa, wb) = ((2 * pass.ra + 1) as f64, (2 * pass.rb + 1) as f64);
        for i in 0..w {
            let va = (a / wa) as f32;
            row_out[i] = if mix { va + (((b / wb) as f32) - va) * pass.t } else { va };
            let enter = |r: usize| row_in[(i + r + 1).min(last)] as f64;
            let leave = |r: usize| row_in[i.saturating_sub(r)] as f64;
            a += enter(pass.ra) - leave(pass.ra);
            if mix {
                b += enter(pass.rb) - leave(pass.rb);
            }
        }
    }
}

/// Sum of the window centred on sample 0 with radius `r`, border samples repeated.
fn window_sum(row: &[f32], r: usize) -> f64 {
    let last = row.len() - 1;
    (0..=2 * r).map(|j| row[j.saturating_sub(r).min(last)] as f64).sum::<f64>()
}

/// One vertical pass; keeps one running sum per column so memory access stays row by row.
fn pass_columns(src: &[f32], dst: &mut [f32], w: usize, h: usize, pass: Pass) {
    if w == 0 || h == 0 {
        return;
    }
    let last = h - 1;
    let mix = pass.t > 0.0 && pass.rb != pass.ra;
    let at = |y: isize| -> &[f32] { &src[(y.clamp(0, last as isize) as usize) * w..][..w] };
    let init = |r: usize| -> Vec<f64> {
        let mut sums = vec![0.0f64; w];
        for j in 0..=2 * r {
            for (s, v) in sums.iter_mut().zip(at(j as isize - r as isize)) {
                *s += *v as f64;
            }
        }
        sums
    };
    let mut a = init(pass.ra);
    let mut b = if mix { init(pass.rb) } else { Vec::new() };
    let (wa, wb) = ((2 * pass.ra + 1) as f64, (2 * pass.rb + 1) as f64);
    for y in 0..h {
        let out = &mut dst[y * w..][..w];
        for x in 0..w {
            let va = (a[x] / wa) as f32;
            out[x] = if mix { va + (((b[x] / wb) as f32) - va) * pass.t } else { va };
        }
        let step = |sums: &mut Vec<f64>, r: usize| {
            let enter = at(y as isize + r as isize + 1);
            let leave = at(y as isize - r as isize);
            for ((s, e), l) in sums.iter_mut().zip(enter).zip(leave) {
                *s += *e as f64 - *l as f64;
            }
        };
        step(&mut a, pass.ra);
        if mix {
            step(&mut b, pass.rb);
        }
    }
}

/// Blur one linear plane of `w x h` samples in place with `sigma_w` (three passes per axis).
pub fn blur_plane(plane: &mut [f32], scratch: &mut [f32], w: usize, h: usize, sigma_w: f32) {
    debug_assert_eq!(plane.len(), w * h);
    debug_assert_eq!(scratch.len(), w * h);
    if w == 0 || h == 0 || sigma_w <= 0.0 {
        return;
    }
    // Six passes ping-pong between the two buffers; an even count leaves the result in `plane`.
    let (mut from, mut to): (&mut [f32], &mut [f32]) = (plane, scratch);
    let passes = kernels(sigma_w);
    for pass in passes {
        pass_rows(from, to, w, pass);
        std::mem::swap(&mut from, &mut to);
    }
    for pass in passes {
        pass_columns(from, to, w, h, pass);
        std::mem::swap(&mut from, &mut to);
    }
}

// ---- sRGB <-> linear ------------------------------------------------------------------------------

fn decode_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut t = [0.0f32; 256];
        for (i, v) in t.iter_mut().enumerate() {
            let c = i as f64 / 255.0;
            *v = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } as f32;
        }
        t
    })
}

const ENCODE_STEPS: usize = 4096;

/// sRGB levels (0..255, fractional) at `ENCODE_STEPS + 1` evenly spaced linear values.
fn encode_lut() -> &'static [f32] {
    static LUT: OnceLock<Vec<f32>> = OnceLock::new();
    LUT.get_or_init(|| {
        (0..=ENCODE_STEPS)
            .map(|i| {
                let l = i as f64 / ENCODE_STEPS as f64;
                let c = if l <= 0.0031308 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
                (c * 255.0) as f32
            })
            .collect()
    })
}

/// Linear light to an 8-bit sRGB level, `floor(x * 255 + 0.5)` clamped (wallpaper-blur-spec 4.4).
pub fn encode_level(linear: f32) -> u8 {
    let lut = encode_lut();
    let x = linear.clamp(0.0, 1.0) * ENCODE_STEPS as f32;
    let i = (x as usize).min(ENCODE_STEPS - 1);
    let f = x - i as f32;
    let v = lut[i] + (lut[i + 1] - lut[i]) * f;
    (v + 0.5).floor().clamp(0.0, 255.0) as u8
}

/// 8-bit sRGB level to linear light.
pub fn decode_level(level: u8) -> f32 {
    decode_lut()[level as usize]
}

/// Blur an opaque 8-bit BGRX/BGRA image in linear light and return BGRX with alpha 255
/// (wallpaper-blur-spec 4.2-4.4). One colour plane at a time keeps the transient copy small.
pub fn blur_bgrx(src: &[u8], w: usize, h: usize, sigma_w: f32) -> Vec<u8> {
    let n = w * h;
    let mut out = vec![255u8; n * 4];
    if n == 0 || src.len() < n * 4 {
        return out;
    }
    let lut = decode_lut();
    let mut plane = vec![0.0f32; n];
    let mut scratch = vec![0.0f32; n];
    for channel in 0..3 {
        for (v, px) in plane.iter_mut().zip(src.chunks_exact(4)) {
            *v = lut[px[channel] as usize];
        }
        blur_plane(&mut plane, &mut scratch, w, h, sigma_w);
        for (v, px) in plane.iter().zip(out.chunks_exact_mut(4)) {
            px[channel] = encode_level(*v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane_from(w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> Vec<f32> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| f(x, y)).collect()
    }

    fn blurred(mut p: Vec<f32>, w: usize, h: usize, sigma: f32) -> Vec<f32> {
        let mut s = vec![0.0; w * h];
        blur_plane(&mut p, &mut s, w, h, sigma);
        p
    }

    #[test]
    fn kernel_variance_matches_sigma_squared() {
        // wallpaper-blur-spec 9.2: within 3 % for sigma_w 1..12 in steps of 0.25.
        let mut sigma = 1.0f32;
        while sigma <= 12.0 {
            let v = kernel_variance(&kernels(sigma));
            let want = (sigma as f64).powi(2);
            assert!((v - want).abs() <= 0.03 * want, "sigma {sigma}: variance {v} vs {want}");
            sigma += 0.25;
        }
    }

    #[test]
    fn measured_impulse_variance_matches_the_plan() {
        for sigma in [1.0f32, 2.5, 7.5, 12.0] {
            let n = 201;
            let mut row = vec![0.0f32; n];
            row[n / 2] = 1.0;
            let out = blurred(row, n, 1, sigma);
            let sum: f64 = out.iter().map(|v| *v as f64).sum();
            let var: f64 = out.iter().enumerate().map(|(i, v)| (i as f64 - (n / 2) as f64).powi(2) * *v as f64).sum();
            assert!((sum - 1.0).abs() < 1e-5, "sum {sum}");
            let want = (sigma as f64).powi(2);
            assert!((var - want).abs() <= 0.03 * want, "sigma {sigma}: measured {var} vs {want}");
        }
    }

    #[test]
    fn constant_image_is_unchanged_edges_included() {
        for level in 0..=255u8 {
            let src: Vec<u8> = (0..7 * 5).flat_map(|_| [level, level, level, 255]).collect();
            let out = blur_bgrx(&src, 7, 5, 6.0);
            assert!(out.chunks_exact(4).all(|p| p[..3] == [level; 3] && p[3] == 255), "level {level}");
        }
    }

    #[test]
    fn impulse_gives_a_symmetric_kernel_summing_to_one() {
        let (w, h) = (81, 81);
        let c = 40;
        let out = blurred(plane_from(w, h, |x, y| (x == c && y == c) as u8 as f32), w, h, 5.0);
        let sum: f64 = out.iter().map(|v| *v as f64).sum();
        assert!((sum - 1.0).abs() < 1e-5, "sum {sum}");
        for y in 0..h {
            for x in 0..w {
                let a = out[y * w + x];
                assert!((a - out[y * w + (w - 1 - x)]).abs() < 1e-6);
                assert!((a - out[(h - 1 - y) * w + x]).abs() < 1e-6);
                assert!((a - out[x * w + y]).abs() < 1e-6, "transpose symmetry");
            }
        }
    }

    #[test]
    fn mean_is_preserved_away_from_the_border() {
        let (w, h) = (96, 64);
        let p = plane_from(w, h, |x, y| if (40..56).contains(&x) && (24..40).contains(&y) { 0.8 } else { 0.1 });
        let before = p.iter().map(|v| *v as f64).sum::<f64>() / p.len() as f64;
        let after = blurred(p, w, h, 4.0).iter().map(|v| *v as f64).sum::<f64>() / (w * h) as f64;
        assert!((before - after).abs() < 1e-4, "{before} vs {after}");
    }

    #[test]
    fn step_edge_is_monotone_without_overshoot() {
        let w = 120;
        let out = blurred(plane_from(w, 1, |x, _| if x < 60 { 0.2 } else { 0.9 }), w, 1, 6.0);
        assert!(out.windows(2).all(|p| p[1] >= p[0] - 1e-7), "monotone");
        assert!(out.iter().all(|v| *v >= 0.2 - 1e-6 && *v <= 0.9 + 1e-6), "no over/undershoot");
        assert!(out[0] < 0.21 && out[w - 1] > 0.89);
    }

    #[test]
    fn mirror_symmetry() {
        let (w, h) = (31, 17);
        let img = plane_from(w, h, |x, y| ((x * 7 + y * 13) % 11) as f32 / 10.0);
        let flipped = plane_from(w, h, |x, y| img[y * w + (w - 1 - x)]);
        let a = blurred(img, w, h, 3.0);
        let b = blurred(flipped, w, h, 3.0);
        for y in 0..h {
            for x in 0..w {
                assert!((a[y * w + x] - b[y * w + (w - 1 - x)]).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn tiny_inputs_do_not_panic() {
        for (w, h) in [(1usize, 1usize), (3, 3), (1, 9), (9, 1), (2, 2)] {
            let src: Vec<u8> = (0..w * h).flat_map(|i| [i as u8 * 20, 10, 200, 255]).collect();
            for sigma in [0.5f32, 3.0, 7.5, 60.0] {
                let out = blur_bgrx(&src, w, h, sigma);
                assert_eq!(out.len(), src.len());
            }
        }
        assert!(blur_bgrx(&[], 0, 0, 3.0).is_empty());
    }

    #[test]
    fn black_white_checker_blurs_to_linear_light_mid_grey() {
        // wallpaper-blur-spec 9.2: sRGB 188 +- 1, not 128.
        let (w, h) = (64, 64);
        let src: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let v = if ((i % w) + (i / w)) % 2 == 0 { 255 } else { 0 };
                [v, v, v, 255]
            })
            .collect();
        let out = blur_bgrx(&src, w, h, 7.5);
        let mid = (32 * w + 32) * 4;
        for c in 0..3 {
            assert!((out[mid + c] as i32 - 188).abs() <= 1, "channel {c}: {}", out[mid + c]);
        }
    }

    #[test]
    fn ramp_stays_monotone() {
        let src: Vec<u8> = (0..256).flat_map(|i| [i as u8, i as u8, i as u8, 255]).collect();
        let out = blur_bgrx(&src, 256, 1, 5.0);
        let reds: Vec<u8> = out.chunks_exact(4).map(|p| p[2]).collect();
        assert!(reds.windows(2).all(|p| p[1] >= p[0]), "ramp must not wobble: {reds:?}");
    }

    #[test]
    fn encode_inverts_decode_for_every_level() {
        for level in 0..=255u8 {
            assert_eq!(encode_level(decode_level(level)), level);
        }
    }

    #[test]
    fn budget_smoke_960x540_medium() {
        // wallpaper-blur-spec 9.8: logged, not a gate.
        let (w, h) = (960usize, 540usize);
        let src: Vec<u8> = (0..w * h).flat_map(|i| [(i * 3) as u8, (i / 7) as u8, (i / w) as u8, 255]).collect();
        let started = std::time::Instant::now();
        let out = blur_bgrx(&src, w, h, 7.5);
        eprintln!("blur 960x540 sigma_w 7.5: {:?}", started.elapsed());
        assert_eq!(out.len(), src.len());
    }
}
