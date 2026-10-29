//! Basic Peek (UI name "Glance"): the Corrals, Folder Gate popups and scrollbars are lifted above
//! every application window (`HWND_TOPMOST`) until Peek ends. Desktop mode only; the preview cannot host it.
//!
//! The shortcuts, their consumption rules and the hook threads are in `peek_keys` (peek-hotkeys-spec). This
//! file reacts to the events the hook thread posts and owns the presentation and the dismissal policy:
//! * Esc, the hold combo's release and the toggle combo end Peek; so does another application becoming active
//!   (click, Alt+Tab, Win+number) unless `CorralPeekKeepOpen` is on, see
//!   [`activation_ends_peek`]; a fullscreen app, a session change, a settings change and an Explorer restart
//!   end it as well;
//! * while a fullscreen app is in front the hook consumes nothing (the combo belongs to Windows), see
//!   [`fullscreen_refusal`] and [`App::peek_fullscreen_refresh`].
//!
//! Presentation (variant A of the proof of concept, verified live 2026-10-14: E1, E2, E3, E6, E9):
//! * groups stay owned by the surface; while Peek is on, `window_proc` adds `SWP_NOZORDER` to every
//!   `WM_WINDOWPOSCHANGING` of the surface (an activation of an owned group would otherwise pull the
//!   full-screen surface above the applications), see [`guard_surface_zorder`];
//! * `restack()` does not run its desktop ordering meanwhile, it only re-promotes windows that appeared
//!   or lost the flag (a portal popup recreated by navigation, a scrollbar created on demand);
//! * the press-time `SetFocus(surface)` is skipped, rename/naming edit boxes and message boxes are made
//!   topmost, so they are not hidden below the lifted groups.
//!
//! Evidence labels in comments: [spike] = live result of the proof of concept. Remaining notes are this
//! implementation's choice. The Desktop-mode shortcuts (`HotkeysDesktop*`) and the hot corner are not
//! implemented.

use super::{App, OLE_DRAG_RUNNING, log, tick_ms};
use crate::peek_keys::{self, Config, Keys};
use crate::quick_hide::Phase;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, TRUE, WPARAM};
use windows::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetDoubleClickTime, SetFocus, VK_ESCAPE};
use windows::Win32::UI::Shell::SHQueryUserNotificationState;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Events of `peek_keys::Keys` arrive on the message window as this message (`wParam` = event).
pub(super) const WM_PEEK_KEYS: u32 = WM_APP + 0x40;
/// Re-check of the fullscreen state every 2 s while the shortcuts run or Peek is on (peek-hotkeys-spec 7);
/// foreground and display changes re-check at once.
pub(super) const TIMER_PEEK: usize = 0x5201;
const FULLSCREEN_POLL_MS: u32 = 2000;
/// `WM_WTSSESSION_CHANGE` (lock, unlock, console/remote connect and disconnect, logon, logoff).
pub(super) const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
/// Resume from sleep or hibernation (`PBT_APMRESUMESUSPEND`, `PBT_APMRESUMEAUTOMATIC`): the hook is installed again.
const PBT_APMRESUMESUSPEND: usize = 0x0007;
const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;

/// The surface to guard (0 = Peek off). Mirrors `Peek::mode` for `window_proc`, which runs before (and
/// outside) the `APP` borrow, and for free functions that cannot reach `App`.
static GUARD_SURFACE: AtomicIsize = AtomicIsize::new(0);
/// Z-order changes of the surface suppressed during the current Peek (logged at its end).
static SUPPRESSED: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------- pure decisions

/// How Peek was turned on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Toggle,
    Hold,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Event {
    Toggle,
    HoldStart,
    HoldEnd,
    Dismiss,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Ignore,
    Enter(Mode),
    Exit,
}

pub(super) fn event_from_wparam(wparam: usize) -> Option<Event> {
    match peek_keys::event_of(wparam) {
        peek_keys::EVENT_TOGGLE => Some(Event::Toggle),
        peek_keys::EVENT_HOLD_START => Some(Event::HoldStart),
        peek_keys::EVENT_HOLD_END => Some(Event::HoldEnd),
        peek_keys::EVENT_DISMISS => Some(Event::Dismiss),
        _ => None,
    }
}

/// State machine (peek-hotkeys-spec 3). A toggle event flips, whichever mode is on. A hold start turns Peek
/// on in Hold mode; when Peek is already open (from a toggle) the hold neither closes nor re-opens it. A hold
/// end ends only a Hold-mode Peek (the opening mode is the "opened by hold" record). Esc ends any Peek and,
/// like the hold end, never looks at `CorralPeekKeepOpen`.
pub(super) fn transition(current: Option<Mode>, event: Event) -> Action {
    match (current, event) {
        (None, Event::Toggle) => Action::Enter(Mode::Toggle),
        (Some(_), Event::Toggle) => Action::Exit,
        (None, Event::HoldStart) => Action::Enter(Mode::Hold),
        (Some(_), Event::HoldStart) => Action::Ignore,
        (Some(Mode::Hold), Event::HoldEnd) => Action::Exit,
        (_, Event::HoldEnd) => Action::Ignore,
        (Some(_), Event::Dismiss) => Action::Exit,
        (None, Event::Dismiss) => Action::Ignore,
    }
}

/// Why Peek ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Cause {
    /// The toggle combo again.
    Toggle,
    /// The hold combo was released.
    HoldEnd,
    /// Esc.
    Esc,
    /// Another application became active (toggle Peek only).
    Activation,
    /// Left press on the empty desktop (the surface).
    DesktopClick,
    /// Right or middle button released on the empty desktop.
    DesktopButton,
    /// An application went fullscreen.
    Fullscreen,
    /// Session lock/unlock/connect/disconnect.
    Session,
    /// The settings turned the shortcuts off.
    Settings,
    /// Explorer restarted: the windows are about to be destroyed and recreated.
    Reattach,
}

impl Cause {
    /// Whether the exit may give the foreground back to the window that was active before Peek [spike rule].
    /// Not after another application's activation (it IS the new foreground), a fullscreen app, a session
    /// change or silent exits. A right/middle click on the empty desktop opens the desktop menu, which takes
    /// the foreground again at once.
    pub(super) fn restores_foreground(self) -> bool {
        matches!(self, Cause::Toggle | Cause::HoldEnd | Cause::Esc | Cause::DesktopClick)
    }

    /// The full exit path (restack, quick-hide display) runs; Reattach only demotes, the new host gets
    /// a fresh stack from `reattach()`.
    fn full_exit(self) -> bool {
        self != Cause::Reattach
    }
}

/// Give the foreground back after the demotion: the old window must exist and not already be
/// foreground, and the foreground must still be Corral's own or none - never steal it from an
/// application the user switched to meanwhile [spike].
pub(super) fn should_restore_foreground(prev_alive: bool, prev_is_foreground: bool, foreground_is_ours_or_none: bool) -> bool {
    prev_alive && !prev_is_foreground && foreground_is_ours_or_none
}

/// First reason Peek must not start. Refusals are logged by the caller. The fullscreen probe runs
/// last (it calls the shell).
pub(super) fn entry_refusal(detached: bool, ole_drag: bool, pages_motion: bool, editing: bool, fullscreen: impl FnOnce() -> Option<String>) -> Option<String> {
    if !detached {
        return Some("not in desktop mode (preview cannot host Glance)".into());
    }
    if ole_drag {
        return Some("an OLE drag is running".into());
    }
    if pages_motion {
        return Some("a Range transition is running".into());
    }
    if editing {
        return Some("a rename or naming edit is open".into());
    }
    fullscreen()
}

/// Reason for the fullscreen refusal from the shell's notification state (QUERY_USER_NOTIFICATION_STATE:
/// 2 BUSY, 3 RUNNING_D3D_FULL_SCREEN, 4 PRESENTATION_MODE, peek-hotkeys-spec 7) and the foreground window.
/// With the desktop, the taskbar or Corral itself in front there is no fullscreen app to protect, and Windows
/// reports QUNS_BUSY for Corral's own full-screen surface (live 2026-10-14: Peek on an empty desktop was
/// refused). Windows 11 taskbar flyouts (button thumbnails, previews) are transparent windows
/// covering the whole monitor and count as shell too (live 2026-10-15).
pub(super) fn fullscreen_refusal(notification_state: Option<i32>, fg_class: &str, fg_own: bool) -> Option<String> {
    let shell = fg_own || matches!(fg_class, "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        | "XamlExplorerHostIslandWindow" | "XamlWindow");
    if shell {
        return None;
    }
    match notification_state {
        Some(2) => Some("notification state QUNS_BUSY (fullscreen app)".into()),
        Some(3) => Some("notification state QUNS_RUNNING_D3D_FULL_SCREEN".into()),
        Some(4) => Some("notification state QUNS_PRESENTATION_MODE".into()),
        _ => None,
    }
}

/// Classes of the desktop shell: the desktop, the taskbar and its Windows 11 flyout hosts. Activating one of
/// them is not "another application" (Corral's own surface and the tray-icon logic decide there).
pub(super) fn is_shell_class(class: &str) -> bool {
    matches!(class, "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd" | "XamlExplorerHostIslandWindow" | "XamlWindow")
}

/// Does the activation of a window end Peek? Only a toggled Peek, and not with
/// `CorralPeekKeepOpen`. The new foreground window must exist, belong to another process, and not be the desktop
/// shell (`root_class` is the class of its top-level window). Hold Peek ends on release instead.
pub(super) fn activation_ends_peek(mode: Option<Mode>, keep_open: bool, hwnd_present: bool, own_process: bool, root_class: &str) -> bool {
    mode == Some(Mode::Toggle) && !keep_open && hwnd_present && !own_process && !is_shell_class(root_class)
}

/// New `WINDOWPOS.flags` for a `WM_WINDOWPOSCHANGING` while the guard is set: a z-order change of
/// the guarded surface is removed with `SWP_NOZORDER`. `None` when nothing needs changing [spike].
pub(super) fn suppress_zorder(guarded_surface: isize, hwnd: isize, flags: u32) -> Option<u32> {
    (guarded_surface != 0 && hwnd == guarded_surface && flags & 0x4 == 0).then_some(flags | 0x4)
}

/// What a message on the surface does while Peek is on (or just ended by a click).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SurfaceAction {
    Pass,
    /// Eat the message.
    Swallow,
    /// End Peek; `true` = also eat the message.
    End(Cause, bool),
}

/// Mouse on the empty desktop (the surface plays Progman's role): `WM_LBUTTONDOWN` ends Peek and is swallowed
/// (no band selection or drag starts), `WM_RBUTTONUP` and `WM_MBUTTONUP` end it. The second click of a double-click that ended Peek is swallowed so it does
/// not toggle quick-hide.
pub(super) fn surface_action(on: bool, msg: u32, now_ms: u64, dbl_until_ms: u64) -> SurfaceAction {
    match msg {
        WM_LBUTTONDOWN if on => SurfaceAction::End(Cause::DesktopClick, true),
        WM_RBUTTONUP | WM_MBUTTONUP if on => SurfaceAction::End(Cause::DesktopButton, false),
        WM_LBUTTONDBLCLK if !on && now_ms < dbl_until_ms => SurfaceAction::Swallow,
        _ => SurfaceAction::Pass,
    }
}

/// Windows to promote, in stacking order: groups, portal popups, scrollbars. The surface is never part
/// of it. Null handles and duplicates are dropped.
pub(super) fn promotion_list(groups: &[HWND], popups: &[HWND], bars: &[HWND]) -> Vec<HWND> {
    let mut seen = std::collections::HashSet::new();
    groups.iter().chain(popups).chain(bars).copied().filter(|h| !h.0.is_null() && seen.insert(h.0 as isize)).collect()
}

// ---------------------------------------------------------------- QA z-order dump (CORRAL_QA_PEEK=1)

/// Verbose z-order dumps to the `CORRAL_LOG` file for the live checks.
fn qa_dump_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("CORRAL_QA_PEEK").is_ok_and(|v| v == "1"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WinInfo {
    hwnd: isize,
    class: String,
    title: String,
    owner: isize,
    pid: u32,
    corral: bool,
    topmost: bool,
    visible: bool,
}

/// Indices of the flagged entries plus `radius` neighbours on each side, ascending, no duplicates.
fn neighbour_indices(flags: &[bool], radius: usize) -> Vec<usize> {
    let mut keep = vec![false; flags.len()];
    for (i, flagged) in flags.iter().enumerate() {
        if *flagged {
            for slot in keep.iter_mut().take((i + radius + 1).min(flags.len())).skip(i.saturating_sub(radius)) {
                *slot = true;
            }
        }
    }
    keep.iter().enumerate().filter(|(_, k)| **k).map(|(i, _)| i).collect()
}

/// One-line summary: the surface's index in the z-order and how many visible foreign windows are above
/// it (anything above 0 with Peek off means the surface covers an application).
fn summary(all: &[WinInfo], surface: isize) -> String {
    let Some(idx) = all.iter().position(|w| w.hwnd == surface) else { return "surface not found in the chain".into() };
    let foreign_above = all[..idx].iter().filter(|w| !w.corral && w.visible).count();
    let topmost = all.iter().filter(|w| w.corral && w.topmost).count();
    format!("surface_index={idx} surface_topmost={} foreign_visible_above_surface={foreign_above} corral_topmost_windows={topmost}", all[idx].topmost)
}

// ---------------------------------------------------------------- Win32 helpers

pub(super) fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

fn window_title(hwnd: HWND) -> String {
    let mut buf = [0u16; 48];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize]).replace(['\r', '\n', '"'], " ")
}

pub(super) fn is_own_process(hwnd: HWND) -> bool {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid == unsafe { GetCurrentProcessId() }
}

unsafe extern "system" fn collect_window(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let list = unsafe { &mut *(lparam.0 as *mut Vec<WinInfo>) };
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let corral = pid == unsafe { GetCurrentProcessId() };
    list.push(WinInfo {
        hwnd: hwnd.0 as isize,
        class: class_name(hwnd),
        title: if corral { String::new() } else { window_title(hwnd) },
        owner: unsafe { GetWindow(hwnd, GW_OWNER) }.map(|o| o.0 as isize).unwrap_or(0),
        pid,
        corral,
        topmost: ex & WS_EX_TOPMOST.0 != 0,
        visible: unsafe { IsWindowVisible(hwnd) }.as_bool(),
    });
    TRUE
}

/// Every top-level window, top of the z-order first.
fn enumerate_windows() -> Vec<WinInfo> {
    let mut list: Vec<WinInfo> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(collect_window), LPARAM(&mut list as *mut Vec<WinInfo> as isize));
    }
    list
}

/// Apply one z-order position to every window in one `DeferWindowPos` batch.
fn batch_zorder(list: &[HWND], after: HWND) -> Result<(), String> {
    if list.is_empty() {
        return Ok(());
    }
    let flags = SWP_NOOWNERZORDER | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE;
    unsafe {
        let mut hdwp = BeginDeferWindowPos(list.len() as i32).map_err(|e| format!("BeginDeferWindowPos: {e}"))?;
        for hwnd in list {
            hdwp = DeferWindowPos(hdwp, *hwnd, after, 0, 0, 0, 0, flags).map_err(|e| format!("DeferWindowPos({:#x}): {e}", hwnd.0 as isize))?;
        }
        EndDeferWindowPos(hdwp).map_err(|e| format!("EndDeferWindowPos: {e}"))
    }
}

/// Called from `window_proc` for every `WM_WINDOWPOSCHANGING`, before the `APP` borrow. While Peek is
/// on, any z-order change of the surface (an activation pulling the owner forward, a stray
/// `SetWindowPos`) becomes a no-op [spike E2].
pub(super) fn guard_surface_zorder(hwnd: HWND, lparam: LPARAM) {
    let surface = GUARD_SURFACE.load(Ordering::Relaxed);
    if surface == 0 || lparam.0 == 0 || hwnd.0 as isize != surface {
        return;
    }
    let pos = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
    if let Some(flags) = suppress_zorder(surface, hwnd.0 as isize, pos.flags.0) {
        SUPPRESSED.fetch_add(1, Ordering::Relaxed);
        pos.flags = SET_WINDOW_POS_FLAGS(flags);
    }
}

/// Peek is on (for code that cannot reach `App`).
pub(super) fn is_active() -> bool {
    GUARD_SURFACE.load(Ordering::Relaxed) != 0
}


/// Ask for `WM_WTSSESSION_CHANGE` on the message window.
fn register_session_notification(hwnd: HWND) -> bool {
    unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) }.is_ok()
}

// ---------------------------------------------------------------- App integration

/// Peek state kept in `App`.
#[derive(Default)]
pub(super) struct Peek {
    mode: Option<Mode>,
    /// Foreground window at entry (given back at exit, see [`should_restore_foreground`]).
    prev_fg: HWND,
    /// The hook thread; only while the shortcuts are on (peek-hotkeys-spec 4: no hook otherwise).
    keys: Option<Keys>,
    /// The configuration the hook thread runs with.
    cfg: Option<Config>,
    /// The settings window shows the hook-failure warning while this is set (peek-hotkeys-spec 8).
    hook_failed: bool,
    /// Last fullscreen verdict handed to the hook thread.
    fullscreen: bool,
    wts_registered: bool,
    /// Ignore `WM_LBUTTONDBLCLK` on the surface until this tick (the click that ended Peek).
    dbl_until: u64,
    /// Handles last found without `WS_EX_TOPMOST` (log and retry only on change).
    last_missing: Vec<isize>,
}

impl Peek {
    pub(super) fn is_on(&self) -> bool {
        self.mode.is_some()
    }
}

/// The windows Peek lifts.
struct Targets {
    /// (scrollbar, its owner)
    bars: Vec<(HWND, HWND)>,
    /// groups, portal popups, scrollbars in promotion order
    all: Vec<HWND>,
}

impl App {
    /// Called from `create` (desktop mode): lock/unlock and similar reset Peek and the key tracking.
    pub(super) fn peek_init(&mut self) {
        if self.detached() {
            self.peek.wts_registered = register_session_notification(self.message_window);
            log(&format!("peek: session notifications registered={}", self.peek.wts_registered));
        }
    }

    /// Run first in `shutdown`: stop the hotkeys and the session notification. The windows die with the
    /// process (nothing about topmost persists).
    pub(super) fn peek_shutdown(&mut self) {
        self.peek.keys = None; // unhooks before the process exits
        self.set_hook_failed(false);
        unsafe {
            let _ = KillTimer(self.message_window, TIMER_PEEK);
        }
        GUARD_SURFACE.store(0, Ordering::Relaxed);
        self.peek.mode = None;
        if std::mem::take(&mut self.peek.wts_registered) {
            unsafe {
                let _ = WTSUnRegisterSessionNotification(self.message_window);
            }
        }
    }

    fn set_hook_failed(&mut self, failed: bool) {
        if self.peek.hook_failed != failed {
            self.peek.hook_failed = failed;
            peek_keys::publish_hook_failure(failed);
        }
    }

    /// (De)configure the shortcuts from the settings: the hook thread runs only in desktop mode, while
    /// `HotkeysGlanceEnabled` is on and at least one gesture is configured; with the feature off there is no
    /// hook at all (peek-hotkeys-spec 4). The Desktop-mode shortcuts (`HotkeysDesktop*`) stay unimplemented.
    pub(super) fn peek_configure(&mut self) {
        let cfg = Config::from_stored(
            !self.settings.disabled,
            self.settings.hotkeys_combo_glance_toggle,
            self.settings.hotkeys_combo_glance_hold,
        );
        let want = self.detached() && self.settings.hotkeys_glance_enabled && cfg.enabled && cfg.has_combo();
        if !want {
            self.peek.cfg = None;
            self.set_hook_failed(false);
            if self.peek.keys.take().is_some() {
                log("peek: shortcuts stopped (off in the settings, Corral off or not desktop mode)");
                if self.peek.is_on() {
                    self.peek_exit(Cause::Settings);
                } else {
                    unsafe {
                        let _ = KillTimer(self.message_window, TIMER_PEEK);
                    }
                }
            }
            return;
        }
        match &self.peek.keys {
            Some(keys) => {
                if self.peek.cfg != Some(cfg) {
                    keys.set_config(cfg);
                    log(&format!("peek: shortcuts changed to {cfg:?}"));
                }
            }
            None => match Keys::start(self.message_window, WM_PEEK_KEYS, cfg) {
                Ok(keys) => {
                    keys.set_peek_visible(self.peek.is_on());
                    self.peek.keys = Some(keys);
                    self.set_hook_failed(false);
                    unsafe {
                        SetTimer(self.message_window, TIMER_PEEK, FULLSCREEN_POLL_MS, None);
                    }
                    self.peek.fullscreen = false;
                    log(&format!("peek: shortcuts started ({cfg:?})"));
                    self.peek_fullscreen_refresh();
                }
                Err(e) => {
                    log(&format!("peek: shortcuts not available: {e}"));
                    self.set_hook_failed(true);
                }
            },
        }
        self.peek.cfg = Some(cfg);
    }

    /// `WM_PEEK_KEYS` from the hook thread.
    pub(super) fn peek_key_event(&mut self, wparam: usize) {
        match peek_keys::event_of(wparam) {
            peek_keys::EVENT_HOOK_LOST => {
                let lost = peek_keys::hook_lost_of(wparam);
                log(&format!("peek: keyboard hook {}", if lost { "lost" } else { "installed again" }));
                self.set_hook_failed(lost);
            }
            _ => {
                let Some(event) = event_from_wparam(wparam) else {
                    log(&format!("peek: unknown key event {wparam:#x}"));
                    return;
                };
                if event == Event::Dismiss && self.peek_editing() {
                    // the hook took the Esc away from the edit box: hand it over, the edit cancels itself
                    self.peek_forward_escape_to_edit();
                    return;
                }
                match transition(self.peek.mode, event) {
                    Action::Ignore => log(&format!("peek: {event:?} ignored (mode {:?})", self.peek.mode)),
                    Action::Enter(mode) => self.peek_enter(mode),
                    Action::Exit => self.peek_exit(match event {
                        Event::HoldEnd => Cause::HoldEnd,
                        Event::Dismiss => Cause::Esc,
                        _ => Cause::Toggle,
                    }),
                }
            }
        }
    }

    /// Esc while a name is edited belongs to the edit box (it cancels the edit), Glance stays.
    fn peek_forward_escape_to_edit(&self) {
        let edit = self.naming.as_ref().map(|n| n.0).or_else(|| self.renaming.as_ref().map(|r| r.0));
        if let Some(edit) = edit {
            unsafe {
                let _ = PostMessageW(edit, WM_KEYDOWN, WPARAM(VK_ESCAPE.0 as usize), LPARAM(0));
            }
        }
    }

    /// Foreground change while Peek is on (from the quick-hide WinEvent hook; works with or without the keyboard
    /// shortcuts): another application's window ends a toggled Peek unless `CorralPeekKeepOpen` is on.
    pub(super) fn peek_foreground_changed(&mut self, hwnd: HWND) {
        if hwnd.0.is_null() || self.peek.mode != Some(Mode::Toggle) {
            return;
        }
        let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
        let class = class_name(if root.0.is_null() { hwnd } else { root });
        let own = is_own_process(hwnd);
        let ends = activation_ends_peek(self.peek.mode, self.settings.peek_keep_open, true, own, &class);
        log(&format!("peek: foreground {:#x} class={class} own={own} -> {}", hwnd.0 as isize, if ends { "ends Glance" } else { "ignored" }));
        if ends {
            self.peek_exit(Cause::Activation);
        }
    }

    /// `WM_WTSSESSION_CHANGE`: the key tracking is reset, the hook installed again, and Peek ends silently.
    pub(super) fn peek_session_change(&mut self, code: usize) {
        log(&format!("peek: session change {code}"));
        // wallpaper-blur-spec 6: unlock and reconnect re-check the wallpaper (identical content changes nothing).
        crate::tint::invalidate_wallpaper();
        if let Some(keys) = &self.peek.keys {
            keys.reset();
        }
        if self.peek.is_on() {
            self.peek_exit(Cause::Session);
        }
    }

    /// `WM_POWERBROADCAST`: after a resume the hook is installed again (peek-hotkeys-spec 6).
    pub(super) fn peek_power(&mut self, code: usize) {
        if matches!(code, PBT_APMRESUMESUSPEND | PBT_APMRESUMEAUTOMATIC) {
            log("peek: resume from sleep, installing the keyboard hook again");
            // wallpaper-blur-spec 6: resume re-checks the wallpaper (identical content changes nothing).
            crate::tint::invalidate_wallpaper();
            if let Some(keys) = &self.peek.keys {
                keys.reset();
            }
        }
    }

    /// Fullscreen re-check every 2 s.
    pub(super) fn peek_timer(&mut self) {
        if self.peek.keys.is_none() && !self.peek.is_on() {
            unsafe {
                let _ = KillTimer(self.message_window, TIMER_PEEK);
            }
            return;
        }
        if let Some(keys) = &self.peek.keys {
            let lost = keys.is_lost();
            self.set_hook_failed(lost);
        }
        self.peek_fullscreen_refresh();
    }

    /// Ask the shell whether a fullscreen app is in front, tell the hook thread, and end Peek if one took
    /// over (peek-hotkeys-spec 7). Called on foreground change, display change and every 2 s.
    pub(super) fn peek_fullscreen_refresh(&mut self) {
        if self.peek.keys.is_none() && !self.peek.is_on() {
            return;
        }
        let reason = self.fullscreen_check();
        let refuse = reason.is_some();
        if refuse != self.peek.fullscreen {
            self.peek.fullscreen = refuse;
            log(&format!("peek: fullscreen {}", reason.as_deref().unwrap_or("ended")));
        }
        if let Some(keys) = &self.peek.keys {
            keys.set_fullscreen(refuse);
        }
        if refuse && self.peek.is_on() {
            self.peek_exit(Cause::Fullscreen);
        }
    }

    fn fullscreen_check(&self) -> Option<String> {
        let state = unsafe { SHQueryUserNotificationState() }.ok().map(|s| s.0);
        let fg = unsafe { GetForegroundWindow() };
        let (class, own) = if fg.0.is_null() { (String::new(), false) } else { (class_name(fg), is_own_process(fg)) };
        fullscreen_refusal(state, &class, own)
    }

    /// Group windows and scrollbars that exist now, in promotion order.
    fn peek_targets(&self) -> Targets {
        let alive = |h: &HWND| unsafe { IsWindow(*h) }.as_bool();
        let groups: Vec<HWND> = self.groups.iter().map(|g| g.hwnd).filter(alive).collect();
        let mut bars: Vec<_> = self.scrollbars.iter().filter(|(_, b)| b.alive()).map(|(id, b)| (*id, b.hwnd(), b.owner())).collect();
        bars.sort_by_key(|b| b.0);
        let bar_hwnds: Vec<HWND> = bars.iter().map(|b| b.1).collect();
        let all = promotion_list(&groups, &[], &bar_hwnds);
        Targets { bars: bars.into_iter().map(|b| (b.1, b.2)).collect(), all }
    }

    fn peek_editing(&self) -> bool {
        self.naming.is_some() || self.renaming.is_some()
    }

    /// Esc on a Corral window ends Peek (peek-hotkeys-spec 7). With the hook running the Esc is taken before
    /// the window sees it; this path serves Glance opened without the shortcuts (the tray icon). Not while a
    /// name is edited: there Esc cancels the edit.
    pub(super) fn peek_escape(&mut self, target: HWND) -> bool {
        if !self.peek.is_on() || self.peek_editing() || class_name(target).eq_ignore_ascii_case("Edit") {
            return false;
        }
        log("peek: Esc");
        self.peek_exit(Cause::Esc);
        true
    }

    fn peek_refusal(&self) -> Option<String> {
        entry_refusal(
            self.detached(),
            self.ole_drag.is_some() || OLE_DRAG_RUNNING.with(|c| c.get()),
            self.pages.moving(),
            self.peek_editing(),
            || self.fullscreen_check(),
        )
    }

    /// Turn Peek on: record the foreground window, lift the Corrals, guard the surface, show hidden
    /// groups, bring Corral to the foreground on the surface. A refusal after the hook consumed the key
    /// (a stale fullscreen flag) shows nothing: a rare lost key (peek-hotkeys-spec 7).
    fn peek_enter(&mut self, mode: Mode) {
        if let Some(reason) = self.peek_refusal() {
            log(&format!("peek: REFUSED, {reason}"));
            return;
        }
        let mut prev = unsafe { GetForegroundWindow() };
        if self.taskbar_click_foreground(prev) {
            // a click on the tray icon made the taskbar foreground: the window to give the foreground
            // back to at exit is the application that was active before
            prev = super::taskbar_item::last_foreign_foreground();
        }
        self.peek.prev_fg = prev;
        log(&format!("peek: ENTER {mode:?}, previous foreground={:#x} class={}", prev.0 as isize, class_name(prev)));
        self.peek_dump("before enter");
        self.qh_discard_pending_foreground(); // a stale activation must not read as a dismissal
        let t = self.peek_targets();
        // the guard first: from here on the surface cannot change z-order
        SUPPRESSED.store(0, Ordering::Relaxed);
        GUARD_SURFACE.store(self.surface_hwnd.0 as isize, Ordering::Relaxed);
        self.peek.mode = Some(mode);
        self.set_restacking(true);
        let result = batch_zorder(&t.all, HWND_TOPMOST);
        self.set_restacking(false);
        match &result {
            Ok(()) => log(&format!("peek: promoted {} windows to HWND_TOPMOST in one batch", t.all.len())),
            Err(e) => log(&format!("peek: promote batch FAILED: {e} (restack will retry the missing ones)")),
        }
        self.peek.last_missing.clear();
        self.qh_peek_suspend_idle();
        self.peek_display_changed();
        let ok = unsafe { SetForegroundWindow(self.surface_hwnd) }.as_bool();
        log(&format!("peek: SetForegroundWindow(surface) {}", if ok { "OK" } else { "refused by the system" }));
        if let Some(keys) = &self.peek.keys {
            keys.set_peek_visible(true); // Esc is taken while Glance is visible
        }
        unsafe {
            SetTimer(self.message_window, TIMER_PEEK, FULLSCREEN_POLL_MS, None);
        }
        self.peek_dump("after enter");
    }

    /// Turn Peek off: demote the lifted windows (and open edit boxes) in one batch, release the guard,
    /// run the normal restack, restore the quick-hide display and, if the cause allows, the foreground.
    pub(super) fn peek_exit(&mut self, cause: Cause) {
        let Some(mode) = self.peek.mode else { return };
        log(&format!("peek: EXIT ({mode:?}) cause {cause:?}"));
        if let Some(keys) = &self.peek.keys {
            keys.set_peek_visible(false);
        } else {
            unsafe {
                let _ = KillTimer(self.message_window, TIMER_PEEK);
            }
        }
        self.peek_dump("before exit");
        let t = self.peek_targets();
        self.set_restacking(true);
        let result = batch_zorder(&t.all, HWND_NOTOPMOST);
        // rename / naming edits were raised topmost by `peek_edit_zorder`
        let edits: Vec<HWND> = self.naming.iter().map(|n| n.0).chain(self.renaming.iter().map(|r| r.0)).collect();
        for edit in edits {
            unsafe {
                let _ = SetWindowPos(edit, HWND_NOTOPMOST, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
            }
        }
        self.set_restacking(false);
        match &result {
            Ok(()) => log(&format!("peek: demoted {} windows to HWND_NOTOPMOST in one batch", t.all.len())),
            Err(e) => log(&format!("peek: demote batch FAILED: {e}")),
        }
        log(&format!("peek: {} z-order change(s) of the surface suppressed", SUPPRESSED.swap(0, Ordering::Relaxed)));
        GUARD_SURFACE.store(0, Ordering::Relaxed);
        self.peek.mode = None;
        self.peek.last_missing.clear();
        let prev = std::mem::take(&mut self.peek.prev_fg);
        if !cause.full_exit() {
            return;
        }
        self.restack();
        // restack() orders the surface, groups and popups only: each scrollbar goes back directly above its owner
        self.set_restacking(true);
        for (bar, owner) in &t.bars {
            let above = unsafe { GetWindow(*owner, GW_HWNDPREV) }.unwrap_or_default();
            let after = if above.0.is_null() { HWND_TOP } else { above };
            unsafe {
                let _ = SetWindowPos(*bar, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
            }
        }
        self.set_restacking(false);
        self.peek_display_changed();
        if cause.restores_foreground() {
            let current = unsafe { GetForegroundWindow() };
            let ours = current.0.is_null() || is_own_process(current);
            let prev_alive = !prev.0.is_null() && unsafe { IsWindow(prev) }.as_bool();
            if should_restore_foreground(prev_alive, prev == current, ours) {
                let ok = unsafe { SetForegroundWindow(prev) }.as_bool();
                log(&format!("peek: foreground restore to {:#x}: {}", prev.0 as isize, if ok { "OK" } else { "refused by the system" }));
            } else {
                log(&format!("peek: foreground restore skipped (previous={:#x}, current={:#x}, current is Corral/none={ours})", prev.0 as isize, current.0 as isize));
            }
        }
        self.qh_peek_resume();
        self.peek_dump("after exit");
    }

    /// While Peek is on: lift windows that appeared or lost the flag since (a portal popup recreated by
    /// navigation, a scrollbar created on demand, a new group). Cheap; runs on every `restack()` request
    /// [spike E3].
    pub(super) fn peek_reapply(&mut self) {
        let t = self.peek_targets();
        let missing: Vec<HWND> = t.all.iter().copied().filter(|h| unsafe { GetWindowLongPtrW(*h, GWL_EXSTYLE) } as u32 & WS_EX_TOPMOST.0 == 0).collect();
        let sig: Vec<isize> = missing.iter().map(|h| h.0 as isize).collect();
        if sig == self.peek.last_missing {
            return;
        }
        if !missing.is_empty() {
            self.set_restacking(true);
            let result = batch_zorder(&missing, HWND_TOPMOST);
            self.set_restacking(false);
            let what: Vec<String> = missing.iter().map(|h| format!("{:#x}({})", h.0 as isize, class_name(*h))).collect();
            match result {
                Ok(()) => log(&format!("peek: re-promoted {} window(s) created or reset while on: {}", missing.len(), what.join(", "))),
                Err(e) => log(&format!("peek: re-promote of {} FAILED: {e}", what.join(", "))),
            }
            self.peek_dump("after re-promote");
        }
        self.peek.last_missing = sig;
    }

    /// The display follows Peek: hidden groups show at full alpha (quick-hide state untouched,
    /// `qh_group_alpha`) and groups with opacity != 255 show fully (`ShowHiddenCorrals` = 1 for the
    /// duration). Re-presents windows only where something changes.
    fn peek_display_changed(&mut self) {
        if self.layout.any_translucent() && !self.settings.show_hidden_corrals {
            for index in 0..self.groups.len() {
                let id = self.groups[index].id;
                if self.layout.groups.iter().any(|g| g.id == id && g.opacity != 255) {
                    self.render_group(index);
                }
            }
        }
        if self.quick_hide.phase() != Phase::Shown {
            self.quick_hide_apply();
        }
    }

    /// Group fade of View > Opacity is off (the global opacity override) or Peek shows everything.
    pub(super) fn transparency_disabled(&self) -> bool {
        self.settings.show_hidden_corrals || self.peek.is_on()
    }

    /// Mouse on the surface (called first in `handle_surface`). `Some` = handled.
    pub(super) fn peek_surface_input(&mut self, msg: u32) -> Option<LRESULT> {
        match surface_action(self.peek.is_on(), msg, tick_ms(), self.peek.dbl_until) {
            SurfaceAction::Pass => None,
            SurfaceAction::Swallow => Some(LRESULT(0)),
            SurfaceAction::End(cause, swallow) => {
                if cause == Cause::DesktopClick {
                    self.peek.dbl_until = tick_ms() + unsafe { GetDoubleClickTime() } as u64;
                }
                self.peek_exit(cause);
                swallow.then_some(LRESULT(0))
            }
        }
    }

    /// Replaces `SetFocus` on the surface in the press paths: focusing the surface while Peek is on would
    /// activate it and, through ownership, ask to raise it above the applications [spike E2]. Other
    /// windows are focused as before.
    pub(super) fn peek_set_focus(&self, hwnd: HWND) {
        if self.peek.is_on() && hwnd == self.surface_hwnd {
            return;
        }
        unsafe {
            let _ = SetFocus(hwnd);
        }
    }

    /// z-order arguments for raising a rename/naming edit: `HWND_TOP` without `SWP_NOOWNERZORDER` would
    /// also raise the edit's owner chain (group, surface). While Peek is on the edit goes topmost
    /// instead; it is demoted again with the rest [spike].
    pub(super) fn peek_edit_zorder(&self) -> (HWND, SET_WINDOW_POS_FLAGS) {
        if self.peek.is_on() { (HWND_TOPMOST, SWP_NOOWNERZORDER) } else { (HWND_TOP, SET_WINDOW_POS_FLAGS(0)) }
    }

    // ---- QA dump

    fn peek_roles(&self) -> Vec<(isize, String)> {
        let mut roles = vec![(self.surface_hwnd.0 as isize, "SURFACE".to_string()), (self.message_window.0 as isize, "message".to_string())];
        roles.extend(self.groups.iter().map(|g| (g.hwnd.0 as isize, format!("group#{}", g.id))));
        let mut bars: Vec<_> = self.scrollbars.iter().map(|(id, b)| (*id, b.hwnd())).collect();
        bars.sort_by_key(|p| p.0);
        roles.extend(bars.into_iter().map(|(id, h)| (h.0 as isize, format!("scrollbar#{id}"))));
        roles
    }

    /// Write the z-order neighbourhood of Corral's windows (and the foreground window) to the log
    /// (`CORRAL_QA_PEEK=1` only).
    fn peek_dump(&self, label: &str) {
        if !qa_dump_enabled() {
            return;
        }
        let all = enumerate_windows();
        let fg = unsafe { GetForegroundWindow() }.0 as isize;
        let roles = self.peek_roles();
        let flags: Vec<bool> = all.iter().map(|w| w.corral || w.hwnd == fg).collect();
        let shown = neighbour_indices(&flags, 1);
        log(&format!("peek: ---- z-order dump: {label} ({} top-level windows, {} shown; index 0 = top) ----", all.len(), shown.len()));
        for i in shown {
            let w = &all[i];
            let role = roles.iter().find(|(h, _)| *h == w.hwnd).map(|(_, r)| r.as_str()).unwrap_or(if w.corral { "corral-other" } else { "" });
            log(&format!(
                "peek: [{i:3}] hwnd={:#x} class={} role={role} owner={:#x} pid={} corral={} topmost={} visible={}{} title=\"{}\"",
                w.hwnd, w.class, w.owner, w.pid, w.corral, w.topmost, w.visible, if w.hwnd == fg { " FOREGROUND" } else { "" }, w.title,
            ));
        }
        log(&format!("peek: summary {}", summary(&all, self.surface_hwnd.0 as isize)));
    }
}

/// The message is one of Peek's (handled by `peek_message`).
pub(super) fn is_peek_message(msg: u32) -> bool {
    msg == WM_PEEK_KEYS || msg == WM_WTSSESSION_CHANGE || msg == WM_POWERBROADCAST || msg == super::taskbar_item::WM_TASKBAR_FLIP
}

/// Used by `handle_message_window`.
impl App {
    pub(super) fn peek_message(&mut self, msg: u32, wparam: WPARAM) {
        if msg == WM_PEEK_KEYS {
            self.peek_key_event(wparam.0);
        } else if msg == WM_WTSSESSION_CHANGE {
            self.peek_session_change(wparam.0);
        } else if msg == WM_POWERBROADCAST {
            self.peek_power(wparam.0);
        } else if msg == super::taskbar_item::WM_TASKBAR_FLIP {
            self.taskbar_flip();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(v: usize) -> HWND {
        HWND(v as *mut _)
    }

    #[test]
    fn state_machine_event_by_state() {
        use Action::*;
        use Event::*;
        // off
        assert_eq!(transition(None, Toggle), Enter(Mode::Toggle));
        assert_eq!(transition(None, HoldStart), Enter(Mode::Hold));
        assert_eq!(transition(None, HoldEnd), Ignore);
        assert_eq!(transition(None, Dismiss), Ignore);
        // toggle mode on: a hold neither closes nor re-opens it, its end never ends it
        assert_eq!(transition(Some(Mode::Toggle), Toggle), Exit);
        assert_eq!(transition(Some(Mode::Toggle), HoldStart), Ignore);
        assert_eq!(transition(Some(Mode::Toggle), HoldEnd), Ignore);
        assert_eq!(transition(Some(Mode::Toggle), Dismiss), Exit);
        // hold mode on
        assert_eq!(transition(Some(Mode::Hold), HoldEnd), Exit);
        assert_eq!(transition(Some(Mode::Hold), HoldStart), Ignore);
        assert_eq!(transition(Some(Mode::Hold), Toggle), Exit);
        assert_eq!(transition(Some(Mode::Hold), Dismiss), Exit);
    }

    #[test]
    fn key_events_map_from_the_peek_keys_constants() {
        assert_eq!(event_from_wparam(peek_keys::EVENT_TOGGLE), Some(Event::Toggle));
        assert_eq!(event_from_wparam(peek_keys::EVENT_HOLD_START), Some(Event::HoldStart));
        assert_eq!(event_from_wparam(peek_keys::EVENT_HOLD_END), Some(Event::HoldEnd));
        assert_eq!(event_from_wparam(peek_keys::EVENT_DISMISS), Some(Event::Dismiss));
        assert_eq!(event_from_wparam(0), None);
        assert_eq!(event_from_wparam(99), None);
    }

    #[test]
    fn entry_refusals_in_order_and_fullscreen_last() {
        let probe_called = std::cell::Cell::new(false);
        let probe = |reply: Option<String>| {
            let probe_called = &probe_called;
            move || {
                probe_called.set(true);
                reply
            }
        };
        assert!(entry_refusal(false, true, true, true, probe(Some("fs".into()))).unwrap().contains("not in desktop mode"));
        assert!(!probe_called.get(), "the shell probe is skipped once something else refuses");
        assert!(entry_refusal(true, true, true, true, probe(None)).unwrap().contains("OLE drag"));
        assert!(entry_refusal(true, false, true, true, probe(None)).unwrap().contains("Range"));
        assert!(entry_refusal(true, false, false, true, probe(None)).unwrap().contains("edit"));
        assert_eq!(entry_refusal(true, false, false, false, probe(Some("fullscreen".into()))), Some("fullscreen".into()));
        assert!(probe_called.get());
        assert_eq!(entry_refusal(true, false, false, false, || None), None);
    }

    #[test]
    fn fullscreen_refusal_follows_the_notification_state() {
        assert!(fullscreen_refusal(Some(2), "Chrome_WidgetWin_1", false).is_some());
        assert!(fullscreen_refusal(Some(3), "", false).is_some());
        assert!(fullscreen_refusal(Some(4), "", false).is_some());
        for ok in [Some(1), Some(5), Some(6), Some(7), None] {
            assert!(fullscreen_refusal(ok, "Notepad", false).is_none(), "{ok:?}");
        }
        // desktop, taskbar and Corral's own full-screen surface report QUNS_BUSY but are not fullscreen apps
        for class in ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd", "XamlExplorerHostIslandWindow", "XamlWindow"] {
            assert!(fullscreen_refusal(Some(2), class, false).is_none(), "{class}");
        }
        assert!(fullscreen_refusal(Some(2), "CorralSurface", true).is_none());
        assert!(fullscreen_refusal(Some(3), "CorralSurface", true).is_none());
    }

    #[test]
    fn activation_of_another_application_ends_a_toggled_peek() {
        let t = Some(Mode::Toggle);
        // another process, not the shell: ends
        assert!(activation_ends_peek(t, false, true, false, "Notepad"));
        assert!(activation_ends_peek(t, false, true, false, "CabinetWClass")); // File Explorer is an application
        assert!(activation_ends_peek(t, false, true, false, "Windows.UI.Core.CoreWindow"));
        // Corral's own windows (groups, popups, dialogs, menus)
        assert!(!activation_ends_peek(t, false, true, true, "CorralGroup"));
        assert!(!activation_ends_peek(t, false, true, true, "#32770"));
        // the desktop shell
        for class in ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd", "XamlExplorerHostIslandWindow", "XamlWindow"] {
            assert!(!activation_ends_peek(t, false, true, false, class), "{class}");
        }
        // keep-open, hold mode, Peek off, no window
        assert!(!activation_ends_peek(t, true, true, false, "Notepad"));
        assert!(!activation_ends_peek(Some(Mode::Hold), false, true, false, "Notepad"));
        assert!(!activation_ends_peek(None, false, true, false, "Notepad"));
        assert!(!activation_ends_peek(t, false, false, false, "Notepad"));
    }

    #[test]
    fn esc_and_hold_end_ignore_keep_open() {
        // `transition` and `Cause` do not know the setting at all: Esc and the hold end always act
        assert_eq!(transition(Some(Mode::Toggle), Event::Dismiss), Action::Exit);
        assert_eq!(transition(Some(Mode::Hold), Event::HoldEnd), Action::Exit);
        assert_eq!(transition(Some(Mode::Hold), Event::Dismiss), Action::Exit);
    }

    #[test]
    fn peek_key_event_codes_have_no_click() {
        assert_eq!(event_from_wparam(6), None, "hook-lost is handled apart from the Peek events");
    }

    #[test]
    fn surface_zorder_changes_are_suppressed_only_for_the_guarded_surface() {
        assert_eq!(suppress_zorder(7, 7, 0x10 | 0x200 | 0x1 | 0x2), Some(0x10 | 0x200 | 0x1 | 0x2 | 0x4));
        assert_eq!(suppress_zorder(7, 7, 0x10), Some(0x14)); // the activation without NOOWNERZORDER too
        assert_eq!(suppress_zorder(7, 7, 0x4 | 0x1), None); // already NOZORDER
        assert_eq!(suppress_zorder(0, 7, 0x10), None); // Peek off
        assert_eq!(suppress_zorder(7, 8, 0x10), None); // another window
    }

    #[test]
    fn foreground_is_restored_only_when_safe() {
        assert!(should_restore_foreground(true, false, true));
        assert!(!should_restore_foreground(false, false, true)); // window gone
        assert!(!should_restore_foreground(true, true, true)); // already foreground
        assert!(!should_restore_foreground(true, false, false)); // the user switched to another app
    }

    #[test]
    fn only_deliberate_exits_give_the_foreground_back() {
        for cause in [Cause::Toggle, Cause::HoldEnd, Cause::Esc, Cause::DesktopClick] {
            assert!(cause.restores_foreground(), "{cause:?}");
        }
        for cause in [Cause::Activation, Cause::DesktopButton, Cause::Fullscreen, Cause::Session, Cause::Settings, Cause::Reattach] {
            assert!(!cause.restores_foreground(), "{cause:?}");
        }
        assert!(Cause::Activation.full_exit() && !Cause::Reattach.full_exit());
    }

    #[test]
    fn surface_mouse_while_on_and_the_double_click_after() {
        use SurfaceAction::*;
        assert_eq!(surface_action(true, WM_LBUTTONDOWN, 0, 0), End(Cause::DesktopClick, true));
        assert_eq!(surface_action(true, WM_RBUTTONUP, 0, 0), End(Cause::DesktopButton, false));
        assert_eq!(surface_action(true, WM_MBUTTONUP, 0, 0), End(Cause::DesktopButton, false));
        assert_eq!(surface_action(true, WM_RBUTTONDOWN, 0, 0), Pass);
        assert_eq!(surface_action(true, WM_MOUSEMOVE, 0, 0), Pass);
        // off: untouched, except the second click of the double-click that ended Peek
        assert_eq!(surface_action(false, WM_LBUTTONDOWN, 10, 0), Pass);
        assert_eq!(surface_action(false, WM_RBUTTONUP, 10, 0), Pass);
        assert_eq!(surface_action(false, WM_LBUTTONDBLCLK, 100, 500), Swallow);
        assert_eq!(surface_action(false, WM_LBUTTONDBLCLK, 500, 500), Pass);
    }

    #[test]
    fn promotion_list_orders_groups_popups_bars_and_drops_null_and_duplicates() {
        let list = promotion_list(&[h(1), h(2), HWND::default()], &[h(3), h(2)], &[h(4), h(1)]);
        assert_eq!(list, vec![h(1), h(2), h(3), h(4)]);
        assert!(promotion_list(&[], &[], &[]).is_empty());
    }

    #[test]
    fn neighbour_indices_adds_radius_and_clamps() {
        let flags = [false, false, true, false, false, false, false, true];
        assert_eq!(neighbour_indices(&flags, 1), vec![1, 2, 3, 6, 7]);
        assert_eq!(neighbour_indices(&[true, false, false], 1), vec![0, 1]);
        assert_eq!(neighbour_indices(&[false, true, true, false], 1), vec![0, 1, 2, 3]);
        assert!(neighbour_indices(&[false, false], 1).is_empty());
        assert!(neighbour_indices(&[], 1).is_empty());
    }

    fn info(hwnd: isize, corral: bool, topmost: bool, visible: bool) -> WinInfo {
        WinInfo { hwnd, class: String::new(), title: String::new(), owner: 0, pid: 0, corral, topmost, visible }
    }

    #[test]
    fn summary_counts_foreign_windows_above_the_surface() {
        let all = [info(1, false, false, true), info(2, false, false, false), info(3, true, false, true), info(4, true, true, true)];
        assert_eq!(summary(&all, 3), "surface_index=2 surface_topmost=false foreign_visible_above_surface=1 corral_topmost_windows=1");
        assert_eq!(summary(&all, 99), "surface not found in the chain");
    }
}
