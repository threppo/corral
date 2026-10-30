//! How a Corral is drawn (corral-painting-spec): geometry, body, title strip, outline, shades, tab
//! strip, folded look and title glyphs. The pure helpers (`strip_px`, `layout_title`,
//! `see_through_alpha`, `title_overlay`, `outline_color`, `auto_text`, ...) carry every rule; the two
//! painters (`paint_fills`, `paint_chrome`) only turn them into draw calls on a [`Sink`], so the
//! desktop, the Personalization sample and the Tabs preview tiles show the same pixels.
//!
//! Colours are straight 8-bit sRGB. A DIP is 1/96 inch; `px(d) = floor(d * dpi / 96 + 0.5)`.

use crate::geometry::Rect;
use crate::glyphs::{self, Glyph, IconFont};
use crate::render::{Align, RectF, Rgba};
use std::sync::OnceLock;
use windows::core::Result;

pub type Rgb = [u8; 3];

// ---- 2.1 constants (DIPs) -----------------------------------------------------------------------

/// Smallest title strip; also the 24 x 24 pointer target.
pub const STRIP_MIN: f64 = 24.0;
/// Space above and below the text line.
pub const STRIP_PAD_V: f64 = 4.0;
/// Title text size.
pub const TITLE_FONT_DIP: f64 = 12.0;
/// Corner radius of the outer shape.
pub const RADIUS: f64 = 8.0;
/// Text distance from the strip edge when no button is there.
pub const EDGE_PAD: f64 = 8.0;
/// Button hit box width (the height is the strip height).
pub const BTN_W: f64 = 24.0;
/// Glyph size.
pub const GLYPH: f64 = 14.0;
/// Hover plate inset inside the button box, and its radius.
pub const PLATE_INSET: f64 = 2.0;
pub const PLATE_R: f64 = 4.0;
/// Clear space between a button and the title text.
pub const TEXT_GAP: f64 = 4.0;
/// Narrowest title span worth drawing.
pub const TEXT_MIN: f64 = 24.0;
/// Alpha floor of the body so the most transparent Corral stays findable (3.3).
pub const ALPHA_FLOOR: i64 = 20;
/// The default `TransparencyFactor` (1380) draws about 54 % opacity, as Corral always has.
pub const DEFAULT_FILL_ALPHA: i64 = 138;
/// Distance of the button clusters from the strip ends (2.6).
pub const CLUSTER_PAD: f64 = 4.0;
/// Weight of titles without a stored weight (Semibold).
pub const DEFAULT_WEIGHT: u16 = 600;

/// `px(d) = floor(d * dpi / 96 + 0.5)` (round half up).
pub fn px(dip: f64, dpi: u32) -> i32 {
    (dip * dpi as f64 / 96.0 + 0.5).floor() as i32
}

/// Short name of [`px`] for layout code.
pub fn r(dip: f64, dpi: u32) -> i32 {
    px(dip, dpi)
}

// ---- fonts and the strip height (2.2) -----------------------------------------------------------

/// Em metrics of Segoe UI Variable / Segoe UI at 12 DIP: 1.079 + 0.251 + 0 (spec 9 test 1).
pub const SEGOE_LINE_EM: (f64, f64, f64) = (1.079, 0.251, 0.0);

/// Title family: Segoe UI Variable (Text optical size), else Segoe UI (4.4). Resolved once.
pub fn title_family() -> &'static str {
    static FAMILY: OnceLock<&'static str> = OnceLock::new();
    FAMILY.get_or_init(|| {
        #[cfg(not(test))]
        {
            if crate::render::font_family_exists("Segoe UI Variable Text") { "Segoe UI Variable Text" } else { "Segoe UI" }
        }
        #[cfg(test)]
        {
            "Segoe UI Variable Text"
        }
    })
}

/// Em metrics of the title font, measured once per process (a fallback font with a taller line makes
/// the strip taller instead of clipping, 2.2). Tests use the Segoe numbers so they do not depend on
/// the machine.
fn title_line_em() -> (f64, f64, f64) {
    static EM: OnceLock<(f64, f64, f64)> = OnceLock::new();
    *EM.get_or_init(|| {
        #[cfg(not(test))]
        {
            crate::render::font_line_metrics_em(title_family(), DEFAULT_WEIGHT)
                .filter(|(a, d, g)| (1.0..=2.0).contains(&(a + d + g)))
                .unwrap_or(SEGOE_LINE_EM)
        }
        #[cfg(test)]
        {
            SEGOE_LINE_EM
        }
    })
}

/// Title text size in device pixels at `dpi` (not rounded: 17.5 for the glyph font at 120 dpi).
pub fn title_text_px(dpi: u32) -> f32 {
    (TITLE_FONT_DIP * dpi as f64 / 96.0) as f32
}

/// Glyph font size in device pixels.
pub fn glyph_px(dpi: u32) -> f32 {
    (GLYPH * dpi as f64 / 96.0) as f32
}

/// Width in px of `text` in the title font (the one it is drawn with: weight and italic included).
pub fn measure_title(text: &str, dpi: u32, weight: u16, italic: bool) -> i32 {
    crate::render::measure_text_styled(text, title_family(), title_text_px(dpi), weight, italic)
        .map(|(w, _)| w.ceil() as i32)
        .unwrap_or(0)
}

/// Stored font weight (1..=999), else Semibold.
pub fn title_weight(stored: Option<u32>) -> u16 {
    stored.map_or(DEFAULT_WEIGHT, |w| w.clamp(1, 999) as u16)
}

/// `max(px(24), line_px + 2 * px(4))`, `line_px = ceil((ascent + descent + gap) * title font px)`.
pub fn strip_px(dpi: u32, ascent_em: f64, descent_em: f64, line_gap_em: f64) -> i32 {
    let line = (ascent_em + descent_em + line_gap_em) * (TITLE_FONT_DIP * dpi as f64 / 96.0);
    let line_px = (line - 1e-6).ceil().max(0.0) as i32;
    px(STRIP_MIN, dpi).max(line_px + 2 * px(STRIP_PAD_V, dpi))
}

/// What the layout code needs of a Corral's chrome at one DPI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    /// Title strip height (`strip_px`).
    pub title_h: i32,
    /// Space between strip and body: none, the strip abuts the body (2.3).
    pub gap: i32,
    /// Corner radius before clamping.
    pub radius: i32,
}

pub fn metrics(dpi: u32) -> Metrics {
    let (a, d, g) = title_line_em();
    Metrics { title_h: strip_px(dpi, a, d, g), gap: 0, radius: px(RADIUS, dpi) }
}

// ---- 2.3 shape and strip rectangles -------------------------------------------------------------

/// Corner radius of the outer shape: `px(8)` clamped to half the shorter side and, for a folded
/// Corral, to half the strip height; `SquareEdges` gives 0 (2.3).
pub fn corner_radius(width: i32, height: i32, dpi: u32, square: bool, folded_strip_h: Option<i32>) -> f32 {
    if square {
        return 0.0;
    }
    let mut radius = px(RADIUS, dpi) as f32;
    radius = radius.min(width.min(height).max(0) as f32 / 2.0);
    if let Some(strip) = folded_strip_h {
        radius = radius.min(strip.max(0) as f32 / 2.0);
    }
    radius
}

#[cfg(test)] // the app decides the side itself (`model::title_below`)
/// Not enough room above the body inside the monitor for the strip: the strip goes below. Whether it
/// really does also depends on `TitleEdgeAutoAdjust` and the stored side (`model::title_below`).
pub fn near_monitor_top(body: Rect, monitor_top: i32, m: &Metrics) -> bool {
    body.top - monitor_top <= m.title_h + m.gap - 1
}

/// Title strip rect above (`below == false`) or below the body.
pub fn title_rect_on(body: Rect, below: bool, m: &Metrics) -> Rect {
    if below {
        let top = body.bottom + m.gap;
        Rect::new(body.left, top, body.right, top + m.title_h)
    } else {
        Rect::new(body.left, body.top - m.gap - m.title_h, body.right, body.top - m.gap)
    }
}

/// Outer rect = strip plus body; a folded Corral is the strip alone (2.5).
pub fn union_rect(body: Rect, title: Rect, folded: bool) -> Rect {
    if folded {
        return title;
    }
    Rect::new(body.left.min(title.left), body.top.min(title.top), body.right.max(title.right), body.bottom.max(title.bottom))
}

/// Per-corner radii `[tl, tr, br, bl]` of the strip: it owns the outer corners on its side; a folded
/// Corral's strip owns all four; the seam to the body is square (2.3).
pub fn strip_radii(radius: f32, above: bool, folded: bool) -> [f32; 4] {
    if folded {
        [radius; 4]
    } else if above {
        [radius, radius, 0.0, 0.0]
    } else {
        [0.0, 0.0, radius, radius]
    }
}

// ---- 2.4 visibility ------------------------------------------------------------------------------

/// Title visibility modes (`LabelMode`).
#[cfg_attr(not(test), allow(dead_code))]
pub const LABEL_NEVER: u32 = 0;
pub const LABEL_ON_HOVER: u32 = 1;
pub const LABEL_ALWAYS: u32 = 2;

/// Does the Corral reserve a strip? On hover reserves it (nothing shifts when it appears); Never does
/// not; a folded Corral always has its strip (2.4, 2.5).
pub fn strip_reserved(label_mode: u32, folded: bool) -> bool {
    folded || label_mode >= LABEL_ON_HOVER
}

/// Reveal durations of a reserved strip (2.4): 120 ms in, 200 ms out; `motion::duration` makes them 0
/// while Windows animations are off. The strip's eased value is `reveal_for`'s `hover`.
pub const REVEAL_IN_MS: f64 = 120.0;
pub const REVEAL_OUT_MS: f64 = 200.0;

/// The amount of the strip that is shown: Always 1; On hover follows `hover` (0..1); Never 0; a
/// folded Corral always 1.
pub fn reveal_for(label_mode: u32, folded: bool, hover: f64) -> f64 {
    if folded || label_mode >= LABEL_ALWAYS {
        1.0
    } else if label_mode == LABEL_ON_HOVER {
        hover.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

// ---- 3 body --------------------------------------------------------------------------------------

/// Body alpha for the stored `TransparencyFactor` (0..2000, how solid the Corral is: higher = more
/// opaque, as Corral has always drawn it; user decision 2026-10-21). Two straight segments, monotone:
/// 0 gives the floor of 20, the default 1380 gives 138 (about 54 %), 2000 and above give 255.
/// `20 + (v * 118 + 690) / 1380` up to 1380, then `138 + ((v - 1380) * 117 + 310) / 620` (3.3).
pub fn see_through_alpha(stored: i64) -> u8 {
    let v = stored.clamp(0, 2000);
    let alpha = if v <= 1380 {
        ALPHA_FLOOR + (v * 118 + 690) / 1380
    } else {
        DEFAULT_FILL_ALPHA + ((v - 1380) * 117 + 310) / 620
    };
    alpha.clamp(ALPHA_FLOOR, 255) as u8
}

/// `floor(a * fade + 0.5)` (3.5).
pub fn apply_fade(alpha: u8, fade: f64) -> u8 {
    (alpha as f64 * fade.clamp(0.0, 1.0) + 0.5).floor().clamp(0.0, 255.0) as u8
}

/// Percent of full alpha as a byte (button plates, tab fills).
pub fn pct_alpha(percent: u32) -> u8 {
    ((255 * percent + 50) / 100).min(255) as u8
}

fn rgba(rgb: Rgb, alpha: u8) -> Rgba {
    Rgba::new(rgb[0], rgb[1], rgb[2], alpha)
}

/// COLORREF (`0x00BBGGRR`) to RGB.
pub fn colorref_rgb(c: u32) -> Rgb {
    [c as u8, (c >> 8) as u8, (c >> 16) as u8]
}

// ---- 4 title -------------------------------------------------------------------------------------

/// `DarkenTitlebars` values.
#[cfg_attr(not(test), allow(dead_code))]
pub const SHADE_NONE: u32 = 0;
pub const SHADE_DARK: u32 = 1;
pub const SHADE_DARKER: u32 = 2;

/// Shade overlay of the strip: Dark scales the RGB to 75 %, Darker to 50 % (`(c * pct + 50) / 100`);
/// alpha is `max(fill_alpha, 112)` / `max(fill_alpha, 128)`. Before `fade` (4.2).
pub fn title_overlay(rgb: Rgb, shade: u32, fill_alpha: u8) -> Option<Rgba> {
    let (pct, floor) = match shade {
        SHADE_DARK => (75u32, 112u8),
        SHADE_DARKER => (50, 128),
        _ => return None,
    };
    let scale = |c: u8| ((c as u32 * pct + 50) / 100) as u8;
    Some(Rgba::new(scale(rgb[0]), scale(rgb[1]), scale(rgb[2]), fill_alpha.max(floor)))
}

/// Strength of the hover mix: white or black at 28/255 (11 %), 4.3.
pub const HOVER_MIX_ALPHA: u8 = 28;

/// White `#FFFFFF` when its contrast with the backdrop is at least that of `#1B1D22`, else
/// `#1B1D22` (camouflage-spec 3.5).
pub fn auto_text(backdrop: Rgb) -> Rgb {
    let c = crate::camouflage::auto_text_color(backdrop);
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// "Dark" colours (Auto picks white text): the outline and the hover mix use the same notion (4.4).
pub fn prefers_light_text(rgb: Rgb) -> bool {
    auto_text(rgb) == [0xff; 3]
}

/// `color` composited by `alpha` over `under`, per encoded channel.
pub fn over(color: Rgb, alpha: u8, under: Rgb) -> Rgb {
    std::array::from_fn(|i| ((color[i] as u32 * alpha as u32 + under[i] as u32 * (255 - alpha as u32) + 127) / 255) as u8)
}

/// Final colour of the strip over the sampled mean wallpaper colour: body fill, then the shade overlay,
/// both with their real alpha (4.4). `fade` does not enter, so nothing flips while it animates.
pub fn strip_backdrop(mean: Rgb, body: Rgb, fill_alpha: u8, overlay: Option<Rgba>) -> Rgb {
    let mut c = over(body, fill_alpha, mean);
    if let Some(o) = overlay {
        c = over([o.r, o.g, o.b], o.a, c);
    }
    c
}

/// How the title text is coloured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextMode {
    Auto,
    Custom(Rgb),
}

/// Title text colour: `Custom` as stored, `Auto` by the contrast against the strip's final colour.
pub fn title_text_color(mode: TextMode, strip_final: Rgb) -> Rgb {
    match mode {
        TextMode::Custom(c) => c,
        TextMode::Auto => auto_text(strip_final),
    }
}

// ---- 5 outline -----------------------------------------------------------------------------------

/// Final body RGB mixed 35 % toward white for dark colours, toward black for light ones (5).
pub fn outline_color(rgb: Rgb) -> Rgb {
    if prefers_light_text(rgb) {
        rgb.map(|c| (c as u32 + ((255 - c as u32) * 35 + 50) / 100) as u8)
    } else {
        rgb.map(|c| (c as u32 - (c as u32 * 35 + 50) / 100) as u8)
    }
}

/// Outline alpha: `floor(180 * fade + 0.5)`.
pub fn outline_opacity(fade: f64) -> u8 {
    apply_fade(180, fade)
}

// ---- 8 high contrast -----------------------------------------------------------------------------

/// System colours of a high-contrast theme (8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighContrast {
    pub window: Rgb,
    pub window_text: Rgb,
    pub highlight: Rgb,
    pub highlight_text: Rgb,
}

/// The current high-contrast theme, if one is active. Re-read on `WM_SETTINGCHANGE` and
/// `WM_THEMECHANGED` (the caller repaints).
pub fn high_contrast() -> Option<HighContrast> {
    use windows::Win32::Graphics::Gdi::{COLOR_HIGHLIGHT, COLOR_HIGHLIGHTTEXT, COLOR_WINDOW, COLOR_WINDOWTEXT, GetSysColor, SYS_COLOR_INDEX};
    use windows::Win32::UI::Accessibility::{HCF_HIGHCONTRASTON, HIGHCONTRASTW};
    use windows::Win32::UI::WindowsAndMessaging::{SPI_GETHIGHCONTRAST, SystemParametersInfoW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
    let mut info = HIGHCONTRASTW { cbSize: size_of::<HIGHCONTRASTW>() as u32, ..Default::default() };
    // SAFETY: `info` is a correctly sized HIGHCONTRASTW for SPI_GETHIGHCONTRAST.
    let ok = unsafe {
        SystemParametersInfoW(SPI_GETHIGHCONTRAST, info.cbSize, Some(&mut info as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0))
    };
    if ok.is_err() || info.dwFlags.0 & HCF_HIGHCONTRASTON.0 == 0 {
        return None;
    }
    let sys = |index: SYS_COLOR_INDEX| colorref_rgb(unsafe { GetSysColor(index) });
    Some(HighContrast {
        window: sys(COLOR_WINDOW),
        window_text: sys(COLOR_WINDOWTEXT),
        highlight: sys(COLOR_HIGHLIGHT),
        highlight_text: sys(COLOR_HIGHLIGHTTEXT),
    })
}

/// Outline width in pixels: 1 at every DPI; in high contrast `max(2, px(1))` (5, 8).
pub fn outline_width(dpi: u32, high_contrast: bool) -> f32 {
    if high_contrast { 2.max(px(1.0, dpi)) as f32 } else { 1.0 }
}

// ---- 6 buttons -----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ButtonState {
    #[default]
    Rest,
    Hover,
    /// Mouse down on the button (capture).
    Pressed,
    /// Up at the root of a Gate: shown, never removed.
    Disabled,
}

/// Plate alpha: Hover 14 %, Pressed 24 %, none otherwise.
pub fn plate_alpha(state: ButtonState) -> u8 {
    match state {
        ButtonState::Hover => pct_alpha(14),
        ButtonState::Pressed => pct_alpha(24),
        ButtonState::Rest | ButtonState::Disabled => 0,
    }
}

/// Glyph alpha: 100 %, Disabled 40 %.
pub fn glyph_alpha(state: ButtonState) -> u8 {
    if state == ButtonState::Disabled { pct_alpha(40) } else { 255 }
}

/// The hover plate: the button box inset by 2 DIP.
pub fn plate_rect(button: Rect, dpi: u32) -> RectF {
    let inset = px(PLATE_INSET, dpi);
    rectf(Rect::new(button.left + inset, button.top + inset, button.right - inset, button.bottom - inset))
}

pub fn rectf(rect: Rect) -> RectF {
    RectF::new(rect.left as f32, rect.top as f32, rect.right as f32, rect.bottom as f32)
}

/// Which buttons a strip has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ButtonSet {
    pub menu: bool,
    pub up: bool,
    pub close: bool,
}

/// Input of [`layout_title`]; `strip` is in the coordinates the result is returned in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TitleLayoutInput {
    pub strip: Rect,
    pub dpi: u32,
    pub want: ButtonSet,
    /// A Gate's inline folder glyph before the name.
    pub folder: bool,
    /// Measured width of the title text in px (0 for an empty title).
    pub text_w: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TitleLayout {
    pub menu: Option<Rect>,
    pub up: Option<Rect>,
    pub close: Option<Rect>,
    pub folder: Option<Rect>,
    pub text: Option<Rect>,
    /// The span between the clusters the run is centred in.
    pub span: (i32, i32),
}

/// Left to right: leading cluster (menu, Up) from 4 DIP, title run (folder glyph, 4 DIP, text) centred
/// in the span, trailing cluster (close) ending 4 DIP from the end. Clusters are dropped in the order
/// close, Up, menu until the span holds `TEXT_MIN`. Room is reserved whether or not a button is hovered
/// (2.6).
pub fn layout_title(input: &TitleLayoutInput) -> TitleLayout {
    let TitleLayoutInput { strip, dpi, want, folder, text_w } = *input;
    let (w, top, bottom) = (strip.width(), strip.top, strip.bottom);
    let (btn, pad, edge, gap, min) = (px(BTN_W, dpi), px(CLUSTER_PAD, dpi), px(EDGE_PAD, dpi), px(TEXT_GAP, dpi), px(TEXT_MIN, dpi));
    let mut set = want;
    let span_of = |set: &ButtonSet| {
        let lead = set.menu as i32 + set.up as i32;
        let left = if lead > 0 { pad + lead * btn + gap } else { edge };
        let right = if set.close { w - pad - btn - gap } else { w - edge };
        (left, right)
    };
    loop {
        let (l, rr) = span_of(&set);
        if rr - l >= min {
            break;
        }
        if set.close {
            set.close = false;
        } else if set.up {
            set.up = false;
        } else if set.menu {
            set.menu = false;
        } else {
            break;
        }
    }
    let at = |x0: i32, x1: i32| Rect::new(strip.left + x0, top, strip.left + x1, bottom);
    let mut out = TitleLayout::default();
    let mut x = pad;
    if set.menu {
        out.menu = Some(at(x, x + btn));
        x += btn;
    }
    if set.up {
        out.up = Some(at(x, x + btn));
    }
    if set.close {
        out.close = Some(at(w - pad - btn, w - pad));
    }
    let (left, right) = span_of(&set);
    out.span = (strip.left + left, strip.left + right);
    let span_w = right - left;
    if span_w <= 0 {
        return out;
    }
    let glyph_w = if folder { px(GLYPH, dpi) } else { 0 };
    let after_glyph = if folder { gap } else { 0 };
    let avail = span_w - glyph_w - after_glyph;
    let draw_text = span_w >= min && text_w > 0 && avail > 0;
    let text_draw = if draw_text { text_w.min(avail) } else { 0 };
    let run_w = if folder && glyph_w <= span_w { glyph_w + if draw_text { after_glyph + text_draw } else { 0 } } else { text_draw };
    let x0 = if run_w < span_w { left + (span_w - run_w) / 2 } else { left };
    if folder && glyph_w <= span_w {
        out.folder = Some(at(x0, x0 + glyph_w));
    }
    if draw_text {
        let tx = if folder { x0 + glyph_w + after_glyph } else { x0 };
        out.text = Some(at(tx, tx + text_draw));
    }
    out
}

// ---- 7 tab strip ---------------------------------------------------------------------------------

/// `TabsStyle` values.
pub const TABS_MENU_ONLY: u32 = 0;
pub const TABS_FLAT: u32 = 1;
pub const TABS_SEGMENTED: u32 = 2;
pub const TABS_ROUNDED: u32 = 3;

mod segments;
pub use segments::*;

/// One tab to paint. `hit` is its cell in the strip's coordinates.
#[derive(Clone, Debug, PartialEq)]
pub struct TabItem {
    pub title: String,
    pub hit: Rect,
    pub active: bool,
    pub hovered: bool,
    /// `TabsColors` on: this tab's own effective colour (colour-state-spec 2.3).
    pub own_color: Option<Rgb>,
    /// The tab's Custom text colour, else Auto against its own fill.
    pub custom_text: Option<Rgb>,
    /// The member's own title font weight and italic flag (measured and drawn the same way).
    pub weight: u16,
    pub italic: bool,
    /// Separate title-bar chrome; absent on the original narrow Rounded pill path.
    pub segment: Option<SegmentChrome>,
}

pub struct TabsPaint<'a> {
    pub style: u32,
    pub tabs: &'a [TabItem],
}

fn tab_text_alpha(tab: &TabItem, hc: bool) -> u8 {
    if hc || tab.active || tab.hovered { 255 } else { pct_alpha(70) }
}

// ---- the painters --------------------------------------------------------------------------------

/// Font of title text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TitleFont {
    pub family: &'static str,
    pub size_px: f32,
    pub weight: u16,
    pub italic: bool,
}

/// Where the painters send their draw calls: the real context, or a recorder in tests.
pub trait Sink {
    /// Fill with per-corner radii `[tl, tr, br, bl]`.
    fn fill_shape(&mut self, rect: RectF, radii: [f32; 4], color: Rgba) -> Result<()>;
    /// Ring of `width` px inside the shape.
    fn fill_ring(&mut self, rect: RectF, radii: [f32; 4], width: f32, color: Rgba) -> Result<()>;
    fn fill_round(&mut self, rect: RectF, radius: f32, color: Rgba) -> Result<()>;
    fn fill_rect(&mut self, rect: RectF, color: Rgba) -> Result<()>;
    fn line(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: Rgba) -> Result<()>;
    fn text(&mut self, text: &str, rect: RectF, font: &TitleFont, color: Rgba, align: Align) -> Result<()>;
    /// One glyph centred in `box_rect` at `size_px`: the installed icon font, else vector strokes (6).
    fn glyph(&mut self, glyph: Glyph, box_rect: RectF, size_px: f32, color: Rgba) -> Result<()> {
        match glyphs::font_for(glyph) {
            IconFont::Vector => {
                let (cx, cy) = ((box_rect.left + box_rect.right) / 2.0, (box_rect.top + box_rect.bottom) / 2.0);
                let half = size_px / 2.0;
                let square = RectF::new(cx - half, cy - half, cx + half, cy + half);
                let width = glyphs::vector_stroke_width(size_px);
                for s in glyphs::vector_strokes(glyph, square) {
                    self.line(s.from, s.to, width, color)?;
                }
                Ok(())
            }
            font => {
                let family = font.family().unwrap_or(glyphs::FLUENT);
                let font = TitleFont { family, size_px, weight: 400, italic: false };
                self.text(&glyph.ch().to_string(), box_rect, &font, color, Align::Center)
            }
        }
    }
}

/// The real drawing context as a [`Sink`].
pub struct CtxSink<'a, 'b>(pub &'a crate::render::Ctx<'b>);

impl Sink for CtxSink<'_, '_> {
    fn fill_shape(&mut self, rect: RectF, radii: [f32; 4], color: Rgba) -> Result<()> {
        self.0.fill_corner_shape(rect, radii, color)
    }
    fn fill_ring(&mut self, rect: RectF, radii: [f32; 4], width: f32, color: Rgba) -> Result<()> {
        self.0.fill_corner_ring(rect, radii, width, color)
    }
    fn fill_round(&mut self, rect: RectF, radius: f32, color: Rgba) -> Result<()> {
        self.0.fill_rounded_rect(rect, radius, color)
    }
    fn fill_rect(&mut self, rect: RectF, color: Rgba) -> Result<()> {
        self.0.fill_rect(rect, color)
    }
    fn line(&mut self, from: (f32, f32), to: (f32, f32), width: f32, color: Rgba) -> Result<()> {
        self.0.stroke_line(from, to, width, color)
    }
    fn text(&mut self, text: &str, rect: RectF, font: &TitleFont, color: Rgba, align: Align) -> Result<()> {
        let style = crate::render::TextStyle { family: font.family, size_px: font.size_px, weight: font.weight, color, align, shadow: None };
        self.0.draw_text_italic(text, rect, &style, font.italic)
    }
}

/// Everything the painters need. Rectangles are in the surface's pixels.
pub struct PaintInputs<'a> {
    /// Body plus strip (the strip alone when folded).
    pub outer: Rect,
    /// The strip, or `None` when the title is Never and the Corral is not folded.
    pub strip: Option<Rect>,
    pub strip_above: bool,
    pub folded: bool,
    pub dpi: u32,
    pub square_edges: bool,
    /// Resolved body colour after Camouflage recolouring (3.2).
    pub body_rgb: Rgb,
    /// `see_through_alpha`, before fade.
    pub fill_alpha: u8,
    pub shade: u32,
    pub outline: bool,
    /// 0..1, multiplies every alpha drawn (3.5).
    pub fade: f64,
    /// 0..1, how much of a reserved strip is shown (2.4).
    pub reveal: f64,
    /// 0..1, the pointer over the strip (4.3).
    pub strip_hover: f64,
    pub text: TextMode,
    /// Mean wallpaper colour behind the Corral, for the Auto text decision.
    pub backdrop_mean: Rgb,
    pub title: &'a str,
    pub weight: u16,
    pub italic: bool,
    pub layout: TitleLayout,
    pub menu: ButtonState,
    pub up: ButtonState,
    pub close: ButtonState,
    /// 0..1 visibility of the menu and close buttons (shown on hover); the room is reserved regardless.
    /// Up is navigation, not a hover control: it is always fully visible while the strip is.
    pub buttons: f64,
    pub tabs: Option<TabsPaint<'a>>,
    pub high_contrast: Option<HighContrast>,
}

impl PaintInputs<'_> {
    pub fn radius(&self) -> f32 {
        corner_radius(self.outer.width(), self.outer.height(), self.dpi, self.square_edges,
            self.folded.then(|| self.strip.map_or(0, |s| s.height())))
    }

    /// Strip overlay colour before fade/reveal.
    pub fn overlay(&self) -> Option<Rgba> {
        title_overlay(self.body_rgb, self.shade, self.fill_alpha)
    }

    /// Title text colour (Auto: against the strip's final colour over the mean wallpaper).
    pub fn text_rgb(&self) -> Rgb {
        if let Some(hc) = &self.high_contrast {
            return hc.window_text;
        }
        title_text_color(self.text, strip_backdrop(self.backdrop_mean, self.body_rgb, self.fill_alpha, self.overlay()))
    }

    fn font(&self) -> TitleFont {
        TitleFont { family: title_family(), size_px: title_text_px(self.dpi), weight: self.weight, italic: self.italic }
    }
}

/// Steps 2 to 4 of the paint order (3.1): the body fill over the whole outer rect, the strip overlay
/// and hover mix scaled by `reveal`, and the tab fills. The caller composites a blurred backdrop under
/// these pixels (step 1) before `paint_chrome`.
pub fn paint_fills(sink: &mut dyn Sink, p: &PaintInputs) -> Result<()> {
    if has_segments(p) { return paint_segment_fills(sink, p); }
    let radius = p.radius();
    let outer = rectf(p.outer);
    if let Some(hc) = &p.high_contrast {
        // Opaque window colour; see-through, shade, hover mix and blur are ignored. fade 0 stays clickable.
        let a = if p.fade <= 0.0 { 1 } else { 255 };
        sink.fill_shape(outer, [radius; 4], rgba(hc.window, a))?;
        return paint_tab_fills(sink, p);
    }
    // The alpha floor of 1 keeps a fully faded Corral hit-testable on a layered window.
    sink.fill_shape(outer, [radius; 4], rgba(p.body_rgb, apply_fade(p.fill_alpha, p.fade).max(1)))?;
    if let Some(strip) = p.strip {
        let shown = p.fade * p.reveal.clamp(0.0, 1.0);
        let radii = strip_radii(radius, p.strip_above, p.folded);
        if let Some(o) = p.overlay() {
            let a = apply_fade(o.a, shown);
            if a > 0 {
                sink.fill_shape(rectf(strip), radii, Rgba::new(o.r, o.g, o.b, a))?;
            }
        }
        let mix = apply_fade(HOVER_MIX_ALPHA, shown * p.strip_hover.clamp(0.0, 1.0));
        if mix > 0 {
            let c = if prefers_light_text(p.body_rgb) { [255; 3] } else { [0; 3] };
            sink.fill_shape(rectf(strip), radii, rgba(c, mix))?;
        }
    }
    paint_tab_fills(sink, p)
}

/// Fill rectangle of a tab inside the strip for the style (7).
fn tab_shape(style: u32, tab: &TabItem, strip: Rect, dpi: u32) -> Option<(RectF, f32)> {
    let hit = tab.hit;
    match style {
        TABS_FLAT => {
            let inset = px(PLATE_INSET, dpi);
            Some((rectf(Rect::new(hit.left + inset, strip.top + inset, hit.right - inset, strip.bottom - inset)), px(PLATE_R, dpi) as f32))
        }
        TABS_SEGMENTED => {
            // The segment sits 2 DIP inside the track, which is the strip inset by 2 DIP.
            let inset = px(PLATE_INSET, dpi);
            let rect = Rect::new(hit.left + inset, strip.top + 2 * inset, hit.right - inset, strip.bottom - 2 * inset);
            Some((rectf(rect), px(4.0, dpi) as f32))
        }
        TABS_ROUNDED => {
            // Pills 4 DIP apart (2 DIP off each cell edge), height strip - 8 DIP, fully rounded.
            let (h_inset, v_inset) = (px(2.0, dpi), px(4.0, dpi));
            let rect = Rect::new(hit.left + h_inset, strip.top + v_inset, hit.right - h_inset, strip.bottom - v_inset);
            let radius = rect.height().min(rect.width()).max(0) as f32 / 2.0;
            Some((rectf(rect), radius))
        }
        _ => None,
    }
}

fn paint_tab_fills(sink: &mut dyn Sink, p: &PaintInputs) -> Result<()> {
    let (Some(tabs), Some(strip)) = (&p.tabs, p.strip) else { return Ok(()) };
    if tabs.style == TABS_MENU_ONLY || tabs.tabs.is_empty() {
        return Ok(());
    }
    let shown = p.fade * p.reveal.clamp(0.0, 1.0);
    if shown <= 0.0 && p.high_contrast.is_none() {
        return Ok(());
    }
    let dpi = p.dpi;
    let fade = |a: u8| apply_fade(a, if p.high_contrast.is_some() { 1.0 } else { shown });
    let text = p.text_rgb();
    if tabs.style == TABS_SEGMENTED && p.high_contrast.is_none() {
        // One track holding all tabs.
        let inset = px(PLATE_INSET, dpi);
        let track = Rect::new(strip.left + inset, strip.top + inset, strip.right - inset, strip.bottom - inset);
        sink.fill_round(rectf(track), px(6.0, dpi) as f32, rgba(text, fade(pct_alpha(10))))?;
    }
    for tab in tabs.tabs {
        let Some((rect, radius)) = tab_shape(tabs.style, tab, strip, dpi) else { continue };
        if let Some(hc) = &p.high_contrast {
            if tab.active {
                if tabs.style == TABS_FLAT {
                    sink.fill_rect(flat_bar(tab, strip, p.strip_above, dpi), rgba(hc.window_text, 255))?;
                } else {
                    sink.fill_round(rect, radius, rgba(hc.highlight, 255))?;
                }
            }
            continue;
        }
        let (fill, edge): (Option<Rgba>, Option<Rgba>) = match tab.own_color {
            Some(own) => {
                let alpha = if tab.active { 255 } else { 120 };
                (Some(rgba(own, fade(alpha))), Some(rgba(outline_color(own), fade(180))))
            }
            None => {
                let percent = match (tabs.style, tab.active, tab.hovered) {
                    (TABS_FLAT, _, true) => 10,
                    (TABS_FLAT, _, false) => 0,
                    (TABS_SEGMENTED, true, _) => 22,
                    (TABS_SEGMENTED, false, true) => 10,
                    (TABS_SEGMENTED, false, false) => 0,
                    (_, true, _) => 22,
                    // a hovered inactive pill sits between inactive (8) and active (22)
                    (_, false, true) => 14,
                    (_, false, false) => 8,
                };
                (Some(rgba(text, fade(pct_alpha(percent)))).filter(|c| c.a > 0), None)
            }
        };
        match tabs.style {
            TABS_FLAT => {
                if tab.hovered && tab.own_color.is_none() {
                    if let Some(f) = fill {
                        sink.fill_round(rect, radius, f)?;
                    }
                }
                if tab.active {
                    let bar = flat_bar(tab, strip, p.strip_above, dpi);
                    let color = tab.own_color.map_or(rgba(text, fade(255)), |own| rgba(own, fade(255)));
                    sink.fill_rect(bar, color)?;
                    if let Some(e) = edge {
                        sink.fill_ring(bar, [0.0; 4], 1.0, e)?;
                    }
                } else if let (Some(own), Some(e)) = (tab.own_color, edge) {
                    // Inactive coloured tab: the bar at alpha 120.
                    let bar = flat_bar(tab, strip, p.strip_above, dpi);
                    sink.fill_rect(bar, rgba(own, fade(120)))?;
                    sink.fill_ring(bar, [0.0; 4], 1.0, e)?;
                }
            }
            _ => {
                if let Some(f) = fill {
                    sink.fill_round(rect, radius, f)?;
                }
                if let Some(e) = edge {
                    sink.fill_ring(rect, [radius; 4], 1.0, e)?;
                }
            }
        }
    }
    Ok(())
}

/// The 2 DIP bar of the Flat style along the edge facing the body.
fn flat_bar(tab: &TabItem, strip: Rect, above: bool, dpi: u32) -> RectF {
    let bar = px(2.0, dpi).max(1);
    let (top, bottom) = if above { (strip.bottom - bar, strip.bottom) } else { (strip.top, strip.top + bar) };
    rectf(Rect::new(tab.hit.left, top, tab.hit.right, bottom))
}

/// Steps 5 and 6 (3.1): the outline after every fill and after Camouflage recolouring, then glyphs and
/// title text in the resolved colour.
pub fn paint_chrome(sink: &mut dyn Sink, p: &PaintInputs) -> Result<()> {
    if has_segments(p) { return paint_segment_chrome(sink, p); }
    let hc = p.high_contrast;
    let radius = p.radius();
    if p.outline || hc.is_some() {
        let color = hc.map_or_else(
            || rgba(outline_color(p.body_rgb), outline_opacity(p.fade)),
            |h| rgba(h.window_text, 255),
        );
        if color.a > 0 {
            sink.fill_ring(rectf(p.outer), [radius; 4], outline_width(p.dpi, hc.is_some()), color)?;
        }
    }
    paint_title_chrome(sink, p)
}

/// Text and controls without the outer outline, also used by separate Rounded title bars.
fn paint_title_chrome(sink: &mut dyn Sink, p: &PaintInputs) -> Result<()> {
    let hc = p.high_contrast;
    let Some(strip) = p.strip else { return Ok(()) };
    let shown = if hc.is_some() { 1.0 } else { p.fade * p.reveal.clamp(0.0, 1.0) };
    if shown <= 0.0 {
        return Ok(());
    }
    let text_rgb = p.text_rgb();
    let alpha = |a: u8, extra: f64| apply_fade(a, shown * extra);
    let dpi = p.dpi;
    // Buttons: plate for hover and pressed, then the glyph.
    let hover_cluster = if hc.is_some() { 1.0 } else { p.buttons.clamp(0.0, 1.0) };
    let buttons = [(p.layout.menu, Glyph::Menu, p.menu, hover_cluster), (p.layout.up, Glyph::Up, p.up, 1.0), (p.layout.close, Glyph::Close, p.close, hover_cluster)];
    for (rect, glyph, state, cluster) in buttons {
        let Some(rect) = rect else { continue };
        if cluster <= 0.0 {
            continue;
        }
        let (plate, ink) = match (&hc, state) {
            (Some(h), ButtonState::Hover | ButtonState::Pressed) => (Some(h.highlight), h.highlight_text),
            (_, _) => (None, text_rgb),
        };
        let plate_a = if hc.is_some() && plate.is_some() { 255 } else { plate_alpha(state) };
        if plate_a > 0 {
            sink.fill_round(plate_rect(rect, dpi), px(PLATE_R, dpi) as f32, rgba(plate.unwrap_or(text_rgb), alpha(plate_a, cluster)))?;
        }
        sink.glyph(glyph, rectf(rect), glyph_px(dpi), rgba(ink, alpha(glyph_alpha(state), cluster)))?;
    }
    if let Some(folder) = p.layout.folder {
        sink.glyph(Glyph::Folder, rectf(folder), glyph_px(dpi), rgba(text_rgb, alpha(255, 1.0)))?;
    }
    let font = p.font();
    match &p.tabs {
        Some(tabs) if tabs.style != TABS_MENU_ONLY && !tabs.tabs.is_empty() => {
            for tab in tabs.tabs {
                let ink = match (&hc, tab.own_color) {
                    (Some(h), _) => if tab.active { h.highlight_text } else { h.window_text },
                    (None, Some(own)) => tab.custom_text.unwrap_or_else(|| auto_text(own)),
                    (None, None) => tab.custom_text.unwrap_or(text_rgb),
                };
                let pad = px(10.0, dpi);
                let label = Rect::new(tab.hit.left + pad.min(tab.hit.width() / 2), strip.top, tab.hit.right - pad.min(tab.hit.width() / 2), strip.bottom);
                let font = TitleFont { weight: tab.weight, italic: tab.italic, ..font };
                sink.text(&tab.title, rectf(label), &font, rgba(ink, alpha(tab_text_alpha(tab, hc.is_some()), 1.0)), Align::Center)?;
            }
        }
        _ => {
            if let (Some(text), false) = (p.layout.text, p.title.is_empty()) {
                sink.text(p.title, rectf(text), &font, rgba(text_rgb, alpha(255, 1.0)), Align::Left)?;
            }
        }
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
/// Both painters in order, for callers with no backdrop to place in between (the Tabs preview tiles).
pub fn paint_corral(sink: &mut dyn Sink, p: &PaintInputs) -> Result<()> {
    paint_fills(sink, p)?;
    paint_chrome(sink, p)
}

#[cfg(test)]
mod tests;
