//! Desktop-mode z-order guard: activating a portal (or a group) must never lift Corral's own windows.
//!
//! Observed live symptom: "the whole Monitor B screen flashes on every action in a Monitor A portal".
//! The portal popup is owned by its group window and the group by the surface, which spans the whole
//! virtual desktop. Activating the popup (a click in it, or in the group) makes Windows raise the
//! activated window together with its owner chain, so for a frame the surface (loose icons, click
//! catcher) sits above the applications on every monitor until the posted `WM_RESTACK` puts the
//! stack back. This is the same mechanism the Peek spike measured (E2); Peek's own guard
//! (`peek::guard_surface_zorder`) stays as it is and takes precedence while Peek is on.
//!
//! The guard runs in `window_proc` on `WM_WINDOWPOSCHANGING`, before the `APP` borrow, and removes the
//! z-order part of any change to the surface, a group window or a portal popup that does not come
//! from Corral's own restack/peek code (`App::set_restacking` mirrors `restacking` into [`OWN`]).
//! A suppressed change is logged, rate limited, so a live log can confirm the hypothesis.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, SET_WINDOW_POS_FLAGS, WINDOWPOS};

/// Desktop mode with live windows (set by `restack`, cleared at shutdown/reattach).
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// Mirror of `App::restacking`: Corral itself is changing the z-order right now.
static OWN: AtomicBool = AtomicBool::new(false);
static PENDING: AtomicU32 = AtomicU32::new(0);
static LAST_LOG_MS: AtomicU64 = AtomicU64::new(0);

const SWP_NOZORDER: u32 = 0x0004;
/// At most one log line per interval; the count of suppressed changes is carried over.
const LOG_INTERVAL_MS: u64 = 2000;

pub(super) fn set_active(on: bool) {
    ACTIVE.store(on, Ordering::Relaxed);
}

pub(super) fn set_own_restack(on: bool) {
    OWN.store(on, Ordering::Relaxed);
}

/// The window classes whose z-order only Corral may change.
pub(super) fn is_guarded_class(name: &str) -> bool {
    matches!(name, "CorralSurface" | "CorralGroup")
}

/// Pure decision: the new flags for a `WM_WINDOWPOSCHANGING`, or `None` to leave it alone. A change
/// is foreign when Corral is not restacking and Peek (which has its own guard) is off.
pub(super) fn foreign_zorder(active: bool, peek_on: bool, own: bool, guarded: bool, flags: u32) -> Option<u32> {
    (active && !peek_on && !own && guarded && flags & SWP_NOZORDER == 0).then_some(flags | SWP_NOZORDER)
}

/// Pure decision: log now? (first suppression, then at most once per interval)
pub(super) fn log_due(now_ms: u64, last_ms: u64, interval_ms: u64) -> bool {
    last_ms == 0 || now_ms.saturating_sub(last_ms) >= interval_ms
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

/// Called from `window_proc` for every `WM_WINDOWPOSCHANGING`, before the `APP` borrow.
pub(super) fn guard(hwnd: HWND, lparam: LPARAM) {
    if !ACTIVE.load(Ordering::Relaxed) || lparam.0 == 0 {
        return;
    }
    let pos = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
    if pos.flags.0 & SWP_NOZORDER != 0 || OWN.load(Ordering::Relaxed) || super::peek::is_active() {
        return; // no z-order part, or one of ours, or Peek's own guard rules
    }
    let class = class_of(hwnd);
    let Some(flags) = foreign_zorder(true, false, false, is_guarded_class(&class), pos.flags.0) else { return };
    let after = pos.hwndInsertAfter;
    pos.flags = SET_WINDOW_POS_FLAGS(flags);
    let pending = PENDING.fetch_add(1, Ordering::Relaxed) + 1;
    let now = super::tick_ms().max(1);
    if log_due(now, LAST_LOG_MS.load(Ordering::Relaxed), LOG_INTERVAL_MS) {
        LAST_LOG_MS.store(now, Ordering::Relaxed);
        PENDING.store(0, Ordering::Relaxed);
        super::log(&format!(
            "z-guard: suppressed {pending} foreign z-order change(s) of Corral windows (last: {class} {:?} insert_after={:?})",
            hwnd.0, after.0
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_corral_layer_classes_are_guarded() {
        for c in ["CorralSurface", "CorralGroup"] {
            assert!(is_guarded_class(c));
        }
        for c in ["CorralScrollbar", "CorralMessage", "CorralSettings", "Progman", "Notepad", ""] {
            assert!(!is_guarded_class(c));
        }
    }

    #[test]
    fn foreign_changes_lose_their_z_order_and_nothing_else() {
        // activation raising the owner chain: z part present, not ours, Peek off
        assert_eq!(foreign_zorder(true, false, false, true, 0x0010 | 0x0002 | 0x0001), Some(0x0010 | 0x0002 | 0x0001 | 0x0004));
        // already without a z-order part: untouched (nothing to suppress)
        assert_eq!(foreign_zorder(true, false, false, true, 0x0004 | 0x0010), None);
    }

    #[test]
    fn corral_s_own_restack_peek_and_inactive_guard_pass() {
        assert_eq!(foreign_zorder(true, false, true, true, 0), None, "restack must still work");
        assert_eq!(foreign_zorder(true, true, false, true, 0), None, "Peek keeps its own guard");
        assert_eq!(foreign_zorder(false, false, false, true, 0), None, "preview mode / before init");
        assert_eq!(foreign_zorder(true, false, false, false, 0), None, "foreign windows are not ours");
    }

    #[test]
    fn log_line_is_rate_limited() {
        assert!(log_due(500, 0, 2000), "first suppression logs at once");
        assert!(!log_due(1500, 500, 2000));
        assert!(!log_due(2499, 500, 2000));
        assert!(log_due(2500, 500, 2000));
        assert!(!log_due(10, 5_000, 2000), "a clock that went backwards never spams");
    }
}
