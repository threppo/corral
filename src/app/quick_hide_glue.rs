//! Clear view window glue: applies the fade of `crate::quick_hide` to the group windows and the loose icons,
//! feeds it the pointer, idle and drag input, and keeps the persisted flag (ranges-dock-clearview-spec 5).

use super::{App, Drag, Item, log, tick_ms};
use crate::pages::PointerEvent;
use crate::quick_hide::{self, ClickEnv, ClickOut, Exclusions, FgClass, Flags, Hidden, Machine, PeekAct, Phase};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, GetDoubleClickTime, GetLastInputInfo, LASTINPUTINFO, VK_LBUTTON, VK_RBUTTON};
use crate::render::{Align, Ctx, RectF, Rgba, TextStyle, present_layered};
use crate::settings as settings_store;
use crate::style;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Fade frame timer (`quick_hide::FRAME_MS`; one coordinator timer on the engine's message window).
pub(super) const TIMER_QH_FRAME: usize = 0x5101;
/// One-shot after start: present a restored hidden state.
pub(super) const TIMER_QH_BOOT: usize = 0x5102;

/// A foreground change or an OLE drag entering a Corral window was noticed (see [`FgHook`]).
pub(super) const TIMER_QH_FG: usize = 0x5103;
/// Slow poll: idle time, and a Clear view that was switched off.
pub(super) const TIMER_QH_IDLE: usize = 0x5104;
/// Poll of the drop peek: the end of the drag and the 800 ms after it.
pub(super) const TIMER_QH_SHOW: usize = 0x5105;
const IDLE_POLL_MS: u32 = 250;
const SHOW_POLL_MS: u32 = 50;
const BOOT_APPLY_MS: u32 = 300;
const EVENT_SYSTEM_FOREGROUND: u32 = 0x0003;
const WINEVENT_OUTOFCONTEXT: u32 = 0;

pub(super) fn is_timer(id: usize) -> bool {
    matches!(id, TIMER_QH_FRAME | TIMER_QH_BOOT | TIMER_QH_FG | TIMER_QH_IDLE | TIMER_QH_SHOW)
}

static MSG_WND: AtomicIsize = AtomicIsize::new(0);
static FG_HWND: AtomicIsize = AtomicIsize::new(0);
static OLE_ENTER: AtomicBool = AtomicBool::new(false);

/// `SetWinEventHook(EVENT_SYSTEM_FOREGROUND, WINEVENT_OUTOFCONTEXT)`: no DLL is injected anywhere;
/// the callback runs on the installing thread inside its message loop and only records the window
/// and arms a 1 ms timer on the message window, so all work happens in `quick_hide_timer`.
pub(super) struct FgHook(HWINEVENTHOOK);

impl Drop for FgHook {
    fn drop(&mut self) {
        unsafe {
            let _ = UnhookWinEvent(self.0);
        }
    }
}

unsafe extern "system" fn fg_proc(_hook: HWINEVENTHOOK, event: u32, hwnd: HWND, id_object: i32, _id_child: i32, _thread: u32, time: u32) {
    // show-desktop raises/lowers Progman and activates it or the restored app: restack at once, not
    // from the 1 ms timer below (`showdesk.rs`)
    super::showdesk::note(event, hwnd, id_object, time);
    FG_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
    super::taskbar_item::note_foreground(hwnd);
    wake_fg_timer();
}

fn wake_fg_timer() {
    let msg = MSG_WND.load(Ordering::Relaxed);
    if msg != 0 {
        unsafe {
            SetTimer(HWND(msg as *mut core::ffi::c_void), TIMER_QH_FG, 1, None);
        }
    }
}

/// An OLE drag entered one of Corral's drop targets (called from the drop-target callback; only
/// sets a flag and wakes the message window, so it is safe while the app is borrowed).
pub(super) fn note_ole_drag_enter() {
    OLE_ENTER.store(true, Ordering::Relaxed);
    wake_fg_timer();
}

fn buttons_down() -> bool {
    unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 | GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16) & 0x8000 != 0 }
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetTickCount() -> u32;
}

/// Milliseconds since the last keyboard or mouse input anywhere in the session (`GetLastInputInfo`).
fn system_idle_ms() -> u64 {
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return 0;
    }
    u64::from(unsafe { GetTickCount() }.wrapping_sub(info.dwTime))
}

fn qa_quick_hide() -> bool {
    std::env::var("CORRAL_QA_QUICK_HIDE").is_ok_and(|v| v == "1")
}

impl App {
    fn qh_flags(&self) -> Flags {
        Flags {
            exclude_loose_icons: self.settings.quick_hide_exclude_loose_icons,
            exclude_corral_groups: self.settings.quick_hide_exclude_corral_groups,
        }
    }

    fn qh_profile() -> String {
        std::env::var("USERPROFILE").unwrap_or_default()
    }

    fn qh_exclusions(&self) -> Exclusions {
        match &self.settings.quick_hide_exclusions {
            Some(text) => Exclusions::parse(text),
            None => Exclusions::default_list(&Self::qh_profile()),
        }
    }

    /// The ids of the tabs that share a window with `id` (just `id` for an ordinary Corral).
    fn qh_members(&self, id: u64) -> Vec<u64> {
        crate::tabs::collect(&self.layout.groups, id).map_or_else(|| vec![id], |v| v.members)
    }

    /// "Keep visible" is set on the Corral, or on any tab of its group (spec 5.1, tabs-spec 2.5); the menu
    /// check follows.
    pub(super) fn qh_group_excluded(&self, id: u64) -> bool {
        let list = self.qh_exclusions();
        crate::tabs::kept_in_clear_view(&self.qh_members(id), |m| list.contains_group(m))
    }

    /// Menu command 5: toggle `GROUP::<id>|<title>` in `Settings\Exclusions`,
    /// persist it and re-apply the fade at once.
    pub(super) fn qh_toggle_group_exclusion(&mut self, id: u64) {
        let Some(title) = self.layout.groups.iter().find(|g| g.id == id).map(|g| g.title.clone()) else { return };
        let path = self.opts.settings_path.clone();
        let mut stored = settings_store::load_with_backup(&path);
        let mut list = match &stored.quick_hide_exclusions {
            Some(text) => Exclusions::parse(text),
            None => Exclusions::default_list(&Self::qh_profile()),
        };
        let excluded = list.toggle_group(id, &title);
        let text = list.serialize();
        stored.quick_hide_exclusions = Some(text.clone());
        match settings_store::save(&path, &stored) {
            Ok(()) => self.settings_stamp = self.settings_file_stamp(),
            Err(e) => log(&format!("quick-hide: persisting Exclusions failed: {e}")),
        }
        log(&format!("quick-hide: group {id} {} (Exclusions={text})", if excluded { "excluded" } else { "included again" }));
        self.settings.quick_hide_exclusions = Some(text);
        self.quick_hide_apply();
    }

    fn qh_group_exempt(&self, id: u64) -> bool {
        quick_hide::group_kept(self.qh_flags(), &self.qh_exclusions(), self.qh_members(id))
    }

    fn qh_loose_exempt(&self, name: &str) -> bool {
        quick_hide::loose_icon_exempt(self.qh_flags(), &self.qh_exclusions(), name, &Self::qh_profile())
    }

    /// Alpha of a group window under the quick-hide fade (255 when exempt or shown).
    pub(super) fn qh_group_alpha(&self, id: u64) -> u8 {
        if self.peek.is_on() {
            return 255; // Peek shows hidden groups; the quick-hide state itself is untouched
        }
        quick_hide::effective_alpha(self.quick_hide.alpha(), self.qh_group_exempt(id))
    }

    /// Alpha of a loose icon under the quick-hide fade.
    pub(super) fn qh_loose_alpha(&self, item: &Item) -> u8 {
        // loose icons are Explorer's: they switch at the fade edges instead of fading (motion-sorting-spec 4)
        quick_hide::effective_alpha(self.quick_hide.loose_alpha(), self.qh_loose_exempt(&item.shell.parsing_name))
    }

    /// The item `name` is invisible or fading under quick-hide: it must not be pressed, selected,
    /// dragged, opened, renamed or deleted (members follow their group's exemption).
    pub(super) fn qh_name_hidden(&self, name: &str) -> bool {
        if self.peek.is_on() && self.group_of(name).is_some() {
            return false; // members of the groups Peek shows are actionable
        }
        let exempt = match self.group_of(name) {
            Some(gid) => self.qh_group_exempt(gid),
            None => self.qh_loose_exempt(name),
        };
        !quick_hide::actionable(&self.quick_hide, exempt)
    }

    /// `draw_cell` with an overall alpha (fading loose icons).
    pub(super) fn draw_cell_alpha(&self, ctx: &Ctx, item: &Item, x: f32, y: f32, cell: (i32, i32), alpha: u8) -> windows::core::Result<()> {
        if alpha == 255 {
            return self.draw_cell(ctx, item, x, y, cell);
        }
        let dpi = self.dpi();
        let (cw, ch) = (cell.0 as f32, cell.1 as f32);
        let icon = self.icon_px as f32;
        let opacity = alpha as f32 / 255.0;
        if let Some(bitmap) = &item.icon {
            let left = x + (cw - icon) / 2.0;
            let top = y + 4.0 * dpi as f32 / 96.0;
            ctx.draw_bitmap(&bitmap.bgra_premultiplied, bitmap.width, bitmap.height, RectF::new(left, top, left + icon, top + icon), opacity)?;
        }
        let label_top = y + icon + 8.0 * dpi as f32 / 96.0;
        let label_rect = RectF::new(x + 2.0, label_top, x + cw - 2.0, y + ch);
        // same style as `App::label_style`, colours scaled by the fade
        let text = TextStyle {
            family: style::FONT,
            size_px: style::LABEL_FONT_PX * dpi as f32 / 96.0,
            weight: 400,
            color: Self::qh_scaled(style::LABEL_TEXT, alpha),
            align: Align::Center,
            shadow: Some((Self::qh_scaled(style::LABEL_SHADOW, alpha), 1.0, 1.0)),
        };
        ctx.draw_label(&item.shell.display_name, label_rect, &text, if item.selected { 6 } else { 2 }).map(|_| ())
    }

    /// Start-up (spec 5.7): the persisted `Hidden(Manual)` is restored without a fade unless `AutoShowOnBoot`,
    /// idle hiding or a switched-off Clear view says otherwise; `Hidden(Idle)` is never restored. Then the
    /// poll timers and the foreground hook start.
    pub(super) fn quick_hide_init(&mut self) {
        let s = &self.settings;
        let idle_on = s.quick_hide_enabled && s.quick_hide_auto_hide_on_idle > 0;
        let hidden = quick_hide::boot_hidden(s.quick_hide_auto_show_on_boot, idle_on, s.quick_hide_active, s.quick_hide_enabled);
        self.qh = Machine::starting(hidden);
        self.qh.idle.restart(tick_ms());
        if hidden {
            self.quick_hide = quick_hide::State::new(true);
        } else if s.quick_hide_active {
            // started shown although the flag was set: the stored choice is dropped
            self.quick_hide_persist(false);
        }
        MSG_WND.store(self.message_window.0 as isize, Ordering::Relaxed);
        let hook = unsafe { SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, None, Some(fg_proc), 0, 0, WINEVENT_OUTOFCONTEXT) };
        self.qh_fg_hook = (!hook.is_invalid()).then_some(FgHook(hook));
        unsafe {
            SetTimer(self.message_window, TIMER_QH_IDLE, IDLE_POLL_MS, None);
            if hidden || qa_quick_hide() {
                SetTimer(self.message_window, TIMER_QH_BOOT, if qa_quick_hide() { 2000 } else { BOOT_APPLY_MS }, None);
            }
        }
    }

    /// Timer messages of the module; true when `id` was one of them.
    pub(super) fn quick_hide_timer(&mut self, id: usize) -> bool {
        match id {
            TIMER_QH_BOOT => {
                self.kill_qh_timer(TIMER_QH_BOOT);
                if qa_quick_hide() {
                    log("quick-hide: QA toggle");
                    self.qh.toggle();
                    self.qh_sync();
                } else {
                    self.quick_hide_apply();
                }
                true
            }
            TIMER_QH_FRAME => {
                if self.quick_hide.tick() {
                    log(&format!("quick-hide: {} alpha={}", self.quick_hide.phase().name(), self.quick_hide.alpha()));
                    self.quick_hide_apply();
                }
                if !self.quick_hide.animating() {
                    self.kill_qh_timer(TIMER_QH_FRAME);
                    self.quick_hide_log_summary();
                }
                true
            }
            TIMER_QH_FG => {
                self.kill_qh_timer(TIMER_QH_FG);
                // every foreground change: Camouflage's "full color when the desktop is in use" follows it at once
                self.sync_desktop_active();
                self.quick_hide_foreground_changed();
                true
            }
            TIMER_QH_IDLE => {
                self.quick_hide_poll();
                true
            }
            TIMER_QH_SHOW => {
                self.quick_hide_peek_poll();
                true
            }
            _ => false,
        }
    }

    fn kill_qh_timer(&self, id: usize) {
        unsafe {
            let _ = KillTimer(self.message_window, id);
        }
    }

    fn qh_classify(&self, hwnd: HWND) -> FgClass {
        if hwnd.0.is_null() {
            return FgClass::Transient;
        }
        let own_desktop = hwnd == self.surface_hwnd
            || hwnd == self.parent
            || self.preview_host == Some(hwnd)
            || self.groups.iter().any(|g| g.hwnd == hwnd);
        let mut pid = 0u32;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        let own_process = pid == unsafe { GetCurrentProcessId() };
        let mut buf = [0u16; 256];
        let n = unsafe { GetClassNameW(hwnd, &mut buf) }.max(0) as usize;
        let class = String::from_utf16_lossy(&buf[..n]);
        let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
        let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
        let tool = ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0;
        quick_hide::classify_foreground(&class, own_desktop, own_process, visible, tool)
    }

    // ---- the state machine and the fade -----------------------------------------------------------------

    /// Bring the fade in line with the state: hidden when `Hidden(..)` and not peeking, shown otherwise.
    /// `Hidden(Manual)` is the only state written to the settings.
    pub(super) fn qh_sync(&mut self) {
        // a running Range transition ends where it was going (a carry keeps its Range)
        self.page_finish_motion();
        let hide = self.qh.want_hidden(self.settings.quick_hide_enabled);
        let animate = !self.settings.no_quick_hide_animation && unsafe { GetSystemMetrics(SM_REMOTESESSION) } == 0;
        let from = self.quick_hide.phase();
        if self.quick_hide.begin(hide, animate) {
            log(&format!("quick-hide: {} -> {}", from.name(), self.quick_hide.phase().name()));
            if self.quick_hide.animating() {
                unsafe {
                    SetTimer(self.message_window, TIMER_QH_FRAME, quick_hide::FRAME_MS, None);
                }
                self.quick_hide_apply();
            } else {
                self.quick_hide_apply();
                self.quick_hide_log_summary();
            }
        }
        self.quick_hide_persist(self.qh.kind == Some(Hidden::Manual));
    }

    /// Creating a Corral or Gate, a Range switch with `SwipeShowIcons`, or Clear view turned off: any hidden
    /// state ends first (spec 5.4).
    pub(super) fn qh_end_hidden(&mut self) {
        if self.qh.end() || self.quick_hide.is_hidden() {
            self.qh_sync();
        }
    }

    /// The pointer events of the desktop: the double-click toggle and the click that shows again.
    pub(super) fn qh_pointer(&mut self, ev: PointerEvent) {
        let enabled = self.settings.quick_hide_enabled;
        if !enabled && !self.qh.is_hidden() { return; }
        // a move matters only to a press that is waiting for its release
        if ev.kind == crate::pages::PointerKind::Move && !self.qh.click.tracking() { return; }
        let env = ClickEnv {
            gate: enabled && self.desk_gate(),
            double_ms: u64::from(unsafe { GetDoubleClickTime() }),
            double_half: unsafe { (GetSystemMetrics(SM_CXDOUBLECLK) / 2, GetSystemMetrics(SM_CYDOUBLECLK) / 2) },
            drag: unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) },
            drag_active: matches!(self.drag, Drag::MoveGroup { .. } | Drag::ResizeGroup { .. } | Drag::Item { active: true, .. })
                || self.ole_drag.is_some(),
        };
        match self.qh.click.feed(ev, &env) {
            ClickOut::Toggle => {
                log("quick-hide: double-click toggle");
                self.qh.toggle();
                self.qh_sync();
            }
            ClickOut::Click => {
                if self.qh.click(quick_hide::auto_show_on(self.settings.quick_hide_auto_show_when_active)) {
                    log("quick-hide: click on the desktop -> auto-show");
                    self.qh_sync();
                }
            }
            ClickOut::None => {}
        }
    }

    /// The recorded foreground window changed, or an OLE drag entered a Corral window.
    fn quick_hide_foreground_changed(&mut self) {
        // a fullscreen app in front ends Glance and makes the shortcuts pass (peek-hotkeys-spec 7)
        self.peek_fullscreen_refresh();
        if self.peek.is_on() {
            // Peek owns the foreground meanwhile: no auto-show (Peek's own activation of the surface
            // would read as "desktop activated"); a toggled Peek ends on another application
            OLE_ENTER.store(false, Ordering::Relaxed);
            let hwnd = HWND(FG_HWND.swap(0, Ordering::Relaxed) as *mut core::ffi::c_void);
            self.peek_foreground_changed(hwnd);
            return;
        }
        let enabled = self.settings.quick_hide_enabled;
        let mode = self.settings.quick_hide_auto_show_when_active;
        if OLE_ENTER.swap(false, Ordering::Relaxed) && enabled {
            self.qh_drop_over(mode);
        }
        let hwnd = HWND(FG_HWND.swap(0, Ordering::Relaxed) as *mut core::ffi::c_void);
        if hwnd.0.is_null() {
            return;
        }
        let class = self.qh_classify(hwnd);
        log(&format!("quick-hide: foreground {:#x} is {class:?} (auto-show {mode}, hidden {:?})", hwnd.0 as isize, self.qh.kind));
        // the desktop coming to the front (Win+D, the taskbar corner) shows again from Hidden(Idle)
        if class == FgClass::Desktop && enabled && self.qh.auto_show(quick_hide::auto_show_on(mode)) {
            log("quick-hide: desktop activated -> auto-show");
            self.qh_sync();
        }
    }

    /// A file drag is over a desktop surface: a hidden desktop shows meanwhile (spec 5.5).
    fn qh_drop_over(&mut self, mode: u32) {
        let allowed = quick_hide::auto_show_on(mode) && quick_hide::auto_show_on_drag_on(mode);
        let hidden = self.qh.is_hidden();
        let act = self.qh.peek.drag_over(hidden, allowed);
        self.qh.drag_live = true;
        if act == PeekAct::Show {
            log("quick-hide: file drag over the desktop -> show");
            self.qh_sync();
        }
        unsafe {
            SetTimer(self.message_window, TIMER_QH_SHOW, SHOW_POLL_MS, None);
        }
    }

    /// The end of the drag, and the hide 800 ms later.
    fn quick_hide_peek_poll(&mut self) {
        if self.qh.drag_live && !buttons_down() && self.ole_drag.is_none() {
            self.qh.drag_live = false;
            self.qh.peek.drag_ended(tick_ms());
        }
        if self.qh.peek.poll(tick_ms()) == PeekAct::Hide {
            log("quick-hide: 800 ms after the drop -> hide again");
            self.qh_sync();
        }
        if !self.qh.drag_live && !self.qh.peek.pending() {
            self.kill_qh_timer(TIMER_QH_SHOW);
        }
    }

    /// Glance was opened: idle hiding is suspended (it restarts when Glance ends).
    pub(super) fn qh_peek_suspend_idle(&mut self) {
        self.qh.idle.restart(tick_ms());
    }

    /// Glance ended: the idle count starts again.
    pub(super) fn qh_peek_resume(&mut self) {
        self.qh.idle.restart(tick_ms());
    }

    /// A foreground event recorded before Peek started must not be read as a dismissal.
    pub(super) fn qh_discard_pending_foreground(&self) {
        FG_HWND.store(0, Ordering::Relaxed);
        OLE_ENTER.store(false, Ordering::Relaxed);
        self.kill_qh_timer(TIMER_QH_FG);
    }

    /// Idle auto-hide is suspended during full-screen use, a locked session, Glance, an open menu or dialog
    /// and any drag (spec 5.3).
    fn qh_idle_suspended(&self) -> bool {
        self.peek.is_on() || !self.desk_gate_open() || !matches!(self.drag, Drag::None) || self.ole_drag.is_some()
            || self.quick_hide.animating()
    }

    /// Every 250 ms: Clear view switched off ends any hidden state; idle time hides a shown desktop.
    fn quick_hide_poll(&mut self) {
        if !self.settings.quick_hide_enabled {
            if self.qh.is_hidden() || self.quick_hide.is_hidden() {
                self.qh.end();
                self.qh_sync();
            }
            return;
        }
        let Some(threshold) = quick_hide::idle_threshold_ms(self.settings.quick_hide_auto_hide_on_idle) else { return };
        let suspended = self.qh_idle_suspended();
        if self.qh.idle.due(tick_ms(), system_idle_ms(), suspended, threshold) && self.qh.idle_fired() {
            log(&format!("quick-hide: idle {threshold} ms -> hide"));
            self.qh_sync();
        }
    }

    /// `Settings\QuickHideActive` mirrors `Hidden(Manual)`; `EnabledTemp` is dropped when shown.
    pub(super) fn quick_hide_persist(&mut self, hidden: bool) {
        let path = self.opts.settings_path.clone();
        if self.settings.quick_hide_active == hidden && !(self.settings.quick_hide_enabled_temp && !hidden) {
            return;
        }
        let mut stored = settings_store::load_with_backup(&path);
        let mut changed = stored.quick_hide_active != hidden;
        stored.quick_hide_active = hidden;
        if !stored.quick_hide_enabled && stored.quick_hide_enabled_temp && !hidden {
            stored.quick_hide_enabled_temp = false;
            changed = true;
        }
        self.settings.quick_hide_active = hidden;
        if changed {
            match settings_store::save(&path, &stored) {
                Ok(()) => {
                    self.settings.quick_hide_enabled_temp = stored.quick_hide_enabled_temp;
                    // our own write is not a settings-window edit
                    self.settings_stamp = self.settings_file_stamp();
                }
                Err(e) => log(&format!("quick-hide: persisting QuickHideActive failed: {e}")),
            }
        }
    }

    /// Re-present every group window at its fade alpha, hide fully faded ones, redraw loose icons.
    pub(super) fn quick_hide_apply(&mut self) {
        // items going invisible leave the selection (no verb may reach them)
        let gone: Vec<bool> = self.items.iter().map(|i| i.selected && self.qh_name_hidden(&i.shell.parsing_name)).collect();
        for (item, gone) in self.items.iter_mut().zip(gone) {
            if gone {
                item.selected = false;
            }
        }
        let mut restack = false;
        for index in 0..self.groups.len() {
            let id = self.groups[index].id;
            let alpha = if self.layout.groups.iter().find(|g| g.id == id).is_some_and(|g| self.page_layer_visible(g)) { self.qh_group_alpha(id) } else { 0 };
            let hwnd = self.groups[index].hwnd;
            let visible = unsafe { IsWindowVisible(hwnd) }.as_bool();
            if alpha == 0 {
                if visible {
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                }
                continue;
            }
            let rect = self.groups[index].rect;
            let pos = self.window_xy((rect.left, rect.top));
            let _ = present_layered(hwnd, &self.groups[index].surface, pos, alpha);
            if !visible {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
                restack = true;
            }
        }
        if restack {
            self.restack();
        }
        self.layout_portal_popups();
        self.render_surface();
    }

    fn quick_hide_log_summary(&self) {
        let visible = self.groups.iter().filter(|g| unsafe { IsWindowVisible(g.hwnd) }.as_bool()).count();
        let loose_drawn = self
            .items
            .iter()
            .filter(|i| self.group_of(&i.shell.parsing_name).is_none() && self.qh_loose_alpha(i) > 0)
            .count();
        let state = self.quick_hide.phase();
        log(&format!(
            "quick-hide: settled {} (group windows visible {visible}/{}, loose icons drawn {loose_drawn}, label {})",
            state.name(),
            self.groups.len(),
            if matches!(state, Phase::Hidden) { "hidden" } else { "shown" }
        ));
    }

    /// QA aid: the loose layer as drawn right now.
    pub(super) fn quick_hide_dump_surface(&self) {
        if let Ok(dir) = std::env::var("CORRAL_QA_DUMP_DIR") {
            let _ = self.surface.save_png(&std::path::Path::new(&dir).join(format!("surface-{}.png", self.quick_hide.phase().name())));
        }
    }

    /// Selection fill colour scaled by the icon alpha.
    pub(super) fn qh_scaled(color: Rgba, alpha: u8) -> Rgba {
        Rgba::new(color.r, color.g, color.b, (color.a as u32 * alpha as u32 / 255) as u8)
    }
}
