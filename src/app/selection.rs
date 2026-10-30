//! Desktop-style multi-selection of icons (loose on the surface and inside groups): the pure rules.
//!
//! Icons are real desktop list-view items, so the selection follows Explorer's list-view
//! semantics (observed 2026-10-12: several icons can be selected and dragged together):
//! a plain click selects one item and clears the rest; Ctrl+click toggles; Shift+click selects the
//! range between the anchor and the clicked item in display order; pressing an already-selected
//! item keeps the selection so the whole set can be dragged, and only a release without a drag
//! collapses it; a rubber band selects the items it touches (Ctrl/Shift keeps the earlier set).
//! Everything here is free of window state so it can be unit tested; `input.rs` applies it.

use crate::geometry::Rect;
use crate::model::{GroupContent, Layout};

/// `MK_SHIFT` / `MK_CONTROL` in the `wParam` of a mouse message.
const MK_SHIFT: usize = 0x0004;
const MK_CONTROL: usize = 0x0008;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
}

impl Modifiers {
    /// Modifier keys carried by a mouse message (posted QA messages can set them, unlike key state).
    pub fn from_mouse_keys(wparam: usize) -> Self {
        Self { ctrl: wparam & MK_CONTROL != 0, shift: wparam & MK_SHIFT != 0 }
    }

    /// A press on empty space keeps the earlier selection (the band adds to it).
    pub fn keeps_selection(&self) -> bool {
        self.ctrl || self.shift
    }
}

/// What a left press on an item changes at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PressAction {
    /// Only the pressed item is selected.
    Replace,
    /// The pressed (unselected) item joins the selection (Ctrl).
    Add,
    /// The selection stays as it is (pressed item already selected); a drag moves all of it.
    Keep,
    /// Range from the anchor to the pressed item; `add` (Ctrl+Shift) keeps the earlier selection.
    Range { add: bool },
}

/// What the button release does when no drag happened in between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Release {
    /// Plain click on a selected item: it becomes the only selected item.
    CollapseToPressed,
    /// Ctrl+click on a selected item: it is deselected.
    DeselectPressed,
}

pub(super) fn plan_press(mods: Modifiers, pressed_selected: bool) -> (PressAction, Option<Release>) {
    if mods.shift {
        return (PressAction::Range { add: mods.ctrl }, None);
    }
    match (mods.ctrl, pressed_selected) {
        (true, true) => (PressAction::Keep, Some(Release::DeselectPressed)),
        (true, false) => (PressAction::Add, None),
        (false, true) => (PressAction::Keep, Some(Release::CollapseToPressed)),
        (false, false) => (PressAction::Replace, None),
    }
}

/// Selection bookkeeping that is not a per-item flag (`Item::selected` holds the flags).
#[derive(Default)]
pub(super) struct State {
    /// Start of Shift ranges (last plain/Ctrl click).
    pub anchor: Option<String>,
    /// The item with the keyboard focus (last clicked); F2 renames only this one.
    pub focus: Option<String>,
    /// Deferred effect of the current press, applied on release when no drag started.
    pub release: Option<(Release, String)>,
    /// Lower-case names that stay selected under a band (Ctrl/Shift held at its start).
    pub band_base: Vec<String>,
    /// Group whose body the current band was started in (`None` = the desktop surface).
    pub band_group: Option<u64>,
}

/// Items between `anchor` and `target` (inclusive) in `order`; `None` when one is not in it.
pub(super) fn range<'a>(order: &'a [String], anchor: &str, target: &str) -> Option<&'a [String]> {
    let a = order.iter().position(|n| n.eq_ignore_ascii_case(anchor))?;
    let t = order.iter().position(|n| n.eq_ignore_ascii_case(target))?;
    Some(&order[a.min(t)..=a.max(t)])
}

/// Indices of the `cells` the band touches. The band counts as one pixel larger on its far edges
/// so that a band dragged along a single row or column still hits.
pub(super) fn band_hits(band: Rect, cells: &[(usize, Rect)]) -> Vec<usize> {
    let band = Rect::new(band.left, band.top, band.right + 1, band.bottom + 1);
    cells.iter().filter(|(_, cell)| band.overlap_area(cell) > 0).map(|(i, _)| *i).collect()
}

/// Column-major reading order of loose icons (as on a desktop sorted by name and arranged in
/// columns): by cell column, then top to bottom. `items` = (key, cell origin).
pub(super) fn visual_order(items: &[(usize, (i32, i32))], cell: (i32, i32)) -> Vec<usize> {
    let mut sorted: Vec<&(usize, (i32, i32))> = items.iter().collect();
    let width = cell.0.max(1);
    sorted.sort_by_key(|(_, (x, y))| ((*x + width / 2).div_euclid(width), *y, *x));
    sorted.into_iter().map(|(i, _)| *i).collect()
}

/// Folder an item lives in, as a comparison key: the directory of a file-system path, or `::` for
/// namespace items such as This PC (children of the desktop root). Mirrors the "one parent folder"
/// rule of `dragdrop::begin_drag` and `ShellMenu::for_items`, which compare the parent pidls (the
/// user's and the public Desktop are different parents).
pub(super) fn parent_key(parsing_name: &str) -> String {
    if parsing_name.starts_with("::") {
        return "::".to_string();
    }
    let trimmed = parsing_name.trim_end_matches(['\\', '/']);
    match trimmed.rfind(['\\', '/']) {
        Some(at) => trimmed[..at].trim_end_matches(['\\', '/']).to_lowercase(),
        None => String::new(),
    }
}

/// Splits `names` by parent folder, keeping the order; the first set holds the first name.
pub(super) fn split_by_parent(names: &[String]) -> Vec<Vec<String>> {
    let mut sets: Vec<(String, Vec<String>)> = Vec::new();
    for name in names {
        let key = parent_key(name);
        match sets.iter_mut().find(|(k, _)| *k == key) {
            Some((_, set)) => set.push(name.clone()),
            None => sets.push((key, vec![name.clone()])),
        }
    }
    sets.into_iter().map(|(_, set)| set).collect()
}

/// The part of `names` that can travel in one shell data object / context menu together with
/// `pressed`: the names sharing its parent folder (order kept). The second value counts the others.
pub(super) fn sharing_parent_with(pressed: &str, names: &[String]) -> (Vec<String>, usize) {
    let key = parent_key(pressed);
    let kept: Vec<String> = names.iter().filter(|n| parent_key(n) == key).cloned().collect();
    let dropped = names.len() - kept.len();
    (kept, dropped)
}

/// Where an item sits, for keeping the dragged set's arrangement when it is dropped on the desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Home {
    /// Loose icon: cell origin in client pixels.
    Loose((i32, i32)),
    /// Member of a group: grid column and row of its cell.
    Group { id: u64, col: i32, row: i32 },
}

/// Offsets (px) from the pressed item's drop origin for every item of a dragged set. Items that
/// share the pressed item's container keep their relative arrangement (loose: pixel offsets; group:
/// grid offsets in `cell` units); the rest are stacked in a column below, so no two overlap.
pub(super) fn loose_offsets(items: &[(String, Home)], pressed: usize, cell: (i32, i32)) -> Vec<(String, (i32, i32))> {
    let anchor = items[pressed].1;
    let aligned = |home: &Home| match (anchor, home) {
        (Home::Loose(p), Home::Loose(q)) => Some((q.0 - p.0, q.1 - p.1)),
        (Home::Group { id: a, col: pc, row: pr }, Home::Group { id: b, col, row }) if a == *b => Some(((col - pc) * cell.0, (row - pr) * cell.1)),
        _ => None,
    };
    let max_y = items.iter().filter_map(|(_, h)| aligned(h)).map(|o| o.1).max().unwrap_or(0).max(0);
    let mut stacked = 0;
    items
        .iter()
        .map(|(name, home)| match aligned(home) {
            Some(offset) => (name.clone(), offset),
            None => {
                stacked += 1;
                (name.clone(), (0, max_y + stacked * cell.1))
            }
        })
        .collect()
}

// ---- placing icons on the desktop surface (shared by drops and `arrange_loose`) ------------------

/// Index of the monitor whose `bounds` contain `point`, else the nearest one (`None` = no monitor).
pub(super) fn monitor_index_at(point: (i32, i32), bounds: &[Rect]) -> Option<usize> {
    let distance = |r: &Rect| {
        let dx = (r.left - point.0).max(0).max(point.0 - (r.right - 1)) as i64;
        let dy = (r.top - point.1).max(0).max(point.1 - (r.bottom - 1)) as i64;
        dx * dx + dy * dy
    };
    // `min_by_key` keeps the first of equal distances, so the containing (distance 0) monitor wins
    bounds.iter().enumerate().min_by_key(|(_, r)| distance(r)).map(|(i, _)| i)
}

/// Cell origin moved the shortest way so the whole `cell` lies inside `area` (a cell larger than
/// the area is pinned to its top-left corner).
pub(super) fn clamp_cell(origin: (i32, i32), cell: (i32, i32), area: &Rect) -> (i32, i32) {
    (origin.0.min(area.right - cell.0).max(area.left), origin.1.min(area.bottom - cell.1).max(area.top))
}

fn cell_rect(origin: (i32, i32), cell: (i32, i32)) -> Rect {
    Rect::new(origin.0, origin.1, origin.0 + cell.0, origin.1 + cell.1)
}

/// Rows of the column-major cell grid of `area` (as `arrange_loose` lays out icons).
pub(super) fn grid_rows(area: &Rect, cell: (i32, i32)) -> i32 {
    (area.height() / cell.1.max(1)).max(1)
}

/// Origin of grid slot `slot` (column-major: down the rows of a column, then the next column).
pub(super) fn grid_cell(area: &Rect, cell: (i32, i32), rows: i32, slot: i32) -> (i32, i32) {
    (area.left + (slot / rows) * cell.0, area.top + (slot % rows) * cell.1)
}

/// The first grid cell at or after slot `from` that overlaps nothing in `blocked`; a search that
/// ran past slot `limit` stops at the last cell tried (the caller accepts an overlap then).
/// Returns the cell origin and the next slot to try.
pub(super) fn first_free_grid_cell(area: &Rect, cell: (i32, i32), blocked: &[Rect], from: i32, limit: i32) -> ((i32, i32), i32) {
    let rows = grid_rows(area, cell);
    let mut slot = from;
    loop {
        let origin = grid_cell(area, cell, rows, slot);
        slot += 1;
        if !blocked.iter().any(|b| b.overlap_area(&cell_rect(origin, cell)) > 0) || slot > limit {
            return (origin, slot);
        }
    }
}

/// Where a dropped loose icon goes: `want` clamped into `area`; when that cell overlaps something
/// in `blocked` (other loose icons, groups) the nearest free cell of the area's grid. When no
/// cell is free the clamped `want` is kept (on screen, overlapping), never an off-screen one.
pub(super) fn nearest_free_cell(want: (i32, i32), cell: (i32, i32), area: &Rect, blocked: &[Rect]) -> (i32, i32) {
    let want = clamp_cell(want, cell, area);
    let free = |origin: (i32, i32)| !blocked.iter().any(|b| b.overlap_area(&cell_rect(origin, cell)) > 0);
    if free(want) {
        return want;
    }
    let rows = grid_rows(area, cell);
    let columns = (area.width() / cell.0.max(1)).max(1);
    let mut best: Option<(i64, (i32, i32))> = None;
    for slot in 0..rows * columns {
        let origin = clamp_cell(grid_cell(area, cell, rows, slot), cell, area);
        let (dx, dy) = ((origin.0 - want.0) as i64, (origin.1 - want.1) as i64);
        let distance = dx * dx + dy * dy;
        if best.map_or(true, |(d, _)| distance < d) && free(origin) {
            best = Some((distance, origin));
        }
    }
    best.map_or(want, |(_, origin)| origin)
}

/// Final cells of a dropped set of loose icons: every wanted origin is clamped into the work area
/// of the monitor under the cursor and moved to the nearest free cell if it collides with `blocked`
/// or with an icon of the set placed before it (the order of `wanted` is the priority).
pub(super) fn place_loose_set(wanted: &[(String, (i32, i32))], cell: (i32, i32), area: &Rect, blocked: &[Rect]) -> Vec<(String, (i32, i32))> {
    let mut blocked = blocked.to_vec();
    wanted
        .iter()
        .map(|(name, want)| {
            let origin = nearest_free_cell(*want, cell, area, &blocked);
            blocked.push(cell_rect(origin, cell));
            (name.clone(), origin)
        })
        .collect()
}

/// Items opened at once above this count are confirmed first. Explorer's own default of the
/// `MultipleInvokePromptMinimum` policy value is 15.
pub(super) const DEFAULT_MULTIPLE_INVOKE_PROMPT_MINIMUM: u32 = 15;

/// Whether opening `count` items at once needs the "Are you sure you want to open these N items?"
/// question (as in Explorer: more items than the minimum, never for a single item).
pub(super) fn needs_open_confirmation(count: usize, minimum: u32) -> bool {
    count > 1 && count as u64 > minimum as u64
}

/// Put `names` (in the given order) into group `id` at consecutive slots starting at `slot`
/// (clamped to the end). They are removed from wherever they were first, so `slot` is the final
/// position of the first one. References only: no file is touched.
pub(super) fn remaining_insertion_slot(order: &[String], names: &[String], slot: usize) -> usize {
    let removed = order.iter().take(slot).filter(|n| names.iter().any(|name| name.eq_ignore_ascii_case(n))).count();
    slot.saturating_sub(removed)
}

pub(super) fn insert_set(layout: &mut Layout, names: &[String], id: u64, slot: usize) {
    if !layout.groups.iter().any(|g| g.id == id && matches!(g.content, GroupContent::Items { .. })) {
        return;
    }
    for name in names {
        layout.assign_item(name, None);
        layout.loose.remove(&name.to_lowercase());
    }
    for (k, name) in names.iter().enumerate() {
        layout.insert_item(name, id, slot + k);
        // a drop into a Corral is the user's own placement (round-up-spec 3)
        if layout.group_of(name) == Some(id) {
            layout.mark_manual_now(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn list_insertion_line_accounts_for_the_rows_dragged_out() {
        let order = names(&["a", "b", "c", "d"]);
        let moving = names(&["a", "C"]);
        assert_eq!(remaining_insertion_slot(&order, &moving, 0), 0);
        assert_eq!(remaining_insertion_slot(&order, &moving, 1), 0);
        assert_eq!(remaining_insertion_slot(&order, &moving, 3), 1);
        assert_eq!(remaining_insertion_slot(&order, &moving, 4), 2);
        let mut layout = Layout::new();
        let id = layout.add_group("List".into(), "m".into(), Rect::new(0, 0, 200, 200), GroupContent::Items { items: order.clone() });
        insert_set(&mut layout, &moving, id, remaining_insertion_slot(&order, &moving, 3));
        let GroupContent::Items { items } = &layout.groups[0].content else { panic!() };
        assert_eq!(items, &names(&["b", "a", "C", "d"]));
    }

    #[test]
    fn modifiers_come_from_mouse_keys() {
        assert_eq!(Modifiers::from_mouse_keys(0x0001), Modifiers { ctrl: false, shift: false });
        assert_eq!(Modifiers::from_mouse_keys(0x0009), Modifiers { ctrl: true, shift: false });
        assert_eq!(Modifiers::from_mouse_keys(0x0005), Modifiers { ctrl: false, shift: true });
        assert!(Modifiers { ctrl: true, shift: false }.keeps_selection());
        assert!(!Modifiers::default().keeps_selection());
    }

    #[test]
    fn press_plans_follow_explorer() {
        let none = Modifiers::default();
        let ctrl = Modifiers { ctrl: true, shift: false };
        let shift = Modifiers { ctrl: false, shift: true };
        let both = Modifiers { ctrl: true, shift: true };
        assert_eq!(plan_press(none, false), (PressAction::Replace, None));
        // already selected: nothing collapses before a possible drag
        assert_eq!(plan_press(none, true), (PressAction::Keep, Some(Release::CollapseToPressed)));
        assert_eq!(plan_press(ctrl, false), (PressAction::Add, None));
        assert_eq!(plan_press(ctrl, true), (PressAction::Keep, Some(Release::DeselectPressed)));
        assert_eq!(plan_press(shift, true), (PressAction::Range { add: false }, None));
        assert_eq!(plan_press(both, false), (PressAction::Range { add: true }, None));
    }

    #[test]
    fn range_runs_in_display_order_either_direction() {
        let order = names(&["a", "b", "c", "d", "e"]);
        assert_eq!(range(&order, "b", "d").unwrap(), &names(&["b", "c", "d"])[..]);
        assert_eq!(range(&order, "D", "B").unwrap(), &names(&["b", "c", "d"])[..]);
        assert_eq!(range(&order, "c", "c").unwrap(), &names(&["c"])[..]);
        assert!(range(&order, "x", "c").is_none());
        assert!(range(&order, "c", "x").is_none());
    }

    #[test]
    fn band_hits_touching_cells_only() {
        let cells = vec![
            (0, Rect::new(0, 0, 100, 100)),
            (1, Rect::new(100, 0, 200, 100)),
            (2, Rect::new(0, 100, 100, 200)),
            (3, Rect::new(100, 100, 200, 200)),
        ];
        assert_eq!(band_hits(Rect::new(50, 50, 150, 150), &cells), vec![0, 1, 2, 3]);
        assert_eq!(band_hits(Rect::new(10, 10, 40, 40), &cells), vec![0]);
        assert_eq!(band_hits(Rect::new(110, 10, 190, 90), &cells), vec![1]);
        assert!(band_hits(Rect::new(300, 300, 400, 400), &cells).is_empty());
        // a band dragged along one row has no height but still hits the cells under it
        assert_eq!(band_hits(Rect::new(10, 50, 150, 50), &cells), vec![0, 1]);
        // edges that only touch do not hit
        assert_eq!(band_hits(Rect::new(10, 10, 99, 99), &cells), vec![0]);
    }

    #[test]
    fn loose_reading_order_is_column_major() {
        // two columns (x 0 and 100), icons at rows 0 and 100; entries given scrambled
        let items = vec![(0, (100, 100)), (1, (0, 100)), (2, (100, 0)), (3, (0, 0)), (4, (3, 50))];
        assert_eq!(visual_order(&items, (100, 100)), vec![3, 4, 1, 2, 0]);
    }

    #[test]
    fn parent_keys_separate_user_and_public_desktop() {
        let user = r"C:\Users\me\Desktop\a.txt";
        let public = r"C:\Users\Public\Desktop\b.lnk";
        assert_ne!(parent_key(user), parent_key(public));
        assert_eq!(parent_key(user), parent_key(r"c:\users\ME\desktop\Other.txt"));
        assert_eq!(parent_key(r"C:\Users\me\Desktop\folder\"), parent_key(user), "a folder shares the parent of its siblings");
        assert_eq!(parent_key("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"), parent_key("::{645FF040-5081-101B-9F08-00AA002F954E}"));
        assert_ne!(parent_key("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"), parent_key(user));
    }

    #[test]
    fn drag_set_keeps_the_pressed_items_parent_only() {
        let all = names(&[r"C:\Users\me\Desktop\a.txt", r"C:\Users\Public\Desktop\b.lnk", r"C:\Users\me\Desktop\c.txt"]);
        let (kept, dropped) = sharing_parent_with(&all[0], &all);
        assert_eq!(kept, names(&[r"C:\Users\me\Desktop\a.txt", r"C:\Users\me\Desktop\c.txt"]));
        assert_eq!(dropped, 1);
        let (kept, dropped) = sharing_parent_with(&all[1], &all);
        assert_eq!(kept, names(&[r"C:\Users\Public\Desktop\b.lnk"]));
        assert_eq!(dropped, 2);
        let sets = split_by_parent(&all);
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[0].len(), 2);
        assert_eq!(sets[1], names(&[r"C:\Users\Public\Desktop\b.lnk"]));
    }

    #[test]
    fn loose_items_keep_their_relative_offsets() {
        let items = vec![
            ("p".to_string(), Home::Loose((200, 300))),
            ("q".to_string(), Home::Loose((200, 400))),
            ("r".to_string(), Home::Loose((100, 300))),
        ];
        let offsets = loose_offsets(&items, 0, (100, 100));
        assert_eq!(offsets, vec![("p".to_string(), (0, 0)), ("q".to_string(), (0, 100)), ("r".to_string(), (-100, 0))]);
    }

    #[test]
    fn group_members_keep_their_grid_and_strangers_stack_below() {
        let items = vec![
            ("p".to_string(), Home::Group { id: 1, col: 1, row: 0 }),
            ("q".to_string(), Home::Group { id: 1, col: 2, row: 1 }),
            ("s".to_string(), Home::Group { id: 2, col: 0, row: 0 }),
            ("t".to_string(), Home::Loose((5, 5))),
        ];
        let offsets = loose_offsets(&items, 0, (100, 80));
        assert_eq!(
            offsets,
            vec![("p".to_string(), (0, 0)), ("q".to_string(), (100, 80)), ("s".to_string(), (0, 160)), ("t".to_string(), (0, 240))]
        );
        let rects: Vec<(i32, i32)> = offsets.iter().map(|(_, o)| *o).collect();
        for (i, a) in rects.iter().enumerate() {
            assert!(rects.iter().skip(i + 1).all(|b| a != b), "no two icons share a cell");
        }
    }

    // ---- loose placement -------------------------------------------------------------------

    const CELL: (i32, i32) = (100, 80);

    fn disjoint(cells: &[(i32, i32)]) -> bool {
        cells.iter().enumerate().all(|(i, a)| cells.iter().skip(i + 1).all(|b| cell_rect(*a, CELL).overlap_area(&cell_rect(*b, CELL)) == 0))
    }

    fn inside(origin: (i32, i32), area: &Rect) -> bool {
        origin.0 >= area.left && origin.1 >= area.top && origin.0 + CELL.0 <= area.right && origin.1 + CELL.1 <= area.bottom
    }

    fn wanted(list: &[(&str, (i32, i32))]) -> Vec<(String, (i32, i32))> {
        list.iter().map(|(n, c)| (n.to_string(), *c)).collect()
    }

    fn cells_of(placed: &[(String, (i32, i32))]) -> Vec<(i32, i32)> {
        placed.iter().map(|(_, c)| *c).collect()
    }

    #[test]
    fn clamp_pulls_a_cell_back_into_the_area() {
        let area = Rect::new(0, 0, 1000, 800);
        assert_eq!(clamp_cell((300, 200), CELL, &area), (300, 200));
        assert_eq!(clamp_cell((-50, -10), CELL, &area), (0, 0));
        assert_eq!(clamp_cell((990, 790), CELL, &area), (900, 720), "whole cell stays inside");
        assert_eq!(clamp_cell((5000, 5000), CELL, &area), (900, 720));
        // an area smaller than a cell pins the cell to its top-left corner
        assert_eq!(clamp_cell((50, 50), CELL, &Rect::new(10, 20, 60, 70)), (10, 20));
    }

    #[test]
    fn drop_past_the_bottom_stays_on_screen_and_apart() {
        let area = Rect::new(0, 0, 1000, 800);
        // pressed icon near the bottom edge, three strangers stacked below it (loose_offsets column)
        let placed = place_loose_set(&wanted(&[("p", (400, 700)), ("s1", (400, 780)), ("s2", (400, 860)), ("s3", (400, 940))]), CELL, &area, &[]);
        let cells = cells_of(&placed);
        assert!(cells.iter().all(|c| inside(*c, &area)), "{cells:?}");
        assert!(disjoint(&cells), "{cells:?}");
        assert_eq!(placed[0].1, (400, 700), "the first (pressed) icon keeps its cell");
        assert_eq!(placed.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), vec!["p", "s1", "s2", "s3"]);
    }

    #[test]
    fn drop_past_the_right_edge_stays_on_screen() {
        let area = Rect::new(0, 0, 1000, 800);
        let placed = place_loose_set(&wanted(&[("a", (950, 100)), ("b", (1050, 100)), ("c", (1150, 180))]), CELL, &area, &[]);
        let cells = cells_of(&placed);
        assert!(cells.iter().all(|c| inside(*c, &area)), "{cells:?}");
        assert!(disjoint(&cells), "{cells:?}");
        assert_eq!(placed[0].1, (900, 100));
    }

    #[test]
    fn colliding_with_a_loose_icon_takes_the_nearest_free_cell() {
        let area = Rect::new(0, 0, 1000, 800);
        let stranger = cell_rect((300, 160), CELL);
        let got = nearest_free_cell((310, 170), CELL, &area, &[stranger]);
        assert!(inside(got, &area));
        assert_eq!(cell_rect(got, CELL).overlap_area(&stranger), 0);
        // the grid cell next to the stranger, not one across the screen
        assert!((got.0 - 310).abs() <= 200 && (got.1 - 170).abs() <= 160, "{got:?}");
        // a free wanted cell is used as it is, off the grid
        assert_eq!(nearest_free_cell((517, 333), CELL, &area, &[stranger]), (517, 333));
    }

    #[test]
    fn colliding_with_a_group_rect_moves_out_of_it() {
        let area = Rect::new(0, 0, 1000, 800);
        let group = Rect::new(0, 0, 450, 800); // a tall group on the left half
        let placed = place_loose_set(&wanted(&[("a", (100, 100)), ("b", (100, 180))]), CELL, &area, &[group]);
        let cells = cells_of(&placed);
        assert!(cells.iter().all(|c| inside(*c, &area) && cell_rect(*c, CELL).overlap_area(&group) == 0), "{cells:?}");
        assert!(disjoint(&cells));
        assert_eq!(cells[0].0, 500, "the nearest grid column right of the group");
    }

    #[test]
    fn a_crowded_area_keeps_the_clamped_cell_instead_of_leaving_it() {
        let area = Rect::new(0, 0, 200, 160);
        assert_eq!(nearest_free_cell((500, 500), CELL, &area, &[area]), (100, 80));
    }

    #[test]
    fn the_monitor_under_the_cursor_decides_the_area() {
        // second monitor left of the primary (negative coordinates), third one above
        let bounds = [Rect::new(0, 0, 1920, 1080), Rect::new(-1280, 0, 0, 1024), Rect::new(0, -1080, 1920, 0)];
        assert_eq!(monitor_index_at((100, 100), &bounds), Some(0));
        assert_eq!(monitor_index_at((-1, 500), &bounds), Some(1));
        assert_eq!(monitor_index_at((-1280, 0), &bounds), Some(1));
        assert_eq!(monitor_index_at((1000, -1), &bounds), Some(2));
        // outside every monitor: the nearest one
        assert_eq!(monitor_index_at((-3000, 400), &bounds), Some(1));
        assert_eq!(monitor_index_at((5000, 5000), &bounds), Some(0));
        assert_eq!(monitor_index_at((0, 0), &[]), None);
        // a set dropped with the cursor on the left monitor lands inside that monitor's work area
        let work = Rect::new(-1280, 0, 0, 984); // taskbar at the bottom
        let placed = place_loose_set(&wanted(&[("a", (-50, 900)), ("b", (-50, 980)), ("c", (30, 100))]), CELL, &work, &[]);
        let cells = cells_of(&placed);
        assert!(cells.iter().all(|c| inside(*c, &work)), "{cells:?}");
        assert!(disjoint(&cells));
    }

    #[test]
    fn grid_search_for_arrange_loose_skips_blocked_cells() {
        let area = Rect::new(0, 0, 300, 160); // 2 rows x 3 columns of CELL
        let blocked = [cell_rect((0, 0), CELL), cell_rect((0, 80), CELL)];
        let (first, next) = first_free_grid_cell(&area, CELL, &blocked, 0, 10_000);
        assert_eq!(first, (100, 0), "column 0 is taken, the search continues in column 1");
        assert_eq!(next, 3);
        let (second, _) = first_free_grid_cell(&area, CELL, &blocked, next, 10_000);
        assert_eq!(second, (100, 80));
        // everything blocked: the search stops at the limit with the last cell tried
        let (last, next) = first_free_grid_cell(&area, CELL, &[area], 0, 4);
        assert_eq!(next, 5);
        assert_eq!(last, grid_cell(&area, CELL, 2, 4));
    }

    #[test]
    fn many_items_open_only_after_confirmation() {
        assert!(!needs_open_confirmation(1, DEFAULT_MULTIPLE_INVOKE_PROMPT_MINIMUM));
        assert!(!needs_open_confirmation(15, DEFAULT_MULTIPLE_INVOKE_PROMPT_MINIMUM));
        assert!(needs_open_confirmation(16, DEFAULT_MULTIPLE_INVOKE_PROMPT_MINIMUM));
        assert!(needs_open_confirmation(3, 2), "a lower policy value prompts earlier");
        assert!(!needs_open_confirmation(1, 0), "never for a single item");
        assert!(needs_open_confirmation(2, 0));
    }

    fn layout_with(items: &[&str]) -> (Layout, u64) {
        let mut layout = Layout::new();
        let id = layout.add_group("G".into(), "m".into(), Rect::default(), GroupContent::Items { items: names(items) });
        (layout, id)
    }

    fn items_of(layout: &Layout, id: u64) -> Vec<String> {
        match &layout.groups.iter().find(|g| g.id == id).unwrap().content {
            GroupContent::Items { items } => items.clone(),
            GroupContent::Portal { .. } => Vec::new(),
        }
    }

    #[test]
    fn set_is_inserted_at_consecutive_slots() {
        let (mut layout, id) = layout_with(&["a", "b", "c", "d"]);
        // move b and d to slot 0
        insert_set(&mut layout, &names(&["b", "d"]), id, 0);
        assert_eq!(items_of(&layout, id), names(&["b", "d", "a", "c"]));
        // move the first two to slot 2 (final position of the first one)
        insert_set(&mut layout, &names(&["b", "d"]), id, 2);
        assert_eq!(items_of(&layout, id), names(&["a", "c", "b", "d"]));
        // a slot past the end appends, still consecutive
        insert_set(&mut layout, &names(&["a", "c"]), id, 9);
        assert_eq!(items_of(&layout, id), names(&["b", "d", "a", "c"]));
    }

    #[test]
    fn set_from_other_homes_joins_the_group_once() {
        let (mut layout, id) = layout_with(&["a"]);
        let other = layout.add_group("H".into(), "m".into(), Rect::default(), GroupContent::Items { items: names(&["x", "y"]) });
        layout.place_loose("L", crate::model::LoosePosition { monitor: "m".into(), x: 1, y: 2 });
        insert_set(&mut layout, &names(&["x", "L"]), id, 1);
        assert_eq!(items_of(&layout, id), names(&["a", "x", "L"]));
        assert_eq!(items_of(&layout, other), names(&["y"]));
        assert!(layout.loose_position("L").is_none(), "a grouped item has no loose position");
    }

    #[test]
    fn set_into_a_portal_changes_nothing() {
        let mut layout = Layout::new();
        let portal = layout.add_group("P".into(), "m".into(), Rect::default(), GroupContent::Portal { path: "C:\\x".into() });
        layout.place_loose("L", crate::model::LoosePosition { monitor: "m".into(), x: 1, y: 2 });
        insert_set(&mut layout, &names(&["L"]), portal, 0);
        assert!(layout.loose_position("L").is_some());
    }
}
