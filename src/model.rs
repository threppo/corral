//! Layout model and atomic persistence.
//!
//! Groups reference desktop items by Shell parsing name; regrouping never moves files.
//! The on-disk format is Corral's own JSON (versioned).

use crate::geometry::Rect;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const LAYOUT_VERSION: u32 = 1;

fn initial_tab_set_id() -> u64 { 1 }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RollState {
    /// Body visible.
    #[default]
    Expanded,
    /// Rolled up by the user (title double-click).
    Manual,
    /// Rolled up automatically because the group is docked to a screen edge.
    Docked,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupContent {
    /// Ordinary group: references to desktop items, in display order.
    Items { items: Vec<String> },
    /// Folder portal: shows the contents of a folder.
    Portal { path: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: u64,
    /// Separate Corral identity/content; absent membership keeps pre-tabs layouts standalone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<crate::tabs::TabMembership>,
    pub title: String,
    /// Monitor device name the rectangle is relative to.
    pub monitor: String,
    /// Icon/body rectangle in DIPs relative to the monitor work-area origin; title is external.
    pub rect: Rect,
    /// Corral choice: User-selected expanded body size before fitting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred_size_dip: Option<(i32, i32)>,
    pub content: GroupContent,
    #[serde(default)]
    pub roll: RollState,
    /// Corral top-dock latch, independent of fold state and automatic title placement.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dock_top: bool,
    /// Corral bottom-edge latch, independent of fold state. Old layouts omit it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub dock_bottom: bool,
    /// Runtime mirror of the scrollbar offset; never persisted (scrollbar-spec 1).
    #[serde(skip)]
    pub scroll: i32,
    /// Folder Gate back stack, newest first; entry 0 is the folder shown (stored with the layout).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nav_history: Vec<String>,
    /// Folder Portal flags (`Groups` field 21 `dwViewFlags`); bit
    /// [`VIEW_FLAG_NO_NAVIGATION`] = "Allow navigation" unchecked.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub view_flags: u32,
    /// Corral extra: hide only the Shell link overlay in this portal's hosted list.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hide_link_overlays: bool,
    /// `SortMethod` (Groups field 12; `sorting::METHOD_*`, 0 = unsorted); ordinary Corrals only.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sort_method: u32,
    /// `SortFlags` (Groups field 13 without the 0x4000 pages bit; `sorting::FLAG_*`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub sort_flags: u32,
    /// Pages assignment (`Groups` field 15 `SwipeIndex`). Icons in a Corral follow it.
    #[serde(default, skip_serializing_if = "is_zero_signed")]
    pub swipe_index: i32,
    /// "Show this Corral on all pages".
    #[serde(default, skip_serializing_if = "is_false")]
    pub all_pages: bool,
    /// View > Opacity (`Groups` field 14; 0 = "Visible only on mouseover", 255 = fully
    /// visible, the default). The menu only produces [`OPACITY_STEPS`].
    #[serde(default = "full_opacity", skip_serializing_if = "is_full_opacity")]
    pub opacity: u8,
    /// Own style (`Styling\<id>...` values); values that are absent
    /// inherit the global setting.
    #[serde(default, skip_serializing_if = "GroupStyle::is_empty")]
    pub style: GroupStyle,
    /// Title side preference (`Groups` field 17 `Edge`): -1 auto, 0 above, 1 below.
    #[serde(default = "undecided", skip_serializing_if = "is_undecided")]
    pub title_side: i32,
    /// Last computed title side (`Groups` field 18 `EdgeLast`): -1 undecided, 0 above,
    /// 1 below. Written by every layout; it is what keeps a "title at bottom" Corral there when
    /// `TitleEdgeAutoAdjust` is switched off.
    #[serde(default = "undecided", skip_serializing_if = "is_undecided")]
    pub title_side_last: i32,
    /// Corral extra (Ctrl + wheel over a Corral, `icon_zoom.rs`): this Corral's icon zoom in percent of
    /// the desktop icon size; `None` = follow the desktop (100 %). Unknown or malformed values load
    /// as `None`/are clamped, old layouts without the field are unchanged.
    #[serde(default, deserialize_with = "lenient_zoom", skip_serializing_if = "Option::is_none")]
    pub icon_zoom: Option<u16>,
    /// An offset stored by an older build under `scroll`: read so that it is not carried as an unknown
    /// key, never written (scrollbar-spec 1).
    #[serde(default, rename = "scroll", skip_serializing)]
    legacy_scroll: IgnoredKey,
    /// Keys on a Corral that this build does not know are carried through a load and save (tabs-spec 8).
    /// Only Corral-level keys: unknown keys elsewhere in the file are not kept.
    #[serde(flatten, default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

/// A stored key that is read and dropped; two groups never differ by it.
#[derive(Clone, Copy, Debug, Default)]
pub struct IgnoredKey;

impl PartialEq for IgnoredKey {
    fn eq(&self, _: &Self) -> bool { true }
}

impl Eq for IgnoredKey {}

impl<'de> Deserialize<'de> for IgnoredKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(d).map(|_| IgnoredKey)
    }
}

fn lenient_zoom<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u16>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(d)?;
    Ok(value.and_then(|v| v.as_u64()).and_then(|v| u16::try_from(v).ok()))
}

/// Opacity values of View > Opacity: 0 %, 20 %, 40 %, 60 %, 80 %, 100 %.
pub const OPACITY_STEPS: [u8; 6] = [0, 0x33, 0x66, 0x99, 0xcc, 0xff];

fn full_opacity() -> u8 {
    255
}

fn is_full_opacity(v: &u8) -> bool {
    *v == 255
}

fn undecided() -> i32 {
    -1
}

fn is_undecided(v: &i32) -> bool {
    *v == -1
}

/// The 13 per-Corral style values of the `Styling` key (`%iColor`, `%iTextColor`,
/// `%iTransparencyFactor`, `%iFontName`, `%iFontSize`, `%iFontWeight`, `%iFontItalics`,
/// `%iBlendIcons`, `%iBlendBackground`, `%iTintIcons`, `%iTintIconsColor`, `%iTintIconsColorUse`,
/// `%iTabsStyle`; Copy/Paste color moves all of them). `None` = the value is
/// absent = inherits the global setting. Corral paints Color, TextColor and TransparencyFactor; the
/// other values are carried so Copy/Paste/Reset color treat the whole set together.
/// `color` and `text_color` are COLORREFs (`0x00BBGGRR`, as `Settings::color`), the registry
/// stores ARGB.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupStyle {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_color: Option<u32>,
    /// camouflage-spec 3.5: absent inherits shared Auto/custom text choice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_color_auto: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transparency_factor: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_name: Option<String>,
    /// Font size x 1000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_weight: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_italics: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blend_icons: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blend_background: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tint_icons: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tint_icons_color: Option<u32>,
    /// camouflage-spec 5.2: absent inherits the shared source; "corral" or "custom".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tint_icons_color_source: Option<String>,
    /// camouflage-spec 5.2: read only, as input of the tint-source migration; never written.
    #[serde(skip_serializing)]
    pub tint_icons_color_use: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tabs_style: Option<u32>,
}

impl Group {
    pub fn dock_edge(&self) -> Option<crate::rollup::DockEdge> {
        if self.dock_top { Some(crate::rollup::DockEdge::Top) }
        else if self.dock_bottom { Some(crate::rollup::DockEdge::Bottom) }
        else { None }
    }
    pub fn set_dock_edge(&mut self, edge: Option<crate::rollup::DockEdge>) {
        self.dock_top = edge == Some(crate::rollup::DockEdge::Top);
        self.dock_bottom = edge == Some(crate::rollup::DockEdge::Bottom);
    }
}

impl GroupStyle {
    /// camouflage-spec 5.2. Call after settings load, so an absent
    /// own background colour compares against the actual shared Corral colour.
    pub fn migrate_tint_source(&mut self, global_color: u32, _global_tint: u32) {
        if let Some(legacy) = self.tint_icons_color_use.filter(|_| self.tint_icons_color_source.is_none()) {
            self.tint_icons_color_source = Some(crate::settings::migrated_tint_source(
                legacy, self.color.unwrap_or(global_color)).into());
            if self.tint_icons_color.is_none() {
                self.tint_icons_color = Some(legacy & 0x00ff_ffff);
            }
        }
        // camouflage-spec 3.5: an old explicit non-white text colour remains custom.
        // colour-state-spec 5: a stored text colour without the mode key becomes that Corral's own
        // choice: white was the old Auto encoding, anything else stays a custom colour.
        if self.text_color_auto.is_none() {
            if let Some(color) = self.text_color {
                self.text_color_auto = Some(color & 0x00ff_ffff == 0x00ff_ffff);
            }
        }
    }

    /// No value present: the Corral follows the global style.
    pub fn is_empty(&self) -> bool {
        *self == GroupStyle::default()
    }
}

/// Title side of a Corral: an
/// explicit preference (`pref` 1) is taken as is; with `pref == -1` the title goes below only when
/// the Corral is near the monitor top (`near_top`) and either `TitleEdgeAutoAdjust` is on or the
/// Corral already was below (`last == 1`), the "stays docked" case of the info box shown when the
/// setting is switched off. The result is stored back in `title_side_last`.
pub fn title_below(pref: i32, last: i32, auto_adjust: bool, near_top: bool) -> bool {
    match pref {
        1 => true,
        -1 => near_top && (auto_adjust || last == 1),
        _ => false,
    }
}

/// Group fade 0..255 from View > Opacity:
/// 255 for a fully visible Corral or while the global opacity override (`ShowHiddenCorrals`) is
/// on; otherwise, with `FadeInOpacityOnMouseover` (default on, not modelled as a setting) the
/// opacity is lerped to 255 by the hover factor `hover` (0..1): `v + (int)((255 - v) * hover)`.
/// A Folder Portal never goes below 1 (`max(v, 1)`).
pub fn group_fade(opacity: u8, hover: f64, transparency_disabled: bool, portal: bool) -> u8 {
    if opacity == 255 || transparency_disabled {
        return 255;
    }
    let base = if portal { opacity.max(1) } else { opacity } as i32;
    (base + ((255 - base) as f64 * hover.clamp(0.0, 1.0)) as i32).clamp(0, 255) as u8
}

/// `dwViewFlags & 1`: navigation disabled (folder activation opens Explorer instead).
pub const VIEW_FLAG_NO_NAVIGATION: u32 = 0x1;

fn is_zero(v: &u32) -> bool {
    *v == 0
}

fn is_zero_signed(v: &i32) -> bool {
    *v == 0
}

fn is_false(v: &bool) -> bool {
    !*v
}

/// Corral-owned position of a loose desktop item that the user moved (Explorer's own saved layout
/// is never written). DIPs relative to the named monitor's work area.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoosePosition {
    pub monitor: String,
    pub x: i32,
    pub y: i32,
}

/// round-up-spec 3: an explicit membership choice survives automatic runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualPlacement {
    pub placed_ms: u64,
    pub last_seen_ms: u64,
}

/// A manual mark is dropped once its item has been gone this long (30 days).
pub const MANUAL_FORGET_MS: u64 = 30 * 24 * 60 * 60 * 1000;
const MANUAL_REFRESH_MS: u64 = 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub version: u32,
    pub next_id: u64,
    /// Independent tab-set allocator high-water mark (never reuse dissolved set IDs).
    #[serde(default = "initial_tab_set_id")]
    pub last_tab_set_id: u64,
    /// Corral layout lock gate. No settings UI or registry binding is implied.
    #[serde(default, skip_serializing_if = "is_false")]
    pub desktop_locked: bool,
    /// Clear view exclusion flags of Corrals that are or were tabs, keyed by stable Corral id (tabs-spec
    /// 2.5): the command writes one value to every tab, a join or a tear-off leaves each flag as it is.
    /// Kept in the layout so tab edits need only the layout-save transaction.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub tab_quick_hide_exclusions: std::collections::BTreeMap<u64, bool>,
    pub groups: Vec<Group>,
    /// Loose items moved within Corral, keyed by parsing name (lower-case).
    #[serde(default)]
    pub loose: std::collections::BTreeMap<String, LoosePosition>,
    /// Nonzero Pages assignment of loose icons, including icons with no Corral-owned position.
    /// Lower-case parsing names; the current page is runtime-only and never belongs in a layout.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub icon_pages: std::collections::BTreeMap<String, i32>,
    /// Folder Gate preferences (folder-gate-views-spec 7), shared by folder key.
    #[serde(default, skip_serializing_if = "crate::view_state::ViewStates::is_empty")]
    pub view_states: crate::view_state::ViewStates,
    /// folder-gate-views-spec 7: raw non-object entries archived only when that folder is patched.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub view_unparsed: std::collections::BTreeMap<String, crate::view_state::StoredJson>,
    /// Parsing names (lower-case) the rules evaluator already treated as new once; an untouched
    /// loose icon is evaluated only the first time (an icon is evaluated when it has no
    /// saved settings row).
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub seen: std::collections::BTreeSet<String>,
    /// The seen set has been established. False in layouts written before the set existed: the
    /// first start then records every present item as seen without evaluating it.
    #[serde(default)]
    pub seen_initialized: bool,
    /// Round-up's one-time pass has run: present items counted as seen, Corral members marked as
    /// placed by the user, nothing moved (round-up-spec 3, "First start with an older layout").
    /// False in every layout written before Round-up, whatever the old engine recorded.
    #[serde(default)]
    pub round_up_initialized: bool,
    /// Items whose Corral the user chose by hand, keyed by lower-case parsing name (round-up-spec 3).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub manual: std::collections::BTreeMap<String, ManualPlacement>,
    /// Saved desk setups (desk-setups-spec 5). Legacy keys are migration input only.
    #[serde(default)]
    pub desk_setups: crate::desk_setup::DeskSetups,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub profiles: serde_json::Value,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub active_profile: String,
}

impl Layout {
    /// camouflage-spec 5.2: turn each Corral's legacy packed tint choice into the explicit source before the
    /// layout is written (the legacy value is never written back). `global_color` is the shared Corral colour.
    pub fn migrate_tint_sources(&mut self, global_color: u32) {
        for group in &mut self.groups { group.style.migrate_tint_source(global_color, 0); }
    }

    pub fn new() -> Self {
        Self {
            version: LAYOUT_VERSION,
            next_id: 1,
            last_tab_set_id: 1,
            desktop_locked: false,
            tab_quick_hide_exclusions: std::collections::BTreeMap::new(),
            groups: Vec::new(),
            loose: Default::default(),
            icon_pages: Default::default(),
            view_states: Default::default(),
            view_unparsed: Default::default(),
            seen: Default::default(),
            manual: Default::default(),
            // a brand-new layout has nothing to protect: its first items are evaluated as new
            seen_initialized: true,
            round_up_initialized: true,
            desk_setups: Default::default(),
            profiles: Default::default(),
            active_profile: String::new(),
        }
    }

    pub fn is_seen(&self, item: &str) -> bool {
        self.seen.contains(&item.to_lowercase())
    }

    /// The user placed `item` by hand: automatic runs leave it alone (round-up-spec 3).
    pub fn is_manual(&self, item: &str) -> bool {
        self.manual.contains_key(&item.to_lowercase())
    }
    pub fn mark_manual(&mut self, item: &str, now_ms: u64) {
        self.manual.insert(
            item.to_lowercase(),
            ManualPlacement { placed_ms: now_ms, last_seen_ms: now_ms },
        );
    }
    pub fn mark_manual_now(&mut self, item: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        self.mark_manual(item, now);
    }
    pub fn clear_manual(&mut self, item: &str) {
        self.manual.remove(&item.to_lowercase());
    }
    /// Round-up-spec 3, "Seen" and "Manual placement", once per read of the desktop.
    ///
    /// On the first start with a layout from before Round-up (`round_up_initialized` absent) every
    /// present item counts as seen, nothing is moved, and every item inside a Corral (present or
    /// not) becomes manual: its membership came from the user or an older version. Manual marks of
    /// items present now are refreshed (daily); marks of items absent for 30 days are dropped.
    /// Returns whether anything changed.
    pub fn round_up_observe(&mut self, present: &[String], now_ms: u64) -> bool {
        if !self.round_up_initialized && !present.is_empty() {
            // The one-time pass: nothing moves, nothing is pruned.
            for name in present {
                self.mark_seen(name);
            }
            // Names the layout already knows, in case an early-boot read missed some icons.
            let known: Vec<String> = self
                .loose
                .keys()
                .chain(self.icon_pages.keys())
                .cloned()
                .collect();
            for name in known {
                self.mark_seen(&name);
            }
            let members: Vec<String> = self
                .groups
                .iter()
                .filter_map(|g| match &g.content {
                    GroupContent::Items { items } => Some(items.clone()),
                    GroupContent::Portal { .. } => None,
                })
                .flatten()
                .collect();
            for name in members {
                self.mark_manual(&name, now_ms);
                self.mark_seen(&name);
            }
            self.seen_initialized = true;
            self.round_up_initialized = true;
            return true;
        }
        let before = (self.seen.len(), self.manual.clone());
        let keys: std::collections::BTreeSet<String> = present.iter().map(|s| s.to_lowercase()).collect();
        // Refreshed at most daily, so reading the desktop does not rewrite the layout every time.
        for (name, mark) in &mut self.manual {
            if keys.contains(name) && now_ms.saturating_sub(mark.last_seen_ms) >= MANUAL_REFRESH_MS {
                mark.last_seen_ms = now_ms;
            }
        }
        self.manual.retain(|_, mark| now_ms.saturating_sub(mark.last_seen_ms) < MANUAL_FORGET_MS);
        before != (self.seen.len(), self.manual.clone())
    }

    /// Record `item` as evaluated; true when it was not recorded before.
    pub fn mark_seen(&mut self, item: &str) -> bool {
        self.seen.insert(item.to_lowercase())
    }

    /// Insert `item` into group `id` at `index` (clamped), removing it from everywhere else.
    pub fn insert_item(&mut self, item: &str, id: u64, index: usize) {
        self.assign_item(item, None);
        self.loose.remove(&item.to_lowercase());
        self.icon_pages.remove(&item.to_lowercase());
        if let Some(group) = self.group_mut(id) {
            if let GroupContent::Items { items } = &mut group.content {
                let at = index.min(items.len());
                items.insert(at, item.to_string());
            }
        }
    }

    /// Make `item` loose at a Corral-owned position.
    pub fn place_loose(&mut self, item: &str, position: LoosePosition) {
        self.assign_item(item, None);
        self.loose.insert(item.to_lowercase(), position);
    }

    pub fn loose_position(&self, item: &str) -> Option<&LoosePosition> {
        self.loose.get(&item.to_lowercase())
    }

    /// Loose icon page. Grouped icons follow their group's `swipe_index` instead.
    pub fn icon_page(&self, item: &str) -> i32 {
        self.icon_pages.get(&item.to_lowercase()).copied().unwrap_or(0)
    }

    /// Assign a loose icon to a page without creating or changing a saved position.
    pub fn set_icon_page(&mut self, item: &str, page: i32) {
        let key = item.to_lowercase();
        if page == 0 {
            self.icon_pages.remove(&key);
        } else {
            self.icon_pages.insert(key, page);
        }
    }

    /// A successful Shell rename keeps a loose icon's page even without an owned position.
    pub fn rename_icon_page(&mut self, old: &str, new: &str) {
        let page = self.icon_pages.remove(&old.to_lowercase()).unwrap_or(0);
        self.set_icon_page(new, page);
    }

    pub fn allocate_id(&mut self) -> u64 {
        let used = self.groups.iter().map(|g| g.id).max().unwrap_or(0);
        let id = self.next_id.max(used + 1);
        self.next_id = id + 1;
        id
    }

    pub fn add_group(&mut self, title: String, monitor: String, rect: Rect, content: GroupContent) -> u64 {
        let id = self.allocate_id();
        self.groups.push(Group { id, tab: None, title, monitor, rect, preferred_size_dip: None, content, roll: RollState::Expanded, dock_top: false, dock_bottom: false, scroll: 0, nav_history: Vec::new(), view_flags: 0, hide_link_overlays: false, sort_method: 0, sort_flags: 0, swipe_index: 0, all_pages: false, opacity: 255, style: GroupStyle::default(), title_side: -1, title_side_last: -1, icon_zoom: None, legacy_scroll: IgnoredKey, extra: Default::default() });
        id
    }

    /// Corral choice: switching "title underneath near the screen top" on hands every Corral back to
    /// the automatic side. An explicit side (`Edge` 0/1) only arrives from imported or scripted
    /// layouts: no Corral UI sets or shows it. Returns the
    /// ids that changed; the stored last side (`title_side_last`) is kept.
    pub fn release_title_sides(&mut self) -> Vec<u64> {
        let mut changed = Vec::new();
        for g in self.groups.iter_mut().filter(|g| g.title_side != -1) {
            g.title_side = -1;
            changed.push(g.id);
        }
        changed
    }

    /// A translucent Corral makes the global opacity override available in View > Opacity
    /// (the flags are computed over all groups).
    pub fn any_translucent(&self) -> bool {
        self.groups.iter().any(|g| g.opacity != 255)
    }

    pub fn group_mut(&mut self, id: u64) -> Option<&mut Group> {
        self.groups.iter_mut().find(|g| g.id == id)
    }

    /// Sets the icon zoom of one Corral; each tab of a set keeps its own (tabs-spec 2.4). Returns
    /// whether anything changed. Layout only.
    pub fn set_icon_zoom(&mut self, id: u64, zoom: Option<u16>) -> bool {
        let Some(g) = self.groups.iter_mut().find(|g| g.id == id) else { return false };
        let changed = g.icon_zoom != zoom;
        g.icon_zoom = zoom;
        changed
    }

    /// The data the tab operations read and write (tabs-spec 7).
    pub fn tab_doc(&self) -> crate::tabs::TabDoc {
        crate::tabs::TabDoc { groups: self.groups.clone(), next_tab_set: self.last_tab_set_id }
    }

    /// Swap in the result of a tab operation that already passed `tabs::validate`. True when
    /// anything changed. Used for edits that follow the normal debounced layout save (selection, the
    /// live steps of a gesture); joins and separations go through [`Layout::commit_tab_doc`].
    pub fn apply_tab_doc(&mut self, doc: crate::tabs::TabDoc) -> bool {
        let changed = self.groups != doc.groups || self.last_tab_set_id != doc.next_tab_set;
        self.groups = doc.groups;
        self.last_tab_set_id = doc.next_tab_set.max(self.last_tab_set_id);
        changed
    }

    /// Atomic tab edit (tabs-spec 7): build the new layout, write it, and only then swap it in. If
    /// the write fails nothing changed and the error says so. `Ok(false)` = nothing to do.
    pub fn commit_tab_doc(&mut self, doc: crate::tabs::TabDoc, write: impl FnOnce(&Layout) -> io::Result<()>) -> Result<bool, TabCommitError> {
        if self.desktop_locked { return Err(TabCommitError::Tab(crate::tabs::TabError::Locked)) }
        let mut next = self.clone();
        if !next.apply_tab_doc(doc) { return Ok(false) }
        write(&next).map_err(TabCommitError::Write)?;
        *self = next;
        Ok(true)
    }

    /// Ids must be unique; a layout with a repeated id is rejected on load.
    pub fn check_group_ids(&self) -> Result<(), crate::tabs::TabError> {
        let mut seen = std::collections::BTreeSet::new();
        if self.groups.iter().all(|g| seen.insert(g.id)) { Ok(()) } else { Err(crate::tabs::TabError::Invalid("a Corral id appears twice")) }
    }

    #[cfg(test)]
    pub fn join_tabs(&mut self, target: u64, source: u64) -> Result<bool, crate::tabs::TabError> {
        if self.desktop_locked { return Err(crate::tabs::TabError::Locked) }
        let policy = crate::tabs::JoinPolicy { create: true, titles: true };
        let doc = crate::tabs::join(&self.tab_doc(), target, source, None, &policy)?;
        Ok(self.apply_tab_doc(doc))
    }

    /// Propagate the selected tab's current geometry/Range/fold/dock/title side to its set
    /// (tabs-spec 2.3). Only the selected tab speaks for the set.
    pub fn sync_tab_common(&mut self, active: u64) -> Result<bool, crate::tabs::TabError> {
        if !self.groups.iter().any(|g| g.id == active) { return Err(crate::tabs::TabError::MissingMember(active)) }
        Ok(crate::tabs::sync_shared(&mut self.groups, active))
    }

    /// After a geometry change that may have touched several Corrals (a desk setup, a display
    /// change): every set takes the selected tab's value (tabs-spec 2.3).
    pub fn sync_all_tab_sets(&mut self) {
        let selected: Vec<u64> = crate::tabs::normalize_sets(&self.groups).iter().map(|s| s.selected_id()).collect();
        for id in selected { crate::tabs::sync_shared(&mut self.groups, id); }
    }

    /// The set-wide Clear view exclusion: the flag is written to every tab (tabs-spec 2.5).
    pub fn set_tab_exclusion(&mut self, member: u64, excluded: bool) -> Result<(), crate::tabs::TabError> {
        let view = crate::tabs::collect(&self.groups, member).ok_or(crate::tabs::TabError::MissingMember(member))?;
        for id in view.members { self.tab_quick_hide_exclusions.insert(id, excluded); }
        Ok(())
    }

    /// Remove exactly one Corral record, releasing ordinary icon references to its page; its tab set
    /// shrinks by the tab rules. This does not read, create, move or delete any Desktop/portal file.
    pub fn remove_tab_record(&mut self, member: u64, current_page: i32) -> Result<(), crate::tabs::TabError> {
        if self.desktop_locked { return Err(crate::tabs::TabError::Locked) }
        let group = self.groups.iter().find(|g| g.id == member).ok_or(crate::tabs::TabError::MissingMember(member))?;
        let page = if group.all_pages { current_page } else { group.swipe_index };
        let items = match &group.content { GroupContent::Items { items } => items.clone(), _ => Vec::new() };
        let doc = crate::tabs::delete(&self.tab_doc(), member)?;
        self.apply_tab_doc(doc);
        self.tab_quick_hide_exclusions.remove(&member);
        for name in items { self.set_icon_page(&name, page); }
        Ok(())
    }

    /// Move an item reference into `target` (removing it from any other group). Files are untouched.
    pub fn assign_item(&mut self, item: &str, target: Option<u64>) {
        if target.is_some() {
            self.loose.remove(&item.to_lowercase());
            self.icon_pages.remove(&item.to_lowercase());
        }
        for group in &mut self.groups {
            if let GroupContent::Items { items } = &mut group.content {
                items.retain(|i| !i.eq_ignore_ascii_case(item));
            }
        }
        if let Some(group) = target.and_then(|id| self.group_mut(id)) {
            if let GroupContent::Items { items } = &mut group.content {
                items.push(item.to_string());
            }
        }
    }

    pub fn group_of(&self, item: &str) -> Option<u64> {
        self.groups.iter().find_map(|g| match &g.content {
            GroupContent::Items { items } if items.iter().any(|i| i.eq_ignore_ascii_case(item)) => Some(g.id),
            _ => None,
        })
    }
}

/// Why [`Layout::commit_tab_doc`] changed nothing.
#[derive(Debug)]
pub enum TabCommitError {
    Tab(crate::tabs::TabError),
    Write(io::Error),
}

#[derive(Debug)]
pub enum LoadError {
    Io(io::Error),
    Parse(serde_json::Error),
    UnsupportedVersion(u32),
    InvalidTabs(crate::tabs::TabError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "cannot read layout: {e}"),
            LoadError::Parse(e) => write!(f, "invalid layout: {e}"),
            LoadError::UnsupportedVersion(v) => write!(f, "unsupported layout version {v}"),
            LoadError::InvalidTabs(e) => write!(f, "invalid tab membership: {e:?}"),
        }
    }
}

pub fn load(path: &Path) -> Result<Layout, LoadError> {
    let text = fs::read_to_string(path).map_err(LoadError::Io)?;
    // Windows PowerShell 5 writes UTF-8 with a BOM; serde_json rejects it.
    let mut layout: Layout = serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(LoadError::Parse)?;
    if layout.version != LAYOUT_VERSION {
        return Err(LoadError::UnsupportedVersion(layout.version));
    }
    layout.check_group_ids().map_err(LoadError::InvalidTabs)?;
    crate::desk_setup::migrate(&mut layout);
    Ok(layout)
}

/// Load `path`, falling back to `path.bak` if the primary is missing or corrupt.
pub fn load_with_backup(path: &Path) -> Result<Layout, LoadError> {
    load(path).or_else(|primary| load(&backup_path(path)).map_err(|_| primary))
}

pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

/// Atomic save: write `<path>.tmp`, flush to disk, keep the previous file as `<path>.bak`, rename.
pub fn save(path: &Path, layout: &Layout) -> io::Result<()> {
    // desk-setups-spec 5.4: never replace old keys until the byte backup succeeded.
    crate::desk_setup::ensure_migration_backup(path)?;
    let mut migrated = layout.clone();
    crate::desk_setup::migrate(&mut migrated);
    let layout = &migrated;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    {
        let mut file = fs::File::create(&temp)?;
        file.write_all(serde_json::to_string_pretty(layout).map_err(io::Error::other)?.as_bytes())?;
        file.sync_all()?;
    }
    if path.exists() {
        fs::copy(path, backup_path(path))?;
    }
    fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scroll_offset_is_never_persisted_and_old_values_are_ignored() {
        let mut layout = Layout::new();
        layout.add_group("S".into(), "m".into(), Rect::new(0, 0, 100, 100), GroupContent::Items { items: vec![] });
        layout.groups[0].scroll = 37;
        let text = serde_json::to_string(&layout).unwrap();
        assert!(!text.contains("\"scroll\""), "scrollbar-spec 1: the offset is never written");
        let legacy = text.replacen("\"roll\":", "\"scroll\":19,\"roll\":", 1);
        assert!(legacy.contains("\"scroll\":19"));
        let back: Layout = serde_json::from_str(&legacy).unwrap();
        assert_eq!(back.groups[0].scroll, 0, "a stored offset from an older build is ignored");
        assert!(!serde_json::to_string(&back).unwrap().contains("\"scroll\""), "and not carried as an unknown key either");
    }

    #[test]
    fn icon_zoom_is_absent_by_default_persists_and_tolerates_garbage() {
        let mut layout = Layout::new();
        let id = layout.add_group("Z".into(), "m".into(), Rect::new(0, 0, 100, 100), GroupContent::Items { items: vec![] });
        let plain = serde_json::to_string(&layout).unwrap();
        assert!(!plain.contains("icon_zoom"), "absent while None: old readers and old files see no change");
        assert_eq!(layout.groups[0].icon_zoom, None);
        // an old layout (no field) loads unchanged
        let old: Layout = serde_json::from_str(&plain).unwrap();
        assert_eq!(old, layout);
        // set -> round trip through the real atomic save/load
        assert!(layout.set_icon_zoom(id, Some(200)));
        assert!(!layout.set_icon_zoom(id, Some(200)), "unchanged is not a change");
        let dir = std::env::temp_dir().join(format!("corral-zoom-{}", std::process::id()));
        let path = dir.join("layout.json");
        save(&path, &layout).unwrap();
        let back = load(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(back.groups[0].icon_zoom, Some(200));
        assert_eq!(back, layout);
        assert!(layout.set_icon_zoom(id, None));
        assert!(!serde_json::to_string(&layout).unwrap().contains("icon_zoom"), "back to default drops the key");
        // garbage tolerated: the layout still loads and the Corral follows the desktop
        for bad in ["-5", "70000", "\"big\"", "1.5", "null", "[1]"] {
            let text = plain.replacen("\"title\"", &format!("\"icon_zoom\": {bad}, \"title\""), 1);
            let l: Layout = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{bad}: {e}"));
            assert_eq!(l.groups[0].icon_zoom, None, "{bad}");
        }
        // unknown but valid numbers load as stored (the app clamps them when it draws)
        let text = plain.replacen("\"title\"", "\"icon_zoom\": 9000, \"title\"", 1);
        assert_eq!(serde_json::from_str::<Layout>(&text).unwrap().groups[0].icon_zoom, Some(9000));
    }

    #[test]
    fn icon_zoom_is_kept_per_tab() {
        let mut layout = Layout::new();
        let a = layout.add_group("A".into(), "m".into(), Rect::new(0, 0, 100, 100), GroupContent::Items { items: vec![] });
        let b = layout.add_group("B".into(), "m".into(), Rect::new(0, 0, 100, 100), GroupContent::Items { items: vec![] });
        let c = layout.add_group("C".into(), "m".into(), Rect::new(200, 0, 300, 100), GroupContent::Items { items: vec![] });
        layout.join_tabs(a, b).unwrap();
        assert!(layout.set_icon_zoom(a, Some(133)));
        let zoom = |l: &Layout, id| l.groups.iter().find(|g| g.id == id).unwrap().icon_zoom;
        assert_eq!((zoom(&layout, a), zoom(&layout, b), zoom(&layout, c)), (Some(133), None, None), "tabs-spec 2.4: zoom travels with the Corral");
        assert!(layout.set_icon_zoom(b, Some(80)));
        assert!(layout.set_icon_zoom(a, None));
        assert_eq!((zoom(&layout, a), zoom(&layout, b)), (None, Some(80)));
        assert!(!layout.set_icon_zoom(99, Some(1)));
    }

    #[test]
    fn hide_link_overlays_defaults_off_and_round_trips() {
        let mut layout = Layout::new();
        let id = layout.add_group("P".into(), "m".into(), Rect::new(0, 0, 100, 100), GroupContent::Portal { path: "scratch".into() });
        let json = serde_json::to_string(&layout).unwrap();
        assert!(!json.contains("hide_link_overlays"), "absent while off: old readers see no change");
        layout.group_mut(id).unwrap().hide_link_overlays = true;
        let back: Layout = serde_json::from_str(&serde_json::to_string(&layout).unwrap()).unwrap();
        assert!(back.groups[0].hide_link_overlays);
        let old: Layout = serde_json::from_str(&json).unwrap();
        assert!(!old.groups[0].hide_link_overlays);
    }

    fn two_tabs() -> Layout {
        let mut layout = Layout::new();
        layout.add_group("Files".into(), "m".into(), Rect::new(20, 100, 320, 300), GroupContent::Items { items: vec!["item-a".into()] });
        layout.add_group("Folder".into(), "other".into(), Rect::new(50, 150, 550, 450), GroupContent::Portal { path: "scratch".into() });
        for (index, group) in layout.groups.iter_mut().enumerate() {
            group.tab = Some(crate::tabs::TabMembership { set_id: 12, index: index as u32, selected: index == 1 });
        }
        layout.groups[0].scroll = 20;
        layout.groups[0].sort_method = 2;
        layout.groups[0].style.color = Some(0x332211);
        layout.groups[1].nav_history = vec!["scratch/subfolder".into(), "scratch".into()];
        layout.groups[1].view_flags = 1;
        layout.groups[1].opacity = 153;
        layout.groups[1].swipe_index = -2;
        layout
    }

    #[test]
    fn tabs_are_read_as_stored_and_a_load_save_cycle_of_this_build_is_bit_identical() {
        // A file written by this build (not by other tools). tabs-spec 8: members with different rects, duplicate indices and two selected tabs are not
        // repaired on disk by loading or saving.
        let dir = temp_dir("tabs-bit-identical");
        let path = dir.join("layout.json");
        let mut layout = two_tabs();
        layout.last_tab_set_id = 3; // below the existing set id 12
        layout.groups[0].tab.as_mut().unwrap().index = 8;
        layout.groups[1].tab.as_mut().unwrap().index = 8;
        layout.groups[0].tab.as_mut().unwrap().selected = true;
        save(&path, &layout).unwrap();
        let first = fs::read(&path).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.last_tab_set_id, 3, "the stored counter is not raised by a load");
        assert_eq!((loaded.groups[0].tab, loaded.groups[1].tab), (layout.groups[0].tab, layout.groups[1].tab));
        assert_eq!(loaded.groups[0].rect, layout.groups[0].rect, "mismatched member rects stay as stored");
        save(&path, &loaded).unwrap();
        assert_eq!(fs::read(&path).unwrap(), first);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_corral_level_keys_survive_a_load_and_save() {
        let mut layout = Layout::new();
        layout.add_group("A".into(), "m".into(), Rect::new(0, 0, 10, 10), GroupContent::Items { items: vec![] });
        let text = serde_json::to_string(&layout).unwrap().replacen("\"title\"", "\"future_key\":{\"x\":[1,2]},\"title\"", 1);
        let loaded: Layout = serde_json::from_str(&text).unwrap();
        assert_eq!(loaded.groups[0].extra.get("future_key"), Some(&serde_json::json!({"x": [1, 2]})));
        assert!(serde_json::to_string(&loaded).unwrap().contains("\"future_key\":{\"x\":[1,2]}"));
    }

    #[test]
    fn tabs_duplicate_corral_id_is_rejected_and_the_backup_is_used() {
        let mut layout = two_tabs();
        layout.groups[1].id = layout.groups[0].id;
        assert!(layout.check_group_ids().is_err());
        let dir = temp_dir("tabs-backup");
        let path = dir.join("layout.json");
        save(&path, &two_tabs()).unwrap();
        save(&path, &layout).unwrap();
        assert!(matches!(load(&path), Err(LoadError::InvalidTabs(_))));
        assert_eq!(load_with_backup(&path).unwrap().groups.len(), 2);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_write_changes_nothing_and_the_file_is_always_a_complete_layout() {
        // tabs-spec 7: write first, swap after. The injected failure and a real unwritable path both
        // leave the live layout and the file as they were.
        let dir = temp_dir("tabs-commit");
        let path = dir.join("layout.json");
        let mut layout = Layout::new();
        for n in 0..3 { layout.add_group(format!("G{n}"), "m".into(), Rect::new(n * 50, 0, n * 50 + 40, 40), GroupContent::Items { items: vec![format!("i{n}")] }); }
        save(&path, &layout).unwrap();
        let on_disk = fs::read(&path).unwrap();
        let join = |l: &Layout| crate::tabs::join(&l.tab_doc(), 1, 2, None, &crate::tabs::JoinPolicy { create: true, titles: true }).unwrap();
        let before = layout.clone();
        let doc = join(&layout);
        let injected = layout.commit_tab_doc(doc.clone(), |_| Err(io::Error::other("injected")));
        assert!(matches!(injected, Err(TabCommitError::Write(_))));
        assert_eq!(layout, before, "the live state stays");
        let blocked = dir.join("file.txt");
        fs::write(&blocked, b"x").unwrap();
        let unwritable = layout.commit_tab_doc(doc.clone(), |l| save(&blocked.join("layout.json"), l));
        assert!(matches!(unwritable, Err(TabCommitError::Write(_))));
        assert_eq!(layout, before);
        assert_eq!(fs::read(&path).unwrap(), on_disk, "the file is the old complete layout");
        assert_eq!(layout.commit_tab_doc(doc, |l| save(&path, l)).unwrap(), true);
        assert_eq!(load(&path).unwrap().groups[1].tab.map(|t| t.set_id), Some(1), "the file is the new complete layout");
        assert_eq!(layout.last_tab_set_id, 2);
        layout.desktop_locked = true;
        let locked = join(&before);
        assert!(matches!(layout.commit_tab_doc(locked, |_| Ok(())), Err(TabCommitError::Tab(crate::tabs::TabError::Locked))));
        fs::remove_dir_all(dir).unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("corral-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn renaming_an_unpositioned_icon_keeps_its_page_and_removes_the_old_key() {
        let mut layout = Layout::new();
        layout.set_icon_page("Old.TXT", -2);
        layout.rename_icon_page("OLD.txt", "New.txt");
        assert_eq!(layout.icon_page("old.txt"), 0);
        assert_eq!(layout.icon_page("NEW.TXT"), -2);
        assert!(layout.loose.is_empty());
        layout.rename_icon_page("New.txt", "NEW.TXT");
        assert_eq!(layout.icon_page("new.txt"), -2);
    }

    #[test]
    fn seen_set_is_case_insensitive_and_old_layouts_load_uninitialized() {
        let mut layout = Layout::new();
        assert!(layout.seen_initialized, "a brand-new layout evaluates its first items");
        assert!(layout.mark_seen("C:/Desktop/A.txt"));
        assert!(!layout.mark_seen("c:/desktop/a.TXT"));
        assert!(layout.is_seen("C:/DESKTOP/a.txt") && !layout.is_seen("b"));
        let back: Layout = serde_json::from_str(&serde_json::to_string(&layout).unwrap()).unwrap();
        assert_eq!(back, layout);
        let old: Layout = serde_json::from_str(r#"{"version":1,"next_id":1,"groups":[]}"#).unwrap();
        assert!(!old.seen_initialized && old.seen.is_empty(), "layout from before the seen set");
    }

    #[test]
    fn ids_are_unique_and_monotonic() {
        let mut layout = Layout::new();
        let a = layout.add_group("A".into(), "m".into(), Rect::new(0, 0, 10, 10), GroupContent::Items { items: vec![] });
        let b = layout.add_group("B".into(), "m".into(), Rect::new(0, 0, 10, 10), GroupContent::Items { items: vec![] });
        assert!(b > a);
        layout.groups.retain(|g| g.id != b);
        assert!(layout.allocate_id() > b, "ids are never reused within a layout");
    }

    #[test]
    fn assigning_moves_reference_not_duplicate() {
        let mut layout = Layout::new();
        let a = layout.add_group("A".into(), "m".into(), Rect::default(), GroupContent::Items { items: vec![] });
        let b = layout.add_group("B".into(), "m".into(), Rect::default(), GroupContent::Items { items: vec![] });
        layout.assign_item(r"C:\Users\x\Desktop\a.txt", Some(a));
        layout.assign_item(r"c:\users\x\desktop\A.TXT", Some(b));
        assert_eq!(layout.group_of(r"C:\Users\x\Desktop\a.txt"), Some(b));
        layout.assign_item(r"C:\Users\x\Desktop\a.txt", None);
        assert_eq!(layout.group_of(r"C:\Users\x\Desktop\a.txt"), None);
    }

    #[test]
    fn insert_and_place_loose_keep_one_home_per_item() {
        let mut layout = Layout::new();
        let a = layout.add_group("A".into(), "m".into(), Rect::default(), GroupContent::Items { items: vec!["x".into(), "y".into()] });
        layout.insert_item("Z", a, 1);
        assert_eq!(layout.groups[0].content, GroupContent::Items { items: vec!["x".into(), "Z".into(), "y".into()] });
        layout.insert_item("x", a, 99);
        assert_eq!(layout.groups[0].content, GroupContent::Items { items: vec!["Z".into(), "y".into(), "x".into()] });
        layout.place_loose("Y", LoosePosition { monitor: "m".into(), x: 10, y: 20 });
        assert_eq!(layout.group_of("y"), None);
        assert_eq!(layout.loose_position("y").map(|p| (p.x, p.y)), Some((10, 20)));
        layout.insert_item("y", a, 0);
        assert!(layout.loose_position("y").is_none(), "moving into a group forgets the loose position");
    }

    #[test]
    fn older_groups_load_with_default_opacity_style_and_title_side() {
        let text = r#"{"version":1,"next_id":2,"groups":[{"id":1,"title":"A","monitor":"m","rect":{"left":0,"top":0,"right":10,"bottom":10},"content":{"kind":"items","items":[]}}]}"#;
        let layout: Layout = serde_json::from_str(text).unwrap();
        let g = &layout.groups[0];
        assert_eq!((g.opacity, g.title_side, g.title_side_last), (255, -1, -1));
        assert!(g.style.is_empty());
        assert!(g.tab.is_none());
        assert_eq!(layout.last_tab_set_id, 1);
        // defaults are not written back
        let json = serde_json::to_string(&layout).unwrap();
        assert!(!json.contains("opacity") && !json.contains("style") && !json.contains("title_side"));
    }

    #[test]
    fn old_layout_pages_default_to_zero_and_are_omitted() {
        let text = r#"{"version":1,"next_id":2,"groups":[{"id":1,"title":"A","monitor":"m","rect":{"left":0,"top":0,"right":10,"bottom":10},"content":{"kind":"items","items":["a"]}}]}"#;
        let layout: Layout = serde_json::from_str(text).unwrap();
        assert_eq!(layout.groups[0].swipe_index, 0);
        assert!(!layout.groups[0].all_pages);
        assert_eq!(layout.icon_page("unpositioned"), 0);
        assert!(layout.icon_pages.is_empty());
        let json = serde_json::to_string(&layout).unwrap();
        assert!(!json.contains("swipe_index") && !json.contains("all_pages") && !json.contains("icon_pages"));
        assert_eq!(serde_json::from_str::<Layout>(&json).unwrap(), layout);
    }

    #[test]
    fn pages_roundtrip_signed_group_and_unpositioned_loose_icon() {
        let mut layout = Layout::new();
        let id = layout.add_group("Across pages".into(), "m".into(), Rect::default(), GroupContent::Items { items: vec![] });
        let group = layout.group_mut(id).unwrap();
        group.swipe_index = -3;
        group.all_pages = true;
        layout.set_icon_page("C:/Desktop/Unmoved.TXT", 2);
        assert!(layout.loose.is_empty(), "page assignment must not manufacture a position");
        let json = serde_json::to_string(&layout).unwrap();
        let restored: Layout = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, layout);
        assert_eq!(restored.icon_page("c:/DESKTOP/unmoved.txt"), 2);
        assert!(!json.contains("current_page"), "current page is session-only");
        layout.set_icon_page("C:/Desktop/Unmoved.TXT", 0);
        assert!(layout.icon_pages.is_empty(), "zero is the default, not a stored row");
    }

    #[test]
    fn regrouping_removes_loose_page_and_loose_moves_preserve_it() {
        let mut layout = Layout::new();
        let id = layout.add_group("A".into(), "m".into(), Rect::default(), GroupContent::Items { items: vec![] });
        layout.set_icon_page("File", -2);
        layout.place_loose("FILE", LoosePosition { monitor: "m".into(), x: 10, y: 20 });
        assert_eq!(layout.icon_page("file"), -2, "moving within loose space preserves the page");
        layout.assign_item("file", Some(id));
        assert_eq!(layout.icon_page("FILE"), 0);
        assert!(layout.loose_position("file").is_none());
        layout.assign_item("file", None);
        layout.set_icon_page("File", 3);
        layout.insert_item("FILE", id, 0);
        assert_eq!(layout.icon_page("file"), 0, "ordered insertion also clears the old loose page");
    }

    #[test]
    fn opacity_style_and_title_side_roundtrip() {
        let mut layout = Layout::new();
        let id = layout.add_group("A".into(), "m".into(), Rect::default(), GroupContent::Items { items: vec![] });
        let g = layout.group_mut(id).unwrap();
        g.opacity = 0x66;
        g.style.color = Some(0x0011_2233);
        g.style.font_name = Some("Segoe UI".into());
        g.title_side_last = 1;
        let text = serde_json::to_string(&layout).unwrap();
        assert!(text.contains(r#""opacity":102"#) && text.contains(r#""color":1122867"#) && text.contains("title_side_last"));
        assert!(!text.contains("text_color"), "absent style values stay absent");
        let back: Layout = serde_json::from_str(&text).unwrap();
        assert_eq!(back, layout);
        assert!(layout.any_translucent());
    }

    #[test]
    fn q33j_switching_title_underneath_on_releases_explicit_sides() {
        let mut layout = Layout::new();
        let ids: Vec<u64> = (0..3).map(|i| layout.add_group(format!("C{i}"), "m".into(), Rect::default(), GroupContent::Items { items: vec![] })).collect();
        layout.groups[0].title_side = 0; // QA scripts and imports wrote an explicit "above"
        layout.groups[1].title_side = 1;
        layout.groups[1].title_side_last = 1;
        assert_eq!(layout.release_title_sides(), vec![ids[0], ids[1]]);
        assert!(layout.groups.iter().all(|g| g.title_side == -1));
        assert_eq!(layout.groups[1].title_side_last, 1, "the last side is not touched");
        assert!(layout.release_title_sides().is_empty());
        // Now the rule follows the setting near the top, and the side is serialized as absent.
        assert!(title_below(layout.groups[0].title_side, 0, true, true));
        assert!(!serde_json::to_string(&layout.groups[0]).unwrap().contains("\"title_side\""));
    }

    #[test]
    fn title_side_rule() {
        // auto: below near the top only while TitleEdgeAutoAdjust is on, or when it already was below
        assert!(title_below(-1, -1, true, true));
        assert!(!title_below(-1, -1, true, false));
        assert!(!title_below(-1, -1, false, true), "setting off, was above: stays above");
        assert!(title_below(-1, 1, false, true), "setting off, was below: stays below");
        assert!(!title_below(-1, 1, false, false), "moved away from the top: back above");
        assert!(!title_below(-1, 0, false, true));
        // an explicit preference wins over the setting and the position
        assert!(title_below(1, -1, false, false));
        assert!(!title_below(0, 1, true, true));
    }

    #[test]
    fn fade_follows_opacity_hover_and_the_disable_switch() {
        assert_eq!(group_fade(255, 0.0, false, false), 255);
        assert_eq!(group_fade(0, 0.0, false, false), 0, "0 % is invisible at rest");
        assert_eq!(group_fade(0, 1.0, false, false), 255, "... and fully visible on mouseover");
        assert_eq!(group_fade(0x66, 0.0, false, false), 0x66);
        assert_eq!(group_fade(0x33, 0.5, false, false), 0x33 + ((255 - 0x33) as f64 * 0.5) as u8);
        assert_eq!(group_fade(0, 0.0, true, false), 255, "Ignore individual Corral opacity");
        assert_eq!(group_fade(0, 0.0, false, true), 1, "portals keep at least 1");
    }

    #[test]
    fn layout_file_with_utf8_bom_loads() {
        let path = std::env::temp_dir().join(format!("corral-bom-{}.json", std::process::id()));
        fs::write(&path, "\u{feff}{\"version\":1,\"next_id\":1,\"groups\":[]}").unwrap();
        let loaded = load(&path);
        let _ = fs::remove_file(&path);
        assert!(loaded.is_ok(), "{:?}", loaded.err());
    }

    #[test]
    fn layouts_without_loose_field_still_load() {
        let layout: Layout = serde_json::from_str(r#"{"version":1,"next_id":1,"groups":[]}"#).unwrap();
        assert!(layout.loose.is_empty());
    }

    #[test]
    fn save_load_roundtrip_and_backup_recovery() {
        let dir = temp_dir("persist");
        let path = dir.join("layout.json");
        let mut layout = Layout::new();
        layout.add_group("Docs".into(), r"\\.\DISPLAY1".into(), Rect::new(10, 20, 300, 200), GroupContent::Portal { path: "C:\\x".into() });
        save(&path, &layout).unwrap();
        assert_eq!(load(&path).unwrap(), layout);
        let first = layout.clone();
        layout.groups[0].title = "Changed".into();
        save(&path, &layout).unwrap();
        assert_eq!(load(&backup_path(&path)).unwrap(), first);
        fs::write(&path, "{ corrupt").unwrap();
        assert_eq!(load_with_backup(&path).unwrap(), first);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_unknown_version() {
        let dir = temp_dir("version");
        let path = dir.join("layout.json");
        fs::write(&path, r#"{"version":99,"next_id":1,"groups":[]}"#).unwrap();
        assert!(matches!(load(&path), Err(LoadError::UnsupportedVersion(99))));
        fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod tab_input_tests {
    use super::*;
    use crate::tabs;

    fn fixture() -> Layout {
        let mut l = Layout::new();
        l.add_group("Files".into(), "m".into(), Rect::new(10, 20, 310, 220),
            GroupContent::Items { items: vec![r"C:\QA\alpha.txt".into()] });
        l.add_group("Portal".into(), "m".into(), Rect::new(100, 150, 400, 350),
            GroupContent::Portal { path: r"C:\QA\nonexistent-folder".into() });
        l.add_group("More files".into(), "m".into(), Rect::new(500, 550, 800, 750),
            GroupContent::Items { items: vec![r"C:\QA\beta.txt".into()] });
        l.groups[1].nav_history = vec![r"C:\QA\nonexistent-folder\child".into()];
        l.groups[1].scroll = 37;
        l.groups[1].style.color = Some(0xabc123);
        l.groups[2].sort_method = 3;
        l
    }

    fn selected(l: &Layout, member: u64) -> u64 {
        tabs::collect(&l.groups, member).unwrap().active
    }

    fn select(l: &mut Layout, member: u64) {
        let doc = tabs::select(&l.tab_doc(), member).unwrap();
        l.apply_tab_doc(doc);
    }

    #[test]
    fn join_preserves_private_content_and_uses_destination_common_state() {
        let mut l = fixture();
        let before = l.clone();
        l.groups[0].swipe_index = -2;
        l.groups[0].all_pages = true;
        l.groups[0].opacity = 173;
        l.join_tabs(1, 2).unwrap();
        assert_eq!(l.last_tab_set_id, 2);
        assert_eq!(selected(&l, 1), 2);
        assert_eq!(l.groups[1].rect, before.groups[0].rect);
        assert_eq!((l.groups[1].swipe_index, l.groups[1].all_pages, l.groups[1].opacity), (-2, true, 173));
        for i in 0..3 {
            assert_eq!(l.groups[i].content, before.groups[i].content);
            assert_eq!(l.groups[i].nav_history, before.groups[i].nav_history);
            assert_eq!(l.groups[i].scroll, before.groups[i].scroll);
            assert_eq!(l.groups[i].style.color, before.groups[i].style.color);
        }
    }

    #[test]
    fn failed_or_locked_join_does_not_consume_the_counter_or_mutate_records() {
        let mut l = fixture();
        let before = l.clone();
        assert!(l.join_tabs(1, 99).is_err());
        assert_eq!(l, before);
        assert!(l.join_tabs(1, 1).is_err());
        assert_eq!(l, before);
        l.desktop_locked = true;
        let before = l.clone();
        assert_eq!(l.join_tabs(1, 2), Err(tabs::TabError::Locked));
        assert_eq!(l, before);
    }

    #[test]
    fn shared_move_resize_roll_and_page_survive_switch_and_tear_off() {
        let mut l = fixture();
        l.join_tabs(1, 2).unwrap();
        l.join_tabs(2, 3).unwrap();
        let private = l.groups[1].clone();
        l.groups[2].rect = Rect::new(45, 65, 745, 765);
        l.groups[2].roll = RollState::Docked;
        l.groups[2].swipe_index = 4;
        l.sync_tab_common(3).unwrap();
        assert!(l.groups.iter().all(|g| g.rect == l.groups[2].rect && g.roll == RollState::Docked && g.swipe_index == 4));
        select(&mut l, 2);
        assert_eq!(selected(&l, 1), 2);
        let place = tabs::Placement { monitor: "m".into(), rect: Rect::new(120, 200, 820, 900) };
        let doc = tabs::remove_tab(&l.tab_doc(), 2, &place).unwrap();
        l.apply_tab_doc(doc);
        assert_eq!(selected(&l, 1), 3);
        assert_eq!(l.groups[1].rect, place.rect);
        assert_eq!(l.groups[1].roll, RollState::Expanded);
        assert_eq!(l.groups[1].content, private.content);
        assert_eq!(l.groups[1].nav_history, private.nav_history);
        assert_eq!(l.groups[1].scroll, private.scroll);
    }

    #[test]
    fn removal_releases_only_that_members_references_and_repairs_selection() {
        let mut l = fixture();
        l.groups[0].swipe_index = -3;
        l.join_tabs(1, 2).unwrap();
        l.join_tabs(2, 3).unwrap();
        select(&mut l, 1);
        l.remove_tab_record(1, 7).unwrap();
        assert_eq!(selected(&l, 2), 2);
        assert_eq!(l.group_of(r"C:\QA\alpha.txt"), None);
        assert_eq!(l.icon_page(r"C:\QA\alpha.txt"), -3);
        assert_eq!(l.group_of(r"C:\QA\beta.txt"), Some(3));
        assert_eq!(l.groups[0].nav_history, vec![r"C:\QA\nonexistent-folder\child"]);
        l.remove_tab_record(2, 7).unwrap();
        assert!(l.groups[0].tab.is_none());
        l.remove_tab_record(3, 7).unwrap();
        assert!(l.groups.is_empty());
        assert_eq!(l.icon_page(r"C:\QA\beta.txt"), -3);
    }

    #[test]
    fn all_pages_removal_uses_the_current_page_and_a_locked_desktop_changes_nothing() {
        let mut l = fixture();
        l.groups[0].all_pages = true;
        l.join_tabs(1, 2).unwrap();
        l.desktop_locked = true;
        let before = l.clone();
        assert_eq!(l.remove_tab_record(1, 5), Err(tabs::TabError::Locked));
        assert_eq!(l, before);
        l.desktop_locked = false;
        l.remove_tab_record(1, 5).unwrap();
        assert_eq!(l.icon_page(r"C:\QA\alpha.txt"), 5);
        assert!(l.groups.iter().all(|g| g.tab.is_none()), "the set dissolved");
    }

    #[test]
    fn layout_roundtrip_keeps_tab_flags_lock_and_membership() {
        let mut l = fixture();
        l.join_tabs(1, 2).unwrap();
        l.tab_quick_hide_exclusions.insert(1, true);
        l.tab_quick_hide_exclusions.insert(2, true);
        let place = tabs::Placement { monitor: "m".into(), rect: Rect::new(0, 0, 50, 50) };
        let doc = tabs::remove_tab(&l.tab_doc(), 2, &place).unwrap();
        l.apply_tab_doc(doc);
        l.desktop_locked = true;
        let back: Layout = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        l.groups.iter_mut().for_each(|g| g.scroll = 0); // scrollbar-spec 1: offsets are not persisted
        assert_eq!(back, l);
        let mut back = back;
        back.desktop_locked = false;
        back.remove_tab_record(2, 0).unwrap();
        assert!(!back.tab_quick_hide_exclusions.contains_key(&2));
        assert_eq!(back.tab_quick_hide_exclusions.get(&1), Some(&true));
    }

    #[test]
    fn the_clear_view_flag_is_written_to_every_tab_and_each_keeps_its_own_afterwards() {
        let mut l = fixture();
        l.join_tabs(1, 2).unwrap();
        l.set_tab_exclusion(2, true).unwrap();
        assert_eq!(l.tab_quick_hide_exclusions.get(&1), Some(&true));
        assert_eq!(l.tab_quick_hide_exclusions.get(&2), Some(&true));
        assert!(!l.tab_quick_hide_exclusions.contains_key(&3));
        let place = tabs::Placement { monitor: "m".into(), rect: Rect::new(0, 0, 50, 50) };
        let doc = tabs::remove_tab(&l.tab_doc(), 2, &place).unwrap();
        l.apply_tab_doc(doc);
        l.set_tab_exclusion(2, false).unwrap();
        assert_eq!(l.tab_quick_hide_exclusions.get(&1), Some(&true));
        assert_eq!(l.tab_quick_hide_exclusions.get(&2), Some(&false));
        let before = l.clone();
        assert!(l.set_tab_exclusion(99, true).is_err());
        assert_eq!(l, before);
    }
}

#[cfg(test)]
mod q33l_tests {
    use super::*;
    #[test]
    fn dock_latch_survives_fold_unfold_restart_and_tab_selection() {
        let mut l = Layout::new();
        let a=l.add_group("A".into(),"QA".into(),Rect::new(10,34,410,334),GroupContent::Items { items: vec![] });
        let b=l.add_group("B".into(),"QA".into(),Rect::new(10,34,410,334),GroupContent::Items { items: vec![] });
        l.join_tabs(a,b).unwrap();
        for edge in [crate::rollup::DockEdge::Top,crate::rollup::DockEdge::Bottom] {
            for roll in [RollState::Docked,RollState::Expanded,RollState::Manual] {
                let active=crate::tabs::collect(&l.groups,a).unwrap().active;
                let g=l.group_mut(active).unwrap();g.set_dock_edge(Some(edge));g.roll=roll;
                l.sync_tab_common(active).unwrap();
                assert!(l.groups.iter().all(|g| g.dock_edge()==Some(edge) && g.roll==roll));
                let restored:Layout=serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
                assert_eq!(restored,l);
                let selected=if active==a { b } else { a };
                let doc=crate::tabs::select(&l.tab_doc(),selected).unwrap();
                l.apply_tab_doc(doc);
                assert!(l.groups.iter().all(|g| g.dock_edge()==Some(edge)));
            }
        }
        let doc=crate::tabs::select(&l.tab_doc(),a).unwrap();l.apply_tab_doc(doc);
        l.group_mut(a).unwrap().set_dock_edge(None);l.group_mut(a).unwrap().roll=RollState::Expanded;
        l.sync_tab_common(a).unwrap();
        assert!(l.groups.iter().all(|g| g.dock_edge().is_none() && g.roll==RollState::Expanded));
    }
    #[test]
    fn old_layout_defaults_undocked_and_saved_setup_placements_round_trip_dock_flags() {
        let mut l=Layout::new();
        l.add_group("A".into(),"QA".into(),Rect::new(10,34,410,334),GroupContent::Items { items: vec![] });
        let old=serde_json::to_string(&l).unwrap();assert!(!old.contains("dock_top"));assert!(!old.contains("dock_bottom"));
        let old:Layout=serde_json::from_str(&old).unwrap();assert!(old.groups[0].dock_edge().is_none());
        let p:crate::desk_setup::SetupPlacement=serde_json::from_str(r#"{"monitor":0,"rect":{"left":10,"top":34,"right":410,"bottom":334}}"#).unwrap();
        assert!(!p.dock_top && !p.dock_bottom);
        for edge in [crate::rollup::DockEdge::Top,crate::rollup::DockEdge::Bottom] {
            l.groups[0].set_dock_edge(Some(edge));
            let monitors=[crate::desk_setup::MonitorState{gdi_name:"QA".into(),size_px:(1920,1080),scale_pct:100,..Default::default()}];
            crate::desk_setup::evaluate(&mut l,&monitors,1);
            l.desk_setups.setups.clear();l.desk_setups.runtime.baseline=None;
            assert!(crate::desk_setup::user_edit(&mut l,2,false));
            let restored:Layout=serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
            assert_eq!(restored.desk_setups.setups[0].groups,l.desk_setups.setups[0].groups);
            let p=&restored.desk_setups.setups[0].groups[&1];
            assert_eq!((p.dock_top,p.dock_bottom),(l.groups[0].dock_top,l.groups[0].dock_bottom));
            l.desk_setups.setups.clear();
        }
    }
}

#[cfg(test)]
mod camouflage_storage_tests {
    use super::*;

    #[test]
    fn per_corral_sources_migrate_independently_and_missing_source_inherits() {
        let mut inherited: GroupStyle = serde_json::from_str("{}").unwrap();
        inherited.migrate_tint_source(0x112233, 0x998877);
        assert_eq!(inherited.tint_icons_color_source, None);
        let mut same: GroupStyle = serde_json::from_value(serde_json::json!({
            "color":0x223344, "tint_icons_color":0x233245,
            "tint_icons_color_use":0xff233245u32})).unwrap();
        same.migrate_tint_source(0x111111, 0x555555);
        assert_eq!(same.tint_icons_color_source.as_deref(), Some("corral"));
        let mut different = same.clone();
        different.tint_icons_color_source = None;
        different.tint_icons_color = Some(0x998877);
        different.tint_icons_color_use = Some(0xff998877);
        different.migrate_tint_source(0, 0);
        assert_eq!(different.tint_icons_color_source.as_deref(), Some("custom"));
        // The actual shared background is used when this Corral has no own one.
        let mut shared: GroupStyle = serde_json::from_value(serde_json::json!({
            "tint_icons_color":0x112233, "tint_icons_color_use":0xff112233u32})).unwrap();
        shared.migrate_tint_source(0x112233, 0x998877);
        assert_eq!(shared.tint_icons_color_source.as_deref(), Some("corral"));
        for style in [&same, &different, &shared] {
            let saved = serde_json::to_value(style).unwrap();
            assert!(saved.get("tint_icons_color_use").is_none());
            let restored: GroupStyle = serde_json::from_value(saved).unwrap();
            assert_eq!(restored.tint_icons_color_source, style.tint_icons_color_source);
        }
    }

    #[test]
    fn explicit_per_corral_source_wins_over_legacy() {
        for source in ["corral", "custom"] {
            let mut style: GroupStyle = serde_json::from_value(serde_json::json!({
                "color":0, "tint_icons_color":0xffffff, "tint_icons_color_use":7,
                "tint_icons_color_source":source})).unwrap();
            style.migrate_tint_source(0, 0);
            assert_eq!(style.tint_icons_color_source.as_deref(), Some(source));
        }
    }
}
