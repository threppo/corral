//! Corral's application icon on its own windows. `build.rs` embeds `assets/icon/corral.ico` as
//! RT_GROUP_ICON #1 (16/20/24/32 hand-drawn, 40/48/64/256 from the master); `LoadImageW` picks the
//! entry that fits the requested size. [Corral policy, own artwork]
//!
//! Which windows carry it (the ones with a taskbar button or a title-bar icon):
//! * the settings window (`CorralSettings`): class icons plus per-DPI window icons ([`set_for_dpi`]),
//!   so the title bar shows a crisp icon on every monitor;
//! * the colour editor and the sort prompt (`WS_EX_APPWINDOW`, so they get a taskbar button): class
//!   icons only, their `WS_EX_DLGMODALFRAME` frame draws no title-bar icon and the taskbar and Alt-Tab
//!   scale the large icon themselves;
//! * the preview window (`CorralPreview`, QA only): window icons at its DPI when created.
//!
//! The backup and rules dialogs are owned `WS_EX_DLGMODALFRAME` popups without a taskbar button or
//! title-bar icon, the screen picker and the engine's other windows are tool or hidden windows: none
//! of them gets an icon.

use std::sync::OnceLock;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetSystemMetrics, HICON, ICON_BIG, ICON_SMALL, IMAGE_ICON, LR_DEFAULTCOLOR, LoadImageW, SM_CXICON,
    SM_CXSMICON, SYSTEM_METRICS_INDEX, SendMessageW, WM_SETICON,
};
use windows::core::PCWSTR;

/// RT_GROUP_ICON ID written by `build.rs`.
const GROUP_ID: usize = 1;

/// The app icon at `size` x `size` pixels (the caller owns it), or `None` when this binary has no icon
/// resource (unit-test binaries are linked without it).
pub fn load(size: i32) -> Option<HICON> {
    // SAFETY: plain resource lookup in this module; an integer resource ID is a valid PCWSTR.
    unsafe {
        let module = GetModuleHandleW(None).ok()?;
        LoadImageW(HINSTANCE(module.0), PCWSTR(GROUP_ID as *const u16), IMAGE_ICON, size, size, LR_DEFAULTCOLOR)
            .ok()
            .map(|handle| HICON(handle.0))
    }
}

/// `(hIcon, hIconSm)` for a window class, at the system DPI (`SM_CXICON`, `SM_CXSMICON`). Loaded once
/// per process and shared by every class (callers register their class on each open), never destroyed.
/// Null handles when the resource is missing.
pub fn class_icons() -> (HICON, HICON) {
    static ICONS: OnceLock<(isize, isize)> = OnceLock::new();
    let (big, small) = *ICONS.get_or_init(|| {
        let handle = |metric| load(unsafe { GetSystemMetrics(metric) }).map_or(0, |icon| icon.0 as isize);
        (handle(SM_CXICON), handle(SM_CXSMICON))
    });
    (HICON(big as *mut core::ffi::c_void), HICON(small as *mut core::ffi::c_void))
}

/// Window icon sizes at `dpi`: (small for the title bar, big for the taskbar and Alt-Tab).
fn sizes_for_dpi(dpi: u32, metric: impl Fn(SYSTEM_METRICS_INDEX, u32) -> i32) -> [(u32, i32); 2] {
    [(ICON_SMALL, metric(SM_CXSMICON, dpi)), (ICON_BIG, metric(SM_CXICON, dpi))]
}

/// Give `hwnd` its own icons sized for `dpi`. Call after creation (with `GetDpiForWindow`) and on
/// `WM_DPICHANGED` (with the new DPI): the class icons are sized for the system DPI and would be scaled
/// on a monitor with another one. The icons the window had before are destroyed; the last ones stay
/// with the window (the settings window lives as long as its process).
pub fn set_for_dpi(hwnd: HWND, dpi: u32) {
    let metric = |index, dpi| unsafe { GetSystemMetricsForDpi(index, dpi) };
    for (kind, size) in sizes_for_dpi(dpi.max(96), metric) {
        let Some(icon) = load(size) else { return };
        // SAFETY: WM_SETICON returns the window's previous icon, which only this function set.
        unsafe {
            let old = SendMessageW(hwnd, WM_SETICON, WPARAM(kind as usize), LPARAM(icon.0 as isize));
            if old.0 != 0 {
                let _ = DestroyIcon(HICON(old.0 as *mut core::ffi::c_void));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_icons_follow_the_dpi_metrics() {
        // the documented 100 % sizes, scaled like GetSystemMetricsForDpi
        let metric = |index: SYSTEM_METRICS_INDEX, dpi: u32| (if index == SM_CXSMICON { 16 } else { 32 }) * dpi as i32 / 96;
        assert_eq!(sizes_for_dpi(96, metric), [(ICON_SMALL, 16), (ICON_BIG, 32)]);
        assert_eq!(sizes_for_dpi(120, metric), [(ICON_SMALL, 20), (ICON_BIG, 40)]);
        assert_eq!(sizes_for_dpi(144, metric), [(ICON_SMALL, 24), (ICON_BIG, 48)]);
    }
}
