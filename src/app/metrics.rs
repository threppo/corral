//! Read-only sampling of the desktop's icon spacing and size, and the Corral grid built on it
//! (grid-snapping-spec 2). Only values the engine already loaded from Explorer's view, or Windows'
//! public metrics, are read; nothing here changes a system preference.
use crate::geometry::Rect;
use crate::icon_metrics::{self as cells, CellInputs, CellMetrics, DesktopMetrics};
use super::{App, log};
use crate::view_state::{View, ColumnKey, DetailsColumns};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct HeaderCell { pub key: ColumnKey, pub rect: Rect }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HeaderHit { Cell(ColumnKey), Separator(ColumnKey) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Slot {
    pub icon: Rect,
    pub label: Rect,
    pub hit: Rect,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Nav { Previous, Next, First, Last, PageUp, PageDown, Stay }

pub(super) fn effective_body(mut body: Rect, minimum: (i32, i32)) -> Rect {
    body.right = body.left + body.width().max(minimum.0);
    body.bottom = body.top + body.height().max(minimum.1);
    body
}

/// A Corral's grid on its body: the one object painting, hit testing, selection bands, drops and
/// grab offsets read, so they cannot disagree (grid-snapping-spec 2.1). Coordinates are those of
/// `body` (group-window or parent-client pixels), `scroll_px` is the current vertical scroll.
#[derive(Clone, Copy, Debug)]
pub(super) struct Grid {
    pub m: CellMetrics,
    pub cols: i32,
    pub body: Rect,
    pub scroll_px: i32,
    pub view: View,
    pub dpi: u32,
    pub header: Rect,
    pub headers: [Option<HeaderCell>; 4],
}

impl Grid {
    pub fn new(m: CellMetrics, body: Rect, scroll_px: i32) -> Self {
        Self { m, cols: m.cols_for(body.width()), body, scroll_px: scroll_px.max(0), view: View::Icons, dpi: 96, header: Rect::default(), headers: [None; 4] }
    }

    pub fn for_view(view: View, mut m: CellMetrics, dpi: u32, body: Rect, scroll_px: i32) -> Self {
        if view == View::Icons { return Self { dpi, ..Self::new(m, body, scroll_px) }; }
        m.cell_w_px = body.width().max(0);
        m.cell_h_px = crate::geometry::to_px(24, dpi).max(1);
        m.pad_px = 0;
        m.label_lines = 1;
        let header = Rect::new(body.left, body.top, body.right,
            if view == View::Details { (body.top + crate::geometry::to_px(24, dpi)).min(body.bottom) } else { body.top });
        let rows = Rect { top: header.bottom, ..body };
        Self { m, cols: 1, body: rows, scroll_px: scroll_px.max(0), view, dpi, header, headers: [None; 4] }.with_columns(&DetailsColumns::default())
    }

    pub fn with_columns(mut self, columns: &DetailsColumns) -> Self {
        if self.view != View::Details { return self; }
        let px = |d| crate::geometry::to_px(d, self.dpi);
        let mut shown = columns.visible();
        let width_px = |d: i32| ((d as i64 * self.dpi as i64 + 48) / 96).min(i32::MAX as i64);
        while !shown.is_empty() && shown.iter().map(|(_, w)| width_px(*w)).sum::<i64>() + px(96) as i64 > self.body.width() as i64 { shown.pop(); }
        let name_w = (self.body.width() as i64 - shown.iter().map(|(_, w)| width_px(*w)).sum::<i64>()).max(0) as i32;
        let mut x = self.body.left;
        self.headers = [None; 4];
        self.headers[0] = Some(HeaderCell { key: ColumnKey::Name, rect: Rect::new(x, self.header.top, x + name_w, self.header.bottom) });
        x += name_w;
        for (i, (key, width)) in shown.into_iter().enumerate() {
            let right = (x + px(width)).min(self.body.right);
            self.headers[i + 1] = Some(HeaderCell { key, rect: Rect::new(x, self.header.top, right, self.header.bottom) });
            x = right;
        }
        self
    }
    pub fn header_hit(&self, p: (i32, i32)) -> Option<HeaderHit> {
        if self.view != View::Details || !self.header.contains(p.0, p.1) { return None; }
        let tolerance = crate::geometry::to_px(3, self.dpi);
        for (i, cell) in self.headers.iter().flatten().enumerate() {
            if cell.rect.right < self.header.right && (p.0 - cell.rect.right).abs() <= tolerance {
                // Fixed columns are anchored at the right; their left edges resize them.
                // This makes every optional column (including the last) independently resizable,
                // while Name absorbs the difference and never acquires a stored width.
                return Some(HeaderHit::Separator(self.headers[i + 1]?.key));
            }
        }
        self.headers.iter().flatten().find(|c| c.rect.contains(p.0, p.1)).map(|c| HeaderHit::Cell(c.key))
    }
    pub fn column_rect(&self, key: ColumnKey, index: usize) -> Option<Rect> {
        let cell = self.headers.iter().flatten().find(|c| c.key == key)?;
        let row = self.slot_rect(index);
        let inset = crate::geometry::to_px(8, self.dpi).min(cell.rect.width() / 2);
        Some(Rect::new(cell.rect.left + inset, row.top, cell.rect.right - inset, row.bottom))
    }

    pub fn is_list(&self) -> bool { self.view != View::Icons }

    pub fn slot(&self, index: usize) -> Slot {
        if !self.is_list() {
            let hit = self.slot_rect(index);
            let left = hit.left + (hit.width() - self.m.icon_px) / 2;
            let top = hit.top + crate::geometry::to_px(4, self.dpi);
            return Slot { hit, icon: Rect::new(left, top, left + self.m.icon_px, top + self.m.icon_px),
                label: Rect::new(hit.left + 2, hit.top + self.m.icon_px + crate::geometry::to_px(8, self.dpi), hit.right - 2, hit.bottom) };
        }
        let px = |d| crate::geometry::to_px(d, self.dpi);
        let y = self.body.top + (index.min(i32::MAX as usize) as i32).saturating_mul(self.m.cell_h_px) - self.scroll_px;
        let left = (self.body.left + px(8)).min(self.body.right);
        let name_right = self.headers[0].map_or(self.body.right, |c| c.rect.right);
        let right = (name_right - px(8)).max(left);
        let icon_right = (left + self.m.icon_px).min(right);
        let top = y + (self.m.cell_h_px - self.m.icon_px) / 2;
        let inset = px(2).min(self.body.width().max(0) / 2);
        Slot { icon: Rect::new(left, top, icon_right, top + self.m.icon_px),
            label: Rect::new((icon_right + px(8)).min(right), y, right, y + self.m.cell_h_px),
            hit: Rect::new(self.body.left + inset, y + px(2), self.body.right - inset, y + self.m.cell_h_px - px(2)) }
    }

    /// Where slot `index` is drawn (scrolled).
    pub fn slot_rect(&self, index: usize) -> Rect {
        if self.is_list() { return self.slot(index).hit; }
        self.m.cell_rect(index, self.cols).offset(self.body.left, self.body.top - self.scroll_px)
    }

    /// Slot under `point`; `None` in the padding or right of the last whole column.
    pub fn slot_at(&self, point: (i32, i32)) -> Option<usize> {
        if self.is_list() {
            if !self.body.contains(point.0, point.1) { return None; }
            let y = point.1 - self.body.top + self.scroll_px;
            if y < 0 { return None; }
            let index = (y / self.m.cell_h_px) as usize;
            return self.slot(index).hit.contains(point.0, point.1).then_some(index);
        }
        self.m.cell_at((point.0 - self.body.left, point.1 - self.body.top), self.cols, self.scroll_px)
    }

    pub fn hit_test(&self, point: (i32, i32), count: usize) -> Option<usize> {
        self.slot_at(point).filter(|i| *i < count)
    }

    pub fn key_slot(&self, current: Option<usize>, nav: Nav, count: usize) -> Option<usize> {
        if count == 0 { return None; }
        let at = current.unwrap_or(0).min(count - 1);
        let page = (self.body.height() / self.m.cell_h_px).max(1) as usize * self.cols as usize;
        Some(match nav {
            Nav::First => 0,
            Nav::Last => count - 1,
            Nav::Previous => at.saturating_sub(self.cols as usize),
            Nav::Next => if current.is_none() { 0 } else { at.saturating_add(self.cols as usize).min(count - 1) },
            Nav::PageUp => at.saturating_sub(page),
            Nav::PageDown => at.saturating_add(page).min(count - 1),
            Nav::Stay => at,
        })
    }

    /// Insertion slot for a drop at `point`.
    pub fn drop_slot(&self, point: (i32, i32), count: usize) -> usize {
        if self.is_list() {
            let y = (point.1 - self.body.top + self.scroll_px + self.m.cell_h_px / 2).max(0);
            return ((y / self.m.cell_h_px) as usize).min(count);
        }
        self.m.drop_slot((point.0 - self.body.left, point.1 - self.body.top), self.cols, self.scroll_px, count)
    }

    /// Largest scroll offset for `count` items.
    pub fn max_scroll_px(&self, count: usize) -> i32 {
        (self.content_px(count) - self.body.height()).max(0)
    }

    /// Content height including the padding.
    pub fn content_px(&self, count: usize) -> i32 {
        self.m.content_h_px(count, self.cols)
    }

    pub fn first_visible(&self, count: usize) -> usize {
        if count == 0 { return 0; }
        (((self.scroll_px - self.m.pad_px).max(0) / self.m.cell_h_px) as usize * self.cols as usize).min(count - 1)
    }

    pub fn scroll_to(&self, index: usize, count: usize) -> i32 {
        let r = self.slot_rect(index);
        let next = if r.top < self.body.top { self.scroll_px + r.top - self.body.top }
            else if r.bottom > self.body.bottom { self.scroll_px + r.bottom - self.body.bottom }
            else { self.scroll_px };
        next.clamp(0, self.max_scroll_px(count))
    }

    pub fn insertion_y(&self, index: usize) -> i32 {
        self.body.top + index as i32 * self.m.cell_h_px - self.scroll_px
    }
}

#[cfg(test)]
mod list_tests {
    use super::*;
    use crate::geometry::to_px;

    fn list(dpi: u32, width: i32, scroll: i32) -> Grid {
        let m = CellMetrics { cell_w_px: 75, cell_h_px: 88, icon_px: to_px(16, dpi), label_lines: 2, pad_px: 8 };
        Grid::for_view(View::List, m, dpi, Rect::new(10, 20, 10 + width, 20 + to_px(72, dpi)), scroll)
    }

    #[test]
    fn list_slot_rectangles_at_each_dpi() {
        for dpi in [96, 120, 144, 240] {
            let p = |d| to_px(d, dpi);
            let grid = list(dpi, p(200), 0);
            let slot = grid.slot(1);
            assert_eq!(slot.icon, Rect::new(10 + p(8), 20 + p(24) + (p(24) - p(16)) / 2,
                10 + p(8) + p(16), 20 + p(24) + (p(24) - p(16)) / 2 + p(16)));
            assert_eq!(slot.label, Rect::new(10 + p(8) + p(16) + p(8), 20 + p(24), 10 + p(200) - p(8), 20 + 2 * p(24)));
            assert_eq!(slot.hit, Rect::new(10 + p(2), 20 + p(24) + p(2), 10 + p(200) - p(2), 20 + 2 * p(24) - p(2)));
        }
    }

    #[test]
    fn list_is_one_column_and_all_rectangles_fit_even_tiny_bodies() {
        for dpi in [96, 120, 144, 240] {
            for width in [0, 1, 12, 40, 200, 2000] {
                let grid = list(dpi, width, 0);
                assert_eq!(grid.cols, 1);
                for i in 0..5 {
                    let slot = grid.slot(i);
                    for r in [slot.hit, slot.icon, slot.label] {
                        assert!(r.left >= grid.body.left && r.right <= grid.body.right && r.width() >= 0, "{dpi} {width}: {r:?}");
                    }
                    assert_eq!(slot.icon.left, grid.slot(0).icon.left);
                    assert_eq!(grid.slot(i + 1).label.top - slot.label.top, to_px(24, dpi));
                }
            }
        }
    }

    #[test]
    fn hit_band_drop_and_scroll_share_the_slots() {
        for dpi in [96, 120, 144, 240] {
            let grid = list(dpi, to_px(200, dpi), to_px(24, dpi));
            for i in 1..4 {
                let r = grid.slot(i).hit;
                let point = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
                assert_eq!(grid.slot_at(point), Some(i));
                assert_eq!(grid.slot_rect(i), r);
                assert_eq!(grid.slot_at((grid.body.left, point.1)), None);
                assert_eq!(grid.slot_at((point.0, grid.insertion_y(i))), None);
                assert_eq!(grid.drop_slot((point.0, grid.insertion_y(i)), 10), i);
            }
            assert_eq!(grid.slot_at((11, grid.body.bottom)), None);
            assert_eq!(grid.content_px(10), 10 * to_px(24, dpi));
            assert_eq!(grid.max_scroll_px(10), 7 * to_px(24, dpi));
            assert_eq!(grid.first_visible(10), 1);
            assert_eq!(grid.drop_slot((0, 100000), 10), 10);
            assert_eq!(grid.hit_test((grid.body.left + to_px(30, dpi), grid.slot(2).hit.top), 2), None);
            let next = grid.scroll_to(9, 10);
            let moved = Grid { scroll_px: next, ..grid };
            assert!(moved.slot_rect(9).bottom <= moved.body.bottom);
            assert_eq!(moved.scroll_to(0, 10), to_px(2, dpi));
        }
    }

    #[test]
    fn list_keys_follow_rows_and_the_visible_page() {
        for dpi in [96, 120, 144, 240] {
            let grid = list(dpi, 1000, 0);
            assert_eq!(grid.key_slot(None, Nav::Next, 10), Some(0));
            assert_eq!(grid.key_slot(Some(4), Nav::Next, 10), Some(5));
            assert_eq!(grid.key_slot(Some(4), Nav::Previous, 10), Some(3));
            assert_eq!(grid.key_slot(Some(0), Nav::Previous, 10), Some(0));
            assert_eq!(grid.key_slot(Some(9), Nav::Next, 10), Some(9));
            assert_eq!(grid.key_slot(Some(4), Nav::PageDown, 10), Some(7));
            assert_eq!(grid.key_slot(Some(4), Nav::PageUp, 10), Some(1));
            assert_eq!(grid.key_slot(Some(4), Nav::First, 10), Some(0));
            assert_eq!(grid.key_slot(Some(4), Nav::Last, 10), Some(9));
            assert_eq!(grid.key_slot(None, Nav::Next, 0), None);
        }
    }

    #[test]
    fn details_adds_header_and_icons_are_unchanged() {
        let grid = list(144, 500, 30);
        let details = Grid::for_view(View::Details, grid.m, 144, grid.body, 30);
        for i in 0..5 { assert_eq!(grid.slot(i).hit.offset(0, crate::geometry::to_px(24, 144)), details.slot(i).hit); }
        let icons = Grid::new(grid.m, grid.body, 30);
        let through_view = Grid::for_view(View::Icons, grid.m, 144, grid.body, 30);
        for i in 0..5 { assert_eq!(icons.slot_rect(i), through_view.slot_rect(i)); }
    }
}

impl App {
    /// Called before consumers: startup/settings, item reload, DPI notifications, and again after
    /// profile settlement. Re-reads the desktop view's spacing and icon size (a failed read keeps the
    /// cached value) and converts the pixels with the view's own DPI, not the first monitor's. True when
    /// the cell inputs changed (the caller then reloads items).
    pub(super) fn refresh_icon_metrics(&mut self) -> bool {
        // Fixture previews must never sample the user's Desktop.
        let fixture = self.opts.mode == super::Mode::Preview && std::env::var_os("CORRAL_QA_ITEMS_DIR").is_some();
        let sample_dpi = self.desktop_sample_dpi();
        let view = if fixture {
            (self.spacing.0 > 0 && self.spacing.1 > 0).then(|| (self.spacing, cells::px_to_dip(self.icon_px, sample_dpi).round() as i32))
        } else {
            crate::desktop_view::read_spacing_and_icon_size().ok()
        };
        let next = cells::desktop_metrics_from_view(self.desktop_metrics, view, sample_dpi, win32::system_spacing_dip());
        let changed = next != self.desktop_metrics;
        if changed {
            log(&format!("icon metrics: {next:?} (was {:?})", self.desktop_metrics));
        }
        if next.windows_spacing_dip.is_none() {
            win32::log_unavailable_once();
        }
        self.desktop_metrics = next;
        // Geometry, scroll limits and drawing must use the same new cell. Keep the model's expanded
        // rectangle (especially rolled groups) intact.
        self.clamp_metric_scrolls();
        changed
    }

    /// DPI of the desktop view's pixel values: the view window's DPI, else the primary monitor's.
    fn desktop_sample_dpi(&self) -> u32 {
        use windows::Win32::UI::HiDpi::GetDpiForWindow;
        let host = self.host.map(|h| unsafe { GetDpiForWindow(h.listview) });
        let monitors: Vec<_> = self.monitors.iter().map(|m| (m.bounds, m.dpi)).collect();
        cells::desktop_sample_dpi(host, &monitors, self.dpi())
    }

    pub(super) fn cell_inputs(&self, dpi: u32, zoom_pct: u32, match_desktop: bool) -> CellInputs {
        let DesktopMetrics { windows_spacing_dip, icon_dip, .. } = self.desktop_metrics;
        CellInputs { dpi, zoom_pct, windows_spacing_dip, desktop_icon_dip: icon_dip, match_desktop }
    }

    /// Cell of the loose desktop icons: Explorer's own pitch at the primary DPI.
    pub(super) fn loose_cell(&self) -> (i32, i32) {
        let dpi = if self.desktop_metrics.sample_dpi > 0 { self.desktop_metrics.sample_dpi } else { self.dpi() };
        let m = cells::cell_metrics(&self.cell_inputs(dpi, cells::ZOOM_DEFAULT, true));
        (m.cell_w_px, m.cell_h_px)
    }

    /// The metrics of Corral `id` at `dpi`: its zoom and the "match the desktop spacing" setting.
    pub(super) fn group_cells(&self, id: u64, dpi: u32) -> CellMetrics {
        cells::cell_metrics(&self.cell_inputs(dpi, self.group_zoom_percent(id), self.settings.group_spacing_matches_desktop))
    }

    /// Pitch of Corral `id` at `dpi` (for callers that only need the cell size).
    pub(super) fn group_cell(&self, id: u64, dpi: u32) -> (i32, i32) {
        let m = self.group_cells(id, dpi);
        (m.cell_w_px, m.cell_h_px)
    }

    /// The grid of Corral `id` on `body`, scrolled by `scroll_dip` (the model's unit).
    pub(super) fn group_grid(&self, id: u64, dpi: u32, body: Rect, scroll_dip: i32) -> Grid {
        let view = self.group_view(id);
        let mut m = self.group_cells(id, dpi);
        if view != View::Icons { m.icon_px = self.small_icon_px(dpi); }
        Grid::for_view(view, m, dpi, body, crate::geometry::to_px(scroll_dip, dpi)).with_columns(&self.group_columns(id))
    }

    fn small_icon_px(&self, dpi: u32) -> i32 {
        use windows::Win32::UI::{HiDpi::GetSystemMetricsForDpi, WindowsAndMessaging::SM_CXSMICON};
        let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi) };
        if size > 0 { size } else { crate::geometry::to_px(16, dpi).max(1) }
    }

    /// The governing monitor's DPI of Corral `id`: the interaction centre DPI wins; at rest the
    /// effective-body centre DPI that `group_geometry` resolves is used, so a straddling group keeps
    /// its centre's metrics after release. The stored identity monitor's DPI is the last fallback.
    pub(super) fn group_metrics_dpi(&self, id: u64) -> u32 {
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id) else { return self.dpi() };
        let identity = self.monitor_for(&group.monitor).map_or(self.dpi(), |m| m.dpi);
        let resolved = self.requested_body(group).and_then(|r| self.placement_for(id, r)).map(|(p, _)| p.effective_dpi);
        self.metric_dpi(id, resolved, identity)
    }

    pub(super) fn group_dpi(&self, id: u64) -> u32 {
        self.group_metrics_dpi(id)
    }

    /// Icon zoom of an ordinary Corral in percent (`icon_zoom.rs`); 100 for Gates (their Explorer view
    /// zooms itself) and unknown groups.
    pub(super) fn group_zoom_percent(&self, id: u64) -> u32 {
        self.layout.groups.iter().find(|g| g.id == id)
            .filter(|g| matches!(g.content, crate::model::GroupContent::Items { .. }))
            .map_or(crate::icon_zoom::DEFAULT, |g| crate::icon_zoom::percent(g.icon_zoom))
    }

    /// Icon size of Corral `id` at `dpi` in device pixels.
    pub(super) fn group_icon_px_of(&self, id: u64, dpi: u32) -> u32 {
        if self.group_view(id) != View::Icons { return self.small_icon_px(dpi) as u32; }
        self.group_cells(id, dpi).icon_px as u32
    }

    /// Smallest body of a Corral at `dpi` (grid-snapping-spec 3.3), lowered to `stored_px` on an axis
    /// where the stored body is already smaller: loading and fitting never force a Corral larger.
    pub(super) fn min_body_px(&self, dpi: u32, stored_px: Option<(i32, i32)>) -> (i32, i32) {
        let min = crate::snap::min_body_px(dpi);
        stored_px.map_or(min, |(w, h)| (min.0.min(w.max(1)), min.1.min(h.max(1))))
    }

    pub(super) fn clamp_metric_scrolls(&mut self) {
        let updates: Vec<_> = self.layout.groups.iter().filter_map(|g| {
            if !matches!(g.content, crate::model::GroupContent::Items { .. }) { return None; }
            let geo = self.group_geometry(g);
            let grid = self.group_grid(g.id, geo.dpi, geo.full_body, 0);
            let value = clamp_scroll(g.scroll, geo.dpi, self.group_members(g).len(), &grid);
            (value != g.scroll).then_some((g.id, value))
        }).collect();
        for (id, value) in updates { if let Some(g) = self.layout.group_mut(id) { g.scroll = value; } }
    }
}

fn clamp_scroll(scroll: i32, dpi: u32, count: usize, grid: &Grid) -> i32 {
    crate::geometry::to_dip(crate::geometry::to_px(scroll, dpi).clamp(0, grid.max_scroll_px(count)), dpi)
}

mod win32 {
    use std::sync::atomic::{AtomicBool, Ordering};
    use windows::Win32::UI::HiDpi::{GetDpiForSystem, GetSystemMetricsForDpi};
    use windows::Win32::UI::WindowsAndMessaging::{ICONMETRICSW, SM_CXICONSPACING, SM_CYICONSPACING, SPI_GETICONMETRICS, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW};

    /// Windows' icon spacing in DIPs (grid-snapping-spec 2.2 step 1): `GetSystemMetricsForDpi` at the
    /// system DPI, else `SPI_GETICONMETRICS`; an unusable value (zero, outside 40..160 DIP) is `None`.
    pub(super) fn system_spacing_dip() -> Option<(f64, f64)> {
        unsafe {
            let dpi = GetDpiForSystem();
            if dpi == 0 {
                return None;
            }
            let (mut cx, mut cy) = (GetSystemMetricsForDpi(SM_CXICONSPACING, dpi), GetSystemMetricsForDpi(SM_CYICONSPACING, dpi));
            if cx <= 0 || cy <= 0 {
                let mut metrics = ICONMETRICSW { cbSize: std::mem::size_of::<ICONMETRICSW>() as u32, ..Default::default() };
                if SystemParametersInfoW(SPI_GETICONMETRICS, metrics.cbSize, Some((&mut metrics as *mut ICONMETRICSW).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)).is_ok() {
                    (cx, cy) = (metrics.iHorzSpacing, metrics.iVertSpacing);
                }
            }
            let scale = f64::from(dpi) / 96.0;
            super::cells::spacing_from_api(f64::from(cx) / scale, f64::from(cy) / scale)
        }
    }

    static LOGGED: AtomicBool = AtomicBool::new(false);

    /// The fallback is logged once per run (grid-snapping-spec 2.3).
    pub(super) fn log_unavailable_once() {
        if !LOGGED.swap(true, Ordering::Relaxed) {
            super::log("icon metrics: Windows icon spacing unavailable; using the 75 x 75 DIP default");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::icon_metrics::{cell_metrics, CellInputs};

    fn grid(dpi: u32, body: Rect, scroll: i32) -> Grid {
        let m = cell_metrics(&CellInputs { dpi, zoom_pct: 100, windows_spacing_dip: Some((75.0, 75.0)), desktop_icon_dip: Some(48.0), match_desktop: true });
        Grid::new(m, body, scroll)
    }

    #[test]
    fn new_cells_clamp_scroll_against_the_full_body_without_mutating_it() {
        let body = Rect::new(0, 0, 262, 202);
        let g = grid(96, body, 0);
        let rows = g.m.rows_for(40, g.cols);
        let max = rows * g.m.cell_h_px + 2 * g.m.pad_px - 202;
        assert_eq!(clamp_scroll(100_000, 96, 40, &g), max);
        assert_eq!(clamp_scroll(10, 96, 2, &g), 0, "everything fits: no scrolling");
        assert_eq!(body.height(), 202);
    }

    #[test]
    fn grid_slots_hit_draw_and_drop_alike() {
        let body = Rect::new(40, 30, 440, 330);
        for scroll in [0, 17, 88] {
            let g = grid(96, body, scroll);
            for i in 0..30 {
                let r = g.slot_rect(i);
                let centre = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
                if centre.1 < body.top || centre.1 >= body.bottom { continue; }
                assert_eq!(g.slot_at(centre), Some(i), "scroll {scroll} slot {i}");
                assert_eq!(g.drop_slot(centre, 30), i);
            }
            assert_eq!(g.slot_at((body.left + 2, body.top + 2)), None, "the padding is not a cell");
        }
    }

    #[test]
    fn effective_minimum_keeps_expanded_geometry_and_origin() {
        let stored = Rect::new(-10, 20, 80, 60);
        let effective = effective_body(stored, (100, 80));
        assert_eq!(effective, Rect::new(-10, 20, 90, 100));
        assert_eq!(effective_body(effective, (60, 50)), effective);
        assert_eq!(stored, Rect::new(-10, 20, 80, 60));
    }
}


#[cfg(test)]
mod details_tests {
    use super::*;
    use crate::geometry::to_px;
    use serde_json::json;
    fn grid(dpi: u32, width: i32, scroll: i32, columns: &DetailsColumns) -> Grid {
        let m = CellMetrics { cell_w_px: 100, cell_h_px: 100, icon_px: to_px(16, dpi), label_lines: 2, pad_px: 8 };
        Grid::for_view(View::Details, m, dpi, Rect::new(10, 20, 10 + width, 20 + to_px(120, dpi)), scroll).with_columns(columns)
    }
    #[test]
    fn pinned_header_slots_scroll_and_hits_at_each_dpi() {
        for dpi in [96, 120, 144, 240] {
            let p = |d| to_px(d, dpi);
            let g = grid(dpi, p(300), 0, &DetailsColumns::default());
            assert_eq!(g.header, Rect::new(10, 20, 10 + p(300), 20 + p(24)));
            assert_eq!(g.body, Rect::new(10, 20 + p(24), 10 + p(300), 20 + p(120)));
            assert_eq!(g.slot(0).icon, Rect::new(10 + p(8), g.body.top + (p(24) - p(16)) / 2,
                10 + p(8) + p(16), g.body.top + (p(24) - p(16)) / 2 + p(16)));
            assert_eq!(g.slot(0).label, Rect::new(10 + p(8) + p(16) + p(8), g.body.top,
                10 + p(300) - p(80) - p(8), g.body.top + p(24)));
            assert_eq!(g.slot(0).hit, Rect::new(10 + p(2), g.body.top + p(2), 10 + p(300) - p(2), g.body.top + p(24) - p(2)));
            let moved = grid(dpi, p(300), p(48), &DetailsColumns::default());
            assert_eq!(g.header, moved.header);
            assert_eq!(g.headers, moved.headers);
            assert_eq!(g.slot(0).hit, moved.slot(2).hit);
            let row = moved.slot(2).hit;
            assert_eq!(moved.hit_test((row.left + 2, row.top + 2), 10), Some(2));
            assert_eq!(moved.hit_test((row.left, moved.header.top + 1), 10), None);
            assert_eq!(moved.hit_test((row.left, moved.body.top), 10), None);
            assert_eq!(moved.max_scroll_px(10), p(240) - p(96));
            assert_eq!(moved.key_slot(Some(0), Nav::PageDown, 10), Some(4));
            let next = Grid { scroll_px: moved.scroll_to(9, 10), ..moved };
            assert!(next.slot(9).hit.bottom <= next.body.bottom);
        }
    }
    #[test]
    fn narrow_hides_only_rightmost_columns_and_widening_restores_without_mutation() {
        let columns = DetailsColumns::parse(Some(&json!([{"key":"size","width":80},{"key":"type","width":80},{"key":"date_modified","width":160}])));
        let before = columns.clone();
        for dpi in [96, 120, 144, 240] {
            for (width, count) in [(416,4),(415,3),(256,3),(255,2),(176,2),(175,1),(96,1),(0,1)] {
                let g = grid(dpi, to_px(width,dpi), 0, &columns);
                assert_eq!(g.headers.iter().flatten().count(), count, "{dpi} {width}");
                if width >= 96 { assert!(g.headers[0].unwrap().rect.width() >= to_px(96,dpi)); }
                for i in 0..3 {
                    let slot = g.slot(i);
                    for r in [slot.icon, slot.label, slot.hit] { assert!(r.left >= g.body.left && r.right <= g.body.right && r.width() >= 0); }
                }
            }
            assert_eq!(grid(dpi, to_px(500,dpi), 0, &columns).headers.iter().flatten().count(), 4);
        }
        assert_eq!(columns, before);
    }
    #[test]
    fn header_cells_separators_and_nothing_share_column_rectangles() {
        for dpi in [96,120,144,240] {
            let p = |d| to_px(d,dpi);
            let g = grid(dpi,p(300),p(72),&DetailsColumns::default());
            let name = g.headers[0].unwrap().rect;
            let size = g.headers[1].unwrap().rect;
            let y = g.header.top + p(12);
            assert_eq!(g.header_hit((name.left+p(20),y)),Some(HeaderHit::Cell(ColumnKey::Name)));
            assert_eq!(g.header_hit((size.left+p(20),y)),Some(HeaderHit::Cell(ColumnKey::Size)));
            assert_eq!(g.header_hit((name.right,y)),Some(HeaderHit::Separator(ColumnKey::Size)));
            for point in [(name.left-1,y),(size.right,y),(name.left,g.header.top-1),(name.left,g.header.bottom)] { assert_eq!(g.header_hit(point),None); }
        }
    }
}

#[cfg(test)]
mod separator_resize_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn every_optional_column_has_a_resize_separator_and_live_width_does_not_mutate_storage() {
        let stored=DetailsColumns::parse(Some(&json!([{"key":"size","width":80},{"key":"type","width":90},{"key":"date_modified","width":160}])));
        for dpi in [96,120,144,240] {
            let m=CellMetrics { cell_w_px:100,cell_h_px:100,icon_px:crate::geometry::to_px(16,dpi),label_lines:2,pad_px:8 };
            let g=Grid::for_view(View::Details,m,dpi,Rect::new(0,0,crate::geometry::to_px(600,dpi),crate::geometry::to_px(200,dpi)),0).with_columns(&stored);
            for cell in g.headers.iter().flatten().skip(1) {
                assert_eq!(g.header_hit((cell.rect.left,cell.rect.top+2)),Some(HeaderHit::Separator(cell.key)));
                let mut live=stored.clone();
                live.resize(cell.key,10);
                assert_eq!(live.visible().iter().find(|(k,_)| *k==cell.key).unwrap().1,40);
                let resized=g.with_columns(&live);
                assert_eq!(resized.headers.iter().flatten().find(|c|c.key==cell.key).unwrap().rect.width(),crate::geometry::to_px(40,dpi));
            }
        }
        assert_eq!(stored.visible(),vec![(ColumnKey::Size,80),(ColumnKey::Type,90),(ColumnKey::DateModified,160)]);
    }
}
