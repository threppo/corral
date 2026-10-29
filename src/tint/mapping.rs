//! Settings mapping and working-size choice (wallpaper-blur-spec 2 and 4.1). Pure.

/// Largest stored radius that is honoured (wallpaper-blur-spec 2).
pub const MAX_RADIUS: f32 = 255.0;
/// Smallest working-size kernel sigma when the working size is reduced (wallpaper-blur-spec 4.1).
#[cfg(test)]
pub const MIN_WORKING_SIGMA: f32 = 3.0;
/// Largest working-size divisor (wallpaper-blur-spec 4.1).
pub const MAX_K: u32 = 8;
/// Divisor of the unblurred copy that Camouflage and the sample use when nothing is blurred
/// (wallpaper-blur-spec 8).
pub const SHARP_K: u32 = 4;

/// Effective radius `R` (wallpaper-blur-spec 2): 0 when the toggle is present and false, else the
/// stored value clamped to 0..=255. A non-finite stored value counts as 0.
pub fn effective_radius(enabled: Option<bool>, stored: f32) -> f32 {
    if enabled == Some(false) || !stored.is_finite() {
        return 0.0;
    }
    stored.clamp(0.0, MAX_RADIUS)
}

/// `sigma_dip = R / 2` (wallpaper-blur-spec 2).
pub fn sigma_dip(radius: f32) -> f32 {
    radius.max(0.0) / 2.0
}

/// `sigma_px = sigma_dip x dpi / 96` for the monitor being blurred (wallpaper-blur-spec 2).
pub fn sigma_px(radius: f32, dpi: u32) -> f32 {
    sigma_dip(radius) * dpi.max(1) as f32 / 96.0
}

/// Working-size divisor `k = clamp(2^floor(log2(sigma_px / 3)), 1, 8)`, 1 when `sigma_px < 6`
/// (wallpaper-blur-spec 4.1). Written with thresholds so no rounding of a logarithm can move a step.
pub fn working_k(sigma_px: f32) -> u32 {
    if !(sigma_px >= 6.0) {
        1
    } else if sigma_px < 12.0 {
        2
    } else if sigma_px < 24.0 {
        4
    } else {
        MAX_K
    }
}

/// Working-size plan for one monitor and radius (wallpaper-blur-spec 4.1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Plan {
    pub k: u32,
    /// Kernel sigma in working pixels (`sigma_px / k`).
    pub sigma_w: f32,
}

pub fn plan(radius: f32, dpi: u32) -> Plan {
    let sp = sigma_px(radius, dpi);
    let k = working_k(sp);
    Plan { k, sigma_w: sp / k as f32 }
}

/// `sigma_w` rounded to 1/16 for the blur cache key (wallpaper-blur-spec 6).
pub fn sigma_key(sigma_w: f32) -> u32 {
    (sigma_w.max(0.0) * 16.0).round() as u32
}

/// Working size for a canvas: `ceil(canvas / k)` per axis, at least 1 (wallpaper-blur-spec 4.1).
pub fn working_size(canvas_w: i32, canvas_h: i32, k: u32) -> (usize, usize) {
    let k = k.max(1) as i64;
    let f = |v: i32| ((v.max(1) as i64 + k - 1) / k).max(1) as usize;
    (f(canvas_w), f(canvas_h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_map_to_documented_sigma_in_dip() {
        // wallpaper-blur-spec 9.1
        for (stored, expected) in [(0.0, 0.0), (15.0, 7.5), (30.0, 15.0), (60.0, 30.0), (120.0, 60.0)] {
            assert_eq!(sigma_dip(effective_radius(Some(true), stored)), expected);
        }
    }

    #[test]
    fn disabled_zero_and_out_of_range_values() {
        assert_eq!(effective_radius(Some(false), 60.0), 0.0);
        assert_eq!(effective_radius(None, 60.0), 60.0);
        assert_eq!(effective_radius(Some(true), 300.0), 255.0);
        assert_eq!(effective_radius(Some(true), -4.0), 0.0);
        assert_eq!(effective_radius(Some(true), f32::NAN), 0.0);
        // A value between steps is honoured as stored.
        assert_eq!(effective_radius(Some(true), 48.5), 48.5);
    }

    #[test]
    fn k_for_every_step_at_the_documented_dpis() {
        // wallpaper-blur-spec 4.1: at 96 dpi Subtle 2, Soft 4, Medium 8, Strong 8; Subtle at 192 dpi 4.
        let at = |r: f32, dpi: u32| plan(r, dpi).k;
        assert_eq!([at(15.0, 96), at(30.0, 96), at(60.0, 96), at(120.0, 96)], [2, 4, 8, 8]);
        assert_eq!(at(15.0, 192), 4);
        assert_eq!(at(0.0, 96), 1);
        assert_eq!(plan(120.0, 96).sigma_w, 7.5);
    }

    #[test]
    fn k_matches_the_written_formula_for_every_step_and_dpi() {
        // wallpaper-blur-spec 4.1 written out with logarithms as an independent oracle.
        for dpi in [96u32, 120, 144, 192, 240] {
            for stored in [15.0f32, 30.0, 60.0, 120.0] {
                let sp = sigma_px(stored, dpi);
                assert_eq!(sp, stored / 2.0 * dpi as f32 / 96.0);
                let expected = if sp < 6.0 { 1 } else { (2f64.powf((sp as f64 / 3.0).log2().floor()) as u32).clamp(1, 8) };
                assert_eq!(working_k(sp), expected, "R {stored} at {dpi} dpi (sigma_px {sp})");
            }
        }
    }

    #[test]
    fn k_is_bounded_and_keeps_the_kernel_wide_enough() {
        for dpi in [96, 120, 144, 192, 240] {
            for r in 0..=255 {
                let p = plan(r as f32, dpi);
                assert!((1..=8).contains(&p.k), "k {} for r {r} dpi {dpi}", p.k);
                assert!(p.k.is_power_of_two());
                if p.k > 1 {
                    assert!(p.sigma_w >= MIN_WORKING_SIGMA - 1e-4, "sigma_w {} for r {r} dpi {dpi}", p.sigma_w);
                }
                let sp = sigma_px(r as f32, dpi);
                if sp < 6.0 {
                    assert_eq!(p.k, 1);
                }
            }
        }
    }

    #[test]
    fn sigma_px_follows_the_monitor_dpi() {
        assert_eq!(sigma_px(60.0, 96), 30.0);
        assert_eq!(sigma_px(60.0, 192), 60.0);
        assert!((sigma_px(60.0, 120) - 37.5).abs() < 1e-4);
    }

    #[test]
    fn working_size_rounds_up_and_never_collapses() {
        assert_eq!(working_size(3840, 2160, 4), (960, 540));
        assert_eq!(working_size(1366, 768, 8), (171, 96));
        assert_eq!(working_size(1, 1, 8), (1, 1));
    }

    #[test]
    fn sigma_key_rounds_to_a_sixteenth() {
        assert_eq!(sigma_key(7.5), 120);
        assert_eq!(sigma_key(7.5 + 0.01), 120);
        assert_ne!(sigma_key(7.5), sigma_key(7.6));
    }
}
