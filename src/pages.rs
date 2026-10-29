//! Ranges: switching gestures, carrying an item to a neighbouring Range, adding and removing Ranges, and
//! the Range transition (ranges-dock-clearview-spec 3; motion-sorting-spec 5 for the animation).
//!
//! Everything here is pure: the recogniser and the carry tracker are fed literal events and a clock by
//! the engine and never read the OS. Ranges are numbered `0..n-1` for the whole desktop; a Corral has one
//! number (or is pinned by "Show on every Range"). Only numbers are ever stored, never positions.

use crate::geometry::Rect;
use crate::motion::{self, Curve, Tween};

/// Most Ranges there can be (spec 3.1): enough for themes, and a held drag cannot create Ranges forever.
pub const MAX_RANGES: i32 = 8;
/// Width of the edge strip that starts an edge pull (spec 3.2 `PULL_ZONE`).
pub const PULL_ZONE_DIP: f64 = 6.0;
/// A deliberate pan is a tenth of the monitor width, at least 120 and at most 320 DIP (spec 3.2 `PAN_T`).
pub const PAN_MIN_DIP: f64 = 120.0;
pub const PAN_MAX_DIP: f64 = 320.0;
/// Touchpad wheel units (120 = one notch) that make one switch: three notches (spec 3.2 `TP_UNITS`).
pub const TP_UNITS: i32 = 360;
/// Silence that resets the touchpad accumulators, and the silence that ends the lockout after a fire.
pub const TP_QUIET_MS: u64 = 200;
pub const TP_LOCKOUT_MS: u64 = 300;
/// Least time between two key or wheel steps (spec 3.2 `KEY_MIN`).
pub const KEY_MIN_MS: u64 = 150;
/// Width of the carry strip along a free screen edge (spec 3.2 `CARRY_ZONE`).
pub const CARRY_ZONE_DIP: f64 = 24.0;
/// Dwell in the strip before an item is carried, before the next carry, and before a Range is added.
pub const CARRY_DWELL_MS: u64 = 600;
pub const CARRY_REPEAT_MS: u64 = 900;
pub const ADD_DWELL_MS: u64 = 1000;
/// Alt+wheel accumulator: stale input older than this is forgotten.
const WHEEL_STALE_MS: u64 = 1000;

const VK_LEFT: u32 = 0x25;
const VK_RIGHT: u32 = 0x27;

/// DIP to pixels at `dpi`: `floor(d * dpi / 96 + 0.5)` (spec section 2).
pub fn px(dip: f64, dpi: u32) -> i32 {
    (dip * f64::from(dpi.max(1)) / 96.0 + 0.5).floor() as i32
}

/// `PAN_T` in DIP for a monitor `width_dip` wide.
pub fn pan_threshold_dip(width_dip: f64) -> f64 {
    (width_dip * 0.10).clamp(PAN_MIN_DIP, PAN_MAX_DIP)
}

// ---- visibility -------------------------------------------------------------------------------

/// A Corral shows on the current Range, or on every Range when pinned. Everything shows with the feature off.
pub fn group_visible(enabled: bool, page: i32, all_pages: bool, current: i32) -> bool {
    !enabled || all_pages || page == current
}

pub fn icon_visible(enabled: bool, page: i32, current: i32) -> bool {
    !enabled || page == current
}

// ---- monitors and edges -------------------------------------------------------------------------

/// One monitor as the gesture code sees it: physical pixel bounds in virtual-screen coordinates and DPI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mon {
    pub bounds: Rect,
    pub dpi: u32,
}

impl Mon {
    pub fn new(bounds: Rect, dpi: u32) -> Self {
        Self { bounds, dpi }
    }

    pub fn width_dip(&self) -> f64 {
        f64::from(self.bounds.width()) * 96.0 / f64::from(self.dpi.max(1))
    }

    /// `PAN_T` in pixels on this monitor.
    pub fn pan_px(&self) -> i32 {
        px(pan_threshold_dip(self.width_dip()), self.dpi)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    /// Range step of a pull from this edge: the left edge shows the previous Range, the right the next.
    pub fn step(self) -> i32 {
        match self {
            Side::Left => -1,
            Side::Right => 1,
        }
    }
}

pub fn monitor_at(mons: &[Mon], p: (i32, i32)) -> Option<usize> {
    mons.iter().position(|m| m.bounds.contains(p.0, p.1))
}

/// A monitor edge is free at height `y` when no other monitor touches it there (edges between monitors
/// never count, spec section 1).
pub fn edge_free(mons: &[Mon], index: usize, side: Side, y: i32) -> bool {
    let Some(mon) = mons.get(index) else { return false };
    !mons.iter().enumerate().any(|(i, other)| {
        i != index && other.bounds.top <= y && y < other.bounds.bottom
            && match side {
                Side::Left => other.bounds.left < mon.bounds.left && other.bounds.right >= mon.bounds.left,
                Side::Right => other.bounds.right > mon.bounds.right && other.bounds.left <= mon.bounds.right,
            }
    })
}

/// The free edge strip (`zone_dip` wide, measured in pixels of the monitor's own DPI) that contains `p`.
pub fn edge_zone_at(mons: &[Mon], p: (i32, i32), zone_dip: f64) -> Option<(usize, Side)> {
    let index = monitor_at(mons, p)?;
    let mon = &mons[index];
    let zone = px(zone_dip, mon.dpi).max(1);
    let side = if p.0 - mon.bounds.left < zone {
        Side::Left
    } else if mon.bounds.right - 1 - p.0 < zone {
        Side::Right
    } else {
        return None;
    };
    edge_free(mons, index, side, p.1).then_some((index, side))
}

// ---- the recogniser (spec 2 and 3.3) ---------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub win: bool,
}

impl Mods {
    pub fn is_none(self) -> bool {
        !(self.shift || self.ctrl || self.alt || self.win)
    }

    pub fn only_alt(self) -> bool {
        self.alt && !(self.shift || self.ctrl || self.win)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    fn index(self) -> usize {
        match self {
            Button::Left => 0,
            Button::Right => 1,
            Button::Middle => 2,
        }
    }
}

/// What is under the pointer. Any other window, the taskbar and menus are `Other`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    EmptyDesktop,
    LooseIcon,
    Corral,
    Other,
}

impl Target {
    /// "Desktop surface": empty desktop, a loose icon or any part of a Corral (spec section 1).
    pub fn is_surface(self) -> bool {
        self != Target::Other
    }
}

/// Which buttons are held after the event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub left: bool,
    pub right: bool,
    pub middle: bool,
}

impl Held {
    fn get(self, button: Button) -> bool {
        match button {
            Button::Left => self.left,
            Button::Right => self.right,
            Button::Middle => self.middle,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Down(Button),
    Up(Button),
    Move,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerEvent {
    pub t_ms: u64,
    pub pos: (i32, i32),
    pub kind: PointerKind,
    pub held: Held,
    pub mods: Mods,
    pub target: Target,
}

/// Wheel units: 120 is one notch; finer values accumulate. `dx` positive is Windows' horizontal scroll to
/// the right, `dy` positive is a wheel turned forward (up).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WheelEvent {
    pub t_ms: u64,
    pub pos: (i32, i32),
    pub mods: Mods,
    pub dx: i32,
    pub dy: i32,
    pub target: Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub t_ms: u64,
    pub vk: u32,
    pub down: bool,
    pub repeat: bool,
    pub mods: Mods,
    /// The foreground is the shell desktop or a Corral and no rename box or text edit has focus.
    pub focus_ok: bool,
}

/// Per-gesture settings (the master `SwipeEnabled` is part of the gate).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Config {
    pub edge: bool,
    pub touch: bool,
    /// Alt+Left, Alt+Right and Alt+wheel share one setting.
    pub keys: bool,
    pub middle: bool,
    pub dual: bool,
}

pub struct Ctx<'a> {
    /// Common gate (spec section 2): Corral on, session unlocked, no full-screen app, no menu, no modal
    /// dialog and the master setting on. Closed: every event passes and the recogniser resets.
    pub gate: bool,
    pub cfg: Config,
    pub mons: &'a [Mon],
    /// The Range a step starts from: the shown one, or the target of a running transition.
    pub current: i32,
    pub count: i32,
    /// A Corral move or resize, or an icon drag, runs: no both-buttons chord starts over it.
    pub busy: bool,
}

impl Ctx<'_> {
    /// The Range one step away, when there is one (no wrap-around).
    pub fn neighbour(&self, direction: i32) -> Option<i32> {
        let target = self.current.checked_add(direction)?;
        (0..self.count).contains(&target).then_some(target)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Swallow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Out {
    pub verdict: Verdict,
    /// The Range to switch to.
    pub step: Option<i32>,
    /// A both-buttons chord started: a Corral rectangle in progress is over.
    pub cancel_rectangle: bool,
    /// The first button of a chord that fired went up: a context menu may follow within 250 ms.
    pub chord_ended: bool,
}

impl Out {
    const PASS: Out = Out { verdict: Verdict::Pass, step: None, cancel_rectangle: false, chord_ended: false };
    const SWALLOW: Out = Out { verdict: Verdict::Swallow, step: None, cancel_rectangle: false, chord_ended: false };

    fn with_step(verdict: Verdict, step: Option<i32>) -> Out {
        Out { verdict, step, ..Out::PASS }
    }
}

#[derive(Clone, Copy, Debug)]
struct EdgePull {
    side: Side,
    start: (i32, i32),
    pan: i32,
    fired: bool,
}

/// Anchor of a middle drag or a both-buttons chord; `x` moves on each fire, `y` stays at the press.
#[derive(Clone, Copy, Debug)]
struct Anchor {
    x: i32,
    y: i32,
    pan: i32,
}

impl Anchor {
    /// A step when the travel since the anchor is at least `PAN_T`, at least twice the vertical travel:
    /// dragging right pulls the desktop right, which shows the previous Range.
    fn travel(&mut self, pos: (i32, i32)) -> Option<i32> {
        let dx = i64::from(pos.0) - i64::from(self.x);
        let dy = (i64::from(pos.1) - i64::from(self.y)).abs();
        if dx.abs() < i64::from(self.pan) || dx.abs() < 2 * dy {
            return None;
        }
        self.x = pos.0;
        Some(if dx > 0 { -1 } else { 1 })
    }
}

#[derive(Clone, Copy, Debug)]
struct Chord {
    first: Button,
    anchor: Anchor,
}

#[derive(Clone, Copy, Debug, Default)]
struct Pad {
    h: i32,
    v: i32,
    last: Option<u64>,
    locked: bool,
}

/// The pure recogniser of the six switching methods. One instance lives for the whole run.
#[derive(Clone, Debug, Default)]
pub struct Recognizer {
    /// Buttons whose next up is owed a `Swallow` (their down was swallowed).
    owed_up: [bool; 3],
    /// Keys whose down was swallowed; repeats and the up follow.
    owed_keys: Vec<u32>,
    edge: Option<EdgePull>,
    middle: Option<Anchor>,
    chord: Option<Chord>,
    chord_fired: bool,
    /// A button went down on a desktop surface (the first half of a possible chord).
    surface_down: [bool; 3],
    pad: Pad,
    wheel_acc: i32,
    wheel_last: Option<u64>,
    last_step: Option<u64>,
}

impl Recognizer {
    /// Forget all gesture state (the gate closed, a setting changed). Owed swallows survive so the
    /// down/up pairing invariant holds.
    pub fn reset(&mut self) {
        self.edge = None;
        self.middle = None;
        self.chord = None;
        self.chord_fired = false;
        self.surface_down = [false; 3];
        self.pad = Pad::default();
        self.wheel_acc = 0;
        self.wheel_last = None;
    }

    /// Whether an edge pull holds the pointer (the strip is reserved while it runs).
    pub fn edge_active(&self) -> bool {
        self.edge.is_some()
    }

    /// The mouse capture was lost or cancelled (the release will not arrive): a swallowed press owes nothing.
    pub fn lost_capture(&mut self) {
        if self.edge.take().is_some() {
            self.owed_up[Button::Left.index()] = false;
        }
        self.middle = None;
        self.chord = None;
        self.chord_fired = false;
    }

    fn step_allowed(&mut self, t: u64) -> bool {
        if self.last_step.is_some_and(|last| t.saturating_sub(last) < KEY_MIN_MS) {
            return false;
        }
        self.last_step = Some(t);
        true
    }

    pub fn pointer(&mut self, ev: PointerEvent, ctx: &Ctx) -> Out {
        // a new press means the earlier release of this button (if it was owed one) went elsewhere
        if let PointerKind::Down(button) = ev.kind {
            self.owed_up[button.index()] = false;
        }
        if let PointerKind::Up(button) = ev.kind {
            let first_of_fired_chord = self.chord_fired && self.chord.is_some_and(|c| c.first == button);
            if let Some(chord) = self.chord {
                if chord.first == button {
                    self.chord = None;
                }
            }
            if button == Button::Left && self.edge.is_some() {
                self.edge = None;
            }
            if button == Button::Middle {
                self.middle = None;
            }
            self.surface_down[button.index()] = false;
            if first_of_fired_chord {
                self.chord_fired = false;
            }
            let owed = std::mem::take(&mut self.owed_up[button.index()]);
            let mut out = if owed { Out::SWALLOW } else { Out::PASS };
            out.chord_ended = first_of_fired_chord;
            return out;
        }
        if !ctx.gate {
            self.reset();
            return Out::PASS;
        }
        match ev.kind {
            PointerKind::Down(button) => self.down(button, ev, ctx),
            PointerKind::Move => self.motion(ev, ctx),
            PointerKind::Up(_) => Out::PASS,
        }
    }

    fn down(&mut self, button: Button, ev: PointerEvent, ctx: &Ctx) -> Out {
        let surface = ev.target.is_surface();
        if button == Button::Left && ctx.cfg.edge && ev.target == Target::EmptyDesktop {
            if let Some((index, side)) = edge_zone_at(ctx.mons, ev.pos, PULL_ZONE_DIP) {
                self.edge = Some(EdgePull { side, start: ev.pos, pan: ctx.mons[index].pan_px(), fired: false });
                self.owed_up[Button::Left.index()] = true;
                return Out::SWALLOW;
            }
        }
        if button == Button::Middle {
            if ctx.cfg.middle && surface {
                self.middle = Some(Anchor { x: ev.pos.0, y: ev.pos.1, pan: self.pan_at(ctx, ev.pos) });
            }
            return Out::PASS;
        }
        let other = if button == Button::Left { Button::Right } else { Button::Left };
        if ctx.cfg.dual && surface && !ctx.busy && self.chord.is_none() && ev.held.get(other) && self.surface_down[other.index()] {
            self.chord = Some(Chord { first: other, anchor: Anchor { x: ev.pos.0, y: ev.pos.1, pan: self.pan_at(ctx, ev.pos) } });
            self.owed_up[button.index()] = true;
            return Out { cancel_rectangle: true, ..Out::SWALLOW };
        }
        self.surface_down[button.index()] = surface;
        Out::PASS
    }

    fn pan_at(&self, ctx: &Ctx, pos: (i32, i32)) -> i32 {
        monitor_at(ctx.mons, pos).map_or_else(|| Mon::new(Rect::new(0, 0, 1920, 1080), 96).pan_px(), |i| ctx.mons[i].pan_px())
    }

    fn motion(&mut self, ev: PointerEvent, ctx: &Ctx) -> Out {
        if let Some(mut pull) = self.edge {
            if !ev.held.left {
                self.edge = None;
                return Out::PASS;
            }
            let travel = match pull.side {
                Side::Left => ev.pos.0 - pull.start.0,
                Side::Right => pull.start.0 - ev.pos.0,
            };
            let dy = (ev.pos.1 - pull.start.1).abs();
            let mut step = None;
            if !pull.fired && travel >= pull.pan && travel >= 2 * dy {
                pull.fired = true;
                step = ctx.neighbour(pull.side.step());
            }
            self.edge = Some(pull);
            return Out::with_step(Verdict::Swallow, step);
        }
        if let Some(mut anchor) = self.middle {
            if !ev.held.middle {
                self.middle = None;
                return Out::PASS;
            }
            let step = anchor.travel(ev.pos).and_then(|d| ctx.neighbour(d));
            self.middle = Some(anchor);
            return Out::with_step(Verdict::Pass, step);
        }
        if let Some(mut chord) = self.chord {
            if !ev.held.get(chord.first) {
                self.chord = None;
                return Out::PASS;
            }
            let fired = chord.anchor.travel(ev.pos);
            let step = fired.and_then(|d| ctx.neighbour(d));
            if fired.is_some() {
                self.chord_fired = true;
            }
            self.chord = Some(chord);
            return Out::with_step(Verdict::Pass, step);
        }
        Out::PASS
    }

    pub fn wheel(&mut self, ev: WheelEvent, ctx: &Ctx) -> Out {
        if ev.mods.only_alt() {
            self.pad = Pad::default();
            if !ctx.gate || !ctx.cfg.keys || !ev.target.is_surface() {
                self.wheel_acc = 0;
                return Out::PASS;
            }
            if self.wheel_last.is_some_and(|last| ev.t_ms.saturating_sub(last) >= WHEEL_STALE_MS) {
                self.wheel_acc = 0;
            }
            self.wheel_last = Some(ev.t_ms);
            self.wheel_acc = self.wheel_acc.saturating_add(ev.dy);
            let mut step = None;
            if self.wheel_acc.abs() >= 120 {
                // up (forward) is the previous Range, down advances (spec 3.3.4)
                let direction = if self.wheel_acc > 0 { -1 } else { 1 };
                self.wheel_acc = 0;
                if self.step_allowed(ev.t_ms) {
                    step = ctx.neighbour(direction);
                }
            }
            return Out::with_step(Verdict::Swallow, step);
        }
        self.wheel_acc = 0;
        if !ctx.gate || !ctx.cfg.touch || !ev.mods.is_none() || !ev.target.is_surface() {
            self.pad = Pad::default();
            return Out::PASS;
        }
        let pad = &mut self.pad;
        if pad.locked {
            let quiet = pad.last.is_none_or(|last| ev.t_ms.saturating_sub(last) >= TP_LOCKOUT_MS);
            pad.last = Some(ev.t_ms);
            if !quiet {
                return Out::PASS;
            }
            *pad = Pad::default();
        }
        if pad.last.is_some_and(|last| ev.t_ms.saturating_sub(last) >= TP_QUIET_MS) {
            pad.h = 0;
            pad.v = 0;
        }
        pad.last = Some(ev.t_ms);
        pad.h = pad.h.saturating_add(ev.dx);
        pad.v = pad.v.saturating_add(ev.dy);
        let mut step = None;
        if pad.h.abs() >= TP_UNITS && pad.h.abs() >= 2 * pad.v.abs() {
            let direction = if pad.h > 0 { 1 } else { -1 };
            pad.h = 0;
            pad.v = 0;
            pad.locked = true;
            step = ctx.neighbour(direction);
        }
        Out::with_step(Verdict::Pass, step)
    }

    pub fn key(&mut self, ev: KeyEvent, ctx: &Ctx) -> Out {
        let mut owed = self.owed_keys.contains(&ev.vk);
        // a swallowed key owes its repeats and its release; a fresh press (no repeat flag) starts over
        if ev.down && !ev.repeat && owed {
            self.owed_keys.retain(|&vk| vk != ev.vk);
            owed = false;
        }
        if !ev.down {
            self.owed_keys.retain(|&vk| vk != ev.vk);
            return if owed { Out::SWALLOW } else { Out::PASS };
        }
        let arrow = ev.vk == VK_LEFT || ev.vk == VK_RIGHT;
        let wanted = arrow && ctx.gate && ctx.cfg.keys && ev.focus_ok && ev.mods.only_alt();
        if !owed && !wanted {
            return Out::PASS;
        }
        if !owed {
            self.owed_keys.push(ev.vk);
        }
        let direction = if ev.vk == VK_LEFT { -1 } else { 1 };
        let step = if self.step_allowed(ev.t_ms) { ctx.neighbour(direction) } else { None };
        Out::with_step(Verdict::Swallow, step)
    }
}

/// A two-finger pan on a touch screen (spec 3.3.2): the centroid of exactly two contacts travels `PAN_T`
/// sideways (at least twice its vertical travel), once per pan. A finger moving right pulls the desktop
/// right, which shows the previous Range.
#[derive(Clone, Copy, Debug, Default)]
pub struct TouchPan {
    start: Option<(i32, i32)>,
    fired: bool,
}

impl TouchPan {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Feed the contacts of one touch report. Returns the Range to show.
    pub fn feed(&mut self, contacts: &[(i32, i32)], ctx: &Ctx) -> Option<i32> {
        if contacts.len() != 2 || !ctx.gate || !ctx.cfg.touch {
            self.reset();
            return None;
        }
        let centre = ((contacts[0].0 + contacts[1].0) / 2, (contacts[0].1 + contacts[1].1) / 2);
        let start = *self.start.get_or_insert(centre);
        if self.fired {
            return None;
        }
        let pan = monitor_at(ctx.mons, start).map_or(120, |i| ctx.mons[i].pan_px());
        let dx = centre.0 - start.0;
        let dy = (centre.1 - start.1).abs();
        if dx.abs() < pan || dx.abs() < 2 * dy {
            return None;
        }
        self.fired = true;
        ctx.neighbour(if dx > 0 { -1 } else { 1 })
    }
}

// ---- carrying an item to a neighbouring Range (spec 3.4) -------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragKind {
    /// A Corral dragged by its title.
    Corral,
    /// One or more loose icons or Corral icons.
    Icons,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CarryAct {
    None,
    /// Show this Range (the drag continues).
    Switch(i32),
    /// Append an empty Range and show it.
    Add,
}

/// What the progress bar along the edge shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub mon: usize,
    pub side: Side,
    /// 0..1 over the dwell.
    pub fraction: f64,
    /// The bar ends in a "+" chip (a Range will be added).
    pub add: bool,
    pub y: i32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CarryOut {
    pub act: CarryAct,
    pub progress: Option<Progress>,
}

#[derive(Clone, Copy, Debug)]
struct Zone {
    mon: usize,
    side: Side,
    since: u64,
}

/// Dwell tracker polled every 50 ms during a drag.
#[derive(Clone, Copy, Debug, Default)]
pub struct Carry {
    zone: Option<Zone>,
    last_fire: Option<u64>,
}

impl Carry {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn poll(&mut self, t: u64, pos: (i32, i32), kind: DragKind, mons: &[Mon], current: i32, count: i32) -> CarryOut {
        let none = CarryOut { act: CarryAct::None, progress: None };
        let Some((mon, side)) = edge_zone_at(mons, pos, CARRY_ZONE_DIP) else {
            self.reset();
            return none;
        };
        if !self.zone.is_some_and(|z| z.mon == mon && z.side == side) {
            self.zone = Some(Zone { mon, side, since: t });
            self.last_fire = None;
        }
        let zone = self.zone.unwrap_or(Zone { mon, side, since: t });
        let target = current.saturating_add(side.step());
        let neighbour = (0..count).contains(&target);
        let can_add = kind == DragKind::Corral && side == Side::Right && current == count - 1 && count < MAX_RANGES;
        let need = if neighbour {
            if self.last_fire.is_some() { CARRY_REPEAT_MS } else { CARRY_DWELL_MS }
        } else if can_add {
            ADD_DWELL_MS
        } else {
            return none;
        };
        let elapsed = t.saturating_sub(self.last_fire.unwrap_or(zone.since));
        let mut out = CarryOut {
            act: CarryAct::None,
            progress: Some(Progress { mon, side, fraction: (elapsed as f64 / need as f64).min(1.0), add: !neighbour, y: pos.1 }),
        };
        if elapsed >= need {
            self.last_fire = Some(t);
            out.act = if neighbour { CarryAct::Switch(target) } else { CarryAct::Add };
        }
        out
    }
}

// ---- removing empty Ranges (spec 3.5) ------------------------------------------------------------

/// Occupancy of Ranges `0..count`: a Range is occupied by a numbered, unpinned Corral or loose icon, or by
/// the payload of a drag in flight. Numbers outside `0..count` are ignored.
pub fn occupancy(count: i32, numbers: impl IntoIterator<Item = i32>) -> Vec<bool> {
    let mut occupied = vec![false; count.max(0) as usize];
    for n in numbers {
        if let Some(slot) = usize::try_from(n).ok().and_then(|i| occupied.get_mut(i)) {
            *slot = true;
        }
    }
    occupied
}

/// The Ranges to delete, highest first: every empty Range except the current one and the `protected`
/// ones. Range 0 goes too when others remain; the current Range always stays, so at least one remains.
pub fn plan_prune(occupied: &[bool], current: i32, protected: &[i32]) -> Vec<i32> {
    (0..occupied.len() as i32).rev()
        .filter(|&k| !occupied[k as usize] && k != current && !protected.contains(&k))
        .collect()
}

/// A number after the Ranges in `removed` were deleted: every deleted Range below it shifts it down by one.
pub fn renumber(n: i32, removed: &[i32]) -> i32 {
    n - removed.iter().filter(|&&k| k < n).count() as i32
}

/// The transition position `s` (Range `i` sits at `(i - s) x W`) after Range `k` was deleted: continuous,
/// so nothing jumps. Removal runs only when no transition is in flight, so the engine does not need it today.
#[cfg(test)]
pub fn remap_position(s: f64, k: i32) -> f64 {
    s - (s - f64::from(k)).clamp(0.0, 1.0)
}

/// Stored negative numbers (older layouts): the shift that makes the lowest one 0. Numbers above the last
/// Range are never clamped, so nothing is lost; they are Ranges of their own until pruned.
pub fn lift_negatives(numbers: &[i32]) -> Option<i32> {
    numbers.iter().copied().min().filter(|&lowest| lowest < 0).map(|lowest| -lowest)
}

/// Ranges to delete from the numbers of everything that lives on a Range: unpinned Corrals, loose icons of the
/// desktop, and the stored rows of icons that are not listed right now (hand-over, partial reads, hidden
/// files). Stored rows keep their Range alive.
pub fn removals(count: i32, current: i32, groups: &[i32], icons_present: &[i32], icons_stored: &[i32]) -> Vec<i32> {
    let numbers = groups.iter().chain(icons_present).chain(icons_stored).copied();
    plan_prune(&occupancy(count, numbers), current, &[])
}

// ---- the transition (motion-sorting-spec 5) ------------------------------------------------------

/// Transition of the stored Animation choice (motion-sorting-spec 5): 1 Slide = 240 ms OUT, 2 Bounce =
/// 360 ms with one overshoot of about 3 % of the travel. `None` = swap instantly: Off, or Windows
/// animations off (spec 7), whatever the choice says.
pub fn transition(style: u32, animate: bool) -> Option<(Curve, f64)> {
    if !animate {
        return None;
    }
    match style {
        1 => Some((Curve::Out, motion::RANGES_SLIDE_MS)),
        2 => Some((Curve::Bounce, motion::RANGES_BOUNCE_MS)),
        _ => None,
    }
}

/// Move the scroll offset (pixels, 0 = the current Range fills the monitor) towards `to`, from
/// wherever the picture is now: duration = base x clamp(|distance| / width, 0.5, 1.0), so a long
/// retarget is not slower than a normal one and a tiny reversal is not a snap (spec 5).
pub fn retarget_offset(tween: &mut Tween, now: f64, to: f64, width: i32, style: u32, animate: bool) {
    match transition(style, animate) {
        Some((curve, base)) => tween.retarget_clamped(now, to, base, curve, f64::from(width.max(1)), true),
        None => tween.retarget_exact(now, to, 0.0, Curve::Out),
    }
}

/// What a switch made while a transition runs does (spec 5, "Interrupting").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retarget {
    /// Already heading there.
    Ignore,
    /// Back to the Range the `from` picture shows: the same pictures, the offset runs home.
    Return,
    /// Slide on from the picture on screen; `direction` (+1 = higher Ranges, to the right) comes
    /// from the previous target.
    Continue { direction: i32 },
}

pub fn retarget_plan(from_page: Option<i32>, target: i32, next: i32) -> Retarget {
    if next == target {
        Retarget::Ignore
    } else if from_page == Some(next) {
        Retarget::Return
    } else {
        Retarget::Continue { direction: next.saturating_sub(target).signum() }
    }
}

/// Where the Range being left and, when another Range is on its way in, that Range are drawn for a
/// scroll offset in pixels: Range i sits at x = (i - s) x W with s = current - offset / W, so a higher
/// Range comes in from the right while the content travels left (spec 5).
pub fn range_xs(current: i32, candidate: i32, offset: i32, width: i32) -> (i32, Option<i32>) {
    let direction = candidate.saturating_sub(current).signum();
    (offset, (direction != 0).then(|| offset.saturating_add(direction.saturating_mul(width))))
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: Out = Out::PASS;

    fn mon(left: i32, right: i32, dpi: u32) -> Mon {
        Mon::new(Rect::new(left, 0, right, 1080), dpi)
    }

    fn cfg_all() -> Config {
        Config { edge: true, touch: true, keys: true, middle: true, dual: true }
    }

    fn ctx<'a>(mons: &'a [Mon], current: i32, count: i32) -> Ctx<'a> {
        Ctx { gate: true, cfg: cfg_all(), mons, current, count, busy: false }
    }

    fn held(left: bool, right: bool, middle: bool) -> Held {
        Held { left, right, middle }
    }

    fn ptr(t: u64, pos: (i32, i32), kind: PointerKind, h: Held, target: Target) -> PointerEvent {
        PointerEvent { t_ms: t, pos, kind, held: h, mods: Mods::default(), target }
    }

    fn down(t: u64, pos: (i32, i32), button: Button, h: Held, target: Target) -> PointerEvent {
        ptr(t, pos, PointerKind::Down(button), h, target)
    }

    fn up(t: u64, pos: (i32, i32), button: Button, h: Held, target: Target) -> PointerEvent {
        ptr(t, pos, PointerKind::Up(button), h, target)
    }

    fn mv(t: u64, pos: (i32, i32), h: Held, target: Target) -> PointerEvent {
        ptr(t, pos, PointerKind::Move, h, target)
    }

    fn wheel(t: u64, dx: i32, dy: i32, mods: Mods) -> WheelEvent {
        WheelEvent { t_ms: t, pos: (500, 500), mods, dx, dy, target: Target::EmptyDesktop }
    }

    fn key(t: u64, vk: u32, down: bool, repeat: bool, mods: Mods) -> KeyEvent {
        KeyEvent { t_ms: t, vk, down, repeat, mods, focus_ok: true }
    }

    const ALT: Mods = Mods { shift: false, ctrl: false, alt: true, win: false };

    #[test]
    fn dips_convert_to_pixels_with_half_up_rounding() {
        assert_eq!(px(6.0, 96), 6);
        assert_eq!(px(6.0, 120), 8);
        assert_eq!(px(6.0, 144), 9);
        assert_eq!(px(6.0, 192), 12);
        assert_eq!(px(6.0, 240), 15);
        assert_eq!(px(0.5, 96), 1);
        assert_eq!(px(0.49, 96), 0);
    }

    #[test]
    fn pan_threshold_is_a_tenth_of_the_width_within_120_and_320_dip() {
        let widths = [600.0, 1920.0, 3840.0, 7680.0];
        let expected = [120.0, 192.0, 320.0, 320.0];
        for (w, e) in widths.into_iter().zip(expected) {
            assert_eq!(pan_threshold_dip(w), e);
        }
        // zones and distances equal px(d) at each DPI
        for dpi in [96, 120, 144, 192, 240] {
            let m = Mon::new(Rect::new(0, 0, px(1200.0, dpi), 1080), dpi);
            assert_eq!(m.pan_px(), px(120.0, dpi));
        }
    }

    #[test]
    fn edge_pull_fires_at_pan_t_with_a_clear_sideways_dominance() {
        let mons = [mon(0, 1200, 96)];
        let c = ctx(&mons, 1, 3);
        let mut r = Recognizer::default();
        // press at x 3, drag 130 right with dy 40: fires previous
        let d = r.pointer(down(0, (3, 100), Button::Left, held(true, false, false), Target::EmptyDesktop), &c);
        assert_eq!(d.verdict, Verdict::Swallow);
        let m = r.pointer(mv(10, (133, 140), held(true, false, false), Target::EmptyDesktop), &c);
        assert_eq!((m.verdict, m.step), (Verdict::Swallow, Some(0)));
        // one switch per press
        let m = r.pointer(mv(20, (300, 140), held(true, false, false), Target::EmptyDesktop), &c);
        assert_eq!((m.verdict, m.step), (Verdict::Swallow, None));
        let u = r.pointer(up(30, (300, 140), Button::Left, held(false, false, false), Target::EmptyDesktop), &c);
        assert_eq!(u.verdict, Verdict::Swallow);
        assert!(!r.edge_active());
        // dy 80 does not fire (130 < 160)
        let mut r = Recognizer::default();
        r.pointer(down(0, (0, 100), Button::Left, held(true, false, false), Target::EmptyDesktop), &c);
        let m = r.pointer(mv(10, (130, 180), held(true, false, false), Target::EmptyDesktop), &c);
        assert_eq!((m.verdict, m.step), (Verdict::Swallow, None));
        // exactly at the threshold fires, one below does not
        let mut r = Recognizer::default();
        r.pointer(down(0, (0, 100), Button::Left, held(true, false, false), Target::EmptyDesktop), &c);
        assert_eq!(r.pointer(mv(1, (119, 100), held(true, false, false), Target::EmptyDesktop), &c).step, None);
        assert_eq!(r.pointer(mv(2, (120, 100), held(true, false, false), Target::EmptyDesktop), &c).step, Some(0));
    }

    #[test]
    fn edge_pull_from_the_right_edge_shows_the_next_range_and_missing_ranges_do_nothing() {
        let mons = [mon(0, 1200, 96)];
        let mut r = Recognizer::default();
        let c = ctx(&mons, 1, 3);
        r.pointer(down(0, (1197, 50), Button::Left, held(true, false, false), Target::EmptyDesktop), &c);
        let m = r.pointer(mv(5, (1070, 50), held(true, false, false), Target::EmptyDesktop), &c);
        assert_eq!(m.step, Some(2));
        // last Range: nothing to show, still swallowed
        let last = ctx(&mons, 2, 3);
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (1199, 50), Button::Left, held(true, false, false), Target::EmptyDesktop), &last).verdict, Verdict::Swallow);
        let m = r.pointer(mv(5, (900, 50), held(true, false, false), Target::EmptyDesktop), &last);
        assert_eq!((m.verdict, m.step), (Verdict::Swallow, None));
    }

    #[test]
    fn edge_pull_only_starts_in_the_strip_on_empty_desktop_on_a_free_edge() {
        let mons = [mon(0, 1200, 96)];
        let c = ctx(&mons, 1, 3);
        let e = held(true, false, false);
        // 6 px in is outside the 6 DIP strip (x 0..5)
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (6, 10), Button::Left, e, Target::EmptyDesktop), &c).verdict, Verdict::Pass);
        assert!(!r.edge_active());
        assert_eq!(r.pointer(down(0, (5, 10), Button::Left, e, Target::EmptyDesktop), &c).verdict, Verdict::Swallow);
        // an icon, a Corral or another window under the press never starts one
        for target in [Target::LooseIcon, Target::Corral, Target::Other] {
            let mut r = Recognizer::default();
            assert_eq!(r.pointer(down(0, (2, 10), Button::Left, e, target), &c).verdict, Verdict::Pass);
        }
        // a shared edge is no free edge
        let two = [mon(0, 1200, 96), mon(1200, 2400, 96)];
        let c2 = ctx(&two, 1, 3);
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (1199, 10), Button::Left, e, Target::EmptyDesktop), &c2).verdict, Verdict::Pass);
        assert_eq!(r.pointer(down(0, (1201, 10), Button::Left, e, Target::EmptyDesktop), &c2).verdict, Verdict::Pass);
        assert_eq!(r.pointer(down(0, (2399, 10), Button::Left, e, Target::EmptyDesktop), &c2).verdict, Verdict::Swallow);
        // setting off, or a right/middle press, passes
        let mut off = ctx(&mons, 1, 3);
        off.cfg.edge = false;
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (2, 10), Button::Left, e, Target::EmptyDesktop), &off).verdict, Verdict::Pass);
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (2, 10), Button::Right, held(false, true, false), Target::EmptyDesktop), &c).verdict, Verdict::Pass);
    }

    #[test]
    fn an_edge_that_a_monitor_only_partly_touches_is_free_where_nothing_touches_it() {
        let tall = Mon::new(Rect::new(0, 0, 1000, 1000), 96);
        let small = Mon::new(Rect::new(1000, 0, 2000, 400), 96);
        let mons = [tall, small];
        assert!(!edge_free(&mons, 0, Side::Right, 100));
        assert!(edge_free(&mons, 0, Side::Right, 700));
        assert!(edge_free(&mons, 1, Side::Right, 100));
        assert!(!edge_free(&mons, 1, Side::Left, 100));
    }

    #[test]
    fn a_press_keeps_the_pixels_of_its_own_monitor() {
        // a 100 % monitor next to a 250 % one: the strip and the distance are measured at the press
        let a = Mon::new(Rect::new(0, 0, 1920, 1080), 96);
        let b = Mon::new(Rect::new(1920, 0, 1920 + 4800, 2700), 240);
        let mons = [a, b];
        let c = ctx(&mons, 1, 3);
        let e = held(true, false, false);
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (0, 100), Button::Left, e, Target::EmptyDesktop), &c).verdict, Verdict::Swallow);
        // 192 px = PAN_T of a 1920 DIP monitor at 100 %; entering the 250 % monitor changes nothing
        assert_eq!(r.pointer(mv(1, (191, 100), e, Target::EmptyDesktop), &c).step, None);
        assert_eq!(r.pointer(mv(2, (192, 100), e, Target::EmptyDesktop), &c).step, Some(0));
        // on the 250 % monitor the strip is 15 px wide and PAN_T is 192 DIP = 480 px
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (6719, 100), Button::Left, e, Target::EmptyDesktop), &c).verdict, Verdict::Swallow);
        assert_eq!(r.pointer(mv(1, (6719 - 479, 100), e, Target::EmptyDesktop), &c).step, None);
        assert_eq!(r.pointer(mv(2, (6719 - 480, 100), e, Target::EmptyDesktop), &c).step, Some(2));
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (6719 - 14, 100), Button::Left, e, Target::EmptyDesktop), &c).verdict, Verdict::Swallow);
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (6719 - 15, 100), Button::Left, e, Target::EmptyDesktop), &c).verdict, Verdict::Pass);
    }

    #[test]
    fn a_closed_gate_passes_everything_and_resets_but_owes_the_up() {
        let mons = [mon(0, 1200, 96)];
        let mut c = ctx(&mons, 1, 3);
        let e = held(true, false, false);
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (2, 10), Button::Left, e, Target::EmptyDesktop), &c).verdict, Verdict::Swallow);
        c.gate = false;
        assert_eq!(r.pointer(mv(5, (400, 10), e, Target::EmptyDesktop), &c).verdict, Verdict::Pass);
        assert!(!r.edge_active());
        assert_eq!(r.pointer(up(9, (400, 10), Button::Left, held(false, false, false), Target::EmptyDesktop), &c).verdict, Verdict::Swallow);
        assert_eq!(r.pointer(up(9, (400, 10), Button::Left, held(false, false, false), Target::EmptyDesktop), &c).verdict, Verdict::Pass);
        c.gate = false;
        assert_eq!(r.pointer(down(0, (2, 10), Button::Left, e, Target::EmptyDesktop), &c).verdict, Verdict::Pass);
    }

    #[test]
    fn touchpad_pans_fire_at_three_notches_and_never_swallow() {
        let mons = [mon(0, 1920, 96)];
        let c = ctx(&mons, 1, 3);
        let mut r = Recognizer::default();
        assert_eq!(r.wheel(wheel(0, 359, 0, Mods::default()), &c), R);
        // 1 more unit: 360 reached
        let o = r.wheel(wheel(10, 1, 0, Mods::default()), &c);
        assert_eq!((o.verdict, o.step), (Verdict::Pass, Some(2)));
        // negative is previous
        let mut r = Recognizer::default();
        let o = r.wheel(wheel(0, -360, 0, Mods::default()), &c);
        assert_eq!((o.verdict, o.step), (Verdict::Pass, Some(0)));
        // sideways must dominate: 400 against 250 does not fire
        let mut r = Recognizer::default();
        assert_eq!(r.wheel(wheel(0, 400, 250, Mods::default()), &c).step, None);
        // a pure vertical scroll never fires
        let mut r = Recognizer::default();
        for t in 0..20 {
            assert_eq!(r.wheel(wheel(t * 10, 0, 120, Mods::default()), &c).step, None);
        }
    }

    #[test]
    fn touchpad_accumulators_reset_after_200_ms_and_inertia_is_ignored_for_300_ms() {
        let mons = [mon(0, 1920, 96)];
        let c = ctx(&mons, 1, 3);
        let mut r = Recognizer::default();
        assert_eq!(r.wheel(wheel(0, 300, 0, Mods::default()), &c).step, None);
        // 200 ms of silence forgot the 300 units
        assert_eq!(r.wheel(wheel(200, 100, 0, Mods::default()), &c).step, None);
        assert_eq!(r.wheel(wheel(210, 100, 0, Mods::default()), &c).step, None);
        assert_eq!(r.wheel(wheel(220, 160, 0, Mods::default()), &c).step, Some(2));
        // inertia keeps arriving within 300 ms: ignored, and it keeps the lockout going
        for t in 1..=20 {
            assert_eq!(r.wheel(wheel(220 + t * 100, 200, 0, Mods::default()), &c).step, None);
        }
        // 300 ms of silence ends the lockout; the next pan counts from zero
        assert_eq!(r.wheel(wheel(2220 + 300, 200, 0, Mods::default()), &c).step, None);
        assert_eq!(r.wheel(wheel(2530, 200, 0, Mods::default()), &c).step, Some(2));
    }

    #[test]
    fn touchpad_needs_a_free_modifier_state_a_surface_and_the_setting() {
        let mons = [mon(0, 1920, 96)];
        let mut c = ctx(&mons, 1, 3);
        for mods in [ALT, Mods { shift: true, ..Mods::default() }, Mods { ctrl: true, ..Mods::default() }, Mods { win: true, ..Mods::default() }] {
            let mut r = Recognizer::default();
            let o = r.wheel(wheel(0, 400, 0, mods), &c);
            assert_eq!(o.step, None, "{mods:?}");
        }
        let mut r = Recognizer::default();
        let mut w = wheel(0, 400, 0, Mods::default());
        w.target = Target::Other;
        assert_eq!(r.wheel(w, &c), R);
        c.cfg.touch = false;
        assert_eq!(Recognizer::default().wheel(wheel(0, 400, 0, Mods::default()), &c), R);
        c.cfg.touch = true;
        c.gate = false;
        assert_eq!(Recognizer::default().wheel(wheel(0, 400, 0, Mods::default()), &c), R);
    }

    #[test]
    fn alt_arrows_step_swallow_and_pair_their_up() {
        let mons = [mon(0, 1920, 96)];
        let c = ctx(&mons, 1, 3);
        let mut r = Recognizer::default();
        let o = r.key(key(0, VK_RIGHT, true, false, ALT), &c);
        assert_eq!((o.verdict, o.step), (Verdict::Swallow, Some(2)));
        // repeats inside KEY_MIN are swallowed without a step, later ones step
        let o = r.key(key(100, VK_RIGHT, true, true, ALT), &c);
        assert_eq!((o.verdict, o.step), (Verdict::Swallow, None));
        let o = r.key(key(150, VK_RIGHT, true, true, ALT), &c);
        assert_eq!((o.verdict, o.step), (Verdict::Swallow, Some(2)));
        // the up is swallowed even when Alt was released first
        let o = r.key(key(200, VK_RIGHT, false, false, Mods::default()), &c);
        assert_eq!(o.verdict, Verdict::Swallow);
        assert_eq!(r.key(key(210, VK_RIGHT, false, false, Mods::default()), &c).verdict, Verdict::Pass);
        let o = r.key(key(400, VK_LEFT, true, false, ALT), &c);
        assert_eq!((o.verdict, o.step), (Verdict::Swallow, Some(0)));
    }

    #[test]
    fn alt_arrows_have_a_gate_and_exact_modifiers() {
        let mons = [mon(0, 1920, 96)];
        let last = ctx(&mons, 2, 3);
        let mut r = Recognizer::default();
        // Right on the last Range does nothing
        let o = r.key(key(0, VK_RIGHT, true, false, ALT), &last);
        assert_eq!(o.step, None);
        // Ctrl+Alt+Right passes, with its up
        let ctrl_alt = Mods { ctrl: true, alt: true, ..Mods::default() };
        let c = ctx(&mons, 1, 3);
        let mut r = Recognizer::default();
        assert_eq!(r.key(key(0, VK_RIGHT, true, false, ctrl_alt), &c), R);
        assert_eq!(r.key(key(5, VK_RIGHT, false, false, ctrl_alt), &c), R);
        // another window in front, or a text edit focused: passes
        let mut r = Recognizer::default();
        let mut e = key(0, VK_RIGHT, true, false, ALT);
        e.focus_ok = false;
        assert_eq!(r.key(e, &c), R);
        // setting off / gate closed
        let mut off = ctx(&mons, 1, 3);
        off.cfg.keys = false;
        assert_eq!(Recognizer::default().key(key(0, VK_LEFT, true, false, ALT), &off), R);
        off.cfg.keys = true;
        off.gate = false;
        assert_eq!(Recognizer::default().key(key(0, VK_LEFT, true, false, ALT), &off), R);
        // other keys with Alt are not ours
        assert_eq!(Recognizer::default().key(key(0, 0x26, true, false, ALT), &c), R);
    }

    #[test]
    fn alt_wheel_accumulates_notches_and_down_advances() {
        let mons = [mon(0, 1920, 96)];
        let c = ctx(&mons, 1, 3);
        let mut r = Recognizer::default();
        assert_eq!(r.wheel(wheel(0, 0, -119, ALT), &c).step, None);
        let o = r.wheel(wheel(10, 0, -1, ALT), &c);
        assert_eq!((o.verdict, o.step), (Verdict::Swallow, Some(2)));
        // up is previous; small events accumulate
        let mut r = Recognizer::default();
        let mut got = None;
        for i in 0..8 {
            let o = r.wheel(wheel(i * 10, 0, 15, ALT), &c);
            assert_eq!(o.verdict, Verdict::Swallow);
            got = got.or(o.step);
        }
        assert_eq!(got, Some(0));
        // at most one step per KEY_MIN
        let mut r = Recognizer::default();
        assert_eq!(r.wheel(wheel(0, 0, -120, ALT), &c).step, Some(2));
        assert_eq!(r.wheel(wheel(100, 0, -120, ALT), &c).step, None);
        assert_eq!(r.wheel(wheel(300, 0, -120, ALT), &c).step, Some(2));
        // Alt off or the gate closed passes
        assert_eq!(Recognizer::default().wheel(wheel(0, 0, -240, Mods::default()), &c).verdict, Verdict::Pass);
        let mut closed = ctx(&mons, 1, 3);
        closed.gate = false;
        assert_eq!(Recognizer::default().wheel(wheel(0, 0, -240, ALT), &closed), R);
        let mut other = wheel(0, 0, -240, ALT);
        other.target = Target::Other;
        assert_eq!(Recognizer::default().wheel(other, &c), R);
    }

    #[test]
    fn middle_drag_chains_steps_and_a_reversal_goes_back() {
        let mons = [mon(0, 1200, 96)];
        let mut cur = 1;
        let mut r = Recognizer::default();
        let m = held(false, false, true);
        let t = Target::EmptyDesktop;
        let c = ctx(&mons, cur, 5);
        assert_eq!(r.pointer(down(0, (400, 300), Button::Middle, m, t), &c), R);
        // 119 px: nothing; 120 px dragging right pulls the desktop right: previous
        assert_eq!(r.pointer(mv(1, (519, 300), m, t), &c).step, None);
        let o = r.pointer(mv(2, (520, 300), m, t), &c);
        assert_eq!((o.verdict, o.step), (Verdict::Pass, Some(0)));
        cur = 0;
        // anchor moved: a further 120 right steps again (no Range before 0: nothing to show)
        let c = ctx(&mons, cur, 5);
        assert_eq!(r.pointer(mv(3, (640, 300), m, t), &c).step, None);
        // reversal: 120 left of the new anchor goes to the next Range
        cur = 1;
        let c = ctx(&mons, cur, 5);
        let o = r.pointer(mv(4, (520, 300), m, t), &c);
        assert_eq!(o.step, Some(2));
        // a short click does nothing; diagonals do not fire
        let mut r = Recognizer::default();
        r.pointer(down(0, (400, 300), Button::Middle, m, t), &c);
        assert_eq!(r.pointer(mv(1, (410, 302), m, t), &c).step, None);
        assert_eq!(r.pointer(up(2, (410, 302), Button::Middle, held(false, false, false), t), &c), R);
        r.pointer(down(3, (400, 300), Button::Middle, m, t), &c);
        assert_eq!(r.pointer(mv(4, (560, 500), m, t), &c).step, None);
    }

    #[test]
    fn middle_drag_chained_steps_need_two_pan_distances() {
        let mons = [mon(0, 1200, 96)];
        let m = held(false, false, true);
        let t = Target::LooseIcon;
        let mut r = Recognizer::default();
        let mut cur = 2;
        let mut steps = vec![];
        r.pointer(down(0, (1000, 300), Button::Middle, m, t), &ctx(&mons, cur, 5));
        for (i, x) in [880, 760].into_iter().enumerate() {
            let o = r.pointer(mv(i as u64 + 1, (x, 300), m, t), &ctx(&mons, cur, 5));
            if let Some(s) = o.step {
                steps.push(s);
                cur = s;
            }
        }
        assert_eq!(steps, [3, 4]);
        // middle drag on another window never starts
        let mut r = Recognizer::default();
        r.pointer(down(0, (1000, 300), Button::Middle, m, Target::Other), &ctx(&mons, 2, 5));
        assert_eq!(r.pointer(mv(1, (500, 300), m, Target::Other), &ctx(&mons, 2, 5)).step, None);
    }

    #[test]
    fn both_buttons_drag_swallows_the_second_button_either_order() {
        let mons = [mon(0, 1200, 96)];
        let t = Target::EmptyDesktop;
        let c = ctx(&mons, 1, 4);
        // left first, right second
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (400, 300), Button::Left, held(true, false, false), t), &c), R);
        let o = r.pointer(down(10, (400, 300), Button::Right, held(true, true, false), t), &c);
        assert_eq!(o.verdict, Verdict::Swallow);
        assert!(o.cancel_rectangle, "a rectangle in progress is cancelled");
        let o = r.pointer(mv(20, (520, 300), held(true, true, false), t), &c);
        assert_eq!((o.verdict, o.step), (Verdict::Pass, Some(0)));
        // the second button's up is swallowed, the first passes
        assert_eq!(r.pointer(up(30, (520, 300), Button::Right, held(true, false, false), t), &c).verdict, Verdict::Swallow);
        let o = r.pointer(up(40, (520, 300), Button::Left, held(false, false, false), t), &c);
        assert_eq!(o.verdict, Verdict::Pass);
        assert!(o.chord_ended);
        // right first, left second
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (400, 300), Button::Right, held(false, true, false), t), &c), R);
        let o = r.pointer(down(10, (400, 300), Button::Left, held(true, true, false), t), &c);
        assert_eq!(o.verdict, Verdict::Swallow);
        assert!(o.cancel_rectangle, "a right-button rectangle is cancelled too");
        let o = r.pointer(mv(20, (280, 300), held(true, true, false), t), &c);
        assert_eq!(o.step, Some(2));
        assert_eq!(r.pointer(up(30, (280, 300), Button::Left, held(false, true, false), t), &c).verdict, Verdict::Swallow);
        let o = r.pointer(up(40, (280, 300), Button::Right, held(false, false, false), t), &c);
        assert_eq!((o.verdict, o.chord_ended), (Verdict::Pass, true));
    }

    #[test]
    fn a_release_that_went_elsewhere_does_not_swallow_a_later_unrelated_up() {
        let mons = [mon(0, 1200, 96)];
        let c = ctx(&mons, 1, 3);
        let e = held(true, false, false);
        let mut r = Recognizer::default();
        assert_eq!(r.pointer(down(0, (2, 10), Button::Left, e, Target::EmptyDesktop), &c).verdict, Verdict::Swallow);
        // released over another window: no up arrives here; the next press clears the debt
        assert_eq!(r.pointer(down(500, (600, 300), Button::Left, e, Target::EmptyDesktop), &c).verdict, Verdict::Pass);
        assert_eq!(r.pointer(up(520, (600, 300), Button::Left, held(false, false, false), Target::EmptyDesktop), &c).verdict, Verdict::Pass);
        // a lost capture clears it too
        let mut r = Recognizer::default();
        r.pointer(down(0, (2, 10), Button::Left, e, Target::EmptyDesktop), &c);
        r.lost_capture();
        assert!(!r.edge_active());
        assert_eq!(r.pointer(up(5, (600, 300), Button::Left, held(false, false, false), Target::Other), &c).verdict, Verdict::Pass);
    }

    #[test]
    fn no_chord_starts_over_a_running_move_resize_or_icon_drag() {
        let mons = [mon(0, 1200, 96)];
        let mut busy = ctx(&mons, 1, 4);
        busy.busy = true;
        let t = Target::EmptyDesktop;
        let mut r = Recognizer::default();
        r.pointer(down(0, (400, 300), Button::Left, held(true, false, false), t), &busy);
        let o = r.pointer(down(5, (400, 300), Button::Right, held(true, true, false), t), &busy);
        assert_eq!((o.verdict, o.cancel_rectangle), (Verdict::Pass, false));
        assert_eq!(r.pointer(mv(9, (900, 300), held(true, true, false), t), &busy).step, None);
    }

    #[test]
    fn a_swallowed_key_owes_only_its_repeats_and_release() {
        let mons = [mon(0, 1920, 96)];
        let c = ctx(&mons, 1, 3);
        let mut r = Recognizer::default();
        assert_eq!(r.key(key(0, VK_RIGHT, true, false, ALT), &c).verdict, Verdict::Swallow);
        // the release went to another window; a later plain arrow press must pass
        assert_eq!(r.key(key(1000, VK_RIGHT, true, false, Mods::default()), &c), R);
        assert_eq!(r.key(key(1010, VK_RIGHT, false, false, Mods::default()), &c), R);
        // repeats of the swallowed press stay swallowed even if Alt was released
        let mut r = Recognizer::default();
        r.key(key(0, VK_RIGHT, true, false, ALT), &c);
        assert_eq!(r.key(key(200, VK_RIGHT, true, true, Mods::default()), &c).verdict, Verdict::Swallow);
    }

    #[test]
    fn a_left_button_alone_never_fires_and_a_chord_needs_surfaces_and_the_setting() {
        let mons = [mon(0, 1200, 96)];
        let t = Target::EmptyDesktop;
        let c = ctx(&mons, 1, 4);
        let mut r = Recognizer::default();
        r.pointer(down(0, (400, 300), Button::Left, held(true, false, false), t), &c);
        assert_eq!(r.pointer(mv(10, (900, 300), held(true, false, false), t), &c), R);
        // right button down over another window: not a chord
        let mut r = Recognizer::default();
        r.pointer(down(0, (400, 300), Button::Left, held(true, false, false), t), &c);
        assert_eq!(r.pointer(down(5, (400, 300), Button::Right, held(true, true, false), Target::Other), &c), R);
        // setting off
        let mut off = ctx(&mons, 1, 4);
        off.cfg.dual = false;
        let mut r = Recognizer::default();
        r.pointer(down(0, (400, 300), Button::Left, held(true, false, false), t), &off);
        assert_eq!(r.pointer(down(5, (400, 300), Button::Right, held(true, true, false), t), &off), R);
        // a chord that fired nothing ends without a menu guard
        let mut r = Recognizer::default();
        r.pointer(down(0, (400, 300), Button::Left, held(true, false, false), t), &c);
        r.pointer(down(5, (400, 300), Button::Right, held(true, true, false), t), &c);
        r.pointer(up(6, (400, 300), Button::Right, held(true, false, false), t), &c);
        assert!(!r.pointer(up(7, (400, 300), Button::Left, held(false, false, false), t), &c).chord_ended);
    }

    #[test]
    fn a_two_finger_pan_fires_once_at_pan_t() {
        let mons = [mon(0, 1200, 96)];
        let c = ctx(&mons, 1, 3);
        let mut pan = TouchPan::default();
        // a single finger never pans; the start is the first two-finger centroid
        assert_eq!(pan.feed(&[(500, 500)], &c), None);
        assert_eq!(pan.feed(&[(500, 500), (520, 500)], &c), None);
        assert_eq!(pan.feed(&[(600, 505), (620, 505)], &c), None, "100 px");
        assert_eq!(pan.feed(&[(620, 505), (640, 505)], &c), Some(0), "120 px right shows the previous Range");
        assert_eq!(pan.feed(&[(900, 505), (920, 505)], &c), None, "one switch per pan");
        // lifting a finger ends the pan; the next pan to the left goes forward
        assert_eq!(pan.feed(&[(900, 505)], &c), None);
        assert_eq!(pan.feed(&[(900, 500), (920, 500)], &c), None);
        assert_eq!(pan.feed(&[(780, 500), (800, 500)], &c), Some(2));
        // diagonal drift and a closed gate never fire
        let mut pan = TouchPan::default();
        assert_eq!(pan.feed(&[(500, 500), (520, 500)], &c), None);
        assert_eq!(pan.feed(&[(640, 700), (660, 700)], &c), None);
        let mut closed = ctx(&mons, 1, 3);
        closed.gate = false;
        let mut pan = TouchPan::default();
        pan.feed(&[(500, 500), (520, 500)], &closed);
        assert_eq!(pan.feed(&[(800, 500), (820, 500)], &closed), None);
    }

    #[test]
    fn carry_switches_after_600_ms_and_repeats_after_900() {
        let mons = [mon(0, 1920, 96)];
        let mut carry = Carry::default();
        let at_right = (1919, 400);
        let mut t = 0;
        // 599 ms: only progress
        for step in 0..=11 {
            t = step * 50;
            let o = carry.poll(t, at_right, DragKind::Icons, &mons, 1, 3);
            assert_eq!(o.act, CarryAct::None, "t={t}");
        }
        assert_eq!(t, 550);
        let o = carry.poll(599, at_right, DragKind::Icons, &mons, 1, 3);
        assert_eq!(o.act, CarryAct::None);
        let o = carry.poll(600, at_right, DragKind::Icons, &mons, 1, 3);
        assert_eq!(o.act, CarryAct::Switch(2));
        // the next carry needs 900 ms in the strip (the caller now shows Range 2 of 4)
        assert_eq!(carry.poll(1499, at_right, DragKind::Icons, &mons, 2, 4).act, CarryAct::None);
        assert_eq!(carry.poll(1500, at_right, DragKind::Icons, &mons, 2, 4).act, CarryAct::Switch(3));
        // leaving at 599 resets the dwell
        let mut carry = Carry::default();
        carry.poll(0, at_right, DragKind::Icons, &mons, 1, 3);
        assert_eq!(carry.poll(599, at_right, DragKind::Icons, &mons, 1, 3).act, CarryAct::None);
        assert!(carry.poll(600, (900, 400), DragKind::Icons, &mons, 1, 3).progress.is_none());
        assert_eq!(carry.poll(601, at_right, DragKind::Icons, &mons, 1, 3).act, CarryAct::None);
        assert_eq!(carry.poll(1200, at_right, DragKind::Icons, &mons, 1, 3).act, CarryAct::None);
        assert_eq!(carry.poll(1201, at_right, DragKind::Icons, &mons, 1, 3).act, CarryAct::Switch(2));
        // the left edge goes back
        let mut carry = Carry::default();
        carry.poll(0, (3, 100), DragKind::Corral, &mons, 1, 3);
        assert_eq!(carry.poll(600, (3, 100), DragKind::Corral, &mons, 1, 3).act, CarryAct::Switch(0));
        // a missing neighbour on the left never carries
        let mut carry = Carry::default();
        carry.poll(0, (3, 100), DragKind::Corral, &mons, 0, 3);
        assert_eq!(carry.poll(5000, (3, 100), DragKind::Corral, &mons, 0, 3).act, CarryAct::None);
    }

    #[test]
    fn only_a_dragged_corral_adds_a_range_on_the_last_one_below_the_limit() {
        let mons = [mon(0, 1920, 96)];
        let edge = (1919, 400);
        // an icon drag never adds
        let mut carry = Carry::default();
        carry.poll(0, edge, DragKind::Icons, &mons, 2, 3);
        assert_eq!(carry.poll(5000, edge, DragKind::Icons, &mons, 2, 3).act, CarryAct::None);
        // a Corral adds after 1000 ms, not before
        let mut carry = Carry::default();
        let o = carry.poll(0, edge, DragKind::Corral, &mons, 2, 3);
        assert!(o.progress.is_some_and(|p| p.add));
        assert_eq!(carry.poll(999, edge, DragKind::Corral, &mons, 2, 3).act, CarryAct::None);
        assert_eq!(carry.poll(1000, edge, DragKind::Corral, &mons, 2, 3).act, CarryAct::Add);
        // never beyond MAX_RANGES, and never from the left edge
        let mut carry = Carry::default();
        carry.poll(0, edge, DragKind::Corral, &mons, MAX_RANGES - 1, MAX_RANGES);
        assert_eq!(carry.poll(9000, edge, DragKind::Corral, &mons, MAX_RANGES - 1, MAX_RANGES).act, CarryAct::None);
        let mut carry = Carry::default();
        carry.poll(0, (2, 400), DragKind::Corral, &mons, 0, 1);
        assert_eq!(carry.poll(9000, (2, 400), DragKind::Corral, &mons, 0, 1).act, CarryAct::None);
        // not on a middle Range (it has a right neighbour: that is a plain carry)
        let mut carry = Carry::default();
        carry.poll(0, edge, DragKind::Corral, &mons, 0, 3);
        assert_eq!(carry.poll(600, edge, DragKind::Corral, &mons, 0, 3).act, CarryAct::Switch(1));
    }

    #[test]
    fn the_carry_strip_is_on_free_edges_only_and_scales_with_the_monitor() {
        let two = [mon(0, 1920, 96), Mon::new(Rect::new(1920, 0, 1920 + 3000, 1500), 144)];
        // the seam between the monitors is no edge
        assert!(edge_zone_at(&two, (1919, 100), CARRY_ZONE_DIP).is_none());
        assert!(edge_zone_at(&two, (1921, 100), CARRY_ZONE_DIP).is_none());
        assert_eq!(edge_zone_at(&two, (23, 100), CARRY_ZONE_DIP), Some((0, Side::Left)));
        assert_eq!(edge_zone_at(&two, (24, 100), CARRY_ZONE_DIP), None);
        // 24 DIP at 150 % (144 dpi) is 36 px
        assert_eq!(edge_zone_at(&two, (4919 - 35, 100), CARRY_ZONE_DIP), Some((1, Side::Right)));
        assert_eq!(edge_zone_at(&two, (4919 - 36, 100), CARRY_ZONE_DIP), None);
    }

    #[test]
    fn empty_ranges_are_deleted_except_the_current_and_the_protected_one() {
        let occupied = occupancy(5, [0, 3, 3]);
        assert_eq!(occupied, [true, false, false, true, false]);
        // standing on 3: 4, 2 and 1 go (highest first)
        assert_eq!(plan_prune(&occupied, 3, &[]), [4, 2, 1]);
        // standing on an empty Range keeps it for now
        assert_eq!(plan_prune(&occupied, 2, &[]), [4, 1]);
        // a drag in flight protects its Range
        assert_eq!(plan_prune(&occupied, 3, &[4]), [2, 1]);
        // everything empty: only the current one stays; Range 0 goes when it is not current
        assert_eq!(plan_prune(&occupancy(3, []), 1, &[]), [2, 0]);
        assert_eq!(plan_prune(&occupancy(3, []), 0, &[]), [2, 1]);
        // out-of-range numbers are ignored
        assert_eq!(occupancy(2, [-1, 5, 1]), [false, true]);
    }

    #[test]
    fn an_added_range_is_empty_again_after_esc_and_goes_while_a_pinned_only_range_counts_as_empty() {
        // Corrals on 0 and 2; a Corral drag added Range 3 and showed it, then Esc went back to 2
        let numbers = [0, 2];
        let occupied = occupancy(4, numbers);
        assert_eq!(plan_prune(&occupied, 2, &[]), [3, 1]);
        // while the drag is in flight the Range it is on is protected, afterwards it is not
        assert_eq!(plan_prune(&occupied, 3, &[3]), [1]);
        // a Range holding only pinned Corrals is empty: the caller leaves pinned ones out of the numbers
        let unpinned_only = occupancy(3, [0]);
        assert_eq!(plan_prune(&unpinned_only, 0, &[]), [2, 1]);
        // the last remaining Range is never removed
        assert_eq!(plan_prune(&occupancy(1, []), 0, &[]), Vec::<i32>::new());
    }

    #[test]
    fn with_the_master_off_every_event_passes_and_nothing_is_decided() {
        let mons = [mon(0, 1200, 96)];
        let mut off = ctx(&mons, 1, 3);
        off.gate = false;
        let mut r = Recognizer::default();
        let e = held(true, false, false);
        assert_eq!(r.pointer(down(0, (2, 10), Button::Left, e, Target::EmptyDesktop), &off), R);
        assert_eq!(r.pointer(mv(5, (300, 10), e, Target::EmptyDesktop), &off), R);
        assert_eq!(r.pointer(up(9, (300, 10), Button::Left, held(false, false, false), Target::EmptyDesktop), &off), R);
        assert_eq!(r.wheel(wheel(0, 500, 0, Mods::default()), &off), R);
        assert_eq!(r.wheel(wheel(1, 0, 240, ALT), &off), R);
        assert_eq!(r.key(key(0, VK_RIGHT, true, false, ALT), &off), R);
        assert_eq!(r.key(key(1, VK_RIGHT, false, false, ALT), &off), R);
        assert_eq!(Carry::default().poll(0, (1, 1), DragKind::Corral, &mons, 1, 3).act, CarryAct::None);
    }

    #[test]
    fn deleting_a_range_decrements_everything_above_it() {
        let removed = [4, 2, 1];
        let numbers = [0, 1, 2, 3, 4, 5];
        let after: Vec<i32> = numbers.iter().map(|&n| renumber(n, &removed)).collect();
        assert_eq!(after, [0, 1, 1, 1, 2, 2]);
        // the current Range is renumbered the same way
        assert_eq!(renumber(3, &removed), 1);
        assert_eq!(renumber(3, &[]), 3);
    }

    #[test]
    fn the_transition_position_stays_continuous_when_a_range_is_deleted() {
        for k in 0..4 {
            let mut last = None;
            for i in 0..=40 {
                let s = f64::from(i) * 0.1;
                let t = remap_position(s, k);
                if let Some(prev) = last {
                    assert!(t >= prev - 1e-9 && t - prev <= 0.1 + 1e-9, "k={k} s={s}");
                }
                last = Some(t);
            }
        }
        assert_eq!(remap_position(1.0, 2), 1.0);
        assert_eq!(remap_position(3.0, 2), 2.0);
        assert_eq!(remap_position(4.0, 2), 3.0);
        assert_eq!(remap_position(2.5, 2), 2.0);
    }

    #[test]
    fn stored_negative_numbers_are_lifted_and_large_ones_are_left_alone() {
        assert_eq!(lift_negatives(&[0, 1, 2]), None);
        assert_eq!(lift_negatives(&[]), None);
        assert_eq!(lift_negatives(&[-2, 0, 3]), Some(2));
        assert_eq!(lift_negatives(&[0, 20]), None, "no lossy clamp");
    }

    #[test]
    fn a_stored_row_of_an_absent_item_keeps_its_range_and_blocks_pruning() {
        // Corral on 0, listed icon on 0, and a stored row (item not in the list right now) on 2
        assert_eq!(removals(3, 0, &[0], &[0], &[2]), [1]);
        assert_eq!(removals(3, 0, &[0], &[0], &[]), [2, 1]);
        // renumbering never has to touch that row: Range 2 was not removed
        assert!(!removals(3, 0, &[0], &[0], &[2]).contains(&2));
    }

    #[test]
    fn corrals_follow_their_number_and_pinned_ones_are_everywhere() {
        for current in 0..3 {
            assert_eq!(group_visible(true, 1, false, current), current == 1);
            assert!(group_visible(true, 1, true, current));
            assert_eq!(icon_visible(true, 1, current), current == 1);
            assert!(icon_visible(false, 1, current));
            assert!(group_visible(false, 1, false, current));
        }
    }

    #[test]
    fn the_stored_choice_picks_the_curve_and_duration_and_animations_off_swaps() {
        assert_eq!(transition(1, true), Some((Curve::Out, 240.0)));
        assert_eq!(transition(2, true), Some((Curve::Bounce, 360.0)));
        assert_eq!(transition(0, true), None);
        assert_eq!(transition(1, false), None, "Windows animations off beat Slide");
        assert_eq!(transition(2, false), None);
        let mut t = Tween::at_rest(0.0);
        retarget_offset(&mut t, 0.0, -1000.0, 1000, 2, false);
        assert_eq!(t.value(0.0), -1000.0, "instant swap");
    }

    #[test]
    fn a_full_width_slide_takes_240_ms_and_a_bounce_overshoots_then_settles() {
        let mut t = Tween::at_rest(0.0);
        retarget_offset(&mut t, 0.0, -1000.0, 1000, 1, true);
        assert!(!t.finished(239.0) && t.finished(240.0));
        let mut last = 0.0;
        for ms in 0..=240 {
            let v = t.value(f64::from(ms));
            assert!(v <= last + 1e-9 && v >= -1000.0 - 1e-9, "monotone, no overshoot");
            last = v;
        }
        let mut b = Tween::at_rest(0.0);
        retarget_offset(&mut b, 0.0, -1000.0, 1000, 2, true);
        assert!(!b.finished(359.0) && b.finished(360.0));
        let furthest = (0..=360).map(|ms| b.value(f64::from(ms))).fold(0.0_f64, f64::min);
        assert!(furthest < -1020.0 && furthest > -1040.0, "about 3 % past the end: {furthest}");
        assert_eq!(b.value(360.0), -1000.0);
    }

    #[test]
    fn interrupting_retargets_from_where_it_is_with_the_clamped_duration() {
        for at in [0.0, 0.4, 0.9] {
            let mut t = Tween::at_rest(0.0);
            retarget_offset(&mut t, 0.0, -1000.0, 1000, 1, true);
            let now = 240.0 * at;
            let shown = t.value(now);
            // back to the start: the distance left is |shown|, but never less than half the base time
            retarget_offset(&mut t, now, 0.0, 1000, 1, true);
            assert!((t.value(now) - shown).abs() < 1e-6, "no jump at {at}");
            if shown != 0.0 {
                assert!(!t.finished(now + 119.0) && t.finished(now + 240.0));
            }
        }
        // a long retarget (two pages) is not slower than a normal one
        let mut t = Tween::at_rest(0.0);
        retarget_offset(&mut t, 0.0, -2000.0, 1000, 1, true);
        assert!(t.finished(240.0));
    }

    #[test]
    fn a_switch_mid_transition_takes_its_direction_from_the_previous_target() {
        // 0 -> 1 -> 2 -> back to 1 -> back to 0, the third and fourth slide the other way
        let (mut from_page, mut target) = (Some(0), 1);
        let mut steps = Vec::new();
        for next in [2, 1, 0] {
            let plan = retarget_plan(from_page, target, next);
            steps.push(plan);
            if let Retarget::Continue { .. } = plan { from_page = None; }
            target = next;
        }
        assert_eq!(steps, [
            Retarget::Continue { direction: 1 },
            Retarget::Continue { direction: -1 },
            Retarget::Continue { direction: -1 },
        ]);
        // the pictures are reused only while `from` still shows a real Range
        assert_eq!(retarget_plan(Some(0), 1, 0), Retarget::Return);
        assert_eq!(retarget_plan(None, 1, 0), Retarget::Continue { direction: -1 });
        assert_eq!(retarget_plan(Some(0), 1, 1), Retarget::Ignore);
        // and the entering Range then comes from the side the previous target is on
        assert_eq!(range_xs(2, 1, 0, 1000).1, Some(-1000));
    }

    #[test]
    fn ranges_sit_at_i_minus_s_times_the_width() {
        let width = 1000;
        for (current, candidate) in [(0, 1), (0, -1), (3, 4), (-2, -3), (5, 5)] {
            for offset in (-1000..=1000).step_by(125) {
                let s = f64::from(current) - f64::from(offset) / f64::from(width);
                let (leaving, entering) = range_xs(current, candidate, offset, width);
                assert_eq!(leaving, ((f64::from(current) - s) * f64::from(width)).round() as i32);
                match entering {
                    Some(x) => assert_eq!(x, ((f64::from(candidate) - s) * f64::from(width)).round() as i32),
                    None => assert_eq!(candidate, current),
                }
            }
        }
        // a higher Range comes in from the right, a lower one from the left
        assert_eq!(range_xs(0, 1, 0, 1000).1, Some(1000));
        assert_eq!(range_xs(0, -1, 0, 1000).1, Some(-1000));
        assert_eq!(range_xs(0, 1, -1000, 1000), (-1000, Some(0)));
        // a Range is drawn only while |i - s| < 1
        for (offset, left_visible, entering_visible) in [(0, true, false), (-500, true, true), (-1000, false, true)] {
            let (a, b) = range_xs(0, 1, offset, width);
            assert_eq!(a.abs() < width, left_visible);
            assert_eq!(b.is_some_and(|x| x.abs() < width), entering_visible);
        }
    }
}
