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
#[derive(Clone, Copy, Debug)]
pub struct WindowLayout {
    pub nav: RectF,
    pub content: RectF,
    pub viewport: RectF,
    pub stacked: bool,
}
impl WindowLayout {
    pub fn new(width: f32, height: f32) -> Self {
        let right = (width - 40.).max(280.).min(256. + 24. + 760.);
        let content = RectF::new(280., 8., right, (height - 40.).max(8.));
        Self {
            nav: RectF::new(16., 8., 240., height),
            content,
            viewport: RectF::new(256., 0., width, height),
            stacked: right - 280. < 520.,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct RowLayout {
    pub bounds: RectF,
    pub text: RectF,
    pub control: RectF,
    pub stacked: bool,
}
/// Text height must be measured with wrapping at the returned text width by the caller.
pub fn setting_row(
    left: f32,
    top: f32,
    width: f32,
    text_height: f32,
    control_width: f32,
    control_height: f32,
) -> RowLayout {
    let stacked = width < 520.;
    let inner = (width - 32.).max(0.);
    let control_width = control_width.min(inner);
    let height = if stacked {
        (20. + text_height + 12. + control_height).max(56.)
    } else {
        (20. + text_height.max(control_height)).max(56.)
    };
    let text_right = if stacked {
        left + width - 16.
    } else {
        left + width - 16. - control_width - 16.
    };
    let control_top = if stacked {
        top + 10. + text_height + 12.
    } else {
        top + (height - control_height) / 2.
    };
    let control_left = if stacked {
        left + 16.
    } else {
        left + width - 16. - control_width
    };
    RowLayout {
        bounds: RectF::new(left, top, left + width, top + height),
        text: RectF::new(left + 16., top + 10., text_right, top + 10. + text_height),
        control: RectF::new(
            control_left,
            control_top,
            control_left + control_width,
            control_top + control_height,
        ),
        stacked,
    }
}
/// Preserve an unchanged row's screen position after a preceding gate changes its world position.
pub fn anchor_scroll(old_scroll: f32, old_anchor: f32, new_anchor: f32, max_scroll: f32) -> f32 {
    (old_scroll + new_anchor - old_anchor).clamp(0., max_scroll.max(0.))
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
/// Client dimensions in device pixels; caller supplies frame size separately if required.
pub fn opening_size(work_width: u32, work_height: u32, dpi: u32) -> (u32, u32) {
    let desired = (snap(1000., dpi) as u32, snap(720., dpi) as u32);
    if desired.0 <= work_width && desired.1 <= work_height {
        desired
    } else {
        (
            (work_width as f32 * 0.9).floor() as u32,
            (work_height as f32 * 0.9).floor() as u32,
        )
    }
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
    fn maximized_content_is_capped_and_left_aligned() {
        let a = WindowLayout::new(1000., 720.);
        let b = WindowLayout::new(1536., 864.);
        assert_eq!(b.content.left, a.content.left);
        assert_eq!(b.content.right - b.content.left, 760.);
    }
    #[test]
    fn nav_is_independent_of_content_scroll() {
        let layout = WindowLayout::new(760., 520.);
        for _scroll in [0., 200., 900.] {
            assert!(contains(layout.nav, 24., 24.));
            assert!(!contains(layout.viewport, 24., 24.));
        }
    }
    #[test]
    fn minimum_window_rows_stack_inside_content() {
        for dpi in [96, 120, 144, 240] {
            let l = WindowLayout::new(760., 520.);
            let r = setting_row(
                l.content.left,
                100.,
                l.content.right - l.content.left,
                48.,
                300.,
                32.,
            );
            assert!(r.stacked);
            let px = snap_rect(r.control, dpi);
            let bounds = snap_rect(l.content, dpi);
            assert!(px.left >= bounds.left && px.right <= bounds.right);
            assert!(r.control.top >= r.text.bottom);
        }
    }
    #[test]
    fn gate_anchor_and_expander_minimum_scroll() {
        assert_eq!(anchor_scroll(80., 120., 170., 500.), 130.);
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
    #[test]
    fn opening_size_never_exceeds_small_work_area() {
        for dpi in [96, 120, 144, 240] {
            let (w, h) = opening_size(1366, 768, dpi);
            assert!(w <= 1366 && h <= 768);
            let (w, h) = opening_size(900, 600, dpi);
            assert_eq!((w, h), (810, 540));
        }
    }
}
