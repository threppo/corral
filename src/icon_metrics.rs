//! The one cell policy of a Corral (grid-snapping-spec 2). Painting, hit testing, drop slots, scroll
//! limits, zoom and whole-cell sizing all take their numbers from [`cell_metrics`]; nobody multiplies a
//! spacing constant on their own. Pure: no window, no globals. All sizes are DIPs (1/96 inch) first and
//! are rounded to device pixels once, half-up (`floor(x + 0.5)`, also for negative values).

use crate::geometry::Rect;

/// Windows' stock icon spacing, used when the real value is unknown (grid-snapping-spec 2.3).
pub const DEFAULT_SPACING_DIP: f64 = 75.0;
/// Range the Windows spacing API may report; outside it the default replaces that axis (spec 2.3).
pub const SPACING_API_RANGE_DIP: (f64, f64) = (40.0, 160.0);
/// Upper sanity bound for a spacing read from Explorer's own view (very large desktop icons).
const SPACING_SANE_MAX_DIP: f64 = 1000.0;
/// Edge of Windows' default (medium) desktop icons (spec 2.2 step 2).
pub const DEFAULT_ICON_DIP: f64 = 48.0;
/// Label lines every cell reserves and the height of one line (labels do not follow zoom).
pub const LABEL_LINES: i32 = 2;
pub const LABEL_LINE_H_DIP: f64 = 16.0;
/// Space above the icon and between icon and label (spec 2.2 step 3).
pub const CELL_MARGIN_DIP: f64 = 4.0;
/// Body padding on all four sides (spec 2.2 step 6).
pub const PAD_DIP: f64 = 8.0;
/// Zoom range in percent (spec 2.2 step 7).
pub const ZOOM_MIN: u32 = 50;
pub const ZOOM_MAX: u32 = 300;
pub const ZOOM_DEFAULT: u32 = 100;

/// Round half up: `floor(x + 0.5)`, so a result never depends on where the origin is.
pub fn round_half_up(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

/// Pixels to DIPs with the exact ratio `px * 96 / dpi` (grid-snapping-spec 8).
pub fn px_to_dip(px: i32, dpi: u32) -> f64 {
    f64::from(px) * 96.0 / f64::from(dpi.max(1))
}

/// DIPs to pixels, round-half-up (grid-snapping-spec 8).
pub fn dip_to_px(dip: f64, dpi: u32) -> i32 {
    round_half_up(dip * f64::from(dpi.max(1)) / 96.0).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Windows' own spacing from `GetSystemMetricsForDpi(SM_CXICONSPACING / SM_CYICONSPACING)` or
/// `SPI_GETICONMETRICS`, already divided into DIPs: an axis that is zero, not finite or outside
/// 40..160 DIP is replaced by the default (spec 2.3). `None` when both axes had to be replaced.
pub fn spacing_from_api(horizontal_dip: f64, vertical_dip: f64) -> Option<(f64, f64)> {
    let ok = |v: f64| v.is_finite() && v >= SPACING_API_RANGE_DIP.0 && v <= SPACING_API_RANGE_DIP.1;
    if !ok(horizontal_dip) && !ok(vertical_dip) {
        return None;
    }
    let axis = |v: f64| if ok(v) { v } else { DEFAULT_SPACING_DIP };
    Some((axis(horizontal_dip), axis(vertical_dip)))
}

/// What the cell depends on. Recompute on DPI change, zoom change, setting change and
/// `WM_SETTINGCHANGE`; the value is the cache key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellInputs {
    pub dpi: u32,
    /// Icon zoom in percent; clamped to 50..300.
    pub zoom_pct: u32,
    /// Windows' icon spacing in DIPs, if known.
    pub windows_spacing_dip: Option<(f64, f64)>,
    /// The desktop icon edge in DIPs, if the engine knows it (else 48).
    pub desktop_icon_dip: Option<f64>,
    /// `GroupSpacingMatchesDesktop`.
    pub match_desktop: bool,
}

/// What the engine has read about the desktop (read only, grid-snapping-spec 2.2 steps 1 and 2).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DesktopMetrics {
    pub windows_spacing_dip: Option<(f64, f64)>,
    pub icon_dip: Option<f64>,
    /// DPI the desktop view's pixel values were measured at (0 = unknown); the loose icons live in it.
    pub sample_dpi: u32,
}

/// DPI Explorer's desktop view reports its pixels in: the view window's own DPI, else the primary
/// monitor's (the one containing the virtual-screen origin), else `fallback`. Never the first entry of
/// the monitor list, which need not be the primary one.
pub fn desktop_sample_dpi(host_dpi: Option<u32>, monitors: &[(Rect, u32)], fallback: u32) -> u32 {
    host_dpi.filter(|d| *d > 0)
        .or_else(|| monitors.iter().find(|(bounds, _)| bounds.contains(0, 0)).map(|(_, d)| *d).filter(|d| *d > 0))
        .unwrap_or(fallback)
}

/// New [`DesktopMetrics`] from one read of the desktop view: its spacing in px at `sample_dpi` and its
/// icon size in DIPs. A failed read (`None`) keeps the previous value; with nothing cached the Windows
/// API value (already in DIPs) is used (grid-snapping-spec 2.2, 2.3).
pub fn desktop_metrics_from_view(previous: DesktopMetrics, view: Option<((i32, i32), i32)>, sample_dpi: u32, api_dip: Option<(f64, f64)>) -> DesktopMetrics {
    let mut next = DesktopMetrics { sample_dpi, ..previous };
    if let Some((spacing, icon_dip)) = view {
        next.windows_spacing_dip = (spacing.0 > 0 && spacing.1 > 0).then(|| (px_to_dip(spacing.0, sample_dpi), px_to_dip(spacing.1, sample_dpi)));
        next.icon_dip = (icon_dip > 0).then(|| f64::from(icon_dip));
    }
    if next.windows_spacing_dip.is_none() {
        next.windows_spacing_dip = api_dip;
    }
    if previous.sample_dpi != 0 && view.is_none() {
        next.sample_dpi = previous.sample_dpi;
    }
    next
}

/// Result of [`cell_metrics`]; every consumer of a Corral grid reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellMetrics {
    pub cell_w_px: i32,
    pub cell_h_px: i32,
    pub icon_px: i32,
    pub label_lines: i32,
    pub pad_px: i32,
}

/// The single cell function (grid-snapping-spec 2.2). Never panics, never returns a zero size.
pub fn cell_metrics(inputs: &CellInputs) -> CellMetrics {
    let dpi = if inputs.dpi == 0 { 96 } else { inputs.dpi };
    let zoom = f64::from(inputs.zoom_pct.clamp(ZOOM_MIN, ZOOM_MAX));
    let icon_edge = inputs.desktop_icon_dip.filter(|v| v.is_finite() && *v > 0.0).unwrap_or(DEFAULT_ICON_DIP);
    let icon_dip = icon_edge * zoom / 100.0;
    // Content minimum: an icon and its two label lines never overlap the next cell.
    let min_w = (icon_dip + 16.0).max(64.0);
    let min_h = icon_dip + CELL_MARGIN_DIP + f64::from(LABEL_LINES) * LABEL_LINE_H_DIP + CELL_MARGIN_DIP;
    let axis = |v: f64| if v.is_finite() && v > 0.0 { v.min(SPACING_SANE_MAX_DIP) } else { DEFAULT_SPACING_DIP };
    let (win_w, win_h) = inputs.windows_spacing_dip.map_or((DEFAULT_SPACING_DIP, DEFAULT_SPACING_DIP), |(w, h)| (axis(w), axis(h)));
    let (w, h) = if inputs.match_desktop {
        ((win_w * zoom / 100.0).max(min_w), (win_h * zoom / 100.0).max(min_h))
    } else {
        (min_w, min_h)
    };
    CellMetrics {
        cell_w_px: dip_to_px(w, dpi).max(1),
        cell_h_px: dip_to_px(h, dpi).max(1),
        icon_px: dip_to_px(icon_dip, dpi).max(1),
        label_lines: LABEL_LINES,
        pad_px: dip_to_px(PAD_DIP, dpi).max(1),
    }
}

impl CellMetrics {
    /// Columns that fit a body of `body_w_px` (padding on both sides is taken off); at least 1. A column
    /// that misses only by the rounding of its cells (half a px each) and of the padding still counts, so a
    /// Corral exactly N cells wide at one DPI keeps N columns at another (live 2026-10-28: 4 -> 3 at 250 %).
    pub fn cols_for(&self, body_w_px: i32) -> i32 {
        let (inner, cell) = (body_w_px - 2 * self.pad_px, self.cell_w_px.max(1));
        let cols = inner / cell;
        let short = (cols + 1) * cell - inner;
        (if short <= (cols + 1) / 2 + 1 { cols + 1 } else { cols }).max(1)
    }

    /// Rows `count` items need in `cols` columns.
    pub fn rows_for(&self, count: usize, cols: i32) -> i32 {
        let cols = cols.max(1) as usize;
        count.div_ceil(cols).min(i32::MAX as usize) as i32
    }

    /// Rectangle of slot `index`, relative to the body's top-left, before scrolling.
    pub fn cell_rect(&self, index: usize, cols: i32) -> Rect {
        let cols = cols.max(1) as usize;
        let (col, row) = ((index % cols) as i32, (index / cols).min(i32::MAX as usize / 2) as i32);
        let x = self.pad_px + col * self.cell_w_px;
        let y = self.pad_px + row * self.cell_h_px;
        Rect::new(x, y, x + self.cell_w_px, y + self.cell_h_px)
    }

    /// Slot under `point` (relative to the body's top-left, scrolled by `scroll_px`); `None` in the
    /// padding or right of the last whole column. The slot may be past the last item.
    pub fn cell_at(&self, point: (i32, i32), cols: i32, scroll_px: i32) -> Option<usize> {
        let cols = cols.max(1);
        let (x, y) = (point.0 - self.pad_px, point.1 - self.pad_px + scroll_px.max(0));
        if x < 0 || y < 0 || x >= cols * self.cell_w_px {
            return None;
        }
        Some(((y / self.cell_h_px) * cols + x / self.cell_w_px) as usize)
    }

    /// Insertion slot for a drop at `point`: the nearest slot, never past `count` (the end).
    pub fn drop_slot(&self, point: (i32, i32), cols: i32, scroll_px: i32, count: usize) -> usize {
        let cols = cols.max(1);
        let x = (point.0 - self.pad_px).clamp(0, cols * self.cell_w_px - 1);
        let y = (point.1 - self.pad_px + scroll_px.max(0)).max(0);
        (((y / self.cell_h_px) * cols + x / self.cell_w_px) as usize).min(count)
    }

    /// Content height: all rows plus the padding above and below.
    pub fn content_h_px(&self, count: usize, cols: i32) -> i32 {
        self.rows_for(count, cols).saturating_mul(self.cell_h_px).saturating_add(2 * self.pad_px)
    }

    /// Largest scroll offset: 0 when all rows fit the `view_h_px` tall body.
    pub fn max_scroll_px(&self, count: usize, cols: i32, view_h_px: i32) -> i32 {
        (self.content_h_px(count, cols) - view_h_px).max(0)
    }

    /// Body size that holds exactly `cols x rows` cells.
    pub fn body_size_for(&self, cols: i32, rows: i32) -> (i32, i32) {
        (cols * self.cell_w_px + 2 * self.pad_px, rows * self.cell_h_px + 2 * self.pad_px)
    }

    /// Whole cells that fit a body of this size (inverse of [`Self::body_size_for`]; may be 0).
    pub fn cells_for_size(&self, body_w_px: i32, body_h_px: i32) -> (i32, i32) {
        (((body_w_px - 2 * self.pad_px) / self.cell_w_px).max(0), ((body_h_px - 2 * self.pad_px) / self.cell_h_px).max(0))
    }

    /// Scroll offset after a zoom: the item at the top of the view stays at the top (grid-snapping-spec
    /// 2.2). `old` is the metrics before the zoom, `self` the metrics after it.
    pub fn scroll_after_zoom(&self, old: &CellMetrics, old_cols: i32, new_cols: i32, scroll_px: i32, count: usize, view_h_px: i32) -> i32 {
        let first = i64::from(scroll_px.max(0) / old.cell_h_px.max(1)) * i64::from(old_cols.max(1));
        let row = first / i64::from(new_cols.max(1));
        let scroll = (row * i64::from(self.cell_h_px)).min(i64::from(i32::MAX)) as i32;
        scroll.clamp(0, self.max_scroll_px(count, new_cols, view_h_px))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DPIS: [u32; 5] = [96, 120, 144, 192, 240];

    fn inputs(dpi: u32, zoom: u32, win: Option<(f64, f64)>, matched: bool) -> CellInputs {
        CellInputs { dpi, zoom_pct: zoom, windows_spacing_dip: win, desktop_icon_dip: Some(48.0), match_desktop: matched }
    }

    #[test]
    fn spec_9_1_cell_widths_at_every_dpi_and_zoom() {
        let win = Some((75.0, 75.0));
        let widths: Vec<i32> = DPIS.iter().map(|d| cell_metrics(&inputs(*d, 100, win, true)).cell_w_px).collect();
        assert_eq!(widths, [75, 94, 113, 150, 188]);
        let zoomed: Vec<i32> = DPIS.iter().map(|d| cell_metrics(&inputs(*d, 150, win, true)).cell_w_px).collect();
        assert_eq!(zoomed, [113, 141, 169, 225, 281]);
    }

    #[test]
    fn spec_9_1_compact_cell_tight_and_missing_metrics() {
        let compact = cell_metrics(&inputs(96, 100, Some((75.0, 75.0)), false));
        assert_eq!((compact.cell_w_px, compact.cell_h_px), (64, 88));
        let tight = cell_metrics(&inputs(96, 100, Some((40.0, 40.0)), true));
        assert_eq!((tight.cell_w_px, tight.cell_h_px), (64, 88), "tight Windows spacing is raised to the content minimum");
        for missing in [None, Some((0.0, 0.0)), Some((f64::NAN, -3.0))] {
            let m = cell_metrics(&inputs(96, 100, missing, true));
            assert_eq!((m.cell_w_px, m.cell_h_px), (75, 88), "{missing:?}: 75 DIP axes, then the content minimum for the height");
        }
        let plain = cell_metrics(&CellInputs { dpi: 96, zoom_pct: 100, windows_spacing_dip: None, desktop_icon_dip: Some(16.0), match_desktop: true });
        assert_eq!((plain.cell_w_px, plain.cell_h_px), (75, 75), "a small icon leaves the 75 x 75 default");
    }

    #[test]
    fn spec_9_1_zoom_is_clamped_and_never_zero() {
        let at = |z| cell_metrics(&inputs(96, z, Some((75.0, 75.0)), false));
        assert_eq!(at(49), at(50));
        assert_eq!(at(301), at(300));
        assert_eq!(at(50).icon_px, 24);
        assert_eq!(at(300).icon_px, 144);
        let degenerate = cell_metrics(&CellInputs { dpi: 0, zoom_pct: 0, windows_spacing_dip: Some((0.0, 0.0)), desktop_icon_dip: Some(-4.0), match_desktop: true });
        assert!(degenerate.cell_w_px > 0 && degenerate.cell_h_px > 0 && degenerate.icon_px > 0 && degenerate.pad_px > 0);
    }

    #[test]
    fn spacing_from_the_api_is_clamped_per_axis() {
        assert_eq!(spacing_from_api(75.0, 75.0), Some((75.0, 75.0)));
        assert_eq!(spacing_from_api(40.0, 160.0), Some((40.0, 160.0)));
        assert_eq!(spacing_from_api(39.9, 100.0), Some((75.0, 100.0)));
        assert_eq!(spacing_from_api(100.0, 161.0), Some((100.0, 75.0)));
        assert_eq!(spacing_from_api(0.0, 0.0), None);
        assert_eq!(spacing_from_api(f64::NAN, 500.0), None);
    }

    #[test]
    fn desktop_metrics_come_from_the_view_in_dips_and_keep_the_cache_when_a_read_fails() {
        let none = DesktopMetrics::default();
        let m = desktop_metrics_from_view(none, Some(((120, 150), 48)), 120, Some((75.0, 75.0)));
        assert_eq!(m, DesktopMetrics { windows_spacing_dip: Some((96.0, 120.0)), icon_dip: Some(48.0), sample_dpi: 120 });
        // a failed read keeps the cached value (even when the API would give another one)
        assert_eq!(desktop_metrics_from_view(m, None, 144, Some((70.0, 80.0))), m);
        // nothing cached and no view: the API value
        let api = desktop_metrics_from_view(none, None, 96, Some((70.0, 80.0)));
        assert_eq!((api.windows_spacing_dip, api.icon_dip), (Some((70.0, 80.0)), None));
        assert_eq!(desktop_metrics_from_view(none, Some(((0, 150), 0)), 96, None), DesktopMetrics { sample_dpi: 96, ..none });
        // the cell at the sampling DPI reproduces the view's own pitch exactly
        let cell = cell_metrics(&CellInputs { dpi: 120, zoom_pct: 100, windows_spacing_dip: m.windows_spacing_dip, desktop_icon_dip: m.icon_dip, match_desktop: true });
        assert_eq!((cell.cell_w_px, cell.cell_h_px), (120, 150));
    }

    #[test]
    fn a_changed_desktop_spacing_or_icon_size_is_a_change() {
        let before = desktop_metrics_from_view(DesktopMetrics::default(), Some(((111, 113), 48)), 96, None);
        assert_eq!(desktop_metrics_from_view(before, Some(((111, 113), 48)), 96, None), before, "same read: no change");
        assert_ne!(desktop_metrics_from_view(before, Some(((150, 160), 64)), 96, None), before, "icon size changed");
        assert_ne!(desktop_metrics_from_view(before, Some(((111, 140), 48)), 96, None), before, "spacing changed");
    }

    #[test]
    fn the_sample_dpi_is_the_view_window_or_the_primary_monitor_not_the_first_listed() {
        let monitors = [(Rect::new(1920, 0, 5760, 2160), 144), (Rect::new(0, 0, 1920, 1080), 96)];
        assert_eq!(desktop_sample_dpi(None, &monitors, 120), 96, "primary (contains the origin), not monitors[0]");
        assert_eq!(desktop_sample_dpi(Some(192), &monitors, 120), 192, "the view window wins");
        assert_eq!(desktop_sample_dpi(Some(0), &monitors, 120), 96);
        assert_eq!(desktop_sample_dpi(None, &[(Rect::new(10, 10, 20, 20), 144)], 120), 120, "nothing at the origin: fallback");
        // 113 px at the primary's 96 DPI is 113 DIP, whichever monitor is listed first
        let m = desktop_metrics_from_view(DesktopMetrics::default(), Some(((113, 113), 48)), desktop_sample_dpi(None, &monitors, 144), None);
        assert_eq!(m.windows_spacing_dip, Some((113.0, 113.0)));
    }

    #[test]
    fn spec_9_2_one_function_feeds_rect_hit_and_drop() {
        for dpi in DPIS {
            let m = cell_metrics(&inputs(dpi, 100, Some((75.0, 75.0)), true));
            for cols in [1, 3, 5] {
                for count in 0..=60usize {
                    let mut previous: Option<Rect> = None;
                    for i in 0..count {
                        let r = m.cell_rect(i, cols);
                        let centre = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
                        assert_eq!(m.cell_at(centre, cols, 0), Some(i), "dpi {dpi} cols {cols} item {i}");
                        assert_eq!(m.drop_slot(centre, cols, 0, count), i);
                        // cells tile: same row touches, the next row starts where the last one ended
                        if let Some(p) = previous {
                            if i % cols as usize != 0 { assert_eq!(p.right, r.left); assert_eq!(p.top, r.top); }
                            else { assert_eq!(r.left, m.pad_px); assert_eq!(r.top, p.bottom); }
                        }
                        previous = Some(r);
                    }
                    // drop_slot is monotone along the reading order and never past the end
                    let mut last = 0usize;
                    for y in (0..m.content_h_px(count, cols) + 40).step_by(7) {
                        for x in (0..m.pad_px * 2 + cols * m.cell_w_px + 30).step_by(11) {
                            let slot = m.drop_slot((x, y), cols, 0, count);
                            assert!(slot <= count);
                            if x == 0 { assert!(slot >= last, "rows never go backwards"); last = slot; }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn cell_at_ignores_padding_and_the_partial_column() {
        let m = cell_metrics(&inputs(96, 100, Some((75.0, 75.0)), true));
        assert_eq!(m.cell_at((m.pad_px - 1, m.pad_px), 4, 0), None);
        assert_eq!(m.cell_at((m.pad_px, m.pad_px - 1), 4, 0), None);
        assert_eq!(m.cell_at((m.pad_px + 4 * m.cell_w_px, m.pad_px), 4, 0), None, "right of the last whole column");
        assert_eq!(m.cell_at((m.pad_px + 4 * m.cell_w_px - 1, m.pad_px), 4, 0), Some(3));
        assert_eq!(m.cell_at((m.pad_px, m.pad_px + m.cell_h_px), 4, 0), Some(4));
        assert_eq!(m.cell_at((m.pad_px, m.pad_px), 4, m.cell_h_px), Some(4), "scrolled by one row");
    }

    #[test]
    fn spec_9_3_scroll_limit_and_zoom_keeps_the_top_item() {
        let m = cell_metrics(&inputs(96, 100, Some((75.0, 75.0)), true));
        let cols = m.cols_for(400);
        assert_eq!(cols, (400 - 16) / 75);
        let rows = m.rows_for(20, cols);
        let content = rows * m.cell_h_px + 2 * m.pad_px;
        assert_eq!(m.max_scroll_px(20, cols, content), 0);
        assert_eq!(m.max_scroll_px(20, cols, content + 50), 0);
        assert_eq!(m.max_scroll_px(20, cols, content - 1), 1);
        assert_eq!(m.max_scroll_px(20, cols, 100), content - 100);
        assert_eq!(m.max_scroll_px(0, cols, 100), 0);
        // zoom: the first visible item stays first, whatever the new cell
        let big = cell_metrics(&inputs(96, 200, Some((75.0, 75.0)), true));
        let (old_cols, new_cols) = (m.cols_for(600), big.cols_for(600));
        let scroll = 3 * m.cell_h_px; // row 3 on top
        let first = 3 * old_cols;
        let after = big.scroll_after_zoom(&m, old_cols, new_cols, scroll, 80, 300);
        assert_eq!(after / big.cell_h_px, first / new_cols, "{after}");
        assert_eq!(big.scroll_after_zoom(&m, old_cols, new_cols, 0, 80, 300), 0);
        assert!(big.scroll_after_zoom(&m, old_cols, new_cols, 10_000, 80, 300) <= big.max_scroll_px(80, new_cols, 300));
    }

    #[test]
    fn a_corral_exactly_n_cells_wide_keeps_n_columns_at_every_dpi() {
        let at = |dpi| cell_metrics(&inputs(dpi, 100, Some((111.0, 107.0)), true));
        // 4 cells of 111 DIP + 2 x 8 DIP padding = 460 DIP: 1150 px at 240 DPI, where a cell rounds to 278 px
        assert_eq!(at(96).cols_for(460), 4);
        assert_eq!(at(240).cols_for(1150), 4);
        assert_eq!(at(240).cols_for(1150 - 278), 3, "a whole cell short is one column less");
    }

    #[test]
    fn body_size_round_trips_through_cells() {
        for dpi in DPIS {
            let m = cell_metrics(&inputs(dpi, 100, Some((75.0, 75.0)), true));
            for (c, r) in [(1, 1), (4, 3), (10, 7)] {
                let (w, h) = m.body_size_for(c, r);
                assert_eq!(m.cells_for_size(w, h), (c, r));
                assert_eq!(m.cells_for_size(w - 1, h - 1), (c - 1, r - 1));
                assert_eq!(m.cols_for(w), c);
            }
        }
    }

    #[test]
    fn spec_9_12_px_dip_round_trip() {
        for dpi in (96..=480u32).step_by(12).chain(DPIS) {
            for px in (-20_000..=20_000).step_by(37).chain([0, 1, -1, 19_999, 20_000, -20_000]) {
                assert_eq!(dip_to_px(px_to_dip(px, dpi), dpi), px, "dpi {dpi} px {px}");
            }
        }
        assert_eq!(round_half_up(-1.5), -1);
        assert_eq!(round_half_up(1.5), 2);
        assert_eq!(round_half_up(-0.5), 0);
        assert_eq!(dip_to_px(-1.0, 144), -1, "negative values round half up too (origin independent)");
    }
}
