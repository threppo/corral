//! Acceptance tests of corral-painting-spec 9 (pure; DirectWrite metrics are plain numbers).

use super::*;
use crate::glyphs::Glyph;

const DPIS: [u32; 5] = [96, 120, 144, 192, 240];

fn rect(l: i32, t: i32, r: i32, b: i32) -> Rect {
    Rect::new(l, t, r, b)
}

// ---- a recording sink ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Op {
    Shape(RectF, [f32; 4], Rgba),
    Ring(RectF, [f32; 4], f32, Rgba),
    Round(RectF, f32, Rgba),
    Rect(RectF, Rgba),
    Line(Rgba),
    Text(String, RectF, Rgba),
    Glyph(Glyph, RectF, Rgba),
}

#[derive(Default)]
struct Rec(Vec<Op>, Vec<TitleFont>);

impl Sink for Rec {
    fn fill_shape(&mut self, rect: RectF, radii: [f32; 4], color: Rgba) -> Result<()> {
        self.0.push(Op::Shape(rect, radii, color));
        Ok(())
    }
    fn fill_ring(&mut self, rect: RectF, radii: [f32; 4], width: f32, color: Rgba) -> Result<()> {
        self.0.push(Op::Ring(rect, radii, width, color));
        Ok(())
    }
    fn fill_round(&mut self, rect: RectF, radius: f32, color: Rgba) -> Result<()> {
        self.0.push(Op::Round(rect, radius, color));
        Ok(())
    }
    fn fill_rect(&mut self, rect: RectF, color: Rgba) -> Result<()> {
        self.0.push(Op::Rect(rect, color));
        Ok(())
    }
    fn line(&mut self, _from: (f32, f32), _to: (f32, f32), _width: f32, color: Rgba) -> Result<()> {
        self.0.push(Op::Line(color));
        Ok(())
    }
    fn text(&mut self, text: &str, rect: RectF, font: &TitleFont, color: Rgba, _align: Align) -> Result<()> {
        self.1.push(*font);
        self.0.push(Op::Text(text.to_string(), rect, color));
        Ok(())
    }
    fn glyph(&mut self, glyph: Glyph, box_rect: RectF, _size_px: f32, color: Rgba) -> Result<()> {
        self.0.push(Op::Glyph(glyph, box_rect, color));
        Ok(())
    }
}

fn inputs<'a>(dpi: u32, title: &'a str, tabs: Option<TabsPaint<'a>>) -> PaintInputs<'a> {
    let m = metrics(dpi);
    let (w, h) = (px(320.0, dpi), px(160.0, dpi));
    let outer = rect(0, 0, w, h);
    let strip = rect(0, 0, w, m.title_h);
    let layout = layout_title(&TitleLayoutInput {
        strip, dpi, want: ButtonSet { menu: true, up: false, close: true }, folder: false, text_w: px(60.0, dpi),
    });
    PaintInputs {
        outer, strip: Some(strip), strip_above: true, folded: false, dpi, square_edges: false,
        body_rgb: [32, 64, 160], fill_alpha: see_through_alpha(1380), shade: SHADE_NONE, outline: false,
        fade: 1.0, reveal: 1.0, strip_hover: 0.0, text: TextMode::Auto, backdrop_mean: [40, 40, 40],
        title, weight: DEFAULT_WEIGHT, italic: false, layout,
        menu: ButtonState::Rest, up: ButtonState::Rest, close: ButtonState::Rest, buttons: 1.0, tabs, high_contrast: None,
    }
}

fn record(p: &PaintInputs) -> Vec<Op> {
    let mut rec = Rec::default();
    paint_corral(&mut rec, p).unwrap();
    rec.0
}

// ---- 1 strip height -----------------------------------------------------------------------------

#[test]
fn strip_height_follows_the_table_for_segoe_metrics() {
    let want = [24, 30, 36, 48, 60];
    for (dpi, strip) in DPIS.into_iter().zip(want) {
        assert_eq!(strip_px(dpi, 1.079, 0.251, 0.0), strip, "dpi {dpi}");
        assert_eq!(metrics(dpi).title_h, strip, "metrics at {dpi}");
        assert_eq!(metrics(dpi).gap, 0, "the strip abuts the body");
    }
}

#[test]
fn a_taller_font_grows_the_strip_and_the_minimum_holds() {
    for dpi in DPIS {
        // ceil(1.6 * 12 * dpi / 96) in exact integer arithmetic
        let line = (16 * 12 * dpi).div_ceil(10 * 96) as i32;
        assert_eq!(strip_px(dpi, 1.2, 0.4, 0.0), line + 2 * px(4.0, dpi), "dpi {dpi}");
        assert!(strip_px(dpi, 0.5, 0.1, 0.0) >= px(24.0, dpi), "never below px(24)");
        assert_eq!(strip_px(dpi, 0.0, 0.0, 0.0), px(24.0, dpi));
    }
}

// ---- 2 corners ----------------------------------------------------------------------------------

#[test]
fn corner_radius_table_and_clamps() {
    for (dpi, radius) in DPIS.into_iter().zip([8.0f32, 10.0, 12.0, 16.0, 20.0]) {
        assert_eq!(px(RADIUS, dpi) as f32, radius);
        assert_eq!(corner_radius(px(320.0, dpi), px(160.0, dpi), dpi, false, None), radius, "dpi {dpi}");
        // a 30 x 24 DIP outer rect keeps the full radius
        assert_eq!(corner_radius(px(30.0, dpi), px(24.0, dpi), dpi, false, None), radius);
        // a 20 x 10 DIP one gets half its height
        assert_eq!(corner_radius(px(20.0, dpi), px(10.0, dpi), dpi, false, None), px(10.0, dpi) as f32 / 2.0);
        assert_eq!(corner_radius(px(320.0, dpi), px(160.0, dpi), dpi, true, None), 0.0, "SquareEdges");
        // folded: half the strip height
        assert_eq!(corner_radius(px(200.0, dpi), px(24.0, dpi), dpi, false, Some(px(24.0, dpi))), radius.min(px(24.0, dpi) as f32 / 2.0));
        assert_eq!(corner_radius(px(200.0, dpi), px(24.0, dpi), dpi, false, Some(px(10.0, dpi))), px(10.0, dpi) as f32 / 2.0);
    }
    assert_eq!(corner_radius(20, 10, 96, false, None), 5.0);
}

#[test]
fn strip_owns_the_corners_on_its_side() {
    assert_eq!(strip_radii(8.0, true, false), [8.0, 8.0, 0.0, 0.0]);
    assert_eq!(strip_radii(8.0, false, false), [0.0, 0.0, 8.0, 8.0]);
    assert_eq!(strip_radii(8.0, true, true), [8.0; 4]);
    assert_eq!(strip_radii(0.0, true, false), [0.0; 4]);
}

#[test]
fn strip_sits_against_the_body_on_either_side() {
    let m = metrics(96);
    let body = rect(100, 200, 400, 300);
    let above = title_rect_on(body, false, &m);
    assert_eq!(above, rect(100, 176, 400, 200));
    let below = title_rect_on(body, true, &m);
    assert_eq!(below, rect(100, 300, 400, 324));
    assert_eq!(union_rect(body, above, false), rect(100, 176, 400, 300));
    assert_eq!(union_rect(body, below, true), below, "folded: the strip alone");
    // boundary: room above needs the strip height
    assert!(near_monitor_top(rect(0, 23, 10, 50), 0, &m));
    assert!(!near_monitor_top(rect(0, 24, 10, 50), 0, &m));
}

// ---- 3 see-through ------------------------------------------------------------------------------

#[test]
fn see_through_endpoints_and_samples() {
    // higher = more opaque; the default 1380 is about 54 %, 2000 and above are solid
    for (v, alpha) in [(0, 20), (1000, 106), (1380, 138), (1690, 197), (2000, 255), (-50, 20), (2500, 255)] {
        assert_eq!(see_through_alpha(v), alpha, "v {v}");
    }
    let mut last = 0u8;
    for v in 0..=2000 {
        let a = see_through_alpha(v);
        assert!(a >= last, "monotone at {v}");
        last = a;
    }
    // close to what the stored values always drew (v / 10 of 255)
    for v in [200, 600, 1000, 1380] {
        assert!((see_through_alpha(v) as i32 - v as i32 / 10).abs() <= 25, "v {v}");
    }
    assert_eq!(see_through_alpha(1380) as f64 / 255.0 * 100.0, 138.0 / 255.0 * 100.0);
}

#[test]
fn fade_multiplies_alpha_and_rounds_half_up() {
    assert_eq!(apply_fade(112, 0.5), 56);
    assert_eq!(apply_fade(255, 1.0), 255);
    assert_eq!(apply_fade(255, 0.0), 0);
    assert_eq!(apply_fade(3, 0.5), 2);
    assert_eq!(apply_fade(200, 2.0), 200, "fade is clamped to 0..1");
}

// ---- 4 shades -----------------------------------------------------------------------------------

#[test]
fn shades_scale_the_colour_and_floor_the_alpha() {
    let c = [200, 100, 50];
    let dark = title_overlay(c, SHADE_DARK, 93).unwrap();
    assert_eq!((dark.r, dark.g, dark.b, dark.a), (150, 75, 38, 112));
    let darker = title_overlay(c, SHADE_DARKER, 93).unwrap();
    assert_eq!((darker.r, darker.g, darker.b, darker.a), (100, 50, 25, 128));
    assert_eq!(title_overlay(c, SHADE_DARK, 200).unwrap().a, 200);
    assert_eq!(title_overlay(c, SHADE_DARKER, 200).unwrap().a, 200);
    assert_eq!(title_overlay(c, SHADE_DARK, 255).unwrap().a, 255);
    assert_eq!(title_overlay(c, SHADE_NONE, 93), None);
    assert_eq!(apply_fade(112, 0.5), 56);
}

// ---- 5 outline ----------------------------------------------------------------------------------

#[test]
fn outline_mixes_35_percent_toward_the_contrasting_end() {
    assert_eq!(outline_color([30, 40, 60]), [109, 115, 128]);
    assert_eq!(outline_color([240, 240, 240]), [156, 156, 156]);
    assert_eq!(outline_opacity(1.0), 180);
    assert_eq!(outline_opacity(0.5), 90);
    let black = outline_color([0, 0, 0]);
    assert!(black.iter().all(|c| *c > 0), "black mixes toward white");
    let white = outline_color([255, 255, 255]);
    assert!(white.iter().all(|c| *c < 255), "white mixes toward black");
}

#[test]
fn outline_is_one_pixel_at_every_dpi_and_absent_when_off() {
    for dpi in DPIS {
        assert_eq!(outline_width(dpi, false), 1.0);
        let mut p = inputs(dpi, "Docs", None);
        p.outline = false;
        assert!(!record(&p).iter().any(|op| matches!(op, Op::Ring(..))), "off draws nothing");
        p.outline = true;
        let ops = record(&p);
        let ring = ops.iter().find_map(|op| if let Op::Ring(rect, _, w, color) = op { Some((*rect, *w, *color)) } else { None }).unwrap();
        assert_eq!(ring.1, 1.0);
        assert_eq!(ring.0, rectf(p.outer), "around the whole outer shape");
        assert_eq!(ring.2, rgba(outline_color(p.body_rgb), 180));
    }
}

// ---- 6 text colour ------------------------------------------------------------------------------

#[test]
fn auto_text_follows_wcag_contrast() {
    assert_eq!(auto_text([0, 0, 0]), [255, 255, 255]);
    assert_eq!(auto_text([255, 255, 255]), [0x1b, 0x1d, 0x22]);
    assert_eq!(auto_text([128, 128, 128]), [0x1b, 0x1d, 0x22]);
    assert!(prefers_light_text([0, 0, 0]) && !prefers_light_text([255, 255, 255]));
}

#[test]
fn a_darker_strip_over_a_mid_grey_body_flips_to_white_text_and_fade_does_not() {
    let mut p = inputs(96, "Docs", None);
    p.body_rgb = [128, 128, 128];
    p.fill_alpha = 255;
    p.backdrop_mean = [128, 128, 128];
    p.shade = SHADE_NONE;
    assert_eq!(p.text_rgb(), [0x1b, 0x1d, 0x22], "labels keep dark text on the body");
    p.shade = SHADE_DARKER;
    assert_eq!(p.text_rgb(), [255, 255, 255], "the Darker strip takes white");
    for fade in [1.0, 0.5, 0.1] {
        p.fade = fade;
        assert_eq!(p.text_rgb(), [255, 255, 255], "fade {fade}");
    }
    p.text = TextMode::Custom([1, 2, 3]);
    assert_eq!(p.text_rgb(), [1, 2, 3]);
}

#[test]
fn strip_backdrop_uses_the_real_alpha() {
    let c = strip_backdrop([0, 0, 0], [200, 200, 200], 255, None);
    assert_eq!(c, [200, 200, 200]);
    let c = strip_backdrop([0, 0, 0], [200, 200, 200], 0, None);
    assert_eq!(c, [0, 0, 0]);
    let o = title_overlay([200, 200, 200], SHADE_DARKER, 255);
    assert_eq!(strip_backdrop([0; 3], [200; 3], 255, o), [100, 100, 100]);
}

// ---- 7 title layout -----------------------------------------------------------------------------

fn layout_for(width_dip: f64, dpi: u32, want: ButtonSet, folder: bool, text_dip: f64) -> (TitleLayoutInput, TitleLayout) {
    let strip = rect(10, 20, 10 + px(width_dip, dpi), 20 + metrics(dpi).title_h);
    let input = TitleLayoutInput { strip, dpi, want, folder, text_w: px(text_dip, dpi) };
    (input, layout_title(&input))
}

fn combos() -> Vec<ButtonSet> {
    let mut all = Vec::new();
    for menu in [false, true] {
        for up in [false, true] {
            for close in [false, true] {
                all.push(ButtonSet { menu, up, close });
            }
        }
    }
    all
}

#[test]
fn title_layout_never_overlaps_and_keeps_a_readable_span() {
    for dpi in DPIS {
        for width in [64.0, 96.0, 160.0, 320.0] {
            for want in combos() {
                for folder in [false, true] {
                    for text in [0.0, 30.0, 90.0, 400.0] {
                        let (input, l) = layout_for(width, dpi, want, folder, text);
                        let strip = input.strip;
                        let mut parts: Vec<Rect> = [l.menu, l.up, l.close, l.folder, l.text].into_iter().flatten().collect();
                        for part in &parts {
                            assert!(part.left >= strip.left && part.right <= strip.right && part.top == strip.top && part.bottom == strip.bottom,
                                "inside the strip: {part:?} in {strip:?}");
                        }
                        parts.sort_by_key(|r| r.left);
                        for pair in parts.windows(2) {
                            assert!(pair[0].right <= pair[1].left, "overlap {pair:?} at {width} DIP, dpi {dpi}, {want:?}");
                        }
                        if let Some(t) = l.text {
                            assert!(l.span.1 - l.span.0 >= px(TEXT_MIN, dpi), "text only in a span of TEXT_MIN or more");
                            assert!(t.left >= l.span.0 && t.right <= l.span.1);
                        }
                        // buttons are never added to what was asked for
                        assert!(!l.menu.is_some() || want.menu);
                        assert!(!l.up.is_some() || want.up);
                        assert!(!l.close.is_some() || want.close);
                    }
                }
            }
        }
    }
}

#[test]
fn narrow_strips_drop_close_then_up_then_menu() {
    let all = ButtonSet { menu: true, up: true, close: true };
    for dpi in DPIS {
        let (_, l) = layout_for(64.0, dpi, all, false, 90.0);
        assert!(l.close.is_none() && l.up.is_none(), "close and Up dropped at 64 DIP");
        assert!(l.menu.is_some(), "the menu stays");
        assert!(l.text.is_some_and(|t| t.width() >= px(TEXT_MIN, dpi)), "about 24 DIP of title");
        let (_, l) = layout_for(160.0, dpi, all, false, 90.0);
        assert!(l.menu.is_some() && l.up.is_some() && l.close.is_some());
        // drop order as the width shrinks
        let mut dropped = Vec::new();
        for width in (30..=160).rev() {
            let (_, l) = layout_for(width as f64, dpi, all, false, 90.0);
            let state = (l.close.is_some(), l.up.is_some(), l.menu.is_some());
            if dropped.last() != Some(&state) {
                dropped.push(state);
            }
        }
        assert_eq!(dropped[0], (true, true, true));
        for pair in dropped.windows(2) {
            assert!(
                (pair[0].0 && !pair[1].0) || (pair[0].1 && !pair[1].1 && !pair[0].0) || (pair[0].2 && !pair[1].2 && !pair[0].1 && !pair[0].0),
                "close goes first, then Up, then menu: {pair:?}"
            );
        }
    }
}

#[test]
fn reserved_room_does_not_depend_on_hover() {
    // the layout has no hover input; the painted state only changes the plate
    for dpi in DPIS {
        let want = ButtonSet { menu: true, up: false, close: true };
        let (_, a) = layout_for(200.0, dpi, want, false, 60.0);
        let (_, b) = layout_for(200.0, dpi, want, false, 60.0);
        assert_eq!(a, b);
        let mut p = inputs(dpi, "Docs", None);
        let rest = record(&p).iter().filter(|op| matches!(op, Op::Glyph(..))).count();
        p.menu = ButtonState::Hover;
        let hover: Vec<_> = record(&p);
        assert_eq!(hover.iter().filter(|op| matches!(op, Op::Glyph(..))).count(), rest);
        assert_eq!(p.layout, inputs(dpi, "Docs", None).layout);
    }
}

#[test]
fn the_run_is_centred_in_the_span_and_left_aligned_when_wider() {
    for dpi in DPIS {
        let (input, l) = layout_for(320.0, dpi, ButtonSet { menu: true, up: false, close: false }, false, 60.0);
        let t = l.text.unwrap();
        assert_eq!(t.width(), px(60.0, dpi));
        let (left, right) = l.span;
        assert!(((t.left - left) - (right - t.right)).abs() <= 1, "centred in the span (+-1 px)");
        assert_eq!(input.strip.top, t.top);
        // wider than the span: left-aligned and trimmed to the span
        let (_, l) = layout_for(200.0, dpi, ButtonSet { menu: true, up: false, close: true }, false, 400.0);
        let t = l.text.unwrap();
        assert_eq!(t.left, l.span.0);
        assert_eq!(t.right, l.span.1);
        // a one-sided cluster shifts the run by half a button
        let (_, both) = layout_for(320.0, dpi, ButtonSet { menu: true, up: false, close: true }, false, 60.0);
        let (_, one) = layout_for(320.0, dpi, ButtonSet { menu: true, up: false, close: false }, false, 60.0);
        let shift = one.text.unwrap().left - both.text.unwrap().left;
        assert!((shift - (px(BTN_W, dpi) + px(CLUSTER_PAD, dpi) - px(EDGE_PAD, dpi) + px(TEXT_GAP, dpi)) / 2).abs() <= 2, "{shift}");
    }
}

#[test]
fn a_gate_run_starts_with_the_folder_glyph() {
    for dpi in DPIS {
        let (_, l) = layout_for(320.0, dpi, ButtonSet { menu: true, up: true, close: true }, true, 60.0);
        let (folder, text) = (l.folder.unwrap(), l.text.unwrap());
        assert_eq!(folder.width(), px(GLYPH, dpi));
        assert_eq!(text.left - folder.right, px(TEXT_GAP, dpi), "4 DIP gap");
    }
}

#[test]
fn empty_and_tiny_spans_draw_no_text() {
    for dpi in DPIS {
        let (_, l) = layout_for(320.0, dpi, ButtonSet::default(), false, 0.0);
        assert!(l.text.is_none(), "an empty title");
        let (_, l) = layout_for(30.0, dpi, ButtonSet::default(), false, 90.0);
        assert!(l.text.is_none(), "a span under TEXT_MIN (30 DIP minus 2 x 8 is 14)");
        let mut p = inputs(dpi, "", None);
        p.layout.text = None;
        assert!(!record(&p).iter().any(|op| matches!(op, Op::Text(..))));
    }
}

// ---- 10 buttons ---------------------------------------------------------------------------------

#[test]
fn buttons_are_24_dip_wide_and_as_tall_as_the_strip() {
    for dpi in DPIS {
        let (input, l) = layout_for(320.0, dpi, ButtonSet { menu: true, up: true, close: true }, false, 60.0);
        for button in [l.menu, l.up, l.close] {
            let b = button.unwrap();
            assert_eq!((b.width(), b.height()), (px(BTN_W, dpi), input.strip.height()));
        }
        assert_eq!(l.menu.unwrap().left, input.strip.left + px(CLUSTER_PAD, dpi));
        assert_eq!(l.up.unwrap().left, l.menu.unwrap().right);
        assert_eq!(l.close.unwrap().right, input.strip.right - px(CLUSTER_PAD, dpi));
        let plate = plate_rect(l.menu.unwrap(), dpi);
        assert_eq!(plate.left - rectf(l.menu.unwrap()).left, px(PLATE_INSET, dpi) as f32);
    }
}

#[test]
fn plate_and_glyph_alphas_follow_the_states() {
    assert_eq!(plate_alpha(ButtonState::Rest), 0);
    assert_eq!(plate_alpha(ButtonState::Hover), pct_alpha(14));
    assert_eq!(plate_alpha(ButtonState::Pressed), pct_alpha(24));
    assert_eq!(plate_alpha(ButtonState::Disabled), 0);
    assert_eq!((pct_alpha(14), pct_alpha(24), pct_alpha(40)), (36, 61, 102));
    assert_eq!(glyph_alpha(ButtonState::Rest), 255);
    assert_eq!(glyph_alpha(ButtonState::Disabled), 102);
}

#[test]
fn up_at_the_root_is_disabled_not_removed() {
    let mut p = inputs(96, "Docs", None);
    p.layout = layout_title(&TitleLayoutInput {
        strip: p.strip.unwrap(), dpi: 96, want: ButtonSet { menu: true, up: true, close: true }, folder: true, text_w: 60,
    });
    p.up = ButtonState::Disabled;
    let ops = record(&p);
    let up = ops.iter().find_map(|op| if let Op::Glyph(Glyph::Up, _, c) = op { Some(*c) } else { None }).expect("Up is drawn");
    assert_eq!(up.a, 102, "at 40 %");
    assert!(!ops.iter().any(|op| matches!(op, Op::Round(_, _, c) if c.a == plate_alpha(ButtonState::Hover))), "no plate");
    p.up = ButtonState::Pressed;
    let ops = record(&p);
    assert!(ops.iter().any(|op| matches!(op, Op::Round(_, _, c) if c.a == pct_alpha(24))), "pressed plate");
}

#[test]
fn up_is_always_visible_and_the_hover_buttons_follow_the_hover_level() {
    for dpi in DPIS {
        // Up alone (the hover toggle is off): leading cluster, from 4 DIP
        let (input, l) = layout_for(320.0, dpi, ButtonSet { menu: false, up: true, close: false }, true, 60.0);
        assert_eq!(l.up.unwrap().left, input.strip.left + px(CLUSTER_PAD, dpi));
        assert!(l.menu.is_none() && l.close.is_none());
        let mut p = inputs(dpi, "Docs", None);
        p.layout = layout_title(&TitleLayoutInput { strip: p.strip.unwrap(), dpi, want: ButtonSet { menu: true, up: true, close: true }, folder: true, text_w: px(60.0, dpi) });
        p.buttons = 0.0; // the pointer is away: menu and close are not shown
        let glyphs: Vec<Glyph> = record(&p).iter().filter_map(|op| if let Op::Glyph(g, _, _) = op { Some(*g) } else { None }).collect();
        assert!(glyphs.contains(&Glyph::Up), "Up stays: {glyphs:?}");
        assert!(!glyphs.contains(&Glyph::Menu) && !glyphs.contains(&Glyph::Close));
        let Some(Op::Glyph(_, _, c)) = record(&p).into_iter().find(|op| matches!(op, Op::Glyph(Glyph::Up, ..))) else { panic!() };
        assert_eq!(c.a, 255);
        p.buttons = 1.0;
        let glyphs: Vec<Glyph> = record(&p).iter().filter_map(|op| if let Op::Glyph(g, _, _) = op { Some(*g) } else { None }).collect();
        assert!(glyphs.contains(&Glyph::Menu) && glyphs.contains(&Glyph::Close));
    }
}

#[test]
fn hover_and_pressed_plates_are_drawn_under_the_glyph() {
    let mut p = inputs(96, "Docs", None);
    p.menu = ButtonState::Hover;
    let ops = record(&p);
    let plate = ops.iter().position(|op| matches!(op, Op::Round(_, r, c) if *r == 4.0 && c.a == 36)).expect("plate");
    let glyph = ops.iter().position(|op| matches!(op, Op::Glyph(Glyph::Menu, ..))).unwrap();
    assert!(plate < glyph);
    // the ink is the resolved text colour, scaled by fade
    p.fade = 0.5;
    let ops = record(&p);
    let Some(Op::Glyph(_, _, c)) = ops.iter().find(|op| matches!(op, Op::Glyph(Glyph::Menu, ..))) else { panic!() };
    assert_eq!(c.a, 128, "glyph alpha times fade");
}

// ---- 11 visibility ------------------------------------------------------------------------------

#[test]
fn visibility_modes() {
    // Always, On hover with the pointer away, Never, folded + Never
    assert_eq!(reveal_for(LABEL_ALWAYS, false, 0.0), 1.0);
    assert_eq!(reveal_for(LABEL_ON_HOVER, false, 0.0), 0.0);
    assert_eq!(reveal_for(LABEL_ON_HOVER, false, 0.5), 0.5);
    assert_eq!(reveal_for(LABEL_NEVER, false, 1.0), 0.0);
    assert_eq!(reveal_for(LABEL_NEVER, true, 0.0), 1.0, "folded shows its strip");
    assert!(strip_reserved(LABEL_ALWAYS, false) && strip_reserved(LABEL_ON_HOVER, false));
    assert!(!strip_reserved(LABEL_NEVER, false));
    assert!(strip_reserved(LABEL_NEVER, true));
}

#[test]
fn a_reserved_strip_with_reveal_zero_is_one_plain_rectangle() {
    let mut p = inputs(96, "Docs", None);
    p.shade = SHADE_DARKER;
    p.strip_hover = 1.0;
    p.reveal = 0.0;
    let ops = record(&p);
    assert_eq!(ops.len(), 1, "only the body fill: {ops:?}");
    assert!(matches!(ops[0], Op::Shape(r, ..) if r == rectf(p.outer)), "the body covers the whole outer rect");
    p.reveal = 1.0;
    assert!(record(&p).len() > 3);
}

#[test]
fn never_has_no_strip_and_no_text() {
    let mut p = inputs(96, "Docs", None);
    p.strip = None;
    p.layout = TitleLayout::default();
    let ops = record(&p);
    assert!(ops.iter().all(|op| matches!(op, Op::Shape(..))), "{ops:?}");
}

#[test]
fn reveal_durations_are_120_in_and_200_out() {
    assert_eq!((REVEAL_IN_MS, REVEAL_OUT_MS), (120.0, 200.0));
}

#[test]
fn a_folded_corral_is_the_strip_with_four_rounded_corners() {
    for dpi in DPIS {
        let m = metrics(dpi);
        let mut p = inputs(dpi, "Docs", None);
        let strip = rect(0, 0, px(200.0, dpi), m.title_h);
        p.outer = strip;
        p.strip = Some(strip);
        p.folded = true;
        p.shade = SHADE_DARK;
        p.outline = true;
        let ops = record(&p);
        let radius = corner_radius(strip.width(), strip.height(), dpi, false, Some(strip.height()));
        assert!(ops.iter().any(|op| matches!(op, Op::Shape(r, radii, _) if *r == rectf(strip) && *radii == [radius; 4])));
        assert!(ops.iter().any(|op| matches!(op, Op::Ring(r, radii, ..) if *r == rectf(strip) && *radii == [radius; 4])), "outline around the strip alone");
    }
}

// ---- 12 tabs ------------------------------------------------------------------------------------

fn tab(title: &str, left: i32, right: i32, active: bool) -> TabItem {
    TabItem { title: title.into(), hit: rect(left, 0, right, 24), active, hovered: false, own_color: None, custom_text: None, weight: 600, italic: false }
}

#[test]
fn every_tab_style_makes_its_documented_primitives() {
    let tabs = [tab("A", 0, 80, true), tab("B", 80, 160, false)];
    let fills = |style: u32| {
        let mut p = inputs(96, "", Some(TabsPaint { style, tabs: &tabs }));
        p.strip = Some(rect(0, 0, 320, 24));
        let mut rec = Rec::default();
        paint_fills(&mut rec, &p).unwrap();
        rec.0.into_iter().skip(1).collect::<Vec<_>>()
    };
    assert!(fills(TABS_MENU_ONLY).is_empty(), "Menu only has no strip");
    let flat = fills(TABS_FLAT);
    assert!(matches!(flat.as_slice(), [Op::Rect(bar, _)] if bar.bottom == 24.0 && bar.top == 22.0 && bar.left == 0.0 && bar.right == 80.0),
        "an active 2 DIP bar on the edge facing the body: {flat:?}");
    let seg = fills(TABS_SEGMENTED);
    assert!(matches!(seg[0], Op::Round(r, rad, c) if rad == 6.0 && r.left == 2.0 && c.a == pct_alpha(10)), "track {seg:?}");
    assert!(matches!(seg[1], Op::Round(_, rad, c) if rad == 4.0 && c.a == pct_alpha(22)), "active segment");
    assert_eq!(seg.len(), 2, "inactive, not hovered: no fill");
    let round = fills(TABS_ROUNDED);
    assert_eq!(round.len(), 2);
    assert!(matches!(round[0], Op::Round(r, rad, c) if c.a == pct_alpha(22) && r.bottom - r.top == 16.0 && rad == 8.0), "active pill {round:?}");
    assert!(matches!(round[1], Op::Round(_, _, c) if c.a == pct_alpha(8)), "inactive pill");
    let gap = if let (Op::Round(a, ..), Op::Round(b, ..)) = (&round[0], &round[1]) { b.left - a.right } else { 0.0 };
    assert_eq!(gap, 4.0, "pills 4 DIP apart");
}

#[test]
fn flat_bar_faces_the_body_on_either_side() {
    let tabs = [tab("A", 0, 80, true)];
    let mut p = inputs(96, "", Some(TabsPaint { style: TABS_FLAT, tabs: &tabs }));
    p.strip = Some(rect(0, 0, 320, 24));
    p.strip_above = false;
    let mut rec = Rec::default();
    paint_fills(&mut rec, &p).unwrap();
    assert!(matches!(rec.0[1], Op::Rect(bar, _) if bar.top == 0.0 && bar.bottom == 2.0), "{:?}", rec.0);
}

#[test]
fn tabs_colors_use_each_tabs_own_colour_and_text_follows_it() {
    let mut a = tab("A", 0, 80, true);
    let mut b = tab("B", 80, 160, false);
    a.own_color = Some([200, 40, 40]);
    b.own_color = Some([240, 240, 120]);
    let tabs = [a, b];
    let mut p = inputs(96, "", Some(TabsPaint { style: TABS_ROUNDED, tabs: &tabs }));
    p.strip = Some(rect(0, 0, 320, 24));
    let ops = record(&p);
    assert!(ops.iter().any(|op| matches!(op, Op::Round(_, _, c) if (c.r, c.g, c.b, c.a) == (200, 40, 40, 255))), "active at 255");
    assert!(ops.iter().any(|op| matches!(op, Op::Round(_, _, c) if (c.r, c.g, c.b, c.a) == (240, 240, 120, 120))), "inactive at 120");
    let edge = outline_color([200, 40, 40]);
    assert!(ops.iter().any(|op| matches!(op, Op::Ring(_, _, w, c) if *w == 1.0 && [c.r, c.g, c.b] == edge)), "1 px edge in the outline mix");
    let texts: Vec<_> = ops.iter().filter_map(|op| if let Op::Text(t, _, c) = op { Some((t.clone(), *c)) } else { None }).collect();
    assert_eq!(texts[0].1.r, auto_text([200, 40, 40])[0]);
    assert_eq!([texts[1].1.r, texts[1].1.g, texts[1].1.b], auto_text([240, 240, 120]));
    assert_eq!(texts[0].1.a, 255);
    assert_eq!(texts[1].1.a, pct_alpha(70), "inactive text at 70 %");
}

#[test]
fn the_tab_strip_is_hidden_with_the_title() {
    let tabs = [tab("A", 0, 80, true), tab("B", 80, 160, false)];
    let mut p = inputs(96, "", Some(TabsPaint { style: TABS_SEGMENTED, tabs: &tabs }));
    p.strip = None;
    p.layout = TitleLayout::default();
    assert!(record(&p).iter().all(|op| matches!(op, Op::Shape(..))));
    p.strip = Some(rect(0, 0, 320, 24));
    p.reveal = 0.0;
    assert_eq!(record(&p).len(), 1, "a reserved strip that is not shown draws no tabs");
}

// ---- 13 paint order -----------------------------------------------------------------------------

#[test]
fn draw_calls_arrive_in_the_order_of_the_spec() {
    let tabs = [tab("A", 0, 80, true), tab("B", 80, 160, false)];
    let mut p = inputs(96, "Docs", Some(TabsPaint { style: TABS_ROUNDED, tabs: &tabs }));
    p.shade = SHADE_DARK;
    p.outline = true;
    p.strip_hover = 1.0;
    p.strip = Some(rect(0, 0, 320, 24));
    let ops = record(&p);
    let index = |f: &dyn Fn(&Op) -> bool| ops.iter().position(f).unwrap();
    let body = index(&|op| matches!(op, Op::Shape(r, _, _) if *r == rectf(p.outer)));
    let overlay = index(&|op| matches!(op, Op::Shape(r, radii, _) if *r == rectf(p.strip.unwrap()) && radii[2] == 0.0));
    let tab_fill = index(&|op| matches!(op, Op::Round(_, _, c) if c.a == pct_alpha(22)));
    let outline = index(&|op| matches!(op, Op::Ring(..)));
    let glyph = index(&|op| matches!(op, Op::Glyph(..)));
    let text = index(&|op| matches!(op, Op::Text(..)));
    assert!(body < overlay && overlay < tab_fill && tab_fill < outline && outline < glyph && outline < text, "{ops:?}");
    // the hover mix follows the shade overlay
    let mix = ops.iter().rposition(|op| matches!(op, Op::Shape(_, _, c) if c.a == HOVER_MIX_ALPHA)).unwrap();
    assert!(overlay < mix && mix < tab_fill);
}

#[test]
fn fade_scales_every_alpha_and_leaves_rgb_alone() {
    let tabs = [tab("A", 0, 80, true), tab("B", 80, 160, false)];
    let mut p = inputs(96, "Docs", Some(TabsPaint { style: TABS_ROUNDED, tabs: &tabs }));
    p.shade = SHADE_DARK;
    p.outline = true;
    p.strip_hover = 1.0;
    p.strip = Some(rect(0, 0, 320, 24));
    let full = record(&p);
    p.fade = 0.5;
    let half = record(&p);
    assert_eq!(full.len(), half.len());
    let parts = |op: &Op| -> ([u8; 3], u8) {
        let c = match op {
            Op::Shape(_, _, c) | Op::Ring(_, _, _, c) | Op::Round(_, _, c) | Op::Rect(_, c) | Op::Text(_, _, c) | Op::Glyph(_, _, c) | Op::Line(c) => *c,
        };
        ([c.r, c.g, c.b], c.a)
    };
    for (a, b) in full.iter().zip(&half) {
        let ((rgb_a, alpha_a), (rgb_b, alpha_b)) = (parts(a), parts(b));
        assert_eq!(rgb_a, rgb_b, "RGB unchanged");
        assert_eq!(alpha_b, apply_fade(alpha_a, 0.5), "{a:?} -> {b:?}");
    }
}

#[test]
fn the_default_and_solid_values_render_as_stored_values_always_did() {
    // TransparencyFactor is how solid the Corral is: 1380 about 54 %, 2000 and above fully solid
    let mut p = inputs(96, "Docs", None);
    for (stored, alpha) in [(1380, 138u8), (2000, 255), (2550, 255)] {
        p.fill_alpha = see_through_alpha(stored);
        let Some(Op::Shape(_, _, body)) = record(&p).first().cloned() else { panic!() };
        assert_eq!(body.a, alpha, "stored {stored}");
    }
    assert_eq!((138.0f64 / 255.0 * 100.0).round(), 54.0);
}

#[test]
fn tab_labels_use_their_members_weight_and_italic() {
    let mut a = tab("A", 0, 80, true);
    let mut b = tab("B", 80, 160, false);
    a.weight = 700;
    b.italic = true;
    let tabs = [a, b];
    let mut p = inputs(96, "", Some(TabsPaint { style: TABS_FLAT, tabs: &tabs }));
    p.strip = Some(rect(0, 0, 320, 24));
    let mut rec = Rec::default();
    paint_corral(&mut rec, &p).unwrap();
    assert_eq!(rec.1.len(), 2);
    assert_eq!((rec.1[0].weight, rec.1[0].italic), (700, false));
    assert_eq!((rec.1[1].weight, rec.1[1].italic), (600, true));
}

#[test]
fn titles_are_measured_with_the_font_they_are_drawn_with() {
    let regular = measure_title("Documents and settings", 96, 400, false);
    let bold = measure_title("Documents and settings", 96, 900, false);
    assert!(bold > regular, "weight changes the width: {regular} vs {bold}");
}

#[test]
fn a_fully_faded_corral_stays_hit_testable_at_alpha_one() {
    let mut p = inputs(96, "Docs", None);
    p.fade = 0.0;
    let ops = record(&p);
    let Some(Op::Shape(_, _, body)) = ops.first() else { panic!("{ops:?}") };
    assert_eq!(body.a, 1, "layered windows are click-through at alpha 0");
    // an already clear body is not raised above its own alpha floor
    p.fade = 1.0;
    p.fill_alpha = see_through_alpha(0);
    let Some(Op::Shape(_, _, body)) = record(&p).first().cloned() else { panic!() };
    assert_eq!(body.a, 20);
}

#[test]
fn the_outline_comes_after_camouflage_recoloured_fills() {
    // the caller recolours `body_rgb` before painting; the outline mixes from that final colour
    let mut p = inputs(96, "Docs", None);
    p.outline = true;
    p.body_rgb = [250, 250, 250];
    let ops = record(&p);
    let ring = ops.iter().find_map(|op| if let Op::Ring(_, _, _, c) = op { Some(*c) } else { None }).unwrap();
    assert_eq!([ring.r, ring.g, ring.b], outline_color([250, 250, 250]));
}

// ---- 14 high contrast ---------------------------------------------------------------------------

#[test]
fn high_contrast_is_opaque_system_coloured_and_ignores_looks() {
    let hc = HighContrast { window: [0, 0, 0], window_text: [255, 255, 0], highlight: [0, 255, 255], highlight_text: [0, 0, 0] };
    let tabs = [tab("A", 0, 80, true), tab("B", 80, 160, false)];
    for dpi in DPIS {
        let mut p = inputs(dpi, "Docs", Some(TabsPaint { style: TABS_ROUNDED, tabs: &tabs }));
        p.high_contrast = Some(hc);
        p.shade = SHADE_DARKER;
        p.outline = false;
        p.fade = 0.3;
        p.fill_alpha = 20;
        p.strip_hover = 1.0;
        p.menu = ButtonState::Hover;
        let ops = record(&p);
        let allowed = [hc.window, hc.window_text, hc.highlight, hc.highlight_text];
        for op in &ops {
            let c = match op {
                Op::Shape(_, _, c) | Op::Ring(_, _, _, c) | Op::Round(_, _, c) | Op::Rect(_, c) | Op::Text(_, _, c) | Op::Glyph(_, _, c) | Op::Line(c) => *c,
            };
            assert_eq!(c.a, 255, "all alphas 255: {op:?}");
            assert!(allowed.contains(&[c.r, c.g, c.b]), "system colours only: {op:?}");
        }
        let ring = ops.iter().find_map(|op| if let Op::Ring(_, _, w, c) = op { Some((*w, *c)) } else { None }).expect("outline is always on");
        assert_eq!(ring.0, 2.0f32.max(px(1.0, dpi) as f32));
        assert_eq!([ring.1.r, ring.1.g, ring.1.b], hc.window_text);
        let body = ops.iter().find_map(|op| if let Op::Shape(_, _, c) = op { Some(*c) } else { None }).unwrap();
        assert_eq!([body.r, body.g, body.b], hc.window);
        assert_eq!(ops.iter().filter(|op| matches!(op, Op::Shape(..))).count(), 1, "no shade overlay or hover mix");
        // active tab filled HIGHLIGHT with HIGHLIGHTTEXT; hover plate HIGHLIGHT
        assert!(ops.iter().any(|op| matches!(op, Op::Round(_, _, c) if [c.r, c.g, c.b] == hc.highlight)));
        let texts: Vec<_> = ops.iter().filter_map(|op| if let Op::Text(_, _, c) = op { Some([c.r, c.g, c.b]) } else { None }).collect();
        assert_eq!(texts, [hc.highlight_text, hc.window_text]);
    }
}

#[test]
fn high_contrast_outline_width_is_at_least_two_pixels() {
    for dpi in DPIS {
        assert_eq!(outline_width(dpi, true), 2.0f32.max(px(1.0, dpi) as f32));
    }
    assert_eq!(outline_width(240, true), 3.0, "px(1) at 240 dpi is 3");
    assert_eq!(outline_width(96, true), 2.0);
}

// ---- text, plumbing ------------------------------------------------------------------------------

#[test]
fn title_weight_defaults_to_semibold_and_stored_wins() {
    assert_eq!(title_weight(None), 600);
    assert_eq!(title_weight(Some(700)), 700);
    assert_eq!(title_weight(Some(0)), 1);
    assert_eq!(title_weight(Some(5000)), 999);
}

#[test]
fn font_sizes_scale_without_rounding() {
    let table = [(96, 12.0, 14.0), (120, 15.0, 17.5), (144, 18.0, 21.0), (192, 24.0, 28.0), (240, 30.0, 35.0)];
    for (dpi, title, glyph) in table {
        assert_eq!((title_text_px(dpi), glyph_px(dpi)), (title, glyph), "dpi {dpi}");
    }
    for (dpi, tab_min) in DPIS.into_iter().zip([48, 60, 72, 96, 120]) {
        assert_eq!(px(48.0, dpi), tab_min);
    }
}


// ---- real drawing -------------------------------------------------------------------------------

mod render {
    use super::*;
    use crate::render::Surface;

    fn ink_columns(surface: &Surface, rect: Rect) -> Option<(i32, i32)> {
        let w = surface.width() as usize;
        let mut first = None;
        let mut last = None;
        for y in rect.top.max(0)..rect.bottom {
            for x in 0..w as i32 {
                if surface.pixels()[(y as usize * w + x as usize) * 4 + 3] > 40 {
                    first = Some(first.map_or(x, |f: i32| f.min(x)));
                    last = Some(last.map_or(x, |l: i32| l.max(x)));
                }
            }
        }
        first.zip(last)
    }

    #[test]
    fn long_titles_trim_inside_their_rect_and_short_ones_are_whole() {
        for dpi in [96, 192] {
            let m = metrics(dpi);
            let (w, h) = (px(300.0, dpi) as u32, m.title_h as u32);
            let span = rect(px(20.0, dpi), 0, px(120.0, dpi), m.title_h);
            let draw = |title: &str| {
                let mut surface = Surface::new(w, h).unwrap();
                surface
                    .draw(|ctx| {
                        let mut sink = CtxSink(ctx);
                        let font = TitleFont { family: title_family(), size_px: title_text_px(dpi), weight: 600, italic: false };
                        sink.text(title, rectf(span), &font, Rgba::new(255, 255, 255, 255), Align::Left)
                    })
                    .unwrap();
                surface
            };
            let long = draw("A very long Corral name that cannot possibly fit into this span");
            let (first, last) = ink_columns(&long, rect(0, 0, w as i32, h as i32)).expect("ink");
            assert!(first >= span.left - 1 && last < span.right, "dpi {dpi}: ink {first}..{last} outside {span:?}");
            assert!(last > span.right - px(30.0, dpi), "the trimmed text fills the span up to the ellipsis");
            let short = draw("Docs");
            let (first, last) = ink_columns(&short, rect(0, 0, w as i32, h as i32)).expect("ink");
            let measured = crate::render::measure_text("Docs", title_family(), title_text_px(dpi), 600).unwrap().0;
            assert!(first >= span.left - 1 && (last - first) as f32 <= measured + 2.0 && last < span.right, "dpi {dpi}");
        }
    }

    #[test]
    fn glyphs_draw_ink_from_the_installed_font_or_the_vector_fallback() {
        for dpi in [96, 144, 240] {
            for glyph in Glyph::ALL {
                let b = px(BTN_W, dpi);
                let mut surface = Surface::new(b as u32, b as u32).unwrap();
                surface
                    .draw(|ctx| {
                        let mut sink = CtxSink(ctx);
                        sink.glyph(glyph, RectF::new(0.0, 0.0, b as f32, b as f32), glyph_px(dpi), Rgba::new(255, 255, 255, 255))
                    })
                    .unwrap();
                let ink = surface.pixels().chunks_exact(4).filter(|p| p[3] > 60).count();
                assert!(ink > 8, "{glyph:?} at dpi {dpi} drew {ink} px");
            }
        }
    }

    /// Writes sample Corral PNGs to `target/corral-painting/` for review by eye (spec 9 test 15).
    /// Run once: `cargo test --offline corral_sample_renders -- --ignored`.
    #[test]
    #[ignore = "writes review images"]
    fn corral_sample_renders() {
        let dir = std::path::Path::new("target/corral-painting");
        std::fs::create_dir_all(dir).unwrap();
        let backdrops: [(&str, Rgb); 3] = [("dark", [24, 28, 36]), ("light", [236, 238, 240]), ("grey", [128, 128, 128])];
        let mut written = Vec::new();
        for (dpi, scale) in [(96u32, "100"), (240, "250")] {
            for (bname, bcolor) in backdrops {
                for (name, shade, outline, square, style, folded, above) in [
                    ("plain", SHADE_NONE, false, false, 0, false, true),
                    ("dark-shade", SHADE_DARK, false, false, 0, false, true),
                    ("darker-shade", SHADE_DARKER, false, false, 0, false, true),
                    ("outline", SHADE_NONE, true, false, 0, false, true),
                    ("square", SHADE_DARK, true, true, 0, false, true),
                    ("below", SHADE_DARK, true, false, 0, false, false),
                    ("folded", SHADE_DARK, true, false, 0, true, true),
                    ("tabs-flat", SHADE_DARK, true, false, TABS_FLAT, false, true),
                    ("tabs-segmented", SHADE_DARK, true, false, TABS_SEGMENTED, false, true),
                    ("tabs-rounded", SHADE_DARK, true, false, TABS_ROUNDED, false, true),
                ] {
                    written.push(render_sample(dir, dpi, scale, bname, bcolor, name, shade, outline, square, style, folded, above));
                }
            }
        }
        println!("{} images in {}", written.len(), dir.display());
        for w in &written {
            println!("{w}");
        }
        assert!(written.len() >= 60);
    }

    #[allow(clippy::too_many_arguments)]
    fn render_sample(dir: &std::path::Path, dpi: u32, scale: &str, bname: &str, backdrop: Rgb, name: &str, shade: u32,
        outline: bool, square: bool, style: u32, folded: bool, above: bool) -> String {
        let m = metrics(dpi);
        let (pad, bw, bh) = (px(16.0, dpi), px(300.0, dpi), px(110.0, dpi));
        let strip_h = m.title_h;
        let (w, h) = (bw + 2 * pad, if folded { strip_h } else { bw / 3 + bh / 2 + strip_h } + 2 * pad);
        let outer_h = if folded { strip_h } else { bh + strip_h };
        let outer = rect(pad, pad, pad + bw, pad + outer_h);
        let strip = if above { rect(outer.left, outer.top, outer.right, outer.top + strip_h) } else { rect(outer.left, outer.bottom - strip_h, outer.right, outer.bottom) };
        let want = ButtonSet { menu: true, up: false, close: true };
        let title = "Documents";
        let text_w = crate::render::measure_text(title, title_family(), title_text_px(dpi), 600).map_or(0, |t| t.0.ceil() as i32);
        let layout = layout_title(&TitleLayoutInput { strip, dpi, want, folder: false, text_w });
        let names = ["Docs", "Photos", "Music"];
        let cell_w = (strip.width() - px(2.0 * 28.0, dpi)) / 3;
        let items: Vec<TabItem> = names.iter().enumerate().map(|(i, n)| TabItem {
            title: n.to_string(),
            hit: rect(strip.left + px(28.0, dpi) + i as i32 * cell_w, strip.top, strip.left + px(28.0, dpi) + (i as i32 + 1) * cell_w, strip.bottom),
            active: i == 0, hovered: i == 1, own_color: (style != 0 && i == 2).then_some([200, 90, 60]), custom_text: None, weight: 600, italic: false,
        }).collect();
        let tabs = (style != 0).then_some(TabsPaint { style, tabs: &items });
        let p = PaintInputs {
            outer, strip: Some(strip), strip_above: above, folded, dpi, square_edges: square,
            body_rgb: [48, 96, 168], fill_alpha: see_through_alpha(1380), shade, outline, fade: 1.0, reveal: 1.0,
            strip_hover: 0.0, text: TextMode::Auto, backdrop_mean: backdrop, title, weight: 600, italic: false, layout,
            menu: ButtonState::Hover, up: ButtonState::Rest, close: ButtonState::Rest, buttons: 1.0, tabs, high_contrast: None,
        };
        let mut surface = Surface::new(w as u32, h as u32).unwrap();
        surface
            .draw(|ctx| {
                ctx.clear(Rgba::new(backdrop[0], backdrop[1], backdrop[2], 255));
                // simple wallpaper detail under the Corral so the see-through is visible
                for i in 0..8 {
                    let x = (i * w / 8) as f32;
                    ctx.fill_rect(RectF::new(x, 0.0, x + (w / 16) as f32, h as f32), Rgba::new(255, 255, 255, 18))?;
                }
                let mut sink = CtxSink(ctx);
                paint_corral(&mut sink, &p)?;
                if !folded {
                    // two icon stand-ins in the body
                    let body_top = if above { strip.bottom } else { outer.top };
                    for k in 0..3 {
                        let s = px(32.0, dpi) as f32;
                        let x = (outer.left + px(24.0, dpi) + k * px(84.0, dpi)) as f32;
                        let y = (body_top + px(16.0, dpi)) as f32;
                        ctx.fill_rounded_rect(RectF::new(x, y, x + s, y + s), px(4.0, dpi) as f32, Rgba::new(240, 190, 65, 255))?;
                    }
                }
                Ok(())
            })
            .unwrap();
        let file = format!("corral-{scale}-{bname}-{name}.png");
        surface.save_png(&dir.join(&file)).unwrap();
        file
    }
}
