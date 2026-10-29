//! Fold (roll-up), hover opening and edge docking rules (motion-sorting-spec 3, grid spec 3.6).
//! Pure and unit-tested; the window glue is in `app/input.rs`.

use crate::geometry::Rect;
use crate::model::RollState;
use crate::motion::{self, Curve, Tween};

/// Fold openness: 0 = only the title strip, 1 = fully open. The value animated by [`retarget_fold`]
/// is the already eased picture, so geometry reads it directly.
pub const OPEN: f64 = 1.0;

/// A temporary open closes this long after the pointer left the outer rect (spec 3).
pub const LEAVE_GRACE_MS: f64 = 300.0;

/// Hover delay in milliseconds from the stored `RollupRequireDelayTime`, clamped to 100..2000 (spec 3).
pub fn hover_delay_ms(stored: u32) -> u32 {
    stored.clamp(100, 2000)
}

/// Start or retarget a Fold animation towards open (180 ms OUT) or folded (140 ms IN). A reversal
/// starts from the displayed value and takes the base time x the distance left (spec 3,
/// "Interrupting"); with Windows animations off it lands at once (spec 7).
pub fn retarget_fold(tween: &mut Tween, now: f64, open: bool, animate: bool) {
    let (to, base, curve) = if open { (OPEN, motion::FOLD_OPEN_MS, Curve::Out) } else { (0.0, motion::FOLD_CLOSE_MS, Curve::In) };
    tween.retarget_scaled(now, to, base, curve, OPEN, animate);
}

/// The leave rule of a temporary open (spec 3): 300 ms after the pointer left the outer rect; the
/// pointer coming back cancels it, and it never runs while `blocked` (a mouse button is down, an
/// icon or Corral drag started here is running, or a menu, tooltip or rename box is open). The
/// 300 ms start when the blocking ends.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LeaveGrace {
    since: Option<f64>,
}

impl LeaveGrace {
    pub fn reset(&mut self) {
        self.since = None;
    }

    /// True when the open Corral should close now.
    pub fn update(&mut self, now: f64, inside: bool, blocked: bool) -> bool {
        if inside || blocked {
            self.since = None;
            return false;
        }
        let since = *self.since.get_or_insert(now);
        now - since >= LEAVE_GRACE_MS
    }
}

/// A Corral that docks folds with the close animation unless it is already shut: it is closing
/// when it is not folded yet, is the hot (hover-open) Corral, or still shows some body.
pub fn close_on_dock(was_rolled: bool, hot: bool, openness: f64) -> bool {
    !was_rolled || hot || openness > 0.0
}

/// What a roll state change after a drop (`derived_roll`) needs from the animation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FoldFollow {
    /// Nothing changed.
    Keep,
    /// Newly folded: run the close animation from what is shown.
    Close,
    /// Newly unfolded while docked: run the open animation from what is shown.
    Open,
    /// Newly unfolded and not docked: the body is shown at once, forget the animation.
    Clear,
}

pub fn fold_follow(was_rolled: bool, now_rolled: bool, docked: bool) -> FoldFollow {
    match (was_rolled, now_rolled) {
        (false, true) => FoldFollow::Close,
        (true, false) if docked => FoldFollow::Open,
        (true, false) => FoldFollow::Clear,
        _ => FoldFollow::Keep,
    }
}

/// Visible body height while folding: round(height * openness), including small heights.
pub fn visible_body_height(height: i32, openness: f64) -> i32 {
    (height as f64 * openness).round() as i32
}

// ---- docking, keep-open area and the minimum open body (ranges-dock-clearview-spec 4) ----------------

/// A dropped Corral docks when its outer rect is at most this far from a work-area edge (spec 4).
pub const DOCK_IN_DIP: i32 = 8;
/// A docked Corral stays docked while its raw outer edge is within this distance (hysteresis).
pub const DOCK_OUT_DIP: i32 = 12;
/// A hover-opened Corral stays open this far around its outer rect (spec 4, "Keep-open area").
pub const KEEP_MARGIN_DIP: i32 = 12;
/// Smallest body shown when a Corral is open, however little is stored (spec 4, "Nearly zero body").
pub const MIN_OPEN_BODY_DIP: i32 = 48;

/// Whether a gap of `gap_px` pixels is at most `limit_dip` DIP at `dpi` (exact integer test, so 8.0 DIP
/// qualifies and anything above it does not).
fn within(gap_px: i32, dpi: u32, limit_dip: i32) -> bool {
    i64::from(gap_px) * 96 <= i64::from(limit_dip) * i64::from(dpi.max(1))
}

fn gap_to(edge: DockEdge, outer: Rect, work: Rect) -> i32 {
    match edge {
        DockEdge::Top => outer.top - work.top,
        DockEdge::Bottom => work.bottom - outer.bottom,
    }
}

/// The dock flag after a title drag ends. `outer` is the dropped outer rect (body plus title strip; the
/// strip alone when folded) and `work` the monitor's work area, in the same pixels at `dpi`. A docked
/// Corral that is still within [`DOCK_OUT_DIP`] keeps its edge; otherwise the smaller gap within
/// [`DOCK_IN_DIP`] docks (ties to the top), and Shift never docks anew.
pub fn dock_on_drop(outer: Rect, work: Rect, dpi: u32, shift: bool, current: Option<DockEdge>) -> Option<DockEdge> {
    if let Some(edge) = current {
        if within(gap_to(edge, outer, work), dpi, DOCK_OUT_DIP) {
            return Some(edge);
        }
    }
    if shift {
        return None;
    }
    let (top, bottom) = (gap_to(DockEdge::Top, outer, work), gap_to(DockEdge::Bottom, outer, work));
    match (within(top, dpi, DOCK_IN_DIP), within(bottom, dpi, DOCK_IN_DIP)) {
        (true, true) => Some(if top <= bottom { DockEdge::Top } else { DockEdge::Bottom }),
        (true, false) => Some(DockEdge::Top),
        (false, true) => Some(DockEdge::Bottom),
        (false, false) => None,
    }
}

/// During a title drag: the raw outer rect is still close enough for the Corral to stay docked.
pub fn stays_docked(edge: DockEdge, raw_outer: Rect, work: Rect, dpi: u32) -> bool {
    within(gap_to(edge, raw_outer, work), dpi, DOCK_OUT_DIP)
}

/// The area that keeps a hover-opened Corral open: its expanded outer rect clamped to the work area, grown
/// by [`KEEP_MARGIN_DIP`] on every side except along a docked edge, where the screen edge ends it.
pub fn keep_open_area(expanded_outer: Rect, work: Rect, docked: Option<DockEdge>, dpi: u32) -> Rect {
    let margin = crate::geometry::to_px(KEEP_MARGIN_DIP, dpi);
    let r = expanded_outer.clamp_into(&work);
    Rect::new(
        r.left - margin,
        if docked == Some(DockEdge::Top) { r.top } else { r.top - margin },
        r.right + margin,
        if docked == Some(DockEdge::Bottom) { r.bottom } else { r.bottom + margin },
    )
}

/// Whether the pointer keeps the Corral open: inside its expanded outer rect (the strip belongs to it), or
/// in the margin unless that point is inside another Corral's outer rect.
pub fn keeps_open(point: (i32, i32), expanded_outer: Rect, area: Rect, others: &[Rect]) -> bool {
    if expanded_outer.contains(point.0, point.1) {
        return true;
    }
    !others.iter().any(|o| o.contains(point.0, point.1)) && area.contains(point.0, point.1)
}

/// Height an open body is shown at: never below [`MIN_OPEN_BODY_DIP`] (display and hit-testing only; the
/// stored rect is untouched).
pub fn open_body_height(stored_px: i32, dpi: u32) -> i32 {
    stored_px.max(crate::geometry::to_px(MIN_OPEN_BODY_DIP, dpi))
}

/// The body as shown: lifted to the minimum height away from its title strip.
pub fn lifted_body(body: Rect, title_below: bool, dpi: u32) -> Rect {
    let height = open_body_height(body.height(), dpi);
    if height == body.height() {
        body
    } else if title_below {
        Rect::new(body.left, body.bottom - height, body.right, body.bottom)
    } else {
        Rect::new(body.left, body.top, body.right, body.top + height)
    }
}

/// Corral policy: Reserve the full title strip while dragging. For a move,
/// translate the body; for a resize, stop the top edge without moving the bottom anchor.
/// The side is frozen until release, preventing shape/grab changes under the pointer.
pub fn clamp_top(mut body: Rect, below: bool, title_h: i32, gap: i32,
    work_top: i32, resizing_top: bool) -> Rect {
    let reserve = if below { 0 } else { title_h.max(0) + gap.max(0) };
    let top = work_top.saturating_add(reserve);
    if body.top < top {
        if !resizing_top { body.bottom = body.bottom.saturating_add(top - body.top); }
        body.top = top;
        body.bottom = body.bottom.max(top.saturating_add(1));
    }
    body
}

/// User policy: With automatic title placement, anchor the body at
/// the work-area top and move its title below the revealed portion. Otherwise keep
/// the fixed top title. The saved body height is independent of animation/setting.
pub fn top_layout(body: Rect, work_top: i32, title_h: i32, gap: i32, visible: i32, below: bool) -> (Rect, Rect, Rect) {
    let visible = visible.clamp(0, body.height());
    if below {
        let full = Rect::new(body.left, work_top, body.right, work_top + body.height());
        let shown = Rect::new(body.left, work_top, body.right, work_top + visible);
        let top = if visible == 0 { work_top } else { shown.bottom + gap };
        return (Rect::new(body.left, top, body.right, top + title_h), shown, full);
    }
    let title = Rect::new(body.left, work_top, body.right, work_top + title_h);
    let top = title.bottom + gap;
    let full = Rect::new(body.left, top, body.right, top + body.height());
    let shown = Rect::new(body.left, top, body.right, top + visible);
    (title, shown, full)
}

/// Normal expanded body to use for a title drag. A partly revealed below-title
/// body may not fit above the grabbed strip: put the body below it for that drag,
/// retaining the grab point rather than jumping the expanded title to its bottom.
/// Normal near-top title-side hysteresis resumes on release.
pub fn top_drag_body(full: Rect, title: Rect, below: bool, gap: i32, work_top: i32) -> (Rect, bool) {
    let below = below && title.top - gap - full.height() >= work_top;
    let top = if below { title.top - gap - full.height() } else { title.bottom + gap };
    (full.offset(0, top - full.top), below)
}

/// User policy: Bottom docking is the vertical mirror of the
/// top state machine. Only top docking's fixed-title alternative is conditional
/// on the title-underneath setting; bottom docking always uses the moving strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockEdge { Top, Bottom }

impl DockEdge {
    fn rect(self, r: Rect) -> Rect {
        match self { Self::Top => r, Self::Bottom => Rect::new(r.left, -r.bottom, r.right, -r.top) }
    }
    fn side(self, below: bool) -> bool { match self { Self::Top => below, Self::Bottom => !below } }
    fn anchor(self, work: Rect) -> i32 { match self { Self::Top => work.top, Self::Bottom => -work.bottom } }
    pub fn title_below(self, auto: bool) -> bool { self == Self::Top && auto }

    pub fn layout(self, body: Rect, work: Rect, title_h: i32, gap: i32, visible: i32, auto: bool) -> (Rect, Rect, Rect) {
        let below = self.side(self.title_below(auto));
        let (title, shown, full) = top_layout(self.rect(body), self.anchor(work), title_h, gap, visible, below);
        (self.rect(title), self.rect(shown), self.rect(full))
    }
    pub fn drag_body(self, full: Rect, title: Rect, below: bool, gap: i32, work: Rect) -> (Rect, bool) {
        let (body, side) = top_drag_body(self.rect(full), self.rect(title), self.side(below), gap, self.anchor(work));
        (self.rect(body), self.side(side))
    }
    pub fn clamp(self, body: Rect, below: bool, title_h: i32, gap: i32, work: Rect, resizing_edge: bool) -> Rect {
        self.rect(clamp_top(self.rect(body), self.side(below), title_h, gap, self.anchor(work), resizing_edge))
    }
}

/// Roll state kept after a drag (spec 3, "After dragging"): a drag never changes a user fold
/// (`Manual`). The dock part of the fold is derived from the dock state, so the `Docked` marker only
/// mirrors "enabled, RollupOnDock on and docked": dragging off the edge unfolds, docking folds.
pub fn derived_roll(state: RollState, rollup_enabled: bool, on_dock: bool, docked_now: bool) -> RollState {
    if state == RollState::Manual {
        return state;
    }
    if rollup_enabled && on_dock && docked_now { RollState::Docked } else { RollState::Expanded }
}

/// Title double-click toggle: expanded -> rolled by the user; any rolled state -> expanded.
pub fn toggle(state: RollState) -> RollState {
    if state == RollState::Expanded { RollState::Manual } else { RollState::Expanded }
}

/// Layout of a rolled (possibly hover-animating) group. `title` is the strip at its normal place
/// (above or below the body), `visible` the animated body height. Body stays anchored to the title
/// side; a docked group's strip sits flush with the screen edge when fully rolled.
pub fn rolled_layout(body: Rect, title: Rect, title_above: bool, docked: bool, work: Rect, gap: i32, visible: i32) -> (Rect, Rect) {
    let h = title.height();
    if title_above {
        if docked {
            // body anchored at the bottom edge; strip rides on top of the visible part
            let body_vis = Rect::new(body.left, body.bottom - visible, body.right, body.bottom);
            let strip_bottom = if visible > 0 { body_vis.top - gap } else { work.bottom };
            (Rect::new(title.left, strip_bottom - h, title.right, strip_bottom), body_vis)
        } else {
            let body_vis = Rect::new(body.left, body.top, body.right, body.top + visible);
            (title, body_vis)
        }
    } else if docked {
        // docked at the top edge, title below: body grows down from the edge, strip below it
        let body_vis = Rect::new(body.left, work.top, body.right, work.top + visible);
        let strip_top = if visible > 0 { body_vis.bottom + gap } else { work.top };
        (Rect::new(title.left, strip_top, title.right, strip_top + h), body_vis)
    } else {
        let body_vis = Rect::new(body.left, body.bottom - visible, body.right, body.bottom);
        (title, body_vis)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work() -> Rect {
        Rect::new(0, 0, 1920, 1040)
    }

    fn outer_at(top: i32, bottom: i32) -> Rect {
        Rect::new(100, top, 500, bottom)
    }

    #[test]
    fn a_drop_docks_within_8_dip_and_not_beyond() {
        // 96 dpi: 8 px is 8.0 DIP
        assert_eq!(dock_on_drop(outer_at(8, 300), work(), 96, false, None), Some(DockEdge::Top));
        assert_eq!(dock_on_drop(outer_at(9, 300), work(), 96, false, None), None);
        assert_eq!(dock_on_drop(outer_at(700, 1032), work(), 96, false, None), Some(DockEdge::Bottom));
        assert_eq!(dock_on_drop(outer_at(700, 1031), work(), 96, false, None), None);
        // 240 dpi: 8.0 DIP is 20 px, 8.1 DIP is 21 px
        assert_eq!(dock_on_drop(outer_at(20, 300), work(), 240, false, None), Some(DockEdge::Top));
        assert_eq!(dock_on_drop(outer_at(21, 300), work(), 240, false, None), None);
        // overlapping the edge counts
        assert_eq!(dock_on_drop(outer_at(-3, 300), work(), 96, false, None), Some(DockEdge::Top));
    }

    #[test]
    fn both_edges_pick_the_smaller_gap_and_ties_go_to_the_top() {
        let tall = |top: i32, bottom: i32| dock_on_drop(Rect::new(0, top, 100, bottom), work(), 96, false, None);
        assert_eq!(tall(5, 1040 - 2), Some(DockEdge::Bottom));
        assert_eq!(tall(2, 1040 - 5), Some(DockEdge::Top));
        assert_eq!(tall(4, 1040 - 4), Some(DockEdge::Top));
    }

    #[test]
    fn shift_never_docks_anew_and_does_not_need_the_snap_setting() {
        assert_eq!(dock_on_drop(outer_at(0, 300), work(), 96, true, None), None);
        // no snapping is involved: the decision depends on the rect only
        assert_eq!(dock_on_drop(outer_at(6, 300), work(), 96, false, None), Some(DockEdge::Top));
    }

    #[test]
    fn a_docked_corral_sticks_to_12_dip_and_releases_beyond() {
        let docked = Some(DockEdge::Top);
        assert_eq!(dock_on_drop(outer_at(12, 300), work(), 96, false, docked), docked);
        assert_eq!(dock_on_drop(outer_at(13, 300), work(), 96, false, docked), None);
        // sliding along the edge keeps it
        assert_eq!(dock_on_drop(Rect::new(900, 3, 1300, 300), work(), 96, false, docked), docked);
        // Shift does not release a docked Corral that is still close, but never docks a new one
        assert_eq!(dock_on_drop(outer_at(10, 300), work(), 96, true, docked), docked);
        assert_eq!(dock_on_drop(outer_at(13, 300), work(), 96, true, docked), None);
        // 144 dpi: 12.0 DIP is 18 px
        assert_eq!(dock_on_drop(outer_at(18, 300), work(), 144, false, docked), docked);
        assert_eq!(dock_on_drop(outer_at(19, 300), work(), 144, false, docked), None);
        assert!(stays_docked(DockEdge::Top, outer_at(12, 300), work(), 96));
        assert!(!stays_docked(DockEdge::Top, outer_at(13, 300), work(), 96));
        assert!(stays_docked(DockEdge::Bottom, outer_at(700, 1028), work(), 96));
        assert!(!stays_docked(DockEdge::Bottom, outer_at(700, 1027), work(), 96));
    }

    #[test]
    fn dropping_on_the_opposite_edge_docks_there() {
        assert_eq!(dock_on_drop(outer_at(740, 1036), work(), 96, false, Some(DockEdge::Top)), Some(DockEdge::Bottom));
        assert_eq!(dock_on_drop(outer_at(4, 300), work(), 96, false, Some(DockEdge::Bottom)), Some(DockEdge::Top));
    }

    #[test]
    fn the_keep_open_margin_is_12_dip_around_the_clamped_rect() {
        let outer = Rect::new(200, 300, 600, 700);
        let area = keep_open_area(outer, work(), None, 96);
        assert_eq!(area, Rect::new(188, 288, 612, 712));
        // points in the margin keep it open, one pixel further out closes
        assert!(keeps_open((612 - 1, 500), outer, area, &[]));
        assert!(!keeps_open((612, 500), outer, area, &[]));
        assert!(keeps_open((400, 288), outer, area, &[]));
        assert!(!keeps_open((400, 287), outer, area, &[]));
        // the margin scales with the DPI: 12 DIP is 18 px at 144
        assert_eq!(keep_open_area(outer, work(), None, 144), Rect::new(182, 282, 618, 718));
        // clamped to the work area as drawn
        let big = Rect::new(-50, 200, 2000, 700);
        let area = keep_open_area(big, work(), None, 96);
        assert_eq!((area.left, area.right), (-12, 1932));
    }

    #[test]
    fn a_docked_edge_has_no_margin() {
        let top = Rect::new(200, 0, 600, 300);
        let area = keep_open_area(top, work(), Some(DockEdge::Top), 96);
        assert_eq!((area.top, area.bottom), (0, 312));
        let bottom = Rect::new(200, 740, 600, 1040);
        let area = keep_open_area(bottom, work(), Some(DockEdge::Bottom), 96);
        assert_eq!((area.top, area.bottom), (728, 1040));
    }

    #[test]
    fn another_corral_is_never_in_the_margin() {
        let outer = Rect::new(200, 300, 600, 700);
        let area = keep_open_area(outer, work(), None, 96);
        let neighbour = Rect::new(605, 300, 900, 700);
        assert!(!keeps_open((606, 500), outer, area, &[neighbour]));
        assert!(keeps_open((606, 500), outer, area, &[]));
        // inside the Corral itself (strip or body) always keeps it open
        assert!(keeps_open((210, 305), outer, area, &[neighbour]));
    }

    #[test]
    fn a_docked_write_back_measures_the_stored_body_not_the_lifted_one() {
        // 10 DIP stored at 96 dpi: the docked layout of the stored body keeps 10, the lifted display body is 48
        let stored = Rect::new(100, 600, 300, 610);
        let work = work();
        let (_, _, full) = DockEdge::Bottom.layout(stored, work, 30, 4, 0, true);
        assert_eq!(full.height(), 10, "what is written back");
        let lifted = lifted_body(stored, false, 96);
        assert_eq!(lifted.height(), 48);
        let (_, _, shown) = DockEdge::Bottom.layout(lifted, work, 30, 4, 0, true);
        assert_eq!(shown.height(), 48, "only the display may use this one");
        assert_eq!(stored.height(), 10, "lifting never changes its input");
    }

    #[test]
    fn a_nearly_empty_body_is_shown_at_48_dip() {
        let tiny = Rect::new(100, 100, 300, 110);
        assert_eq!(lifted_body(tiny, false, 96), Rect::new(100, 100, 300, 148));
        assert_eq!(lifted_body(tiny, true, 96), Rect::new(100, 62, 300, 110));
        assert_eq!(lifted_body(tiny, false, 192), Rect::new(100, 100, 300, 196));
        // 80 DIP is untouched, so is exactly 48
        let big = Rect::new(100, 100, 300, 180);
        assert_eq!(lifted_body(big, false, 96), big);
        let edge = Rect::new(100, 100, 300, 148);
        assert_eq!(lifted_body(edge, true, 96), edge);
        assert_eq!(open_body_height(10, 96), 48);
        assert_eq!(open_body_height(80, 96), 80);
    }

    #[test]
    fn q33n_bottom_title_mirrors_top_through_open_close_reversal_and_undock() {
        let edge = DockEdge::Bottom;
        for dpi in [96,120,144,192,240] {
            let m = crate::group_paint::metrics(dpi);
            for bottom in [-1400,1040,2160] {
                let work=Rect::new(-1920,bottom-1200,0,bottom);
                let body=Rect::new(-800,bottom-500,-400,bottom);
                for auto in [false,true] {
                    let mut last=bottom;
                    for opening in [true,false] {
                        for step in 0..=250 {
                            let p=if opening { step } else { 250-step };
                            let visible=visible_body_height(body.height(),p as f64/250.0);
                            let (title,shown,full)=edge.layout(body,work,m.title_h,m.gap,visible,auto);
                            assert_eq!(full,body);
                            assert_eq!((shown.height(),shown.bottom),(visible,bottom));
                            assert_eq!(title.bottom,if visible==0 { bottom } else { shown.top-m.gap });
                            assert_eq!(title.height(),m.title_h);
                            if opening { assert!(title.bottom<=last); } else { assert!(title.bottom>=last); }
                            last=title.bottom;
                        }
                    }
                    assert_eq!(last,bottom);
                    // Reversing or changing the title-underneath preference uses
                    // the same instantaneous geometry without changing full size.
                    for p in [0.0,0.24,0.5,0.8,1.0,0.72,0.32,0.5,0.0] {
                        let visible=visible_body_height(body.height(),p);
                        let (title,_,full)=edge.layout(body,work,m.title_h,m.gap,visible,auto);
                        assert_eq!(edge.layout(body,work,m.title_h,m.gap,visible,!auto),(title,
                            Rect::new(body.left,bottom-visible,body.right,bottom),full));
                        let (drag,below)=edge.drag_body(full,title,false,m.gap,work);
                        assert_eq!(crate::group_paint::title_rect_on(drag,below,&m),title,"retain title grab");
                        assert_eq!(drag.height(),full.height());
                        assert!(drag.bottom<=bottom);
                    }
                }
            }
        }
    }

    #[test]
    fn q33l_top_title_is_fixed_through_both_animation_directions() {
        for dpi in [96,120,144,192,240] {
            let m = crate::group_paint::metrics(dpi);
            for top in [-1400,40,2160] {
                let body = Rect::new(10,top+100,400,top+600);
                let mut last_bottom = top;
                for progress in (0..=250).chain((0..250).rev()) {
                    let visible = visible_body_height(body.height(),progress as f64/250.0);
                    let (title,shown,full) = top_layout(body,top,m.title_h,m.gap,visible,false);
                    assert_eq!(title.top,top);
                    assert_eq!(shown.top,title.bottom+m.gap);
                    assert_eq!(shown.height(),visible);
                    assert_eq!(full.height(),body.height());
                    if progress > 0 { assert!(shown.bottom >= top); }
                    last_bottom = shown.bottom;
                }
                assert_eq!(last_bottom,top+m.title_h+m.gap);
            }
        }
    }

    #[test]
    fn q33m_top_body_is_fixed_and_title_travels_both_directions_when_auto_is_on() {
        for dpi in [96,120,144,192,240] {
            let m = crate::group_paint::metrics(dpi);
            for top in [-1400,40,2160] {
                let body = Rect::new(-800,top+100,-400,top+600);
                let mut last = top;
                for expanding in [true,false] {
                    for step in 0..=250 {
                        let progress = if expanding { step } else { 250-step };
                        let visible = visible_body_height(body.height(),progress as f64/250.0);
                        let (title,shown,full) = top_layout(body,top,m.title_h,m.gap,visible,true);
                        assert_eq!((full.top,shown.top),(top,top));
                        assert_eq!(full.height(),body.height());
                        assert_eq!(shown.height(),visible);
                        assert_eq!(title.top,if visible==0 { top } else { shown.bottom+m.gap });
                        assert_eq!(title.height(),m.title_h);
                        if expanding { assert!(title.top>=last); } else { assert!(title.top<=last); }
                        last=title.top;
                    }
                }
                assert_eq!(last,top,"folded strip returns flush to work-area top");
            }
        }
    }

    #[test]
    fn top_clamp_keeps_the_full_title_and_body_with_resize_anchor() {
        for dpi in [96, 120, 144, 192, 240] {
            let m = crate::group_paint::metrics(dpi);
            for work_top in [-1440, 40, 2160] {
                for below in [false, true] {
                    let body = Rect::new(100, work_top - 400, 500, work_top + 500);
                    let moved = clamp_top(body, below, m.title_h, m.gap, work_top, false);
                    let title = crate::group_paint::title_rect_on(moved, below, &m);
                    assert!(moved.top >= work_top && title.top >= work_top);
                    assert_eq!((moved.width(), moved.height()), (body.width(), body.height()));
                    assert_eq!(clamp_top(moved, below, m.title_h, m.gap, work_top, false), moved);
                    let resized = clamp_top(body, below, m.title_h, m.gap, work_top, true);
                    assert_eq!(resized.bottom, body.bottom, "top resize preserves the bottom anchor");
                    assert_eq!(resized.top, moved.top);
                }
            }
        }
    }

    #[test]
    fn visible_height_follows_rounded_openness_without_a_small_height_cutoff() {
        for (height, openness, expected) in [
            (300, 0.0, 0), (300, 1.0, 300), (300, 0.5, 150),
            (300, 0.005, 2), (100, 0.01, 1), (100, 0.02, 2),
            (1, 0.49, 0), (1, 0.5, 1), (0, 1.0, 0),
        ] {
            assert_eq!(visible_body_height(height, openness), expected);
        }
    }

    #[test]
    fn fold_opens_in_180_ms_and_closes_in_140_ms() {
        let mut t = Tween::at_rest(0.0);
        retarget_fold(&mut t, 0.0, true, true);
        assert!(!t.finished(179.0) && t.finished(180.0));
        assert_eq!(t.value(180.0), OPEN);
        retarget_fold(&mut t, 1000.0, false, true);
        assert!(!t.finished(1139.0) && t.finished(1140.0));
        assert_eq!(t.value(1140.0), 0.0);
        // with Windows animations off the rect switches in one frame
        retarget_fold(&mut t, 2000.0, true, false);
        assert_eq!(t.value(2000.0), OPEN);
    }

    #[test]
    fn fold_reversal_continues_from_the_picture_and_takes_the_distance_left() {
        for at in [0.0, 0.4, 0.9] {
            let mut t = Tween::at_rest(0.0);
            retarget_fold(&mut t, 0.0, true, true);
            let now = 180.0 * at;
            let shown = t.value(now);
            retarget_fold(&mut t, now, false, true);
            assert!((t.value(now) - shown).abs() < 1e-6, "no jump at {at}");
            if shown > 0.0 {
                let expected = (140.0 * shown).max(motion::FRAME_MS);
                assert!(!t.finished(now + expected - 1.0) && t.finished(now + expected + 0.5), "at {at}");
            }
        }
    }

    #[test]
    fn docking_closes_a_hovered_open_corral_and_follows_a_changed_fold() {
        // hovered-open (folded state, hot, body showing) closes; shut and idle stays; expanded closes
        assert!(close_on_dock(true, true, 1.0));
        assert!(close_on_dock(true, false, 0.4), "closing or opening mid-flight");
        assert!(!close_on_dock(true, false, 0.0));
        assert!(close_on_dock(false, false, 1.0));
        // the close really ends folded: 140 ms IN from the open picture lands on 0
        let mut t = Tween::at_rest(OPEN);
        retarget_fold(&mut t, 0.0, false, true);
        assert_eq!(t.value(140.0), 0.0);
        assert_eq!(fold_follow(false, true, true), FoldFollow::Close);
        assert_eq!(fold_follow(true, false, true), FoldFollow::Open);
        assert_eq!(fold_follow(true, false, false), FoldFollow::Clear);
        assert_eq!(fold_follow(true, true, true), FoldFollow::Keep);
        assert_eq!(fold_follow(false, false, false), FoldFollow::Keep);
    }

    #[test]
    fn the_hover_delay_is_milliseconds_clamped_to_100_2000() {
        assert_eq!((hover_delay_ms(500), hover_delay_ms(0), hover_delay_ms(50), hover_delay_ms(9000)), (500, 100, 100, 2000));
    }

    #[test]
    fn the_leave_rule_waits_300_ms_and_restarts_or_cancels() {
        let mut g = LeaveGrace::default();
        assert!(!g.update(0.0, false, false));
        assert!(!g.update(299.0, false, false));
        assert!(g.update(300.0, false, false));
        // coming back cancels, leaving again restarts the wait
        assert!(!g.update(400.0, true, false));
        assert!(!g.update(500.0, false, false));
        assert!(!g.update(799.0, false, false));
        assert!(g.update(800.0, false, false));
    }

    #[test]
    fn the_leave_rule_never_runs_while_blocked_and_starts_when_the_block_ends() {
        let mut g = LeaveGrace::default();
        // button down, an icon or Corral drag, a menu, tooltip or rename box: all are `blocked`
        assert!(!g.update(0.0, false, true));
        assert!(!g.update(10_000.0, false, true));
        assert!(!g.update(10_100.0, false, false), "the 300 ms start now");
        assert!(!g.update(10_399.0, false, false));
        assert!(g.update(10_400.0, false, false));
        g.update(10_450.0, false, true);
        assert!(!g.update(10_460.0, false, false), "a block in between restarts the wait");
    }

    #[test]
    fn the_dock_fold_is_derived_and_a_drag_never_touches_a_user_fold() {
        // RollupEnabled x RollupOnDock x docked
        for (enabled, on_dock, docked, expected) in [
            (true, true, true, RollState::Docked),
            (true, true, false, RollState::Expanded),
            (true, false, true, RollState::Expanded),
            (false, true, true, RollState::Expanded),
        ] {
            assert_eq!(derived_roll(RollState::Expanded, enabled, on_dock, docked), expected);
            assert_eq!(derived_roll(RollState::Docked, enabled, on_dock, docked), expected, "dragging off the edge unfolds by itself");
            assert_eq!(derived_roll(RollState::Manual, enabled, on_dock, docked), RollState::Manual);
        }
        assert_eq!(toggle(RollState::Docked), RollState::Expanded);
        assert_eq!(toggle(RollState::Expanded), RollState::Manual);
    }

    #[test]
    fn docked_rolled_strip_is_flush_with_the_edge() {
        let work = Rect::new(0, 0, 1920, 1040);
        // bottom-docked, title above
        let body = Rect::new(10, 800, 300, 1040);
        let title = Rect::new(10, 766, 300, 796);
        let (strip, vis) = rolled_layout(body, title, true, true, work, 4, 0);
        assert_eq!((strip.bottom, vis.height()), (1040, 0));
        let (strip, vis) = rolled_layout(body, title, true, true, work, 4, 240);
        assert_eq!((strip, vis), (title, body));
        // top-docked, title below
        let body = Rect::new(10, 0, 300, 240);
        let title = Rect::new(10, 244, 300, 274);
        let (strip, _) = rolled_layout(body, title, false, true, work, 4, 0);
        assert_eq!(strip.top, 0);
        let (strip, vis) = rolled_layout(body, title, false, true, work, 4, 240);
        assert_eq!((strip, vis), (title, body));
    }
}
