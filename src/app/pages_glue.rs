//! Ranges window integration (ranges-dock-clearview-spec 3). All Range state is transient except the Range
//! numbers of Corrals and loose icons. Decisions: `pages.rs` (gestures, carry, removal) and its transition
//! (motion-sorting-spec 5); the engine here feeds events and applies the results.
use crate::model::Group;
use crate::motion::{self, Tween};
use crate::pages::{self, Button, Carry, CarryAct, Config, Held, Mods, PointerEvent, PointerKind, Recognizer, Target, TouchPan, Verdict};
use crate::render::{RectF, Rgba, Surface, present_layered};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::HMENU;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, GetAsyncKeyState, VK_CONTROL, VK_ESCAPE, VK_LBUTTON, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT};
use windows::Win32::UI::Input::Touch::{CloseTouchInputHandle, GetTouchInputInfo, HTOUCHINPUT, RegisterTouchWindow, TOUCHEVENTF_UP, TOUCHINPUT, TWF_FINETOUCH};
use windows::Win32::UI::Shell::SHQueryUserNotificationState;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;
use super::{App, Drag, log, tick_ms};
use super::hosting::client_size;

/// Frame timer of a Range transition.
pub(super) const TIMER_FRAME: usize = 0x5301;
/// Poll of the pointer while an item is dragged (carry, spec 3.4): 50 ms.
pub(super) const TIMER_CARRY: usize = 0x5302;
/// Slow tick that removes empty Ranges (spec 3.5).
pub(super) const TIMER_PRUNE: usize = 0x5303;
/// Short poll that closes a context menu opened by the both-buttons chord (spec 3.3.6).
pub(super) const TIMER_CHORD: usize = 0x5304;

const CARRY_POLL_MS: u32 = 50;
const PRUNE_MS: u32 = 1000;
/// Nothing is removed until this long after start: the desktop items are still being read.
const PRUNE_ARM_MS: u64 = 4000;
const CHORD_MENU_GUARD_MS: u64 = 250;
const GATE_CACHE_MS: u64 = 250;

#[derive(Default)]
pub(super) struct State {
    pub(super) current: i32,
    pub(super) snapshotting: bool,
    animation: Option<Animation>,
    /// Ranges that exist: the highest used number + 1, or more while an added Range is still empty.
    count: i32,
    rec: Recognizer,
    carry: Carry,
    touch_pan: TouchPan,
    /// The Range the running drag started on (carry restores it on Esc), and whether it added Ranges.
    carry_start: Option<i32>,
    carry_added: bool,
    bar: Option<Bar>,
    chord_until: u64,
    edge_capture: bool,
    gate_cache: Option<(u64, bool)>,
    last_msg: Option<(isize, u32, usize, isize, u32)>,
    pub(super) drag_names: Vec<String>,
    touches: HashMap<u32, (i32, i32)>,
}

impl State {
    pub(super) fn moving(&self) -> bool { self.animation.is_some() }
}

/// A Range transition: the scroll offset in pixels (0 = the current Range fills the monitor) is a
/// tween; the pictures are snapshots of the Range being left and of the one coming in.
struct Animation {
    /// The Range this transition ends on.
    target: i32,
    /// The Range the `from` picture shows; `None` when it is a frame captured mid-flight.
    from_page: Option<i32>,
    /// The Range that picture stands for when the direction of the entering one is decided.
    ref_page: i32,
    candidate: i32,
    from: Surface,
    to: Surface,
    width: i32,
    tween: Tween,
}

fn qa() -> bool { std::env::var_os("CORRAL_QA_PAGES").is_some() }

pub(super) fn register_touch(hwnd: HWND) { unsafe { let _ = RegisterTouchWindow(hwnd, TWF_FINETOUCH); } }

fn key_held(vk: u16) -> bool { unsafe { GetAsyncKeyState(i32::from(vk)) as u16 & 0x8000 != 0 } }

fn current_mods() -> Mods {
    Mods {
        shift: key_held(VK_SHIFT.0),
        ctrl: key_held(VK_CONTROL.0),
        alt: key_held(VK_MENU.0),
        win: key_held(VK_LWIN.0) || key_held(VK_RWIN.0),
    }
}

impl App {
    pub(super) fn pages_init(&mut self) {
        if let Some(preview) = self.preview_host { register_touch(preview); }
        register_touch(self.surface_hwnd);
        // Negative numbers of older layouts are lifted to 0 (spec 3.1), only while Ranges are on; nothing is
        // clamped, and holes stay until pruned.
        let shift = self.settings.swipe_enabled.then(|| pages::lift_negatives(&self.pages_numbers(true))).flatten();
        if let Some(shift) = shift {
            for g in &mut self.layout.groups { g.swipe_index = g.swipe_index.saturating_add(shift); }
            for v in self.layout.icon_pages.values_mut() { *v = v.saturating_add(shift); }
        }
        self.pages.count = self.pages_derived_count();
        unsafe { SetTimer(self.message_window, TIMER_PRUNE, PRUNE_MS, None); }
        self.pages_report();
    }

    /// Range numbers in use: unpinned Corrals and loose icons. `stored` also lists icon rows whose item is
    /// not on the desktop (used before the desktop is read).
    fn pages_numbers(&self, stored: bool) -> Vec<i32> {
        let mut numbers: Vec<i32> = self.layout.groups.iter().filter(|g| !g.all_pages).map(|g| g.swipe_index).collect();
        numbers.extend(self.items.iter().filter(|i| self.group_of(&i.shell.parsing_name).is_none())
            .map(|i| self.layout.icon_page(&i.shell.parsing_name)));
        if stored { numbers.extend(self.layout.icon_pages.values().copied()); }
        numbers
    }

    fn pages_derived_count(&self) -> i32 {
        self.pages_numbers(true).into_iter().max().map_or(1, |m| m.saturating_add(1)).max(1)
    }

    /// Ranges there are: the used ones, the shown one and any added, still empty one.
    pub(super) fn pages_count(&self) -> i32 {
        self.pages.count.max(self.pages_derived_count()).max(self.pages.current.saturating_add(1)).max(1)
    }

    /// The Range a step starts from: the shown one, or the target of the running transition.
    fn pages_target(&self) -> i32 {
        self.pages.animation.as_ref().map_or(self.pages.current, |a| a.target)
    }

    pub(super) fn pages_menu_entry(&self, id: u64) -> Option<bool> {
        (self.settings.swipe_enabled && self.pages_count() > 1)
            .then(|| self.layout.groups.iter().find(|g| g.id == id).map(|g| g.all_pages)).flatten()
    }

    /// The Corral or icons being dragged follow the view to the Range it is carried to (spec 3.4).
    fn page_carried_group(&self, id: u64) -> bool {
        matches!(self.drag, Drag::MoveGroup { id: dragged, moved: true, .. } if dragged == id)
    }

    pub(super) fn page_group_visible(&self, group: &Group) -> bool {
        pages::group_visible(self.settings.swipe_enabled, group.swipe_index, group.all_pages, self.pages.current)
            || (self.settings.swipe_enabled && self.page_carried_group(group.id))
    }

    pub(super) fn page_group_id_visible(&self, id: u64) -> bool {
        self.layout.groups.iter().find(|g| g.id == id).is_some_and(|g| self.page_group_visible(g))
    }

    pub(super) fn page_name_visible(&self, name: &str) -> bool {
        match self.group_of(name) {
            Some(id) => self.page_group_id_visible(id),
            None => {
                let carried = self.settings.swipe_enabled && matches!(self.drag, Drag::Item { active: true, .. })
                    && self.pages.drag_names.iter().any(|n| n.eq_ignore_ascii_case(name));
                carried || pages::icon_visible(self.settings.swipe_enabled, self.layout.icon_page(name), self.pages.current)
            }
        }
    }

    pub(super) fn page_layer_visible(&self, group: &Group) -> bool {
        self.page_group_visible(group) && (!self.pages.moving() || group.all_pages || matches!(self.drag, Drag::MoveGroup { id, .. } if id == group.id))
    }

    pub(super) fn page_refresh(&mut self) {
        // Off-page selection cannot reach shell verbs, drag sets or later Shift ranges.
        if !matches!(self.drag, Drag::Item { active: true, .. }) && self.ole_drag.is_none() {
            let visible: Vec<bool> = self.items.iter().map(|i| self.page_name_visible(&i.shell.parsing_name)).collect();
            for (item, visible) in self.items.iter_mut().zip(visible) { if !visible { item.selected = false; } }
        }
        self.refresh_all();
        self.pages_report();
    }

    fn pages_report(&self) {
        if !qa() { return; }
        log(&format!("pages: current={} count={}", self.pages.current, self.pages_count()));
        for group in &self.layout.groups {
            log(&format!("pages: group={} visible={} page={}", group.id, self.page_group_visible(group), group.swipe_index));
        }
    }

    fn page_width(&self, point: (i32, i32)) -> i32 {
        let width = client_size(self.parent).0.max(1);
        self.monitors.iter().find(|m| m.bounds.contains(point.0, point.1))
            .map_or(width, |m| width.min(m.bounds.width()).max(1))
    }

    fn range_mons(&self) -> Vec<pages::Mon> {
        self.monitors.iter().map(|m| pages::Mon::new(m.bounds, m.dpi)).collect()
    }

    fn page_snapshot(&mut self, page: i32) -> Result<Surface, String> {
        let old_page = self.pages.current;
        self.pages.current = page;
        self.pages.snapshotting = true;
        self.render_surface();
        for i in 0..self.groups.len() { self.render_group(i); }
        let (w, h) = client_size(self.parent);
        let captured = Surface::new(w.max(1) as u32, h.max(1) as u32).map_err(|e| e.to_string()).and_then(|mut frame| {
            frame.draw(|ctx| {
                ctx.clear(Rgba::TRANSPARENT);
                ctx.draw_bitmap(self.surface.pixels(), self.surface.width(), self.surface.height(), RectF::new(0.0, 0.0, w as f32, h as f32), 1.0)?;
                for window in &self.groups {
                    let Some(group) = self.layout.groups.iter().find(|g| g.id == window.id) else { continue };
                    if group.all_pages || !self.page_group_visible(group) || matches!(self.drag, Drag::MoveGroup { id, .. } if id == group.id) { continue; }
                    let r = window.rect;
                    ctx.draw_bitmap(window.surface.pixels(), window.surface.width(), window.surface.height(),
                        RectF::new(r.left as f32, r.top as f32, r.right as f32, r.bottom as f32), self.qh_group_alpha(group.id) as f32 / 255.0)?;
                }
                Ok(())
            }).map_err(|e| e.to_string())?;
            Ok(frame)
        });
        self.pages.current = old_page;
        self.pages.snapshotting = false;
        captured
    }

    fn page_dump(frame: &Surface, name: &str) {
        if let Ok(dir) = std::env::var("CORRAL_QA_DUMP_DIR") { let _ = frame.save_png(&std::path::Path::new(&dir).join(name)); }
    }

    fn page_hide_moving_layers(&mut self) {
        for group in &self.layout.groups {
            if !group.all_pages && !matches!(self.drag, Drag::MoveGroup { id, .. } if id == group.id) {
                if let Some(window) = self.groups.iter().find(|g| g.id == group.id) { unsafe { let _ = ShowWindow(window.hwnd, SW_HIDE); } }
                if let Some(bar) = self.scrollbars.get(&group.id) { bar.hide(); }
            }
        }
    }

    fn page_frame(&mut self, from: &Surface, to: &Surface, ref_page: i32, candidate: i32, width: i32, offset: i32) {
        let (w, h) = client_size(self.parent);
        let _ = self.surface.resize(w.max(1) as u32, h.max(1) as u32);
        let result = self.surface.draw(|ctx| {
            if self.preview_host.is_some() {
                ctx.fill_linear_gradient(RectF::new(0.0, 0.0, w as f32, h as f32), Rgba::new(28, 64, 110, 255), Rgba::new(10, 22, 40, 255))?;
            } else { ctx.clear(Rgba::new(0, 0, 0, 1)); }
            let (leaving, entering) = pages::range_xs(ref_page, candidate, offset, width);
            ctx.draw_bitmap(from.pixels(), from.width(), from.height(), RectF::new(leaving as f32, 0.0, (leaving + w) as f32, h as f32), 1.0)?;
            if let Some(x) = entering {
                ctx.draw_bitmap(to.pixels(), to.width(), to.height(), RectF::new(x as f32, 0.0, (x + w) as f32, h as f32), 1.0)?;
            }
            Ok(())
        });
        if result.is_ok() { let _ = present_layered(self.surface_hwnd, &self.surface, self.window_xy((0, 0)), 255); }
        Self::page_dump(&self.surface, "pages-frame.png");
        if qa() { log(&format!("pages: frame offset={offset}")); }
    }

    pub(super) fn page_cancel_motion(&mut self) {
        self.pages.animation = None;
        self.pages.touches.clear();
        self.pages.touch_pan.reset();
        unsafe { let _ = KillTimer(self.message_window, TIMER_FRAME); }
    }

    /// The transition ended on `target`. Nothing is stored here: a carried Corral takes the Range number at
    /// the drop (spec 3.4), and empty Ranges go once nothing is in flight (spec 3.5).
    fn page_finish(&mut self, target: i32) {
        self.pages.current = target;
        self.page_refresh();
        Self::page_dump(&self.surface, "pages-current.png");
        self.pages_prune();
    }

    fn page_animate(&mut self, animation: Animation) {
        let now = motion::now_ms();
        if animation.tween.finished(now) {
            self.page_finish(animation.target);
        } else {
            self.page_hide_moving_layers();
            self.page_frame(&animation.from, &animation.to, animation.ref_page, animation.candidate, animation.width, animation.tween.value(now).round() as i32);
            self.pages.animation = Some(animation);
            unsafe { SetTimer(self.message_window, TIMER_FRAME, motion::FRAME_MS as u32, None); }
        }
    }

    /// Show Range `target` (a neighbour of the effective current one), by the stored transition. A switch
    /// while a transition runs is accepted at once.
    pub(super) fn page_to(&mut self, target: i32) {
        if target < 0 || target >= self.pages_count() { return; }
        if self.pages.animation.is_some() { self.page_retarget(target); return; }
        if target == self.pages.current { return; }
        // SwipeShowIcons: any switch first ends Clear view, so its fade-in runs with the slide (spec 3.6)
        if self.settings.swipe_show_icons_on_initiate { self.qh_end_hidden(); }
        let (style, animate) = (self.settings.swipe_animation_style, motion::animations_enabled());
        if pages::transition(style, animate).is_none() { self.page_finish(target); return; }
        let at = self.range_pointer().unwrap_or(self.origin());
        let width = self.page_width(at);
        let (Ok(from), Ok(to)) = (self.page_snapshot(self.pages.current), self.page_snapshot(target)) else {
            self.page_finish(target); return;
        };
        Self::page_dump(&from, "pages-from.png"); Self::page_dump(&to, "pages-to.png");
        let mut tween = Tween::at_rest(0.0);
        let end = -target.saturating_sub(self.pages.current).signum() * width;
        pages::retarget_offset(&mut tween, motion::now_ms(), f64::from(end), width, style, animate);
        let ref_page = self.pages.current;
        self.page_animate(Animation { target, from_page: Some(ref_page), ref_page, candidate: target, from, to, width, tween });
    }

    /// A switch while a transition runs is accepted at once (spec 5, "Interrupting"). Going back to the
    /// Range being left reuses the pictures and runs the offset home; going further (or back from a
    /// captured frame) continues from the picture on screen, which becomes the picture being left. The
    /// slide direction comes from the previous target, never from `pages.current`.
    fn page_retarget(&mut self, next: i32) {
        let Some(mut animation) = self.pages.animation.take() else { return };
        let (style, animate) = (self.settings.swipe_animation_style, motion::animations_enabled());
        let now = motion::now_ms();
        let slide = match pages::retarget_plan(animation.from_page, animation.target, next) {
            pages::Retarget::Ignore => { self.pages.animation = Some(animation); return; }
            pages::Retarget::Return => {
                animation.target = next;
                pages::retarget_offset(&mut animation.tween, now, 0.0, animation.width, style, animate);
                self.pages.animation = Some(animation);
                return;
            }
            pages::Retarget::Continue { direction } => direction,
        };
        let shown = Surface::new(self.surface.width(), self.surface.height()).ok().map(|mut s| {
            s.pixels_mut().copy_from_slice(self.surface.pixels());
            s
        });
        let to = self.page_snapshot(next);
        let (Some(from), Ok(to)) = (shown, to) else {
            unsafe { let _ = KillTimer(self.message_window, TIMER_FRAME); }
            self.page_finish(animation.target);
            return;
        };
        let mut tween = Tween::at_rest(0.0);
        let end = -slide * animation.width;
        pages::retarget_offset(&mut tween, now, f64::from(end), animation.width, style, animate);
        let ref_page = animation.target;
        self.page_frame(&from, &to, ref_page, next, animation.width, 0);
        self.pages.animation = Some(Animation { target: next, from_page: None, ref_page, candidate: next, from, to, width: animation.width, tween });
    }

    /// A Corral move or resize runs, or an icon is pressed or dragged (a press can still become a drag).
    fn range_busy(&self) -> bool {
        matches!(self.drag, Drag::MoveGroup { .. } | Drag::ResizeGroup { .. } | Drag::Item { .. }) || self.ole_drag.is_some()
    }

    /// A transition that is running ends where it was going (another feature needs a settled picture).
    pub(super) fn page_finish_motion(&mut self) {
        if let Some(animation) = self.pages.animation.take() {
            unsafe { let _ = KillTimer(self.message_window, TIMER_FRAME); }
            self.page_finish(animation.target);
        }
    }

    fn range_pointer(&self) -> Option<(i32, i32)> {
        let mut p = POINT::default();
        unsafe { GetCursorPos(&mut p) }.ok().map(|_| (p.x, p.y))
    }

    // ---- gate (spec section 2) -------------------------------------------------------------------

    /// The common gate for Ranges: the master setting and the desktop gate.
    fn ranges_gate(&mut self) -> bool {
        self.settings.swipe_enabled && self.desk_gate()
    }

    /// The desktop gate shared by Ranges and Clear view (no full-screen or presentation app, locked screen
    /// or screen saver, menu in menu mode, rename box), cached for 250 ms.
    pub(super) fn desk_gate(&mut self) -> bool {
        let now = tick_ms();
        if let Some((at, open)) = self.pages.gate_cache {
            if now.saturating_sub(at) < GATE_CACHE_MS { return open; }
        }
        let open = self.desk_gate_open();
        self.pages.gate_cache = Some((now, open));
        open
    }

    /// Whether the desktop gestures may run at all: shared by Ranges and Clear view.
    pub(super) fn desk_gate_open(&self) -> bool {
        if self.naming.is_some() || self.renaming.is_some() { return false; }
        let mut gui = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        let menu = unsafe { GetGUIThreadInfo(0, &mut gui) }.is_ok() && gui.flags.0 & (GUI_INMENUMODE.0 | GUI_POPUPMENUMODE.0) != 0;
        if menu { return false; }
        let state = unsafe { SHQueryUserNotificationState() }.ok().map(|s| s.0);
        // 1 = QUNS_NOT_PRESENT: locked session or screen saver
        if state == Some(1) { return false; }
        let fg = unsafe { GetForegroundWindow() };
        let (class, own) = if fg.0.is_null() { (String::new(), false) } else { (super::peek::class_name(fg), super::peek::is_own_process(fg)) };
        super::peek::fullscreen_refusal(state, &class, own).is_none()
    }

    // ---- event translation ------------------------------------------------------------------------

    /// What is under a screen point: the empty desktop, a loose icon, a Corral (or a part of one), or
    /// another window.
    fn range_target_at(&self, screen: (i32, i32)) -> Target {
        let under = unsafe { WindowFromPoint(POINT { x: screen.0, y: screen.1 }) };
        if under.0.is_null() { return Target::Other; }
        self.range_target_of(under, screen)
    }

    fn range_target_of(&self, hwnd: HWND, screen: (i32, i32)) -> Target {
        if self.groups.iter().any(|g| g.hwnd == hwnd)
            || self.scrollbars.values().any(|b| b.hwnd() == hwnd) {
            return Target::Corral;
        }
        if hwnd == self.surface_hwnd || hwnd == self.parent || self.preview_host == Some(hwnd) {
            let (ox, oy) = self.origin();
            return if self.item_at_surface((screen.0 - ox, screen.1 - oy)).is_some() { Target::LooseIcon } else { Target::EmptyDesktop };
        }
        Target::Other
    }

    fn range_cfg(&self) -> Config {
        Config {
            edge: self.settings.swipe_switch_via_drag_edge,
            touch: self.settings.swipe_switch_via_touch,
            keys: self.settings.swipe_switch_via_keyboard,
            middle: self.settings.swipe_switch_via_middle_drag,
            dual: self.settings.swipe_switch_via_dual_drag,
        }
    }

    /// Feed one window message to the recogniser and Clear view's click detector. `Some` = swallow it.
    fn range_input(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let pointer = match msg {
            WM_MOUSEMOVE => Some(PointerKind::Move),
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => Some(PointerKind::Down(Button::Left)),
            WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => Some(PointerKind::Down(Button::Right)),
            WM_MBUTTONDOWN | WM_MBUTTONDBLCLK => Some(PointerKind::Down(Button::Middle)),
            WM_LBUTTONUP => Some(PointerKind::Up(Button::Left)),
            WM_RBUTTONUP => Some(PointerKind::Up(Button::Right)),
            WM_MBUTTONUP => Some(PointerKind::Up(Button::Middle)),
            _ => None,
        };
        let wheel = matches!(msg, WM_MOUSEWHEEL | WM_MOUSEHWHEEL);
        let key = matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP);
        if pointer.is_none() && !wheel && !key { return None; }
        // The message loop and the window procedure both see a message: count it once.
        let stamp = (hwnd.0 as isize, msg, wparam.0, lparam.0, unsafe { GetMessageTime() } as u32);
        if self.pages.last_msg == Some(stamp) { return None; }
        self.pages.last_msg = Some(stamp);

        let gate = self.ranges_gate();
        let mons = self.range_mons();
        let (current, count) = (self.pages_target(), self.pages_count());
        let cfg = self.range_cfg();
        let busy = self.range_busy();
        let ctx = pages::Ctx { gate, cfg, mons: &mons, current, count, busy };
        let now = tick_ms();
        let out = if let Some(kind) = pointer {
            let mut p = POINT { x: (lparam.0 & 0xffff) as i16 as i32, y: ((lparam.0 >> 16) & 0xffff) as i16 as i32 };
            unsafe { let _ = ClientToScreen(hwnd, &mut p); }
            let pos = (p.x, p.y);
            let mut held = Held { left: wparam.0 & 0x1 != 0, right: wparam.0 & 0x2 != 0, middle: wparam.0 & 0x10 != 0 };
            match kind {
                PointerKind::Down(Button::Left) => held.left = true,
                PointerKind::Down(Button::Right) => held.right = true,
                PointerKind::Down(Button::Middle) => held.middle = true,
                PointerKind::Up(Button::Left) => held.left = false,
                PointerKind::Up(Button::Right) => held.right = false,
                PointerKind::Up(Button::Middle) => held.middle = false,
                PointerKind::Move => {}
            }
            // Moves are not classified: no gesture decides by what a move passes over.
            let target = if kind == PointerKind::Move { Target::Other } else { self.range_target_of(hwnd, pos) };
            let ev = PointerEvent { t_ms: now, pos, kind, held, mods: current_mods(), target };
            let out = self.pages.rec.pointer(ev, &ctx);
            if out.verdict == Verdict::Pass { self.qh_pointer(ev); }
            out
        } else if wheel {
            let pos = (((lparam.0 & 0xffff) as i16) as i32, (((lparam.0 >> 16) & 0xffff) as i16) as i32);
            let under = unsafe { WindowFromPoint(POINT { x: pos.0, y: pos.1 }) };
            // a wheel for another window of ours is re-posted there (see `pre_translate`): count that copy
            if !under.0.is_null() && under != hwnd && self.range_target_of(under, pos) != Target::Other { return None; }
            let delta = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
            let (dx, dy) = if msg == WM_MOUSEHWHEEL { (delta, 0) } else { (0, delta) };
            let target = self.range_target_at(pos);
            self.pages.rec.wheel(pages::WheelEvent { t_ms: now, pos, mods: current_mods(), dx, dy, target }, &ctx)
        } else {
            let focus_ok = (self.is_layer(hwnd) || hwnd == self.parent || self.preview_host == Some(hwnd))
                && self.naming.is_none() && self.renaming.is_none()
                && !super::peek::class_name(hwnd).eq_ignore_ascii_case("Edit");
            let mods = Mods { alt: key_held(VK_MENU.0) || (lparam.0 as usize & (1 << 29) != 0 && msg == WM_SYSKEYDOWN), ..current_mods() };
            let ev = pages::KeyEvent {
                t_ms: now, vk: wparam.0 as u32, down: matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN),
                repeat: lparam.0 as usize & (1 << 30) != 0, mods, focus_ok,
            };
            self.pages.rec.key(ev, &ctx)
        };
        // the chord ends a rectangle in progress (a marquee or a Corral being drawn), never another drag
        if out.cancel_rectangle {
            // a Corral being drawn (creation-glue) ends through its own cancel path, which also clears `right_start`
            if !self.draw_cancel(crate::creation::CancelReason::OtherButton) {
                if matches!(self.drag, Drag::Pending { .. } | Drag::Band { .. }) { self.cancel_drag(); }
                self.right_start = None;
            }
        }
        // a swallowed edge-pull press owns the mouse until its release, wherever that happens
        if let Some(PointerKind::Down(Button::Left)) = pointer {
            if out.verdict == Verdict::Swallow && self.pages.rec.edge_active() {
                unsafe { SetCapture(hwnd); }
                self.pages.edge_capture = true;
            }
        }
        if let Some(PointerKind::Up(Button::Left)) = pointer {
            if std::mem::take(&mut self.pages.edge_capture) { unsafe { let _ = ReleaseCapture(); } }
        }
        if out.chord_ended { self.pages.chord_until = now + CHORD_MENU_GUARD_MS; unsafe { SetTimer(self.message_window, TIMER_CHORD, 16, None); } }
        if let Some(target) = out.step { self.page_to(target); }
        (out.verdict == Verdict::Swallow).then_some(LRESULT(0))
    }

    // ---- touch screen (spec 3.3.2) ----------------------------------------------------------------

    fn page_touch(&mut self, hwnd: HWND, wparam: WPARAM, lparam: LPARAM) {
        let count = (wparam.0 & 0xffff) as u32;
        let mut inputs = vec![TOUCHINPUT::default(); count.min(16) as usize];
        let handle = HTOUCHINPUT(lparam.0 as *mut _);
        if count == 0 || unsafe { GetTouchInputInfo(handle, &mut inputs, std::mem::size_of::<TOUCHINPUT>() as i32) }.is_err() {
            unsafe { let _ = CloseTouchInputHandle(handle); }
            return;
        }
        unsafe { let _ = CloseTouchInputHandle(handle); }
        for input in &inputs {
            // positions are hundredths of a pixel in screen coordinates
            if input.dwFlags.0 & TOUCHEVENTF_UP.0 != 0 { self.pages.touches.remove(&input.dwID); }
            else { self.pages.touches.insert(input.dwID, (input.x / 100, input.y / 100)); }
        }
        let _ = hwnd;
        let contacts: Vec<(i32, i32)> = self.pages.touches.values().copied().collect();
        let gate = self.ranges_gate();
        let mons = self.range_mons();
        let ctx = pages::Ctx { gate, cfg: self.range_cfg(), mons: &mons, current: self.pages_target(), count: self.pages_count(), busy: self.range_busy() };
        if let Some(target) = self.pages.touch_pan.feed(&contacts, &ctx) { self.page_to(target); }
    }

    // ---- carrying an item to a neighbouring Range (spec 3.4) -------------------------------------

    fn carry_kind(&self) -> Option<pages::DragKind> {
        match &self.drag {
            Drag::MoveGroup { moved: true, .. } => Some(pages::DragKind::Corral),
            Drag::Item { active: true, .. } => Some(pages::DragKind::Icons),
            _ if self.ole_drag.is_some() => Some(pages::DragKind::Icons),
            _ => None,
        }
    }

    /// One 50 ms poll of the pointer during a drag.
    fn carry_tick(&mut self) {
        let Some(kind) = self.carry_kind().filter(|_| self.settings.swipe_enabled) else {
            // an icon drag that ended without a drop (Esc in the OLE loop) returns the view to where it started
            if self.pages.carry_start.is_some() && self.pages_carried() { self.pages_abort_carry(); }
            self.pages.carry.reset();
            self.pages.carry_start = None;
            self.pages.carry_added = false;
            self.bar_hide();
            // a pressed button may still turn into a drag: keep polling until it is released
            if !key_held(VK_LBUTTON.0) { unsafe { let _ = KillTimer(self.message_window, TIMER_CARRY); } }
            return;
        };
        if self.pages.carry_start.is_none() {
            self.pages.carry_start = Some(self.pages_target());
            self.pages.carry_added = false;
        }
        if !self.ranges_gate() { self.pages.carry.reset(); self.bar_hide(); return; }
        let Some(pos) = self.range_pointer() else { return };
        let mons = self.range_mons();
        let (current, count) = (self.pages_target(), self.pages_count());
        let out = self.pages.carry.poll(tick_ms(), pos, kind, &mons, current, count);
        match out.act {
            CarryAct::Switch(target) => self.page_to(target),
            CarryAct::Add => {
                self.pages.count = count + 1;
                self.pages.carry_added = true;
                self.page_to(count);
            }
            CarryAct::None => {}
        }
        match out.progress {
            Some(progress) => self.bar_show(progress),
            None => self.bar_hide(),
        }
    }

    /// The drag ended with a drop: a Corral takes the number of the shown Range (spec 3.4); empty Ranges go.
    pub(super) fn pages_settle(&mut self) {
        if self.settings.swipe_enabled {
            if let Some(animation) = self.pages.animation.take() {
                unsafe { let _ = KillTimer(self.message_window, TIMER_FRAME); }
                self.pages.current = animation.target;
                self.page_refresh();
            }
            if let Drag::MoveGroup { id, moved: true, .. } = self.drag {
                let current = self.pages.current;
                if let Some(g) = self.layout.group_mut(id).filter(|g| !g.all_pages) { g.swipe_index = current; }
            }
        }
        if let Some(press) = self.tab_press.as_mut() { press.page = self.pages.current; }
        self.pages.carry.reset();
        self.pages.carry_start = None;
        self.pages.carry_added = false;
        self.bar_hide();
    }

    /// A Corral title drag whose view was carried to another Range: its tab press goes on across the switch.
    pub(super) fn pages_carrying_title(&self) -> bool {
        self.pages.carry_start.is_some() && self.tab_press.is_some()
    }

    /// Esc during a carry: the view goes back to the Range the drag started on. The Corral's rect is
    /// restored by the caller; the Ranges added meanwhile are empty again and go (spec 3.4).
    pub(super) fn pages_abort_carry(&mut self) {
        let Some(start) = self.pages.carry_start.take() else { return };
        self.pages.carry.reset();
        self.bar_hide();
        if self.pages_target() != start { self.page_cancel_motion(); self.page_finish(start); }
        self.pages.carry_added = false;
    }

    /// Whether the drag in flight carried the view away from where it started (or added a Range).
    pub(super) fn pages_carried(&self) -> bool {
        self.pages.carry_start.is_some_and(|s| s != self.pages_target()) || self.pages.carry_added
    }

    pub(super) fn pages_ole_hover(&mut self, _point: (i32, i32), keys: u32) {
        if keys & 1 == 0 { self.pages_settle(); }
        else { unsafe { SetTimer(self.message_window, TIMER_CARRY, CARRY_POLL_MS, None); } }
    }

    pub(super) fn pages_ole_leave(&mut self) {}

    // ---- removing empty Ranges (spec 3.5) ------------------------------------------------------

    /// Delete every empty Range except the shown one, renumbering everything above it. Only numbers change.
    pub(super) fn pages_prune(&mut self) {
        if !self.settings.swipe_enabled || self.pages.animation.is_some() || self.carry_kind().is_some() { return; }
        if tick_ms() < PRUNE_ARM_MS { return; }
        if self.items.is_empty() && !self.layout.icon_pages.is_empty() { return; }
        let count = self.pages_count();
        // stored icon rows of items that are not listed right now keep their Range alive
        let groups: Vec<i32> = self.layout.groups.iter().filter(|g| !g.all_pages).map(|g| g.swipe_index).collect();
        let present: Vec<i32> = self.items.iter().filter(|i| self.group_of(&i.shell.parsing_name).is_none())
            .map(|i| self.layout.icon_page(&i.shell.parsing_name)).collect();
        let stored: Vec<i32> = self.layout.icon_pages.values().copied().collect();
        let removed = pages::removals(count, self.pages.current, &groups, &present, &stored);
        if removed.is_empty() { self.pages.count = count; return; }
        for g in &mut self.layout.groups { g.swipe_index = pages::renumber(g.swipe_index, &removed); }
        for v in self.layout.icon_pages.values_mut() { *v = pages::renumber(*v, &removed); }
        self.pages.current = pages::renumber(self.pages.current, &removed);
        self.pages.count = count - removed.len() as i32;
        log(&format!("pages: removed empty Ranges {removed:?}, {} left", self.pages.count));
        self.save();
        self.page_refresh();
    }

    // ---- timers ----------------------------------------------------------------------------

    pub(super) fn pages_timer(&mut self, id: usize) -> bool {
        match id {
            TIMER_FRAME => {
                let Some(animation) = self.pages.animation.take() else {
                    unsafe { let _ = KillTimer(self.message_window, TIMER_FRAME); }
                    return true;
                };
                let now = motion::now_ms();
                if animation.tween.finished(now) || !motion::animations_enabled() {
                    unsafe { let _ = KillTimer(self.message_window, TIMER_FRAME); }
                    self.page_finish(animation.target);
                } else {
                    let offset = animation.tween.value(now).round() as i32;
                    self.page_frame(&animation.from, &animation.to, animation.ref_page, animation.candidate, animation.width, offset);
                    self.pages.animation = Some(animation);
                }
                true
            }
            TIMER_CARRY => { self.carry_tick(); true }
            TIMER_PRUNE => { self.pages_prune(); true }
            TIMER_CHORD => {
                // the both-buttons chord must not leave a context menu behind (spec 3.3.6)
                let mut gui = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
                if unsafe { GetGUIThreadInfo(0, &mut gui) }.is_ok() && gui.flags.0 & (GUI_INMENUMODE.0 | GUI_POPUPMENUMODE.0) != 0 {
                    unsafe { let _ = EndMenu(); }
                }
                if tick_ms() >= self.pages.chord_until { unsafe { let _ = KillTimer(self.message_window, TIMER_CHORD); } }
                true
            }
            _ => false,
        }
    }

    // ---- progress bar along the edge ------------------------------------------------------------

    fn bar_show(&mut self, progress: pages::Progress) {
        let Some(mon) = self.monitors.get(progress.mon) else { return };
        let dpi = mon.dpi;
        let bounds = mon.bounds;
        let thick = pages::px(4.0, dpi).max(1);
        let reach = pages::px(120.0, dpi);
        let chip = pages::px(32.0, dpi);
        let gap = pages::px(6.0, dpi);
        let (w, h) = (thick + gap + chip, reach * 2);
        let x = match progress.side { pages::Side::Left => bounds.left, pages::Side::Right => bounds.right - w };
        let y = (progress.y - h / 2).clamp(bounds.top, (bounds.bottom - h).max(bounds.top));
        let bar = self.pages.bar.get_or_insert_with(|| Bar::new(self.instance));
        let Some(hwnd) = bar.hwnd else { return };
        if bar.surface.as_ref().is_none_or(|s| s.width() as i32 != w || s.height() as i32 != h) {
            bar.surface = Surface::new(w as u32, h as u32).ok();
        }
        let Some(surface) = bar.surface.as_mut() else { return };
        let accent = Rgba::new(0, 120, 215, 235);
        let reach_now = (f64::from(reach) * progress.fraction.clamp(0.0, 1.0)) as f32;
        let mid = h as f32 / 2.0;
        let bar_x = match progress.side { pages::Side::Left => 0.0, pages::Side::Right => (w - thick) as f32 };
        let drawn = surface.draw(|ctx| {
            ctx.clear(Rgba::TRANSPARENT);
            if reach_now >= 1.0 {
                ctx.fill_rounded_rect(RectF::new(bar_x, mid - reach_now, bar_x + thick as f32, mid + reach_now), thick as f32 / 2.0, accent)?;
            }
            if reach_now >= 1.0 && progress.add {
                // the "+" chip of a Range that will be added
                let left = match progress.side { pages::Side::Left => (thick + gap) as f32, pages::Side::Right => 0.0 };
                let c = RectF::new(left, mid - chip as f32 / 2.0, left + chip as f32, mid + chip as f32 / 2.0);
                ctx.fill_rounded_rect(c, chip as f32 / 4.0, accent)?;
                let (cx, cy, arm) = (left + chip as f32 / 2.0, mid, chip as f32 / 4.0);
                let white = Rgba::new(255, 255, 255, 255);
                ctx.stroke_line((cx - arm, cy), (cx + arm, cy), 2.0, white)?;
                ctx.stroke_line((cx, cy - arm), (cx, cy + arm), 2.0, white)?;
            }
            Ok(())
        });
        if drawn.is_ok() {
            let _ = present_layered(hwnd, surface, (x, y), 255);
            if !bar.shown {
                unsafe { let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE); }
                bar.shown = true;
            }
        }
    }

    fn bar_hide(&mut self) {
        if let Some(bar) = self.pages.bar.as_mut() {
            if bar.shown {
                if let Some(hwnd) = bar.hwnd { unsafe { let _ = ShowWindow(hwnd, SW_HIDE); } }
                bar.shown = false;
            }
        }
    }

    // ---- the message hook ------------------------------------------------------------------

    pub(super) fn handle_pages_input(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if matches!(msg, WM_CAPTURECHANGED | WM_CANCELMODE) {
            self.pages.rec.lost_capture();
            self.pages.edge_capture = false;
        }
        if msg == WM_LBUTTONDOWN { unsafe { SetTimer(self.message_window, TIMER_CARRY, CARRY_POLL_MS, None); } }
        if msg == WM_LBUTTONUP && matches!(self.drag, Drag::MoveGroup { .. } | Drag::Item { active: true, .. }) { self.pages_settle(); }
        // Esc during an icon drag returns the view to the Range it started on; a Corral drag does the same in
        // its own Esc handler, which also restores the rect.
        if msg == WM_KEYDOWN && wparam.0 == VK_ESCAPE.0 as usize && matches!(self.drag, Drag::Item { active: true, .. }) && self.pages_carried() {
            self.pages_abort_carry();
        }
        if msg == WM_TOUCH && self.settings.swipe_enabled { self.page_touch(hwnd, wparam, lparam); return Some(LRESULT(0)); }
        if self.pages.animation.is_some() && matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_LBUTTONDBLCLK) { return Some(LRESULT(0)); }
        if let Some(swallowed) = self.range_input(hwnd, msg, wparam, lparam) { return Some(swallowed); }
        if let Some(id) = self.group_index(hwnd).map(|i| self.groups[i].id) {
            let carrying = matches!(self.drag, Drag::Item { owner, active: true, .. } if owner == hwnd)
                && matches!(msg, WM_MOUSEMOVE | WM_LBUTTONUP | WM_CAPTURECHANGED);
            if !carrying && !self.page_group_id_visible(id) && matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MOUSEMOVE | WM_MOUSEWHEEL | WM_KEYDOWN | WM_SYSKEYDOWN | WM_LBUTTONDBLCLK) { return Some(LRESULT(0)); }
        }
        None
    }
}

/// The small window that shows the carry dwell: a 4 DIP accent bar along the screen edge (spec 3.4).
/// Click-through, never activated.
struct Bar {
    hwnd: Option<HWND>,
    surface: Option<Surface>,
    shown: bool,
}

unsafe extern "system" fn bar_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

static BAR_CLASS: AtomicBool = AtomicBool::new(false);

impl Bar {
    fn new(instance: windows::Win32::Foundation::HINSTANCE) -> Bar {
        if !BAR_CLASS.swap(true, Ordering::Relaxed) {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(bar_proc),
                hInstance: instance,
                lpszClassName: w!("CorralRangeBar"),
                ..Default::default()
            };
            unsafe { RegisterClassExW(&class) };
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                w!("CorralRangeBar"), w!(""), WS_POPUP, 0, 0, 1, 1, HWND::default(), HMENU::default(), instance, None,
            )
        }.ok();
        Bar { hwnd, surface: None, shown: false }
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        if let Some(hwnd) = self.hwnd.take() { unsafe { let _ = DestroyWindow(hwnd); } }
    }
}
