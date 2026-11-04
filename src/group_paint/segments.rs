//! Tabs with enough room become full-width title bars, each with its own buttons. Rounded bars stand apart like
//! small Corrals; Segmented bars are square boxes above the body; Flat bars are square and sit on the body with an
//! underline. Only Rounded has rounded tabs (user decision 2026-11-04).
use super::*;

pub const SEGMENT_GAP: f64 = 4.0;
pub const SEGMENT_BODY_GAP: f64 = 2.0;
pub const SEGMENT_TITLE_MIN: f64 = 48.0;

#[derive(Clone, Copy, Debug)]
pub struct SegmentInput {
    pub want: ButtonSet,
    pub folder: bool,
    pub text_w: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabSegment {
    pub hit: Rect,
    pub layout: TitleLayout,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentPart { Title, Menu, Close, Up }

impl TabSegment {
    /// Disabled Up and invisible hover controls still reserve room, but are title hits.
    pub fn hit_test(&self, point: (i32, i32), buttons: bool, up_enabled: bool) -> Option<SegmentPart> {
        let inside = |r: Option<Rect>| r.is_some_and(|r| r.contains(point.0, point.1));
        if !self.hit.contains(point.0, point.1) { return None }
        Some(if buttons && inside(self.layout.menu) { SegmentPart::Menu }
            else if buttons && inside(self.layout.close) { SegmentPart::Close }
            else if up_enabled && inside(self.layout.up) { SegmentPart::Up }
            else { SegmentPart::Title })
    }
}

/// Equal widths, with remainder pixels distributed across the strip. A bar without room for its buttons plus 48 DIP
/// of text drops buttons in the strip order (close, Up, menu; user decision 2026-10-30); if even the bare title does
/// not fit, use the narrow cells of `tab_cells` instead.
pub fn tab_segments(strip: Rect, dpi: u32, style: u32, members: &[SegmentInput]) -> Option<Vec<TabSegment>> {
    let n = members.len();
    if n < 2 || strip.height() <= 0 { return None }
    let gap = px(if style == TABS_ROUNDED { SEGMENT_GAP } else { TAB_GAP }, dpi) as i64;
    let usable = strip.width() as i64 - gap * (n - 1) as i64;
    if usable <= 0 { return None }
    let mut out = Vec::with_capacity(n);
    for (i, member) in members.iter().enumerate() {
        let left = strip.left as i64 + usable * i as i64 / n as i64 + gap * i as i64;
        let right = strip.left as i64 + usable * (i + 1) as i64 / n as i64 + gap * i as i64;
        let hit = Rect::new(left as i32, strip.top, right as i32, strip.bottom);
        let folder_space = if member.folder { px(GLYPH, dpi) + px(TEXT_GAP, dpi) } else { 0 };
        let mut want = member.want;
        let layout = loop {
            let layout = layout_title(&TitleLayoutInput { strip: hit, dpi, want, folder: member.folder, text_w: member.text_w });
            if layout.span.1 - layout.span.0 - folder_space >= px(SEGMENT_TITLE_MIN, dpi) { break layout }
            if want.close { want.close = false } else if want.up { want.up = false } else if want.menu { want.menu = false } else { return None }
        };
        out.push(TabSegment { hit, layout });
    }
    Some(out)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentChrome {
    pub layout: TitleLayout,
    pub hover: f64,
    pub buttons: f64,
    pub menu: ButtonState,
    pub up: ButtonState,
    pub close: ButtonState,
}

pub(super) fn has_segments(p: &PaintInputs) -> bool {
    p.strip.is_some() && p.tabs.as_ref().is_some_and(|t| (TABS_FLAT..=TABS_ROUNDED).contains(&t.style)
        && !t.tabs.is_empty() && t.tabs.iter().all(|t| t.segment.is_some()))
}

fn segment_body(p: &PaintInputs) -> Option<Rect> {
    if p.folded { return None }
    let strip = p.strip?;
    let gap = if segment_body_gap(segment_style(p), p.square_edges) { px(SEGMENT_BODY_GAP, p.dpi) } else { 0 };
    let body = if p.strip_above {
        Rect::new(p.outer.left, strip.bottom + gap, p.outer.right, p.outer.bottom)
    } else { Rect::new(p.outer.left, p.outer.top, p.outer.right, strip.top - gap) };
    (body.height() > 0).then_some(body)
}

/// Round tabs stand apart from the body also when the body is square; a square tab can sit on a square body.
pub fn segment_body_gap(style: u32, square_edges: bool) -> bool {
    match style {
        TABS_FLAT => false,
        TABS_ROUNDED => true,
        _ => !square_edges,
    }
}

fn segment_style(p: &PaintInputs) -> u32 {
    p.tabs.as_ref().map_or(TABS_ROUNDED, |t| t.style)
}

/// Flat's body joins its bars on a square seam; the others are shapes of their own.
fn body_radii(p: &PaintInputs) -> [f32; 4] {
    if segment_style(p) == TABS_FLAT { strip_radii(p.radius(), !p.strip_above, false) } else { [p.radius(); 4] }
}

/// Round because the tab style says so: "Rounded corners" is about the Corral's body (user report 2026-11-04).
fn bar_radii(p: &PaintInputs) -> [f32; 4] {
    if segment_style(p) != TABS_ROUNDED { return [0.0; 4] }
    let folded = p.folded.then(|| p.strip.map_or(0, |s| s.height()));
    [corner_radius(p.outer.width(), p.outer.height(), p.dpi, false, folded); 4]
}

/// The blur mask shares exactly the fill geometry, leaving the inter-bar and body gaps transparent.
pub fn paint_mask(sink: &mut dyn Sink, p: &PaintInputs) -> Result<()> {
    let color = rgba([255; 3], 255);
    if !has_segments(p) { return sink.fill_shape(rectf(p.outer), [p.radius(); 4], color) }
    if let Some(body) = segment_body(p) { sink.fill_shape(rectf(body), body_radii(p), color)?; }
    for tab in p.tabs.as_ref().unwrap().tabs { sink.fill_shape(rectf(tab.hit), bar_radii(p), color)?; }
    Ok(())
}

/// Composite straight-alpha paint layers without adding an opaque backing in the gaps.
fn composite(top: Rgba, under: Rgba) -> Rgba {
    let ta = top.a as u32;
    let ua = under.a as u32 * (255 - ta);
    let a = ta * 255 + ua;
    if a == 0 { return rgba([0; 3], 0) }
    let channel = |t: u8, u: u8| ((t as u32 * ta * 255 + u as u32 * ua + a / 2) / a) as u8;
    Rgba::new(channel(top.r, under.r), channel(top.g, under.g), channel(top.b, under.b), ((a + 127) / 255) as u8)
}

fn segment_fill(p: &PaintInputs, tab: &TabItem) -> Rgba {
    segment_fill_at(p, tab, p.fade * p.reveal.clamp(0.0, 1.0))
}

fn segment_fill_at(p: &PaintInputs, tab: &TabItem, shown: f64) -> Rgba {
    if let Some(hc) = p.high_contrast { return rgba(if tab.active { hc.highlight } else { hc.window }, 255) }
    let chrome = tab.segment.unwrap();
    // The same body + shade + hover stack as a normal title strip, flattened so that the
    // inactive multiplier applies to the resulting fill alpha (rather than each layer).
    // As on a single strip, On-hover revelation fades its overlay and chrome; the
    // reserved strip's base fill remains visible along with the body.
    let mut fill = rgba(p.body_rgb, apply_fade(p.fill_alpha, p.fade));
    if let Some(o) = p.overlay() {
        fill = composite(Rgba { a: apply_fade(o.a, shown), ..o }, fill);
    }
    if let Some(own) = tab.own_color { fill = rgba(own, fill.a); }
    let rgb = tab.own_color.unwrap_or(p.body_rgb);
    let mix = rgba(if prefers_light_text(rgb) { [255; 3] } else { [0; 3] },
        apply_fade(HOVER_MIX_ALPHA, shown * chrome.hover.clamp(0.0, 1.0)));
    fill = composite(mix, fill);
    fill.a = apply_fade(fill.a, if tab.active { 1.0 } else if tab.hovered { 0.8 } else { 0.6 });
    fill
}

pub(super) fn paint_segment_fills(sink: &mut dyn Sink, p: &PaintInputs) -> Result<()> {
    if let Some(body) = segment_body(p) {
        let color = p.high_contrast.map_or_else(|| rgba(p.body_rgb, apply_fade(p.fill_alpha, p.fade).max(1)), |h| rgba(h.window, 255));
        sink.fill_shape(rectf(body), body_radii(p), color)?;
    }
    for tab in p.tabs.as_ref().unwrap().tabs {
        let fill = segment_fill(p, tab);
        if fill.a > 0 { sink.fill_shape(rectf(tab.hit), bar_radii(p), fill)?; }
    }
    Ok(())
}

pub(super) fn paint_segment_chrome(sink: &mut dyn Sink, p: &PaintInputs) -> Result<()> {
    let flat = segment_style(p) == TABS_FLAT;
    if p.outline || p.high_contrast.is_some() {
        let ink = p.high_contrast.map_or_else(|| rgba(outline_color(p.body_rgb), outline_opacity(p.fade)), |h| rgba(h.window_text, 255));
        let width = outline_width(p.dpi, p.high_contrast.is_some());
        // Flat reads as one Corral: a single outline around the bars and the body.
        if flat { sink.fill_ring(rectf(p.outer), if p.folded { [0.0; 4] } else { body_radii(p) }, width, ink)?; }
        else if let Some(body) = segment_body(p) { sink.fill_ring(rectf(body), [p.radius(); 4], width, ink)?; }
    }
    let shown = if p.high_contrast.is_some() { 1.0 } else { p.fade * p.reveal.clamp(0.0, 1.0) };
    if shown <= 0.0 { return Ok(()) }
    for tab in p.tabs.as_ref().unwrap().tabs {
        let c = tab.segment.unwrap();
        let rgb = tab.own_color.unwrap_or(p.body_rgb);
        let ink = match p.high_contrast {
            Some(h) => if tab.active { h.highlight_text } else { h.window_text },
            // The same stable Auto decision as normal strips/pills: hover, fade and
            // inactive alpha do not flip the text colour during an animation.
            None => tab.custom_text.unwrap_or_else(|| tab.own_color.map_or_else(
                || auto_text(strip_backdrop(p.backdrop_mean, p.body_rgb, p.fill_alpha, p.overlay())), auto_text)),
        };
        let mut layout = c.layout;
        layout.text = None;
        layout.folder = None;
        // Reuse normal strip button plates, glyphs and disabled/pressed rules. The text run
        // is drawn separately so only its alpha follows the active/inactive title rule.
        let child = PaintInputs {
            outer: tab.hit, strip: Some(tab.hit), strip_above: true, folded: true,
            dpi: p.dpi, square_edges: p.square_edges, body_rgb: rgb, fill_alpha: p.fill_alpha,
            shade: p.shade, outline: false, fade: p.fade, reveal: p.reveal, strip_hover: c.hover,
            text: TextMode::Custom(ink), backdrop_mean: p.backdrop_mean, title: "", weight: tab.weight,
            italic: tab.italic, layout, menu: c.menu, up: c.up, close: c.close, buttons: c.buttons,
            tabs: None, high_contrast: p.high_contrast.map(|h| HighContrast { window_text: ink, ..h }),
        };
        if !flat && (p.outline || p.high_contrast.is_some()) {
            let outline = p.high_contrast.map_or_else(|| rgba(outline_color(rgb), apply_fade(180, shown)), |h| rgba(h.window_text, 255));
            sink.fill_ring(rectf(tab.hit), bar_radii(p), outline_width(p.dpi, p.high_contrast.is_some()), outline)?;
        }
        if flat && tab.active {
            // The 2 DIP bar along the edge facing the body.
            let bar = px(2.0, p.dpi).max(1);
            let (top, bottom) = if p.strip_above { (tab.hit.bottom - bar, tab.hit.bottom) } else { (tab.hit.top, tab.hit.top + bar) };
            sink.fill_rect(rectf(Rect::new(tab.hit.left, top, tab.hit.right, bottom)),
                rgba(ink, apply_fade(255, if p.high_contrast.is_some() { 1.0 } else { shown })))?;
        }
        paint_title_chrome(sink, &child)?;
        let color = rgba(ink, apply_fade(tab_text_alpha(tab, p.high_contrast.is_some()), shown));
        if let Some(folder) = c.layout.folder { sink.glyph(Glyph::Folder, rectf(folder), glyph_px(p.dpi), color)?; }
        if let Some(text) = c.layout.text {
            sink.text(&tab.title, rectf(text), &TitleFont { weight: tab.weight, italic: tab.italic, ..p.font() }, color, Align::Left)?;
        }
    }
    Ok(())
}
