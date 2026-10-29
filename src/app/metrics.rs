//! Read-only sampling of the desktop's icon spacing and size, and the Corral grid built on it
//! (grid-snapping-spec 2). Only values the engine already loaded from Explorer's view, or Windows'
//! public metrics, are read; nothing here changes a system preference.
use crate::geometry::Rect;
use crate::icon_metrics::{self as cells, CellInputs, CellMetrics, DesktopMetrics};
use super::{App, log};

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
}

impl Grid {
    pub fn new(m: CellMetrics, body: Rect, scroll_px: i32) -> Self {
        Self { m, cols: m.cols_for(body.width()), body, scroll_px: scroll_px.max(0) }
    }

    /// Where slot `index` is drawn (scrolled).
    pub fn slot_rect(&self, index: usize) -> Rect {
        self.m.cell_rect(index, self.cols).offset(self.body.left, self.body.top - self.scroll_px)
    }

    /// Slot under `point`; `None` in the padding or right of the last whole column.
    pub fn slot_at(&self, point: (i32, i32)) -> Option<usize> {
        self.m.cell_at((point.0 - self.body.left, point.1 - self.body.top), self.cols, self.scroll_px)
    }

    /// Insertion slot for a drop at `point`.
    pub fn drop_slot(&self, point: (i32, i32), count: usize) -> usize {
        self.m.drop_slot((point.0 - self.body.left, point.1 - self.body.top), self.cols, self.scroll_px, count)
    }

    /// Largest scroll offset for `count` items.
    pub fn max_scroll_px(&self, count: usize) -> i32 {
        self.m.max_scroll_px(count, self.cols, self.body.height())
    }

    /// Content height including the padding.
    pub fn content_px(&self, count: usize) -> i32 {
        self.m.content_h_px(count, self.cols)
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
        Grid::new(self.group_cells(id, dpi), body, crate::geometry::to_px(scroll_dip, dpi))
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
