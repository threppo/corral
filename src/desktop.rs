//! Explorer desktop host discovery, monitor enumeration, and safe list-view hiding.
//!
//! Supported layouts:
//! - Windows 11 24H2+: `Progman` -> `SHELLDLL_DefView` -> `SysListView32`, wallpaper in a child
//!   `WorkerW` of `Progman` (observed on 10.0.26200).
//! - Earlier: `SHELLDLL_DefView` under `Progman` or under a top-level `WorkerW` sibling.
//!
//! Hiding uses a non-persistent `ShowWindow(SW_HIDE)` and is undone only when the list view was
//! initially visible (so a user's own "Show desktop icons = off" is preserved).

use crate::geometry::{Monitor, Rect};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFOEXW};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, GetClassNameW, GetWindowThreadProcessId, IsWindow, IsWindowVisible,
    ShowWindow, SW_HIDE, SW_SHOWNA,
};
use windows::core::{PCWSTR, w};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DesktopHost {
    pub progman: HWND,
    /// Window that owns `SHELLDLL_DefView` (Progman or a legacy top-level WorkerW).
    pub view_parent: HWND,
    pub defview: HWND,
    pub listview: HWND,
    /// Wallpaper WorkerW (child of Progman on 24H2+, top-level sibling earlier), if present.
    pub wallpaper: Option<HWND>,
    pub explorer_pid: u32,
    pub listview_was_visible: bool,
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buffer) };
    String::from_utf16_lossy(&buffer[..len.max(0) as usize])
}

pub fn window_pid(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

fn child(parent: HWND, class: PCWSTR) -> Option<HWND> {
    unsafe { FindWindowExW(parent, HWND::default(), class, PCWSTR::null()) }.ok().filter(|h| !h.is_invalid())
}

fn top_level_workerws() -> Vec<HWND> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
        if class_name(hwnd) == "WorkerW" {
            list.push(hwnd);
        }
        TRUE
    }
    let mut list: Vec<HWND> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect), LPARAM(&mut list as *mut _ as isize));
    }
    list
}

/// Locate the desktop host. Returns `None` while Explorer is not running or still starting.
pub fn find_host() -> Option<DesktopHost> {
    let progman = unsafe { FindWindowW(w!("Progman"), PCWSTR::null()) }.ok().filter(|h| !h.is_invalid())?;
    let explorer_pid = window_pid(progman);
    let (view_parent, defview) = match child(progman, w!("SHELLDLL_DefView")) {
        Some(defview) => (progman, defview),
        None => top_level_workerws()
            .into_iter()
            .filter(|w| window_pid(*w) == explorer_pid)
            .find_map(|w| child(w, w!("SHELLDLL_DefView")).map(|d| (w, d)))?,
    };
    let listview = child(defview, w!("SysListView32"))?;
    let wallpaper = child(progman, w!("WorkerW")).or_else(|| {
        top_level_workerws()
            .into_iter()
            .find(|w| *w != view_parent && window_pid(*w) == explorer_pid && child(*w, w!("SHELLDLL_DefView")).is_none())
    });
    let listview_was_visible = unsafe { IsWindowVisible(listview) }.as_bool();
    Some(DesktopHost { progman, view_parent, defview, listview, wallpaper, explorer_pid, listview_was_visible })
}

/// True if `hwnd` is still the Explorer desktop list view recorded at attach time.
pub fn is_same_listview(hwnd: HWND, explorer_pid: u32) -> bool {
    unsafe { IsWindow(hwnd) }.as_bool() && class_name(hwnd) == "SysListView32" && window_pid(hwnd) == explorer_pid
}

pub fn hide_listview(host: &DesktopHost) {
    if is_same_listview(host.listview, host.explorer_pid) {
        unsafe {
            let _ = ShowWindow(host.listview, SW_HIDE);
        }
    }
}

/// Restore the list view to its initial visibility. Safe to call repeatedly.
pub fn restore_listview(listview: HWND, explorer_pid: u32, was_visible: bool) -> bool {
    if !was_visible || !is_same_listview(listview, explorer_pid) {
        return false;
    }
    unsafe {
        if !IsWindowVisible(listview).as_bool() {
            let _ = ShowWindow(listview, SW_SHOWNA);
        }
    }
    drop_stray_scrollbars(listview);
    true
}

const LVS_NOSCROLL: u32 = 0x2000;
const WS_HSCROLL_BITS: u32 = 0x0010_0000;
const WS_VSCROLL_BITS: u32 = 0x0020_0000;

/// Scrollbars the desktop list view shows although it is a no-scroll list: after a session with
/// two displays (Monitor B 2K + Monitor A 4K, 2026-10-14) the re-shown list kept a vertical bar at the
/// right edge until a reboot; refresh, "Show desktop icons" and an Explorer restart did not
/// clear it. Returns the `ShowScrollBar` bar to hide (SB_HORZ 0, SB_VERT 1, SB_BOTH 3).
fn stray_scrollbars(style: u32) -> Option<i32> {
    if style & LVS_NOSCROLL == 0 {
        return None;
    }
    match (style & WS_HSCROLL_BITS != 0, style & WS_VSCROLL_BITS != 0) {
        (false, false) => None,
        (true, false) => Some(0),
        (false, true) => Some(1),
        (true, true) => Some(3),
    }
}

/// Hide such bars (non-persistent, like the hiding itself). The cause is not known; the user's
/// observation is that the bar appeared after each desktop-mode session ended.
fn drop_stray_scrollbars(listview: HWND) {
    use windows::Win32::UI::Controls::ShowScrollBar;
    use windows::Win32::UI::WindowsAndMessaging::{GWL_STYLE, GetWindowLongW, SCROLLBAR_CONSTANTS};
    let style = unsafe { GetWindowLongW(listview, GWL_STYLE) } as u32;
    if let Some(bar) = stray_scrollbars(style) {
        let _ = unsafe { ShowScrollBar(listview, SCROLLBAR_CONSTANTS(bar), false) };
    }
}

pub fn monitors() -> Vec<Monitor> {
    display_snapshot().map(|s| s.displays().iter().map(|d| Monitor {
        name: d.device.clone(), bounds: d.bounds, work: d.work, dpi: d.dpi,
    }).collect()).unwrap_or_default()
}

/// rcMonitor/rcWork and MONITORINFOF_PRIMARY come from the
/// same enumeration. Hardware identity is unavailable in the existing adapter.
pub fn display_snapshot() -> Option<crate::dpi_transition::Snapshot> {
    unsafe extern "system" fn collect(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
        let list = unsafe { &mut *(data.0 as *mut Vec<crate::dpi_transition::DisplaySnapshot>) };
        let mut info = MONITORINFOEXW::default();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if unsafe { GetMonitorInfoW(monitor, &mut info as *mut _ as *mut _) }.as_bool() {
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
            let name_len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
            let r = info.monitorInfo.rcMonitor;
            let wa = info.monitorInfo.rcWork;
            let m = Monitor {
                name: String::from_utf16_lossy(&info.szDevice[..name_len]),
                bounds: Rect::new(r.left, r.top, r.right, r.bottom),
                work: Rect::new(wa.left, wa.top, wa.right, wa.bottom),
                dpi: dpi_x,
            };
            list.push(crate::dpi_transition::DisplaySnapshot::from_monitor(&m,
                info.monitorInfo.dwFlags & 1 != 0, None)); // MONITORINFOF_PRIMARY
        }
        else { return BOOL(0); }
        TRUE
    }
    let mut list: Vec<crate::dpi_transition::DisplaySnapshot> = Vec::new();
    unsafe {
        if !EnumDisplayMonitors(HDC::default(), None, Some(collect), LPARAM(&mut list as *mut _ as isize)).as_bool() {
            return None;
        }
    }
    crate::dpi_transition::Snapshot::new(list)
}

/// Human-readable description of the host for `--probe` and logs.
pub fn describe(host: &DesktopHost) -> String {
    format!(
        "progman={:#x} view_parent={:#x}({}) defview={:#x} listview={:#x} visible={} wallpaper={} explorer_pid={}",
        host.progman.0 as usize,
        host.view_parent.0 as usize,
        class_name(host.view_parent),
        host.defview.0 as usize,
        host.listview.0 as usize,
        host.listview_was_visible,
        host.wallpaper.map(|w| format!("{:#x}", w.0 as usize)).unwrap_or_else(|| "none".into()),
        host.explorer_pid
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_no_scroll_list_loses_its_bars() {
        // the observed desktop list after a session: 0x56203B40 (LVS_NOSCROLL + WS_VSCROLL)
        assert_eq!(stray_scrollbars(0x5620_3B40), Some(1));
        assert_eq!(stray_scrollbars(0x5600_3B40), None, "no bar, nothing to do");
        assert_eq!(stray_scrollbars(0x5630_3B40), Some(3));
        assert_eq!(stray_scrollbars(0x5610_3B40), Some(0));
        assert_eq!(stray_scrollbars(0x5620_1B40), None, "a scrolling list keeps its bars");
    }
}
