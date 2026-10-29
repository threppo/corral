//! Clear view (quick-hide): the fade state machine, the hide set and its exemptions, the click, idle,
//! show-again and drop-peek rules, and the start-up rule (ranges-dock-clearview-spec 5). Pure logic; the
//! window glue is in `app/quick_hide_glue.rs`. The fade follows motion-sorting-spec 4: one opacity factor
//! per Corral window, 150 ms IN out and 220 ms OUT back, retargeted from the current opacity when toggled
//! again mid-fade.

use crate::motion::{self, Curve, Tween};
use crate::pages::{Button, Mods, PointerEvent, PointerKind, Target};

/// Pacing of the fade frames (`WM_TIMER`, motion-sorting-spec 8).
pub const FRAME_MS: u32 = motion::FRAME_MS as u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Shown,
    Hiding,
    Hidden,
    Showing,
}

impl Phase {
    pub fn name(self) -> &'static str {
        match self {
            Phase::Shown => "shown",
            Phase::Hiding => "hiding",
            Phase::Hidden => "hidden",
            Phase::Showing => "showing",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct State {
    /// Opacity of non-exempt Corrals, 0 (hidden) .. 1 (shown).
    opacity: Tween,
    phase: Phase,
}

impl State {
    pub fn new(hidden: bool) -> State {
        if hidden {
            State { opacity: Tween::at_rest(0.0), phase: Phase::Hidden }
        } else {
            State { opacity: Tween::at_rest(1.0), phase: Phase::Shown }
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The target state is hidden: input follows the target, never the in-flight picture.
    pub fn is_hidden(&self) -> bool {
        matches!(self.phase, Phase::Hiding | Phase::Hidden)
    }

    pub fn animating(&self) -> bool {
        matches!(self.phase, Phase::Hiding | Phase::Showing)
    }

    /// Opacity at monotonic time `now` (milliseconds).
    pub fn opacity_at(&self, now: f64) -> f64 {
        self.opacity.value(now).clamp(0.0, 1.0)
    }

    /// Alpha of non-exempt Corral windows now.
    pub fn alpha(&self) -> u8 {
        self.alpha_at(motion::now_ms())
    }

    pub fn alpha_at(&self, now: f64) -> u8 {
        (self.opacity_at(now) * 255.0).round() as u8
    }

    /// Loose desktop icons belong to Explorer and cannot be faded by Corral: they switch at the
    /// fade edges, hidden once the fade-out has ended and shown from the first frame of the
    /// fade-in (spec 4).
    pub fn loose_alpha(&self) -> u8 {
        if self.phase == Phase::Hidden { 0 } else { 255 }
    }

    /// Start a transition towards `hide`, or retarget a running one from the current opacity. False
    /// when already heading there. Without animation (`NoQuickhideAnimation`, remote session, Windows
    /// animations off) the end state is taken at once.
    pub fn begin(&mut self, hide: bool, animate: bool) -> bool {
        self.begin_at(hide, animate && motion::animations_enabled(), motion::now_ms())
    }

    pub fn begin_at(&mut self, hide: bool, animate: bool, now: f64) -> bool {
        if self.is_hidden() == hide {
            return false;
        }
        let (to, base, curve) = if hide {
            (0.0, motion::CLEAR_VIEW_OUT_MS, Curve::In)
        } else {
            (1.0, motion::CLEAR_VIEW_IN_MS, Curve::Out)
        };
        self.opacity.retarget_scaled(now, to, base, curve, 1.0, animate);
        self.phase = if self.opacity.finished(now) {
            if hide { Phase::Hidden } else { Phase::Shown }
        } else if hide {
            Phase::Hiding
        } else {
            Phase::Showing
        };
        true
    }

    /// Advance one fade frame; true while the picture may have changed.
    pub fn tick(&mut self) -> bool {
        if !motion::animations_enabled() {
            // Windows animations switched off mid-fade: finish at the target (spec 7)
            self.opacity = Tween::at_rest(self.opacity.target());
        }
        self.tick_at(motion::now_ms())
    }

    pub fn tick_at(&mut self, now: f64) -> bool {
        if !self.animating() {
            return false;
        }
        if self.opacity.finished(now) {
            self.phase = if self.phase == Phase::Hiding { Phase::Hidden } else { Phase::Shown };
        }
        true
    }
}

/// May the user act on (press, select, drag, delete, open) an item? An exempt item always; a
/// non-exempt one while the target is "shown": a fading-out Corral is already non-interactive and a
/// fading-in one takes input from its first frame (spec 4).
pub fn actionable(state: &State, exempt: bool) -> bool {
    exempt || !state.is_hidden()
}

/// Alpha an item gets: exempt items keep 255.
pub fn effective_alpha(fade: u8, exempt: bool) -> u8 {
    if exempt { 255 } else { fade }
}

// ---- the hide set and its exemptions (spec 5.1, 5.6) -------------------------------------------------

/// `Settings\Exclusions`: records `<key>|<display>?`. Keys are compared lower-case. Per-Corral "Keep
/// visible" is a `GROUP::<id>` record; a loose icon can be listed by its shell key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exclusions {
    records: Vec<(String, String)>,
}

/// Shell identities of the three icons of a fresh Windows desktop: Recycle Bin, This PC and the user's
/// profile folder (spec 5.6). They stay visible by identity, never by display name.
pub const NEVER_HIDE: [&str; 3] = [
    "::{645ff040-5081-101b-9f08-00aa002f954e}",
    "::{20d04fe0-3aea-1069-a2d8-08002b30309d}",
    "::{59031a47-3f72-44a7-89c5-5595fe6b30ee}",
];

fn plain_path(path: &str) -> String {
    path.replace('/', "\\").trim_end_matches('\\').to_lowercase()
}

/// A loose item on the fixed never-hide list: one of the three identities, or the real folder of the
/// user's profile (`profile_path`, the same item when the desktop shows it as a plain folder).
pub fn never_hide(key: &str, profile_path: &str) -> bool {
    let lower = key.to_lowercase();
    if NEVER_HIDE.contains(&lower.as_str()) {
        return true;
    }
    !profile_path.is_empty() && plain_path(key) == plain_path(profile_path)
}

impl Exclusions {
    pub fn parse(text: &str) -> Exclusions {
        let records = text
            .split('?')
            .filter_map(|record| {
                let (key, display) = record.split_once('|')?;
                (!key.is_empty()).then(|| (key.to_string(), display.to_string()))
            })
            .collect();
        Exclusions { records }
    }

    pub fn serialize(&self) -> String {
        self.records.iter().map(|(k, d)| format!("{k}|{d}?")).collect()
    }

    /// The list when the value is absent: empty. The never-hide icons are a fixed rule ([`never_hide`]),
    /// not stored records.
    pub fn default_list(_profile_path: &str) -> Exclusions {
        Exclusions::default()
    }

    pub fn contains(&self, key: &str) -> bool {
        let key = key.to_lowercase();
        self.records.iter().any(|(k, _)| k.to_lowercase() == key)
    }

    /// The record key written for a group (`GROUP::<id>`, compared lower-case).
    pub fn group_key(id: u64) -> String {
        format!("GROUP::{id}")
    }

    /// Menu command 5: add the `GROUP::<id>|<title>` record, or remove it when present. `|` and `?`
    /// in the title are replaced by a space. True when the group is
    /// excluded afterwards.
    pub fn toggle_group(&mut self, id: u64, title: &str) -> bool {
        let key = Self::group_key(id).to_lowercase();
        if self.contains(&key) {
            self.records.retain(|(k, _)| k.to_lowercase() != key);
            false
        } else {
            let display: String = title.chars().map(|c| if c == '|' || c == '?' { ' ' } else { c }).collect();
            self.records.push((Self::group_key(id), display));
            true
        }
    }

    pub fn contains_group(&self, id: u64) -> bool {
        self.contains(&Self::group_key(id))
    }
}

/// What Clear view hides, from `QuickHideWhat` (0 Corrals and loose icons, 1 loose icons, 2 Corrals).
#[derive(Clone, Copy, Debug, Default)]
pub struct Flags {
    pub exclude_loose_icons: bool,
    pub exclude_corral_groups: bool,
}

/// A tab group is kept when any member tab is marked "Keep visible" (spec 5.1).
pub fn group_kept(flags: Flags, list: &Exclusions, members: impl IntoIterator<Item = u64>) -> bool {
    flags.exclude_corral_groups || members.into_iter().any(|id| list.contains_group(id))
}

/// The same test for a loose icon: outside the hide set, on the never-hide list, or marked.
pub fn loose_icon_exempt(flags: Flags, list: &Exclusions, key: &str, profile_path: &str) -> bool {
    flags.exclude_loose_icons || never_hide(key, profile_path) || list.contains(key)
}

// ---- the foreground window class ---------------------------------------------------------------------

/// What the foreground window is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FgClass {
    /// The desktop: Progman / WorkerW / the desktop window, or one of Corral's own desktop windows
    /// (Corral windows, loose-icon surface, portal views).
    Desktop,
    /// Neither: shell flyouts, menus, the taskbar, Start / search, tool windows, Corral's own dialogs.
    Transient,
    /// A normal application window.
    Normal,
}

/// Classify a foreground window from its class name and flags. `own_desktop` = one of Corral's
/// desktop windows, `own_process` = any window of this process, `visible`/`tool_window` from the
/// window styles (`WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`).
pub fn classify_foreground(class: &str, own_desktop: bool, own_process: bool, visible: bool, tool_window: bool) -> FgClass {
    if own_desktop {
        return FgClass::Desktop;
    }
    if own_process || class.is_empty() || !visible {
        return FgClass::Transient;
    }
    let c = class.to_lowercase();
    if matches!(c.as_str(), "progman" | "workerw" | "shelldll_defview" | "#32769") {
        return FgClass::Desktop;
    }
    const EXACT: [&str; 12] = [
        "#32768",
        "shell_traywnd",
        "shell_secondarytraywnd",
        "traynotifywnd",
        "notifyiconoverflowwindow",
        "toplevelwindowforoverflowxamlisland",
        "windows.ui.core.corewindow",
        "foregroundstaging",
        "multitaskingviewframe",
        "taskswitcherwnd",
        "tooltips_class32",
        "windows.ui.input.inputsite.windowclass",
    ];
    const CONTAINS: [&str; 4] = ["xaml_windowedpopupclass", "xamlexplorerhostislandwindow", "microsoft.ui.content.popupwindowsitebridge", "msctfime"];
    if tool_window || EXACT.contains(&c.as_str()) || CONTAINS.iter().any(|p| c.contains(p)) {
        return FgClass::Transient;
    }
    FgClass::Normal
}

// ---- double-click toggle (spec 5.2) --------------------------------------------------------------------

/// What the engine knows about the system and the moment.
#[derive(Clone, Copy, Debug, Default)]
pub struct ClickEnv {
    /// The common gate of the spec (Corral on, session unlocked, no full-screen app, no menu, no modal
    /// dialog) and `QuickHideEnabled`. Closed: nothing counts and the detector resets.
    pub gate: bool,
    /// `GetDoubleClickTime()`.
    pub double_ms: u64,
    /// Half of the system double-click rectangle (`SM_CXDOUBLECLK`, `SM_CYDOUBLECLK`), px.
    pub double_half: (i32, i32),
    /// The system drag distance (`SM_CXDRAG`, `SM_CYDRAG`), px.
    pub drag: (i32, i32),
    /// A drag of an icon, a Corral or a rectangle is running.
    pub drag_active: bool,
}

/// A completed clean left click on the empty desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClickOut {
    None,
    /// A clean single click (the first of a possible pair).
    Click,
    /// The second clean click of a pair: toggle Clear view.
    Toggle,
}

#[derive(Clone, Copy, Debug)]
struct Press {
    t: u64,
    pos: (i32, i32),
    second: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ClickDetector {
    first: Option<(u64, (i32, i32))>,
    press: Option<Press>,
    /// The click in flight was used for something else (a show on click): it cannot start a pair.
    skip_first: bool,
}

impl ClickDetector {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// A left press is waiting for its release (moves matter only then).
    pub fn tracking(&self) -> bool {
        self.press.is_some()
    }

    /// The last click was used for something else (a show on click): it cannot start a pair, nor can the
    /// click that is in flight right now.
    pub fn forget_pair(&mut self) {
        self.first = None;
        self.skip_first = self.press.is_some();
    }

    pub fn feed(&mut self, ev: PointerEvent, env: &ClickEnv) -> ClickOut {
        if !env.gate {
            self.reset();
            return ClickOut::None;
        }
        match ev.kind {
            PointerKind::Down(button) => {
                let clean = button == Button::Left && ev.mods == Mods::default() && ev.target == Target::EmptyDesktop && !env.drag_active;
                if !clean {
                    self.reset();
                    return ClickOut::None;
                }
                let second = self.first.is_some_and(|(t, pos)| {
                    ev.t_ms.saturating_sub(t) <= env.double_ms
                        && (ev.pos.0 - pos.0).abs() <= env.double_half.0
                        && (ev.pos.1 - pos.1).abs() <= env.double_half.1
                });
                self.skip_first = false;
                self.press = Some(Press { t: ev.t_ms, pos: ev.pos, second });
                ClickOut::None
            }
            PointerKind::Move => {
                if let Some(press) = self.press {
                    if (ev.pos.0 - press.pos.0).abs() >= env.drag.0.max(1) || (ev.pos.1 - press.pos.1).abs() >= env.drag.1.max(1) {
                        self.reset();
                    }
                }
                ClickOut::None
            }
            PointerKind::Up(button) => {
                let Some(press) = self.press.take() else { return ClickOut::None };
                let clean = button == Button::Left && ev.target == Target::EmptyDesktop && ev.mods == Mods::default()
                    && !env.drag_active
                    && (ev.pos.0 - press.pos.0).abs() < env.drag.0.max(1)
                    && (ev.pos.1 - press.pos.1).abs() < env.drag.1.max(1);
                if !clean {
                    self.first = None;
                    return ClickOut::None;
                }
                if press.second {
                    // three quick clicks toggle once: the pair is used up
                    self.first = None;
                    ClickOut::Toggle
                } else {
                    if !std::mem::take(&mut self.skip_first) {
                        self.first = Some((press.t, press.pos));
                    }
                    ClickOut::Click
                }
            }
        }
    }
}

// ---- idle auto-hide (spec 5.3) ---------------------------------------------------------------------------

/// The stored 1 ("Immediately") acts as this: a pause the eye notices.
pub const IDLE_MIN_MS: u32 = 500;
pub const IDLE_MAX_MS: u32 = 3_600_000;

/// The idle threshold from `AutoHideIdleChoice` (milliseconds); `None` when idle hiding is off.
pub fn idle_threshold_ms(stored: u32) -> Option<u32> {
    match stored {
        0 => None,
        1 => Some(IDLE_MIN_MS),
        v => Some(v.clamp(IDLE_MIN_MS, IDLE_MAX_MS)),
    }
}

/// Idle time with suspensions: the time since the last input, but never counted from before the end of a
/// suspension (full-screen use, locked session, Glance, an open menu or dialog, a drag): resuming counts as
/// activity.
#[derive(Clone, Copy, Debug, Default)]
pub struct IdleWatch {
    floor: u64,
}

impl IdleWatch {
    /// Count from `now_ms` again (a suspension ended elsewhere, e.g. Glance closed).
    pub fn restart(&mut self, now_ms: u64) {
        self.floor = now_ms;
    }

    /// True when the desktop has been idle for `threshold_ms`. `since_input_ms` is the system's idle time
    /// (`GetLastInputInfo`), `now_ms` any monotonic clock.
    pub fn due(&mut self, now_ms: u64, since_input_ms: u64, suspended: bool, threshold_ms: u32) -> bool {
        if suspended {
            self.floor = now_ms;
            return false;
        }
        since_input_ms.min(now_ms.saturating_sub(self.floor)) >= u64::from(threshold_ms)
    }
}

// ---- show while dropping (spec 5.5) -----------------------------------------------------------------------

/// How long after a drag ends the hide set fades out again.
pub const PEEK_HIDE_MS: u64 = 800;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeekAct {
    None,
    Show,
    Hide,
}

/// The overlay that shows a hidden desktop while a file is dragged over it. It never changes the
/// underlying state.
#[derive(Clone, Copy, Debug, Default)]
pub struct DropPeek {
    showing: bool,
    ended: Option<u64>,
}

impl DropPeek {
    pub fn is_showing(&self) -> bool {
        self.showing
    }

    /// A hide is waiting for its 800 ms.
    pub fn pending(&self) -> bool {
        self.ended.is_some()
    }

    /// A file drag is over a desktop surface. `allowed` = `AutoShow` and `AutoShowOnDrag` and Clear view on.
    pub fn drag_over(&mut self, hidden: bool, allowed: bool) -> PeekAct {
        self.ended = None;
        if self.showing || !hidden || !allowed {
            return PeekAct::None;
        }
        self.showing = true;
        PeekAct::Show
    }

    /// The drag ended (drop or cancel) at `t_ms`.
    pub fn drag_ended(&mut self, t_ms: u64) {
        if self.showing {
            self.ended = Some(t_ms);
        }
    }

    /// A click intervened: the pending hide is cancelled.
    pub fn cancel_pending(&mut self) {
        self.ended = None;
    }

    pub fn poll(&mut self, t_ms: u64) -> PeekAct {
        match self.ended {
            Some(ended) if self.showing && t_ms.saturating_sub(ended) >= PEEK_HIDE_MS => {
                self.showing = false;
                self.ended = None;
                PeekAct::Hide
            }
            _ => PeekAct::None,
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

// ---- the state of Clear view (spec 5.1, 5.4) ------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hidden {
    /// Hidden by the double-click (persisted).
    Manual,
    /// Hidden by idle time (never persisted).
    Idle,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Machine {
    /// A file drag started over the desktop is still running (its end starts the peek's 800 ms).
    pub drag_live: bool,
    pub kind: Option<Hidden>,
    pub click: ClickDetector,
    pub idle: IdleWatch,
    pub peek: DropPeek,
}

impl Machine {
    pub fn starting(hidden: bool) -> Machine {
        Machine { kind: hidden.then_some(Hidden::Manual), ..Machine::default() }
    }

    pub fn is_hidden(&self) -> bool {
        self.kind.is_some()
    }

    /// The picture should be hidden now: the underlying state, unless a drop peek shows it.
    pub fn want_hidden(&self, enabled: bool) -> bool {
        enabled && self.kind.is_some() && !self.peek.is_showing()
    }

    /// Double-click: `Shown -> Hidden(Manual)`, `Hidden(any) -> Shown`.
    pub fn toggle(&mut self) {
        self.peek.clear();
        self.kind = if self.kind.is_some() { None } else { Some(Hidden::Manual) };
    }

    /// Idle time ran out. Only a shown desktop is hidden; `Hidden(Manual)` is never changed by idle.
    pub fn idle_fired(&mut self) -> bool {
        if self.kind.is_some() {
            return false;
        }
        self.kind = Some(Hidden::Idle);
        true
    }

    /// One left click on the empty desktop, or the desktop coming to the front: `AutoShow` leaves
    /// `Hidden(Idle)` only. The click that does it cannot start a double-click pair.
    pub fn auto_show(&mut self, auto_show_on: bool) -> bool {
        if !auto_show_on || self.kind != Some(Hidden::Idle) {
            return false;
        }
        self.kind = None;
        self.peek.clear();
        self.click.forget_pair();
        true
    }

    /// One clean left click on the empty desktop. While a drop peek waits to hide again, the click keeps the
    /// picture, so the hidden state ends (the desktop must not look shown while `Hidden(Manual)` is stored).
    /// Otherwise `AutoShow` applies. True when the state became shown.
    pub fn click(&mut self, auto_show_on: bool) -> bool {
        if self.peek.pending() {
            return self.end();
        }
        self.auto_show(auto_show_on)
    }

    /// Creating a Corral, a Range switch with `SwipeShowIcons`, or turning Clear view or Corral off.
    pub fn end(&mut self) -> bool {
        self.peek.clear();
        self.kind.take().is_some()
    }
}

/// The stored `AutoShow` value (`QuickHideAutoShowWhenActive`): non-zero switches the show-again rules on;
/// 2 also shows while dropping.
pub fn auto_show_on(stored: u32) -> bool {
    stored != 0
}

pub fn auto_show_on_drag_on(stored: u32) -> bool {
    stored == 2
}

/// Whether the persisted `Hidden(Manual)` is restored at start (spec 5.7). The state is `Shown` if
/// `AutoShowOnBoot` is on, idle hiding is on (the timer hides soon; `AutoShowOnBoot` is then disabled and its
/// stored value untouched), the stored flag is false, or Clear view is off.
pub fn boot_hidden(auto_show_on_boot: bool, idle_on: bool, stored_hidden: bool, enabled: bool) -> bool {
    enabled && stored_hidden && !auto_show_on_boot && !idle_on
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pages::Held;

    #[test]
    fn hide_takes_150_ms_in_and_show_220_ms_out() {
        let mut s = State::new(false);
        assert!(s.begin_at(true, true, 1000.0));
        assert_eq!(s.phase(), Phase::Hiding);
        assert!(s.is_hidden() && s.animating());
        assert_eq!(s.alpha_at(1000.0), 255);
        assert!(s.tick_at(1149.0) && s.phase() == Phase::Hiding);
        assert!(s.tick_at(1150.0));
        assert_eq!((s.phase(), s.alpha_at(1150.0)), (Phase::Hidden, 0));
        assert!(!s.tick_at(1200.0));
        assert!(s.begin_at(false, true, 2000.0));
        assert_eq!(s.phase(), Phase::Showing);
        assert!(!s.is_hidden());
        assert!(s.tick_at(2219.0) && s.phase() == Phase::Showing);
        assert!(s.tick_at(2220.0));
        assert_eq!((s.phase(), s.alpha_at(2220.0)), (Phase::Shown, 255));
    }

    #[test]
    fn the_fade_is_monotone_and_the_return_is_gentler_than_the_exit() {
        let mut s = State::new(false);
        s.begin_at(true, true, 0.0);
        let out: Vec<u8> = (0..=150).map(|ms| s.alpha_at(ms as f64)).collect();
        assert!(out.windows(2).all(|w| w[1] <= w[0]) && out[0] == 255 && out[150] == 0);
        let mut s = State::new(true);
        s.begin_at(false, true, 0.0);
        let back: Vec<u8> = (0..=220).map(|ms| s.alpha_at(ms as f64)).collect();
        assert!(back.windows(2).all(|w| w[1] >= w[0]) && back[0] == 0 && back[220] == 255);
        // IN starts slowly (the Corral lingers a moment), OUT starts fast (the return is felt at once)
        assert!(out[30] > 200 && back[30] > 60);
    }

    #[test]
    fn begin_rejects_only_the_same_target() {
        let mut s = State::new(false);
        assert!(!s.begin_at(false, true, 0.0));
        assert!(s.begin_at(true, true, 0.0));
        assert!(!s.begin_at(true, true, 10.0), "already heading there");
        assert!(s.begin_at(false, true, 10.0), "a second toggle mid-fade retargets");
    }

    #[test]
    fn a_toggle_mid_fade_continues_from_the_current_opacity() {
        for at in [0.0, 75.0, 140.0] {
            let mut s = State::new(false);
            s.begin_at(true, true, 0.0);
            let before = s.opacity_at(at);
            assert!(s.begin_at(false, true, at));
            assert!((s.opacity_at(at) - before).abs() < 1e-6, "no jump at {at}");
            // duration = base x the distance left, one frame at least
            let left = 1.0 - before;
            let expected = (220.0 * left).max(motion::FRAME_MS);
            assert!(s.tick_at(at + expected - 1.0) && s.phase() == Phase::Showing || left == 0.0);
            s.tick_at(at + expected + 0.5);
            assert_eq!(s.phase(), Phase::Shown);
            assert_eq!(s.alpha_at(at + expected + 0.5), 255);
        }
    }

    #[test]
    fn no_animation_jumps() {
        let mut s = State::new(false);
        assert!(s.begin_at(true, false, 0.0));
        assert_eq!((s.phase(), s.alpha_at(0.0)), (Phase::Hidden, 0));
        assert!(s.begin_at(false, false, 1.0));
        assert_eq!((s.phase(), s.alpha_at(1.0)), (Phase::Shown, 255));
    }

    #[test]
    fn input_follows_the_target_and_loose_icons_switch_at_the_edges() {
        let mut s = State::new(false);
        assert!(actionable(&s, false) && s.loose_alpha() == 255);
        s.begin_at(true, true, 0.0);
        assert!(!actionable(&s, false), "a fading-out Corral is already non-interactive");
        assert!(actionable(&s, true), "exempt");
        assert_eq!(s.loose_alpha(), 255, "loose icons stay until the fade-out ends");
        s.tick_at(150.0);
        assert!(!actionable(&s, false) && s.loose_alpha() == 0);
        s.begin_at(false, true, 200.0);
        assert!(actionable(&s, false), "a fading-in Corral takes input from its first frame");
        assert_eq!(s.loose_alpha(), 255, "loose icons are back at the start of the fade-in");
    }

    #[test]
    fn exclusions_parse_roundtrip_and_lookup() {
        let list = Exclusions::parse("::{ABC}|Recycle Bin?GROUP::7|Games?");
        assert!(list.contains("::{abc}"));
        assert!(list.contains_group(7));
        assert!(!list.contains_group(8));
        assert_eq!(list.serialize(), "::{ABC}|Recycle Bin?GROUP::7|Games?");
        assert_eq!(Exclusions::parse("").serialize(), "");
        assert!(!Exclusions::parse("garbage").contains("garbage"));
    }

    #[test]
    fn toggle_group_adds_and_removes_the_record() {
        let mut list = Exclusions::parse("::{abc}|Recycle Bin?");
        assert!(list.toggle_group(7, "Games|x?y"));
        assert_eq!(list.serialize(), "::{abc}|Recycle Bin?GROUP::7|Games x y?");
        assert!(list.contains_group(7));
        assert!(!list.toggle_group(7, "ignored"));
        assert_eq!(list.serialize(), "::{abc}|Recycle Bin?");
        // a lower-case record written by an older build is found and removed too
        let mut old = Exclusions::parse("group::9|Old?");
        assert!(old.contains_group(9));
        assert!(!old.toggle_group(9, "Old"));
        assert_eq!(old.serialize(), "");
    }

    #[test]
    fn foreground_classes() {
        let c = |class: &str| classify_foreground(class, false, false, true, false);
        assert_eq!(c("Notepad"), FgClass::Normal);
        assert_eq!(c("Chrome_WidgetWin_1"), FgClass::Normal);
        assert_eq!(c("Progman"), FgClass::Desktop);
        assert_eq!(c("WorkerW"), FgClass::Desktop);
        for t in ["#32768", "Shell_TrayWnd", "Windows.UI.Core.CoreWindow", "XamlExplorerHostIslandWindow_WASDK", "Xaml_WindowedPopupClass", "Microsoft.UI.Content.PopupWindowSiteBridge"] {
            assert_eq!(c(t), FgClass::Transient, "{t}");
        }
        assert_eq!(classify_foreground("Notepad", false, false, true, true), FgClass::Transient, "tool window");
        assert_eq!(classify_foreground("Notepad", false, false, false, false), FgClass::Transient, "invisible");
        assert_eq!(classify_foreground("Notepad", false, true, true, false), FgClass::Transient, "own dialog");
        assert_eq!(classify_foreground("Whatever", true, true, true, false), FgClass::Desktop, "own desktop window");
    }

    // ---- hide set -------------------------------------------------------------------------------------

    const PROFILE: &str = "C:\\Users\\someone";

    #[test]
    fn the_hide_set_modes_select_corrals_loose_icons_or_both() {
        // QuickHideWhat 0, 1, 2
        let both = Flags::default();
        let loose_only = Flags { exclude_corral_groups: true, exclude_loose_icons: false };
        let corrals_only = Flags { exclude_corral_groups: false, exclude_loose_icons: true };
        let list = Exclusions::default();
        for (flags, corral_hidden, loose_hidden) in [(both, true, true), (loose_only, false, true), (corrals_only, true, false)] {
            assert_eq!(!group_kept(flags, &list, [5]), corral_hidden);
            assert_eq!(!loose_icon_exempt(flags, &list, r"c:\a.txt", PROFILE), loose_hidden);
        }
        // items that appear later are judged by the same rule: nothing is remembered per item
        assert!(!group_kept(both, &list, [99]) && !loose_icon_exempt(both, &list, r"c:\new.txt", PROFILE));
    }

    #[test]
    fn keep_visible_on_any_tab_exempts_the_whole_group() {
        let list = Exclusions::parse("GROUP::12|Tab two?");
        let flags = Flags::default();
        assert!(group_kept(flags, &list, [11, 12, 13]));
        assert!(!group_kept(flags, &list, [11, 13]));
        assert!(group_kept(Flags { exclude_corral_groups: true, ..flags }, &list, [11]));
        assert!(!group_kept(flags, &list, []));
    }

    #[test]
    fn the_three_identities_are_exempt_by_identity_and_never_by_name() {
        let list = Exclusions::default();
        let flags = Flags::default();
        let exempt = |key: &str| loose_icon_exempt(flags, &list, key, PROFILE);
        assert!(exempt("::{645FF040-5081-101B-9F08-00AA002F954E}"));
        assert!(exempt("::{20d04fe0-3aea-1069-a2d8-08002b30309d}"));
        assert!(exempt("::{59031A47-3F72-44A7-89C5-5595FE6B30EE}"));
        // the profile folder as a plain path, with other slashes and a trailing separator
        assert!(exempt("c:/users/SOMEONE/"));
        // a file called "Recycle Bin" is not the Recycle Bin; a renamed one keeps its identity
        assert!(!exempt("C:\\Users\\someone\\Desktop\\Recycle Bin.lnk"));
        assert!(!exempt("C:\\Recycle Bin"));
        // no profile known: only the identities
        assert!(!loose_icon_exempt(flags, &list, "", ""));
        assert!(never_hide("::{645ff040-5081-101b-9f08-00aa002f954e}", ""));
        // a marked icon is exempt too, and marking a Corral does not exempt icons
        let marked = Exclusions::parse("c:\\a.txt|a?GROUP::1|g?");
        assert!(loose_icon_exempt(flags, &marked, "C:\\A.TXT", PROFILE));
        assert!(!loose_icon_exempt(flags, &marked, "c:\\b.txt", PROFILE));
        assert_eq!(effective_alpha(40, true), 255);
        assert_eq!(effective_alpha(40, false), 40);
    }

    #[test]
    fn no_default_records_are_written() {
        assert_eq!(Exclusions::default_list(PROFILE).serialize(), "");
    }

    // ---- double-click toggle ---------------------------------------------------------------------------

    fn env() -> ClickEnv {
        ClickEnv { gate: true, double_ms: 500, double_half: (2, 2), drag: (4, 4), drag_active: false }
    }

    fn ev(t: u64, pos: (i32, i32), kind: PointerKind, target: Target) -> PointerEvent {
        PointerEvent { t_ms: t, pos, kind, held: Held::default(), mods: Mods::default(), target }
    }

    fn click(d: &mut ClickDetector, t: u64, pos: (i32, i32), target: Target, e: &ClickEnv) -> ClickOut {
        d.feed(ev(t, pos, PointerKind::Down(Button::Left), target), e);
        d.feed(ev(t + 30, pos, PointerKind::Up(Button::Left), target), e)
    }

    #[test]
    fn two_clean_clicks_on_the_empty_desktop_toggle_and_three_toggle_once() {
        let e = env();
        let mut d = ClickDetector::default();
        assert_eq!(click(&mut d, 0, (100, 100), Target::EmptyDesktop, &e), ClickOut::Click);
        assert_eq!(click(&mut d, 200, (101, 100), Target::EmptyDesktop, &e), ClickOut::Toggle);
        // the third click starts a new pair
        assert_eq!(click(&mut d, 400, (100, 100), Target::EmptyDesktop, &e), ClickOut::Click);
        assert_eq!(click(&mut d, 600, (100, 100), Target::EmptyDesktop, &e), ClickOut::Toggle);
    }

    #[test]
    fn each_failing_condition_of_the_toggle_gives_no_toggle() {
        let e = env();
        let second = |first: &dyn Fn(&mut ClickDetector, &ClickEnv), t: u64, pos: (i32, i32), e: &ClickEnv| {
            let mut d = ClickDetector::default();
            first(&mut d, e);
            click(&mut d, t, pos, Target::EmptyDesktop, e)
        };
        let plain = |d: &mut ClickDetector, e: &ClickEnv| {
            click(d, 0, (100, 100), Target::EmptyDesktop, e);
        };
        // too slow, outside the double-click rectangle
        assert_eq!(second(&plain, 531, (100, 100), &e), ClickOut::Click);
        assert_eq!(second(&plain, 500 + 30, (100, 100), &e), ClickOut::Click, "measured press to press");
        assert_eq!(second(&plain, 100, (104, 100), &e), ClickOut::Click);
        // right button, middle button
        for button in [Button::Right, Button::Middle] {
            let mut d = ClickDetector::default();
            plain(&mut d, &e);
            d.feed(ev(100, (100, 100), PointerKind::Down(button), Target::EmptyDesktop), &e);
            assert_eq!(d.feed(ev(130, (100, 100), PointerKind::Up(button), Target::EmptyDesktop), &e), ClickOut::None);
            assert_eq!(click(&mut d, 200, (100, 100), Target::EmptyDesktop, &e), ClickOut::Click);
        }
        // modifiers
        for mods in [Mods { ctrl: true, ..Mods::default() }, Mods { shift: true, ..Mods::default() }, Mods { alt: true, ..Mods::default() }, Mods { win: true, ..Mods::default() }] {
            let mut d = ClickDetector::default();
            plain(&mut d, &e);
            let mut down = ev(100, (100, 100), PointerKind::Down(Button::Left), Target::EmptyDesktop);
            down.mods = mods;
            d.feed(down, &e);
            let mut up = ev(130, (100, 100), PointerKind::Up(Button::Left), Target::EmptyDesktop);
            up.mods = mods;
            assert_eq!(d.feed(up, &e), ClickOut::None, "{mods:?}");
        }
        // an icon, a Corral, another window: the pair never counts
        for target in [Target::LooseIcon, Target::Corral, Target::Other] {
            let mut d = ClickDetector::default();
            plain(&mut d, &e);
            assert_eq!(click(&mut d, 100, (100, 100), target, &e), ClickOut::None);
            assert_eq!(click(&mut d, 200, (100, 100), Target::EmptyDesktop, &e), ClickOut::Click, "{target:?}");
        }
        // a release over an icon (the pointer moved there) is no click
        let mut d = ClickDetector::default();
        plain(&mut d, &e);
        d.feed(ev(100, (100, 100), PointerKind::Down(Button::Left), Target::EmptyDesktop), &e);
        assert_eq!(d.feed(ev(130, (100, 100), PointerKind::Up(Button::Left), Target::LooseIcon), &e), ClickOut::None);
    }

    #[test]
    fn movement_a_running_drag_and_a_closed_gate_stop_a_toggle() {
        let e = env();
        // a marquee: the second press moves by the drag distance before the release
        let mut d = ClickDetector::default();
        click(&mut d, 0, (100, 100), Target::EmptyDesktop, &e);
        d.feed(ev(100, (100, 100), PointerKind::Down(Button::Left), Target::EmptyDesktop), &e);
        d.feed(ev(110, (104, 100), PointerKind::Move, Target::EmptyDesktop), &e);
        assert_eq!(d.feed(ev(130, (104, 100), PointerKind::Up(Button::Left), Target::EmptyDesktop), &e), ClickOut::None);
        // a drawn gesture (creation-spec 2.2) is no click: the quick click after it starts a new pair
        let mut d = ClickDetector::default();
        d.feed(ev(0, (100, 100), PointerKind::Down(Button::Left), Target::EmptyDesktop), &e);
        d.feed(ev(10, (140, 100), PointerKind::Move, Target::EmptyDesktop), &e);
        d.feed(ev(20, (100, 100), PointerKind::Move, Target::EmptyDesktop), &e);
        assert_eq!(d.feed(ev(30, (100, 100), PointerKind::Up(Button::Left), Target::EmptyDesktop), &e), ClickOut::None);
        assert_eq!(click(&mut d, 100, (100, 100), Target::EmptyDesktop, &e), ClickOut::Click);
        assert_eq!(click(&mut d, 200, (100, 100), Target::EmptyDesktop, &e), ClickOut::Toggle);
        // less than the distance is still a click
        let mut d = ClickDetector::default();
        click(&mut d, 0, (100, 100), Target::EmptyDesktop, &e);
        d.feed(ev(100, (100, 100), PointerKind::Down(Button::Left), Target::EmptyDesktop), &e);
        d.feed(ev(110, (103, 100), PointerKind::Move, Target::EmptyDesktop), &e);
        assert_eq!(d.feed(ev(130, (103, 100), PointerKind::Up(Button::Left), Target::EmptyDesktop), &e), ClickOut::Toggle);
        // a drag in progress, a closed gate (menu, dialog, full-screen, disabled)
        let busy = ClickEnv { drag_active: true, ..e };
        let closed = ClickEnv { gate: false, ..e };
        for env in [busy, closed] {
            let mut d = ClickDetector::default();
            assert_eq!(click(&mut d, 0, (100, 100), Target::EmptyDesktop, &env), ClickOut::None);
            assert_eq!(click(&mut d, 100, (100, 100), Target::EmptyDesktop, &env), ClickOut::None);
        }
        // a gate that closes between the clicks forgets the first
        let mut d = ClickDetector::default();
        click(&mut d, 0, (100, 100), Target::EmptyDesktop, &e);
        click(&mut d, 50, (100, 100), Target::EmptyDesktop, &closed);
        assert_eq!(click(&mut d, 100, (100, 100), Target::EmptyDesktop, &e), ClickOut::Click);
    }

    // ---- idle ------------------------------------------------------------------------------------------

    #[test]
    fn the_idle_threshold_follows_the_choice_and_clamps() {
        assert_eq!(idle_threshold_ms(0), None);
        assert_eq!(idle_threshold_ms(1), Some(500), "the stored 1 acts as 500 ms");
        assert_eq!(idle_threshold_ms(499), Some(500));
        assert_eq!(idle_threshold_ms(500), Some(500));
        assert_eq!(idle_threshold_ms(30_000), Some(30_000));
        assert_eq!(idle_threshold_ms(120_000), Some(120_000));
        assert_eq!(idle_threshold_ms(9_999_999), Some(3_600_000));
        assert_eq!(idle_threshold_ms(45_000), Some(45_000), "unknown values stay as they are");
    }

    #[test]
    fn idle_time_counts_from_the_last_input_and_from_the_end_of_a_suspension() {
        let mut w = IdleWatch::default();
        assert!(!w.due(100_000, 29_999, false, 30_000));
        assert!(w.due(100_001, 30_000, false, 30_000));
        // input resets the system's idle time
        assert!(!w.due(100_500, 10, false, 30_000));
        // a suspension restarts the count from its end, even if the system idle time is large
        let mut w = IdleWatch::default();
        assert!(!w.due(200_000, 90_000, true, 30_000));
        assert!(!w.due(210_000, 100_000, true, 30_000));
        assert!(!w.due(220_000, 110_000, false, 30_000), "resumed just now");
        assert!(!w.due(239_999, 129_999, false, 30_000));
        assert!(w.due(250_000, 140_000, false, 30_000));
    }

    #[test]
    fn idle_hides_a_shown_desktop_only_and_never_a_manual_one() {
        let mut m = Machine::default();
        assert!(m.idle_fired());
        assert_eq!(m.kind, Some(Hidden::Idle));
        let mut manual = Machine::starting(true);
        assert_eq!(manual.kind, Some(Hidden::Manual));
        assert!(!manual.idle_fired());
        assert_eq!(manual.kind, Some(Hidden::Manual));
    }

    // ---- showing again ---------------------------------------------------------------------------------

    #[test]
    fn auto_show_leaves_hidden_idle_only_and_the_paired_second_click_cannot_hide_again() {
        let e = env();
        let mut m = Machine::default();
        m.idle_fired();
        // first click of a double-click: shown, and it does not start the pair
        assert_eq!(click(&mut m.click, 0, (100, 100), Target::EmptyDesktop, &e), ClickOut::Click);
        assert!(m.auto_show(true));
        assert!(!m.is_hidden());
        // the second click is a plain first click, not a toggle
        assert_eq!(click(&mut m.click, 200, (100, 100), Target::EmptyDesktop, &e), ClickOut::Click);
        assert!(!m.is_hidden());
        // never from Hidden(Manual), and not with AutoShow off
        let mut manual = Machine::starting(true);
        assert!(!manual.auto_show(true));
        let mut idle = Machine::default();
        idle.idle_fired();
        assert!(!idle.auto_show(false));
        assert_eq!(idle.kind, Some(Hidden::Idle), "only the double-click leaves it");
        idle.toggle();
        assert!(!idle.is_hidden());
    }

    #[test]
    fn a_show_on_click_while_the_press_is_in_flight_cannot_start_a_pair() {
        let e = env();
        let mut d = ClickDetector::default();
        d.feed(ev(0, (100, 100), PointerKind::Down(Button::Left), Target::EmptyDesktop), &e);
        d.forget_pair();
        assert_eq!(d.feed(ev(30, (100, 100), PointerKind::Up(Button::Left), Target::EmptyDesktop), &e), ClickOut::Click);
        assert_eq!(click(&mut d, 200, (100, 100), Target::EmptyDesktop, &e), ClickOut::Click, "the first click was not recorded");
        assert_eq!(click(&mut d, 400, (100, 100), Target::EmptyDesktop, &e), ClickOut::Toggle);
    }

    #[test]
    fn creation_a_range_switch_or_a_master_off_end_the_hidden_state() {
        let mut m = Machine::starting(true);
        assert!(m.end());
        assert!(!m.is_hidden());
        assert!(!m.end());
        let mut idle = Machine::default();
        idle.idle_fired();
        assert!(idle.end());
        // the master off keeps nothing hidden whatever the state says
        assert!(!Machine::starting(true).want_hidden(false));
        assert!(Machine::starting(true).want_hidden(true));
    }

    #[test]
    fn a_click_during_the_peek_ends_the_hidden_state_so_the_picture_and_the_stored_flag_agree() {
        let mut m = Machine::starting(true);
        m.peek.drag_over(true, true);
        m.peek.drag_ended(0);
        assert!(m.click(false), "even with AutoShow off, and from Hidden(Manual)");
        assert!(!m.is_hidden() && !m.peek.is_showing());
        // without a pending hide the click is plain AutoShow: Manual stays hidden
        let mut manual = Machine::starting(true);
        assert!(!manual.click(true));
        assert!(manual.is_hidden());
        let mut idle = Machine::default();
        idle.idle_fired();
        assert!(idle.click(true));
    }

    #[test]
    fn each_range_switch_ends_clear_view_once() {
        let mut m = Machine::starting(true);
        assert!(m.end(), "the first switch shows");
        assert!(!m.end(), "later switches find nothing to end");
        m.toggle();
        assert!(m.is_hidden() && m.end(), "hidden again by a later double-click: the next switch ends it again");
    }

    #[test]
    fn the_stored_auto_show_value_selects_the_rules() {
        assert!(!auto_show_on(0) && auto_show_on(1) && auto_show_on(2));
        assert!(!auto_show_on_drag_on(0) && !auto_show_on_drag_on(1) && auto_show_on_drag_on(2));
    }

    // ---- show while dropping ---------------------------------------------------------------------------

    #[test]
    fn a_drag_over_the_desktop_shows_it_and_it_hides_800_ms_after_the_end() {
        let mut p = DropPeek::default();
        assert_eq!(p.drag_over(true, true), PeekAct::Show);
        assert!(p.is_showing());
        // staying shown while the drag lasts and the pointer leaves
        assert_eq!(p.poll(10_000), PeekAct::None);
        assert_eq!(p.drag_over(true, true), PeekAct::None);
        p.drag_ended(20_000);
        assert_eq!(p.poll(20_799), PeekAct::None);
        assert_eq!(p.poll(20_800), PeekAct::Hide);
        assert!(!p.is_showing());
        assert_eq!(p.poll(30_000), PeekAct::None);
    }

    #[test]
    fn a_new_drag_or_a_click_cancels_the_pending_hide_and_without_auto_show_it_is_inert() {
        let mut p = DropPeek::default();
        p.drag_over(true, true);
        p.drag_ended(0);
        assert_eq!(p.drag_over(true, true), PeekAct::None, "new drag");
        assert_eq!(p.poll(5000), PeekAct::None);
        p.drag_ended(6000);
        p.cancel_pending();
        assert_eq!(p.poll(9000), PeekAct::None, "a click kept the picture");
        assert!(p.is_showing());
        // inert: not hidden, or AutoShow / AutoShowOnDrag not both on
        let mut q = DropPeek::default();
        assert_eq!(q.drag_over(false, true), PeekAct::None);
        assert_eq!(q.drag_over(true, false), PeekAct::None);
        assert!(!q.is_showing());
        q.drag_ended(0);
        assert_eq!(q.poll(100_000), PeekAct::None);
    }

    #[test]
    fn the_peek_leaves_the_underlying_state_alone() {
        let mut m = Machine::starting(true);
        let hidden = m.is_hidden();
        assert_eq!(m.peek.drag_over(hidden, true), PeekAct::Show);
        assert!(m.is_hidden(), "still Hidden(Manual) underneath");
        assert!(!m.want_hidden(true), "the picture is shown meanwhile");
        m.peek.drag_ended(0);
        assert_eq!(m.peek.poll(800), PeekAct::Hide);
        assert!(m.want_hidden(true));
        // a toggle ends the peek with the state change
        m.peek.drag_over(true, true);
        m.toggle();
        assert!(!m.peek.is_showing() && !m.is_hidden());
    }

    // ---- start-up --------------------------------------------------------------------------------------

    #[test]
    fn start_up_restores_hidden_manual_only_when_nothing_argues_against_it() {
        for auto_show_on_boot in [false, true] {
            for idle_on in [false, true] {
                for stored_hidden in [false, true] {
                    for enabled in [false, true] {
                        let want = enabled && stored_hidden && !auto_show_on_boot && !idle_on;
                        assert_eq!(boot_hidden(auto_show_on_boot, idle_on, stored_hidden, enabled), want,
                            "boot={auto_show_on_boot} idle={idle_on} stored={stored_hidden} enabled={enabled}");
                    }
                }
            }
        }
        assert!(boot_hidden(false, false, true, true));
        assert!(!boot_hidden(false, false, true, false));
        // Hidden(Idle) is never what is restored: the machine starts Manual or Shown
        assert_eq!(Machine::starting(true).kind, Some(Hidden::Manual));
        assert_eq!(Machine::starting(false).kind, None);
    }
}
