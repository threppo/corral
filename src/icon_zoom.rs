//! Per-Corral icon zoom (Corral extra; grid-snapping-spec 2.2 step 7).
//!
//! Ctrl + mouse wheel over a Corral resizes only that Corral's icons. The zoom is an integer percent of
//! the icon size the desktop has, clamped to 50..300, changed by 10 per wheel notch; `None` in the
//! layout means 100 %. The cell and the icon size come from `icon_metrics::cell_metrics`; zoom never
//! changes the Corral's size, the cells reflow and the first visible row is kept.

use crate::icon_metrics::{ZOOM_DEFAULT, ZOOM_MAX, ZOOM_MIN};

pub const DEFAULT: u32 = ZOOM_DEFAULT;
/// Percent per wheel notch.
pub const STEP: u32 = 10;
/// One wheel notch (`WHEEL_DELTA`); high-resolution wheels send fractions that accumulate.
pub const WHEEL_NOTCH: i32 = 120;

/// Effective zoom in percent of a stored value: `None` is 100; a hand-edited value is clamped to the
/// range, so a stored 5 or 9000 cannot produce an unusable Corral.
pub fn percent(stored: Option<u16>) -> u32 {
    stored.map_or(DEFAULT, |v| u32::from(v).clamp(ZOOM_MIN, ZOOM_MAX))
}

/// The value to store for an effective zoom: 100 % is `None`.
pub fn canonical(percent: u32) -> Option<u16> {
    (percent != DEFAULT).then(|| percent.clamp(ZOOM_MIN, ZOOM_MAX) as u16)
}

/// One notch up or down from `stored`, onto the next multiple of 10; at the ends of the range the zoom
/// stays (clamped). Stepping onto 100 % returns `None`: the way back to the default.
pub fn step(stored: Option<u16>, up: bool) -> Option<u16> {
    let current = percent(stored);
    let next = if up { (current / STEP + 1) * STEP } else if current % STEP == 0 { current.saturating_sub(STEP) } else { current / STEP * STEP };
    canonical(next.clamp(ZOOM_MIN, ZOOM_MAX))
}

/// Adds a wheel delta to the carried remainder: returns the new remainder and the whole notches
/// (positive = wheel up). A reversal of direction drops the old remainder.
pub fn accumulate(carry: i32, delta: i32) -> (i32, i32) {
    let carry = if carry != 0 && delta != 0 && (carry > 0) != (delta > 0) { 0 } else { carry };
    let total = carry + delta;
    (total % WHEEL_NOTCH, total / WHEEL_NOTCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_and_unknown_values_are_tolerated() {
        assert_eq!(percent(None), 100);
        assert_eq!(percent(Some(100)), 100);
        assert_eq!(percent(Some(133)), 133, "an older stored step is kept until the next wheel notch");
        assert_eq!(percent(Some(0)), 50, "a hand-edited 0 is clamped");
        assert_eq!(percent(Some(9000)), 300);
        assert_eq!(canonical(100), None);
        assert_eq!(canonical(200), Some(200));
        assert_eq!(canonical(10), Some(50));
        assert_eq!(canonical(999), Some(300));
    }

    #[test]
    fn wheel_changes_the_zoom_by_ten_clamps_and_returns_to_default() {
        assert_eq!(step(None, true), Some(110));
        assert_eq!(step(Some(110), true), Some(120));
        assert_eq!(step(None, false), Some(90));
        assert_eq!(step(Some(300), true), Some(300), "clamped at the top");
        assert_eq!(step(Some(50), false), Some(50), "clamped at the bottom");
        assert_eq!(step(Some(110), false), None, "back at 100 % the zoom is cleared");
        assert_eq!(step(Some(90), true), None);
        assert_eq!(step(Some(133), true), Some(140), "an off-step value snaps to the next multiple of 10");
        assert_eq!(step(Some(133), false), Some(130));
        assert_eq!(step(Some(267), true), Some(270));
        // a full sweep and back is the identity
        let mut z = None;
        for _ in 0..40 { z = step(z, true); }
        assert_eq!(z, Some(300));
        for _ in 0..40 { z = step(z, false); }
        assert_eq!(z, Some(50));
        for _ in 0..5 { z = step(z, true); }
        assert_eq!(z, None);
    }

    #[test]
    fn wheel_remainders_accumulate_and_reverse() {
        assert_eq!(accumulate(0, 120), (0, 1));
        assert_eq!(accumulate(0, -120), (0, -1));
        assert_eq!(accumulate(0, 30), (30, 0));
        assert_eq!(accumulate(30, 90), (0, 1), "four fractions make one notch");
        assert_eq!(accumulate(90, 90), (60, 1));
        assert_eq!(accumulate(60, -30), (-30, 0), "a reversal restarts the remainder");
        assert_eq!(accumulate(0, 240), (0, 2));
        assert_eq!(accumulate(0, 0), (0, 0));
    }
}
