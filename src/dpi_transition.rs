#![allow(dead_code)]
//! Pure transition decisions; no windows, persistence, timers or file operations.
//!
//! Identity uses the body top-left; effective DPI uses the
//! expanded effective body's centre.
//! Corral choice: Keep an authoritative physical requested rectangle
//! during interaction. Integer model DIPs are a lossy persistence projection,
//! never the source for the next rebase. DPI notifications do not resize it.
//! Profile resolution
//! transforms are handled by the saved desk setup engine.

use crate::geometry::{Monitor, Rect};
use crate::desk_setup::RuntimeHardware;

/// All coordinates are physical screen pixels, including
/// negative virtual origins. Primary is explicit; hardware is optional data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisplaySnapshot {
    pub device: String,
    pub bounds: Rect,
    pub work: Rect,
    pub dpi: u32,
    pub primary: bool,
    pub hardware: RuntimeHardware,
}

impl DisplaySnapshot {
    pub fn from_monitor(m: &Monitor, primary: bool, hardware: Option<RuntimeHardware>) -> Self {
        Self {
            device: m.name.clone(),
            bounds: m.bounds,
            work: m.work,
            dpi: m.dpi,
            primary,
            hardware: hardware.unwrap_or_default(),
        }
    }

    /// Unavailable work area falls back to full bounds for rescue.
    pub fn usable_work(&self) -> Rect {
        if self.work == Rect::default() {
            self.bounds
        } else {
            self.work
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    displays: Vec<DisplaySnapshot>,
}

impl Snapshot {
    /// Reject ambiguous names, zero DPI, invalid/oversized bounds and
    /// nonempty work areas outside their display. Empty snapshots are valid
    /// during unplug bursts; decisions then return None, without overwriting.
    pub fn new(displays: Vec<DisplaySnapshot>) -> Option<Self> {
        for (i, d) in displays.iter().enumerate() {
            if d.device.is_empty()
                || d.dpi == 0
                || !valid_rect(d.bounds, false)
                || displays[..i]
                    .iter()
                    .any(|p| p.device.eq_ignore_ascii_case(&d.device))
                || (d.work != Rect::default()
                    && (!valid_rect(d.work, false)
                        || d.work.left < d.bounds.left
                        || d.work.top < d.bounds.top
                        || d.work.right > d.bounds.right
                        || d.work.bottom > d.bounds.bottom))
            {
                return None;
            }
        }
        Some(Self { displays })
    }

    pub fn displays(&self) -> &[DisplaySnapshot] {
        &self.displays
    }

    pub fn find(&self, device: &str) -> Option<&DisplaySnapshot> {
        self.displays
            .iter()
            .find(|d| d.device.eq_ignore_ascii_case(device))
    }

    /// Corral tie policy: Containment uses half-open rectangles.
    /// Otherwise minimize squared Euclidean distance to the closed bounds.
    /// Containing displays beat outside displays at the same boundary distance;
    /// equal containment/distance prefers primary, then ASCII-folded device
    /// name. Thus enumeration order never changes a tie. Windows' exact tie
    /// policy is [unknown]; this models MonitorFromPoint NEAREST, not overlap.
    pub fn nearest(&self, point: (i32, i32)) -> Option<&DisplaySnapshot> {
        self.displays.iter().min_by_key(|d| {
            let axis = |p: i32, low: i32, high: i32| -> i128 {
                (i128::from(low) - i128::from(p)).max(0) + (i128::from(p) - i128::from(high)).max(0)
            };
            let dx = axis(point.0, d.bounds.left, d.bounds.right);
            let dy = axis(point.1, d.bounds.top, d.bounds.bottom);
            (
                !d.bounds.contains(point.0, point.1),
                dx * dx + dy * dy,
                !d.primary,
                d.device.to_ascii_lowercase(),
            )
        })
    }
}

fn valid_rect(r: Rect, allow_empty: bool) -> bool {
    let w = i64::from(r.right) - i64::from(r.left);
    let h = i64::from(r.bottom) - i64::from(r.top);
    let min = if allow_empty { 0 } else { 1 };
    (min..=i64::from(i32::MAX)).contains(&w) && (min..=i64::from(i32::MAX)).contains(&h)
}

/// Exact integer half-away-from-zero conversion with overflow checks.
fn convert(value: i64, numerator: u32, denominator: u32) -> Option<i32> {
    if numerator == 0 || denominator == 0 {
        return None;
    }
    let n = i128::from(value) * i128::from(numerator);
    let d = i128::from(denominator);
    let rounded = (n.abs() * 2 + d) / (d * 2);
    i32::try_from(if n < 0 { -rounded } else { rounded }).ok()
}

/// Same independent-edge rounding as App::px_to_model. No old/new
/// DPI ratio and no virtual-origin subtraction: input is already screen space.
pub fn physical_to_dips(r: Rect, d: &DisplaySnapshot) -> Option<Rect> {
    if !valid_rect(r, true) {
        return None;
    }
    let edge = |v: i32, origin: i32| convert(i64::from(v) - i64::from(origin), 96, d.dpi);
    let result = Rect::new(
        edge(r.left, d.work.left)?,
        edge(r.top, d.work.top)?,
        edge(r.right, d.work.left)?,
        edge(r.bottom, d.work.top)?,
    );
    valid_rect(result, true).then_some(result)
}

pub fn dips_to_physical(r: Rect, d: &DisplaySnapshot) -> Option<Rect> {
    if !valid_rect(r, true) {
        return None;
    }
    let edge = |v: i32, origin: i32| convert(i64::from(v), d.dpi, 96)?.checked_add(origin);
    let result = Rect::new(
        edge(r.left, d.work.left)?,
        edge(r.top, d.work.top)?,
        edge(r.right, d.work.left)?,
        edge(r.bottom, d.work.top)?,
    );
    valid_rect(result, true).then_some(result)
}

/// Corral choice: Durable same-DPI work-origin update: round the
/// translation once, then apply it to every edge, preserving DIP size. Unlike
/// reconstructing integer pixels from every saved edge this is reversible for
/// an A->B->A work-area round trip, including half-pixel DPI conversions. The
/// remaining sub-DIP translation error is unavoidable in integer persistence;
/// a live interaction should instead retain RequestedBody for exact pixels.
pub fn rebase_work_origin(r: Rect, old: &DisplaySnapshot, live: &DisplaySnapshot) -> Option<Rect> {
    if old.dpi != live.dpi || !valid_rect(r, true) {
        return None;
    }
    let dx = convert(
        i64::from(old.work.left) - i64::from(live.work.left),
        96,
        live.dpi,
    )?;
    let dy = convert(
        i64::from(old.work.top) - i64::from(live.work.top),
        96,
        live.dpi,
    )?;
    Some(Rect::new(
        r.left.checked_add(dx)?,
        r.top.checked_add(dy)?,
        r.right.checked_add(dx)?,
        r.bottom.checked_add(dy)?,
    ))
}

/// Authoritative runtime requested body, distinct from DPI-dependent minimum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestedBody(Rect);

impl RequestedBody {
    pub fn from_physical(rect: Rect) -> Option<Self> {
        valid_rect(rect, false).then_some(Self(rect))
    }

    /// Use once at load/drag start; retain physical state after that. Rebuilding
    /// from each projected rect_dip would introduce quantization drift.
    pub fn from_dips(rect: Rect, display: &DisplaySnapshot) -> Option<Self> {
        Self::from_physical(dips_to_physical(rect, display)?)
    }

    pub fn physical(self) -> Rect {
        self.0
    }

    pub fn offset(self, dx: i32, dy: i32) -> Option<Self> {
        Self::from_physical(Rect::new(
            self.0.left.checked_add(dx)?,
            self.0.top.checked_add(dy)?,
            self.0.right.checked_add(dx)?,
            self.0.bottom.checked_add(dy)?,
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacementDecision {
    pub monitor: String,
    /// Lossy integer DIPs relative to identity monitor's work origin/DPI.
    pub rect_dip: Rect,
    /// Exact input requested geometry, preserved even for nonrepresentable DIPs.
    pub requested: RequestedBody,
    pub centre_monitor: String,
    pub effective_dpi: u32,
}

/// Call after physical move/resize/snap. Supply the full
/// expanded effective body separately (including minimum expansion, never the
/// rolled envelope); its centre selects metrics DPI but not stored monitor.
/// On a DPI-only notification call again with unchanged requested geometry.
pub fn place(
    requested: RequestedBody,
    effective_body: Rect,
    live: &Snapshot,
) -> Option<PlacementDecision> {
    if !valid_rect(effective_body, false) {
        return None;
    }
    let r = requested.physical();
    let identity = live.nearest((r.left, r.top))?;
    let centre = (
        effective_body.left + effective_body.width() / 2,
        effective_body.top + effective_body.height() / 2,
    );
    let dpi = live.nearest(centre)?;
    Some(PlacementDecision {
        monitor: identity.device.clone(),
        rect_dip: physical_to_dips(r, identity)?,
        requested,
        centre_monitor: dpi.device.clone(),
        effective_dpi: dpi.dpi,
    })
}

/// DIP-sized placement rect: top-left from the physical rect, size either the
/// explicit DIP size or the independently rounded physical edges.
fn grid_rect(r: Rect, d: &DisplaySnapshot, size_dip: Option<(i32, i32)>) -> Option<Rect> {
    match size_dip {
        None => physical_to_dips(r, d),
        Some((sx, sy)) => {
            let tl = physical_to_dips(Rect::new(r.left, r.top, r.left, r.top), d)?;
            Some(Rect::new(tl.left, tl.top, tl.left.checked_add(sx)?, tl.top.checked_add(sy)?))
        }
    }
}

/// Corral choice (user decision 2026-10-14): Placement with the
/// GOVERNING monitor chosen by the caller (see [`govern`]). A Corral keeps its
/// GOVERNING monitor chosen by the caller (see [`govern`]). A Corral keeps its
/// monitor is the one whose DPI scales the whole rect (all four edges), not the
/// top-left monitor. `size_dip` fixes the persisted DIP size
/// (title moves); `None` rounds the physical edges independently (resize).
/// Restart decodes `rect_dip` with `dips_to_physical` at the same monitor.
pub fn place_governed(
    requested: RequestedBody,
    effective_body: Rect,
    governing: &DisplaySnapshot,
    live: &Snapshot,
    size_dip: Option<(i32, i32)>,
) -> Option<PlacementDecision> {
    if !valid_rect(effective_body, false) {
        return None;
    }
    let centre = (
        effective_body.left + effective_body.width() / 2,
        effective_body.top + effective_body.height() / 2,
    );
    Some(PlacementDecision {
        monitor: governing.device.clone(),
        rect_dip: grid_rect(requested.physical(), governing, size_dip)?,
        requested,
        centre_monitor: live.nearest(centre)?.device.clone(),
        effective_dpi: governing.dpi,
    })
}

/// Physical size of a DIP size at `dpi` (half-away rounding).
pub fn scale_size(dip: (i32, i32), dpi: u32) -> Option<(i32, i32)> {
    Some((convert(i64::from(dip.0), dpi, 96)?, convert(i64::from(dip.1), dpi, 96)?))
}

/// Corral choice: Largest uniform fit, reserving the external title.
/// If uniform fitting falls below a minimum, clamp that axis independently.
/// Work-area capacity takes precedence when even the minimum cannot fit.
pub fn fit_size(preferred: (i32, i32), d: &DisplaySnapshot, minimum_px: (i32, i32),
    title_px: i32) -> Option<(i32, i32)> {
    if preferred.0 <= 0 || preferred.1 <= 0 || d.dpi == 0 { return None; }
    let work = d.usable_work();
    let floor_dip = |px: i32| (i64::from(px.max(0)) * 96 / i64::from(d.dpi)) as i32;
    let room = (floor_dip(work.width()).max(1),
        floor_dip(work.height().saturating_sub(title_px.max(0))).max(1));
    let ceil_dip = |px: i32| ((i64::from(px.max(1)) * 96 + i64::from(d.dpi) - 1) / i64::from(d.dpi)) as i32;
    // Rational arithmetic avoids floating-point drift at fractional-DPI boundaries.
    let (num, den) = if i64::from(room.0) * i64::from(preferred.1)
        < i64::from(room.1) * i64::from(preferred.0) {
        (room.0.min(preferred.0), preferred.0)
    } else { (room.1.min(preferred.1), preferred.1) };
    let axis = |v: i32, min: i32, capacity: i32| {
        let uniform = (i64::from(v) * i64::from(num) / i64::from(den)) as i32;
        uniform.max(ceil_dip(min).min(v)).clamp(1, capacity)
    };
    Some((axis(preferred.0, minimum_px.0, room.0), axis(preferred.1, minimum_px.1, room.1)))
}

/// Rendering must not undo a fit by expanding to an impossible minimum.
pub fn fitting_minimum(d: &DisplaySnapshot, minimum: (i32, i32), title_px: i32) -> (i32, i32) {
    let work = d.usable_work();
    (minimum.0.min(work.width()).max(1),
        minimum.1.min(work.height().saturating_sub(title_px.max(0))).max(1))
}

/// Translate a fitted expanded body plus its measured title into the
/// work area after load/profile/release. An impossible oversized minimum aligns
/// at the work origin. Drag candidate probes deliberately never use this clamp.
pub fn contain_fitted(body: RequestedBody, envelope: Rect, d: &DisplaySnapshot) -> Option<RequestedBody> {
    let work = d.usable_work();
    let axis = |pos: i32, lo: i32, hi: i32, extent: i32| {
        let target = i64::from(pos).clamp(i64::from(lo), (i64::from(hi)-i64::from(extent)).max(i64::from(lo)));
        i32::try_from(target-i64::from(pos)).ok()
    };
    body.offset(axis(envelope.left,work.left,work.right,envelope.width())?,
        axis(envelope.top,work.top,work.bottom,envelope.height())?)
}

/// Fit a body at its existing top-left; callers retain the preferred
/// size separately and translate with the existing keep-on-screen/title rescue.
pub fn fit_body(body: RequestedBody, shown: (i32, i32), d: &DisplaySnapshot) -> Option<RequestedBody> {
    let r = body.physical();
    // A settled DIP-grid rect may have a one-pixel edge-rounding difference
    // from round(size*dpi). Keep it so release and restart agree exactly.
    let dip = physical_to_dips(r, d)?;
    if (dip.width(), dip.height()) == shown { return Some(body); }
    let (w, h) = scale_size(shown, d.dpi)?;
    RequestedBody::from_physical(Rect::new(r.left, r.top, r.left.checked_add(w)?, r.top.checked_add(h)?))
}

/// Candidate-specific fitted drag. Grab.px is normalized to the
/// preferred size at press; negative vertical offsets belong to the title and
/// keep their DPI scale rather than shrinking the strip.
pub fn move_layout_fitted(grab: Grab, cursor: (i32, i32), d: &DisplaySnapshot,
    shown: (i32, i32)) -> Option<RequestedBody> {
    let (gx, gy) = grab.at(d.dpi)?;
    let gx = convert(i64::from(gx), shown.0 as u32, grab.size_dip.0 as u32)?;
    let gy = if gy > 0 { convert(i64::from(gy), shown.1 as u32, grab.size_dip.1 as u32)? } else { gy };
    let (w, h) = scale_size(shown, d.dpi)?;
    let left = cursor.0.checked_sub(gx)?;
    let top = cursor.1.checked_sub(gy)?;
    RequestedBody::from_physical(Rect::new(left, top, left.checked_add(w)?, top.checked_add(h)?))
}

/// Round `r` onto the DIP grid of `d` so that a restart (which decodes the
/// persisted DIP rect) reproduces exactly the same pixels. `size_dip` as in
/// [`place_governed`].
pub fn settle_on_grid(
    r: RequestedBody,
    d: &DisplaySnapshot,
    size_dip: Option<(i32, i32)>,
) -> Option<RequestedBody> {
    RequestedBody::from_physical(dips_to_physical(grid_rect(r.physical(), d, size_dip)?, d)?)
}

/// Title-drag grab point, kept proportionally across DPI: `px` is measured from
/// the body top-left at `dpi` at the press, `size_dip` is the persisted size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grab {
    pub px: (i32, i32),
    pub dpi: u32,
    pub size_dip: (i32, i32),
}

impl Grab {
    /// The grab offset re-derived at another DPI (from the press values, never
    /// from a previous step, so crossings cannot accumulate rounding).
    pub fn at(&self, dpi: u32) -> Option<(i32, i32)> {
        Some((
            convert(i64::from(self.px.0), dpi, self.dpi)?,
            convert(i64::from(self.px.1), dpi, self.dpi)?,
        ))
    }
}

/// Physical body of a title drag when the cursor is at `cursor` and the Corral is
/// laid out at `dpi`: DIP size scaled, grab point under the cursor.
pub fn move_layout(grab: Grab, cursor: (i32, i32), dpi: u32) -> Option<RequestedBody> {
    let (w, h) = scale_size(grab.size_dip, dpi)?;
    let (gx, gy) = grab.at(dpi)?;
    let left = cursor.0.checked_sub(gx)?;
    let top = cursor.1.checked_sub(gy)?;
    RequestedBody::from_physical(Rect::new(left, top, left.checked_add(w)?, top.checked_add(h)?))
}

/// Corral choice: Which monitor's DPI governs a Corral while it is
/// dragged. `probe(m)` returns the effective body as it would be laid out at
/// `m`'s DPI (the caller keeps the grab point/anchor edge fixed). A layout is
/// CONSISTENT when its centre lies on that candidate monitor ("more than half
/// of the body is on the monitor", Windows' convention). The current monitor is
/// kept while consistent; otherwise the first consistent candidate wins (the
/// monitor under the current layout's centre first); with none the current one
/// stays. Only ever moving into a consistent layout is what prevents
/// oscillation at the seam: the flip itself changes the size and grab-derived
/// position, so the new layout must be valid by itself. The result is the
/// monitor under the consistent layout's centre (work origin of the stored rect).
pub fn govern<'a>(
    live: &'a Snapshot,
    current: &str,
    probe: impl Fn(&DisplaySnapshot) -> Option<Rect>,
) -> Option<&'a DisplaySnapshot> {
    let cur = live.find(current).or_else(|| live.displays.first())?;
    let centre_of = |r: Rect| (r.left + r.width() / 2, r.top + r.height() / 2);
    let consistent = |m: &DisplaySnapshot| -> Option<&'a DisplaySnapshot> {
        let at = live.nearest(centre_of(probe(m)?))?;
        // Equal DPI alone is insufficient: candidates can now have
        // different fitted sizes because their work areas differ.
        at.device.eq_ignore_ascii_case(&m.device).then_some(at)
    };
    if let Some(c) = consistent(cur) {
        return Some(c);
    }
    let under = probe(cur).and_then(|r| live.nearest(centre_of(r)));
    let mut others: Vec<&DisplaySnapshot> = live
        .displays
        .iter()
        .filter(|d| !std::ptr::eq(*d, cur))
        .collect();
    others.sort_by_key(|d| {
        (
            !under.is_some_and(|u| std::ptr::eq(u, *d)),
            !d.primary,
            d.device.to_ascii_lowercase(),
        )
    });
    others.into_iter().find_map(consistent).or(Some(cur))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Unchanged,
    DpiOnly,
    ResolutionOrTopology,
    WorkAreaOnly,
    DpiAndWorkArea,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub dpi: bool,
    /// Resolution, origin, device/hardware identity, count or primary change.
    pub topology: bool,
    pub work_area: bool,
}

impl Changes {
    pub fn kind(self) -> EventKind {
        match (self.topology, self.dpi, self.work_area) {
            (true, _, _) => EventKind::ResolutionOrTopology,
            (false, true, true) => EventKind::DpiAndWorkArea,
            (false, true, false) => EventKind::DpiOnly,
            (false, false, true) => EventKind::WorkAreaOnly,
            _ => EventKind::Unchanged,
        }
    }
}

/// Classify measured old/live facts, not the Windows message name or
/// profile signature. Preserve simultaneous flags; ignore enumeration order.
pub fn classify(old: &Snapshot, live: &Snapshot) -> Changes {
    let mut changes = Changes {
        topology: old.displays.len() != live.displays.len(),
        ..Changes::default()
    };
    for a in &old.displays {
        match live.find(&a.device) {
            None => changes.topology = true,
            Some(b) => {
                changes.topology |=
                    a.bounds != b.bounds || a.primary != b.primary || a.hardware != b.hardware;
                changes.dpi |= a.dpi != b.dpi;
                changes.work_area |= a.work != b.work;
            }
        }
    }
    changes
}

/// Corral rescue choice: Measured title rectangle relative to the
/// requested body's top-left, in physical pixels. Supports above/below titles
/// and caller-measured gap; no hardcoded DPI/font/title height. Keep its whole
/// height and at least visible_width pixels reachable (bounded by work size).
#[derive(Clone, Copy, Debug)]
pub struct TitleReachability {
    pub relative: Rect,
    pub visible_width: i32,
}

pub fn clamp_title(
    body: RequestedBody,
    display: &DisplaySnapshot,
    title: TitleReachability,
) -> Option<RequestedBody> {
    if !valid_rect(title.relative, false) || title.visible_width <= 0 {
        return None;
    }
    let work = display.usable_work();
    if !valid_rect(work, false) {
        return None;
    }
    let r = body.physical();
    let t = title.relative;
    let visible = title.visible_width.min(t.width()).min(work.width());
    let x_min = i64::from(work.left) + i64::from(visible) - i64::from(t.right);
    let x_max = i64::from(work.right) - i64::from(visible) - i64::from(t.left);
    let y_min = i64::from(work.top) - i64::from(t.top);
    // Oversized title: align to work top; as much height as possible is visible.
    let y_max = (i64::from(work.bottom) - i64::from(t.bottom)).max(y_min);
    let x = i64::from(r.left).clamp(x_min, x_max);
    let y = i64::from(r.top).clamp(y_min, y_max);
    RequestedBody::from_physical(Rect::new(
        i32::try_from(x).ok()?,
        i32::try_from(y).ok()?,
        i32::try_from(x + i64::from(r.width())).ok()?,
        i32::try_from(y + i64::from(r.height())).ok()?,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryKind {
    Nearest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovery {
    pub kind: RecoveryKind,
    pub mapped_monitor: String,
    pub placement: PlacementDecision,
}

/// Temporary recovery only (desk-setups-spec 4.3): select the nearest surviving
/// saved monitor by saved centre distance and arrangement rank. Keep the DIP
/// offset and size; the existing title clamp keeps the recovered title reachable.
/// Saved placements are untouched because this function accepts snapshots only.
pub fn recover_removed_monitor(
    device: &str,
    requested: RequestedBody,
    old: &Snapshot,
    live: &Snapshot,
    title: TitleReachability,
) -> Option<Recovery> {
    if live.find(device).is_some() {
        return None;
    }
    let source = old.find(device);
    let centre = |r: Rect| (i128::from(r.left) + i128::from(r.right),
                           i128::from(r.top) + i128::from(r.bottom));
    let source_centre = centre(source.map_or(requested.physical(), |d| d.bounds));
    let distance = |r: Rect| {
        let (x, y) = centre(r);
        (x - source_centre.0).pow(2) + (y - source_centre.1).pow(2)
    };
    // Runtime device names join two snapshots of the same display-change event;
    // they do not participate in persisted setup matching (spec 2.2).
    let target = old.displays.iter()
        .filter_map(|saved| live.find(&saved.device).map(|current| (saved, current)))
        .min_by_key(|(saved, _)| (distance(saved.bounds), saved.bounds.left, saved.bounds.top))
        .map(|(_, current)| current)
        // No surviving saved monitor: new monitors use their current arrangement.
        // This fallback is Corral's conservative choice for an incomplete snapshot.
        .or_else(|| live.displays.iter().min_by_key(|d|
            (distance(d.bounds), d.bounds.left, d.bounds.top)))?;
    let recovered = if let Some(source) = source {
        RequestedBody::from_dips(physical_to_dips(requested.physical(), source)?, target)?
    } else {
        requested
    };
    let recovered = clamp_title(recovered, target, title)?;
    Some(Recovery {
        kind: RecoveryKind::Nearest,
        mapped_monitor: target.device.clone(),
        placement: place_governed(recovered, recovered.physical(), target, live, None)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(device: &str, bounds: Rect, dpi: u32, primary: bool) -> DisplaySnapshot {
        DisplaySnapshot {
            device: device.into(),
            bounds,
            work: bounds,
            dpi,
            primary,
            hardware: RuntimeHardware::default(),
        }
    }
    fn side_by_side() -> Snapshot {
        Snapshot::new(vec![
            display("A", Rect::new(0, 0, 1920, 1080), 96, true),
            display("B", Rect::new(1920, 0, 5760, 2160), 240, false),
        ])
        .unwrap()
    }
    fn body(r: Rect) -> RequestedBody {
        RequestedBody::from_physical(r).unwrap()
    }
    fn title() -> TitleReachability {
        TitleReachability {
            relative: Rect::new(0, -30, 300, -4),
            visible_width: 64,
        }
    }

    #[test]
    fn negative_virtual_origin_and_work_origin_are_not_confused() {
        let mut d = display("left", Rect::new(-3840, -2160, 0, 0), 240, false);
        d.work = Rect::new(-3780, -2100, 0, 0);
        let r = Rect::new(24, 40, 224, 140);
        let physical = dips_to_physical(r, &d).unwrap();
        assert_eq!(physical, Rect::new(-3720, -2000, -3220, -1750));
        assert_eq!(physical_to_dips(physical, &d), Some(r));
        let snapshot = Snapshot::new(vec![
            d,
            display("primary", Rect::new(0, 0, 1920, 1080), 96, true),
        ])
        .unwrap();
        let p = place(body(physical), physical, &snapshot).unwrap();
        assert_eq!(
            (p.monitor.as_str(), p.effective_dpi, p.rect_dip),
            ("left", 240, r)
        );
    }

    #[test]
    fn side_by_side_crossing_preserves_requested_physical_extent() {
        let live = side_by_side();
        let start =
            RequestedBody::from_dips(Rect::new(100, 100, 600, 400), &live.displays[0]).unwrap();
        let moved = start.offset(2000, 0).unwrap();
        let p = place(moved, moved.physical(), &live).unwrap();
        assert_eq!(
            (
                p.monitor.as_str(),
                p.centre_monitor.as_str(),
                p.effective_dpi
            ),
            ("B", "B", 240)
        );
        assert_eq!(p.requested.physical(), Rect::new(2100, 100, 2600, 400));
        assert_eq!(p.rect_dip, Rect::new(72, 40, 272, 160));
        assert_eq!(
            dips_to_physical(p.rect_dip, &live.displays[1]),
            Some(moved.physical())
        );
    }

    #[test]
    fn stacked_crossing_uses_body_points_not_horizontal_overlap() {
        let live = Snapshot::new(vec![
            display("A", Rect::new(0, 0, 1920, 1080), 96, true),
            display("B", Rect::new(0, -2160, 3840, 0), 240, false),
        ])
        .unwrap();
        let r = Rect::new(100, -2000, 600, -1700);
        let p = place(body(r), r, &live).unwrap();
        assert_eq!(
            (p.monitor.as_str(), p.effective_dpi, p.rect_dip),
            ("B", 240, Rect::new(40, 64, 240, 184))
        );
        let spanning = Rect::new(100, -100, 600, 400);
        let p = place(body(spanning), spanning, &live).unwrap();
        assert_eq!(
            (
                p.monitor.as_str(),
                p.centre_monitor.as_str(),
                p.effective_dpi
            ),
            ("B", "A", 96)
        );
    }

    #[test]
    fn wide_straddling_body_identity_and_centre_dpi_are_independent() {
        let live = side_by_side();
        let r = Rect::new(1700, 100, 2700, 500);
        let p = place(body(r), r, &live).unwrap();
        assert_eq!(
            (
                p.monitor.as_str(),
                p.centre_monitor.as_str(),
                p.effective_dpi
            ),
            ("A", "B", 240)
        );
        assert_eq!(p.rect_dip, r); // identity-monitor conversion, never centre DPI
    }

    #[test]
    fn effective_minimum_can_cross_without_resizing_requested_body() {
        let live = side_by_side();
        let requested = body(Rect::new(1800, 100, 1850, 150));
        let p = place(requested, Rect::new(1800, 100, 2300, 700), &live).unwrap();
        assert_eq!(
            (
                p.monitor.as_str(),
                p.centre_monitor.as_str(),
                p.effective_dpi
            ),
            ("A", "B", 240)
        );
        assert_eq!(p.requested, requested);
        assert_eq!(p.rect_dip, requested.physical());
    }

    #[test]
    fn right_resize_changes_only_centre_but_left_resize_can_change_identity() {
        let live = side_by_side();
        let right = Rect::new(1800, 100, 2400, 500);
        let p = place(body(right), right, &live).unwrap();
        assert_eq!((p.monitor.as_str(), p.effective_dpi), ("A", 240));
        let left = Rect::new(1950, 100, 2400, 500);
        let p = place(body(left), left, &live).unwrap();
        assert_eq!((p.monitor.as_str(), p.effective_dpi), ("B", 240));
    }

    #[test]
    fn nearest_uses_distance_and_containment_with_stable_primary_name_ties() {
        let a = display("Z", Rect::new(-100, 0, 0, 100), 96, true);
        let b = display("A", Rect::new(100, 0, 200, 100), 240, false);
        let snapshot = Snapshot::new(vec![b.clone(), a.clone()]).unwrap();
        assert_eq!(snapshot.nearest((50, 50)).unwrap().device, "Z");
        assert_eq!(snapshot.nearest((60, 50)).unwrap().device, "A");
        let mut a = a;
        a.primary = false;
        let one = Snapshot::new(vec![a.clone(), b.clone()]).unwrap();
        let two = Snapshot::new(vec![b, a]).unwrap();
        assert_eq!(one.nearest((50, 50)).unwrap().device, "A");
        assert_eq!(two.nearest((50, 50)).unwrap().device, "A");
        // At an adjoining edge the right display contains the point; the left
        // is equally close to its closed right edge, but does not contain it.
        let sides = side_by_side();
        assert_eq!(sides.nearest((1920, 20)).unwrap().device, "B");
    }

    #[test]
    fn nearest_extreme_points_do_not_overflow_squared_distance() {
        let live = side_by_side();
        assert_eq!(live.nearest((i32::MIN, i32::MIN)).unwrap().device, "A");
        assert_eq!(live.nearest((i32::MAX, i32::MAX)).unwrap().device, "B");
    }

    #[test]
    fn classify_dpi_resolution_both_and_work_area_only() {
        let old = side_by_side();
        for (dpi, resolution, work, expected) in [
            (false, false, false, EventKind::Unchanged),
            (true, false, false, EventKind::DpiOnly),
            (false, true, false, EventKind::ResolutionOrTopology),
            (true, true, false, EventKind::ResolutionOrTopology),
            (false, false, true, EventKind::WorkAreaOnly),
            (true, false, true, EventKind::DpiAndWorkArea),
        ] {
            let mut ds = old.displays.clone();
            if dpi {
                ds[0].dpi = 240;
            }
            if resolution {
                ds[0].bounds.right += 100;
            }
            if work {
                ds[0].work.bottom -= 40;
            }
            let changes = classify(&old, &Snapshot::new(ds).unwrap());
            assert_eq!(
                changes,
                Changes {
                    dpi,
                    topology: resolution,
                    work_area: work
                }
            );
            assert_eq!(changes.kind(), expected);
        }
    }

    #[test]
    fn classify_detects_same_signature_origins_primary_hardware_and_device() {
        let old = side_by_side();
        for field in 0..4 {
            let mut ds = old.displays.clone();
            match field {
                0 => {
                    ds[1].bounds = ds[1].bounds.offset(0, 100);
                    ds[1].work = ds[1].bounds;
                }
                1 => ds[1].primary = true,
                2 => ds[1].hardware.serial = "replacement".into(),
                _ => ds[1].device = "C".into(),
            }
            assert_eq!(
                classify(&old, &Snapshot::new(ds).unwrap()).kind(),
                EventKind::ResolutionOrTopology
            );
        }
        let reordered = Snapshot::new(old.displays.iter().rev().cloned().collect()).unwrap();
        assert_eq!(classify(&old, &reordered), Changes::default());
        let mut case = old.displays.clone();
        case[0].device = "a".into();
        assert_eq!(
            classify(&old, &Snapshot::new(case).unwrap()),
            Changes::default()
        );
    }

    #[test]
    fn dpi_notification_preserves_pixels_without_old_new_ratio() {
        let old = Snapshot::new(vec![display("A", Rect::new(0, 0, 3840, 2160), 96, true)]).unwrap();
        let live =
            Snapshot::new(vec![display("A", Rect::new(0, 0, 3840, 2160), 240, true)]).unwrap();
        let r = body(Rect::new(100, 200, 601, 501));
        assert_eq!(classify(&old, &live).kind(), EventKind::DpiOnly);
        let p = place(r, r.physical(), &live).unwrap();
        assert_eq!(p.requested, r);
        assert_eq!(p.rect_dip, Rect::new(40, 80, 240, 200));
        assert_eq!(p.effective_dpi, 240);
    }

    #[test]
    fn repeated_mixed_dpi_round_trips_retain_nonrepresentable_physical_pixels() {
        let live = side_by_side();
        let original = body(Rect::new(101, 103, 602, 406)); // odd physical extent
        let mut current = original;
        for _ in 0..1000 {
            current = current.offset(2000, 0).unwrap();
            let p = place(current, current.physical(), &live).unwrap();
            assert_eq!(p.monitor, "B");
            assert_eq!(p.requested.physical().width(), 501);
            // Explicitly demonstrate integer DIP projection is lossy. Never
            // feed these rounded pixels back into the next move calculation.
            assert_ne!(
                dips_to_physical(p.rect_dip, &live.displays[1]).unwrap(),
                current.physical()
            );
            current = p.requested.offset(-2000, 0).unwrap();
            let p = place(current, current.physical(), &live).unwrap();
            assert_eq!(p.requested, original);
            assert_eq!(p.rect_dip, original.physical());
        }
    }

    #[test]
    fn work_origin_rounding_is_reversible_and_does_not_resize_persistence() {
        let old = display("A", Rect::new(0, 0, 3840, 2160), 240, true);
        for offset in [1, 4, 40, 61] {
            let mut live = old.clone();
            live.work.left = offset;
            live.work.top = offset;
            let original = Rect::new(1, 3, 202, 104);
            let mut rect = original;
            for _ in 0..1000 {
                rect = rebase_work_origin(rect, &old, &live).unwrap();
                assert_eq!((rect.width(), rect.height()), (201, 101));
                rect = rebase_work_origin(rect, &live, &old).unwrap();
                assert_eq!(rect, original);
            }
        }
    }

    #[test]
    fn recovery_uses_saved_centres_even_after_current_arrangement_changes() {
        let old = Snapshot::new(vec![
            display("home", Rect::new(1000, 0, 2000, 1000), 96, false),
            display("near", Rect::new(2000, 0, 3000, 1000), 96, false),
            display("far", Rect::new(-2000, 0, -1000, 1000), 96, true),
        ]).unwrap();
        let live = Snapshot::new(vec![
            display("near", Rect::new(-5000, 0, -4000, 1000), 96, false),
            display("far", Rect::new(1000, 0, 2000, 1000), 96, true),
        ]).unwrap();
        let r = body(Rect::new(1100, 100, 1400, 300));
        let recovered = recover_removed_monitor("home", r, &old, &live, title()).unwrap();
        assert_eq!(recovered.mapped_monitor, "near");
        assert_eq!(recovered.placement.rect_dip, Rect::new(100, 100, 400, 300));
        assert_eq!(old.find("home").unwrap().bounds, Rect::new(1000, 0, 2000, 1000));
    }

    #[test]
    fn recovery_ties_use_saved_arrangement_rank_instead_of_primary_or_list_order() {
        let home = display("home", Rect::new(0, 0, 1000, 1000), 96, false);
        let right = display("right", Rect::new(1000, 0, 2000, 1000), 96, true);
        let left = display("left", Rect::new(-1000, 0, 0, 1000), 96, false);
        for saved in [vec![home.clone(), right.clone(), left.clone()],
                      vec![left.clone(), home.clone(), right.clone()]] {
            let old = Snapshot::new(saved).unwrap();
            let live = Snapshot::new(vec![right.clone(), left.clone()]).unwrap();
            let recovered = recover_removed_monitor("home", body(Rect::new(80, 80, 380, 280)),
                &old, &live, title()).unwrap();
            assert_eq!(recovered.mapped_monitor, "left");
        }
    }

    #[test]
    fn recovery_preserves_dip_offset_and_size_across_dpi_and_resolution_changes() {
        let home = display("home", Rect::new(-4000, -500, 0, 2500), 192, false);
        let old = Snapshot::new(vec![home.clone(),
            display("present", Rect::new(0, 0, 2000, 1500), 96, true)]).unwrap();
        for (bounds, dpi) in [(Rect::new(20, 50, 3020, 2050), 144),
                              (Rect::new(-2000, -1000, 0, 500), 96)] {
            let live = Snapshot::new(vec![display("present", bounds, dpi, true)]).unwrap();
            let dip = Rect::new(100, 100, 400, 300);
            let r = RequestedBody::from_dips(dip, &home).unwrap();
            let recovered = recover_removed_monitor("home", r, &old, &live, title()).unwrap();
            assert_eq!(recovered.placement.rect_dip, dip);
            assert_eq!((recovered.placement.requested.physical().width(),
                        recovered.placement.requested.physical().height()),
                       scale_size((300, 200), dpi).unwrap());
            assert_eq!(r, RequestedBody::from_dips(dip, &home).unwrap());
        }
    }

    #[test]
    fn recovery_does_nothing_for_a_present_home() {
        let old = side_by_side();
        assert!(recover_removed_monitor("A", body(Rect::new(10, 50, 310, 250)),
            &old, &old, title()).is_none());
    }

    #[test]
    fn title_clamp_above_below_partial_and_oversized_bodies() {
        let mut d = display("A", Rect::new(0, 0, 1000, 800), 96, true);
        d.work = Rect::new(40, 50, 1000, 760);
        let upper = clamp_title(body(Rect::new(-400, -300, 200, 200)), &d, title())
            .unwrap()
            .physical();
        assert_eq!(
            (upper.left, upper.top, upper.width(), upper.height()),
            (-196, 80, 600, 500)
        );
        let below = TitleReachability {
            relative: Rect::new(0, 500, 600, 530),
            visible_width: 64,
        };
        let lower = clamp_title(body(Rect::new(1100, 1000, 1700, 1500)), &d, below)
            .unwrap()
            .physical();
        assert_eq!((lower.left, lower.top), (936, 230));
        let huge = clamp_title(
            body(Rect::new(10, 10, 2010, 2010)),
            &d,
            TitleReachability {
                relative: Rect::new(0, 2000, 2000, 2100),
                visible_width: 100,
            },
        )
        .unwrap()
        .physical();
        assert_eq!(huge.top + 2000, 660);
        assert_eq!((huge.width(), huge.height()), (2000, 2000));
        let visible = body(Rect::new(100, 200, 700, 700));
        assert_eq!(clamp_title(visible, &d, title()), Some(visible));
        d.work = Rect::default();
        assert!(clamp_title(visible, &d, title()).is_some());
    }

    #[test]
    fn invalid_snapshots_geometry_and_overflow_fail_without_fabricated_defaults() {
        let d = side_by_side().displays[0].clone();
        for field in 0..5 {
            let mut bad = d.clone();
            match field {
                0 => bad.dpi = 0,
                1 => bad.device.clear(),
                2 => bad.bounds.right = bad.bounds.left,
                3 => bad.work.right += 1,
                _ => bad.bounds = Rect::new(i32::MIN, 0, i32::MAX, 1),
            }
            assert!(Snapshot::new(vec![bad]).is_none());
        }
        let mut duplicate = d.clone();
        duplicate.device = "a".into();
        assert!(Snapshot::new(vec![d.clone(), duplicate]).is_none());
        assert!(RequestedBody::from_physical(Rect::new(0, 0, 0, 100)).is_none());
        assert!(body(Rect::new(0, 0, 10, 10)).offset(i32::MAX, 0).is_none());
        assert!(
            dips_to_physical(Rect::new(0, 0, i32::MAX, 1), &side_by_side().displays[1]).is_none()
        );
        let empty = Snapshot::new(vec![]).unwrap();
        assert!(
            place(
                body(Rect::new(0, 0, 100, 100)),
                Rect::new(0, 0, 100, 100),
                &empty
            )
            .is_none()
        );
        assert!(
            recover_removed_monitor(
                "A",
                body(Rect::new(0, 0, 100, 100)),
                &side_by_side(),
                &empty,
                title()
            )
            .is_none()
        );
        assert!(
            clamp_title(
                body(Rect::new(0, 0, 100, 100)),
                &d,
                TitleReachability {
                    relative: Rect::default(),
                    visible_width: 1
                }
            )
            .is_none()
        );
    }

    // ---- DIP-size moves across DPIs (user decision 2026-10-14) -----------------------------

    /// The user's arrangement: Monitor A (240 dpi) left of the primary Monitor B (96 dpi); optionally mirrored.
    fn user_displays(mirrored: bool) -> Snapshot {
        if mirrored {
            Snapshot::new(vec![
                display("Monitor B", Rect::new(-2560, 0, 0, 1440), 96, true),
                display("Monitor A", Rect::new(0, 0, 3840, 2160), 240, false),
            ])
            .unwrap()
        } else {
            Snapshot::new(vec![
                display("Monitor B", Rect::new(0, 0, 2560, 1440), 96, true),
                display("Monitor A", Rect::new(-3840, 0, 0, 2160), 240, false),
            ])
            .unwrap()
        }
    }

    fn min_at(dpi: u32) -> (i32, i32) {
        crate::snap::min_body_px(dpi)
    }

    /// One step of the App's title drag: sticky governing monitor, DIP size, grab proportional.
    fn drag_step(live: &Snapshot, gov: &str, grab: Grab, cursor: (i32, i32)) -> (String, RequestedBody) {
        let g = govern(live, gov, |m| {
            let b = move_layout(grab, cursor, m.dpi)?.physical();
            Some(Rect::new(
                b.left,
                b.top,
                b.left + b.width().max(min_at(m.dpi).0),
                b.top + b.height().max(min_at(m.dpi).1),
            ))
        })
        .unwrap();
        (g.device.clone(), move_layout(grab, cursor, g.dpi).unwrap())
    }

    /// Sweep the cursor along y from x0 to x1 in 1 px steps; returns the final monitor and the
    /// number of governing-monitor changes.
    fn sweep(live: &Snapshot, mut gov: String, grab: Grab, x0: i32, x1: i32, y: i32) -> (String, usize) {
        let mut flips = 0;
        let step = if x1 >= x0 { 1 } else { -1 };
        let mut x = x0;
        loop {
            let (g, body) = drag_step(live, &gov, grab, (x, y));
            let dpi = live.find(&g).unwrap().dpi;
            // size is the DIP size at the governing DPI; the grab point stays under the cursor
            assert_eq!(
                (body.physical().width(), body.physical().height()),
                scale_size(grab.size_dip, dpi).unwrap()
            );
            let (gx, gy) = grab.at(dpi).unwrap();
            assert_eq!((body.physical().left + gx, body.physical().top + gy), (x, y));
            if g != gov {
                flips += 1;
                gov = g;
            }
            if x == x1 {
                return (gov, flips);
            }
            x += step;
        }
    }

    #[test]
    fn dip_size_survives_mon_b_mon_a_mon_b_round_trips_without_drift() {
        for mirrored in [false, true] {
            let live = user_displays(mirrored);
            let (mon_b_x, mon_a_x) = if mirrored { (-1500, 2500) } else { (1500, -2500) };
            // odd DIP sizes, grab at the left/centre/right edge, in the title strip or the body
            for size_dip in [(601, 303), crate::snap::MIN_BODY_DIP, (1001, 777)] {
                for fx in [0, 50, 100] {
                    for gy in [-20, 10] {
                        let grab = Grab { px: (size_dip.0 * fx / 100, gy), dpi: 96, size_dip };
                        let mut gov = "Monitor B".to_string();
                        let mut last = None;
                        for _ in 0..10 {
                            let (g1, f1) = sweep(&live, gov, grab, mon_b_x, mon_a_x, 300);
                            assert_eq!((g1.as_str(), f1), ("Monitor A", 1), "forward {mirrored} {size_dip:?} {fx}");
                            let (g2, f2) = sweep(&live, g1, grab, mon_a_x, mon_b_x, 300);
                            assert_eq!((g2.as_str(), f2), ("Monitor B", 1), "back {mirrored} {size_dip:?} {fx}");
                            gov = g2;
                            // end state: persisted DIP size is exactly the starting size
                            let (_, body) = drag_step(&live, &gov, grab, (mon_b_x, 300));
                            let d = live.find(&gov).unwrap();
                            let p = place_governed(body, body.physical(), d, &live, Some(size_dip)).unwrap();
                            assert_eq!((p.rect_dip.width(), p.rect_dip.height()), size_dip);
                            if let Some(prev) = last {
                                assert_eq!(p.rect_dip, prev, "no drift");
                            }
                            last = Some(p.rect_dip);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn straddling_a_seam_flips_once_and_is_stable_under_jitter() {
        for mirrored in [false, true] {
            let live = user_displays(mirrored);
            let size_dip = (601, 303);
            for fx in [0, 25, 50, 75, 100] {
                let grab = Grab { px: (size_dip.0 * fx / 100, 12), dpi: 96, size_dip };
                for (from, to) in [(-3000, 3000), (3000, -3000)] {
                    // the monitor that is on the `from` side of the seam
                    let start = if (from < 0) == mirrored { "Monitor B" } else { "Monitor A" };
                    let step = if to > from { 1 } else { -1 };
                    let mut gov = start.to_string();
                    let mut x = from;
                    let mut flips = 0;
                    while x != to {
                        let (g, _) = drag_step(&live, &gov, grab, (x, 300));
                        if g != gov {
                            flips += 1;
                            // jitter back and forth around the flip: the same cursor always
                            // resolves to the same monitor (no flip-flop at the seam)
                            for j in [-3, -2, -1, 0, 1, 2, 3, 1, -1, 2, -2] {
                                let c = (x - step * j, 300);
                                let (g2, _) = drag_step(&live, &g, grab, c);
                                let (g3, _) = drag_step(&live, &g2, grab, c);
                                assert_eq!(g2, g3);
                            }
                            gov = g;
                        }
                        x += step;
                    }
                    assert_eq!(flips, 1, "mirrored {mirrored} fx {fx} from {from}");
                }
            }
        }
    }

    #[test]
    fn no_flip_flop_with_the_cursor_held_in_the_jump_zone() {
        // Monitor B->Monitor A, grab at the left edge: the 96 layout's centre is already on Monitor A while the
        // 240 layout's centre is still on Monitor B. Neither is consistent; the current one stays.
        let live = user_displays(false);
        let grab = Grab { px: (0, 10), dpi: 96, size_dip: (601, 303) };
        // cursor inside Monitor A, 700 px left of the seam: 96 centre at -700+300 = -400 (Monitor A),
        // 240 centre at -700+751 = +51 (Monitor B)
        for start in ["Monitor B", "Monitor A"] {
            let (g, _) = drag_step(&live, start, grab, (-700, 300));
            assert_eq!(g, start);
        }
    }

    #[test]
    fn single_monitor_never_changes_the_governing_monitor() {
        let live = Snapshot::new(vec![display("A", Rect::new(0, 0, 1920, 1080), 96, true)]).unwrap();
        let grab = Grab { px: (5, 5), dpi: 96, size_dip: (300, 200) };
        let (g, f) = sweep(&live, "A".into(), grab, -500, 2500, 100);
        assert_eq!((g.as_str(), f), ("A", 0));
    }

    #[test]
    fn grab_is_rederived_from_the_press_not_from_the_previous_step() {
        let grab = Grab { px: (37, -19), dpi: 96, size_dip: (601, 303) };
        for _ in 0..1000 {
            assert_eq!(grab.at(240), Some((93, -48))); // 92.5 and -47.5 round away from zero
            assert_eq!(grab.at(96), Some((37, -19)));
        }
        let at240 = Grab { px: grab.at(240).unwrap(), dpi: 240, ..grab };
        // pressing at 240 and dragging at 96 is within one pixel of the starting size
        let back = at240.at(96).unwrap();
        assert!((back.0 - 37).abs() <= 1 && (back.1 + 19).abs() <= 1);
    }

    #[test]
    fn restart_decodes_to_the_settled_pixels() {
        let live = user_displays(false);
        let mon_a = live.find("Monitor A").unwrap();
        let mon_b = live.find("Monitor B").unwrap();
        let size_dip = (601, 303);
        // a body dropped on Monitor A with an off-grid top-left (cursor pixel)
        let raw = body(Rect::new(-2001, 203, -2001 + 1503, 203 + 758));
        let settled = settle_on_grid(raw, mon_a, Some(size_dip)).unwrap();
        let p = place_governed(settled, settled.physical(), mon_a, &live, Some(size_dip)).unwrap();
        assert_eq!(p.monitor, "Monitor A");
        assert_eq!((p.rect_dip.width(), p.rect_dip.height()), size_dip);
        assert_eq!(RequestedBody::from_dips(p.rect_dip, mon_a).unwrap(), settled);
        // settling is idempotent
        assert_eq!(settle_on_grid(settled, mon_a, Some(size_dip)).unwrap(), settled);
        // the resize path (no explicit size) settles edges independently, also idempotent
        let r = settle_on_grid(raw, mon_a, None).unwrap();
        assert_eq!(settle_on_grid(r, mon_a, None).unwrap(), r);
        // the same DIP rect on the other monitor is the proportional (not the same-pixel) body
        let on_mon_b = dips_to_physical(p.rect_dip, mon_b).unwrap();
        assert_eq!((on_mon_b.width(), on_mon_b.height()), size_dip);
    }

    #[test]
    fn governed_placement_does_not_use_the_top_left_monitor() {
        let live = user_displays(false);
        // top-left on Monitor A, centre on Monitor B, governed by Monitor B: DIPs are relative to Monitor B's work
        // origin (negative left) and the size is the Monitor B-scale DIP size.
        let mon_b = live.find("Monitor B").unwrap();
        let r = body(Rect::new(-100, 200, 501, 503));
        let p = place_governed(r, r.physical(), mon_b, &live, None).unwrap();
        assert_eq!((p.monitor.as_str(), p.effective_dpi), ("Monitor B", 96));
        assert_eq!(p.rect_dip, Rect::new(-100, 200, 501, 503));
        assert_eq!(p.centre_monitor, "Monitor B");
    }
    #[test]
    fn narrow_tall_and_wide_short_fit_independently_and_restore() {
        for dpi in [96,120,144,168,192,240] {
            let d=display("portrait",Rect::new(173,-1920,1253,0),dpi,false);
            let title=crate::group_paint::metrics(dpi);
            let strip=title.title_h+title.gap;
            let min=scale_size(crate::snap::MIN_BODY_DIP,dpi).unwrap();
            for preferred in [(crate::snap::MIN_BODY_DIP.0,4000),(4000,crate::snap::MIN_BODY_DIP.1),(4000,4000)] {
                let shown=fit_size(preferred,&d,min,strip).unwrap();
                let px=scale_size(shown,dpi).unwrap();
                assert!(px.0<=d.work.width() && px.1+strip<=d.work.height(),"{dpi} {preferred:?}");
                assert!(px.0>=min.0 && px.1>=min.1);
                let roomy=display("roomy",Rect::new(0,0,16000,16000),dpi,true);
                assert_eq!(fit_size(preferred,&roomy,min,strip),Some(preferred));
            }
        }
    }

    #[test]
    fn single_monitor_common_scales_move_settle_and_title_fit() {
        for dpi in [96,120,144,168,192] {
            let d=display("single",Rect::new(37,-97,1957,983),dpi,true);
            let live=Snapshot::new(vec![d.clone()]).unwrap();
            let m=crate::group_paint::metrics(dpi);
            for preferred in [(301,203),(crate::snap::MIN_BODY_DIP.0,3000),(3000,crate::snap::MIN_BODY_DIP.1)] {
                let shown=fit_size(preferred,&d,scale_size(crate::snap::MIN_BODY_DIP,dpi).unwrap(),m.title_h+m.gap).unwrap();
                let grab=Grab{px:(37,-17),dpi:120,size_dip:preferred};
                let raw=move_layout_fitted(grab,(700,300),&d,shown).unwrap();
                assert_eq!(govern(&live,"single", |_| Some(raw.physical())).unwrap().device,"single");
                let settled=settle_on_grid(raw,&d,Some(shown)).unwrap();
                let p=place_governed(settled,settled.physical(),&d,&live,Some(shown)).unwrap();
                assert_eq!(RequestedBody::from_dips(p.rect_dip,&d),Some(settled));
                for below in [false,true] {
                    let title=crate::group_paint::title_rect_on(settled.physical(),below,&m);
                    assert_eq!(title.height(),m.title_h);
                    let envelope=crate::group_paint::union_rect(settled.physical(),title,false);
                    let contained=contain_fitted(settled,envelope,&d).unwrap();
                    let delta=(contained.physical().left-settled.physical().left,contained.physical().top-settled.physical().top);
                    let envelope=envelope.offset(delta.0,delta.1);
                    assert!(envelope.left>=d.work.left && envelope.right<=d.work.right);
                    assert!(envelope.top>=d.work.top && envelope.bottom<=d.work.bottom);
                }
            }
        }
    }

    #[test]
    fn common_pairs_all_directions_and_three_monitors_have_stable_moves() {
        for (dpi_a,dpi_b) in [(96,144),(120,168)] {
            for bounds in [Rect::new(1920,0,3840,1080),Rect::new(0,-1080,1920,0),
                Rect::new(0,1080,1920,2160),Rect::new(-1080,-300,0,1620)] {
                let live=Snapshot::new(vec![display("A",Rect::new(0,0,1920,1080),dpi_a,true),
                    display("B",bounds,dpi_b,false),display("C",Rect::new(-1600,1800,0,2700),192,false)]).unwrap();
                let preferred=(301,203);
                let grab=Grab{px:(17,-13),dpi:dpi_a,size_dip:preferred};
                let mut gov="A".to_string();
                for _ in 0..20 {
                    for name in ["B","C","A"] {
                        let target=live.find(name).unwrap();
                        let cursor=(target.bounds.left+target.bounds.width()/2,target.bounds.top+target.bounds.height()/2);
                        let probe=|d: &DisplaySnapshot| {
                            let m=crate::group_paint::metrics(d.dpi);
                            let shown=fit_size(preferred,d,scale_size(crate::snap::MIN_BODY_DIP,d.dpi)?,m.title_h+m.gap)?;
                            Some(move_layout_fitted(grab,cursor,d,shown)?.physical())
                        };
                        gov=govern(&live,&gov,probe).unwrap().device.clone();
                        assert_eq!(gov,name);
                        assert_eq!(govern(&live,&gov,probe).unwrap().device,gov);
                        let d=live.find(&gov).unwrap();
                        let raw=RequestedBody::from_physical(probe(d).unwrap()).unwrap();
                        let r=settle_on_grid(raw,d,Some(preferred)).unwrap();
                        let p=place_governed(r,r.physical(),d,&live,Some(preferred)).unwrap();
                        assert_eq!((p.rect_dip.width(),p.rect_dip.height()),preferred);
                        assert_eq!(RequestedBody::from_dips(p.rect_dip,d),Some(r));
                    }
                }
            }
        }
    }

    #[test]
    fn fractional_fit_rounding_never_exceeds_capacity() {
        for dpi in [120,144,168] {
            for width in [333,997,1080] {
                for height in [399,1001,1920] {
                    let d=display("odd",Rect::new(-width,47,0,47+height),dpi,false);
                    let m=crate::group_paint::metrics(dpi);
                    for pref in [(crate::snap::MIN_BODY_DIP.0,4001),(4001,crate::snap::MIN_BODY_DIP.1),(1777,999)] {
                        let shown=fit_size(pref,&d,scale_size(crate::snap::MIN_BODY_DIP,dpi).unwrap(),m.title_h+m.gap).unwrap();
                        let px=scale_size(shown,dpi).unwrap();
                        assert!(px.0<=width && px.1+m.title_h+m.gap<=height);
                        assert_eq!(fit_size(pref,&d,scale_size(crate::snap::MIN_BODY_DIP,dpi).unwrap(),m.title_h+m.gap),Some(shown));
                    }
                }
            }
        }
    }

}
