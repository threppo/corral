//! Monitor rectangles in physical pixels (virtual-screen coordinates, after rotation) and their DPI.

use windows::Win32::Foundation::{BOOL, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};

const MONITORINFOF_PRIMARY: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Monitor {
    /// `[left, top, right, bottom]` in virtual-screen physical pixels.
    pub rect: [i32; 4],
    pub dpi: u32,
    pub primary: bool,
}

impl Monitor {
    pub fn width(&self) -> i32 {
        self.rect[2] - self.rect[0]
    }
    #[cfg(test)]
    pub fn height(&self) -> i32 {
        self.rect[3] - self.rect[1]
    }
}

/// Bounding rectangle of all monitors; `[0, 0, 0, 0]` for none.
pub fn virtual_rect(monitors: &[Monitor]) -> [i32; 4] {
    monitors.iter().fold(None, |acc: Option<[i32; 4]>, m| {
        Some(match acc {
            None => m.rect,
            Some(a) => [a[0].min(m.rect[0]), a[1].min(m.rect[1]), a[2].max(m.rect[2]), a[3].max(m.rect[3])],
        })
    })
    .unwrap_or([0; 4])
}

/// Index of the monitor containing `(x, y)`, else the nearest one by clamped distance.
pub fn monitor_index_for(rects: &[[i32; 4]], x: i32, y: i32) -> Option<usize> {
    if let Some(i) = rects.iter().position(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3]) {
        return Some(i);
    }
    rects
        .iter()
        .enumerate()
        .min_by_key(|(_, r)| {
            let dx = (r[0] - x).max(0).max(x - (r[2] - 1)) as i64;
            let dy = (r[1] - y).max(0).max(y - (r[3] - 1)) as i64;
            dx * dx + dy * dy
        })
        .map(|(i, _)| i)
}

unsafe extern "system" fn collect(hmon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let list = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
    let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    if unsafe { GetMonitorInfoW(hmon, &mut info) }.as_bool() {
        let (mut dx, mut dy) = (96u32, 96u32);
        if unsafe { GetDpiForMonitor(hmon, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) }.is_err() || dx == 0 {
            dx = 96;
        }
        let r = info.rcMonitor;
        list.push(Monitor { rect: [r.left, r.top, r.right, r.bottom], dpi: dx, primary: info.dwFlags & MONITORINFOF_PRIMARY != 0 });
    }
    BOOL(1)
}

/// All attached monitors, primary first. Physical pixels because the process is per-monitor aware.
pub fn enumerate() -> Vec<Monitor> {
    let mut list: Vec<Monitor> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(HDC::default(), None, Some(collect), LPARAM(&mut list as *mut _ as isize));
    }
    list.sort_by_key(|m| !m.primary);
    list
}

/// The primary monitor's rectangle `(left, top, right, bottom)` in physical pixels.
pub fn primary_rect() -> Option<(i32, i32, i32, i32)> {
    let list = enumerate();
    list.iter().find(|m| m.primary).or(list.first()).map(|m| (m.rect[0], m.rect[1], m.rect[2], m.rect[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(rect: [i32; 4]) -> Monitor {
        Monitor { rect, dpi: 96, primary: false }
    }

    #[test]
    fn virtual_rect_is_the_bounding_box_with_negative_origins() {
        let list = [m([0, 0, 1920, 1080]), m([-1920, -200, 0, 1000]), m([0, -1080, 2560, 0])];
        assert_eq!(virtual_rect(&list), [-1920, -1080, 2560, 1080]);
        assert_eq!(virtual_rect(&[]), [0; 4]);
    }

    #[test]
    fn point_lookup_prefers_containment_then_distance() {
        let rects = [[0, 0, 1920, 1080], [-1920, -200, 0, 1000]];
        assert_eq!(monitor_index_for(&rects, 10, 10), Some(0));
        assert_eq!(monitor_index_for(&rects, -1, 500), Some(1));
        assert_eq!(monitor_index_for(&rects, 0, 1050), Some(0), "boundary belongs to the right/lower monitor");
        assert_eq!(monitor_index_for(&rects, -2500, -300), Some(1), "outside: the nearest one");
        assert_eq!(monitor_index_for(&[], 0, 0), None);
    }

    #[test]
    fn live_enumeration_finds_a_monitor_when_a_desktop_exists() {
        // Read-only query; an empty list (no desktop session) is accepted.
        for mon in enumerate() {
            assert!(mon.width() > 0 && mon.height() > 0 && mon.dpi >= 96);
        }
    }
}
