//! Engine adapters for camouflage-spec 3.3, 3.5, 4 and 5.3: per-Corral resolution, palette
//! cache, real-colour fades and the icon cache. Pure maths lives in the parent module.
use super::*;
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::{Mutex, OnceLock, mpsc}, time::{Duration, Instant}};
use crate::motion::Curve;

/// Posted to the engine message window; `lparam` = Corral id, 0 = every Corral.
pub const WM_READY: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 0x173;

/// What one Corral draws with (camouflage-spec 5.3). `body` and `icons` are effective strengths
/// 0..1 after the real-colour fade; `palette` is a `Palette::hash` resolved by `palette()`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Effects {
    pub body: f64,
    pub icons: f64,
    pub palette: u64,
    /// COLORREF of the titlebar text (spec 3.5).
    pub text: u32,
    /// COLORREF of item labels: Auto picks by contrast, otherwise the fixed light label colour.
    pub label: u32,
    /// Mean wallpaper colour behind the Corral (RGB), the backdrop of the Auto title text
    /// (corral-painting-spec 4.4); mid grey when nothing was sampled.
    pub mean: Rgb,
}
impl Default for Effects {
    fn default() -> Self {
        Effects { body: 0.0, icons: 0.0, palette: 0, text: 0x00ff_ffff, label: 0x00ff_ffff, mean: [128; 3] }
    }
}

pub fn colorref_argb(c: u32) -> u32 { 0xff00_0000 | ((c & 255) << 16) | (c & 0xff00) | ((c >> 16) & 255) }
pub fn colorref_rgb(c: u32) -> Rgb { [c as u8, (c >> 8) as u8, (c >> 16) as u8] }
pub fn rgb_colorref(c: Rgb) -> u32 { c[0] as u32 | ((c[1] as u32) << 8) | ((c[2] as u32) << 16) }
/// Persisted strength byte to the percent of spec 5.1 (round(byte * 100 / 255)).
pub fn byte_percent(v: u8) -> u8 { ((v as u32 * 100 + 127) / 255) as u8 }
fn percent(strength: f64) -> u8 { (strength * 100.0).round().clamp(0.0, 100.0) as u8 }

/// Spec 6 geometry: a rectangle outside every display samples the display nearest its centre.
pub fn sampling_region(rect: [i32; 4], monitors: &[[i32; 4]]) -> [i32; 4] {
    if monitors.iter().any(|m| rect[0] < m[2] && rect[2] > m[0] && rect[1] < m[3] && rect[3] > m[1]) { return rect; }
    let x = (rect[0] as i64 + rect[2] as i64) / 2;
    let y = (rect[1] as i64 + rect[3] as i64) / 2;
    monitors.iter().min_by_key(|m| {
        let dx = x - x.clamp(m[0] as i64, m[2] as i64);
        let dy = y - y.clamp(m[1] as i64, m[3] as i64);
        dx as i128 * dx as i128 + dy as i128 * dy as i128
    }).copied().unwrap_or(rect)
}

/// Foreground query only; never changes Explorer or its registry.
pub fn desktop_active() -> bool {
    use windows::Win32::UI::WindowsAndMessaging::*;
    unsafe {
        let h = GetForegroundWindow();
        if h.0.is_null() { return false; }
        let mut name = [0u16; 64];
        let n = GetClassNameW(h, &mut name);
        desktop_surface_class(String::from_utf16_lossy(&name[..n.max(0) as usize]).as_str())
    }
}

/// folder-gate-host-spec 2.5 / camouflage-spec 2: keyboard focus in a Gate's
/// owned popup is still interaction with the desktop's Corrals.
fn desktop_surface_class(name: &str) -> bool {
    matches!(name, "Progman" | "WorkerW" | "CorralPreview" | "CorralGroup" | "CorralSurface")
}

// ---- palette cache (spec 4) -------------------------------------------------------------------

/// Spec 4 cache key. DPI is not part of it: the rectangle is already in physical pixels.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key { id: u64, rect: [i32; 4], generation: u32, blur: u32 }
struct Entry {
    key: Option<Key>,
    pending: Option<Key>,
    wall: Rc<Palette>,
    mean: Rgb,
    tint: Option<(u32, Rc<Palette>)>,
}
struct Work { key: Key, samples: Vec<Rgb>, notify: isize }
struct Derived { palette: Palette, mean: Rgb }

fn derive(samples: &[Rgb]) -> Derived {
    Derived { palette: derive_wallpaper_palette(samples), mean: mean_color(samples) }
}

/// Finished palettes by Corral; a newer result for a Corral replaces an older one.
static READY: OnceLock<Mutex<HashMap<u64, (Key, Derived)>>> = OnceLock::new();
fn ready() -> &'static Mutex<HashMap<u64, (Key, Derived)>> { READY.get_or_init(|| Mutex::new(HashMap::new())) }

fn post(window: isize, id: u64) {
    if window == 0 { return; }
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            windows::Win32::Foundation::HWND(window as *mut _), WM_READY,
            windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(id as isize));
    }
}

/// Spec 4: the palette maths runs on one worker and never inside a paint. The paint thread only
/// reads the capture it already owns (a bounded grid of samples) and hands those over.
fn worker() -> &'static mpsc::Sender<Work> {
    static WORK: OnceLock<mpsc::Sender<Work>> = OnceLock::new();
    WORK.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Work>();
        std::thread::Builder::new().name("corral-palettes".into()).spawn(move || {
            while let Ok(job) = rx.recv() {
                let derived = derive(&job.samples);
                let id = job.key.id;
                if let Ok(mut done) = ready().lock() { done.insert(id, (job.key, derived)); }
                post(job.notify, id);
            }
        }).expect("palette worker");
        tx
    })
}

thread_local! {
    static ENTRIES: RefCell<HashMap<u64, Entry>> = RefCell::new(HashMap::new());
    static PALETTES: RefCell<Palettes> = RefCell::new(Palettes::default());
    static FADES: RefCell<HashMap<u64, Fade>> = RefCell::new(HashMap::new());
    /// (icon content hash, palette hash) -> the full-strength result D (spec 4), bounded by bytes.
    static PROCESSED: RefCell<Processed> = RefCell::new(Processed::default());
}

/// Palettes that stored `Effects` refer to by hash. Least recently used go first, so a palette in
/// current use is never dropped by a burst of new ones.
#[derive(Default)]
struct Palettes { map: HashMap<u64, (Rc<Palette>, u64)>, clock: u64 }
impl Palettes {
    const LIMIT: usize = 256;
    fn insert(&mut self, palette: &Rc<Palette>) {
        self.clock += 1;
        self.map.insert(palette.hash, (palette.clone(), self.clock));
        if self.map.len() > Self::LIMIT {
            let mut stamps: Vec<u64> = self.map.values().map(|(_, s)| *s).collect();
            stamps.sort_unstable();
            let cut = stamps[stamps.len() - Self::LIMIT * 3 / 4];
            self.map.retain(|_, (_, stamp)| *stamp >= cut);
        }
    }
    fn get(&mut self, hash: u64) -> Option<Rc<Palette>> {
        self.clock += 1;
        let clock = self.clock;
        self.map.get_mut(&hash).map(|(p, stamp)| { *stamp = clock; p.clone() })
    }
}

/// The icon cache. Evicts oldest-first beyond `LIMIT` bytes, and by palette when one changes.
#[derive(Default)]
struct Processed { map: HashMap<(u64, u64), Rc<Vec<u8>>>, order: std::collections::VecDeque<(u64, u64)>, bytes: usize }
impl Processed {
    const LIMIT: usize = 32 << 20;
    fn get_or_insert_with(&mut self, key: (u64, u64), make: impl FnOnce() -> Vec<u8>) -> Rc<Vec<u8>> {
        if let Some(found) = self.map.get(&key) { return found.clone(); }
        let value = Rc::new(make());
        self.bytes += value.len();
        self.map.insert(key, value.clone());
        self.order.push_back(key);
        while self.bytes > Self::LIMIT && self.order.len() > 1 {
            if let Some(old) = self.order.pop_front().and_then(|k| self.map.remove(&k)) { self.bytes -= old.len(); }
        }
        value
    }
    fn drop_palette(&mut self, palette: u64) {
        self.map.retain(|k, v| { let keep = k.1 != palette; if !keep { self.bytes -= v.len(); } keep });
        self.order.retain(|k| k.1 != palette);
    }
}

fn register(palette: &Rc<Palette>) { PALETTES.with(|p| p.borrow_mut().insert(palette)); }

/// Forget everything held for Corrals that no longer exist.
pub fn prune(live: &[u64]) {
    ENTRIES.with(|e| e.borrow_mut().retain(|id, _| live.contains(id)));
    FADES.with(|f| f.borrow_mut().retain(|id, _| live.contains(id)));
    if let Ok(mut r) = ready().lock() { r.retain(|id, _| live.contains(id)); }
}

/// The wallpaper palette and sampled mean colour of one Corral. A new key is derived off the paint
/// path; until it is ready the previous pair stays (no flash). The first pair is derived at once.
/// `freeze` (move/resize in progress) keeps the settled pair; the drop changes the key (spec 4).
fn wall_for(id: u64, rect: [i32; 4], freeze: bool, notify: isize) -> (Rc<Palette>, Rgb) {
    ENTRIES.with(|entries| {
        let mut entries = entries.borrow_mut();
        let e = entries.entry(id).or_insert_with(|| Entry {
            key: None, pending: None, wall: Rc::new(derive_tint_palette([128; 3])), mean: [128; 3], tint: None,
        });
        if !freeze {
            if let Some(pending) = e.pending.clone() {
                let done = ready().lock().ok().and_then(|mut r| match r.get(&id) {
                    Some((key, _)) if *key == pending => r.remove(&id).map(|(_, derived)| derived),
                    _ => None,
                });
                if let Some(derived) = done { adopt(e, pending, derived); }
            }
            let key = Key { id, rect, generation: crate::tint::wallpaper_generation(),
                blur: crate::tint::blur_radius().map_or(0, f32::to_bits) };
            if e.key.as_ref() != Some(&key) && e.pending.as_ref() != Some(&key) {
                let samples = crate::tint::camouflage_source().sample_rect(key.rect, 32);
                if e.key.is_none() {
                    adopt(e, key, derive(&samples));
                } else if worker().send(Work { key: key.clone(), samples, notify }).is_ok() {
                    e.pending = Some(key);
                }
            }
        }
        register(&e.wall);
        (e.wall.clone(), e.mean)
    })
}

fn adopt(e: &mut Entry, key: Key, derived: Derived) {
    let old = e.wall.hash;
    if old != derived.palette.hash { PROCESSED.with(|c| c.borrow_mut().drop_palette(old)); }
    e.key = Some(key);
    e.pending = None;
    e.wall = Rc::new(derived.palette);
    e.mean = derived.mean;
}

fn tint_for(id: u64, chosen: u32) -> Rc<Palette> {
    ENTRIES.with(|entries| {
        let mut entries = entries.borrow_mut();
        let e = entries.entry(id).or_insert_with(|| Entry {
            key: None, pending: None, wall: Rc::new(derive_tint_palette([128; 3])), mean: [128; 3], tint: None,
        });
        if e.tint.as_ref().is_none_or(|(c, _)| *c != chosen) {
            e.tint = Some((chosen, Rc::new(derive_tint_palette(colorref_rgb(chosen)))));
        }
        let palette = e.tint.as_ref().expect("tint palette").1.clone();
        register(&palette);
        palette
    })
}

// ---- real-colour fades (spec 2, 6) -------------------------------------------------------------

/// Timer id on the engine message window that steps running fades (one timer for all Corrals).
pub const FADE_TIMER: usize = 0x7101;
const FADE_FRAME_MS: u32 = 16;

/// Shaped by the motion curves (OUT to real colours, IN back to the effect: motion-sorting-spec 1.1); a
/// retarget starts from the current value so re-entry never jumps.
struct Fade { from: f64, target: f64, start: Instant, duration: Duration, settled: bool, curve: Curve }
impl Fade {
    fn value(&self) -> f64 { fade_value(self.from, self.target, self.start.elapsed().as_secs_f64(), self.duration.as_secs_f64(), self.curve) }
    fn running(&self) -> bool { self.start.elapsed() < self.duration }
}

/// One tick of the fade timer: the Corrals to repaint (running fades, and each fade once more when
/// it lands) and whether any fade is still running; when none is, the caller kills the timer.
pub fn fade_tick() -> (Vec<u64>, bool) {
    FADES.with(|f| {
        let mut repaint = Vec::new();
        let mut any = false;
        for (id, fade) in f.borrow_mut().iter_mut() {
            if fade.running() { any = true; repaint.push(*id); }
            else if !fade.settled { fade.settled = true; repaint.push(*id); }
        }
        (repaint, any)
    })
}
fn fade_value(from: f64, target: f64, elapsed: f64, duration: f64, curve: Curve) -> f64 {
    if duration <= 0.0 { return target; }
    from + (target - from) * crate::motion::ease(curve, elapsed / duration)
}

/// 1 = Camouflage fully shown, 0 = real colours. Real colours fade in over 120 ms (OUT), back over 200 ms
/// (IN); with Windows animations off both are instant (motion-sorting-spec 7).
fn visibility(id: u64, restore: bool, notify: isize) -> f64 {
    FADES.with(|f| {
        let mut map = f.borrow_mut();
        let target = if restore { 0.0 } else { 1.0 };
        let fade = map.entry(id).or_insert_with(|| Fade { from: 1.0, target: 1.0, start: Instant::now(), duration: Duration::from_millis(200), settled: true, curve: Curve::In });
        if fade.target != target {
            let from = fade.value();
            let (base, curve) = if restore { (crate::motion::CAMOUFLAGE_TO_REAL_MS, Curve::Out) } else { (crate::motion::CAMOUFLAGE_TO_EFFECT_MS, Curve::In) };
            let duration = Duration::from_millis(crate::motion::duration(base) as u64);
            *fade = Fade { from, target, start: Instant::now(), duration, settled: false, curve };
            if notify != 0 {
                unsafe {
                    windows::Win32::UI::WindowsAndMessaging::SetTimer(
                        windows::Win32::Foundation::HWND(notify as *mut _), FADE_TIMER, FADE_FRAME_MS, None);
                }
            }
        }
        fade.value()
    })
}

// ---- resolution (spec 5.3) ----------------------------------------------------------------------

#[derive(Debug)]
struct Choices { on: bool, tint: bool, chosen: u32, strength: u8 }
/// colour-state-spec 2.2: the shared resolver supplies every fallback.
fn choices(s: &crate::settings::Settings, look: &crate::color_state::Effective) -> Choices {
    let (on, tint) = (look.blend_icons, look.tint_icons);
    Choices { on, tint, chosen: look.tint_in_use(), strength: if on { byte_percent(s.desktop_blend_icon_opacity) } else if tint { 100 } else { 0 } }
}

/// `rect` is the Corral's outer rectangle in virtual-screen pixels (already through
/// `sampling_region`). `hovered` is the pointer being inside it.
pub fn resolve(s: &crate::settings::Settings, g: &crate::model::Group, rect: [i32; 4], hovered: bool, freeze: bool, notify: isize) -> Effects {
    let look = crate::color_state::effective_of(s, &g.style);
    let options = choices(s, &look);
    let treated = (options.on || options.tint) && options.strength > 0;
    let auto = look.text_auto;
    let use_chosen = !options.on || s.desktop_blend_uses_tint_icons_color;
    let mut fx = Effects { text: look.text_color, ..Default::default() };
    // The sampled region is needed for the wallpaper source and for the Auto text colour.
    let sampled = (treated && !use_chosen) || auto;
    let wall = sampled.then(|| wall_for(g.id, rect, freeze, notify));
    let mean = wall.as_ref().map_or([128; 3], |(_, mean)| *mean);
    fx.mean = mean;
    if treated {
        let palette = match wall {
            Some((wall, _)) if !use_chosen => wall,
            _ => tint_for(g.id, options.chosen),
        };
        // Plain tint never fades back to the real colours (spec 3.4).
        let visibility = if options.on {
            visibility(g.id, (s.desktop_blend_color_on_mouseover && hovered) || (s.desktop_blend_color_on_active && desktop_active()), notify)
        } else { 1.0 };
        let strength = if options.on { options.strength as f64 / 100.0 * visibility } else { 1.0 };
        fx.icons = strength;
        fx.body = if options.on && look.blend_background { strength } else { 0.0 };
        fx.palette = palette.hash;
    }
    fx.label = 0x00ff_ffff;
    if auto {
        let background = colorref_rgb(recolor_colorref(look.color, fx));
        let alpha = crate::group_paint::see_through_alpha(look.see_through as i64);
        let colour = auto_text_color(composite_backdrop(background, mean, 255 - alpha));
        fx.text = rgb_colorref([(colour >> 16) as u8, (colour >> 8) as u8, colour as u8]);
        fx.label = fx.text;
    }
    fx
}

pub fn palette(fx: Effects) -> Option<Rc<Palette>> { PALETTES.with(|p| p.borrow_mut().get(fx.palette)) }

/// Straight-pixel adapter for surfaces (scrollbar track, thumb): premultiplied BGRA bytes.
pub fn apply_pixels(pixels: &mut [u8], fx: Effects, body: bool) {
    let s = percent(if body { fx.body } else { fx.icons });
    if s == 0 { return; }
    let Some(p) = palette(fx) else { return };
    apply_duotone_bytes(&p, s, pixels);
}

/// One COLORREF (background, titlebar stop) through the same mapping; alpha is not part of it.
pub fn recolor_colorref(c: u32, fx: Effects) -> u32 {
    let s = percent(fx.body);
    let Some(p) = palette(fx).filter(|_| s > 0) else { return c };
    let argb = recolor_color(&p, s, colorref_argb(c));
    rgb_colorref([(argb >> 16) as u8, (argb >> 8) as u8, argb as u8])
}

// ---- icon cache (spec 4) --------------------------------------------------------------------------

fn words(bytes: &[u8]) -> Vec<u32> { bytes.chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect() }
fn bytes_of(words: &[u32]) -> Vec<u8> { words.iter().flat_map(|w| w.to_le_bytes()).collect() }

/// The recoloured copy of a premultiplied BGRA icon. The full-strength result D is cached by icon
/// content and palette hash; any strength or fade step is one lerp (spec 3.3, 4).
pub fn cached_pixels(original: &[u8], fx: Effects) -> Vec<u8> {
    use std::hash::{Hash, Hasher};
    let s = percent(fx.icons);
    let Some(p) = palette(fx).filter(|_| s > 0) else { return original.to_vec() };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    original.hash(&mut hasher);
    let key = (hasher.finish(), fx.palette);
    let full = PROCESSED.with(|cache| cache.borrow_mut().get_or_insert_with(key, || {
        let mut d = words(original);
        apply_duotone(&p, 100, &mut d);
        bytes_of(&d)
    }));
    let mut out = vec![0; original.len()];
    lerp_bytes(original, &full, s, &mut out);
    out
}


#[cfg(test)]
mod tests {
    fn choices_of(s: &crate::settings::Settings, st: &crate::model::GroupStyle) -> Choices {
        choices(s, &crate::color_state::effective_of(s, st))
    }
    use super::*;

    #[test]
    fn active_restore_includes_folder_gate_focus() {
        for class in ["Progman", "WorkerW", "CorralPreview", "CorralGroup", "CorralSurface"] {
            assert!(desktop_surface_class(class));
        }
        for class in ["", "CabinetWClass", "Notepad", "SysListView32"] {
            assert!(!desktop_surface_class(class));
        }
    }



    #[test]
    fn fades_are_monotone_and_reentry_continues_without_a_jump() {
        let values: Vec<_> = (0..=120).map(|ms| fade_value(1.0, 0.0, ms as f64, 120.0, Curve::Out)).collect();
        assert_eq!(values[0], 1.0);
        assert_eq!(values[120], 0.0);
        assert!(values.windows(2).all(|w| w[1] <= w[0]));
        let current = values[60];
        assert_eq!(fade_value(current, 1.0, 0.0, 200.0, Curve::In), current, "re-entry mid-fade continues from the current value");
        assert_eq!(fade_value(current, 1.0, 200.0, 200.0, Curve::In), 1.0);
        let back: Vec<_> = (0..=200).map(|ms| fade_value(current, 1.0, ms as f64, 200.0, Curve::In)).collect();
        assert!(back.windows(2).all(|w| w[1] >= w[0]));
        assert_eq!(fade_value(1.0, 0.0, 0.0, 0.0, Curve::Out), 0.0, "animations off: instant");
    }

    #[test]
    fn hover_fade_state_machine_restores_and_returns() {
        let id = 0xfade;
        assert_eq!(visibility(id, false, 0), 1.0);
        let at_restore = visibility(id, true, 0);
        assert!(at_restore <= 1.0 && at_restore >= 0.0);
        FADES.with(|f| {
            let mut f = f.borrow_mut();
            let fade = f.get_mut(&id).unwrap();
            assert_eq!((fade.target, fade.duration.as_millis()), (0.0, 120));
            fade.start -= Duration::from_millis(500);
        });
        assert_eq!(visibility(id, true, 0), 0.0, "fully restored: bit-identical original (strength 0)");
        let back = visibility(id, false, 0);
        FADES.with(|f| assert_eq!(f.borrow()[&id].duration.as_millis(), 200));
        assert!(back <= 0.01, "return starts from the current value, not from 1");
    }

    #[test]
    fn palette_registry_evicts_least_recently_used_and_keeps_the_one_in_use() {
        let mut registry = Palettes::default();
        let keep = Rc::new(derive_tint_palette([9, 9, 9]));
        registry.insert(&keep);
        for i in 0..(Palettes::LIMIT as u8) {
            registry.insert(&Rc::new(derive_tint_palette([i, 40, 200 - i / 2])));
            assert!(registry.get(keep.hash).is_some(), "a palette in current use stays");
        }
        assert!(registry.map.len() <= Palettes::LIMIT);
    }

    #[test]
    fn icon_cache_is_bounded_by_bytes_and_drops_by_palette() {
        let mut cache = Processed::default();
        let chunk = Processed::LIMIT / 4;
        for i in 0..10u64 { cache.get_or_insert_with((i, i % 2), || vec![0; chunk]); }
        assert!(cache.bytes <= Processed::LIMIT && cache.map.len() == cache.order.len());
        let before = cache.bytes;
        cache.drop_palette(1);
        assert!(cache.bytes < before && cache.map.keys().all(|k| k.1 != 1));
        assert_eq!(cache.bytes, cache.map.values().map(|v| v.len()).sum::<usize>());
    }

    #[test]
    fn fade_timer_repaints_running_fades_once_more_when_they_land_and_prune_forgets_corrals() {
        let id = 0xf00d;
        visibility(id, false, 0);
        visibility(id, true, 0);
        let (ids, running) = fade_tick();
        assert!(running && ids.contains(&id));
        FADES.with(|f| f.borrow_mut().get_mut(&id).unwrap().start -= Duration::from_millis(500));
        let (ids, running) = fade_tick();
        assert!(!running && ids.contains(&id), "the landing frame is painted");
        let (ids, _) = fade_tick();
        assert!(!ids.contains(&id), "then it stops being repainted");
        prune(&[]);
        assert!(FADES.with(|f| f.borrow().is_empty()));
    }

    #[test]
    fn percent_byte_scale_endpoints_and_default() {
        assert_eq!(byte_percent(0), 0);
        assert_eq!(byte_percent(255), 100);
        assert_eq!(byte_percent(153), 60);
        assert_eq!(percent(0.6), 60);
    }

    #[test]
    fn per_corral_resolution_beats_globals_and_plain_tint_ignores_hover() {
        let mut s = crate::settings::Settings::default();
        s.desktop_blend_icons = true;
        let mut st = crate::model::GroupStyle::default();
        st.blend_icons = Some(0);
        st.tint_icons = Some(1);
        let c = choices_of(&s, &st);
        assert!(!c.on && c.tint);
        assert_eq!(c.strength, 100);
        st.blend_icons = Some(1);
        s.desktop_blend_icons = false;
        assert!(choices_of(&s, &st).on);
        s.desktop_tint_icons_color_source = "custom".into();
        s.desktop_tint_icons_color = 0x112233;
        assert_eq!(choices_of(&s, &st).chosen, 0x112233);
        st.tint_icons_color_source = Some("corral".into());
        st.color = Some(0xabcdef);
        assert_eq!(choices_of(&s, &st).chosen, 0xabcdef);
        st.tint_icons_color_source = Some("custom".into());
        st.tint_icons_color = Some(0x445566);
        assert_eq!(choices_of(&s, &st).chosen, 0x445566, "a per-Corral custom colour beats the global one");
        st.tint_icons_color_source = None;
        st.tint_icons_color = None;
        assert_eq!(choices_of(&s, &st).chosen, 0x112233, "absent per-Corral source inherits the global source");
        st.blend_icons = None;
        st.tint_icons = None;
        let c = choices_of(&s, &st);
        assert!(!c.on && !c.tint && c.strength == 0, "both off: original icons");
    }

    #[test]
    fn resolve_is_off_for_originals_and_blocks_nothing_without_a_window() {
        let s = crate::settings::Settings::default();
        let mut s = s;
        s.text_color_auto = false; // Auto samples the wallpaper capture; tests never touch the desktop.
        let mut layout = crate::model::Layout::new();
        let id = layout.add_group("QA".into(), "m".into(), Default::default(), crate::model::GroupContent::Items { items: vec![] });
        let g = layout.groups[0].clone();
        assert_eq!(g.id, id);
        let fx = resolve(&s, &g, [0, 0, 10, 10], false, false, 0);
        assert_eq!((fx.body, fx.icons, fx.palette), (0.0, 0.0, 0));
        let mut bytes = vec![1, 2, 3, 255, 9, 9, 9, 128];
        assert_eq!(cached_pixels(&bytes, fx), bytes);
        apply_pixels(&mut bytes, fx, false);
        assert_eq!(bytes, [1, 2, 3, 255, 9, 9, 9, 128]);
    }

    #[test]
    fn nearest_monitor_fallback_uses_whole_region_and_preserves_partial_rect() {
        let monitors = [[-1920, -200, 0, 880], [0, 0, 3840, 2160]];
        assert_eq!(sampling_region([-3000, 50, -2500, 500], &monitors), monitors[0]);
        let partial = [-2200, -100, -1500, 500];
        assert_eq!(sampling_region(partial, &monitors), partial);
    }

    #[test]
    fn icon_cache_matches_direct_rounding_at_every_strength() {
        let p = Rc::new(derive_tint_palette([200, 90, 30]));
        register(&p);
        let original = [0xff000000u32, 0x80010203, 0, 0xfffefefe, 0x01000001, 0x40302010];
        let bytes = bytes_of(&original);
        for s in 0..=100u8 {
                let fx = Effects { palette: p.hash, icons: s as f64 / 100.0, ..Default::default() };
                let mut expected = original;
                apply_duotone(&p, s, &mut expected);
                assert_eq!(cached_pixels(&bytes, fx), bytes_of(&expected), "strength {s}");
        }
    }

    #[test]
    fn palette_cache_key_changes_with_every_listed_trigger() {
        let k = Key { id: 7, rect: [0, 0, 100, 100], generation: 1, blur: 0 };
        assert_eq!(k, k.clone());
        let mut changed = k.clone();
        changed.rect[0] = 1;
        assert_ne!(k, changed);
        let mut changed = k.clone();
        changed.generation += 1;
        assert_ne!(k, changed);
        let mut changed = k.clone();
        changed.blur = 60f32.to_bits();
        assert_ne!(k, changed);
        let mut changed = k.clone();
        changed.id = 8;
        assert_ne!(k, changed);
    }

    #[test]
    fn background_colour_follows_the_same_mapping_and_hover_restores_it() {
        let p = Rc::new(derive_tint_palette([40, 120, 200]));
        register(&p);
        let full = Effects { palette: p.hash, body: 1.0, icons: 1.0, ..Default::default() };
        let mapped = recolor_colorref(0x00203040, full);
        assert_ne!(mapped, 0x00203040);
        assert_eq!(recolor_colorref(0x00203040, Effects { body: 0.0, ..full }), 0x00203040);
        let expected = recolor_color(&p, 100, colorref_argb(0x00203040));
        assert_eq!(colorref_argb(mapped), expected);
    }
}
