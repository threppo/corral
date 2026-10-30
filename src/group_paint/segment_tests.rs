use super::*;

fn member(dpi: u32, gate: bool) -> SegmentInput {
    SegmentInput { want: ButtonSet { menu: true, up: gate, close: true }, folder: gate, text_w: px(300.0, dpi) }
}

fn segmented_items(strip: Rect, dpi: u32, count: usize) -> Vec<TabItem> {
    let layout = tab_segments(strip, dpi, &vec![member(dpi, false); count]).unwrap();
    layout.into_iter().enumerate().map(|(i, s)| TabItem {
        title: format!("Member {i}"), hit: s.hit, active: i == 0, hovered: false, own_color: None,
        custom_text: None, weight: 400 + i as u16 * 100, italic: i == 1,
        segment: Some(SegmentChrome { layout: s.layout, hover: 0.0, buttons: 0.0,
            menu: ButtonState::Rest, up: ButtonState::Disabled, close: ButtonState::Rest }),
    }).collect()
}

#[test]
fn segments_share_full_width_and_preserve_buttons_and_title_at_all_dpis() {
    for dpi in DPIS {
        for count in [2, 3, 5] {
            let strip = rect(-px(35.0, dpi), px(19.0, dpi), px(180.0 * count as f64 - 35.0, dpi) + 1, px(43.0, dpi));
            let members: Vec<_> = (0..count).map(|i| member(dpi, i % 2 != 0)).collect();
            let segments = tab_segments(strip, dpi, &members).unwrap();
            assert_eq!(segments.len(), count);
            assert_eq!(segments.first().unwrap().hit.left, strip.left);
            assert_eq!(segments.last().unwrap().hit.right, strip.right);
            let widths: Vec<_> = segments.iter().map(|s| s.hit.width()).collect();
            assert!(widths.iter().max().unwrap() - widths.iter().min().unwrap() <= 1);
            for pair in segments.windows(2) { assert_eq!(pair[1].hit.left - pair[0].hit.right, px(4.0, dpi)); }
            for (s, m) in segments.iter().zip(&members) {
                let l = s.layout;
                assert_eq!(s.hit.height(), strip.height());
                assert_eq!(l.menu.unwrap().left, s.hit.left + px(CLUSTER_PAD, dpi));
                assert_eq!(l.close.unwrap().right, s.hit.right - px(CLUSTER_PAD, dpi));
                assert_eq!(l.up.is_some(), m.want.up);
                let reserved = if m.folder { px(GLYPH, dpi) + px(TEXT_GAP, dpi) } else { 0 };
                assert!(l.span.1 - l.span.0 - reserved >= px(48.0, dpi));
                let mut parts: Vec<_> = [l.menu, l.up, l.close, l.folder, l.text].into_iter().flatten().collect();
                parts.sort_by_key(|r| r.left);
                for pair in parts.windows(2) { assert!(pair[0].right <= pair[1].left); }
            }
        }
    }
}

#[test]
fn segment_minimum_is_exact_and_narrow_width_uses_the_unchanged_pill_path() {
    for dpi in DPIS {
        for count in [2, 3, 5] {
            for gate in [false, true] {
                let m = member(dpi, gate);
                let min = 2 * (px(CLUSTER_PAD, dpi) + px(TEXT_GAP, dpi))
                    + (2 + gate as i32) * px(BTN_W, dpi) + px(48.0, dpi)
                    + if gate { px(GLYPH, dpi) + px(TEXT_GAP, dpi) } else { 0 };
                let width = min * count as i32 + px(4.0, dpi) * (count as i32 - 1);
                assert!(tab_segments(rect(0, 0, width, metrics(dpi).title_h), dpi, &vec![m; count]).is_some());
                let narrow = rect(0, 0, width - 1, metrics(dpi).title_h);
                assert!(tab_segments(narrow, dpi, &vec![m; count]).is_none());
                assert!(!crate::tabs::tab_cells(narrow, (px(32.0, dpi), width - px(32.0, dpi)),
                    &vec![px(90.0, dpi); count], 0, TABS_ROUNDED, dpi).is_empty());
            }
        }
    }
    assert!(tab_segments(rect(0, 0, 800, 24), 96, &[member(96, false)]).is_none());
    assert!(tab_segments(rect(0, 0, 800, 0), 96, &[member(96, false); 2]).is_none());
}

#[test]
fn segment_hit_tests_route_every_button_and_never_hit_the_gaps() {
    for dpi in DPIS {
        for count in [2, 3, 5] {
            let strip = rect(17, 23, 17 + px(180.0 * count as f64, dpi), 23 + metrics(dpi).title_h);
            let segments = tab_segments(strip, dpi, &vec![member(dpi, true); count]).unwrap();
            for s in &segments {
                let centre = |r: Rect| ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
                for (r, part) in [(s.layout.menu.unwrap(), SegmentPart::Menu), (s.layout.close.unwrap(), SegmentPart::Close), (s.layout.up.unwrap(), SegmentPart::Up)] {
                    assert_eq!(s.hit_test(centre(r), true, true), Some(part));
                    assert_eq!(segments.iter().filter(|s| s.hit_test(centre(r), true, true).is_some()).count(), 1);
                }
                assert_eq!(s.hit_test(centre(s.layout.text.unwrap()), true, true), Some(SegmentPart::Title));
                assert_eq!(s.hit_test(centre(s.layout.up.unwrap()), true, false), Some(SegmentPart::Title));
                assert_eq!(s.hit_test(centre(s.layout.menu.unwrap()), false, true), Some(SegmentPart::Title));
                assert_eq!(s.hit_test(centre(s.layout.close.unwrap()), false, true), Some(SegmentPart::Title));
            }
            for pair in segments.windows(2) {
                let gap = (pair[0].hit.right, strip.top + strip.height() / 2);
                assert!(segments.iter().all(|s| s.hit_test(gap, true, true).is_none()));
            }
        }
    }
}

#[test]
fn rounded_segments_record_n_shapes_with_the_corral_radius_and_separate_body_mask() {
    for dpi in DPIS {
        for count in [2, 3, 5] {
            for square in [false, true] {
                let strip = rect(0, 0, px(180.0 * count as f64, dpi), metrics(dpi).title_h);
                let tabs = segmented_items(strip, dpi, count);
                let mut p = inputs(dpi, "", Some(TabsPaint { style: TABS_ROUNDED, tabs: &tabs }));
                p.outer.right = strip.right;
                p.strip = Some(strip);
                p.square_edges = square;
                for folded in [false, true] {
                    p.folded = folded;
                    if folded { p.outer.bottom = strip.bottom; }
                    let mut rec = Rec::default();
                    paint_fills(&mut rec, &p).unwrap();
                    assert_eq!(rec.0.len(), count + if folded { 0 } else { 1 });
                    for (s, op) in tabs.iter().zip(rec.0.iter().rev().take(count).rev()) {
                        assert!(matches!(op, Op::Shape(r, radii, _) if *r == rectf(s.hit) && *radii == [p.radius(); 4]));
                    }
                    let mut mask = Rec::default();
                    paint_mask(&mut mask, &p).unwrap();
                    assert_eq!(mask.0.len(), rec.0.len());
                    if !folded {
                        let Op::Shape(body, _, _) = mask.0[0] else { panic!() };
                        assert_eq!(body.top, (strip.bottom + if square { 0 } else { px(2.0, dpi) }) as f32);
                    }
                    assert!(mask.0.iter().all(|op| matches!(op, Op::Shape(_, radii, c) if *radii == [p.radius(); 4] && c.a == 255)));
                }
            }
        }
    }
}

#[test]
fn segment_fill_uses_normal_title_layers_inactive_alpha_and_own_colour() {
    let strip = rect(0, 0, 600, 24);
    let tabs = segmented_items(strip, 96, 3);
    let mut p = inputs(96, "", None);
    p.outer.right = strip.right;
    p.strip = Some(strip);
    p.shade = SHADE_DARK;
    p.strip_hover = 0.0;
    p.tabs = Some(TabsPaint { style: TABS_ROUNDED, tabs: &tabs });
    let mut rec = Rec::default();
    paint_fills(&mut rec, &p).unwrap();
    let colors: Vec<_> = rec.0.iter().skip(1).map(|o| if let Op::Shape(_, _, c) = o { *c } else { panic!() }).collect();
    assert_eq!(colors[1].a, apply_fade(colors[0].a, 0.6));
    // Normal body then shade over the same wallpaper: flattening differs by at most rounding.
    let normal = strip_backdrop(p.backdrop_mean, p.body_rgb, p.fill_alpha, p.overlay());
    let segment = over([colors[0].r, colors[0].g, colors[0].b], colors[0].a, p.backdrop_mean);
    for i in 0..3 { assert!((normal[i] as i32 - segment[i] as i32).abs() <= 1); }
    p.reveal = 0.0;
    let mut hidden = Rec::default();
    paint_fills(&mut hidden, &p).unwrap();
    assert!(matches!(hidden.0[1], Op::Shape(_, _, c) if c == rgba(p.body_rgb, p.fill_alpha)),
        "On-hover strips retain the same base fill as a single Corral");
    p.reveal = 1.0;
    let mut updated = tabs.clone();
    updated[1].hovered = true;
    updated[2].own_color = Some([20, 80, 160]);
    p.tabs = Some(TabsPaint { style: TABS_ROUNDED, tabs: &updated });
    let mut rec = Rec::default();
    paint_fills(&mut rec, &p).unwrap();
    assert!(matches!(rec.0[2], Op::Shape(_, _, c) if c.a == apply_fade(colors[0].a, 0.8)));
    assert!(matches!(rec.0[3], Op::Shape(_, _, c) if [c.r,c.g,c.b] == [20,80,160]));
}

#[test]
fn segment_chrome_preserves_member_fonts_text_alpha_buttons_and_high_contrast() {
    let strip = rect(0, 0, 500, 24);
    let mut tabs = segmented_items(strip, 96, 2);
    tabs[1].segment.as_mut().unwrap().buttons = 1.0;
    tabs[1].segment.as_mut().unwrap().menu = ButtonState::Pressed;
    let mut p = inputs(96, "", Some(TabsPaint { style: TABS_ROUNDED, tabs: &tabs }));
    p.strip = Some(strip);
    p.outer.right = strip.right;
    let mut rec = Rec::default();
    paint_chrome(&mut rec, &p).unwrap();
    let text: Vec<_> = rec.0.iter().filter_map(|o| if let Op::Text(_, _, c) = o { Some(c.a) } else { None }).collect();
    assert_eq!(text, [255, pct_alpha(70)]);
    assert_eq!((rec.1[0].weight, rec.1[1].weight, rec.1[1].italic), (400, 500, true));
    assert_eq!(rec.0.iter().filter(|o| matches!(o, Op::Glyph(Glyph::Menu, ..))).count(), 1);
    assert_eq!(rec.0.iter().filter(|o| matches!(o, Op::Glyph(Glyph::Close, ..))).count(), 1);
    let hc = HighContrast { window: [1,2,3], window_text: [4,5,6], highlight: [7,8,9], highlight_text: [10,11,12] };
    p.high_contrast = Some(hc);
    let mut rec = Rec::default();
    paint_fills(&mut rec, &p).unwrap();
    assert!(matches!(rec.0[1], Op::Shape(_, _, c) if c == rgba(hc.highlight,255)));
    assert!(matches!(rec.0[2], Op::Shape(_, _, c) if c == rgba(hc.window,255)));
    paint_chrome(&mut rec, &p).unwrap();
    assert!(rec.0.iter().filter_map(|o| if let Op::Text(_,_,c) = o { Some(c) } else { None }).all(|c| c.a == 255));
}
