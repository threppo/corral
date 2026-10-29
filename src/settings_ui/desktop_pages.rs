//! U2 page-specific compound rows. Geometry is shared by rendering, hit testing and focus.
//! These are Corral designs from settings-redesign-spec section 7.
use super::*;

pub(super) fn owns(page: Page) -> bool {
    matches!(
        page,
        Page::Welcome | Page::Tabs | Page::Hide | Page::Peek | Page::Pages | Page::Layout
    )
}
pub(super) fn grid(kind: RowKind) -> bool {
    matches!(kind, RowKind::ExploreTile | RowKind::TabStyleTile)
}
pub(super) fn custom(kind: RowKind) -> bool {
    grid(kind) || matches!(kind, RowKind::Status | RowKind::NumberedStep)
}
pub(super) fn columns(kind: RowKind, width: f32) -> usize {
    // Four 132-DIP tab tiles cannot fit the minimum content width without cutting their labels.
    // Keep Welcome in two columns; tab previews wrap to two columns below 564 DIPs.
    if kind == RowKind::TabStyleTile && width >= 564.0 {
        4
    } else {
        2
    }
}
pub(super) fn row_height(row: &Row, width: f32, dpi: u32) -> Result<f32> {
    if row.kind == RowKind::TabStyleTile {
        return Ok(scf(120.0, dpi));
    }
    let reserved = match row.kind {
        RowKind::ExploreTile => 102.0,
        RowKind::Status => 104.0,
        _ => 36.0,
    };
    let text_width = (width - scf(reserved, dpi)).max(scf(40.0, dpi));
    let title = wrap(row.text, scf(14.0, dpi), 600, text_width)?.len() as f32 * scf(20.0, dpi);
    let desc = row
        .desc
        .map(|s| wrap(s, scf(12.0, dpi), 400, text_width))
        .transpose()?
        .map_or(0.0, |v| v.len() as f32 * scf(16.0, dpi));
    Ok((title + desc + scf(26.0, dpi)).max(scf(
        if row.kind == RowKind::ExploreTile {
            64.0
        } else {
            58.0
        },
        dpi,
    )))
}
fn text(
    ctx: &crate::render::Ctx,
    s: &str,
    r: RectF,
    size: f32,
    weight: u16,
    color: Rgba,
    dpi: u32,
) -> Result<()> {
    ctx.draw_text_cached(
        s,
        r,
        &style(
            kit::theme::font_family(),
            scf(size, dpi),
            weight,
            color,
            Align::Left,
        ),
    )
}
fn glyph(
    ctx: &crate::render::Ctx,
    s: &str,
    r: RectF,
    size: f32,
    color: Rgba,
    dpi: u32,
) -> Result<()> {
    ctx.draw_text_cached(
        s,
        r,
        &style(icon_family(), scf(size, dpi), 400, color, Align::Center),
    )
}
pub(super) fn draw(
    ctx: &crate::render::Ctx,
    lay: &RowLay,
    theme: &Theme,
    dpi: u32,
    settings: &Settings,
) -> Result<()> {
    use kit::controls::Control;
    let t = kit::theme::Theme::new(theme.light);
    let (row, x, y, w, h) = (&lay.row, lay.x, lay.y, lay.w, lay.h);
    let px = |v| scf(v, dpi);
    if grid(row.kind) {
        let bounds = RectF::new(x, y, x + w, y + h);
        let hovered = CONTROL_POINTER.with(|p| {
            p.get()
                .is_some_and(|(a, b, _)| a >= x && a < x + w && b >= y && b < y + h)
        });
        ctx.fill_rounded_rect(
            bounds,
            px(8.0),
            if hovered { t.control_hover } else { t.surface },
        )?;
        let selected =
            row.kind == RowKind::TabStyleTile && settings.get(Field::TabsStyle) == row.value;
        ctx.stroke_rounded_rect(
            bounds,
            px(8.0),
            px(if selected { 2.0 } else { 1.0 }),
            if selected { t.accent } else { t.border },
        )?;
    }
    if row.kind == RowKind::TabStyleTile {
        let preview = RectF::new(x + px(11.0), y + px(11.0), x + w - px(11.0), y + px(75.0));
        ctx.fill_rounded_rect(preview, px(4.0), t.surface_raised)?;
        let pw = preview.right - preview.left;
        let strip = RectF::new(
            preview.left + px(5.0),
            preview.top + px(5.0),
            preview.right - px(5.0),
            preview.top + px(17.0),
        );
        if row.value == 0 {
            ctx.fill_rect(strip, t.border)?;
            ctx.fill_rect(
                RectF::new(
                    strip.left + px(5.0),
                    strip.top + px(5.0),
                    strip.left + pw * 0.42,
                    strip.top + px(7.0),
                ),
                t.secondary,
            )?;
            glyph(
                ctx,
                "\u{E70D}",
                RectF::new(strip.right - px(13.0), strip.top, strip.right, strip.bottom),
                10.0,
                t.secondary,
                dpi,
            )?;
        } else {
            for i in 0..3 {
                let tw = (strip.right - strip.left - px(4.0)) / 3.0;
                let left = (strip.left + i as f32 * (tw + px(2.0))).round();
                let r = RectF::new(left, strip.top, left + tw, strip.bottom);
                let radius = if row.value == 3 {
                    6.0
                } else if row.value == 2 {
                    2.0
                } else {
                    0.0
                };
                ctx.fill_rounded_rect(r, px(radius), if i == 0 { t.control } else { t.border })?;
                if row.value >= 2 {
                    ctx.stroke_rounded_rect(r, px(radius), px(1.0), t.border_strong)?;
                }
                ctx.fill_rect(
                    RectF::new(
                        left + px(5.0),
                        r.top + px(5.0),
                        left + tw - px(5.0),
                        r.top + px(7.0),
                    ),
                    t.secondary,
                )?;
                if row.value == 1 && i == 0 {
                    ctx.fill_rect(
                        RectF::new(left, r.bottom - px(2.0), left + tw, r.bottom),
                        t.accent,
                    )?;
                }
            }
        }
        for i in 0..4 {
            let left = preview.left + px(9.0) + i as f32 * (pw - px(25.0)) / 4.0;
            ctx.fill_rounded_rect(
                RectF::new(
                    left,
                    preview.top + px(31.0),
                    left + px(12.0),
                    preview.top + px(43.0),
                ),
                px(2.0),
                t.border_strong,
            )?;
        }
        let radio = RectF::new(x + px(11.0), y + px(86.0), x + px(31.0), y + px(106.0));
        kit_draw(
            ctx,
            Control::Radio,
            radio,
            "",
            true,
            settings.get(Field::TabsStyle) == row.value,
            theme,
            dpi,
            0.0,
        )?;
        return text(
            ctx,
            row.text,
            RectF::new(x + px(39.0), y + px(86.0), x + w - px(10.0), y + px(106.0)),
            14.0,
            400,
            t.text,
            dpi,
        );
    }
    let (tx, right, mut yy) = if row.kind == RowKind::ExploreTile {
        let badge = RectF::new(x + px(14.0), y + px(12.0), x + px(50.0), y + px(48.0));
        ctx.fill_rounded_rect(badge, px(6.0), t.accent_subtle)?;
        let g = row
            .goto
            .map(|p| p.glyph())
            .unwrap_or('\u{E80F}')
            .to_string();
        glyph(ctx, &g, badge, 20.0, t.text, dpi)?;
        glyph(
            ctx,
            "\u{E76C}",
            RectF::new(x + w - px(26.0), y, x + w - px(10.0), y + h),
            12.0,
            t.secondary,
            dpi,
        )?;
        (x + px(62.0), x + w - px(40.0), y + px(12.0))
    } else if row.kind == RowKind::Status {
        kit::hero::draw(ctx, RectF::new(x, y + px(16.0), x + px(32.0), y + px(48.0)))?;
        kit_draw(
            ctx,
            Control::Toggle,
            RectF::new(
                x + w - px(40.0),
                y + (h - px(20.0)) / 2.0,
                x + w,
                y + (h + px(20.0)) / 2.0,
            ),
            "",
            true,
            settings.is_on(Field::EnableCorral),
            theme,
            dpi,
            0.0,
        )?;
        (x + px(48.0), x + w - px(56.0), y + px(12.0))
    } else {
        let badge = RectF::new(x, y + px(12.0), x + px(24.0), y + px(36.0));
        ctx.fill_rounded_rect(badge, px(12.0), t.accent_subtle)?;
        ctx.draw_text_cached(
            &row.value.to_string(),
            badge,
            &style(
                kit::theme::font_family(),
                px(14.0),
                600,
                t.text,
                Align::Center,
            ),
        )?;
        (x + px(36.0), x + w, y + px(12.0))
    };
    let title = if row.kind == RowKind::Status && !settings.is_on(Field::EnableCorral) {
        "Corral is off"
    } else {
        row.text
    };
    for line in wrap(title, px(14.0), 600, right - tx)? {
        text(
            ctx,
            &line,
            RectF::new(tx, yy, right, yy + px(20.0)),
            14.0,
            600,
            t.text,
            dpi,
        )?;
        yy += px(20.0);
    }
    yy += px(2.0);
    if let Some(desc) = row.desc {
        for line in wrap(desc, px(12.0), 400, right - tx)? {
            text(
                ctx,
                &line,
                RectF::new(tx, yy, right, yy + px(16.0)),
                12.0,
                400,
                t.secondary,
                dpi,
            )?;
            yy += px(16.0);
        }
    }
    Ok(())
}

/// Radio-group arrow movement shared with the shell (including tab preview Left/Right).
pub(super) fn radio_step(key: usize, position: usize, len: usize) -> usize {
    if matches!(key, 0x26 | 0x25) {
        position.saturating_sub(1)
    } else {
        (position + 1).min(len.saturating_sub(1))
    }
}

/// Pure U2 segmented keyboard adapter; the shell saves/reloads after applying this result.
pub(super) fn segment_arrow(hit: Hit, key: usize, settings: &Settings) -> Option<i64> {
    if !matches!(key, 0x25 | 0x27) {
        return None;
    }
    let value = match hit {
        Hit::AnimationMode(_) => settings.swipe_animation_style as i64,
        Hit::Radio(Field::QuickHideWhat, _) => settings.get(Field::QuickHideWhat),
        _ => return None,
    };
    Some((value + if key == 0x25 { -1 } else { 1 }).clamp(0, 2))
}
pub(super) fn apply_segment(settings: &mut Settings, hit: Hit, value: i64) -> bool {
    match hit {
        Hit::AnimationMode(_) => content::apply_animation(settings, value),
        Hit::Radio(Field::QuickHideWhat, _) if (0..=2).contains(&value) => {
            settings.set(Field::QuickHideWhat, value);
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PAGES: [Page; 6] = [
        Page::Welcome,
        Page::Tabs,
        Page::Hide,
        Page::Peek,
        Page::Pages,
        Page::Layout,
    ];
    fn settings() -> Settings {
        let mut s = Settings::default();
        s.set(Field::SwipeEnabled, 1);
        s.set(Field::TabsMouseover, 1);
        s.set(Field::HotCornerEnabled, 1);
        s.set(Field::SnapSpaced, 1);
        s
    }
    #[test]
    fn u2_tiles_pointer_and_keyboard_targets_at_all_dpis() -> Result<()> {
        let s = settings();
        for dpi in [96, 120, 144, 240] {
            for width in [760.0, 1000.0] {
                let (w, h) = (sc(width, dpi), sc(520.0, dpi));
                for page in [Page::Welcome, Page::Tabs] {
                    let lay = layout_page(page, &s, dpi, w as f32, h as f32, 0)?;
                    let rows: Vec<_> = lay
                        .sections
                        .iter()
                        .flat_map(|s| &s.rows)
                        .filter(|r| grid(r.row.kind))
                        .collect();
                    assert_eq!(rows.len(), if page == Page::Welcome { 10 } else { 4 });
                    let targets = focus_targets(&s, page, dpi, w, h, 0)?;
                    if page == Page::Tabs {
                        let mut position = 0;
                        for (key, expect) in [
                            (0x27, 1),
                            (0x27, 2),
                            (0x27, 3),
                            (0x27, 3),
                            (0x25, 2),
                            (0x25, 1),
                            (0x25, 0),
                            (0x25, 0),
                        ] {
                            position = radio_step(key, position, 4);
                            assert_eq!(position, expect);
                            assert!(
                                targets.iter().any(|(hit, _)| *hit
                                    == Hit::Radio(Field::TabsStyle, position as i64))
                            );
                        }
                    }
                    for row in rows {
                        let expected = if page == Page::Welcome {
                            Hit::Goto(row.row.goto.unwrap())
                        } else {
                            Hit::Radio(Field::TabsStyle, row.row.value)
                        };
                        let center = (
                            ((row.x + row.w / 2.0).round()) as i32,
                            ((row.y + row.h / 2.0).round()) as i32,
                        );
                        assert_eq!(hit_test(&s, page, center, dpi, w, h, 0)?, expected);
                        assert!(
                            targets.iter().any(|(hit, _)| *hit == expected),
                            "missing Enter/Space focus target {expected:?}"
                        );
                        // The gaps between tiles are inert.
                        assert_eq!(
                            hit_test(
                                &s,
                                page,
                                ((row.x + row.w + scf(3.0, dpi)) as i32, center.1),
                                dpi,
                                w,
                                h,
                                0
                            )?,
                            Hit::Nothing
                        );
                        let scroll = (row.y - scf(40.0, dpi)).max(0.0) as i32;
                        assert_eq!(
                            hit_test(&s, page, (center.0, center.1 - scroll), dpi, w, h, scroll)?,
                            expected
                        );
                    }
                }
            }
        }
        Ok(())
    }
    #[test]
    fn u2_segmented_mapping_both_directions_and_keyboard() -> Result<()> {
        let mut s = settings();
        let row = content(Page::Pages).sections[2].rows[0];
        for value in [0, 1, 2, 1, 0, 2] {
            assert!(row.apply_value(&mut s, value));
            assert_eq!(row.current_value(&s), value);
            assert_eq!(s.swipe_animation_style, value as u32);
            assert_eq!(s.is_on(Field::SwipeAnimate), value > 0);
            assert_eq!(s.is_on(Field::SwipeBounce), value == 2);
        }
        let before = s.clone();
        assert!(!row.apply_value(&mut s, 3));
        assert_eq!(before, s);
        for hit in [Hit::AnimationMode(0), Hit::Radio(Field::QuickHideWhat, 0)] {
            apply_segment(&mut s, hit, 0);
            for key in [0x27, 0x27, 0x27, 0x25, 0x25, 0x25] {
                let v = segment_arrow(hit, key, &s).unwrap();
                assert!(apply_segment(&mut s, hit, v));
            }
            assert_eq!(segment_arrow(hit, 0x0d, &s), None);
        }
        for dpi in [96, 120, 144, 240] {
            for page in [Page::Pages, Page::Hide] {
                let lay = layout_page(page, &s, dpi, scf(760.0, dpi), scf(520.0, dpi), 0)?;
                let row = lay
                    .sections
                    .iter()
                    .flat_map(|s| &s.rows)
                    .find(|r| r.row.kind == RowKind::Segmented)
                    .unwrap();
                let r = control_rect(row, dpi)?;
                for i in 0..3 {
                    let hit = hit_test(
                        &s,
                        page,
                        (
                            (r.left + (i as f32 + 0.5) * (r.right - r.left) / 3.0) as i32,
                            ((r.top + r.bottom) / 2.0) as i32,
                        ),
                        dpi,
                        sc(760.0, dpi),
                        sc(520.0, dpi),
                        0,
                    )?;
                    assert_eq!(
                        hit,
                        if page == Page::Pages {
                            Hit::AnimationMode(i)
                        } else {
                            Hit::Radio(Field::QuickHideWhat, i)
                        }
                    );
                }
            }
        }
        Ok(())
    }
    #[test]
    fn u2_idle_dropdown_round_trip_never_and_existing_autoshow_side_effects() {
        let mut s = Settings::default();
        let row = content(Page::Hide).sections[2].rows[0];
        for delay in [1, 1000, 30000, 120000, 600000] {
            assert!(row.apply_value(&mut s, delay));
            assert_eq!(row.current_value(&s), delay);
            assert!(s.is_on(Field::AutoShow));
            assert!(row.apply_value(&mut s, 0));
            assert_eq!(row.current_value(&s), 0);
            assert!(!s.is_on(Field::AutoHideIdle));
            assert_eq!(
                s.quick_hide_auto_hide_on_idle, 0,
                "existing schema encodes Never as zero"
            );
            assert!(row.apply_value(&mut s, delay));
            assert_eq!(row.current_value(&s), delay);
        }
        s.set(Field::AutoShow, 0);
        assert!(row.apply_value(&mut s, 1000));
        assert!(
            !s.is_on(Field::AutoShow),
            "changing an active delay must not re-enable AutoShow"
        );
        let before = s.clone();
        assert!(!row.apply_value(&mut s, 123));
        assert_eq!(before, s);
        for (raw, shown) in [
            (0, 0),
            (2, 1),
            (999, 1),
            (1000, 1000),
            (45000, 30000),
            (600001, 600000),
        ] {
            s.quick_hide_auto_hide_on_idle = raw;
            assert_eq!(row.current_value(&s), shown);
        }
    }
    #[test]
    fn u2_page_bounds_fixed_nav_focus_gates_and_expander_anchors() -> Result<()> {
        let mut s = settings();
        for dpi in [96, 120, 144, 240] {
            let (w, h) = (sc(760.0, dpi), sc(520.0, dpi));
            for page in PAGES {
                let lay = layout_page(page, &s, dpi, w as f32, h as f32, 0)?;
                let targets = focus_targets(&s, page, dpi, w, h, 0)?;
                for section in &lay.sections {
                    for row in &section.rows {
                        assert!(
                            row.x >= section.card.left && row.x + row.w <= section.card.right + 1.0
                        );
                        assert!(
                            row.y >= section.card.top && row.y + row.h <= section.card.bottom + 1.0
                        );
                        if row.row.kind == RowKind::TabStyleTile {
                            let measured = measure_text(
                                row.row.text,
                                kit::theme::font_family(),
                                scf(14.0, dpi),
                                400,
                            )?
                            .0;
                            assert!(measured <= row.w - scf(49.0, dpi));
                        }
                    }
                }
                for scroll in [0, sc(48.0, dpi), sc(180.0, dpi)] {
                    for nav in Page::ALL {
                        let (x, y, nw, nh) = nav_item_rect(nav.index(), dpi, h);
                        assert_eq!(
                            hit_test(&s, page, (x + nw / 2, y + nh / 2), dpi, w, h, scroll)?,
                            Hit::Nav(nav)
                        );
                    }
                    assert_eq!(focus_targets(&s, page, dpi, w, h, scroll)?[0], targets[0]);
                }
                let before = layout_page(page, &s, dpi, w as f32, h as f32, sc(48.0, dpi))?;
                if let Some(row) = before
                    .sections
                    .iter()
                    .flat_map(|s| &s.rows)
                    .find(|r| r.row.kind == RowKind::Expander)
                {
                    let y = row.y;
                    toggle_expander(page, row.row.text);
                    let after = layout_page(page, &s, dpi, w as f32, h as f32, sc(48.0, dpi))?;
                    assert_eq!(
                        after
                            .sections
                            .iter()
                            .flat_map(|s| &s.rows)
                            .find(|r| r.row.kind == RowKind::Expander)
                            .unwrap()
                            .y,
                        y
                    );
                    toggle_expander(page, row.row.text);
                }
                // The same focus reveal math used by the shell always provides the 16-DIP margin.
                for (_, r) in targets.iter().skip(1) {
                    let scale = dpi as f32 / 96.0;
                    let dip = RectF::new(
                        r.left / scale,
                        r.top / scale,
                        r.right / scale,
                        r.bottom / scale,
                    );
                    let offset = kit::layout::reveal_focus(0.0, dip, 520.0, 10000.0, false);
                    if dip.bottom - dip.top <= 488.0 {
                        assert!(dip.bottom - offset <= 504.01 || offset == 0.0);
                    }
                }
            }
            // A show-gate inserts only below the unchanged hover row.
            s.set(Field::TabsMouseover, 0);
            let before = layout_page(Page::Tabs, &s, dpi, w as f32, h as f32, sc(80.0, dpi))?;
            s.set(Field::TabsMouseover, 1);
            let after = layout_page(Page::Tabs, &s, dpi, w as f32, h as f32, sc(80.0, dpi))?;
            for row in before.sections[1].rows.iter().take(3) {
                assert_eq!(
                    after.sections[1]
                        .rows
                        .iter()
                        .find(|r| r.row.bind == row.row.bind)
                        .unwrap()
                        .y,
                    row.y
                );
            }
            s.set(Field::QuickHideEnabled, 0);
            let lay = layout_page(Page::Hide, &s, dpi, w as f32, h as f32, 0)?;
            assert!(lay.sections[1].rows[0].enabled);
            assert!(!lay.sections[2].rows[0].enabled);
            s.set(Field::QuickHideEnabled, 1);
        }
        Ok(())
    }
    #[test]
    fn u2_layout_bindings_gates_and_dropdown_popup_dimensions() -> Result<()> {
        let s = settings();
        let layout = content(Page::Layout);
        assert_eq!(layout.sections[0].gates, &[Gate::LayoutAuto]);
        assert_eq!(
            layout.sections[0]
                .rows
                .iter()
                .take(3)
                .map(|r| (r.bind, r.value))
                .collect::<Vec<_>>(),
            [
                (Some(Field::LayoutPreserve), 2),
                (Some(Field::LayoutPreserve), 1),
                (Some(Field::LayoutPreserve), 0)
            ]
        );
        assert_eq!(layout.sections[0].rows[3].gates, &[Gate::MultiMonitor]);
        assert_eq!(layout.sections[0].rows[3].button_text, Some("Swap"));
        assert!(!s.multi_monitor_options_visible(1));
        assert!(s.multi_monitor_options_visible(2));
        assert_eq!(layout.sections[1].rows[2].range, Some((0, 100, 1)));
        for dpi in [96, 120, 144, 240] {
            for page in PAGES {
                let lay = layout_page(page, &s, dpi, scf(760.0, dpi), scf(520.0, dpi), 0)?;
                for row in lay
                    .sections
                    .iter()
                    .flat_map(|s| &s.rows)
                    .filter(|r| r.row.kind == RowKind::Dropdown)
                {
                    let r = control_rect(row, dpi)?;
                    let choices = combo_choices(
                        row.row.bind.unwrap(),
                        content::choice_value(&s, row.row.bind.unwrap()),
                    );
                    for (label, _) in &choices {
                        let measured =
                            measure_text(label, kit::theme::font_family(), scf(14.0, dpi), 400)?.0;
                        assert!(
                            measured + scf(32.0, dpi) <= r.right - r.left,
                            "dropdown clips {label} at {dpi}"
                        );
                    }
                    for popup_dpi in [96, 120, 144, 240] {
                        let geometry = kit::menu::PopupGeometry::new(
                            RectF::new(1800.0, 400.0, 1900.0, 440.0),
                            RectF::new(0.0, 0.0, 3840.0, 2160.0),
                            popup_dpi,
                            220.0,
                            choices.len(),
                        );
                        assert_eq!(geometry.dpi, popup_dpi);
                        assert_eq!(geometry.surface_size().0, sc(220.0, popup_dpi) as u32);
                        assert_eq!(
                            geometry.row(0).bottom - geometry.row(0).top,
                            scf(32.0, popup_dpi)
                        );
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn u2_golden_renders() -> Result<()> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/qa/settings-redesign/u2");
        std::fs::create_dir_all(&dir).unwrap();
        let s = settings();
        crate::settings::set_layout_flags(crate::settings::LayoutFlags {
            any_group_styled: true,
            any_group_blend: false,
            any_group_faded: false,
        });
        EXPANDERS.with(|e| e.borrow_mut().clear());
        for (name, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
            for dpi in [96, 120, 144, 240] {
                for page in PAGES {
                    let full_h =
                        content_height(&s, page, dpi, sc(1000.0, dpi), sc(720.0, dpi))? as u32;
                    let mut full =
                        Surface::new(sc(1000.0, dpi) as u32, full_h.max(sc(720.0, dpi) as u32))?;
                    render_settings_state(
                        &mut full,
                        &View {
                            page,
                            hover: None,
                            scroll: 0,
                        },
                        &s,
                        &theme,
                        dpi,
                    )?;
                    full.save_png(
                        &dir.join(format!("{name}-{dpi}-{page:?}-full.png").to_lowercase()),
                    )?;
                    let mut state = s.clone();
                    match page {
                        Page::Welcome => state.set(Field::EnableCorral, 0),
                        Page::Tabs => state.set(Field::TabsStyle, 0),
                        Page::Hide => {
                            content::apply_idle_mode(&mut state, 120000);
                        }
                        Page::Peek => {
                            state.set(Field::HotkeysGlance, 0);
                            state.set(Field::HotCornerEnabled, 0);
                        }
                        Page::Pages => state.set(Field::SwipeEnabled, 0),
                        Page::Layout => state.set(Field::SnapEnabled, 0),
                        _ => {}
                    }
                    let scroll = max_scroll(&state, page, dpi, sc(760.0, dpi), sc(520.0, dpi))?;
                    let mut surface = Surface::new(sc(760.0, dpi) as u32, sc(520.0, dpi) as u32)?;
                    render_settings_state(
                        &mut surface,
                        &View {
                            page,
                            hover: None,
                            scroll,
                        },
                        &state,
                        &theme,
                        dpi,
                    )?;
                    surface.save_png(
                        &dir.join(format!("{name}-{dpi}-{page:?}-state.png").to_lowercase()),
                    )?;
                    for (width, height, suffix) in
                        [(1000.0, 720.0, "default"), (760.0, 520.0, "minimum")]
                    {
                        let (w, h) = (sc(width, dpi), sc(height, dpi));
                        let max = max_scroll(&s, page, dpi, w, h)?;
                        for (position, scroll) in [("top", 0), ("bottom", max)] {
                            let mut surface = Surface::new(w as u32, h as u32)?;
                            render_settings_state(
                                &mut surface,
                                &View {
                                    page,
                                    hover: None,
                                    scroll,
                                },
                                &s,
                                &theme,
                                dpi,
                            )?;
                            surface.save_png(
                                &dir.join(
                                    format!("{name}-{dpi}-{page:?}-{suffix}-{position}.png")
                                        .to_lowercase(),
                                ),
                            )?;
                        }
                    }
                }
            }
        }
        crate::settings::set_layout_flags(crate::settings::LayoutFlags::default());
        Ok(())
    }
}
