//! Drawn Folder Gates: items, navigation, sort state and the Gate menu (folder-gate-host-spec 2 to 5).

use crate::gate_host::{self, Core};
use crate::geometry::Rect;
use crate::model::{self, GroupContent};
use crate::portal::{self, Probe};
use crate::portal_menu::{self, GateMenuState, Kind};
use crate::shell;
use crate::view_state;
use std::cell::RefCell;
use std::rc::Rc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::{FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};
use super::{App, PortalView, log};
use super::group_menu;

thread_local! {
    static QA_PORTAL_MENU: std::cell::RefCell<Option<std::collections::VecDeque<u32>>> = const { std::cell::RefCell::new(None) };
}

/// Next answer of the `CORRAL_QA_PORTAL_MENU` queue (0 = dismiss the menu), if the hook is set.
pub(super) fn qa_portal_menu_choice() -> Option<u32> {
    QA_PORTAL_MENU.with(|cell| {
        let mut cell = cell.borrow_mut();
        let queue = cell.get_or_insert_with(|| {
            std::env::var("CORRAL_QA_PORTAL_MENU")
                .unwrap_or_default()
                .split(',')
                .filter_map(|v| u32::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok())
                .collect()
        });
        queue.pop_front()
    })
}

/// Message-window timers of drawn Gates, one per Gate: the 3000 ms probe limit and the offline retry.
pub(super) const GATE_PROBE_TIMER: usize = 0x4750_0000;
pub(super) const GATE_RETRY_TIMER: usize = 0x4760_0000;

fn gate_timer(base: usize, id: u64) -> usize {
    base + (id as usize & 0xf_ffff)
}

/// Corral sort method and flags for a stored Gate sort (folder-gate-views-spec 2: +-1 Name, 2 Size, 3 Type,
/// 4 Date modified, 5 Date created; negative = descending; 0 = Name ascending).
fn gate_sort(sort: i32) -> (u32, u32) {
    use crate::sorting::*;
    let method = match sort.abs() { 2 => METHOD_SIZE, 3 => METHOD_TYPE, 4 => METHOD_MODIFIED, 5 => METHOD_CREATED, _ => METHOD_NAME };
    (method, if sort < 0 { FLAG_REVERSE } else { 0 })
}

impl Drop for PortalView {
    fn drop(&mut self) {
        super::hosting::deregister_notifications(self.notify);
    }
}

pub(super) const WM_GATE_ITEMS: u32 = WM_APP + 0x45;
/// Hands over Gate messages that arrived while the application was busy (`defer_gate_message`).
const GATE_DEFER_TIMER: usize = 0x4770_0000;

thread_local! {
    static DEFERRED_GATE: RefCell<Vec<(u32, isize)>> = const { RefCell::new(Vec::new()) };
}

/// Keep a Gate listing or probe answer (its boxed payload) that arrived inside a modal loop; the timer hands it
/// to the application once it is free again.
pub(super) fn defer_gate_message(window: HWND, msg: u32, lparam: LPARAM) {
    DEFERRED_GATE.with(|queue| queue.borrow_mut().push((msg, lparam.0)));
    unsafe { let _ = SetTimer(window, GATE_DEFER_TIMER, 50, None); }
}

struct GateItems {
    id: u64,
    seq: u64,
    path: String,
    result: Result<Vec<(shell::ShellItem, super::details::Text)>, u32>,
}

/// Global sequence numbers also reject results from a removed and recreated Gate with the same id.
fn gate_read_matches(current: Option<(u64, &str)>, seq: u64, path: &str) -> bool {
    current.is_some_and(|(latest, folder)| latest != 0 && latest == seq && portal::same_folder(folder, path))
}

/// A navigation step of a Gate command (folder-gate-host-spec 4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PortalStep {
    Up,
    Back,
    Forward,
    Root,
}

impl PortalView {
    pub(super) fn core_state(&self) -> (bool, bool) {
        self.core.try_borrow().map_or((false, true), |c| (c.nav.allow(), c.nav.at_root()))
    }
}

impl App {
    /// An inactive Gate can still own a visible Rounded title segment.
    fn render_gate_member(&mut self, member: u64) {
        let active = crate::tabs::collect(&self.layout.groups, member).map_or(member, |set| set.active);
        if let Some(i) = self.groups.iter().position(|g| g.id == active) { self.render_group(i); }
    }

    /// Read accessor for frame/tab drops, including inactive Gates.
    pub(super) fn portal_current_path(&self, id: u64) -> Option<&str> {
        self.portals.get(&id).map(|view| view.current.as_str()).filter(|path| !path.is_empty())
    }

    /// Create Gates for new portal groups and drop those of removed groups.
    pub(super) fn sync_portals(&mut self) {
        let wanted: Vec<(u64, String, Vec<String>, bool)> = self
            .layout
            .groups
            .iter()
            .filter_map(|g| match &g.content {
                GroupContent::Portal { path } => Some((g.id, path.clone(), g.nav_history.clone(), g.view_flags & model::VIEW_FLAG_NO_NAVIGATION == 0)),
                GroupContent::Items { .. } => None,
            })
            .collect();
        let removed: Vec<_> = self.portals.keys().copied().filter(|id| !wanted.iter().any(|(w, _, _, _)| w == id)).collect();
        for id in removed { self.portals.remove(&id); }
        for (id, root, history, allow) in wanted {
            let stale_root = self.portals.get(&id).is_some_and(|v| !portal::same_folder(&v.root, &root));
            if stale_root {
                self.portals.remove(&id);
            }
            if !self.portals.contains_key(&id) {
                let nav = portal::GateNav::from_stored(&root, &history, allow);
                let current = nav.current().to_string();
                self.portals.insert(id, PortalView {
                    root, current, core: Rc::new(RefCell::new(Core::new(nav))), step: None, probe_seq: 0, probe_pending: false, read_seq: 0, error: None, loaded: false, notify: 0,
                });
                // corral-views-spec 2.1: no Windows view; the Gate reads its folder and draws it as a Corral does.
                self.start_portal(id);
            }
        }
        let before = self.items.len();
        self.items.retain(|i| i.gate.is_none_or(|g| self.portals.contains_key(&g)));
        if self.items.len() != before { self.index_gate_items(); }
    }

    /// Re-read a Gate on a worker, keeping the previous listing and selection until it answers.
    pub(super) fn reload_gate(&mut self, id: u64) {
        let desktop = self.items.iter().filter(|i| i.gate.is_none())
            .map(|i| i.shell.parsing_name.to_lowercase()).collect();
        self.gate_items(id, desktop);
        self.render_gate_member(id);
    }

    /// Only owned item data crosses apartments; enumeration, sort facts and Details text stay on this worker.
    pub(super) fn gate_items(&mut self, id: u64, desktop: std::collections::HashSet<String>) {
        let sort = self.gate_sort_of(id);
        let Some(view) = self.portals.get_mut(&id) else { return };
        if view.probe_pending || view.error.is_some() { return; }
        let seq = portal::next_probe_seq();
        view.read_seq = seq;
        let path = view.current.clone();
        let window = self.message_window.0 as isize;
        std::thread::spawn(move || {
            use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
            let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            let result = if initialized.is_ok() {
                let result = shell::enumerate_folder(&path).map(|all| {
                    let all: Vec<_> = all.into_iter().filter(|s| !desktop.contains(&s.parsing_name.to_lowercase())).collect();
                    let (method, flags) = gate_sort(sort);
                    let keys: Vec<_> = all.iter().map(|s| group_menu::sort_key(s, method)).collect();
                    crate::sorting::order(&keys, method, flags, &crate::sorting::SystemCollator::user_default())
                        .into_iter().map(|k| (all[k].clone(), super::details::Text::load(&all[k]))).collect()
                }).map_err(|e| e.code().0 as u32);
                unsafe { CoUninitialize(); }
                result
            } else { Err(initialized.0 as u32) };
            let result = Box::into_raw(Box::new(GateItems { id, seq, path, result }));
            if unsafe { PostMessageW(HWND(window as *mut _), WM_GATE_ITEMS, WPARAM(0), LPARAM(result as isize)) }.is_err() {
                drop(unsafe { Box::from_raw(result) });
            }
        });
    }

    fn gate_items_done(&mut self, batch: GateItems) {
        let state = self.portals.get(&batch.id).map(|v| (v.read_seq, v.current.as_str()));
        if !gate_read_matches(state, batch.seq, &batch.path) { return; }
        let all = match batch.result {
            Ok(all) => all,
            Err(code) => {
                // Shell HRESULTs with FACILITY_WIN32 use the same error table as the probe.
                let code = if code & 0xffff_0000 == 0x8007_0000 { code & 0xffff } else { code };
                if let Some(view) = self.portals.get_mut(&batch.id) { view.probe_pending = true; }
                let seq = self.portals[&batch.id].probe_seq;
                self.portal_probe_done(batch.id, Some(portal::ProbeResult { path: batch.path, code, seq, replaced: false }));
                return;
            }
        };
        let selected: std::collections::HashSet<_> = self.items.iter().filter(|i| i.gate == Some(batch.id) && i.selected)
            .map(|i| i.shell.parsing_name.to_lowercase()).collect();
        self.items.retain(|i| i.gate != Some(batch.id));
        self.drop_item_hover(batch.id); // the listing is replaced
        // Desktop membership may have changed while this batch was being read.
        let desktop: std::collections::HashSet<_> = self.items.iter().filter(|i| i.gate.is_none())
            .map(|i| i.shell.parsing_name.to_lowercase()).collect();
        self.items.extend(all.into_iter().filter(|(s, _)| !desktop.contains(&s.parsing_name.to_lowercase())).map(|(shell, details)| {
            let selected = selected.contains(&shell.parsing_name.to_lowercase());
            super::Item { shell, details, icon: None, pos: (0, 0), placed: false, selected, gate: Some(batch.id) }
        }));
        if let Some(view) = self.portals.get_mut(&batch.id) { view.loaded = true; }
        self.index_gate_items();
        self.retain_group_icon_rasters();
        self.render_gate_member(batch.id);
    }

    /// Open `name` when it is a folder a Gate shows: inside the Gate when Allow navigation is on and it is
    /// inside the root, otherwise in Explorer (folder-gate-host-spec 4.3). False for anything else (the
    /// caller opens it as Windows does).
    pub(super) fn gate_open(&mut self, name: &str) -> bool {
        let Some(item) = self.items.iter().find(|i| i.gate.is_some() && i.shell.parsing_name.eq_ignore_ascii_case(name)) else { return false };
        if !item.shell.is_folder || item.shell.is_link { return false; }
        let Some(id) = item.gate else { return false };
        let Some(view) = self.portals.get(&id) else { return false };
        let decision = portal::decide_nav(&view.core.borrow().nav, Some(name), portal::NavOrigin::View, false);
        log(&format!("portal {id}: open folder {name:?}: {decision:?}"));
        match decision {
            portal::NavDecision::Allow => {
                let name = name.to_string();
                self.probe_target(id, &name);
            }
            portal::NavDecision::OpenExternally => { let _ = shell::open_item(name); }
            portal::NavDecision::Cancel => {}
        }
        true
    }

    /// Watch the folder Gate `id` shows; changes arrive with the desktop's (`WM_SHELL_CHANGE`).
    fn watch_gate(&mut self, id: u64) {
        let window = self.message_window;
        let Some(view) = self.portals.get_mut(&id) else { return };
        super::hosting::deregister_notifications(std::mem::take(&mut view.notify));
        view.notify = super::hosting::register_folder_notifications(window, &view.current);
    }

    /// Begin loading the stored folder: a worker climbs from it to the nearest existing folder
    /// inside the root (folder-gate-host-spec 4.4) within the probe limit.
    fn start_portal(&mut self, id: u64) {
        let window = self.message_window;
        let Some(view) = self.portals.get_mut(&id) else { return };
        view.probe_seq = portal::next_probe_seq();
        view.probe_pending = true;
        view.read_seq = 0;
        let nav = view.core.borrow().nav.clone();
        if let Ok(mut core) = view.core.try_borrow_mut() {
            core.begin_own_navigation(nav.current());
        }
        portal::probe_start(window, nav, view.probe_seq);
        unsafe { let _ = SetTimer(window, gate_timer(GATE_PROBE_TIMER, id), portal::PROBE_LIMIT.as_millis() as u32, None); }
    }

    /// Probe `target` before a Corral navigation (folder-gate-host-spec 3.3, 4.2).
    fn probe_target(&mut self, id: u64, target: &str) {
        let window = self.message_window;
        let Some(view) = self.portals.get_mut(&id) else { return };
        view.probe_seq = portal::next_probe_seq();
        view.probe_pending = true;
        view.read_seq = 0;
        super::hosting::deregister_notifications(std::mem::take(&mut view.notify));
        if let Ok(mut core) = view.core.try_borrow_mut() {
            core.begin_own_navigation(target);
        }
        portal::probe_folder(window, target, view.probe_seq);
        unsafe { let _ = SetTimer(window, gate_timer(GATE_PROBE_TIMER, id), portal::PROBE_LIMIT.as_millis() as u32, None); }
    }

    /// Messages of drawn Gates on the message window: probe answers and the probe / retry timers.
    pub(super) fn gate_window_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_GATE_ITEMS => {
                if lparam.0 != 0 { self.gate_items_done(*unsafe { Box::from_raw(lparam.0 as *mut GateItems) }); }
                Some(LRESULT(0))
            }
            portal::WM_PORTAL_PROBE => {
                let result = portal::take_probe(lparam)?;
                // The newest probe of a Gate, or a late answer for a Gate stuck in an error on that folder.
                let id = self.portals.iter().find(|(_, v)| v.probe_seq == result.seq).map(|(id, _)| *id)
                    .or_else(|| self.portals.iter().find(|(_, v)| v.error.as_ref().is_some_and(|(_, p)| portal::same_folder(p, &result.path))).map(|(id, _)| *id));
                if let Some(id) = id { self.portal_probe_done(id, Some(result)); }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == GATE_DEFER_TIMER => {
                unsafe { let _ = KillTimer(self.message_window, GATE_DEFER_TIMER); }
                for (msg, lparam) in DEFERRED_GATE.with(|queue| std::mem::take(&mut *queue.borrow_mut())) {
                    self.gate_window_message(msg, WPARAM(0), LPARAM(lparam));
                }
                Some(LRESULT(0))
            }
            WM_TIMER => {
                let ids: Vec<u64> = self.portals.keys().copied().collect();
                if let Some(id) = ids.iter().copied().find(|id| wparam.0 == gate_timer(GATE_PROBE_TIMER, *id)) {
                    unsafe { let _ = KillTimer(self.message_window, wparam.0); }
                    self.portal_probe_done(id, None);
                    return Some(LRESULT(0));
                }
                let Some(id) = ids.into_iter().find(|id| wparam.0 == gate_timer(GATE_RETRY_TIMER, *id)) else {
                    // A removed Gate's timer: stop it.
                    if (GATE_PROBE_TIMER..GATE_RETRY_TIMER + 0x10_0000).contains(&wparam.0) {
                        unsafe { let _ = KillTimer(self.message_window, wparam.0); }
                        return Some(LRESULT(0));
                    }
                    return None;
                };
                // Offline Gates retry while visible; a hidden Gate waits until it shows again.
                let visible = self.layout.groups.iter().find(|g| g.id == id).is_some_and(|g| crate::tabs::body_visible(&self.layout.groups, g) && self.page_layer_visible(g));
                if visible { self.portal_retry(id); }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    /// The worker answered, or the 3000 ms limit passed (`result` is `None`).
    fn portal_probe_done(&mut self, id: u64, result: Option<portal::ProbeResult>) {
        let (kind, path, replaced) = {
            let Some(view) = self.portals.get(&id) else { return };
            match result {
                None => {
                    if !view.probe_pending { return; }
                    let path = view.core.borrow().target().map_or_else(|| view.current.clone(), str::to_string);
                    (portal::classify_probe(None), path, false)
                }
                Some(r) => {
                    // A superseded probe is ignored, except an Ok for the folder the Gate is stuck on
                    // after a timeout: that late answer recovers the Gate by itself.
                    let stale = r.seq != view.probe_seq;
                    let in_error = view.error.is_some();
                    // Nobody waits for this answer any more (a timeout with a working view dropped the target).
                    if !view.probe_pending && !in_error { return; }
                    let same = view.core.borrow().target().is_some_and(|t| portal::same_folder(t, &r.path));
                    if stale && !(r.code == 0 && in_error && same) { return; }
                    (portal::classify_probe(Some(r.code)), r.path, r.replaced)
                }
            }
        };
        if let Some(view) = self.portals.get_mut(&id) {
            view.probe_pending = false;
        }
        let window = self.message_window;
        unsafe { let _ = KillTimer(window, gate_timer(GATE_PROBE_TIMER, id)); }
        if kind == Probe::Ok {
            // corral-views-spec 2.2: the folder is there; record the visit, read it and draw it.
            let _ = replaced; // the start-up climb's ancestor is visited like any other folder
            // Another folder starts at its top: a fresh bar has no offset.
            if self.portals.get(&id).is_some_and(|v| !portal::same_folder(&v.current, &path)) {
                self.scrollbars.remove(&id);
                // a new bar is seeded from the stored offset (views): the old folder's must not carry over
                if let Some(g) = self.layout.group_mut(id) { g.scroll = 0; }
                self.drop_item_hover(id);
            }
            if let Some(view) = self.portals.get_mut(&id) {
                if let Ok(mut core) = view.core.try_borrow_mut() {
                    core.nav.visit(&path); // Back/Forward already moved the cursor there: nothing to add
                    core.cancel_own_navigation();
                }
                view.step = None;
                view.error = None;
                if !portal::same_folder(&view.current, &path) {
                    view.loaded = false;
                    self.items.retain(|i| i.gate != Some(id));
                }
            }
            self.index_gate_items();
            unsafe { let _ = KillTimer(window, gate_timer(GATE_RETRY_TIMER, id)); }
            self.sync_portal_history(id);
            self.watch_gate(id);
            self.reload_gate(id);
            return;
        }
        // Missing, offline, denied or another failure.
        let Some(view) = self.portals.get_mut(&id) else { return };
        // A Back or Forward step that cannot be browsed leaves the trail (no dead ends).
        if let Some(step) = view.step.take() {
            if let Ok(mut core) = view.core.try_borrow_mut() {
                core.nav.step_failed(&step);
            }
            self.sync_portal_history(id);
        }
        let Some(view) = self.portals.get_mut(&id) else { return };
        // A failed step away from a folder that still works keeps that folder shown (folder-gate-host-spec 4.6).
        if view.loaded && view.error.is_none() && !portal::same_folder(&view.current, &path) {
            log(&format!("portal {id}: {path} is unavailable ({kind:?}); staying on {}", view.current));
            if let Ok(mut core) = view.core.try_borrow_mut() { core.cancel_own_navigation(); }
            self.watch_gate(id);
            self.reload_gate(id);
            return;
        }
        view.error = Some((kind, path.clone()));
        view.loaded = false;
        view.read_seq = 0;
        self.items.retain(|i| i.gate != Some(id));
        self.index_gate_items();
        unsafe {
            let _ = KillTimer(window, gate_timer(GATE_RETRY_TIMER, id));
            if let Some(wait) = portal::auto_retry(kind) {
                let _ = SetTimer(window, gate_timer(GATE_RETRY_TIMER, id), wait.as_millis() as u32, None);
            }
        }
        log(&format!("portal {id}: {path} is unavailable ({kind:?})"));
        self.render_gate_member(id);
    }

    /// Try again: probe the folder the Gate is stuck on (the Try again button, Refresh in an error
    /// state, a device arrival, a resume from sleep, the 60 s timer of an offline Gate).
    pub(super) fn portal_retry(&mut self, id: u64) {
        let Some(view) = self.portals.get(&id) else { return };
        // Try again goes to the folder the Gate is on, not to a dead target.
        let path = view.core.borrow().nav.current().to_string();
        if view.error.is_none() { return; }
        self.probe_target(id, &path);
    }

    /// Every Gate showing an error tries again (`DBT_DEVICEARRIVAL`, `PBT_APMRESUMEAUTOMATIC`).
    pub(super) fn retry_portal_errors(&mut self) {
        let ids: Vec<u64> = self.portals.iter().filter(|(_, v)| v.error.is_some()).map(|(id, _)| *id).collect();
        for id in ids { self.portal_retry(id); }
    }

    /// folder-gate-views-spec 3, 7: inheritance is the shared setting, not unknown per-Gate bits.
    fn portal_inherits(&self, _id: u64) -> bool {
        self.settings.folder_portals_inherit_view
    }

    // folder-gate-views-spec 3: every group resolves independently; no creation-time seed.

    /// Whether Windows' own view of the Gate shows the details header hidden etc.: the menu's inputs.
    fn gate_menu_state(&self, id: u64) -> Option<GateMenuState> {
        let view = self.portals.get(&id)?;
        let group = self.layout.groups.iter().find(|g| g.id == id)?;
        let (allow, at_root) = view.core_state();
        Some(GateMenuState {
            allow,
            at_root,
            mode: self.group_view(id).mode(),
            icon_px: 0,
            header_hidden: false,
            hide_arrows: group.hide_link_overlays,
            sort: self.gate_sort_of(id),
        })
    }

    /// The signed sort column of the folder Gate `id` shows (folder-gate-views-spec 2; default Name ascending).
    pub(super) fn gate_sort_of(&self, id: u64) -> i32 {
        let Some(view) = self.portals.get(&id) else { return 1 };
        let ctx = view_state::Context { desktop_icon_size: 32 };
        match view_state::resolve_view(&view.current, &self.layout.view_states, self.portal_inherits(id), ctx).sort {
            0 => 1,
            sort => sort,
        }
    }

    /// Run a Gate-family command (folder-gate-host-spec 5).
    fn run_gate_command(&mut self, id: u64, command: u32) {
        use portal_menu::id as ids;
        if self.column_command(id, command) { return; }
        match command {
            ids::CLASSIC => self.set_group_view(id, view_state::View::Icons),
            ids::LIST => self.set_group_view(id, view_state::View::List),
            ids::DETAILS => self.set_group_view(id, view_state::View::Details),
            ids::OPEN_FOLDER => self.portal_open_folder(id),
            ids::CHOOSE_FOLDER => self.portal_choose_folder(id),
            ids::CONFIGURE_FOLDER => self.portal_configure_folder(id),
            ids::REFRESH => self.portal_refresh(id),
            ids::NAVIGATE_UP => self.portal_navigate(id, PortalStep::Up),
            ids::NAVIGATE_ROOT => self.portal_navigate(id, PortalStep::Root),
            ids::ALLOW_NAVIGATION => self.portal_toggle_navigation(id),
            ids::HIDE_ARROWS => self.portal_toggle_arrows(id),
            ids::REVERSE => {
                let sort = self.gate_sort_of(id);
                self.portal_sort(id, -sort);
            }
            other => {
                if let Some(column) = portal_menu::sort_column(other) {
                    // Picking the shown column keeps its direction; another column starts ascending.
                    let sort = self.gate_sort_of(id);
                    self.portal_sort(id, if sort.abs() == column { sort } else { column });
                }
            }
        }
    }

    pub(super) fn portal_open_folder(&mut self, id: u64) {
        let Some(current) = self.portals.get(&id).map(|v| v.current.clone()) else { return };
        if let Err(e) = shell::open_item(&current) {
            log(&format!("portal {id}: open folder failed: {e}"));
        }
    }

    /// "Configure folder": Windows' Properties of the folder shown (folder-gate-host-spec question 4).
    fn portal_configure_folder(&mut self, id: u64) {
        let Some(current) = self.portals.get(&id).map(|v| v.current.clone()) else { return };
        let owner = self.groups.iter().find(|g| g.id == id).map_or(self.surface_hwnd, |g| g.hwnd);
        let path = HSTRING::from(current.as_str());
        const SHOP_FILEPATH: i32 = 2;
        let shown = unsafe { windows::Win32::UI::Shell::SHObjectProperties(owner, windows::Win32::UI::Shell::SHOP_TYPE(SHOP_FILEPATH), &path, PCWSTR::null()) };
        if !shown.as_bool() {
            log(&format!("portal {id}: folder properties did not open for {current}"));
        }
    }

    /// Refresh: probe the current folder off-thread, then browse it again (folder-gate-host-spec 3.3). The view's
    /// own Refresh on a folder that has gone shows Windows' modal "Location is not available" box (live 2026-10-28).
    fn portal_refresh(&mut self, id: u64) {
        let Some(path) = self.portals.get(&id).map(|v| v.core.borrow().nav.current().to_string()) else { return };
        self.probe_target(id, &path);
    }

    /// "Hide shortcut arrows": bound to the Gate's shortcut-arrow flag.
    fn portal_toggle_arrows(&mut self, id: u64) {
        let Some(group) = self.layout.group_mut(id) else { return };
        if !matches!(group.content, GroupContent::Portal { .. }) { return; }
        group.hide_link_overlays = !group.hide_link_overlays;
        let hide = group.hide_link_overlays;
        self.save();
        self.retain_group_icon_rasters();
        if let Some(i) = self.groups.iter().position(|g| g.id == id) { self.render_group(i); }
        log(&format!("portal {id}: hide shortcut arrows {hide}"));
    }

    /// `route_key` for Gate `id` with its Allow navigation and rename state.
    pub(super) fn route_gate_input(&self, id: u64, input: crate::gate_host::Input) -> Option<PortalStep> {
        let view = self.portals.get(&id)?;
        let allow = view.core_state().0;
        let renaming = self.renaming.is_some();
        gate_host::route_key(input, allow, renaming).map(|claim| match claim {
            gate_host::Claim::Back => PortalStep::Back,
            gate_host::Claim::Forward => PortalStep::Forward,
            gate_host::Claim::Up => PortalStep::Up,
        })
    }

    /// Keys on a Gate's window: Alt+Left / Alt+Right / Alt+Up and Backspace navigate (folder-gate-host-spec
    /// 2.5); everything else is the Corral's own key handling.
    pub(super) fn portal_key(&mut self, msg: &MSG) -> bool {
        let Some(id) = self.groups.iter().find(|g| g.hwnd == msg.hwnd).map(|g| g.id).filter(|id| self.portals.contains_key(id)) else {
            return false;
        };
        let input = if msg.message == WM_APPCOMMAND {
            match ((msg.lParam.0 >> 16) & 0x7ff) as u32 {
                1 => Some(gate_host::Input::AppBack),
                2 => Some(gate_host::Input::AppForward),
                _ => None,
            }
        } else { None };
        if let Some(step) = input.and_then(|input| self.route_gate_input(id, input)) {
            self.portal_navigate(id, step);
            return true;
        }
        if matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN) {
            use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_MENU, VK_SHIFT};
            let (shift, ctrl, alt) = unsafe {
                (GetKeyState(VK_SHIFT.0 as i32) < 0, GetKeyState(VK_CONTROL.0 as i32) < 0, GetKeyState(VK_MENU.0 as i32) < 0 || msg.message == WM_SYSKEYDOWN)
            };
            let input = crate::gate_host::Input::Key { vk: msg.wParam.0 as u16, alt, ctrl, shift };
            if let Some(step) = self.route_gate_input(id, input) {
                self.portal_navigate(id, step);
                return true;
            }
        }
        false
    }

    /// Mirror the core into the Gate's App-side fields and the layout: the folder shown and the
    /// stored history (folder-gate-host-spec 4.4: written on each visit, never on a failure).
    fn sync_portal_history(&mut self, id: u64) {
        let Some(view) = self.portals.get_mut(&id) else { return };
        let (current, stored) = {
            let core = view.core.borrow();
            (core.nav.current().to_string(), core.nav.to_stored())
        };
        view.current = current;
        let changed = self.layout.group_mut(id).is_some_and(|g| {
            let changed = g.nav_history != stored;
            if changed { g.nav_history = stored; }
            changed
        });
        if changed { self.save(); }
    }

    /// Apply a navigation step of a Gate command: take the target from the model, probe it on the
    /// worker, and read its items once it answers (folder-gate-host-spec 4.2).
    pub(super) fn portal_navigate(&mut self, id: u64, step: PortalStep) {
        let Some(view) = self.portals.get_mut(&id) else { return };
        let (target, token) = {
            let Ok(mut core) = view.core.try_borrow_mut() else { return };
            match step {
                PortalStep::Up => (core.nav.up_target(), None),
                PortalStep::Root => (core.nav.root_target(), None),
                PortalStep::Back => match core.nav.back() { Some(s) => (Some(s.target.clone()), Some(s)), None => (None, None) },
                PortalStep::Forward => match core.nav.forward() { Some(s) => (Some(s.target.clone()), Some(s)), None => (None, None) },
            }
        };
        let Some(target) = target else { return };
        if portal::decide_nav(&view.core.borrow().nav, Some(&target), portal::NavOrigin::Corral, false) != portal::NavDecision::Allow { return; }
        view.step = token;
        self.probe_target(id, &target);
    }

    /// Left click on a Gate's folder glyph (button-up): the Gate menu below the glyph. Going up is
    /// the Up button's job (`portal_up_click`).
    pub(super) fn portal_glyph_click(&mut self, id: u64, glyph: Rect) {
        self.show_portal_glyph_menu(id, glyph);
    }

    /// Left click on a Gate's Up button (button-up): Shift = navigate to root, a plain click = Up.
    /// The button is disabled (and not hit-tested) at the root.
    pub(super) fn portal_up_click(&mut self, id: u64) {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_SHIFT};
        let shift = unsafe { GetKeyState(VK_SHIFT.0 as i32) } < 0;
        self.portal_navigate(id, if shift { PortalStep::Root } else { PortalStep::Up });
    }

    pub(super) fn show_portal_glyph_menu(&mut self, id: u64, glyph: Rect) {
        let anchor = portal::glyph_menu_anchor(glyph);
        self.show_portal_title_menu(id, anchor);
    }

    /// The Gate menu (folder-gate-host-spec 5): opened by the strip's menu button and by a right
    /// click on the strip. The Gate section comes from `portal_menu::gate_menu`, then the Tabs entry
    /// and the shared Corral section.
    pub(super) fn show_portal_title_menu(&mut self, id: u64, screen: (i32, i32)) {
        let owner = self.tab_menu_owner(id);
        let (choice, tab_members) = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let Some(tab_members) = self.append_portal_title_menu(menu, id) else { let _ = DestroyMenu(menu); return };
            let _ = SetForegroundWindow(owner);
            let choice = match qa_portal_menu_choice() {
                Some(answer) => answer as usize,
                None => TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, screen.0, screen.1, 0, owner, None).0 as usize,
            };
            let _ = DestroyMenu(menu);
            (choice, tab_members)
        };
        self.portal_menu_command(id, choice, &tab_members);
    }

    /// Shared prefix for the title and empty-body menus; the returned tab order belongs to this popup.
    pub(super) fn append_portal_title_menu(&self, menu: HMENU, id: u64) -> Option<Vec<u64>> {
        let group = self.layout.groups.iter().find(|g| g.id == id)?;
        let state = self.gate_menu_state(id)?;
        let shared = self.view_menu_state(id, group);
        unsafe {
            append_gate_items(menu, &portal_menu::gate_menu_columns(&state, &self.group_columns(id)));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let tab_members = self.append_tabs_menu(menu, None, id);
            group_menu::append_entries(menu, &group_menu::view_menu_entries(&shared));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, group_menu::MENU_BASE + group_menu::corral_cmd::RENAME as usize, w!("Rename Corral"));
            let _ = AppendMenuW(menu, MF_STRING, group_menu::MENU_BASE + group_menu::corral_cmd::CONFIGURE as usize, w!("Configure Corral..."));
            Some(tab_members)
        }
    }

    pub(super) fn portal_menu_command(&mut self, id: u64, choice: usize, tab_members: &[u64]) {
        log(&format!("portal {id}: menu choice {choice:#x}"));
        if choice == 0 { return; }
        if self.tab_menu_command(id, choice, tab_members) { return; }
        if portal_menu::id::is_gate(choice as u32) {
            self.run_gate_command(id, choice as u32);
        } else if choice == group_menu::MENU_BASE + group_menu::corral_cmd::RENAME as usize {
            self.portal_rename(id);
        } else if choice == group_menu::MENU_BASE + group_menu::corral_cmd::CONFIGURE as usize {
            self.open_settings_window();
        } else if choice > group_menu::MENU_BASE {
            self.view_command(id, (choice - group_menu::MENU_BASE) as u32);
        }
    }

    /// "Choose a different folder...": the folder picker titled "Choose folder" starting at the
    /// root; the result becomes the new root with a fresh trail (`GateNav::choose_root`). The title
    /// is not changed.
    pub(super) fn portal_choose_folder(&mut self, id: u64) {
        let Some(root) = self.portals.get(&id).map(|v| v.root.clone()) else { return };
        let owner = self.groups.iter().find(|g| g.id == id).map_or(self.surface_hwnd, |g| g.hwnd);
        let Some(path) = pick_folder_with(owner, Some("Choose folder"), Some(&root)) else { return };
        if portal::same_folder(&path, &root) {
            return;
        }
        if let Some(group) = self.layout.group_mut(id) {
            group.content = GroupContent::Portal { path: path.clone() };
            group.nav_history.clear();
            group.scroll = 0;
        }
        if let Some(view) = self.portals.get_mut(&id) {
            if let Ok(mut core) = view.core.try_borrow_mut() {
                core.nav.choose_root(&path);
            }
            view.root = path.clone();
            view.current = path.clone();
            view.step = None;
            view.loaded = false;
            view.error = None;
        }
        self.items.retain(|i| i.gate != Some(id));
        self.index_gate_items();
        self.scrollbars.remove(&id);
        self.drop_item_hover(id);
        self.save();
        self.probe_target(id, &path);
        self.render_gate_member(id);
    }

    /// "Allow navigation" (Gate flag bit 0x1): turning it off sends the Gate to its root now.
    fn portal_toggle_navigation(&mut self, id: u64) {
        let Some(group) = self.layout.group_mut(id) else { return };
        group.view_flags ^= model::VIEW_FLAG_NO_NAVIGATION;
        let allow = group.view_flags & model::VIEW_FLAG_NO_NAVIGATION == 0;
        self.save();
        let Some(view) = self.portals.get_mut(&id) else { return };
        let to_root = view.core.try_borrow_mut().ok().and_then(|mut core| core.nav.set_allow(allow));
        view.step = None;
        self.sync_portal_history(id);
        if let Some(root) = to_root {
            self.probe_target(id, &root);
        }
        self.render_gate_member(id);
    }

    /// Rename: refused with an explanation while navigated below the root and the title follows
    /// the folder; otherwise the in-place title editor.
    fn portal_rename(&mut self, id: u64) {
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id).cloned() else { return };
        if self.portal_title(&group) != group.title {
            let owner = self.groups.iter().find(|g| g.id == id).map_or(self.surface_hwnd, |g| g.hwnd);
            crate::dialog::notice(owner, crate::dialog::system_light(), "Return to the Gate root to rename it",
                "A Gate shows the current subfolder name. Return to its root folder before setting a custom title; that title is saved for the root.");
            return;
        }
        self.begin_naming(id);
    }

    /// Sort by / Reverse order: store the folder's sort (a user change, folder-gate-views-spec 4) and re-read
    /// the Gate in that order.
    pub(super) fn portal_sort(&mut self, id: u64, sort: i32) {
        if !(1..=4).contains(&sort.abs()) {
            return;
        }
        let Some(current) = self.portals.get(&id).map(|v| v.current.clone()) else { return };
        let ctx = view_state::Context { desktop_icon_size: 32 };
        for (path, raw) in view_state::unparsed_for_folder(&self.layout.view_states, &current) {
            self.layout.view_unparsed.entry(path).or_insert(raw);
        }
        view_state::apply_patch(&mut self.layout.view_states, &current, view_state::Patch { style: None, sort: Some(sort), columns: None }, ctx);
        log(&format!("portal {id}: sort {sort}"));
        self.save();
        self.reload_gate(id);
    }

    /// Strip title of a portal: the shown folder's name while navigated, unless the user
    /// gave the portal a custom title.
    pub(super) fn portal_title(&self, group: &model::Group) -> String {
        let Some(view) = self.portals.get(&group.id) else { return group.title.clone() };
        if portal::same_folder(&view.current, &view.root) {
            return group.title.clone();
        }
        let root_name = shell::display_name_of(&view.root).unwrap_or_else(|| portal::last_component(&view.root).to_string());
        let target_name = shell::display_name_of(&view.current).unwrap_or_else(|| portal::last_component(&view.current).to_string());
        portal::navigated_title(&group.title, &root_name, &target_name).unwrap_or_else(|| group.title.clone())
    }
}

/// Turn the Gate menu model into `HMENU` items: hidden items are left out, a disabled item is grayed,
/// a checked item carries the check mark or the radio bullet.
pub(super) unsafe fn append_gate_items(menu: HMENU, items: &[portal_menu::Item]) {
    for item in items.iter().filter(|i| !i.hidden) {
        unsafe {
            match &item.kind {
                Kind::Separator => { let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null()); }
                Kind::Submenu(children) => {
                    if let Ok(sub) = CreatePopupMenu() {
                        append_gate_items(sub, children);
                        let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, &HSTRING::from(item.label));
                    }
                }
                kind => {
                    let mut flags = MF_STRING;
                    if !item.enabled { flags |= MF_GRAYED; }
                    if item.checked && *kind != Kind::Radio { flags |= MF_CHECKED; }
                    let _ = AppendMenuW(menu, flags, item.id as usize, &HSTRING::from(item.label));
                    if item.checked && *kind == Kind::Radio {
                        let _ = CheckMenuRadioItem(menu, item.id, item.id, item.id, MF_BYCOMMAND.0);
                    }
                }
            }
        }
    }
}

/// Folder picker with an optional dialog title and starting folder.
fn pick_folder_with(owner: HWND, title: Option<&str>, initial: Option<&str>) -> Option<String> {
    // QA hook: CORRAL_QA_PICK_FOLDER=<path> answers the folder picker without UI (automated trials only).
    if let Ok(path) = std::env::var("CORRAL_QA_PICK_FOLDER") {
        return Some(path).filter(|p| !p.is_empty());
    }
    unsafe {
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let options = dialog.GetOptions().ok()?;
        dialog.SetOptions(options | FOS_PICKFOLDERS).ok()?;
        if let Some(title) = title {
            let _ = dialog.SetTitle(&HSTRING::from(title));
        }
        if let Some(folder) = initial.and_then(|p| windows::Win32::UI::Shell::SHCreateItemFromParsingName::<_, _, windows::Win32::UI::Shell::IShellItem>(&HSTRING::from(p), None).ok()) {
            let _ = dialog.SetFolder(&folder);
        }
        dialog.Show(owner).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        CoTaskMemFree(Some(name.0 as *const _));
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stored_gate_sort_maps_to_the_corral_sort() {
        use crate::sorting::*;
        assert_eq!(gate_sort(0), (METHOD_NAME, 0), "no preference = Name ascending");
        assert_eq!(gate_sort(1), (METHOD_NAME, 0));
        assert_eq!(gate_sort(-1), (METHOD_NAME, FLAG_REVERSE));
        assert_eq!(gate_sort(2), (METHOD_SIZE, 0));
        assert_eq!(gate_sort(-3), (METHOD_TYPE, FLAG_REVERSE));
        assert_eq!(gate_sort(4), (METHOD_MODIFIED, 0));
        assert_eq!(gate_sort(-5), (METHOD_CREATED, FLAG_REVERSE));
        assert_eq!(gate_sort(99), (METHOD_NAME, 0));
        assert_ne!(gate_timer(GATE_PROBE_TIMER, 7), gate_timer(GATE_RETRY_TIMER, 7));
        assert_ne!(gate_timer(GATE_PROBE_TIMER, 7), gate_timer(GATE_PROBE_TIMER, 8));
    }

    #[test]
    fn only_the_latest_read_of_the_current_gate_is_accepted() {
        assert!(gate_read_matches(Some((8, r"C:\Gate")), 8, r"c:\gate"));
        assert!(!gate_read_matches(Some((9, r"C:\Gate")), 8, r"C:\Gate"), "superseded read or recreated Gate");
        assert!(!gate_read_matches(Some((0, r"C:\Gate")), 8, r"C:\Gate"), "navigation started");
        assert!(!gate_read_matches(Some((8, r"C:\Other")), 8, r"C:\Gate"), "different folder");
        assert!(!gate_read_matches(None, 8, r"C:\Gate"), "removed Gate");
    }

    #[test]
    fn gate_messages_do_not_alias() {
        let messages = [portal::WM_PORTAL_PROBE, WM_GATE_ITEMS];
        let mut sorted = messages.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), messages.len());
    }

    #[test]
    fn gate_menu_ids_stay_clear_of_the_shared_corral_numbers() {
        // The shared section is `MENU_BASE + n` (n below 0x200) and the Tabs entries sit in the same band.
        assert!(portal_menu::id::FIRST as usize > group_menu::MENU_BASE + 0x200);
        // The Gate body menu's shell ids sit between the Gate ids and shell_items' own 0x9001.. entries.
        assert!(crate::shell_menu::GATE_BACKGROUND_FIRST_ID > portal_menu::id::LAST);
        assert!(crate::shell_menu::GATE_BACKGROUND_LAST_ID < 0x9001);
    }
}
