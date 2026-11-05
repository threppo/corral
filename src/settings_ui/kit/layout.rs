//! Shared layout and scroll maths. World coordinates are DIPs; nav has no scroll input.
use crate::render::RectF;
pub fn snap(v: f32, dpi: u32) -> f32 {
    (v * dpi.max(1) as f32 / 96.).round()
}
pub fn snap_rect(r: RectF, dpi: u32) -> RectF {
    RectF::new(
        snap(r.left, dpi),
        snap(r.top, dpi),
        snap(r.right, dpi),
        snap(r.bottom, dpi),
    )
}
pub fn contains(r: RectF, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}
pub fn inset(r: RectF, d: f32) -> RectF {
    RectF::new(r.left + d, r.top + d, r.right - d, r.bottom - d)
}
/// Expander headers are the anchor. Reveal only the first opened row when it fits without hiding
/// the header; a viewport too short to contain both preserves the header (no jump).
pub fn expand_scroll(
    scroll: f32,
    header: RectF,
    first_row: RectF,
    viewport: f32,
    max_scroll: f32,
) -> f32 {
    let needed = (first_row.bottom - viewport + 16.).max(scroll);
    needed.min(header.top).max(scroll).min(max_scroll.max(0.))
}
pub fn reveal_focus(
    scroll: f32,
    element: RectF,
    viewport: f32,
    max_scroll: f32,
    master: bool,
) -> f32 {
    if master {
        return 0.;
    }
    // 16 DIP margin includes the 4 DIP outer extent of the 2 DIP focus stroke.
    let result = if element.top - 16. < scroll {
        element.top - 16.
    } else if element.bottom + 16. > scroll + viewport {
        element.bottom + 16. - viewport
    } else {
        scroll
    };
    result.clamp(0., max_scroll.max(0.))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_supported_dpis_have_integral_device_rectangles() {
        for dpi in [96, 120, 144, 240] {
            let r = snap_rect(RectF::new(1.3, 8.1, 219.7, 46.6), dpi);
            for x in [r.left, r.top, r.right, r.bottom] {
                assert_eq!(x.fract(), 0.);
            }
        }
    }
    #[test]
    fn expander_minimum_scroll() {
        assert_eq!(
            expand_scroll(
                100.,
                RectF::new(0., 300., 300., 348.),
                RectF::new(0., 348., 300., 404.),
                300.,
                600.
            ),
            120.
        );
        assert_eq!(
            expand_scroll(
                100.,
                RectF::new(0., 100., 300., 148.),
                RectF::new(0., 148., 300., 600.),
                200.,
                600.
            ),
            100.
        );
    }
    #[test]
    fn focus_reveals_both_directions_and_master() {
        assert_eq!(
            reveal_focus(0., RectF::new(0., 600., 20., 632.), 400., 900., false),
            248.
        );
        assert_eq!(
            reveal_focus(300., RectF::new(0., 100., 20., 132.), 400., 900., false),
            84.
        );
        assert_eq!(
            reveal_focus(300., RectF::new(0., 100., 20., 132.), 400., 900., true),
            0.
        );
    }
}
