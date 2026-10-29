//! Resize zones, resizing, move snapping and keep-on-screen (grid-snapping-spec 3, 4 and 5).
//! Pure: no Win32, no globals. Every rectangle of one call lives in the same pixel space (the caller
//! picks monitor-local pixels, origin = the governing monitor's work-area top-left); nothing here depends
//! on where that origin is. "Outer rect" = body plus title strip, "body" = the stored `rect`.

use crate::geometry::Rect;
use crate::icon_metrics::{dip_to_px, round_half_up, CellMetrics};

/// Smallest body in DIPs (spec 3.3).
pub const MIN_BODY_DIP: (i32, i32) = (64, 48);
/// Corner zone edge, edge band depth, capture and release distance in DIPs (spec 3.1, 4.3).
pub const CORNER_DIP: f64 = 16.0;
pub const BAND_DIP: f64 = 8.0;
pub const CAPTURE_DIP: f64 = 8.0;
pub const RELEASE_DIP: f64 = 12.0;
/// Pointer-independent floors of the zones, in pixels (spec 3.1).
const CORNER_MIN_PX: i32 = 8;
const BAND_MIN_PX: i32 = 4;

/// Smallest body (px) at `dpi` (spec 3.3).
pub fn min_body_px(dpi: u32) -> (i32, i32) {
    (dip_to_px(f64::from(MIN_BODY_DIP.0), dpi).max(1), dip_to_px(f64::from(MIN_BODY_DIP.1), dpi).max(1))
}

pub fn capture_px(dpi: u32) -> i32 {
    dip_to_px(CAPTURE_DIP, dpi).max(1)
}

pub fn release_px(dpi: u32) -> i32 {
    dip_to_px(RELEASE_DIP, dpi).max(capture_px(dpi))
}

// ---------------------------------------------------------------------------------------------
// Settings (spec 4.3, 8)
// ---------------------------------------------------------------------------------------------

/// The snapping settings. `padding == 0` is the "Keep a gap between Corrals" toggle off.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SnapSettings {
    /// `SnapEnabled`: master switch for all snapping and whole-cell sizing.
    pub enabled: bool,
    /// `SnapPadding`, 0..=100.
    pub padding: u32,
    /// `SnapShouldScale`: the padding follows the DPI.
    pub should_scale: bool,
    /// `SnapLineUpCorrals`: alignment targets.
    pub line_up: bool,
    /// `SnapToDiscreteSizes`: whole-cell sizing (needs `enabled`).
    pub discrete: bool,
}

impl SnapSettings {
    pub fn new(enabled: bool, padding: u32, should_scale: bool, line_up: bool, discrete: bool) -> Self {
        Self { enabled, padding: padding.min(100), should_scale, line_up, discrete }
    }

    /// Gap between outer rects in px at the governing monitor's DPI (spec 4.3).
    pub fn gap_px(&self, dpi: u32) -> i32 {
        if !self.enabled || self.padding == 0 {
            0
        } else if self.should_scale {
            dip_to_px(f64::from(self.padding), dpi)
        } else {
            self.padding as i32
        }
    }

    /// Whole-cell sizing is active (spec 3.4).
    pub fn whole_cells(&self) -> bool {
        self.enabled && self.discrete
    }
}

// ---------------------------------------------------------------------------------------------
// Zones (spec 3.1)
// ---------------------------------------------------------------------------------------------

/// Hit-test flags: LEFT 0x10, RIGHT 0x20, TOP 0x40, BOTTOM 0x80, TITLE 0x02, BODY 0x01.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EdgeZone(pub u32);

impl EdgeZone {
    pub const NONE: EdgeZone = EdgeZone(0);
    pub const BODY: EdgeZone = EdgeZone(0x01);
    pub const TITLE: EdgeZone = EdgeZone(0x02);
    pub const LEFT: EdgeZone = EdgeZone(0x10);
    pub const RIGHT: EdgeZone = EdgeZone(0x20);
    pub const TOP: EdgeZone = EdgeZone(0x40);
    pub const BOTTOM: EdgeZone = EdgeZone(0x80);

    pub const fn bits(self) -> u32 {
        self.0
    }
    /// Shares at least one bit with `other`.
    pub const fn intersects(self, other: EdgeZone) -> bool {
        self.0 & other.0 != 0
    }
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
    /// Any of the four edge bits.
    pub const fn is_edge(self) -> bool {
        self.0 & 0xf0 != 0
    }
    /// Left and/or right bit.
    pub const fn horizontal(self) -> bool {
        self.0 & 0x30 != 0
    }
    /// Top and/or bottom bit.
    pub const fn vertical(self) -> bool {
        self.0 & 0xc0 != 0
    }
}

impl std::ops::BitOr for EdgeZone {
    type Output = EdgeZone;
    fn bitor(self, rhs: EdgeZone) -> EdgeZone {
        EdgeZone(self.0 | rhs.0)
    }
}

/// What [`resize_zone`] needs to know about the Corral.
#[derive(Clone, Copy, Debug)]
pub struct ZoneCtx<'a> {
    pub dpi: u32,
    /// The desktop is locked: the whole Corral answers as body.
    pub locked: bool,
    /// Folded: only the left and right edges answer (spec 3.6).
    pub folded: bool,
    /// A docked edge has no zone (spec 3.6).
    pub no_top: bool,
    pub no_bottom: bool,
    /// The title's own buttons (menu, close, Gate glyph) in the same space; they win inside the title.
    pub buttons: &'a [Rect],
}

/// Zone of `p` on a Corral whose outer rect is `outer` and whose title strip is `title`: corner beats
/// edge band beats title beats body (spec 3.1). Points outside `outer` are `NONE` (no invisible margin).
pub fn resize_zone(outer: Rect, title: Rect, p: (i32, i32), ctx: &ZoneCtx) -> EdgeZone {
    if !outer.contains(p.0, p.1) {
        return EdgeZone::NONE;
    }
    if ctx.locked {
        return EdgeZone::BODY;
    }
    if ctx.buttons.iter().any(|b| b.contains(p.0, p.1)) {
        return EdgeZone::TITLE;
    }
    let (w, h) = (outer.width(), outer.height());
    let corner = dip_to_px(CORNER_DIP, ctx.dpi);
    let (cw, ch) = (corner.min(w / 3).max(CORNER_MIN_PX), corner.min(h / 3).max(CORNER_MIN_PX));
    let band = dip_to_px(BAND_DIP, ctx.dpi).min(w.min(h) / 3).max(BAND_MIN_PX);
    let (x, y) = (p.0 - outer.left, p.1 - outer.top);
    let mut zone = 0u32;
    if (x < cw || x >= w - cw) && (y < ch || y >= h - ch) {
        zone |= if x < cw { EdgeZone::LEFT.0 } else { EdgeZone::RIGHT.0 };
        zone |= if y < ch { EdgeZone::TOP.0 } else { EdgeZone::BOTTOM.0 };
    } else {
        if x < band { zone |= EdgeZone::LEFT.0; }
        if x >= w - band { zone |= EdgeZone::RIGHT.0; }
        if y < band { zone |= EdgeZone::TOP.0; }
        if y >= h - band { zone |= EdgeZone::BOTTOM.0; }
    }
    let mut off = 0u32;
    if ctx.folded || ctx.no_top { off |= EdgeZone::TOP.0; }
    if ctx.folded || ctx.no_bottom { off |= EdgeZone::BOTTOM.0; }
    if zone & off != 0 {
        // A corner next to a disabled edge becomes the plain left/right edge, only inside the band.
        zone &= !off;
        if zone & EdgeZone::LEFT.0 != 0 && x >= band { zone &= !EdgeZone::LEFT.0; }
        if zone & EdgeZone::RIGHT.0 != 0 && x < w - band { zone &= !EdgeZone::RIGHT.0; }
    }
    if zone != 0 {
        EdgeZone(zone)
    } else if title.contains(p.0, p.1) {
        EdgeZone::TITLE
    } else {
        EdgeZone::BODY
    }
}

/// System cursor chosen by the zone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorKind {
    Arrow,
    SizeWE,
    SizeNS,
    SizeNWSE,
    SizeNESW,
    SizeAll,
    /// Over a title glyph; chosen by the caller, not by `cursor_for`.
    Hand,
}

/// Horizontal edge -> WE, vertical -> NS, left+top / right+bottom -> NWSE, right+top / left+bottom ->
/// NESW, title -> SIZEALL, else arrow.
pub fn cursor_for(zone: EdgeZone) -> CursorKind {
    let l = zone.intersects(EdgeZone::LEFT);
    let rt = zone.intersects(EdgeZone::RIGHT);
    let t = zone.intersects(EdgeZone::TOP);
    let bt = zone.intersects(EdgeZone::BOTTOM);
    if (l && t) || (rt && bt) {
        CursorKind::SizeNWSE
    } else if (rt && t) || (l && bt) {
        CursorKind::SizeNESW
    } else if l || rt {
        CursorKind::SizeWE
    } else if t || bt {
        CursorKind::SizeNS
    } else if zone.intersects(EdgeZone::TITLE) {
        CursorKind::SizeAll
    } else {
        CursorKind::Arrow
    }
}

// ---------------------------------------------------------------------------------------------
// Snap targets (spec 4.2, 4.3)
// ---------------------------------------------------------------------------------------------

/// Kind of a snap target; the declaration order is the tie-break order (spec 4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TargetKind {
    Work,
    Adjacent,
    Aligned,
}

/// One position an edge of the moving rect may snap to. `edge` 0 = low side (left/top), 1 = high.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub edge: u8,
    pub pos: i32,
    pub kind: TargetKind,
    pub source: Option<usize>,
}

/// The target each axis currently holds (edge, position); makes snapping sticky (spec 4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SnapLock {
    pub x: Option<(u8, i32)>,
    pub y: Option<(u8, i32)>,
}

/// A line drawn along an aligned edge while the snap holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Guide {
    pub vertical: bool,
    pub at: i32,
    pub from: i32,
    pub to: i32,
}

fn axis_candidates(horizontal: bool, work: Rect, neighbours: &[Rect], cross: (i32, i32), gap: i32, capture: i32, line_up: bool) -> Vec<Candidate> {
    let (w_lo, w_hi) = if horizontal { (work.left, work.right) } else { (work.top, work.bottom) };
    let mut v = vec![
        Candidate { edge: 0, pos: w_lo, kind: TargetKind::Work, source: None },
        Candidate { edge: 1, pos: w_hi, kind: TargetKind::Work, source: None },
    ];
    for (i, n) in neighbours.iter().enumerate() {
        let (n_lo, n_hi, c_lo, c_hi) = if horizontal { (n.left, n.right, n.top, n.bottom) } else { (n.top, n.bottom, n.left, n.right) };
        if c_lo < cross.1.saturating_add(capture) && c_hi > cross.0.saturating_sub(capture) {
            v.push(Candidate { edge: 1, pos: n_lo - gap, kind: TargetKind::Adjacent, source: Some(i) });
            v.push(Candidate { edge: 0, pos: n_hi + gap, kind: TargetKind::Adjacent, source: Some(i) });
        }
        if line_up {
            v.push(Candidate { edge: 0, pos: n_lo, kind: TargetKind::Aligned, source: Some(i) });
            v.push(Candidate { edge: 1, pos: n_hi, kind: TargetKind::Aligned, source: Some(i) });
        }
    }
    v
}

/// Chooses the target: the held one while the raw edge is within the release distance, else the
/// nearest within the capture distance (ties: work edge, adjacency, alignment).
fn pick(raw: [i32; 2], allow: [bool; 2], cands: &[Candidate], lock: Option<(u8, i32)>, capture: i32, release: i32) -> Option<Candidate> {
    if let Some((e, t)) = lock {
        if allow[usize::from(e & 1)] && (i64::from(raw[usize::from(e & 1)]) - i64::from(t)).abs() <= i64::from(release) {
            if let Some(c) = cands.iter().filter(|c| c.edge == e && c.pos == t).min_by_key(|c| c.kind) {
                return Some(*c);
            }
        }
    }
    cands
        .iter()
        .filter(|c| allow[usize::from(c.edge)])
        .map(|c| ((i64::from(raw[usize::from(c.edge)]) - i64::from(c.pos)).abs(), *c))
        .filter(|(d, _)| *d <= i64::from(capture))
        .min_by_key(|(d, c)| (*d, c.kind, c.edge, c.pos))
        .map(|(_, c)| c)
}

fn guides_for(rect: Rect, x: Option<Candidate>, y: Option<Candidate>, neighbours: &[Rect]) -> Vec<Guide> {
    let mut out = Vec::new();
    if let Some(c) = x.filter(|c| c.kind == TargetKind::Aligned) {
        if let Some(n) = c.source.and_then(|i| neighbours.get(i)) {
            out.push(Guide { vertical: true, at: c.pos, from: rect.top.min(n.top), to: rect.bottom.max(n.bottom) });
        }
    }
    if let Some(c) = y.filter(|c| c.kind == TargetKind::Aligned) {
        if let Some(n) = c.source.and_then(|i| neighbours.get(i)) {
            out.push(Guide { vertical: false, at: c.pos, from: rect.left.min(n.left), to: rect.right.max(n.right) });
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Move snapping (spec 4)
// ---------------------------------------------------------------------------------------------

pub struct MoveInput<'a> {
    /// The moving outer rect at the raw pointer position.
    pub raw: Rect,
    pub work: Rect,
    /// Outer rects of the neighbours (see [`select_neighbours`]).
    pub neighbours: &'a [Rect],
    pub settings: SnapSettings,
    pub dpi: u32,
    pub shift: bool,
    pub lock: SnapLock,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MoveOutput {
    pub rect: Rect,
    pub lock: SnapLock,
    pub guides: Vec<Guide>,
}

/// Snaps a moving outer rect. Only `x` and `y` change: width and height are copied from `raw`, so a
/// move never drifts (spec 4.5). Shift or `SnapEnabled` off return `raw` untouched.
pub fn snap_move(inp: &MoveInput) -> MoveOutput {
    let raw = inp.raw;
    if !inp.settings.enabled || inp.shift {
        return MoveOutput { rect: raw, lock: SnapLock::default(), guides: Vec::new() };
    }
    let (capture, release, gap) = (capture_px(inp.dpi), release_px(inp.dpi), inp.settings.gap_px(inp.dpi));
    let cx = axis_candidates(true, inp.work, inp.neighbours, (raw.top, raw.bottom), gap, capture, inp.settings.line_up);
    let hx = pick([raw.left, raw.right], [true, true], &cx, inp.lock.x, capture, release);
    let dx = hx.map_or(0, |c| c.pos - if c.edge == 0 { raw.left } else { raw.right });
    let shifted_x = raw.offset(dx, 0);
    let cy = axis_candidates(false, inp.work, inp.neighbours, (shifted_x.left, shifted_x.right), gap, capture, inp.settings.line_up);
    let hy = pick([raw.top, raw.bottom], [true, true], &cy, inp.lock.y, capture, release);
    let dy = hy.map_or(0, |c| c.pos - if c.edge == 0 { raw.top } else { raw.bottom });
    let rect = raw.offset(dx, dy);
    MoveOutput {
        rect,
        lock: SnapLock { x: hx.map(|c| (c.edge, c.pos)), y: hy.map(|c| (c.edge, c.pos)) },
        guides: guides_for(rect, hx, hy, inp.neighbours),
    }
}

// ---------------------------------------------------------------------------------------------
// Neighbours (spec 4.4)
// ---------------------------------------------------------------------------------------------

/// A Corral as the neighbour test sees it.
#[derive(Clone, Copy, Debug)]
pub struct NeighbourInfo {
    pub id: u64,
    /// Tab-group id, 0 = not tabbed.
    pub tab_set: u64,
    /// Index of the governing monitor.
    pub monitor: usize,
    /// Range (desktop page).
    pub range: i32,
    /// Shown right now (not hidden by Clear view, not an inactive tab).
    pub visible: bool,
    /// Outer rect; the folded rect for a folded Corral.
    pub outer: Rect,
}

/// Outer rects of the Corrals `me` may snap to: same monitor, same Range, visible, not itself and not
/// a tab sibling. Faded and folded Corrals count (spec 4.4).
pub fn select_neighbours(me: &NeighbourInfo, all: &[NeighbourInfo]) -> Vec<Rect> {
    all.iter()
        .filter(|o| o.id != me.id && o.visible && o.monitor == me.monitor && o.range == me.range && !(me.tab_set != 0 && o.tab_set == me.tab_set))
        .map(|o| o.outer)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Resize (spec 3.2 - 3.5)
// ---------------------------------------------------------------------------------------------

pub struct ResizeInput<'a> {
    pub zone: EdgeZone,
    /// The body at the press.
    pub start: Rect,
    /// Title strip heights outside the body (above / below); at most one is non-zero.
    pub strip_top: i32,
    pub strip_bottom: i32,
    pub start_pointer: (i32, i32),
    pub pointer: (i32, i32),
    pub dpi: u32,
    pub work: Rect,
    /// Cell metrics of the governing monitor, for whole-cell sizing.
    pub cell: CellMetrics,
    pub settings: SnapSettings,
    pub shift: bool,
    /// Neighbour outer rects.
    pub neighbours: &'a [Rect],
    pub lock: SnapLock,
    /// Minimum body in px; `None` = the spec's minimum, lowered to the start size (spec 3.3). Creation
    /// starts from an empty rect and passes the plain minimum.
    pub min: Option<(i32, i32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResizeOutput {
    /// The new body.
    pub rect: Rect,
    pub lock: SnapLock,
    pub guides: Vec<Guide>,
}

struct AxisIn {
    lo: i32,
    hi: i32,
    delta: i32,
    grab: Option<u8>,
    strip_lo: i32,
    strip_hi: i32,
    work_lo: i32,
    work_hi: i32,
    min: i32,
    /// `Some((cell, pad))` = whole-cell sizing.
    whole: Option<(i32, i32)>,
    snap: bool,
    capture: i32,
    release: i32,
    lock: Option<(u8, i32)>,
}

/// Moves the grabbed edge of one axis; the opposite edge is returned bit for bit (spec 3.2).
fn resize_axis(a: &AxisIn, cands: &[Candidate]) -> (i32, i32, Option<Candidate>) {
    let Some(g) = a.grab else { return (a.lo, a.hi, None) };
    // The limit never pulls an edge that already lies beyond the work area back in.
    let limit_lo = a.work_lo.saturating_add(a.strip_lo).min(a.lo);
    let limit_hi = a.work_hi.saturating_sub(a.strip_hi).max(a.hi);
    let whole = a.whole.map(|(cell, pad)| {
        let cell = cell.max(1);
        let n_min = ((a.min - 2 * pad).max(0) + cell - 1) / cell;
        (cell, pad, n_min.max(1))
    });
    if g == 1 {
        let raw = a.hi.saturating_add(a.delta);
        let floor = a.lo.saturating_add(a.min);
        if let Some((cell, pad, n_min)) = whole {
            let n_max = ((limit_hi - a.lo - 2 * pad).div_euclid(cell)).max(n_min);
            let n = (round_half_up(f64::from(raw - a.lo - 2 * pad) / f64::from(cell)).clamp(i64::from(n_min), i64::from(n_max))) as i32;
            return (a.lo, a.lo + n * cell + 2 * pad, None);
        }
        let mut hit = None;
        let mut v = raw;
        if a.snap {
            let c = pick([raw - a.strip_lo, raw + a.strip_hi], [false, true], cands, a.lock, a.capture, a.release);
            if let Some(c) = c { v = c.pos - a.strip_hi; hit = Some(c); }
        }
        (a.lo, v.min(limit_hi.max(floor)).max(floor), hit)
    } else {
        let raw = a.lo.saturating_add(a.delta);
        let ceil = a.hi.saturating_sub(a.min);
        if let Some((cell, pad, n_min)) = whole {
            let n_max = ((a.hi - limit_lo - 2 * pad).div_euclid(cell)).max(n_min);
            let n = (round_half_up(f64::from(a.hi - raw - 2 * pad) / f64::from(cell)).clamp(i64::from(n_min), i64::from(n_max))) as i32;
            return (a.hi - (n * cell + 2 * pad), a.hi, None);
        }
        let mut hit = None;
        let mut v = raw;
        if a.snap {
            let c = pick([raw - a.strip_lo, raw + a.strip_hi], [true, false], cands, a.lock, a.capture, a.release);
            if let Some(c) = c { v = c.pos + a.strip_lo; hit = Some(c); }
        }
        (v.max(limit_lo.min(ceil)).min(ceil), a.hi, hit)
    }
}

/// One resize step from the press state and the current pointer (not from the previous message, so the
/// result does not depend on how many messages arrived). The edge opposite to the grabbed zone never
/// moves; a side that is not grabbed keeps its value bit for bit.
pub fn resize_rect(inp: &ResizeInput) -> ResizeOutput {
    let z = inp.zone;
    let s = inp.start;
    let (dx, dy) = (inp.pointer.0 - inp.start_pointer.0, inp.pointer.1 - inp.start_pointer.1);
    let min_body = min_body_px(inp.dpi);
    // A Corral already smaller than the minimum may keep that size: resizing never forces it larger.
    let min = inp.min.unwrap_or((min_body.0.min(s.width()).max(1), min_body.1.min(s.height()).max(1)));
    let snap_on = inp.settings.enabled && !inp.shift;
    let whole = snap_on && inp.settings.discrete;
    let edge_snap = snap_on && !inp.settings.discrete;
    let (capture, release, gap) = (capture_px(inp.dpi), release_px(inp.dpi), inp.settings.gap_px(inp.dpi));
    let grab_x = if z.intersects(EdgeZone::LEFT) { Some(0) } else if z.intersects(EdgeZone::RIGHT) { Some(1) } else { None };
    let grab_y = if z.intersects(EdgeZone::TOP) { Some(0) } else if z.intersects(EdgeZone::BOTTOM) { Some(1) } else { None };
    // Outer rect at the raw pointer, to decide which neighbours are beside it.
    let raw_outer = Rect::new(
        s.left + if grab_x == Some(0) { dx } else { 0 },
        s.top - inp.strip_top + if grab_y == Some(0) { dy } else { 0 },
        s.right + if grab_x == Some(1) { dx } else { 0 },
        s.bottom + inp.strip_bottom + if grab_y == Some(1) { dy } else { 0 },
    );
    let line_up = inp.settings.line_up;
    let cx = if edge_snap && grab_x.is_some() { axis_candidates(true, inp.work, inp.neighbours, (raw_outer.top, raw_outer.bottom), gap, capture, line_up) } else { Vec::new() };
    let cy = if edge_snap && grab_y.is_some() { axis_candidates(false, inp.work, inp.neighbours, (raw_outer.left, raw_outer.right), gap, capture, line_up) } else { Vec::new() };
    let cell = inp.cell;
    let (left, right, hx) = resize_axis(&AxisIn {
        lo: s.left, hi: s.right, delta: dx, grab: grab_x, strip_lo: 0, strip_hi: 0,
        work_lo: inp.work.left, work_hi: inp.work.right, min: min.0,
        whole: whole.then_some((cell.cell_w_px, cell.pad_px)), snap: edge_snap, capture, release, lock: inp.lock.x,
    }, &cx);
    let (top, bottom, hy) = resize_axis(&AxisIn {
        lo: s.top, hi: s.bottom, delta: dy, grab: grab_y, strip_lo: inp.strip_top, strip_hi: inp.strip_bottom,
        work_lo: inp.work.top, work_hi: inp.work.bottom, min: min.1,
        whole: whole.then_some((cell.cell_h_px, cell.pad_px)), snap: edge_snap, capture, release, lock: inp.lock.y,
    }, &cy);
    let rect = Rect::new(left, top, right, bottom);
    let outer = Rect::new(rect.left, rect.top - inp.strip_top, rect.right, rect.bottom + inp.strip_bottom);
    ResizeOutput {
        rect,
        lock: SnapLock { x: hx.map(|c| (c.edge, c.pos)), y: hy.map(|c| (c.edge, c.pos)) },
        guides: guides_for(outer, hx, hy, inp.neighbours),
    }
}

// ---------------------------------------------------------------------------------------------
// Size tooltip text (spec 7)
// ---------------------------------------------------------------------------------------------

/// `640 x 360` (U+00D7) in px of the governing monitor; with whole-cell sizing active the cell count is
/// appended: `640 x 360 (8 x 3 icons)`.
pub fn size_tip_text(outer_px: (i32, i32), cells: Option<(i32, i32)>) -> String {
    match cells {
        Some((c, r)) => format!("{} \u{d7} {} ({} \u{d7} {} icons)", outer_px.0, outer_px.1, c, r),
        None => format!("{} \u{d7} {}", outer_px.0, outer_px.1),
    }
}

// ---------------------------------------------------------------------------------------------
// Keep on screen and rescue (spec 5)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placed {
    pub rect: Rect,
    /// Index into the monitor list that was used.
    pub monitor: Option<usize>,
}

fn distance_to(r: &Rect, p: (i32, i32)) -> i64 {
    let dx = (i64::from(r.left) - i64::from(p.0)).max(i64::from(p.0) - i64::from(r.right)).max(0);
    let dy = (i64::from(r.top) - i64::from(p.1)).max(i64::from(p.1) - i64::from(r.bottom)).max(0);
    dx * dx + dy * dy
}

/// The monitor for a rescue (spec 5.1): the engine's `preferred` one, else the one under the pointer,
/// else the largest overlap with the outer rect, else the one nearest its centre.
pub fn rescue_monitor(outer: Rect, works: &[Rect], preferred: Option<usize>, pointer: Option<(i32, i32)>) -> Option<usize> {
    if works.is_empty() {
        return None;
    }
    if let Some(p) = preferred.filter(|p| *p < works.len()) {
        return Some(p);
    }
    if let Some(i) = pointer.and_then(|p| works.iter().position(|w| w.contains(p.0, p.1))) {
        return Some(i);
    }
    let (best, area) = works.iter().enumerate().map(|(i, w)| (i, w.overlap_area(&outer))).max_by_key(|(i, a)| (*a, std::cmp::Reverse(*i)))?;
    if area > 0 {
        return Some(best);
    }
    let centre = ((outer.left + outer.right) / 2, (outer.top + outer.bottom) / 2);
    works.iter().enumerate().min_by_key(|(i, w)| (distance_to(w, centre), *i)).map(|(i, _)| i)
}

/// Puts an outer rect fully inside one work area (spec 5): an axis larger than the work area is reduced
/// to it (live size only; the caller keeps `preferred_size_dip`), then the whole rect is shifted inside.
/// Idempotent, and a rect that is already inside is returned unchanged.
pub fn place_on_screen(outer: Rect, works: &[Rect], preferred: Option<usize>, pointer: Option<(i32, i32)>) -> Placed {
    let Some(i) = rescue_monitor(outer, works, preferred, pointer) else { return Placed { rect: outer, monitor: None } };
    let work = works[i];
    let (w, h) = (outer.width().min(work.width()), outer.height().min(work.height()));
    let left = outer.left.clamp(work.left, work.right - w);
    let top = outer.top.clamp(work.top, work.bottom - h);
    Placed { rect: Rect::new(left, top, left + w, top + h), monitor: Some(i) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icon_metrics::{cell_metrics, CellInputs};

    const DPIS: [u32; 5] = [96, 120, 144, 192, 240];
    const ORIGINS: [(i32, i32); 4] = [(0, 0), (-1920, -200), (-3840, 0), (0, -1080)];

    fn px(dip: f64, dpi: u32) -> i32 {
        dip_to_px(dip, dpi)
    }
    fn cell(dpi: u32) -> CellMetrics {
        cell_metrics(&CellInputs { dpi, zoom_pct: 100, windows_spacing_dip: Some((75.0, 75.0)), desktop_icon_dip: Some(48.0), match_desktop: true })
    }
    fn plain() -> SnapSettings {
        SnapSettings::new(true, 0, true, false, false)
    }
    fn shifted(r: Rect, o: (i32, i32)) -> Rect {
        r.offset(o.0, o.1)
    }
    fn zctx(dpi: u32) -> ZoneCtx<'static> {
        ZoneCtx { dpi, locked: false, folded: false, no_top: false, no_bottom: false, buttons: &[] }
    }

    // ---- settings ----

    #[test]
    fn gap_scales_with_dpi_only_when_asked_and_is_zero_with_the_toggle_off() {
        let scaled = SnapSettings::new(true, 8, true, true, false);
        let literal = SnapSettings::new(true, 8, false, true, false);
        for dpi in DPIS {
            assert_eq!(scaled.gap_px(dpi), (8.0 * f64::from(dpi) / 96.0 + 0.5).floor() as i32, "dpi {dpi}");
            assert_eq!(literal.gap_px(dpi), 8);
            assert_eq!(SnapSettings::new(true, 0, true, true, false).gap_px(dpi), 0, "toggle off");
            assert_eq!(SnapSettings::new(false, 8, true, true, false).gap_px(dpi), 0, "snapping off");
        }
        assert_eq!(SnapSettings::new(true, 999, false, false, false).padding, 100);
        assert!(!SnapSettings::new(false, 8, true, true, true).whole_cells());
        assert!(SnapSettings::new(true, 8, true, true, true).whole_cells());
    }

    // ---- spec 9.4 zones ----

    #[test]
    fn every_zone_is_found_at_every_dpi() {
        for dpi in DPIS {
            let outer = Rect::new(100, 100, 100 + px(300.0, dpi), 100 + px(200.0, dpi));
            let title = Rect::new(outer.left, outer.top, outer.right, outer.top + px(24.0, dpi));
            let (w, h) = (outer.width(), outer.height());
            let band = px(8.0, dpi);
            let c = px(16.0, dpi);
            let z = |x, y| resize_zone(outer, title, (outer.left + x, outer.top + y), &zctx(dpi));
            assert_eq!(z(0, 0), EdgeZone::LEFT | EdgeZone::TOP);
            assert_eq!(z(w - 1, 0), EdgeZone::RIGHT | EdgeZone::TOP);
            assert_eq!(z(0, h - 1), EdgeZone::LEFT | EdgeZone::BOTTOM);
            assert_eq!(z(w - 1, h - 1), EdgeZone::RIGHT | EdgeZone::BOTTOM);
            assert_eq!(z(c - 1, c - 1), EdgeZone::LEFT | EdgeZone::TOP, "corner square is 16 DIP");
            assert_eq!(z(c, c), EdgeZone::TITLE, "just inside the corner square: the title strip is taller than the corner");
            let mid = h / 2;
            assert_eq!(z(0, mid), EdgeZone::LEFT);
            assert_eq!(z(band - 1, mid), EdgeZone::LEFT, "last pixel of the band (inner side, inclusive)");
            assert_eq!(z(band, mid), EdgeZone::BODY, "first pixel past the band");
            assert_eq!(z(w - band, mid), EdgeZone::RIGHT);
            assert_eq!(z(w - band - 1, mid), EdgeZone::BODY);
            assert_eq!(z(w / 2, 0), EdgeZone::TOP);
            assert_eq!(z(w / 2, band - 1), EdgeZone::TOP);
            assert_eq!(z(w / 2, band), EdgeZone::TITLE, "the title strip is below the band");
            assert_eq!(z(w / 2, h - 1), EdgeZone::BOTTOM);
            assert_eq!(z(w / 2, h - band), EdgeZone::BOTTOM);
            assert_eq!(z(w / 2, h - band - 1), EdgeZone::BODY);
            assert_eq!(z(w / 2, title.height() - 1), EdgeZone::TITLE);
            assert_eq!(z(w / 2, title.height()), EdgeZone::BODY);
            // no invisible margin outside the outer rect
            assert_eq!(resize_zone(outer, title, (outer.left - 1, outer.top + mid), &zctx(dpi)), EdgeZone::NONE);
            assert_eq!(resize_zone(outer, title, (outer.right, outer.top + mid), &zctx(dpi)), EdgeZone::NONE);
        }
    }

    #[test]
    fn corner_beats_edge_beats_title_beats_body_and_buttons_win_in_the_title() {
        let dpi = 96;
        let outer = Rect::new(0, 0, 300, 200);
        let title = Rect::new(0, 0, 300, 24);
        let close = Rect::new(270, 4, 290, 20);
        let ctx = ZoneCtx { buttons: &[close], ..zctx(dpi) };
        assert_eq!(resize_zone(outer, title, (285, 10), &ctx), EdgeZone::TITLE, "the close button is inside the right band and corner");
        assert_eq!(resize_zone(outer, title, (285, 10), &zctx(dpi)), EdgeZone::RIGHT | EdgeZone::TOP, "without it the corner wins");
        assert_eq!(resize_zone(outer, title, (150, 4), &ctx), EdgeZone::TOP, "edge beats title");
        assert_eq!(resize_zone(outer, title, (150, 12), &ctx), EdgeZone::TITLE);
        assert_eq!(resize_zone(outer, title, (150, 100), &ctx), EdgeZone::BODY);
        let locked = ZoneCtx { locked: true, ..zctx(dpi) };
        assert_eq!(resize_zone(outer, title, (0, 0), &locked), EdgeZone::BODY);
        assert_eq!(resize_zone(outer, title, (-1, 0), &locked), EdgeZone::NONE);
    }

    #[test]
    fn a_tiny_outer_rect_still_has_sane_zones() {
        for dpi in DPIS {
            let side = px(20.0, dpi);
            let outer = Rect::new(0, 0, side, side);
            let title = Rect::new(0, 0, side, side / 2);
            let mut seen = std::collections::BTreeSet::new();
            for y in 0..side {
                for x in 0..side {
                    let z = resize_zone(outer, title, (x, y), &zctx(dpi));
                    assert!(!z.is_empty());
                    seen.insert(z.bits());
                }
            }
            assert!(seen.contains(&(EdgeZone::LEFT | EdgeZone::TOP).bits()), "dpi {dpi}");
            assert!(seen.contains(&(EdgeZone::RIGHT | EdgeZone::BOTTOM).bits()));
            // the centre is never an edge: a third on each side stays free of bands
            let centre = resize_zone(outer, title, (side / 2, side / 2), &zctx(dpi));
            assert!(!centre.is_edge() || side / 3 < 8, "dpi {dpi}: {centre:?}");
        }
    }

    #[test]
    fn folded_and_docked_corrals_lose_their_vertical_zones() {
        let dpi = 96;
        let outer = Rect::new(0, 0, 300, 24);
        let title = outer;
        let folded = ZoneCtx { folded: true, ..zctx(dpi) };
        assert_eq!(resize_zone(outer, title, (0, 0), &folded), EdgeZone::LEFT, "corner becomes the left edge");
        assert_eq!(resize_zone(outer, title, (299, 23), &folded), EdgeZone::RIGHT);
        assert_eq!(resize_zone(outer, title, (150, 0), &folded), EdgeZone::TITLE, "no top zone");
        assert_eq!(resize_zone(outer, title, (150, 23), &folded), EdgeZone::TITLE, "no bottom zone");
        assert_eq!(resize_zone(outer, title, (12, 12), &folded), EdgeZone::TITLE, "a corner point beyond the band is not an edge");
        let tall = Rect::new(0, 0, 300, 200);
        let docked_top = ZoneCtx { no_top: true, ..zctx(dpi) };
        assert_eq!(resize_zone(tall, Rect::new(0, 0, 300, 24), (150, 0), &docked_top), EdgeZone::TITLE);
        assert_eq!(resize_zone(tall, Rect::new(0, 0, 300, 24), (0, 0), &docked_top), EdgeZone::LEFT);
        assert_eq!(resize_zone(tall, Rect::new(0, 0, 300, 24), (150, 199), &docked_top), EdgeZone::BOTTOM, "the free edge stays");
        assert_eq!(resize_zone(tall, Rect::new(0, 0, 300, 24), (299, 199), &docked_top), EdgeZone::RIGHT | EdgeZone::BOTTOM);
        let docked_bottom = ZoneCtx { no_bottom: true, ..zctx(dpi) };
        assert_eq!(resize_zone(tall, Rect::new(0, 176, 300, 200), (150, 199), &docked_bottom), EdgeZone::TITLE);
        assert_eq!(resize_zone(tall, Rect::new(0, 176, 300, 200), (150, 0), &docked_bottom), EdgeZone::TOP);
    }

    #[test]
    fn cursors_follow_the_zone() {
        use CursorKind::*;
        assert_eq!(cursor_for(EdgeZone::LEFT), SizeWE);
        assert_eq!(cursor_for(EdgeZone::BOTTOM), SizeNS);
        assert_eq!(cursor_for(EdgeZone::LEFT | EdgeZone::TOP), SizeNWSE);
        assert_eq!(cursor_for(EdgeZone::RIGHT | EdgeZone::BOTTOM), SizeNWSE);
        assert_eq!(cursor_for(EdgeZone::RIGHT | EdgeZone::TOP), SizeNESW);
        assert_eq!(cursor_for(EdgeZone::LEFT | EdgeZone::BOTTOM), SizeNESW);
        assert_eq!(cursor_for(EdgeZone::TITLE), SizeAll);
        assert_eq!(cursor_for(EdgeZone::BODY), Arrow);
    }

    // ---- resize helpers ----

    fn base_input<'a>(zone: EdgeZone, dpi: u32, start: Rect, dx: i32, dy: i32, neighbours: &'a [Rect]) -> ResizeInput<'a> {
        ResizeInput {
            zone, start, strip_top: px(24.0, dpi), strip_bottom: 0, start_pointer: (500, 500), pointer: (500 + dx, 500 + dy),
            dpi, work: Rect::new(0, 0, px(1920.0, dpi), px(1080.0, dpi)), cell: cell(dpi), settings: plain(), shift: false, neighbours,
            lock: SnapLock::default(), min: None,
        }
    }
    const ZONES: [EdgeZone; 8] = [
        EdgeZone::LEFT, EdgeZone::RIGHT, EdgeZone::TOP, EdgeZone::BOTTOM,
        EdgeZone(EdgeZone::LEFT.0 | EdgeZone::TOP.0), EdgeZone(EdgeZone::RIGHT.0 | EdgeZone::TOP.0),
        EdgeZone(EdgeZone::LEFT.0 | EdgeZone::BOTTOM.0), EdgeZone(EdgeZone::RIGHT.0 | EdgeZone::BOTTOM.0),
    ];

    // ---- spec 9.5 anchor ----

    #[test]
    fn the_opposite_side_and_ungrabbed_axes_are_bit_identical() {
        for dpi in DPIS {
            let start = Rect::new(px(300.0, dpi) + 1, px(200.0, dpi) + 1, px(700.0, dpi) + 3, px(500.0, dpi) + 3);
            for zone in ZONES {
                for (dx, dy) in [(37, 41), (-37, -41), (-5000, -5000), (5000, 5000), (0, 0), (1, -1)] {
                    for (shift, discrete) in [(false, false), (true, false), (false, true)] {
                        let mut inp = base_input(zone, dpi, start, dx, dy, &[]);
                        inp.shift = shift;
                        inp.settings = SnapSettings::new(true, 0, true, false, discrete);
                        let out = resize_rect(&inp).rect;
                        let tag = format!("dpi {dpi} zone {:#x} d=({dx},{dy}) shift {shift} discrete {discrete}", zone.bits());
                        if !zone.intersects(EdgeZone::LEFT) { assert_eq!(out.left, start.left, "{tag}"); }
                        if !zone.intersects(EdgeZone::RIGHT) { assert_eq!(out.right, start.right, "{tag}"); }
                        if !zone.intersects(EdgeZone::TOP) { assert_eq!(out.top, start.top, "{tag}"); }
                        if !zone.intersects(EdgeZone::BOTTOM) { assert_eq!(out.bottom, start.bottom, "{tag}"); }
                        assert!(out.width() >= min_body_px(dpi).0.min(start.width()) && out.height() >= min_body_px(dpi).1.min(start.height()), "{tag}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_grabbed_edge_follows_the_pointer_by_the_exact_delta() {
        for dpi in DPIS {
            let start = Rect::new(400, 300, 400 + px(300.0, dpi), 300 + px(200.0, dpi));
            let out = |zone, dx, dy| resize_rect(&base_input(zone, dpi, start, dx, dy, &[])).rect;
            assert_eq!(out(EdgeZone::RIGHT, 33, 99), Rect::new(start.left, start.top, start.right + 33, start.bottom));
            assert_eq!(out(EdgeZone::LEFT, 33, 99), Rect::new(start.left + 33, start.top, start.right, start.bottom));
            assert_eq!(out(EdgeZone::TOP, 33, -27), Rect::new(start.left, start.top - 27, start.right, start.bottom));
            assert_eq!(out(EdgeZone::BOTTOM, 33, 27), Rect::new(start.left, start.top, start.right, start.bottom + 27));
            assert_eq!(out(EdgeZone::RIGHT | EdgeZone::BOTTOM, 10, 20), Rect::new(start.left, start.top, start.right + 10, start.bottom + 20));
            assert_eq!(out(EdgeZone::LEFT | EdgeZone::TOP, -10, -20), Rect::new(start.left - 10, start.top - 20, start.right, start.bottom));
            // path independence: the same pointer gives the same rect however it got there
            assert_eq!(out(EdgeZone::RIGHT, 33, 0), out(EdgeZone::RIGHT, 33, 0));
        }
    }

    #[test]
    fn minimum_and_work_area_stop_the_grabbed_edge_only() {
        let dpi = 96;
        let start = Rect::new(400, 300, 700, 500);
        let min = min_body_px(dpi);
        let r = resize_rect(&base_input(EdgeZone::RIGHT, dpi, start, -5000, 0, &[])).rect;
        assert_eq!((r.left, r.right), (400, 400 + min.0));
        let r = resize_rect(&base_input(EdgeZone::LEFT, dpi, start, 5000, 0, &[])).rect;
        assert_eq!((r.left, r.right), (700 - min.0, 700));
        let r = resize_rect(&base_input(EdgeZone::BOTTOM, dpi, start, 0, -5000, &[])).rect;
        assert_eq!((r.top, r.bottom), (300, 300 + min.1));
        let r = resize_rect(&base_input(EdgeZone::TOP, dpi, start, 0, 5000, &[])).rect;
        assert_eq!((r.top, r.bottom), (500 - min.1, 500));
        // work area: the outer rect (strip included) stops at the edge
        let r = resize_rect(&base_input(EdgeZone::RIGHT, dpi, start, 5000, 0, &[])).rect;
        assert_eq!(r.right, 1920);
        let r = resize_rect(&base_input(EdgeZone::BOTTOM, dpi, start, 0, 5000, &[])).rect;
        assert_eq!(r.bottom, 1080);
        let r = resize_rect(&base_input(EdgeZone::LEFT, dpi, start, -5000, 0, &[])).rect;
        assert_eq!(r.left, 0);
        let r = resize_rect(&base_input(EdgeZone::TOP, dpi, start, 0, -5000, &[])).rect;
        assert_eq!(r.top - 24, 0, "the title strip above the body also has to fit");
        // an already smaller Corral keeps its size and is never forced larger
        let small = Rect::new(400, 300, 440, 330);
        let r = resize_rect(&base_input(EdgeZone::RIGHT, dpi, small, -5000, 0, &[])).rect;
        assert_eq!(r.width(), 40);
        let r = resize_rect(&base_input(EdgeZone::BOTTOM, dpi, small, 0, 7, &[])).rect;
        assert_eq!(r.height(), 37);
        // beyond the work area from the start (a file loaded with "don't adjust"): not pulled back
        let outside = Rect::new(1800, 300, 2100, 500);
        let r = resize_rect(&base_input(EdgeZone::RIGHT, dpi, outside, 10, 0, &[])).rect;
        assert_eq!(r.right, 2100);
        let r = resize_rect(&base_input(EdgeZone::RIGHT, dpi, outside, -10, 0, &[])).rect;
        assert_eq!(r.right, 2090);
    }

    // ---- spec 9.6 whole cells ----

    #[test]
    fn whole_cell_sizes_are_n_cells_plus_padding_with_an_exact_half_cell_jump() {
        for dpi in DPIS {
            let c = cell(dpi);
            let start = Rect::new(100, 100, 100 + c.body_size_for(3, 2).0, 100 + c.body_size_for(3, 2).1);
            let settings = SnapSettings::new(true, 0, true, false, true);
            let width_after = |dx: i32| {
                let mut i = base_input(EdgeZone::RIGHT, dpi, start, dx, 0, &[]);
                i.settings = settings;
                resize_rect(&i).rect.width()
            };
            for n in 1..=6 {
                let target = c.body_size_for(n, 1).0;
                assert_eq!(width_after(target - start.width()), target, "dpi {dpi} n {n}");
            }
            // half a cell: floor(raw_inner / cell + 0.5) jumps exactly at n + 0.5
            let raw_for = |extra: i32| width_after(extra);
            let below = (c.cell_w_px - 1) / 2; // strictly below half a cell
            assert_eq!(raw_for(below), start.width(), "dpi {dpi}: just under half a cell stays");
            let at = (c.cell_w_px + 1) / 2; // half a cell (rounded up) or more
            assert_eq!(raw_for(at), start.width() + c.cell_w_px, "dpi {dpi}: half a cell jumps");
            // minimum and maximum
            assert_eq!(width_after(-5000), c.body_size_for(1, 1).0.max(((min_body_px(dpi).0 - 2 * c.pad_px).max(0) + c.cell_w_px - 1) / c.cell_w_px * c.cell_w_px + 2 * c.pad_px));
            let widest = width_after(5000);
            let space = px(1920.0, dpi) - start.left;
            assert_eq!(widest, c.body_size_for(((space - 2 * c.pad_px) / c.cell_w_px).max(1), 1).0);
            assert!(start.left + widest <= px(1920.0, dpi));
        }
    }

    #[test]
    fn without_discrete_sizes_a_resize_is_raw_pixels_and_shift_cancels_whole_cells() {
        for dpi in DPIS {
            let start = Rect::new(100, 100, 100 + px(333.0, dpi), 100 + px(222.0, dpi));
            let mut i = base_input(EdgeZone::RIGHT | EdgeZone::BOTTOM, dpi, start, 13, 17, &[]);
            assert_eq!(resize_rect(&i).rect, Rect::new(start.left, start.top, start.right + 13, start.bottom + 17));
            i.settings = SnapSettings::new(true, 0, true, false, true);
            assert_ne!(resize_rect(&i).rect, Rect::new(start.left, start.top, start.right + 13, start.bottom + 17));
            i.shift = true;
            assert_eq!(resize_rect(&i).rect, Rect::new(start.left, start.top, start.right + 13, start.bottom + 17), "dpi {dpi}: Shift gives the raw pointer result");
            i.shift = false;
            i.settings = SnapSettings::new(false, 0, true, false, true);
            assert_eq!(resize_rect(&i).rect, Rect::new(start.left, start.top, start.right + 13, start.bottom + 17), "SnapEnabled off removes whole cells");
        }
    }

    // ---- spec 9.7 Shift ----

    #[test]
    fn pressing_and_releasing_shift_mid_drag_toggles_without_residue() {
        for dpi in DPIS {
            let c = cell(dpi);
            let start = Rect::new(100, 100, 100 + c.body_size_for(2, 2).0, 100 + c.body_size_for(2, 2).1);
            let neighbour = [Rect::new(start.right + 40, 80, start.right + 300, 400)];
            let mut i = base_input(EdgeZone::RIGHT | EdgeZone::BOTTOM, dpi, start, 77, 63, &neighbour);
            i.settings = SnapSettings::new(true, 8, true, true, true);
            let snapped = resize_rect(&i).rect;
            i.shift = true;
            let raw = resize_rect(&i).rect;
            assert_eq!(raw, Rect::new(start.left, start.top, start.right + 77, start.bottom + 63));
            i.shift = false;
            assert_eq!(resize_rect(&i).rect, snapped, "dpi {dpi}: back to the first snapped value");
        }
    }

    // ---- edge snapping while resizing (spec 3.5) ----

    #[test]
    fn resizing_snaps_the_grabbed_edge_to_neighbours_and_the_work_area() {
        let dpi = 96;
        let start = Rect::new(100, 100, 400, 300);
        let n = [Rect::new(520, 100, 800, 400)];
        let mut i = base_input(EdgeZone::RIGHT, dpi, start, 0, 0, &n);
        i.settings = SnapSettings::new(true, 8, true, true, false);
        i.pointer = (500 + 113, 500); // raw right = 513, neighbour.left - gap = 512
        assert_eq!(resize_rect(&i).rect.right, 512);
        i.pointer = (500 + 100, 500); // raw right 500: 12 away from 512 -> beyond capture
        assert_eq!(resize_rect(&i).rect.right, 500);
        // alignment with the neighbour's right edge needs SnapLineUpCorrals
        i.pointer = (500 + 395, 500); // raw right 795, aligns to 800
        assert_eq!(resize_rect(&i).rect.right, 800);
        i.settings = SnapSettings::new(true, 8, true, false, false);
        assert_eq!(resize_rect(&i).rect.right, 795);
        // work area edge, flush
        i.settings = SnapSettings::new(true, 8, true, false, false);
        i.pointer = (500 + 1516, 500); // raw right 1916, work right 1920
        assert_eq!(resize_rect(&i).rect.right, 1920);
        // top edge: the outer edge (strip included) is what aligns
        let mut t = base_input(EdgeZone::TOP, dpi, Rect::new(100, 200, 400, 400), 0, 0, &[]);
        t.settings = SnapSettings::new(true, 0, true, false, false);
        t.pointer = (500, 500 - 170); // raw body top 30, outer top 6 -> snaps to work top 0 (body top 24)
        assert_eq!(resize_rect(&t).rect.top, 24);
        // whole cells win over alignment
        let mut w = base_input(EdgeZone::RIGHT, dpi, start, 0, 0, &n);
        w.settings = SnapSettings::new(true, 8, true, true, true);
        let c = cell(dpi);
        let r = resize_rect(&w).rect;
        assert_eq!((r.width() - 2 * c.pad_px) % c.cell_w_px, 0);
    }

    // ---- spec 9.9 move snapping ----

    fn mv<'a>(raw: Rect, neighbours: &'a [Rect], settings: SnapSettings, dpi: u32, lock: SnapLock) -> MoveOutput {
        snap_move(&MoveInput { raw, work: Rect::new(0, 0, px(1920.0, dpi), px(1080.0, dpi)), neighbours, settings, dpi, shift: false, lock })
    }

    #[test]
    fn each_target_kind_alone_snaps_just_inside_8_dip_and_not_just_outside() {
        for dpi in DPIS {
            let inside = (7.9 * f64::from(dpi) / 96.0).floor() as i32;
            let outside = (8.1 * f64::from(dpi) / 96.0).ceil() as i32;
            let (w, h) = (px(200.0, dpi), px(120.0, dpi));
            let n = Rect::new(px(600.0, dpi), px(300.0, dpi), px(900.0, dpi), px(500.0, dpi));
            let only = |align: bool| SnapSettings::new(true, 0, true, align, false);
            // work edge: left edge near 0
            let left = |x: i32| mv(Rect::new(x, 700, x + w, 700 + h), &[], only(false), dpi, SnapLock::default()).rect.left;
            assert_eq!(left(inside), 0, "dpi {dpi}");
            assert_eq!(left(outside), outside);
            // adjacency: my right edge next to the neighbour's left
            let y0 = n.top + 10;
            let right = |x: i32| mv(Rect::new(x, y0, x + w, y0 + h), &[n], only(false), dpi, SnapLock::default()).rect;
            let flush = n.left - w;
            assert_eq!(right(flush - inside).left, flush, "dpi {dpi}");
            assert_eq!(right(flush - outside).left, flush - outside);
            // adjacency the other way round
            let after = |x: i32| mv(Rect::new(x, y0, x + w, y0 + h), &[n], only(false), dpi, SnapLock::default()).rect.left;
            assert_eq!(after(n.right + inside), n.right);
            assert_eq!(after(n.right + outside), n.right + outside);
            // alignment: left with left, any neighbour, even far above
            let far = Rect::new(px(100.0, dpi), 5, px(300.0, dpi), px(100.0, dpi));
            let aligned = |x: i32, align: bool| mv(Rect::new(x, 700, x + w, 700 + h), &[far], only(align), dpi, SnapLock::default()).rect.left;
            assert_eq!(aligned(far.left + inside, true), far.left);
            assert_eq!(aligned(far.left + outside, true), far.left + outside);
            assert_eq!(aligned(far.left + inside, false), far.left + inside, "SnapLineUpCorrals off removes alignment only");
            // the far neighbour cannot grab an adjacency (no overlap on the other axis)
            let adjacent_far = |x: i32| mv(Rect::new(x, 700, x + w, 700 + h), &[far], only(false), dpi, SnapLock::default()).rect.left;
            assert_eq!(adjacent_far(far.right + inside), far.right + inside);
        }
    }

    #[test]
    fn a_snapped_axis_holds_until_12_dip_and_then_releases() {
        for dpi in DPIS {
            let (w, h) = (px(200.0, dpi), px(120.0, dpi));
            let s = SnapSettings::new(true, 0, true, false, false);
            let release = release_px(dpi);
            let first = mv(Rect::new(3, 700, 3 + w, 700 + h), &[], s, dpi, SnapLock::default());
            assert_eq!(first.rect.left, 0);
            let lock = first.lock;
            assert!(lock.x.is_some());
            // drifting away, still held up to 12 DIP
            let held = mv(Rect::new(release, 700, release + w, 700 + h), &[], s, dpi, lock);
            assert_eq!(held.rect.left, 0, "dpi {dpi}");
            let free = mv(Rect::new(release + 1, 700, release + 1 + w, 700 + h), &[], s, dpi, lock);
            assert_eq!(free.rect.left, release + 1);
            assert!(free.lock.x.is_none());
            // without the lock the same position does not snap at all (sticky, not just wider)
            let fresh = mv(Rect::new(release, 700, release + w, 700 + h), &[], s, dpi, SnapLock::default());
            assert_eq!(fresh.rect.left, release);
        }
    }

    #[test]
    fn ties_go_to_work_edge_then_adjacency_then_alignment() {
        let dpi = 96;
        let w = 100;
        // a neighbour whose right edge + gap 0 sits exactly on the work edge: work wins the tie
        let n = Rect::new(-50, 300, 0, 420);
        let out = mv(Rect::new(4, 320, 4 + w, 440), &[n], SnapSettings::new(true, 0, true, true, false), dpi, SnapLock::default());
        assert_eq!(out.rect.left, 0);
        // aligned vs adjacent at equal distance: adjacency first
        let a = Rect::new(500, 300, 640, 420);
        let out = mv(Rect::new(644, 320, 644 + w, 440), &[a], SnapSettings::new(true, 0, true, true, false), dpi, SnapLock::default());
        assert_eq!(out.rect.left, 640);
        // the same position offered as adjacency (a.right) and alignment (c.left): adjacency wins, so no guide
        let c = Rect::new(640, 600, 800, 700);
        let out = mv(Rect::new(643, 320, 643 + w, 440), &[a, c], SnapSettings::new(true, 0, true, true, false), dpi, SnapLock::default());
        assert_eq!(out.rect.left, 640);
        assert!(out.guides.is_empty(), "{:?}", out.guides);
        // the nearer target wins whatever its kind
        let b = Rect::new(500, 600, 700, 700);
        let out = mv(Rect::new(503, 500, 503 + w, 560), &[a, b], SnapSettings::new(true, 0, true, true, false), dpi, SnapLock::default());
        assert_eq!(out.rect.left, 500, "alignment, 3 away");
    }

    #[test]
    fn the_gap_is_kept_between_outer_rects() {
        for dpi in DPIS {
            let n = Rect::new(px(600.0, dpi), px(300.0, dpi), px(900.0, dpi), px(500.0, dpi));
            let w = px(200.0, dpi);
            let y0 = n.top + 10;
            let raw = Rect::new(n.left - w - px(8.0, dpi) - 2, y0, n.left - px(8.0, dpi) - 2, y0 + px(100.0, dpi));
            let scaled = mv(raw, &[n], SnapSettings::new(true, 8, true, false, false), dpi, SnapLock::default()).rect;
            assert_eq!(scaled.right, n.left - px(8.0, dpi), "dpi {dpi}");
            let literal = mv(raw.offset(px(8.0, dpi) - 8, 0), &[n], SnapSettings::new(true, 8, false, false, false), dpi, SnapLock::default()).rect;
            assert_eq!(literal.right, n.left - 8);
            let none = mv(raw.offset(px(8.0, dpi) + 2, 0), &[n], SnapSettings::new(true, 0, true, false, false), dpi, SnapLock::default()).rect;
            assert_eq!(none.right, n.left, "gap toggle off: flush");
            // SnapEnabled off removes everything, Shift too
            let off = mv(raw, &[n], SnapSettings::new(false, 8, true, true, true), dpi, SnapLock::default());
            assert_eq!(off.rect, raw);
            let shift = snap_move(&MoveInput { raw, work: Rect::new(0, 0, 4000, 4000), neighbours: &[n], settings: SnapSettings::new(true, 8, true, true, false), dpi, shift: true, lock: SnapLock::default() });
            assert_eq!(shift.rect, raw);
            assert_eq!(shift.lock, SnapLock::default());
        }
    }

    #[test]
    fn aligned_edges_report_guide_lines() {
        let dpi = 96;
        let n = Rect::new(500, 100, 700, 200);
        let out = mv(Rect::new(503, 400, 603, 500), &[n], SnapSettings::new(true, 0, true, true, false), dpi, SnapLock::default());
        assert_eq!(out.rect.left, 500);
        assert_eq!(out.guides, vec![Guide { vertical: true, at: 500, from: 100, to: 500 }]);
        let out = mv(Rect::new(503, 400, 603, 500), &[n], SnapSettings::new(true, 0, true, false, false), dpi, SnapLock::default());
        assert!(out.guides.is_empty());
    }

    // ---- spec 9.10 neighbours ----

    #[test]
    fn only_visible_corrals_of_the_same_monitor_and_range_are_neighbours() {
        let r = |n: i32| Rect::new(n, n, n + 10, n + 10);
        let info = |id, tab_set, monitor, range, visible, n| NeighbourInfo { id, tab_set, monitor, range, visible, outer: r(n) };
        let me = info(1, 7, 0, 2, true, 0);
        let all = [
            me,
            info(2, 0, 1, 2, true, 10),   // other monitor
            info(3, 0, 0, 3, true, 20),   // other Range
            info(4, 0, 0, 2, false, 30),  // hidden (Clear view / inactive tab)
            info(5, 7, 0, 2, true, 40),   // tab sibling
            info(6, 0, 0, 2, true, 50),   // usable
            info(7, 8, 0, 2, true, 60),   // another tab group: usable (folded or faded count too)
        ];
        assert_eq!(select_neighbours(&me, &all), vec![r(50), r(60)]);
        // a perfectly aligned Corral on another Range does not snap
        let other_range = [r(100)];
        let kept = select_neighbours(&me, &[info(9, 0, 0, 5, true, 100)]);
        assert!(kept.is_empty());
        let out = mv(Rect::new(103, 400, 203, 500), &kept, SnapSettings::new(true, 0, true, true, false), 96, SnapLock::default());
        assert_eq!(out.rect.left, 103);
        let out = mv(Rect::new(103, 400, 203, 500), &other_range, SnapSettings::new(true, 0, true, true, false), 96, SnapLock::default());
        assert_eq!(out.rect.left, 100, "the same Corral on the same Range does snap");
    }

    // ---- spec 9.11 negative origins ----

    #[test]
    fn results_do_not_depend_on_the_origin() {
        let dpi = 144;
        let work = Rect::new(0, 0, 2880, 1560);
        let n = [Rect::new(400, 200, 700, 500), Rect::new(1000, 900, 1300, 1100)];
        let s = SnapSettings::new(true, 8, true, true, false);
        let reference = snap_move(&MoveInput { raw: Rect::new(712, 207, 912, 327), work, neighbours: &n, settings: s, dpi, shift: false, lock: SnapLock::default() });
        let ri = {
            let mut i = base_input(EdgeZone::RIGHT | EdgeZone::BOTTOM, dpi, Rect::new(100, 100, 400, 300), 108, 77, &n);
            i.work = work;
            i.settings = s;
            resize_rect(&i)
        };
        for o in ORIGINS {
            let nn: Vec<Rect> = n.iter().map(|r| shifted(*r, o)).collect();
            let m = snap_move(&MoveInput { raw: shifted(Rect::new(712, 207, 912, 327), o), work: shifted(work, o), neighbours: &nn, settings: s, dpi, shift: false, lock: SnapLock::default() });
            assert_eq!(shifted(m.rect, (-o.0, -o.1)), reference.rect, "origin {o:?}");
            let mut i = base_input(EdgeZone::RIGHT | EdgeZone::BOTTOM, dpi, shifted(Rect::new(100, 100, 400, 300), o), 108, 77, &nn);
            i.work = shifted(work, o);
            i.settings = s;
            let r = resize_rect(&i);
            assert_eq!(shifted(r.rect, (-o.0, -o.1)), ri.rect, "origin {o:?}");
            let z = resize_zone(shifted(Rect::new(0, 0, 300, 200), o), shifted(Rect::new(0, 0, 300, 24), o), (o.0 + 3, o.1 + 3), &zctx(96));
            assert_eq!(z, EdgeZone::LEFT | EdgeZone::TOP);
        }
        // a Corral at x = -1500 snaps to a neighbour at x = -1620 + width + gap
        let w = 400;
        let gap = s.gap_px(96);
        let neighbour = Rect::new(-1620 - w, 100, -1620, 400);
        let out = snap_move(&MoveInput { raw: Rect::new(-1620 + gap + 5, 120, -1620 + gap + 5 + w, 300), work: Rect::new(-1920, 0, 0, 1040), neighbours: &[neighbour], settings: SnapSettings::new(true, 8, false, false, false), dpi: 96, shift: false, lock: SnapLock::default() });
        assert_eq!(out.rect.left, -1620 + 8);
    }

    // ---- spec 9.12 no drift ----

    #[test]
    fn ten_thousand_random_moves_that_sum_to_zero_return_the_start_rect() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move |m: i32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % (2 * m as u64 + 1)) as i32 - m
        };
        for dpi in [96, 144, 240] {
            let start = Rect::new(300, 250, 300 + px(333.0, dpi), 250 + px(177.0, dpi));
            let (w, h) = (start.width(), start.height());
            let mut at = start;
            let (mut sx, mut sy) = (0, 0);
            for _ in 0..10_000 {
                let (dx, dy) = (next(37), next(29));
                sx += dx;
                sy += dy;
                // snapping off: the position is exactly start + pointer delta
                let raw = start.offset(sx, sy);
                at = snap_move(&MoveInput { raw, work: Rect::new(-5000, -5000, 5000, 5000), neighbours: &[], settings: SnapSettings::new(false, 0, true, true, true), dpi, shift: false, lock: SnapLock::default() }).rect;
                assert_eq!((at.width(), at.height()), (w, h));
            }
            // bring the pointer back to the start
            let raw = start.offset(sx - sx, sy - sy);
            at = snap_move(&MoveInput { raw, work: Rect::new(-5000, -5000, 5000, 5000), neighbours: &[], settings: SnapSettings::new(false, 0, true, true, true), dpi, shift: false, lock: SnapLock::default() }).rect;
            assert_eq!(at, start);
            // with snapping on, width and height still never change in a move
            let n = [Rect::new(700, 100, 900, 300)];
            for _ in 0..2000 {
                sx += next(23);
                sy += next(23);
                let out = snap_move(&MoveInput { raw: start.offset(sx, sy), work: Rect::new(-5000, -5000, 5000, 5000), neighbours: &n, settings: SnapSettings::new(true, 8, true, true, false), dpi, shift: false, lock: SnapLock::default() });
                assert_eq!((out.rect.width(), out.rect.height()), (w, h));
            }
        }
    }

    #[test]
    fn a_saved_rect_survives_save_load_save_bit_for_bit() {
        // grid-snapping-spec 8 and 9.12: loading never rounds, snaps or clamps a stored rect
        let mut layout = crate::model::Layout::new();
        let odd = [Rect::new(-1503, 7, -1103, 193), Rect::new(1, 2, 3, 4), Rect::new(19, -4, 2000, 40)];
        for (i, r) in odd.iter().enumerate() {
            let id = layout.add_group(format!("g{i}"), "DISPLAY1".into(), *r, crate::model::GroupContent::Items { items: vec![] });
            layout.group_mut(id).unwrap().preferred_size_dip = Some((r.width().max(1) + 7, r.height().max(1) + 3));
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("c6-roundtrip");
        std::fs::create_dir_all(&dir).unwrap();
        let (first, second) = (dir.join("a.json"), dir.join("b.json"));
        crate::model::save(&first, &layout).unwrap();
        let loaded = crate::model::load(&first).unwrap();
        crate::model::save(&second, &loaded).unwrap();
        let again = crate::model::load(&second).unwrap();
        for (a, b) in layout.groups.iter().zip(&again.groups) {
            assert_eq!((a.rect, a.preferred_size_dip), (b.rect, b.preferred_size_dip));
        }
        assert_eq!(std::fs::read(&first).unwrap(), std::fs::read(&second).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- spec 9.13 rescue ----

    fn monitors() -> Vec<Rect> {
        // a 1920x1040 primary and a smaller monitor to its right, bottoms not aligned
        vec![Rect::new(0, 0, 1920, 1040), Rect::new(1920, 200, 3200, 1000)]
    }

    #[test]
    fn a_rect_past_any_edge_comes_back_inside_without_resizing() {
        let works = monitors();
        for r in [
            Rect::new(-300, 100, -100, 300),       // past the left edge
            Rect::new(1800, 100, 2020, 300),       // straddling two monitors
            Rect::new(100, -500, 300, -300),       // above
            Rect::new(100, 1200, 300, 1400),       // below the primary
            Rect::new(1500, 1000, 1700, 1100),     // partly under the taskbar
        ] {
            let p = place_on_screen(r, &works, None, None);
            let i = p.monitor.unwrap();
            assert!(p.rect.left >= works[i].left && p.rect.right <= works[i].right && p.rect.top >= works[i].top && p.rect.bottom <= works[i].bottom, "{r:?} -> {:?}", p.rect);
            assert_eq!((p.rect.width(), p.rect.height()), (r.width(), r.height()));
            assert_eq!(place_on_screen(p.rect, &works, None, None).rect, p.rect, "idempotent");
        }
        let inside = Rect::new(10, 10, 110, 110);
        assert_eq!(place_on_screen(inside, &works, None, None), Placed { rect: inside, monitor: Some(0) });
    }

    #[test]
    fn oversized_dead_space_and_removed_monitor_cases() {
        let works = monitors();
        // bigger than the work area on one axis: that axis is reduced to the work area
        let big = Rect::new(100, 100, 2500, 400);
        let p = place_on_screen(big, &works, Some(0), None);
        assert_eq!((p.rect.left, p.rect.right), (0, 1920));
        assert_eq!((p.rect.top, p.rect.bottom), (100, 400), "the other axis keeps its size");
        let both = place_on_screen(Rect::new(-50, -50, 3000, 2000), &works, Some(1), None);
        assert_eq!(both.rect, works[1]);
        // dead space between the monitors below the small one: nearest
        let dead = Rect::new(2000, 1050, 2200, 1200);
        let p = place_on_screen(dead, &works, None, None);
        assert_eq!(p.monitor, Some(1));
        assert!(works[1].contains(p.rect.left, p.rect.top) && p.rect.bottom <= works[1].bottom);
        // a removed monitor: the preferred index no longer exists
        let p = place_on_screen(Rect::new(5000, 100, 5200, 300), &works, Some(5), None);
        assert_eq!(p.monitor, Some(1), "falls back to the nearest");
        // the pointer decides before the overlap when there is no preferred monitor
        let p = place_on_screen(Rect::new(1700, 300, 2000, 500), &works, None, Some((1950, 400)));
        assert_eq!(p.monitor, Some(1));
        let p = place_on_screen(Rect::new(1700, 300, 2000, 500), &works, None, None);
        assert_eq!(p.monitor, Some(0), "largest overlap");
        // no monitors: unchanged, never a panic
        let r = Rect::new(1, 2, 3, 4);
        assert_eq!(place_on_screen(r, &[], None, None), Placed { rect: r, monitor: None });
    }

    #[test]
    fn rescue_is_origin_independent() {
        for o in ORIGINS {
            let works: Vec<Rect> = monitors().into_iter().map(|w| shifted(w, o)).collect();
            let p = place_on_screen(shifted(Rect::new(-300, 100, -100, 300), o), &works, None, None);
            assert_eq!(shifted(p.rect, (-o.0, -o.1)), place_on_screen(Rect::new(-300, 100, -100, 300), &monitors(), None, None).rect);
        }
    }

    // ---- spec 9.15 tooltip text ----

    #[test]
    fn tooltip_text_shows_pixels_and_the_cell_count_with_whole_cells() {
        assert_eq!(size_tip_text((640, 360), None), "640 \u{d7} 360");
        assert_eq!(size_tip_text((640, 360), Some((8, 3))), "640 \u{d7} 360 (8 \u{d7} 3 icons)");
    }
}
