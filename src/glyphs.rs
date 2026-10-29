//! Title-strip glyphs (corral-painting-spec 6). The glyphs are characters of Segoe Fluent Icons, with
//! Segoe MDL2 Assets as the fallback family (same code points for these six). The font is chosen once
//! per process and checked per code point; a code point neither font has is drawn as a built-in
//! vector shape instead, so a button is never blank. No bitmaps.

use crate::render::RectF;
use std::sync::OnceLock;

pub const FLUENT: &str = "Segoe Fluent Icons";
pub const MDL2: &str = "Segoe MDL2 Assets";

/// The six title glyphs (spec 6 table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// GlobalNavButton: opens the Corral menu.
    Menu,
    /// ChevronUp: fold (Fold on, Corral open).
    Collapse,
    /// ChevronDown: unfold.
    Expand,
    /// ChromeClose: the close action.
    Close,
    /// Folder: inline before a Gate's name, not a button.
    Folder,
    /// Up: parent folder of a Gate.
    Up,
}

impl Glyph {
    pub const ALL: [Glyph; 6] = [Glyph::Menu, Glyph::Collapse, Glyph::Expand, Glyph::Close, Glyph::Folder, Glyph::Up];

    /// Private-use code point in both icon fonts (one UTF-16 code unit).
    pub const fn codepoint(self) -> u32 {
        match self {
            Glyph::Menu => 0xE700,
            Glyph::Collapse => 0xE70E,
            Glyph::Expand => 0xE70D,
            Glyph::Close => 0xE8BB,
            Glyph::Folder => 0xE8B7,
            Glyph::Up => 0xE74A,
        }
    }

    pub fn ch(self) -> char {
        char::from_u32(self.codepoint()).unwrap_or('\u{E700}')
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|g| *g == self).unwrap_or(0)
    }
}

/// What draws one glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconFont {
    Fluent,
    Mdl2,
    /// Neither font has the code point: strokes from [`vector_strokes`].
    Vector,
}

impl IconFont {
    pub const fn family(self) -> Option<&'static str> {
        match self {
            IconFont::Fluent => Some(FLUENT),
            IconFont::Mdl2 => Some(MDL2),
            IconFont::Vector => None,
        }
    }
}

/// Fluent when installed and it has the glyph, else MDL2 under the same two conditions, else the
/// vector shape. The decision is per glyph: one missing code point does not demote the others.
pub fn pick_icon_font(available: &dyn Fn(&str) -> bool, has_glyph: &dyn Fn(&str, u32) -> bool, glyph: Glyph) -> IconFont {
    let cp = glyph.codepoint();
    for (family, font) in [(FLUENT, IconFont::Fluent), (MDL2, IconFont::Mdl2)] {
        if available(family) && has_glyph(family, cp) {
            return font;
        }
    }
    IconFont::Vector
}

/// The installed choice for `glyph`, resolved once per process with DirectWrite
/// (`FindFamilyName` + `GetGlyphIndices`).
pub fn font_for(glyph: Glyph) -> IconFont {
    static CHOICE: OnceLock<[IconFont; 6]> = OnceLock::new();
    CHOICE.get_or_init(|| {
        let available = |family: &str| crate::render::font_family_exists(family);
        let has_glyph = |family: &str, cp: u32| {
            crate::render::font_has_glyphs(family, &[cp]).is_some_and(|v| v.first().copied().unwrap_or(false))
        };
        Glyph::ALL.map(|g| pick_icon_font(&available, &has_glyph, g))
    })[glyph.index()]
}

/// One straight stroke of a vector glyph, in the coordinates of the glyph box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub from: (f32, f32),
    pub to: (f32, f32),
}

/// Stroke width of the vector shapes for a glyph box of `size` px: 1 DIP, never thinner than one pixel.
pub fn vector_stroke_width(size: f32) -> f32 {
    (size / 14.0).max(1.0)
}

/// Built-in outline of a glyph inside the square `rect` (the glyph box, 14 DIP): three lines for the
/// menu, an X, chevron strokes, arrow strokes and a folder outline (spec 6).
pub fn vector_strokes(glyph: Glyph, rect: RectF) -> Vec<Segment> {
    let size = (rect.right - rect.left).min(rect.bottom - rect.top).max(0.0);
    let (cx, cy) = ((rect.left + rect.right) / 2.0, (rect.top + rect.bottom) / 2.0);
    let u = size / 14.0;
    let at = |x: f32, y: f32| (cx + x * u, cy + y * u);
    let seg = |a: (f32, f32), b: (f32, f32)| Segment { from: at(a.0, a.1), to: at(b.0, b.1) };
    match glyph {
        Glyph::Menu => [-4.0, 0.0, 4.0].iter().map(|y| seg((-6.0, *y), (6.0, *y))).collect(),
        Glyph::Close => vec![seg((-5.0, -5.0), (5.0, 5.0)), seg((5.0, -5.0), (-5.0, 5.0))],
        Glyph::Collapse => vec![seg((-5.0, 2.5), (0.0, -2.5)), seg((0.0, -2.5), (5.0, 2.5))],
        Glyph::Expand => vec![seg((-5.0, -2.5), (0.0, 2.5)), seg((0.0, 2.5), (5.0, -2.5))],
        Glyph::Up => vec![seg((0.0, 6.0), (0.0, -6.0)), seg((-5.0, -1.0), (0.0, -6.0)), seg((5.0, -1.0), (0.0, -6.0))],
        Glyph::Folder => vec![
            seg((-6.5, -4.0), (-6.5, 5.0)),
            seg((-6.5, 5.0), (6.5, 5.0)),
            seg((6.5, 5.0), (6.5, -2.5)),
            seg((6.5, -2.5), (0.0, -2.5)),
            seg((0.0, -2.5), (-1.5, -4.0)),
            seg((-1.5, -4.0), (-6.5, -4.0)),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codepoints_are_the_six_of_the_spec_table() {
        let table = [
            (Glyph::Menu, 0xE700),
            (Glyph::Collapse, 0xE70E),
            (Glyph::Expand, 0xE70D),
            (Glyph::Close, 0xE8BB),
            (Glyph::Folder, 0xE8B7),
            (Glyph::Up, 0xE74A),
        ];
        for (glyph, cp) in table {
            assert_eq!(glyph.codepoint(), cp);
            assert_eq!(glyph.ch() as u32, cp);
            assert_eq!(glyph.ch().len_utf16(), 1, "one UTF-16 code unit");
        }
        assert_eq!(Glyph::ALL.len(), 6);
    }

    #[test]
    fn icon_font_choice_is_fluent_then_mdl2_then_vector() {
        let both = |_: &str| true;
        let every = |_: &str, _: u32| true;
        assert_eq!(pick_icon_font(&both, &every, Glyph::Menu), IconFont::Fluent);
        let only_mdl2 = |f: &str| f == MDL2;
        assert_eq!(pick_icon_font(&only_mdl2, &every, Glyph::Menu), IconFont::Mdl2);
        let none = |_: &str| false;
        assert_eq!(pick_icon_font(&none, &every, Glyph::Menu), IconFont::Vector);
        // installed but without the glyph is as good as missing
        let blank = |_: &str, _: u32| false;
        assert_eq!(pick_icon_font(&both, &blank, Glyph::Menu), IconFont::Vector);
    }

    #[test]
    fn a_missing_code_point_in_fluent_falls_back_for_that_glyph_only() {
        let both = |_: &str| true;
        let fluent_lacks_up = |f: &str, cp: u32| !(f == FLUENT && cp == Glyph::Up.codepoint());
        for glyph in Glyph::ALL {
            let expected = if glyph == Glyph::Up { IconFont::Mdl2 } else { IconFont::Fluent };
            assert_eq!(pick_icon_font(&both, &fluent_lacks_up, glyph), expected, "{glyph:?}");
        }
        let neither_has_folder = |_: &str, cp: u32| cp != Glyph::Folder.codepoint();
        assert_eq!(pick_icon_font(&both, &neither_has_folder, Glyph::Folder), IconFont::Vector);
        assert_eq!(pick_icon_font(&both, &neither_has_folder, Glyph::Close), IconFont::Fluent);
    }

    #[test]
    fn families_name_the_two_icon_fonts() {
        assert_eq!(IconFont::Fluent.family(), Some("Segoe Fluent Icons"));
        assert_eq!(IconFont::Mdl2.family(), Some("Segoe MDL2 Assets"));
        assert_eq!(IconFont::Vector.family(), None);
    }

    #[test]
    fn vector_shapes_stay_inside_the_box_and_have_strokes() {
        for size in [14.0f32, 17.5, 21.0, 28.0, 35.0] {
            let rect = RectF::new(10.0, 20.0, 10.0 + size, 20.0 + size);
            for glyph in Glyph::ALL {
                let strokes = vector_strokes(glyph, rect);
                assert!(!strokes.is_empty(), "{glyph:?}");
                for s in strokes {
                    for p in [s.from, s.to] {
                        assert!(p.0 >= rect.left && p.0 <= rect.right && p.1 >= rect.top && p.1 <= rect.bottom,
                            "{glyph:?} at {size}: {p:?}");
                    }
                }
            }
        }
        assert_eq!(vector_strokes(Glyph::Menu, RectF::new(0.0, 0.0, 14.0, 14.0)).len(), 3);
        assert_eq!(vector_strokes(Glyph::Close, RectF::new(0.0, 0.0, 14.0, 14.0)).len(), 2);
        assert_eq!(vector_strokes(Glyph::Collapse, RectF::new(0.0, 0.0, 14.0, 14.0)).len(), 2);
        assert_eq!(vector_strokes(Glyph::Up, RectF::new(0.0, 0.0, 14.0, 14.0)).len(), 3);
        assert!(vector_stroke_width(14.0) >= 1.0 && vector_stroke_width(35.0) > vector_stroke_width(14.0));
    }

    #[test]
    fn collapse_and_expand_are_mirror_images() {
        let rect = RectF::new(0.0, 0.0, 14.0, 14.0);
        let up = vector_strokes(Glyph::Collapse, rect);
        let down = vector_strokes(Glyph::Expand, rect);
        for (a, b) in up.iter().zip(&down) {
            assert_eq!((a.from.0, 14.0 - a.from.1), b.from);
        }
    }

    #[test]
    fn the_installed_font_choice_resolves_for_every_glyph() {
        // Whatever this machine has, the answer is stable and a real choice.
        for glyph in Glyph::ALL {
            assert_eq!(font_for(glyph), font_for(glyph));
        }
    }
}
