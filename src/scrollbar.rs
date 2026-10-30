//! Corral overlay scrollbar, implemented from scrollbar-spec sections 2-8.
use crate::render::{Surface, present_layered};
use std::{cell::RefCell, collections::HashMap, rc::Rc, time::Instant};
use windows::{
    Win32::{
        Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        Graphics::Gdi::{COLOR_HOTLIGHT, COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{
                GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT,
                TrackMouseEvent, VK_CONTROL, VK_MENU,
            },
            WindowsAndMessaging::*,
        },
    },
    core::w,
};

const TIMER_FRAME: usize = 0x7301;
const TIMER_WAKE: usize = 0x7302;
const TIMER_PAGE: usize = 0x7303;
const WM_DELIVER: u32 = WM_APP + 0x61;
const WM_MOUSELEAVE: u32 = 0x02a3;
pub const BASE_OPACITY: u32 = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Thumb { pos: i32 },
}

/// scrollbar-spec 2: all output coordinates are whole device pixels, relative to the hit lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
impl PixelRect {
    fn width(self) -> i32 {
        self.right - self.left
    }
    fn height(self) -> i32 {
        self.bottom - self.top
    }
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}
#[derive(Clone, Copy, Debug)]
pub struct BarLayout {
    pub track: PixelRect,
    pub thumb: PixelRect,
    pub lane: i32,
    pub max_offset: f64,
}
fn scale(dpi: u32) -> f64 {
    dpi.max(1) as f64 / 96.0
}
fn px(dip: f64, dpi: u32) -> i32 {
    (dip * scale(dpi)).round() as i32
}
pub fn window_width(dpi: u32, thin: bool) -> i32 {
    px(if thin { 10.0 } else { 14.0 }, dpi).max(1)
}
fn overflow(view: f64, content: f64) -> bool {
    view > 0.0 && content > view + 0.5
}
pub fn needed(content: i32, area: i32) -> bool {
    overflow(area as f64, content as f64)
}
pub fn bar_layout(
    view: f64,
    content: f64,
    offset: f64,
    dpi: u32,
    thin: bool,
    corner: f64,
) -> Option<BarLayout> {
    layout_widen(view, content, offset, dpi, thin, corner, 0.0)
}
fn layout_widen(
    view: f64,
    content: f64,
    offset: f64,
    dpi: u32,
    thin: bool,
    corner: f64,
    wide: f64,
) -> Option<BarLayout> {
    if !view.is_finite() || !content.is_finite() || !overflow(view, content) {
        return None;
    }
    let h = px(view, dpi).max(1);
    let inset = px(corner.max(6.0), dpi).max(0).min((h - 1) / 2);
    let len = (h - 2 * inset).max(1);
    let lane = window_width(dpi, thin);
    let gap = px(if thin { 2.0 } else { 3.0 }, dpi).max(0).min(lane - 1);
    let (rest, hot, min_len) = if thin {
        (2.0, 5.0, 20.0)
    } else {
        (4.0, 8.0, 28.0)
    };
    let width = px(rest + (hot - rest) * wide.clamp(0.0, 1.0), dpi)
        .max(1)
        .min(lane - gap);
    let thumb_len = ((len as f64 * view / content).round() as i32)
        .max(px(min_len, dpi))
        .min(len);
    let max_offset = content - view;
    let top = inset
        + ((len - thumb_len) as f64 * offset.clamp(0.0, max_offset) / max_offset).round() as i32;
    let right = lane - gap;
    Some(BarLayout {
        track: PixelRect {
            left: right - width,
            top: inset,
            right,
            bottom: h - inset,
        },
        thumb: PixelRect {
            left: right - width,
            top,
            right,
            bottom: top + thumb_len,
        },
        lane,
        max_offset,
    })
}

/// scrollbar-spec 3/4: clock supplied by the host, so reversal and reduced motion are testable.
#[derive(Clone, Copy, Debug)]
struct Tween {
    from: f64,
    to: f64,
    at: u64,
    duration: u64,
    ease_in: bool,
}
impl Tween {
    fn fixed(v: f64) -> Self {
        Self {
            from: v,
            to: v,
            at: 0,
            duration: 0,
            ease_in: false,
        }
    }
    fn value(self, now: u64) -> f64 {
        let t = if self.duration == 0 {
            1.0
        } else {
            (now.saturating_sub(self.at) as f64 / self.duration as f64).clamp(0.0, 1.0)
        };
        let t = if self.ease_in {
            t * t
        } else {
            1.0 - (1.0 - t) * (1.0 - t)
        };
        self.from + (self.to - self.from) * t
    }
    fn active(self, now: u64) -> bool {
        self.from != self.to && now < self.at + self.duration
    }
    fn to(&mut self, v: f64, now: u64, duration: u64, ease_in: bool) {
        if self.to != v {
            *self = Self {
                from: self.value(now),
                to: v,
                at: now,
                duration,
                ease_in,
            };
        }
    }
}
#[derive(Clone, Debug)]
pub struct Visibility {
    alpha: Tween,
    width: Tween,
    inside: bool,
    lane: bool,
    drag: bool,
    hide_at: Option<u64>,
    narrow_at: Option<u64>,
    pub hide_mode: bool,
    pub reduced: bool,
    pub high_contrast: bool,
}
impl Default for Visibility {
    fn default() -> Self {
        Self {
            alpha: Tween::fixed(0.0),
            width: Tween::fixed(0.0),
            inside: false,
            lane: false,
            drag: false,
            hide_at: None,
            narrow_at: None,
            hide_mode: true,
            reduced: false,
            high_contrast: false,
        }
    }
}
impl Visibility {
    fn duration(&self, ms: u64) -> u64 {
        if self.reduced || self.high_contrast {
            0
        } else {
            ms
        }
    }
    pub fn pointer(&mut self, inside: bool, lane: bool, now: u64) {
        self.tick(now);
        if inside != self.inside {
            self.inside = inside;
            if inside {
                self.hide_at = None;
                self.alpha.to(1.0, now, self.duration(120), false);
            } else if !self.drag {
                self.hide_at = Some(now + 800);
            }
        }
        if lane != self.lane {
            self.lane = lane;
            if lane {
                self.narrow_at = None;
                self.width.to(1.0, now, self.duration(100), false);
            } else if !self.drag {
                self.narrow_at = Some(now + 250);
            }
        }
        // The hidden lane reveals immediately (scrollbar-spec 2).
        if lane {
            self.alpha = Tween::fixed(1.0);
        }
    }
    pub fn scroll(&mut self, now: u64) {
        self.alpha.to(1.0, now, self.duration(120), false);
        self.hide_at = Some(now + 1000);
    }
    pub fn dragging(&mut self, on: bool, now: u64) {
        self.drag = on;
        if on {
            self.alpha = Tween::fixed(1.0);
            self.width.to(1.0, now, self.duration(100), false);
            self.hide_at = None;
            self.narrow_at = None;
        } else {
            self.hide_at = Some(now + 1000);
            if !self.lane {
                self.narrow_at = Some(now + 250);
            }
        }
    }
    pub fn tick(&mut self, now: u64) {
        if !self.hide_mode || self.high_contrast {
            self.alpha = Tween::fixed(1.0);
        }
        if self.high_contrast {
            self.width = Tween::fixed(1.0);
        }
        if !self.drag {
            if let Some(at) = self.hide_at.filter(|at| now >= *at) {
                self.hide_at = None;
                if !self.inside && self.hide_mode && !self.high_contrast {
                    self.alpha.to(0.0, at, self.duration(240), true);
                }
            }
            if let Some(at) = self.narrow_at.filter(|at| now >= *at) {
                self.narrow_at = None;
                if !self.lane && !self.high_contrast {
                    self.width.to(0.0, at, self.duration(150), false);
                }
            }
        }
        if self.reduced {
            self.alpha = Tween::fixed(self.alpha.to);
            self.width = Tween::fixed(self.width.to);
        }
    }
    pub fn values(&self, now: u64) -> (f64, f64) {
        (self.alpha.value(now), self.width.value(now))
    }
    fn active(&self, now: u64) -> bool {
        self.alpha.active(now) || self.width.active(now)
    }
    fn deadline(&self) -> Option<u64> {
        self.hide_at.into_iter().chain(self.narrow_at).min()
    }
}

#[derive(Clone, Debug)]
pub struct ScrollModel {
    pub offset: f64,
    pub target: f64,
    pub view: f64,
    pub content: f64,
    pub pitch: f64,
    motion: Tween,
    /// An offset to jump to right after the next `sync` (a view switch or a newly created bar): until then
    /// the content height is unknown and clamping would lose it.
    pub seed: Option<f64>,
}
impl Default for ScrollModel {
    fn default() -> Self {
        Self {
            offset: 0.0,
            target: 0.0,
            view: 0.0,
            content: 0.0,
            pitch: 1.0,
            motion: Tween::fixed(0.0),
            seed: None,
        }
    }
}
impl ScrollModel {
    pub fn max(&self) -> f64 {
        (self.content - self.view).max(0.0)
    }
    pub fn sync(&mut self, view: f64, content: f64, pitch: f64, now: u64) {
        self.tick(now);
        let pitch = pitch.max(0.001);
        let ratio = pitch / self.pitch;
        let changed = self.view != view || self.content != content || self.pitch != pitch;
        self.view = view.max(0.0);
        self.content = content.max(0.0);
        self.pitch = pitch;
        if changed {
            self.offset = (self.offset * ratio).clamp(0.0, self.max());
            self.target = (self.target * ratio).clamp(0.0, self.max());
            self.motion = Tween::fixed(self.offset);
            self.target = self.offset;
        }
        if let Some(seed) = self.seed.take() {
            self.offset = seed.clamp(0.0, self.max());
            self.target = self.offset;
            self.motion = Tween::fixed(self.offset);
        }
    }
    pub fn tick(&mut self, now: u64) {
        self.offset = self.motion.value(now).clamp(0.0, self.max());
    }
    pub fn set(&mut self, target: f64, now: u64, duration: u64) -> bool {
        self.tick(now);
        let target = target.clamp(0.0, self.max());
        if (target - self.target).abs() < 1e-9 {
            return false;
        }
        self.target = target;
        self.motion = Tween {
            from: self.offset,
            to: target,
            at: now,
            duration,
            ease_in: false,
        };
        self.tick(now);
        true
    }
    fn finish(&mut self) -> bool {
        let changed = self.offset != self.target;
        self.offset = self.target.clamp(0.0, self.max());
        self.motion = Tween::fixed(self.offset);
        changed
    }
    pub fn page_size(&self) -> f64 {
        ((self.view - self.pitch) / self.pitch).floor().max(1.0) * self.pitch
    }
    pub fn page(&mut self, down: bool, now: u64, reduced: bool) -> bool {
        self.set(
            self.target
                + if down {
                    self.page_size()
                } else {
                    -self.page_size()
                },
            now,
            if reduced { 0 } else { 120 },
        )
    }
    pub fn wheel(&mut self, delta: i32, lines: u32, now: u64, reduced: bool) -> bool {
        if delta == 0 || lines == 0 {
            return false;
        }
        let standard = delta % 120 == 0;
        let rows = ((lines as f64 / 3.0).round() as u32).clamp(1, 4) as f64;
        let distance = if lines == u32::MAX {
            self.page_size()
        } else {
            rows * self.pitch
        };
        let target = if !standard {
            self.tick(now);
            self.offset - delta as f64 / 120.0 * distance
        } else if lines == u32::MAX {
            self.target - delta as f64 / 120.0 * distance
        } else {
            let boundary = if delta < 0 {
                (self.target / self.pitch).floor()
            } else {
                (self.target / self.pitch).ceil()
            };
            (boundary - delta as f64 / 120.0 * rows) * self.pitch
        };
        self.set(target, now, if !standard || reduced { 0 } else { 90 })
    }
    pub fn ensure_visible(&mut self, top: f64, bottom: f64, now: u64) -> bool {
        let target = if top - 6.0 < self.offset {
            top - 6.0
        } else if bottom + 6.0 > self.offset + self.view {
            bottom + 6.0 - self.view
        } else {
            self.offset
        };
        self.set(target, now, 0)
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ThumbDrag {
    grab: f64,
    pub original: f64,
}
impl ThumbDrag {
    pub fn begin(bar: BarLayout, y: f64, offset: f64) -> Self {
        let travel = (bar.track.height() - bar.thumb.height()) as f64;
        let ideal_top =
            bar.track.top as f64 + travel * offset.clamp(0.0, bar.max_offset) / bar.max_offset;
        Self {
            grab: y - ideal_top,
            original: offset,
        }
    }
    pub fn offset(self, bar: BarLayout, y: f64) -> f64 {
        let travel = bar.track.height() - bar.thumb.height();
        if travel <= 0 {
            return self.original.clamp(0.0, bar.max_offset);
        }
        ((y - self.grab - bar.track.top as f64) / travel as f64 * bar.max_offset)
            .clamp(0.0, bar.max_offset)
    }
}
fn page_direction(bar: BarLayout, y: i32) -> Option<bool> {
    if y < bar.thumb.top {
        Some(false)
    } else if y >= bar.thumb.bottom {
        Some(true)
    } else {
        None
    }
}

/// scrollbar-spec 4: paging repetition uses a single deadline, without catch-up bursts.
#[derive(Clone, Copy, Debug)]
struct TrackPress {
    y: i32,
    down: bool,
    next: u64,
}
impl TrackPress {
    fn begin(y: i32, down: bool, now: u64) -> Self {
        Self {
            y,
            down,
            next: now + 400,
        }
    }
    /// WM_TIMER runs on the system tick and can arrive a few ms before the deadline: the time still to wait.
    fn early(&self, now: u64) -> Option<u32> {
        (now < self.next).then(|| (self.next - now) as u32)
    }
    fn repeat(&mut self, current: BarLayout, target: BarLayout, now: u64) -> bool {
        if now < self.next
            || page_direction(current, self.y) != Some(self.down)
            || page_direction(target, self.y) != Some(self.down)
        {
            return false;
        }
        self.next = now + 60;
        true
    }
}

/// A scroll range: `pos` within `min..=max - page`, as the bar draws it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortalRange {
    pub max: i32,
    pub page: i32,
    pub pos: i32,
    pub min: i32,
}


/// scrollbar-spec 5: sRGB contrast after compositing, with an alpha-preserving recolour guard.
fn luminance(c: [u8; 3]) -> f64 {
    let v = c.map(|v| {
        let v = v as f64 / 255.0;
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    });
    0.2126 * v[0] + 0.7152 * v[1] + 0.0722 * v[2]
}
pub fn contrast(c: [u8; 3], alpha: u8, backdrop: [u8; 3]) -> f64 {
    let a = alpha as f64 / 255.0;
    let blend =
        std::array::from_fn(|i| (c[i] as f64 * a + backdrop[i] as f64 * (1.0 - a)).round() as u8);
    let (a, b) = (luminance(blend), luminance(backdrop));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}
pub fn thumb_color(base: [u8; 3], mapped: [u8; 3], alpha: u8, backdrop: [u8; 3]) -> ([u8; 3], u8) {
    let c = if contrast(mapped, 141, backdrop) >= 3.0 {
        mapped
    } else {
        base
    };
    let a = (alpha..=255)
        .find(|a| contrast(c, *a, backdrop) >= 3.0)
        .unwrap_or(255);
    (c, a)
}
fn premul(c: [u8; 3], a: u8) -> [u8; 4] {
    [
        (c[2] as u32 * a as u32 / 255) as u8,
        (c[1] as u32 * a as u32 / 255) as u8,
        (c[0] as u32 * a as u32 / 255) as u8,
        a,
    ]
}
fn rounded_fill(bytes: &mut [u8], w: i32, h: i32, r: PixelRect, mut color: [u8; 4]) {
    color[3] = color[3].max(1);
    let radius = r.width().min(r.height()) as f64 / 2.0;
    for y in r.top.max(0)..r.bottom.min(h) {
        for x in r.left.max(0)..r.right.min(w) {
            let dx = (x as f64 + 0.5).clamp(r.left as f64 + radius, r.right as f64 - radius)
                - (x as f64 + 0.5);
            let dy = (y as f64 + 0.5).clamp(r.top as f64 + radius, r.bottom as f64 - radius)
                - (y as f64 + 0.5);
            if dx * dx + dy * dy <= radius * radius {
                let i = ((y * w + x) * 4) as usize;
                bytes[i..i + 4].copy_from_slice(&color);
            }
        }
    }
}
/// Keep the Camouflage surface adapter with body=true (scrollbar-spec 5).
pub fn render_camouflage(px: &mut [u8], effects: crate::camouflage::runtime::Effects) {
    crate::camouflage::runtime::apply_pixels(px, effects, true);
}

#[derive(Clone, Copy, Debug)]
pub struct Params {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub dpi: u32,
    pub thin: bool,
    pub corner: f64,
    pub max: i32,
    pub page: i32,
    pub pitch: i32,
    pub thumb: (u8, u8, u8),
    pub backdrop: [u8; 3],
    pub opacity: u8,
    pub hide_mode: bool,
    /// Parent Corral bounds in screen pixels, including its titlebar.
    pub parent: [i32; 4],
    pub input: HWND,
}
#[derive(Clone, Copy, Default)]
struct Preferences {
    reduced: bool,
    high_contrast: bool,
    window: [u8; 3],
    text: [u8; 3],
    hot: [u8; 3],
}
fn preferences() -> Preferences {
    #[repr(C)]
    struct Hc {
        size: u32,
        flags: u32,
        scheme: *mut u16,
    }
    let mut hc = Hc {
        size: std::mem::size_of::<Hc>() as u32,
        flags: 0,
        scheme: std::ptr::null_mut(),
    };
    let mut animation = BOOL(1);
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            hc.size,
            Some((&mut hc as *mut Hc).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let _ = SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            Some((&mut animation as *mut BOOL).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
        let rgb = |v: u32| [v as u8, (v >> 8) as u8, (v >> 16) as u8];
        Preferences {
            reduced: !animation.as_bool(),
            high_contrast: hc.flags & 1 != 0,
            window: rgb(GetSysColor(COLOR_WINDOW)),
            text: rgb(GetSysColor(COLOR_WINDOWTEXT)),
            hot: rgb(GetSysColor(COLOR_HOTLIGHT)),
        }
    }
}
/// scrollbar-spec 5/6: pure raster boundary; high contrast bypasses every mapped colour.
#[allow(clippy::too_many_arguments)]
fn paint_pixels(
    pixels: &mut [u8],
    w: i32,
    h: i32,
    bar: BarLayout,
    alpha: f64,
    wide: f64,
    hot: bool,
    drag: bool,
    prefs: Preferences,
    base: [u8; 3],
    mapped: [u8; 3],
    backdrop: [u8; 3],
) {
    let requested = if drag {
        255
    } else if hot {
        204
    } else {
        141
    };
    let (color, a) = thumb_color(base, mapped, requested, backdrop);
    // Layered windows skip alpha-zero input. One alpha unit keeps the entire hidden lane live.
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[0, 0, 0, 1]);
    }
    if prefs.high_contrast {
        rounded_fill(pixels, w, h, bar.track, premul(prefs.text, 255));
        let r = PixelRect {
            left: bar.track.left + 1,
            top: bar.track.top + 1,
            right: bar.track.right - 1,
            bottom: bar.track.bottom - 1,
        };
        if r.width() > 0 && r.height() > 0 {
            rounded_fill(pixels, w, h, r, premul(prefs.window, 255));
        }
        rounded_fill(
            pixels,
            w,
            h,
            bar.thumb,
            premul(if hot || drag { prefs.hot } else { prefs.text }, 255),
        );
    } else {
        if wide > 0.0 {
            rounded_fill(
                pixels,
                w,
                h,
                bar.track,
                premul(mapped, (31.0 * alpha * wide) as u8),
            );
        }
        rounded_fill(
            pixels,
            w,
            h,
            bar.thumb,
            premul(color, (a as f64 * alpha) as u8),
        );
    }
}
pub fn wheel_lines() -> u32 {
    let mut lines = 3u32;
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWHEELSCROLLLINES,
            0,
            Some((&mut lines as *mut u32).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    lines
}
struct State {
    id: u64,
    hwnd: HWND,
    notify: fn(u64, Request),
    surface: Surface,
    p: Params,
    clock: Instant,
    visual: Visibility,
    scroll: ScrollModel,
    drag: Option<ThumbDrag>,
    page_hold: Option<TrackPress>,
    pointer: (i32, i32),
    shown: bool,
    frame_running: bool,
    prefs: Preferences,
    camouflage: crate::camouflage::runtime::Effects,
}
thread_local! {
    static STATES:RefCell<HashMap<isize,Rc<RefCell<State>>>>=RefCell::new(HashMap::new());
    static DRAG_KEYS:std::cell::Cell<(isize,HHOOK)>=std::cell::Cell::new((0,HHOOK(std::ptr::null_mut())));
}
// scrollbar-spec 4: a thread-local hook observes Esc while the no-activate popup has capture.
// No global hook, keyboard focus change, or Tab stop is needed.
unsafe extern "system" fn drag_key_proc(code: i32, key: WPARAM, flags: LPARAM) -> LRESULT {
    if code >= 0 && key.0 == 27 {
        let target = DRAG_KEYS.with(|h| h.get().0);
        if target != 0 {
            if flags.0 & (1isize << 31) == 0 {
                unsafe {
                    let _ = PostMessageW(HWND(target as *mut _), WM_KEYDOWN, key, flags);
                }
            }
            return LRESULT(1);
        }
    }
    unsafe { CallNextHookEx(HHOOK::default(), code, key, flags) }
}
fn arm_drag_keys(hwnd: HWND) {
    DRAG_KEYS.with(|cell| {
        if cell.get().0 != 0 {
            return;
        }
        if let Ok(hook) = unsafe {
            SetWindowsHookExW(
                WH_KEYBOARD,
                Some(drag_key_proc),
                HINSTANCE::default(),
                GetWindowThreadProcessId(hwnd, None),
            )
        } {
            cell.set((hwnd.0 as isize, hook));
        }
    });
}
fn disarm_drag_keys(hwnd: HWND) {
    DRAG_KEYS.with(|cell| {
        let (target, hook) = cell.get();
        if target == hwnd.0 as isize {
            cell.set((0, HHOOK::default()));
            unsafe {
                let _ = UnhookWindowsHookEx(hook);
            }
        }
    });
}
fn register_class() -> HINSTANCE {
    thread_local! { static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }
        .map(|m| HINSTANCE(m.0))
        .unwrap_or_default();
    if !DONE.with(|d| d.replace(true)) {
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(bar_proc),
            hInstance: instance,
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            lpszClassName: w!("CorralScrollbar"),
            ..Default::default()
        };
        unsafe { RegisterClassExW(&class) };
    }
    instance
}

/// Window ownership and layered presentation are retained; behaviour comes from scrollbar-spec.
pub struct Bar {
    state: Rc<RefCell<State>>,
    hwnd: HWND,
    owner: HWND,
}
impl Bar {
    pub fn create(id: u64, owner: HWND, notify: fn(u64, Request)) -> windows::core::Result<Self> {
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("CorralScrollbar"),
                w!(""),
                WS_POPUP,
                -100,
                -100,
                14,
                10,
                owner,
                HMENU::default(),
                register_class(),
                None,
            )?
        };
        let surface = match Surface::new(14, 10) {
            Ok(s) => s,
            Err(e) => {
                unsafe {
                    let _ = DestroyWindow(hwnd);
                }
                return Err(e);
            }
        };
        let p = Params {
            x: -100,
            y: -100,
            w: 14,
            h: 10,
            dpi: 96,
            thin: false,
            corner: 0.0,
            max: 1,
            page: 1,
            pitch: 1,
            thumb: (255, 255, 255),
            backdrop: [0; 3],
            opacity: 255,
            hide_mode: true,
            parent: [0; 4],
            input: owner,
        };
        let state = Rc::new(RefCell::new(State {
            id,
            hwnd,
            notify,
            surface,
            p,
            clock: Instant::now(),
            visual: Visibility::default(),
            scroll: ScrollModel::default(),
            drag: None,
            page_hold: None,
            pointer: (-1, -1),
            shown: false,
            frame_running: false,
            prefs: preferences(),
            camouflage: Default::default(),
        }));
        STATES.with(|s| s.borrow_mut().insert(hwnd.0 as isize, state.clone()));
        Ok(Self { state, hwnd, owner })
    }
    pub fn alive(&self) -> bool {
        unsafe { IsWindow(self.hwnd).as_bool() }
    }
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }
    pub fn owner(&self) -> HWND {
        self.owner
    }
    pub fn rect(&self) -> (i32, i32, i32, i32) {
        let s = self.state.borrow();
        (s.p.x, s.p.y, s.p.w, s.p.h)
    }
    /// Jump (no animation) to `px` at the next paint.
    pub fn seed(&self, px: i32, dpi: u32) {
        self.state.borrow_mut().scroll.seed = Some(px as f64 / scale(dpi));
    }
    pub fn offset_px(&self, view: i32, content: i32, pitch: i32, dpi: u32) -> i32 {
        let mut s = self.state.borrow_mut();
        let now = s.now();
        let k = scale(dpi);
        s.scroll
            .sync(view as f64 / k, content as f64 / k, pitch as f64 / k, now);
        px(s.scroll.offset, dpi)
    }
    pub fn update(&self, p: &Params) {
        let Ok(mut s) = self.state.try_borrow_mut() else {
            return;
        };
        // scrollbar-spec 2/5: a hidden Corral has no input overlay; only the bar's own fade keeps a live lane.
        if p.opacity == 0 || p.w <= 0 || p.h <= 0 || p.w >= 10000 || p.h >= 10000 {
            s.hide();
            return;
        }
        if s.p.dpi != p.dpi {
            let ratio = scale(p.dpi) / scale(s.p.dpi);
            if let Some(d) = s.drag.as_mut() {
                d.grab *= ratio;
            }
        }
        s.p = *p;
        let now = s.now();
        let k = scale(p.dpi);
        s.scroll
            .sync(p.page as f64 / k, p.max as f64 / k, p.pitch as f64 / k, now);
        if s.visual.hide_mode != p.hide_mode {
            s.visual.hide_mode = p.hide_mode;
            if p.hide_mode && !s.visual.inside {
                s.visual.hide_at = Some(now + 800);
            }
        }
        s.apply_preferences();
        s.sample_pointer();
        s.visual.tick(now);
        if s.surface.resize(p.w as u32, p.h as u32).is_err() {
            return;
        }
        s.redraw();
        if !s.shown {
            unsafe {
                let _ = ShowWindow(s.hwnd, SW_SHOWNOACTIVATE);
            }
            s.shown = true;
        }
        s.arm();
    }
    pub fn set_camouflage(&self, effects: crate::camouflage::runtime::Effects) {
        if let Ok(mut s) = self.state.try_borrow_mut() {
            s.camouflage = effects;
        }
    }
    pub fn hide(&self) {
        if let Ok(mut s) = self.state.try_borrow_mut() {
            s.hide();
        }
    }
    pub fn move_to(&self, x: i32, y: i32) {
        if let Ok(mut s) = self.state.try_borrow_mut() {
            let (dx, dy) = (x - s.p.x, y - s.p.y);
            s.p.x = x;
            s.p.y = y;
            s.p.parent = [
                s.p.parent[0] + dx,
                s.p.parent[1] + dy,
                s.p.parent[2] + dx,
                s.p.parent[3] + dy,
            ];
            if s.shown {
                s.redraw();
            }
        }
    }
    pub fn parent_pointer(&self) {
        if let Ok(mut s) = self.state.try_borrow_mut() {
            if !s.shown {
                return;
            }
            s.sample_pointer();
            let now = s.now();
            s.visual.tick(now);
            s.redraw();
            s.arm();
        }
    }
    pub fn wheel(&self, delta: i32) -> bool {
        let Ok(mut s) = self.state.try_borrow_mut() else {
            return false;
        };
        if !s.shown {
            return false;
        }
        let now = s.now();
        let reduced = s.prefs.reduced || s.prefs.high_contrast;
        let changed = s.scroll.wheel(delta, wheel_lines(), now, reduced);
        if changed {
            s.visual.scroll(now);
            s.deliver();
            s.redraw();
            s.arm();
        }
        changed
    }
    pub fn cancel_drag(&self) -> bool {
        let Ok(mut s) = self.state.try_borrow_mut() else {
            return false;
        };
        let Some(drag) = s.drag.take() else {
            return false;
        };
        let now = s.now();
        s.scroll.set(drag.original, now, 0);
        s.end_press();
        s.deliver();
        s.redraw();
        s.arm();
        true
    }
    pub fn end_press(&self) {
        if let Ok(mut s) = self.state.try_borrow_mut() {
            s.end_press();
            s.redraw();
            s.arm();
        }
    }
}
impl Drop for Bar {
    fn drop(&mut self) {
        disarm_drag_keys(self.hwnd);
        let _ = STATES.try_with(|s| s.borrow_mut().remove(&(self.hwnd.0 as isize)));
        unsafe {
            if IsWindow(self.hwnd).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}
impl State {
    fn now(&self) -> u64 {
        self.clock.elapsed().as_millis() as u64
    }
    fn apply_preferences(&mut self) {
        let was_high_contrast = self.visual.high_contrast;
        self.visual.reduced = self.prefs.reduced;
        self.visual.high_contrast = self.prefs.high_contrast;
        if (self.prefs.reduced || self.prefs.high_contrast) && self.scroll.finish() {
            self.deliver();
        }
        if was_high_contrast && !self.prefs.high_contrast {
            self.visual.width = Tween::fixed(if self.visual.lane || self.drag.is_some() {
                1.0
            } else {
                0.0
            });
            if self.visual.hide_mode && !self.visual.inside && self.drag.is_none() {
                self.visual.hide_at = Some(self.now() + 800);
            }
        }
    }
    fn layout_at(&self, offset: f64, wide: f64) -> Option<BarLayout> {
        let view = self.p.h as f64 / scale(self.p.dpi);
        let mut bar = layout_widen(
            view,
            self.scroll.content,
            offset,
            self.p.dpi,
            self.p.thin,
            self.p.corner,
            wide,
        )?;
        bar.max_offset = self.scroll.max();
        Some(bar)
    }
    fn layout(&self) -> Option<BarLayout> {
        self.layout_at(self.scroll.offset, self.visual.values(self.now()).1)
    }
    fn sample_pointer(&mut self) {
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            return;
        }
        // A covered Corral is not hovered just because its rectangle contains the pointer.
        let under = unsafe { WindowFromPoint(pt) };
        let owner = unsafe { GetWindow(self.hwnd, GW_OWNER) }.unwrap_or_default();
        let inside = (under == self.p.input
            || under == owner
            || under == self.hwnd
            || unsafe { IsChild(self.p.input, under) }.as_bool())
            && pt.x >= self.p.parent[0]
            && pt.x < self.p.parent[2]
            && pt.y >= self.p.parent[1]
            && pt.y < self.p.parent[3];
        self.pointer = (pt.x - self.p.x, pt.y - self.p.y);
        let lane = inside
            && self.pointer.0 >= 0
            && self.pointer.0 < self.p.w
            && self.pointer.1 >= 0
            && self.pointer.1 < self.p.h;
        self.visual.pointer(inside, lane, self.now());
    }
    fn arm(&mut self) {
        let now = self.now();
        unsafe {
            let active = self.visual.active(now) || self.scroll.motion.active(now);
            if active && !self.frame_running {
                SetTimer(self.hwnd, TIMER_FRAME, 16, None);
            }
            if !active && self.frame_running {
                let _ = KillTimer(self.hwnd, TIMER_FRAME);
            }
            self.frame_running = active;
            let _ = KillTimer(self.hwnd, TIMER_WAKE);
            if let Some(at) = self.visual.deadline() {
                SetTimer(
                    self.hwnd,
                    TIMER_WAKE,
                    at.saturating_sub(now).clamp(1, u32::MAX as u64) as u32,
                    None,
                );
            }
        }
    }
    fn hide(&mut self) {
        self.end_press();
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_FRAME);
            let _ = KillTimer(self.hwnd, TIMER_WAKE);
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.shown = false;
        self.frame_running = false;
    }
    fn deliver(&self) {
        unsafe {
            let _ = PostMessageW(
                self.hwnd,
                WM_DELIVER,
                WPARAM(px(self.scroll.offset, self.p.dpi).max(0) as usize),
                LPARAM(0),
            );
        }
    }
    fn end_press(&mut self) {
        disarm_drag_keys(self.hwnd);
        self.drag = None;
        self.page_hold = None;
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_PAGE);
            if windows::Win32::UI::Input::KeyboardAndMouse::GetCapture() == self.hwnd {
                let _ = ReleaseCapture();
            }
        }
        let now = self.now();
        self.visual.dragging(false, now);
        self.sample_pointer();
    }
    fn redraw(&mut self) {
        let (w, h) = (self.p.w, self.p.h);
        let Some(bar) = self.layout() else { return };
        let now = self.now();
        let (alpha, wide) = self.visual.values(now);
        let hot = self.visual.lane || bar.thumb.contains(self.pointer.0, self.pointer.1);
        let requested = if self.drag.is_some() {
            255
        } else if hot {
            204
        } else {
            141
        };
        let base = [self.p.thumb.0, self.p.thumb.1, self.p.thumb.2];
        let mut sample = premul(base, requested);
        render_camouflage(&mut sample, self.camouflage);
        let mapped = if sample[3] == 0 {
            base
        } else {
            [
                ((sample[2] as u32 * 255) / sample[3] as u32).min(255) as u8,
                ((sample[1] as u32 * 255) / sample[3] as u32).min(255) as u8,
                ((sample[0] as u32 * 255) / sample[3] as u32).min(255) as u8,
            ]
        };
        paint_pixels(
            self.surface.pixels_mut(),
            w,
            h,
            bar,
            alpha,
            wide,
            hot,
            self.drag.is_some(),
            self.prefs,
            base,
            mapped,
            self.p.backdrop,
        );
        let _ = present_layered(
            self.hwnd,
            &self.surface,
            (self.p.x, self.p.y),
            self.p.opacity.max(1),
        );
    }
}
fn lookup(hwnd: HWND) -> Option<Rc<RefCell<State>>> {
    STATES.with(|s| s.borrow().get(&(hwnd.0 as isize)).cloned())
}
unsafe extern "system" fn bar_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(state) = lookup(hwnd) else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    if msg == WM_DELIVER {
        let (id, notify) = {
            let s = state.borrow();
            (s.id, s.notify)
        };
        notify(id, Request::Thumb { pos: wparam.0 as i32 });
        return LRESULT(0);
    }
    let Ok(mut s) = state.try_borrow_mut() else {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    };
    let now = s.now();
    let point = (lparam.0 as i16 as i32, (lparam.0 >> 16) as i16 as i32);
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => return LRESULT(HTCLIENT as isize),
        WM_RBUTTONDOWN | WM_RBUTTONUP | WM_CONTEXTMENU | WM_RBUTTONDBLCLK => return LRESULT(0),
        WM_MOUSEMOVE => {
            s.pointer = point;
            let lane = point.0 >= 0 && point.0 < s.p.w && point.1 >= 0 && point.1 < s.p.h;
            let screen = (point.0 + s.p.x, point.1 + s.p.y);
            let inside = screen.0 >= s.p.parent[0]
                && screen.0 < s.p.parent[2]
                && screen.1 >= s.p.parent[1]
                && screen.1 < s.p.parent[3];
            s.visual.pointer(inside, lane, now);
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = TrackMouseEvent(&mut track);
            }
            if let Some(press) = s.page_hold.as_mut() {
                press.y = point.1;
            }
            if let (Some(drag), Some(bar)) = (s.drag, s.layout()) {
                s.scroll.set(drag.offset(bar, point.1 as f64), now, 0);
                s.deliver();
            }
        }
        WM_MOUSELEAVE => {
            s.sample_pointer();
        }
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            s.pointer = point;
            s.visual.pointer(true, true, now);
            s.scroll.tick(now);
            s.scroll.target = s.scroll.offset;
            s.scroll.motion = Tween::fixed(s.scroll.offset);
            if let Some(bar) = s.layout() {
                unsafe {
                    SetCapture(hwnd);
                }
                if bar.thumb.contains(point.0, point.1) {
                    s.drag = Some(ThumbDrag::begin(bar, point.1 as f64, s.scroll.offset));
                    arm_drag_keys(hwnd);
                    s.visual.dragging(true, now);
                } else if let Some(down) = page_direction(bar, point.1) {
                    let reduced = s.prefs.reduced || s.prefs.high_contrast;
                    s.scroll.page(down, now, reduced);
                    s.visual.scroll(now);
                    s.page_hold = Some(TrackPress::begin(point.1, down, now));
                    s.deliver();
                    unsafe {
                        SetTimer(hwnd, TIMER_PAGE, 400, None);
                    }
                }
            }
        }
        WM_LBUTTONUP | WM_CAPTURECHANGED | WM_CANCELMODE | WM_KILLFOCUS => s.end_press(),
        WM_ACTIVATEAPP if wparam.0 == 0 => s.end_press(),
        WM_KEYDOWN if wparam.0 == 27 => {
            if let Some(drag) = s.drag {
                s.scroll.set(drag.original, now, 0);
                s.deliver();
            }
            s.end_press();
        }
        WM_MOUSEWHEEL => {
            let ctrl = wparam.0 & 8 != 0 || unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            let alt = unsafe { GetKeyState(VK_MENU.0 as i32) } < 0;
            let input = s.p.input;
            drop(s);
            if ctrl || alt {
                unsafe {
                    let _ = PostMessageW(input, msg, wparam, lparam);
                }
                return LRESULT(0);
            }
            let delta = (wparam.0 >> 16) as i16 as i32;
            let mut s = state.borrow_mut();
            let reduced = s.prefs.reduced || s.prefs.high_contrast;
            if !s.scroll.wheel(delta, wheel_lines(), now, reduced) {
                unsafe {
                    let _ = PostMessageW(input, msg, wparam, lparam);
                }
                return LRESULT(0);
            }
            s.visual.scroll(now);
            s.deliver();
            s.redraw();
            s.arm();
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == TIMER_PAGE => {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_PAGE);
            }
            if let (Some(mut press), Some(bar), Some(target)) =
                (s.page_hold, s.layout(), s.layout_at(s.scroll.target, 1.0))
            {
                if let Some(wait) = press.early(now) {
                    unsafe {
                        SetTimer(hwnd, TIMER_PAGE, wait, None);
                    }
                } else if press.repeat(bar, target, now) {
                    let reduced = s.prefs.reduced || s.prefs.high_contrast;
                    if s.scroll.page(press.down, now, reduced) {
                        s.page_hold = Some(press);
                        s.visual.scroll(now);
                        s.deliver();
                        unsafe {
                            SetTimer(hwnd, TIMER_PAGE, 60, None);
                        }
                    } else {
                        s.page_hold = None;
                    }
                } else {
                    s.page_hold = None;
                }
            }
        }
        WM_TIMER if wparam.0 == TIMER_FRAME || wparam.0 == TIMER_WAKE => {
            let before = s.scroll.offset;
            s.scroll.tick(now);
            s.visual.tick(now);
            if s.scroll.offset != before {
                s.deliver();
            }
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            s.prefs = preferences();
            s.apply_preferences();
            s.visual.tick(now);
        }
        WM_ERASEBKGND => return LRESULT(1),
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    s.redraw();
    s.arm();
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-8, "{a} != {b}");
    }
    fn model() -> ScrollModel {
        let mut m = ScrollModel::default();
        m.sync(320.0, 3000.0, 64.0, 0);
        m
    }

    #[test]
    fn a_seed_lands_after_the_next_sync_without_animation_and_clamped() {
        let mut m = ScrollModel::default();
        m.seed = Some(500.0);
        m.sync(320.0, 3000.0, 64.0, 0);
        close(m.offset, 500.0);
        close(m.target, 500.0);
        assert!(m.seed.is_none());
        m.seed = Some(9000.0);
        m.sync(320.0, 3000.0, 64.0, 10);
        close(m.offset, 2680.0);
    }

    // scrollbar-spec 8.1-4
    #[test]
    fn fits_and_degenerate_viewports() {
        assert!(bar_layout(100.0, 100.5, 0.0, 96, false, 0.0).is_none());
        assert!(bar_layout(100.0, 101.0, 0.0, 96, false, 0.0).is_some());
        assert!(bar_layout(0.0, 100.0, 0.0, 96, false, 0.0).is_none());
        assert!(bar_layout(f64::NAN, 100.0, 0.0, 96, false, 0.0).is_none());
        let b = bar_layout(1.0, 100.0, 0.0, 96, false, 900.0).unwrap();
        assert_eq!(b.track.height(), 1);
        assert_eq!(b.thumb.height(), 1);
    }
    #[test]
    fn thumb_proportions_and_minimums() {
        for (thin, minimum) in [(false, 28), (true, 20)] {
            let a = bar_layout(400.0, 40000.0, 0.0, 96, thin, 6.0).unwrap();
            assert_eq!(a.thumb.height(), minimum);
            let a = bar_layout(400.0, 800.0, 0.0, 96, thin, 6.0).unwrap();
            assert_eq!(a.thumb.height(), a.track.height() / 2);
            let a = bar_layout(10.0, 10000.0, 0.0, 96, thin, 6.0).unwrap();
            assert!(a.thumb.height() <= a.track.height());
        }
    }
    #[test]
    fn endpoints_and_monotone_positions() {
        let mut previous = -1;
        for offset in (0..=600).step_by(10) {
            let b = bar_layout(300.0, 900.0, offset as f64, 120, false, 8.0).unwrap();
            assert!(b.thumb.top >= previous);
            previous = b.thumb.top;
            if offset == 0 {
                assert_eq!(b.thumb.top, b.track.top);
            }
            if offset == 600 {
                assert_eq!(b.thumb.bottom, b.track.bottom);
            }
        }
    }
    #[test]
    fn dpi_geometry_is_snapped_bounded_and_grows_left() {
        for dpi in [96, 120, 144, 240] {
            for thin in [false, true] {
                for wide in [0.0, 0.37, 1.0] {
                    let b = layout_widen(400.0, 1600.0, 153.4, dpi, thin, 9.0, wide).unwrap();
                    assert!(b.thumb.left >= 0);
                    assert!(b.thumb.right <= b.lane);
                    assert!(b.thumb.width() >= 1);
                    assert!(b.thumb.top >= b.track.top);
                    assert!(b.thumb.bottom <= b.track.bottom);
                    let rest = bar_layout(400.0, 1600.0, 153.4, dpi, thin, 9.0).unwrap();
                    assert_eq!(b.thumb.right, rest.thumb.right);
                    assert!(b.thumb.width() >= rest.thumb.width());
                }
            }
            let normal = bar_layout(400.0, 1600.0, 0.0, dpi, false, 0.0).unwrap();
            let thin = bar_layout(400.0, 1600.0, 0.0, dpi, true, 0.0).unwrap();
            assert!(thin.thumb.width() < normal.thumb.width());
            assert!(thin.lane < normal.lane);
        }
    }
    // scrollbar-spec 8.5-10
    #[test]
    fn wheel_notches_system_lines_and_accumulated_target() {
        let mut m = model();
        m.set(17.0, 0, 0);
        assert!(m.wheel(-120, 3, 0, false));
        close(m.target, 64.0);
        close(m.offset, 17.0);
        assert!(m.wheel(-120, 3, 30, false));
        close(m.target, 128.0);
        m.tick(120);
        close(m.offset, 128.0);
        assert!(m.wheel(-120, 6, 120, true));
        close(m.offset, 256.0);
        assert!(m.wheel(240, 3, 120, true));
        close(m.offset, 128.0);
        assert!(m.wheel(-120, 99, 120, true));
        close(m.offset, 384.0);
        let before = m.offset;
        assert!(!m.wheel(-120, 0, 120, true));
        close(m.offset, before);
        m.wheel(-120, u32::MAX, 120, true);
        close(m.offset, before + 256.0);
    }
    #[test]
    fn partial_row_wheel_moves_to_the_next_boundary_both_directions() {
        let mut m = model();
        m.set(95.0, 0, 0);
        m.wheel(120, 3, 0, true);
        close(m.offset, 64.0);
        m.set(95.0, 0, 0);
        m.wheel(-120, 3, 0, true);
        close(m.offset, 128.0);
    }
    #[test]
    fn precision_deltas_keep_fractional_remainders_and_pass_ends() {
        let mut m = model();
        m.set(500.0, 0, 0);
        for delta in [30, -45, 7] {
            assert!(m.wheel(delta, 3, 0, false));
        }
        close(m.offset, 500.0 - (-8.0) / 120.0 * 64.0);
        m.set(0.0, 0, 0);
        assert!(!m.wheel(30, 3, 0, false));
        close(m.offset, 0.0);
        m.set(m.max(), 0, 0);
        assert!(!m.wheel(-7, 3, 0, false));
        close(m.offset, m.max());
        assert!(m.wheel(7, 3, 0, false));
        assert!(m.offset < m.max());
    }
    #[test]
    fn dragging_preserves_grab_and_roundtrips_at_each_dpi() {
        for dpi in [96, 120, 144, 240] {
            for offset in [0.0, 40.3, 500.0, 900.0] {
                let b = bar_layout(300.0, 1200.0, offset, dpi, false, 6.0).unwrap();
                let grab = 9.0;
                let drag = ThumbDrag::begin(b, b.thumb.top as f64 + grab, offset);
                let mapped = drag.offset(b, b.thumb.top as f64 + grab);
                close(mapped, offset);
                let again = bar_layout(300.0, 1200.0, mapped, dpi, false, 6.0).unwrap();
                assert!((again.thumb.top - b.thumb.top).abs() <= 1);
                close(drag.original, offset); // Esc uses this DIP value without re-quantizing.
                close(drag.offset(b, -10000.0), 0.0);
                close(drag.offset(b, 10000.0), 900.0);
            }
        }
    }
    #[test]
    fn drag_press_does_not_change_position_and_escape_restores() {
        let mut m = model();
        m.set(300.25, 0, 0);
        let bar = bar_layout(m.view, m.content, m.offset, 144, false, 6.0).unwrap();
        let drag = ThumbDrag::begin(bar, bar.thumb.top as f64 + 8.0, m.offset);
        close(m.offset, 300.25);
        m.set(drag.offset(bar, bar.thumb.top as f64 + 48.0), 0, 0);
        assert!(m.offset > 300.25);
        m.set(drag.original, 0, 0);
        close(m.offset, 300.25);
    }
    #[test]
    fn page_distances_repeat_direction_and_end_clamping() {
        let mut m = model();
        close(m.page_size(), 256.0);
        m.page(true, 0, false);
        close(m.target, 256.0);
        m.tick(120);
        close(m.offset, 256.0);
        let b = bar_layout(m.view, m.content, m.offset, 96, false, 6.0).unwrap();
        assert_eq!(page_direction(b, b.thumb.top - 1), Some(false));
        assert_eq!(page_direction(b, b.thumb.bottom), Some(true));
        assert_eq!(page_direction(b, b.thumb.top), None);
        let pointer = b.thumb.top;
        m.page(true, 120, true);
        let b = bar_layout(m.view, m.content, m.offset, 96, false, 6.0).unwrap();
        assert_ne!(page_direction(b, pointer), Some(true));
        m.sync(30.0, 3000.0, 64.0, 120);
        close(m.page_size(), 64.0);
        m.set(m.max(), 120, 0);
        assert!(!m.page(true, 120, true));
    }
    #[test]
    fn zoom_resize_and_content_change_keep_anchor_then_clamp() {
        let mut m = model();
        m.set(200.5, 0, 0);
        m.sync(320.0, 3000.0, 80.0, 0);
        close(m.offset, 200.5 / 64.0 * 80.0);
        m.sync(500.0, 3000.0, 80.0, 0);
        close(m.offset, 200.5 / 64.0 * 80.0);
        m.sync(500.0, 600.0, 80.0, 0);
        close(m.offset, 100.0);
        m.sync(600.0, 600.0, 80.0, 0);
        close(m.offset, 0.0);
    }
    #[test]
    fn ensure_visible_is_minimal_and_clamped() {
        let mut m = model();
        m.set(500.0, 0, 0);
        assert!(!m.ensure_visible(550.0, 600.0, 0));
        close(m.offset, 500.0);
        m.ensure_visible(490.0, 550.0, 0);
        close(m.offset, 484.0);
        m.ensure_visible(790.0, 850.0, 0);
        close(m.offset, 536.0);
        m.ensure_visible(0.0, 30.0, 0);
        close(m.offset, 0.0);
        m.ensure_visible(2990.0, 3000.0, 0);
        close(m.offset, m.max());
    }
    #[test]
    fn paging_repeat_deadlines_stop_at_the_pointer_and_do_not_catch_up() {
        let bar = bar_layout(320.0, 3000.0, 0.0, 96, false, 6.0).unwrap();
        let mut press = TrackPress::begin(200, true, 0);
        // a WM_TIMER a few ms early waits for the deadline instead of ending the hold (live 2026-10-28)
        assert_eq!(press.early(395), Some(5));
        assert_eq!(press.early(400), None);
        assert!(!press.repeat(bar, bar, 399));
        assert!(press.repeat(bar, bar, 400));
        assert!(!press.repeat(bar, bar, 459));
        assert!(press.repeat(bar, bar, 460));
        assert!(press.repeat(bar, bar, 5000));
        assert_eq!(press.next, 5060);
        let at_pointer = bar_layout(320.0, 3000.0, 1800.0, 96, false, 6.0).unwrap();
        assert!(!press.repeat(at_pointer, at_pointer, 5060));
        assert!(!press.repeat(bar, at_pointer, 5060));
    }
    // scrollbar-spec 8.11-13: fake clock, no window or desktop.
    #[test]
    fn enter_leave_linger_fade_and_reversal() {
        let mut v = Visibility::default();
        v.pointer(true, false, 0);
        close(v.values(0).0, 0.0);
        v.tick(60);
        close(v.values(60).0, 0.75);
        v.tick(120);
        close(v.values(120).0, 1.0);
        v.pointer(false, false, 200);
        v.tick(1000);
        close(v.values(1000).0, 1.0);
        v.tick(1120);
        close(v.values(1120).0, 0.75);
        let before = v.values(1120).0;
        v.pointer(true, false, 1120);
        close(v.values(1120).0, before);
        v.tick(1240);
        close(v.values(1240).0, 1.0);
        v.pointer(false, false, 1240);
        v.tick(2280);
        close(v.values(2280).0, 0.0);
    }
    #[test]
    fn wheel_restarts_linger_and_drag_blocks_hide() {
        let mut v = Visibility::default();
        v.pointer(true, false, 0);
        v.pointer(false, false, 200);
        v.scroll(700);
        v.tick(1000);
        close(v.values(1000).0, 1.0);
        v.tick(1700);
        close(v.values(1700).0, 1.0);
        v.tick(1940);
        close(v.values(1940).0, 0.0);
        v.dragging(true, 2000);
        v.tick(20000);
        close(v.values(20000).0, 1.0);
        v.dragging(false, 20000);
        v.tick(21000);
        close(v.values(21000).0, 1.0);
        v.tick(21240);
        close(v.values(21240).0, 0.0);
    }
    #[test]
    fn hidden_lane_is_live_and_widen_has_its_own_clock() {
        let mut v = Visibility::default();
        v.pointer(true, true, 0);
        close(v.values(0).0, 1.0);
        v.tick(50);
        close(v.values(50).1, 0.75);
        v.tick(100);
        close(v.values(100).1, 1.0);
        v.pointer(true, false, 100);
        v.tick(350);
        close(v.values(350).1, 1.0);
        v.tick(500);
        close(v.values(500).1, 0.0);
        v.dragging(true, 500);
        v.tick(600);
        close(v.values(600).1, 1.0);
        v.pointer(false, false, 600);
        v.tick(10000);
        close(v.values(10000).1, 1.0);
    }
    #[test]
    fn always_visible_reduced_motion_and_idle_timer_eligibility() {
        let mut v = Visibility {
            hide_mode: false,
            ..Default::default()
        };
        v.tick(0);
        close(v.values(0).0, 1.0);
        v.pointer(true, true, 10);
        v.pointer(false, false, 110);
        v.tick(10000);
        close(v.values(10000).0, 1.0);
        assert!(!v.active(10000));
        assert_eq!(v.deadline(), None);
        let mut m = model();
        m.wheel(-120, 3, 0, false);
        assert!(m.finish());
        close(m.offset, 64.0);
        assert!(!m.motion.active(0));
        let mut v = Visibility {
            reduced: true,
            ..Default::default()
        };
        v.pointer(true, true, 0);
        v.tick(0);
        assert_eq!(v.values(0), (1.0, 1.0));
        assert!(!v.active(0));
        v.pointer(false, false, 0);
        v.tick(250);
        close(v.values(250).1, 0.0);
        v.tick(799);
        close(v.values(799).0, 1.0);
        v.tick(800);
        close(v.values(800).0, 0.0);
    }
    #[test]
    fn high_contrast_forces_visibility_width_and_system_colours() {
        let mut v = Visibility {
            high_contrast: true,
            ..Default::default()
        };
        v.tick(0);
        assert_eq!(v.values(0), (1.0, 1.0));
        assert!(!v.active(0));
        let b = layout_widen(200.0, 800.0, 0.0, 96, false, 6.0, 1.0).unwrap();
        let prefs = Preferences {
            high_contrast: true,
            window: [1, 2, 3],
            text: [200, 210, 220],
            hot: [10, 20, 30],
            ..Default::default()
        };
        let mut a = vec![0; 14 * 200 * 4];
        let mut z = a.clone();
        paint_pixels(
            &mut a, 14, 200, b, 0.0, 0.0, false, false, prefs, [255; 3], [128; 3], [0; 3],
        );
        paint_pixels(
            &mut z,
            14,
            200,
            b,
            1.0,
            1.0,
            false,
            false,
            prefs,
            [0; 3],
            [255, 0, 0],
            [255; 3],
        );
        assert_eq!(a, z, "high contrast ignores alpha and recolour");
        let at = ((b.thumb.top + 10) * 14 + b.thumb.left + 3) as usize * 4;
        assert_eq!(&a[at..at + 4], &premul(prefs.text, 255));
        paint_pixels(
            &mut a, 14, 200, b, 1.0, 1.0, true, false, prefs, [0; 3], [0; 3], [0; 3],
        );
        assert_eq!(&a[at..at + 4], &premul(prefs.hot, 255));
    }
    // scrollbar-spec 8.14
    #[test]
    fn contrast_backdrops_and_guard() {
        for backdrop in [[0; 3], [255; 3], [128; 3], [255, 0, 0]] {
            let dark = [27, 29, 34];
            let light = [255; 3];
            let base = if contrast(dark, 255, backdrop) > contrast(light, 255, backdrop) {
                dark
            } else {
                light
            };
            let (c, a) = thumb_color(base, backdrop, 141, backdrop);
            assert_eq!(c, base);
            assert!(contrast(c, a, backdrop) >= 3.0);
        }
        let (c, a) = thumb_color([255; 3], [0; 3], 204, [0; 3]);
        assert_eq!(c, [255; 3]);
        assert!(a >= 204);
    }
    #[test]
    fn camouflage_mapping_preserves_alpha_and_fade_factor() {
        let palette = crate::camouflage::derive_tint_palette([90, 120, 60]);
        for alpha in [0u8, 31, 141, 204, 255] {
            let original = (alpha as u32) << 24 | 0x00ff_ffff;
            let zero = crate::camouflage::recolor_color(&palette, 0, original);
            let half = crate::camouflage::recolor_color(&palette, 50, original);
            let full = crate::camouflage::recolor_color(&palette, 100, original);
            assert_eq!(zero, original);
            assert_eq!(half >> 24, alpha as u32);
            assert_eq!(full >> 24, alpha as u32);
        }
        let b = bar_layout(200.0, 800.0, 0.0, 96, false, 6.0).unwrap();
        let mut bytes = vec![0; 14 * 200 * 4];
        paint_pixels(
            &mut bytes,
            14,
            200,
            b,
            0.0,
            0.0,
            false,
            false,
            Preferences::default(),
            [255; 3],
            [255; 3],
            [0; 3],
        );
        assert!(
            bytes.chunks_exact(4).all(|p| p[3] == 1),
            "the hidden lane stays hittable, without zero-alpha holes"
        );
    }
}
