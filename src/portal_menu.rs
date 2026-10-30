//! The Folder Gate menu (folder-gate-host-spec 5) and the hiding of the installed product's Shell
//! entry (folder-gate-host-spec 6). Both are pure: the menu is a function of the Gate's state, the
//! Shell entry filter works on a model of a real `HMENU` read by `shell_menu`.

/// Gate family command ids (folder-gate-host-spec 5): internal numbers with no relation to
/// Windows' ids.
pub mod id {
    pub const FIRST: u32 = 0x6101;
    pub const OPEN_FOLDER: u32 = 0x6101;
    pub const CHOOSE_FOLDER: u32 = 0x6102;
    pub const CONFIGURE_FOLDER: u32 = 0x6103;
    pub const REFRESH: u32 = 0x6104;
    pub const NAVIGATE_UP: u32 = 0x6105;
    pub const NAVIGATE_ROOT: u32 = 0x6106;
    pub const ALLOW_NAVIGATION: u32 = 0x6107;
    #[cfg(test)]
    pub const EXTRA_LARGE: u32 = 0x6110;
    #[cfg(test)]
    pub const LARGE: u32 = 0x6111;
    #[cfg(test)]
    pub const MEDIUM: u32 = 0x6112;
    pub const CLASSIC: u32 = 0x6113;
    pub const LIST: u32 = 0x6114;
    pub const DETAILS: u32 = 0x6115;
    #[cfg(test)]
    pub const HIDE_HEADER: u32 = 0x6116;
    pub const HIDE_ARROWS: u32 = 0x6117;
    pub const SORT_NAME: u32 = 0x6120;
    pub const SORT_SIZE: u32 = 0x6121;
    pub const SORT_TYPE: u32 = 0x6122;
    pub const SORT_DATE: u32 = 0x6123;
    pub const REVERSE: u32 = 0x6124;
    pub const COLUMN_SIZE: u32 = 0x6130;
    pub const COLUMN_TYPE: u32 = 0x6131;
    pub const COLUMN_DATE: u32 = 0x6132;
    pub const COLUMN_RESET: u32 = 0x6133;
    pub const LAST: u32 = COLUMN_RESET;

    /// Whether `command` belongs to the Gate family.
    pub fn is_gate(command: u32) -> bool {
        (FIRST..=LAST).contains(&command)
    }
}

/// Windows' view modes the Gate uses (`FOLDERVIEWMODE`).
#[cfg(test)]
pub const MODE_ICON: i32 = 1;
#[cfg(test)]
pub const MODE_LIST: i32 = 3;
#[cfg(test)]
pub const MODE_DETAILS: i32 = 4;
/// Thumbnail mode: the shell reports it for the larger icon sizes.
#[cfg(test)]
pub const MODE_THUMBNAIL: i32 = 5;

/// Stored sort columns of folder-gate-views-spec 2, in menu order.
pub const SORTS: [(u32, &str, i32); 4] = [
    (id::SORT_NAME, "Name", 1),
    (id::SORT_SIZE, "Size", 2),
    (id::SORT_TYPE, "Item type", 3),
    (id::SORT_DATE, "Date modified", 4),
];

/// What a Gate shows in its menu (folder-gate-host-spec 5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GateMenuState {
    /// Allow navigation.
    pub allow: bool,
    /// The Gate shows its root.
    pub at_root: bool,
    /// The resolved view mode (`FOLDERVIEWMODE`), 0 when unknown.
    pub mode: i32,
    /// The effective icon size in pixels of the icon view.
    pub icon_px: i32,
    /// The folder flag "header hidden".
    pub header_hidden: bool,
    /// The shortcut-arrow flag of the settings dialog.
    pub hide_arrows: bool,
    /// Signed 1-based sort column (negative = descending), 0 or 5 when not one of the four stored.
    pub sort: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Command,
    Check,
    Radio,
    Separator,
    Submenu(Vec<Item>),
}

/// One menu item: ids are the Gate family's, labels are English and never name another product.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub id: u32,
    pub label: &'static str,
    pub enabled: bool,
    pub checked: bool,
    pub hidden: bool,
    pub kind: Kind,
}

impl Item {
    fn command(id: u32, label: &'static str) -> Item {
        Item { id, label, enabled: true, checked: false, hidden: false, kind: Kind::Command }
    }
    fn check(id: u32, label: &'static str, checked: bool) -> Item {
        Item { id, label, enabled: true, checked, hidden: false, kind: Kind::Check }
    }
    fn radio(id: u32, label: &'static str, checked: bool) -> Item {
        Item { id, label, enabled: true, checked, hidden: false, kind: Kind::Radio }
    }
    fn separator() -> Item {
        Item { id: 0, label: "", enabled: true, checked: false, hidden: false, kind: Kind::Separator }
    }
    fn submenu(label: &'static str, items: Vec<Item>) -> Item {
        Item { id: 0, label, enabled: true, checked: false, hidden: false, kind: Kind::Submenu(items) }
    }
    fn disabled(mut self, disabled: bool) -> Item {
        self.enabled = !disabled;
        self
    }
    fn hidden(mut self, hidden: bool) -> Item {
        self.hidden = hidden;
        self
    }
}

/// The Gate menu above the shared Corral section (folder-gate-host-spec 5).
#[cfg(test)]
pub fn gate_menu(s: &GateMenuState) -> Vec<Item> {
    gate_menu_columns(s, &crate::view_state::DetailsColumns::default())
}
pub fn column_menu(columns: &crate::view_state::DetailsColumns) -> Vec<Item> {
    use crate::view_state::ColumnKey;
    vec![Item::check(id::COLUMN_SIZE, "Size", columns.contains(ColumnKey::Size)),
        Item::check(id::COLUMN_TYPE, "Type", columns.contains(ColumnKey::Type)),
        Item::check(id::COLUMN_DATE, "Date modified", columns.contains(ColumnKey::DateModified)),
        Item::separator(), Item::command(id::COLUMN_RESET, "Reset columns")]
}
pub fn gate_menu_columns(s: &GateMenuState, columns: &crate::view_state::DetailsColumns) -> Vec<Item> {
    let mode = crate::view_state::View::from_mode(s.mode);
    let mut view = vec![
        Item::radio(id::CLASSIC, "Icons", mode == crate::view_state::View::Icons),
        Item::radio(id::LIST, "List", mode == crate::view_state::View::List),
        Item::radio(id::DETAILS, "Details", mode == crate::view_state::View::Details),
        Item::separator(),
        Item::check(id::HIDE_ARROWS, "Hide shortcut arrows", s.hide_arrows),
    ];
    if mode == crate::view_state::View::Details { view.insert(3, Item::separator()); view.insert(4, Item::submenu("Columns", column_menu(columns))); }
    let stored = (1..=4).contains(&s.sort.abs());
    let mut sort: Vec<Item> = SORTS.iter().map(|&(id, label, column)| Item::radio(id, label, s.sort.abs() == column)).collect();
    sort.push(Item::separator());
    sort.push(Item::check(id::REVERSE, "Reverse order", stored && s.sort < 0).disabled(!stored));
    vec![
        Item::command(id::OPEN_FOLDER, "Open folder"),
        Item::command(id::CHOOSE_FOLDER, "Choose a different folder..."),
        Item::command(id::CONFIGURE_FOLDER, "Configure folder"),
        Item::separator(),
        Item::command(id::REFRESH, "Refresh"),
        Item::command(id::NAVIGATE_UP, "Navigate up").hidden(!s.allow).disabled(s.at_root),
        Item::command(id::NAVIGATE_ROOT, "Navigate to root").hidden(!s.allow).disabled(s.at_root),
        Item::check(id::ALLOW_NAVIGATION, "Allow navigation", s.allow),
        Item::separator(),
        Item::submenu("View", view),
        Item::submenu("Sort by", sort),
    ]
}

/// Stored sort column (1..=4) of a Sort by command.
pub fn sort_column(command: u32) -> Option<i32> {
    SORTS.iter().find(|(id, _, _)| *id == command).map(|&(_, _, column)| column)
}

/// Folder-background entries allowed after the Gate menu (corral-views-spec 2.3), by canonical verb.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundEntry { New, Properties }

pub fn gate_background_entry(verb: Option<&str>, submenu: bool) -> Option<BackgroundEntry> {
    if submenu && verb.is_some_and(|v| v.eq_ignore_ascii_case("new")) {
        Some(BackgroundEntry::New)
    } else if !submenu && verb.is_some_and(|v| v.eq_ignore_ascii_case("properties")) {
        Some(BackgroundEntry::Properties)
    } else {
        None
    }
}

// ---- 6 hiding the installed product's Shell entry ------------------------------------------------

/// One entry of a Shell menu as the filter sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellItem {
    pub id: u32,
    pub text: String,
    pub separator: bool,
    pub submenu: Option<Vec<ShellItem>>,
}

impl ShellItem {
    pub fn text(id: u32, text: &str) -> ShellItem {
        ShellItem { id, text: text.to_string(), separator: false, submenu: None }
    }
    pub fn separator() -> ShellItem {
        ShellItem { id: 0, text: String::new(), separator: true, submenu: None }
    }
    #[cfg(test)]
    pub fn submenu(text: &str, items: Vec<ShellItem>) -> ShellItem {
        ShellItem { id: 0, text: text.to_string(), separator: false, submenu: Some(items) }
    }
}

/// Labels Corral removes from its Shell menus (`HiddenShellMenuEntries`, folder-gate-host-spec 6),
/// normalised. Set once at start-up; empty until then.
static HIDDEN_ENTRIES: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Store the `HiddenShellMenuEntries` setting for [`hidden_entries`]. The first call wins.
pub fn set_hidden_entries(labels: &[String]) {
    let _ = HIDDEN_ENTRIES.set(labels.iter().map(|l| normalize_label(l)).filter(|l| !l.is_empty()).collect());
}

/// The normalised labels to hide (empty when none are configured).
pub fn hidden_entries() -> &'static [String] {
    HIDDEN_ENTRIES.get().map(Vec::as_slice).unwrap_or(&[])
}

/// Normal form of a Shell menu label: accelerator marks removed, trimmed, one trailing `...` or
/// U+2026 removed, case-folded, runs of spaces collapsed.
pub fn normalize_label(text: &str) -> String {
    let no_marks: String = text.chars().filter(|&c| c != '&').collect();
    let trimmed = no_marks.trim();
    let without_dots = trimmed.strip_suffix("...").or_else(|| trimmed.strip_suffix('\u{2026}')).unwrap_or(trimmed);
    without_dots.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// Whether a Shell menu label is one of the normalised `hidden` labels. The full string is
/// compared, so with "Configure Widgets" hidden, "Configure Widgets Pro" or "Configure folder"
/// are untouched.
pub fn is_hidden_entry(text: &str, hidden: &[String]) -> bool {
    let label = normalize_label(text);
    !label.is_empty() && hidden.iter().any(|h| *h == label)
}

/// Positions to remove from one menu level, highest first: the matching items, then a separator
/// the removal left leading, trailing or doubled - and only then. Submenus are not looked into
/// here (the caller walks them).
pub fn removal_plan(items: &[ShellItem], hidden: &[String]) -> Vec<usize> {
    let matches: Vec<bool> = items.iter().map(|i| !i.separator && i.submenu.is_none() && is_hidden_entry(&i.text, hidden)).collect();
    if !matches.iter().any(|m| *m) {
        return Vec::new();
    }
    // A separator is "affected" when a removed item sat right next to it.
    let affected = |i: usize| (i > 0 && matches[i - 1]) || (i + 1 < items.len() && matches[i + 1]);
    let mut kept: Vec<(usize, bool)> = items.iter().enumerate().filter(|(i, _)| !matches[*i]).map(|(i, it)| (i, it.separator)).collect();
    let mut remove: Vec<usize> = matches.iter().enumerate().filter(|(_, m)| **m).map(|(i, _)| i).collect();
    // Leading and trailing separators the removal exposed.
    while let Some(&(i, true)) = kept.first() {
        if !affected(i) {
            break;
        }
        remove.push(i);
        kept.remove(0);
    }
    while let Some(&(i, true)) = kept.last() {
        if !affected(i) {
            break;
        }
        remove.push(i);
        kept.pop();
    }
    // Separators the removal made adjacent.
    let mut k = 1;
    while k < kept.len() {
        let ((a, a_sep), (b, b_sep)) = (kept[k - 1], kept[k]);
        let adjacent_before = b == a + 1;
        if a_sep && b_sep && !adjacent_before && (affected(a) || affected(b)) {
            remove.push(b);
            kept.remove(k);
        } else {
            k += 1;
        }
    }
    remove.sort_unstable_by(|a, b| b.cmp(a));
    remove
}

/// Apply [`removal_plan`] to `items` and to every submenu, returning the ids that were removed (the
/// caller never sends them). Idempotent; submenus are filtered level by level.
#[cfg(test)]
pub fn filter_shell_menu(items: &mut Vec<ShellItem>, hidden: &[String]) -> Vec<u32> {
    let mut removed = Vec::new();
    for position in removal_plan(items, hidden) {
        let item = items.remove(position);
        if !item.separator {
            removed.push(item.id);
        }
    }
    for item in items.iter_mut() {
        if let Some(sub) = item.submenu.as_mut() {
            removed.extend(filter_shell_menu(sub, hidden));
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_background_keeps_only_canonical_new_and_properties() {
        assert_eq!(gate_background_entry(Some("New"), true), Some(BackgroundEntry::New));
        assert_eq!(gate_background_entry(Some("NEW"), true), Some(BackgroundEntry::New));
        assert_eq!(gate_background_entry(Some("PrOpErTiEs"), false), Some(BackgroundEntry::Properties));
        for verb in [None, Some("view"), Some("sort"), Some("groupby"), Some("refresh"), Some("paste"), Some("pastelink"), Some("git_gui"), Some("Windows.Share"), Some("GameLibrary"), Some("newish")] {
            assert_eq!(gate_background_entry(verb, false), None, "{verb:?}");
            assert_eq!(gate_background_entry(verb, true), None, "{verb:?}");
        }
        assert_eq!(gate_background_entry(Some("new"), false), None);
        assert_eq!(gate_background_entry(Some("properties"), true), None);
    }

    fn flatten<'a>(items: &'a [Item], out: &mut Vec<&'a Item>) {
        for item in items {
            out.push(item);
            if let Kind::Submenu(sub) = &item.kind {
                flatten(sub, out);
            }
        }
    }

    fn all(state: &GateMenuState) -> Vec<Item> {
        let menu = gate_menu(state);
        let mut flat = Vec::new();
        flatten(&menu, &mut flat);
        flat.into_iter().cloned().collect()
    }

    fn find(state: &GateMenuState, command: u32) -> Item {
        all(state).into_iter().find(|i| i.id == command).unwrap_or_else(|| panic!("{command:#x} missing"))
    }

    fn state() -> GateMenuState {
        GateMenuState { allow: true, at_root: false, mode: MODE_ICON, icon_px: 48, header_hidden: false, hide_arrows: false, sort: 1 }
    }

    // ---- test 7 --------------------------------------------------------------------------------

    #[test]
    fn the_item_set_and_labels_follow_the_spec_table() {
        let menu = gate_menu(&state());
        let top: Vec<(u32, &str)> = menu.iter().map(|i| (i.id, i.label)).collect();
        assert_eq!(top, [
            (0x6101, "Open folder"),
            (0x6102, "Choose a different folder..."),
            (0x6103, "Configure folder"),
            (0, ""),
            (0x6104, "Refresh"),
            (0x6105, "Navigate up"),
            (0x6106, "Navigate to root"),
            (0x6107, "Allow navigation"),
            (0, ""),
            (0, "View"),
            (0, "Sort by"),
        ]);
        let Kind::Submenu(view) = &menu[9].kind else { panic!() };
        let view: Vec<(u32, &str)> = view.iter().map(|i| (i.id, i.label)).collect();
        assert_eq!(view, [(0x6113, "Icons"), (0x6114, "List"), (0x6115, "Details"), (0, ""), (0x6117, "Hide shortcut arrows")]);
        let Kind::Submenu(sort) = &menu[10].kind else { panic!() };
        let sort: Vec<(u32, &str)> = sort.iter().map(|i| (i.id, i.label)).collect();
        assert_eq!(sort, [(0x6120, "Name"), (0x6121, "Size"), (0x6122, "Item type"), (0x6123, "Date modified"), (0, ""), (0x6124, "Reverse order")]);
    }

    #[test]
    fn ids_are_unique_inside_the_gate_range_and_no_label_names_a_product() {
        let flat = all(&state());
        let mut ids: Vec<u32> = flat.iter().filter(|i| i.id != 0).map(|i| i.id).collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "unique");
        assert_eq!(count, 16);
        for id in &ids {
            assert!((id::FIRST..=id::LAST).contains(id), "{id:#x}");
            assert!(id::is_gate(*id));
        }
        for item in &flat {
            let label = item.label.to_lowercase();
            for name in ["nimi", "explorer", "corral"] {
                assert!(!label.contains(name), "{:?} contains {name}", item.label);
            }
        }
    }

    #[test]
    fn columns_are_offered_only_in_details_and_match_header_menu() {
        let mut columns = crate::view_state::DetailsColumns::default();
        columns.toggle(crate::view_state::ColumnKey::Type);
        for mode in [1,3,4,8] {
            let menu = gate_menu_columns(&GateMenuState { mode, ..state() }, &columns);
            let Kind::Submenu(view) = &menu[9].kind else { panic!() };
            let submenu = view.iter().find(|i| i.label == "Columns");
            if matches!(mode,4 | 8) {
                let Kind::Submenu(items) = &submenu.unwrap().kind else { panic!() };
                assert_eq!(*items, column_menu(&columns));
                assert!(items[0].checked && items[1].checked && !items[2].checked);
                assert_eq!(items[3].kind,Kind::Separator);
                assert_eq!(items[4].label,"Reset columns");
            } else { assert!(submenu.is_none()); }
        }
    }

    #[test]
    fn navigation_entries_follow_allow_and_the_root() {
        let on = state();
        assert!(!find(&on, id::NAVIGATE_UP).hidden && find(&on, id::NAVIGATE_UP).enabled);
        assert!(!find(&on, id::NAVIGATE_ROOT).hidden && find(&on, id::NAVIGATE_ROOT).enabled);
        assert!(find(&on, id::ALLOW_NAVIGATION).checked);
        let at_root = GateMenuState { at_root: true, ..on };
        assert!(!find(&at_root, id::NAVIGATE_UP).hidden && !find(&at_root, id::NAVIGATE_UP).enabled, "disabled at the root");
        assert!(!find(&at_root, id::NAVIGATE_ROOT).enabled);
        let off = GateMenuState { allow: false, ..on };
        assert!(find(&off, id::NAVIGATE_UP).hidden && find(&off, id::NAVIGATE_ROOT).hidden, "hidden with Allow navigation off");
        assert!(!find(&off, id::ALLOW_NAVIGATION).checked);
        assert!(!find(&off, id::ALLOW_NAVIGATION).hidden);
        for command in [id::OPEN_FOLDER, id::CHOOSE_FOLDER, id::CONFIGURE_FOLDER, id::REFRESH] {
            let item = find(&off, command);
            assert!(item.enabled && !item.hidden);
        }
    }

    #[test]
    fn old_windows_sizes_and_header_are_absent_and_arrows_follow_the_flag() {
        for mode in [MODE_ICON, MODE_LIST, MODE_DETAILS, MODE_THUMBNAIL, 0] {
            let flat = all(&GateMenuState { mode, ..state() });
            for gone in [id::EXTRA_LARGE, id::LARGE, id::MEDIUM, id::HIDE_HEADER] {
                assert!(flat.iter().all(|i| i.id != gone), "{gone:#x} in mode {mode}");
            }
        }
        assert!(find(&GateMenuState { hide_arrows: true, ..state() }, id::HIDE_ARROWS).checked);
        assert!(!find(&state(), id::HIDE_ARROWS).checked);
    }

    #[test]
    fn drawn_view_radio_maps_all_three_legacy_modes() {
        for mode in [0, 1, 2, 3, 4, 5, 6, 8, 99] {
            let s = GateMenuState { mode, ..state() };
            let icons = find(&s, id::CLASSIC);
            let list = find(&s, id::LIST);
            assert_eq!(icons.kind, Kind::Radio);
            assert_eq!(list.kind, Kind::Radio);
            let details = find(&s,id::DETAILS);
            assert_eq!(details.kind,Kind::Radio);
            assert_eq!([icons.checked,list.checked,details.checked].into_iter().filter(|v| *v).count(),1);
            assert_eq!(list.checked, matches!(mode,2 | 3));
            assert_eq!(details.checked, matches!(mode,4 | 8));
        }
    }

    #[test]
    fn sort_radios_and_reverse_follow_the_stored_sort() {
        let checked = |sort: i32| -> Vec<u32> {
            let s = GateMenuState { sort, ..state() };
            [id::SORT_NAME, id::SORT_SIZE, id::SORT_TYPE, id::SORT_DATE].into_iter().filter(|c| find(&s, *c).checked).collect()
        };
        assert_eq!(checked(1), [id::SORT_NAME]);
        assert_eq!(checked(-2), [id::SORT_SIZE]);
        assert_eq!(checked(3), [id::SORT_TYPE]);
        assert_eq!(checked(-4), [id::SORT_DATE]);
        assert_eq!(checked(5), Vec::<u32>::new(), "Date created checks none");
        assert_eq!(checked(-5), Vec::<u32>::new());
        assert_eq!(checked(0), Vec::<u32>::new());
        let reverse = |sort: i32| find(&GateMenuState { sort, ..state() }, id::REVERSE);
        assert!(!reverse(1).checked && reverse(1).enabled);
        assert!(reverse(-1).checked && reverse(-3).checked);
        assert!(!reverse(5).enabled && !reverse(-5).checked && !reverse(0).enabled, "a non-stored sort cannot be reversed");
    }

    #[test]
    fn commands_map_to_sort_columns() {
        assert_eq!(sort_column(id::SORT_NAME), Some(1));
        assert_eq!(sort_column(id::SORT_DATE), Some(4));
        assert_eq!(sort_column(id::REVERSE), None);
    }

    // ---- test 8 --------------------------------------------------------------------------------

    fn hidden() -> Vec<String> {
        vec![normalize_label("Configure Widgets")]
    }

    fn labels(items: &[ShellItem]) -> Vec<String> {
        items.iter().map(|i| if i.separator { "-".to_string() } else { i.text.clone() }).collect()
    }

    #[test]
    fn the_entry_is_recognised_by_its_normalised_label() {
        for label in ["Configure Widgets", "&Configure Widgets...", "CONFIGURE  WIDGETS", "configure widgets\u{2026}", "  Configure &Widgets  ", "Configure\tWidgets"] {
            assert!(is_hidden_entry(label, &hidden()), "{label:?}");
        }
        for label in ["Configure Widgets Pro", "Configure folder", "Properties", "Widgets", "Configure", "Configure Widgets backup", "Reconfigure Widgets", "Configure Widgets ..x", ""] {
            assert!(!is_hidden_entry(label, &hidden()), "{label:?}");
        }
        assert!(!is_hidden_entry("Configure Widgets", &[]), "nothing is hidden by default");
        assert!(!is_hidden_entry("", &[String::new()]));
    }

    #[test]
    fn filtering_removes_the_entry_and_leaves_the_others() {
        let mut menu = vec![
            ShellItem::text(10, "Open"),
            ShellItem::text(11, "Configure Widgets"),
            ShellItem::text(12, "Configure Widgets Pro"),
            ShellItem::text(13, "Configure folder"),
            ShellItem::text(14, "Properties"),
        ];
        let removed = filter_shell_menu(&mut menu, &hidden());
        assert_eq!(removed, [11]);
        assert_eq!(labels(&menu), ["Open", "Configure Widgets Pro", "Configure folder", "Properties"]);
        for label in ["&Configure Widgets...", "CONFIGURE  WIDGETS", "Configure Widgets\u{2026}"] {
            let mut menu = vec![ShellItem::text(1, "Open"), ShellItem::text(2, label), ShellItem::text(3, "Paste")];
            assert_eq!(filter_shell_menu(&mut menu, &hidden()), [2], "{label:?}");
            assert_eq!(labels(&menu), ["Open", "Paste"]);
        }
    }

    #[test]
    fn separators_are_fixed_only_around_a_removal() {
        // leading
        let mut m = vec![ShellItem::text(1, "Configure Widgets"), ShellItem::separator(), ShellItem::text(2, "Open")];
        filter_shell_menu(&mut m, &hidden());
        assert_eq!(labels(&m), ["Open"]);
        // trailing
        let mut m = vec![ShellItem::text(2, "Open"), ShellItem::separator(), ShellItem::text(1, "Configure Widgets")];
        filter_shell_menu(&mut m, &hidden());
        assert_eq!(labels(&m), ["Open"]);
        // doubled
        let mut m = vec![ShellItem::text(2, "Open"), ShellItem::separator(), ShellItem::text(1, "Configure Widgets"), ShellItem::separator(), ShellItem::text(3, "Paste")];
        filter_shell_menu(&mut m, &hidden());
        assert_eq!(labels(&m), ["Open", "-", "Paste"]);
        // a separator between two kept items next to the removal stays
        let mut m = vec![ShellItem::text(2, "Open"), ShellItem::text(1, "Configure Widgets"), ShellItem::separator(), ShellItem::text(3, "Paste")];
        filter_shell_menu(&mut m, &hidden());
        assert_eq!(labels(&m), ["Open", "-", "Paste"]);
        // a menu without the entry is never touched, even with odd separators
        let mut m = vec![ShellItem::separator(), ShellItem::separator(), ShellItem::text(2, "Open"), ShellItem::separator()];
        let before = m.clone();
        assert!(filter_shell_menu(&mut m, &hidden()).is_empty());
        assert_eq!(m, before);
        // pre-existing doubles away from the removal stay
        let mut m = vec![ShellItem::text(2, "Open"), ShellItem::separator(), ShellItem::separator(), ShellItem::text(3, "Paste"), ShellItem::text(1, "Configure Widgets")];
        filter_shell_menu(&mut m, &hidden());
        assert_eq!(labels(&m), ["Open", "-", "-", "Paste"]);
    }

    #[test]
    fn filtering_is_idempotent_and_walks_submenus() {
        let mut menu = vec![
            ShellItem::text(5, "Open"),
            ShellItem::submenu("Show more", vec![ShellItem::separator(), ShellItem::text(20, "&Configure Widgets..."), ShellItem::text(21, "Share")]),
            ShellItem::text(6, "Configure Widgets"),
        ];
        let removed = filter_shell_menu(&mut menu, &hidden());
        assert_eq!(removed, [6, 20]);
        let sub = menu[1].submenu.as_ref().unwrap();
        assert_eq!(labels(sub), ["Share"]);
        let once = menu.clone();
        assert!(filter_shell_menu(&mut menu, &hidden()).is_empty());
        assert_eq!(menu, once);
    }

    #[test]
    fn the_plan_lists_positions_highest_first_for_the_live_walker() {
        let items = vec![ShellItem::text(2, "Open"), ShellItem::separator(), ShellItem::text(1, "Configure Widgets"), ShellItem::separator(), ShellItem::text(3, "Paste")];
        assert_eq!(removal_plan(&items, &hidden()), [3, 2]);
        assert!(removal_plan(&[ShellItem::text(2, "Open")], &hidden()).is_empty());
        assert!(removal_plan(&[], &hidden()).is_empty());
    }
}
