//! Window side of creating Corrals and Folder Gates (creation-spec): the draw gesture on the desktop
//! surface, the chooser, the folder picker and the Gates an Alt-drop makes. The rules are in
//! `crate::creation`; this file only reads input state, calls them and applies their answers.

use super::{App, log};
use crate::creation::{self, CancelReason, ChooserChoice, DrawButton, DrawEffect, DrawEvent, DrawState};
use crate::geometry::{Monitor, Rect, to_dip};
use crate::model::GroupContent;
use crate::size_tip;
use crate::snap;
use windows::Win32::Foundation::{HWND, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::HiDpi::GetSystemMetricsForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, GetKeyState, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT};
use windows::Win32::UI::Shell::{FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FOLDERID_Documents, FileOpenDialog, IFileOpenDialog, IShellItem, KF_FLAG_DEFAULT, SHCreateItemFromParsingName, SHGetKnownFolderPath, SIGDN_FILESYSPATH};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::HSTRING;

const MK_LBUTTON: usize = 0x0001;
const MK_RBUTTON: usize = 0x0002;
const MK_MBUTTON: usize = 0x0010;

fn key_down(key: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(key.0 as i32) < 0 || GetAsyncKeyState(key.0 as i32) < 0 }
}

/// What the caller of [`App::draw_finish`] still has to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DrawEnd {
    /// The press-release was an ordinary click: the caller runs the normal desktop click.
    Click,
    /// Handled here (chooser shown, or nothing to create); no click.
    Done,
}

impl App {
    pub(super) fn draw_active(&self) -> bool {
        self.draw != DrawState::Idle
    }

    fn monitor_at_screen(&self, p: (i32, i32)) -> Option<Monitor> {
        self.monitors.iter().find(|m| m.bounds.contains(p.0, p.1)).or(self.monitors.first()).cloned()
    }

    /// Facts of a button press for [`creation::qualifies_press`] (creation-spec 2.1).
    fn press_facts(&self, button: DrawButton, point: (i32, i32), wparam: WPARAM) -> creation::PressFacts {
        let buttons = wparam.0 & (MK_LBUTTON | MK_RBUTTON | MK_MBUTTON);
        let own = match button { DrawButton::Left => MK_LBUTTON, DrawButton::Right => MK_RBUTTON, DrawButton::Middle => MK_MBUTTON };
        creation::PressFacts {
            button,
            mode: self.mouse_mode,
            touch_or_pen: creation::is_touch_or_pen(unsafe { GetMessageExtraInfo() }.0),
            on_empty_desktop: self.item_at_surface(point).is_none(),
            shift: key_down(VK_SHIFT),
            ctrl: key_down(VK_CONTROL),
            alt: key_down(VK_MENU),
            win: key_down(VK_LWIN) || key_down(VK_RWIN),
            other_button_down: buttons & !own != 0,
            clear_view: self.quick_hide.is_hidden() || self.quick_hide.animating(),
            range_transition: self.pages.moving(),
        }
    }

    /// A button went down on the empty surface. True when it became a pending draw, in which case the
    /// desktop selection is left alone until the gesture is known (creation-spec 2.1, 2.2).
    pub(super) fn draw_begin(&mut self, button: DrawButton, point: (i32, i32), wparam: WPARAM) -> bool {
        if self.draw_active() {
            self.draw_cancel(CancelReason::OtherButton);
        }
        let qualifies = creation::qualifies_press(&self.press_facts(button, point, wparam));
        if !qualifies {
            return false;
        }
        let (next, _) = creation::draw_step(self.draw, DrawEvent::Press { button, at: point, qualifies });
        self.draw = next;
        unsafe {
            // [inferred] taken at the press, not at arming: the moves must keep reaching this window when
            // the pointer crosses a Corral window before the drag arms
            SetCapture(self.surface_hwnd);
            self.peek_set_focus(self.surface_hwnd);
        }
        true
    }

    fn draw_dpi(&self, start: (i32, i32)) -> u32 {
        let (ox, oy) = self.origin();
        self.monitor_at_screen((start.0 + ox, start.1 + oy)).map_or(96, |m| m.dpi)
    }

    /// A mouse move while a draw is pending or running.
    pub(super) fn draw_move(&mut self, point: (i32, i32), wparam: WPARAM) {
        let (button, start) = match self.draw {
            DrawState::Pending { button, start } | DrawState::Drawing { button, start } => (button, start),
            DrawState::Idle => return,
        };
        let own = if button == DrawButton::Left { MK_LBUTTON } else { MK_RBUTTON };
        if wparam.0 & own == 0 {
            // the button went up where this window did not see it
            self.draw_cancel(CancelReason::CaptureLost);
            return;
        }
        if wparam.0 & (MK_LBUTTON | MK_RBUTTON | MK_MBUTTON) & !own != 0 {
            self.draw_cancel(CancelReason::OtherButton);
            return;
        }
        if self.quick_hide.is_hidden() || self.quick_hide.animating() {
            self.draw_cancel(CancelReason::ClearView);
            return;
        }
        if self.pages.moving() {
            self.draw_cancel(CancelReason::RangeChange);
            return;
        }
        let dpi = self.draw_dpi(start);
        let drag = unsafe { (GetSystemMetricsForDpi(SM_CXDRAG, dpi), GetSystemMetricsForDpi(SM_CYDRAG, dpi)) };
        let (next, effect) = creation::draw_step(self.draw, DrawEvent::Move { at: point, arm_px: creation::arm_px(dpi, drag) });
        self.draw = next;
        match effect {
            DrawEffect::Armed => {
                self.update_creation_preview(start, point);
                self.render_surface();
            }
            DrawEffect::Preview => {
                self.update_creation_preview(start, point);
                self.render_surface();
            }
            _ => {}
        }
    }

    /// Ends a pending or running gesture without creating anything and without a click.
    pub(super) fn draw_cancel(&mut self, reason: CancelReason) -> bool {
        if !self.draw_active() {
            return false;
        }
        let was_drawing = matches!(self.draw, DrawState::Drawing { .. });
        let (next, _) = creation::draw_step(self.draw, DrawEvent::Cancel(reason));
        self.draw = next;
        log(&format!("creation: draw cancelled ({reason:?})"));
        // the press that was cancelled is no click: its release must not open the desktop menu
        self.right_start = None;
        self.draw_cleanup();
        if was_drawing {
            self.render_surface();
        }
        true
    }

    /// A button went down while a gesture may be live. A gesture of the other button ends and swallows
    /// this press (true); a stale gesture of the same button (its release was lost) just ends.
    pub(super) fn draw_cancel_for_press(&mut self, pressed: DrawButton) -> bool {
        let live = match self.draw {
            DrawState::Pending { button, .. } | DrawState::Drawing { button, .. } => button,
            DrawState::Idle => return false,
        };
        self.draw_cancel(CancelReason::OtherButton);
        live != pressed
    }

    fn draw_cleanup(&mut self) {
        self.creation_preview = None;
        self.snap_guides.clear();
        self.snap_lock = snap::SnapLock::default();
        size_tip::hide();
        unsafe {
            let _ = ReleaseCapture();
        }
    }

    /// The drawing button went up. `None` when no gesture of that button is live.
    pub(super) fn draw_finish(&mut self, button: DrawButton, point: (i32, i32)) -> Option<DrawEnd> {
        let (live, start) = match self.draw {
            DrawState::Pending { button, start } | DrawState::Drawing { button, start } => (button, start),
            DrawState::Idle => return None,
        };
        if live != button {
            return None;
        }
        // Clear view or a Range change since the last move ends the gesture too (creation-spec 2.1 rule 5, 2.2)
        if self.quick_hide.is_hidden() || self.quick_hide.animating() {
            self.draw_cancel(CancelReason::ClearView);
            return Some(DrawEnd::Done);
        }
        if self.pages.moving() {
            self.draw_cancel(CancelReason::RangeChange);
            return Some(DrawEnd::Done);
        }
        let shift = key_down(VK_SHIFT);
        let valid = matches!(self.draw, DrawState::Drawing { .. })
            && self.creation_geometry(start, point, shift, snap::SnapLock::default()).is_some();
        let (next, effect) = creation::draw_step(self.draw, DrawEvent::Release { rect_valid: valid });
        self.draw = next;
        match effect {
            DrawEffect::Chooser => {
                // the final point with the held snap lock: what is created equals the last preview
                if self.creation_preview.as_ref().is_none_or(|p| p.start != start || p.end != point) {
                    self.update_creation_preview(start, point);
                }
                unsafe {
                    let _ = ReleaseCapture();
                }
                size_tip::hide();
                self.render_surface(); // the preview stays visible, frozen, while the chooser is open
                self.show_chooser(start, point);
                self.draw_cleanup();
                self.render_surface();
                Some(DrawEnd::Done)
            }
            DrawEffect::Click => {
                self.draw_cleanup();
                Some(DrawEnd::Click)
            }
            _ => {
                self.draw_cleanup();
                self.render_surface();
                Some(DrawEnd::Done)
            }
        }
    }

    /// Title strip above the body of a new Corral at `dpi`; a hidden label takes no room.
    fn title_strip_px(&self, dpi: u32) -> i32 {
        if self.settings.label_mode >= 1 {
            let m = crate::group_paint::metrics(dpi);
            m.title_h + m.gap
        } else {
            0
        }
    }

    /// The Corral a drag from `start` to `end` (parent-client px) would create (grid-snapping-spec 6):
    /// the monitor under the press point and the snapped outer rect and body in screen px. `None` for a
    /// click or a slip.
    pub(super) fn creation_geometry(&self, start: (i32, i32), end: (i32, i32), shift: bool, lock: snap::SnapLock)
        -> Option<(Monitor, creation::CreationRect)> {
        let (ox, oy) = self.origin();
        let (press, pointer) = ((start.0 + ox, start.1 + oy), (end.0 + ox, end.1 + oy));
        let monitor = self.monitor_at_screen(press)?;
        let dpi = monitor.dpi;
        let neighbours = self.snap_neighbours(0, &monitor.name);
        let cell = crate::icon_metrics::cell_metrics(&self.cell_inputs(dpi, crate::icon_metrics::ZOOM_DEFAULT, self.settings.group_spacing_matches_desktop));
        let out = creation::creation_rect(&creation::CreationInput {
            anchor: press, pointer, work: monitor.work, dpi, strip_top: self.title_strip_px(dpi), strip_bottom: 0, cell,
            settings: self.snap_settings(), shift, neighbours: &neighbours, lock,
        })?;
        Some((monitor, out))
    }

    /// Updates the preview and the size tooltip of a drawing drag (creation-spec 2.3, grid-snapping-spec 7).
    fn update_creation_preview(&mut self, start: (i32, i32), point: (i32, i32)) {
        let shift = key_down(VK_SHIFT);
        let lock = if self.creation_preview.is_some() { self.snap_lock } else { snap::SnapLock::default() };
        match self.creation_geometry(start, point, shift, lock) {
            Some((monitor, rect)) => {
                self.snap_lock = rect.lock;
                self.snap_guides = rect.guides.clone();
                let settings = self.snap_settings();
                let cells = (settings.whole_cells() && !shift).then(|| {
                    let cell = crate::icon_metrics::cell_metrics(&self.cell_inputs(monitor.dpi, crate::icon_metrics::ZOOM_DEFAULT, self.settings.group_spacing_matches_desktop));
                    cell.cells_for_size(rect.body.width(), rect.body.height())
                });
                size_tip::show(self.message_window, &snap::size_tip_text((rect.outer.width(), rect.outer.height()), cells));
                self.creation_preview = Some(super::CreationPreview { start, end: point, monitor: monitor.name.clone(), rect });
            }
            None => {
                self.snap_lock = snap::SnapLock::default();
                self.snap_guides.clear();
                self.creation_preview = None;
                size_tip::hide();
            }
        }
    }

    /// The chooser (creation-spec 3): a popup menu with two items; it never creates when it is closed
    /// without a choice. Keyboard, hover and focus loss are the system menu's.
    fn show_chooser(&mut self, start: (i32, i32), end: (i32, i32)) {
        let (ox, oy) = self.origin();
        let release = (end.0 + ox, end.1 + oy);
        let Some(monitor) = self.monitor_at_screen((start.0 + ox, start.1 + oy)) else { return };
        let place = creation::chooser_placement(release, monitor.dpi, monitor.work);
        let Some(geometry) = self.previewed_geometry(start, end) else { return };
        let choice = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            for (index, (_, text)) in creation::CHOOSER_ITEMS.iter().enumerate() {
                let _ = AppendMenuW(menu, MF_STRING, index + 1, &HSTRING::from(*text));
            }
            // QA hook: CORRAL_QA_MENU_CHOICE=<1-based item> answers the chooser without UI (automated trials only).
            let qa_choice = std::env::var("CORRAL_QA_MENU_CHOICE").ok().and_then(|v| v.parse::<i32>().ok());
            let flags = TPM_RETURNCMD | TPM_RIGHTBUTTON
                | if place.align_right { TPM_RIGHTALIGN } else { TPM_LEFTALIGN }
                | if place.align_bottom { TPM_BOTTOMALIGN } else { TPM_TOPALIGN };
            let id = match qa_choice {
                Some(choice) => windows::Win32::Foundation::BOOL(choice),
                None => TrackPopupMenu(menu, flags, place.at.0, place.at.1, 0, self.surface_hwnd, None),
            };
            let _ = DestroyMenu(menu);
            id.0 as usize
        };
        let Some(&(choice, _)) = creation::CHOOSER_ITEMS.get(choice.wrapping_sub(1)) else { return };
        let created = match choice {
            ChooserChoice::Corral => self.create_group(geometry, GroupContent::Items { items: Vec::new() }, creation::DEFAULT_TITLE.to_string()),
            ChooserChoice::FolderGate => {
                // the preview is hidden while the picker is open; the previewed monitor and rect are kept
                self.creation_preview = None;
                self.render_surface();
                let Some(path) = pick_gate_folder() else { return };
                let title = folder_title(&path);
                self.create_group(geometry, GroupContent::Portal { path }, title)
            }
        };
        if created.is_none() {
            return;
        }
        self.record_desk_edit();
        self.save();
        self.arrange_loose();
        self.refresh_all();
    }

    /// Creates the Corral of a drawn rectangle from the geometry the user saw previewed. The body is
    /// stored relative to the work area of the monitor under the press point and its size becomes the
    /// preferred size (creation-spec 4).
    fn create_group(&mut self, geometry: (Monitor, creation::CreationRect), content: GroupContent, title: String) -> Option<u64> {
        let (monitor, out) = geometry;
        let rect_dip = creation::body_to_dip(out.body, monitor.work, monitor.dpi);
        log(&format!("create_group outer={:?} body=({},{}) monitor={} work={:?} dpi={} dip={rect_dip:?}",
            out.outer, out.body.width(), out.body.height(), monitor.name, monitor.work, monitor.dpi));
        Some(self.add_created_group(title, monitor.name, rect_dip, content, None))
    }

    /// The geometry of the drawn rectangle: the preview the user saw, else the same rules on the final
    /// points. The preview stays where it is.
    fn previewed_geometry(&self, start: (i32, i32), end: (i32, i32)) -> Option<(Monitor, creation::CreationRect)> {
        let preview = self.creation_preview.as_ref().filter(|p| p.start == start && p.end == end);
        match preview.and_then(|p| Some((self.monitors.iter().find(|m| m.name == p.monitor)?.clone(), p.rect.clone()))) {
            Some(found) => Some(found),
            None => self.creation_geometry(start, end, key_down(VK_SHIFT), snap::SnapLock::default()),
        }
    }

    /// Adds a Corral on the active Range and records its created size (creation-spec 4).
    fn add_created_group(&mut self, title: String, monitor: String, rect_dip: Rect, content: GroupContent, preferred: Option<(i32, i32)>) -> u64 {
        // a new Corral or Gate ends Clear view first (ranges-dock-clearview-spec 5.4)
        self.qh_end_hidden();
        let id = self.layout.add_group(title, monitor, rect_dip, content);
        if let Some(group) = self.layout.group_mut(id) {
            group.preferred_size_dip = Some(preferred.unwrap_or((rect_dip.width().max(1), rect_dip.height().max(1))));
            if self.settings.swipe_enabled { group.swipe_index = self.pages.current; }
        }
        id
    }

    /// Alt-drop (creation-spec 5, 6): one Folder Gate per folder, tiled around the drop point. Only the
    /// layout changes; the folders are never read beyond their path.
    pub(super) fn create_quick_gates(&mut self, paths: &[String], screen: (i32, i32)) {
        let bounds: Vec<Rect> = self.monitors.iter().map(|m| m.bounds).collect();
        let Some(governing) = super::selection::monitor_index_at(screen, &bounds) else { return };
        let monitor = self.monitors[governing].clone();
        let works: Vec<Rect> = self.monitors.iter().map(|m| m.work).collect();
        let dpi = monitor.dpi;
        let cell = crate::icon_metrics::cell_metrics(&self.cell_inputs(dpi, crate::icon_metrics::ZOOM_DEFAULT, self.settings.group_spacing_matches_desktop));
        let strip = self.title_strip_px(dpi);
        let neighbours = self.snap_neighbours(0, &monitor.name);
        let rects = creation::quick_gate_rects(&creation::QuickGateInput {
            drop: screen, count: paths.len(), works: &works, governing, dpi, strip_px: strip, cell,
            settings: self.snap_settings(), neighbours: &neighbours,
        });
        let (body_w, body_h) = cell.body_size_for(creation::QUICK_GATE_CELLS.0, creation::QUICK_GATE_CELLS.1);
        let preferred = (to_dip(body_w, dpi).max(1), to_dip(body_h, dpi).max(1));
        for (path, outer) in paths.iter().zip(rects) {
            let body = Rect::new(outer.left, outer.top + strip, outer.right, outer.bottom);
            let rect_dip = creation::body_to_dip(body, monitor.work, dpi);
            log(&format!("quick gate outer={outer:?} dip={rect_dip:?}"));
            self.add_created_group(folder_title(path), monitor.name.clone(), rect_dip, GroupContent::Portal { path: path.clone() }, Some(preferred));
        }
        self.record_desk_edit();
        self.save();
        self.refresh_all();
    }
}

/// Default title of a Gate: the folder's name, else its whole path (a drive root).
fn folder_title(path: &str) -> String {
    std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

/// The system folder picker for a new Gate (creation-spec 4): file-system folders only, owned by no
/// Corral window, starting in Documents. Cancel returns `None`.
fn pick_gate_folder() -> Option<String> {
    // QA hook: CORRAL_QA_PICK_FOLDER=<path> answers the folder picker without UI (automated trials only).
    if let Ok(path) = std::env::var("CORRAL_QA_PICK_FOLDER") {
        return Some(path).filter(|p| !p.is_empty());
    }
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM).ok()?;
        if let Ok(known) = SHGetKnownFolderPath(&FOLDERID_Documents, KF_FLAG_DEFAULT, None) {
            let start = known.to_string().ok();
            CoTaskMemFree(Some(known.0 as *const _));
            if let Some(folder) = start.and_then(|p| SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(p), None).ok()) {
                let _ = dialog.SetFolder(&folder);
            }
        }
        dialog.Show(HWND::default()).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        path
    }
}
