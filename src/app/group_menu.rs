//! Group title menu, sorting commands and group removal.

use crate::color_state;
use crate::model::{self, GroupContent, GroupStyle, RollState};
use crate::settings as settings_store;
use crate::shell::{self, ShellItem};
use crate::sorting;
use crate::tabs;
use std::sync::atomic::{AtomicU64, Ordering};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};
use super::{App, log};
use super::portals::qa_portal_menu_choice;

/// Menu ids of the group title menu are `BASE + n`, where `n` is the command number below.
const BASE: usize = 0x100;
/// Shared with the Gate menu, whose shared Corral section is appended unchanged.
pub(super) const MENU_BASE: usize = BASE;
// Corral menu numbering follows the fully expanded menu, from top to bottom.
pub(super) mod corral_cmd {
    pub const RENAME: u32 = 1;
    pub const ROLL_UP: u32 = 2;
    pub const QUICK_HIDE: u32 = 3;
    pub const ALL_PAGES: u32 = 4;
    pub const TITLE_EDGE: u32 = 5;
    /// Six consecutive opacity steps, indexed by model::OPACITY_STEPS.
    pub const OPACITY_FIRST: u32 = 6;
    pub const DISABLE_TRANSPARENCY: u32 = 12;
    pub const COPY_COLOR: u32 = 13;
    pub const PASTE_COLOR: u32 = 14;
    pub const RESET_COLOR: u32 = 15;
    pub const EDIT_COLOR: u32 = 16;
    pub const SORT_NONE: u32 = 17;
    pub const SORT_NAME: u32 = 18;
    pub const SORT_SIZE: u32 = 19;
    pub const SORT_TYPE: u32 = 20;
    pub const SORT_MODIFIED: u32 = 21;
    pub const SORT_CREATED: u32 = 22;
    pub const REVERSE: u32 = 23;
    pub const CONFIGURE: u32 = 24;
    pub const VIEW_ICONS: u32 = 25;
    pub const VIEW_LIST: u32 = 26;
    pub const VIEW_DETAILS: u32 = 27;
}
use corral_cmd::*;

/// Corral sort commands and persisted sort methods are separate namespaces.
const SORT_COMMANDS: [(u32, u32); 6] = [
    (SORT_NONE, sorting::METHOD_NONE),
    (SORT_NAME, sorting::METHOD_NAME),
    (SORT_SIZE, sorting::METHOD_SIZE),
    (SORT_TYPE, sorting::METHOD_TYPE),
    (SORT_MODIFIED, sorting::METHOD_MODIFIED),
    (SORT_CREATED, sorting::METHOD_CREATED),
];

fn sort_method_for_command(command: u32) -> Option<u32> {
    SORT_COMMANDS.iter().find(|(id, _)| *id == command).map(|(_, method)| *method)
}

/// Commands of the Tabs submenu (tabs-spec 6). Menu ids are `BASE + command`; the tab list of
/// "Show tab" starts at [`CMD_TAB_SHOW`].
const CMD_TAB_LEFT: u32 = 0x3f0;
const CMD_TAB_RIGHT: u32 = 0x3f1;
const CMD_TAB_REMOVE: u32 = 0x3f2;
const CMD_TAB_SEPARATE: u32 = 0x3f3;
const CMD_TAB_SHOW: u32 = 0x400;

/// The commands below "Show tab" as data: (command, label, enabled). A single Corral and a stale index
/// have none; a locked desktop disables every one of them.
fn tab_menu_commands(index: usize, count: usize, locked: bool) -> Vec<(u32, &'static str, bool)> {
    if count < 2 || index >= count { return Vec::new() }
    vec![
        (CMD_TAB_LEFT, "Move tab left", !locked && index > 0),
        (CMD_TAB_RIGHT, "Move tab right", !locked && index + 1 < count),
        (CMD_TAB_REMOVE, "Remove from tabs", !locked),
        (CMD_TAB_SEPARATE, "Separate all tabs", !locked),
    ]
}

/// A tab title in the "Show tab" list: `&` is doubled for the menu, 40 characters at most.
fn tab_menu_label(title: &str) -> String {
    let title = if title.is_empty() { "Unnamed Corral" } else { title };
    let mut shown: String = title.chars().take(40).collect();
    if title.chars().count() > 40 { shown.push('\u{2026}'); }
    shown.replace('&', "&&")
}

/// Labels of the Opacity submenu: 0 % and 100 % carry the words "Visible only on mouseover" /
/// "Fully visible".
const OPACITY_LABELS: [&str; 6] = ["0% (Visible only on mouseover)", "20%", "40%", "60%", "80%", "100% (Fully visible)"];

/// "Copy color" source: a process-wide id of the copied group,
/// 0 = nothing copied.
static COPIED_COLOR_SOURCE: AtomicU64 = AtomicU64::new(0);

/// One entry of a menu description. The View submenu is described as data first (pure, unit-tested)
/// and only then turned into an `HMENU`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Entry {
    /// `cmd` is the command number (menu id = `BASE + cmd`).
    Item { cmd: u32, label: &'static str, checked: bool },
    Separator,
    Popup { label: &'static str, items: Vec<Entry> },
}

/// State the View submenu of an ordinary Corral depends on.
#[derive(Clone, Copy, Debug)]
pub(super) struct ViewMenuState {
    /// Setting `RollupEnabled`.
    pub rollup_enabled: bool,
    pub rolled: bool,
    /// Setting `TitleEdgeAutoAdjust`.
    pub title_edge_auto_adjust: bool,
    /// The group's opacity.
    pub opacity: u8,
    /// A Corral is translucent, so the global opacity override can be offered.
    pub any_translucent: bool,
    /// Setting `ShowHiddenCorrals`.
    pub transparency_disabled: bool,
    /// A Corral's colour was copied before and that Corral is not this one.
    pub can_paste: bool,
    /// The group has its own style.
    pub has_own_style: bool,
    /// Keep visible in Clear view (`QuickHideEnabled`): `Some(checked)` when offered, checked iff
    /// the group id is in the exclusion list.
    pub quick_hide_entry: Option<bool>,
    /// Offered only when Pages is enabled and the used page range spans multiple pages.
    pub all_pages_entry: Option<bool>,
}

/// Content of the View submenu in its fixed order and wording. Entries
/// include only supported actions.
/// The separator after Fold / Keep visible / Show on every Range requires a preceding item, so with all
/// three absent it does not become the first item.
pub(super) fn view_menu_entries(s: &ViewMenuState) -> Vec<Entry> {
    let mut v = Vec::new();
    if s.rollup_enabled {
        v.push(Entry::Item { cmd: ROLL_UP, label: "Fold", checked: s.rolled });
    }
    if let Some(checked) = s.quick_hide_entry {
        v.push(Entry::Item { cmd: QUICK_HIDE, label: "Keep visible in Clear view", checked });
    }
    if let Some(checked) = s.all_pages_entry {
        v.push(Entry::Item { cmd: ALL_PAGES, label: "Show on every Range", checked });
    }
    if s.rollup_enabled || s.quick_hide_entry.is_some() || s.all_pages_entry.is_some() {
        v.push(Entry::Separator);
    }
    v.push(Entry::Popup {
        label: "Titlebar position",
        items: vec![Entry::Item { cmd: TITLE_EDGE, label: "Place title below Corrals near the screen top", checked: s.title_edge_auto_adjust }],
    });
    let mut opacity: Vec<Entry> = OPACITY_LABELS
        .iter()
        .zip(model::OPACITY_STEPS)
        .enumerate()
        .map(|(i, (label, step))| Entry::Item { cmd: OPACITY_FIRST + i as u32, label, checked: s.opacity == step })
        .collect();
    if s.any_translucent {
        opacity.push(Entry::Separator);
        opacity.push(Entry::Item { cmd: DISABLE_TRANSPARENCY, label: "Ignore individual Corral opacity", checked: s.transparency_disabled });
    }
    v.push(Entry::Popup { label: "Opacity", items: opacity });
    v.push(Entry::Separator);
    v.push(Entry::Item { cmd: COPY_COLOR, label: "Copy color", checked: false });
    if s.can_paste {
        v.push(Entry::Item { cmd: PASTE_COLOR, label: "Paste color", checked: false });
    }
    if s.has_own_style {
        v.push(Entry::Item { cmd: RESET_COLOR, label: "Reset color", checked: false });
    }
    v.push(Entry::Item { cmd: EDIT_COLOR, label: "Colors…", checked: false });
    v
}

/// Append `entries` to `menu` (`MF_CHECKED` is the plain check mark of `CheckMenuItem(.., 8)`).
pub(super) fn append_entries(menu: HMENU, entries: &[Entry]) {
    for entry in entries {
        unsafe {
            match entry {
                Entry::Item { cmd, label, checked } => {
                    let flags = if *checked { MF_STRING | MF_CHECKED } else { MF_STRING };
                    let text = HSTRING::from(*label);
                    let _ = AppendMenuW(menu, flags, BASE + *cmd as usize, PCWSTR(text.as_ptr()));
                }
                Entry::Separator => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
                Entry::Popup { label, items } => {
                    if let Ok(sub) = CreatePopupMenu() {
                        append_entries(sub, items);
                        let text = HSTRING::from(*label);
                        let _ = AppendMenuW(menu, MF_POPUP, sub.0 as usize, PCWSTR(text.as_ptr()));
                    }
                }
            }
        }
    }
}

/// Picking an opacity step: nothing happens when the value is unchanged
/// and the global opacity override is off; otherwise the value is stored and the switch is
/// cleared.
pub(super) fn opacity_pick_applies(current: u8, picked: u8, transparency_disabled: bool) -> bool {
    current != picked || transparency_disabled
}

/// "Paste color": the copied Corral's 13 style values replace this Corral's (values the
/// source lacks are removed from the target). Pasting a Corral onto itself does nothing.
pub(super) fn pasted_style(target_id: u64, source_id: u64, source: &GroupStyle) -> Option<GroupStyle> {
    (target_id != source_id).then(|| source.clone())
}

/// QA hook result: the colour becomes this Corral's own value, also when it equals the shared one
/// (colour-state-spec 5).
pub(super) fn with_edited_color(mut style: GroupStyle, color: u32) -> GroupStyle {
    style.color = Some(color);
    style
}

/// Turning off Show on every Range assigns the group to the displayed Range.
fn toggle_all_pages(group: &mut model::Group, current_page: i32) {
    group.all_pages = !group.all_pages;
    if !group.all_pages { group.swipe_index = current_page; }
}

/// QA hook (automated trials only): `CORRAL_QA_MESSAGEBOX=yes|no|ok` answers message boxes of the
/// title menu commands without showing them.
fn qa_message_box() -> Option<MESSAGEBOX_RESULT> {
    match std::env::var("CORRAL_QA_MESSAGEBOX").ok()?.to_ascii_lowercase().as_str() {
        "yes" => Some(IDYES),
        "no" => Some(IDNO),
        "ok" => Some(IDOK),
        _ => None,
    }
}

fn message_box(owner: HWND, text: PCWSTR, flags: MESSAGEBOX_STYLE) -> MESSAGEBOX_RESULT {
    qa_message_box().unwrap_or_else(|| {
        let text=unsafe{text.to_string()}.unwrap_or_default();
        if flags.0 & 0x0f == MB_YESNO.0 {
            if crate::dialog::confirm(owner,crate::dialog::system_light(),"Corral",&text,"Continue",false){IDYES}else{IDNO}
        }else{crate::dialog::notice(owner,crate::dialog::system_light(),"Corral",&text);IDOK}
    })
}

/// QA hook (automated trials only): `CORRAL_QA_EDIT_COLOR=RRGGBB` sets the colour of "Colors…"
/// without opening the window. Returns a COLORREF (`0x00BBGGRR`).
fn qa_edit_color() -> Option<u32> {
    let value = std::env::var("CORRAL_QA_EDIT_COLOR").ok()?;
    let rgb = u32::from_str_radix(value.trim().trim_start_matches('#'), 16).ok()?;
    Some(((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff))
}

fn combine_tab_spec(target: &str, target_tabs: usize, source: &str, source_tabs: usize) -> crate::dialog::DialogSpec {
    let mut spec = crate::dialog::DialogSpec::new("Combine into tabs?", tabs::join_sentence(target, target_tabs, source, source_tabs), "Combine");
    spec.checks.push(crate::dialog::Checkbox::new("Don't ask again", false));
    spec
}

fn accepted_tab_combine(result: &crate::dialog::DialogResult) -> (bool, bool) {
    let accepted = result.action == crate::dialog::Action::Primary;
    (
        accepted,
        accepted && result.checks.first().copied().unwrap_or(false),
    )
}

impl App {
    pub(super) fn tab_menu_owner(&self, member: u64) -> HWND {
        let active = tabs::collect(&self.layout.groups, member).map_or(member, |v| v.active);
        self.groups.iter().find(|g| g.id == active).map(|g| g.hwnd).unwrap_or(self.surface_hwnd)
    }

    /// The pure part of a confirmed or unconfirmed combine (tabs-spec 3.4): ask, re-validate against the
    /// live layout, write, swap. False when nothing was combined (Cancel, a refusal, a failed write);
    /// the caller then restores the gesture.
    pub(super) fn combine_dropped(&mut self, zone: tabs::ZoneHit, source: u64) -> bool {
        let settings = tabs::TabSettings::from(&self.settings);
        let policy = settings.join_policy(self.settings.label_mode != 0);
        let (Some(dest), Some(incoming)) = (tabs::collect(&self.layout.groups, zone.target), tabs::collect(&self.layout.groups, source)) else {
            return false;
        };
        if self.pages.moving() || tabs::check_join(&self.layout.tab_doc(), zone.target, source, &policy).is_err() {
            return false;
        }
        if settings.confirm {
            let (accepted, never_ask) = if let Some(answer) = qa_message_box() {
                (answer == IDYES, false)
            } else {
                let title_of = |id| {
                    self.layout.groups.iter().find(|g| g.id == id).map(|g| self.portal_title(g)).filter(|t| !t.is_empty())
                        .unwrap_or_else(|| "Unnamed Corral".to_string())
                };
                let spec = combine_tab_spec(&title_of(dest.active), dest.members.len(), &title_of(incoming.active), incoming.members.len());
                accepted_tab_combine(&crate::dialog::show(self.tab_menu_owner(zone.target), crate::dialog::system_light(), spec))
            };
            if !accepted { return false }
            if never_ask {
                self.settings.tabs_require_confirmation = false;
                self.persist_settings();
            }
        }
        // Nothing was touched while the dialog was open; the join is checked again against the live data.
        let combine = if incoming.members.len() > 1 { tabs::merge } else { tabs::join };
        let doc = match combine(&self.layout.tab_doc(), zone.target, source, Some(zone.insertion), &policy) {
            Ok(doc) => doc,
            Err(e) => { log(&format!("tab join abandoned: {e:?}")); return false }
        };
        if !self.commit_tab_edit(doc) { return false }
        let joined = tabs::collect(&self.layout.groups, zone.target);
        let shown = joined.as_ref().map_or(dest.active, |v| v.active);
        // Every tab of the set shares the target's place, so they share its runtime body too.
        let body = self.runtime_bodies.get(&dest.active).cloned();
        for member in joined.map(|v| v.members).unwrap_or_default() {
            match &body {
                Some(state) => { self.runtime_bodies.insert(member, state.clone()); }
                None => { self.runtime_bodies.remove(&member); }
            }
        }
        self.transfer_tab_runtime_for_join(dest.active, shown, &incoming.members);
        self.refresh_tabs();
        true
    }

    /// Write-then-swap of a tab edit (tabs-spec 7). On a failed write nothing changed; a notice says so.
    pub(super) fn commit_tab_edit(&mut self, doc: tabs::TabDoc) -> bool {
        // While a display transition is pending the live layout is not the stored one (`layout_to_save`).
        if let Err(e) = tabs::edit_allowed(self.layout.desktop_locked, self.profile_pending.is_some()) {
            log(&format!("tab edit refused: {e:?}"));
            return false;
        }
        let mut layout = std::mem::take(&mut self.layout);
        let result = layout.commit_tab_doc(doc, |next| self.write_layout_for_tabs(next));
        self.layout = layout;
        match result {
            Ok(changed) => {
                if changed { self.record_desk_edit(); }
                true
            }
            Err(model::TabCommitError::Write(e)) => {
                log(&format!("tab edit not saved: {e}"));
                crate::dialog::notice(self.tab_menu_owner(0), crate::dialog::system_light(), "Corral", "Couldn't save the layout, so nothing was changed.");
                false
            }
            Err(model::TabCommitError::Tab(e)) => {
                log(&format!("tab edit rejected: {e:?}"));
                false
            }
        }
    }

    /// Persist `next` the way `save` does; a save that is deferred or suspended (a pending desk setup
    /// switch, a snapshot) leaves the write to the normal path.
    fn write_layout_for_tabs(&self, next: &model::Layout) -> std::io::Result<()> {
        if crate::desk_setup::defers_save(next.desk_setups.runtime.edit_pending, self.profile_pending.is_some(), self.profiles_enabled())
            || !self.snapshot_may_save_layout() {
            return Ok(());
        }
        let mut layout = match self.color_overlay.current() {
            Some(overlay) => {
                let mut layout = next.clone();
                overlay.restore(&mut layout, &mut self.settings.clone());
                layout
            }
            None => next.clone(),
        };
        for g in &mut layout.groups { g.style.migrate_tint_source(self.settings.color, self.settings.desktop_tint_icons_color); }
        model::save(&self.opts.layout_path, &layout)
    }

    /// The on-screen area for a body rect of `monitor`, in DIPs of its work area (room for the strip kept
    /// on both sides).
    fn tab_area_dip(&self, monitor: &str) -> Option<crate::geometry::Rect> {
        let m = self.monitor_for(monitor)?;
        let strip = crate::group_paint::metrics(m.dpi);
        let margin = crate::geometry::to_dip(strip.title_h + strip.gap, m.dpi);
        Some(crate::geometry::Rect::new(0, margin,
            crate::geometry::to_dip(m.work.width(), m.dpi), crate::geometry::to_dip(m.work.height(), m.dpi) - margin))
    }

    /// Persist join-inherited exemptions only in the normal Corral layout, never Settings/files.
    pub(super) fn sync_tab_exclusions(&mut self, member: u64, excluded: bool) {
        if let Err(e) = self.layout.set_tab_exclusion(member, excluded) {
            log(&format!("tab exemption rejected: {e:?}")); return;
        }
        self.apply_tab_exclusions();
    }

    pub(super) fn apply_tab_exclusions(&mut self) {
        let mut list = self.tab_base_exclusions.as_deref().map(crate::quick_hide::Exclusions::parse)
            .unwrap_or_else(|| crate::quick_hide::Exclusions::default_list(&std::env::var("USERPROFILE").unwrap_or_default()));
        for (id, excluded) in &self.layout.tab_quick_hide_exclusions {
            if let Some(group) = self.layout.groups.iter().find(|g| g.id == *id) {
                if list.contains_group(*id) != *excluded { list.toggle_group(*id, &group.title); }
            }
        }
        self.settings.quick_hide_exclusions = Some(list.serialize());
    }

    /// Append (or, with `at`, insert at that position) the Tabs submenu of a tab set (tabs-spec 6).
    /// A plain Corral has none. Returns the members in display order, for [`App::tab_menu_command`].
    pub(super) fn append_tabs_menu(&self, menu: HMENU, at: Option<u32>, id: u64) -> Vec<u64> {
        let Some(view) = tabs::collect(&self.layout.groups, id).filter(|v| v.members.len() > 1) else { return Vec::new() };
        let Some(index) = view.members.iter().position(|m| *m == id) else { return Vec::new() };
        unsafe {
            let Ok(sub) = CreatePopupMenu() else { return Vec::new() };
            if let Ok(list) = CreatePopupMenu() {
                for (i, member) in view.members.iter().enumerate() {
                    let Some(group) = self.layout.groups.iter().find(|g| g.id == *member) else { continue };
                    let flags = if view.active == *member { MF_STRING | MF_CHECKED } else { MF_STRING };
                    let _ = AppendMenuW(list, flags, BASE + CMD_TAB_SHOW as usize + i, &HSTRING::from(tab_menu_label(&self.portal_title(group))));
                }
                let _ = AppendMenuW(sub, MF_POPUP, list.0 as usize, w!("Show tab"));
            }
            let commands = tab_menu_commands(index, view.members.len(), self.layout.desktop_locked);
            if !commands.is_empty() { let _ = AppendMenuW(sub, MF_SEPARATOR, 0, PCWSTR::null()); }
            for (cmd, label, enabled) in commands {
                let flags = if enabled { MF_STRING } else { MF_STRING | MF_GRAYED };
                let _ = AppendMenuW(sub, flags, BASE + cmd as usize, &HSTRING::from(label));
            }
            let added = match at {
                Some(pos) => InsertMenuW(menu, pos, MF_BYPOSITION | MF_POPUP, sub.0 as usize, w!("Tabs")),
                None => AppendMenuW(menu, MF_POPUP, sub.0 as usize, w!("Tabs")),
            };
            if added.is_err() { let _ = DestroyMenu(sub); return Vec::new() }
        }
        view.members
    }

    pub(super) fn tab_menu_command(&mut self, id: u64, choice: usize, members: &[u64]) -> bool {
        let Some(view) = tabs::collect(&self.layout.groups, id) else { return false };
        if view.members != members { return false }
        if self.profile_pending.is_some() && choice.checked_sub(BASE + CMD_TAB_SHOW as usize).is_none() {
            return choice.checked_sub(BASE).is_some_and(|c| matches!(c as u32, CMD_TAB_LEFT | CMD_TAB_RIGHT | CMD_TAB_REMOVE | CMD_TAB_SEPARATE));
        }
        if let Some(member) = choice.checked_sub(BASE + CMD_TAB_SHOW as usize).and_then(|i| members.get(i)).copied() {
            self.select_tab(member);
            return true;
        }
        let Some(index) = view.members.iter().position(|m| *m == id) else { return false };
        let Some(cmd) = choice.checked_sub(BASE).map(|c| c as u32) else { return false };
        if !tab_menu_commands(index, members.len(), self.layout.desktop_locked).iter().any(|(c, _, enabled)| *c == cmd && *enabled) {
            return matches!(cmd, CMD_TAB_LEFT | CMD_TAB_RIGHT | CMD_TAB_REMOVE | CMD_TAB_SEPARATE);
        }
        match cmd {
            CMD_TAB_LEFT => self.move_tab(id, index.saturating_sub(1)),
            CMD_TAB_RIGHT => self.move_tab(id, index + 1),
            CMD_TAB_REMOVE => self.remove_from_tabs(id),
            CMD_TAB_SEPARATE => self.separate_all_tabs(id),
            _ => return false,
        }
        true
    }

    fn move_tab(&mut self, id: u64, to: usize) {
        match tabs::reorder(&self.layout.tab_doc(), id, to) {
            Ok(doc) => if self.commit_tab_edit(doc) { self.refresh_tabs(); },
            Err(e) => log(&format!("tab move rejected: {e:?}")),
        }
    }

    /// "Remove from tabs": the tab leaves at the set's place offset by 24 DIP (tabs-spec 6).
    fn remove_from_tabs(&mut self, id: u64) {
        let Some(view) = tabs::collect(&self.layout.groups, id).filter(|v| v.members.len() > 1) else { return };
        let Some(shown) = self.layout.groups.iter().find(|g| g.id == view.active).cloned() else { return };
        let moved = shown.rect.offset(tabs::CASCADE_DIP, tabs::CASCADE_DIP);
        let rect = self.tab_area_dip(&shown.monitor).map_or(moved, |area| tabs::shift_into(moved, area));
        let place = tabs::Placement { monitor: shown.monitor.clone(), rect };
        let doc = match tabs::remove_tab(&self.layout.tab_doc(), id, &place) {
            Ok(doc) => doc,
            Err(e) => { log(&format!("remove from tabs rejected: {e:?}")); return }
        };
        if !self.commit_tab_edit(doc) { return }
        if view.active == id {
            if let Some(next) = view.members.iter().copied().find(|m| *m != id).and_then(|m| tabs::collect(&self.layout.groups, m)) {
                self.transfer_tab_runtime(id, next.active);
            }
        }
        self.roll_progress.remove(&id);
        self.roll_target.remove(&id);
        self.refresh_tabs();
        if let Some(i) = self.groups.iter().position(|g| g.id == id) { self.raise_group(i); }
    }

    /// "Separate all tabs": every tab becomes its own Corral (tabs-spec 6).
    fn separate_all_tabs(&mut self, id: u64) {
        let Some(_) = tabs::collect(&self.layout.groups, id).filter(|v| v.members.len() > 1) else { return };
        let doc = match tabs::separate_all(&self.layout.tab_doc(), id, &|monitor| self.tab_area_dip(monitor)) {
            Ok(doc) => doc,
            Err(e) => { log(&format!("separate all rejected: {e:?}")); return }
        };
        if !self.commit_tab_edit(doc) { return }
        self.refresh_tabs();
    }


    /// Remove a group from the layout ("Remove this Folder Portal": no
    /// confirmation from the menu). Files are never touched; a removed Corral's members become loose.
    pub(super) fn remove_group(&mut self, id: u64) {
        if self.layout.desktop_locked { return }
        self.drop_item_hover(id);
        let view = tabs::collect(&self.layout.groups, id);
        if let Err(e) = self.layout.remove_tab_record(id, self.pages.current) {
            log(&format!("tab removal rejected: {e:?}"));
            return;
        }
        if let Some(view) = &view {
            if view.active == id {
                if let Some(remaining) = view.members.iter().copied().find(|m| *m != id).and_then(|m| tabs::collect(&self.layout.groups, m)) {
                    self.transfer_tab_runtime(id, remaining.active);
                }
            }
        }
        self.portals.remove(&id);
        self.roll_progress.remove(&id);
        self.roll_target.remove(&id);
        self.title_hover.remove(&id);
        for slot in [&mut self.hot_group, &mut self.pending_hover, &mut self.roll_hover_suppressed, &mut self.title_hot] {
            if *slot == Some(id) { *slot = None; }
        }
        if view.is_some_and(|v| v.members.len() > 1) { self.arm_tab_save(); } else { self.record_desk_edit(); self.save(); }
        self.sync_group_windows();
        self.arrange_loose();
        self.refresh_tabs();
    }

    /// What the shared Corral section of a title menu depends on (Fold, Keep visible in Clear view,
    /// Show on every Range, Titlebar position, Opacity, colours).
    pub(super) fn view_menu_state(&self, id: u64, group: &model::Group) -> ViewMenuState {
        ViewMenuState {
            rollup_enabled: self.settings.rollup_enabled && tabs::body_visible(&self.layout.groups, group),
            rolled: group.roll != RollState::Expanded,
            title_edge_auto_adjust: self.settings.title_edge_auto_adjust,
            opacity: group.opacity,
            any_translucent: self.layout.any_translucent(),
            transparency_disabled: self.settings.show_hidden_corrals,
            can_paste: self.copied_color_source().is_some_and(|source| source != id),
            has_own_style: color_state::has_own_look(&group.style),
            quick_hide_entry: self.settings.quick_hide_enabled.then(|| self.qh_group_excluded(id)),
            all_pages_entry: self.pages_menu_entry(id),
        }
    }

    /// Group menu from the title strip (right-click or the menu glyph). Portals get their body menu;
    /// ordinary Corrals get a minimal menu until the full title menu is modelled.
    /// Title / menu-glyph menu of a group. Ordinary group, default settings: Rename
    /// Corral, separator, View >, Sort by >, separator, Configure Corral... Only entries backed by a
    /// working Corral feature are built: View holds Fold, Titlebar position, Opacity and the
    /// colour entries, Keep visible in Clear view and Show on every Range ([`view_menu_entries`]; Adjust
    /// spacing and the Lock items are not implemented); Sort by leaves out "Date added to Corral"
    /// and "Number of times opened" (no tracking yet) and "Maintain sorting" (only offered with
    /// SortFlags & 2, which nothing sets).
    /// "Remove Corral" is absent while the title-bar controls are shown (`ctl`).
    pub(super) fn show_group_menu(&mut self, id: u64, screen: (i32, i32)) {
        if self.portals.contains_key(&id) {
            // A tabbed portal gets its full title menu with the Tabs submenu (live 2026-10-14: it
            // used to show only Tabs).
            self.show_portal_title_menu(id, screen);
            return;
        }
        use windows::Win32::UI::WindowsAndMessaging::{CheckMenuItem, CheckMenuRadioItem, MF_CHECKED, MF_POPUP};
        const RENAME_ID: usize = BASE + RENAME as usize;
        const CONFIGURE_ID: usize = BASE + CONFIGURE as usize;
        const REVERSE_ID: usize = BASE + REVERSE as usize;
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id).cloned() else { return };
        let owner = self.tab_menu_owner(id);
        let (method, flags) = (group.sort_method, group.sort_flags);
        let view = self.view_menu_state(id, &group);
        let (choice, tab_members) = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let _ = AppendMenuW(menu, MF_STRING, RENAME_ID, w!("Rename Corral"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let tab_members = self.append_tabs_menu(menu, None, id);
            if let Ok(view_menu) = CreatePopupMenu() {
                let _ = AppendMenuW(view_menu, MF_STRING, BASE + VIEW_ICONS as usize, w!("Icons"));
                let _ = AppendMenuW(view_menu, MF_STRING, BASE + VIEW_LIST as usize, w!("List"));
                let _ = AppendMenuW(view_menu, MF_STRING, BASE + VIEW_DETAILS as usize, w!("Details"));
                let checked = BASE + match self.group_view(id) { crate::view_state::View::Icons => VIEW_ICONS, crate::view_state::View::List => VIEW_LIST, crate::view_state::View::Details => VIEW_DETAILS } as usize;
                let _ = CheckMenuRadioItem(view_menu, (BASE + VIEW_ICONS as usize) as u32,
                    (BASE + VIEW_DETAILS as usize) as u32, checked as u32, MF_BYCOMMAND.0);
                let _ = AppendMenuW(view_menu, MF_SEPARATOR, 0, PCWSTR::null());
                if self.group_view(id) == crate::view_state::View::Details {
                    if let Ok(columns) = CreatePopupMenu() {
                        self.append_columns_menu(columns, id);
                        let _ = AppendMenuW(view_menu, MF_POPUP, columns.0 as usize, w!("Columns"));
                        let _ = AppendMenuW(view_menu, MF_SEPARATOR, 0, PCWSTR::null());
                    }
                }
                append_entries(view_menu, &view_menu_entries(&view));
                let _ = AppendMenuW(menu, MF_POPUP, view_menu.0 as usize, w!("&View"));
            }
            if let Ok(sort) = CreatePopupMenu() {
                let _ = AppendMenuW(sort, MF_STRING, BASE + SORT_NONE as usize, w!("None"));
                let _ = AppendMenuW(sort, MF_SEPARATOR, 0, PCWSTR::null());
                for (offset, label) in [(SORT_NAME, w!("Name")), (SORT_SIZE, w!("Size")), (SORT_TYPE, w!("Item type")), (SORT_MODIFIED, w!("Date modified")), (SORT_CREATED, w!("Date created"))] {
                    let _ = AppendMenuW(sort, MF_STRING, BASE + offset as usize, label);
                }
                if method != sorting::METHOD_NONE {
                    let _ = AppendMenuW(sort, MF_SEPARATOR, 0, PCWSTR::null());
                    let _ = AppendMenuW(sort, MF_STRING, REVERSE_ID, w!("Reverse order"));
                    if flags & sorting::FLAG_REVERSE != 0 {
                        CheckMenuItem(sort, REVERSE_ID as u32, (MF_BYCOMMAND | MF_CHECKED).0);
                    }
                }
                // radio on the current method (None while not maintained)
                let shown = if flags & sorting::FLAG_NOT_MAINTAINED != 0 { sorting::METHOD_NONE } else { method };
                if let Some(offset) = SORT_COMMANDS.iter().find(|(_, method)| *method == shown).map(|(command, _)| *command) {
                    let item = (BASE + offset as usize) as u32;
                    let _ = CheckMenuRadioItem(sort, item, item, item, MF_BYCOMMAND.0);
                }
                let _ = AppendMenuW(menu, MF_POPUP, sort.0 as usize, w!("S&ort by"));
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, CONFIGURE_ID, w!("Configure Corral..."));
            let _ = SetForegroundWindow(owner);
            // QA hook: the CORRAL_QA_PORTAL_MENU queue answers this menu too (hex ids)
            let choice = match qa_portal_menu_choice() {
                Some(answer) => answer as usize,
                None => TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, screen.0, screen.1, 0, owner, None).0 as usize,
            };
            let _ = DestroyMenu(menu);
            (choice, tab_members)
        };
        log(&format!("group {id}: title menu choice {choice:#x}"));
        if self.column_command(id, choice as u32) { return; }
        if self.tab_menu_command(id, choice, &tab_members) { return }
        match choice {
            RENAME_ID => self.begin_naming(id),
            CONFIGURE_ID => self.open_settings_window(),
            REVERSE_ID => self.set_group_sort(id, None, sorting::FLAG_REVERSE),
            c if c.checked_sub(BASE).and_then(|id| sort_method_for_command(id as u32)).is_some() => {
                if let Some(method) = sort_method_for_command((c - BASE) as u32) {
                    self.set_group_sort(id, Some(method), 0);
                }
            }
            c if c > BASE => self.view_command(id, (c - BASE) as u32),
            _ => {}
        }
    }

    /// Group whose colour was copied, if it still exists.
    pub(super) fn copied_color_source(&self) -> Option<u64> {
        let id = COPIED_COLOR_SOURCE.load(Ordering::Relaxed);
        (id != 0 && self.layout.groups.iter().any(|g| g.id == id)).then_some(id)
    }

    /// Apply a shared View command using the Corral-owned command namespace.
    pub(super) fn view_command(&mut self, id: u64, cmd: u32) {
        // Page/opacity/roll belong to the visible set; RGB, fonts and sorting remain private.
        let id = if cmd == ALL_PAGES || cmd == ROLL_UP || (OPACITY_FIRST..OPACITY_FIRST + 6).contains(&cmd) {
            tabs::collect(&self.layout.groups, id).map_or(id, |v| v.active)
        } else { id };
        match cmd {
            VIEW_ICONS => self.set_group_view(id, crate::view_state::View::Icons),
            VIEW_LIST => self.set_group_view(id, crate::view_state::View::List),
            VIEW_DETAILS => self.set_group_view(id, crate::view_state::View::Details),
            ROLL_UP => self.toggle_roll(id),
            QUICK_HIDE => {
                if tabs::collect(&self.layout.groups, id).is_some_and(|v| v.members.len() > 1)
                    || self.layout.tab_quick_hide_exclusions.contains_key(&id) {
                    self.sync_tab_exclusions(id, !self.qh_group_excluded(id));
                    self.arm_tab_save();
                    self.refresh_tabs();
                } else {
                    self.qh_toggle_group_exclusion(id);
                    self.tab_base_exclusions = settings_store::load_with_backup(&self.opts.settings_path).quick_hide_exclusions;
                    self.apply_tab_exclusions();
                }
            }
            ALL_PAGES => {
                let Some(group) = self.layout.group_mut(id) else { return };
                toggle_all_pages(group, self.pages.current);
                let _ = self.layout.sync_tab_common(id);
                self.record_desk_edit(); self.save();
                self.page_refresh();
            }
            c if (OPACITY_FIRST..OPACITY_FIRST + 6).contains(&c) => {
                self.set_group_opacity(id, model::OPACITY_STEPS[(c - OPACITY_FIRST) as usize]);
            }
            DISABLE_TRANSPARENCY => {
                self.settings.show_hidden_corrals = !self.settings.show_hidden_corrals;
                self.persist_settings();
                self.refresh_all();
            }
            TITLE_EDGE => self.toggle_title_edge(id),
            COPY_COLOR => COPIED_COLOR_SOURCE.store(id, Ordering::Relaxed),
            PASTE_COLOR => self.paste_group_color(id),
            RESET_COLOR => self.reset_group_color(id),
            EDIT_COLOR => self.edit_group_color(id),
            _ => {}
        }
    }

    /// Write the settings file after a menu command changed a setting (as the sort prompt does).
    fn persist_settings(&mut self) {
        // An unfinished colour draft must not be published with an unrelated settings save.
        let (_, mut stored) = self.color_overlay.stored_copy(&self.layout, &self.settings);
        stored.quick_hide_exclusions = self.tab_base_exclusions.clone();
        if let Err(e) = settings_store::save(&self.opts.settings_path, &stored) {
            log(&format!("saving settings failed: {e}"));
        }
        self.settings_stamp = self.settings_file_stamp();
    }

    pub(super) fn rerender_group(&mut self, id: u64) {
        // An inactive member's RGB/text/font edit is visible in the selected frame's strip.
        let id = tabs::collect(&self.layout.groups, id).map_or(id, |v| v.active);
        if let Some(index) = self.groups.iter().position(|g| g.id == id) {
            self.render_group(index);
        }
    }

    /// View > Opacity: store the value, clear the global opacity override, save.
    fn set_group_opacity(&mut self, id: u64, value: u8) {
        let Some(group) = self.layout.group_mut(id) else { return };
        if !opacity_pick_applies(group.opacity, value, self.settings.show_hidden_corrals) {
            return;
        }
        group.opacity = value;
        let _ = self.layout.sync_tab_common(id);
        log(&format!("group {id}: opacity {value:#x}"));
        if self.settings.show_hidden_corrals {
            self.settings.show_hidden_corrals = false;
            self.persist_settings();
            self.record_desk_edit(); self.save();
            self.refresh_all();
        } else {
            self.record_desk_edit(); self.save();
            self.rerender_group(id);
        }
    }

    /// Automatically place the title below Corrals near the top of the screen:
    /// toggles `TitleEdgeAutoAdjust`; switching it off while this Corral has its title at the bottom
    /// shows the information box. The Corrals re-layout so every title moves.
    fn toggle_title_edge(&mut self, id: u64) {
        // Dock latches select their own layout; the undock-to-change notice
        // applies only to the ordinary title-side rules.
        let below = self.layout.groups.iter().find(|g| g.id == id).is_some_and(|g| g.dock_edge().is_none() && g.title_side_last == 1);
        let was = self.settings.title_edge_auto_adjust;
        self.settings.title_edge_auto_adjust = !was;
        log(&format!("TitleEdgeAutoAdjust now {}", self.settings.title_edge_auto_adjust));
        self.persist_settings();
        self.title_edge_changed(was);
        if below && !self.settings.title_edge_auto_adjust {
            message_box(
                self.surface_hwnd,
                w!("Titles will now stay above Corrals near the screen top.\n\nFor a Corral already docked there, drag it away from the edge to apply the change."),
                MB_ICONINFORMATION,
            );
        }
        self.refresh_all();
        self.record_desk_edit(); self.save();
    }

    /// `TitleEdgeAutoAdjust` changed (this menu or the settings window, `was` = before). Switching
    /// it on releases explicit title sides so the option reaches every Corral (see
    /// `Layout::release_title_sides`). Each change logs how every side is decided (CORRAL_LOG).
    pub(super) fn title_edge_changed(&mut self, was: bool) {
        let now = self.settings.title_edge_auto_adjust;
        if was == now { return; }
        if now {
            let released = self.layout.release_title_sides();
            if !released.is_empty() {
                log(&format!("title side: TitleEdgeAutoAdjust on, explicit sides released for groups {released:?}"));
                self.record_desk_edit(); self.save();
            }
        }
        self.log_title_sides();
    }

    /// Paste color: the copied Corral's style values replace this Corral's.
    fn paste_group_color(&mut self, id: u64) {
        self.end_color_overlay(); // never paste over (or save) an unfinished draft
        let Some(source_id) = self.copied_color_source() else { return };
        let Some(source) = self.layout.groups.iter().find(|g| g.id == source_id).map(|g| g.style.clone()) else { return };
        if let Some(style) = pasted_style(id, source_id, &source) {
            if let Some(group) = self.layout.group_mut(id) {
                group.style = style;
                log(&format!("group {id}: color pasted from group {source_id}"));
            }
            self.record_desk_edit(); self.save();
            self.rerender_group(id);
        }
    }

    /// Reset color: Yes/No confirmation, then the Corral follows the global style.
    fn reset_group_color(&mut self, id: u64) {
        self.end_color_overlay();
        let owner = self.groups.iter().find(|g| g.id == id).map(|g| g.hwnd).unwrap_or(self.surface_hwnd);
        let answer = message_box(
            owner,
            w!("Restore the shared colors for this Corral?\n\nIts custom color settings will be cleared."),
            MB_YESNO | MB_ICONQUESTION,
        );
        if answer != IDYES {
            return;
        }
        // colour-state-spec 4: the five resettable groups only; font and tab style stay.
        if let Some(group) = self.layout.group_mut(id) {
            color_state::reset_group(&mut group.style);
            log(&format!("group {id}: color reset"));
        }
        self.record_desk_edit(); self.save();
        self.rerender_group(id);
    }

    /// Colors (colour-state-spec 3.1): the settings window comes forward on
    /// Personalization with this Corral as its scope and the Color popover open under the Custom
    /// swatch. `CORRAL_QA_EDIT_COLOR=RRGGBB` sets this Corral's own background colour without UI.
    fn edit_group_color(&mut self, id: u64) {
        let Some(color) = qa_edit_color() else {
            log(&format!("group {id}: Colors command"));
            let request = crate::settings::style_request::FocusRequest::new(self.opts.layout_path.clone(), id);
            if crate::settings::style_request::send_focus(&request) != crate::settings::style_request::LiveOutcome::Applied {
                // No settings window for this layout: start one that opens straight on this Corral.
                if let Ok(exe) = std::env::current_exe() {
                    let _ = std::process::Command::new(exe)
                        .arg("--settings").arg("--settings-path").arg(&self.opts.settings_path)
                        .arg("--layout").arg(&self.opts.layout_path)
                        .arg("--page").arg("Personalization")
                        .env("CORRAL_SETTINGS_COLOR_TARGET", id.to_string())
                        .spawn();
                }
            }
            return;
        };
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id) else { return };
        let style = group.style.clone();
        if let Some(group) = self.layout.group_mut(id) {
            group.style = with_edited_color(style, color);
            log(&format!("group {id}: color edited to {color:#08x}"));
        }
        self.record_desk_edit(); self.save();
        self.rerender_group(id);
    }

    /// Sort commands: a method is stored only when it changes; Reverse
    /// order toggles its flag (folders are always first, motion-sorting-spec 2). Then save and relayout (re-sort).
    pub(super) fn set_group_sort(&mut self, id: u64, method: Option<u32>, toggle_flags: u32) {
        let Some(group) = self.layout.group_mut(id) else { return };
        if let Some(method) = method {
            if group.sort_method == method { return; }
            group.sort_method = method;
        }
        group.sort_flags ^= toggle_flags;
        log(&format!("group {id}: sort method {:#x} flags {:#x}", group.sort_method, group.sort_flags));
        self.record_desk_edit(); self.save();
        self.refresh_all();
    }

    /// Re-sort every maintained, sorted Corral at layout: members present on the
    /// desktop are ordered by the comparator; members not found keep their relative order after
    /// them. Returns true when an order changed.
    pub(super) fn apply_sorting(&mut self) -> bool {
        let mut changed = false;
        for gi in 0..self.layout.groups.len() {
            let (method, flags) = (self.layout.groups[gi].sort_method, self.layout.groups[gi].sort_flags);
            if !sorting::is_maintained(method, flags) {
                continue;
            }
            let GroupContent::Items { items } = &self.layout.groups[gi].content else { continue };
            let mut found: Vec<(String, sorting::SortKey)> = Vec::new();
            let mut missing: Vec<String> = Vec::new();
            for name in items {
                match self.items.iter().find(|i| i.shell.parsing_name.eq_ignore_ascii_case(name)) {
                    Some(item) => {
                        // Details' type fact was read during loading. A type-sort command must not
                        // ask a Shell extension for it on the UI thread (including before it arrives).
                        let mut key = sort_key(&item.shell, if method == sorting::METHOD_TYPE { sorting::METHOD_NONE } else { method });
                        if method == sorting::METHOD_TYPE { key.type_text = item.details.kind.clone(); }
                        found.push((name.clone(), key));
                    }
                    None => missing.push(name.clone()),
                }
            }
            let keys: Vec<sorting::SortKey> = found.iter().map(|(_, k)| k.clone()).collect();
            let collator = sorting::SystemCollator::user_default();
            let mut sorted: Vec<String> = sorting::order(&keys, method, flags, &collator).into_iter().map(|i| found[i].0.clone()).collect();
            sorted.extend(missing);
            if let GroupContent::Items { items } = &mut self.layout.groups[gi].content {
                if *items != sorted {
                    *items = sorted;
                    changed = true;
                }
            }
        }
        changed
    }

    /// An icon moved inside a sorted Corral: when the group sorts
    /// (method set, maintained) and the drop is at least 3/4 of a cell width from the drag start
    /// (a distance scaled by DPI), the sorting is turned off at once with
    /// `AllowSortingBreakup`, nothing happens with `Tips\NoPromptForSortingBreakup`, else the
    /// "Turn off sorting?" prompt decides:
    /// Yes = method 0 (the icon stays where it was dropped), No = the group re-sorts.
    pub(super) fn sorted_move(&mut self, id: u64, start: (i32, i32), end: (i32, i32)) {
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id) else { return };
        if !sorting::is_maintained(group.sort_method, group.sort_flags) {
            return;
        }
        let cell = self.group_cell(id, self.group_dpi(id)).0 as f64;
        let distance = (((end.0 - start.0) as f64).powi(2) + ((end.1 - start.1) as f64).powi(2)).sqrt();
        if distance < (cell * 3.0 / 4.0).floor() {
            return;
        }
        let turn_off = if self.settings.allow_sorting_breakup {
            true
        } else if self.settings.tips.get("NoPromptForSortingBreakup").is_some_and(|v| *v != 0) {
            false
        } else {
            let (yes, always) = show_break_sorting_prompt(self.surface_hwnd);
            if always {
                if yes {
                    self.settings.allow_sorting_breakup = true;
                } else {
                    self.settings.tips.insert("NoPromptForSortingBreakup".to_string(), 1);
                }
                if let Err(e) = settings_store::save(&self.opts.settings_path, &self.settings_to_save()) {
                    log(&format!("saving the sorting answer failed: {e}"));
                }
                self.settings_stamp = self.settings_file_stamp();
            }
            yes
        };
        log(&format!("group {id}: icon moved in a sorted Corral, sorting turned off {turn_off}"));
        if turn_off {
            if let Some(group) = self.layout.group_mut(id) {
                group.sort_method = sorting::METHOD_NONE;
            }
        }
    }

    /// Close glyph: Yes/No confirmation, then remove; files are untouched.
    pub(super) fn confirm_remove_group(&mut self, id: u64) {
        let owner = self.tab_menu_owner(id);
        if crate::dialog::confirm(owner,crate::dialog::system_light(),"Remove this Corral?",
            "Remove this Corral from the layout?\n\nThe files stay on your desktop.","Remove",true) {
            self.remove_group(id);
        }
    }
}

/// "Turn off sorting?": the dialog
/// rebuilt in `sort_prompt`. Returns (Yes, always); the caption's
/// close button answers No without the check box.
fn show_break_sorting_prompt(owner: HWND) -> (bool, bool) {
    let (answer, always) = crate::sort_prompt::ask_owned(owner, false);
    log(&format!("break-sorting prompt: {answer:?} always {always}"));
    match answer {
        crate::sort_prompt::Answer::Yes => (true, always),
        crate::sort_prompt::Answer::No => (false, always),
        crate::sort_prompt::Answer::Closed => (false, false),
    }
}

/// Sort input of one item (motion-sorting-spec 2): the visible label, the kind, and only the facts
/// the chosen key needs. The Date key reads the creation time for "Date created" and the last-write
/// time otherwise; "Date added" and "Date opened" have no facts and leave every item unknown.
pub(super) fn sort_key(item: &ShellItem, method: u32) -> sorting::SortKey {
    let path = std::path::Path::new(&item.parsing_name);
    let kind = if item.parsing_name.starts_with("::") {
        sorting::Kind::Virtual
    } else if path.is_dir() {
        sorting::Kind::Folder
    } else if path.exists() {
        sorting::Kind::File
    } else {
        sorting::Kind::Virtual
    };
    let unix = |t: std::io::Result<std::time::SystemTime>| t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64);
    let date = match method {
        sorting::METHOD_MODIFIED => item.modified_unix,
        sorting::METHOD_CREATED => unix(std::fs::metadata(path).and_then(|m| m.created())),
        _ => None,
    };
    let type_text = if method == sorting::METHOD_TYPE {
        let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let cache_key = match kind {
            sorting::Kind::File => format!("ext:{}", path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default()),
            sorting::Kind::Folder => "folder".to_string(),
            sorting::Kind::Virtual => format!("item:{}", item.parsing_name),
        };
        sorting::cached_type_text(&cache_key, &file_name, || shell::type_name_of(&item.parsing_name))
    } else {
        String::new()
    };
    sorting::SortKey {
        name: item.display_name.clone(),
        path: item.parsing_name.clone(),
        kind,
        size: if kind == sorting::Kind::File { item.size } else { None },
        type_text,
        date,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> ViewMenuState {
        ViewMenuState {
            rollup_enabled: true,
            rolled: false,
            title_edge_auto_adjust: true,
            opacity: 255,
            any_translucent: false,
            transparency_disabled: false,
            can_paste: false,
            has_own_style: false,
            quick_hide_entry: None,
            all_pages_entry: None,
        }
    }

    fn labels(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| match e {
                Entry::Item { label, checked, .. } => format!("{label}{}", if *checked { " [x]" } else { "" }),
                Entry::Separator => "----".to_string(),
                Entry::Popup { label, items } => format!("{label} > {{{}}}", labels(items).join(" | ")),
            })
            .collect()
    }

    #[test]
    fn default_view_menu_matches_the_expected_order_minus_missing_features() {
        // Expected order: Fold, Keep visible in Clear view, ----, Titlebar position >, Opacity >,
        // Adjust spacing..., ----, Copy color, Colors… (Exclude and Adjust spacing are not built here).
        let got = labels(&view_menu_entries(&state()));
        assert_eq!(
            got,
            [
                "Fold",
                "----",
                "Titlebar position > {Place title below Corrals near the screen top [x]}",
                "Opacity > {0% (Visible only on mouseover) | 20% | 40% | 60% | 80% | 100% (Fully visible) [x]}",
                "----",
                "Copy color",
                "Colors…",
            ]
        );
    }

    #[test]
    fn shared_commands_follow_menu_order_without_collisions() {
        let entries = view_menu_entries(&ViewMenuState {
            quick_hide_entry: Some(false), all_pages_entry: Some(false),
            any_translucent: true, can_paste: true, has_own_style: true, ..state()
        });
        fn collect(entries: &[Entry], ids: &mut Vec<u32>) {
            for entry in entries {
                match entry {
                    Entry::Item { cmd, .. } => ids.push(*cmd),
                    Entry::Popup { items, .. } => collect(items, ids),
                    Entry::Separator => {}
                }
            }
        }
        let mut ids = vec![RENAME];
        collect(&entries, &mut ids);
        ids.extend(SORT_COMMANDS.iter().map(|(command, _)| *command));
        ids.extend([REVERSE, CONFIGURE]);
        assert_eq!(ids, (1..=24).collect::<Vec<_>>());
        assert!((BASE + CONFIGURE as usize) < BASE + CMD_TAB_LEFT as usize);
        assert!(!crate::portal_menu::id::is_gate((BASE + CONFIGURE as usize) as u32));
        assert_eq!(sort_method_for_command(SORT_CREATED), Some(sorting::METHOD_CREATED));
        assert_eq!(sort_method_for_command(EDIT_COLOR), None);
    }

    #[test]
    fn paste_reset_and_disable_entries_appear_only_with_their_state() {
        let all = labels(&view_menu_entries(&ViewMenuState { any_translucent: true, can_paste: true, has_own_style: true, transparency_disabled: true, ..state() }));
        assert!(all.iter().any(|l| l == "Paste color") && all.iter().any(|l| l == "Reset color"));
        assert!(all.iter().any(|l| l.contains("Ignore individual Corral opacity [x]")), "{all:?}");
        let none = labels(&view_menu_entries(&state()));
        assert!(!none.iter().any(|l| l == "Paste color" || l == "Reset color" || l.contains("Disable")));
        // order of the colour block: Copy, Paste, Reset, Edit
        let tail: Vec<&String> = all.iter().rev().take(4).collect();
        assert_eq!(tail, ["Colors…", "Reset color", "Paste color", "Copy color"]);
    }

    #[test]
    fn checks_follow_the_group_state() {
        let menu = labels(&view_menu_entries(&ViewMenuState { rolled: true, title_edge_auto_adjust: false, opacity: 0x66, ..state() }));
        assert_eq!(menu[0], "Fold [x]");
        assert!(menu[2].ends_with("top}"), "titlebar position unchecked: {}", menu[2]);
        assert!(menu[3].contains("40% [x]") && !menu[3].contains("(Fully visible) [x]"), "{}", menu[3]);
        // a value that is not one of the steps (e.g. imported 1) checks nothing
        let odd = labels(&view_menu_entries(&ViewMenuState { opacity: 1, ..state() }));
        assert!(!odd[3].contains("[x]"));
    }

    #[test]
    fn quick_hide_entry_follows_roll_up_with_its_checked_state() {
        let got = labels(&view_menu_entries(&ViewMenuState { quick_hide_entry: Some(false), ..state() }));
        assert_eq!(&got[..3], ["Fold", "Keep visible in Clear view", "----"]);
        let got = labels(&view_menu_entries(&ViewMenuState { quick_hide_entry: Some(true), rollup_enabled: false, ..state() }));
        assert_eq!(&got[..3], ["Keep visible in Clear view [x]", "----", "Titlebar position > {Place title below Corrals near the screen top [x]}"]);
    }

    #[test]
    fn without_roll_up_there_is_no_leading_separator() {
        let got = labels(&view_menu_entries(&ViewMenuState { rollup_enabled: false, ..state() }));
        assert!(got[0].starts_with("Titlebar position"), "{got:?}");
    }

    #[test]
    fn all_ranges_entry_follows_clear_view_and_uses_shared_command() {
        let entries = view_menu_entries(&ViewMenuState { quick_hide_entry: Some(false), all_pages_entry: Some(true), ..state() });
        assert_eq!(&labels(&entries)[..4], ["Fold", "Keep visible in Clear view", "Show on every Range [x]", "----"]);
        assert!(entries.iter().any(|e| matches!(e, Entry::Item { cmd: ALL_PAGES, checked: true, .. })));
        let entries = view_menu_entries(&ViewMenuState { rollup_enabled: false, all_pages_entry: Some(false), ..state() });
        assert_eq!(&labels(&entries)[..2], ["Show on every Range", "----"]);
        assert!(!labels(&view_menu_entries(&state())).iter().any(|l| l.starts_with("Show on every Range")));
    }

    #[test]
    fn all_pages_toggle_keeps_assignment_on_enable_and_uses_current_page_on_disable() {
        let mut layout = model::Layout::new();
        let id = layout.add_group("QA".into(), String::new(), crate::geometry::Rect::default(), GroupContent::Items { items: Vec::new() });
        let group = layout.group_mut(id).unwrap();
        group.swipe_index = -2;
        toggle_all_pages(group, 3);
        assert!(group.all_pages);
        assert_eq!(group.swipe_index, -2);
        toggle_all_pages(group, 3);
        assert!(!group.all_pages);
        assert_eq!(group.swipe_index, 3);
    }

    #[test]
    fn opacity_pick_rules() {
        assert!(!opacity_pick_applies(255, 255, false), "unchanged and not disabled: nothing happens");
        assert!(opacity_pick_applies(255, 0, false));
        assert!(opacity_pick_applies(0x66, 0x66, true), "re-picking clears Ignore individual Corral opacity");
    }

    #[test]
    fn paste_copies_the_whole_style_and_ignores_the_source_itself() {
        let source = GroupStyle { color: Some(0x00ff_0000), font_name: Some("Arial".into()), tabs_style: Some(2), ..Default::default() };
        assert_eq!(pasted_style(2, 1, &source), Some(source.clone()));
        assert_eq!(pasted_style(1, 1, &source), None);
        // a source without own style resets the target to the global style
        assert_eq!(pasted_style(2, 1, &GroupStyle::default()), Some(GroupStyle::default()));
    }

    #[test]
    fn edited_color_stays_own_when_equal_to_the_shared_one() {
        // colour-state-spec 5: nothing converts an explicit choice back to "inherit".
        let own = GroupStyle { transparency_factor: Some(500), ..Default::default() };
        let edited = with_edited_color(own.clone(), 0x0033_2211);
        assert_eq!((edited.color, edited.transparency_factor), (Some(0x0033_2211), Some(500)));
        let same = with_edited_color(edited, 0);
        assert_eq!(same.color, Some(0));
        assert!(color_state::has_own_look(&with_edited_color(GroupStyle::default(), 0)));
    }

    #[test]
    fn reset_color_keeps_font_and_tab_style() {
        let mut style = GroupStyle { color: Some(1), blend_icons: Some(1), font_weight: Some(700), tabs_style: Some(2), ..Default::default() };
        color_state::reset_group(&mut style);
        assert!(!color_state::has_own_look(&style));
        assert_eq!((style.font_weight, style.tabs_style), (Some(700), Some(2)));
    }
}

#[cfg(test)]
mod tab_menu_tests {
    use super::*;

    #[test]
    fn commands_have_the_spec_labels_and_edge_availability() {
        let labels = |i, n, locked| tab_menu_commands(i, n, locked).into_iter().map(|(_, l, e)| (l, e)).collect::<Vec<_>>();
        assert_eq!(labels(0, 3, false), vec![("Move tab left", false), ("Move tab right", true), ("Remove from tabs", true), ("Separate all tabs", true)]);
        assert_eq!(labels(1, 3, false), vec![("Move tab left", true), ("Move tab right", true), ("Remove from tabs", true), ("Separate all tabs", true)]);
        assert_eq!(labels(2, 3, false), vec![("Move tab left", true), ("Move tab right", false), ("Remove from tabs", true), ("Separate all tabs", true)]);
        assert!(labels(1, 3, true).iter().all(|(_, enabled)| !enabled), "a locked desktop disables every command");
    }

    #[test]
    fn a_single_corral_or_a_stale_index_has_no_tab_commands() {
        assert!(tab_menu_commands(0, 1, false).is_empty());
        assert!(tab_menu_commands(3, 3, false).is_empty());
        assert!(tab_menu_commands(0, 0, false).is_empty());
    }

    #[test]
    fn there_is_no_combine_with_command_and_ids_do_not_clash() {
        let ids: Vec<u32> = tab_menu_commands(1, 3, false).into_iter().map(|(c, _, _)| c).collect();
        assert_eq!(ids, vec![CMD_TAB_LEFT, CMD_TAB_RIGHT, CMD_TAB_REMOVE, CMD_TAB_SEPARATE]);
        assert!(ids.iter().all(|c| *c < CMD_TAB_SHOW));
        assert!(!tab_menu_commands(1, 3, false).iter().any(|(_, label, _)| label.contains("Combine")), "tab menu has no combine command");
    }

    #[test]
    fn list_labels_are_trimmed_to_40_characters_and_escape_ampersands() {
        assert_eq!(tab_menu_label("Docs & Pics"), "Docs && Pics");
        assert_eq!(tab_menu_label(""), "Unnamed Corral");
        let long = "x".repeat(41);
        assert_eq!(tab_menu_label(&long), format!("{}\u{2026}", "x".repeat(40)));
        assert_eq!(tab_menu_label(&"y".repeat(40)), "y".repeat(40));
    }
}

#[cfg(test)]
mod u4_prompt_tests {
    #[test]
    fn combine_tabs_prompt_goldens()->windows::core::Result<()> {
        use crate::render::Surface;
        let dir=std::path::Path::new("target/qa/settings-redesign/u4");std::fs::create_dir_all(dir).unwrap();
        let spec=super::combine_tab_spec("Projects", 1, "Downloads", 1);let lay=crate::dialog::layout(&spec,spec.width);
        for light in [true,false]{for dpi in [96u32,120,144,240]{let scale=dpi as f32/96.0;
            let mut surface=Surface::new((lay.width*scale).round()as u32,(lay.height*scale).round()as u32)?;
            surface.draw(|ctx|crate::dialog::draw_spec(ctx,&spec,dpi,light))?;
            surface.save_png(&dir.join(format!("combine-tabs-{}-{dpi}.png",if light{"light"}else{"dark"})))?;
        }}Ok(())
    }
    #[test]
    fn combining_only_remembers_an_accepted_checkbox() {
        use crate::dialog::{Action, DialogResult};
        for action in [Action::Primary, Action::Secondary, Action::Closed] {
            for checked in [false, true] {
                let result = DialogResult {
                    action,
                    checks: vec![checked],
                    text: String::new(),
                };
                assert_eq!(
                    super::accepted_tab_combine(&result),
                    (
                        action == Action::Primary,
                        action == Action::Primary && checked
                    )
                );
            }
        }
    }
}
