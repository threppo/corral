//! Recognising a live-wallpaper host (wallpaper-blur-spec 3.3): a visible window under the shell's
//! wallpaper windows that another process owns. Corral does not capture it; the Blur rows only get a
//! caption saying that blur uses the still picture Windows reports.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Pure rule: a child of a wallpaper window is a live-wallpaper host when it is visible and neither
/// the shell nor Corral owns it.
pub fn is_host(window_pid: u32, shell_pid: u32, own_pid: u32, visible: bool) -> bool {
    visible && window_pid != 0 && window_pid != shell_pid && window_pid != own_pid
}

fn probe() -> bool {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, EnumWindows, FindWindowW, GetClassNameW, GetWindowThreadProcessId, IsWindowVisible,
    };
    use windows::core::w;

    struct Scan {
        shell: u32,
        own: u32,
        found: bool,
    }
    unsafe extern "system" fn child(hwnd: HWND, data: LPARAM) -> BOOL {
        let scan = unsafe { &mut *(data.0 as *mut Scan) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if is_host(pid, scan.shell, scan.own, unsafe { IsWindowVisible(hwnd) }.as_bool()) {
            scan.found = true;
            return BOOL(0);
        }
        BOOL(1)
    }
    unsafe extern "system" fn top(hwnd: HWND, data: LPARAM) -> BOOL {
        let mut class = [0u16; 32];
        let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
        if String::from_utf16_lossy(&class[..len.min(32)]) == "WorkerW" {
            unsafe {
                let _ = EnumChildWindows(hwnd, Some(child), data);
            }
            if unsafe { &*(data.0 as *const Scan) }.found {
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    unsafe {
        let Ok(progman) = FindWindowW(w!("Progman"), windows::core::PCWSTR::null()) else { return false };
        let mut shell = 0u32;
        GetWindowThreadProcessId(progman, Some(&mut shell));
        let mut scan = Scan { shell, own: GetCurrentProcessId(), found: false };
        let data = LPARAM(&mut scan as *mut Scan as isize);
        let _ = EnumChildWindows(progman, Some(child), data);
        if !scan.found {
            let _ = EnumWindows(Some(top), data);
        }
        scan.found
    }
}

/// Whether a live-wallpaper host is present; the answer is cached for a few seconds.
pub fn live_wallpaper_host() -> bool {
    // Unit tests never look at the real desktop.
    if cfg!(test) {
        return false;
    }
    static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if let Some((at, value)) = *cache {
        if at.elapsed() < Duration::from_secs(5) {
            return value;
        }
    }
    let value = probe();
    *cache = Some((Instant::now(), value));
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_rule() {
        assert!(is_host(500, 100, 200, true));
        assert!(!is_host(100, 100, 200, true), "the shell's own windows");
        assert!(!is_host(200, 100, 200, true), "Corral's windows");
        assert!(!is_host(500, 100, 200, false), "hidden");
        assert!(!is_host(0, 100, 200, true));
    }
}
