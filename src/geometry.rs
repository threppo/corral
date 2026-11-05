//! Pure geometry: rectangles, monitors, DPI conversion and rubber-band selection mapping.
//!
//! Rubber-band selection: monitor choice by
//! largest overlap with a start-point tie break and work-area clamping are implemented.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }

    pub fn from_points(x0: i32, y0: i32, x1: i32, y1: i32) -> Self {
        Self::new(x0.min(x1), y0.min(y1), x0.max(x1), y0.max(y1))
    }

    pub const fn width(&self) -> i32 {
        self.right - self.left
    }

    pub const fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub const fn contains(&self, x: i32, y: i32) -> bool {
        self.left <= x && x < self.right && self.top <= y && y < self.bottom
    }

    pub fn overlap_area(&self, other: &Rect) -> i64 {
        let width = self.right.min(other.right) - self.left.max(other.left);
        let height = self.bottom.min(other.bottom) - self.top.max(other.top);
        if width > 0 && height > 0 { width as i64 * height as i64 } else { 0 }
    }

    /// Clamp every edge into `area`; a selection fully outside collapses onto the nearest edge.
    pub fn clamp_into(&self, area: &Rect) -> Rect {
        let left = self.left.clamp(area.left, area.right);
        let top = self.top.clamp(area.top, area.bottom);
        Rect::new(left, top, self.right.clamp(left, area.right), self.bottom.clamp(top, area.bottom))
    }

    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.left + dx, self.top + dy, self.right + dx, self.bottom + dy)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Monitor {
    /// Device name, e.g. `\\.\DISPLAY1`.
    pub name: String,
    /// Physical pixels in virtual-screen coordinates.
    pub bounds: Rect,
    /// Work area (excludes taskbars) in physical pixels.
    pub work: Rect,
    /// Effective DPI (96 = 100 %).
    pub dpi: u32,
}

/// Physical pixels -> DIPs, rounding half away from zero with exact integer arithmetic.
pub fn to_dip(px: i32, dpi: u32) -> i32 {
    let numerator = (px.unsigned_abs() as u64) * 96;
    let dpi = dpi.max(1) as u64;
    let rounded = ((2 * numerator + dpi) / (2 * dpi)) as i32;
    if px < 0 { -rounded } else { rounded }
}

/// DIPs -> physical pixels, rounding half away from zero.
pub fn to_px(dip: i32, dpi: u32) -> i32 {
    let numerator = (dip.unsigned_abs() as u64) * dpi as u64;
    let rounded = ((2 * numerator + 96) / 192) as i32;
    if dip < 0 { -rounded } else { rounded }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dip_rounding_is_half_away_from_zero() {
        assert_eq!(to_dip(3, 192), 2);
        assert_eq!(to_dip(-3, 192), -2);
        assert_eq!(to_dip(100, 144), 67);
        assert_eq!(to_px(67, 144), 101);
        assert_eq!(to_px(-1, 144), -2);
    }

}
