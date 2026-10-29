//! Size tooltip shown while a Corral is resized or drawn (grid-snapping-spec 7). One shared tracking
//! tooltip: the text is the outer size (`640 × 360`, plus the cell count with whole-cell sizing), the
//! position is the pointer plus (16, 20) DIP, flipped to the other side of the pointer when it would
//! leave the work area of the monitor under the pointer, and the window is rebuilt when that monitor's
//! DPI differs from the one it was created at. Corral owns the tooltip by its message window and removes
//! the tool on every hide so the next show starts clean.

use crate::geometry::Rect;
use crate::icon_metrics::dip_to_px;
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO, MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GetCursorPos, IsWindow, SendMessageW, WINDOW_STYLE, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::{PWSTR, w};

const TTS_ALWAYSTIP: u32 = 0x01;
const TTS_NOPREFIX: u32 = 0x02;
const TTF_TRACK: u32 = 0x20;
const TTF_ABSOLUTE: u32 = 0x80;
const TTM_TRACKACTIVATE: u32 = 0x411;
const TTM_TRACKPOSITION: u32 = 0x412;
const TTM_GETBUBBLESIZE: u32 = 0x41e;
const TTM_ADDTOOLW: u32 = 0x432;
const TTM_DELTOOLW: u32 = 0x433;
const TTM_UPDATETIPTEXTW: u32 = 0x439;
const TTM_UPDATE: u32 = 0x41d;
const TOOL_ID: usize = 100;
/// Offset of the tooltip's top-left from the pointer (grid-snapping-spec 7).
const OFFSET_DIP: (f64, f64) = (16.0, 20.0);
/// Gap kept between the pointer and a tooltip flipped above it.
const ABOVE_GAP_DIP: f64 = 8.0;

/// `TOOLINFOW` (0x48 bytes on x64).
#[repr(C)]
struct ToolInfo {
    size: u32,
    flags: u32,
    hwnd: HWND,
    id: usize,
    rect: RECT,
    instance: isize,
    text: PWSTR,
    lparam: LPARAM,
    reserved: *mut core::ffi::c_void,
}

#[derive(Default)]
struct Tip {
    hwnd: Option<HWND>,
    owner: HWND,
    dpi: u32,
    shown: Option<(String, POINT)>,
}

thread_local! {
    static TIP: RefCell<Tip> = RefCell::new(Tip::default());
}

/// Top-left of a tooltip of `size` px for a pointer at `pointer` (grid-snapping-spec 7): pointer +
/// (16, 20) DIP; flipped to the left of or above the pointer when it would leave `work`, and never
/// partly outside it.
pub fn place_tip(pointer: (i32, i32), size: (i32, i32), work: Rect, dpi: u32) -> (i32, i32) {
    let (dx, dy) = (dip_to_px(OFFSET_DIP.0, dpi), dip_to_px(OFFSET_DIP.1, dpi));
    let mut x = pointer.0.saturating_add(dx);
    let mut y = pointer.1.saturating_add(dy);
    if x.saturating_add(size.0) > work.right {
        x = pointer.0 - dx - size.0;
    }
    if y.saturating_add(size.1) > work.bottom {
        y = pointer.1 - dip_to_px(ABOVE_GAP_DIP, dpi) - size.1;
    }
    (x.min(work.right - size.0).max(work.left), y.min(work.bottom - size.1).max(work.top))
}

fn tool(owner: HWND, text: &mut [u16], at: (i32, i32), flags: u32) -> ToolInfo {
    ToolInfo {
        size: std::mem::size_of::<ToolInfo>() as u32,
        flags,
        hwnd: owner,
        id: TOOL_ID,
        rect: RECT { left: at.0, top: at.1, right: at.0 + 100, bottom: at.1 + 100 },
        instance: 0,
        text: PWSTR(text.as_mut_ptr()),
        lparam: LPARAM(0),
        reserved: std::ptr::null_mut(),
    }
}

/// Work area and DPI of the monitor under `pointer`.
fn monitor_at(pointer: POINT) -> (Rect, u32) {
    unsafe {
        let monitor = MonitorFromPoint(pointer, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let work = if GetMonitorInfoW(monitor, &mut info).as_bool() {
            Rect::new(info.rcWork.left, info.rcWork.top, info.rcWork.right, info.rcWork.bottom)
        } else {
            Rect::new(i32::MIN / 2, i32::MIN / 2, i32::MAX / 2, i32::MAX / 2)
        };
        let (mut dx, mut dy) = (96u32, 96u32);
        let dpi = if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_ok() && dx > 0 { dx } else { 96 };
        (work, dpi)
    }
}

/// Show `text` next to the pointer; an empty text hides the tip.
pub fn show(owner: HWND, text: &str) {
    if text.is_empty() {
        hide();
        return;
    }
    let mut cursor = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cursor);
    }
    let (work, dpi) = monitor_at(cursor);
    TIP.with(|cell| {
        let mut tip = cell.borrow_mut();
        let alive = tip.hwnd.is_some_and(|h| unsafe { IsWindow(h) }.as_bool());
        if !alive || tip.owner != owner || tip.dpi != dpi {
            if let Some(old) = tip.hwnd.take().filter(|_| alive) {
                unsafe {
                    let _ = DestroyWindow(old);
                }
            }
            tip.shown = None;
            let style = WINDOW_STYLE(WS_POPUP.0 | TTS_NOPREFIX | TTS_ALWAYSTIP);
            // Created at the pointer so the window starts on (and scales to) the monitor under it.
            tip.hwnd = unsafe {
                CreateWindowExW(WS_EX_TOPMOST, w!("tooltips_class32"), None, style, cursor.x, cursor.y, 0, 0, owner, None, None, None)
            }
            .ok();
            tip.owner = owner;
            tip.dpi = dpi;
        }
        let Some(hwnd) = tip.hwnd else { return };
        let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut info = tool(owner, &mut wide, (cursor.x, cursor.y), TTF_TRACK | TTF_ABSOLUTE);
        let info_ptr = LPARAM(&mut info as *mut ToolInfo as isize);
        unsafe {
            match &tip.shown {
                None => {
                    SendMessageW(hwnd, TTM_ADDTOOLW, WPARAM(0), info_ptr);
                    SendMessageW(hwnd, TTM_TRACKACTIVATE, WPARAM(1), info_ptr);
                    SendMessageW(hwnd, TTM_UPDATETIPTEXTW, WPARAM(0), info_ptr);
                }
                Some((old, _)) if old != text => {
                    SendMessageW(hwnd, TTM_UPDATETIPTEXTW, WPARAM(0), info_ptr);
                    // a tracking tip does not always re-measure itself for a longer text; force it
                    SendMessageW(hwnd, TTM_UPDATE, WPARAM(0), LPARAM(0));
                }
                Some(_) => {}
            }
            let bubble = SendMessageW(hwnd, TTM_GETBUBBLESIZE, WPARAM(0), info_ptr).0 as u32;
            let size = ((bubble & 0xffff) as i32, ((bubble >> 16) & 0xffff) as i32);
            let (x, y) = place_tip((cursor.x, cursor.y), size, work, dpi);
            let at = POINT { x, y };
            if tip.shown.as_ref().is_some_and(|(t, p)| t == text && p.x == at.x && p.y == at.y) {
                return;
            }
            SendMessageW(hwnd, TTM_TRACKPOSITION, WPARAM(0), LPARAM(((x & 0xffff) | (y << 16)) as isize));
            tip.shown = Some((text.to_string(), at));
        }
    });
}

/// Hide the tooltip (button-up, cancelled drag); a no-op when it is not shown.
pub fn hide() {
    TIP.with(|cell| {
        let mut tip = cell.borrow_mut();
        let (Some(hwnd), Some(_)) = (tip.hwnd, tip.shown.take()) else { return };
        let mut empty = [0u16];
        let mut info = tool(tip.owner, &mut empty, (0, 0), 0);
        let info_ptr = LPARAM(&mut info as *mut ToolInfo as isize);
        unsafe {
            SendMessageW(hwnd, TTM_TRACKACTIVATE, WPARAM(0), info_ptr);
            SendMessageW(hwnd, TTM_DELTOOLW, WPARAM(0), info_ptr);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const DPIS: [u32; 5] = [96, 120, 144, 192, 240];

    #[test]
    fn tooltip_info_has_the_win32_size() {
        assert_eq!(std::mem::size_of::<ToolInfo>(), 0x48);
    }

    #[test]
    fn tip_sits_right_of_and_below_the_pointer_at_every_dpi() {
        for dpi in DPIS {
            let work = Rect::new(0, 0, 3840, 2160);
            let (dx, dy) = (dip_to_px(16.0, dpi), dip_to_px(20.0, dpi));
            assert_eq!(place_tip((500, 300), (200, 40), work, dpi), (500 + dx, 300 + dy), "dpi {dpi}");
        }
    }

    #[test]
    fn tip_flips_at_the_right_and_bottom_work_edge_and_stays_on_its_monitor() {
        for dpi in DPIS {
            let work = Rect::new(1920, 0, 3840, 1080);
            let (w, h) = (180, 36);
            let (x, y) = place_tip((3830, 500), (w, h), work, dpi);
            assert!(x + w <= 3830, "left of the pointer at dpi {dpi}: {x}");
            assert!(y > 500, "still below");
            let (x, y) = place_tip((2500, 1075), (w, h), work, dpi);
            assert!(y + h <= 1075, "above the pointer: {y}");
            assert!(x > 2500);
            // both at once, and a pointer at the very corner
            for p in [(3839, 1079), (1920, 0), (3839, 0), (1920, 1079)] {
                let (x, y) = place_tip(p, (w, h), work, dpi);
                assert!(x >= work.left && x + w <= work.right && y >= work.top && y + h <= work.bottom, "{p:?} -> {x},{y}");
            }
        }
        // a tooltip bigger than the work area is pinned to its origin instead of panicking
        assert_eq!(place_tip((10, 10), (5000, 5000), Rect::new(0, 0, 100, 100), 96), (0, 0));
    }
}
