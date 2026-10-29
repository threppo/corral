//! Personalization/Camouflage adapters. Geometry is shared by paint, pointer and keyboard targets.
use super::{
    content::{Row, RowKind},
    style_menus::Cmd,
};
use crate::{
    color_state::{self, Effective, TintMode},
    settings::Settings,
};
use std::cell::RefCell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorChannel {
    Background,
    Text,
    Tint,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorChoice {
    Auto,
    Preset(usize),
    Custom,
    Eyedropper,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SliderKind {
    SeeThrough,
    Blur,
    BlendStrength,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Color {
        channel: ColorChannel,
        choice: ColorChoice,
        anchor: (i32, i32),
    },
    Slider {
        kind: SliderKind,
        left: i32,
        right: i32,
    },
    Style(Cmd),
    TintMode(i64),
    CamouflageMode(i64),
}
#[derive(Clone, Default, Debug)]
pub struct ScopeState {
    pub title: Option<String>,
    pub own_titles: Vec<String>,
    pub own: bool,
}
thread_local! {
    static SLIDER_POINTER:std::cell::Cell<Option<(SliderKind,f32)>> = const { std::cell::Cell::new(None) };
    static CAMOUFLAGE:std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    static SCOPE:RefCell<ScopeState> = RefCell::new(ScopeState::default());
    static EFFECTIVE:RefCell<Option<Effective>> = const { RefCell::new(None) };
}
pub fn set_scope(title: Option<String>, own_titles: Vec<String>, own: bool) {
    SCOPE.with(|s| {
        *s.borrow_mut() = ScopeState {
            title,
            own_titles,
            own,
        }
    });
}
pub fn scope() -> ScopeState {
    SCOPE.with(|s| s.borrow().clone())
}
pub fn set_camouflage_override(value: Option<bool>) { CAMOUFLAGE.with(|v| v.set(value)); }
pub fn camouflage_mode() -> i64 { CAMOUFLAGE.with(|v| match v.get() {None=>0,Some(true)=>1,Some(false)=>2}) }
/// The values the colour rows show: the scope's effective values with any open draft on top.
pub fn set_effective(effective: &Effective) {
    EFFECTIVE.with(|m| *m.borrow_mut() = Some(effective.clone()));
}
pub fn effective(settings: &Settings) -> Effective {
    EFFECTIVE
        .with(|m| m.borrow().clone())
        .unwrap_or_else(|| color_state::effective_of(settings, &Default::default()))
}
pub fn reset_label() -> String {
    scope().title.map_or_else(
        || "Reset all Corrals…".into(),
        |t| format!("Reset \"{t}\"…"),
    )
}
/// "A", "A and B", "A, B and C", "A, B, C and 2 more".
pub fn title_list(titles: &[String]) -> String {
    let shown = &titles[..titles.len().min(3)];
    let more = titles.len() - shown.len();
    match (shown, more) {
        ([], _) => String::new(),
        ([only], 0) => only.clone(),
        (_, 0) => format!("{} and {}", shown[..shown.len() - 1].join(", "), shown[shown.len() - 1]),
        _ => format!("{} and {more} more", shown.join(", ")),
    }
}
pub fn scope_caption() -> String {
    let s = scope();
    if let Some(title) = s.title {
        return format!(
            "Changes apply only to \"{title}\".{}",
            if s.own { " It has its own look." } else { "" }
        );
    }
    let n = s.own_titles.len();
    if n == 0 {
        return "All Corrals share this look.".into();
    }
    let names = title_list(&s.own_titles);
    format!(
        "{n} {} {} {} own look: {names}. Pick one above to edit only that Corral.",
        if n == 1 { "Corral" } else { "Corrals" },
        if n == 1 { "has" } else { "have" },
        if n == 1 { "its" } else { "their" }
    )
}
pub const BLUR_RADII: [u32; 5] = [0, 15, 30, 60, 120];
pub const BLUR_NAMES: [&str; 5] = ["Off", "Subtle", "Soft", "Medium", "Strong"];
pub fn nearest_blur(radius: impl Into<f64>) -> usize {
    let radius = radius.into();
    BLUR_RADII
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            ((**a as f64 - radius).abs()).total_cmp(&(**b as f64 - radius).abs())
        })
        .map_or(0, |(i, _)| i)
}
pub fn blur_radius(index: usize) -> u32 {
    BLUR_RADII[index.min(4)]
}
/// The "See-through" slider shows 100 % minus how solid the stored value is (`TransparencyFactor`
/// 0..2000, higher = more opaque): 1380 reads 31 %, 2000 reads 0 %.
pub fn see_through_percent(raw: u32) -> i32 {
    100 - ((raw.min(2000) + 10) / 20) as i32
}
/// Inverse of [`see_through_percent`].
pub fn see_through_raw(percent: i32) -> u32 {
    (100 - percent.clamp(0, 100)) as u32 * 20
}
pub fn blend_percent(raw: u8) -> i32 {
    (i32::from(raw) * 100 + 127) / 255
}
pub fn blend_raw(percent: i32) -> u8 {
    ((percent.clamp(0, 100) * 255 + 50) / 100) as u8
}
pub fn set_slider_pointer(value: Option<(SliderKind, f32)>) {
    SLIDER_POINTER.with(|v| v.set(value));
}
pub fn slider_value_at(kind: SliderKind, x: i32, left: i32, right: i32) -> i32 {
    let max = if kind == SliderKind::Blur { 4.0 } else { 100.0 };
    (((x - left) as f32 / (right - left).max(1) as f32).clamp(0.0, 1.0) * max).round() as i32
}
/// RGB hex constants converted at the boundary to COLORREF.
pub fn preset(channel: ColorChannel, index: usize) -> Option<u32> {
    const BODY: [u32; 8] = [
        0x292c32, 0x494e58, 0x3f6fb5, 0x2e8b8b, 0x3d8b57, 0xb97a33, 0xb8505f, 0x7a5ba8,
    ];
    const TEXT: [u32; 7] = [
        0xffffff, 0xd7dadf, 0x8b919c, 0x1b1d22, 0xf3cd9c, 0x9db8e2, 0xf08a80,
    ];
    let c = *if channel == ColorChannel::Text {
        TEXT.get(index)?
    } else {
        BODY.get(index)?
    };
    Some(((c >> 16) & 255) | (c & 0xff00) | ((c & 255) << 16))
}
/// `text_auto` is the effective text mode (colour-state-spec 2.1): Auto is a mode, not a colour.
pub fn selected_swatch(channel: ColorChannel, color: u32, text_auto: bool) -> ColorChoice {
    if channel == ColorChannel::Text && text_auto {
        return ColorChoice::Auto;
    }
    (0..if channel == ColorChannel::Text { 7 } else { 8 })
        .find(|&i| preset(channel, i) == Some(color & 0xffffff))
        .map_or(ColorChoice::Custom, ColorChoice::Preset)
}
pub fn channel(row: &Row) -> Option<ColorChannel> {
    match row.text {
        "Color" => Some(ColorChannel::Background),
        "Text color" => Some(ColorChannel::Text),
        "Tint color" => Some(ColorChannel::Tint),
        _ => None,
    }
}
pub fn slider(row: &Row) -> Option<SliderKind> {
    match row.text {
        "See-through" => Some(SliderKind::SeeThrough),
        "Blur" | "Wallpaper blur" => Some(SliderKind::Blur),
        "Blend strength" => Some(SliderKind::BlendStrength),
        _ => None,
    }
}
pub fn style_command(row: &Row, value: i64) -> Option<Cmd> {
    Some(match row.text {
        "Rounded corners" => Cmd::RoundCorners,
        "Outline" => Cmd::Outline,
        "Titlebar shade" => Cmd::Darken(value.clamp(0, 2) as u32),
        "Titlebar buttons on hover" => Cmd::ButtonsOnHover,
        "Titlebar below near the screen top" => Cmd::TitleBelowNearTop,
        _ => return None,
    })
}
pub fn supports_group(row: &Row) -> bool {
    matches!(
        row.text,
        "Color" | "See-through" | "Text color" | "Tint icons" | "Tint color" | "Camouflage"
    ) || row.kind == RowKind::Expander
}
pub fn adapt_row(mut row: Row, settings: &Settings) -> (Row, bool) {
    let mut enabled = true;
    if scope().title.is_some() && !supports_group(&row) {
        row.desc = Some("Applies to all Corrals.");
        enabled = false;
    }
    let m = effective(settings);
    if row.text == "Tint color" {
        enabled &= m.tint_mode() == TintMode::Custom;
    }
    (row, enabled)
}
pub fn tint_mode(effective: &Effective) -> i64 {
    effective.tint_mode().index()
}
pub fn checked(row: &Row, settings: &Settings) -> bool {
    match row.text {
        "Rounded corners" => !settings.square_edges,
        "Outline" => settings.outline,
        "Titlebar buttons on hover" => settings.use_titlebar_controls,
        "Titlebar below near the screen top" => settings.title_edge_auto_adjust,
        _ => row.bind.is_some_and(|f| settings.is_on(f)),
    }
}
pub fn current(row: &Row, settings: &Settings) -> i64 {
    match row.text {
        "Tint icons" => tint_mode(&effective(settings)),
        "Camouflage" => camouflage_mode(),
        "Titlebar shade" => i64::from(settings.darken_titlebars),
        _ => row.current_value(settings),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mappings_and_inbetween_blur_preserve_storage() {
        for (i, &r) in BLUR_RADII.iter().enumerate() {
            assert_eq!(nearest_blur(r), i);
            assert_eq!(blur_radius(i), r);
        }
        assert_eq!(nearest_blur(48), 3);
        assert_eq!(nearest_blur(255), 4);
        assert_eq!((see_through_percent(1380), see_through_percent(2000), see_through_percent(0)), (31, 0, 100));
        assert_eq!((see_through_raw(0), see_through_raw(100)), (2000, 0));
        for p in 0..=100 {
            assert_eq!(see_through_percent(see_through_raw(p)), p);
            assert_eq!(blend_percent(blend_raw(p)), p);
        }
        // The three-way choice is a view over `tint_icons` plus the source (colour-state-spec 2.1).
        let base = color_state::effective_of(&Settings::default(), &Default::default());
        for v in 0..3 {
            let mode = TintMode::from_index(v).unwrap();
            let patch = color_state::ColorEdit::TintMode(mode).patch(&base);
            let mut shared = color_state::Globals::from_settings(&Settings::default());
            patch.apply_to_globals(&mut shared);
            let shown = color_state::resolve(&shared, &Default::default());
            assert_eq!(tint_mode(&shown), v);
            assert_eq!(shown.tint_source == crate::color_state::TintSource::Custom, v == 2);
        }
        assert!(TintMode::from_index(3).is_none());
    }
    #[test]
    fn scope_copy_names_reset_scope_and_all_own_looks() {
        set_scope(None, vec![], false);
        assert_eq!(scope_caption(), "All Corrals share this look.");
        assert_eq!(reset_label(), "Reset all Corrals…");
        set_scope(None, vec!["One".into()], false);
        assert!(scope_caption().starts_with("1 Corral has its own look: One."));
        set_scope(
            None,
            vec!["A".into(), "B".into(), "C".into(), "D".into()],
            false,
        );
        assert!(scope_caption().contains("A, B, C and 1 more"));
        let t = |v: &[&str]| title_list(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!((t(&["A"]), t(&["A", "B"]), t(&["A", "B", "C"])), ("A".into(), "A and B".into(), "A, B and C".into()));
        set_scope(Some("Downloads".into()), vec![], true);
        assert_eq!(reset_label(), "Reset \"Downloads\"…");
        assert!(scope_caption().ends_with("It has its own look."));
        set_scope(None, vec![], false);
    }
    #[test]
    fn swatch_matching_auto_and_custom() {
        for ch in [
            ColorChannel::Background,
            ColorChannel::Text,
            ColorChannel::Tint,
        ] {
            for i in 0..if ch == ColorChannel::Text { 7 } else { 8 } {
                assert_eq!(
                    selected_swatch(ch, preset(ch, i).unwrap(), false),
                    ColorChoice::Preset(i)
                );
            }
            assert_eq!(selected_swatch(ch, 0x123456, false), ColorChoice::Custom);
        }
        // Auto is the mode, whatever colour is remembered next to it.
        assert_eq!(selected_swatch(ColorChannel::Text, 0x123456, true), ColorChoice::Auto);
        assert_eq!(selected_swatch(ColorChannel::Background, 0x123456, true), ColorChoice::Custom);
    }
}
use super::{Hit, RowLay, Theme, control_rect, kit, kit_draw, scf, style, wrap};
use crate::render::{Align, Ctx, RectF, Rgba};
use windows::core::Result;
#[derive(Clone)]
pub(super) struct ScopeLay {
    pub label: RectF,
    pub dropdown: RectF,
    pub reset: RectF,
    pub caption: Vec<(String, RectF)>,
    pub bottom: f32,
}
pub(super) fn scope_layout(left: f32, right: f32, y: f32, dpi: u32) -> Result<ScopeLay> {
    let top = y + scf(12.0, dpi);
    let narrow = right - left < scf(570.0, dpi);
    let label = RectF::new(left, top, left + scf(120.0, dpi), top + scf(32.0, dpi));
    let dropdown = RectF::new(
        label.right + scf(12.0, dpi),
        top,
        (label.right + scf(232.0, dpi)).min(right),
        top + scf(32.0, dpi),
    );
    let reset_width = (crate::render::measure_text(
        &reset_label(),
        kit::theme::font_family(),
        scf(14.0, dpi),
        400,
    )?
    .0 + scf(24.0, dpi))
    .min(if narrow {
        (right - left) * 0.55
    } else {
        (right - dropdown.right - scf(12.0, dpi)).max(scf(80.0, dpi))
    })
    .max(scf(80.0, dpi));
    let reset_top = if narrow { top + scf(36.0, dpi) } else { top };
    let reset = RectF::new(
        right - reset_width,
        reset_top,
        right,
        reset_top + scf(32.0, dpi),
    );
    let mut yy = reset.bottom + scf(4.0, dpi);
    let mut caption = vec![];
    for line in wrap_long(&scope_caption(), right - left, dpi)? {
        caption.push((line, RectF::new(left, yy, right, yy + scf(16.0, dpi))));
        yy += scf(16.0, dpi);
    }
    Ok(ScopeLay {
        label,
        dropdown,
        reset,
        caption,
        bottom: yy + scf(14.0, dpi),
    })
}
/// Scope titles can contain a single 255-character word; preserve every Unicode scalar on wrapping.
fn wrap_long(text: &str, width: f32, dpi: u32) -> Result<Vec<String>> {
    let mut out = vec![];
    for line in wrap(text, scf(12.0, dpi), 400, width)? {
        if crate::render::measure_text(&line, kit::theme::font_family(), scf(12.0, dpi), 400)?.0
            <= width
        {
            out.push(line);
            continue;
        }
        let mut chunk = String::new();
        for c in line.chars() {
            let mut next = chunk.clone();
            next.push(c);
            if !chunk.is_empty()
                && crate::render::measure_text(
                    &next,
                    kit::theme::font_family(),
                    scf(12.0, dpi),
                    400,
                )?
                .0 > width
            {
                out.push(std::mem::take(&mut chunk));
            }
            chunk.push(c);
        }
        if !chunk.is_empty() {
            out.push(chunk);
        }
    }
    Ok(out)
}
pub(super) fn scope_targets(s: &ScopeLay) -> Vec<(Hit, RectF)> {
    vec![
        (
            Hit::ColorTarget {
                anchor: (s.dropdown.left as i32, s.dropdown.bottom as i32),
            },
            s.dropdown,
        ),
        (Hit::ResetAppearance, s.reset),
    ]
}
pub(super) fn scope_hit(s: &ScopeLay, x: f32, y: f32) -> Option<Hit> {
    scope_targets(s)
        .into_iter()
        .find(|(_, r)| contains(*r, x, y))
        .map(|(h, _)| h)
}
fn contains(r: RectF, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}
pub(super) fn draw_scope(ctx: &Ctx, s: &ScopeLay, t: &Theme, dpi: u32) -> Result<()> {
    ctx.draw_text_cached(
        "Apply changes to",
        s.label,
        &style(
            kit::theme::font_family(),
            scf(14.0, dpi),
            400,
            t.nav_text,
            Align::Left,
        ),
    )?;
    kit_draw(
        ctx,
        kit::controls::Control::Dropdown,
        s.dropdown,
        scope().title.as_deref().unwrap_or("All Corrals"),
        true,
        false,
        t,
        dpi,
        0.0,
    )?;
    kit_draw(
        ctx,
        kit::controls::Control::Button(kit::controls::ButtonStyle::Subtle),
        s.reset,
        &reset_label(),
        true,
        false,
        t,
        dpi,
        0.0,
    )?;
    for (line, r) in &s.caption {
        ctx.draw_text_cached(
            line,
            *r,
            &style(
                kit::theme::font_family(),
                scf(12.0, dpi),
                400,
                t.sub_header_text,
                Align::Left,
            ),
        )?;
    }
    Ok(())
}
fn swatches(row: &RowLay, dpi: u32) -> Result<Vec<(ColorChoice, RectF)>> {
    let Some(channel) = channel(&row.row) else {
        return Ok(vec![]);
    };
    let r = control_rect(row, dpi)?;
    let n = if channel == ColorChannel::Text { 7 } else { 8 };
    let mut out = vec![];
    let mut x = r.left;
    if channel == ColorChannel::Text {
        out.push((
            ColorChoice::Auto,
            RectF::new(
                x,
                r.top + scf(2.0, dpi),
                x + scf(28.0, dpi),
                r.top + scf(30.0, dpi),
            ),
        ));
        x += scf(32.0, dpi);
    }
    for i in 0..n {
        out.push((
            ColorChoice::Preset(i),
            RectF::new(
                x,
                r.top + scf(2.0, dpi),
                x + scf(28.0, dpi),
                r.top + scf(30.0, dpi),
            ),
        ));
        x += scf(32.0, dpi);
    }
    out.push((
        ColorChoice::Custom,
        RectF::new(
            x,
            r.top + scf(2.0, dpi),
            x + scf(28.0, dpi),
            r.top + scf(30.0, dpi),
        ),
    ));
    out.push((
        ColorChoice::Eyedropper,
        RectF::new(r.right - scf(32.0, dpi), r.top, r.right, r.bottom),
    ));
    Ok(out)
}
/// The Custom swatch of a colour row (the Corral menu's "Colors..." opens the popover under it).
pub(super) fn custom_swatch(row: &RowLay, dpi: u32) -> Result<Option<RectF>> {
    Ok(swatches(row, dpi)?
        .into_iter()
        .find(|(choice, _)| *choice == ColorChoice::Custom)
        .map(|(_, rect)| rect))
}
fn slider_rects(row: &RowLay, dpi: u32) -> Result<(RectF, RectF)> {
    let r = control_rect(row, dpi)?;
    Ok((
        RectF::new(r.left, r.top, r.right - scf(88.0, dpi), r.bottom),
        RectF::new(
            r.right - scf(72.0, dpi),
            r.top + scf(4.0, dpi),
            r.right,
            r.bottom - scf(4.0, dpi),
        ),
    ))
}
pub(super) fn row_targets(row: &RowLay, dpi: u32) -> Result<Vec<(Hit, RectF)>> {
    if !row.enabled {
        return Ok(vec![]);
    }
    if let Some(ch) = channel(&row.row) {
        return Ok(swatches(row, dpi)?
            .into_iter()
            .map(|(choice, r)| {
                (
                    Hit::LookAction(Action::Color {
                        channel: ch,
                        choice,
                        anchor: (r.left as i32, r.bottom as i32),
                    }),
                    r,
                )
            })
            .collect());
    }
    if let Some(kind) = slider(&row.row) {
        let (r, _) = slider_rects(row, dpi)?;
        return Ok(vec![(
            Hit::LookAction(Action::Slider {
                kind,
                left: (r.left + scf(10.0, dpi)) as i32,
                right: (r.right - scf(10.0, dpi)) as i32,
            }),
            r,
        )]);
    }
    if row.row.kind == RowKind::Segmented && matches!(row.row.text, "Tint icons" | "Titlebar shade" | "Camouflage") {
        let r = control_rect(row, dpi)?;
        let w = (r.right - r.left) / 3.0;
        return Ok((0..3)
            .map(|i| {
                let a = if row.row.text == "Tint icons" {
                    Action::TintMode(i)
                } else if row.row.text == "Camouflage" {
                    Action::CamouflageMode(i)
                } else {
                    Action::Style(Cmd::Darken(i as u32))
                };
                (
                    Hit::LookAction(a),
                    RectF::new(
                        r.left + w * i as f32,
                        r.top,
                        r.left + w * (i + 1) as f32,
                        r.bottom,
                    ),
                )
            })
            .collect());
    }
    if let Some(cmd) = style_command(&row.row, 0) {
        return Ok(vec![(
            Hit::LookAction(Action::Style(cmd)),
            RectF::new(
                row.x - scf(16.0, dpi),
                row.y,
                row.x + row.w + scf(16.0, dpi),
                row.y + row.h,
            ),
        )]);
    }
    Ok(vec![])
}
pub(super) fn hit_row(row: &RowLay, x: f32, y: f32, dpi: u32) -> Result<Option<Hit>> {
    Ok(row_targets(row, dpi)?
        .into_iter()
        .find(|(_, r)| contains(*r, x, y))
        .map(|(h, _)| h))
}
fn color(e: &Effective, channel: ColorChannel) -> u32 {
    match channel {
        ColorChannel::Background => e.color,
        ColorChannel::Text => e.text_color,
        ColorChannel::Tint => e.tint_in_use(),
    }
}
fn rgba(color: u32, enabled: bool) -> Rgba {
    Rgba::new(
        color as u8,
        (color >> 8) as u8,
        (color >> 16) as u8,
        if enabled { 255 } else { 102 },
    )
}
/// Returns true when this is one of the appearance-specific compound rows.
pub(super) fn draw_row(
    ctx: &Ctx,
    row: &RowLay,
    t: &Theme,
    dpi: u32,
    settings: &Settings,
) -> Result<bool> {
    if channel(&row.row).is_none() && slider(&row.row).is_none() {
        return Ok(false);
    }
    let r = control_rect(row, dpi)?;
    let stack = row.w < scf(488.0, dpi);
    let right = if stack {
        row.x + row.w
    } else {
        r.left - scf(16.0, dpi)
    };
    let fg = super::with_alpha(t.nav_text, if row.enabled { 255 } else { 102 });
    let secondary = super::with_alpha(t.sub_header_text, if row.enabled { 255 } else { 102 });
    let mut yy = row.y + scf(10.0, dpi);
    for line in wrap(row.row.text, scf(14.0, dpi), 400, right - row.x)? {
        ctx.draw_text_cached(
            &line,
            RectF::new(row.x, yy, right, yy + scf(20.0, dpi)),
            &style(
                kit::theme::font_family(),
                scf(14.0, dpi),
                400,
                fg,
                Align::Left,
            ),
        )?;
        yy += scf(20.0, dpi);
    }
    if let Some(desc) = row.row.desc {
        yy += scf(2.0, dpi);
        for line in wrap(desc, scf(12.0, dpi), 400, right - row.x)? {
            ctx.draw_text_cached(
                &line,
                RectF::new(row.x, yy, right, yy + scf(16.0, dpi)),
                &style(
                    kit::theme::font_family(),
                    scf(12.0, dpi),
                    400,
                    secondary,
                    Align::Left,
                ),
            )?;
            yy += scf(16.0, dpi);
        }
    }
    let m = effective(settings);
    if let Some(ch) = channel(&row.row) {
        let current = color(&m, ch);
        let selected = selected_swatch(ch, current, m.text_auto);
        let controls = swatches(row, dpi)?;
        for (choice, swatch) in controls {
            if choice == ColorChoice::Eyedropper {
                kit_draw(
                    ctx,
                    kit::controls::Control::IconButton,
                    swatch,
                    "",
                    row.enabled,
                    false,
                    t,
                    dpi,
                    0.0,
                )?;
                ctx.draw_text_cached(
                    "\u{EF3C}",
                    swatch,
                    &style(super::icon_family(), scf(16.0, dpi), 400, fg, Align::Center),
                )?;
                continue;
            }
            let custom = choice == ColorChoice::Custom;
            let auto = choice == ColorChoice::Auto;
            kit_draw(
                ctx,
                if auto {
                    kit::controls::Control::AutoSwatch
                } else if custom {
                    kit::controls::Control::CustomSwatch
                } else {
                    kit::controls::Control::Swatch
                },
                swatch,
                "",
                row.enabled,
                choice == selected,
                t,
                dpi,
                0.0,
            )?;
            if !auto && (!custom || selected == ColorChoice::Custom) {
                let c = if let ColorChoice::Preset(i) = choice {
                    preset(ch, i).unwrap()
                } else {
                    current
                };
                let inset = scf(4.0, dpi);
                ctx.fill_rounded_rect(
                    RectF::new(
                        swatch.left + inset,
                        swatch.top + inset,
                        swatch.right - inset,
                        swatch.bottom - inset,
                    ),
                    scf(10.0, dpi),
                    rgba(c, row.enabled),
                )?;
                ctx.stroke_rounded_rect(
                    RectF::new(
                        swatch.left + inset,
                        swatch.top + inset,
                        swatch.right - inset,
                        swatch.bottom - inset,
                    ),
                    scf(10.0, dpi),
                    scf(1.0, dpi),
                    super::with_alpha(
                        kit::theme::Theme::new(t.light).border_strong,
                        if row.enabled { 255 } else { 102 },
                    ),
                )?;
            }
            if custom && selected != ColorChoice::Custom {
                // Small dash segments around the empty Custom swatch preserve the standard colour-row affordance.
                let cx = (swatch.left + swatch.right) / 2.0;
                let cy = (swatch.top + swatch.bottom) / 2.0;
                ctx.stroke_rounded_rect(
                    RectF::new(
                        cx - scf(10.0, dpi),
                        cy - scf(10.0, dpi),
                        cx + scf(10.0, dpi),
                        cy + scf(10.0, dpi),
                    ),
                    scf(10.0, dpi),
                    scf(2.0, dpi),
                    t.group_fill,
                )?;
                for i in 0..16 {
                    let a = i as f32 * std::f32::consts::TAU / 16.0;
                    let x = cx + a.cos() * scf(10.0, dpi);
                    let y = cy + a.sin() * scf(10.0, dpi);
                    ctx.fill_rounded_rect(
                        RectF::new(
                            x - scf(0.8, dpi),
                            y - scf(0.8, dpi),
                            x + scf(0.8, dpi),
                            y + scf(0.8, dpi),
                        ),
                        scf(0.7, dpi),
                        secondary,
                    )?;
                }
            }
        }
        let dx = r.right - scf(49.0, dpi);
        ctx.fill_rect(
            RectF::new(
                dx,
                r.top + scf(6.0, dpi),
                dx + scf(1.0, dpi),
                r.bottom - scf(6.0, dpi),
            ),
            kit::theme::Theme::new(t.light).border_subtle,
        )?;
    } else if let Some(kind) = slider(&row.row) {
        let (track, chip) = slider_rects(row, dpi)?;
        let (value, label) = match kind {
            SliderKind::SeeThrough => {
                let v = see_through_percent(m.see_through);
                (v as f32 / 100.0, format!("{v} %"))
            }
            SliderKind::BlendStrength => {
                let v = blend_percent(settings.desktop_blend_icon_opacity);
                (v as f32 / 100.0, format!("{v} %"))
            }
            SliderKind::Blur => {
                let index = nearest_blur(if settings.background_blur_enabled {
                    settings.background_blur_radius
                } else {
                    0.0
                });
                (index as f32 / 4.0, BLUR_NAMES[index].to_owned())
            }
        };
        let value = SLIDER_POINTER
            .with(|p| p.get())
            .filter(|(k, _)| *k == kind)
            .map(|(_, x)| {
                ((x - track.left - scf(10.0, dpi)) / (track.right - track.left - scf(20.0, dpi)))
                    .clamp(0.0, 1.0)
            })
            .unwrap_or(value);
        // While dragging, Blur shows the step under the pointer; the value is applied on release.
        let dragging = SLIDER_POINTER.with(|p| p.get()).is_some_and(|(k, _)| k == kind);
        let label = if kind == SliderKind::Blur && dragging {
            BLUR_NAMES[(value * 4.0).round() as usize].to_owned()
        } else {
            label
        };
        kit_draw(
            ctx,
            if kind == SliderKind::Blur {
                kit::controls::Control::StepSlider
            } else {
                kit::controls::Control::Slider
            },
            track,
            "",
            row.enabled,
            false,
            t,
            dpi,
            value,
        )?;
        kit_draw(
            ctx,
            kit::controls::Control::ValueChip,
            chip,
            &label,
            row.enabled,
            false,
            t,
            dpi,
            0.0,
        )?;
    }
    Ok(true)
}

#[cfg(test)]
mod long_scope_tests {
    use super::*;
    #[test]
    fn long_scope_caption_wraps_every_character_and_reset_never_overlaps_dropdown() -> Result<()> {
        let title = "W".repeat(255);
        let settings = Settings::default();
        set_scope(Some(title), vec![], true);
        set_effective(&color_state::effective_of(&settings, &Default::default()));
        for dpi in [96, 120, 144, 240] {
            for width in [440.0, 680.0] {
                let lay = scope_layout(
                    scf(280.0, dpi),
                    scf(280.0 + width, dpi),
                    scf(76.0, dpi),
                    dpi,
                )?;
                let flattened: String = lay
                    .caption
                    .iter()
                    .flat_map(|(s, _)| s.chars())
                    .filter(|c| !c.is_whitespace())
                    .collect();
                let expected: String = scope_caption()
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                assert_eq!(flattened, expected);
                for (line, r) in &lay.caption {
                    assert!(
                        crate::render::measure_text(
                            line,
                            kit::theme::font_family(),
                            scf(12.0, dpi),
                            400
                        )?
                        .0 <= r.right - r.left + 1.0
                    );
                }
                assert!(
                    lay.reset.top >= lay.dropdown.bottom
                        || lay.reset.left >= lay.dropdown.right + scf(12.0, dpi) - 0.1
                );
            }
        }
        set_scope(None, vec![], false);
        Ok(())
    }
}
