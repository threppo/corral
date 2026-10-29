//! Tabs: stacking Corrals in one place (tabs-spec).
//!
//! Pure model, set operations, gesture rules and strip layout helpers. Nothing here touches a
//! window, the registry or a file. Membership is stored as `Group.tab` plus the layout counter
//! `Layout.last_tab_set_id`; stored values are read as written and repaired in memory only
//! (tabs-spec 8). The `Doc` operations return a complete new state that has passed [`validate`]
//! (tabs-spec 7); the caller swaps it in after the layout was written.

use crate::geometry::Rect;
use crate::group_paint::px;
use crate::model::{Group, RollState};
use crate::settings::Settings;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

// ---- stored facts (tabs-spec 2.1) ---------------------------------------------------------------

/// Stored membership of a Corral in a tab set. Absent (`Group.tab == None`) means standalone; a
/// stored set id of 0 is read as standalone too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TabMembership {
    pub set_id: u64,
    pub index: u32,
    pub selected: bool,
}

/// Why an operation was refused (tabs-spec 3.3, 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// `TabsCreate` is off.
    CreateOff,
    /// The target (its set) is folded.
    TargetFolded,
    /// `LabelMode` Never: there is no strip to drop on.
    TitleHidden,
    /// Source and target are the same Corral.
    SelfTarget,
    /// Source is a tab of the target's set.
    SameSet,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabError {
    /// The desktop is locked.
    Locked,
    MissingMember(u64),
    /// The Corral is not a tab of a set.
    NotATab(u64),
    Refused(Refusal),
    /// A state that [`validate`] rejects.
    Invalid(&'static str),
    /// The set id counter cannot grow any more.
    Overflow,
    /// A display transition is pending: the live layout is not what gets written.
    Busy,
}

// ---- settings (tabs-spec 1) ---------------------------------------------------------------------

/// The tab settings as this module uses them. `wait` is the `TabsMouseoverDelay` flag: on = wait
/// [`HOVER_WAIT_MS`], off = switch at once; no millisecond value is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabSettings {
    pub style: u32,
    pub create: bool,
    pub confirm: bool,
    pub mouseover: bool,
    pub wait: bool,
    pub colors: bool,
}

impl Default for TabSettings {
    fn default() -> Self {
        Self { style: 3, create: true, confirm: false, mouseover: false, wait: true, colors: true }
    }
}

impl From<&Settings> for TabSettings {
    fn from(s: &Settings) -> Self {
        Self {
            style: s.tabs_style,
            create: s.tabs_allow_create_with_drag,
            confirm: s.tabs_require_confirmation,
            mouseover: s.tabs_select_on_mouseover > 0,
            wait: s.tabs_select_on_mouseover_delay != 0,
            colors: s.tabs_style != 1 && (s.tabs_always_show_colors || s.tabs_colored_per_tab_in_segment_view),
        }
    }
}

impl TabSettings {
    /// A Corral's own `TabsStyle` wins over the global one (0 = follow the global value).
    pub fn effective_style(&self, per_member: Option<u32>) -> u32 {
        per_member.filter(|v| *v != 0).unwrap_or(self.style)
    }

    /// Wait before a tab under the pointer is selected (tabs-spec 4): `None` = no switching,
    /// `Some(0)` = at once. An icon drag springs after [`SPRING_MS`] even with hover switching off.
    pub fn hover_wait(&self, icon_drag: bool) -> Option<u32> {
        if icon_drag { return Some(SPRING_MS) }
        self.mouseover.then_some(if self.wait { HOVER_WAIT_MS } else { 0 })
    }

    /// Joining rules for `LabelMode` (`titles` = a strip can be shown).
    pub fn join_policy(&self, titles: bool) -> JoinPolicy {
        JoinPolicy { create: self.create, titles }
    }
}

// ---- constants (tabs-spec 3.2, 4, 5), all in one place -------------------------------------------

/// A drop zone arms after the pointer stayed in it this long.
pub const ARM_MS: u64 = 120;
/// Hover switching wait with `TabsMouseoverDelay` on.
pub const HOVER_WAIT_MS: u32 = 300;
/// An icon drag held over an inactive tab selects it after this long.
pub const SPRING_MS: u32 = 600;
/// Tear-off distance from the strip.
pub const TEAR_DIP: f64 = 32.0;
/// Cascade step of "Separate all tabs" and the offset of "Remove from tabs".
pub const CASCADE_DIP: i32 = 24;
/// Strip autoscroll zone at either end while reordering, and the step interval.
pub const AUTOSCROLL_DIP: f64 = 24.0;
pub const AUTOSCROLL_MS: u64 = 200;

// ---- the in-memory model (tabs-spec 2.2) ---------------------------------------------------------

/// A tab set as the app uses it: 2+ tabs in display order and the selected one. Built from the stored
/// values, never written back unless the set is modified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabSet {
    pub id: u64,
    pub tabs: Vec<u64>,
    pub selected: usize,
}

impl TabSet {
    pub fn selected_id(&self) -> u64 {
        self.tabs[self.selected]
    }
}

fn stored_set(group: &Group) -> Option<u64> {
    group.tab.filter(|t| t.set_id != 0).map(|t| t.set_id)
}

fn build_set(groups: &[Group], set_id: u64) -> Option<TabSet> {
    let mut found: Vec<(u32, usize, u64, bool)> = groups.iter().enumerate()
        .filter_map(|(pos, g)| g.tab.filter(|t| t.set_id == set_id).map(|t| (t.index, pos, g.id, t.selected)))
        .collect();
    if found.len() < 2 { return None }
    found.sort_by_key(|e| (e.0, e.1));
    let selected = found.iter().position(|e| e.3).unwrap_or(0);
    Some(TabSet { id: set_id, tabs: found.iter().map(|e| e.2).collect(), selected })
}

/// Every set of the layout, repaired in memory (tabs-spec 8): sorted by `(tab_index, file order)`,
/// the first selected tab wins else index 0, one-member sets are plain Corrals. Sorted by set id.
pub fn normalize_sets(groups: &[Group]) -> Vec<TabSet> {
    let ids: BTreeSet<u64> = groups.iter().filter_map(stored_set).collect();
    ids.into_iter().filter_map(|id| build_set(groups, id)).collect()
}

/// The set `group` belongs to, if it has two or more members.
pub fn set_for(groups: &[Group], group: &Group) -> Option<TabSet> {
    build_set(groups, stored_set(group)?)
}

/// Ordered borrowed members of the set `group` is in; empty for a plain Corral.
pub fn members<'a>(groups: &'a [Group], group: &Group) -> Vec<&'a Group> {
    let Some(set) = set_for(groups, group) else { return Vec::new() };
    set.tabs.iter().filter_map(|id| groups.iter().find(|g| g.id == *id)).collect()
}

/// The selected tab shows its body; a plain Corral always does.
pub fn body_visible(groups: &[Group], group: &Group) -> bool {
    set_for(groups, group).is_none_or(|s| s.selected_id() == group.id)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabSetView {
    /// 0 for a plain Corral.
    pub set_id: u64,
    pub members: Vec<u64>,
    pub active: u64,
}

/// Set of Corral `member` (a plain Corral is its own one-member view). `None` when it does not exist.
pub fn collect(groups: &[Group], member: u64) -> Option<TabSetView> {
    let group = groups.iter().find(|g| g.id == member)?;
    Some(match set_for(groups, group) {
        Some(s) => TabSetView { set_id: s.id, active: s.selected_id(), members: s.tabs },
        None => TabSetView { set_id: 0, members: vec![member], active: member },
    })
}

/// `new_id = max(counter, 1 + highest set id in the layout)`, at least 1 (tabs-spec 2.6). The caller
/// stores `new_id + 1` as the counter. `None` when the counter cannot grow.
pub fn next_set_id(counter: u64, groups: &[Group]) -> Option<u64> {
    let highest = groups.iter().filter_map(stored_set).max().unwrap_or(0);
    let id = counter.max(highest.checked_add(1)?).max(1);
    id.checked_add(1).map(|_| id)
}

/// The value a set shares (tabs-spec 2.3): the selected tab's stored rect, monitor, preferred size,
/// Range, fold, dock flags, title side and opacity (one window, one value).
fn copy_shared(from: &Group, to: &mut Group) {
    to.monitor = from.monitor.clone();
    to.rect = from.rect;
    to.preferred_size_dip = from.preferred_size_dip;
    to.roll = from.roll;
    to.dock_top = from.dock_top;
    to.dock_bottom = from.dock_bottom;
    to.title_side = from.title_side;
    to.title_side_last = from.title_side_last;
    to.swipe_index = from.swipe_index;
    to.all_pages = from.all_pages;
    to.opacity = from.opacity;
}

/// A group with the shared and membership fields neutralised: what must survive every operation
/// unchanged (title, items, colour slots, Gate folder and view, sort, zoom; tabs-spec 2.4, 7).
fn private_view(group: &Group) -> Group {
    let mut g = group.clone();
    g.tab = None;
    g.monitor = String::new();
    g.rect = Rect::default();
    g.preferred_size_dip = None;
    g.roll = RollState::Expanded;
    g.dock_top = false;
    g.dock_bottom = false;
    g.title_side = -1;
    g.title_side_last = -1;
    g.swipe_index = 0;
    g.all_pages = false;
    g.opacity = 255;
    g
}

/// Copy the shared value of the selected tab to every other member; true when anything changed.
/// Called after a geometry change so the whole set stays equal (tabs-spec 2.3). Membership keys
/// are not touched.
pub fn sync_shared(groups: &mut [Group], member: u64) -> bool {
    let Some(group) = groups.iter().find(|g| g.id == member).cloned() else { return false };
    let Some(set) = set_for(groups, &group) else { return false };
    if set.selected_id() != member { return false }
    let mut changed = false;
    for g in groups.iter_mut().filter(|g| g.id != member && set.tabs.contains(&g.id)) {
        let before = g.clone();
        copy_shared(&group, g);
        changed |= *g != before;
    }
    changed
}

// ---- Clear view exclusion (tabs-spec 2.5) --------------------------------------------------------

/// A set stays visible in Clear view when any tab is flagged.
pub fn kept_in_clear_view(members: &[u64], flagged: impl Fn(u64) -> bool) -> bool {
    members.iter().any(|id| flagged(*id))
}

// ---- documents and operations (tabs-spec 3.5, 5.3, 6, 7) ------------------------------------------

/// The part of a layout tab operations read and write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabDoc {
    pub groups: Vec<Group>,
    /// Layout counter for new set ids (`Layout.last_tab_set_id`).
    pub next_tab_set: u64,
}

/// What a join needs to know besides the data (tabs-spec 3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JoinPolicy {
    /// `TabsCreate`.
    pub create: bool,
    /// `LabelMode` is not Never.
    pub titles: bool,
}

/// Where a Corral that leaves a set lands (DIPs relative to the work-area origin of `monitor`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    pub monitor: String,
    pub rect: Rect,
}

fn folded(group: &Group) -> bool {
    group.roll != RollState::Expanded
}

struct Work {
    groups: Vec<Group>,
    sets: Vec<TabSet>,
    next: u64,
    touched: BTreeSet<u64>,
    cleared: BTreeSet<u64>,
}

impl Work {
    fn new(doc: &TabDoc) -> Self {
        Self { groups: doc.groups.clone(), sets: normalize_sets(&doc.groups), next: doc.next_tab_set,
            touched: BTreeSet::new(), cleared: BTreeSet::new() }
    }

    fn pos(&self, id: u64) -> Result<usize, TabError> {
        self.groups.iter().position(|g| g.id == id).ok_or(TabError::MissingMember(id))
    }

    fn set_of(&self, id: u64) -> Option<usize> {
        self.sets.iter().position(|s| s.tabs.contains(&id))
    }

    fn spread_shared(&mut self, from: u64, to: &[u64]) -> Result<(), TabError> {
        let source = self.groups[self.pos(from)?].clone();
        for id in to {
            if *id == from { continue }
            let p = self.pos(*id)?;
            copy_shared(&source, &mut self.groups[p]);
        }
        Ok(())
    }

    /// Write contiguous membership for touched sets, clear dissolved ones, raise the counter, check
    /// the result (tabs-spec 7).
    fn finish(mut self, before: &TabDoc, removed: &[u64]) -> Result<TabDoc, TabError> {
        for set in &self.sets {
            if !self.touched.contains(&set.id) { continue }
            for (i, id) in set.tabs.iter().enumerate() {
                if let Some(g) = self.groups.iter_mut().find(|g| g.id == *id) {
                    g.tab = Some(TabMembership { set_id: set.id, index: i as u32, selected: i == set.selected });
                }
            }
        }
        for g in self.groups.iter_mut().filter(|g| self.cleared.contains(&g.id)) { g.tab = None }
        let highest = self.groups.iter().filter_map(stored_set).max().unwrap_or(0);
        let need = highest.checked_add(1).ok_or(TabError::Overflow)?;
        let after = TabDoc { groups: self.groups, next_tab_set: self.next.max(need).max(before.next_tab_set) };
        validate(before, &after, removed)?;
        Ok(after)
    }
}

/// Can `source` join `target`? Checks only the refusals of tabs-spec 3.3; the pointer rules live in
/// [`drop_zone`]. Different monitors, a folded source and Folder Gates are allowed on purpose.
pub fn check_join(doc: &TabDoc, target: u64, source: u64, policy: &JoinPolicy) -> Result<(), TabError> {
    let t = doc.groups.iter().find(|g| g.id == target).ok_or(TabError::MissingMember(target))?;
    doc.groups.iter().find(|g| g.id == source).ok_or(TabError::MissingMember(source))?;
    if !policy.create { return Err(TabError::Refused(Refusal::CreateOff)) }
    if target == source { return Err(TabError::Refused(Refusal::SelfTarget)) }
    let set = set_for(&doc.groups, t);
    if set.as_ref().is_some_and(|s| s.tabs.contains(&source)) { return Err(TabError::Refused(Refusal::SameSet)) }
    if !policy.titles { return Err(TabError::Refused(Refusal::TitleHidden)) }
    // The live value of a set is its selected tab's.
    let live = set.map_or(target, |s| s.selected_id());
    if doc.groups.iter().find(|g| g.id == live).is_some_and(folded) { return Err(TabError::Refused(Refusal::TargetFolded)) }
    Ok(())
}

/// Join `source` (a Corral or a whole set) into `target`'s place (tabs-spec 3.5). `insertion` is the
/// index among the target's tabs (`None` = append). The target's rect, monitor, Range, fold and dock
/// state win; the set's selected tab becomes the source's selected tab.
pub fn join(doc: &TabDoc, target: u64, source: u64, insertion: Option<usize>, policy: &JoinPolicy) -> Result<TabDoc, TabError> {
    check_join(doc, target, source, policy)?;
    let mut w = Work::new(doc);
    let (t_set, s_set) = (w.set_of(target), w.set_of(source));
    let s_set_id = s_set.map(|i| w.sets[i].id);
    let (mut tabs, t_selected) = match t_set {
        Some(i) => (w.sets[i].tabs.clone(), w.sets[i].selected_id()),
        None => (vec![target], target),
    };
    let (incoming, s_selected) = match s_set {
        Some(i) => (w.sets[i].tabs.clone(), w.sets[i].selected_id()),
        None => (vec![source], source),
    };
    let set_id = match t_set {
        Some(i) => w.sets[i].id,
        None => {
            let id = next_set_id(w.next, &w.groups).ok_or(TabError::Overflow)?;
            w.next = id.checked_add(1).ok_or(TabError::Overflow)?;
            id
        }
    };
    let at = insertion.unwrap_or(tabs.len()).min(tabs.len());
    for (k, id) in incoming.iter().enumerate() { tabs.insert(at + k, *id) }
    let selected = tabs.iter().position(|id| *id == s_selected).unwrap_or(0);
    // The target's place wins: its live value goes to every tab.
    w.spread_shared(t_selected, &tabs)?;
    w.sets.retain(|s| s.id != set_id && Some(s.id) != s_set_id);
    w.sets.push(TabSet { id: set_id, tabs, selected });
    w.touched.insert(set_id);
    w.finish(doc, &[])
}

/// A set onto a set (or onto a plain Corral): the same join with the source's tabs in their order.
pub fn merge(doc: &TabDoc, target: u64, source: u64, insertion: Option<usize>, policy: &JoinPolicy) -> Result<TabDoc, TabError> {
    let s = doc.groups.iter().find(|g| g.id == source).ok_or(TabError::MissingMember(source))?;
    if set_for(&doc.groups, s).is_none() { return Err(TabError::NotATab(source)) }
    join(doc, target, source, insertion, policy)
}

/// Select `member` (a tab of a set). The shared value of the tab that was shown moves to every tab,
/// so nothing jumps (tabs-spec 2.3, 4). Selecting the shown tab changes nothing.
pub fn select(doc: &TabDoc, member: u64) -> Result<TabDoc, TabError> {
    let mut w = Work::new(doc);
    let i = w.set_of(member).ok_or(TabError::NotATab(member))?;
    if w.sets[i].selected_id() == member { return Ok(doc.clone()) }
    let (old, tabs, id) = (w.sets[i].selected_id(), w.sets[i].tabs.clone(), w.sets[i].id);
    w.sets[i].selected = tabs.iter().position(|t| *t == member).unwrap_or(0);
    w.spread_shared(old, &tabs)?;
    w.touched.insert(id);
    w.finish(doc, &[])
}

/// Move `member` to `new_index` within its set; the selected tab stays the same Corral.
pub fn reorder(doc: &TabDoc, member: u64, new_index: usize) -> Result<TabDoc, TabError> {
    let mut w = Work::new(doc);
    let i = w.set_of(member).ok_or(TabError::NotATab(member))?;
    let mut tabs = w.sets[i].tabs.clone();
    let from = tabs.iter().position(|t| *t == member).unwrap_or(0);
    let to = new_index.min(tabs.len() - 1);
    if from == to { return Ok(doc.clone()) }
    let selected_id = w.sets[i].selected_id();
    let tab = tabs.remove(from);
    tabs.insert(to, tab);
    w.sets[i].selected = tabs.iter().position(|t| *t == selected_id).unwrap_or(0);
    w.sets[i].tabs = tabs.clone();
    w.spread_shared(selected_id, &tabs)?;
    w.touched.insert(w.sets[i].id);
    w.finish(doc, &[])
}

fn leave_set(w: &mut Work, set_index: usize, member: u64) -> Result<(), TabError> {
    let set = w.sets[set_index].clone();
    let live = set.selected_id();
    let at = set.tabs.iter().position(|t| *t == member).unwrap_or(0);
    let remaining: Vec<u64> = set.tabs.iter().copied().filter(|t| *t != member).collect();
    // The set's value goes to the tabs that stay.
    w.spread_shared(live, &remaining)?;
    if remaining.len() < 2 {
        w.sets.remove(set_index);
        w.cleared.extend(remaining.iter().copied());
    } else {
        // The tab now at the same index, else the previous one, when the torn tab was selected.
        let selected = if member == live { at.min(remaining.len() - 1) } else { remaining.iter().position(|t| *t == live).unwrap_or(0) };
        w.sets[set_index] = TabSet { id: set.id, tabs: remaining, selected };
        w.touched.insert(set.id);
    }
    Ok(())
}

fn settle_alone(g: &mut Group, to: &Placement) {
    g.tab = None;
    g.monitor = to.monitor.clone();
    g.rect = to.rect;
    g.roll = RollState::Expanded;
    g.dock_top = false;
    g.dock_bottom = false;
}

/// The tab leaves its set and becomes a Corral at `to`, unfolded and undocked (tab-off, "Remove from
/// tabs"; tabs-spec 5.3, 6). Items, title and colours stay; `preferred_size_dip` is the set's.
pub fn remove_tab(doc: &TabDoc, member: u64, to: &Placement) -> Result<TabDoc, TabError> {
    let mut w = Work::new(doc);
    let i = w.set_of(member).ok_or(TabError::NotATab(member))?;
    let shared = w.groups[w.pos(w.sets[i].selected_id())?].clone();
    // The set's value goes to the tabs that stay before the leaving tab takes its new place.
    leave_set(&mut w, i, member)?;
    let p = w.pos(member)?;
    copy_shared(&shared, &mut w.groups[p]);
    settle_alone(&mut w.groups[p], to);
    w.cleared.insert(member);
    w.finish(doc, &[])
}

/// Every tab becomes its own Corral (tabs-spec 6): the selected tab keeps the set's place, the others
/// cascade [`CASCADE_DIP`] per tab in display order, unfolded and undocked. `area` gives the
/// on-screen area (DIPs, work-area origin) of a monitor; each moved rect is shifted inside it.
pub fn separate_all(doc: &TabDoc, member: u64, area: &dyn Fn(&str) -> Option<Rect>) -> Result<TabDoc, TabError> {
    let mut w = Work::new(doc);
    let i = w.set_of(member).ok_or(TabError::NotATab(member))?;
    let set = w.sets[i].clone();
    let shared = w.groups[w.pos(set.selected_id())?].clone();
    let mut step = 0;
    for id in &set.tabs {
        let p = w.pos(*id)?;
        copy_shared(&shared, &mut w.groups[p]);
        w.groups[p].tab = None;
        if *id == set.selected_id() { continue }
        step += 1;
        let moved = shared.rect.offset(CASCADE_DIP * step, CASCADE_DIP * step);
        let rect = area(&shared.monitor).map_or(moved, |a| shift_into(moved, a));
        settle_alone(&mut w.groups[p], &Placement { monitor: shared.monitor.clone(), rect });
    }
    w.sets.remove(i);
    w.cleared.extend(set.tabs.iter().copied());
    w.finish(doc, &[])
}

/// The Corral is deleted by the existing delete command: it leaves the data and its set shrinks by the
/// same rules (tabs-spec 7). Plain Corrals just disappear.
pub fn delete(doc: &TabDoc, id: u64) -> Result<TabDoc, TabError> {
    let mut w = Work::new(doc);
    w.pos(id)?;
    if let Some(i) = w.set_of(id) { leave_set(&mut w, i, id)? }
    w.groups.retain(|g| g.id != id);
    w.finish(doc, &[id])
}

/// For imported data only (an import is a conversion, not a load): every set is written in canonical
/// form, one-member sets dissolve and the counter passes every id seen.
pub fn canonicalize(doc: &TabDoc) -> TabDoc {
    let mut groups = doc.groups.clone();
    let sets = normalize_sets(&groups);
    let highest = groups.iter().filter_map(stored_set).max().unwrap_or(0);
    for g in groups.iter_mut() { g.tab = None }
    for set in &sets {
        for (i, id) in set.tabs.iter().enumerate() {
            if let Some(g) = groups.iter_mut().find(|g| g.id == *id) {
                g.tab = Some(TabMembership { set_id: set.id, index: i as u32, selected: i == set.selected });
            }
        }
    }
    TabDoc { groups, next_tab_set: doc.next_tab_set.max(highest.saturating_add(1)).max(1) }
}

/// Whether a tab edit may be written now: not on a locked desktop and not while a display transition is
/// pending (the live layout is then not the one that is stored).
pub fn edit_allowed(locked: bool, display_transition_pending: bool) -> Result<(), TabError> {
    if locked { Err(TabError::Locked) } else if display_transition_pending { Err(TabError::Busy) } else { Ok(()) }
}

/// Undo what a gesture changed, and only that: for `ids` (the Corrals of the pressed set) the membership
/// and the shared value come back from `before`; titles, items, colours and every other Corral keep their
/// live state, so a change made by a timer during the gesture (Round-up assigning an item, a rename) stays.
/// The set counter never goes down.
pub fn restore_members(live: &TabDoc, before: &TabDoc, ids: &[u64]) -> TabDoc {
    let mut doc = live.clone();
    for g in doc.groups.iter_mut().filter(|g| ids.contains(&g.id)) {
        if let Some(old) = before.groups.iter().find(|o| o.id == g.id) {
            copy_shared(old, g);
            g.tab = old.tab;
        }
    }
    doc.next_tab_set = doc.next_tab_set.max(before.next_tab_set);
    doc
}

/// Move `rect` (without resizing) so it lies inside `area`; a rect larger than the area aligns to
/// the area's top left.
pub fn shift_into(rect: Rect, area: Rect) -> Rect {
    let dx = if rect.right > area.right { area.right - rect.right } else { 0 };
    let dy = if rect.bottom > area.bottom { area.bottom - rect.bottom } else { 0 };
    let moved = rect.offset(dx, dy);
    let dx = if moved.left < area.left { area.left - moved.left } else { 0 };
    let dy = if moved.top < area.top { area.top - moved.top } else { 0 };
    moved.offset(dx, dy)
}

/// Section 7 invariants. `removed` lists the Corrals the operation deletes on purpose.
pub fn validate(before: &TabDoc, after: &TabDoc, removed: &[u64]) -> Result<(), TabError> {
    let mut seen = BTreeSet::new();
    for g in &after.groups {
        if !seen.insert(g.id) { return Err(TabError::Invalid("a Corral appears twice")) }
    }
    for g in &before.groups {
        let Some(now) = after.groups.iter().find(|a| a.id == g.id) else {
            if removed.contains(&g.id) { continue }
            return Err(TabError::Invalid("a Corral was lost"));
        };
        if removed.contains(&g.id) { return Err(TabError::Invalid("a Corral was not removed")) }
        if private_view(g) != private_view(now) { return Err(TabError::Invalid("a Corral's own data changed")) }
    }
    if after.groups.iter().any(|a| !before.groups.iter().any(|g| g.id == a.id)) {
        return Err(TabError::Invalid("a Corral appeared"));
    }
    let stored = |doc: &TabDoc, set: u64| -> Vec<(u64, u32, bool)> {
        let mut v: Vec<_> = doc.groups.iter().filter_map(|g| g.tab.filter(|t| t.set_id == set).map(|t| (g.id, t.index, t.selected))).collect();
        v.sort();
        v
    };
    let ids: BTreeSet<u64> = after.groups.iter().filter_map(stored_set).collect();
    for set in &ids {
        let now = stored(after, *set);
        if stored(before, *set) == now { continue } // untouched sets are read as stored
        if now.len() < 2 { return Err(TabError::Invalid("a set has fewer than two tabs")) }
        let mut indices: Vec<u32> = now.iter().map(|e| e.1).collect();
        indices.sort();
        if indices.iter().enumerate().any(|(i, v)| *v != i as u32) { return Err(TabError::Invalid("tab indices are not contiguous")) }
        if now.iter().filter(|e| e.2).count() != 1 { return Err(TabError::Invalid("a set needs exactly one selected tab")) }
    }
    if ids.iter().any(|set| after.next_tab_set <= *set) { return Err(TabError::Invalid("the set counter is not above every set id")) }
    if after.next_tab_set < before.next_tab_set { return Err(TabError::Invalid("the set counter went down")) }
    Ok(())
}

// ---- joining by pointer (tabs-spec 3.1, 3.2, 3.4) --------------------------------------------------

/// A visible Corral strip that could take a drop, topmost first in the list passed to [`drop_zone`].
/// The caller leaves out the dragged Corral and the tabs of its set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StripInfo {
    pub id: u64,
    /// The full strip rect, buttons and resize bands included (same space as the pointer).
    pub strip: Rect,
    /// The painted tab cells in the same space (empty for a plain Corral or style 0).
    pub cells: Vec<TabCell>,
    /// Tabs in the target's set (1 for a plain Corral).
    pub tabs: usize,
    /// Why this target refuses the drop, if it does.
    pub refusal: Option<Refusal>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZoneHit {
    pub target: u64,
    /// Index among the target's tabs where the dropped tabs go.
    pub insertion: usize,
    /// The target is already a set.
    pub is_set: bool,
}

/// The target under the pointer: the topmost strip containing it (the dragged window's own rect is
/// irrelevant). A refusing strip hides what lies under it, so there is no zone then.
pub fn drop_zone(strips: &[StripInfo], pointer: (i32, i32)) -> Option<ZoneHit> {
    let strip = strips.iter().find(|s| s.strip.contains(pointer.0, pointer.1))?;
    if strip.refusal.is_some() { return None }
    Some(ZoneHit { target: strip.id, insertion: insertion_index(&strip.cells, pointer.0, strip.tabs), is_set: strip.tabs > 1 })
}

/// Before tab `i` when the pointer is left of its centre, else after it. Empty strip area, a plain
/// target and style 0 (no cells) append.
pub fn insertion_index(cells: &[TabCell], x: i32, count: usize) -> usize {
    match cells.iter().find(|c| x >= c.hit.left && x < c.hit.right) {
        Some(c) => {
            let centre = c.hit.left + c.hit.width() / 2;
            c.index as usize + usize::from(x >= centre)
        }
        None => count,
    }
}

/// Where the insertion mark of an armed set is drawn: the left edge of tab `index`, or the right edge
/// of the last tab when the drop appends. `None` when that place is scrolled out of the strip.
pub fn insertion_mark(cells: &[TabCell], index: usize, count: usize) -> Option<i32> {
    if let Some(c) = cells.iter().find(|c| c.index as usize == index) { return Some(c.hit.left) }
    let last = cells.last()?;
    (index == count && last.index as usize + 1 == count).then_some(last.hit.right)
}

/// Arming of a drop zone: it arms after the pointer stayed on the same target [`ARM_MS`]; leaving
/// disarms at once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArmState {
    pub target: Option<u64>,
    pub since_ms: u64,
}

pub fn arm_step(state: ArmState, zone: Option<u64>, now_ms: u64) -> (ArmState, bool) {
    let next = if zone.is_some() && zone == state.target { state } else { ArmState { target: zone, since_ms: now_ms } };
    (next, zone.is_some() && now_ms.saturating_sub(next.since_ms) >= ARM_MS)
}

/// The label near the pointer while armed (tabs-spec 3.2).
pub fn join_label(target_tabs: usize, source_tabs: usize) -> &'static str {
    if source_tabs > 1 { "Merge tabs" } else if target_tabs > 1 { "Add to tabs" } else { "Combine as tabs" }
}

/// The confirmation sentence (tabs-spec 3.4): titles are the selected tabs', counts are tabs per side.
pub fn join_sentence(target: &str, target_tabs: usize, source: &str, source_tabs: usize) -> String {
    let side = |title: &str, n: usize| if n > 1 { format!("\"{title}\" ({n} tabs)") } else { format!("\"{title}\"") };
    let total = target_tabs.max(1) + source_tabs.max(1);
    let total = if total == 2 { "two tabs".to_string() } else { format!("{total} tabs") };
    format!("{} and {} will share one place, as {total}. Hold Shift and drag a tab away to separate them again.",
        side(target, target_tabs), side(source, source_tabs))
}

// ---- reordering and tearing off (tabs-spec 5) -------------------------------------------------------

/// Drag threshold around the press point, per axis in pixels: half of the system drag rectangle, at
/// least 4 DIP.
pub fn drag_threshold(system_cx: i32, system_cy: i32, dpi: u32) -> (i32, i32) {
    ((system_cx / 2).max(px(4.0, dpi)), (system_cy / 2).max(px(4.0, dpi)))
}

/// The pointer left the threshold rectangle around `press`.
pub fn beyond_threshold(press: (i32, i32), now: (i32, i32), threshold: (i32, i32)) -> bool {
    (now.0 - press.0).abs() > threshold.0 || (now.1 - press.1).abs() > threshold.1
}

/// Euclidean distance in pixels from the pointer to the strip (0 inside it).
pub fn tear_distance(strip: Rect, pointer: (i32, i32)) -> f64 {
    let dx = (strip.left - pointer.0).max(pointer.0 - (strip.right - 1)).max(0) as f64;
    let dy = (strip.top - pointer.1).max(pointer.1 - (strip.bottom - 1)).max(0) as f64;
    dx.hypot(dy)
}

/// The tab tears off when the pointer is farther than [`TEAR_DIP`] from the strip.
pub fn tears(strip: Rect, pointer: (i32, i32), dpi: u32) -> bool {
    tear_distance(strip, pointer) > TEAR_DIP * dpi as f64 / 96.0
}

/// Slot index for the dragged tab: it moves one place each time its centre crosses a neighbour's
/// centre in the live strip (`centres` = centre x of every tab cell in display order, the dragged
/// tab's own cell at `current`).
pub fn reorder_target(centres: &[i32], current: usize, dragged_centre: i32) -> usize {
    let mut at = current.min(centres.len().saturating_sub(1));
    while at + 1 < centres.len() && dragged_centre >= centres[at + 1] { at += 1 }
    while at > 0 && dragged_centre <= centres[at - 1] { at -= 1 }
    at
}

/// Direction the strip scrolls while a tab is dragged near an end: -1, 0 or +1.
pub fn autoscroll_dir(strip: Rect, x: i32, dpi: u32) -> i32 {
    let zone = px(AUTOSCROLL_DIP, dpi);
    if x < strip.left + zone { -1 } else if x >= strip.right - zone { 1 } else { 0 }
}

/// One tab per [`AUTOSCROLL_MS`] while the pointer stays in the zone. Returns the new last-step time
/// and the step to take now (0 or +-1).
pub fn autoscroll_step(last_ms: Option<u64>, now_ms: u64, dir: i32) -> (Option<u64>, i32) {
    if dir == 0 { return (None, 0) }
    match last_ms {
        Some(t) if now_ms.saturating_sub(t) < AUTOSCROLL_MS => (last_ms, 0),
        _ => (Some(now_ms), dir),
    }
}

// ---- hover switching and spring loading (tabs-spec 4) ------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HoverState {
    /// The inactive tab the timer belongs to.
    pub tab: Option<u64>,
    pub since_ms: u64,
    /// The timer has fired or was cancelled for this visit: the pointer must leave and re-enter.
    pub spent: bool,
}

impl HoverState {
    /// After any other switch the tab under a stationary pointer never re-arms.
    pub fn after_switch(under: Option<u64>, now_ms: u64) -> Self {
        Self { tab: under, since_ms: now_ms, spent: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HoverInput {
    /// The inactive tab under the pointer.
    pub tab: Option<u64>,
    pub now_ms: u64,
    /// Wait for this tab ([`TabSettings::hover_wait`]); `None` = no switching.
    pub wait_ms: Option<u32>,
    /// A press, menu, tooltip, Corral move or tab drag is going on, or the set is faded by Clear view.
    pub cancel: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverOut {
    Idle,
    /// Check again after this many milliseconds.
    Wait(u32),
    Select(u64),
}

pub fn hover_switch_step(state: HoverState, input: HoverInput) -> (HoverState, HoverOut) {
    let Some(wait) = input.wait_ms else { return (HoverState::default(), HoverOut::Idle) };
    if input.cancel { return (HoverState::after_switch(input.tab, input.now_ms), HoverOut::Idle) }
    let Some(tab) = input.tab else { return (HoverState::default(), HoverOut::Idle) };
    if state.tab != Some(tab) {
        let entered = HoverState { tab: Some(tab), since_ms: input.now_ms, spent: wait == 0 };
        return (entered, if wait == 0 { HoverOut::Select(tab) } else { HoverOut::Wait(wait) });
    }
    if state.spent { return (state, HoverOut::Idle) }
    let elapsed = input.now_ms.saturating_sub(state.since_ms);
    if elapsed >= wait as u64 {
        (HoverState { spent: true, ..state }, HoverOut::Select(tab))
    } else {
        (state, HoverOut::Wait((wait as u64 - elapsed) as u32))
    }
}


fn empty(rect: Rect) -> bool {
    rect.left >= rect.right || rect.top >= rect.bottom
}

fn sat(value: i64) -> i32 {
    value.clamp(i32::MIN as i64, i32::MAX as i64) as i32
}

// ---- tab strip layout (corral-painting-spec 7) ------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabCell {
    pub index: u32,
    pub hit: Rect,
    pub label: Rect,
}

/// Tab widths (corral-painting-spec 7): text plus 2 x 10 DIP, clamped to 48..160 DIP.
pub const TAB_MIN_DIP: f64 = 48.0;
pub const TAB_MAX_DIP: f64 = 160.0;
pub const TAB_PAD_DIP: f64 = 10.0;

/// Natural width of a tab whose title measures `text_w` px.
pub fn tab_width(text_w: i32, dpi: u32) -> i32 {
    (text_w.max(0) + 2 * px(TAB_PAD_DIP, dpi)).clamp(px(TAB_MIN_DIP, dpi), px(TAB_MAX_DIP, dpi))
}

/// Widths that fit `avail`: the natural ones, else all shrunk by the same amount down to `min`.
/// `None` when even all-minimum tabs do not fit (the strip then scrolls).
fn shrink_equally(natural: &[i32], avail: i32, min: i32) -> Option<Vec<i32>> {
    let total: i64 = natural.iter().map(|w| *w as i64).sum();
    if total <= avail as i64 {
        return Some(natural.to_vec());
    }
    let sum_at = |d: i32| -> i64 { natural.iter().map(|w| (*w - d).max(min) as i64).sum() };
    let widest = natural.iter().copied().max().unwrap_or(0);
    if sum_at(widest) > avail as i64 {
        return None;
    }
    let (mut lo, mut hi) = (0, widest);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if sum_at(mid) <= avail as i64 { hi = mid } else { lo = mid + 1 }
    }
    Some(natural.iter().map(|w| (*w - lo).max(min)).collect())
}

/// The visible tabs of a strip. `span` is the x range between the button clusters (`layout_title`),
/// `text_widths` the measured title of every member, `active` the selected member's index. When the
/// tabs exceed the span they shrink equally down to 48 DIP, then the strip scrolls by whole tabs so the
/// active tab is fully visible; hidden tabs have no cell (the Corral menu lists all). Menu only (0), a
/// single tab or a span narrower than one tab gives no cells.
pub fn tab_cells(strip: Rect, span: (i32, i32), text_widths: &[i32], active: usize, style: u32, dpi: u32) -> Vec<TabCell> {
    let count = text_widths.len();
    let avail = span.1 as i64 - span.0 as i64;
    let min = px(TAB_MIN_DIP, dpi);
    if count < 2 || !(1..=3).contains(&style) || empty(strip) || avail < min as i64 {
        return Vec::new();
    }
    let avail = avail.min(i32::MAX as i64) as i32;
    let natural: Vec<i32> = text_widths.iter().map(|w| tab_width(*w, dpi)).collect();
    let (widths, first) = match shrink_equally(&natural, avail, min) {
        Some(widths) => (widths, 0),
        None => {
            // All tabs at the minimum: start at the earliest tab that still shows the active one whole.
            let active = active.min(count - 1);
            let fits = avail / min.max(1);
            let first = (active as i32 + 1 - fits).max(0) as usize;
            (vec![min; count], first)
        }
    };
    let pad = px(TAB_PAD_DIP, dpi);
    let mut x = span.0 as i64;
    let mut cells = Vec::new();
    for (i, width) in widths.iter().enumerate().skip(first) {
        let right = x + *width as i64;
        if right > span.1 as i64 {
            break;
        }
        let hit = Rect::new(sat(x), strip.top, sat(right), strip.bottom);
        let inset = pad.min(hit.width() / 2);
        cells.push(TabCell { index: i as u32, hit, label: Rect::new(hit.left + inset, strip.top, hit.right - inset, strip.bottom) });
        x = right;
    }
    cells
}

pub fn hit_tab(cells: &[TabCell], cursor: (i32, i32)) -> Option<u32> {
    cells
        .iter()
        .find(|cell| cell.hit.contains(cursor.0, cursor.1))
        .map(|cell| cell.index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GroupContent, Layout};
    use std::collections::BTreeMap;

    const DPIS: [u32; 5] = [96, 120, 144, 192, 240];

    fn policy() -> JoinPolicy {
        JoinPolicy { create: true, titles: true }
    }

    /// `n` plain Corrals with ids 1..=n, distinct titles, rects, items and colours.
    fn doc(n: usize) -> TabDoc {
        let mut layout = Layout::new();
        for i in 0..n {
            let id = layout.add_group(format!("Corral {}", i + 1), format!("MON{}", i % 2), Rect::new(100 * i as i32, 50, 100 * i as i32 + 400, 300),
                GroupContent::Items { items: vec![format!("item-{}-a", i + 1), format!("item-{}-b", i + 1)] });
            let g = layout.group_mut(id).unwrap();
            g.style.color = Some(0x10 + i as u32);
            g.preferred_size_dip = Some((400 + i as i32, 250));
            g.swipe_index = i as i32;
        }
        TabDoc { groups: layout.groups, next_tab_set: layout.last_tab_set_id }
    }

    fn g(doc: &TabDoc, id: u64) -> &Group {
        doc.groups.iter().find(|g| g.id == id).unwrap()
    }

    fn gm(doc: &mut TabDoc, id: u64) -> &mut Group {
        doc.groups.iter_mut().find(|g| g.id == id).unwrap()
    }

    fn sets(doc: &TabDoc) -> Vec<TabSet> {
        normalize_sets(&doc.groups)
    }

    fn private_multiset(doc: &TabDoc) -> Vec<Group> {
        let mut v: Vec<Group> = doc.groups.iter().map(private_view).collect();
        v.sort_by_key(|g| g.id);
        v
    }

    /// Build a set directly in the stored form: `ids` in display order, `selected` shown.
    fn with_set(mut d: TabDoc, set_id: u64, ids: &[u64], selected: u64) -> TabDoc {
        for (i, id) in ids.iter().enumerate() {
            gm(&mut d, *id).tab = Some(TabMembership { set_id, index: i as u32, selected: *id == selected });
        }
        d.next_tab_set = d.next_tab_set.max(set_id + 1);
        d
    }

    // ---- 1, 2: joining -----------------------------------------------------------------------

    #[test]
    fn join_plain_onto_plain_makes_a_set_in_the_targets_place() {
        let mut d = doc(3);
        gm(&mut d, 1).roll = RollState::Expanded;
        gm(&mut d, 1).dock_top = true;
        let after = join(&d, 1, 2, None, &policy()).unwrap();
        assert_eq!(sets(&after), vec![TabSet { id: 1, tabs: vec![1, 2], selected: 1 }], "source selected");
        for id in [1, 2] {
            let t = g(&after, id);
            assert_eq!((t.rect, &t.monitor, t.preferred_size_dip, t.swipe_index, t.dock_top), (g(&d, 1).rect, &g(&d, 1).monitor, g(&d, 1).preferred_size_dip, g(&d, 1).swipe_index, true));
        }
        assert_eq!(g(&after, 3), g(&d, 3), "others untouched");
        assert_eq!(private_multiset(&after), private_multiset(&d), "items, titles and colour slots unchanged");
        assert_eq!(after.next_tab_set, 2, "counter = used id + 1");
    }

    #[test]
    fn join_into_a_set_at_every_insertion_index_and_append() {
        let base = with_set(doc(5), 7, &[1, 2, 3], 2);
        for (at, expect) in [(Some(0), vec![4, 1, 2, 3]), (Some(1), vec![1, 4, 2, 3]), (Some(2), vec![1, 2, 4, 3]), (Some(3), vec![1, 2, 3, 4]), (None, vec![1, 2, 3, 4]), (Some(99), vec![1, 2, 3, 4])] {
            let after = join(&base, 2, 4, at, &policy()).unwrap();
            let s = sets(&after);
            assert_eq!(s.len(), 1);
            assert_eq!((s[0].id, s[0].tabs.clone(), s[0].selected_id()), (7, expect, 4), "insertion {at:?}");
            assert_eq!(after.next_tab_set, 8, "no new id for an existing set");
            assert_eq!(g(&after, 4).rect, g(&base, 2).rect, "the live value of the set (selected tab) wins");
        }
    }

    #[test]
    fn merge_keeps_order_target_id_and_the_sources_selected_tab() {
        let base = with_set(with_set(doc(6), 3, &[1, 2], 1), 9, &[3, 4, 5], 5);
        let after = merge(&base, 1, 3, Some(1), &policy()).unwrap();
        let s = sets(&after);
        assert_eq!(s, vec![TabSet { id: 3, tabs: vec![1, 3, 4, 5, 2], selected: 3 }], "target id kept, source id 9 retired, source selected = tab 5");
        assert!(after.groups.iter().all(|g| g.tab.is_none_or(|t| t.set_id == 3)));
        assert_eq!(after.next_tab_set, 10, "counter never decreases");
        assert_eq!(merge(&base, 1, 6, None, &policy()), Err(TabError::NotATab(6)), "merge needs a set");
        // a set onto a plain Corral takes a new id and the source's tabs in order
        let onto_plain = join(&base, 6, 3, None, &policy()).unwrap();
        assert_eq!(sets(&onto_plain).iter().find(|s| s.tabs.contains(&6)).unwrap().tabs, vec![6, 3, 4, 5]);
        assert_eq!(private_multiset(&after), private_multiset(&base));
    }

    #[test]
    fn set_ids_are_never_reused_over_random_join_and_leave_sequences() {
        let mut rng = Rng(7);
        for run in 0..1000 {
            let mut d = doc(5);
            let mut used = BTreeSet::new();
            for _ in 0..8 {
                let (a, b) = (1 + rng.below(5), 1 + rng.below(5));
                let before_sets: BTreeSet<u64> = sets(&d).iter().map(|s| s.id).collect();
                if rng.below(2) == 0 {
                    if let Ok(next) = join(&d, a, b, None, &policy()) {
                        for s in sets(&next).iter().filter(|s| !before_sets.contains(&s.id)) {
                            assert!(used.insert(s.id), "run {run}: set id {} reused", s.id);
                        }
                        d = next;
                    }
                } else if let Ok(next) = remove_tab(&d, a, &Placement { monitor: "MON0".into(), rect: Rect::new(0, 0, 100, 100) }) {
                    d = next;
                }
            }
        }
    }

    // ---- 3: refusals ---------------------------------------------------------------------------

    #[test]
    fn joins_are_refused_exactly_where_the_spec_says() {
        let d = with_set(doc(5), 2, &[3, 4], 3);
        let refused = |d: &TabDoc, t, s, p: JoinPolicy| join(d, t, s, None, &p).unwrap_err();
        assert_eq!(refused(&d, 1, 2, JoinPolicy { create: false, titles: true }), TabError::Refused(Refusal::CreateOff));
        assert_eq!(refused(&d, 1, 2, JoinPolicy { create: true, titles: false }), TabError::Refused(Refusal::TitleHidden));
        assert_eq!(refused(&d, 1, 1, policy()), TabError::Refused(Refusal::SelfTarget));
        assert_eq!(refused(&d, 3, 4, policy()), TabError::Refused(Refusal::SameSet), "tab of the target's own set");
        assert_eq!(refused(&d, 4, 3, policy()), TabError::Refused(Refusal::SameSet));
        let mut folded = d.clone();
        gm(&mut folded, 1).roll = RollState::Manual;
        assert_eq!(refused(&folded, 1, 2, policy()), TabError::Refused(Refusal::TargetFolded), "persistent fold");
        gm(&mut folded, 1).roll = RollState::Docked;
        assert_eq!(refused(&folded, 1, 2, policy()), TabError::Refused(Refusal::TargetFolded), "dock-derived fold");
        let mut set_folded = d.clone();
        gm(&mut set_folded, 3).roll = RollState::Manual;
        assert_eq!(refused(&set_folded, 4, 1, policy()), TabError::Refused(Refusal::TargetFolded), "a set is folded as a whole");
        // allowed on purpose: other monitor, folded source, Gates in either role
        let mut ok = d.clone();
        gm(&mut ok, 2).roll = RollState::Manual;
        gm(&mut ok, 2).monitor = "OTHER".into();
        gm(&mut ok, 1).content = GroupContent::Portal { path: "C:\\x".into() };
        let joined = join(&ok, 1, 2, None, &policy()).unwrap();
        assert_eq!(g(&joined, 2).monitor, g(&ok, 1).monitor, "the source takes the target's monitor");
        assert_eq!(g(&joined, 2).roll, RollState::Expanded, "the set's state governs a folded source");
        assert_eq!(g(&joined, 1).content, GroupContent::Portal { path: "C:\\x".into() }, "each tab keeps its folder");
        assert!(join(&ok, 3, 1, None, &policy()).is_ok(), "a Gate onto a set of Corrals");
    }

    // ---- 4, 5: drop zone, arming, insertion ----------------------------------------------------

    fn strip(dpi: u32, origin: (i32, i32), left: i32) -> Rect {
        let h = px(24.0, dpi);
        Rect::new(origin.0 + left, origin.1, origin.0 + left + px(400.0, dpi), origin.1 + h)
    }

    fn info(id: u64, dpi: u32, origin: (i32, i32), left: i32, widths: &[i32], active: usize, refusal: Option<Refusal>) -> StripInfo {
        let s = strip(dpi, origin, left);
        let cells = tab_cells(s, (s.left + 4, s.right - 4), widths, active, 3, dpi);
        StripInfo { id, strip: s, cells, tabs: widths.len().max(1), refusal }
    }

    #[test]
    fn the_pointer_not_the_dragged_window_decides_and_z_order_picks_the_top() {
        for dpi in DPIS {
            for origin in [(0, 0), (-1920, -200), (-3840, 0)] {
                let top = info(1, dpi, origin, 0, &[], 0, None);
                let under = info(2, dpi, origin, px(100.0, dpi), &[], 0, None);
                let strips = [top.clone(), under.clone()];
                let in_both = (origin.0 + px(150.0, dpi), origin.1 + 2);
                assert_eq!(drop_zone(&strips, in_both).map(|z| z.target), Some(1), "topmost first");
                let only_under = (origin.0 + px(450.0, dpi), origin.1 + 2);
                assert_eq!(drop_zone(&strips, only_under).map(|z| z.target), Some(2));
                assert_eq!(drop_zone(&strips, (origin.0 + px(450.0, dpi), origin.1 + px(30.0, dpi))), None, "below the strip");
                // the strip edge band arms: first and last pixel of the strip
                assert!(drop_zone(&strips, (top.strip.left, top.strip.top)).is_some());
                assert!(drop_zone(&strips, (top.strip.right - 1, top.strip.bottom - 1)).is_some() || under.strip.contains(top.strip.right - 1, top.strip.bottom - 1));
                assert_eq!(drop_zone(&strips, (top.strip.left - 1, top.strip.top)), None);
                // a refusing strip on top hides what lies under it
                let refusing = [StripInfo { refusal: Some(Refusal::TargetFolded), ..top.clone() }, under.clone()];
                assert_eq!(drop_zone(&refusing, in_both), None);
            }
        }
    }

    #[test]
    fn a_zone_arms_after_120_ms_exactly_and_leaving_resets() {
        let s = ArmState::default();
        let (s, armed) = arm_step(s, Some(4), 1000);
        assert!(!armed);
        let (s, armed) = arm_step(s, Some(4), 1119);
        assert!(!armed, "119 ms");
        let (s, armed) = arm_step(s, Some(4), 1120);
        assert!(armed, "120 ms");
        let (s, armed) = arm_step(s, None, 1130);
        assert!(!armed && s.target.is_none(), "leaving disarms at once");
        let (s, armed) = arm_step(s, Some(4), 1140);
        assert!(!armed, "re-entering starts again");
        let (s, _) = arm_step(s, Some(5), 1250);
        assert_eq!(s.since_ms, 1250, "another target restarts the clock");
        assert_eq!(ARM_MS, 120);
    }

    #[test]
    fn insertion_index_follows_the_tab_centres_and_empty_area_appends() {
        for dpi in DPIS {
            for origin in [(0, 0), (-3840, 0)] {
                let target = info(1, dpi, origin, 0, &[40, 40, 40], 0, None);
                assert_eq!(target.cells.len(), 3);
                for (i, c) in target.cells.iter().enumerate() {
                    let centre = c.hit.left + c.hit.width() / 2;
                    assert_eq!(insertion_index(&target.cells, centre - 1, 3), i, "left of the centre of tab {i}");
                    assert_eq!(insertion_index(&target.cells, centre, 3), i + 1, "right of / at the centre of tab {i}");
                    assert_eq!(insertion_index(&target.cells, c.hit.right - 1, 3), i + 1);
                }
                assert_eq!(insertion_index(&target.cells, target.cells[2].hit.right + 5, 3), 3, "empty strip area appends");
                assert_eq!(insertion_index(&target.cells, target.strip.left, 3), 3, "the edge band is not a tab");
                // style 0 (Menu only) and a plain target have no cells
                assert_eq!(insertion_index(&[], origin.0 + 50, 3), 3);
                assert_eq!(insertion_index(&[], origin.0 + 50, 1), 1);
                let zone = drop_zone(&[target.clone()], (target.cells[1].hit.left + 1, origin.1 + 2)).unwrap();
                assert_eq!((zone.insertion, zone.is_set), (1, true));
            }
        }
    }

    #[test]
    fn the_insertion_mark_sits_on_the_tab_edge_or_the_strip_end() {
        let s = strip(96, (0, 0), 0);
        let cells = tab_cells(s, (s.left + 4, s.right - 4), &[40, 40, 40], 0, 3, 96);
        assert_eq!(insertion_mark(&cells, 0, 3), Some(cells[0].hit.left));
        assert_eq!(insertion_mark(&cells, 2, 3), Some(cells[2].hit.left));
        assert_eq!(insertion_mark(&cells, 3, 3), Some(cells[2].hit.right), "append");
        assert_eq!(insertion_mark(&[], 3, 3), None);
        // a scrolled strip that hides the place shows no mark
        let crowded = tab_cells(strip(96, (0, 0), 0), (0, 100), &[200; 6], 5, 3, 96);
        assert!(crowded.first().unwrap().index > 0);
        assert_eq!(insertion_mark(&crowded, 0, 6), None);
    }

    #[test]
    fn feedback_label_and_sentence_follow_the_sides() {
        assert_eq!(join_label(1, 1), "Combine as tabs");
        assert_eq!(join_label(3, 1), "Add to tabs");
        assert_eq!(join_label(3, 2), "Merge tabs");
        assert_eq!(join_label(1, 2), "Merge tabs");
        assert_eq!(join_sentence("A", 1, "B", 1),
            "\"A\" and \"B\" will share one place, as two tabs. Hold Shift and drag a tab away to separate them again.");
        assert_eq!(join_sentence("A", 3, "B", 2),
            "\"A\" (3 tabs) and \"B\" (2 tabs) will share one place, as 5 tabs. Hold Shift and drag a tab away to separate them again.");
    }

    // ---- 6, 7: reordering and tearing off ------------------------------------------------------

    #[test]
    fn reorder_moves_one_tab_and_keeps_the_selected_corral() {
        let d = with_set(doc(4), 1, &[1, 2, 3], 2);
        let after = reorder(&d, 3, 0).unwrap();
        assert_eq!(sets(&after), vec![TabSet { id: 1, tabs: vec![3, 1, 2], selected: 2 }]);
        assert_eq!(sets(&after)[0].selected_id(), 2, "same Corral stays selected");
        assert_eq!(after.groups.iter().filter_map(|g| g.tab).map(|t| t.index).collect::<BTreeSet<_>>(), BTreeSet::from([0, 1, 2]));
        assert_eq!(reorder(&d, 1, 0).unwrap(), d, "no move, no change (a cancelled drag leaves the vector alone)");
        assert_eq!(reorder(&d, 1, 99).unwrap().groups.iter().find(|g| g.id == 1).unwrap().tab.unwrap().index, 2, "clamped to the end");
        assert_eq!(reorder(&d, 4, 0), Err(TabError::NotATab(4)));
    }

    #[test]
    fn a_tab_moves_when_its_centre_crosses_a_neighbours_centre() {
        let centres = [50, 150, 250];
        assert_eq!(reorder_target(&centres, 0, 149), 0);
        assert_eq!(reorder_target(&centres, 0, 150), 1, "exactly on the boundary crosses");
        assert_eq!(reorder_target(&centres, 0, 400), 2);
        assert_eq!(reorder_target(&centres, 2, 151), 2);
        assert_eq!(reorder_target(&centres, 2, 150), 1);
        assert_eq!(reorder_target(&centres, 2, -10), 0);
        assert_eq!(reorder_target(&[], 0, 5), 0);
        // at every DPI the boundary scales with the tabs, so it stays exact
        for dpi in DPIS {
            let c = [px(50.0, dpi), px(150.0, dpi), px(250.0, dpi)];
            assert_eq!(reorder_target(&c, 0, c[1] - 1), 0);
            assert_eq!(reorder_target(&c, 0, c[1]), 1);
        }
    }

    #[test]
    fn strip_autoscroll_steps_one_tab_per_200_ms_near_an_end() {
        for dpi in DPIS {
            let s = strip(dpi, (-1920, 0), 0);
            assert_eq!(autoscroll_dir(s, s.left + px(24.0, dpi) - 1, dpi), -1);
            assert_eq!(autoscroll_dir(s, s.left + px(24.0, dpi), dpi), 0);
            assert_eq!(autoscroll_dir(s, s.right - px(24.0, dpi), dpi), 1);
            assert_eq!(autoscroll_dir(s, s.right - px(24.0, dpi) - 1, dpi), 0);
        }
        let (last, step) = autoscroll_step(None, 1000, 1);
        assert_eq!(step, 1);
        let (last, step) = autoscroll_step(last, 1199, 1);
        assert_eq!(step, 0);
        let (last, step) = autoscroll_step(last, 1200, 1);
        assert_eq!(step, 1);
        assert_eq!(autoscroll_step(last, 1300, 0), (None, 0), "leaving the zone resets");
    }

    #[test]
    fn tearing_off_needs_more_than_32_dip_from_the_strip_at_every_dpi() {
        for dpi in DPIS {
            for origin in [(0, 0), (-1920, -200), (-3840, 0)] {
                let s = strip(dpi, origin, 0);
                let limit = (32.0 * dpi as f64 / 96.0) as i32; // exact at these DPIs
                assert!(!tears(s, (s.left + 10, s.top - limit), dpi), "at the limit: stays");
                assert!(tears(s, (s.left + 10, s.top - limit - 1), dpi), "one pixel more: tears");
                assert!(!tears(s, (s.left + 10, s.bottom - 1 + limit), dpi));
                assert!(tears(s, (s.left + 10, s.bottom + limit), dpi));
                assert!(!tears(s, (s.left + 10, s.top + 2), dpi), "inside");
                assert!(!tears(s, (s.left + px(300.0, dpi), s.bottom - 1 + px(20.0, dpi)), dpi));
                // corner: Euclidean, not per axis
                let diag = (limit as f64 / 2f64.sqrt()) as i32;
                assert!(!tears(s, (s.right - 1 + diag, s.bottom - 1 + diag), dpi));
                assert!(tears(s, (s.right - 1 + diag + 2, s.bottom - 1 + diag + 2), dpi));
            }
        }
        assert_eq!(tear_distance(Rect::new(0, 0, 10, 10), (5, 5)), 0.0);
        assert_eq!(tear_distance(Rect::new(0, 0, 10, 10), (13, 5)), 4.0);
    }

    #[test]
    fn drag_threshold_is_half_the_system_rectangle_but_at_least_4_dip() {
        assert_eq!(drag_threshold(4, 4, 96), (4, 4));
        assert_eq!(drag_threshold(8, 8, 96), (4, 4));
        assert_eq!(drag_threshold(16, 16, 96), (8, 8));
        assert_eq!(drag_threshold(16, 8, 192), (8, 8), "4 DIP = 8 px at 192 dpi");
        assert_eq!(drag_threshold(4, 4, 144), (6, 6));
        assert!(!beyond_threshold((10, 10), (14, 10), (4, 4)));
        assert!(beyond_threshold((10, 10), (15, 10), (4, 4)));
        assert!(beyond_threshold((10, 10), (10, 5), (4, 4)));
    }

    // ---- 8, 9: leaving and separating ---------------------------------------------------------

    fn land() -> Placement {
        Placement { monitor: "MON1".into(), rect: Rect::new(500, 400, 900, 650) }
    }

    #[test]
    fn a_torn_tab_becomes_an_unfolded_undocked_corral_with_its_items() {
        let mut d = with_set(doc(4), 5, &[1, 2, 3], 2);
        gm(&mut d, 2).roll = RollState::Manual;
        gm(&mut d, 2).dock_top = true;
        let shared_pref = g(&d, 2).preferred_size_dip;
        let after = remove_tab(&d, 3, &land()).unwrap();
        let torn = g(&after, 3);
        assert_eq!((torn.tab, torn.rect, &torn.monitor, torn.roll, torn.dock_top, torn.dock_bottom), (None, land().rect, &land().monitor, RollState::Expanded, false, false));
        assert_eq!(torn.preferred_size_dip, shared_pref, "preferred size is the set's");
        assert_eq!(torn.content, g(&d, 3).content, "keeps its items");
        assert_eq!(sets(&after), vec![TabSet { id: 5, tabs: vec![1, 2], selected: 1 }], "selected tab unchanged");
        assert_eq!(g(&after, 1).roll, RollState::Manual, "the set keeps its fold state");
        assert_eq!(private_multiset(&after), private_multiset(&d));
    }

    #[test]
    fn tearing_the_selected_tab_selects_the_one_at_the_same_index_else_the_previous() {
        let d = with_set(doc(4), 5, &[1, 2, 3, 4], 2);
        assert_eq!(sets(&remove_tab(&d, 2, &land()).unwrap())[0].selected_id(), 3, "same index");
        let last = with_set(doc(4), 5, &[1, 2, 3, 4], 4);
        assert_eq!(sets(&remove_tab(&last, 4, &land()).unwrap())[0].selected_id(), 3, "previous at the end");
        let first = with_set(doc(4), 5, &[1, 2, 3, 4], 1);
        assert_eq!(sets(&remove_tab(&first, 1, &land()).unwrap())[0].selected_id(), 2);
    }

    #[test]
    fn a_two_tab_set_dissolves_when_one_tab_leaves() {
        let d = with_set(doc(3), 5, &[1, 2], 1);
        let after = remove_tab(&d, 1, &land()).unwrap();
        assert!(sets(&after).is_empty());
        assert!(after.groups.iter().all(|g| g.tab.is_none()), "membership cleared on both");
        assert_eq!(g(&after, 2).rect, g(&d, 1).rect, "the remaining Corral stays where the set was");
        assert_eq!(remove_tab(&d, 3, &land()), Err(TabError::NotATab(3)));
    }

    #[test]
    fn separate_all_cascades_and_keeps_the_selected_tab_in_place() {
        let d = with_set(doc(4), 5, &[1, 2, 3, 4], 2);
        let area = |_: &str| Some(Rect::new(0, 0, 1000, 700));
        let after = separate_all(&d, 3, &area).unwrap();
        assert!(sets(&after).is_empty() && after.groups.iter().all(|g| g.tab.is_none()));
        let home = g(&d, 2).rect;
        assert_eq!(g(&after, 2).rect, home, "selected keeps the place");
        let others: Vec<u64> = vec![1, 3, 4];
        for (k, id) in others.iter().enumerate() {
            let step = 24 * (k as i32 + 1);
            assert_eq!(g(&after, *id).rect, shift_into(home.offset(step, step), Rect::new(0, 0, 1000, 700)), "tab {id}");
            assert_eq!(g(&after, *id).roll, RollState::Expanded);
        }
        assert_eq!(private_multiset(&after), private_multiset(&d), "all items unchanged");
        // clamped on screen
        let tight = |_: &str| Some(Rect::new(0, 0, 450, 320));
        let squeezed = separate_all(&d, 2, &tight).unwrap();
        assert!(squeezed.groups.iter().filter(|g| g.id != 2).all(|g| g.rect.right <= 450 && g.rect.bottom <= 320 && g.rect.left >= 0 && g.rect.top >= 0));
    }

    // ---- 10: hover switching and spring ---------------------------------------------------------

    fn hover(state: HoverState, tab: Option<u64>, now: u64, wait: Option<u32>, cancel: bool) -> (HoverState, HoverOut) {
        hover_switch_step(state, HoverInput { tab, now_ms: now, wait_ms: wait, cancel })
    }

    #[test]
    fn hover_selects_after_300_ms_not_299() {
        let (s, out) = hover(HoverState::default(), Some(2), 1000, Some(300), false);
        assert_eq!(out, HoverOut::Wait(300));
        let (s, out) = hover(s, Some(2), 1299, Some(300), false);
        assert_eq!(out, HoverOut::Wait(1), "299 ms: no switch");
        let (s, out) = hover(s, Some(2), 1300, Some(300), false);
        assert_eq!(out, HoverOut::Select(2), "300 ms");
        assert_eq!(hover(s, Some(2), 2000, Some(300), false).1, HoverOut::Idle, "a stationary pointer does not re-arm");
    }

    #[test]
    fn hover_without_the_wait_switches_at_once_and_leaving_cancels() {
        assert_eq!(hover(HoverState::default(), Some(2), 5, Some(0), false).1, HoverOut::Select(2));
        let (s, _) = hover(HoverState::default(), Some(2), 0, Some(300), false);
        let (s, out) = hover(s, None, 100, Some(300), false);
        assert_eq!(out, HoverOut::Idle);
        let (s, out) = hover(s, Some(2), 150, Some(300), false);
        assert_eq!(out, HoverOut::Wait(300), "re-entering starts the full wait");
        let (_, out) = hover(s, Some(3), 200, Some(300), false);
        assert_eq!(out, HoverOut::Wait(300), "another tab restarts it");
        assert_eq!(hover(HoverState::default(), Some(2), 0, None, false).1, HoverOut::Idle, "switching off");
    }

    #[test]
    fn press_menu_drag_and_clear_view_cancel_until_the_pointer_leaves() {
        let (s, _) = hover(HoverState::default(), Some(2), 0, Some(300), false);
        let (s, out) = hover(s, Some(2), 100, Some(300), true);
        assert_eq!(out, HoverOut::Idle);
        assert_eq!(hover(s, Some(2), 1000, Some(300), false).1, HoverOut::Idle, "the same visit never re-arms");
        let (s, _) = hover(s, None, 1100, Some(300), false);
        assert_eq!(hover(s, Some(2), 1200, Some(300), false).1, HoverOut::Wait(300), "leave and re-enter arms again");
    }

    #[test]
    fn a_tab_under_a_stationary_pointer_after_another_switch_does_not_re_arm() {
        let s = HoverState::after_switch(Some(2), 500);
        assert_eq!(hover(s, Some(2), 5000, Some(300), false).1, HoverOut::Idle);
        assert_eq!(hover(s, Some(2), 5000, Some(0), false).1, HoverOut::Idle);
        let (s, _) = hover(s, None, 5100, Some(300), false);
        assert_eq!(hover(s, Some(2), 5200, Some(0), false).1, HoverOut::Select(2));
    }

    #[test]
    fn an_icon_drag_springs_after_600_ms_even_with_hover_switching_off() {
        let off = TabSettings { mouseover: false, ..TabSettings::default() };
        assert_eq!(off.hover_wait(false), None);
        assert_eq!(off.hover_wait(true), Some(600));
        let on = TabSettings { mouseover: true, wait: true, ..TabSettings::default() };
        assert_eq!((on.hover_wait(false), on.hover_wait(true)), (Some(300), Some(600)));
        assert_eq!(TabSettings { wait: false, ..on }.hover_wait(false), Some(0));
        let w = off.hover_wait(true);
        let (s, _) = hover(HoverState::default(), Some(2), 0, w, false);
        assert_eq!(hover(s, Some(2), 599, w, false).1, HoverOut::Wait(1));
        assert_eq!(hover(s, Some(2), 600, w, false).1, HoverOut::Select(2));
    }

    #[test]
    fn settings_map_to_the_spec_names() {
        let mut s = Settings::default();
        assert_eq!(TabSettings::from(&s), TabSettings::default());
        s.tabs_select_on_mouseover = 1;
        s.tabs_select_on_mouseover_delay = 750; // an old stored number only means "wait"
        s.tabs_style = 1;
        let t = TabSettings::from(&s);
        assert!(t.mouseover && t.wait && !t.colors);
        assert_eq!(t.hover_wait(false), Some(300));
        assert_eq!(t.effective_style(Some(2)), 2);
        assert_eq!(t.effective_style(Some(0)), 1);
        assert_eq!(t.effective_style(None), 1);
        assert_eq!(t.join_policy(false), JoinPolicy { create: true, titles: false });
    }

    // ---- 11: Clear view -------------------------------------------------------------------------

    #[test]
    fn a_set_is_kept_in_clear_view_when_any_tab_is_flagged() {
        let flags: BTreeMap<u64, bool> = BTreeMap::from([(1, false), (2, true), (3, false)]);
        let flagged = |id| flags.get(&id).copied().unwrap_or(false);
        assert!(kept_in_clear_view(&[1, 2, 3], flagged));
        assert!(!kept_in_clear_view(&[1, 3], flagged));
        assert!(!kept_in_clear_view(&[], flagged));
        // tearing a tab off changes no flag: each Corral keeps its own
        let d = with_set(doc(3), 1, &[1, 2, 3], 1);
        let after = remove_tab(&d, 2, &land()).unwrap();
        assert!(kept_in_clear_view(&[2], flagged) && !kept_in_clear_view(&[1, 3], flagged));
        assert_eq!(after.groups.len(), 3);
    }

    // ---- 12: normalisation ---------------------------------------------------------------------

    fn raw(d: &mut TabDoc, id: u64, set: u64, index: u32, selected: bool) {
        gm(d, id).tab = Some(TabMembership { set_id: set, index, selected });
    }

    #[test]
    fn stored_oddities_are_repaired_in_memory_and_never_written_by_a_load() {
        let mut d = doc(6);
        // gapped and duplicate indices sort by (index, file order)
        raw(&mut d, 1, 4, 9, false);
        raw(&mut d, 2, 4, 3, false);
        raw(&mut d, 3, 4, 3, true);
        // zero selected resolve to the first tab; two selected to the first selected in order
        raw(&mut d, 4, 5, 0, false);
        raw(&mut d, 5, 5, 1, false);
        // one member behaves as a plain Corral
        raw(&mut d, 6, 8, 0, true);
        let s = normalize_sets(&d.groups);
        assert_eq!(s, vec![TabSet { id: 4, tabs: vec![2, 3, 1], selected: 1 }, TabSet { id: 5, tabs: vec![4, 5], selected: 0 }]);
        raw(&mut d, 4, 5, 0, true);
        raw(&mut d, 5, 5, 1, true);
        assert_eq!(normalize_sets(&d.groups)[1].selected, 0);
        assert!(body_visible(&d.groups, g(&d, 4)) && !body_visible(&d.groups, g(&d, 5)));
        assert!(body_visible(&d.groups, g(&d, 6)), "a one-member set is plain");
        assert_eq!(collect(&d.groups, 6), Some(TabSetView { set_id: 0, members: vec![6], active: 6 }));
        assert!(members(&d.groups, g(&d, 6)).is_empty());
        // a counter below an existing id is raised only when a new id is needed
        d.next_tab_set = 2;
        assert_eq!(next_set_id(d.next_tab_set, &d.groups), Some(9));
        assert_eq!(d.next_tab_set, 2, "the stored counter is untouched");
        // mismatched member rects follow the selected tab
        gm(&mut d, 2).rect = Rect::new(1, 2, 3, 4);
        assert_eq!(collect(&d.groups, 1).unwrap().active, 3);
        // the first edit writes contiguous indices to all members of the touched set only
        let before_other = (g(&d, 4).tab, g(&d, 5).tab, g(&d, 6).tab);
        let after = select(&d, 2).unwrap();
        let mut idx: Vec<(u64, u32, bool)> = [2, 3, 1].iter().map(|id| { let t = g(&after, *id).tab.unwrap(); (*id, t.index, t.selected) }).collect();
        idx.sort_by_key(|e| e.1);
        assert_eq!(idx, vec![(2, 0, true), (3, 1, false), (1, 2, false)]);
        assert_eq!((g(&after, 4).tab, g(&after, 5).tab, g(&after, 6).tab), before_other, "other sets and the one-member set stay as stored");
        assert!(after.next_tab_set > 8, "the counter is raised with the first edit");
    }

    #[test]
    fn imported_data_is_written_in_canonical_form() {
        let mut d = doc(5);
        raw(&mut d, 1, 4, 5, false);
        raw(&mut d, 2, 4, 2, true);
        raw(&mut d, 3, 9, 0, true);
        let c = canonicalize(&d);
        assert_eq!(g(&c, 2).tab, Some(TabMembership { set_id: 4, index: 0, selected: true }));
        assert_eq!(g(&c, 1).tab, Some(TabMembership { set_id: 4, index: 1, selected: false }));
        assert_eq!(g(&c, 3).tab, None, "a one-member set dissolves");
        assert_eq!(c.next_tab_set, 10, "ids seen are never reused");
        assert_eq!(canonicalize(&c), c);
    }

    #[test]
    fn undoing_a_gesture_keeps_changes_made_meanwhile() {
        let before = with_set(doc(4), 5, &[1, 2, 3], 2);
        // the gesture tears tab 3 off; meanwhile a timer assigns an item to Corral 3 and renames Corral 4
        let mut live = remove_tab(&before, 3, &land()).unwrap();
        if let GroupContent::Items { items } = &mut gm(&mut live, 3).content { items.push("new-item".into()); }
        gm(&mut live, 4).title = "renamed".into();
        let undone = restore_members(&live, &before, &[1, 2, 3]);
        assert_eq!(sets(&undone), sets(&before), "membership is back");
        assert_eq!(g(&undone, 3).rect, g(&before, 3).rect);
        assert_eq!(g(&undone, 3).roll, g(&before, 3).roll);
        assert!(matches!(&g(&undone, 3).content, GroupContent::Items { items } if items.last().map(String::as_str) == Some("new-item")), "the item stays");
        assert_eq!(g(&undone, 4).title, "renamed", "other Corrals are not touched");
        assert!(undone.next_tab_set >= live.next_tab_set);
        // a Corral deleted meanwhile is not brought back
        let mut gone = live.clone();
        gone.groups.retain(|g| g.id != 3);
        assert_eq!(restore_members(&gone, &before, &[1, 2, 3]).groups.len(), 3);
    }

    #[test]
    fn tab_edits_wait_for_a_display_transition_and_a_locked_desktop() {
        assert_eq!(edit_allowed(false, false), Ok(()));
        assert_eq!(edit_allowed(true, false), Err(TabError::Locked));
        assert_eq!(edit_allowed(false, true), Err(TabError::Busy));
    }

    #[test]
    fn unknown_keys_survive_an_edit() {
        let mut d = with_set(doc(3), 1, &[1, 2], 1);
        gm(&mut d, 2).extra.insert("FutureKey".into(), serde_json::json!({"a": [1, 2]}));
        let after = join(&d, 1, 3, None, &policy()).unwrap();
        assert_eq!(g(&after, 2).extra.get("FutureKey"), Some(&serde_json::json!({"a": [1, 2]})));
        let text = serde_json::to_string(&after.groups).unwrap();
        let back: Vec<Group> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, after.groups);
    }

    // ---- 13: validate ---------------------------------------------------------------------------

    #[test]
    fn validate_rejects_what_the_spec_lists() {
        let before = with_set(doc(4), 1, &[1, 2], 1);
        let ok = join(&before, 1, 3, None, &policy()).unwrap();
        assert_eq!(validate(&before, &ok, &[]), Ok(()));
        let mut dup = ok.clone();
        dup.groups.push(dup.groups[0].clone());
        assert!(validate(&before, &dup, &[]).is_err(), "duplicate Corral");
        let mut one = ok.clone();
        for id in [2, 3] { gm(&mut one, id).tab = None }
        assert!(validate(&before, &one, &[]).is_err(), "set of one");
        let mut none_selected = ok.clone();
        for id in [1, 2, 3] { gm(&mut none_selected, id).tab.as_mut().unwrap().selected = false }
        assert!(validate(&before, &none_selected, &[]).is_err(), "no selected tab");
        let mut two = ok.clone();
        for id in [1, 2, 3] { gm(&mut two, id).tab.as_mut().unwrap().selected = true }
        assert!(validate(&before, &two, &[]).is_err(), "two selected tabs");
        let mut gap = ok.clone();
        gm(&mut gap, 3).tab.as_mut().unwrap().index = 7;
        assert!(validate(&before, &gap, &[]).is_err(), "indices not contiguous");
        let mut counter = ok.clone();
        counter.next_tab_set = 1;
        assert!(validate(&before, &counter, &[]).is_err(), "counter not above ids");
        let mut lost_item = ok.clone();
        if let GroupContent::Items { items } = &mut gm(&mut lost_item, 3).content { items.pop(); }
        assert!(validate(&before, &lost_item, &[]).is_err(), "lost item");
        let mut retitled = ok.clone();
        gm(&mut retitled, 2).title = "x".into();
        assert!(validate(&before, &retitled, &[]).is_err(), "title changed");
        let mut recoloured = ok.clone();
        gm(&mut recoloured, 2).style.color = Some(1);
        assert!(validate(&before, &recoloured, &[]).is_err(), "colour slot changed");
        let mut gone = ok.clone();
        gone.groups.retain(|g| g.id != 4);
        assert!(validate(&before, &gone, &[]).is_err(), "lost Corral");
        assert!(validate(&before, &gone, &[4]).is_ok(), "deleted on purpose");
        let mut lower = ok.clone();
        lower.next_tab_set = 0;
        assert!(validate(&before, &lower, &[]).is_err());
    }

    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: u64) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 % n
        }
    }

    #[test]
    fn every_operation_passes_validate_over_ten_thousand_random_sequences() {
        let mut rng = Rng(0x9E3779B97F4A7C15);
        let area = |_: &str| Some(Rect::new(0, 0, 1600, 900));
        let mut applied = 0;
        for _ in 0..10_000 {
            let mut d = doc(6);
            let items = private_multiset(&d);
            for _ in 0..6 {
                let (a, b) = (1 + rng.below(6), 1 + rng.below(6));
                let result = match rng.below(7) {
                    0 | 1 => join(&d, a, b, Some(rng.below(4) as usize), &policy()),
                    2 => merge(&d, a, b, None, &policy()),
                    3 => select(&d, a),
                    4 => reorder(&d, a, rng.below(5) as usize),
                    5 => if rng.below(2) == 0 { remove_tab(&d, a, &land()) } else { separate_all(&d, a, &area) },
                    _ => Ok(d.clone()),
                };
                if let Ok(next) = result {
                    validate(&d, &next, &[]).expect("validate");
                    for set in sets(&next) {
                        let live = g(&next, set.selected_id());
                        assert!(set.tabs.iter().all(|id| {
                            let t = g(&next, *id);
                            (&t.monitor, t.rect, t.preferred_size_dip, t.roll, t.dock_top, t.dock_bottom, t.swipe_index, t.opacity)
                                == (&live.monitor, live.rect, live.preferred_size_dip, live.roll, live.dock_top, live.dock_bottom, live.swipe_index, live.opacity)
                        }), "every tab of a set carries the set's value");
                    }
                    applied += 1;
                    d = next;
                }
            }
            assert_eq!(private_multiset(&d), items, "no item, title or colour is ever lost");
            let ids: BTreeSet<u64> = d.groups.iter().map(|g| g.id).collect();
            assert_eq!(ids.len(), 6);
        }
        assert!(applied > 20_000, "the sequences really exercised the operations: {applied}");
    }

    #[test]
    fn deleting_a_tab_shrinks_its_set_by_the_same_rules() {
        let d = with_set(doc(4), 5, &[1, 2, 3], 2);
        let after = delete(&d, 2).unwrap();
        assert_eq!(after.groups.len(), 3);
        assert_eq!(sets(&after), vec![TabSet { id: 5, tabs: vec![1, 3], selected: 1 }]);
        let last = delete(&after, 1).unwrap();
        assert!(sets(&last).is_empty() && last.groups.iter().all(|g| g.tab.is_none()), "dissolves into the remaining Corral");
        assert_eq!(delete(&d, 4).unwrap().groups.len(), 3, "a plain Corral just disappears");
        assert_eq!(delete(&d, 9), Err(TabError::MissingMember(9)));
    }

    #[test]
    fn select_hands_the_shown_tabs_place_to_the_new_one() {
        let mut d = with_set(doc(3), 1, &[1, 2], 1);
        gm(&mut d, 2).rect = Rect::new(900, 900, 1000, 1000);
        let after = select(&d, 2).unwrap();
        assert_eq!(g(&after, 2).rect, g(&d, 1).rect, "nothing jumps");
        assert_eq!(select(&after, 2).unwrap(), after, "selecting the shown tab changes nothing");
        assert_eq!(select(&d, 3), Err(TabError::NotATab(3)));
    }

    #[test]
    fn sync_shared_copies_the_selected_tabs_value_to_the_others() {
        let mut d = with_set(doc(4), 1, &[1, 2, 3], 2);
        gm(&mut d, 2).rect = Rect::new(7, 8, 9, 10);
        gm(&mut d, 2).title_side_last = 1;
        assert!(!sync_shared(&mut d.groups, 1), "only the selected tab speaks for the set");
        assert!(sync_shared(&mut d.groups, 2));
        assert!(d.groups.iter().take(3).all(|g| g.rect == Rect::new(7, 8, 9, 10) && g.title_side_last == 1));
        assert!(!sync_shared(&mut d.groups, 2));
        assert_eq!(d.groups[3].rect, doc(4).groups[3].rect);
    }

    #[test]
    fn counters_ids_and_overflow() {
        let d = doc(2);
        assert_eq!(next_set_id(1, &d.groups), Some(1));
        assert_eq!(next_set_id(0, &d.groups), Some(1));
        let d = with_set(doc(3), 12, &[1, 2], 1);
        assert_eq!(next_set_id(1, &d.groups), Some(13));
        assert_eq!(next_set_id(40, &d.groups), Some(40));
        assert_eq!(next_set_id(u64::MAX, &d.groups), None);
        let mut full = doc(3);
        full.next_tab_set = u64::MAX - 1;
        assert!(join(&full, 1, 2, None, &policy()).is_ok());
        full.next_tab_set = u64::MAX;
        assert_eq!(join(&full, 1, 2, None, &policy()), Err(TabError::Overflow));
    }

    #[test]
    fn membership_serialises_flat_and_reads_as_stored() {
        let text = r#"{"set_id":4,"index":2,"selected":true}"#;
        let t: TabMembership = serde_json::from_str(text).unwrap();
        assert_eq!(t, TabMembership { set_id: 4, index: 2, selected: true });
        assert_eq!(serde_json::to_string(&t).unwrap(), text);
    }
}

#[cfg(test)]
mod strip_tests {
    use super::*;
    use crate::group_paint::r;

    fn strip_at(dpi: u32) -> Rect {
        Rect::new(0, 10, 4000, 10 + r(24.0, dpi))
    }

    #[test]
    fn tab_widths_are_text_plus_padding_clamped_to_48_160_dip() {
        for dpi in [96, 120, 144, 192, 240] {
            assert_eq!(tab_width(0, dpi), r(48.0, dpi), "short titles take the minimum");
            assert_eq!(tab_width(r(60.0, dpi), dpi), r(60.0, dpi) + 2 * r(10.0, dpi));
            assert_eq!(tab_width(r(500.0, dpi), dpi), r(160.0, dpi), "long titles take the maximum");
        }
    }

    #[test]
    fn tabs_that_fit_keep_their_natural_widths_side_by_side() {
        let dpi = 96;
        let cells = tab_cells(strip_at(dpi), (30, 530), &[40, 100, 20], 0, 2, dpi);
        assert_eq!(cells.iter().map(|c| c.hit.width()).collect::<Vec<_>>(), [60, 120, 48]);
        assert_eq!(cells[0].hit.left, 30);
        for pair in cells.windows(2) {
            assert_eq!(pair[0].hit.right, pair[1].hit.left, "no gaps or overlaps");
        }
        for cell in &cells {
            assert!(cell.label.left >= cell.hit.left && cell.label.right <= cell.hit.right);
        }
        assert_eq!(hit_tab(&cells, (31, 20)), Some(0));
        assert_eq!(hit_tab(&cells, (cells[2].hit.right, 20)), None);
    }

    #[test]
    fn crowded_tabs_shrink_equally_down_to_the_minimum() {
        let dpi = 96;
        // natural 160 + 160 + 160 = 480 in a 400 px span: each loses the same amount
        let cells = tab_cells(strip_at(dpi), (0, 400), &[300, 300, 300], 1, 1, dpi);
        let widths: Vec<_> = cells.iter().map(|c| c.hit.width()).collect();
        assert_eq!(widths.len(), 3);
        assert!(widths.iter().all(|w| *w >= 48) && widths.iter().sum::<i32>() <= 400);
        assert!(widths.iter().sum::<i32>() >= 398, "{widths:?}");
        assert!(widths.iter().max().unwrap() - widths.iter().min().unwrap() <= 1);
        // a short tab keeps its width while the long ones give way
        let cells = tab_cells(strip_at(dpi), (0, 300), &[0, 300, 300], 0, 1, dpi);
        assert_eq!(cells[0].hit.width(), 48);
        assert!(cells[1].hit.width() >= 48 && cells[1].hit.width() < 160);
    }

    #[test]
    fn beyond_the_minimum_the_strip_scrolls_by_whole_tabs_and_keeps_the_active_tab_whole() {
        for dpi in [96, 144, 240] {
            let min = r(48.0, dpi);
            let span = (r(20.0, dpi), r(20.0, dpi) + 4 * min + min / 2);
            for count in 5..=12usize {
                for active in 0..count {
                    let cells = tab_cells(strip_at(dpi), span, &vec![r(200.0, dpi); count], active, 3, dpi);
                    assert!(!cells.is_empty());
                    let shown: Vec<u32> = cells.iter().map(|c| c.index).collect();
                    assert!(shown.contains(&(active as u32)), "active {active} of {count} visible: {shown:?}");
                    assert!(cells.iter().all(|c| c.hit.width() == min && c.hit.left >= span.0 && c.hit.right <= span.1));
                    assert!(shown.windows(2).all(|w| w[1] == w[0] + 1), "whole tabs, in order");
                    if active == 0 {
                        assert_eq!(shown[0], 0);
                    }
                }
            }
        }
    }

    #[test]
    fn no_cells_for_menu_only_one_tab_or_a_span_narrower_than_a_tab() {
        let dpi = 96;
        assert!(tab_cells(strip_at(dpi), (0, 400), &[10, 10], 0, 0, dpi).is_empty(), "Menu only");
        assert!(tab_cells(strip_at(dpi), (0, 400), &[10, 10], 0, 4, dpi).is_empty(), "unknown style");
        assert!(tab_cells(strip_at(dpi), (0, 400), &[10], 0, 1, dpi).is_empty(), "one tab");
        assert!(tab_cells(strip_at(dpi), (0, 47), &[10, 10], 0, 1, dpi).is_empty(), "narrower than 48 DIP");
        assert!(tab_cells(Rect::default(), (0, 400), &[10, 10], 0, 1, dpi).is_empty());
    }

    #[test]
    fn every_pixel_of_the_cells_hits_exactly_one_tab() {
        for dpi in [96, 120, 144, 192, 240] {
            for style in [1, 2, 3] {
                for span_w in [100, 200, 333, 700] {
                    let cells = tab_cells(strip_at(dpi), (-20, -20 + span_w), &[10, 80, 30, 120], 2, style, dpi);
                    if cells.is_empty() {
                        continue;
                    }
                    let (left, right) = (cells.first().unwrap().hit.left, cells.last().unwrap().hit.right);
                    for x in left..right {
                        assert_eq!(cells.iter().filter(|c| c.hit.contains(x, 20)).count(), 1);
                    }
                    assert_eq!(hit_tab(&cells, (right, 20)), None);
                    assert!(right <= -20 + span_w);
                }
            }
        }
    }
}
