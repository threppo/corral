//! Item drags: internal drag with ghost image, hand-over to an OLE drag, drops on Corral's windows,
//! external drops from Explorer.

#[path = "drop_grid.rs"]
mod drop_grid;

use crate::geometry::{Rect, to_dip, to_px};
use crate::model::{self, GroupContent};
use crate::render::{Rgba, Surface, present_layered};
use crate::dragdrop::{self, Destination, DropCallbacks, DropHint, DropTarget, Dropped};
use std::time::Instant;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use windows::Win32::Foundation::{HWND, POINT, WPARAM};
use windows::Win32::System::Ole::DROPEFFECT_MOVE;
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};
use super::selection::{self, Home};
use super::{APP, App, Drag, DropSite, OLE_DRAG_RUNNING, PENDING_OLE_DRAG, PendingPlacement, log};

#[derive(Debug, PartialEq, Eq)]
enum DeferredDrop {
    CreatePortal { paths: Vec<String>, screen: (i32, i32) },
    SelectTab(u64),
}

thread_local! {
    static DROP_ITEM: RefCell<Option<String>> = const { RefCell::new(None) };
    static DEFERRED_DROPS: RefCell<Vec<DeferredDrop>> = const { RefCell::new(Vec::new()) };
    /// Portal members that received a label drop: their view is re-read with the item reload
    /// that follows the drop. The tab is selected at once, before the Shell has finished the
    /// transfer, and the view did not show the new item until a manual refresh (user, 2026-10-14).
    static PORTAL_REFRESH: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    /// Spring-loaded tabs for drags that Windows delivers to the drop target (tabs-spec 4).
    static SPRING: Cell<crate::tabs::HoverState> = const { Cell::new(crate::tabs::HoverState { tab: None, since_ms: 0, spent: false }) };
    static SPRING_SEEN: Cell<bool> = const { Cell::new(false) };
}

/// One DragOver of a file drag: `under` is the inactive tab label the pointer is on. After 600 ms on
/// the same tab it is selected, even with hover switching off, so an item can be dropped into it.
fn spring_ole(under: Option<u64>, message_window: HWND) {
    SPRING_SEEN.with(|seen| seen.set(true));
    let input = crate::tabs::HoverInput { tab: under, now_ms: super::tick_ms(), wait_ms: Some(crate::tabs::SPRING_MS), cancel: false };
    let (next, out) = crate::tabs::hover_switch_step(SPRING.with(|s| s.get()), input);
    SPRING.with(|s| s.set(next));
    if let crate::tabs::HoverOut::Select(member) = out { enqueue_drop(DeferredDrop::SelectTab(member), message_window); }
}

fn spring_reset() {
    SPRING.with(|s| s.set(crate::tabs::HoverState::default()));
}

pub(super) fn item_drop_highlight(name: &str) -> bool {
    DROP_ITEM.with(|item| item.borrow().as_ref().is_some_and(|n| n.eq_ignore_ascii_case(name)))
}

fn update_drop_highlight(name: Option<&str>) {
    let wanted = name.map(str::to_owned);
    APP.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some(app) = guard.as_mut() {
                let changed = DROP_ITEM.with(|item| {
                    if *item.borrow() == wanted { false }
                    else { *item.borrow_mut() = wanted; true }
                });
                if changed {
                    // Feedback paints only: no sorting, saving or window/portal reconstruction
                    // while a Shell target owns a modal drag (Esc must not alter the layout).
                    app.render_surface();
                    for index in 0..app.groups.len() { app.render_group(index); }
                }
            }
        }
    });
}

/// Item hand-overs only reload, including cancelled/failed Shell operations. They never
/// feed the internal reference-placement path or the external pending-placement map.
fn positional_drop(destination: Option<&Destination>) -> bool {
    !matches!(destination, Some(Destination::Item(_)))
}

fn drop_route(internal: bool, item: Option<String>) -> DropHint {
    match item {
        Some(name) => DropHint::ShellTo(Destination::Item(name)),
        None if internal => DropHint::Internal(DROPEFFECT_MOVE),
        None => DropHint::Shell,
    }
}

fn take_deferred(queue: &mut Vec<DeferredDrop>) -> Vec<DeferredDrop> { std::mem::take(queue) }

fn enqueue_drop(action: DeferredDrop, message_window: HWND) {
    DEFERRED_DROPS.with(|queue| queue.borrow_mut().push(action));
    // Runs outside this callback on its own timer: the item reload that follows a drop waits
    // 1 s for the Shell, and a tab switch must not wait for it (user, 2026-10-14: lag).
    unsafe { SetTimer(message_window, super::TIMER_DEFERRED_DROPS, 1, None); }
}

struct FolderMember<'a> {
    id: u64,
    root: Option<&'a str>,
    current: Option<&'a str>,
}

/// Pure frame/member resolver; selection is independent from destination identity.
fn resolve_folder<'a>(frame: Option<u64>, tab: Option<usize>, members: &[FolderMember<'a>]) -> Option<(u64, &'a str, bool)> {
    let frame = frame?;
    if !members.iter().any(|member| member.id == frame) { return None }
    let member = match tab { Some(index) => members.get(index)?, None => members.iter().find(|m| m.id == frame)? };
    let root = member.root.filter(|path| !path.is_empty())?;
    let current = member.current.filter(|path| !path.is_empty());
    Some((member.id, current.unwrap_or(root), current.is_none()))
}

/// How a frame drop is routed once the hit tab (if any) is known.
#[derive(Debug, PartialEq, Eq)]
enum FrameRoute<'a> {
    /// The hit portal member's folder (current, or root with `fallback`).
    Folder { member: u64, path: &'a str, fallback: bool },
    /// The ordinary desktop route: a label of an ordinary Corral member takes it even when the
    /// visible member is a portal (the title branch needs a portal hit member).
    Own,
    /// No tab hit and no portal folder: decide by the frame's own content.
    Content,
}

fn frame_route<'a>(frame: u64, tab: Option<usize>, members: &[FolderMember<'a>]) -> FrameRoute<'a> {
    match resolve_folder(Some(frame), tab, members) {
        Some((member, path, fallback)) => FrameRoute::Folder { member, path, fallback },
        None if tab.is_some() => FrameRoute::Own,
        None => FrameRoute::Content,
    }
}

fn select_drop_tab(hit: u64, active: u64, keys: u32, buttons_down: bool, pending: Option<u64>) -> bool {
    hit != active && keys & 3 == 0 && !buttons_down && pending.is_none_or(|member| member == hit)
}

/// QA destinations must stay below this worktree's scratch root, including navigated portals.
fn safe_qa_folder(path: &str) -> bool {
    std::env::current_dir().ok().and_then(|cwd| cwd.join("analysis").join("qa").canonicalize().ok())
        .zip(std::path::Path::new(path).canonicalize().ok())
        .is_some_and(|(root, path)| path.is_dir() && path.starts_with(root))
}

fn default_drop_destination(fixture_requested: bool, fixture: Option<String>) -> Destination {
    match fixture {
        Some(path) => Destination::Folder(path),
        // The callback rejects too; retain no real-Desktop fallback even if it cannot run.
        None if fixture_requested => Destination::Folder(String::new()),
        None => Destination::Desktop,
    }
}

/// An icon drag that left Corral's own windows (or entered a folder-portal popup) and must continue
/// as a real shell (OLE) drag. `App::hand_item_drag_to_ole` records it while `APP` is borrowed;
/// `window_proc` runs it once that borrow is released, because `DoDragDrop` is modal and the drop
/// target callbacks and the nested window messages need `APP`.
pub(super) struct OleDragRequest {
    pub(super) owner: HWND,
    pub(super) names: Vec<String>,
    pub(super) image: Option<dragdrop::DragImage>,
    /// Corral references (internal) or files a Gate shows (a Shell file operation wherever they land).
    pub(super) internal: bool,
}

/// State of the item drag while OLE owns it (`App::ole_drag`): what `drop_item` needs to turn a drop
/// on one of Corral's own windows into the same reference change as the internal path.
pub(super) struct OleDrag {
    /// The pressed item and the items the data object carries (the pressed item's selection that
    /// shares its parent folder, display order).
    pub(super) name: String,
    pub(super) names: Vec<String>,
    /// Cursor offset inside the dragged cell and the press point (screen px), as in `Drag::Item`.
    pub(super) grab: (i32, i32),
    pub(super) start: (i32, i32),
    /// Group the icon was dragged out of (`sorted_move`); by id, windows may be rebuilt meanwhile.
    pub(super) source: Option<u64>,
    /// Screen point of an internal drop reported by the drop target (`Dropped::internal`).
    pub(super) dropped_at: Option<(i32, i32)>,
}

/// What lies under the cursor during an internal icon drag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragHit {
    /// Corral's group/surface/preview windows (or no window): the internal drag continues.
    Own,
    /// A Gate tab label whose folder supplies a Shell drop target.
    Portal,
    /// A Corral-drawn icon whose shell object supplies an eligible drop target.
    Item,
    /// Any window of another process (Explorer, other apps).
    Foreign,
}

/// Classifies the window under the cursor: `hit_pid` is its process (`None` = no window), `ghost`
/// the drag image window (hit-test transparent in practice, never a drop site).
fn classify_drag_hit(hit_pid: Option<u32>, own_pid: u32, ghost: bool) -> DragHit {
    match hit_pid {
        None => DragHit::Own,
        Some(_) if ghost => DragHit::Own,
        Some(pid) if pid != own_pid => DragHit::Foreign,
        Some(_) => DragHit::Own,
    }
}

/// Foreign windows, folder tab labels and eligible item targets use an OLE drag.
fn hands_over_to_ole(hit: DragHit) -> bool {
    !matches!(hit, DragHit::Own)
}

/// `path` is the user's or the public Desktop folder (the folders whose items Corral shows).
fn is_desktop_folder(path: &str) -> bool {
    use windows::Win32::UI::Shell::{FOLDERID_Desktop, FOLDERID_PublicDesktop, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    let wanted = path.trim_end_matches('\\');
    [FOLDERID_Desktop, FOLDERID_PublicDesktop].iter().any(|id| unsafe {
        SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None).ok().is_some_and(|p| {
            let known = p.to_string().unwrap_or_default();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as *const _));
            known.trim_end_matches('\\').eq_ignore_ascii_case(wanted)
        })
    })
}

impl App {
    pub(super) fn cancel_item_drag(&mut self) {
        self.drag = Drag::None;
        self.right_start = None;
        self.selection.release = None;
        self.pages.drag_names.clear();
        self.end_item_drag_visuals();
    }

    /// Register `hwnd` as an OLE drop target once. Files dragged in from Explorer are dropped by the
    /// Shell into the desktop folder (or a portal's folder); Corral only records where to show them.
    pub(super) fn ensure_drop_target(&mut self, hwnd: HWND, site: DropSite) {
        let key = hwnd.0 as isize;
        if self.drop_registrations.contains_key(&key) {
            return;
        }
        // An isolated preview must also route external portal drops into its scratch
        // namespace. Otherwise the Shell moves them to Desktop, outside preview enumeration.
        let fixture_requested = self.opts.mode == super::Mode::Preview
            && std::env::var_os("CORRAL_QA_ITEMS_DIR").is_some();
        let fixture = self.qa_fixture_folder();
        let reject_external = fixture_requested && fixture.is_none();
        let destination = default_drop_destination(fixture_requested, fixture.map(|p| p.to_string_lossy().into_owned()));
        let message_window = self.message_window;
        let selected_tab = Rc::new(Cell::new(None));
        let selected_hover = selected_tab.clone();
        // Explorer files dropped on an ordinary member's tab label belong to that member.
        let label_target: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
        let label_hover = label_target.clone();
        let portal_target: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
        let portal_hover = portal_target.clone();
        let callbacks = DropCallbacks::new(
            move |info| {
                selected_hover.set(None);
                label_hover.set(None);
                portal_hover.set(None);
                if reject_external { return DropHint::Reject; }
                APP.with(|cell| {
                    if let Ok(mut guard) = cell.try_borrow_mut() {
                        if let Some(app) = guard.as_mut() { app.pages_ole_hover((info.point.x, info.point.y), info.keys); }
                    }
                });
                if info.entering {
                    // quick-hide auto-show on drag (QuickHideAutoShowWhenActive = 2)
                    super::quick_hide_glue::note_ole_drag_enter();
                }
                // An icon dragged out of Corral and back over its own groups/surface (OLE drag, see
                // `hand_item_drag_to_ole`): a reference change only, the Shell is never consulted
                // (it would move the item onto the desktop it is already on). If the app state is
                // busy the fallback `Shell` is turned into a rejection by the drop target.
                let own = drop_route(info.internal, None);
                SPRING_SEEN.with(|seen| seen.set(false));
                let hint = APP.with(|cell| {
                        cell.try_borrow()
                            .ok()
                            .and_then(|guard| {
                                guard.as_ref().and_then(|app| {
                                    if let Some(name) = app.drop_item_at((info.point.x, info.point.y), site) {
                                        // Capability only; the delegate negotiates the effect using the real
                                        // data object and OLE key flags. Never infer eligibility from file type.
                                        if dragdrop::item_drop_target(hwnd, &name, info.sources).is_ok() {
                                            return Some(drop_route(info.internal, Some(name)));
                                        }
                                        // An external drag on its own folder's (or own) icon: nothing, like
                                        // Explorer; never the Desktop fallback below. Internal drags keep it
                                        // (an icon dropped on itself is a reposition).
                                        if !info.internal && dragdrop::item_target_excluded(&name, info.sources) {
                                            return Some(DropHint::Reject);
                                        }
                                    }
                                    let (ox, oy) = app.origin();
                                    let client = (info.point.x - ox, info.point.y - oy);
                                    let frame = match site {
                                        DropSite::Group(id) => app.groups.iter().find(|gw| gw.id == id),
                                        // The surface stays the desktop route: a group window that is hidden
                                        // (quick-hide, inactive tab, other page) must never turn a surface drop
                                        // into a transfer into its folder.
                                        DropSite::Surface => return Some(own.clone()),
                                    };
                                    let Some(frame) = frame else { return Some(own.clone()) };
                                    let g = app.layout.groups.iter().find(|g| g.id == frame.id && app.page_layer_visible(g))?;
                                    let members = crate::tabs::members(&app.layout.groups, g);
                                    let local = (client.0 - frame.rect.left, client.1 - frame.rect.top);
                                    let cells = app.tab_cells_of(g, frame.title, app.group_dpi(g.id));
                                    let tab = (members.len() > 1 && app.page_group_visible(g)
                                        && !app.pages.moving() && app.qh_group_alpha(g.id) == 255)
                                        .then(|| crate::tabs::hit_tab(&cells, local)).flatten().map(|i| i as usize);
                                    if !info.dropping {
                                        let inactive = tab.and_then(|i| members.get(i)).map(|m| m.id).filter(|id| *id != g.id);
                                        spring_ole(inactive, message_window);
                                    }
                                    // Held-button label hover retains the desktop delegate.
                                    if tab.is_some() && !info.dropping { return Some(own.clone()) }
                                    // An untabbed portal is its own one-member list, so its frame also uses the
                                    // current (navigated) folder.
                                    let solo = [g];
                                    let listed: &[&crate::model::Group] = if members.is_empty() { &solo } else { &members };
                                    let folders: Vec<_> = listed.iter().map(|member| FolderMember {
                                        id: member.id,
                                        root: match &member.content { GroupContent::Portal { path } => Some(path.as_str()), _ => None },
                                        current: app.portal_current_path(member.id),
                                    }).collect();
                                    match frame_route(g.id, tab, &folders) {
                                        FrameRoute::Own => {
                                            // An internal drop is placed by `drop_items` (same label rule).
                                            if let Some(hit) = tab.and_then(|i| listed.get(i)).map(|m| m.id).filter(|_| info.dropping && !info.internal) {
                                                label_hover.set(Some(hit));
                                                if select_drop_tab(hit, g.id, info.keys,
                                                    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(1) < 0 || windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(2) < 0 },
                                                    app.tab_hover_state.tab.filter(|_| !app.tab_hover_state.spent)) {
                                                    selected_hover.set(Some(hit));
                                                }
                                            }
                                            return Some(own.clone());
                                        }
                                        FrameRoute::Content => {}
                                        FrameRoute::Folder { member, path, fallback } => {
                                            if fallback { log(&format!("portal drop {member}: no current view path; using root {path:?}")); }
                                            if fixture_requested && !safe_qa_folder(path) { return Some(DropHint::Reject) }
                                            if info.dropping && tab.is_some() { portal_hover.set(Some(member)); }
                                            if info.dropping && tab.is_some() && select_drop_tab(member, g.id, info.keys,
                                                unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(1) < 0 || windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(2) < 0 },
                                                app.tab_hover_state.tab.filter(|_| !app.tab_hover_state.spent)) {
                                                selected_hover.set(Some(member));
                                            }
                                            return Some(if info.internal && is_desktop_folder(path) { own.clone() }
                                                else { DropHint::ShellTo(Destination::Folder(path.to_string())) });
                                        }
                                    }
                                    // No tab hit and no portal folder resolved: the frame's own content decides.
                                    Some(match &g.content {
                                        // an icon of the desktop itself dropped on a portal of the Desktop folder:
                                        // a Shell drop would only duplicate it ("- Copy"), so a reference change
                                        GroupContent::Portal { path } if info.internal && is_desktop_folder(path) => own.clone(),
                                        // a Folder Portal group's frame: the Shell moves/copies into its folder
                                        GroupContent::Portal { path } if fixture_requested && !safe_qa_folder(path) => DropHint::Reject,
                                        GroupContent::Portal { path } => DropHint::ShellTo(Destination::Folder(path.clone())),
                                        GroupContent::Items { .. } => own.clone(),
                                    })
                                })
                            })
                            .unwrap_or(DropHint::Reject)
                    });
                if !SPRING_SEEN.with(|seen| seen.get()) { spring_ole(None, message_window); }
                hint
            },
            move |dropped| {
                spring_reset();
                if let Some(member) = portal_target.take() {
                    PORTAL_REFRESH.with(|list| list.borrow_mut().push(member));
                }
                if let Some(member) = selected_tab.take() {
                    enqueue_drop(DeferredDrop::SelectTab(member), message_window);
                }
                APP.with(|cell| {
                    if let Ok(mut guard) = cell.try_borrow_mut() {
                        if let Some(app) = guard.as_mut() {
                            app.record_external_drop(label_target.take().map(DropSite::Group).unwrap_or(site), dropped);
                        }
                    }
                });
            },
        ).with_item_feedback(update_drop_highlight).with_leave(|| { spring_reset(); APP.with(|cell| {
            if let Ok(mut guard) = cell.try_borrow_mut() { if let Some(app) = guard.as_mut() { app.pages_ole_leave(); } }
        }) }).with_quick_create(
            move |point| APP.with(|cell| cell.try_borrow().ok().is_some_and(|guard| guard.as_ref().is_some_and(|app|
                !app.settings.disable_create_folder_portal_here
                    && !app.layout.desktop_locked
                    && (!fixture_requested || app.qa_fixture_folder().is_some())
                    // a Gate's tab label hovers as the desktop but drops into the folder: the drop target wins
                    && !app.portal_label_at((point.x, point.y))))),
            move |paths, point| enqueue_drop(DeferredDrop::CreatePortal { paths, screen: (point.x, point.y) }, message_window),
        );
        match dragdrop::register(hwnd, DropTarget::new(hwnd, destination, callbacks)) {
            Ok(registration) => {
                self.drop_registrations.insert(key, registration);
            }
            Err(e) => log(&format!("RegisterDragDrop failed for {key:#x}: {e}")),
        }
    }

    /// Remember where the shell-created items should appear, then refresh items.
    fn record_external_drop(&mut self, site: DropSite, dropped: &Dropped) {
        if !positional_drop(dropped.destination.as_ref()) {
            self.pages_ole_leave();
            // A plain move into a folder or the Recycle Bin: the Shell finishes it in the background
            // and reports only then; hide the moved icons at once (display only, the layout keeps
            // them). The re-reads below are the truth: a cancelled or failed move shows them again.
            if dropped.effect == DROPEFFECT_MOVE && dropped.target_effect.is_some() {
                let moved: Vec<String> = dropped.sources.iter().map(|s| s.to_lowercase()).collect();
                let before = self.items.len();
                self.items.retain(|i| !moved.contains(&i.shell.parsing_name.to_lowercase()));
                if self.items.len() != before {
                    log(&format!("item drop hand-over: {} moved icon(s) hidden until the re-read", before - self.items.len()));
                    self.refresh_all();
                }
            }
            self.reload_soon_after_handover();
            return;
        }
        self.pages_settle();
        self.pages_ole_leave();
        // Corral's own OLE drag released over its own window (`DropHint::Internal`): remember the
        // point; `finish_ole_drag` applies the reference change after `DoDragDrop` returned (the
        // sorting prompt must not run inside this callback).
        if dropped.internal && dropped.destination.is_none() {
            if let Some(ole) = self.ole_drag.as_mut() {
                ole.dropped_at = Some((dropped.point.x, dropped.point.y));
            }
            return;
        }
        let (ox, oy) = self.origin();
        let client = (dropped.point.x - ox, dropped.point.y - oy);
        let (cw, ch) = self.loose_cell();
        let placement = match site {
            DropSite::Group(id) => PendingPlacement::Group(id),
            DropSite::Surface => PendingPlacement::Loose((client.0 - cw / 2, client.1 - ch / 3), self.pages.current),
        };
        let now = Instant::now();
        for (index, name) in dropped.expected_destinations.iter().enumerate() {
            let placement = match placement {
                // several files dropped loose: stack them in a column like Explorer does
                PendingPlacement::Loose((x, y), page) => PendingPlacement::Loose((x, y + index as i32 * ch), page),
                other => other,
            };
            self.pending_drops.insert(name.to_lowercase(), (placement, now));
        }
        log(&format!("external drop: {} item(s), effect {:?}", dropped.expected_destinations.len(), dropped.effect));
        self.schedule_item_reload();
    }

    /// Apply pending placements for items that now exist; expire requests older than 15 s.
    pub(super) fn apply_pending_drops(&mut self) {
        self.run_deferred_drops();
        for id in PORTAL_REFRESH.with(|list| std::mem::take(&mut *list.borrow_mut())) {
            self.reload_gate(id);
        }
        if self.pending_drops.is_empty() {
            return;
        }
        self.apply_pending_placements();
    }

    /// Deferred drop actions queued by OLE callbacks; each runs once. True if any ran.
    pub(super) fn run_deferred_drops(&mut self) -> bool {
        let actions = DEFERRED_DROPS.with(|queue| take_deferred(&mut queue.borrow_mut()));
        let ran = !actions.is_empty();
        for action in actions {
            match action {
                DeferredDrop::SelectTab(member) => self.select_tab(member),
                DeferredDrop::CreatePortal { paths, screen } => {
                    // The folders were classified before the drop; nothing is read again here, so a dead
                    // network path cannot hang the desktop (creation-spec 5.3).
                    if self.layout.desktop_locked { log("Alt-drop: desktop locked"); continue }
                    let paths: Vec<String> = paths.into_iter()
                        .filter(|path| !(self.opts.mode == super::Mode::Preview && std::env::var_os("CORRAL_QA_ITEMS_DIR").is_some()
                            && !safe_qa_folder(path)))
                        .collect();
                    if paths.is_empty() { log("Alt-drop: nothing to create"); continue }
                    self.create_quick_gates(&paths, screen);
                }
            }
        }
        ran
    }

    /// Placements for dropped items that now exist; requests older than 15 s expire.
    fn apply_pending_placements(&mut self) {
        let mut changed = false;
        let names: Vec<String> = self.items.iter().map(|i| i.shell.parsing_name.clone()).collect();
        for name in names {
            let Some((placement, _)) = self.pending_drops.remove(&name.to_lowercase()) else { continue };
            match placement {
                PendingPlacement::Group(id) => {
                    // A Gate shows what is in its folder: nothing to record, and no manual mark for its files.
                    let len = self.layout.groups.iter().find(|g| g.id == id).and_then(|g| match &g.content {
                        GroupContent::Items { items } => Some(items.len()),
                        GroupContent::Portal { .. } => None,
                    });
                    if let Some(len) = len {
                        self.layout.insert_item(&name, id, len);
                        self.layout.mark_manual_now(&name);
                        changed = true;
                    }
                }
                PendingPlacement::Loose(cell_origin, page) => {
                    let (ox, oy) = self.origin();
                    let screen = (cell_origin.0 + ox, cell_origin.1 + oy);
                    if let Some(m) = self.monitors.iter().find(|m| m.bounds.contains(screen.0, screen.1)).or(self.monitors.first()).cloned() {
                        let position = model::LoosePosition {
                            monitor: m.name.clone(),
                            x: to_dip(screen.0 - m.work.left, m.dpi),
                            y: to_dip(screen.1 - m.work.top, m.dpi),
                        };
                        self.layout.place_loose(&name, position);
                        self.layout.mark_manual_now(&name);
                        self.layout.set_icon_page(&name, page);
                        if let Some(item) = self.items.iter_mut().find(|i| i.shell.parsing_name.eq_ignore_ascii_case(&name)) {
                            item.pos = cell_origin;
                            item.placed = true;
                        }
                        changed = true;
                    }
                }
            }
        }
        self.pending_drops.retain(|_, (_, at)| at.elapsed().as_secs() < 15);
        if changed {
            self.record_desk_edit(); self.save();
        }
    }

    /// Parent-client position of a loose item the user placed in Corral, if any.
    pub(super) fn loose_client_position(&self, name: &str) -> Option<(i32, i32)> {
        let placed = self.layout.loose_position(name)?;
        let m = self.monitor_for(&placed.monitor)?;
        let (ox, oy) = self.origin();
        Some((m.work.left + to_px(placed.x, m.dpi) - ox, m.work.top + to_px(placed.y, m.dpi) - oy))
    }

    pub(super) fn update_item_drag(&mut self, screen: (i32, i32)) {
        let Drag::Item { start, current, active, .. } = &mut self.drag else { return };
        *current = screen;
        if !*active {
            let (tx, ty) = unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
            if (screen.0 - start.0).abs() <= tx && (screen.1 - start.1).abs() <= ty {
                return;
            }
            *active = true;
        }
        if self.pages.drag_names.is_empty() {
            if let Drag::Item { name, .. } = &self.drag { let name = name.clone(); self.pages.drag_names = self.drag_set(&name); }
        }
        self.render_ghost();
        self.spring_tab(Some(screen));
    }

    /// Spring-loaded tabs (tabs-spec 4): an icon drag held over an inactive tab selects it after 600 ms
    /// even with hover switching off, so an item can be dropped into another tab.
    pub(super) fn spring_tab(&mut self, screen: Option<(i32, i32)>) {
        let under = screen.and_then(|s| self.tab_label_at(s)).map(|(_, member)| member)
            .filter(|m| self.layout.groups.iter().find(|g| g.id == *m).is_some_and(|g| !crate::tabs::body_visible(&self.layout.groups, g)));
        let input = crate::tabs::HoverInput { tab: under, now_ms: super::tick_ms(), wait_ms: screen.map(|_| crate::tabs::SPRING_MS), cancel: false };
        let (state, out) = crate::tabs::hover_switch_step(self.tab_hover_state, input);
        self.tab_hover_state = state;
        match out {
            crate::tabs::HoverOut::Select(member) => self.select_tab(member),
            crate::tabs::HoverOut::Wait(ms) => unsafe { SetTimer(self.message_window, super::TIMER_TAB_HOVER, ms.clamp(1, 30), None); },
            crate::tabs::HoverOut::Idle => unsafe { let _ = KillTimer(self.message_window, super::TIMER_TAB_HOVER); },
        }
    }

    /// Draw/position the translucent drag image (icon + label at 60 % opacity).
    fn render_ghost(&mut self) {
        let Drag::Item { name, current, grab, .. } = &self.drag else { return };
        let Some(item) = self.items.iter().find(|i| i.shell.parsing_name.eq_ignore_ascii_case(name)) else { return };
        let cell = self.loose_cell();
        if self.ghost.is_none() {
            let hwnd = unsafe {
                CreateWindowExW(WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST, w!("CorralGhost"),
                    PCWSTR::null(), WS_POPUP, 0, 0, cell.0, cell.1, HWND::default(), HMENU::default(), self.instance, None)
            };
            let (Ok(hwnd), Ok(surface)) = (hwnd, Surface::new(cell.0.max(1) as u32, cell.1.max(1) as u32)) else { return };
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            }
            self.ghost = Some((hwnd, surface));
        }
        let position = (current.0 - grab.0, current.1 - grab.1);
        let Some((hwnd, mut surface)) = self.ghost.take() else { return };
        let _ = surface.resize(cell.0.max(1) as u32, cell.1.max(1) as u32);
        let drawn = surface.draw(|ctx| {
            ctx.clear(Rgba::TRANSPARENT);
            self.draw_cell(ctx, item, 0.0, 0.0, cell)
        });
        if drawn.is_ok() {
            let _ = present_layered(hwnd, &surface, position, 153);
        }
        self.ghost = Some((hwnd, surface));
    }

    /// Release the mouse capture and destroy the drag image window (every exit of an item drag).
    fn end_item_drag_visuals(&mut self) {
        self.tab_hover_state = crate::tabs::HoverState::default();
        unsafe {
            let _ = ReleaseCapture();
        }
        if let Some((hwnd, _)) = self.ghost.take() {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
    }

    /// Hand an active icon drag over to OLE when the cursor is over a foreign window or a Corral
    /// portal popup, so the item can be dropped into Explorer folders, other applications and the
    /// portals' own shell views. Runs from the `WM_MOUSEMOVE` handler: the ghost and the capture
    /// are released here and the request is queued for `run_pending_ole_drag`, which calls
    /// `dragdrop::begin_drag` once `APP` is no longer borrowed (the initiating button is held).
    pub(super) fn hand_item_drag_to_ole(&mut self, wparam: WPARAM) {
        let Drag::Item { name, grab, active: true, start, owner, .. } = &self.drag else { return };
        if wparam.0 & 0x0003 == 0 /* real initiating mouse button */ || OLE_DRAG_RUNNING.with(|c| c.get()) {
            return;
        }
        // The initiating button must really be down in this thread's input state, not only in
        // `wparam`: posted QA mouse messages can carry button flags without a pressed button, and
        // `DoDragDrop` would then drop at once on whatever window is under the real cursor.
        let button = if wparam.0 & 1 != 0 { 1 } else { 2 };
        if unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(button) } >= 0 {
            return;
        }
        if self.ole_drag.take().is_some() {
            log("ole drag: stale state from an earlier drag discarded");
        }
        let (name, grab, start, owner) = (name.clone(), *grab, *start, *owner);
        let mut cursor = POINT::default();
        if unsafe { GetCursorPos(&mut cursor) }.is_err() {
            return;
        }
        let hit = unsafe { WindowFromPoint(cursor) };
        let hit_pid = (!hit.0.is_null()).then(|| {
            let mut pid = 0u32;
            let thread = unsafe { GetWindowThreadProcessId(hit, Some(&mut pid)) };
            (thread != 0).then_some(pid)
        });
        let ghost = self.ghost.as_ref().is_some_and(|(g, _)| *g == hit);
        let mut class = classify_drag_hit(hit_pid.flatten(), std::process::id(), ghost);
        // A portal member's tab label on Corral's own window: only the Shell can transfer into its
        // folder, so the drag becomes an OLE drag there too (the drop target then routes it to
        // that member's current folder and selects it).
        if class == DragHit::Own && !ghost && self.portal_label_at((cursor.x, cursor.y)) {
            class = DragHit::Portal;
        }
        if class == DragHit::Own && !ghost {
            let site = self.visible_frame_at((cursor.x - self.origin().0, cursor.y - self.origin().1))
                .map(|i| DropSite::Group(self.groups[i].id)).unwrap_or(DropSite::Surface);
            let sources = self.drag_set(&name);
            if self.drop_item_at((cursor.x, cursor.y), site)
                .is_some_and(|target| dragdrop::item_drop_target(owner, &target, &sources).is_ok()) {
                class = DragHit::Item;
            }
        }
        // Right drags always enter the OLE loop after the threshold. Its source retains the
        // real right button and the destination Shell owns the action menu. Files a Gate shows are not
        // Corral references: wherever they go, the Shell moves or copies them (corral-views-spec 2.3).
        let from_gate = self.items.iter().any(|i| i.gate.is_some() && i.shell.parsing_name.eq_ignore_ascii_case(&name));
        if !hands_over_to_ole(class) && button != 2 && !from_gate {
            return;
        }
        let image = self.ghost_drag_image(grab);
        // The whole selection travels, but one data object needs one parent folder: when the
        // selection also holds items of another folder (user vs public Desktop) only the items
        // sharing the pressed item's parent are dragged.
        let (names, dropped) = selection::sharing_parent_with(&name, &self.drag_set(&name));
        if dropped > 0 {
            log(&format!("ole drag: {dropped} selected item(s) outside the folder of {name:?} are not dragged"));
        }
        self.drag = Drag::None;
        self.right_start = None;
        self.selection.release = None; // a drag, not a click: the selection stays as it is
        self.end_item_drag_visuals();
        let source = self.groups.iter().find(|g| g.hwnd == owner).map(|g| g.id);
        log(&format!("item drag {name:?} ({} item(s)) leaves Corral ({class:?} at {},{}): OLE drag, source group {source:?}", names.len(), cursor.x, cursor.y));
        self.ole_drag = Some(OleDrag { name: name.clone(), names: names.clone(), grab, start, source, dropped_at: None });
        PENDING_OLE_DRAG.with(|cell| *cell.borrow_mut() = Some(OleDragRequest { owner, names, image, internal: !from_gate }));
    }

    /// The item whose ICON is under `screen` (drop-onto target). Only the icon counts, not the whole
    /// cell: Corral cells tile without gaps and a drop on a cell is how icons are reordered, so a
    /// whole-cell target turned every reorder onto a folder/shortcut slot into a move into the folder
    /// or a program launch (file-safety review 2026-10-15, finding 1; Explorer's list view also
    /// targets the item, not its spacing).
    fn drop_item_at(&self, screen: (i32, i32), site: DropSite) -> Option<String> {
        let (ox, oy) = self.origin();
        let client = (screen.0 - ox, screen.1 - oy);
        match site {
            DropSite::Group(id) => {
                let index = self.groups.iter().position(|g| g.id == id)?;
                let rect = self.groups[index].rect;
                let local = (client.0 - rect.left, client.1 - rect.top);
                let name = self.item_in_group(index, local)?;
                let group = self.layout.groups.iter().find(|g| g.id == id)?;
                let dpi = self.group_dpi(id);
                let grid = self.group_grid(id, dpi, self.groups[index].body, group.scroll);
                let slot = grid.slot_rect(grid.slot_at(local)?);
                let in_cell = (local.0 - slot.left, local.1 - slot.top);
                on_icon(in_cell, (slot.width(), slot.height()), grid.m.icon_px, dpi).then_some(name)
            }
            DropSite::Surface => {
                if self.visible_frame_at(client).is_some() { return None }
                let index = self.item_at_surface(client)?;
                let item = &self.items[index];
                let in_cell = (client.0 - item.pos.0, client.1 - item.pos.1);
                (item.placed && on_icon(in_cell, self.loose_cell(), self.icon_px, self.dpi()))
                    .then(|| item.shell.parsing_name.clone())
            }
        }
    }

    /// The tab label under `screen` on a visible tab set: (group window index, hit member id).
    fn tab_label_at(&self, screen: (i32, i32)) -> Option<(usize, u64)> {
        let (ox, oy) = self.origin();
        let client = (screen.0 - ox, screen.1 - oy);
        let index = self.visible_frame_at(client)?;
        let rect = self.groups[index].rect;
        Some((index, self.tab_hit(index, (client.0 - rect.left, client.1 - rect.top))?))
    }

    /// Topmost visible group window containing the client point (hidden tab members excluded).
    fn visible_frame_at(&self, client: (i32, i32)) -> Option<usize> {
        self.groups.iter().rposition(|gw| self.page_group_id_visible(gw.id) && gw.rect.contains(client.0, client.1)
            && self.layout.groups.iter().any(|g| g.id == gw.id && crate::tabs::body_visible(&self.layout.groups, g)))
    }

    /// `screen` is over the tab label of a portal member. Only labels: the title branch
    /// needs a label hit on a set with a portal member; a standalone portal's
    /// title or frame keeps the internal drag, whose drop moves the files into its folder.
    fn portal_label_at(&self, screen: (i32, i32)) -> bool {
        self.tab_label_at(screen).is_some_and(|(_, member)| self.layout.groups.iter()
            .any(|g| g.id == member && matches!(g.content, GroupContent::Portal { .. })))
    }

    /// The ghost's pixels as the OLE drag image (hot spot = cursor offset inside the cell).
    fn ghost_drag_image(&self, grab: (i32, i32)) -> Option<dragdrop::DragImage> {
        let (_, surface) = self.ghost.as_ref()?;
        let (w, h) = (surface.width(), surface.height());
        let hot = (grab.0.clamp(0, w as i32 - 1), grab.1.clamp(0, h as i32 - 1));
        dragdrop::DragImage::from_bgra_premultiplied(w, h, surface.pixels(), hot).ok()
    }

    /// The OLE drag ended (`DoDragDrop` returned). Corral never deletes files and moves them only into a
    /// Folder Gate it was dropped on (`drop_items`). Here the
    /// Shell target already moved them, so this only refreshes the items (a moved-away item disappears
    /// from the enumeration; an optimised move reports `DROPEFFECT_NONE`, so the effect is not
    /// trusted) and applies a drop on Corral's own windows as the same reference change as the
    /// internal path.
    pub(super) fn finish_ole_drag(&mut self, result: windows::core::Result<dragdrop::DragOutcome>) {
        self.pages.drag_names.clear();
        let Some(ole) = self.ole_drag.take() else { return };
        // The hand-over released these; nothing during the modal loop may have started a drag.
        if !matches!(self.drag, Drag::None) {
            log("ole drag: unexpected internal drag state after the drag; reset");
            self.drag = Drag::None;
        }
        self.end_item_drag_visuals();
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(e) => {
                log(&format!("ole drag of {:?} failed: {e}", ole.name));
                return;
            }
        };
        log(&format!(
            "ole drag of {:?} ended: effect {:?} cancelled {} performed {:?} internal drop {:?}",
            ole.name, outcome.effect, outcome.cancelled, outcome.performed_effect, ole.dropped_at
        ));
        if outcome.cancelled {
            return; // Esc / right button: nothing changes
        }
        match ole.dropped_at {
            Some(screen) => self.drop_items(&ole.name, &ole.names, ole.grab, ole.start, ole.source, screen),
            None => self.schedule_item_reload(),
        }
    }

    /// Drop: into the group under the cursor (at the slot under it) or loose on the desktop.
    /// Only Corral's references change; the file itself moves only onto a Folder Gate (into its folder). The whole selection moves
    /// when the pressed item is part of it; a button release without a drag applies the deferred
    /// part of the press instead (a plain click on a selected item selects only it).
    pub(super) fn finish_item_drag(&mut self, screen: (i32, i32)) {
        self.pages_settle();
        let names = match &self.drag { Drag::Item { name, .. } => self.drag_set(name), _ => Vec::new() };
        let drag = std::mem::replace(&mut self.drag, Drag::None);
        self.end_item_drag_visuals();
        let Drag::Item { name, grab, active, start, owner, .. } = drag else { return };
        if !active {
            self.apply_release();
            return;
        }
        self.selection.release = None;
        let source = self.groups.iter().find(|g| g.hwnd == owner).map(|g| g.id);
        self.pages.drag_names.clear();
        if self.alt_drop_of_items(&name, &names, screen) { return }
        self.drop_items(&name, &names, grab, start, source, screen);
    }

    /// Alt-drop of dragged icons (creation-spec 5): with Alt alone held, a left-button drop of folders on
    /// empty desktop, a Corral's title strip or a Corral's empty body makes one Folder Gate per folder.
    /// Nothing moves: the icons stay where they were and no file is touched. True when it applied (or was
    /// refused for being more than six folders).
    fn alt_drop_of_items(&mut self, pressed: &str, names: &[String], screen: (i32, i32)) -> bool {
        use crate::creation::{self, AltDrop, AltTarget, DropKeys};
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VIRTUAL_KEY, VK_CONTROL, VK_LCONTROL, VK_LMENU, VK_MENU, VK_RCONTROL, VK_RMENU, VK_SHIFT};
        if self.layout.desktop_locked { return false }
        let down = |key: VIRTUAL_KEY| unsafe { GetAsyncKeyState(key.0 as i32) } < 0;
        let keys = DropKeys {
            mk: [(VK_MENU, creation::MK_ALT), (VK_CONTROL, creation::MK_CONTROL), (VK_SHIFT, creation::MK_SHIFT)]
                .into_iter().filter(|(key, _)| down(*key)).fold(0, |acc, (_, flag)| acc | flag),
            lmenu: down(VK_LMENU), rmenu: down(VK_RMENU), lctrl: down(VK_LCONTROL), rctrl: down(VK_RCONTROL),
        };
        let mods = creation::drop_modifiers(&keys);
        if !mods.alt || mods.ctrl || mods.shift { return false }
        let (ox, oy) = self.origin();
        let client = (screen.0 - ox, screen.1 - oy);
        // a Gate's tab label drops into its folder, like the OLE path (the whole Gate frame is the folder)
        let target = if self.portal_label_at(screen) { AltTarget::Other } else { match self.visible_frame_at(client) {
            Some(index) => {
                let (id, rect) = (self.groups[index].id, self.groups[index].rect);
                let items_group = self.layout.groups.iter().any(|g| g.id == id && matches!(g.content, GroupContent::Items { .. }));
                if items_group && self.item_in_group(index, (client.0 - rect.left, client.1 - rect.top)).is_none() { AltTarget::Empty } else { AltTarget::Other }
            }
            None if self.item_at_surface(client).is_some() => AltTarget::Other,
            None => AltTarget::Empty,
        } };
        if target != AltTarget::Empty { return false }
        let mut all: Vec<String> = names.to_vec();
        if !all.iter().any(|n| n.eq_ignore_ascii_case(pressed)) { all.insert(0, pressed.to_string()); }
        // The items' own Shell attributes decide (a folder, not a stream folder such as a .zip, not a
        // shortcut, with a file-system path): no file is touched and nothing waits on a share.
        let known: Vec<String> = all.iter()
            .filter(|n| self.items.iter().find(|i| i.shell.parsing_name.eq_ignore_ascii_case(n))
                .is_some_and(|i| creation::known_item_is_folder(&i.shell.parsing_name, i.shell.is_folder, i.shell.is_link)))
            .cloned().collect();
        let folders = creation::classify_folders(&known, |_| true);
        match creation::alt_drop_decision(&keys, creation::MK_LBUTTON, Some(&folders), target, true, true) {
            AltDrop::Ordinary => false,
            AltDrop::TooMany | AltDrop::Refused(_) => { log("Alt-drop: refused"); true }
            AltDrop::Accept(_) => {
                self.create_quick_gates(&folders, screen);
                true
            }
        }
    }

    /// Reference change of the items dropped at `screen`: shared by the internal drag and by OLE
    /// drops on Corral's own windows. `pressed` is the item the drag was started on (the one under
    /// the cursor), `source` the group it was dragged out of.
    fn drop_items(&mut self, pressed: &str, names: &[String], grab: (i32, i32), start: (i32, i32), source: Option<u64>, screen: (i32, i32)) {
        let mut names: Vec<String> =
            names.iter().filter(|n| self.items.iter().any(|i| i.shell.parsing_name.eq_ignore_ascii_case(n))).cloned().collect();
        if !names.iter().any(|n| n.eq_ignore_ascii_case(pressed)) {
            names.insert(0, pressed.to_string());
        }
        let (ox, oy) = self.origin();
        let client = (screen.0 - ox, screen.1 - oy);
        // A tab label of an ordinary member: the items join that member, which becomes the visible
        // tab (a drop selects the hit tab; the frame's visible member does not take them).
        if let Some((_, member)) = self.tab_label_at(screen) {
            let end = self.layout.groups.iter().find(|g| g.id == member).and_then(|g| match &g.content {
                GroupContent::Items { items } => Some(items.len()),
                GroupContent::Portal { .. } => None,
            });
            if let Some(end) = end {
                log(&format!("drop of {} item(s) on the tab label of group {member}", names.len()));
                selection::insert_set(&mut self.layout, &names, member, end);
                self.select_tab(member);
                self.record_desk_edit(); self.save();
                self.arrange_loose();
                self.refresh_all();
                return;
            }
        }
        // hidden tab members share the frame rectangle and must not take the drop
        let target = self.visible_frame_at(client).map(|i| &self.groups[i]).map(|g| (g.id, g.rect, g.body));
        match target {
            Some((id, _, _)) if self.gate_drop_folder(id).is_some() => {
                let folder = self.gate_drop_folder(id).unwrap_or_default();
                self.move_into_gate(id, &names, &folder);
                return;
            }
            Some((id, rect, body)) => {
                if source == Some(id) {
                    self.sorted_move(id, start, screen);
                }
                let dpi = self.group_dpi(id);
                let scroll = self.layout.groups.iter().find(|g| g.id == id).map_or(0, |g| g.scroll);
                let grid = self.group_grid(id, dpi, body, scroll);
                let count = self.layout.groups.iter().find(|g| g.id == id).map_or(0, |g| self.group_members(g).len());
                let slot = grid.drop_slot((client.0 - rect.left, client.1 - rect.top), count) as i32;
                log(&format!("drop of {} item(s) into group {id} at slot {slot}", names.len()));
                selection::insert_set(&mut self.layout, &names, id, slot.max(0) as usize);
            }
            None => {
                let cell_origin = (client.0 - grab.0, client.1 - grab.1);
                log(&format!("drop of {} item(s) loose at {cell_origin:?}", names.len()));
                let offsets = self.drop_offsets(pressed, &names);
                self.place_loose_set(pressed, screen, cell_origin, offsets);
            }
        }
        self.record_desk_edit(); self.save();
        self.arrange_loose();
        self.refresh_all();
    }

    /// The folder an icon dropped on group `id` moves into: the folder a Folder Gate shows now (its root
    /// until it navigates), except the Desktop folder itself (its icons are already there; the drop stays
    /// a reference change).
    fn gate_drop_folder(&self, id: u64) -> Option<String> {
        let GroupContent::Portal { path } = &self.layout.groups.iter().find(|g| g.id == id)?.content else { return None };
        let shown = self.portal_current_path(id).unwrap_or(path);
        (!is_desktop_folder(shown)).then(|| shown.to_string())
    }

    /// Icons dropped on a Folder Gate move into its folder through the Shell, as a drop that Windows
    /// carries onto the Gate would. A QA preview never moves files outside its scratch root.
    fn move_into_gate(&mut self, id: u64, names: &[String], folder: &str) {
        let fixture_requested = self.opts.mode == super::Mode::Preview && std::env::var_os("CORRAL_QA_ITEMS_DIR").is_some();
        if fixture_requested && !safe_qa_folder(folder) {
            log(&format!("drop into Gate {id} refused: {folder:?} is outside the QA scratch root"));
            return;
        }
        log(&format!("drop of {} item(s) into the folder of Gate {id}", names.len()));
        if let Err(e) = super::shell_items::move_with_shell(self.surface_hwnd, names, folder) {
            log(&format!("move into Gate {id} failed: {e}"));
        }
        self.reload_gate(id);
        self.reload_soon_after_handover();
    }

    /// Offsets of the dragged items from the pressed one when dropped on the desktop: they keep
    /// their arrangement (loose pixel offsets, or the grid of their group in desktop cells).
    fn drop_offsets(&self, pressed: &str, names: &[String]) -> Vec<(String, (i32, i32))> {
        let homes: Vec<(String, Home)> = names
            .iter()
            .map(|name| {
                let home = match self.group_of(name) {
                    Some(id) => {
                        let cells = self.group_cells(id, self.group_dpi(id));
                        let slot = self.container_order(Some(id)).iter().position(|n| n.eq_ignore_ascii_case(name)).unwrap_or(0) as i32;
                        let body_width = self.groups.iter().find(|g| g.id == id).map_or(cells.cell_w_px, |g| g.body.width());
                        let cols = cells.cols_for(body_width);
                        Home::Group { id, col: slot % cols, row: slot / cols }
                    }
                    None => Home::Loose(self.items.iter().find(|i| i.shell.parsing_name.eq_ignore_ascii_case(name)).map_or((0, 0), |i| i.pos)),
                };
                (name.clone(), home)
            })
            .collect();
        let at = names.iter().position(|n| n.eq_ignore_ascii_case(pressed)).unwrap_or(0);
        selection::loose_offsets(&homes, at, self.loose_cell())
    }

    /// Loose drop of several icons: every icon lands inside the work area of the monitor under the
    /// cursor (`screen`), clear of groups and of the loose icons that stay; the one under the
    /// cursor is placed first. The grid search returns None when the monitor is full.
    fn place_loose_set(&mut self, pressed: &str, screen: (i32, i32), cell_origin: (i32, i32), offsets: Vec<(String, (i32, i32))>) {
        let bounds: Vec<Rect> = self.monitors.iter().map(|m| m.bounds).collect();
        let Some(monitor) = selection::monitor_index_at(screen, &bounds).map(|i| self.monitors[i].clone()) else { return };
        let (ox, oy) = self.origin();
        let area = monitor.work.offset(-ox, -oy);
        let render_cell = self.loose_cell();
        // Read at release, not from the last item reload: Explorer may have moved icons since.
        // Fixture previews must never sample the user's Desktop for placement.
        let fixture = self.opts.mode == super::Mode::Preview && std::env::var_os("CORRAL_QA_ITEMS_DIR").is_some();
        let view = if fixture { None } else { crate::desktop_view::read_desktop_view().ok() };
        let sample_dpi = self.host.map(|h| unsafe {
            windows::Win32::UI::HiDpi::GetDpiForWindow(h.listview)
        }).filter(|d| *d != 0).unwrap_or(self.dpi());
        let spacing = view.as_ref().map_or(self.spacing, |v| v.spacing);
        let spacing = if spacing.0 > 0 && spacing.1 > 0 { spacing } else { render_cell };
        let cell = drop_grid::scaled_spacing(spacing, sample_dpi, monitor.dpi);
        // The current loose renderer still has a global footprint. Reserve that too, so mixed-DPI
        // placement cannot overlap the displayed cells even when target grid pitch is smaller.
        let footprint = (cell.0.max(render_cell.0), cell.1.max(render_cell.1));
        let moving = |name: &str| offsets.iter().any(|(n, _)| n.eq_ignore_ascii_case(name));
        let mut blocked: Vec<Rect> = self.layout.groups.iter().filter(|g| self.page_group_visible(g)).map(|g| self.group_client_rect(g)).collect();
        blocked.extend(
            self.items
                .iter()
                .filter(|i| i.placed && self.page_name_visible(&i.shell.parsing_name) && self.group_of(&i.shell.parsing_name).is_none() && !moving(&i.shell.parsing_name))
                .map(|i| Rect::new(i.pos.0, i.pos.1, i.pos.0 + render_cell.0, i.pos.1 + render_cell.1)),
        );
        let mut wanted: Vec<(String, (i32, i32))> =
            offsets.iter().map(|(name, (dx, dy))| (name.clone(), (cell_origin.0 + dx, cell_origin.1 + dy))).collect();
        wanted.sort_by_key(|(name, _)| !name.eq_ignore_ascii_case(pressed)); // stable: the pressed icon first
        let lv = self.listview_origin();
        let mut sample = None;
        if let Some(view) = &view {
            for icon in &view.icons {
                let p = (icon.position.0 + lv.0, icon.position.1 + lv.1);
                if area.contains(p.0, p.1) && sample.is_none() { sample = Some(p); }
                if !moving(&icon.parsing_name) {
                    blocked.push(Rect::new(p.0, p.1, p.0 + footprint.0, p.1 + footprint.1));
                }
            }
        }
        let bounds = (area.left, area.top, area.right, area.bottom);
        let anchor = drop_grid::anchor(bounds, cell, sample);
        let mut blocked: Vec<_> = blocked.iter().map(|r| (r.left, r.top, r.right, r.bottom)).collect();
        for (name, want) in wanted {
            if let Some(origin) = drop_grid::nearest(want, cell, footprint, bounds, anchor, &blocked) {
                blocked.push((origin.0, origin.1, origin.0 + footprint.0, origin.1 + footprint.1));
                self.place_loose_item(&name, origin);
            } else {
                log(&format!("desktop drop: no free grid cell for {name:?}; retaining its placement"));
            }
        }
    }

    /// Make `name` a loose icon whose cell origin is `cell_origin` (parent-client px). The cell
    /// belongs to the monitor containing its origin; an origin on no monitor is pulled into the
    /// work area of the nearest one, so an icon is never saved where it cannot be seen.
    fn place_loose_item(&mut self, name: &str, cell_origin: (i32, i32)) {
        let (ox, oy) = self.origin();
        let bounds: Vec<Rect> = self.monitors.iter().map(|m| m.bounds).collect();
        let screen_origin = (cell_origin.0 + ox, cell_origin.1 + oy);
        let monitor = selection::monitor_index_at(screen_origin, &bounds).map(|i| self.monitors[i].clone());
        let (mut cell_origin, mut screen_origin) = (cell_origin, screen_origin);
        if let Some(m) = &monitor {
            if !m.bounds.contains(screen_origin.0, screen_origin.1) {
                screen_origin = selection::clamp_cell(screen_origin, self.loose_cell(), &m.work);
                cell_origin = (screen_origin.0 - ox, screen_origin.1 - oy);
            }
        }
        if let Some(m) = monitor {
            let position = model::LoosePosition {
                monitor: m.name.clone(),
                x: to_dip(screen_origin.0 - m.work.left, m.dpi),
                y: to_dip(screen_origin.1 - m.work.top, m.dpi),
            };
            // dragging an icon out of a Corral onto the desktop is the user's own placement
            let left_a_corral = self.group_of(name).is_some();
            self.layout.place_loose(name, position);
            if left_a_corral {
                self.layout.mark_manual_now(name);
            }
            if self.settings.swipe_enabled { self.layout.set_icon_page(name, self.pages.current); }
            if let Some(item) = self.items.iter_mut().find(|i| i.shell.parsing_name.eq_ignore_ascii_case(name)) {
                item.pos = cell_origin;
                item.placed = true;
            }
        }
    }
}

/// Pure: `in_cell` (cell-relative pixels) lies on the item drawn in a `cell`: its icon (centred,
/// 4 DIP below the cell top, `icon` px square, the layout of `draw_cell`) or the first two lines of
/// its label under it, in a centred strip at least as wide as the icon and 60 % of the cell. The
/// cell's side margins stay free for reordering. (Live 2026-10-15: icon only missed drops on the
/// name; Explorer targets icon and label.)
fn on_icon(in_cell: (i32, i32), cell: (i32, i32), icon: i32, dpi: u32) -> bool {
    let left = (cell.0 - icon) / 2;
    let top = crate::geometry::to_px(4, dpi);
    let on_icon = in_cell.0 >= left && in_cell.0 < left + icon && in_cell.1 >= top && in_cell.1 < top + icon;
    let strip = icon.max(cell.0 * 3 / 5).min(cell.0);
    let (label_left, label_top) = ((cell.0 - strip) / 2, top + icon + crate::geometry::to_px(4, dpi));
    let label_bottom = (label_top + crate::geometry::to_px(32, dpi)).min(cell.1);
    let on_label = in_cell.0 >= label_left && in_cell.0 < label_left + strip && in_cell.1 >= label_top && in_cell.1 < label_bottom;
    on_icon || on_label
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_icon_of_a_cell_is_a_drop_onto_target() {
        // 96 DPI, 110x108 cell, 48 px icon: icon spans x 31..79, y 4..52
        assert!(on_icon((55, 28), (110, 108), 48, 96), "icon centre");
        assert!(on_icon((31, 4), (110, 108), 48, 96) && on_icon((78, 51), (110, 108), 48, 96), "icon corners");
        assert!(!on_icon((30, 28), (110, 108), 48, 96), "left of the icon: reorder, not a hand-over");
        assert!(!on_icon((55, 2), (110, 108), 48, 96), "above the icon");
        // label strip: 66 px wide (x 22..88), y 56..88
        assert!(on_icon((55, 60), (110, 108), 48, 96) && on_icon((23, 70), (110, 108), 48, 96), "the name counts");
        assert!(!on_icon((10, 70), (110, 108), 48, 96), "the side margin beside the name repositions");
        assert!(!on_icon((55, 95), (110, 108), 48, 96), "below two label lines");
        assert!(!on_icon((5, 100), (110, 108), 48, 96), "cell corner");
        // 250 %: 4 DIP = 10 px
        assert!(!on_icon((137, 9), (275, 270), 133, 240) && on_icon((137, 10), (275, 270), 133, 240));
    }

    #[test]
    fn item_target_excludes_every_source_parent_and_descendant() {
        let sources = vec![r"C:\Desktop\a.txt".into(), r"C:\Desktop\Folder".into()];
        for target in [r"C:\Desktop\a.txt", r"c:/desktop/A.TXT/", r"C:\Desktop", r"C:\Desktop\Folder", r"C:\Desktop\Folder\child"] {
            assert!(!dragdrop::item_target_eligible(target, &sources), "{target}");
        }
        for target in [r"C:\Desktop\Other", r"C:\Desktop\Folder2", "::{645FF040-5081-101B-9F08-00AA002F954E}"] {
            assert!(dragdrop::item_target_eligible(target, &sources), "{target}");
        }
        assert!(!dragdrop::item_target_eligible("", &sources));
        assert!(!dragdrop::item_target_eligible(r"C:\Other", &[]));
        assert!(!dragdrop::item_target_eligible(r"C:\Other", &[String::new()]));
    }

    #[test]
    fn internal_explorer_and_portal_sources_prefer_item_then_restore_background_route() {
        // Portal sources and Explorer sources both arrive as external OLE data objects;
        // only begin_drag sets the internal marker. No drag kind bypasses the item route.
        for (kind, internal) in [("internal", true), ("Explorer", false), ("portal", false)] {
            let target = Some(r"C:\Desktop\folder".to_string());
            assert_eq!(drop_route(internal, target), DropHint::ShellTo(Destination::Item(r"C:\Desktop\folder".into())), "{kind}");
            assert_eq!(drop_route(internal, None), if internal { DropHint::Internal(DROPEFFECT_MOVE) } else { DropHint::Shell });
        }
    }

    #[test]
    fn item_handover_never_requests_positional_or_pending_placement() {
        let item = Destination::Item("bin".into());
        // Independent of negotiated/returned effect: shell success, optimized MOVE -> NONE,
        // cancelled menu or error all use the same reload-only disposition.
        assert!(!positional_drop(Some(&item)));
        assert!(positional_drop(None));
        assert!(positional_drop(Some(&Destination::Desktop)));
        assert!(positional_drop(Some(&Destination::Folder("scratch".into()))));
    }

    #[test]
    fn isolated_preview_has_no_desktop_fallback_even_with_invalid_fixture() {
        assert_eq!(default_drop_destination(true, None), Destination::Folder(String::new()));
        assert_eq!(default_drop_destination(true, Some("scratch".into())), Destination::Folder("scratch".into()));
        assert_eq!(default_drop_destination(false, None), Destination::Desktop);
    }

    #[test]
    fn portal_frame_and_active_inactive_tab_current_folder_resolver() {
        let members = [
            FolderMember { id: 10, root: Some("root-a"), current: Some("root-a/nav") },
            FolderMember { id: 20, root: Some("root-b"), current: Some("root-b/nav") },
            FolderMember { id: 30, root: None, current: None },
        ];
        assert_eq!(resolve_folder(Some(10), None, &members), Some((10, "root-a/nav", false)));
        assert_eq!(resolve_folder(Some(10), Some(0), &members), Some((10, "root-a/nav", false)));
        assert_eq!(resolve_folder(Some(10), Some(1), &members), Some((20, "root-b/nav", false)));
        assert_eq!(resolve_folder(Some(20), Some(0), &members), Some((10, "root-a/nav", false)));
        assert!(resolve_folder(Some(10), Some(2), &members).is_none());
        assert!(resolve_folder(None, Some(0), &members).is_none());
        assert!(resolve_folder(Some(999), Some(0), &members).is_none());
        // an ordinary member's label in a set whose visible member is a portal: desktop route
        let mixed = [
            FolderMember { id: 10, root: Some("root-a"), current: Some("root-a/nav") },
            FolderMember { id: 30, root: None, current: None },
        ];
        assert_eq!(frame_route(10, Some(1), &mixed), FrameRoute::Own);
        assert_eq!(frame_route(10, Some(0), &mixed), FrameRoute::Folder { member: 10, path: "root-a/nav", fallback: false });
        assert_eq!(frame_route(30, None, &mixed), FrameRoute::Content);
        assert_eq!(frame_route(10, None, &[FolderMember { id: 10, root: Some("r"), current: None }]),
            FrameRoute::Folder { member: 10, path: "r", fallback: true });
        assert!(resolve_folder(Some(10), Some(3), &members).is_none());
    }

    #[test]
    fn portal_resolver_missing_view_falls_back_only_to_own_root() {
        for current in [None, Some(""), Some("root")] {
            let members = [FolderMember { id: 1, root: Some("root"), current }];
            assert_eq!(resolve_folder(Some(1), None, &members), Some((1, "root", current != Some("root"))));
        }
    }

    #[test]
    fn tab_drop_selection_requires_release_and_no_conflicting_dwell() {
        assert!(select_drop_tab(20, 10, 0, false, None));
        assert!(select_drop_tab(20, 10, 0x20, false, Some(20)));
        for (keys, buttons, pending) in [(1, false, None), (2, false, None), (0, true, None), (0, false, Some(30))] {
            assert!(!select_drop_tab(20, 10, keys, buttons, pending));
        }
        assert!(!select_drop_tab(10, 10, 0, false, None));
    }

    #[test]
    fn deferred_creation_and_selection_are_consumed_exactly_once() {
        let mut queue = vec![DeferredDrop::CreatePortal { paths: vec!["scratch".into()], screen: (10, 20) }, DeferredDrop::SelectTab(5)];
        let work = take_deferred(&mut queue);
        assert_eq!(work.len(), 2);
        assert_eq!(work[0], DeferredDrop::CreatePortal { paths: vec!["scratch".into()], screen: (10, 20) });
        assert!(take_deferred(&mut queue).is_empty());
    }

    const OWN: u32 = 100;

    #[test]
    fn icon_drag_stays_internal_over_corral_windows() {
        // a group/surface/preview window of this process
        let hit = classify_drag_hit(Some(OWN), OWN, false);
        assert_eq!(hit, DragHit::Own);
        assert!(!hands_over_to_ole(hit));
        // no window under the cursor: keep the internal drag
        assert!(!hands_over_to_ole(classify_drag_hit(None, OWN, false)));
        // the drag image window itself is never a drop site, whatever process it reports
        assert!(!hands_over_to_ole(classify_drag_hit(Some(OWN), OWN, true)));
        assert!(!hands_over_to_ole(classify_drag_hit(Some(7), OWN, true)));
    }

    #[test]
    fn icon_drag_becomes_ole_over_foreign_windows_and_portals() {
        // Explorer / any other application
        let foreign = classify_drag_hit(Some(7), OWN, false);
        assert_eq!(foreign, DragHit::Foreign);
        assert!(hands_over_to_ole(foreign));
        assert!(hands_over_to_ole(DragHit::Portal)); // a Gate tab label
        assert!(hands_over_to_ole(DragHit::Item));
    }
}
