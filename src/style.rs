//! Visual constants (DIPs at 96 DPI). Every value carries its provenance; `PROVISIONAL` values are
//! placeholders until measured.

use crate::render::Rgba;

/// Corner radius. PROVISIONAL.
pub const CORNER_RADIUS: f32 = 4.0;
/// Group body fill. PROVISIONAL (dark translucent).
pub const BODY_COLOR: Rgba = Rgba::new(16, 16, 16, 110);
/// Title band fill. PROVISIONAL.
pub const TITLE_COLOR: Rgba = Rgba::new(0, 0, 0, 150);
pub const TITLE_TEXT: Rgba = Rgba::new(255, 255, 255, 255);
pub const TITLE_FONT_PX: f32 = 15.0;
/// Desktop icon edge in DIPs when the desktop's own size is unknown: Windows' medium icons
/// (grid-snapping-spec 2.2). The grid itself comes from `icon_metrics::cell_metrics`, never from a constant here.
pub const ICON_PX: i32 = 48;
pub const LABEL_FONT_PX: f32 = 12.0;
pub const LABEL_TEXT: Rgba = Rgba::new(255, 255, 255, 255);
pub const LABEL_SHADOW: Rgba = Rgba::new(0, 0, 0, 200);
/// Rubber band used while drawing a new group. PROVISIONAL.
pub const BAND_FILL: Rgba = Rgba::new(0, 120, 215, 60);
pub const BAND_BORDER: Rgba = Rgba::new(0, 120, 215, 220);
pub const FONT: &str = "Segoe UI";
/// Loose-icon selection highlight. PROVISIONAL (Explorer list-view selection look, not measured).
pub const SELECTION_FILL: Rgba = Rgba::new(204, 232, 255, 60);
pub const SELECTION_BORDER: Rgba = Rgba::new(153, 209, 255, 140);
