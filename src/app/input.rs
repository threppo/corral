//! Input handling of the surface and group windows: mouse/keyboard messages, selection, creation by
//! drawing, naming, group moves, hover-open and roll-up animation, title-bar hover.

use crate::creation::{CancelReason, DrawButton};
use crate::geometry::{Rect, to_dip, to_px};
use crate::group_paint::SegmentPart;
use crate::model::{GroupContent, RollState};
use crate::motion::{self, Tween};
use crate::shell;
use crate::snap::{self, CursorKind, EdgeZone, NeighbourInfo, ResizeInput, SnapSettings, ZoneCtx};
use crate::rollup;
use crate::size_tip;
use crate::style;
use crate::tabs;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HGDIOBJ, ScreenToClient};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, ReleaseCapture, SetCapture, SetFocus, VK_CONTROL, VK_DELETE, VK_ESCAPE, VK_F2, VK_RETURN, VK_SHIFT, VK_MENU, VK_LBUTTON, VK_RBUTTON};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, w};
use super::creation_glue::DrawEnd;
use super::{APP, App, Drag, TabPress, TitleButton, EM_SETSEL, TIMER_HOVER_DELAY, TIMER_HOVER_POLL, TIMER_LAYOUT_SAVE, TIMER_TITLE_HOVER, TIMER_TAB_ARM, TIMER_TAB_SCROLL, TIMER_TAB_HOVER, TIMER_TAB_SAVE, log, lparam_point, tick_ms};

/// Translate a title right-click into surface coordinates; the menu follows the cursor.
fn title_context_menu_point(cursor: (i32, i32), group: Rect) -> (i32, i32) {
    (cursor.0 + group.left, cursor.1 + group.top)
}

/// `MK_CONTROL` in the `wParam` of a mouse message.
const MK_CONTROL: usize = 0x0008;
use super::selection::{self, Modifiers, PressAction, Release};

/// Zone of a point on the painted Corral (grid-snapping-spec 3.1): the bands sit inside the painted
/// rectangles, a folded Corral answers on its left and right edges only, and the anchored edge of a
/// docked Corral has no zone. `buttons` are the title's own glyphs in the same coordinates.
pub(super) fn group_hit_zone(geo: &super::GroupGeometry, group: &crate::model::Group,
    point: (i32, i32), locked: bool, buttons: &[Rect]) -> EdgeZone {
    let edge = group.dock_edge();
    let ctx = ZoneCtx {
        dpi: geo.dpi,
        locked,
        folded: group.roll != RollState::Expanded,
        no_top: edge == Some(rollup::DockEdge::Top),
        no_bottom: edge == Some(rollup::DockEdge::Bottom),
        buttons,
    };
    snap::resize_zone(geo.union, geo.title, point, &ctx)
}

/// A key is down by the thread's key state or the physical (async) state.
fn key_down(key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    unsafe { GetKeyState(key.0 as i32) < 0 || GetAsyncKeyState(key.0 as i32) < 0 }
}

/// `MK_LBUTTON` in the `wParam` of a mouse message.
const MK_LBUTTON: usize = 0x0001;
const MK_RBUTTON: usize = 0x0002;
const MK_MBUTTON: usize = 0x0010;
/// `MK_SHIFT` in the `wParam` of a mouse message (posted QA messages can set it, unlike key state).
const MK_SHIFT: usize = 0x0004;

/// Edit-control subclass for naming: Enter commits, Escape cancels, focus loss commits.
pub(super) unsafe extern "system" fn naming_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM, _: usize, _: usize) -> LRESULT {
    if matches!(msg, WM_KEYDOWN | WM_KILLFOCUS) || msg == WM_APP + 1 {
        log(&format!("naming_proc msg={msg:#x} wparam={}", wparam.0));
    }
    let finish = match msg {
        WM_KEYDOWN if wparam.0 == VK_RETURN.0 as usize => Some(true),
        WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => Some(false),
        WM_KILLFOCUS => Some(true),
        _ => None,
    };
    if let Some(commit) = finish {
        unsafe {
            let _ = PostMessageW(hwnd, WM_APP + 1, WPARAM(commit as usize), LPARAM(0));
        }
        if msg == WM_KEYDOWN {
            return LRESULT(0);
        }
    }
    if msg == WM_APP + 1 {
        APP.with(|cell| {
            if let Ok(mut guard) = cell.try_borrow_mut() {
                if let Some(app) = guard.as_mut() {
                    if app.renaming.as_ref().is_some_and(|(edit, _, _)| *edit == hwnd) {
                        app.finish_rename(wparam.0 != 0);
                    } else {
                        app.finish_naming(wparam.0 != 0);
                    }
                }
            }
        });
        return LRESULT(0);
    }
    unsafe { DefSubclassProc(hwnd, msg, wparam, lparam) }
}

impl App {
    /// Text box for in-place naming/renaming at a parent-client rect: a child of the preview host,
    /// or a bordered popup owned by `owner` on the desktop (a child of a layered window would not
    /// be drawn).
    pub(super) fn create_edit(&self, text: &str, extra: WINDOW_STYLE, rect: Rect, owner: HWND) -> windows::core::Result<HWND> {
        let (x, y) = self.window_xy((rect.left, rect.top));
        let (style, ex, parent) = if self.detached() {
            (WS_POPUP | WS_VISIBLE | WS_BORDER | extra, WS_EX_TOOLWINDOW, owner)
        } else {
            (WS_CHILD | WS_VISIBLE | WS_BORDER | extra, WINDOW_EX_STYLE(0), self.parent)
        };
        unsafe { CreateWindowExW(ex, w!("EDIT"), &HSTRING::from(text), style, x, y, rect.width(), rect.height(), parent, HMENU::default(), self.instance, None) }
    }

    /// Pointer entered a group: it becomes the hover group (title darkening, controls).
    fn title_hover_enter(&mut self, id: u64) {
        if self.title_hot == Some(id) {
            return;
        }
        self.title_hot = Some(id);
        self.title_hover.entry(id).or_insert(0.0);
        self.title_clock.get_or_insert_with(Instant::now);
        unsafe {
            SetTimer(self.message_window, TIMER_TITLE_HOVER, 15, None);
        }
    }

    /// The pointer over the title strip and over a title button (corral-painting-spec 4.3, 6):
    /// repaints the window when either changes. `None` = the pointer left the window.
    pub(super) fn update_strip_hover(&mut self, index: usize, point: Option<(i32, i32)>) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{TRACKMOUSEEVENT, TrackMouseEvent, TME_LEAVE};
        let gw = &self.groups[index];
        let segment_hot = point.and_then(|p| self.segment_hit(index, p));
        let segment_changed = segment_hot != gw.segment_hot;
        let (strip_hot, hot) = match point {
            Some((x, y)) => {
                let hot = if gw.menu_glyph.contains(x, y) { Some(TitleButton::Menu) }
                    else if gw.up_glyph.contains(x, y) { Some(TitleButton::Up) }
                    else if gw.close_glyph.contains(x, y) { Some(TitleButton::Close) }
                    else { None };
                (gw.title.contains(x, y), hot)
            }
            None => (false, None),
        };
        if point.is_some() {
            let mut track = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: gw.hwnd, dwHoverTime: 0 };
            unsafe { let _ = TrackMouseEvent(&mut track); }
        }
        if segment_changed || (strip_hot, hot) != (gw.strip_hot, gw.hot_button) {
            let gw = &mut self.groups[index];
            gw.strip_hot = strip_hot;
            gw.hot_button = hot;
            gw.segment_hot = segment_hot;
            if let Some((member, _)) = segment_hot { gw.segment_hover.entry(member).or_insert(0.0); }
            if segment_changed {
                self.title_clock.get_or_insert_with(Instant::now);
                unsafe { SetTimer(self.message_window, TIMER_TITLE_HOVER, 15, None); }
            }
            self.render_group(index);
        }
    }

    /// 15 ms tick: drop the hover group when the pointer left its window, advance every hover level
    /// toward 250 (hover group) or 0 by the elapsed milliseconds, repaint the groups that changed.
    pub(super) fn step_title_hover(&mut self) {
        if let Some(id) = self.title_hot {
            let mut cursor = POINT::default();
            unsafe {
                let _ = GetCursorPos(&mut cursor);
            }
            let (ox, oy) = self.origin();
            let inside = self.groups.iter().find(|g| g.id == id).is_some_and(|g| g.rect.contains(cursor.x - ox, cursor.y - oy));
            if !inside && !matches!(self.drag, Drag::MoveGroup { .. } | Drag::ResizeGroup { .. }) {
                self.title_hot = None;
                if let Some(gw) = self.groups.iter_mut().find(|g| g.id == id) {
                    gw.strip_hot = false;
                    gw.hot_button = None;
                    gw.segment_hot = None;
                }
            }
        }
        let now = Instant::now();
        let elapsed = self.title_clock.map(|c| now.duration_since(c).as_secs_f64() * 1000.0).unwrap_or(0.0);
        self.title_clock = Some(now);
        let hot = self.title_hot;
        let mut changed = Vec::new();
        for gw in &mut self.groups {
            for (member, level) in &mut gw.segment_hover {
                let target = if hot == Some(gw.id) && gw.segment_hot.is_some_and(|(id, _)| id == *member) { 250.0 } else { 0.0 };
                let duration = motion::duration(if target > *level { crate::group_paint::REVEAL_IN_MS } else { crate::group_paint::REVEAL_OUT_MS });
                let next = if duration <= 0.0 { target } else if target > *level { (*level + elapsed * 250.0 / duration).min(target) }
                    else { (*level - elapsed * 250.0 / duration).max(target) };
                if next != *level { *level = next; if !changed.contains(&gw.id) { changed.push(gw.id); } }
            }
            gw.segment_hover.retain(|member, level| *level > 0.0 || gw.segment_hot.is_some_and(|(id, _)| id == *member));
        }
        for (id, level) in self.title_hover.iter_mut() {
            let target = if Some(*id) == hot { 250.0 } else { 0.0 };
            // Shown over 120 ms, hidden over 200 ms (corral-painting-spec 2.4); 0 while animations are off.
            let (up, down) = (motion::duration(crate::group_paint::REVEAL_IN_MS), motion::duration(crate::group_paint::REVEAL_OUT_MS));
            let next = if target > *level {
                if up <= 0.0 { target } else { (*level + elapsed * 250.0 / up).min(target) }
            } else if down <= 0.0 { target } else { (*level - elapsed * 250.0 / down).max(target) };
            if next != *level {
                *level = next;
                if !changed.contains(id) { changed.push(*id); }
            }
        }
        self.title_hover.retain(|id, level| *level > 0.0 || Some(*id) == hot);
        for id in changed {
            if let Some(i) = self.groups.iter().position(|g| g.id == id) {
                self.render_group(i);
            }
        }
        if self.title_hot.is_none() && self.title_hover.is_empty() && self.groups.iter().all(|g| g.segment_hover.is_empty()) {
            self.title_clock = None;
            unsafe {
                let _ = KillTimer(self.message_window, TIMER_TITLE_HOVER);
            }
        }
    }

    /// Fold toggle (title double-click, View > Fold): expanded <-> folded by the user (motion-sorting-spec 3).
    /// A fold that only comes from `RollupOnDock` is derived from the dock state: a double-click has no
    /// persistent effect on it and opens it for now.
    pub(super) fn toggle_roll(&mut self, id: u64) {
        let Some((roll, dock_edge)) = self.layout.groups.iter().find(|g| g.id == id).map(|g| (g.roll, g.dock_edge().is_some())) else { return };
        if roll == RollState::Docked && self.settings.rollup_on_dock_to_edge && self.settings.rollup_enabled {
            self.open_hot(id);
            return;
        }
        let current = self.roll_openness(id);
        if let Some(group) = self.layout.group_mut(id) {
            group.roll = rollup::toggle(group.roll);
        }
        let _ = self.layout.sync_tab_common(id);
        self.roll_clear(id);
        let rolled = self.is_rolled(id);
        // closing animates for every Corral; opening only where the folded geometry is shown (docked)
        if rolled || dock_edge {
            self.roll_progress.insert(id, current);
            self.roll_animate(id, !rolled);
        }
        if self.hot_group == Some(id) {
            self.hot_group = None;
        }
        // Folded under the pointer: stay folded until the pointer leaves (polled below).
        if rolled {
            self.roll_hover_suppressed = Some(id);
            unsafe {
                SetTimer(self.message_window, TIMER_HOVER_POLL, 30, None);
            }
        } else if self.roll_hover_suppressed == Some(id) {
            self.roll_hover_suppressed = None;
        }
        self.record_desk_edit(); self.save();
        if let Some(index) = self.groups.iter().position(|g| g.id == id) {
            self.render_group(index);
        }
    }

    pub(super) fn group_index(&self, hwnd: HWND) -> Option<usize> {
        self.groups.iter().position(|g| g.hwnd == hwnd)
    }

    /// Loose icon under `point` that the user can act on (not hidden by quick-hide): the choke
    /// point for press, drag, band and right-click paths.
    pub(super) fn item_at_surface(&self, point: (i32, i32)) -> Option<usize> {
        self.item_at_surface_raw(point).filter(|&i| !self.qh_name_hidden(&self.items[i].shell.parsing_name))
    }

    /// Loose icon cell under `point`, hidden or not (a hidden icon still counts as a double-click hit).
    fn item_at_surface_raw(&self, point: (i32, i32)) -> Option<usize> {
        let (cw, ch) = self.loose_cell();
        self.items.iter().position(|i| {
            self.group_of(&i.shell.parsing_name).is_none() && self.page_name_visible(&i.shell.parsing_name)
                && Rect::new(i.pos.0, i.pos.1, i.pos.0 + cw, i.pos.1 + ch).contains(point.0, point.1)
        })
    }

    pub(super) fn item_in_group(&self, index: usize, point: (i32, i32)) -> Option<String> {
        let group = self.layout.groups.iter().find(|g| g.id == self.groups[index].id)?;
        if !self.page_group_visible(group) { return None; }
        let body = self.groups[index].body;
        if !body.contains(point.0, point.1) {
            return None;
        }
        let dpi = self.group_dpi(group.id);
        let members = self.group_members(group);
        let slot = self.group_grid(group.id, dpi, body, group.scroll).hit_test(point, members.len())?;
        members
            .get(slot)
            .map(|i| i.shell.parsing_name.clone())
            .filter(|n| !self.qh_name_hidden(n) && self.page_name_visible(n))
    }

    pub(super) fn handle_surface(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if matches!(self.drag, Drag::Item { .. }) && (matches!(msg, WM_CANCELMODE | WM_CAPTURECHANGED)
            || (msg == WM_KEYDOWN && wparam.0 == VK_ESCAPE.0 as usize)) {
            self.cancel_item_drag();
            return Some(LRESULT(0));
        }
        if let Some(result) = self.peek_surface_input(msg) {
            return Some(result); // Peek on: a press on the empty desktop ends it (swallowed)
        }
        let point = lparam_point(lparam);
        match msg {
            WM_LBUTTONDOWN => {
                if self.draw_cancel_for_press(DrawButton::Left) { return Some(LRESULT(0)); } // a second button ends a live draw
                self.right_start = None;
                let mods = Modifiers::from_mouse_keys(wparam.0);
                let before = self.selected_keys();
                match self.item_at_surface(point) {
                    Some(index) => {
                        let name = self.items[index].shell.parsing_name.clone();
                        self.press_item(&name, mods);
                        let screen = self.to_screen(point);
                        let pos = self.items[index].pos;
                        // capture first: taking it from another window sends that window
                        // WM_CAPTURECHANGED, which must not see the new press
                        unsafe {
                            SetCapture(self.surface_hwnd);
                            self.peek_set_focus(self.surface_hwnd);
                        }
                        self.pages.drag_names.clear();
                        self.drag = Drag::Item {
                            name,
                            start: screen,
                            current: screen,
                            grab: (point.0 - pos.0, point.1 - pos.1),
                            active: false,
                            owner: self.surface_hwnd,
                        };
                    }
                    // a press that may start a draw leaves the selection alone until the gesture is known
                    // (creation-spec 2.1)
                    None if self.draw_begin(DrawButton::Left, point, wparam) => {}
                    None => {
                        // empty surface: a plain press clears the selection, Ctrl/Shift keep it for the band
                        self.begin_band_selection(mods, None);
                        unsafe {
                            SetCapture(self.surface_hwnd);
                            self.peek_set_focus(self.surface_hwnd);
                        }
                        self.drag = Drag::Pending { start: point };
                    }
                }
                self.repaint_selection(&before, true, None);
                Some(LRESULT(0))
            }
            WM_RBUTTONDOWN => {
                // a press that ends a live draw is no new gesture (and its release opens no menu)
                if self.draw_cancel_for_press(DrawButton::Right) { return Some(LRESULT(0)); }
                // Right press: remember it; decided at button release.
                self.right_start = Some(point);
                if let Some(index) = self.item_at_surface(point) {
                    let name = self.items[index].shell.parsing_name.clone();
                    let pos = self.items[index].pos;
                    self.begin_right_item(name, self.surface_hwnd, self.to_screen(point), (point.0 - pos.0, point.1 - pos.1));
                } else {
                    self.draw_begin(DrawButton::Right, point, wparam);
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                if matches!(self.drag, Drag::Item { active: true, .. }) {
                    // a right DRAG that did not hand over to OLE: end it (ghost included), no menu
                    // (file-safety review 2026-10-15, finding 4)
                    self.cancel_item_drag();
                    unsafe { let _ = ReleaseCapture(); }
                    self.right_start = None;
                    return Some(LRESULT(0));
                }
                if matches!(self.drag, Drag::Item { .. }) {
                    self.cancel_item_press();
                    unsafe { let _ = ReleaseCapture(); }
                }
                // a drawn rectangle ends here (chooser, or nothing); only a plain press-release goes on
                // to the desktop menu (creation-spec 2.2)
                let drawn = self.draw_finish(DrawButton::Right, point);
                if drawn == Some(DrawEnd::Done) {
                    self.right_start = None;
                    return Some(LRESULT(0));
                }
                if let Some(_start) = self.right_start.take() {
                    if let Some(index) = self.item_at_surface(point) {
                        let name = self.items[index].shell.parsing_name.clone();
                        self.select_for_menu(&name);
                        let names = self.menu_names(&name);
                        self.show_item_menu(self.surface_hwnd, names, point);
                    } else {
                        let before = self.selected_keys();
                        self.clear_selection();
                        self.repaint_selection(&before, true, None);
                        self.show_background_menu(point, None);
                    }
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                if matches!(self.drag, Drag::Item { .. }) {
                    let screen = self.to_screen(point);
                    self.update_item_drag(screen);
                    self.hand_item_drag_to_ole(wparam);
                    return Some(LRESULT(0));
                }
                if self.draw_active() {
                    self.draw_move(point, wparam);
                    return Some(LRESULT(0));
                }
                match self.drag {
                    // the button went up somewhere we did not see: no band starts without it
                    Drag::Pending { .. } if wparam.0 & MK_LBUTTON == 0 => self.cancel_drag(),
                    Drag::Pending { start } => {
                        let (tx, ty) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
                        if (point.0 - start.0).abs() > tx || (point.1 - start.1).abs() > ty {
                            self.drag = Drag::Band { start, current: point };
                            self.update_marquee_selection(start, point);
                        }
                    }
                    Drag::Band { start, .. } => {
                        self.drag = Drag::Band { start, current: point };
                        self.update_marquee_selection(start, point);
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                if matches!(self.drag, Drag::Item { .. }) {
                    let screen = self.to_screen(point);
                    self.finish_item_drag(screen);
                    return Some(LRESULT(0));
                }
                if let Some(end) = self.draw_finish(DrawButton::Left, point) {
                    // a plain press-release is an ordinary desktop click: it clears the selection now
                    // (creation-spec 2.2); a drawn rectangle already ended in the chooser or nothing
                    if end == DrawEnd::Click {
                        let before = self.selected_keys();
                        self.begin_band_selection(Modifiers::from_mouse_keys(0), None);
                        self.repaint_selection(&before, true, None);
                    }
                    return Some(LRESULT(0));
                }
                self.drag = Drag::None;
                unsafe {
                    let _ = ReleaseCapture();
                }
                size_tip::hide();
                self.snap_guides.clear();
                self.selection.band_base.clear();
                self.render_surface();
                Some(LRESULT(0))
            }
            // Esc, a cancelled mode or a lost capture ends a draw without creating or clicking (creation-spec 2.2)
            WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize && self.draw_active() => {
                self.draw_cancel(CancelReason::Escape);
                Some(LRESULT(0))
            }
            WM_CANCELMODE if self.draw_active() => {
                self.draw_cancel(CancelReason::CancelMode);
                Some(LRESULT(0))
            }
            WM_KEYDOWN if matches!(self.drag, Drag::None) => self.handle_selection_key(wparam.0 as u16, None),
            // Esc cancels a band (a modifier key pressed mid-band must not)
            WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize && matches!(self.drag, Drag::Band { .. } | Drag::Pending { .. }) => {
                self.cancel_drag();
                Some(LRESULT(0))
            }
            // The capture went elsewhere: a band, a pending band press and a press on an icon that has
            // not started dragging are over (an active icon drag is ended by its own hand-over/drop).
            WM_CAPTURECHANGED => {
                self.draw_cancel(CancelReason::CaptureLost);
                match self.drag {
                    Drag::Band { .. } | Drag::Pending { .. } => self.cancel_drag(),
                    Drag::Item { active: false, .. } => self.cancel_item_press(),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDBLCLK => {
                match self.item_at_surface_raw(point) {
                    // a hidden icon still counts as a hit: nothing happens
                    Some(index) => {
                        if self.qh_loose_alpha(&self.items[index]) > 0 {
                            let _ = shell::open_item(&self.items[index].shell.parsing_name);
                        }
                    }
                    // empty space: the Clear view toggle is decided by the click detector (quick_hide.rs)
                    None => {}
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    // ---- selection (rules in `selection.rs`) -------------------------------------------------

    /// Lower-case names of the selected items, for before/after comparisons.
    pub(super) fn selected_keys(&self) -> Vec<String> {
        self.items.iter().filter(|i| i.selected).map(|i| i.shell.parsing_name.to_lowercase()).collect()
    }

    fn clear_selection(&mut self) {
        for item in &mut self.items {
            item.selected = false;
        }
    }

    fn set_selected(&mut self, name: &str, on: bool) {
        if on && self.qh_name_hidden(name) {
            return;
        }
        if let Some(item) = self.items.iter_mut().find(|i| i.shell.parsing_name.eq_ignore_ascii_case(name)) {
            item.selected = on;
        }
    }

    fn is_selected(&self, name: &str) -> bool {
        self.items.iter().any(|i| i.selected && i.shell.parsing_name.eq_ignore_ascii_case(name))
    }

    /// Item names of one container in display order: a group's members, or the loose icons in
    /// column-major reading order (`home` = `None`).
    pub(super) fn container_order(&self, home: Option<u64>) -> Vec<String> {
        match home {
            Some(id) => self
                .layout
                .groups
                .iter()
                .find(|g| g.id == id)
                .map(|g| self.group_members(g).iter().map(|i| i.shell.parsing_name.clone()).collect())
                .unwrap_or_default(),
            None => {
                let loose: Vec<(usize, (i32, i32))> = self
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, i)| self.group_of(&i.shell.parsing_name).is_none() && self.page_name_visible(&i.shell.parsing_name))
                    .map(|(k, i)| (k, i.pos))
                    .collect();
                selection::visual_order(&loose, self.loose_cell()).into_iter().map(|k| self.items[k].shell.parsing_name.clone()).collect()
            }
        }
    }

    /// Selected items in display order: group by group (layout order), then the loose icons. Members
    /// of groups whose body is not on screen are left out, so no verb (Delete, Enter, menus, drags)
    /// may reach them through a selection made before the group rolled up. A hover-opened rolled
    /// group shows its body, so its members count (see `body_hidden`).
    pub(super) fn selected_in_display_order(&self) -> Vec<String> {
        let keys: Vec<String> = self.selected_keys().into_iter().filter(|k| !self.qh_name_hidden(k) && self.page_name_visible(k)).collect();
        let mut order = Vec::new();
        let homes = self
            .layout
            .groups
            .iter()
            .filter(|g| !self.body_hidden(g.id)) // Corrals and Gates (a Gate's items are its folder's files)
            .map(|g| Some(g.id))
            .chain(std::iter::once(None));
        for home in homes {
            order.extend(
                self.container_order(home)
                    .into_iter()
                    .filter(|n| keys.contains(&n.to_lowercase()))
                    // a loose icon saved off every monitor (older layouts, external drops) is invisible:
                    // never let Delete/Enter/menus/drags act on it
                    .filter(|n| home.is_some() || self.loose_on_screen(n)),
            );
        }
        // Two Gates on one folder both list a selected path: act on each file once.
        let mut seen = std::collections::HashSet::new();
        order.retain(|n| seen.insert(n.to_lowercase()));
        order
    }

    /// The loose icon `name` has its cell origin on some monitor.
    fn loose_on_screen(&self, name: &str) -> bool {
        let Some(item) = self.items.iter().find(|i| i.shell.parsing_name.eq_ignore_ascii_case(name)) else { return false };
        let (x, y) = self.to_screen(item.pos);
        self.monitors.iter().any(|m| m.bounds.contains(x, y))
    }

    /// The items an icon drag started on `pressed` carries: the whole selection when `pressed` is
    /// part of it, else just `pressed`.
    pub(super) fn drag_set(&self, pressed: &str) -> Vec<String> {
        if (matches!(self.drag, Drag::Item { active: true, .. }) || self.ole_drag.is_some())
            && self.pages.drag_names.iter().any(|n| n.eq_ignore_ascii_case(pressed)) {
            return self.pages.drag_names.clone();
        }
        let selected = self.selected_in_display_order();
        if selected.iter().any(|n| n.eq_ignore_ascii_case(pressed)) { selected } else { vec![pressed.to_string()] }
    }

    /// The selected items that fit one shell menu with `pressed`: they must share its parent folder
    /// (the user's and the public Desktop are different folders to the shell).
    pub(super) fn menu_names(&self, pressed: &str) -> Vec<String> {
        let (kept, dropped) = selection::sharing_parent_with(pressed, &self.drag_set(pressed));
        if dropped > 0 {
            log(&format!("item menu: {dropped} selected item(s) outside the folder of {pressed:?} left out"));
        }
        kept
    }

    /// Right-press on an item: an unselected item becomes the only selection, a selected one keeps it.
    fn select_for_menu(&mut self, name: &str) {
        if self.is_selected(name) {
            return;
        }
        let before = self.selected_keys();
        self.clear_selection();
        self.set_selected(name, true);
        self.selection.anchor = Some(name.to_string());
        self.selection.focus = Some(name.to_string());
        self.repaint_selection(&before, false, None);
    }

    /// Left press on item `name` (selection part): Explorer list-view rules, see `selection::plan_press`.
    fn press_item(&mut self, name: &str, mods: Modifiers) {
        let (action, release) = selection::plan_press(mods, self.is_selected(name));
        self.selection.release = release.map(|r| (r, name.to_string()));
        let single = |app: &mut App, keep_others: bool| {
            if !keep_others {
                app.clear_selection();
            }
            app.set_selected(name, true);
            app.selection.anchor = Some(name.to_string());
        };
        match action {
            PressAction::Replace => single(self, false),
            PressAction::Add => single(self, true),
            PressAction::Keep => self.selection.anchor = Some(name.to_string()),
            PressAction::Range { add } => {
                let order = self.container_order(self.group_of(name));
                let span = self.selection.anchor.as_deref().and_then(|anchor| selection::range(&order, anchor, name)).map(<[String]>::to_vec);
                match span {
                    Some(span) => {
                        if !add {
                            self.clear_selection();
                        }
                        for member in &span {
                            self.set_selected(member, true);
                        }
                    }
                    // no anchor in this container: the clicked item starts a new range
                    None => single(self, add),
                }
            }
        }
        self.selection.focus = Some(name.to_string());
    }

    /// Button released on an item without a drag: the deferred part of the press (a plain click on
    /// a selected item selects only it; Ctrl+click on a selected item deselects it).
    pub(super) fn apply_release(&mut self) {
        let Some((release, name)) = self.selection.release.take() else { return };
        let before = self.selected_keys();
        match release {
            Release::CollapseToPressed => {
                self.clear_selection();
                self.set_selected(&name, true);
                self.selection.anchor = Some(name.clone());
                self.selection.focus = Some(name);
            }
            Release::DeselectPressed => self.set_selected(&name, false),
        }
        self.repaint_selection(&before, false, None);
    }

    /// Repaint what a selection change touched: the surface when a loose icon changed (or
    /// `force_surface`), every group with a changed member, and `extra_group`.
    pub(super) fn repaint_selection(&mut self, before: &[String], force_surface: bool, extra_group: Option<u64>) {
        let after = self.selected_keys();
        if before.len() != after.len() || before.iter().any(|k| !after.contains(k)) {
            // QA evidence for posted-input trials (opt-in log)
            let short: Vec<&str> = after.iter().map(|n| n.rsplit(['\\', '/']).next().unwrap_or(n)).collect();
            log(&format!("selection: {} item(s) {short:?}", after.len()));
        }
        let mut surface = force_surface;
        let mut groups: Vec<u64> = extra_group.into_iter().collect();
        for key in before.iter().filter(|k| !after.contains(k)).chain(after.iter().filter(|k| !before.contains(k))) {
            match self.group_of(key) {
                Some(id) if !groups.contains(&id) => groups.push(id),
                Some(_) => {}
                None => surface = true,
            }
        }
        if surface {
            self.render_surface();
        }
        for id in groups {
            if let Some(index) = self.groups.iter().position(|g| g.id == id) {
                self.render_group(index);
            }
        }
    }

    /// Ctrl+A: every visible item (loose icons and the members of unrolled groups).
    fn select_all(&mut self) {
        let before = self.selected_keys();
        let hidden: Vec<String> = self
            .layout
            .groups
            .iter()
            .filter(|g| self.body_hidden(g.id))
            .flat_map(|g| match &g.content {
                GroupContent::Items { items } => items.iter().map(|n| n.to_lowercase()).collect::<Vec<_>>(),
                GroupContent::Portal { .. } => Vec::new(),
            })
            .collect();
        let qh_hidden: Vec<bool> = self.items.iter().map(|i| self.qh_name_hidden(&i.shell.parsing_name) || !self.page_name_visible(&i.shell.parsing_name)).collect();
        for (item, qh) in self.items.iter_mut().zip(qh_hidden) {
            item.selected = !qh && !hidden.contains(&item.shell.parsing_name.to_lowercase());
        }
        self.repaint_selection(&before, false, None);
    }

    /// Keyboard verbs on the selection (the surface and the groups share them): Enter opens every
    /// selected item, Delete recycles them, F2 renames the focused one, Esc clears, Ctrl+A selects all,
    /// Ctrl+C / Ctrl+X copy / cut the selection, Ctrl+V pastes into the desktop (into Corral `target`
    /// when the key reached a Corral, like Paste from its body menu).
    pub(super) fn handle_selection_key(&mut self, vk: u16, target: Option<u64>) -> Option<LRESULT> {
        let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
        if let Some(id) = target.filter(|id| self.group_view(*id) != crate::view_state::View::Icons) {
            use windows::Win32::UI::Input::KeyboardAndMouse::{VK_UP, VK_DOWN, VK_LEFT, VK_RIGHT, VK_HOME, VK_END, VK_PRIOR, VK_NEXT, VK_SPACE};
            if [VK_UP.0, VK_DOWN.0, VK_LEFT.0, VK_RIGHT.0, VK_HOME.0, VK_END.0, VK_PRIOR.0, VK_NEXT.0, VK_SPACE.0].contains(&vk) {
                let g = self.layout.groups.iter().find(|g| g.id == id)?;
                let geo = self.group_geometry(g);
                let grid = self.group_grid(id, geo.dpi, geo.full_body, g.scroll);
                let order = self.container_order(Some(id));
                if order.is_empty() { return Some(LRESULT(0)); }
                let focus = self.selection.focus.as_deref().and_then(|name| order.iter().position(|n| n.eq_ignore_ascii_case(name)));
                let at = focus.unwrap_or(0);
                use super::metrics::Nav;
                let nav = if vk == VK_HOME.0 { Nav::First } else if vk == VK_END.0 { Nav::Last }
                    else if vk == VK_UP.0 { Nav::Previous } else if vk == VK_DOWN.0 { Nav::Next }
                    else if vk == VK_PRIOR.0 { Nav::PageUp } else if vk == VK_NEXT.0 { Nav::PageDown } else { Nav::Stay };
                let slot = grid.key_slot(focus, nav, order.len())?;
                let before = self.selected_keys();
                let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
                if vk == VK_SPACE.0 && ctrl {
                    self.set_selected(&order[slot], !self.is_selected(&order[slot]));
                } else if shift {
                    if self.selection.anchor.is_none() { self.selection.anchor = Some(order[at].clone()); }
                    let span = self.selection.anchor.as_deref().and_then(|anchor| selection::range(&order, anchor, &order[slot])).unwrap_or(&order[slot..=slot]).to_vec();
                    if !ctrl { self.clear_selection(); }
                    for name in span { self.set_selected(&name, true); }
                } else if !ctrl {
                    self.clear_selection();
                    self.set_selected(&order[slot], true);
                    self.selection.anchor = Some(order[slot].clone());
                }
                self.selection.focus = Some(order[slot].clone());
                self.scroll_item_into_view(id, slot);
                self.repaint_selection(&before, false, Some(id));
                return Some(LRESULT(0));
            }
        }
        match vk {
            k if ctrl && (k == b'C' as u16 || k == b'X' as u16) => {
                let selected = self.selected_in_display_order();
                if !selected.is_empty() {
                    self.invoke_verb(&selected, if k == b'C' as u16 { "copy" } else { "cut" });
                }
            }
            k if ctrl && k == b'V' as u16 => self.paste_files(target, None),
            k if k == VK_RETURN.0 => {
                let names = self.selected_in_display_order();
                if let [name] = names.as_slice() { if self.gate_open(name, target) { return Some(LRESULT(0)); } }
                self.open_items(&names); // asks first above Explorer's multiple-invoke minimum
            }
            k if k == VK_DELETE.0 => {
                let selected = self.selected_in_display_order();
                self.invoke_verb(&selected, "delete");
            }
            k if k == VK_F2.0 => {
                if let Some(name) = self.selection.focus.clone().filter(|n| self.is_selected(n) && !self.qh_name_hidden(n) && self.page_name_visible(n)
                    && self.group_of(n).is_none_or(|id| !self.body_hidden(id))) {
                    self.begin_rename(name);
                }
            }
            k if k == VK_ESCAPE.0 => {
                let before = self.selected_keys();
                self.clear_selection();
                self.repaint_selection(&before, false, None);
            }
            k if k == b'A' as u16 && unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0 => self.select_all(),
            _ => return None,
        }
        Some(LRESULT(0))
    }

    /// Press on empty space (surface, or the body of group `group`): a plain press clears the
    /// selection, Ctrl/Shift keep it underneath the band that may follow.
    fn begin_band_selection(&mut self, mods: Modifiers, group: Option<u64>) {
        self.selection.band_group = group;
        if mods.keeps_selection() {
            self.selection.band_base = self.selected_keys();
        } else {
            self.clear_selection();
            self.selection.band_base.clear();
            self.selection.anchor = None;
            self.selection.focus = None;
        }
    }

    /// Marks `band_base` plus the items of `hits` (indices into `items`) as selected.
    fn select_band(&mut self, hits: &[usize]) {
        let base = std::mem::take(&mut self.selection.band_base);
        let qh_hidden: Vec<bool> = self.items.iter().map(|i| self.qh_name_hidden(&i.shell.parsing_name)).collect();
        for (k, item) in self.items.iter_mut().enumerate() {
            item.selected = !qh_hidden[k] && (hits.contains(&k) || base.contains(&item.shell.parsing_name.to_lowercase()));
        }
        self.selection.band_base = base;
    }

    /// Explorer-style marquee selection: loose icons whose cell touches the band are selected.
    fn update_marquee_selection(&mut self, start: (i32, i32), current: (i32, i32)) {
        let before = self.selected_keys();
        let band = Rect::from_points(start.0, start.1, current.0, current.1);
        let (cw, ch) = self.loose_cell();
        let cells: Vec<(usize, Rect)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, i)| self.group_of(&i.shell.parsing_name).is_none() && self.page_name_visible(&i.shell.parsing_name))
            .map(|(k, i)| (k, Rect::new(i.pos.0, i.pos.1, i.pos.0 + cw, i.pos.1 + ch)))
            .collect();
        let hits = selection::band_hits(band, &cells);
        self.select_band(&hits);
        self.repaint_selection(&before, true, None);
    }

    pub(super) fn cancel_drag(&mut self) {
        self.pages.drag_names.clear();
        let was_band = matches!(self.drag, Drag::Band { .. });
        self.drag = Drag::None;
        self.creation_preview = None;
        self.snap_guides.clear();
        size_tip::hide();
        unsafe {
            let _ = ReleaseCapture();
        }
        let before = self.selected_keys();
        if was_band {
            self.select_band(&[]); // Esc: back to the selection before the band
        }
        self.selection.band_group = None;
        self.repaint_selection(&before, true, None);
    }

    /// An icon press that never became a drag lost the mouse capture: nothing is left to finish, so
    /// the deferred part of the press (collapse / deselect on release) must not stay armed.
    fn cancel_item_press(&mut self) {
        self.pages.drag_names.clear();
        self.drag = Drag::None;
        self.selection.release = None;
    }

    fn begin_right_item(&mut self, name: String, owner: HWND, screen: (i32, i32), grab: (i32, i32)) {
        let before = self.selected_keys();
        self.select_for_menu(&name);
        unsafe { SetCapture(owner); self.peek_set_focus(owner); }
        self.pages.drag_names.clear();
        self.drag = Drag::Item { name, start: screen, current: screen, grab, active: false, owner };
        self.repaint_selection(&before, true, None);
    }

    // ---- bands inside a group body ----------------------------------------------------------

    /// A band or its pending press belongs to group `id`.
    fn group_band_active(&self, id: u64) -> bool {
        matches!(self.drag, Drag::Pending { .. } | Drag::Band { .. }) && self.selection.band_group == Some(id)
    }

    /// Press on empty space inside an ordinary group's body.
    fn begin_group_band(&mut self, index: usize, point: (i32, i32), mods: Modifiers) {
        let (id, hwnd) = (self.groups[index].id, self.groups[index].hwnd);
        let before = self.selected_keys();
        self.begin_band_selection(mods, Some(id));
        unsafe {
            SetCapture(hwnd);
            self.peek_set_focus(hwnd);
        }
        self.drag = Drag::Pending { start: point };
        self.repaint_selection(&before, false, Some(id));
    }

    /// Cells of the members of group `index` in group-window pixels, as (index into `items`, rect).
    fn group_cell_rects(&self, index: usize) -> Vec<(usize, Rect)> {
        let Some(group) = self.layout.groups.iter().find(|g| g.id == self.groups[index].id) else { return Vec::new() };
        let dpi = self.group_dpi(group.id);
        let grid = self.group_grid(group.id, dpi, self.groups[index].body, group.scroll);
        self.group_members(group)
            .iter()
            .enumerate()
            .filter_map(|(slot, member)| {
                let k = self.items.iter().position(|i| i.shell.parsing_name.eq_ignore_ascii_case(&member.shell.parsing_name))?;
                let r = grid.slot_rect(slot);
                if grid.view == crate::view_state::View::Details {
                    if r.overlap_area(&grid.body) == 0 { return None; }
                    return Some((k, r.clamp_into(&grid.body)));
                }
                Some((k, r))
            })
            .collect()
    }

    /// Mouse move while a band of group `index` is pending or running (`point` in group pixels).
    fn group_band_move(&mut self, index: usize, point: (i32, i32)) {
        let (id, mut body) = (self.groups[index].id, self.groups[index].body);
        if self.group_view(id) == crate::view_state::View::Details {
            let scroll = self.layout.groups.iter().find(|g| g.id == id).map_or(0, |g| g.scroll);
            body = self.group_grid(id, self.group_dpi(id), body, scroll).body;
        }
        let current = (point.0.clamp(body.left, (body.right - 1).max(body.left)), point.1.clamp(body.top, (body.bottom - 1).max(body.top)));
        let start = match self.drag {
            Drag::Pending { start } => {
                let (tx, ty) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
                if (point.0 - start.0).abs() <= tx && (point.1 - start.1).abs() <= ty {
                    return;
                }
                start
            }
            Drag::Band { start, .. } => start,
            _ => return,
        };
        self.drag = Drag::Band { start, current };
        let before = self.selected_keys();
        let cells = self.group_cell_rects(index);
        let hits = selection::band_hits(Rect::from_points(start.0, start.1, current.0, current.1), &cells);
        self.select_band(&hits);
        self.repaint_selection(&before, false, Some(id));
    }

    /// Button released (or the band cancelled with `restore`) in group `index`.
    fn end_group_band(&mut self, index: usize, restore: bool) {
        let was_band = matches!(self.drag, Drag::Band { .. });
        self.drag = Drag::None;
        unsafe {
            let _ = ReleaseCapture();
        }
        let before = self.selected_keys();
        if restore && was_band {
            self.select_band(&[]);
        }
        self.selection.band_group = None;
        self.selection.band_base.clear();
        self.repaint_selection(&before, false, Some(self.groups[index].id));
    }

    pub(super) fn begin_naming(&mut self, id: u64) {
        if self.layout.desktop_locked { return }
        if self.layout.groups.iter().any(|g| g.id == id && !tabs::body_visible(&self.layout.groups, g)) { self.select_tab(id); }
        let Some(gw) = self.groups.iter().find(|g| g.id == id) else { return };
        let mut strip = gw.title.offset(gw.rect.left, gw.rect.top);
        if let Some(group) = self.layout.groups.iter().find(|g| g.id == id) {
            let members = tabs::members(&self.layout.groups, group);
            let cells = self.tab_cells_of(group, gw.title, self.group_dpi(id));
            if members.len() > 1 {
                if let Some(cell) = cells.iter().find(|c| members[c.index as usize].id == id) { strip = cell.hit.offset(gw.rect.left, gw.rect.top); }
            }
        }
        let initial = self.layout.groups.iter().find(|g| g.id == id).map(|g| g.title.clone()).unwrap_or_default();
        let dpi = self.group_dpi(id);
        let r = strip;
        unsafe {
            let font = CreateFontW(-(style::TITLE_FONT_PX as i32 * dpi as i32 / 96), 0, 0, 0, 400, 0, 0, 0, 0, 0, 0, 5, 0, &HSTRING::from(style::FONT));
            let owner = self.groups.iter().find(|g| g.id == id).map(|g| g.hwnd).unwrap_or(self.surface_hwnd);
            let Ok(edit) = self.create_edit(&initial, WINDOW_STYLE(0x0001 /* ES_CENTER */ | 0x0080 /* ES_AUTOHSCROLL */),
                Rect::new(r.left + 2, r.top + 2, r.left + 2 + (r.width() - 4).max(40), r.top + 2 + (r.height() - 4).max(12)), owner)
            else {
                return;
            };
            SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
            let subclassed = SetWindowSubclass(edit, Some(naming_proc), 1, 0);
            log(&format!("begin_naming edit={:?} subclassed={}", edit.0, subclassed.as_bool()));
            let (z, z_extra) = self.peek_edit_zorder(); // HWND_TOP unless Peek is on
            let _ = SetWindowPos(edit, z, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | z_extra);
            let _ = SetFocus(edit);
            self.naming = Some((edit, id, font));
        }
    }

    fn finish_naming(&mut self, commit: bool) {
        log(&format!("finish_naming commit={commit} active={}", self.naming.is_some()));
        let Some((edit, id, font)) = self.naming.take() else { return };
        if commit {
            let mut buffer = [0u16; 260];
            let len = unsafe { GetWindowTextW(edit, &mut buffer) };
            // Enter / click-away commits the text as typed, even empty.
            let text = String::from_utf16_lossy(&buffer[..len.max(0) as usize]);
            if let Some(group) = self.layout.group_mut(id) {
                group.title = text;
            }
            self.record_desk_edit(); self.save();
        }
        unsafe {
            let _ = DestroyWindow(edit);
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
        if let Some(index) = self.groups.iter().position(|g| g.id == id) {
            self.render_group(index);
        }
    }

    /// The tab under `point` of window `index` (tabs-spec 4). The outer bands of the strip are the
    /// resize bands and never a tab.
    pub(super) fn tab_hit(&self, index: usize, point: (i32, i32)) -> Option<u64> {
        let gw = self.groups.get(index)?;
        let group = self.layout.groups.iter().find(|g| g.id == gw.id)?;
        if !tabs::body_visible(&self.layout.groups, group) || !self.page_group_visible(group)
            || self.pages.moving() || self.qh_group_alpha(group.id) != 255 { return None }
        if self.edge_zone(index, point).is_edge() { return None }
        if !gw.segments.is_empty() && self.segment_hit(index, point).is_some_and(|(_, part)| part != SegmentPart::Title) { return None }
        let members = tabs::members(&self.layout.groups, group);
        if members.len() < 2 { return None }
        let cells = self.tab_cells_of(group, gw.title, self.group_dpi(group.id));
        let hit = tabs::hit_tab(&cells, point)?;
        members.get(hit as usize).map(|g| g.id)
    }

    fn segment_hit(&self, index: usize, point: (i32, i32)) -> Option<(u64, SegmentPart)> {
        self.groups.get(index)?.segments.iter().find_map(|s|
            s.segment.hit_test(point, s.buttons, s.up_enabled).map(|part| (s.member, part)))
    }

    /// A segment button captures its member id, never the selected window's id. Releasing
    /// elsewhere cancels, as do capture loss and Esc; no title drag or tab switch is started.
    fn segment_button_input(&mut self, index: usize, msg: u32, point: (i32, i32), keys: usize) -> bool {
        let hit = self.segment_hit(index, point);
        if self.groups[index].segment_press.is_some() {
            let lost_press = msg == WM_MOUSEMOVE && keys & MK_LBUTTON == 0 && std::env::var_os("CORRAL_QA_POSTED_INPUT").is_none();
            let escape = (msg == WM_KEYDOWN && keys == VK_ESCAPE.0 as usize)
                || (msg == WM_MOUSEMOVE && unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(VK_ESCAPE.0 as i32) } < 0);
            if matches!(msg, WM_CAPTURECHANGED | WM_CANCELMODE) || lost_press || escape {
                self.groups[index].segment_press = None;
                unsafe { let _ = ReleaseCapture(); }
                self.render_group(index);
                return true;
            }
            if msg == WM_LBUTTONUP {
                let press = self.groups[index].segment_press.take();
                unsafe { let _ = ReleaseCapture(); }
                self.render_group(index);
                if let Some((member, part)) = press.filter(|p| Some(*p) == hit) {
                    match part {
                        SegmentPart::Menu => {
                            let rect = self.groups[index].rect;
                            self.show_group_menu(member, self.to_screen((point.0 + rect.left, point.1 + rect.top)));
                        }
                        SegmentPart::Close => self.confirm_remove_group(member),
                        SegmentPart::Up => self.portal_up_click(member),
                        SegmentPart::Title => {}
                    }
                }
                return true;
            }
            if msg == WM_MOUSEMOVE { return true }
        }
        if matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONDBLCLK) && matches!(self.drag, Drag::None) {
            if let Some(hit) = hit.filter(|(_, part)| *part != SegmentPart::Title) {
                self.clear_tab_hover();
                self.groups[index].segment_press = Some(hit);
                unsafe { SetCapture(self.groups[index].hwnd); }
                self.render_group(index);
                return true;
            }
        }
        false
    }

    /// The full text of a trimmed tab title as a tooltip, after the system hover time (tabs-spec 4). It
    /// ends the hover-switch wait, as any tooltip does.
    pub(super) fn tab_title_tip(&mut self, index: usize, point: (i32, i32)) {
        if !matches!(self.drag, Drag::None) || self.tab_press.is_some() { return }
        let Some(member) = self.tab_hit(index, point) else { return };
        let gw = &self.groups[index];
        let Some(group) = self.layout.groups.iter().find(|g| g.id == gw.id) else { return };
        let Some(at) = tabs::members(&self.layout.groups, group).iter().position(|m| m.id == member) else { return };
        let dpi = self.group_dpi(group.id);
        let cells = self.tab_cells_of(group, gw.title, dpi);
        let Some(cell) = cells.iter().find(|c| c.index as usize == at) else { return };
        let metrics = self.title_metrics_of(group, dpi);
        let (Some(title), Some(width)) = (metrics.titles.get(at), metrics.widths.get(at)) else { return };
        if title.is_empty() || *width <= cell.label.width() { return }
        let hwnd = gw.hwnd;
        self.clear_tab_hover();
        self.tab_tip = Some(member);
        size_tip::show(hwnd, title);
    }

    pub(super) fn hide_tab_tip(&mut self) {
        if self.tab_tip.take().is_some() { size_tip::hide(); }
    }

    /// The tab under `point` that is not the shown one.
    fn inactive_tab_hit(&self, index: usize, point: (i32, i32)) -> Option<u64> {
        let member = self.tab_hit(index, point)?;
        self.layout.groups.iter().find(|g| g.id == member)
            .filter(|g| !tabs::body_visible(&self.layout.groups, g)).map(|g| g.id)
    }

    pub(super) fn inactive_tab_window(&self, hwnd: HWND) -> bool {
        let member = self.groups.iter().find(|g| g.hwnd == hwnd).map(|g| g.id);
        member.and_then(|id| self.layout.groups.iter().find(|g| g.id == id)).is_some_and(|g| !tabs::body_visible(&self.layout.groups, g))
    }

    pub(super) fn arm_tab_save(&mut self) {
        self.record_desk_edit();
        self.tab_save_pending = true;
        unsafe { SetTimer(self.message_window, TIMER_TAB_SAVE, 2000, None); }
    }

    /// Hover switching restarts only after the pointer left the tab it rests on (tabs-spec 4): called
    /// after any switch and when a press, a menu or a drag cancels the wait.
    pub(super) fn clear_tab_hover(&mut self) {
        let mut cursor = POINT::default();
        let under = unsafe { GetCursorPos(&mut cursor) }.ok().and_then(|_| {
            let hwnd = unsafe { WindowFromPoint(cursor) };
            let index = self.groups.iter().position(|g| g.hwnd == hwnd)?;
            let (ox, oy) = self.origin();
            let rect = self.groups[index].rect;
            self.inactive_tab_hit(index, (cursor.x - ox - rect.left, cursor.y - oy - rect.top))
        });
        self.tab_hover_state = tabs::HoverState::after_switch(under, tick_ms());
        unsafe { let _ = KillTimer(self.message_window, TIMER_TAB_HOVER); }
    }

    /// Switching preserves the live portal object and all private member state (tabs-spec 4).
    pub(super) fn select_tab(&mut self, member: u64) {
        if self.pages.moving() { return }
        let Some(view) = tabs::collect(&self.layout.groups, member) else { return };
        if member == view.active || view.members.len() < 2 { return }
        match tabs::select(&self.layout.tab_doc(), member) {
            Ok(doc) => {
                if self.layout.apply_tab_doc(doc) {
                    self.transfer_tab_runtime(view.active, member);
                    self.arm_tab_save();
                    self.refresh_tabs();
                    self.clear_tab_hover();
                }
            }
            Err(e) => log(&format!("tab select rejected: {e:?}")),
        }
    }

    /// Transient roll/title references follow the visible representation, not hidden HWNDs.
    pub(super) fn transfer_tab_runtime(&mut self, old: u64, new: u64) {
        if old == new { return }
        // The set changes HWND on selection; its segment fades follow it so a click or
        // spring switch does not blink the controls beneath a stationary pointer.
        let segment_state = self.groups.iter_mut().find(|g| g.id == old).map(|g|
            (g.segment_hot.take(), std::mem::take(&mut g.segment_hover)));
        if let (Some((hot, hover)), Some(g)) = (segment_state, self.groups.iter_mut().find(|g| g.id == new)) {
            g.segment_hot = hot;
            g.segment_hover = hover;
            g.segment_press = None;
        }
        for slot in [&mut self.hot_group, &mut self.pending_hover, &mut self.roll_hover_suppressed, &mut self.title_hot] {
            if *slot == Some(old) { *slot = Some(new); }
        }
        for map in [&mut self.roll_progress, &mut self.title_hover] {
            map.remove(&new);
            if let Some(value) = map.remove(&old) { map.insert(new, value); }
        }
        self.roll_target.remove(&new);
        if let Some(value) = self.roll_target.remove(&old) { self.roll_target.insert(new, value); }
        self.roll_tween.remove(&new);
        if let Some(value) = self.roll_tween.remove(&old) { self.roll_tween.insert(new, value); }
        let mut api = WinTimers(self.message_window);
        self.roll_gate.end(old, &mut api);
        if self.roll_tween.contains_key(&new) { self.roll_gate.start(new, &mut api); }
    }

    pub(super) fn transfer_tab_runtime_for_join(&mut self, target: u64, source: u64, incoming: &[u64]) {
        for id in incoming {
            self.roll_progress.remove(id);
            self.roll_target.remove(id);
            self.title_hover.remove(id);
            for slot in [&mut self.hot_group, &mut self.pending_hover, &mut self.roll_hover_suppressed, &mut self.title_hot] {
                if *slot == Some(*id) { *slot = None; }
            }
        }
        self.transfer_tab_runtime(target, source);
        self.clear_tab_hover();
    }

    pub(super) fn refresh_tabs(&mut self) {
        // No load_items/file transfer. Existing Gate listings stay allocated.
        self.sync_group_windows();
        self.arrange_loose();
        self.render_surface();
        for i in 0..self.groups.len() { self.render_group(i); }
        self.layout_portal_popups();
    }

    /// Hover switching (tabs-spec 4): the pointer entering an inactive tab starts the wait; the pure
    /// step machine decides when it selects.
    fn update_tab_hover(&mut self, index: usize, point: (i32, i32), buttons: bool) {
        let wait = tabs::TabSettings::from(&self.settings).hover_wait(false);
        let under = wait.and(self.inactive_tab_hit(index, point));
        let cancel = buttons || !matches!(self.drag, Drag::None) || self.tab_press.is_some();
        let (state, out) = tabs::hover_switch_step(self.tab_hover_state,
            tabs::HoverInput { tab: under, now_ms: tick_ms(), wait_ms: wait, cancel });
        self.tab_hover_state = state;
        match out {
            tabs::HoverOut::Idle => unsafe { let _ = KillTimer(self.message_window, TIMER_TAB_HOVER); },
            tabs::HoverOut::Wait(ms) => unsafe { SetTimer(self.message_window, TIMER_TAB_HOVER, ms.clamp(1, 30), None); },
            tabs::HoverOut::Select(member) => {
                unsafe { let _ = KillTimer(self.message_window, TIMER_TAB_HOVER); }
                self.select_tab(member);
            }
        }
    }

    pub(super) fn poll_tab_hover(&mut self) {
        let mut cursor = POINT::default();
        if unsafe { GetCursorPos(&mut cursor) }.is_err() { self.clear_tab_hover(); return }
        if matches!(self.drag, Drag::Item { active: true, .. }) {
            // An icon drag held still over a tab keeps the spring timer running (tabs-spec 4).
            self.spring_tab(Some((cursor.x, cursor.y)));
            return;
        }
        let under = unsafe { WindowFromPoint(cursor) };
        let Some(index) = self.groups.iter().position(|g| g.hwnd == under) else { self.clear_tab_hover(); return };
        let (ox, oy) = self.origin();
        let rect = self.groups[index].rect;
        self.update_tab_hover(index, (cursor.x - ox - rect.left, cursor.y - oy - rect.top), key_down(VK_LBUTTON) || key_down(VK_RBUTTON));
    }

    /// The strips a dragged Corral could be dropped on, topmost first (tabs-spec 3.1), in parent-client
    /// pixels: every visible strip except the dragged Corral's own set, with its painted tab cells and
    /// the refusal that applies to it.
    fn tab_drop_strips(&self, source: u64) -> Vec<tabs::StripInfo> {
        let doc = self.layout.tab_doc();
        let policy = tabs::TabSettings::from(&self.settings).join_policy(self.settings.label_mode != 0);
        let own = tabs::collect(&self.layout.groups, source).map(|v| v.members).unwrap_or_default();
        self.groups.iter().rev().filter_map(|gw| {
            let group = self.layout.groups.iter().find(|g| g.id == gw.id)?;
            if own.contains(&group.id) || !tabs::body_visible(&self.layout.groups, group) || !self.page_group_visible(group)
                || self.qh_group_alpha(group.id) != 255 || gw.title.width() <= 0 || gw.title.height() <= 0 { return None }
            let (dx, dy) = (gw.rect.left, gw.rect.top);
            let cells = self.tab_cells_of(group, gw.title, self.group_dpi(group.id)).into_iter()
                .map(|c| tabs::TabCell { index: c.index, hit: c.hit.offset(dx, dy), label: c.label.offset(dx, dy) }).collect();
            let tab_count = tabs::members(&self.layout.groups, group).len().max(1);
            let refusal = match tabs::check_join(&doc, group.id, source, &policy) {
                Err(tabs::TabError::Refused(reason)) => Some(reason),
                _ => None,
            };
            Some(tabs::StripInfo { id: group.id, strip: gw.title.offset(dx, dy), cells, tabs: tab_count, refusal })
        }).collect()
    }

    fn begin_tab_title(&mut self, index: usize, point: (i32, i32), keys: usize) {
        if self.profile_pending.is_some() { return; }
        self.hide_tab_tip();
        self.clear_tab_hover();
        let id = self.groups[index].id;
        let origin = self.groups[index].rect;
        let client = (point.0 + origin.left, point.1 + origin.top);
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id).cloned() else { return };
        // The press is a transaction: Esc or Cancel puts all of this back (tabs-spec 3.4, 5.3).
        let gesture = super::TabGesture {
            undo: super::capture_resize_state(&self.layout, &self.runtime_bodies, id),
            doc: self.layout.tab_doc(),
            arm: tabs::ArmState::default(), armed: None, client,
            torn: false, reordered: false, scroll_anchor: None, last_scroll: None,
            ids: tabs::collect(&self.layout.groups, id).map(|v| v.members).unwrap_or_else(|| vec![id]),
        };
        let geo = self.group_geometry(&group);
        let requested_at_press = self.requested_body(&group);
        let (drag_body, drag_below) = if let Some(edge) = group.dock_edge() {
            let (ox, oy) = self.origin();
            let work = self.monitor_for(&group.monitor).map_or(geo.full_body, |m| m.work.offset(-ox, -oy));
            edge.drag_body(geo.full_body, geo.title, geo.below, geo.metrics.gap, work)
        } else { (geo.full_body, geo.below) };
        if let Some(g) = self.layout.group_mut(id) { g.title_side_last = geo.below as i32; }
        let size_dip = self.preferred_size(id);
        let cell = self.tab_hit(index, point).and_then(|member| {
            let members = tabs::members(&self.layout.groups, &group);
            let at = members.iter().position(|m| m.id == member)?;
            let cells = self.tab_cells_of(&group, self.groups[index].title, geo.dpi);
            cells.iter().find(|c| c.index as usize == at).map(|c| (member, c.hit.offset(origin.left, origin.top)))
        });
        let member = cell.map_or(id, |(member, _)| member);
        // A locked title permits selection, never a move or resize.
        if self.layout.desktop_locked { self.select_tab(member); return; }
        // Shift at the press on a tab starts reorder / tear-off; it is read only here (tabs-spec 5.1).
        let shift = keys & MK_SHIFT != 0 || key_down(VK_SHIFT);
        let tab_cell = cell.map(|(_, hit)| hit).unwrap_or_default();
        self.tab_press = Some(TabPress { owner: id, member, moving: id, press: client,
            shift_tab: shift && cell.is_some(), on_tab: cell.is_some(), tab_left: tab_cell.left,
            tab_centre: tab_cell.left + tab_cell.width() / 2, strip_width: geo.title.width(),
            gated: cell.is_some() || self.is_rolled(id), started: false, page: self.pages.current,
            portal_up: false, portal_glyph: false });
        self.tab_gesture = Some(gesture);
        if self.settings.rollup_require_click && self.is_rolled(id) { self.open_hot(id); }
        let grab = crate::dpi_transition::Grab {
            px: (((client.0 - drag_body.left) as f64 * size_dip.0 as f64 / group.rect.width().max(1) as f64).round() as i32,
                if client.1 > drag_body.top {
                    ((client.1 - drag_body.top) as f64 * size_dip.1 as f64 / group.rect.height().max(1) as f64).round() as i32
                } else { client.1 - drag_body.top }), dpi: geo.dpi,
            size_dip };
        if let Some(r) = requested_at_press { self.apply_body_placement(id, r); }
        self.drag = Drag::MoveGroup { id, grab, moved: false, undock_below: drag_below };
        // No pixels are rebuilt at the press: the interaction surface (Camouflage overlay, portal
        // ghost) is published by the first step that really moves the Corral.
        self.drag_overlay = None;
        unsafe { SetCapture(self.groups[index].hwnd); self.peek_set_focus(self.groups[index].hwnd); }
    }

    fn tab_title_move(&mut self, _index: usize, client: (i32, i32), keys: usize) {
        let Some(mut press) = self.tab_press else { return };
        let carrying = self.pages_carrying_title();
        if (!carrying && (press.page != self.pages.current || self.pages.moving())) || self.layout.desktop_locked {
            self.finish_tab_title(client, false); return;
        }
        if !press.started {
            if client == press.press { return }
            if press.gated {
                // Half of the system drag rectangle, at least 4 DIP (tabs-spec 5.1).
                let dpi = self.group_dpi(press.owner);
                let (cx, cy) = unsafe { (windows::Win32::UI::HiDpi::GetSystemMetricsForDpi(SM_CXDRAG, dpi), windows::Win32::UI::HiDpi::GetSystemMetricsForDpi(SM_CYDRAG, dpi)) };
                if !tabs::beyond_threshold(press.press, client, tabs::drag_threshold(cx, cy, dpi)) { return }
            }
            press.started = true;
            self.tab_press = Some(press);
        }
        if let Some(gesture) = self.tab_gesture.as_mut() { gesture.client = client; }
        let torn = self.tab_gesture.as_ref().is_some_and(|g| g.torn);
        if press.shift_tab && !torn {
            let dpi = self.group_dpi(press.owner);
            let Some(strip) = self.groups.iter().find(|g| g.id == press.owner).map(|gw| gw.title.offset(gw.rect.left, gw.rect.top)) else { return };
            if tabs::tears(strip, client, dpi) {
                self.tear_tab(client);
                self.tab_move_corral(client, keys);
            } else {
                self.reorder_step(client);
            }
            return;
        }
        // A plain title drag (or the torn tab): move the Corral, then look for a drop zone.
        self.tab_move_corral(client, keys);
    }

    /// One step of the Corral move that belongs to this press, then the drop-zone check.
    fn tab_move_corral(&mut self, client: (i32, i32), keys: usize) {
        let Some(press) = self.tab_press else { return };
        let shift = keys & MK_SHIFT != 0 || key_down(VK_SHIFT);
        if let (Some(mi), Drag::MoveGroup { grab, .. }) = (self.groups.iter().position(|g| g.id == press.moving), &self.drag) {
            let grab = *grab;
            self.move_step(mi, client, grab, shift);
        }
        self.tab_zone_update(client);
    }

    /// Shift-drag on a tab inside the strip: the dragged tab changes place when its centre crosses a
    /// neighbour's centre (tabs-spec 5.2). The change is live in memory; the release commits it, Esc
    /// puts the original order back. Near an end of a crowded strip the strip scrolls one tab per 200 ms.
    fn reorder_step(&mut self, client: (i32, i32)) {
        let Some(press) = self.tab_press else { return };
        let Some(oi) = self.groups.iter().position(|g| g.id == press.owner) else { return };
        let Some(group) = self.layout.groups.iter().find(|g| g.id == press.owner).cloned() else { return };
        let members = tabs::members(&self.layout.groups, &group);
        let (count, shown) = (members.len(), members.iter().position(|m| m.id == group.id).unwrap_or(0));
        let Some(current) = members.iter().position(|m| m.id == press.member) else { return };
        let strip = self.groups[oi].title.offset(self.groups[oi].rect.left, self.groups[oi].rect.top);
        let dpi = self.group_dpi(group.id);
        // Autoscroll: the strip keeps another tab in view.
        let dir = tabs::autoscroll_dir(strip, client.0, dpi);
        if let Some(gesture) = self.tab_gesture.as_mut() {
            let (last, step) = tabs::autoscroll_step(gesture.last_scroll, tick_ms(), dir);
            gesture.last_scroll = last;
            if step != 0 {
                let base = gesture.scroll_anchor.unwrap_or(shown) as i32;
                gesture.scroll_anchor = Some((base + step).clamp(0, count as i32 - 1) as usize);
                self.render_group(oi);
            }
        }
        unsafe {
            if dir != 0 { SetTimer(self.message_window, TIMER_TAB_SCROLL, 50, None); }
            else { let _ = KillTimer(self.message_window, TIMER_TAB_SCROLL); }
        }
        let gw = &self.groups[oi];
        let cells = self.tab_cells_of(&group, gw.title, dpi);
        let Some(slot) = cells.iter().position(|c| c.index as usize == current) else { return };
        let centres: Vec<i32> = cells.iter().map(|c| c.hit.left + c.hit.width() / 2 + gw.rect.left).collect();
        // The dragged tab keeps its offset under the pointer and stays inside the strip.
        let dragged = (client.0 - (press.press.0 - press.tab_centre)).clamp(strip.left, strip.right);
        let target = tabs::reorder_target(&centres, slot, dragged);
        if target == slot { return }
        let to = cells[target].index as usize;
        match tabs::reorder(&self.layout.tab_doc(), press.member, to) {
            Ok(doc) => {
                if self.layout.apply_tab_doc(doc) {
                    if let Some(gesture) = self.tab_gesture.as_mut() { gesture.reordered = true; }
                    self.refresh_tabs();
                }
            }
            Err(e) => log(&format!("tab reorder rejected: {e:?}")),
        }
    }

    /// The scroll timer fired with the pointer at rest near an end of the strip.
    pub(super) fn tab_scroll_poll(&mut self) {
        match self.tab_gesture.as_ref().map(|g| g.client) {
            Some(client) if self.tab_press.is_some_and(|p| p.shift_tab) => self.reorder_step(client),
            _ => unsafe { let _ = KillTimer(self.message_window, TIMER_TAB_SCROLL); },
        }
    }

    /// The pointer is more than 32 DIP from the strip: the tab becomes its own Corral under the
    /// pointer and the gesture continues as an ordinary Corral move (tabs-spec 5.3).
    fn tear_tab(&mut self, client: (i32, i32)) {
        let Some(mut press) = self.tab_press else { return };
        let Some(view) = tabs::collect(&self.layout.groups, press.member).filter(|v| v.members.len() > 1) else { return };
        let Some(shown) = self.layout.groups.iter().find(|g| g.id == view.active).cloned() else { return };
        let place = tabs::Placement { monitor: shown.monitor.clone(), rect: shown.rect };
        let doc = match tabs::remove_tab(&self.layout.tab_doc(), press.member, &place) {
            Ok(doc) => doc,
            Err(e) => { log(&format!("tab tear-off rejected: {e:?}")); return }
        };
        if !self.layout.apply_tab_doc(doc) { return }
        let torn = press.member;
        if view.active == torn {
            if let Some(next) = view.members.iter().copied().find(|id| *id != torn).and_then(|m| tabs::collect(&self.layout.groups, m)) {
                self.transfer_tab_runtime(torn, next.active);
            }
        }
        self.roll_progress.remove(&torn);
        self.roll_target.remove(&torn);
        self.refresh_tabs();
        // The pointer keeps its offset inside the strip it had inside the tab (x clamped to the strip).
        let Drag::MoveGroup { mut grab, undock_below, .. } = self.drag else { return };
        let ratio = grab.size_dip.0 as f64 / shown.rect.width().max(1) as f64;
        let inside = (press.press.0 - press.tab_left).clamp(0, press.strip_width.max(0));
        grab.px.0 = (inside as f64 * ratio).round() as i32;
        self.drag = Drag::MoveGroup { id: torn, grab, moved: false, undock_below };
        self.drag_overlay = None;
        press.moving = torn;
        self.tab_press = Some(press);
        if let Some(gesture) = self.tab_gesture.as_mut() { gesture.torn = true; gesture.client = client; }
        if let Some(i) = self.groups.iter().position(|g| g.id == torn) { self.raise_group(i); }
    }

    /// Where the dragged Corral could be combined (tabs-spec 3.1, 3.2): the zone under the pointer
    /// arms after 120 ms and shows the accent outline, the insertion mark and the label.
    fn tab_zone_update(&mut self, client: (i32, i32)) {
        let Some(press) = self.tab_press else { return };
        let moved = self.group_drag_of(press.moving) == Some(true);
        let strips = if moved && press.started { self.tab_drop_strips(press.moving) } else { Vec::new() };
        let zone = tabs::drop_zone(&strips, client);
        let Some(gesture) = self.tab_gesture.as_mut() else { return };
        gesture.client = client;
        let (arm, armed) = tabs::arm_step(gesture.arm, zone.map(|z| z.target), tick_ms());
        let now = zone.filter(|_| armed);
        let previous = gesture.armed;
        gesture.arm = arm;
        gesture.armed = now;
        if zone.is_some() && !armed {
            let left = tabs::ARM_MS.saturating_sub(tick_ms().saturating_sub(arm.since_ms)).max(1) as u32;
            unsafe { SetTimer(self.message_window, TIMER_TAB_ARM, left, None); }
        } else {
            unsafe { let _ = KillTimer(self.message_window, TIMER_TAB_ARM); }
        }
        if previous != now { self.show_armed_zone(previous, now, press.moving); }
    }

    /// The arming timer fired with the pointer at rest: look again.
    pub(super) fn tab_zone_poll(&mut self) {
        unsafe { let _ = KillTimer(self.message_window, TIMER_TAB_ARM); }
        if let Some(client) = self.tab_gesture.as_ref().map(|g| g.client) { self.tab_zone_update(client); }
    }

    /// Feedback of an armed zone (tabs-spec 3.2): overlay on the target, 70 % dragged window, label.
    fn show_armed_zone(&mut self, previous: Option<tabs::ZoneHit>, now: Option<tabs::ZoneHit>, moving: u64) {
        self.tab_overlay = now.map(|z| (z.target, z.is_set.then_some(z.insertion)));
        self.tab_fade = now.map(|_| moving);
        match now {
            Some(zone) => {
                let count = |id| tabs::collect(&self.layout.groups, id).map_or(1, |v| v.members.len());
                size_tip::show(self.message_window, tabs::join_label(count(zone.target), count(moving)));
            }
            None => size_tip::hide(),
        }
        let mut ids = vec![moving];
        ids.extend(previous.map(|z| z.target));
        ids.extend(now.map(|z| z.target));
        ids.dedup();
        for id in ids {
            if let Some(i) = self.groups.iter().position(|g| g.id == id) { self.render_group(i); }
        }
    }

    /// Cancel a tab gesture: the data and the geometry of the press come back (tabs-spec 3.4, 5.3).
    fn restore_tab_gesture(&mut self, gesture: &super::TabGesture) {
        // Only what the gesture changed comes back; items assigned or renamed meanwhile stay.
        let doc = tabs::restore_members(&self.layout.tab_doc(), &gesture.doc, &gesture.ids);
        self.layout.apply_tab_doc(doc);
        super::restore_resize_state(&mut self.layout, &mut self.runtime_bodies, &gesture.undo);
        self.snap_guides.clear();
        self.snap_lock = snap::SnapLock::default();
        self.clear_tab_hover();
        self.refresh_tabs();
    }

    /// Lost capture / Escape cancels the gesture; a release finishes it (tabs-spec 3, 5).
    /// Esc during a title drag: a plain move puts the Corral back where the press found it, then the
    /// drag ends as cancelled (tab gestures restore themselves in `finish_tab_title`).
    fn escape_tab_title(&mut self, client: (i32, i32)) {
        if let (Some(press), Some(gesture)) = (self.tab_press, self.tab_gesture.as_ref()) {
            if !press.shift_tab && !gesture.torn && !gesture.reordered && gesture.armed.is_none() {
                super::restore_resize_state(&mut self.layout, &mut self.runtime_bodies, &gesture.undo);
            }
        }
        self.finish_tab_title(client, false);
    }

    fn finish_tab_title(&mut self, client: (i32, i32), accept: bool) {
        let Some(press) = self.tab_press.take() else { return };
        let Some(gesture) = self.tab_gesture.take() else { return };
        unsafe { let _ = KillTimer(self.message_window, TIMER_TAB_ARM); let _ = KillTimer(self.message_window, TIMER_TAB_SCROLL); }
        let (overlay, fade) = (self.tab_overlay.take(), self.tab_fade.take());
        size_tip::hide();
        let valid = accept && press.page == self.pages.current && !self.pages.moving() && !self.layout.desktop_locked;
        let armed = gesture.armed.filter(|_| valid && press.started);
        // The combine attempt and the other tab gestures end their drag without saving first.
        let ends_alone = armed.is_some() || (!accept && (gesture.torn || gesture.reordered || gesture.armed.is_some()))
            || (press.shift_tab && !gesture.torn);
        if ends_alone {
            self.drag = Drag::None;
            self.drag_render_stop();
            unsafe { let _ = ReleaseCapture(); }
            self.snap_guides.clear();
            self.snap_lock = snap::SnapLock::default();
        }
        // Take the overlay and the faded window off the screen.
        for id in overlay.map(|(target, _)| target).into_iter().chain(fade) {
            if let Some(i) = self.groups.iter().position(|g| g.id == id) { self.render_group(i); }
        }
        if let Some(zone) = armed {
            if !self.combine_dropped(zone, press.moving) { self.restore_tab_gesture(&gesture); }
            self.render_surface();
            return;
        }
        if !accept && (gesture.torn || gesture.reordered || gesture.armed.is_some()) {
            self.restore_tab_gesture(&gesture);
            self.render_surface();
            return;
        }
        if press.shift_tab && !gesture.torn {
            // A reorder (or a Shift click): no Corral moved.
            self.record_drag_edit(false);
            if gesture.reordered {
                // Write first, swap after: the layout goes back to the press for the set, then takes the order.
                let order = self.layout.tab_doc();
                let pressed = tabs::restore_members(&order, &gesture.doc, &gesture.ids);
                self.layout.apply_tab_doc(pressed);
                if !self.commit_tab_edit(order) { self.restore_tab_gesture(&gesture); }
            }
            self.render_surface();
        } else {
            let mut moved = self.group_drag_of(press.moving).unwrap_or(false);
            // Esc after the view was carried to another Range puts the Range and the rect back (spec 3.4).
            if !accept && self.pages_carried() {
                self.pages_abort_carry();
                super::restore_resize_state(&mut self.layout, &mut self.runtime_bodies, &gesture.undo);
                moved = false;
            }
            // A tear-off is written through `commit_tab_edit` once the Corral has landed (tabs-spec 7).
            self.tab_defer_save = gesture.torn && accept;
            self.end_group_drag_with_rescue(press.moving, moved, accept, accept.then_some(self.to_screen(client)));
            if self.tab_defer_save {
                self.tab_defer_save = false;
                let landed = self.layout.tab_doc();
                let pressed = tabs::restore_members(&landed, &gesture.doc, &gesture.ids);
                self.layout.apply_tab_doc(pressed);
                if self.commit_tab_edit(landed) { self.record_desk_edit(); } else { self.restore_tab_gesture(&gesture); }
            }
        }
        if !valid || press.started { return }
        let Some(index) = self.groups.iter().position(|g| g.id == press.owner) else { return };
        let rect = self.groups[index].rect;
        let point = (client.0 - rect.left, client.1 - rect.top);
        if press.portal_glyph {
            let member = press.member;
            if press.portal_up {
                if self.groups[index].up_glyph.contains(point.0, point.1) { self.portal_up_click(member); }
            } else if self.groups[index].glyph.contains(point.0, point.1) {
                let glyph = self.groups[index].glyph;
                let (x, y) = self.to_screen((glyph.left + rect.left, glyph.top + rect.top));
                self.portal_glyph_click(member, Rect::new(x, y, x + glyph.width(), y + glyph.height()));
            }
        } else if press.on_tab && self.tab_hit(index, point) == Some(press.member) {
            // A click on a tab: press and release on the same tab, before the drag threshold.
            self.select_tab(press.member);
        }
    }

    pub(super) fn handle_group(&mut self, index: usize, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        let point = lparam_point(lparam);
        // Every press raises the hit group to the top of the z-order first.
        let index = if matches!(msg, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_LBUTTONDBLCLK) { self.raise_group(index) } else { index };
        let id = self.groups[index].id;
        if self.header_input(index, msg, wparam.0, point) { return Some(LRESULT(1)); }
        if matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK) && matches!(self.drag, Drag::None)
            && self.portals.get(&id).is_some_and(|v| v.error.is_some()) {
            let buttons = crate::portal::error_buttons(self.groups[index].body, self.group_dpi(id));
            if let Some(button) = buttons.iter().position(|r| r.contains(point.0, point.1)) {
                if msg == WM_LBUTTONUP {
                    if button == 0 { self.portal_retry(id); } else { self.portal_choose_folder(id); }
                }
                return Some(LRESULT(0));
            }
        }
        if msg == WM_APPCOMMAND && self.portals.contains_key(&id) {
            let input = match ((lparam.0 >> 16) & 0x7ff) as u32 {
                1 => Some(crate::gate_host::Input::AppBack),
                2 => Some(crate::gate_host::Input::AppForward),
                _ => None,
            };
            if let Some(step) = input.and_then(|input| self.route_gate_input(id, input)) {
                self.portal_navigate(id, step);
                return Some(LRESULT(1));
            }
        }
        // scrollbar-spec 3/4: parent hover includes the title; Esc belongs to an active thumb drag.
        if let Some(bar) = self.scrollbars.get(&id) {
            if msg == WM_KEYDOWN && wparam.0 == VK_ESCAPE.0 as usize && bar.cancel_drag() {
                return Some(LRESULT(0));
            }
            if matches!(msg, WM_KILLFOCUS | WM_CANCELMODE) { bar.end_press(); }
            if msg == WM_MOUSEMOVE || msg == 0x02a3 {
                bar.parent_pointer();
                if msg == WM_MOUSEMOVE {
                    use windows::Win32::UI::Input::KeyboardAndMouse::{TRACKMOUSEEVENT, TrackMouseEvent, TME_LEAVE};
                    let mut track = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE, hwndTrack: self.groups[index].hwnd, dwHoverTime: 0 };
                    unsafe { let _ = TrackMouseEvent(&mut track); }
                }
            }
        }
        let on_title = self.groups[index].title.contains(point.0, point.1);
        if matches!(self.drag, Drag::Item { .. }) && (matches!(msg, WM_CANCELMODE | WM_CAPTURECHANGED)
            || (msg == WM_KEYDOWN && wparam.0 == VK_ESCAPE.0 as usize)) {
            self.cancel_item_drag();
            return Some(LRESULT(0));
        }
        let client = (point.0 + self.groups[index].rect.left, point.1 + self.groups[index].rect.top);
        // A title capture owns its full lifetime. No file/item drag is dispatched from here.
        if self.tab_press.is_some_and(|p| p.owner == id) {
            match msg {
                // Corral windows never hold keyboard focus: Esc held during the drag counts as Esc.
                WM_MOUSEMOVE if (unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(i32::from(VK_ESCAPE.0)) }) < 0 => self.escape_tab_title(client),
                WM_MOUSEMOVE if wparam.0 & MK_LBUTTON != 0 => self.tab_title_move(index, client, wparam.0),
                WM_MOUSEMOVE if std::env::var_os("CORRAL_QA_POSTED_INPUT").is_none() => self.finish_tab_title(client, false),
                WM_LBUTTONUP => {
                    self.tab_title_move(index, client, wparam.0);
                    self.finish_tab_title(client, true);
                }
                WM_CAPTURECHANGED | WM_CANCELMODE => self.finish_tab_title(client, false),
                WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize => self.escape_tab_title(client),
                _ => return None,
            }
            return Some(LRESULT(0));
        }
        if self.segment_button_input(index, msg, point, wparam.0) { return Some(LRESULT(0)); }
        match msg {
            // Corral windows never hold keyboard focus, so Esc reaches them only as its key state: also
            // check it on every move of the drag.
            WM_KEYDOWN | WM_MOUSEMOVE if self.group_drag_of(id).is_some() && (if msg == WM_KEYDOWN { wparam.0 == VK_ESCAPE.0 as usize }
                else { (unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(i32::from(VK_ESCAPE.0)) }) < 0 }) => {
                if let Some(mut moved) = self.group_drag_of(id) {
                    // Esc during a resize restores the rect the drag started with (grid-snapping-spec 3.1).
                    if matches!(self.drag, Drag::ResizeGroup { .. }) && self.restore_resize_start(id) { moved = false; }
                    self.end_group_drag_with_rescue(id, moved, false, None);
                }
                Some(LRESULT(0))
            }
            // Edge/corner/title cursors; a drag keeps its
            // cursor (a title drag shows SIZEALL).
            WM_SETCURSOR if lparam.0 & 0xffff == 1 /* HTCLIENT */ => {
                let cursor = match self.drag {
                    Drag::ResizeGroup { id: resizing, zone, .. } if resizing == id => snap::cursor_for(zone),
                    Drag::MoveGroup { id: moving, .. } if moving == id => CursorKind::SizeAll,
                    Drag::None => {
                        let mut cursor = POINT::default();
                        unsafe {
                            let _ = GetCursorPos(&mut cursor);
                            let _ = ScreenToClient(self.groups[index].hwnd, &mut cursor);
                        }
                        self.hover_cursor(index, (cursor.x, cursor.y))?
                    }
                    _ => return None,
                };
                let cursor = match cursor {
                    CursorKind::SizeWE => IDC_SIZEWE,
                    CursorKind::SizeNS => IDC_SIZENS,
                    CursorKind::SizeNWSE => IDC_SIZENWSE,
                    CursorKind::SizeNESW => IDC_SIZENESW,
                    CursorKind::SizeAll => IDC_SIZEALL,
                    CursorKind::Hand => IDC_HAND,
                    _ => return None,
                };
                unsafe {
                    SetCursor(LoadCursorW(None, cursor).unwrap_or_default());
                }
                Some(LRESULT(1))
            }
            WM_LBUTTONDOWN if matches!(self.drag, Drag::None) && self.edge_zone(index, point).is_edge() => {
                let zone = self.edge_zone(index, point);
                self.begin_resize(index, point, zone);
                Some(LRESULT(0))
            }
            // The resize band (also where the scrollbar passes input through) never opens or drags the row under it.
            WM_LBUTTONDBLCLK if self.edge_zone(index, point).is_edge() => Some(LRESULT(0)),
            WM_RBUTTONDOWN if !on_title => {
                if let Some(name) = self.item_in_group(index, point).filter(|_| !self.edge_zone(index, point).is_edge()) {
                    let rect = self.groups[index].rect;
                    let screen = self.to_screen((point.0 + rect.left, point.1 + rect.top));
                    let body = self.groups[index].body;
                    let grab = self.grab_in_slot(id, body, point);
                    self.begin_right_item(name, self.groups[index].hwnd, screen, grab);
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP if matches!(self.drag, Drag::Item { .. }) => {
                let (name, active) = match &self.drag { Drag::Item { name, active, .. } => (name.clone(), *active), _ => unreachable!() };
                if active {
                    // a right DRAG that did not hand over to OLE: end it (ghost included), no menu
                    // (file-safety review 2026-10-15, finding 4)
                    self.cancel_item_drag();
                    unsafe { let _ = ReleaseCapture(); }
                    return Some(LRESULT(0));
                }
                self.cancel_item_press();
                unsafe { let _ = ReleaseCapture(); }
                let names = self.menu_names(&name);
                let rect = self.groups[index].rect;
                self.show_item_menu(self.groups[index].hwnd, names, (point.0 + rect.left, point.1 + rect.top));
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN if !on_title => {
                let mods = Modifiers::from_mouse_keys(wparam.0);
                if let Some(name) = self.item_in_group(index, point) {
                    let before = self.selected_keys();
                    self.press_item(&name, mods);
                    let rect = self.groups[index].rect;
                    let screen = self.to_screen((point.0 + rect.left, point.1 + rect.top));
                    let hwnd = self.groups[index].hwnd;
                    let body = self.groups[index].body;
                    let grab = self.grab_in_slot(id, body, point);
                    unsafe {
                        SetCapture(hwnd);
                        self.peek_set_focus(hwnd);
                    }
                    self.pages.drag_names.clear();
                    self.drag = Drag::Item { name, start: screen, current: screen, grab, active: false, owner: hwnd };
                    self.repaint_selection(&before, false, Some(id));
                } else if self.groups[index].body.contains(point.0, point.1) {
                    // empty space in the body: a rubber band that selects this Corral's or Gate's icons
                    self.begin_group_band(index, point, mods);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN if self.groups[index].menu_glyph.contains(point.0, point.1) => {
                self.control_press = Some((id, false));
                self.render_group(index);
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN if self.groups[index].close_glyph.contains(point.0, point.1) => {
                self.control_press = Some((id, true));
                self.render_group(index);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP if self.control_press.is_some() => {
                let pressed = self.control_press.take();
                self.render_group(index);
                let gw = &self.groups[index];
                let rect = gw.rect;
                let screen = self.to_screen((point.0 + rect.left, point.1 + rect.top));
                match pressed {
                    Some((pid, false)) if pid == id && gw.menu_glyph.contains(point.0, point.1) => self.show_group_menu(id, screen),
                    Some((pid, true)) if pid == id && gw.close_glyph.contains(point.0, point.1) => self.confirm_remove_group(id),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            // Right-click on the portal glyph = the glyph menu (title hit
            // and the point inside the glyph).
            WM_RBUTTONUP if self.groups[index].glyph.contains(point.0, point.1) && self.portals.contains_key(&id) => {
                let glyph = self.groups[index].glyph;
                let rect = self.groups[index].rect;
                let (left, top) = self.to_screen((glyph.left + rect.left, glyph.top + rect.top));
                self.show_portal_glyph_menu(id, Rect::new(left, top, left + glyph.width(), top + glyph.height()));
                Some(LRESULT(0))
            }
            WM_RBUTTONUP if on_title => {
                let rect = self.groups[index].rect;
                let screen = self.to_screen(title_context_menu_point(point, rect));
                let member = self.segment_hit(index, point).map(|(member, _)| member).or_else(|| self.tab_hit(index, point)).unwrap_or(id);
                self.show_group_menu(member, screen);
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN if self.groups[index].up_glyph.contains(point.0, point.1) => {
                self.begin_tab_title(index, point, wparam.0);
                if let Some(press) = &mut self.tab_press { press.gated = true; press.portal_glyph = true; press.portal_up = true; }
                self.render_group(index);
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN if self.groups[index].glyph.contains(point.0, point.1) => {
                self.begin_tab_title(index, point, wparam.0);
                if let Some(press) = &mut self.tab_press { press.gated = true; press.portal_glyph = true; }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP if self.glyph_press.is_some() => {
                let pressed = self.glyph_press.take();
                if pressed == Some(id) && self.groups[index].glyph.contains(point.0, point.1) {
                    let glyph = self.groups[index].glyph;
                    let rect = self.groups[index].rect;
                    let (left, top) = self.to_screen((glyph.left + rect.left, glyph.top + rect.top));
                    self.portal_glyph_click(id, Rect::new(left, top, left + glyph.width(), top + glyph.height()));
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONDOWN if on_title => {
                self.begin_tab_title(index, point, wparam.0);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if self.group_band_active(id) => {
                if matches!(self.drag, Drag::Pending { .. }) && wparam.0 & MK_LBUTTON == 0 {
                    self.end_group_band(index, true); // button released unseen: no band starts
                } else {
                    self.group_band_move(index, point);
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP if self.group_band_active(id) => {
                self.end_group_band(index, false);
                Some(LRESULT(0))
            }
            WM_KEYDOWN if wparam.0 == VK_ESCAPE.0 as usize && self.group_band_active(id) => {
                self.end_group_band(index, true);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED if self.group_band_active(id) => {
                self.end_group_band(index, true);
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED if matches!(self.drag, Drag::Item { active: false, owner, .. } if owner == self.groups[index].hwnd) => {
                self.cancel_item_press();
                Some(LRESULT(0))
            }
            WM_KEYDOWN if matches!(self.drag, Drag::None) => self.handle_selection_key(wparam.0 as u16, Some(id)),
            WM_MOUSEMOVE if matches!(self.drag, Drag::Item { .. }) => {
                let rect = self.groups[index].rect;
                let screen = self.to_screen((point.0 + rect.left, point.1 + rect.top));
                self.update_item_drag(screen);
                self.hand_item_drag_to_ole(wparam);
                Some(LRESULT(0))
            }
            WM_LBUTTONUP if matches!(self.drag, Drag::Item { .. }) => {
                let rect = self.groups[index].rect;
                let screen = self.to_screen((point.0 + rect.left, point.1 + rect.top));
                self.finish_item_drag(screen);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE if matches!(self.drag, Drag::None) => {
                self.update_tab_hover(index, point, wparam.0 & (MK_LBUTTON | MK_RBUTTON) != 0 || key_down(VK_LBUTTON) || key_down(VK_RBUTTON));
                self.title_hover_enter(id);
                // Hover selection can replace the active HWND; rollout follows the new member.
                let active = tabs::collect(&self.layout.groups, id).map_or(id, |v| v.active);
                self.hover_group(active);
                Some(LRESULT(0))
            }
            // A move/resize that sees the pointer without a button down (a lost button-up) ends as a
            // normal release. Corral also ends it when another window takes
            // the capture, since no button-up would arrive. With `CORRAL_QA_POSTED_INPUT` (message-only
            // QA scripts) such a move is ignored instead: Windows adds real button-less WM_MOUSEMOVEs at the
            // idle cursor whenever a window moves under it, which would end the posted drag or resize the
            // group to the real cursor position.
            WM_MOUSEMOVE if wparam.0 & (MK_LBUTTON | MK_RBUTTON | MK_MBUTTON) == 0 && self.group_drag_of(id).is_some() => {
                if std::env::var_os("CORRAL_QA_POSTED_INPUT").is_none() {
                    if let Some(moved) = self.group_drag_of(id) {
                        self.end_group_drag_with_rescue(id, moved, true, None);
                    }
                }
                Some(LRESULT(0))
            }
            WM_CAPTURECHANGED | WM_CANCELMODE if self.group_drag_of(id).is_some() => {
                if let Some(moved) = self.group_drag_of(id) {
                    self.end_group_drag_with_rescue(id, moved, false, None);
                }
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                // The key flags of a mouse message and GetKeyState follow only the keys this thread has
                // seen; while Corral's window lacks keyboard focus a held Ctrl/Shift is invisible to both
                // (user live check G5: Ctrl never switched the size tip to pixels). Shift is also read with
                // GetAsyncKeyState for the same reason.
                let shift = wparam.0 & MK_SHIFT != 0 || key_down(VK_SHIFT);
                let origin = self.groups[index].rect;
                let client = self.clamp_to_monitors((point.0 + origin.left, point.1 + origin.top));
                match self.drag {
                    Drag::MoveGroup { id: moving, grab, .. } if moving == id => self.move_step(index, client, grab, shift),
                    Drag::ResizeGroup { id: resizing, .. } if resizing == id => self.resize_step(index, client, shift),
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                match self.drag {
                    Drag::MoveGroup { id: moved, moved: changed, .. } | Drag::ResizeGroup { id: moved, moved: changed, .. } => {
                        let origin = self.groups[index].rect;
                        let release = self.to_screen((point.0 + origin.left, point.1 + origin.top));
                        self.end_group_drag(moved, changed, release);
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                if let Some(name) = self.item_in_group(index, point) {
                    let hwnd = self.groups[index].hwnd;
                    let rect = self.groups[index].rect;
                    self.select_for_menu(&name);
                    let names = self.menu_names(&name);
                    self.show_item_menu(hwnd, names, (point.0 + rect.left, point.1 + rect.top));
                } else if !on_title {
                    // Empty space in a Corral body: the desktop's background menu, as on the bare desktop
                    // (observed 2026-10-15: Windows' own desktop menu).
                    let rect = self.groups[index].rect;
                    let before = self.selected_keys();
                    self.clear_selection();
                    self.repaint_selection(&before, true, None);
                    self.show_background_menu((point.0 + rect.left, point.1 + rect.top), Some(id));
                }
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
                // Ctrl + wheel zooms this Corral's icons (Corral extra); a plain wheel scrolls
                if key_down(VK_MENU) { return None; }
                if wparam.0 & MK_CONTROL != 0 || key_down(VK_CONTROL) {
                    return self.zoom_group_icons(index, delta).then_some(LRESULT(0));
                }
                self.scroll_group(index, delta).then_some(LRESULT(0))
            }
            // A quick second click on the glyph is another glyph click, not a roll toggle.
            WM_LBUTTONDBLCLK if self.groups[index].glyph.contains(point.0, point.1) => {
                self.glyph_press = Some(id);
                Some(LRESULT(0))
            }
            WM_LBUTTONDBLCLK => {
                if on_title && self.settings.rollup_enabled && self.settings.rollup_double_click_to_toggle {
                    self.toggle_roll(id);
                } else if let Some(name) = self.item_in_group(index, point) {
                    if !self.gate_open(&name, Some(id)) { let _ = shell::open_item(&name); }
                } else if self.portals.contains_key(&id) && self.groups[index].body.contains(point.0, point.1) {
                    self.portal_open_folder(id); // empty Gate body: Open folder (corral-views-spec 2.3)
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    /// The snapping settings (grid-snapping-spec 4.3 and 8).
    pub(super) fn snap_settings(&self) -> SnapSettings {
        SnapSettings::new(
            self.settings.snap_enabled,
            self.settings.snap_padding,
            self.settings.snap_should_scale,
            self.settings.snap_line_up_corrals,
            self.settings.snap_to_discrete_sizes,
        )
    }

    /// Outer rects (screen px) of the Corrals the moving or resized Corral `id` may snap to, on the
    /// governing monitor `governing`: same Range (page), visible, not a tab sibling; folded and faded
    /// Corrals count (grid-snapping-spec 4.4). `id` 0 = a Corral that does not exist yet.
    pub(super) fn snap_neighbours(&self, id: u64, governing: &str) -> Vec<Rect> {
        let (ox, oy) = self.origin();
        let index_of = |name: &str| self.monitors.iter().position(|m| m.name.eq_ignore_ascii_case(name)).unwrap_or(usize::MAX);
        let infos: Vec<NeighbourInfo> = self.layout.groups.iter().map(|g| {
            let visible = tabs::body_visible(&self.layout.groups, g) && self.page_group_visible(g) && self.qh_group_alpha(g.id) > 0;
            NeighbourInfo {
                id: g.id,
                tab_set: g.tab.map_or(0, |t| t.set_id),
                monitor: index_of(&g.monitor),
                range: 0, // only Corrals of the current Range are ever `visible`
                visible,
                // geometry only for what can be a neighbour
                outer: if visible { self.group_geometry(g).union.offset(ox, oy) } else { Rect::default() },
            }
        }).collect();
        let tab_set = self.layout.groups.iter().find(|g| g.id == id).and_then(|g| g.tab).map_or(0, |t| t.set_id);
        let me = NeighbourInfo { id, tab_set, monitor: index_of(governing), range: 0, visible: true, outer: Rect::default() };
        snap::select_neighbours(&me, &infos)
    }

    /// Pointer offset inside the cell under `point` (group-window px) of Corral `id`, for an icon drag.
    fn grab_in_slot(&self, id: u64, body: Rect, point: (i32, i32)) -> (i32, i32) {
        let dpi = self.group_dpi(id);
        let scroll = self.layout.groups.iter().find(|g| g.id == id).map_or(0, |g| g.scroll);
        let grid = self.group_grid(id, dpi, body, scroll);
        grid.slot_at(point).map_or((0, 0), |slot| {
            let r = grid.slot_rect(slot);
            (point.0 - r.left, point.1 - r.top)
        })
    }

    /// Alignment guide lines to draw while a snap holds; redraws the surface when they change.
    fn set_snap_guides(&mut self, guides: Vec<snap::Guide>) {
        if guides != self.snap_guides {
            self.snap_guides = guides;
            self.render_surface();
        }
    }

    /// Ctrl + wheel over an ordinary Corral: one zoom step per wheel notch (`icon_zoom.rs`), only for this
    /// Corral (each tab keeps its own zoom). True when the wheel was taken (also while a
    /// fractional notch is still accumulating); false for Gates and
    /// rolled-up Corrals, which keep the plain wheel behaviour.
    fn zoom_group_icons(&mut self, index: usize, delta: i32) -> bool {
        let id = self.groups[index].id;
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id).cloned() else { return false };
        if !matches!(group.content, GroupContent::Items { .. }) || self.group_geometry(&group).rolled || self.group_view(id) != crate::view_state::View::Icons {
            return false;
        }
        let (carry, notches) = crate::icon_zoom::accumulate(self.zoom_wheel_carry, delta);
        self.zoom_wheel_carry = carry;
        if notches == 0 {
            return true;
        }
        let mut zoom = group.icon_zoom;
        for _ in 0..notches.unsigned_abs() {
            zoom = crate::icon_zoom::step(zoom, notches > 0);
        }
        self.set_group_icon_zoom(id, crate::icon_zoom::percent(zoom));
        true
    }

    /// Shared wheel/menu path: each Corral or Gate keeps its own zoom and first visible row.
    pub(super) fn set_group_icon_zoom(&mut self, id: u64, percent: u32) {
        if self.group_view(id) != crate::view_state::View::Icons { return; }
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id).cloned() else { return };
        let dpi = self.group_dpi(id);
        let geo = self.group_geometry(&group);
        let old_cell = self.group_cells(id, dpi);
        let zoom = crate::icon_zoom::canonical(percent);
        if zoom == group.icon_zoom || !self.layout.set_icon_zoom(id, zoom) {
            return; // already at this size (or at the end of the wheel range)
        }
        // the item at the top of the view stays at the top (grid-snapping-spec 2.2)
        let new_cell = self.group_cells(id, dpi);
        let count = self.group_members(&group).len();
        if let Some(g) = self.layout.group_mut(id) {
            let (body_w, body_h) = (geo.full_body.width(), geo.full_body.height());
            let px = new_cell.scroll_after_zoom(&old_cell, old_cell.cols_for(body_w), new_cell.cols_for(body_w), to_px(g.scroll, dpi), count, body_h);
            g.scroll = to_dip(px, dpi);
        }
        self.clamp_metric_scrolls();
        log(&format!("icon zoom: group {id} {:?} -> {zoom:?} (cell {old_cell:?} -> {new_cell:?})", group.icon_zoom));
        self.retain_group_icon_rasters();
        // every member of a tab set shares the body; the visible one is the one on screen
        let members: Vec<u64> = self.layout.groups.iter().filter(|g| g.id == id || group.tab.is_some_and(|t| t.set_id != 0 && g.tab.is_some_and(|m| m.set_id == t.set_id))).map(|g| g.id).collect();
        for member in members {
            if let Some(i) = self.groups.iter().position(|g| g.id == member) {
                self.render_group(i);
            }
        }
        self.arm_layout_save();
    }

    /// Re-arm the 3 s debounced layout save.
    fn arm_layout_save(&self) {
        unsafe {
            SetTimer(self.message_window, TIMER_LAYOUT_SAVE, 3000, None);
        }
    }

    /// Edge/title/body zone of `point` (group-window coordinates, grid-snapping-spec 3.1). The group
    /// window is exactly the union of icon area and title strip, so the bands are inside it and there
    /// is no margin outside it.
    fn edge_zone(&self, index: usize, point: (i32, i32)) -> EdgeZone {
        let gw = &self.groups[index];
        let Some(group) = self.layout.groups.iter().find(|g| g.id == gw.id) else { return EdgeZone::NONE };
        if !self.page_group_visible(group) {
            return EdgeZone::NONE;
        }
        // Use the published rectangles if a coalesced repaint is still owed.
        let geo = super::GroupGeometry { union: gw.rect, title: gw.title.offset(gw.rect.left, gw.rect.top), ..self.group_geometry(group) };
        let mut buttons: Vec<Rect> = [gw.menu_glyph, gw.close_glyph, gw.up_glyph, gw.glyph].iter().map(|r| r.offset(gw.rect.left, gw.rect.top)).collect();
        for s in &gw.segments {
            let layout = s.segment.layout;
            buttons.extend([layout.menu.filter(|_| s.buttons), layout.close.filter(|_| s.buttons), layout.up.filter(|_| s.up_enabled)]
                .into_iter().flatten().map(|r| r.offset(gw.rect.left, gw.rect.top)));
        }
        group_hit_zone(&geo, group, (point.0 + gw.rect.left, point.1 + gw.rect.top), self.layout.desktop_locked, &buttons)
    }

    /// `Some(moved)` while group `id` is being moved or resized.
    fn group_drag_of(&self, id: u64) -> Option<bool> {
        match self.drag {
            Drag::MoveGroup { id: d, moved, .. } | Drag::ResizeGroup { id: d, moved, .. } if d == id => Some(moved),
            _ => None,
        }
    }

    /// Hover cursor of a group window point without a drag: size cursors on
    /// the edges, HAND over the title glyphs, SIZEALL elsewhere on the title strip, the class arrow
    /// otherwise (`None`). HAND shows on a plain title only when the title-bar controls apply to every tab; Corral
    /// has no such setting (it always applies).
    fn hover_cursor(&self, index: usize, point: (i32, i32)) -> Option<CursorKind> {
        let zone = self.edge_zone(index, point);
        if zone.is_edge() {
            return Some(snap::cursor_for(zone));
        }
        let gw = &self.groups[index];
        if !gw.title.contains(point.0, point.1) {
            return None;
        }
        let over_glyph = [gw.menu_glyph, gw.close_glyph, gw.up_glyph, gw.glyph].iter().any(|r| r.contains(point.0, point.1));
        Some(if over_glyph { CursorKind::Hand } else { CursorKind::SizeAll })
    }

    /// Raise group `index` to the top of the group z-order; returns
    /// its new index. Desktop mode re-runs the stack order (a plain `HWND_TOP` would lift the popup
    /// above every application until the restack); preview groups are child windows.
    pub(super) fn raise_group(&mut self, index: usize) -> usize {
        let last = self.groups.len() - 1;
        if index >= last {
            return index;
        }
        let gw = self.groups.remove(index);
        let hwnd = gw.hwnd;
        self.groups.push(gw);
        if self.detached() {
            self.restack();
        } else {
            unsafe {
                let _ = SetWindowPos(hwnd, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
            }
        }
        last
    }

    /// Press on an edge or corner: remember the pointer and the body at the press (every step is
    /// computed from these two) and capture.
    fn begin_resize(&mut self, index: usize, point: (i32, i32), zone: EdgeZone) {
        if self.profile_pending.is_some() { return; }
        let id = self.groups[index].id;
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id) else { return };
        let geo = self.group_geometry(group);
        let undo = super::capture_resize_state(&self.layout, &self.runtime_bodies, id);
        if let Some(g) = self.layout.group_mut(id) { g.title_side_last = geo.below as i32; }
        self.resize_undo = Some(undo);
        let origin = self.groups[index].rect;
        let pointer = self.to_screen((point.0 + origin.left, point.1 + origin.top));
        let (ox, oy) = self.origin();
        let start = geo.full_body.offset(ox, oy);
        log(&format!("resize begin id={id} zone={:#x} pointer={pointer:?} start={start:?}", zone.bits()));
        self.drag = Drag::ResizeGroup { id, zone, pointer, start, moved: false };
        self.snap_lock = snap::SnapLock::default();
        self.snap_guides.clear();
        unsafe { SetCapture(self.groups[index].hwnd) };
    }

    /// Esc during a resize: the body, monitor and preferred size of the press come back. False when
    /// there was nothing to restore.
    fn restore_resize_start(&mut self, id: u64) -> bool {
        let Some(undo) = self.resize_undo.take().filter(|u| u.id == id) else { return false };
        super::restore_resize_state(&mut self.layout, &mut self.runtime_bodies, &undo);
        true
    }

    /// One mouse move of a resize (grid-snapping-spec 3): the rect comes from the press state and the
    /// pointer, so it does not depend on how many messages arrived. The governing monitor decides the
    /// minimum, the work-area limit, the cell and the DIP size; the opposite edge never moves.
    fn resize_step(&mut self, index: usize, client: (i32, i32), shift: bool) {
        let id = self.groups[index].id;
        let Drag::ResizeGroup { zone, pointer: start_pointer, start, .. } = self.drag else { return };
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id).cloned() else { return };
        if self.profile_pending.is_some() { return; }
        let cursor = self.to_screen(client);
        let geo = self.group_geometry(&group);
        // A folded Corral is its title strip: only its width resizes and the stored height stays.
        let folded = group.roll != RollState::Expanded;
        let strips = |dpi: u32| -> (i32, i32) {
            if geo.metrics.title_h == 0 { return (0, 0); }
            let m = self.group_title_metrics(&group, dpi);
            let strip = m.title_h + m.gap;
            if geo.below { (0, strip) } else { (strip, 0) }
        };
        let measured = start; // width and edges come from the press-time full body, also when folded
        let Some(live) = self.display_snapshot.as_ref() else { return };
        // Each candidate monitor is judged with its own minimum and work area (sticky: no flip-flop at a seam).
        let off = SnapSettings::new(false, 0, false, false, false);
        let Some(gov) = crate::dpi_transition::govern(live, &group.monitor, |m| {
            let (strip_top, strip_bottom) = if folded { (0, 0) } else { strips(m.dpi) };
            let rect = snap::resize_rect(&ResizeInput {
                zone, start: measured, strip_top, strip_bottom, start_pointer, pointer: cursor, dpi: m.dpi, work: m.usable_work(),
                cell: self.group_cells(id, m.dpi), settings: off, shift: true, neighbours: &[], lock: snap::SnapLock::default(), min: None,
            }).rect;
            let rect = if folded { Rect::new(rect.left, start.top, rect.right, start.bottom) } else { rect };
            let body = crate::dpi_transition::RequestedBody::from_physical(rect)?;
            let dip = crate::dpi_transition::physical_to_dips(body.physical(), m)?;
            let shown = self.fitted_size(id, (dip.width().max(1), dip.height().max(1)), m)?;
            Some(crate::dpi_transition::fit_body(body, shown, m)?.physical())
        }) else { return };
        let (gov, dpi, work) = (gov.device.clone(), gov.dpi, gov.usable_work());
        let (strip_top, strip_bottom) = if folded { (0, 0) } else { strips(dpi) };
        let settings = self.snap_settings();
        let neighbours = self.snap_neighbours(id, &gov);
        let out = snap::resize_rect(&ResizeInput {
            zone, start: measured, strip_top, strip_bottom, start_pointer, pointer: cursor, dpi, work,
            cell: self.group_cells(id, dpi), settings, shift, neighbours: &neighbours, lock: self.snap_lock, min: None,
        });
        self.snap_lock = out.lock;
        self.set_snap_guides(out.guides.clone());
        let mut rect = if folded { Rect::new(out.rect.left, start.top, out.rect.right, start.bottom) } else { out.rect };
        // size tooltip, once the drag has passed the system drag threshold (grid-snapping-spec 7)
        let (tx, ty) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
        if (cursor.0 - start_pointer.0).abs() > tx || (cursor.1 - start_pointer.1).abs() > ty {
            let outer = (out.rect.width(), if folded { geo.title.height() } else { out.rect.height() + strip_top + strip_bottom });
            let cells = (settings.whole_cells() && !shift && !folded)
                .then(|| self.group_cells(id, dpi).cells_for_size(out.rect.width(), out.rect.height()));
            size_tip::show(self.message_window, &snap::size_tip_text(outer, cells));
        }
        // Corral policy: even a top-edge resize keeps the expanded body and
        // full title on screen. The side is frozen during this gesture.
        if let Some(group) = self.layout.groups.iter().find(|g| g.id == id) {
            let geo = self.group_geometry(group);
            let title = self.group_title_metrics(group, dpi);
            if let Some(work) = self.display_snapshot.as_ref().and_then(|s| s.find(&gov)).map(|d| d.work) {
                rect = rollup::clamp_top(rect, geo.below, title.title_h, title.gap, work.top, zone.intersects(EdgeZone::TOP));
                rect = rollup::DockEdge::Bottom.clamp(rect, geo.below, title.title_h, title.gap, work, zone.intersects(EdgeZone::BOTTOM));
            }
        }
        let Some(requested) = crate::dpi_transition::RequestedBody::from_physical(rect) else { return };
        let Some(d) = self.display_snapshot.as_ref().and_then(|s| s.find(&gov)) else { return };
        let Some((preferred, preference_changed)) = super::user_resize_preference(&mut self.layout, id, requested, d) else { return };
        let geometry_changed = self.apply_placement(id, requested, Some(&gov), Some(preferred));
        if !geometry_changed && !preference_changed { return; }
        if let Drag::ResizeGroup { moved, .. } = &mut self.drag {
            *moved = true;
        }
        self.drag_render_resize(index);
        if self.portals.contains_key(&id) {
            self.layout_portal_popups();
        }
    }

    /// One mouse move of a title drag: position from the pointer and the grab offset (px), group
    /// snapping (not with Shift), keep-on-screen, then one conversion to DIPs (section 5).
    /// A captured drag point clamped to the monitors' union (parent-client coordinates). On the real
    /// desktop the cursor already stops at the screen edge; in preview mode it can leave the preview
    /// window, which let groups overshoot far past the keep-on-screen range (live 2026-10-13).
    fn clamp_to_monitors(&self, client: (i32, i32)) -> (i32, i32) {
        let (ox, oy) = self.origin();
        let Some(first) = self.monitors.first() else { return client };
        let all = self.monitors.iter().fold(first.bounds, |u, m| {
            Rect::new(u.left.min(m.bounds.left), u.top.min(m.bounds.top), u.right.max(m.bounds.right), u.bottom.max(m.bounds.bottom))
        });
        let b = all.offset(-ox, -oy);
        (client.0.clamp(b.left, b.right - 1), client.1.clamp(b.top, b.bottom - 1))
    }

    /// The Corral keeps its DIP size across DPIs (user decision 2026-10-14): the physical body is
    /// laid out per candidate monitor DPI with the grab point proportionally under the cursor;
    /// `govern` picks the monitor (sticky, no oscillation at the seam); snapping and
    /// keep-on-screen then work on that monitor.
    fn move_step(&mut self, index: usize, client: (i32, i32), grab: crate::dpi_transition::Grab, shift: bool) {
        if self.profile_pending.is_some() { return; }
        let id = self.groups[index].id;
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id).cloned() else { return };
        let (ox, oy) = self.origin();
        let cursor = self.to_screen(client);
        let Some(live) = self.display_snapshot.as_ref() else { return };
        let Some(gov) = crate::dpi_transition::govern(live, &group.monitor, |m| {
            let shown = self.fitted_size(id, grab.size_dip, m)?;
            let body = crate::dpi_transition::move_layout_fitted(grab, cursor, m, shown)?.physical();
            let min = self.min_body_px(m.dpi, Some((body.width(), body.height())));
            let title = self.group_title_metrics(&group, m.dpi);
            let min = crate::dpi_transition::fitting_minimum(m, min, title.title_h + title.gap);
            Some(super::metrics::effective_body(body, min))
        }) else { return };
        let gov = gov.device.clone();
        let Some(d) = live.find(&gov) else { return };
        let Some(shown) = self.fitted_size(id, grab.size_dip, d) else { return };
        let Some(proposal) = crate::dpi_transition::move_layout_fitted(grab, cursor, d, shown) else { return };
        let mut proposed = proposal.physical().offset(-ox, -oy);
        let snap = self.snap_settings();
        let size = Some(grab.size_dip);
        let Some(raw) = crate::dpi_transition::RequestedBody::from_physical(proposed.offset(ox, oy)) else { return };
        let Some((p, effective)) = self.placement_in(id, raw, Some(&gov), size) else { return };
        // The outer rect (body plus title strip; the strip alone when folded) at the raw pointer
        // position, in screen px: what snaps and what must stay on screen (grid-snapping-spec 4.1, 5).
        let mut raw_outer: Option<(Rect, Rect)> = None;
        if let Some(work_screen) = self.monitor_for(&p.monitor).map(|m| m.work) {
            let geo = self.group_geometry(&group);
            let folded = group.roll != RollState::Expanded;
            let outer = if folded {
                geo.union.offset(effective.left - ox - geo.full_body.left, effective.top - oy - geo.full_body.top)
            } else if geo.metrics.title_h == 0 {
                effective.offset(-ox, -oy)
            } else {
                let m = self.group_title_metrics(&group, p.effective_dpi);
                let strip = m.title_h + m.gap;
                let e = effective.offset(-ox, -oy);
                if geo.below { Rect::new(e.left, e.top, e.right, e.bottom + strip) } else { Rect::new(e.left, e.top - strip, e.right, e.bottom) }
            }.offset(ox, oy);
            raw_outer = Some((outer, work_screen));
            let neighbours = self.snap_neighbours(id, &p.monitor);
            let snapped = snap::snap_move(&snap::MoveInput {
                raw: outer, work: work_screen, neighbours: &neighbours, settings: snap, dpi: p.effective_dpi, shift, lock: self.snap_lock,
            });
            self.snap_lock = snapped.lock;
            // keep on screen: the whole outer rect inside the work area, after snapping (spec 5)
            let kept = snap::place_on_screen(snapped.rect, &[work_screen], Some(0), None).rect;
            // a drop here would dock: the alignment guide runs along that edge across the Corral (spec 4)
            let mut guides = snapped.guides.clone();
            if let Some(edge) = rollup::dock_on_drop(kept, work_screen, p.effective_dpi, shift, group.dock_edge()) {
                let at = match edge { rollup::DockEdge::Top => work_screen.top, rollup::DockEdge::Bottom => work_screen.bottom };
                if !guides.iter().any(|g| !g.vertical && g.at == at) {
                    guides.push(snap::Guide { vertical: false, at, from: kept.left, to: kept.right });
                }
            }
            self.set_snap_guides(guides);
            proposed = proposed.offset(kept.left - outer.left, kept.top - outer.top);
        }
        // The governing monitor stays; only the snapped body is measured again before its clamp.
        let Some(raw) = crate::dpi_transition::RequestedBody::from_physical(proposed.offset(ox, oy)) else { return };
        let Some((p, _)) = self.placement_in(id, raw, Some(&gov), size) else { return };
        // Corral policy: keep-on-screen can leave only
        // a reachable strip. Always contain the full expanded top, even with snapping off.
        if let Some(work) = self.monitor_for(&p.monitor).map(|m| m.work.offset(-ox, -oy)) {
            let geo = self.group_geometry(&group);
            let title = self.group_title_metrics(&group, p.effective_dpi);
            let adjusted = rollup::clamp_top(proposed, geo.below,
                title.title_h, title.gap, work.top, false);
            let adjusted = rollup::DockEdge::Bottom.clamp(adjusted, geo.below,
                title.title_h, title.gap, work, false);
            let dy = adjusted.top - proposed.top;
            proposed = proposed.offset(0, dy);
        }
        let Some(requested) = crate::dpi_transition::RequestedBody::from_physical(proposed.offset(ox, oy)) else { return };
        // A title drag away from either edge releases its latch. Expand before laying out
        // at the pointer so a formerly folded strip cannot be left floating mid-screen.
        let undock_below = matches!(self.drag, Drag::MoveGroup { undock_below: true, .. });
        // A docked Corral sticks while its raw outer edge is within 12 DIP of the edge (ranges-dock-clearview-spec 4).
        let undocked = group.dock_edge().is_some_and(|edge| raw_outer.is_some_and(|(outer, work)| !rollup::stays_docked(edge, outer, work, p.effective_dpi)));
        if undocked {
            self.roll_progress.remove(&id);
            self.roll_target.remove(&id);
            if let Some(g) = self.layout.group_mut(id) {
                g.set_dock_edge(None);
                g.title_side_last = i32::from(undock_below);
                g.roll = RollState::Expanded;
            }
        }
        let old_dpi = self.group_geometry(&group).dpi;
        if !self.apply_placement(id, requested, Some(&gov), size) && !undocked { return; }
        // drag start collapses a temporarily open Corral at once (motion-sorting-spec 3)
        if matches!(self.drag, Drag::MoveGroup { moved: false, .. }) && self.hot_group == Some(id) { self.close_hot(id); }
        if let Drag::MoveGroup { moved, .. } = &mut self.drag { *moved = true; }
        let new_dpi = self.layout.groups.iter().find(|g| g.id == id).map_or(old_dpi, |g| self.group_geometry(g).dpi);
        let size_changed = self.layout.groups.iter().find(|g| g.id == id)
            .is_some_and(|g| g.rect.width() != group.rect.width() || g.rect.height() != group.rect.height());
        if old_dpi != new_dpi || size_changed { self.render_group(index); }
        else { self.drag_render_group(index); }
    }

    /// Button release after a move or resize: dock-change roll (7.2), save, rescue passes (7.3).
    fn end_group_drag(&mut self, id: u64, changed: bool, release: (i32, i32)) {
        self.end_group_drag_with_rescue(id, changed, true, Some(release));
    }

    pub(super) fn cancel_group_for_display(&mut self) {
        self.layout.desk_setups.runtime.display_cancel = true; // spec 3.2: not a user edit
        if self.tab_press.is_some() {
            self.finish_tab_title((0, 0), false);
        } else if let Drag::MoveGroup { id, moved, .. } | Drag::ResizeGroup { id, moved, .. } = self.drag {
            self.end_group_drag_with_rescue(id, moved, false, None);
        }
        self.layout.desk_setups.runtime.display_cancel = false;
    }

    fn end_group_drag_with_rescue(&mut self, id: u64, changed: bool, rescue: bool, release: Option<(i32, i32)>) {
        // Only a finished title drag sets or clears the flag (ranges-dock-clearview-spec 4); a resize, a
        // cancel or a lost capture leaves it as it is. RollupEnabled does not matter here: it only decides
        // whether the flag also folds.
        let title_drop = changed && release.is_some() && matches!(self.drag, Drag::MoveGroup { .. });
        let decision = self.layout.groups.iter().find(|g| g.id == id).filter(|_| title_drop).and_then(|g| {
            let geo = self.group_geometry(g);
            let (ox, oy) = self.origin();
            let work = self.monitor_for(&g.monitor)?.work.offset(-ox, -oy);
            let edge = rollup::dock_on_drop(geo.union, work, geo.dpi, key_down(VK_SHIFT), g.dock_edge());
            Some((geo, work, g.monitor.clone(), edge))
        });
        if let Some((_, _, _, None)) = &decision {
            // dropped farther than 12 DIP from the edge it was docked to: the flag goes, the fold follows
            if let Some(g) = self.layout.group_mut(id).filter(|g| g.dock_edge().is_some()) { g.set_dock_edge(None); }
        }
        let dock_release = decision.and_then(|(geo, work, monitor, edge)| edge.map(|edge| (geo, work, monitor, edge)));
        if let Some((geo, work, monitor, edge)) = &dock_release {
            // The flag is set whatever RollupEnabled says; only with RollupEnabled and RollupOnDock does docking
            // also fold, with the close animation. A Corral that was already shut stays as it is, a
            // hovered-open one (folded state, body showing) closes too.
            let fold = self.settings.rollup_enabled && self.settings.rollup_on_dock_to_edge;
            if fold {
                let was_rolled = self.is_rolled(id);
                if !was_rolled {
                    self.roll_progress.insert(id, rollup::OPEN);
                }
                if rollup::close_on_dock(was_rolled, self.hot_group == Some(id), self.roll_openness(id)) {
                    self.roll_animate(id, false);
                }
                self.hot_group = None;
                self.roll_hover_suppressed = Some(id);
                unsafe {
                    SetTimer(self.message_window, TIMER_HOVER_POLL, 30, None);
                }
            }
            let below = edge.title_below(self.settings.title_edge_auto_adjust);
            let (_, _, body) = edge.layout(geo.full_body, *work, geo.metrics.title_h, geo.metrics.gap, 0, self.settings.title_edge_auto_adjust);
            if let Some(g) = self.layout.group_mut(id) {
                g.set_dock_edge(Some(*edge));
                g.title_side = -1;
                g.title_side_last = i32::from(below);
                if fold && g.roll != RollState::Manual { g.roll = RollState::Docked; }
            }
            let (ox, oy) = self.origin();
            if let Some(body) = crate::dpi_transition::RequestedBody::from_physical(body.offset(ox, oy)) {
                self.apply_placement(id, body, Some(monitor), None);
            }
        }
        // Settle the runtime pixels onto the DIP grid of the governing monitor once, so the saved
        // DIP rect decodes (restart) to exactly the pixels on screen. The requested size is kept
        // (not the minimum-expanded one): moving back must restore the requested DIP size.
        if changed {
            let size = self.layout.groups.iter().find(|g| g.id == id).map(|g| (g.rect.width(), g.rect.height()));
            let settled = self.layout.groups.iter().find(|g| g.id == id).and_then(|g| {
                let requested = self.requested_body(g)?;
                let display = self.display_snapshot.as_ref()?.find(&g.monitor)?;
                let metrics = self.group_title_metrics(g, display.dpi);
                super::settle_title_release(requested, display, size,
                    g.dock_edge().map_or(g.title_side, |edge| i32::from(edge.title_below(self.settings.title_edge_auto_adjust))),
                    g.title_side_last, self.settings.title_edge_auto_adjust, &metrics)
                    .map(|(body, below)| (body, below, g.monitor.clone()))
            });
            if let Some((body, below, monitor)) = settled {
                if let Some(g) = self.layout.group_mut(id) { g.title_side_last = i32::from(below); }
                let _ = self.layout.sync_tab_common(id);
                self.apply_placement(id, body, Some(&monitor), None);
            }
        }
        self.drag = Drag::None;
        size_tip::hide();
        self.drag_render_stop();
        unsafe {
            let _ = ReleaseCapture();
            let _ = KillTimer(self.message_window, TIMER_LAYOUT_SAVE);
        }
        let now_docked = self.group_is_docked(id);
        let (enabled, on_dock) = (self.settings.rollup_enabled, self.settings.rollup_on_dock_to_edge);
        let (was_rolled, shown) = (self.is_rolled(id), self.roll_openness(id));
        if let Some(group) = self.layout.group_mut(id).filter(|_| changed) {
            group.roll = rollup::derived_roll(group.roll, enabled, on_dock, now_docked);
        }
        // the animation follows a fold the drop changed (motion-sorting-spec 3)
        let docked_edge = self.layout.groups.iter().any(|g| g.id == id && g.dock_edge().is_some());
        match rollup::fold_follow(was_rolled, self.is_rolled(id), docked_edge) {
            rollup::FoldFollow::Keep => {}
            follow => {
                self.roll_clear(id);
                if follow == rollup::FoldFollow::Clear && self.hot_group == Some(id) {
                    self.hot_group = None;
                }
                if follow != rollup::FoldFollow::Clear {
                    self.roll_progress.insert(id, shown);
                    self.roll_animate(id, follow == rollup::FoldFollow::Open);
                }
            }
        }
        // the pointer is still on the dropped title: no hover opening until it has left once (spec 3)
        if changed && self.is_rolled(id) && self.hot_group != Some(id) {
            self.roll_hover_suppressed = Some(id);
            unsafe { SetTimer(self.message_window, TIMER_HOVER_POLL, 30, None); }
        }
        let _ = self.layout.sync_tab_common(id);
        if changed && rescue {
            self.rescue_dragged(id);
        }
        self.snap_guides.clear();
        self.snap_lock = snap::SnapLock::default();
        if changed { self.contain_fitted_group(id); }
        if let Some(i) = self.groups.iter().position(|g| g.id == id) {
            self.render_group(i);
        }
        self.record_drag_edit(changed);
        self.save();
        self.arrange_loose();
        self.render_surface();
    }

    /// After a user move or resize the dragged Corral ends fully inside the work area of its monitor
    /// (grid-snapping-spec 5): its outer rect (the title strip alone when folded) is shifted in, never
    /// resized. Other Corrals and a layout that was only loaded are not touched.
    fn rescue_dragged(&mut self, id: u64) {
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id).cloned() else { return };
        if !tabs::body_visible(&self.layout.groups, &group) || !self.page_group_visible(&group) { return; }
        let geo = self.group_geometry_stored(&group);
        let (ox, oy) = self.origin();
        let outer = if group.roll == RollState::Expanded { geo.union } else { geo.title }.offset(ox, oy);
        let works: Vec<Rect> = self.monitors.iter().map(|m| m.work).collect();
        let preferred = self.monitors.iter().position(|m| m.name.eq_ignore_ascii_case(&group.monitor));
        let placed = snap::place_on_screen(outer, &works, preferred, None).rect;
        let (dx, dy) = (placed.left - outer.left, placed.top - outer.top);
        if dx == 0 && dy == 0 { return; }
        if let Some(r) = self.requested_body(&group).and_then(|r| r.offset(dx, dy)) {
            self.apply_body_placement(id, r);
        }
        self.layout.sync_all_tab_sets();
        self.sync_group_windows();
        for index in 0..self.groups.len() {
            self.render_group(index);
        }
    }

    pub(super) fn is_rolled(&self, id: u64) -> bool {
        self.layout.groups.iter().any(|g| g.id == id && g.roll != RollState::Expanded)
    }

    /// The icon area of group `id` is not on screen: a rolled group at rest (visible body height 0).
    /// Not the same as `is_rolled`, which is the roll state: a rolled group that the pointer opened
    /// shows its icons, and they are selectable and actionable while they are.
    pub(super) fn body_hidden(&self, id: u64) -> bool {
        self.layout.groups.iter().find(|g| g.id == id).is_some_and(|g| self.group_geometry(g).rolled)
    }

    /// Whether the Corral is docked: the stored flag is the truth, not where it happens to sit
    /// (ranges-dock-clearview-spec 4).
    pub(super) fn group_is_docked(&self, id: u64) -> bool {
        self.layout.groups.iter().any(|g| g.id == id && g.dock_edge().is_some())
    }

    /// Pointer entered a group: folded groups open on hover unless RequireClick; RequireDelay waits for
    /// `RollupRequireDelayTime` milliseconds (clamped 100..2000) with the pointer still inside (spec 3).
    fn hover_group(&mut self, id: u64) {
        if self.hot_group == Some(id)
            || self.roll_hover_suppressed == Some(id)
            || !self.settings.rollup_enabled
            || !self.is_rolled(id)
            || self.settings.rollup_require_click
        {
            return;
        }
        if self.settings.rollup_require_delay {
            if self.pending_hover != Some(id) {
                self.pending_hover = Some(id);
                unsafe {
                    SetTimer(self.message_window, TIMER_HOVER_DELAY, rollup::hover_delay_ms(self.settings.rollup_require_delay_time), None);
                }
            }
            return;
        }
        self.open_hot(id);
    }

    /// The hover-delay timer fired: open when the pointer is still on the folded Corral, else the wait was cancelled.
    pub(super) fn hover_delay_elapsed(&mut self) {
        if let Some(id) = self.pending_hover.take() {
            if self.roll_hover_suppressed != Some(id) && self.cursor_in_group(id) {
                self.open_hot(id);
            }
        }
    }

    /// Make `id` the single hot (temporarily open) group; the previous one closes.
    pub(super) fn open_hot(&mut self, id: u64) {
        if let Some(previous) = self.hot_group.replace(id) {
            if previous != id {
                self.roll_animate(previous, false);
            }
        }
        self.roll_leave.reset();
        self.roll_animate(id, true);
        unsafe {
            SetTimer(self.message_window, TIMER_HOVER_POLL, 30, None);
        }
    }

    /// Close the hot group `id` with the close animation.
    pub(super) fn close_hot(&mut self, id: u64) {
        if self.hot_group == Some(id) {
            self.hot_group = None;
            self.roll_leave.reset();
            self.roll_animate(id, false);
        }
    }

    /// The temporary open closes 300 ms after the pointer left its outer rect, never while a button is
    /// down, a drag runs or a menu or rename box is open (motion-sorting-spec 3, [`rollup::LeaveGrace`]).
    pub(super) fn poll_hover_leave(&mut self) {
        if let Some(id) = self.roll_hover_suppressed {
            if !self.cursor_in_group(id) {
                self.roll_hover_suppressed = None;
            }
        }
        let Some(id) = self.hot_group else {
            if self.roll_hover_suppressed.is_none() {
                unsafe {
                    let _ = KillTimer(self.message_window, TIMER_HOVER_POLL);
                }
            }
            return;
        };
        let inside = self.cursor_in_open_group(id);
        let blocked = self.leave_blocked();
        if self.roll_leave.update(motion::now_ms(), inside, blocked) {
            self.close_hot(id);
        }
    }

    /// Something that must not lose its Corral under it is going on.
    fn leave_blocked(&self) -> bool {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
        let button = |vk: i32| unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000 != 0;
        let mut gui = GUITHREADINFO { cbSize: size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        let menu = unsafe { GetGUIThreadInfo(0, &mut gui) }.is_ok() && gui.flags.0 & (GUI_INMENUMODE.0 | GUI_POPUPMENUMODE.0) != 0;
        button(VK_LBUTTON.0 as i32) || button(VK_RBUTTON.0 as i32) || button(VK_MBUTTON.0 as i32)
            || !matches!(self.drag, Drag::None) || self.ole_drag.is_some()
            || self.naming.is_some() || self.renaming.is_some() || menu
    }

    /// Whether the cursor is on the folded Corral `id`: opening never uses the keep-open margin.
    fn cursor_in_group(&self, id: u64) -> bool {
        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        let (ox, oy) = self.origin();
        self.groups.iter().find(|g| g.id == id).is_some_and(|g| g.rect.contains(cursor.x - ox, cursor.y - oy))
    }

    /// Whether the cursor keeps the open (target) Corral open: its expanded outer rect clamped to the work area,
    /// grown by 12 DIP except along a docked edge, never into another Corral (ranges-dock-clearview-spec 4).
    fn cursor_in_open_group(&self, id: u64) -> bool {
        let mut cursor = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut cursor);
        }
        let (ox, oy) = self.origin();
        let Some(window) = self.groups.iter().find(|g| g.id == id) else { return false };
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id) else { return false };
        let geo = self.group_geometry(group);
        let full = geo.full_body;
        let open = Rect::new(window.rect.left.min(full.left), window.rect.top.min(full.top), window.rect.right.max(full.right), window.rect.bottom.max(full.bottom));
        let work = self.monitor_for(&group.monitor).map_or(open, |m| m.work.offset(-ox, -oy));
        let area = rollup::keep_open_area(open, work, group.dock_edge(), geo.dpi);
        let others: Vec<Rect> = self.groups.iter().filter(|g| g.id != id && self.page_group_id_visible(g.id)).map(|g| g.rect).collect();
        rollup::keeps_open((cursor.x - ox, cursor.y - oy), open, area, &others)
    }

    /// Fold openness shown now: the running tween's value, else the stored picture, else the state's rest value.
    pub(super) fn roll_openness(&self, id: u64) -> f64 {
        if self.roll_target.contains_key(&id) {
            if let Some(tween) = self.roll_tween.get(&id) {
                return tween.value(motion::now_ms());
            }
        }
        self.roll_progress.get(&id).copied().unwrap_or(if self.is_rolled(id) { 0.0 } else { rollup::OPEN })
    }

    /// Forget the Fold animation of `id`.
    pub(super) fn roll_clear(&mut self, id: u64) {
        self.roll_progress.remove(&id);
        self.roll_target.remove(&id);
        self.roll_tween.remove(&id);
        let mut api = WinTimers(self.message_window);
        self.roll_gate.end(id, &mut api);
    }

    /// Start or retarget the Fold animation of `id` towards open or folded. A reversal starts from the
    /// picture on screen (motion-sorting-spec 3); with Windows animations off it lands on the next tick.
    pub(super) fn roll_animate(&mut self, id: u64, open: bool) {
        let now = motion::now_ms();
        let current = self.roll_openness(id);
        let mut tween = Tween::at_rest(current);
        rollup::retarget_fold(&mut tween, now, open, motion::animations_enabled());
        self.roll_progress.insert(id, current);
        self.roll_target.insert(id, open);
        self.roll_tween.insert(id, tween);
        let mut api = WinTimers(self.message_window);
        self.roll_gate.start(id, &mut api);
    }

    /// One animation tick: advance every running Fold from the monotonic clock and repaint (spec 8).
    pub(super) fn step_roll_animation(&mut self) {
        let now = motion::now_ms();
        let ids: Vec<u64> = self.roll_target.keys().copied().collect();
        for id in ids {
            let Some(tween) = self.roll_tween.get(&id).copied() else { continue };
            let target_open = self.roll_target[&id];
            // Windows animations switched off mid-flight finish the fold at once (spec 7)
            let done = tween.finished(now) || !motion::animations_enabled();
            let value = if done { tween.target() } else { tween.value(now) };
            let changed = self.roll_progress.insert(id, value) != Some(value);
            if done {
                let mut api = WinTimers(self.message_window);
                self.roll_gate.end(id, &mut api);
                self.roll_tween.remove(&id);
                if !target_open {
                    self.roll_progress.remove(&id);
                    self.roll_target.remove(&id);
                }
            }
            if changed || done {
                if let Some(i) = self.groups.iter().position(|g| g.id == id) {
                    self.render_group(i);
                }
            }
        }
        // a Corral forgotten mid-flight must not keep the timer alive
        for key in self.roll_gate.keys() {
            if !self.roll_tween.contains_key(&key) || !self.roll_target.contains_key(&key) {
                self.roll_tween.remove(&key);
                let mut api = WinTimers(self.message_window);
                self.roll_gate.end(key, &mut api);
            }
        }
    }
}

/// The window timer API behind [`motion::TimerGate`].
pub(super) struct WinTimers(pub(super) HWND);

impl motion::TimerApi for WinTimers {
    fn set(&mut self, id: usize, interval_ms: u32) {
        unsafe { SetTimer(self.0, id, interval_ms, None); }
    }
    fn kill(&mut self, id: usize) {
        unsafe { let _ = KillTimer(self.0, id); }
    }
}

#[cfg(test)]
mod mixed_dpi_input_tests {
    use super::*;

    #[test]
    fn q33n_bottom_hits_follow_title_and_disable_only_the_anchored_resize_edge() {
        for dpi in [96,120,144,192,240] {
            let metrics=crate::group_paint::metrics(dpi);
            let work=Rect::new(-1920,-1400,0,0);
            let mut layout=crate::model::Layout::new();
            let id=layout.add_group("QA".into(),"QA".into(),Rect::new(-800,-750,-200,0),GroupContent::Items { items: vec![] });
            let group=layout.group_mut(id).unwrap();
            group.set_dock_edge(Some(rollup::DockEdge::Bottom));
            for roll in [RollState::Docked,RollState::Manual,RollState::Expanded] {
                group.roll=roll;
                for auto in [false,true] {
                    for progress in [0.0,0.16,0.5,0.8,1.0,0.5,0.0] {
                        let geo=super::super::GroupGeometry::edge_docked(group.rect,work,metrics,dpi,progress,rollup::DockEdge::Bottom,auto);
                        let x=geo.title.left+geo.title.width()/2;
                        let centre=(x,geo.title.top+geo.title.height()/2);
                        for _second in [false,true] {
                            assert_eq!(group_hit_zone(&geo,group,centre,false,&[]),EdgeZone::TITLE);
                            assert_eq!(group_hit_zone(&geo,group,centre,true,&[]),EdgeZone::BODY);
                            assert_eq!(group_hit_zone(&geo,group,(x,geo.union.top-20),false,&[]),EdgeZone::NONE);
                            if geo.body.height()>2*metrics.title_h {
                                let middle=(x,geo.body.top+geo.body.height()/2);
                                assert_eq!(group_hit_zone(&geo,group,middle,false,&[]),EdgeZone::BODY);
                                assert_eq!(group_hit_zone(&geo,group,(x,geo.body.bottom-1),false,&[]),EdgeZone::BODY);
                                assert!(!geo.title.contains(middle.0,middle.1));
                                assert_eq!(group_hit_zone(&geo,group,(geo.body.left,middle.1),false,&[]),EdgeZone::LEFT);
                                assert_eq!(group_hit_zone(&geo,group,(geo.body.right-1,middle.1),false,&[]),EdgeZone::RIGHT);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn q33m_hits_follow_the_moving_title_and_revealed_body_for_both_settings() {
        for dpi in [96,120,144,192,240] {
            let metrics=crate::group_paint::metrics(dpi);
            let mut layout=crate::model::Layout::new();
            let id=layout.add_group("QA".into(),"QA".into(),Rect::new(-800,34,-200,534),GroupContent::Items { items: vec![] });
            let group=layout.group_mut(id).unwrap();group.dock_top=true;
            for roll in [RollState::Docked,RollState::Manual,RollState::Expanded] {
                group.roll=roll;
                for auto in [false,true] {
                    for progress in [0.0,0.16,0.5,0.8,1.0,0.5,0.0] {
                        let geo=super::super::GroupGeometry::top_docked(group.rect,-1400,metrics,dpi,progress,auto);
                        let centre=(geo.title.left+geo.title.width()/2,geo.title.top+geo.title.height()/2);
                        for _second in [false,true] {
                            assert_eq!(group_hit_zone(&geo,group,centre,false,&[]),EdgeZone::TITLE);
                            assert_eq!(group_hit_zone(&geo,group,centre,true,&[]),EdgeZone::BODY);
                            if geo.body.height()>2*metrics.title_h {
                                let middle=(centre.0,geo.body.top+geo.body.height()/2);
                                assert_eq!(group_hit_zone(&geo,group,middle,false,&[]),EdgeZone::BODY);
                                if auto {
                                    assert_eq!(group_hit_zone(&geo,group,(centre.0,geo.body.top),false,&[]),EdgeZone::BODY,
                                        "anchored top is neither a phantom title nor a resize band");
                                    assert!(!geo.title.contains(middle.0,middle.1));
                                }
                            }
                            assert_eq!(group_hit_zone(&geo,group,(centre.0,geo.union.bottom+20),false,&[]),EdgeZone::NONE);
                        }
                    }
                }
            }
        }
    }

    use crate::dpi_transition::{RequestedBody, govern, place_governed};
    use crate::app::dpi_wiring_tests::displays;

    #[test]
    fn q33n_bottom_dock_move_across_mixed_dpi_uses_destination_work_edge() {
        use crate::dpi_transition::{Grab,move_layout_fitted};
        use rollup::DockEdge::Bottom;
        for stacked in [false,true] {
            let live=displays(stacked);
            for from in live.displays() {
                let metrics=crate::group_paint::metrics(from.dpi);
                let body=Rect::new(from.work.left+100,from.work.bottom-to_px(300,from.dpi),
                    from.work.left+100+to_px(400,from.dpi),from.work.bottom);
                let (title,_,full)=Bottom.layout(body,from.work,metrics.title_h,metrics.gap,0,false);
                let (drag,below)=Bottom.drag_body(full,title,false,metrics.gap,from.work);
                let press=(title.left+title.width()/2,title.top+title.height()/2);
                let grab=Grab { px:(press.0-drag.left,press.1-drag.top),dpi:from.dpi,size_dip:(400,300) };
                for to in live.displays() {
                    let m=crate::group_paint::metrics(to.dpi);
                    let cursor=(to.work.left+to.work.width()/2,to.work.bottom-1);
                    let raw=move_layout_fitted(grab,cursor,to,(400,300)).unwrap();
                    let winner=govern(&live,&from.device,|d| Some(move_layout_fitted(grab,cursor,d,(400,300))?.physical())).unwrap();
                    assert_eq!(winner.device,to.device);
                    let fitted=Bottom.clamp(raw.physical(),below,m.title_h,m.gap,to.work,false);
                    let title=crate::group_paint::title_rect_on(fitted,below,&m);
                    let geo=super::super::GroupGeometry { body:fitted,full_body:fitted,title,
                        union:crate::group_paint::union_rect(fitted,title,false),below,rolled:false,metrics:m,dpi:to.dpi };
                    assert_eq!(rollup::dock_on_drop(geo.union,to.work,to.dpi,false,None),Some(Bottom));
                    let geo=super::super::GroupGeometry::edge_docked(fitted,to.work,m,to.dpi,0.0,Bottom,true);
                    assert_eq!(geo.title.bottom,to.work.bottom);
                    assert_eq!(geo.full_body.height(),to_px(300,to.dpi));
                    assert_eq!(geo.metrics.title_h,m.title_h);
                }
            }
        }
    }

    /// The press point of a grab on `zone` of `start`, so the grabbed edge follows the cursor exactly.
    fn press_of(zone: EdgeZone, start: Rect) -> (i32, i32) {
        let (mx, my) = ((start.left + start.right) / 2, (start.top + start.bottom) / 2);
        if zone.intersects(EdgeZone::RIGHT) { (start.right, my) }
        else if zone.intersects(EdgeZone::LEFT) { (start.left, my) }
        else if zone.intersects(EdgeZone::TOP) { (mx, start.top) }
        else { (mx, start.bottom) }
    }

    fn sweep_input<'a>(zone: EdgeZone, start: Rect, cursor: (i32, i32), dpi: u32, work: Rect) -> ResizeInput<'a> {
        ResizeInput {
            zone, start, strip_top: 0, strip_bottom: 0, start_pointer: press_of(zone, start), pointer: cursor, dpi, work,
            cell: crate::icon_metrics::cell_metrics(&crate::icon_metrics::CellInputs { dpi, zoom_pct: 100, windows_spacing_dip: Some((75.0, 75.0)), desktop_icon_dip: Some(48.0), match_desktop: true }),
            settings: SnapSettings::new(false, 0, false, false, false), shift: false, neighbours: &[], lock: snap::SnapLock::default(), min: None,
        }
    }

    /// The App's resize step without Win32: sticky governing monitor judged with each candidate's
    /// own minimum and work area, then the resize on the winner (grid-snapping-spec 3).
    fn resize_sweep(live: &crate::dpi_transition::Snapshot, zone: EdgeZone, start: Rect, mut gov: String,
        path: &[(i32, i32)]) -> (String, usize, Vec<Rect>) {
        let (mut flips, mut rects) = (0, Vec::new());
        for &cursor in path {
            let g = govern(live, &gov, |m| Some(snap::resize_rect(&sweep_input(zone, start, cursor, m.dpi, m.work)).rect)).unwrap();
            if g.device != gov { flips += 1; gov = g.device.clone(); }
            let rect = snap::resize_rect(&sweep_input(zone, start, cursor, g.dpi, g.work)).rect;
            // the opposite edge never moves, whatever DPI governs
            if !zone.intersects(EdgeZone::LEFT) { assert_eq!(rect.left, start.left); }
            if !zone.intersects(EdgeZone::RIGHT) { assert_eq!(rect.right, start.right); }
            if !zone.intersects(EdgeZone::TOP) { assert_eq!(rect.top, start.top); }
            if !zone.intersects(EdgeZone::BOTTOM) { assert_eq!(rect.bottom, start.bottom); }
            rects.push(rect);
        }
        (gov, flips, rects)
    }

    #[test]
    fn resize_follows_the_cursor_and_stops_at_the_work_edge_of_the_governing_monitor() {
        let live = displays(false); // Monitor B (96 dpi) left of the seam at x=0, Monitor A (240 dpi) right of it
        let mon_b = live.find("Monitor B").unwrap().work;
        let start = Rect::new(-900, 200, -300, 500);
        let path: Vec<_> = (-300..2500).step_by(7).map(|x| (x, 300)).collect();
        let (gov, flips, rects) = resize_sweep(&live, EdgeZone::RIGHT, start, "Monitor B".into(), &path);
        assert_eq!((gov.as_str(), flips), ("Monitor B", 0), "a Corral never straddles monitors: no flip at the seam");
        for (r, c) in rects.iter().zip(&path) {
            assert!(r.right <= mon_b.right);
            if c.0 <= mon_b.right { assert_eq!(r.right, c.0, "the dragged edge IS the cursor inside the work area"); }
        }
        assert_eq!(rects.last().unwrap().right, mon_b.right);
        // and back: the edge follows the cursor again, nothing is left over
        let back: Vec<_> = path.iter().rev().cloned().collect();
        let (gov, flips, rects) = resize_sweep(&live, EdgeZone::RIGHT, start, gov, &back);
        assert_eq!((gov.as_str(), flips), ("Monitor B", 0));
        assert_eq!(rects.last().unwrap().right, -300);
    }

    #[test]
    fn left_and_top_edge_resizes_keep_the_far_edge_and_stay_on_their_monitor() {
        for stacked in [false, true] {
            let live = displays(stacked);
            let (start, zone, path): (Rect, EdgeZone, Vec<(i32, i32)>) = if stacked {
                (Rect::new(-300, 800, 300, 1300), EdgeZone::TOP, (-1400..700).step_by(5).rev().map(|y| (0, y)).collect())
            } else {
                (Rect::new(600, 200, 1400, 500), EdgeZone::LEFT, (-1500..600).step_by(5).rev().map(|x| (x, 300)).collect())
            };
            let mon_a = live.find("Monitor A").unwrap().work;
            let (gov, flips, rects) = resize_sweep(&live, zone, start, "Monitor A".into(), &path);
            assert_eq!((gov.as_str(), flips), ("Monitor A", 0), "stacked {stacked}");
            let last = rects.last().unwrap();
            assert!(if stacked { last.top == mon_a.top } else { last.left == mon_a.left });
        }
    }

    #[test]
    fn holding_the_cursor_still_reaches_a_fixed_point_whatever_the_starting_monitor() {
        let live = displays(false);
        let start = Rect::new(-80, 200, -20, 700);
        for from in ["Monitor B", "Monitor A"] {
            for x in (-120..200).step_by(3) {
                let (g1, _, r1) = resize_sweep(&live, EdgeZone::RIGHT, start, from.into(), &[(x, 230), (x, 230), (x, 230)]);
                let (g2, f2, r2) = resize_sweep(&live, EdgeZone::RIGHT, start, g1.clone(), &[(x, 230), (x, 230)]);
                assert_eq!((g1, f2), (g2, 0));
                assert_eq!(r1[2], r2[1]);
            }
        }
    }

    #[test]
    fn rescue_keeps_the_outer_rect_on_the_governing_monitor() {
        let live = displays(false);
        let r = RequestedBody::from_physical(Rect::new(10, 200, 100, 503)).unwrap();
        let mon_a = live.find("Monitor A").unwrap();
        let p = place_governed(r, r.physical(), mon_a, &live, None).unwrap();
        let dest = live.find(&p.monitor).unwrap();
        assert_eq!(snap::place_on_screen(r.physical(), &[dest.work], Some(0), None).rect, r.physical());
        assert_ne!(snap::place_on_screen(r.physical(), &[live.find("Monitor B").unwrap().work], Some(0), None).rect, r.physical());
    }

    #[test]
    fn fractional_resize_seams_right_above_below_and_three_displays() {
        use crate::dpi_transition::{DisplaySnapshot,Snapshot};
        use crate::desk_setup::RuntimeHardware;
        let make=|name:&str,bounds:Rect,dpi,primary| DisplaySnapshot{device:name.into(),bounds,work:bounds,dpi,primary,hardware:RuntimeHardware::default()};
        for (a,b) in [(96,144),(120,168)] {
            for direction in [0,1,2] {
                let bounds=match direction {0=>Rect::new(1920,0,3840,1080),1=>Rect::new(0,-1080,1920,0),_=>Rect::new(0,1080,1920,2160)};
                let live=Snapshot::new(vec![make("A",Rect::new(0,0,1920,1080),a,true),make("B",bounds,b,false),
                    make("C",Rect::new(-1080,0,0,1920),192,false)]).unwrap();
                let (zone,start,path)=match direction {
                    0=>(EdgeZone::RIGHT,Rect::new(1750,100,1900,350),(1900..2700).step_by(7).map(|x|(x,200)).collect::<Vec<_>>()),
                    1=>(EdgeZone::TOP,Rect::new(100,10,400,100),(-800..10).rev().step_by(7).map(|y|(200,y)).collect()),
                    _=>(EdgeZone::BOTTOM,Rect::new(100,950,400,1070),(1070..1500).step_by(7).map(|y|(200,y)).collect()),
                };
                let (gov,flips,rects)=resize_sweep(&live,zone,start,"A".into(),&path);
                assert_eq!(gov,"A"); assert_eq!(flips,0);
                let d=live.find(&gov).unwrap();
                let raw=RequestedBody::from_physical(*rects.last().unwrap()).unwrap();
                let settled=crate::dpi_transition::settle_on_grid(raw,d,None).unwrap();
                let p=place_governed(settled,settled.physical(),d,&live,None).unwrap();
                assert_eq!(RequestedBody::from_dips(p.rect_dip,d),Some(settled));
            }
        }
    }

}

#[cfg(test)]
mod context_menu_tests {
    use super::*;

    #[test]
    fn title_menu_tracks_the_cursor_at_each_dpi() {
        for dpi in [96, 240] {
            let group = Rect::new(to_px(-200, dpi), to_px(100, dpi), to_px(400, dpi), to_px(500, dpi));
            for cursor in [(10, 8), (160, 20)] {
                let point = (to_px(cursor.0, dpi), to_px(cursor.1, dpi));
                assert_eq!(title_context_menu_point(point, group),
                    (to_px(-200 + cursor.0, dpi), to_px(100 + cursor.1, dpi)));
            }
        }
    }
}
