//! Reusable draw/hit primitives. Bounds are DIPs; snapping happens once at this API boundary.
//! Control state belongs to the page/window, never a native child widget.
#[cfg(test)]
use super::layout::snap;
use super::{
    layout::{contains, inset, snap_rect},
    theme::{Theme, Type},
};
use crate::render::{Align, Ctx, RectF, Rgba};
use windows::core::Result;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonStyle {
    Primary,
    Secondary,
    Subtle,
    Danger,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Nav,
    GroupLabel,
    PageHeader,
    SectionLabel,
    Card,
    SettingRow,
    Toggle,
    Checkbox,
    Radio,
    RadioRow,
    CheckRow,
    Button(ButtonStyle),
    IconButton,
    Link,
    Dropdown,
    Menu,
    MenuItem,
    Segmented,
    Slider,
    StepSlider,
    SliderRow,
    ValueChip,
    Swatch,
    CustomSwatch,
    AutoSwatch,
    ColorRow,
    Stepper,
    InfoBox,
    WarningBox,
    Expander,
}
#[derive(Clone, Copy, Debug)]
pub struct VisualState {
    pub enabled: bool,
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
    pub selected: bool,
}
impl Default for VisualState {
    fn default() -> Self {
        Self {
            enabled: true,
            hovered: false,
            pressed: false,
            focused: false,
            selected: false,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Hit {
    pub part: usize,
}
/// Whole-row hit targets are supplied as bounds for Toggle, RadioRow, CheckRow and SettingRow.
/// Segmented/ColorRow split into `parts` equal targets. Stepper always returns
/// 0=minus, 1=value, 2=plus using the same fixed end-button geometry as drawing.
pub fn hit_control(
    kind: Control,
    bounds: RectF,
    x_px: f32,
    y_px: f32,
    dpi: u32,
    enabled: bool,
    parts: usize,
) -> Option<Hit> {
    if !enabled || !interactive(kind) {
        return None;
    }
    let r = snap_rect(bounds, dpi);
    if !contains(r, x_px, y_px) {
        return None;
    }
    if kind == Control::Stepper {
        return stepper_parts(bounds, dpi)
            .iter()
            .position(|part| contains(*part, x_px, y_px))
            .map(|part| Hit { part });
    }
    let split = matches!(kind, Control::Segmented | Control::ColorRow);
    let part = if split && parts > 0 {
        (((x_px - r.left) / (r.right - r.left) * parts as f32).floor() as usize).min(parts - 1)
    } else {
        0
    };
    Some(Hit { part })
}
/// Device-pixel rectangles shared by painting and hit testing; caller supplies DIP bounds.
pub fn stepper_parts(bounds: RectF, dpi: u32) -> [RectF; 3] {
    let r = snap_rect(bounds, dpi);
    let side = (32.0 * dpi as f32 / 96.0)
        .round()
        .min((r.right - r.left) / 2.0);
    [
        RectF::new(r.left, r.top, r.left + side, r.bottom),
        RectF::new(r.left + side, r.top, r.right - side, r.bottom),
        RectF::new(r.right - side, r.top, r.right, r.bottom),
    ]
}

/// Five named-step ticks below a 24-DIP slider, snapped once to device pixels.
pub fn slider_ticks(bounds: RectF, dpi: u32) -> [RectF; 5] {
    std::array::from_fn(|i| {
        let x = bounds.left + 10.0 + (bounds.right - bounds.left - 20.0) * i as f32 / 4.0;
        snap_rect(
            RectF::new(x - 1.0, bounds.top + 26.0, x + 1.0, bounds.top + 32.0),
            dpi,
        )
    })
}

pub fn interactive(kind: Control) -> bool {
    !matches!(
        kind,
        Control::GroupLabel
            | Control::PageHeader
            | Control::SectionLabel
            | Control::Card
            | Control::Menu
            | Control::SliderRow
            | Control::ValueChip
            | Control::InfoBox
            | Control::WarningBox
    )
}
pub fn preferred_size(kind: Control) -> (f32, f32) {
    match kind {
        Control::Nav => (224., 36.),
        Control::Toggle => (40., 20.),
        Control::Checkbox | Control::Radio => (20., 20.),
        Control::IconButton => (32., 32.),
        Control::Dropdown => (220., 32.),
        Control::Slider => (240., 24.),
        Control::StepSlider => (240., 32.),
        Control::Swatch | Control::CustomSwatch | Control::AutoSwatch => (28., 28.),
        Control::ValueChip => (72., 24.),
        Control::Expander => (220., 48.),
        Control::Stepper => (104., 32.),
        _ => (80., 32.),
    }
}
fn text(
    ctx: &Ctx,
    label: &str,
    r: RectF,
    ty: Type,
    dpi: u32,
    color: Rgba,
    align: Align,
) -> Result<()> {
    let mut style = ty.style(dpi, color);
    style.align = align;
    ctx.draw_text_cached(label, r, &style)
}
/// Draw one primitive. `value` is an unrounded 0..1 slider fraction; other controls ignore it.
/// Composed rows draw their text, descriptions, chip and trailing primitive separately.
pub fn draw_control(
    ctx: &Ctx,
    kind: Control,
    bounds: RectF,
    label: &str,
    state: VisualState,
    theme: &Theme,
    dpi: u32,
    value: f32,
) -> Result<()> {
    let r = snap_rect(bounds, dpi);
    let s = dpi as f32 / 96.;
    let c = |v| theme.color(v, state.enabled);
    let hover = state.hovered || state.pressed;
    let radius = 4. * s;
    let (fill, border) = match kind {
        Control::Card | Control::Menu | Control::Expander => {
            (Some(theme.surface), Some(theme.border))
        }
        Control::InfoBox => (Some(theme.surface_raised), None),
        Control::WarningBox => (Some(theme.accent_subtle), None),
        Control::Button(ButtonStyle::Primary) => (
            Some(if hover {
                theme.accent_hover
            } else {
                theme.accent
            }),
            None,
        ),
        Control::Button(ButtonStyle::Danger) => (Some(theme.danger), None),
        Control::Button(ButtonStyle::Secondary) | Control::Dropdown | Control::Stepper => (
            Some(if hover {
                theme.control_hover
            } else {
                theme.control
            }),
            Some(if state.pressed {
                theme.accent
            } else {
                theme.border
            }),
        ),
        Control::Segmented => (Some(theme.surface_raised), Some(theme.border)),
        Control::ValueChip => (Some(theme.subtle_selected), None),
        Control::Nav | Control::MenuItem => (
            if state.selected {
                Some(theme.subtle_selected)
            } else if hover {
                Some(theme.subtle_hover)
            } else {
                None
            },
            None,
        ),
        Control::SettingRow
        | Control::RadioRow
        | Control::CheckRow
        | Control::Button(ButtonStyle::Subtle)
        | Control::IconButton => (
            if hover {
                Some(theme.subtle_hover)
            } else {
                None
            },
            None,
        ),
        _ => (None, None),
    };
    let rad = if matches!(
        kind,
        Control::Card | Control::Menu | Control::InfoBox | Control::WarningBox | Control::Expander
    ) {
        8. * s
    } else {
        radius
    };
    if let Some(fill) = fill {
        ctx.fill_rounded_rect(r, rad, c(fill))?;
    }
    if let Some(border) = border {
        ctx.stroke_rounded_rect(inset(r, s / 2.), rad, s, c(border))?;
    }
    let mid_y = (r.top + r.bottom) / 2.;
    let mid_x = (r.left + r.right) / 2.;
    match kind {
        Control::Toggle => {
            if state.selected || hover {
                ctx.fill_rounded_rect(
                    r,
                    10. * s,
                    c(if state.selected {
                        if hover {
                            theme.accent_hover
                        } else {
                            theme.accent
                        }
                    } else {
                        theme.subtle_hover
                    }),
                )?;
            }
            if !state.selected {
                ctx.stroke_rounded_rect(inset(r, s / 2.), 10. * s, s, c(theme.control_strong))?;
            }
            let knob = if hover { 14. } else { 12. } * s;
            let x = if state.selected {
                r.right - 10. * s
            } else {
                r.left + 10. * s
            };
            ctx.fill_rounded_rect(
                RectF::new(
                    x - knob / 2.,
                    mid_y - knob / 2.,
                    x + knob / 2.,
                    mid_y + knob / 2.,
                ),
                knob / 2.,
                c(if state.selected {
                    theme.on_accent
                } else {
                    theme.control_strong
                }),
            )?;
        }
        Control::Checkbox | Control::Radio => {
            let rad = if kind == Control::Radio {
                10. * s
            } else {
                4. * s
            };
            ctx.fill_rounded_rect(
                r,
                rad,
                c(if state.selected {
                    theme.accent
                } else {
                    theme.control
                }),
            )?;
            if !state.selected {
                ctx.stroke_rounded_rect(inset(r, s / 2.), rad, s, c(theme.control_strong))?;
            } else if kind == Control::Radio {
                ctx.fill_rounded_rect(
                    RectF::new(
                        mid_x - 4. * s,
                        mid_y - 4. * s,
                        mid_x + 4. * s,
                        mid_y + 4. * s,
                    ),
                    4. * s,
                    c(theme.on_accent),
                )?;
            } else {
                text(
                    ctx,
                    "✓",
                    r,
                    Type::BodyStrong,
                    dpi,
                    c(theme.on_accent),
                    Align::Center,
                )?;
            }
        }
        Control::Slider | Control::StepSlider => {
            let mid_y = if kind == Control::StepSlider {
                r.top + 12.0 * s
            } else {
                mid_y
            };
            if kind == Control::StepSlider {
                for tick in slider_ticks(bounds, dpi) {
                    ctx.fill_rect(tick, c(theme.control_strong))?;
                }
            }
            let start = r.left + 10. * s;
            let end = r.right - 10. * s;
            let x = start + value.clamp(0., 1.) * (end - start);
            ctx.fill_rounded_rect(
                RectF::new(start, mid_y - 2. * s, end, mid_y + 2. * s),
                2. * s,
                c(theme.control_track),
            )?;
            ctx.fill_rounded_rect(
                RectF::new(start, mid_y - 2. * s, x, mid_y + 2. * s),
                2. * s,
                c(theme.accent),
            )?;
            let thumb = RectF::new(x - 10. * s, mid_y - 10. * s, x + 10. * s, mid_y + 10. * s);
            ctx.fill_rounded_rect(thumb, 10. * s, c(theme.control))?;
            ctx.stroke_rounded_rect(thumb, 10. * s, s, c(theme.border))?;
            let dot = if state.pressed {
                10.
            } else if hover {
                14.
            } else {
                12.
            } * s;
            ctx.fill_rounded_rect(
                RectF::new(
                    x - dot / 2.,
                    mid_y - dot / 2.,
                    x + dot / 2.,
                    mid_y + dot / 2.,
                ),
                dot / 2.,
                c(theme.accent),
            )?;
        }
        Control::Swatch | Control::CustomSwatch | Control::AutoSwatch => {
            let circle = RectF::new(
                mid_x - 10. * s,
                mid_y - 10. * s,
                mid_x + 10. * s,
                mid_y + 10. * s,
            );
            ctx.fill_rounded_rect(
                circle,
                10. * s,
                c(if kind == Control::AutoSwatch {
                    Rgba::new(255, 255, 255, 255)
                } else {
                    theme.control
                }),
            )?;
            if kind == Control::AutoSwatch {
                ctx.with_clip(RectF::new(mid_x, r.top, r.right, r.bottom), |ctx| {
                    ctx.fill_rounded_rect(circle, 10. * s, c(Rgba::new(27, 29, 34, 255)))
                })?;
            }
            ctx.stroke_rounded_rect(circle, 10. * s, s, c(theme.border_strong))?;
            if kind == Control::CustomSwatch {
                text(
                    ctx,
                    "+",
                    circle,
                    Type::Caption,
                    dpi,
                    c(theme.control_strong),
                    Align::Center,
                )?;
            }
            if hover || state.selected {
                ctx.stroke_rounded_rect(
                    inset(r, s),
                    14. * s,
                    if state.selected { 2. * s } else { s },
                    c(if state.selected {
                        theme.text
                    } else {
                        theme.border_strong
                    }),
                )?;
            }
        }
        Control::Stepper => {
            let [minus, center, plus] = stepper_parts(bounds, dpi);
            text(
                ctx,
                "−",
                minus,
                Type::Body,
                dpi,
                c(theme.text),
                Align::Center,
            )?;
            text(
                ctx,
                label,
                center,
                Type::Body,
                dpi,
                c(theme.text),
                Align::Center,
            )?;
            text(
                ctx,
                "+",
                plus,
                Type::Body,
                dpi,
                c(theme.text),
                Align::Center,
            )?;
        }
        Control::Dropdown => {
            text(
                ctx,
                label,
                RectF::new(r.left + 12. * s, r.top, r.right - 32. * s, r.bottom),
                Type::Body,
                dpi,
                c(theme.text),
                Align::Left,
            )?;
            // ChevronDown from the icon font; "⌄" in the body font renders as a plain "v".
            let mut chevron = Type::Caption.style(dpi, c(theme.secondary));
            chevron.family = super::super::icon_family();
            chevron.align = Align::Center;
            ctx.draw_text_cached("\u{E70D}", RectF::new(r.right - 24. * s, r.top, r.right - 8. * s, r.bottom), &chevron)?;
        }
        _ => {
            if matches!(kind, Control::Nav | Control::MenuItem) && state.selected {
                ctx.fill_rounded_rect(
                    RectF::new(r.left, mid_y - 8. * s, r.left + 3. * s, mid_y + 8. * s),
                    1.5 * s,
                    c(theme.accent),
                )?;
            }
            let ty = match kind {
                Control::PageHeader => Type::PageTitle,
                Control::SectionLabel => Type::Section,
                Control::GroupLabel => Type::Group,
                Control::ValueChip => Type::Caption,
                Control::Nav | Control::MenuItem if state.selected => Type::BodyStrong,
                _ => Type::Body,
            };
            let foreground = match kind {
                Control::Button(ButtonStyle::Primary | ButtonStyle::Danger) => theme.on_accent,
                Control::Link => theme.link,
                Control::GroupLabel | Control::ValueChip => theme.secondary,
                _ => theme.text,
            };
            let align = if matches!(kind, Control::Button(_) | Control::IconButton) {
                Align::Center
            } else if kind == Control::ValueChip {
                Align::Right
            } else {
                Align::Left
            };
            text(
                ctx,
                label,
                if kind==Control::ValueChip {
                    RectF::new(r.left+8.*s,r.top,r.right-8.*s,r.bottom)
                }else{inset(r,if matches!(kind,Control::PageHeader|Control::SectionLabel){0.}else{8.*s})},
                ty,
                dpi,
                c(foreground),
                align,
            )?;
            if kind == Control::Link && hover {
                ctx.fill_rect(
                    RectF::new(r.left, r.bottom - s, r.right, r.bottom),
                    c(theme.link),
                )?;
            }
        }
    }
    if state.focused && state.enabled {
        let ring = RectF::new(
            r.left - 3. * s,
            r.top - 3. * s,
            r.right + 3. * s,
            r.bottom + 3. * s,
        );
        ctx.stroke_rounded_rect(ring, rad + 3. * s, 2. * s, theme.focus)?;
    }
    Ok(())
}
/// Compose the model with the appropriate primitive; preserve its unsnapped drag fraction.
pub fn draw_slider_model(
    ctx: &Ctx,
    bounds: RectF,
    model: &super::slider::SliderModel,
    state: VisualState,
    theme: &Theme,
    dpi: u32,
) -> Result<()> {
    let kind = match &model.scale {
        super::slider::SliderScale::Steps(_) => Control::StepSlider,
        super::slider::SliderScale::Continuous { .. } => Control::Slider,
    };
    draw_control(ctx, kind, bounds, "", state, theme, dpi, model.fraction)
}

/// Equal segment widths are based on the widest measured label, never truncate a label.
pub fn segmented_width(measured_labels: &[f32]) -> f32 {
    let width = measured_labels
        .iter()
        .copied()
        .fold(96_f32, |a, b| a.max(b + 16.));
    4. + width * measured_labels.len() as f32 + 2. * measured_labels.len().saturating_sub(1) as f32
}
pub fn draw_segmented(
    ctx: &Ctx,
    bounds: RectF,
    labels: &[&str],
    selected: usize,
    hovered: Option<usize>,
    state: VisualState,
    theme: &Theme,
    dpi: u32,
) -> Result<()> {
    draw_control(ctx, Control::Segmented, bounds, "", state, theme, dpi, 0.)?;
    if labels.is_empty() {
        return Ok(());
    }
    let gap = 2.;
    let width =
        ((bounds.right - bounds.left) - 4. - gap * (labels.len() - 1) as f32) / labels.len() as f32;
    for (i, label) in labels.iter().enumerate() {
        let left = bounds.left + 2. + i as f32 * (width + gap);
        let r = snap_rect(
            RectF::new(left, bounds.top + 2., left + width, bounds.bottom - 2.),
            dpi,
        );
        if i == selected {
            ctx.fill_rounded_rect(
                r,
                4. * dpi as f32 / 96.,
                theme.color(theme.control, state.enabled),
            )?;
            ctx.stroke_rounded_rect(
                inset(r, 0.5 * dpi as f32 / 96.),
                4. * dpi as f32 / 96.,
                dpi as f32 / 96.,
                theme.color(theme.border, state.enabled),
            )?;
            let mid = left + width / 2.;
            ctx.fill_rounded_rect(
                snap_rect(
                    RectF::new(mid - 8., bounds.bottom - 7., mid + 8., bounds.bottom - 4.),
                    dpi,
                ),
                1.5 * dpi as f32 / 96.,
                theme.color(theme.accent, state.enabled),
            )?;
        } else if hovered == Some(i) {
            ctx.fill_rounded_rect(
                r,
                4. * dpi as f32 / 96.,
                theme.color(theme.subtle_hover, state.enabled),
            )?;
        }
        text(
            ctx,
            label,
            r,
            if i == selected {
                Type::BodyStrong
            } else {
                Type::Body
            },
            dpi,
            theme.color(
                if i == selected {
                    theme.text
                } else {
                    theme.secondary
                },
                state.enabled,
            ),
            Align::Center,
        )?;
    }
    Ok(())
}
pub fn draw_swatch(
    ctx: &Ctx,
    bounds: RectF,
    color: Rgba,
    kind: Control,
    state: VisualState,
    theme: &Theme,
    dpi: u32,
) -> Result<()> {
    let x = (bounds.left + bounds.right) / 2.;
    let y = (bounds.top + bounds.bottom) / 2.;
    let r = snap_rect(RectF::new(x - 10., y - 10., x + 10., y + 10.), dpi);
    ctx.fill_rounded_rect(r, 10. * dpi as f32 / 96., theme.color(color, state.enabled))?;
    if kind == Control::CustomSwatch {
        // Dashed 2-on/2-off edge approximated by short circular chords, with no native control.
        let scale = dpi as f32 / 96.;
        let center = ((r.left + r.right) / 2., (r.top + r.bottom) / 2.);
        for i in 0..16 {
            let angle = i as f32 * std::f32::consts::TAU / 16.;
            let cx = center.0 + 10. * scale * angle.cos();
            let cy = center.1 + 10. * scale * angle.sin();
            ctx.fill_rounded_rect(
                RectF::new(
                    cx - scale * 0.7,
                    cy - scale * 0.7,
                    cx + scale * 0.7,
                    cy + scale * 0.7,
                ),
                scale * 0.7,
                theme.color(theme.control_strong, state.enabled),
            )?;
        }
        text(
            ctx,
            "+",
            r,
            Type::Caption,
            dpi,
            theme.color(theme.control_strong, state.enabled),
            Align::Center,
        )?;
    } else {
        ctx.stroke_rounded_rect(
            r,
            10. * dpi as f32 / 96.,
            dpi as f32 / 96.,
            theme.color(theme.border_strong, state.enabled),
        )?;
    }
    if state.selected || state.hovered {
        ctx.stroke_rounded_rect(
            snap_rect(bounds, dpi),
            14. * dpi as f32 / 96.,
            if state.selected { 2. } else { 1. } * dpi as f32 / 96.,
            theme.color(
                if state.selected {
                    theme.text
                } else {
                    theme.border_strong
                },
                state.enabled,
            ),
        )?;
    }
    Ok(())
}
/// Card dividers are inset 16 DIPs and omit the last row by construction.
pub fn draw_dividers(
    ctx: &Ctx,
    card: RectF,
    row_bottoms: &[f32],
    theme: &Theme,
    dpi: u32,
) -> Result<()> {
    for y in row_bottoms.iter().take(row_bottoms.len().saturating_sub(1)) {
        let r = snap_rect(
            RectF::new(card.left + 16., *y, card.right - 16., *y + 1.),
            dpi,
        );
        ctx.fill_rect(r, theme.border_subtle)?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_control_has_consistent_dpi_hit_targets_and_disabled_state() {
        let all = [
            Control::Nav,
            Control::GroupLabel,
            Control::PageHeader,
            Control::SectionLabel,
            Control::Card,
            Control::SettingRow,
            Control::Toggle,
            Control::Checkbox,
            Control::Radio,
            Control::RadioRow,
            Control::CheckRow,
            Control::Button(ButtonStyle::Primary),
            Control::Button(ButtonStyle::Secondary),
            Control::Button(ButtonStyle::Subtle),
            Control::Button(ButtonStyle::Danger),
            Control::IconButton,
            Control::Link,
            Control::Dropdown,
            Control::Menu,
            Control::MenuItem,
            Control::Segmented,
            Control::Slider,
            Control::StepSlider,
            Control::SliderRow,
            Control::ValueChip,
            Control::Swatch,
            Control::CustomSwatch,
            Control::AutoSwatch,
            Control::ColorRow,
            Control::Stepper,
            Control::InfoBox,
            Control::WarningBox,
            Control::Expander,
        ];
        for dpi in [96, 120, 144, 240] {
            for kind in all {
                let r = RectF::new(10., 20., 210., 60.);
                let x = snap(110., dpi);
                let y = snap(40., dpi);
                assert_eq!(
                    hit_control(kind, r, x, y, dpi, true, 3).is_some(),
                    interactive(kind),
                    "{kind:?} at {dpi}"
                );
                assert!(hit_control(kind, r, x, y, dpi, false, 3).is_none());
                assert!(hit_control(kind, r, 0., 0., dpi, true, 3).is_none());
            }
        }
    }
    #[test]
    fn u1_fix_stepper_hit_regions_follow_fixed_painted_end_buttons() {
        for dpi in [96, 120, 144, 240] {
            let bounds = RectF::new(10.0, 20.0, 114.0, 52.0);
            let parts = stepper_parts(bounds, dpi);
            assert_eq!(parts[0].right, snap(42.0, dpi));
            assert_eq!(parts[1].right, snap(82.0, dpi));
            for count in [0, 2, 3] {
                for (i, part) in parts.iter().enumerate() {
                    for x in [part.left, (part.left + part.right) / 2.0, part.right - 1.0] {
                        assert_eq!(
                            hit_control(
                                Control::Stepper,
                                bounds,
                                x,
                                snap(36.0, dpi),
                                dpi,
                                true,
                                count
                            )
                            .unwrap()
                            .part,
                            i
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn u1_fix_stepped_slider_draws_five_ticks_below_continuous_track() -> Result<()> {
        use crate::render::Surface;
        assert_eq!(preferred_size(Control::StepSlider), (240.0, 32.0));
        for dpi in [96, 120, 144, 240] {
            for light in [true, false] {
                let theme = Theme::new(light);
                let bounds = RectF::new(16.0, 8.0, 256.0, 40.0);
                let mut surface = Surface::new(snap(280.0, dpi) as u32, snap(60.0, dpi) as u32)?;
                surface.draw(|ctx| {
                    ctx.clear(theme.window);
                    {
                        let mut model = super::super::slider::SliderModel::new(
                            super::super::slider::SliderScale::Steps(vec![0, 15, 30, 60, 120]),
                            30,
                        );
                        model.drag(0.37, 0.0, 1.0);
                        draw_slider_model(ctx, bounds, &model, VisualState::default(), &theme, dpi)
                    }
                })?;
                for tick in slider_ticks(bounds, dpi) {
                    let x = ((tick.left + tick.right) / 2.0) as usize;
                    let y = ((tick.top + tick.bottom) / 2.0) as usize;
                    let offset = (y * surface.width() as usize + x) * 4;
                    let c = theme.control_strong;
                    assert_eq!(&surface.pixels()[offset..offset + 4], &[c.b, c.g, c.r, c.a]);
                }
            }
        }
        Ok(())
    }

    #[test]
    fn segmented_widens_for_long_copy() {
        assert_eq!(segmented_width(&[40., 70., 60.]), 296.);
        assert_eq!(segmented_width(&[140., 30.]), 318.);
    }
    #[test]
    fn offscreen_kit_goldens_and_slider_render_benchmark() -> Result<()> {
        use crate::render::Surface;
        let output = std::path::Path::new("target/qa/settings-redesign/u1");
        std::fs::create_dir_all(output).expect("create QA output");
        let kinds = [
            Control::Nav,
            Control::Toggle,
            Control::Checkbox,
            Control::Radio,
            Control::Button(ButtonStyle::Primary),
            Control::Button(ButtonStyle::Secondary),
            Control::Button(ButtonStyle::Subtle),
            Control::Button(ButtonStyle::Danger),
            Control::IconButton,
            Control::Link,
            Control::Dropdown,
            Control::MenuItem,
            Control::Segmented,
            Control::Slider,
            Control::StepSlider,
            Control::Swatch,
            Control::CustomSwatch,
            Control::AutoSwatch,
            Control::Stepper,
            Control::InfoBox,
            Control::WarningBox,
            Control::Expander,
        ];
        for dpi in [96, 120, 144, 240] {
            for light in [true, false] {
                let theme = Theme::new(light);
                let mut surface = Surface::new(snap(2000., dpi) as u32, snap(1100., dpi) as u32)?;
                let draw_sheet = |ctx: &Ctx| -> Result<()> {
                    ctx.clear(theme.window);
                    for (column, label) in
                        ["Rest", "Hover", "Pressed", "Selected", "Disabled", "Focus"]
                            .iter()
                            .enumerate()
                    {
                        let left = 16. + column as f32 * 330.;
                        text(
                            ctx,
                            label,
                            snap_rect(RectF::new(left, 0., left + 300., 36.), dpi),
                            Type::Caption,
                            dpi,
                            theme.text,
                            Align::Left,
                        )?;
                    }
                    for (row, kind) in kinds.iter().enumerate() {
                        for col in 0..6 {
                            let state = VisualState {
                                hovered: col == 1,
                                pressed: col == 2,
                                selected: col == 3,
                                enabled: col != 4,
                                focused: col == 5,
                            };
                            let (mut w, h) = preferred_size(*kind);
                            if matches!(kind, Control::Link | Control::MenuItem) {
                                w = 160.;
                            }
                            if *kind == Control::Segmented {
                                w = 296.;
                            }
                            let left = 16. + col as f32 * 330.;
                            let top = 44. + row as f32 * 48.;
                            let label = match kind {
                                Control::Button(_) => "Button",
                                Control::IconButton => "+",
                                Control::Dropdown => "Dropdown",
                                Control::Nav => "Backups",
                                Control::MenuItem => "Menu item",
                                Control::Expander => "How it works",
                                Control::Link => "Open folder",
                                Control::Stepper => "55",
                                _ => "Control",
                            };
                            let bounds = RectF::new(left, top, left + w, top + h.min(36.));
                            if *kind == Control::Segmented {
                                draw_segmented(
                                    ctx,
                                    bounds,
                                    &["One", "Two", "Three"],
                                    if state.selected { 1 } else { 0 },
                                    if state.hovered { Some(2) } else { None },
                                    state,
                                    &theme,
                                    dpi,
                                )?;
                                continue;
                            }
                            if *kind == Control::Swatch {
                                draw_swatch(
                                    ctx,
                                    bounds,
                                    Rgba::new(185, 122, 51, 255),
                                    *kind,
                                    state,
                                    &theme,
                                    dpi,
                                )?;
                                continue;
                            }
                            if *kind == Control::StepSlider {
                                let mut model = super::super::slider::SliderModel::new(
                                    super::super::slider::SliderScale::Steps(vec![
                                        0, 15, 30, 60, 120,
                                    ]),
                                    30,
                                );
                                model.drag(0.43, 0.0, 1.0);
                                draw_slider_model(ctx, bounds, &model, state, &theme, dpi)?;
                            } else {
                                draw_control(ctx, *kind, bounds, label, state, &theme, dpi, 0.43)?;
                            }
                        }
                    }
                    Ok(())
                };
                surface.draw(draw_sheet)?;
                surface.save_png(&output.join(format!(
                    "kit-{}-{dpi}.png",
                    if light { "light" } else { "dark" }
                )))?;
                if light && [96, 240].contains(&dpi) {
                    // This measures offscreen software drawing only, not WM_MOUSEMOVE or presentation.
                    // Warm the DirectWrite/Direct2D caches before recording the sample.
                    surface.draw(draw_sheet)?;
                    let start = std::time::Instant::now();
                    for _ in 0..8 {
                        surface.draw(draw_sheet)?;
                    }
                    let full = start.elapsed().as_secs_f64() * 1000. / 8.;
                    let slider_bounds =
                        RectF::new(16., 44. + 13. * 48., 256., 44. + 13. * 48. + 24.);
                    let pixel_row = snap_rect(
                        RectF::new(0., slider_bounds.top - 6., 2000., slider_bounds.bottom + 6.),
                        dpi,
                    );
                    let start = std::time::Instant::now();
                    for _ in 0..64 {
                        surface.draw(|ctx| {
                            ctx.with_clip(pixel_row, |ctx| {
                                ctx.fill_rect(pixel_row, theme.window)?;
                                draw_control(
                                    ctx,
                                    Control::Slider,
                                    slider_bounds,
                                    "",
                                    VisualState {
                                        pressed: true,
                                        ..Default::default()
                                    },
                                    &theme,
                                    dpi,
                                    0.437,
                                )
                            })
                        })?;
                    }
                    let row = start.elapsed().as_secs_f64() * 1000. / 64.;
                    let report = format!(
                        "dpi={dpi}, full_kit_sheet_ms={full:.4}, slider_row_ms={row:.4}, full_iterations=8, row_iterations=64\n"
                    );
                    println!("{report}");
                    std::fs::write(output.join(format!("slider-benchmark-{dpi}.txt")), report)
                        .expect("write benchmark");
                }
            }
        }
        Ok(())
    }
}
