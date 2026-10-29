//! Cross-fade between the old and the new blurred wallpaper after a wallpaper change
//! (wallpaper-blur-spec 6, "Swap look"). Pure: times are plain milliseconds.

use crate::motion::{Curve, ease};

/// 400 ms, OUT curve (motion-sorting-spec 1.1).
pub const DURATION_MS: f64 = 400.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crossfade {
    start_ms: f64,
    duration_ms: f64,
}

impl Crossfade {
    /// A fade starting at `now_ms`. `animate` false (reduced motion) makes it instant.
    pub fn start(now_ms: f64, animate: bool) -> Crossfade {
        Crossfade { start_ms: now_ms, duration_ms: if animate { DURATION_MS } else { 0.0 } }
    }

    /// Weight of the new picture, 0..=1.
    pub fn new_weight(&self, now_ms: f64) -> f32 {
        if self.duration_ms <= 0.0 {
            return 1.0;
        }
        ease(Curve::Out, (now_ms - self.start_ms) / self.duration_ms) as f32
    }

    /// `(old, new)` weights; they always sum to 1.
    #[cfg(test)]
    pub fn weights(&self, now_ms: f64) -> (f32, f32) {
        let new = self.new_weight(now_ms);
        (1.0 - new, new)
    }

    pub fn finished(&self, now_ms: f64) -> bool {
        self.duration_ms <= 0.0 || now_ms - self.start_ms >= self.duration_ms
    }
}

/// Mix of two single values by the fade at `now_ms` (what the screen shows for one pixel).
#[cfg(test)]
pub fn shown(old: f32, new: f32, fade: &Crossfade, now_ms: f64) -> f32 {
    let (a, b) = fade.weights(now_ms);
    old * a + new * b
}

#[cfg(test)]
/// A new wallpaper change arrives at `now_ms` while `fade` is still running between `old` and `new`:
/// the next fade starts from what is on screen, so nothing jumps. Returns the value to treat as the
/// next fade's "old" (for pixels; the engine does the same for whole images with `blend_levels`).
pub fn retarget_from(old: f32, new: f32, fade: &Crossfade, now_ms: f64) -> f32 {
    shown(old, new, fade, now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lasts_400_ms_on_the_out_curve() {
        let f = Crossfade::start(1000.0, true);
        assert_eq!(f.new_weight(1000.0), 0.0);
        assert_eq!(f.new_weight(1400.0), 1.0);
        assert!(!f.finished(1399.0) && f.finished(1400.0));
        let mid = f.new_weight(1200.0);
        assert!(mid > 0.5 && mid < 1.0, "OUT arrives fast: {mid}");
        assert_eq!(f.new_weight(1200.0), ease(Curve::Out, 0.5) as f32);
        assert!(f.new_weight(900.0) == 0.0 && f.new_weight(5000.0) == 1.0);
    }

    #[test]
    fn weights_sum_to_one() {
        let f = Crossfade::start(0.0, true);
        for t in (0..=450).step_by(10) {
            let (a, b) = f.weights(t as f64);
            assert!((a + b - 1.0).abs() < 1e-6 && (0.0..=1.0).contains(&a) && (0.0..=1.0).contains(&b));
        }
    }

    #[test]
    fn reduced_motion_swaps_at_once() {
        let f = Crossfade::start(10.0, false);
        assert_eq!(f.weights(10.0), (0.0, 1.0));
        assert!(f.finished(10.0));
    }

    #[test]
    fn retarget_continues_from_the_shown_mix() {
        // A -> B at t=0; at t=150 a change to C arrives: the next fade starts from the mix on screen.
        let first = Crossfade::start(0.0, true);
        let (a, b, c) = (0.0f32, 1.0f32, 0.5f32);
        let on_screen = shown(a, b, &first, 150.0);
        let second = Crossfade::start(150.0, true);
        let from = retarget_from(a, b, &first, 150.0);
        assert_eq!(from, on_screen);
        assert_eq!(shown(from, c, &second, 150.0), on_screen, "no jump at the retarget");
        assert!((shown(from, c, &second, 550.0) - c).abs() < 1e-6, "arrives at C");
    }
}
