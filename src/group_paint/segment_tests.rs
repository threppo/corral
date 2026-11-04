use super::*;

fn member(dpi: u32, gate: bool) -> SegmentInput {
    SegmentInput { want: ButtonSet { menu: true, up: gate, close: true }, folder: gate, text_w: px(300.0, dpi) }
}

fn segmented_items(strip: Rect, dpi: u32, count: usize) -> Vec<TabItem> {
    let layout = tab_segments(strip, dpi, TABS_ROUNDED, &vec![member(dpi, false); count]).unwrap();
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
            let segments = tab_segments(strip, dpi, TABS_ROUNDED, &members).unwrap();
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
fn narrow_segments_drop_close_then_up_then_menu_before_the_pill_path() {
    for dpi in DPIS {
        for count in [2, 3, 5] {
            for gate in [false, true] {
                let m = member(dpi, gate);
                let folder = if gate { px(GLYPH, dpi) + px(TEXT_GAP, dpi) } else { 0 };
                let total = |seg: i32| seg * count as i32 + px(4.0, dpi) * (count as i32 - 1);
                let at = |seg: i32| tab_segments(rect(0, 0, total(seg), metrics(dpi).title_h), dpi, TABS_ROUNDED, &vec![m; count]);
                let kept = |seg: i32| at(seg).unwrap().iter()
                    .map(|s| (s.layout.menu.is_some(), s.layout.up.is_some(), s.layout.close.is_some())).collect::<Vec<_>>();
                let full = 2 * (px(CLUSTER_PAD, dpi) + px(TEXT_GAP, dpi)) + (2 + gate as i32) * px(BTN_W, dpi) + px(48.0, dpi) + folder;
                assert!(kept(full).iter().all(|k| *k == (true, gate, true)));
                assert!(kept(full - 1).iter().all(|k| *k == (true, gate, false)), "close goes first");
                // Both sides of every step: the narrowest bar that keeps `lead` leading buttons without close.
                let lead_min = |lead: i32| px(CLUSTER_PAD, dpi) + lead * px(BTN_W, dpi) + px(TEXT_GAP, dpi) + px(EDGE_PAD, dpi) + px(48.0, dpi) + folder;
                assert!(kept(lead_min(1 + gate as i32)).iter().all(|k| *k == (true, gate, false)));
                if gate {
                    assert!(kept(lead_min(2) - 1).iter().all(|k| *k == (true, false, false)), "then Up");
                    assert!(kept(lead_min(1)).iter().all(|k| *k == (true, false, false)));
                }
                assert!(kept(lead_min(1) - 1).iter().all(|k| *k == (false, false, false)), "then the menu");
                let bare = 2 * px(EDGE_PAD, dpi) + px(48.0, dpi) + folder;
                assert!(kept(bare).iter().all(|k| *k == (false, false, false)), "then the menu");
                let width = total(bare);
                assert!(at(bare - 1).is_none());
                let narrow = rect(0, 0, width - 1, metrics(dpi).title_h);
                assert!(tab_segments(narrow, dpi, TABS_ROUNDED, &vec![m; count]).is_none());
                assert!(!crate::tabs::tab_cells(narrow, (px(32.0, dpi), width - px(32.0, dpi)),
                    count, 0, TABS_ROUNDED, dpi).is_empty());
            }
        }
    }
    assert!(tab_segments(rect(0, 0, 800, 24), 96, TABS_ROUNDED, &[member(96, false)]).is_none());
    assert!(tab_segments(rect(0, 0, 800, 0), 96, TABS_ROUNDED, &[member(96, false); 2]).is_none());
}

#[test]
fn segment_hit_tests_route_every_button_and_never_hit_the_gaps() {
    for dpi in DPIS {
        for count in [2, 3, 5] {
            let strip = rect(17, 23, 17 + px(180.0 * count as f64, dpi), 23 + metrics(dpi).title_h);
            let segments = tab_segments(strip, dpi, TABS_ROUNDED, &vec![member(dpi, true); count]).unwrap();
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

#[test]
fn flat_and_segmented_bars_fill_the_strip_with_their_own_corners() {
    for dpi in DPIS {
        for style in [TABS_FLAT, TABS_SEGMENTED] {
            let strip = rect(0, 0, px(540.0, dpi), metrics(dpi).title_h);
            let bars = tab_segments(strip, dpi, style, &vec![member(dpi, false); 3]).unwrap();
            assert_eq!((bars[0].hit.left, bars[2].hit.right), (strip.left, strip.right), "no space at the sides");
            for pair in bars.windows(2) { assert_eq!(pair[1].hit.left - pair[0].hit.right, px(2.0, dpi)); }
            assert!(bars.iter().all(|b| b.layout.menu.is_some() && b.layout.close.is_some()), "each has its own buttons");
        }
    }
    let shapes = |style: u32, above: bool| {
        let strip = if above { rect(0, 0, 540, 24) } else { rect(0, 136, 540, 160) };
        let tabs = segmented_items(strip, 96, 3);
        let mut p = inputs(96, "", Some(TabsPaint { style, tabs: &tabs }));
        p.outer = rect(0, 0, 540, 160);
        p.strip = Some(strip);
        p.strip_above = above;
        let mut rec = Rec::default();
        paint_fills(&mut rec, &p).unwrap();
        rec.0.iter().map(|o| if let Op::Shape(r, radii, _) = o { (*r, *radii) } else { panic!("{o:?}") }).collect::<Vec<_>>()
    };
    let seg = shapes(TABS_SEGMENTED, true);
    assert_eq!((seg[0].0.top, seg[0].1), (26.0, [8.0; 4]), "a body of its own, 2 DIP below");
    assert!(seg[1..].iter().all(|s| s.1 == [0.0; 4]), "square tabs: {seg:?}");
    assert_eq!(shapes(TABS_SEGMENTED, false)[0].0.bottom, 134.0, "the same gap with the strip below");
    assert_eq!(shapes(TABS_ROUNDED, false)[0].0.bottom, 134.0);
    assert!(shapes(TABS_ROUNDED, true).iter().all(|s| s.1 == [8.0; 4]), "only Rounded has rounded tabs");
    let flat = shapes(TABS_FLAT, true);
    assert_eq!((flat[0].0.top, flat[0].1), (24.0, [0.0, 0.0, 8.0, 8.0]), "the body joins the bars");
    assert!(flat[1..].iter().all(|s| s.1 == [0.0; 4]), "square tabs: {flat:?}");
    let below = shapes(TABS_FLAT, false);
    assert_eq!((below[0].0.bottom, below[0].1), (136.0, [8.0, 8.0, 0.0, 0.0]));
}

#[test]
fn the_menu_only_arrow_follows_the_title_and_points_to_the_list() {
    for dpi in DPIS {
        let strip = rect(0, 0, px(300.0, dpi), metrics(dpi).title_h);
        let room = tab_list_room(dpi);
        let text_w = px(80.0, dpi);
        let base = layout_title(&TitleLayoutInput { strip, dpi, want: ButtonSet { menu: true, up: false, close: true }, folder: false, text_w: text_w + room });
        let layout = with_tab_list(base, dpi);
        let (text, arrow) = (layout.text.unwrap(), layout.tab_list.unwrap());
        assert_eq!(text.width(), text_w, "the title keeps its measured width");
        assert_eq!(arrow.left - text.right, room - px(GLYPH, dpi), "a gap between them");
        assert_eq!(arrow.right, base.text.unwrap().right);
        let narrow = TitleLayout { text: Some(rect(0, 0, room - 1, 10)), ..Default::default() };
        assert_eq!(with_tab_list(narrow, dpi), narrow, "no room: only the text");
        let unnamed = TitleLayout { text: Some(rect(0, 0, room, 10)), ..Default::default() };
        assert!(with_tab_list(unnamed, dpi).tab_list.is_some(), "an empty title still gets the arrow");
    }
    let strip = rect(0, 0, 300, 24);
    let layout = with_tab_list(layout_title(&TitleLayoutInput { strip, dpi: 96, want: ButtonSet { menu: true, up: false, close: true }, folder: false, text_w: 100 }), 96);
    for (above, want) in [(true, Glyph::Expand), (false, Glyph::Collapse)] {
        let mut p = inputs(96, "Docs", None);
        p.strip = Some(strip);
        p.strip_above = above;
        p.layout = layout;
        assert!(record(&p).iter().any(|op| matches!(op, Op::Glyph(g, ..) if *g == want)), "{want:?}");
    }
}

#[test]
fn a_flat_bar_underlines_the_active_tab_and_one_outline_goes_around_the_corral() {
    for (above, top, bottom) in [(true, 22.0, 24.0), (false, 136.0, 138.0)] {
        let strip = if above { rect(0, 0, 540, 24) } else { rect(0, 136, 540, 160) };
        let tabs = segmented_items(strip, 96, 2);
        let mut p = inputs(96, "", Some(TabsPaint { style: TABS_FLAT, tabs: &tabs }));
        p.outer = rect(0, 0, 540, 160);
        p.strip = Some(strip);
        p.strip_above = above;
        p.outline = true;
        let mut rec = Rec::default();
        paint_chrome(&mut rec, &p).unwrap();
        let bars: Vec<_> = rec.0.iter().filter_map(|o| if let Op::Rect(r, _) = o { Some(*r) } else { None }).collect();
        assert_eq!(bars.len(), 1, "only the active tab");
        assert_eq!((bars[0].top, bars[0].bottom, bars[0].left), (top, bottom, tabs[0].hit.left as f32));
        let rings: Vec<_> = rec.0.iter().filter_map(|o| if let Op::Ring(r, ..) = o { Some(*r) } else { None }).collect();
        assert_eq!(rings, [rectf(p.outer)]);
    }
}

#[test]
fn a_single_corral_draws_as_a_set_with_one_tab_in_every_style() {
    let strip = rect(0, 0, 540, 24);
    let layout = layout_title(&TitleLayoutInput { strip, dpi: 96, want: ButtonSet { menu: true, up: false, close: true }, folder: false, text_w: 80 });
    let tabs = [TabItem { title: "Solo".into(), hit: strip, active: true, hovered: false, own_color: None, custom_text: None, weight: 400, italic: false,
        segment: Some(SegmentChrome { layout, hover: 0.0, buttons: 1.0, menu: ButtonState::Rest, up: ButtonState::Disabled, close: ButtonState::Rest }) }];
    for (style, body_top, bar_radii) in [(TABS_FLAT, 24.0, [0.0; 4]), (TABS_SEGMENTED, 26.0, [0.0; 4]), (TABS_ROUNDED, 26.0, [8.0; 4])] {
        let mut p = inputs(96, "", Some(TabsPaint { style, tabs: &tabs }));
        p.outer = rect(0, 0, 540, 160);
        p.strip = Some(strip);
        let mut rec = Rec::default();
        paint_fills(&mut rec, &p).unwrap();
        assert_eq!(rec.0.len(), 2, "a body and one bar: {:?}", rec.0);
        assert!(matches!(rec.0[0], Op::Shape(body, ..) if body.top == body_top), "{:?}", rec.0);
        assert!(matches!(rec.0[1], Op::Shape(bar, radii, _) if bar == rectf(strip) && radii == bar_radii), "{:?}", rec.0);
        let ops = record(&p);
        assert!(ops.iter().any(|o| matches!(o, Op::Text(t, ..) if t == "Solo")), "the title");
        assert!(ops.iter().any(|o| matches!(o, Op::Glyph(Glyph::Menu, ..))) && ops.iter().any(|o| matches!(o, Op::Glyph(Glyph::Close, ..))), "its buttons");
    }
}
