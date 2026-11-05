//! Desktop items: loading, item and background context menus, verbs, in-place rename.

use crate::geometry::{Rect, to_px};
use crate::model::GroupContent;
use crate::shell;
use crate::shell_menu::ShellMenu;
use crate::style;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::rc::Rc;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateFontW, DeleteObject, HGDIOBJ};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::Shell::SetWindowSubclass;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};
use super::{App, EM_SETSEL, Item, log, scale};
use super::input::naming_proc;
use super::selection;

/// Corral's own entries appended to the desktop background menu (above the shell id range).
const MENU_REFRESH: u32 = 0x9001;
const MENU_CONFIGURE: u32 = 0x9002;
const MENU_EXIT: u32 = 0x9003;
const MENU_PASTE: u32 = 0x9004;
const MENU_ROUND_UP_DECIDE: u32 = 0x9005;
const MENU_PASTE_LINK: u32 = 0x9006;

pub(super) const GATE_NEW_RENAME_TIMER: usize = 0x4780_0000;
const GATE_NEW_RENAME_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

pub(super) struct GateNewRename {
    gate: u64,
    folder: String,
    probe_seq: u64,
    before: Vec<String>,
    at: std::time::Instant,
    /// Wall-clock start of the command: the new item must have been created at or after it.
    started: std::time::SystemTime,
    retry: bool,
}

/// shortcut: the Shell reports no identity for what New created, so the one new listing entry is
/// accepted when its creation time is not before the command; another program creating a single file
/// after the command and before the listing is still taken for New's, upgrade to an
/// IObjectWithSite identity from the invoked IContextMenu if that is ever seen live.
fn created_since(created: std::time::SystemTime, started: std::time::SystemTime) -> bool {
    // file times come from a coarse system clock that can lag `SystemTime::now` by a few ms
    created + std::time::Duration::from_millis(250) >= started
}

#[derive(Debug, PartialEq, Eq)]
enum NewName<'a> { Wait, Rename(&'a str), Cancel }

/// A complete listing must add exactly one identity; removals do not make it ambiguous.
fn gate_new_name<'a>(before: &[String], after: &'a [String], elapsed: std::time::Duration) -> NewName<'a> {
    if elapsed >= GATE_NEW_RENAME_LIMIT { return NewName::Cancel; }
    let known: HashSet<_> = before.iter().map(|name| name.to_lowercase()).collect();
    let mut new = after.iter().filter(|name| !known.contains(&name.to_lowercase()));
    match (new.next(), new.next()) {
        (None, _) => NewName::Wait,
        (Some(name), None) => NewName::Rename(name),
        _ => NewName::Cancel,
    }
}

/// The clipboard holds files or shell items (what the desktop's Paste accepts).
fn clipboard_has_files() -> bool {
    use windows::Win32::System::DataExchange::{IsClipboardFormatAvailable, RegisterClipboardFormatW};
    unsafe {
        IsClipboardFormatAvailable(15 /* CF_HDROP */).is_ok()
            || IsClipboardFormatAvailable(RegisterClipboardFormatW(w!("Shell IDList Array"))).is_ok()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct RasterKey {
    dpi: u32,
    icon: crate::icon_overlay::CacheKey,
}

impl RasterKey {
    fn new(name: &str, dpi: u32, size: u32, policy: crate::icon_overlay::Policy) -> Self {
        Self { dpi, icon: crate::icon_overlay::CacheKey::new(name, size, policy) }
    }
}

/// UI-thread cache of owned pixel buffers, scoped to Corral's surface.
/// No HWND/COM/image-list resource is retained. A failed request is cached too,
/// so a repaint/drag cannot retry a failing Shell extension on every frame.
struct RasterCache<T> {
    owner: isize,
    entries: HashMap<RasterKey, Option<Rc<T>>>,
    demand_fingerprint: Option<u64>,
}

impl<T> RasterCache<T> {
    fn new() -> Self { Self { owner: 0, entries: HashMap::new(), demand_fingerprint: None } }

    fn demand_current(&self, owner: isize, fingerprint: u64) -> bool {
        self.owner == owner && self.demand_fingerprint == Some(fingerprint)
    }

    fn retain(&mut self, owner: isize, wanted: &HashSet<RasterKey>) {
        if self.owner != owner { self.reset(owner); }
        self.entries.retain(|key, _| wanted.contains(key));
    }

    fn reset(&mut self, owner: isize) {
        self.owner = owner;
        self.entries.clear();
        self.demand_fingerprint = None;
    }
}

/// Hash borrowed inputs, without resolving each member against the whole item list or allocating
/// case-folded RasterKeys. Effective DPI/size/policy cover all their writers (including zoom/views).
/// All tabs and Ranges retain demand, so visibility alone intentionally does not invalidate it.
fn raster_demand_fingerprint<'a>(
    groups: impl IntoIterator<Item = (u64, u32, u32, crate::icon_overlay::Policy, &'a GroupContent)>,
    items: impl IntoIterator<Item = (&'a str, Option<u64>)>,
) -> u64 {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    let mut count = 0usize;
    for (id, dpi, size, policy, content) in groups {
        (id, dpi, size, policy).hash(&mut hash);
        match content {
            GroupContent::Items { items } => { 0u8.hash(&mut hash); items.hash(&mut hash); }
            GroupContent::Portal { .. } => { 1u8.hash(&mut hash); }
        }
        count += 1;
    }
    count.hash(&mut hash);
    for item in items { item.hash(&mut hash); }
    hash.finish()
}

thread_local! {
    // Kept here to avoid changing the App/Item fields. The app has
    // one UI thread; item reload resets it and render prunes unused identities.
    static GROUP_RASTERS: RefCell<RasterCache<shell::IconBitmap>> = RefCell::new(RasterCache::new());
}

fn cached_group_raster(key: RasterKey, request: impl FnOnce() -> Option<shell::IconBitmap>) -> Option<Rc<shell::IconBitmap>> {
    let cached = GROUP_RASTERS.with(|cache| cache.borrow().entries.get(&key).cloned());
    if let Some(bitmap) = cached { return bitmap; }
    // Do not hold a RefCell borrow across Shell COM calls (which may pump messages).
    let bitmap = request().map(Rc::new);
    GROUP_RASTERS.with(|cache| { cache.borrow_mut().entries.insert(key, bitmap.clone()); });
    bitmap
}

impl App {
    pub(super) fn qa_fixture_folder(&self) -> Option<std::path::PathBuf> {
        if self.opts.mode != super::Mode::Preview { return None; }
        std::env::var_os("CORRAL_QA_ITEMS_DIR").and_then(|p| std::path::PathBuf::from(p).canonicalize().ok()).filter(|p| {
            std::env::current_dir().ok().and_then(|d| d.join("analysis").join("qa").canonicalize().ok()).is_some_and(|root| p.starts_with(root))
        }).map(|path| {
            // Shell namespace parsing does not enumerate extended-length filesystem spellings.
            // Canonical containment is already checked; pass the equivalent ordinary local path.
            let value = path.to_string_lossy();
            std::path::PathBuf::from(value.strip_prefix(r"\\?\").unwrap_or(&value))
        })
    }

    fn enumerate_app_items(&self) -> Vec<shell::ShellItem> {
        match self.qa_fixture_folder() {
            Some(path) => shell::enumerate_folder(path.to_str().unwrap_or_default()).unwrap_or_default(),
            None if self.opts.mode == super::Mode::Preview && std::env::var_os("CORRAL_QA_ITEMS_DIR").is_some() => Vec::new(),
            None => shell::enumerate_desktop().unwrap_or_default(),
        }
    }

    /// Visible desktop items with Explorer's current positions (read-only, via the shell view).
    /// Items Explorer does not show (namespace roots) are excluded.
    pub(super) fn load_items(&mut self) {
        // Preview fixtures replace the Desktop namespace, so tests can never hit user icons.
        let fixture = self.qa_fixture_folder();
        let fixture_requested = self.opts.mode == super::Mode::Preview && std::env::var_os("CORRAL_QA_ITEMS_DIR").is_some();
        let all = self.enumerate_app_items();
        if let Some(path) = &fixture { log(&format!("qa items: directory={} count={}", path.display(), all.len())); }
        else if fixture_requested { log("qa items: rejected fixture path; no Desktop items loaded"); }
        let view = if fixture_requested { self.icon_px = 32; self.spacing = (96, 104); None } else { crate::desktop_view::read_desktop_view().ok() };
        let dpi = self.dpi();
        if let Some(view) = &view {
            // IFolderView2 reports the icon size in logical (96-DPI) units; spacing is physical.
            self.icon_px = scale(view.icon_size as i32, dpi);
            self.spacing = view.spacing;
        }
        let icon_px = self.icon_px.max(16) as u32;
        let listview_origin = self.listview_origin();
        // a reload (shell change, verb) keeps the selection of the items that are still there
        let selected = self.selected_keys();
        let mut items = Vec::new();
        for shell_item in all {
            let position = match &view {
                Some(v) => match v.icons.iter().find(|i| i.parsing_name.eq_ignore_ascii_case(&shell_item.parsing_name)) {
                    Some(icon) => Some(icon.position),
                    None => continue, // not shown on the desktop
                },
                None => None,
            };
            let own = self.loose_client_position(&shell_item.parsing_name);
            let pos = own.or(position.map(|(x, y)| (x + listview_origin.0, y + listview_origin.1))).unwrap_or((0, 0));
            let position = own.or(position);
            let icon = shell::icon_for_policy(&shell_item, icon_px, crate::icon_overlay::Policy(self.settings.hide_shortcut_overlays)).ok();
            let selected = selected.contains(&shell_item.parsing_name.to_lowercase());
            items.push(Item { details: Default::default(), shell: shell_item, icon, pos, placed: position.is_some(), selected, gate: None });
        }
        // Keep Gate listings while local folders re-read in the background. Network Gates refresh
        // after their probe, as before, rather than on every desktop notification.
        let desktop: HashSet<String> = items.iter().map(|i| i.shell.parsing_name.to_lowercase()).collect();
        // A Gate whose first read is still out is read again too: the newer read supersedes it (a change
        // during that read must not leave its stale listing).
        let gates: Vec<u64> = self.portals.iter().filter(|(_, v)| v.error.is_none() && !v.current.starts_with("\\\\"))
            .map(|(id, _)| *id).collect();
        for item in std::mem::take(&mut self.items) {
            if item.gate.is_some_and(|g| self.portals.contains_key(&g)) && !desktop.contains(&item.shell.parsing_name.to_lowercase()) { items.push(item); }
        }
        self.items = items;
        self.load_details();
        for id in gates { self.gate_items(id, desktop.clone()); }
        self.index_gate_items();
        GROUP_RASTERS.with(|cache| cache.borrow_mut().reset(self.surface_hwnd.0 as isize));
        self.refresh_icon_metrics();
        let present = |name: &Option<String>| name.as_ref().is_some_and(|n| self.items.iter().any(|i| i.shell.parsing_name.eq_ignore_ascii_case(n)));
        let (anchor_gone, focus_gone) = (!present(&self.selection.anchor), !present(&self.selection.focus));
        if anchor_gone {
            self.selection.anchor = None;
        }
        if focus_gone {
            self.selection.focus = None;
        }
        let _ = dpi;
        self.claim_menu_new_items();
        self.run_rules();
        self.apply_pending_drops();
        self.arrange_loose();
    }

    /// Settings reload already ends in refresh_all. Refresh only stale bitmap identities here,
    /// without enumerating files, running rules/drops or recreating the portal Shell views.
    pub(super) fn refresh_item_icon_policy(&mut self) {
        let policy = crate::icon_overlay::Policy(self.settings.hide_shortcut_overlays);
        let size = self.icon_px.max(16) as u32;
        for item in &mut self.items {
            let key = crate::icon_overlay::CacheKey::new(&item.shell.parsing_name, size, policy);
            if item.icon.as_ref().and_then(|i| i.cache_key.as_ref()) != Some(&key) {
                item.icon = shell::icon_for_policy(&item.shell, size, policy).ok();
            }
        }
        self.retain_group_icon_rasters();
    }

    /// Demand includes inactive tabs/Ranges and current Gate listings.
    /// Dropping the last member/group at a DPI releases those buffers.
    /// This never enumerates the Shell or runs rules/file-transfer operations.
    pub(super) fn retain_group_icon_rasters(&self) {
        let owner = self.surface_hwnd.0 as isize;
        let fingerprint = raster_demand_fingerprint(self.layout.groups.iter().map(|group| {
            let dpi = self.group_metrics_dpi(group.id);
            (group.id, dpi, self.group_icon_px_of(group.id, dpi), self.icon_policy_of(group.id), &group.content)
        }), self.items.iter().map(|item| (item.shell.parsing_name.as_str(), item.gate)));
        if GROUP_RASTERS.with(|cache| cache.borrow().demand_current(owner, fingerprint)) { return; }
        let wanted: HashSet<_> = self.layout.groups.iter().flat_map(|group| {
            let policy = self.icon_policy_of(group.id);
            let dpi = self.group_metrics_dpi(group.id);
            let size = self.group_icon_px_of(group.id, dpi);
            self.group_members(group).into_iter().map(move |item| RasterKey::new(&item.shell.parsing_name, dpi, size, policy))
        }).collect();
        GROUP_RASTERS.with(|cache| {
            let mut cache = cache.borrow_mut();
            cache.retain(owner, &wanted);
            cache.demand_fingerprint = Some(fingerprint);
        });
    }

    pub(super) fn group_icon_bitmap(&self, item: &Item, dpi: u32, group_id: u64) -> Option<Rc<shell::IconBitmap>> {
        let size = self.group_icon_px_of(group_id, dpi);
        let policy = self.icon_policy_of(group_id);
        let key = RasterKey::new(&item.shell.parsing_name, dpi, size, policy);
        cached_group_raster(key, || shell::icon_for_policy(&item.shell, size, policy).ok())
    }

    /// Show a shell item menu; "rename" is not delegated to the shell (Corral renders items).
    pub(super) fn show_item_menu(&mut self, owner: HWND, mut names: Vec<String>, client_point: (i32, i32)) {
        let covered = self.covered_loose_set();
        names.retain(|n| self.name_actionable_in(n, &covered));
        if names.is_empty() {
            return;
        }
        // QA aid: which items the menu was built for (multi-item menus are hard to tell apart on screen)
        let shown: Vec<&str> = names.iter().take(4).map(|n| n.rsplit('\\').next().unwrap_or(n)).collect();
        log(&format!("item menu: {} item(s) {shown:?}{}", names.len(), if names.len() > 4 { " ..." } else { "" }));
        let menu = match ShellMenu::for_items(owner, &names) {
            Ok(menu) => Rc::new(menu),
            Err(e) => {
                log(&format!("item menu failed: {e}"));
                return;
            }
        };
        // Icons the user placed by hand can be handed back to Round-up (round-up-spec 3).
        let any_manual = names.iter().any(|n| self.layout.is_manual(n));
        if any_manual {
            unsafe {
                let _ = AppendMenuW(menu.hmenu(), MF_SEPARATOR, 0, PCWSTR::null());
                let _ = AppendMenuW(menu.hmenu(), MF_STRING, MENU_ROUND_UP_DECIDE as usize, w!("Let Round-up decide"));
            }
        }
        let screen = self.to_screen(client_point);
        if let Some(id) = Self::track(&menu, owner, screen) {
            // The modal menu pumps messages: geometry can change while it is open.
            let covered = self.covered_loose_set();
            if names.iter().any(|n| !self.name_actionable_in(n, &covered)) { return; }
            if any_manual && id == MENU_ROUND_UP_DECIDE {
                self.let_round_up_decide(&names);
                return;
            }
            if menu.is_rename(id) {
                if let [name] = names.as_slice() {
                    let home = self.groups.iter().find(|g| g.hwnd == owner).map(|g| g.id);
                    self.begin_rename(name.clone(), home);
                }
                return;
            } else if let Err(e) = menu.invoke(owner, id, screen, false, false) {
                log(&format!("invoke failed: {e}"));
            }
            self.schedule_item_reload();
        }
    }

    /// Enter on a selection: open every item, after the same question Explorer asks before it opens
    /// many items at once (more than `MultipleInvokePromptMinimum`, default 15). Returns whether
    /// the items were opened.
    pub(super) fn open_items(&mut self, names: &[String]) -> bool {
        let minimum = multiple_invoke_prompt_minimum();
        if selection::needs_open_confirmation(names.len(), minimum) {
            // Wording of Explorer's dialog.
            let text = HSTRING::from(format!("Are you sure you want to open these {} items?", names.len()));
            let answer = qa_answer().unwrap_or_else(|| {
                if crate::dialog::confirm(self.surface_hwnd,crate::dialog::system_light(),"Open selected items?",&text.to_string(),"Open",false){IDYES}else{IDNO}
            });
            if answer != IDYES {
                log(&format!("open: {} item(s) not opened (declined)", names.len()));
                return false;
            }
        }
        for name in names {
            if self.name_actionable(name) { let _ = shell::open_item(name); }
        }
        true
    }

    /// Run a shell verb (e.g. "delete" -> Recycle Bin, undoable) on items via their context menu.
    /// One menu serves items of one parent folder only, so a selection that spans the user and the
    /// public Desktop runs the verb once per folder.
    pub(super) fn invoke_verb(&mut self, names: &[String], verb: &str) {
        let sets = selection::split_by_parent(names);
        if sets.len() > 1 {
            log(&format!("verb {verb}: selection spans {} folders, one invocation each", sets.len()));
        }
        for set in sets {
            self.invoke_verb_on(&set, verb);
        }
    }

    fn invoke_verb_on(&mut self, names: &[String], verb: &str) {
        if names.is_empty() { return; }
        let covered = self.covered_loose_set();
        if names.iter().any(|n| !self.name_actionable_in(n, &covered)) {
            return;
        }
        let Ok(menu) = ShellMenu::for_items(self.surface_hwnd, names) else { return };
        let hmenu = menu.hmenu();
        let count = unsafe { GetMenuItemCount(hmenu) };
        for position in 0..count.max(0) {
            let id = unsafe { GetMenuItemID(hmenu, position) };
            if id != u32::MAX && menu.verb(id).is_some_and(|v| v.eq_ignore_ascii_case(verb)) {
                if let Err(e) = menu.invoke(self.surface_hwnd, id, (0, 0), false, false) {
                    log(&format!("verb {verb} failed: {e}"));
                }
                self.schedule_item_reload();
                return;
            }
        }
        log(&format!("verb {verb} not offered for {names:?}"));
    }

    /// In-place rename edit over a loose item's or Gate item's label (Explorer-style F2 / menu Rename).
    /// `home` is the container the request came from; two Gates may show the same folder, so a Gate
    /// item is only taken from that Gate.
    pub(super) fn begin_rename(&mut self, name: String, home: Option<u64>) {
        self.cancel_gate_new_rename();
        if self.renaming.is_some() || self.naming.is_some() { return; }
        if !self.name_actionable(&name) { return; }
        let gate = home.filter(|id| self.portals.contains_key(id));
        // The window the edit opens in: the Gate the input came from, or the Corral holding the item.
        let container = gate.or_else(|| self.layout.group_of(&name));
        if let Some(id) = container {
            let Some(g) = self.layout.groups.iter().find(|g| g.id == id) else { return };
            if self.body_hidden(id) || self.qh_group_hidden(id) || !crate::tabs::body_visible(&self.layout.groups, g) || !self.page_layer_visible(g) { return; }
            let Some(slot) = self.group_members(g).iter().position(|i| i.shell.parsing_name.eq_ignore_ascii_case(&name)) else { return };
            let before = g.scroll;
            self.scroll_item_into_view(id, slot);
            // the edit opens over what is painted, so show the scrolled body first
            if self.layout.groups.iter().find(|g| g.id == id).is_some_and(|g| g.scroll != before) {
                if let Some(index) = self.groups.iter().position(|g| g.id == id) { self.render_group(index); }
            }
        }
        let Some(item) = self.items.iter().find(|i| i.gate == gate && i.shell.parsing_name.eq_ignore_ascii_case(&name)) else { return };
        let (cw, _) = self.loose_cell();
        let dpi = container.map_or_else(|| self.dpi(), |id| self.group_dpi(id));
        let height = to_px(22, dpi);
        let (rect, owner) = if let Some(id) = container {
            let Some(g) = self.layout.groups.iter().find(|g| g.id == id) else { return };
            let Some(gw) = self.groups.iter().find(|g| g.id == id) else { return };
            let Some(slot) = self.group_members(g).iter().position(|i| i.shell.parsing_name.eq_ignore_ascii_case(&name)) else { return };
            let grid = self.group_grid(id, dpi, gw.body, g.scroll);
            let label = grid.slot(slot).label;
            (Rect::new(label.left, label.top, label.right, (label.top + height).min(grid.body.bottom))
                .offset(gw.rect.left, gw.rect.top), gw.hwnd)
        } else {
            let top = item.pos.1 + self.icon_px + to_px(8, dpi);
            (Rect::new(item.pos.0, top, item.pos.0 + cw, top + height), self.surface_hwnd)
        };
        if rect.width() <= 0 || rect.height() <= 0 { return; }
        let text = item.shell.display_name.clone();
        unsafe {
            let font = CreateFontW(-(style::LABEL_FONT_PX as i32 * dpi as i32 / 96), 0, 0, 0, 400, 0, 0, 0, 0, 0, 0, 5, 0, &HSTRING::from(style::FONT));
            let Ok(edit) = self.create_edit(&text, WINDOW_STYLE(0x0001 | 0x0080), rect, owner)
            else {
                return;
            };
            SendMessageW(edit, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
            let end = rename_selection_end(&name, &text, item.shell.is_folder).map_or(-1, |n| n as isize);
            SendMessageW(edit, EM_SETSEL, WPARAM(0), LPARAM(end));
            let _ = SetWindowSubclass(edit, Some(naming_proc), 2, 0);
            let (z, z_extra) = self.peek_edit_zorder(); // HWND_TOP unless Peek is on
            let _ = SetWindowPos(edit, z, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | z_extra);
            let _ = SetFocus(edit);
            self.renaming = Some((edit, name, font, gate, text, item.shell.is_folder));
        }
    }

    /// Commit an item rename through IFileOperation (shell semantics: undo, conflicts, extension
    /// handling) and move Corral's references to the item's new parsing name.
    pub(super) fn finish_rename(&mut self, commit: bool) {
        let Some((edit, old_name, font, gate, old_display, is_folder)) = self.renaming.take() else { return };
        let mut buffer = [0u16; 260];
        let len = unsafe { GetWindowTextW(edit, &mut buffer) };
        let new_text = String::from_utf16_lossy(&buffer[..len.max(0) as usize]).trim().to_string();
        unsafe {
            let _ = DestroyWindow(edit);
            let _ = DeleteObject(HGDIOBJ(font.0));
        }
        if !commit || !self.name_actionable(&old_name) || new_text.is_empty() || old_display == new_text {
            return;
        }
        let before: Vec<String> = self.items.iter().map(|i| i.shell.parsing_name.to_lowercase()).collect();
        let target = rename_target(&old_name, &old_display, &new_text, is_folder);
        if let Err(e) = rename_with_shell(self.surface_hwnd, &old_name, &target) {
            log(&format!("rename failed: {e}"));
            return;
        }
        if let Some(id) = gate {
            self.reload_gate(id);
            return; // Gate items have no Desktop layout references to move.
        }
        // Find the renamed item (new parsing name, same folder) and carry the references over.
        let after = self.enumerate_app_items();
        let after_names: Vec<String> = after.iter().map(|i| i.parsing_name.clone()).collect();
        if let Some(renamed) = renamed_parsing_name(&old_name, &target, &before, &after_names) {
            let group = self.group_of(&old_name);
            let loose = self.layout.loose_position(&old_name).cloned();
            let index = group.and_then(|id| self.layout.groups.iter().find(|g| g.id == id)).and_then(|g| match &g.content {
                GroupContent::Items { items } => items.iter().position(|i| i.eq_ignore_ascii_case(&old_name)),
                GroupContent::Portal { .. } => None,
            });
            // drop the old spelling first: a case-only rename shares its key with the new one
            self.layout.assign_item(&old_name, None);
            self.layout.loose.remove(&old_name.to_lowercase());
            if let Some(id) = group {
                self.layout.insert_item(&renamed, id, index.unwrap_or(usize::MAX));
            } else if let Some(position) = loose {
                self.layout.place_loose(&renamed, position);
            }
            if group.is_none() { self.layout.rename_icon_page(&old_name, &renamed); }
            else { self.layout.set_icon_page(&old_name, 0); }
            // renamed here, in Corral: the new name keeps the old name's Round-up standing
            self.carry_round_up_state(&old_name, &renamed);
            self.record_desk_edit(); // the loose position moved to the new name (spec 3.2)
            self.save();
        }
        self.load_items();
        self.refresh_all();
    }

    /// Desktop background menu: the shell's folder menu plus Corral's own entries. A Gate's body gets its own menu.
    /// `target`: the Corral whose body was right-clicked; what the chosen command creates goes there.
    pub(super) fn show_background_menu(&mut self, client_point: (i32, i32), target: Option<u64>) {
        if let Some(id) = target.filter(|id| self.layout.groups.iter().any(|g| g.id == *id && matches!(g.content, GroupContent::Portal { .. }))) {
            self.show_gate_background_menu(id, client_point);
            return;
        }
        let owner = self.surface_hwnd;
        let menu = match ShellMenu::for_background(owner, None) {
            Ok(menu) => Rc::new(menu),
            Err(e) => {
                log(&format!("background menu failed: {e}"));
                return;
            }
        };
        unsafe {
            let hmenu = menu.hmenu();
            let _ = InsertMenuW(hmenu, 0, MF_BYPOSITION | MF_STRING, MENU_REFRESH as usize, w!("Refresh"));
            // Paste is a view entry (the folder's own background menu lacks it); greyed without files
            let paste_flags = if clipboard_has_files() { MF_STRING } else { MF_STRING | MF_GRAYED };
            let _ = InsertMenuW(hmenu, 1, MF_BYPOSITION | MF_SEPARATOR, 0, PCWSTR::null());
            let _ = InsertMenuW(hmenu, 2, MF_BYPOSITION | paste_flags, MENU_PASTE as usize, w!("Paste"));
            let _ = InsertMenuW(hmenu, 3, MF_BYPOSITION | MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(hmenu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(hmenu, MF_STRING, MENU_CONFIGURE as usize, w!("Configure Corral"));
            crate::app_icon::set_menu_item_icon(hmenu, MENU_CONFIGURE as u32, windows::Win32::UI::HiDpi::GetDpiForWindow(owner));
            let _ = AppendMenuW(hmenu, MF_STRING, MENU_EXIT as usize, w!("Exit Corral"));
        }
        let screen = self.to_screen(client_point);
        match Self::track(&menu, owner, screen) {
            Some(MENU_REFRESH) => self.schedule_item_reload(),
            Some(MENU_PASTE) => self.paste_files(target, Some(screen)),
            Some(MENU_CONFIGURE) => self.open_settings_window(),
            Some(MENU_EXIT) => unsafe { PostQuitMessage(0) },
            Some(id) => {
                if let Some(group) = target {
                    let known = self.items.iter().map(|i| i.shell.parsing_name.to_lowercase()).collect();
                    self.menu_new_target = Some((group, known, std::time::Instant::now()));
                }
                if let Err(e) = menu.invoke(owner, id, screen, false, false) {
                    log(&format!("invoke failed: {e}"));
                }
                self.schedule_item_reload();
            }
            None => {}
        }
    }

    fn show_gate_background_menu(&mut self, id: u64, client_point: (i32, i32)) {
        self.cancel_gate_new_rename();
        let Some(folder) = self.portal_current_path(id).map(str::to_string) else { return };
        let owner = self.groups.iter().find(|g| g.id == id).map(|g| g.hwnd).unwrap_or(self.surface_hwnd);
        let (prefix, tab_members, start) = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let Some(tabs) = self.append_portal_title_menu(menu, id) else { let _ = DestroyMenu(menu); return };
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            (menu, tabs, GetMenuItemCount(menu).max(0) as u32)
        };
        let menu = match ShellMenu::for_gate_background(owner, &folder, prefix) {
            Ok(menu) => Rc::new(menu),
            Err(e) => {
                // the shell part failed: still offer the Gate menu itself
                log(&format!("Gate background menu failed: {e}"));
                self.show_portal_title_menu(id, self.to_screen(client_point));
                return;
            }
        };
        unsafe {
            let hmenu = menu.hmenu();
            let count = GetMenuItemCount(hmenu).max(0) as u32;
            let paste_at = (start..count).find(|position| {
                menu.verb(GetMenuItemID(hmenu, *position as i32)).is_some_and(|v| v.eq_ignore_ascii_case("properties"))
            }).unwrap_or(count);
            let flags = MF_BYPOSITION | MF_STRING | if clipboard_has_files() { MF_ENABLED } else { MF_GRAYED };
            let _ = InsertMenuW(hmenu, paste_at, flags, MENU_PASTE as usize, w!("Paste"));
            let link_flags = MF_BYPOSITION | MF_STRING | if crate::dragdrop::clipboard_has_link_items() { MF_ENABLED } else { MF_GRAYED };
            let _ = InsertMenuW(hmenu, paste_at + 1, link_flags, MENU_PASTE_LINK as usize, w!("Paste shortcut"));
        }
        let screen = self.to_screen(client_point);
        let Some(choice) = Self::track(&menu, owner, screen) else { return };
        if choice == MENU_PASTE {
            if let Err(e) = menu.invoke_verb(owner, "paste", screen) { log(&format!("Gate paste failed: {e}")); }
            self.schedule_item_reload();
        } else if choice == MENU_PASTE_LINK {
            // the folder's background menu accepts "pastelink" but does nothing (it is a view command in Explorer)
            match crate::dragdrop::paste_link_into(owner, &folder) {
                Ok((_, 0)) => {}
                Ok((created, failed)) => {
                    log(&format!("Gate paste shortcut: {created} created, {failed} failed"));
                    crate::dialog::notice(owner, crate::dialog::system_light(), "Some shortcuts weren't created",
                        &format!("Created {created} shortcut(s). {failed} item(s) couldn't have a shortcut here, for example a shortcut file or an item that no longer exists."));
                }
                Err(e) => {
                    log(&format!("Gate paste shortcut failed: {e}"));
                    crate::dialog::notice(owner, crate::dialog::system_light(), "Couldn't paste shortcuts",
                        "The clipboard has no usable files, or this folder couldn't create shortcuts to them. Copy the files again and try Paste shortcut.");
                }
            }
            self.schedule_item_reload();
        } else if (crate::shell_menu::GATE_BACKGROUND_FIRST_ID..=crate::shell_menu::GATE_BACKGROUND_LAST_ID).contains(&choice) {
            let pending = self.portals.get(&id).filter(|v| menu.is_gate_new(choice) && v.loaded && !v.probe_pending && v.error.is_none())
                .map(|v| GateNewRename {
                    gate: id, folder: folder.clone(), probe_seq: v.probe_seq,
                    before: self.items.iter().filter(|i| i.gate == Some(id)).map(|i| i.shell.parsing_name.clone()).collect(),
                    at: std::time::Instant::now(), started: std::time::SystemTime::now(), retry: false,
                });
            match menu.invoke(owner, choice, screen, false, false) {
                Ok(()) => {
                    self.gate_new_rename = pending;
                    if self.gate_new_rename.is_some() {
                        unsafe { let _ = SetTimer(self.message_window, GATE_NEW_RENAME_TIMER, 200, None); }
                        self.reload_gate(id);
                    }
                }
                Err(e) => log(&format!("Gate invoke failed: {e}")),
            }
            self.schedule_item_reload();
        } else {
            self.portal_menu_command(id, choice as usize, &tab_members);
        }
    }

    pub(super) fn cancel_gate_new_rename(&mut self) {
        if self.gate_new_rename.take().is_some() {
            unsafe { let _ = KillTimer(self.message_window, GATE_NEW_RENAME_TIMER); }
        }
    }

    /// Cancel before input can open another menu, start a drag/rename or switch tabs. Checking on
    /// every message also catches hidden/removed Gates and navigation before a later listing arrives.
    pub(super) fn check_gate_new_rename(&mut self, msg: u32) {
        let Some(pending) = &self.gate_new_rename else { return };
        // An incoming Explorer drag or another application's menu need not send us a button press.
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON};
        let button = |vk: i32| unsafe { GetAsyncKeyState(vk) } < 0;
        let mut gui = GUITHREADINFO { cbSize: std::mem::size_of::<GUITHREADINFO>() as u32, ..Default::default() };
        let menu = unsafe { GetGUIThreadInfo(0, &mut gui) }.is_ok() && gui.flags.0 & (GUI_INMENUMODE.0 | GUI_POPUPMENUMODE.0) != 0;
        let valid = pending.at.elapsed() < GATE_NEW_RENAME_LIMIT
            && self.portals.get(&pending.gate).is_some_and(|v| v.probe_seq == pending.probe_seq
                && !v.probe_pending && v.error.is_none() && crate::portal::same_folder(&v.current, &pending.folder))
            && self.layout.groups.iter().any(|g| g.id == pending.gate && !self.body_hidden(g.id)
                && crate::tabs::body_visible(&self.layout.groups, g) && self.page_layer_visible(g))
            && !self.qh_group_hidden(pending.gate)
            // Corral still has the foreground: a preview's Gates are child windows, so never compare with the Gate's own hwnd.
            && { let mut pid = 0u32; unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut pid)); } pid == std::process::id() }
            && matches!(self.drag, super::Drag::None) && self.ole_drag.is_none()
            && !button(VK_LBUTTON.0 as i32) && !button(VK_MBUTTON.0 as i32) && !button(VK_RBUTTON.0 as i32) && !menu
            && self.naming.is_none() && self.renaming.is_none();
        if !valid || matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_MBUTTONDOWN
            | WM_XBUTTONDOWN | WM_CONTEXTMENU | WM_KEYDOWN | WM_SYSKEYDOWN | WM_ENTERMENULOOP | WM_ENTERSIZEMOVE) {
            self.cancel_gate_new_rename();
        }
    }

    pub(super) fn poll_gate_new_rename(&mut self) {
        self.check_gate_new_rename(WM_TIMER);
        if let Some(pending) = &mut self.gate_new_rename {
            if pending.retry {
                pending.retry = false;
                let id = pending.gate;
                self.reload_gate(id);
            }
        }
    }

    pub(super) fn gate_new_rename_done(&mut self, id: u64) {
        self.check_gate_new_rename(0);
        let Some(pending) = &self.gate_new_rename else { return };
        if pending.gate != id { return; }
        let names: Vec<_> = self.items.iter().filter(|i| i.gate == Some(id)).map(|i| i.shell.parsing_name.clone()).collect();
        match gate_new_name(&pending.before, &names, pending.at.elapsed()) {
            NewName::Wait => self.gate_new_rename.as_mut().unwrap().retry = true,
            NewName::Cancel => self.cancel_gate_new_rename(),
            NewName::Rename(name) => {
                let name = name.to_string();
                let started = self.gate_new_rename.as_ref().map(|p| p.started);
                self.cancel_gate_new_rename();
                // no identity from the Shell: an entry older than the command is not New's
                let created = std::fs::metadata(&name).and_then(|m| m.created()).ok();
                if !started.zip(created).is_some_and(|(s, c)| created_since(c, s)) { return; }
                let before = self.selected_keys();
                for item in &mut self.items { item.selected = item.gate == Some(id) && item.shell.parsing_name == name; }
                self.selection.anchor = Some(name.clone());
                self.selection.focus = Some(name.clone());
                self.selection.release = None;
                self.begin_rename(name, Some(id));
                self.repaint_selection(&before, false, Some(id));
            }
        }
    }

    /// Paste the clipboard's files into the Desktop folder (the shell's own "paste" verb: copy/move,
    /// conflict prompts, progress and undo are Explorer's). With `target`, the pasted items go into
    /// that Corral.
    pub(super) fn paste_files(&mut self, target: Option<u64>, screen: Option<(i32, i32)>) {
        if !clipboard_has_files() {
            return;
        }
        let owner = self.surface_hwnd;
        // A Gate pastes into the folder it shows (corral-views-spec 2.3).
        if let Some(folder) = target.and_then(|g| self.portal_current_path(g)).map(str::to_string) {
            let point = screen.unwrap_or_default();
            match ShellMenu::for_background(owner, Some(&folder)).and_then(|menu| menu.invoke_verb(owner, "paste", point)) {
                Ok(()) => log(&format!("paste: into the Gate folder {folder:?}")),
                Err(e) => log(&format!("paste into {folder:?} failed: {e}")),
            }
            return;
        }
        // Corral extra (user decision 2026-10-15): Ctrl+V with exactly one folder selected pastes INTO
        // that folder (Explorer would paste beside it, onto the Desktop).
        let selected = self.selected_in_display_order();
        if screen.is_none() && selected.len() == 1 && std::path::Path::new(&selected[0]).is_dir() {
            let folder = selected[0].clone();
            let point = { let mut p = windows::Win32::Foundation::POINT::default();
                let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p) }; (p.x, p.y) };
            match ShellMenu::for_background(owner, Some(&folder)).and_then(|menu| menu.invoke_verb(owner, "paste", point)) {
                Ok(()) => log(&format!("paste: into the selected folder {folder:?}")),
                Err(e) => log(&format!("paste into {folder:?} failed: {e}")),
            }
            return;
        }
        let menu = match ShellMenu::for_background(owner, None) {
            Ok(menu) => menu,
            Err(e) => {
                log(&format!("paste: background menu failed: {e}"));
                return;
            }
        };
        if let Some(group) = target {
            let known = self.items.iter().map(|i| i.shell.parsing_name.to_lowercase()).collect();
            self.menu_new_target = Some((group, known, std::time::Instant::now()));
        }
        let point = screen.unwrap_or_else(|| {
            let mut p = windows::Win32::Foundation::POINT::default();
            let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut p) };
            (p.x, p.y)
        });
        match menu.invoke_verb(owner, "paste", point) {
            Ok(()) => log(&format!("paste: into {}", target.map_or("the desktop".to_string(), |g| format!("group {g}")))),
            Err(e) => log(&format!("paste failed: {e}")),
        }
        self.schedule_item_reload();
    }

    /// Items that appeared after a background-menu command in a Corral body (New, Paste) become pending
    /// placements into that Corral, before the rules see them; the first reload that finds any ends it.
    fn claim_menu_new_items(&mut self) {
        let Some((group, known, at)) = self.menu_new_target.take() else { return };
        if at.elapsed().as_secs() >= 15 || !self.layout.groups.iter().any(|g| g.id == group) {
            return;
        }
        let new: Vec<String> = self.items.iter().filter(|i| i.gate.is_none()).map(|i| i.shell.parsing_name.to_lowercase()).filter(|n| !known.contains(n)).collect();
        if new.is_empty() {
            self.menu_new_target = Some((group, known, at)); // not there yet: keep waiting
            return;
        }
        log(&format!("background menu in group {group}: {} new item(s) go into it", new.len()));
        let now = std::time::Instant::now();
        for name in new {
            self.pending_drops.insert(name, (super::PendingPlacement::Group(group), now));
        }
    }
}

/// `MultipleInvokePromptMinimum` of Explorer's policy key (read only), else Explorer's default.
fn multiple_invoke_prompt_minimum() -> u32 {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer"),
            w!("MultipleInvokePromptMinimum"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    if status.is_ok() { value } else { selection::DEFAULT_MULTIPLE_INVOKE_PROMPT_MINIMUM }
}

/// QA hook (automated trials only), same variable as the title menu's: `CORRAL_QA_MESSAGEBOX=yes|no`
/// answers the question without showing it.
fn qa_answer() -> Option<MESSAGEBOX_RESULT> {
    match std::env::var("CORRAL_QA_MESSAGEBOX").ok()?.to_ascii_lowercase().as_str() {
        "yes" => Some(IDYES),
        "no" => Some(IDNO),
        _ => None,
    }
}

/// The name to give the file when the user typed `typed` for an item shown as `display`. With extensions hidden
/// (including .lnk and .url, always hidden) the edit shows no extension and `RenameItem` takes its name literally,
/// so the old one is carried over, as Explorer does. A display name that is not file name minus extension
/// (a folder, a virtual item) leaves the typed text alone. A file whose display name is localized (not
/// its name minus extension) keeps its extension too, unless the display name shows it.
fn rename_target(parsing_name: &str, display: &str, typed: &str, is_folder: bool) -> String {
    if is_folder || parsing_name.starts_with("::") { return typed.to_string(); }
    let path = std::path::Path::new(parsing_name);
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) if !display.to_lowercase().ends_with(&format!(".{}", ext.to_lowercase())) => format!("{typed}.{ext}"),
        _ => typed.to_string(),
    }
}

/// The parsing name the renamed item has in `after`, from its folder and the `target` name given to the shell.
/// An exact-case match comes first (a case-only rename), then a case-insensitive one; both must be a new
/// entry or the old item itself. Failing that, the single new entry in the same folder (the shell resolved
/// a conflict by keeping both, `x (2)`). `before` holds the lowercased parsing names from before the rename.
fn renamed_parsing_name(old: &str, target: &str, before: &[String], after: &[String]) -> Option<String> {
    let parent = |p: &str| std::path::Path::new(p).parent().map(|d| d.to_string_lossy().to_lowercase()).unwrap_or_default();
    let old_parent = parent(old);
    let old_key = old.to_lowercase();
    let wanted = std::path::Path::new(old).parent().map_or_else(|| target.to_string(), |d| d.join(target).to_string_lossy().into_owned());
    let wanted_key = wanted.to_lowercase();
    let usable = |name: &String| { let key = name.to_lowercase(); key == wanted_key && (key == old_key || !before.contains(&key)) };
    after.iter().find(|n| **n == wanted && usable(n))
        .or_else(|| after.iter().find(|n| usable(n)))
        .or_else(|| {
            let mut fresh = after.iter().filter(|n| parent(n) == old_parent && !before.contains(&n.to_lowercase()));
            fresh.next().filter(|_| fresh.next().is_none())
        })
        .cloned()
}

/// Where the rename edit's selection ends, in UTF-16 units: before a visible extension, as Explorer selects it.
/// `None` selects everything.
fn rename_selection_end(parsing_name: &str, display: &str, is_folder: bool) -> Option<usize> {
    if is_folder { return None; }
    let path = std::path::Path::new(parsing_name);
    let file = path.file_name()?.to_str()?;
    let stem = path.file_stem()?.to_str()?;
    (file.eq_ignore_ascii_case(display) && path.extension().is_some() && !stem.is_empty()).then(|| stem.encode_utf16().count())
}

/// Rename through the shell (undoable; the shell applies conflict rules; `new_name` is taken literally).
fn rename_with_shell(owner: HWND, parsing_name: &str, new_name: &str) -> windows::core::Result<()> {
    use windows::Win32::UI::Shell::{FOF_ALLOWUNDO, FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName};
    unsafe {
        let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOwnerWindow(owner)?;
        operation.SetOperationFlags(FOF_ALLOWUNDO)?;
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(parsing_name), None)?;
        operation.RenameItem(&item, &HSTRING::from(new_name), None)?;
        operation.PerformOperations()
    }
}

/// Move files into `folder` through the shell (undoable; the shell applies its conflict rules). A name
/// that no longer resolves is skipped (logged); nothing runs when none is left.
pub(super) fn move_with_shell(owner: HWND, parsing_names: &[String], folder: &str) -> windows::core::Result<()> {
    use windows::Win32::UI::Shell::{FOF_ALLOWUNDO, FileOperation, IFileOperation, IShellItem, SHCreateItemFromParsingName};
    unsafe {
        let operation: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER)?;
        operation.SetOwnerWindow(owner)?;
        operation.SetOperationFlags(FOF_ALLOWUNDO)?;
        let destination: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(folder), None)?;
        let mut queued = 0;
        for name in parsing_names {
            let queue = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(name.as_str()), None)
                .and_then(|item| operation.MoveItem(&item, &destination, PCWSTR::null(), None));
            match queue {
                Ok(()) => queued += 1,
                Err(e) => log(&format!("move into {folder:?}: skipped {name:?} ({e})")),
            }
        }
        if queued == 0 { return Ok(()) }
        operation.PerformOperations()
    }
}

#[cfg(test)]
mod new_name_tests {
    use super::*;
    use std::time::Duration;

    fn names(values: &[&str]) -> Vec<String> { values.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn creation_before_the_command_is_not_news_result() {
        use std::time::{SystemTime, Duration};
        let t = SystemTime::now();
        assert!(created_since(t, t));
        assert!(created_since(t - Duration::from_millis(100), t));
        assert!(created_since(t + Duration::from_secs(2), t));
        assert!(!created_since(t - Duration::from_secs(3), t));
    }

    #[test]
    fn names_are_case_insensitive() {
        assert_eq!(gate_new_name(&names(&["Folder\\Ä.TXT"]), &names(&["folder\\ä.txt"]), Duration::ZERO), NewName::Wait);
    }

    #[test]
    fn no_new_name_keeps_waiting() {
        assert_eq!(gate_new_name(&names(&["old"]), &names(&["old"]), Duration::ZERO), NewName::Wait);
        assert_eq!(gate_new_name(&names(&["old"]), &[], Duration::ZERO), NewName::Wait);
    }

    #[test]
    fn one_new_name_is_returned_with_its_original_spelling() {
        let after = names(&["old", "New folder"]);
        assert_eq!(gate_new_name(&names(&["old"]), &after, Duration::from_millis(4999)), NewName::Rename("New folder"));
    }

    #[test]
    fn two_new_names_cancel_the_request() {
        assert_eq!(gate_new_name(&names(&["old"]), &names(&["old", "a", "b"]), Duration::ZERO), NewName::Cancel);
    }

    #[test]
    fn timeout_cancels_even_when_one_new_name_arrives() {
        for elapsed in [Duration::from_secs(5), Duration::from_secs(6)] {
            assert_eq!(gate_new_name(&[], &names(&["new"]), elapsed), NewName::Cancel);
            assert_eq!(gate_new_name(&[], &[], elapsed), NewName::Cancel);
        }
    }

    #[test]
    fn removed_name_and_one_new_name_are_unambiguous() {
        let after = names(&["kept", "new.txt"]);
        assert_eq!(gate_new_name(&names(&["removed", "kept"]), &after, Duration::ZERO), NewName::Rename("new.txt"));
    }
}

#[cfg(test)]
mod raster_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn raster_demand_tracks_membership_listing_metrics_policy_and_cache_reset() {
        let content = GroupContent::Items { items: vec!["a".into()] };
        let changed = GroupContent::Items { items: vec!["b".into()] };
        let portal = GroupContent::Portal { path: "QA".into() };
        let policy = crate::icon_overlay::Policy(0);
        let fingerprint = |id, dpi, size, policy, content: &GroupContent, items: &[(&str, Option<u64>)]|
            raster_demand_fingerprint([(id, dpi, size, policy, content)], items.iter().copied());
        let listing = [("a", None), ("b", Some(2))];
        let base = fingerprint(1, 96, 32, policy, &content, &listing);
        assert_eq!(base, fingerprint(1, 96, 32, policy, &content, &listing));
        assert_ne!(base, fingerprint(2, 96, 32, policy, &content, &listing));
        assert_ne!(base, fingerprint(1, 144, 32, policy, &content, &listing));
        assert_ne!(base, fingerprint(1, 96, 48, policy, &content, &listing));
        assert_ne!(base, fingerprint(1, 96, 32, crate::icon_overlay::Policy(1), &content, &listing));
        assert_ne!(base, fingerprint(1, 96, 32, policy, &changed, &listing));
        assert_ne!(base, fingerprint(1, 96, 32, policy, &portal, &listing));
        assert_ne!(base, fingerprint(1, 96, 32, policy, &content, &listing[..1]));
        assert_ne!(base, fingerprint(1, 96, 32, policy, &content, &[("a", Some(1)), ("b", Some(2))]));
        assert_ne!(base, fingerprint(1, 96, 32, policy, &content, &[("renamed", None), ("b", Some(2))]));
        let mut cache = RasterCache::<()>::new();
        let mut rebuilds = 0;
        for _ in 0..100 {
            if !cache.demand_current(1, base) {
                rebuilds += 1;
                cache.retain(1, &HashSet::new());
                cache.demand_fingerprint = Some(base);
            }
        }
        assert_eq!(rebuilds, 1);
        assert!(!cache.demand_current(2, base));
        assert!(!cache.demand_current(1, base.wrapping_add(1)));
        cache.reset(1);
        assert!(!cache.demand_current(1, base));
    }

    fn key(dpi: u32) -> RasterKey {
        RasterKey::new("QA.lnk", dpi, dpi / 3, crate::icon_overlay::Policy(crate::icon_overlay::DEFAULT_MASK))
    }

    #[test]
    fn mixed_dpi_raster_keys_include_dpi_size_policy_and_casefolded_name() {
        let keys: HashSet<_> = [96, 144, 240].into_iter().map(key).collect();
        assert_eq!(keys.len(), 3);
        let base = key(96);
        assert_eq!(base, RasterKey::new("qa.LNK", 96, 32, base.icon.policy));
        assert_ne!(base, RasterKey::new("QA.lnk", 144, 32, base.icon.policy));
        assert_ne!(base, RasterKey::new("QA.lnk", 96, 48, base.icon.policy));
        assert_ne!(base, RasterKey::new("QA.lnk", 96, 32, crate::icon_overlay::Policy(0)));
    }

    #[test]
    fn mixed_dpi_raster_eviction_releases_unused_pixels_and_retains_shared_demand() {
        struct Pixels(Rc<Cell<usize>>);
        impl Drop for Pixels { fn drop(&mut self) { self.0.set(self.0.get() + 1); } }
        let dropped = Rc::new(Cell::new(0));
        let mut cache = RasterCache::new();
        cache.owner = 1;
        for dpi in [96, 144, 240] { cache.entries.insert(key(dpi), Some(Rc::new(Pixels(dropped.clone())))); }
        // Two groups demanding the same DPI/item collapse to one key.
        cache.retain(1, &[key(96), key(240), key(240)].into_iter().collect());
        assert_eq!((cache.entries.len(), dropped.get()), (2, 1));
        cache.retain(1, &[key(96)].into_iter().collect());
        assert_eq!((cache.entries.len(), dropped.get()), (1, 2));
        cache.retain(1, &HashSet::new());
        assert_eq!((cache.entries.len(), dropped.get()), (0, 3));
    }

    #[test]
    fn mixed_dpi_raster_repaint_reuses_pixels_and_caches_failures_until_reload() {
        GROUP_RASTERS.with(|cache| cache.borrow_mut().reset(1));
        let calls = Cell::new(0);
        let request = || {
            calls.set(calls.get() + 1);
            Some(shell::IconBitmap { width: 32, height: 32, bgra_premultiplied: vec![0; 32 * 32 * 4], cache_key: None })
        };
        let first = cached_group_raster(key(96), request).unwrap();
        for _ in 0..100 { assert!(Rc::ptr_eq(&first, &cached_group_raster(key(96), request).unwrap())); }
        assert_eq!(calls.get(), 1);
        let failure = || { calls.set(calls.get() + 1); None };
        assert!(cached_group_raster(key(240), failure).is_none());
        assert!(cached_group_raster(key(240), failure).is_none());
        assert_eq!(calls.get(), 2);
        GROUP_RASTERS.with(|cache| cache.borrow_mut().reset(1));
        assert!(cached_group_raster(key(240), failure).is_none());
        assert_eq!(calls.get(), 3);
        GROUP_RASTERS.with(|cache| cache.borrow_mut().reset(0));
    }

    #[test]
    fn mixed_dpi_raster_new_app_or_changed_size_policy_discards_old_identities() {
        let mut cache = RasterCache::<()>::new();
        cache.owner = 1;
        cache.entries.insert(key(96), Some(Rc::new(())));
        let changed = RasterKey::new("QA.lnk", 96, 48, crate::icon_overlay::Policy(0));
        cache.retain(1, &[changed].into_iter().collect());
        assert!(cache.entries.is_empty());
        cache.entries.insert(key(96), None);
        cache.retain(2, &[key(96)].into_iter().collect());
        assert_eq!(cache.owner, 2);
        assert!(cache.entries.is_empty());
    }
}

#[cfg(test)]
mod rename_name_tests {
    use super::*;

    #[test]
    fn a_hidden_extension_survives_a_rename() {
        assert_eq!(rename_target(r"C:\d\report.pdf", "report", "budget", false), "budget.pdf");
        assert_eq!(rename_target(r"C:\d\Gra.lnk", "Gra", "Nowa", false), "Nowa.lnk");
        assert_eq!(rename_target(r"C:\d\Łódź.url", "Łódź", "Kraków", false), "Kraków.url");
        assert_eq!(rename_target(r"C:\d\a.tar.gz", "a.tar", "b", false), "b.gz");
    }

    #[test]
    fn a_localized_file_name_keeps_its_extension() {
        assert_eq!(rename_target(r"C:\d\Browser.lnk", "Przegladarka", "Bar", false), "Bar.lnk");
    }

    #[test]
    fn a_visible_extension_folder_or_virtual_item_takes_the_text_as_typed() {
        assert_eq!(rename_target(r"C:\d\report.pdf", "report.pdf", "budget.txt", false), "budget.txt");
        assert_eq!(rename_target(r"C:\d\REPORT.PDF", "report.pdf", "budget.txt", false), "budget.txt");
        assert_eq!(rename_target(r"C:\d\Folder", "Folder", "Nowy", true), "Nowy");
        assert_eq!(rename_target(r"C:\d\my.folder", "Localized", "Nowy", true), "Nowy");
        assert_eq!(rename_target("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", "This PC", "Nowy", false), "Nowy");
    }

    fn lower(values: &[&str]) -> Vec<String> { values.iter().map(|s| s.to_lowercase()).collect() }
    fn raw(values: &[&str]) -> Vec<String> { values.iter().map(|s| s.to_string()).collect() }

    #[test]
    fn the_renamed_item_is_found_by_folder_and_name() {
        let old = r"C:\d\a.txt";
        // plain rename
        assert_eq!(renamed_parsing_name(old, "b.txt", &lower(&[old, r"C:\d\z.txt"]), &raw(&[r"C:\d\b.txt", r"C:\d\z.txt"])).as_deref(), Some(r"C:\d\b.txt"));
        // case-only rename with non-ASCII letters: the key was already known
        let old = r"C:\d\Łódź.txt";
        assert_eq!(renamed_parsing_name(old, "ŁÓDŹ.txt", &lower(&[old]), &raw(&[r"C:\d\ŁÓDŹ.txt"])).as_deref(), Some(r"C:\d\ŁÓDŹ.txt"));
        // conflict kept both: the name differs from the target
        let old = r"C:\d\a.txt";
        assert_eq!(renamed_parsing_name(old, "x.txt", &lower(&[old, r"C:\d\x.txt"]), &raw(&[r"C:\d\x.txt", r"C:\d\x (2).txt"])).as_deref(), Some(r"C:\d\x (2).txt"));
        // nothing new: no match
        assert_eq!(renamed_parsing_name(old, "x.txt", &lower(&[old, r"C:\d\x.txt"]), &raw(&[r"C:\d\x.txt"])), None);
        // two new entries in the folder are ambiguous
        assert_eq!(renamed_parsing_name(old, "x.txt", &lower(&[old]), &raw(&[r"C:\d\p.txt", r"C:\d\q.txt"])), None);
    }

    #[test]
    fn the_edit_selects_the_stem_of_a_visible_extension_only() {
        assert_eq!(rename_selection_end(r"C:\d\report.pdf", "report.pdf", false), Some(6));
        assert_eq!(rename_selection_end(r"C:\d\Łódź.txt", "Łódź.txt", false), Some(4));
        assert_eq!(rename_selection_end(r"C:\d\report.pdf", "report", false), None, "hidden: all of it");
        assert_eq!(rename_selection_end(r"C:\d\my.folder", "my.folder", true), None);
        assert_eq!(rename_selection_end(r"C:\d\.gitignore", ".gitignore", false), None);
    }
}
