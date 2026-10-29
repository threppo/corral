//! Glance (Peek) keyboard shortcuts, `docs/design/peek-hotkeys-spec.md`.
//!
//! Two layers:
//! * [`HotkeyEngine`] is pure: it takes one key event, a monotonic clock value in microseconds and a
//!   [`KeyOracle`] (the physical key state, `GetAsyncKeyState` in the app) and returns an [`Out`]
//!   (consume the event, at most one [`Emit`], ask for the Start-menu mask key). It never touches Win32.
//! * [`Keys`] is the shell: one hook thread owns the engine and the `WH_KEYBOARD_LL` hook (peek-hotkeys-spec 8).
//!   It posts events to the UI window. There is no mouse hook: dismissal by activating another application is
//!   decided on the UI thread (peek-hotkeys-spec).
//!
//! Stored combo: `(mods << 16) | vk` with Win = 1, Shift = 2, Ctrl = 4, Alt = 8. 0 = off.
//! Choices that name modifiers only (vk 0) are not offered any more: a modifier is never consumed
//! (peek-hotkeys-spec 4), so there would be no key to swallow. A stored modifier-only value runs as the
//! default shortcut of its slot and the settings page says so.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentThreadId, OpenEventW, ResetEvent, SetEvent, WaitForSingleObject, SYNCHRONIZATION_SYNCHRONIZE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, KBDLLHOOKSTRUCT, KillTimer, MSG, PM_NOREMOVE, PeekMessageW, PostMessageW,
    PostThreadMessageW, SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, HHOOK, WH_KEYBOARD_LL, WM_APP,
    WM_QUIT, WM_TIMER, WM_USER,
};
use windows::core::w;

// ---------------------------------------------------------------- combo model (peek-hotkeys-spec 2)

pub const MOD_WIN: u8 = 1;
pub const MOD_SHIFT: u8 = 2;
pub const MOD_CTRL: u8 = 4;
pub const MOD_ALT: u8 = 8;
const MOD_ALL: u8 = 15;

const VK_ESCAPE: u32 = 0x1B;
const VK_SPACE: u32 = 0x20;
const VK_L: u32 = 0x4C;
const VK_F4: u32 = 0x73;
/// The key the settings label "Tilde" (stored with vk 0x60, see [`decode_stored`]).
const VK_TILDE: u32 = 0xC0;
const STORED_TILDE: u32 = 0x60;
const VK_LCONTROL: u32 = 0xA2;

/// Scan code of the fake Ctrl that AltGr layouts send right before Right Alt (peek-hotkeys-spec 3).
const ALTGR_FAKE_CTRL_SCAN: u32 = 0x21D;

/// Modifier virtual keys (generic and left/right) and the merged modifier each one stands for.
const MOD_TABLE: [(u32, u8); 11] = [
    (0x5B, MOD_WIN),
    (0x5C, MOD_WIN),
    (0x10, MOD_SHIFT),
    (0xA0, MOD_SHIFT),
    (0xA1, MOD_SHIFT),
    (0x11, MOD_CTRL),
    (0xA2, MOD_CTRL),
    (0xA3, MOD_CTRL),
    (0x12, MOD_ALT),
    (0xA4, MOD_ALT),
    (0xA5, MOD_ALT),
];

fn mod_bit(vk: u32) -> Option<u8> {
    MOD_TABLE.iter().find(|(v, _)| *v == vk).map(|(_, b)| *b)
}

/// A shortcut: merged modifiers (left and right keys count the same) plus the trigger key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Combo {
    pub mods: u8,
    pub key: u32,
}

pub const DEFAULT_TOGGLE: Combo = Combo { mods: MOD_WIN, key: VK_SPACE };
pub const DEFAULT_HOLD: Combo = Combo { mods: MOD_WIN | MOD_CTRL, key: VK_SPACE };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComboError {
    BadKey,
    NoModifier,
    /// Without Win at least two modifiers are required (Ctrl+K, Alt+F, Shift+A collide with typing).
    TooFewModifiers,
    /// Win+L cannot be intercepted, Alt+F4 closes windows.
    Reserved,
}

fn key_allowed(key: u32) -> bool {
    key == VK_SPACE || (0x41..=0x5A).contains(&key) || (0x30..=0x39).contains(&key) || (0x70..=0x7B).contains(&key) || key == VK_TILDE
}

/// The one validation both the loader and the settings page use (peek-hotkeys-spec 2).
pub fn validate_combo(c: Combo) -> Result<(), ComboError> {
    if !key_allowed(c.key) {
        return Err(ComboError::BadKey);
    }
    if c.mods == 0 || c.mods & !MOD_ALL != 0 {
        return Err(ComboError::NoModifier);
    }
    if (c.mods == MOD_WIN && c.key == VK_L) || (c.mods == MOD_ALT && c.key == VK_F4) {
        return Err(ComboError::Reserved);
    }
    if c.mods & MOD_WIN == 0 && c.mods.count_ones() < 2 {
        return Err(ComboError::TooFewModifiers);
    }
    Ok(())
}

/// What a stored `u32` means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stored {
    /// 0: that gesture is off.
    None,
    Combo(Combo),
    /// Modifier-only (vk 0), unknown bits or a combo `validate_combo` refuses: the slot runs its default.
    Invalid,
}

pub fn decode_stored(stored: u32) -> Stored {
    if stored == 0 {
        return Stored::None;
    }
    let mods = (stored >> 16) & 0xFFFF;
    if mods & !(MOD_ALL as u32) != 0 {
        return Stored::Invalid;
    }
    let raw_key = stored & 0xFFFF;
    let key = if raw_key == STORED_TILDE { VK_TILDE } else { raw_key };
    let combo = Combo { mods: mods as u8, key };
    match validate_combo(combo) {
        Ok(()) => Stored::Combo(combo),
        Err(_) => Stored::Invalid,
    }
}

#[allow(dead_code)] // the dropdown writes the table values; the tests check the round trip
pub fn encode(c: Combo) -> u32 {
    let key = if c.key == VK_TILDE { STORED_TILDE } else { c.key };
    ((c.mods as u32) << 16) | key
}

/// A non-zero stored value that runs as the slot's default (settings page note).
pub fn stored_falls_back(stored: u32) -> bool {
    decode_stored(stored) == Stored::Invalid
}

fn resolve_slot(stored: u32, default: Combo) -> Option<Combo> {
    match decode_stored(stored) {
        Stored::None => None,
        Stored::Combo(c) => Some(c),
        Stored::Invalid => Some(default),
    }
}

/// Engine configuration. `enabled` = Glance shortcuts on and Corral on in desktop mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub enabled: bool,
    pub toggle: Option<Combo>,
    pub hold: Option<Combo>,
}

impl Config {
    /// A combo for toggle and one for hold must differ: an equal pair keeps the toggle.
    pub fn from_stored(enabled: bool, toggle: u32, hold: u32) -> Config {
        let toggle = resolve_slot(toggle, DEFAULT_TOGGLE);
        let mut hold = resolve_slot(hold, DEFAULT_HOLD);
        if toggle.is_some() && toggle == hold {
            hold = None;
        }
        Config { enabled, toggle, hold }
    }

    /// Whether any gesture is configured (otherwise the hook is not installed at all).
    pub fn has_combo(&self) -> bool {
        self.toggle.is_some() || self.hold.is_some()
    }
}

// ---------------------------------------------------------------- engine (peek-hotkeys-spec 3 to 6)

/// What the hook thread tells the UI thread (at most one per key event).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Emit {
    Toggle,
    HoldStart,
    HoldEnd,
    Dismiss,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Out {
    pub consume: bool,
    pub emit: Option<Emit>,
    /// Inject the Start-menu mask key (peek-hotkeys-spec 4).
    pub mask: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct KeyEvent {
    pub vk: u32,
    pub scan: u32,
    pub down: bool,
    /// Injected by another process: treated like a physical key (peek-hotkeys-spec 5), so the engine reads
    /// nothing from it; the field documents the policy and lets the tests feed injected streams.
    #[allow(dead_code)]
    pub injected: bool,
    /// Carries Corral's own marker: ignored entirely.
    pub marker: bool,
}

/// The physical key state (`GetAsyncKeyState` in the app, a fake in the tests).
pub trait KeyOracle {
    fn is_down(&self, vk: u32) -> bool;
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct KeySet([u64; 4]);

impl KeySet {
    fn has(&self, vk: u32) -> bool {
        let v = (vk & 0xFF) as usize;
        (self.0[v >> 6] >> (v & 63)) & 1 == 1
    }
    fn set(&mut self, vk: u32) {
        let v = (vk & 0xFF) as usize;
        self.0[v >> 6] |= 1 << (v & 63);
    }
    fn clear(&mut self, vk: u32) {
        let v = (vk & 0xFF) as usize;
        self.0[v >> 6] &= !(1 << (v & 63));
    }
    fn is_empty(&self) -> bool {
        self.0 == [0; 4]
    }
    /// Lowest set key at or above `from`.
    fn next_from(&self, from: u32) -> Option<u32> {
        let mut v = from as usize;
        while v < 256 {
            let word = self.0[v >> 6] >> (v & 63);
            if word != 0 {
                return Some((v + word.trailing_zeros() as usize) as u32);
            }
            v = (v | 63) + 1;
        }
        None
    }
}

#[derive(Clone, Copy)]
struct Hold {
    mods: u8,
    trigger: u32,
}

/// A repeated down of a consumed trigger is an auto-repeat only while repeats keep coming; a down this long
/// after the last consumed event means the key-up was missed (peek-hotkeys-spec 6, modifier resync).
const STALE_US: u64 = 2_000_000;
/// A callback this slow makes Windows drop the hook (peek-hotkeys-spec 6).
pub const SLOW_US: u64 = 150_000;
/// Budget of one callback (peek-hotkeys-spec 6).
pub const BUDGET_US: u64 = 1_000;

pub struct HotkeyEngine {
    cfg: Config,
    peek_visible: bool,
    fullscreen: bool,
    /// Modifier keys believed down.
    mods_down: KeySet,
    /// Other keys believed down (a combo must be built modifiers first, then the trigger).
    keys_down: KeySet,
    /// Keys whose down was consumed and whose up must be consumed too.
    consumed: KeySet,
    last_seen: [u64; 256],
    hold: Option<Hold>,
    slow: bool,
    max_callback_us: u64,
    over_budget: u32,
}

impl HotkeyEngine {
    pub fn new(cfg: Config) -> HotkeyEngine {
        HotkeyEngine {
            cfg,
            peek_visible: false,
            fullscreen: false,
            mods_down: KeySet::default(),
            keys_down: KeySet::default(),
            consumed: KeySet::default(),
            last_seen: [0; 256],
            hold: None,
            slow: false,
            max_callback_us: 0,
            over_budget: 0,
        }
    }

    pub fn set_peek_visible(&mut self, visible: bool) {
        self.peek_visible = visible;
    }

    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        self.fullscreen = fullscreen;
    }

    /// A changed configuration ends a running hold and keeps the consumed set, so the trigger's up is still
    /// consumed (peek-hotkeys-spec 8).
    pub fn set_config(&mut self, cfg: Config) -> Option<Emit> {
        if cfg == self.cfg {
            return None;
        }
        self.cfg = cfg;
        self.hold.take().map(|_| Emit::HoldEnd)
    }

    /// Session lock, desktop switch, resume, hook reinstall: forget what was believed about the keyboard.
    pub fn reset(&mut self) -> Option<Emit> {
        self.mods_down = KeySet::default();
        self.keys_down = KeySet::default();
        self.consumed = KeySet::default();
        self.hold.take().map(|_| Emit::HoldEnd)
    }

    pub fn hold_active(&self) -> bool {
        self.hold.is_some()
    }

    pub fn tracked_mods(&self) -> u8 {
        let mut m = 0;
        for (vk, bit) in MOD_TABLE {
            if self.mods_down.has(vk) {
                m |= bit;
            }
        }
        m
    }

    #[allow(dead_code)] // statistics and test probes
    pub fn consumed_is_empty(&self) -> bool {
        self.consumed.is_empty()
    }

    #[allow(dead_code)] // statistics and test probes
    pub fn tracking_is_empty(&self) -> bool {
        self.mods_down.is_empty() && self.keys_down.is_empty()
    }

    /// Duration of the last callback, measured by the shell with a monotonic clock.
    pub fn record_callback(&mut self, micros: u64) {
        self.max_callback_us = self.max_callback_us.max(micros);
        if micros > BUDGET_US {
            self.over_budget = self.over_budget.saturating_add(1);
        }
        if micros > SLOW_US {
            self.slow = true;
        }
    }

    /// The hook thread should reinstall the hook (a callback was slow).
    pub fn take_slow(&mut self) -> bool {
        std::mem::take(&mut self.slow)
    }

    #[allow(dead_code)] // statistics and test probes
    pub fn callbacks_over_budget(&self) -> u32 {
        self.over_budget
    }

    #[allow(dead_code)] // statistics and test probes
    pub fn max_callback_us(&self) -> u64 {
        self.max_callback_us
    }

    fn active(&self) -> bool {
        self.cfg.enabled && !self.fullscreen && self.cfg.has_combo()
    }

    /// Drop modifiers and keys the oracle says are up: a missed key-up (Win+L, secure desktop,
    /// Ctrl+Alt+Del). Only removes; a key that is down but untracked changes nothing worse than a missed combo.
    fn resync(&mut self, oracle: &dyn KeyOracle) {
        for (vk, _) in MOD_TABLE {
            if self.mods_down.has(vk) && !oracle.is_down(vk) {
                self.mods_down.clear(vk);
            }
        }
        let mut from = 0;
        while let Some(vk) = self.keys_down.next_from(from) {
            if !oracle.is_down(vk) {
                self.keys_down.clear(vk);
            }
            from = vk + 1;
        }
    }

    pub fn on_key(&mut self, ev: KeyEvent, now: u64, oracle: &dyn KeyOracle) -> Out {
        let mut out = Out::default();
        if ev.marker || ev.vk > 0xFF {
            return out;
        }
        let vk = ev.vk;

        // Modifiers always pass; they only update tracking (peek-hotkeys-spec 4).
        if mod_bit(vk).is_some() {
            if vk == VK_LCONTROL && ev.scan == ALTGR_FAKE_CTRL_SCAN {
                return out;
            }
            if ev.down {
                self.mods_down.set(vk);
            } else {
                self.mods_down.clear(vk);
                if let Some(h) = self.hold {
                    if self.tracked_mods() & h.mods != h.mods {
                        self.hold = None;
                        out.emit = Some(Emit::HoldEnd);
                    }
                }
            }
            return out;
        }

        // A consumed key: its repeats and its up are consumed, whatever the settings say now.
        if self.consumed.has(vk) {
            if !ev.down {
                self.consumed.clear(vk);
                out.consume = true;
                if self.hold.is_some_and(|h| h.trigger == vk) {
                    self.hold = None;
                    out.emit = Some(Emit::HoldEnd);
                }
                return out;
            }
            if now.saturating_sub(self.last_seen[vk as usize]) <= STALE_US {
                self.last_seen[vk as usize] = now;
                out.consume = true;
                return out;
            }
            // The up was missed long ago: forget the key. This down is eaten once more so a hold that
            // still waits for it ends cleanly; the next press starts normally.
            self.consumed.clear(vk);
            out.consume = true;
            if self.hold.is_some_and(|h| h.trigger == vk) {
                self.hold = None;
                out.emit = Some(Emit::HoldEnd);
            }
            return out;
        }
        if !ev.down {
            self.keys_down.clear(vk);
            return out; // never consume an up without a consumed down
        }

        if self.active() {
            if vk == VK_ESCAPE {
                // Esc is taken when no modifier is held, or while a hold runs (its modifiers are down by
                // definition) (peek-hotkeys-spec 3 lists Esc as the end of a hold)
                // A repeat of an Esc whose down already reached the application is never taken, or the
                // application would keep a pressed key without its up.
                if self.peek_visible && !self.keys_down.has(vk) && (self.tracked_mods() == 0 || self.hold.is_some()) {
                    self.consumed.set(vk);
                    self.last_seen[vk as usize] = now;
                    self.hold = None;
                    out.consume = true;
                    out.emit = Some(Emit::Dismiss);
                    return out;
                }
            } else {
                self.resync(oracle);
                if self.keys_down.is_empty() {
                    let mods = self.tracked_mods();
                    let kind = if self.cfg.toggle.is_some_and(|c| c.mods == mods && c.key == vk) {
                        Some((Emit::Toggle, mods))
                    } else if self.cfg.hold.is_some_and(|c| c.mods == mods && c.key == vk) {
                        Some((Emit::HoldStart, mods))
                    } else {
                        None
                    };
                    if let Some((emit, mods)) = kind {
                        self.consumed.set(vk);
                        self.last_seen[vk as usize] = now;
                        out.consume = true;
                        out.mask = mods & (MOD_WIN | MOD_ALT) != 0;
                        if emit == Emit::HoldStart {
                            if self.hold.is_none() {
                                self.hold = Some(Hold { mods, trigger: vk });
                                out.emit = Some(Emit::HoldStart);
                            }
                        } else {
                            out.emit = Some(Emit::Toggle);
                        }
                        return out;
                    }
                }
            }
        }
        self.keys_down.set(vk);
        out
    }

    /// Hold watchdog (every 250 ms while a hold runs): a required modifier that the physical state says is
    /// up ends the hold. The trigger is not probed: a consumed key never reaches the system key state.
    pub fn on_tick(&mut self, _now: u64, oracle: &dyn KeyOracle) -> Out {
        let mut out = Out::default();
        if let Some(h) = self.hold {
            self.resync(oracle);
            if self.tracked_mods() & h.mods != h.mods {
                self.hold = None;
                out.emit = Some(Emit::HoldEnd);
            }
        }
        out
    }
}

// ---------------------------------------------------------------- hook thread shell

const MARKER: usize = 0x4350_4B31; // "CPK1"
const MASK_VK: u16 = 0xE8; // unassigned virtual key

pub const EVENT_TOGGLE: usize = 1;
pub const EVENT_HOLD_START: usize = 2;
pub const EVENT_HOLD_END: usize = 3;
pub const EVENT_DISMISS: usize = 4;
/// The hook could not be (re)installed (bit 8 set) or recovered (bit 8 clear).
pub const EVENT_HOOK_LOST: usize = 6;
const EVENT_MASK: usize = 0xFF;
const HOOK_LOST_FLAG: usize = 0x100;

fn emit_code(e: Emit) -> usize {
    match e {
        Emit::Toggle => EVENT_TOGGLE,
        Emit::HoldStart => EVENT_HOLD_START,
        Emit::HoldEnd => EVENT_HOLD_END,
        Emit::Dismiss => EVENT_DISMISS,
    }
}

/// Event number of a posted `wParam`.
pub fn event_of(wparam: usize) -> usize {
    wparam & EVENT_MASK
}

/// Hook lost (`true`) or recovered (`false`) from an `EVENT_HOOK_LOST` `wParam`.
pub fn hook_lost_of(wparam: usize) -> bool {
    wparam & HOOK_LOST_FLAG != 0
}

const TM_SYNC: u32 = WM_APP + 0x10;
const TM_MASK: u32 = WM_APP + 0x11;
const TM_RESET: u32 = WM_APP + 0x12;
const WATCHDOG_MS: u32 = 250;

static POST_TARGET: AtomicIsize = AtomicIsize::new(0);
static POST_MSG: AtomicU32 = AtomicU32::new(0);

fn post(wparam: usize) {
    let target = POST_TARGET.load(Ordering::Relaxed);
    if target != 0 {
        unsafe {
            let _ = PostMessageW(HWND(target as *mut core::ffi::c_void), POST_MSG.load(Ordering::Relaxed), WPARAM(wparam), LPARAM(0));
        }
    }
}

struct SystemKeys;

impl KeyOracle for SystemKeys {
    fn is_down(&self, vk: u32) -> bool {
        unsafe { (GetAsyncKeyState(vk as i32) as u16) & 0x8000 != 0 }
    }
}

struct Shared {
    config: Mutex<Config>,
    peek_visible: AtomicBool,
    fullscreen: AtomicBool,
    thread_id: AtomicU32,
    lost: AtomicBool,
    /// Set when `Keys::start` gave up waiting: the thread must not install a hook any more.
    cancelled: AtomicBool,
}

struct Ctx {
    engine: HotkeyEngine,
    shared: Arc<Shared>,
    keyboard: HHOOK,
    timer: usize,
    base: Instant,
}

thread_local! {
    /// The hook procedures run on the hook thread only.
    static CTX: RefCell<Option<Ctx>> = const { RefCell::new(None) };
}

fn thread_post_quit(shared: &Shared) {
    let tid = shared.thread_id.load(Ordering::SeqCst);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

fn thread_post(shared: &Shared, msg: u32) {
    let tid = shared.thread_id.load(Ordering::Relaxed);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, msg, WPARAM(0), LPARAM(0));
        }
    }
}

/// One keyboard event: engine step plus the posts it asks for. Returns whether to consume.
fn step_key(kb: &KBDLLHOOKSTRUCT, message: u32) -> bool {
    let started = Instant::now();
    let up = kb.flags.0 & 0x80 != 0 || message == 0x0101 || message == 0x0105; // LLKHF_UP, WM_KEYUP, WM_SYSKEYUP
    CTX.try_with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else { return false };
        let Some(ctx) = guard.as_mut() else { return false };
        ctx.engine.set_peek_visible(ctx.shared.peek_visible.load(Ordering::Relaxed));
        ctx.engine.set_fullscreen(ctx.shared.fullscreen.load(Ordering::Relaxed));
        let now = ctx.base.elapsed().as_micros() as u64;
        let ev = KeyEvent { vk: kb.vkCode, scan: kb.scanCode, down: !up, injected: kb.flags.0 & 0x12 != 0, marker: false };
        let was_holding = ctx.engine.hold_active();
        let out = ctx.engine.on_key(ev, now, &SystemKeys);
        if let Some(e) = out.emit {
            post(emit_code(e));
        }
        if out.mask {
            thread_post(&ctx.shared, TM_MASK);
        }
        ctx.engine.record_callback(started.elapsed().as_micros() as u64);
        if ctx.engine.hold_active() != was_holding || ctx.engine.slow {
            thread_post(&ctx.shared, TM_SYNC);
        }
        out.consume
    })
    .unwrap_or(false)
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == 0 && lparam.0 != 0 {
        // SAFETY: for WH_KEYBOARD_LL lParam points to a KBDLLHOOKSTRUCT valid during the call.
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        // Corral's own mask keys are never looked at (peek-hotkeys-spec 4).
        if kb.dwExtraInfo != MARKER {
            let consume = std::panic::catch_unwind(|| step_key(kb, wparam.0 as u32)).unwrap_or(false);
            if consume {
                return LRESULT(1);
            }
        }
    }
    unsafe { CallNextHookEx(HHOOK::default(), code, wparam, lparam) }
}

fn install_keyboard() -> Result<HHOOK, String> {
    unsafe {
        let hmod = GetModuleHandleW(None).unwrap_or_default();
        SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), hmod, 0).map_err(|e| e.to_string())
    }
}

fn unhook(hook: HHOOK) {
    if !hook.0.is_null() {
        unsafe {
            let _ = UnhookWindowsHookEx(hook);
        }
    }
}

/// The mask key: a down and an up of an unassigned virtual key with Corral's marker, so Windows sees a key
/// between a lone Win (or Alt) press and release and does not open the Start menu (peek-hotkeys-spec 4).
fn inject_mask() {
    let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(MASK_VK), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: MARKER } },
    };
    let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
    unsafe {
        let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn with_ctx<R>(f: impl FnOnce(&mut Ctx) -> R) -> Option<R> {
    CTX.try_with(|cell| cell.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(f))).ok().flatten()
}

/// Bring the hook, the watchdog timer and the engine configuration in line with the shared state.
fn sync() {
    with_ctx(|ctx| {
        let cfg = *ctx.shared.config.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(e) = ctx.engine.set_config(cfg) {
            post(emit_code(e));
        }
        if ctx.engine.take_slow() {
            reinstall(ctx);
        }
        let holding = ctx.engine.hold_active();
        if holding && ctx.timer == 0 {
            ctx.timer = unsafe { SetTimer(HWND::default(), 0, WATCHDOG_MS, None) };
        } else if !holding && ctx.timer != 0 {
            unsafe {
                let _ = KillTimer(HWND::default(), ctx.timer);
            }
            ctx.timer = 0;
        }
    });
}

/// Unhook and hook again, forgetting key tracking (slow callback, resume, unlock).
fn reinstall(ctx: &mut Ctx) {
    unhook(ctx.keyboard);
    ctx.keyboard = HHOOK::default();
    if let Some(e) = ctx.engine.reset() {
        post(emit_code(e));
    }
    match install_keyboard() {
        Ok(h) => {
            ctx.keyboard = h;
            if ctx.shared.lost.swap(false, Ordering::Relaxed) {
                post(EVENT_HOOK_LOST);
            }
        }
        Err(_) => {
            ctx.shared.lost.store(true, Ordering::Relaxed);
            post(EVENT_HOOK_LOST | HOOK_LOST_FLAG);
        }
    }
}

fn tick() {
    with_ctx(|ctx| {
        let now = ctx.base.elapsed().as_micros() as u64;
        let out = ctx.engine.on_tick(now, &SystemKeys);
        if let Some(e) = out.emit {
            post(emit_code(e));
        }
    });
    sync();
}

fn hook_thread(shared: Arc<Shared>, cfg: Config, ready: mpsc::Sender<Result<u32, String>>) {
    unsafe {
        let mut msg = MSG::default();
        // Create the thread's message queue before announcing the thread id.
        let _ = PeekMessageW(&mut msg, HWND::default(), WM_USER, WM_USER, PM_NOREMOVE);
        let tid = GetCurrentThreadId();
        shared.thread_id.store(tid, Ordering::SeqCst);
        // `Keys::start` may have given up already: install nothing, now or later.
        if shared.cancelled.load(Ordering::SeqCst) {
            return;
        }
        let keyboard = match install_keyboard() {
            Ok(h) => h,
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        };
        if shared.cancelled.load(Ordering::SeqCst) {
            unhook(keyboard);
            return;
        }
        CTX.with(|c| {
            *c.borrow_mut() =
                Some(Ctx { engine: HotkeyEngine::new(cfg), shared: shared.clone(), keyboard, timer: 0, base: Instant::now() })
        });
        let _ = ready.send(Ok(tid));
        while GetMessageW(&mut msg, HWND::default(), 0, 0).0 > 0 {
            match msg.message {
                TM_SYNC => sync(),
                TM_MASK => inject_mask(),
                TM_RESET => {
                    with_ctx(reinstall);
                    sync();
                }
                WM_TIMER => {
                    if with_ctx(|ctx| ctx.timer != 0 && msg.wParam.0 == ctx.timer).unwrap_or(false) {
                        tick();
                    }
                }
                _ => {}
            }
        }
        with_ctx(|ctx| {
            unhook(ctx.keyboard);
            if ctx.timer != 0 {
                let _ = KillTimer(HWND::default(), ctx.timer);
            }
        });
        CTX.with(|c| *c.borrow_mut() = None);
    }
}

/// The warning of peek-hotkeys-spec 8 shows when a reinstall failed or the thread is gone.
fn hook_unavailable(reinstall_failed: bool, thread_finished: bool) -> bool {
    reinstall_failed || thread_finished
}

/// Handle to the running hook thread. Dropping it unhooks and ends the thread.
pub struct Keys {
    shared: Arc<Shared>,
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

impl Keys {
    /// Spawn the hook thread and install `WH_KEYBOARD_LL` there. Events are posted to `target` as
    /// `msg` with the event number in `wParam`. `Err` carries the reason when the hook cannot be installed.
    pub fn start(target: HWND, msg: u32, cfg: Config) -> Result<Keys, String> {
        POST_TARGET.store(target.0 as isize, Ordering::Relaxed);
        POST_MSG.store(msg, Ordering::Relaxed);
        let shared = Arc::new(Shared {
            config: Mutex::new(cfg),
            peek_visible: AtomicBool::new(false),
            fullscreen: AtomicBool::new(false),
            thread_id: AtomicU32::new(0),
            lost: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        });
        let (tx, rx) = mpsc::channel();
        let thread_shared = shared.clone();
        let join = std::thread::Builder::new()
            .name("peek-keys-hook".into())
            .spawn(move || hook_thread(thread_shared, cfg, tx))
            .map_err(|e| format!("cannot start the hook thread: {e}"))?;
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(thread_id)) => Ok(Keys { shared, thread_id, join: Some(join) }),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(format!("SetWindowsHookEx(WH_KEYBOARD_LL) failed: {e}"))
            }
            Err(e) => {
                // This thread can never install a hook later, and it ends if it already installed one.
                shared.cancelled.store(true, Ordering::SeqCst);
                thread_post_quit(&shared);
                Err(format!("the hook thread did not report back: {e}"))
            }
        }
    }

    pub fn set_config(&self, cfg: Config) {
        *self.shared.config.lock().unwrap_or_else(|e| e.into_inner()) = cfg;
        thread_post(&self.shared, TM_SYNC);
    }

    /// Glance is shown or hidden: while it is visible Esc is taken (peek-hotkeys-spec 4).
    pub fn set_peek_visible(&self, visible: bool) {
        self.shared.peek_visible.store(visible, Ordering::Relaxed);
    }

    pub fn set_fullscreen(&self, fullscreen: bool) {
        self.shared.fullscreen.store(fullscreen, Ordering::Relaxed);
    }

    /// Resume, unlock, lock: forget key tracking and install the hook again.
    pub fn reset(&self) {
        thread_post(&self.shared, TM_RESET);
    }

    /// The hook is not working: a reinstall failed, or the hook thread has ended on its own.
    pub fn is_lost(&self) -> bool {
        hook_unavailable(self.shared.lost.load(Ordering::Relaxed), self.join.as_ref().is_none_or(|j| j.is_finished()))
    }
}

impl Drop for Keys {
    fn drop(&mut self) {
        unsafe {
            let _ = PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        POST_TARGET.store(0, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------- hook failure flag for the settings window

/// The settings window is its own process: the app publishes "the hook is not available" as a named
/// event that vanishes with the app (peek-hotkeys-spec 8, warning box).
const FAILURE_EVENT: windows::core::PCWSTR = w!("Local\\CorralPeekHookFailed");
static FAILURE_HANDLE: AtomicIsize = AtomicIsize::new(0);

// `CreateEventW` sits behind the `Win32_Security` feature of the `windows` crate, which the project does not enable.
#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateEventW(attributes: *const core::ffi::c_void, manual_reset: i32, initial_state: i32, name: *const u16) -> isize;
}

/// Called by the app: `true` while the shortcuts wanted a hook that is not running.
pub fn publish_hook_failure(failed: bool) {
    unsafe {
        let mut handle = FAILURE_HANDLE.load(Ordering::Relaxed);
        if handle == 0 {
            if !failed {
                return;
            }
            handle = CreateEventW(std::ptr::null(), 1, 0, FAILURE_EVENT.0);
            if handle == 0 {
                return;
            }
            FAILURE_HANDLE.store(handle, Ordering::Relaxed);
        }
        let h = windows::Win32::Foundation::HANDLE(handle as *mut core::ffi::c_void);
        let _ = if failed { SetEvent(h) } else { ResetEvent(h) };
    }
}

/// Called by the settings window.
pub fn hook_failure_published() -> bool {
    unsafe {
        let Ok(h) = OpenEventW(SYNCHRONIZATION_SYNCHRONIZE, BOOL(0), FAILURE_EVENT) else { return false };
        let signalled = WaitForSingleObject(h, 0) == WAIT_OBJECT_0;
        let _ = windows::Win32::Foundation::CloseHandle(h);
        signalled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::HOTKEY_CHOICES;

    const LWIN: u32 = 0x5B;
    const RWIN: u32 = 0x5C;
    const LSHIFT: u32 = 0xA0;
    const LCTRL: u32 = 0xA2;
    const RCTRL: u32 = 0xA3;
    const LALT: u32 = 0xA4;
    const RALT: u32 = 0xA5;
    const SPACE: u32 = 0x20;
    const KEY_A: u32 = 0x41;
    const KEY_X: u32 = 0x58;
    const F5: u32 = 0x74;
    const ESC: u32 = 0x1B;

    fn default_cfg() -> Config {
        Config::from_stored(true, 65568, 327712)
    }

    struct Phys(KeySet);

    impl KeyOracle for Phys {
        fn is_down(&self, vk: u32) -> bool {
            self.0.has(vk)
        }
    }

    /// Fake event stream with a clock and the physical state behind the oracle.
    struct Rig {
        engine: HotkeyEngine,
        phys: Phys,
        now: u64,
        emits: Vec<Emit>,
        masks: u32,
    }

    impl Rig {
        fn new(cfg: Config) -> Rig {
            Rig { engine: HotkeyEngine::new(cfg), phys: Phys(KeySet::default()), now: 1_000_000, emits: Vec::new(), masks: 0 }
        }

        fn raw(&mut self, vk: u32, down: bool, scan: u32, injected: bool, marker: bool) -> Out {
            self.now += 10_000;
            let out = self.engine.on_key(KeyEvent { vk, scan, down, injected, marker }, self.now, &self.phys);
            // The system key state of the event being processed is updated after the hook returns, and a
            // consumed event never updates it.
            if !marker && !out.consume {
                if down { self.phys.0.set(vk) } else { self.phys.0.clear(vk) }
            }
            if let Some(e) = out.emit {
                self.emits.push(e);
            }
            if out.mask {
                self.masks += 1;
            }
            out
        }

        fn down(&mut self, vk: u32) -> Out {
            self.raw(vk, true, 0, false, false)
        }

        fn up(&mut self, vk: u32) -> Out {
            self.raw(vk, false, 0, false, false)
        }

        fn wait(&mut self, ms: u64) {
            self.now += ms * 1000;
        }

        fn tick(&mut self) -> Out {
            let out = self.engine.on_tick(self.now, &self.phys);
            if let Some(e) = out.emit {
                self.emits.push(e);
            }
            out
        }

        fn lose_up(&mut self, vk: u32) {
            self.phys.0.clear(vk);
        }

        fn take_emits(&mut self) -> Vec<Emit> {
            std::mem::take(&mut self.emits)
        }
    }

    fn combo(mods: u8, key: u32) -> Combo {
        Combo { mods, key }
    }

    #[test]
    fn win_space_toggle_for_both_win_keys() {
        for win in [LWIN, RWIN] {
            let mut r = Rig::new(default_cfg());
            assert_eq!(r.down(win), Out::default());
            let o = r.down(SPACE);
            assert_eq!(o, Out { consume: true, emit: Some(Emit::Toggle), mask: true });
            assert_eq!(r.up(SPACE), Out { consume: true, emit: None, mask: false });
            assert_eq!(r.up(win), Out::default());
            assert!(r.engine.consumed_is_empty() && r.engine.tracking_is_empty());
        }
    }

    #[test]
    fn nothing_happens_when_off() {
        let mut off = default_cfg();
        off.enabled = false;
        let none = Config { enabled: true, toggle: None, hold: None };
        for cfg in [off, none] {
            let mut r = Rig::new(cfg);
            for (vk, down) in [(LWIN, true), (SPACE, true), (SPACE, false), (LWIN, false), (LCTRL, true), (ESC, true), (ESC, false)] {
                assert_eq!(r.raw(vk, down, 0, false, false), Out::default());
            }
            assert!(r.emits.is_empty() && r.masks == 0);
        }
    }

    #[test]
    fn combos_need_the_exact_modifier_set_and_the_right_order() {
        // Win alone
        let mut r = Rig::new(default_cfg());
        assert!(!r.down(LWIN).consume);
        assert!(!r.up(LWIN).consume);
        // Win+Shift+Space (reverse language switch) is not Win+Space
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.down(LSHIFT);
        assert_eq!(r.down(SPACE), Out::default());
        // Ctrl+Space
        let mut r = Rig::new(default_cfg());
        r.down(LCTRL);
        assert_eq!(r.down(SPACE), Out::default());
        // Win+Ctrl+Space with only the toggle configured
        let only_toggle = Config { enabled: true, toggle: Some(DEFAULT_TOGGLE), hold: None };
        let mut r = Rig::new(only_toggle);
        r.down(LWIN);
        r.down(LCTRL);
        assert_eq!(r.down(SPACE), Out::default());
        // Space first, then Win
        let mut r = Rig::new(default_cfg());
        assert!(!r.down(SPACE).consume);
        assert!(!r.down(LWIN).consume);
        assert!(r.emits.is_empty());
        // another key held first: Win, X, Space starts nothing
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.down(KEY_X);
        assert_eq!(r.down(SPACE), Out::default());
        // a half-built combo consumes nothing
        let mut r = Rig::new(default_cfg());
        assert_eq!(r.down(LWIN), Out::default());
        assert_eq!(r.down(LCTRL), Out::default());
        assert_eq!(r.up(LCTRL), Out::default());
    }

    #[test]
    fn hold_starts_at_the_trigger_and_ends_with_it() {
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.down(LCTRL);
        let o = r.down(SPACE);
        assert_eq!(o, Out { consume: true, emit: Some(Emit::HoldStart), mask: true });
        assert_eq!(r.up(SPACE), Out { consume: true, emit: Some(Emit::HoldEnd), mask: false });
        r.up(LCTRL);
        r.up(LWIN);
        assert_eq!(r.take_emits(), vec![Emit::HoldStart, Emit::HoldEnd]);
    }

    #[test]
    fn hold_ends_when_a_required_modifier_is_released_first_and_the_trigger_up_is_still_consumed() {
        for released_first in [LWIN, LCTRL] {
            let mut r = Rig::new(default_cfg());
            r.down(LWIN);
            r.down(LCTRL);
            r.down(SPACE);
            let o = r.up(released_first);
            assert!(!o.consume, "a modifier up always passes");
            assert_eq!(o.emit, Some(Emit::HoldEnd));
            assert!(r.up(SPACE).consume);
            let rest = if released_first == LWIN { LCTRL } else { LWIN };
            assert_eq!(r.up(rest), Out::default());
            assert_eq!(r.take_emits(), vec![Emit::HoldStart, Emit::HoldEnd]);
            assert!(r.engine.consumed_is_empty() && r.engine.tracking_is_empty());
        }
    }

    #[test]
    fn auto_repeat_never_restarts_a_hold_or_a_toggle() {
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.down(LCTRL);
        r.down(SPACE);
        for _ in 0..5 {
            r.wait(30);
            assert_eq!(r.down(SPACE), Out { consume: true, emit: None, mask: false });
        }
        r.up(SPACE);
        assert_eq!(r.take_emits(), vec![Emit::HoldStart, Emit::HoldEnd]);
        // toggle: one Toggle, repeats consumed
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.down(SPACE);
        for _ in 0..5 {
            r.wait(30);
            assert!(r.down(SPACE).consume);
        }
        assert!(r.up(SPACE).consume);
        assert_eq!(r.take_emits(), vec![Emit::Toggle]);
        assert_eq!(r.masks, 1);
    }

    #[test]
    fn hold_pressed_while_toggle_is_open_just_reports_start_and_end() {
        // The UI thread decides what a hold does to an open Glance; the engine still reports both events.
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.down(SPACE);
        r.up(SPACE);
        r.down(LCTRL);
        r.down(SPACE);
        r.up(SPACE);
        assert_eq!(r.take_emits(), vec![Emit::Toggle, Emit::HoldStart, Emit::HoldEnd]);
    }

    #[test]
    fn esc_is_consumed_with_its_up_only_while_glance_is_visible() {
        let mut r = Rig::new(default_cfg());
        assert_eq!(r.down(ESC), Out::default());
        assert_eq!(r.up(ESC), Out::default());
        r.engine.set_peek_visible(true);
        assert_eq!(r.down(ESC), Out { consume: true, emit: Some(Emit::Dismiss), mask: false });
        assert!(r.down(ESC).consume, "auto-repeat of the consumed Esc");
        assert_eq!(r.up(ESC), Out { consume: true, emit: None, mask: false });
        // the next Esc reaches the application
        r.engine.set_peek_visible(false);
        assert_eq!(r.down(ESC), Out::default());
        r.up(ESC);
        // Ctrl+Esc is not Esc
        r.engine.set_peek_visible(true);
        r.down(LCTRL);
        assert_eq!(r.down(ESC), Out::default());
    }

    #[test]
    fn esc_ends_a_hold_and_the_trigger_up_is_still_consumed() {
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.down(LCTRL);
        r.down(SPACE);
        r.engine.set_peek_visible(true);
        let o = r.down(ESC);
        assert_eq!(o, Out { consume: true, emit: Some(Emit::Dismiss), mask: false });
        assert!(!r.engine.hold_active());
        assert!(r.up(ESC).consume);
        // the modifiers still pass, no second end event, the trigger up is consumed
        assert_eq!(r.up(LCTRL), Out::default());
        assert!(r.up(SPACE).consume);
        assert_eq!(r.up(LWIN), Out::default());
        assert_eq!(r.take_emits(), vec![Emit::HoldStart, Emit::Dismiss]);
        assert!(r.engine.consumed_is_empty() && r.engine.tracking_is_empty());
        // with a toggle (no hold) and a modifier held, Esc is not taken
        let mut r = Rig::new(default_cfg());
        r.engine.set_peek_visible(true);
        r.down(LWIN);
        assert_eq!(r.down(ESC), Out::default());
    }

    #[test]
    fn altgr_fake_ctrl_does_not_make_ctrl_alt() {
        let cfg = Config { enabled: true, toggle: Some(combo(MOD_CTRL | MOD_ALT, SPACE)), hold: None };
        // AltGr+Space: fake Ctrl (scan 0x21D), Right Alt, Space
        let mut r = Rig::new(cfg);
        assert_eq!(r.raw(LCTRL, true, ALTGR_FAKE_CTRL_SCAN, false, false), Out::default());
        r.down(RALT);
        assert_eq!(r.down(SPACE), Out::default());
        r.up(SPACE);
        r.raw(LCTRL, false, ALTGR_FAKE_CTRL_SCAN, false, false);
        r.up(RALT);
        assert!(r.engine.tracking_is_empty());
        // real Ctrl+Alt+Space
        let mut r = Rig::new(cfg);
        r.raw(LCTRL, true, 0x1D, false, false);
        r.down(LALT);
        let o = r.down(SPACE);
        assert_eq!((o.consume, o.emit, o.mask), (true, Some(Emit::Toggle), true));
    }

    #[test]
    fn a_missed_key_up_is_repaired_before_a_lone_space_is_judged() {
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.lose_up(LWIN); // Win+L: the up never arrives
        let o = r.down(SPACE);
        assert_eq!(o, Out::default(), "a lone Space passes");
        assert_eq!(r.engine.tracked_mods(), 0, "tracking repaired");
        r.up(SPACE);
        // and a real Win+Space works again
        r.down(LWIN);
        assert!(r.down(SPACE).consume);
    }

    #[test]
    fn a_stale_other_key_does_not_block_a_combo() {
        let mut r = Rig::new(default_cfg());
        r.down(KEY_A);
        r.lose_up(KEY_A);
        r.down(LWIN);
        assert!(r.down(SPACE).consume);
    }

    #[test]
    fn a_consumed_trigger_whose_up_was_missed_is_released_after_the_stale_window() {
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.down(SPACE);
        r.up(LWIN);
        r.wait(3000); // the Space up never arrived
        assert!(r.down(SPACE).consume); // eaten once more, then forgotten
        assert!(r.engine.consumed_is_empty());
        assert_eq!(r.down(SPACE), Out::default());
    }

    #[test]
    fn hold_watchdog_ends_a_hold_whose_modifier_up_was_missed() {
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.down(LCTRL);
        r.down(SPACE);
        r.take_emits();
        r.wait(250);
        assert_eq!(r.tick(), Out::default(), "keys still down: no change");
        r.lose_up(LWIN);
        r.wait(250);
        assert_eq!(r.tick().emit, Some(Emit::HoldEnd));
        assert_eq!(r.tick(), Out::default(), "no hold, no change");
        assert!(r.up(SPACE).consume, "the trigger up is still consumed");
        // no events and no hold: nothing
        let mut idle = Rig::new(default_cfg());
        assert_eq!(idle.tick(), Out::default());
    }

    #[test]
    fn fullscreen_consumes_nothing_but_finishes_a_started_stream() {
        let mut r = Rig::new(default_cfg());
        r.engine.set_fullscreen(true);
        r.down(LWIN);
        assert_eq!(r.down(SPACE), Out::default());
        r.up(SPACE);
        r.up(LWIN);
        // the stream started before the flag
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        assert!(r.down(SPACE).consume);
        r.engine.set_fullscreen(true);
        assert!(r.down(SPACE).consume, "repeat");
        assert!(r.up(SPACE).consume);
        assert_eq!(r.up(LWIN), Out::default());
        // and Esc is not taken in fullscreen
        r.engine.set_peek_visible(true);
        assert_eq!(r.down(ESC), Out::default());
    }

    #[test]
    fn a_config_change_mid_hold_ends_it_and_keeps_the_trigger_consumed() {
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.down(LCTRL);
        r.down(SPACE);
        let changed = Config { enabled: true, toggle: Some(combo(MOD_WIN, KEY_A)), hold: None };
        assert_eq!(r.engine.set_config(changed), Some(Emit::HoldEnd));
        assert_eq!(r.engine.set_config(changed), None);
        assert!(r.up(SPACE).consume);
        assert_eq!(r.engine.tracked_mods(), MOD_WIN | MOD_CTRL, "tracking intact");
        r.up(LCTRL);
        r.up(LWIN);
        // switching the feature off still lets the consumed up through the engine
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.down(SPACE);
        r.engine.set_config(Config { enabled: false, ..default_cfg() });
        assert!(r.up(SPACE).consume);
    }

    #[test]
    fn reset_clears_tracking_and_ends_a_hold() {
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        r.down(LCTRL);
        r.down(SPACE);
        assert_eq!(r.engine.reset(), Some(Emit::HoldEnd));
        assert!(r.engine.tracking_is_empty() && r.engine.consumed_is_empty());
        assert_eq!(r.engine.reset(), None);
    }

    #[test]
    fn injected_input_counts_but_the_marker_never_does() {
        // other injected events behave like physical keys
        let mut r = Rig::new(default_cfg());
        r.raw(LWIN, true, 0, true, false);
        let o = r.raw(SPACE, true, 0, true, false);
        assert!(o.consume && o.emit == Some(Emit::Toggle));
        assert!(r.raw(SPACE, false, 0, true, false).consume);
        r.raw(LWIN, false, 0, true, false);
        // an injected up whose down was not consumed passes
        let mut r = Rig::new(default_cfg());
        assert_eq!(r.raw(SPACE, false, 0, true, false), Out::default());
        // the marker leaves tracking and consumption alone
        let mut r = Rig::new(default_cfg());
        r.down(LWIN);
        let before = r.engine.tracked_mods();
        assert_eq!(r.raw(0xE8, true, 0, true, true), Out::default());
        assert_eq!(r.raw(0xE8, false, 0, true, true), Out::default());
        assert_eq!(r.engine.tracked_mods(), before);
        assert!(r.engine.consumed_is_empty());
        let marked_space = r.raw(SPACE, true, 0, true, true);
        assert_eq!(marked_space, Out::default());
        assert!(r.down(SPACE).consume, "the next real Space still starts the combo");
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn invariants_hold_over_generated_streams() {
        const KEYS: [u32; 11] = [LWIN, RWIN, LSHIFT, LCTRL, RCTRL, LALT, SPACE, KEY_A, KEY_X, F5, ESC];
        let cfg = Config { enabled: true, toggle: Some(DEFAULT_TOGGLE), hold: Some(DEFAULT_HOLD) };
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for round in 0..300 {
            let mut r = Rig::new(cfg);
            r.engine.set_peek_visible(round % 2 == 0);
            let mut held = KeySet::default();
            let mut consumed_down = KeySet::default();
            let mut expected_masks = 0;
            for _ in 0..80 {
                let vk = KEYS[(rng.next() % KEYS.len() as u64) as usize];
                let is_mod = mod_bit(vk).is_some();
                let down = if held.has(vk) { rng.next() % 3 == 0 } else { true };
                let repeat = down && held.has(vk);
                let mods_before = r.engine.tracked_mods();
                let o = r.raw(vk, down, 0, rng.next() % 4 == 0, false);
                if is_mod {
                    assert!(!o.consume, "modifier events always pass");
                }
                if down {
                    held.set(vk);
                    if o.consume {
                        consumed_down.set(vk);
                    } else if !repeat {
                        assert!(!consumed_down.has(vk));
                    }
                    if matches!(o.emit, Some(Emit::Toggle | Emit::HoldStart)) && mods_before & (MOD_WIN | MOD_ALT) != 0 {
                        expected_masks += 1;
                    }
                    assert_eq!(o.mask, matches!(o.emit, Some(Emit::Toggle | Emit::HoldStart)) && mods_before & (MOD_WIN | MOD_ALT) != 0);
                } else {
                    held.clear(vk);
                    assert_eq!(o.consume, consumed_down.has(vk), "an up is consumed exactly when its down was");
                    consumed_down.clear(vk);
                }
            }
            // release everything
            let mut from = 0;
            while let Some(vk) = held.next_from(from) {
                let o = r.up(vk);
                assert_eq!(o.consume, consumed_down.has(vk));
                from = vk + 1;
            }
            assert!(r.engine.consumed_is_empty());
            assert!(r.engine.tracking_is_empty(), "round {round}: mods {:?} keys {:?}", r.engine.mods_down.0, r.engine.keys_down.0);
            assert!(!r.engine.hold_active());
            assert_eq!(r.masks, expected_masks);
        }
    }

    #[test]
    fn stored_values_round_trip_and_the_examples() {
        assert_eq!(decode_stored(0), Stored::None);
        assert_eq!(decode_stored(65568), Stored::Combo(DEFAULT_TOGGLE));
        assert_eq!(decode_stored(327712), Stored::Combo(DEFAULT_HOLD));
        assert_eq!(encode(DEFAULT_TOGGLE), 65568);
        assert_eq!(encode(DEFAULT_HOLD), 327712);
        assert_eq!(decode_stored(393248), Stored::Combo(combo(MOD_SHIFT | MOD_CTRL, SPACE)));
        // every valid combo round-trips
        let mut valid = 0;
        for mods in 0..=255u8 {
            for key in (0..=255u32).filter(|k| key_allowed(*k)) {
                let c = combo(mods, key);
                if validate_combo(c).is_ok() {
                    valid += 1;
                    assert_eq!(decode_stored(encode(c)), Stored::Combo(c), "{c:?}");
                }
            }
        }
        assert!(valid > 100);
        // unknown modifier bits and modifier-only values are invalid; they run the slot's default
        assert_eq!(decode_stored(0x0010_0020), Stored::Invalid);
        assert_eq!(decode_stored(851968), Stored::Invalid);
        assert_eq!(decode_stored(458752), Stored::Invalid);
        assert!(stored_falls_back(851968) && !stored_falls_back(0) && !stored_falls_back(65568));
        // the new-format bit of the spec's section 2 is not special here: it is just an unknown modifier bit
        assert_eq!(decode_stored(0x4008_0020), Stored::Invalid);
    }

    #[test]
    fn every_dropdown_choice_decodes() {
        for (label, value) in HOTKEY_CHOICES {
            let decoded = decode_stored(value);
            match label {
                "Disabled" => assert_eq!(decoded, Stored::None),
                "Win + Control + Alt" | "Win + Control + Shift" => assert_eq!(decoded, Stored::Invalid, "{label}"),
                _ => {
                    let Stored::Combo(c) = decoded else { panic!("{label} must be a valid combo") };
                    assert_eq!(encode(c), value, "{label}");
                }
            }
        }
        assert_eq!(decode_stored(65632), Stored::Combo(combo(MOD_WIN, VK_TILDE)));
        assert_eq!(decode_stored(458820), Stored::Combo(combo(MOD_WIN | MOD_CTRL | MOD_SHIFT, 0x44)));
    }

    #[test]
    fn validate_combo_boundaries() {
        assert_eq!(validate_combo(combo(MOD_WIN, SPACE)), Ok(()));
        assert_eq!(validate_combo(combo(MOD_CTRL | MOD_ALT, KEY_A)), Ok(()));
        assert_eq!(validate_combo(combo(MOD_SHIFT | MOD_ALT, 0x30)), Ok(()));
        assert_eq!(validate_combo(combo(MOD_WIN, 0x70)), Ok(()));
        assert_eq!(validate_combo(combo(MOD_WIN, 0x7B)), Ok(()));
        assert_eq!(validate_combo(combo(MOD_WIN, 0x7C)), Err(ComboError::BadKey));
        assert_eq!(validate_combo(combo(MOD_WIN, 0x2F)), Err(ComboError::BadKey));
        assert_eq!(validate_combo(combo(MOD_WIN, 0x3A)), Err(ComboError::BadKey));
        assert_eq!(validate_combo(combo(MOD_WIN, 0x40)), Err(ComboError::BadKey));
        assert_eq!(validate_combo(combo(MOD_WIN, 0x5B)), Err(ComboError::BadKey));
        for key in [ESC, 0x09, 0x2E, 0x08, 0x0D, 0] {
            assert_eq!(validate_combo(combo(MOD_WIN, key)), Err(ComboError::BadKey), "{key:#x}");
        }
        assert_eq!(validate_combo(combo(0, SPACE)), Err(ComboError::NoModifier));
        assert_eq!(validate_combo(combo(MOD_CTRL, 0x4B)), Err(ComboError::TooFewModifiers));
        assert_eq!(validate_combo(combo(MOD_ALT, 0x46)), Err(ComboError::TooFewModifiers));
        assert_eq!(validate_combo(combo(MOD_SHIFT, KEY_A)), Err(ComboError::TooFewModifiers));
        assert_eq!(validate_combo(combo(MOD_WIN, VK_L)), Err(ComboError::Reserved));
        assert_eq!(validate_combo(combo(MOD_ALT, VK_F4)), Err(ComboError::Reserved));
        assert_eq!(validate_combo(combo(MOD_WIN | MOD_CTRL, VK_L)), Ok(()));
        assert_eq!(validate_combo(combo(16, SPACE)), Err(ComboError::NoModifier));
    }

    #[test]
    fn toggle_and_hold_must_differ_and_unknown_values_run_the_default() {
        let c = Config::from_stored(true, 327712, 327712);
        assert_eq!(c.toggle, Some(DEFAULT_HOLD));
        assert_eq!(c.hold, None);
        let c = Config::from_stored(true, 0, 0);
        assert!(!c.has_combo());
        let c = Config::from_stored(true, 851968, 458752);
        assert_eq!((c.toggle, c.hold), (Some(DEFAULT_TOGGLE), Some(DEFAULT_HOLD)));
        let c = Config::from_stored(true, 0, 65568);
        assert_eq!((c.toggle, c.hold), (None, Some(DEFAULT_TOGGLE)));
    }

    #[test]
    fn posted_event_numbers_and_the_hook_lost_flag() {
        for e in [EVENT_TOGGLE, EVENT_HOLD_START, EVENT_HOLD_END, EVENT_DISMISS, EVENT_HOOK_LOST] {
            assert_eq!(event_of(e), e);
            assert!(!hook_lost_of(e));
        }
        assert!(hook_lost_of(EVENT_HOOK_LOST | HOOK_LOST_FLAG) && !hook_lost_of(EVENT_HOOK_LOST));
        assert_eq!(event_of(EVENT_HOOK_LOST | HOOK_LOST_FLAG), EVENT_HOOK_LOST);
    }

    #[test]
    fn the_warning_shows_for_a_failed_reinstall_or_a_dead_thread() {
        assert!(!hook_unavailable(false, false));
        assert!(hook_unavailable(true, false));
        assert!(hook_unavailable(false, true));
        assert!(hook_unavailable(true, true));
    }

    #[test]
    fn a_thread_that_start_gave_up_on_installs_no_hook() {
        let shared = Shared {
            config: Mutex::new(default_cfg()),
            peek_visible: AtomicBool::new(false),
            fullscreen: AtomicBool::new(false),
            thread_id: AtomicU32::new(0),
            lost: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        };
        thread_post_quit(&shared); // no thread id yet: harmless
        assert!(!shared.cancelled.load(Ordering::SeqCst));
        // `start` gave up before the thread ran: the thread returns before it installs anything
        shared.cancelled.store(true, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        hook_thread(Arc::new(shared), default_cfg(), tx);
        assert!(rx.try_recv().is_err(), "a cancelled thread neither installs a hook nor reports ready");
        assert!(CTX.with(|c| c.borrow().is_none()));
    }

    #[test]
    fn a_slow_callback_is_flagged_once() {
        let mut e = HotkeyEngine::new(default_cfg());
        e.record_callback(200);
        assert!(!e.take_slow());
        e.record_callback(BUDGET_US + 1);
        assert!(!e.take_slow());
        assert_eq!(e.callbacks_over_budget(), 1);
        e.record_callback(SLOW_US + 1);
        assert!(e.take_slow());
        assert!(!e.take_slow());
        assert_eq!(e.max_callback_us(), SLOW_US + 1);
    }

    mod counting {
        use std::alloc::{GlobalAlloc, Layout, System};
        use std::cell::Cell;

        thread_local! {
            pub static ALLOCS: Cell<usize> = const { Cell::new(0) };
        }

        pub struct Counting;

        unsafe impl GlobalAlloc for Counting {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
                let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
                unsafe { System.alloc(layout) }
            }
            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
                unsafe { System.dealloc(ptr, layout) }
            }
        }
    }

    #[global_allocator]
    static COUNTING: counting::Counting = counting::Counting;

    #[test]
    fn a_million_events_are_fast_and_allocation_free() {
        let pattern = [
            (LWIN, true), (SPACE, true), (SPACE, true), (SPACE, false), (LWIN, false), (LCTRL, true), (KEY_A, true),
            (KEY_A, false), (LCTRL, false), (ESC, true), (ESC, false), (LSHIFT, true), (LSHIFT, false),
        ];
        let mut e = HotkeyEngine::new(default_cfg());
        e.set_peek_visible(true);
        let phys = Phys(KeySet::default());
        let allocs_before = counting::ALLOCS.with(|c| c.get());
        let started = Instant::now();
        let mut consumed = 0u64;
        for i in 0..1_000_000u64 {
            let (vk, down) = pattern[(i % pattern.len() as u64) as usize];
            let o = e.on_key(KeyEvent { vk, scan: 0, down, injected: false, marker: false }, i * 100, &phys);
            consumed += o.consume as u64;
        }
        let elapsed = started.elapsed();
        assert_eq!(counting::ALLOCS.with(|c| c.get()), allocs_before, "the event loop allocates");
        assert!(consumed > 0);
        let limit = if cfg!(debug_assertions) { Duration::from_secs(20) } else { Duration::from_secs(1) };
        assert!(elapsed < limit, "1,000,000 events took {elapsed:?}");
    }
}
