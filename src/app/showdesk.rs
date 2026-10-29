//! Show desktop (taskbar corner, Win+D): getting Corral's layers back above the desktop host at once.
//!
//! What Windows does [web, Raymond Chen "What is the difference between Show Desktop and Minimize All"
//! (devblogs.microsoft.com/oldnewthing/20040527-00) and a Delphi-Praxis quote of Explorer's
//! `ToggleDesktop` (delphipraxis.net/1151554-post14): "Show Desktop" minimizes every window that can be
//! minimized and then *raises the desktop* to the top of the window stack (a `DTM_RAISE`, `WM_USER+83`,
//! posted to Progman); showing the windows again lowers it and restores them. (Measured on a Windows 11 PC, read
//! only: Progman and ordinary windows share the z-band, `GetWindowBand` = 1, so the raise passes Corral's
//! popups] Corral's layers sit directly above Progman, so the raise puts Progman over them without
//! moving them: no `WM_WINDOWPOSCHANGED` reaches Corral, the icons and Corrals are gone until a restack
//! (formerly the 2 s reassert tick). The first `restack()` then puts the layers above Progman, i.e. at
//! the very top, above the minimized apps; when the apps are restored they come back below the layers
//! until the next restack, which is the "desktop lingers over the restored apps" symptom.
//!
//! Signals (out-of-context WinEvent hooks only, nothing injected into Explorer; to confirm
//! live with `CORRAL_QA_SHOWDESK=1`]:
//! * `EVENT_SYSTEM_FOREGROUND`: Progman (or the restored app) is activated right after the raise/lower.
//! * `EVENT_SYSTEM_MINIMIZESTART` / `MINIMIZEEND`: the apps are minimized / restored (the restore
//!   starts before the foreground change).
//! * `EVENT_OBJECT_REORDER` on the desktop window: a z-order change among the top-level windows (the
//!   raise/lower of Progman itself), where Windows reports it [unverified; the QA log shows whether
//!   the event arrives and which window it names].
//!
//! Every signal only sets a bit and posts [`WM_SHOWDESK`] (coalesced), so no work happens inside the
//! hook callback and none of it needs a timer to start. The handler restacks at once; because Explorer
//! finishes its re-ordering after the events, the restack repeats on a short fixed schedule
//! ([`settle_delay_ms`]); a restack of an already ordered stack touches nothing, so the repeats are
//! free and there is no feedback through `EVENT_OBJECT_REORDER` (own-process events are skipped).

use super::{App, Mode, log, tick_ms};
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::OnceLock;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Posted to the message window by the hook callback (coalesced).
pub(super) const WM_SHOWDESK: u32 = WM_APP + 0x22;
/// One-shot repeats of the restack after a trigger.
pub(super) const TIMER_SETTLE: usize = 0x5203; // distinct from peek::TIMER_PEEK on the same window
/// QA z-order sampler (`CORRAL_QA_SHOWDESK=1`), one second after a trigger.
pub(super) const TIMER_TRACE: usize = 0x5202;

const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const EVENT_SYSTEM_MINIMIZESTART: u32 = 0x0016;
const EVENT_SYSTEM_MINIMIZEEND: u32 = 0x0017;
const EVENT_OBJECT_REORDER: u32 = 0x8004;
const OBJID_WINDOW: i32 = 0;
const WINEVENT_OUTOFCONTEXT: u32 = 0;
const WINEVENT_SKIPOWNPROCESS: u32 = 2;

const TRACE_MS: u64 = 1000;
const TRACE_INTERVAL_MS: u32 = 16;
const TRACE_MAX_ENTRIES: usize = 24;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetTickCount() -> u32;
}

pub(super) fn is_timer(id: usize) -> bool {
    matches!(id, TIMER_SETTLE | TIMER_TRACE)
}

// ---------------------------------------------------------------------------------------- pure part

/// What the hooks report, as bits of the pending mask.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Signal {
    Foreground,
    MinimizeStart,
    MinimizeEnd,
    Reorder,
}

impl Signal {
    const ALL: [Signal; 4] = [Signal::Foreground, Signal::MinimizeStart, Signal::MinimizeEnd, Signal::Reorder];

    pub(super) fn bit(self) -> u32 {
        1 << self as u32
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Signal::Foreground => "Foreground",
            Signal::MinimizeStart => "MinimizeStart",
            Signal::MinimizeEnd => "MinimizeEnd",
            Signal::Reorder => "Reorder",
        }
    }
}

/// "Foreground+Reorder" for a pending mask.
pub(super) fn mask_names(mask: u32) -> String {
    let names: Vec<&str> = Signal::ALL.iter().filter(|s| mask & s.bit() != 0).map(|s| s.name()).collect();
    if names.is_empty() { "none".into() } else { names.join("+") }
}

/// Pure decision: does a WinEvent count as a show-desktop signal? Minimize events count for window
/// objects only; a reorder only when it names the desktop window (the parent of the top-level windows),
/// so the reorders inside list views and trees of other applications are dropped in the callback.
pub(super) fn accept_event(event: u32, id_object: i32, hwnd_is_desktop: bool) -> Option<Signal> {
    match event {
        EVENT_SYSTEM_FOREGROUND => Some(Signal::Foreground),
        EVENT_SYSTEM_MINIMIZESTART if id_object == OBJID_WINDOW => Some(Signal::MinimizeStart),
        EVENT_SYSTEM_MINIMIZEEND if id_object == OBJID_WINDOW => Some(Signal::MinimizeEnd),
        EVENT_OBJECT_REORDER if hwnd_is_desktop => Some(Signal::Reorder),
        _ => None,
    }
}

/// Repeats after the immediate restack of a trigger: delay before repeat `step` (0-based), in ms after
/// the previous one (cumulative 30, 120, 300 ms; the last one is the old `LATE_RESTACK_MS`).
pub(super) fn settle_delay_ms(step: u32) -> Option<u32> {
    [30, 90, 180].get(step as usize).copied()
}

/// Milliseconds from an event's `dwmsEventTime` (a `GetTickCount` value) to `now` (also `GetTickCount`);
/// both wrap every 49.7 days, an event "from the future" (clock granularity) reads as 0.
pub(super) fn latency_ms(now: u32, event_time: u32) -> u32 {
    let d = now.wrapping_sub(event_time);
    if d > u32::MAX / 2 { 0 } else { d }
}

/// A pending batch older than this lost its `WM_SHOWDESK`: `window_proc` drops messages while the app
/// is borrowed (a modal move/resize/menu loop inside a handler), and the batch then never cleared, so
/// no later event posted again and show-desktop stopped working (live 2026-10-15, after a Corral drag).
const STALE_POST_MS: u32 = 500;

/// Pure decision: an event at `time` finds a batch still pending since `first` (both `GetTickCount`).
pub(super) fn post_was_lost(time: u32, first: u32) -> bool {
    latency_ms(time, first) > STALE_POST_MS
}

/// Pure decision: the windows that sit directly above Progman (`run`, lowest first, only Corral's
/// layer windows that restack manages, scroll bars and edit boxes already left out) are exactly the
/// wanted stack `order` (lowest first): then nothing has to be moved.
pub(super) fn stack_is_ordered(run: &[isize], order: &[isize]) -> bool {
    run == order
}

// ------------------------------------------------------------------------------------- hook plumbing

static MSG_WND: AtomicIsize = AtomicIsize::new(0);
/// Bits of [`Signal`] seen since the handler last ran.
static PENDING: AtomicU32 = AtomicU32::new(0);
/// `dwmsEventTime` of the first event of the pending batch, and the window and signal it named.
static FIRST_TIME: AtomicU32 = AtomicU32::new(0);
static FIRST_HWND: AtomicIsize = AtomicIsize::new(0);
static FIRST_SIGNAL: AtomicU32 = AtomicU32::new(0);
/// Reorder events dropped because they did not name the desktop window (QA statistics).
static REORDER_REJECTED: AtomicU32 = AtomicU32::new(0);

pub(super) fn qa_trace() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("CORRAL_QA_SHOWDESK").is_ok_and(|v| v == "1"))
}

struct Hook(HWINEVENTHOOK);

impl Drop for Hook {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWinEvent(self.0);
        }
    }
}

/// The hooks of this module; dropped with the app.
pub(super) struct Hooks {
    _minimize: Option<Hook>,
    _reorder: Option<Hook>,
}

/// Called by the callbacks (here and the foreground callback of `quick_hide_glue`): record the signal,
/// post one message per batch. Safe at any time: touches atomics and `PostMessageW` only.
pub(super) fn note(event: u32, hwnd: HWND, id_object: i32, time: u32) {
    let is_desktop = event == EVENT_OBJECT_REORDER && hwnd == unsafe { GetDesktopWindow() };
    let Some(signal) = accept_event(event, id_object, is_desktop) else {
        if event == EVENT_OBJECT_REORDER {
            REORDER_REJECTED.fetch_add(1, Ordering::Relaxed);
        }
        return;
    };
    let msg = MSG_WND.load(Ordering::Relaxed);
    if msg == 0 {
        return;
    }
    if PENDING.load(Ordering::Relaxed) == 0 {
        FIRST_TIME.store(time, Ordering::Relaxed);
        FIRST_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
        FIRST_SIGNAL.store(signal.bit(), Ordering::Relaxed);
    }
    let prev = PENDING.fetch_or(signal.bit(), Ordering::Relaxed);
    let stale = prev != 0 && post_was_lost(time, FIRST_TIME.load(Ordering::Relaxed));
    if stale {
        FIRST_TIME.store(time, Ordering::Relaxed); // at most one re-post per STALE_POST_MS
    }
    if prev == 0 || stale {
        unsafe {
            let _ = PostMessageW(HWND(msg as *mut core::ffi::c_void), WM_SHOWDESK, WPARAM(0), LPARAM(0));
        }
    }
}

unsafe extern "system" fn hook_proc(_hook: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, _id_child: i32, _thread: u32, time: u32) {
    note(event, hwnd, id_object, time);
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

// ------------------------------------------------------------------------------------------ app part

#[derive(Default)]
pub(super) struct State {
    hooks: Option<Hooks>,
    /// Next entry of the settle schedule.
    step: u32,
    /// `tick_ms()` of the trigger the settle schedule and the trace belong to.
    t0_ms: u64,
    trace_until_ms: u64,
    trace_sig: String,
}

impl App {
    /// Desktop mode only: install the hooks (the foreground one is `quick_hide_glue`'s).
    pub(super) fn showdesk_init(&mut self) {
        if self.opts.mode != Mode::Desktop || !self.detached() {
            return;
        }
        MSG_WND.store(self.message_window.0 as isize, Ordering::Relaxed);
        let install = |min: u32, max: u32| {
            let h = unsafe { SetWinEventHook(min, max, None, Some(hook_proc), 0, 0, WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS) };
            (!h.is_invalid()).then_some(Hook(h))
        };
        let (minimize, reorder) = (install(EVENT_SYSTEM_MINIMIZESTART, EVENT_SYSTEM_MINIMIZEEND), install(EVENT_OBJECT_REORDER, EVENT_OBJECT_REORDER));
        log(&format!("showdesk: hooks minimize={} reorder={} (foreground hook is the quick-hide one), qa trace {}",
            minimize.is_some(), reorder.is_some(), qa_trace()));
        self.showdesk.hooks = Some(Hooks { _minimize: minimize, _reorder: reorder });
    }

    /// `WM_SHOWDESK`: restack now, then repeat on the settle schedule.
    pub(super) fn showdesk_event(&mut self) {
        let mask = PENDING.swap(0, Ordering::Relaxed);
        if mask == 0 {
            return;
        }
        let latency = latency_ms(unsafe { GetTickCount() }, FIRST_TIME.load(Ordering::Relaxed));
        let first_hwnd = HWND(FIRST_HWND.load(Ordering::Relaxed) as *mut core::ffi::c_void);
        let first = Signal::ALL.iter().find(|s| s.bit() == FIRST_SIGNAL.load(Ordering::Relaxed)).map_or("?", |s| s.name());
        let t = tick_ms();
        let started = std::time::Instant::now();
        let outcome = self.restack_for("showdesk");
        log(&format!(
            "showdesk: t={t} trigger={} first={first}({} {:#x}) event_to_handler={latency}ms restack={outcome:?} took={}us reorder_rejected={}",
            mask_names(mask), class_of(first_hwnd), first_hwnd.0 as isize, started.elapsed().as_micros(), REORDER_REJECTED.load(Ordering::Relaxed)
        ));
        self.showdesk.step = 0;
        self.showdesk.t0_ms = t;
        if !self.peek.is_on() {
            if let Some(ms) = settle_delay_ms(0) {
                unsafe {
                    SetTimer(self.message_window, TIMER_SETTLE, ms, None);
                }
            }
        }
        if qa_trace() {
            self.showdesk_trace_start(t);
        }
    }

    pub(super) fn showdesk_timer(&mut self, id: usize) -> bool {
        match id {
            TIMER_SETTLE => {
                unsafe {
                    let _ = KillTimer(self.message_window, TIMER_SETTLE);
                }
                let step = self.showdesk.step;
                let outcome = self.restack_for("showdesk-settle");
                // a Fixed outcome here means the trigger's restack was too early: the log shows by how much
                if outcome == super::hosting::Restack::Fixed || qa_trace() {
                    log(&format!("showdesk: settle step {step} t=+{}ms restack={outcome:?}", tick_ms().saturating_sub(self.showdesk.t0_ms)));
                }
                self.showdesk.step += 1;
                if let Some(ms) = settle_delay_ms(self.showdesk.step) {
                    unsafe {
                        SetTimer(self.message_window, TIMER_SETTLE, ms, None);
                    }
                }
                true
            }
            TIMER_TRACE => {
                self.showdesk_trace_sample();
                true
            }
            _ => false,
        }
    }

    fn showdesk_trace_start(&mut self, t: u64) {
        let running = self.showdesk.trace_until_ms > t;
        self.showdesk.trace_until_ms = t + TRACE_MS;
        if !running {
            self.showdesk.trace_sig.clear();
            unsafe {
                SetTimer(self.message_window, TIMER_TRACE, TRACE_INTERVAL_MS, None);
            }
        }
        log(&format!("showdesk-trace: t={t} trigger, sampling the stack for {TRACE_MS} ms"));
        self.showdesk_trace_sample();
    }

    fn showdesk_trace_sample(&mut self) {
        let now = tick_ms();
        let sig = self.showdesk_signature();
        if sig != self.showdesk.trace_sig {
            log(&format!("showdesk-trace: t=+{}ms {sig}", now.saturating_sub(self.showdesk.t0_ms)));
            self.showdesk.trace_sig = sig;
        }
        if now >= self.showdesk.trace_until_ms {
            unsafe {
                let _ = KillTimer(self.message_window, TIMER_TRACE);
            }
            log("showdesk-trace: end");
        }
    }

    /// Tag of one of Corral's own windows in the trace.
    fn showdesk_tag(&self, hwnd: HWND) -> Option<&'static str> {
        if hwnd == self.surface_hwnd {
            Some("SURFACE")
        } else if self.groups.iter().any(|g| g.hwnd == hwnd) {
            Some("GROUP")
        } else if self.is_layer(hwnd) {
            Some("BAR")
        } else {
            None
        }
    }

    /// The top-level stack from the top down to Progman and four windows below it, listing visible
    /// windows and Corral's own (`I` = iconic, `C` = DWM-cloaked, `h` = Corral window not visible):
    /// the live proof of what Explorer does around a show-desktop press.
    fn showdesk_signature(&self) -> String {
        use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
        let Some(base) = self.host.map(|h| h.progman) else { return "no desktop host".into() };
        let mut parts: Vec<String> = Vec::new();
        let mut below = 0;
        let mut seen_base = false;
        let mut skipped = 0;
        let mut hwnd = unsafe { GetTopWindow(HWND::default()) }.unwrap_or_default();
        let mut guard = 0;
        while !hwnd.0.is_null() && guard < 2000 && below < 4 {
            guard += 1;
            let ours = self.showdesk_tag(hwnd);
            let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
            if hwnd == base {
                seen_base = true;
                parts.push("<Progman>".into());
            } else if ours.is_some() || visible {
                if parts.len() >= TRACE_MAX_ENTRIES {
                    skipped += 1;
                } else {
                    let mut cloaked = 0u32;
                    let _ = unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut core::ffi::c_void, 4) };
                    let iconic = unsafe { IsIconic(hwnd) }.as_bool();
                    let name = ours.map(str::to_owned).unwrap_or_else(|| class_of(hwnd).chars().take(18).collect());
                    let flags = format!("{}{}{}", if iconic { "I" } else { "" }, if cloaked != 0 { "C" } else { "" }, if ours.is_some() && !visible { "h" } else { "" });
                    parts.push(if flags.is_empty() { name } else { format!("{name}[{flags}]") });
                }
                if seen_base {
                    below += 1;
                }
            }
            hwnd = unsafe { GetWindow(hwnd, GW_HWNDNEXT) }.unwrap_or_default();
        }
        if skipped > 0 {
            parts.push(format!("(+{skipped} more)"));
        }
        parts.join(" > ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreground_and_minimize_events_are_signals() {
        assert_eq!(accept_event(0x0003, 0, false), Some(Signal::Foreground));
        assert_eq!(accept_event(0x0016, OBJID_WINDOW, false), Some(Signal::MinimizeStart));
        assert_eq!(accept_event(0x0017, OBJID_WINDOW, false), Some(Signal::MinimizeEnd));
    }

    #[test]
    fn minimize_events_of_non_window_objects_are_dropped() {
        assert_eq!(accept_event(0x0016, -4, false), None);
        assert_eq!(accept_event(0x0017, 1, false), None);
    }

    #[test]
    fn only_desktop_reorders_count_and_unrelated_events_do_not() {
        assert_eq!(accept_event(0x8004, 0, true), Some(Signal::Reorder));
        assert_eq!(accept_event(0x8004, 0, false), None, "list view / tree reorders of other apps");
        assert_eq!(accept_event(0x8001, 0, true), None);
        assert_eq!(accept_event(0x000A, 0, true), None);
    }

    #[test]
    fn masks_name_their_signals() {
        assert_eq!(mask_names(0), "none");
        assert_eq!(mask_names(Signal::Foreground.bit()), "Foreground");
        assert_eq!(mask_names(Signal::Foreground.bit() | Signal::Reorder.bit()), "Foreground+Reorder");
        let bits: Vec<u32> = Signal::ALL.iter().map(|s| s.bit()).collect();
        assert!(bits.iter().enumerate().all(|(i, a)| bits.iter().skip(i + 1).all(|b| a & b == 0)), "one bit each");
    }

    #[test]
    fn settle_schedule_ends_after_three_repeats_and_reaches_the_old_300_ms() {
        let delays: Vec<u32> = (0..10).map_while(settle_delay_ms).collect();
        assert_eq!(delays, vec![30, 90, 180]);
        assert_eq!(delays.iter().sum::<u32>(), 300);
        assert_eq!(settle_delay_ms(3), None);
    }

    #[test]
    fn latency_survives_tick_wrap_and_clock_granularity() {
        assert_eq!(latency_ms(1_050, 1_000), 50);
        assert_eq!(latency_ms(20, u32::MAX - 29), 50, "tick counter wrapped between event and handler");
        assert_eq!(latency_ms(1_000, 1_004), 0, "an event stamped a few ms ahead of the reading");
    }

    #[test]
    fn a_batch_pending_for_over_half_a_second_lost_its_post() {
        assert!(!post_was_lost(1_200, 1_000), "a fresh batch is still on its way to the handler");
        assert!(!post_was_lost(1_500, 1_000));
        assert!(post_was_lost(1_501, 1_000), "the handler never ran: post again");
        assert!(post_was_lost(600, u32::MAX - 99), "across the tick wrap");
        assert!(!post_was_lost(1_000, 1_004), "an event stamped a few ms before the batch");
    }

    #[test]
    fn stack_is_ordered_only_when_the_run_is_exactly_the_wanted_order() {
        let order = [10, 20, 30, 40];
        assert!(stack_is_ordered(&[10, 20, 30, 40], &order));
        assert!(stack_is_ordered(&[], &[]));
        // Progman raised over the layers: nothing of ours directly above it
        assert!(!stack_is_ordered(&[], &order));
        // restored apps sit between Progman and the layers (Progman lowered, layers still on top):
        // the run directly above Progman is empty or partial
        assert!(!stack_is_ordered(&[10, 20], &order));
        // a group below the surface
        assert!(!stack_is_ordered(&[20, 10, 30, 40], &order));
        // an extra window of ours (a closed portal's stale owner) in the run
        assert!(!stack_is_ordered(&[10, 20, 30, 40, 50], &order));
    }
}
