//! Transient loose-icon coverage. Both cells and published group rectangles are parent-client px.

use crate::geometry::Rect;
use crate::model::GroupContent;
use crate::tabs;
use std::collections::HashSet;
use windows::Win32::UI::WindowsAndMessaging::IsWindowVisible;
use super::{App, Item};

/// Lower-case comparison keys of the selected items that pass `ok`. `ok` sees the original names:
/// it compares with ASCII case rules, which a Unicode-lowercased key (`Łódź.txt`) would fail.
pub(super) fn actionable_keys<'a>(selected: impl Iterator<Item = &'a str>, ok: impl Fn(&str) -> bool) -> HashSet<String> {
    selected.filter(|n| ok(n)).map(str::to_lowercase).collect()
}

/// Rectangles are half open, like `Rect::contains`. Use the exact centre, including half pixels.
fn cell_covered(cell: Rect, covering: &[Rect]) -> bool {
    let (x, y) = (i64::from(cell.left) + i64::from(cell.right), i64::from(cell.top) + i64::from(cell.bottom));
    covering.iter().any(|r| 2 * i64::from(r.left) <= x && x < 2 * i64::from(r.right)
        && 2 * i64::from(r.top) <= y && y < 2 * i64::from(r.bottom))
}

/// Filter even a cached drag or a singleton fallback. An unavailable pressed icon starts no verb.
pub(super) fn drag_names(pressed: &str, candidates: &[String], available: impl Fn(&str) -> bool) -> Vec<String> {
    if !available(pressed) { return Vec::new(); }
    if candidates.iter().any(|n| n.eq_ignore_ascii_case(pressed)) {
        candidates.iter().filter(|n| available(n)).cloned().collect()
    } else {
        vec![pressed.to_string()]
    }
}

impl App {
    fn loose_covering_rects(&self) -> Vec<Rect> {
        self.groups.iter().filter_map(|gw| {
            let group = self.layout.groups.iter().find(|g| g.id == gw.id)?;
            if !tabs::body_visible(&self.layout.groups, group) || self.qh_group_alpha(gw.id) == 0 { return None; }
            if self.pages.snapshotting || self.pages.moving() {
                // Range pictures are composed before their HWNDs are shown or moved, and a slide hides the
                // windows: the current Range's Corrals still cover its icons, or Delete could reach them.
                return self.page_group_visible(group).then(|| self.group_geometry(group).union);
            }
            // Published geometry includes only the strip when folded, and the current animated
            // body while hover-opening. Do not use the stored, fully expanded model rectangle.
            (self.page_layer_visible(group) && unsafe { IsWindowVisible(gw.hwnd) }.as_bool()).then_some(gw.rect)
        }).collect()
    }

    fn loose_cell_rect(&self, item: &Item) -> Rect {
        let (cw, ch) = self.loose_cell();
        Rect::new(item.pos.0, item.pos.1, item.pos.0 + cw, item.pos.1 + ch)
    }

    pub(super) fn loose_covered(&self, item: &Item) -> bool {
        cell_covered(self.loose_cell_rect(item), &self.loose_covering_rects())
    }

    /// Only a Corral on the icon's own Range covers it; a carried icon keeps its old cell during the drag.
    fn on_current_range(&self, name: &str) -> bool {
        crate::pages::icon_visible(self.settings.swipe_enabled, self.layout.icon_page(name), self.pages.current)
    }

    pub(super) fn loose_name_covered(&self, name: &str) -> bool {
        self.group_of(name).is_none() && self.on_current_range(name) && self.items.iter()
            .find(|i| i.shell.parsing_name.eq_ignore_ascii_case(name)).is_some_and(|i| self.loose_covered(i))
    }

    /// One eligibility check for selection and verbs, including selections made before coverage.
    pub(super) fn name_actionable(&self, name: &str) -> bool {
        self.name_actionable_in(name, &self.covered_loose_set())
    }

    /// `name_actionable` against a `covered_loose_set` computed once, for loops over many items.
    /// A path two Gates show is held by both; it counts while any holder's body is on screen.
    pub(super) fn name_actionable_in(&self, name: &str, covered: &HashSet<String>) -> bool {
        if self.qh_name_hidden(name) || !self.page_name_visible(name) { return false; }
        let homes = self.homes_of(name);
        if homes.is_empty() {
            self.loose_on_screen(name) && !covered.contains(&name.to_lowercase())
        } else {
            homes.into_iter().any(|id| !self.body_hidden(id))
        }
    }

    /// Lower-case names of the loose icons whose cell centre is under a visible Corral or Gate.
    /// Group membership is built once, so a check costs one pass over the items.
    pub(super) fn covered_loose_set(&self) -> HashSet<String> {
        let covering = self.loose_covering_rects();
        if covering.is_empty() { return HashSet::new(); }
        // ASCII case, as `Layout::group_of` decides membership.
        let held: HashSet<String> = self.layout.groups.iter().filter_map(|g| match &g.content {
            GroupContent::Items { items } => Some(items),
            GroupContent::Portal { .. } => None,
        }).flatten().map(|n| n.to_ascii_lowercase()).collect();
        self.items.iter().filter(|i| cell_covered(self.loose_cell_rect(i), &covering)
                && !held.contains(&i.shell.parsing_name.to_ascii_lowercase())
                && self.on_current_range(&i.shell.parsing_name))
            .map(|i| i.shell.parsing_name.to_lowercase())
            .filter(|k| !self.gate_index.contains_key(k)).collect()
    }

    /// Group moves do no surface pixel work unless the covered set changed. Surface rendering
    /// remembers the set it actually painted; Range snapshots must not replace that live cache.
    pub(super) fn repaint_loose_coverage(&mut self) {
        if !self.pages.snapshotting && !self.pages.moving() && self.covered_loose_set() != self.covered_loose {
            self.render_surface();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polish_names_survive_the_ascii_case_lookup() {
        // The lookups compare with eq_ignore_ascii_case, so they must get the original name.
        let known = |n: &str| ["Łódź.txt", "Żółć.doc"].iter().any(|k| k.eq_ignore_ascii_case(n));
        assert!(!known(&"Łódź.txt".to_lowercase()), "a lowercased key fails the ASCII compare");
        let keys = actionable_keys(["Łódź.txt", "Żółć.doc", "Other.txt"].into_iter(), known);
        assert!(keys.contains("łódź.txt") && keys.contains("żółć.doc") && !keys.contains("other.txt"));
    }

    #[test]
    fn centre_edges_are_half_open() {
        let cell = Rect::new(0, 0, 100, 100);
        assert!(cell_covered(cell, &[Rect::new(50, 50, 100, 100)]));
        assert!(!cell_covered(cell, &[Rect::new(0, 0, 50, 100)]));
        assert!(!cell_covered(cell, &[Rect::new(0, 0, 100, 50)]));
        assert!(!cell_covered(cell, &[Rect::new(50, 0, 50, 100)]));
    }

    #[test]
    fn partial_overlap_needs_the_centre() {
        let cell = Rect::new(0, 0, 100, 100);
        assert!(!cell_covered(cell, &[Rect::new(51, 0, 200, 100)]));
        assert!(cell_covered(cell, &[Rect::new(49, 49, 51, 51)]));
        // A strip above the cell does not cover it; opening the body does.
        assert!(!cell_covered(cell, &[Rect::new(0, -24, 100, 0)]));
        assert!(cell_covered(cell, &[Rect::new(0, -24, 100, 100)]));
    }

    #[test]
    fn any_cover_counts_and_none_uncover() {
        let cell = Rect::new(-100, -100, 0, 0);
        let away = Rect::new(0, 0, 100, 100);
        let over = Rect::new(-75, -75, -25, -25);
        assert!(!cell_covered(cell, &[]));
        assert!(!cell_covered(cell, &[away]));
        assert!(cell_covered(cell, &[away, over]));
        assert!(cell_covered(cell, &[over, away]));
        assert!(cell_covered(Rect::new(-3, -3, 0, 0), &[Rect::new(-2, -2, -1, -1)]));
        assert!(!cell_covered(Rect::new(0, 0, 3, 3), &[Rect::new(0, 0, 1, 3)]));
    }

    #[test]
    fn verbs_filter_stale_selection_cached_drag_and_singleton() {
        let names = vec!["clear".to_string(), "covered".to_string()];
        let cells = [Rect::new(0, 0, 100, 100), Rect::new(100, 0, 200, 100)];
        let covering = [Rect::new(150, 0, 250, 100)];
        let available = |name: &str| !cell_covered(cells[usize::from(name.eq_ignore_ascii_case("covered"))], &covering);
        assert_eq!(drag_names("clear", &names, available), vec!["clear"]);
        assert!(drag_names("COVERED", &names, available).is_empty());
        assert!(drag_names("covered", &[], available).is_empty());
        assert_eq!(drag_names("clear", &[], available), vec!["clear"]);
        assert_eq!(drag_names("covered", &names, |_| true), names);
    }
}
