//! The wallpaper engine (wallpaper-blur-spec 3.3, 6): one background worker keeps a per-monitor set of
//! images (S and B) and swaps a new snapshot in atomically. The UI thread only reads the current
//! snapshot: no decoding, no blur, no timers except a short cross-fade pump on the worker.

use super::cache::{MonitorCache, MonitorInput, build_monitor};
use super::composite::{Blend, Layer};
use super::crossfade::Crossfade;
use super::level::{Level, blend_levels};
use super::monitors::{self, Monitor};
use super::source::{self, SourceId, WicDecoder};
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

/// Wallpaper-change debounce: the file may still be written (wallpaper-blur-spec 3.3).
pub const DEBOUNCE_MS: u64 = 250;
/// Slideshow polling period (wallpaper-blur-spec 3.3).
pub const SLIDESHOW_POLL: Duration = Duration::from_secs(10);
/// A failed read is tried again after this long, at most [`MAX_RETRIES`] times in a row.
const RETRY_DELAY: Duration = Duration::from_millis(1000);
const MAX_RETRIES: u32 = 5;
/// Cross-fade frame pacing on the worker.
const FADE_FRAME: Duration = Duration::from_millis(33);

// ---- what is needed (wallpaper-blur-spec 3.3 "Laziness") -------------------------------------------

/// What has to be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Need {
    /// Blurred copies (effective radius > 0).
    pub blur: bool,
    /// Unblurred `SHARP_K` copies (Camouflage without blur, or a visible preview).
    pub sharp: bool,
}

impl Need {
    pub fn any(&self) -> bool {
        self.blur || self.sharp
    }
}

/// Truth table of the laziness rule: build only while the radius is above zero, or Camouflage samples
/// the wallpaper, or the Camouflage preview / Personalization sample is visible.
pub fn needs(radius: f32, camouflage_uses_wallpaper: bool, preview_visible: bool) -> Need {
    let blur = radius > 0.0;
    Need { blur, sharp: preview_visible || (camouflage_uses_wallpaper && !blur) }
}

// ---- snapshots ------------------------------------------------------------------------------------

/// Images of one monitor.
pub struct MonitorSet {
    pub rect: [i32; 4],
    pub dpi: u32,
    pub primary: bool,
    pub source: SourceId,
    pub blurred: Option<Arc<Level>>,
    pub s: Option<Arc<Level>>,
    pub sharp: Option<Arc<Level>>,
}

/// Everything the UI reads, replaced as a whole.
pub struct Snapshot {
    pub monitors: Vec<MonitorSet>,
    pub radius: f32,
}

fn layer_of(rect: [i32; 4], level: &Level) -> Layer<'_> {
    Layer { rect, k: level.k, w: level.w, h: level.h, px: &level.bgrx, solid: level.solid }
}

impl Snapshot {
    /// Layers of the blurred copies (empty when nothing is blurred).
    pub fn blur_layers(&self) -> Vec<Layer<'_>> {
        self.monitors.iter().filter_map(|m| m.blurred.as_deref().map(|l| layer_of(m.rect, l))).collect()
    }

    /// Layers Camouflage samples: B when the radius is above zero, else S (wallpaper-blur-spec 8).
    pub fn sample_layers(&self) -> Vec<Layer<'_>> {
        self.monitors
            .iter()
            .filter_map(|m| {
                let level = if self.radius > 0.0 { m.blurred.as_deref() } else { m.sharp.as_deref() };
                level.map(|l| layer_of(m.rect, l))
            })
            .collect()
    }

    /// Per-monitor content signature: rect, DPI-derived k and the hashes of S and B.
    pub fn signature(&self) -> Vec<(([i32; 4], u32), Option<u64>, Option<u64>, Option<u64>)> {
        self.monitors
            .iter()
            .map(|m| ((m.rect, m.dpi), m.blurred.as_ref().map(|l| l.hash), m.s.as_ref().map(|l| l.hash), m.sharp.as_ref().map(|l| l.hash)))
            .collect()
    }

    #[cfg(test)]
    pub fn memory_bytes(&self) -> usize {
        let mut seen: HashSet<*const Level> = HashSet::new();
        let mut total = 0;
        for m in &self.monitors {
            for l in [&m.blurred, &m.s, &m.sharp].into_iter().flatten() {
                if seen.insert(Arc::as_ptr(l)) {
                    total += l.bytes();
                }
            }
        }
        total
    }
}

/// True when the new snapshot shows another picture (not only another blur, size or arrangement):
/// the cross-fade applies to wallpaper changes only (wallpaper-blur-spec 6, "Swap look").
pub fn picture_changed(old: &Snapshot, new: &Snapshot) -> bool {
    new.monitors.iter().any(|n| old.monitors.iter().find(|o| o.rect == n.rect).is_some_and(|o| o.source != n.source))
}

/// Mix of two snapshots by the weight of the newer one, for a change that arrives mid-fade
/// (wallpaper-blur-spec 6: a retarget continues from the shown mix).
pub fn mix_snapshots(from: &Arc<Snapshot>, to: &Arc<Snapshot>, w_new: f32) -> Arc<Snapshot> {
    let monitors = to
        .monitors
        .iter()
        .map(|n| {
            let blurred = match (from.monitors.iter().find(|o| o.rect == n.rect).and_then(|o| o.blurred.as_ref()), n.blurred.as_ref()) {
                (Some(a), Some(b)) => Some(blend_levels(a, b, w_new)),
                (_, b) => b.cloned(),
            };
            MonitorSet { rect: n.rect, dpi: n.dpi, primary: n.primary, source: n.source.clone(), blurred, s: n.s.clone(), sharp: n.sharp.clone() }
        })
        .collect();
    Arc::new(Snapshot { monitors, radius: to.radius })
}

// ---- shared state ---------------------------------------------------------------------------------

struct Shared {
    radius: f32,
    camouflage: bool,
    preview: bool,
    /// Corrals whose scrollbar is sampling the wallpaper behind it (scrollbar-spec 5).
    scrollbars: HashSet<u64>,
    pending: Option<Instant>,
    current: Option<Arc<Snapshot>>,
    fade: Option<(Arc<Snapshot>, Crossfade)>,
    slideshow: bool,
    worker_started: bool,
    /// The worker took a job and has not published yet.
    busy: bool,
    /// The settings window process: it only needs the preview picture, never the blurred copies.
    settings_only: bool,
}

impl Shared {
    fn need(&self) -> Need {
        let n = needs(self.radius, self.camouflage || !self.scrollbars.is_empty(), self.preview);
        if self.settings_only { Need { blur: false, sharp: self.preview } } else { n }
    }
}

struct Hub {
    shared: Mutex<Shared>,
    /// Wakes the worker (new request).
    wake: Condvar,
    /// Wakes UI callers waiting for a snapshot.
    published: Condvar,
}

fn hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(|| Hub {
        shared: Mutex::new(Shared {
            radius: 0.0,
            camouflage: false,
            preview: false,
            scrollbars: HashSet::new(),
            pending: None,
            current: None,
            fade: None,
            slideshow: false,
            worker_started: false,
            busy: false,
            settings_only: false,
        }),
        wake: Condvar::new(),
        published: Condvar::new(),
    })
}

fn lock() -> MutexGuard<'static, Shared> {
    hub().shared.lock().unwrap_or_else(|p| p.into_inner())
}

static GENERATION: AtomicU32 = AtomicU32::new(0);
static NOTIFY_WINDOW: AtomicIsize = AtomicIsize::new(0);
/// The image decoder could not be created: no pass can succeed, so nothing wakes the worker again.
static DECODER_FAILED: AtomicBool = AtomicBool::new(false);

/// Content generation: moves only when a rebuilt image differs from the one it replaces
/// (wallpaper-blur-spec 6).
pub fn generation() -> u32 {
    GENERATION.load(Ordering::Relaxed)
}

/// The window that gets `WM_READY` (lparam 0 = repaint everything) when a new snapshot is shown.
pub fn set_notify_window(hwnd: isize) {
    NOTIFY_WINDOW.store(hwnd, Ordering::Relaxed);
    // A snapshot finished before the window existed: tell it now.
    if hwnd != 0 && lock().current.is_some() {
        notify();
    }
}

fn notify() {
    let window = NOTIFY_WINDOW.load(Ordering::Relaxed);
    if window == 0 {
        return;
    }
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
    unsafe {
        let _ = PostMessageW(HWND(window as *mut _), crate::camouflage::runtime::WM_READY, WPARAM(0), LPARAM(0));
    }
}

fn start_worker(shared: &mut Shared) {
    // Unit tests drive the pure parts directly; no background thread touches the desktop.
    if shared.worker_started || cfg!(test) {
        return;
    }
    shared.worker_started = true;
    let spawned = std::thread::Builder::new().name("corral-wallpaper".into()).spawn(worker_main);
    if spawned.is_err() {
        shared.worker_started = false;
    }
}

/// Ask for a rebuild after `delay_ms`. A later request never delays an earlier one; the newest inputs
/// are read when the worker runs, so older queued requests are dropped (wallpaper-blur-spec 6).
pub fn request(delay_ms: u64) {
    let mut s = lock();
    if !s.need().any() || DECODER_FAILED.load(Ordering::Relaxed) {
        return;
    }
    let due = Instant::now() + Duration::from_millis(delay_ms);
    s.pending = Some(s.pending.map_or(due, |p| p.min(due)));
    start_worker(&mut s);
    drop(s);
    hub().wake.notify_all();
}

/// A reader found no snapshot: ask once, unless a build is already queued or running.
pub fn ensure() {
    let idle = {
        let s = lock();
        s.current.is_none() && s.pending.is_none() && !s.busy
    };
    if idle {
        request(0);
    }
}

/// The effective radius changed (settings). A change to or from zero also starts or releases work.
pub fn set_radius(radius: f32) {
    let changed = {
        let mut s = lock();
        let changed = s.radius != radius;
        s.radius = radius;
        changed
    };
    if changed {
        release_if_idle();
        request(0);
    }
}

pub fn radius() -> f32 {
    lock().radius
}

/// This process is the settings window: it keeps the radius for its sample but never builds blurred copies.
pub fn set_settings_only() {
    lock().settings_only = true;
}

/// Camouflage samples the wallpaper (sticky until [`set_camouflage_wanted`] clears it).
pub fn set_camouflage_wanted(wanted: bool) {
    let was = {
        let mut s = lock();
        std::mem::replace(&mut s.camouflage, wanted)
    };
    if wanted && !was {
        request(0);
    } else if !wanted && was {
        release_if_idle();
    }
}

/// A Corral's scrollbar starts or stops sampling the wallpaper behind it.
pub fn set_scrollbar_user(id: u64, wanted: bool) {
    let (changed, first) = {
        let mut s = lock();
        let was_empty = s.scrollbars.is_empty();
        let changed = if wanted { s.scrollbars.insert(id) } else { s.scrollbars.remove(&id) };
        (changed, was_empty)
    };
    if changed && wanted && first {
        request(0);
    } else if changed && !wanted {
        release_if_idle();
    }
}

/// The Camouflage preview or Personalization sample is on screen.
pub fn set_preview_visible(visible: bool) {
    let was = {
        let mut s = lock();
        std::mem::replace(&mut s.preview, visible)
    };
    if visible && !was {
        request(0);
    } else if !visible && was {
        release_if_idle();
    }
}

/// Nothing needed any more: drop everything that is kept (wallpaper-blur-spec 3.3 "Laziness").
fn release_if_idle() {
    let mut s = lock();
    if !s.need().any() {
        s.current = None;
        s.fade = None;
        s.pending = None;
        s.slideshow = false;
        drop(s);
        hub().wake.notify_all();
    }
}

/// The current snapshot, if one has been built.
pub fn current() -> Option<Arc<Snapshot>> {
    lock().current.clone()
}

/// The current snapshot plus, while a wallpaper cross-fade runs, the outgoing one and the new weight.
pub fn current_with_fade() -> Option<(Arc<Snapshot>, Option<(Arc<Snapshot>, f32)>)> {
    let s = lock();
    let current = s.current.clone()?;
    let fade = s.fade.as_ref().and_then(|(from, fade)| {
        let now = crate::motion::now_ms();
        (!fade.finished(now)).then(|| (from.clone(), fade.new_weight(now)))
    });
    Some((current, fade))
}

/// Wait up to `timeout` for a snapshot that satisfies `ready` (used by the settings window, which
/// opens rarely and may wait a moment for the primary monitor's picture).
pub fn wait_for(timeout: Duration, ready: &dyn Fn(&Snapshot) -> bool) -> Option<Arc<Snapshot>> {
    let deadline = Instant::now() + timeout;
    let mut guard = lock();
    loop {
        if let Some(c) = guard.current.as_ref().filter(|c| ready(c)) {
            return Some(c.clone());
        }
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        let (g, _) = hub().published.wait_timeout(guard, deadline - now).unwrap_or_else(|p| p.into_inner());
        guard = g;
    }
}

/// Draw the blurred backdrop under a painted surface; returns false when no blurred copy is ready.
pub fn with_blur_blend<R>(f: impl FnOnce(&Blend) -> R) -> Option<R> {
    let (snap, fade) = current_with_fade()?;
    let layers = snap.blur_layers();
    if layers.is_empty() {
        return None;
    }
    let from_layers = fade.as_ref().map(|(from, w)| (from.blur_layers(), *w));
    let blend = Blend { layers: &layers, from: from_layers.as_ref().map(|(l, w)| (l.as_slice(), *w)) };
    Some(f(&blend))
}

// ---- the worker -----------------------------------------------------------------------------------

static LOGGED: OnceLock<Mutex<HashSet<SourceId>>> = OnceLock::new();

/// Log a source problem once per source (wallpaper-blur-spec 3.1).
fn log_once(id: &SourceId, why: &str) {
    let seen = LOGGED.get_or_init(|| Mutex::new(HashSet::new()));
    if seen.lock().map(|mut s| s.insert(id.clone())).unwrap_or(false) {
        crate::app::log(&format!("wallpaper: {id:?} falls back to a flat colour: {why}"));
    }
}

/// Inputs the worker read when it woke up.
struct Job {
    radius: f32,
    need: Need,
    previous: Option<Arc<Snapshot>>,
    was_fading: Option<(Arc<Snapshot>, Crossfade)>,
}

/// Block until a request is due, a slideshow poll is due, or a cross-fade frame is due.
fn wait_for_job() -> Job {
    let mut guard = lock();
    guard.busy = false;
    loop {
        let now = Instant::now();
        if guard.fade.as_ref().is_some_and(|(_, f)| f.finished(crate::motion::now_ms())) {
            guard.fade = None;
            drop(guard);
            notify();
            guard = lock();
        }
        if guard.pending.is_some_and(|due| due <= now) {
            guard.pending = None;
            guard.busy = true;
            return Job {
                radius: guard.radius,
                need: guard.need(),
                previous: guard.current.clone(),
                was_fading: guard.fade.clone(),
            };
        }
        let active = guard.need().any();
        let fading = guard.fade.is_some();
        let polling = !fading && guard.pending.is_none() && guard.slideshow && active;
        let timeout = if fading {
            Some(FADE_FRAME)
        } else if let Some(due) = guard.pending {
            Some(due.saturating_duration_since(now))
        } else if polling {
            Some(SLIDESHOW_POLL)
        } else {
            None
        };
        match timeout {
            Some(d) => {
                let (g, result) = hub().wake.wait_timeout(guard, d).unwrap_or_else(|p| p.into_inner());
                guard = g;
                if result.timed_out() {
                    if guard.fade.is_some() {
                        // One frame of the cross-fade: repaint, then look again.
                        drop(guard);
                        notify();
                        guard = lock();
                    } else if polling && guard.pending.is_none() {
                        // Slideshow poll: compare the sources again.
                        guard.pending = Some(Instant::now());
                    }
                }
            }
            None => guard = hub().wake.wait(guard).unwrap_or_else(|p| p.into_inner()),
        }
    }
}

fn worker_main() {
    source::init_apartment();
    let decoder = WicDecoder::new().ok();
    if decoder.is_none() {
        DECODER_FAILED.store(true, Ordering::Relaxed);
    }
    let mut caches: Vec<MonitorCache> = Vec::new();
    let mut attempts = 0u32;
    loop {
        let Job { radius, need, previous, was_fading } = wait_for_job();
        if !need.any() {
            caches.clear();
            continue;
        }
        let Some(decoder) = decoder.as_ref() else {
            log_once(&SourceId::Solid(0), "no image decoder");
            DECODER_FAILED.store(true, Ordering::Relaxed);
            lock().pending = None;
            continue;
        };
        let (snapshot, slideshow, retry) = build_snapshot(&mut caches, decoder, radius, need);
        let snapshot = Arc::new(snapshot);
        let mut s = lock();
        // A read that failed just now (a locked file) is tried again, a few times at most.
        if retry && attempts < MAX_RETRIES {
            attempts += 1;
            if s.pending.is_none() {
                s.pending = Some(Instant::now() + RETRY_DELAY);
            }
        } else if !retry {
            attempts = 0;
        }
        s.slideshow = slideshow;
        if s.radius != radius || s.need() != need {
            // Inputs moved while building: make sure a newer pass runs, drop this result.
            if s.pending.is_none() {
                s.pending = Some(Instant::now());
            }
            continue;
        }
        let content_changed = previous.as_ref().is_none_or(|p| p.radius != snapshot.radius || p.signature() != snapshot.signature());
        if !content_changed {
            continue;
        }
        let fading = previous.as_ref().is_some_and(|p| radius > 0.0 && picture_changed(p, &snapshot));
        s.fade = match (&previous, fading && crate::motion::animations_enabled()) {
            (Some(prev), true) => {
                // A change mid-fade starts from the mix on screen.
                let now = crate::motion::now_ms();
                let from = match &was_fading {
                    Some((older, fade)) if !fade.finished(now) => mix_snapshots(older, prev, fade.new_weight(now)),
                    _ => prev.clone(),
                };
                Some((from, Crossfade::start(now, true)))
            }
            _ => None,
        };
        s.current = Some(snapshot);
        GENERATION.fetch_add(1, Ordering::Relaxed);
        drop(s);
        hub().published.notify_all();
        notify();
    }
}

/// Build the snapshot for the current monitors and desktop state. Returns it, whether a slideshow is
/// running, and whether a read failed just now and should be tried again.
fn build_snapshot(caches: &mut Vec<MonitorCache>, decoder: &WicDecoder, radius: f32, need: Need) -> (Snapshot, bool, bool) {
    let list: Vec<Monitor> = monitors::enumerate();
    let rects: Vec<[i32; 4]> = list.iter().map(|m| m.rect).collect();
    let virtual_rect = monitors::virtual_rect(&list);
    let query = source::query_desktop(&rects);
    caches.resize_with(list.len(), MonitorCache::default);
    caches.truncate(list.len());
    let mut sets = Vec::with_capacity(list.len());
    let mut retry = false;
    for (i, monitor) in list.iter().enumerate() {
        let source = source::pick_source(query.paths.get(i).map_or("", String::as_str), &query.system_path, query.background, &source::file_stamp);
        let input = MonitorInput {
            canvas: monitor.rect,
            dpi: monitor.dpi,
            source: source.clone(),
            background: query.background,
            mode: query.mode,
            virtual_rect,
            radius: if need.blur { radius } else { 0.0 },
            want_sharp: need.sharp,
        };
        let built = build_monitor(&mut caches[i], &input, decoder, &mut log_once);
        retry |= built.retry;
        sets.push(MonitorSet {
            rect: monitor.rect,
            dpi: monitor.dpi,
            primary: monitor.primary,
            source,
            blurred: built.blurred,
            s: built.s,
            sharp: built.sharp,
        });
    }
    (Snapshot { monitors: sets, radius: if need.blur { radius } else { 0.0 } }, query.slideshow, retry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn laziness_truth_table() {
        // wallpaper-blur-spec 3.3 / 9.7: (radius, camouflage uses wallpaper, preview visible) -> (blur, sharp)
        let t = |r, c, p| {
            let n = needs(r, c, p);
            (n.blur, n.sharp)
        };
        assert_eq!(t(0.0, false, false), (false, false), "nothing needed: nothing decoded or kept");
        assert_eq!(t(60.0, false, false), (true, false));
        assert_eq!(t(0.0, true, false), (false, true), "Camouflage without blur reads S");
        assert_eq!(t(60.0, true, false), (true, false), "Camouflage with blur reads B");
        assert_eq!(t(0.0, false, true), (false, true), "a visible preview needs S");
        assert_eq!(t(60.0, true, true), (true, true));
        assert!(!needs(0.0, false, false).any() && needs(0.0, false, true).any());
    }

    fn set(rect: [i32; 4], source: SourceId, hash: u8) -> MonitorSet {
        MonitorSet {
            rect,
            dpi: 96,
            primary: false,
            source,
            blurred: Some(Arc::new(Level::new(1, 1, 1, vec![hash, hash, hash, 255], None))),
            s: None,
            sharp: None,
        }
    }

    fn file(name: &str) -> SourceId {
        SourceId::File { path: name.into(), mtime: 1, size: 1 }
    }

    #[test]
    fn picture_changed_ignores_blur_and_arrangement_changes() {
        let a = Snapshot { monitors: vec![set([0, 0, 10, 10], file("a"), 1)], radius: 60.0 };
        let same_picture_other_blur = Snapshot { monitors: vec![set([0, 0, 10, 10], file("a"), 9)], radius: 30.0 };
        let other_picture = Snapshot { monitors: vec![set([0, 0, 10, 10], file("b"), 1)], radius: 60.0 };
        let moved = Snapshot { monitors: vec![set([5, 0, 15, 10], file("b"), 1)], radius: 60.0 };
        assert!(!picture_changed(&a, &same_picture_other_blur));
        assert!(picture_changed(&a, &other_picture));
        assert!(!picture_changed(&a, &moved), "a monitor that did not exist before has no picture to fade from");
    }

    #[test]
    fn signature_follows_content_and_arrangement() {
        let a = Snapshot { monitors: vec![set([0, 0, 10, 10], file("a"), 1)], radius: 60.0 };
        let same = Snapshot { monitors: vec![set([0, 0, 10, 10], file("a"), 1)], radius: 60.0 };
        let other = Snapshot { monitors: vec![set([0, 0, 10, 10], file("a"), 2)], radius: 60.0 };
        assert_eq!(a.signature(), same.signature(), "an identical re-read changes nothing");
        assert_ne!(a.signature(), other.signature());
    }

    #[test]
    fn mid_fade_mix_starts_from_what_is_shown() {
        let from = Arc::new(Snapshot { monitors: vec![set([0, 0, 10, 10], file("a"), 0)], radius: 60.0 });
        let to = Arc::new(Snapshot { monitors: vec![set([0, 0, 10, 10], file("b"), 200)], radius: 60.0 });
        let mixed = mix_snapshots(&from, &to, 0.5);
        assert_eq!(mixed.monitors[0].blurred.as_ref().unwrap().bgrx[0], 100);
        assert_eq!(mixed.monitors[0].source, file("b"));
    }

    #[test]
    fn sample_layers_read_b_when_blurred_and_s_otherwise() {
        let mut m = set([0, 0, 10, 10], file("a"), 50);
        m.sharp = Some(Arc::new(Level::new(1, 1, 4, vec![9, 9, 9, 255], None)));
        let blurred = Snapshot { monitors: vec![m], radius: 30.0 };
        assert_eq!(blurred.sample_layers()[0].px[0], 50);
        let mut m = set([0, 0, 10, 10], file("a"), 50);
        m.sharp = Some(Arc::new(Level::new(1, 1, 4, vec![9, 9, 9, 255], None)));
        let sharp = Snapshot { monitors: vec![m], radius: 0.0 };
        assert_eq!(sharp.sample_layers()[0].px[0], 9);
        assert_eq!(sharp.sample_layers()[0].k, 4);
    }

    #[test]
    fn memory_counts_shared_levels_once() {
        let shared = Arc::new(Level::new(10, 10, 4, vec![0; 400], None));
        let m = MonitorSet {
            rect: [0, 0, 40, 40],
            dpi: 96,
            primary: true,
            source: file("a"),
            blurred: None,
            s: Some(shared.clone()),
            sharp: Some(shared),
        };
        assert_eq!(Snapshot { monitors: vec![m], radius: 0.0 }.memory_bytes(), 400);
    }
}
