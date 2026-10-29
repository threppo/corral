//! Shared motion vocabulary (motion-sorting-spec 1.1, 7, 8): three Bezier curves plus the Ranges
//! bounce, retargetable tweens, the Windows "animations off" switch and the timer gate that keeps one
//! animation timer per window. Pure except [`refresh_system`]; every time is a plain `f64` in
//! milliseconds so tests run on a fake clock.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// One animation frame (`WM_TIMER` pacing, spec 8).
pub const FRAME_MS: f64 = 16.0;

/// Cubic Bezier curves from (0,0) to (1,1) (spec 1.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    /// Things arriving, opening, settling.
    Out,
    /// Things leaving, closing (mirror of `Out`).
    In,
    /// Reserved for moves between two resting places (no use yet, spec 1.1).
    #[allow(dead_code)]
    InOut,
    /// Ranges "Bounce": one overshoot of about 3 % of the travel.
    Bounce,
}

impl Curve {
    /// Control points `(x1, y1, x2, y2)`.
    pub fn points(self) -> (f64, f64, f64, f64) {
        match self {
            Curve::Out => (0.10, 0.50, 0.30, 1.00),
            Curve::In => (0.70, 0.00, 0.90, 0.50),
            Curve::InOut => (0.40, 0.00, 0.60, 1.00),
            Curve::Bounce => (0.34, 1.30, 0.64, 1.00),
        }
    }
}

fn bezier(a: f64, b: f64, t: f64) -> f64 {
    let u = 1.0 - t;
    3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
}

fn bezier_slope(a: f64, b: f64, t: f64) -> f64 {
    let u = 1.0 - t;
    3.0 * u * u * a + 6.0 * u * t * (b - a) + 3.0 * t * t * (1.0 - b)
}

/// `ease(curve, p)`: solves x(t) = p (Newton inside a shrinking bracket, bisection when Newton
/// stalls, tolerance 1e-5) and returns y(t). `p` is clamped to 0..1; the endpoints are exact.
pub fn ease(curve: Curve, p: f64) -> f64 {
    if !(p > 0.0) {
        return 0.0;
    }
    if p >= 1.0 {
        return 1.0;
    }
    let (x1, y1, x2, y2) = curve.points();
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    let mut t = p;
    for _ in 0..64 {
        let err = bezier(x1, x2, t) - p;
        if err.abs() < 1e-7 {
            break;
        }
        if err > 0.0 { hi = t } else { lo = t }
        let slope = bezier_slope(x1, x2, t);
        let newton = t - err / slope;
        t = if slope.abs() > 1e-9 && newton > lo && newton < hi { newton } else { 0.5 * (lo + hi) };
    }
    bezier(y1, y2, t)
}

/// A value moving from `from` to `to` along a curve. Times are monotonic milliseconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tween {
    from: f64,
    to: f64,
    start: f64,
    duration: f64,
    curve: Curve,
}

impl Tween {
    /// A value at rest.
    pub fn at_rest(value: f64) -> Tween {
        Tween { from: value, to: value, start: 0.0, duration: 0.0, curve: Curve::Out }
    }

    pub fn target(&self) -> f64 {
        self.to
    }

    pub fn finished(&self, now: f64) -> bool {
        self.duration <= 0.0 || now - self.start >= self.duration
    }

    /// The displayed value; the target at once when the duration is zero.
    pub fn value(&self, now: f64) -> f64 {
        if self.finished(now) {
            return self.to;
        }
        self.from + (self.to - self.from) * ease(self.curve, (now - self.start) / self.duration)
    }

    /// Start towards `to` from where the picture is now, over exactly `duration_ms` (0 = jump).
    pub fn retarget_exact(&mut self, now: f64, to: f64, duration_ms: f64, curve: Curve) {
        let from = self.value(now);
        *self = Tween { from, to, start: now, duration: duration_ms.max(0.0), curve };
    }

    /// Retarget with the interruption rule of spec 3: duration = `base_ms` x the remaining distance
    /// (as a fraction of `full_range`, at most 1), at least one frame; zero when `animate` is false
    /// (Windows animations off, spec 7) or nothing is left to travel.
    pub fn retarget_scaled(&mut self, now: f64, to: f64, base_ms: f64, curve: Curve, full_range: f64, animate: bool) {
        let distance = ((to - self.value(now)).abs() / full_range.abs().max(f64::EPSILON)).min(1.0);
        self.retarget_exact(now, to, scaled_duration(base_ms, distance, animate), curve);
    }

    /// Ranges rule (spec 5): duration = `base_ms` x clamp(distance, 0.5, 1.0), distance in pages.
    pub fn retarget_clamped(&mut self, now: f64, to: f64, base_ms: f64, curve: Curve, full_range: f64, animate: bool) {
        let distance = ((to - self.value(now)).abs() / full_range.abs().max(f64::EPSILON)).clamp(0.5, 1.0);
        let duration = if (to - self.value(now)).abs() == 0.0 { 0.0 } else { scaled_duration(base_ms, distance, animate) };
        self.retarget_exact(now, to, duration, curve);
    }
}

/// `base_ms` x `fraction`, one frame at least, 0 when animations are off or nothing remains.
pub fn scaled_duration(base_ms: f64, fraction: f64, animate: bool) -> f64 {
    if !animate || base_ms <= 0.0 || fraction <= 0.0 {
        return 0.0;
    }
    (base_ms * fraction.min(1.0)).max(FRAME_MS)
}

/// Durations of the spec (1.1).
pub const FOLD_OPEN_MS: f64 = 180.0;
pub const FOLD_CLOSE_MS: f64 = 140.0;
pub const CLEAR_VIEW_OUT_MS: f64 = 150.0;
pub const CLEAR_VIEW_IN_MS: f64 = 220.0;
pub const RANGES_SLIDE_MS: f64 = 240.0;
pub const RANGES_BOUNCE_MS: f64 = 360.0;
/// Camouflage real-colour fades keep their own durations (spec 1.1).
pub const CAMOUFLAGE_TO_REAL_MS: f64 = 120.0;
pub const CAMOUFLAGE_TO_EFFECT_MS: f64 = 200.0;

// ---- Windows animation switch and clock ----------------------------------------------------------

static ANIMATIONS_ON: AtomicBool = AtomicBool::new(true);

/// False when Windows animations are off: every duration is 0 (spec 7).
pub fn animations_enabled() -> bool {
    ANIMATIONS_ON.load(Ordering::Relaxed)
}

/// `duration(base) -> 0` while animations are off (spec 7).
pub fn duration(base_ms: f64) -> f64 {
    if animations_enabled() { base_ms } else { 0.0 }
}

/// Read `SPI_GETCLIENTAREAANIMATION` (startup, and `WM_SETTINGCHANGE` with `SPI_SETCLIENTAREAANIMATION`).
/// Returns the new value. A failed query leaves animations on.
pub fn refresh_system() -> bool {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};
    let mut on = BOOL(1);
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, Some((&mut on as *mut BOOL).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
    }
    .is_ok();
    let value = !ok || on.as_bool();
    ANIMATIONS_ON.store(value, Ordering::Relaxed);
    value
}

/// Monotonic milliseconds since the first call (`Instant` is the performance counter on Windows).
pub fn now_ms() -> f64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

// ---- timer gate (spec 8) -------------------------------------------------------------------------

/// The two calls a window needs from the timer API.
pub trait TimerApi {
    fn set(&mut self, id: usize, interval_ms: u32);
    fn kill(&mut self, id: usize);
}

/// One animation timer per window however many animators run: requested when the first animator
/// starts, released when the last one ends.
#[derive(Clone, Debug)]
pub struct TimerGate {
    id: usize,
    active: Vec<u64>,
}

impl TimerGate {
    pub fn new(id: usize) -> TimerGate {
        TimerGate { id, active: Vec::new() }
    }

    /// The animators that are active now.
    pub fn keys(&self) -> Vec<u64> {
        self.active.clone()
    }

    /// Animator `key` became active.
    pub fn start(&mut self, key: u64, api: &mut dyn TimerApi) {
        if self.active.contains(&key) {
            return;
        }
        if self.active.is_empty() {
            api.set(self.id, FRAME_MS as u32);
        }
        self.active.push(key);
    }

    /// Animator `key` ended (a no-op for one that never started).
    pub fn end(&mut self, key: u64, api: &mut dyn TimerApi) {
        let before = self.active.len();
        self.active.retain(|k| *k != key);
        if before != 0 && self.active.is_empty() {
            api.kill(self.id);
        }
    }

    /// Every animator ended at once (window reset).
    pub fn clear(&mut self, api: &mut dyn TimerApi) {
        if !self.active.is_empty() {
            self.active.clear();
            api.kill(self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CURVES: [Curve; 4] = [Curve::Out, Curve::In, Curve::InOut, Curve::Bounce];

    #[test]
    fn every_curve_has_exact_endpoints_and_clamps_its_input() {
        for c in CURVES {
            assert_eq!(ease(c, 0.0), 0.0);
            assert_eq!(ease(c, 1.0), 1.0);
            assert_eq!(ease(c, -3.0), 0.0);
            assert_eq!(ease(c, 7.0), 1.0);
            assert_eq!(ease(c, f64::NAN), 0.0);
        }
    }

    #[test]
    fn out_in_and_inout_are_monotone_and_mirror_each_other() {
        for c in [Curve::Out, Curve::In, Curve::InOut] {
            let mut last = 0.0;
            for i in 0..=1000 {
                let v = ease(c, i as f64 / 1000.0);
                assert!(v >= last - 1e-12 && (0.0..=1.0).contains(&v), "{c:?} at {i}");
                last = v;
            }
        }
        assert!(ease(Curve::Out, 0.5) > 0.5);
        assert!(ease(Curve::In, 0.5) < 0.5);
        assert!((ease(Curve::InOut, 0.5) - 0.5).abs() < 1e-4);
        for i in 0..=100 {
            let p = i as f64 / 100.0;
            assert!((ease(Curve::In, p) - (1.0 - ease(Curve::Out, 1.0 - p))).abs() < 1e-4, "mirror at {p}");
        }
    }

    #[test]
    fn bounce_overshoots_about_three_percent_late_in_the_run() {
        let (mut max, mut at) = (0.0_f64, 0.0_f64);
        for i in 0..=1000 {
            let p = i as f64 / 1000.0;
            let v = ease(Curve::Bounce, p);
            if v > max { (max, at) = (v, p); }
        }
        assert!(max > 1.02 && max < 1.04, "max {max}");
        assert!(at > 0.60 && at < 0.75, "at {at}");
        assert_eq!(ease(Curve::Bounce, 1.0), 1.0);
    }

    #[test]
    fn a_zero_duration_tween_reads_its_target_at_once() {
        let mut t = Tween::at_rest(0.25);
        t.retarget_exact(10.0, 1.0, 0.0, Curve::Out);
        assert_eq!(t.value(10.0), 1.0);
        assert!(t.finished(10.0));
        t.retarget_scaled(20.0, 0.0, 180.0, Curve::In, 1.0, false);
        assert_eq!(t.value(20.0), 0.0);
    }

    #[test]
    fn retarget_duration_is_base_times_the_remaining_distance() {
        let mut t = Tween::at_rest(0.0);
        t.retarget_scaled(0.0, 1.0, 180.0, Curve::Out, 1.0, true);
        assert!(!t.finished(179.0) && t.finished(180.0));
        // half way back: the distance left is what the picture shows, not the full range
        let mid = t.value(60.0);
        t.retarget_scaled(60.0, 0.0, 140.0, Curve::In, 1.0, true);
        assert!(!t.finished(60.0 + 140.0 * mid - 1.0) && t.finished(60.0 + 140.0 * mid + 0.5));
        // a tiny remainder still takes one frame
        let mut tiny = Tween::at_rest(0.99);
        tiny.retarget_scaled(0.0, 1.0, 180.0, Curve::Out, 1.0, true);
        assert!(!tiny.finished(FRAME_MS - 1.0) && tiny.finished(FRAME_MS));
    }

    #[test]
    fn clamped_retarget_never_runs_faster_than_half_nor_slower_than_the_base() {
        let mut t = Tween::at_rest(0.0);
        t.retarget_clamped(0.0, 0.1, 240.0, Curve::Out, 1.0, true);
        assert!(!t.finished(119.0) && t.finished(120.0));
        let mut t = Tween::at_rest(0.0);
        t.retarget_clamped(0.0, 3.0, 240.0, Curve::Out, 1.0, true);
        assert!(!t.finished(239.0) && t.finished(240.0));
        let mut t = Tween::at_rest(2.0);
        t.retarget_clamped(5.0, 2.0, 240.0, Curve::Out, 1.0, true);
        assert!(t.finished(5.0), "already there");
    }

    #[test]
    fn retargeting_never_jumps_and_repeated_retargets_converge() {
        for (curve, base) in [(Curve::Out, 180.0), (Curve::In, 140.0), (Curve::Bounce, 360.0)] {
            for at in [0.0, 0.4, 0.9] {
                let mut t = Tween::at_rest(0.0);
                t.retarget_exact(0.0, 1.0, 200.0, curve);
                let now = 200.0 * at;
                let before = t.value(now);
                t.retarget_scaled(now, 0.0, base, curve, 1.0, true);
                assert!((t.value(now) - before).abs() < 1e-6, "{curve:?} at {at}");
                let mut clamped = Tween::at_rest(0.0);
                clamped.retarget_exact(0.0, 1.0, 200.0, curve);
                clamped.retarget_clamped(now, 2.0, base, curve, 1.0, true);
                assert!((clamped.value(now) - before).abs() < 1e-6);
            }
            // retargeting at every frame still lands, inside the curve's own bounds
            let mut t = Tween::at_rest(0.0);
            let mut now = 0.0;
            for _ in 0..400 {
                t.retarget_scaled(now, 1.0, base, curve, 1.0, true);
                now += FRAME_MS;
                let v = t.value(now);
                let limit = if curve == Curve::Bounce { 1.05 } else { 1.0 };
                assert!((0.0..=limit).contains(&v), "{curve:?} {v}");
            }
            assert!((t.value(now) - 1.0).abs() < 1e-3, "{curve:?} converges");
        }
    }

    #[test]
    fn animations_off_makes_every_duration_zero() {
        assert_eq!(scaled_duration(180.0, 1.0, false), 0.0);
        assert_eq!(scaled_duration(180.0, 0.0, true), 0.0);
        assert_eq!(scaled_duration(180.0, 1.0, true), 180.0);
        // flipping the switch mid-animation finishes it at its target on the next retarget
        let mut t = Tween::at_rest(0.0);
        t.retarget_scaled(0.0, 1.0, 180.0, Curve::Out, 1.0, true);
        t.retarget_scaled(50.0, 1.0, 180.0, Curve::Out, 1.0, false);
        assert_eq!(t.value(50.0), 1.0);
    }

    #[derive(Default)]
    struct FakeTimers {
        calls: Vec<(&'static str, usize)>,
    }

    impl TimerApi for FakeTimers {
        fn set(&mut self, id: usize, _interval_ms: u32) {
            self.calls.push(("set", id));
        }
        fn kill(&mut self, id: usize) {
            self.calls.push(("kill", id));
        }
    }

    #[test]
    fn one_timer_per_window_whatever_the_animator_count() {
        let mut api = FakeTimers::default();
        let mut gate = TimerGate::new(9);
        gate.start(1, &mut api);
        gate.start(2, &mut api);
        gate.start(2, &mut api);
        gate.start(3, &mut api);
        assert_eq!(api.calls, vec![("set", 9)]);
        gate.end(2, &mut api);
        gate.end(1, &mut api);
        assert!(!gate.keys().is_empty() && api.calls.len() == 1);
        gate.end(3, &mut api);
        assert!(gate.keys().is_empty());
        assert_eq!(api.calls, vec![("set", 9), ("kill", 9)]);
        gate.end(3, &mut api);
        assert_eq!(api.calls.len(), 2, "ending a stranger releases nothing");
    }
}
