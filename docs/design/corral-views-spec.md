# Corral views and drawn Gates: design spec

Status: approved by the user 2026-10-28 (open questions answered in section 9)
Date: 2026-10-28
Author: Claude Code lead. Inputs: the user's request and two reference screenshots of another desktop organizer (2026-10-28,
visual reference only, no code read), current Corral code, approved Corral design docs, public Microsoft docs.

Terms: "Corral" = an ordinary Corral (items it owns). "Gate" = a Folder Gate (shows a folder). "Body" = the area under the
title strip. DIP = 1/96 inch, `px(d) = floor(d * dpi / 96 + 0.5)` at the Corral's monitor DPI. "Label style" = the font,
colour, Auto text and shadow a Corral uses for icon labels (corral-painting-spec 4.4, colour-state-spec).

## 1. Goal and non-goals

Goal: a Gate looks and behaves exactly like a Corral; the only visible difference is the folder glyph in its title (and the
Up button while it shows a subfolder). Every Corral and every Gate can show its items as **Icons**, **List** or **Details**,
drawn by Corral itself.

Why: a Gate today hosts Windows' own folder view (`IExplorerBrowser`). Its inset frame, label wrapping, blue highlights,
scrollbars, header and sorted-column shading are Windows', and each one has needed a patch to look like a Corral
(2026-10-28: dark scrollbars, overlay scrollbar, sorted-column shade). Drawing the folder ourselves removes the cause.

Non-goals:
- No horizontal scrolling in any view (scrollbar-spec 1 stays true; the 2026-10-28 Gate amendment becomes moot).
- No Windows views beyond the three: Tiles, Content and the extra icon sizes are dropped for Gates (Icons follows the
  Corral's icon size and Ctrl+wheel zoom, as Corrals do today).
- No grouping, filtering, search box, preview pane or column other than the four in 4.3.
- No change to how a Corral owns items, to Round-up rules, Ranges, Tabs, Fold, docking, Camouflage or blur.
- No file is touched by a view change. A view is only a record about how to display.

## 2. Gates without the Windows view

### 2.1 What replaces the host
The Gate host window, `IExplorerBrowser`, its event sink, list-view subclasses, baked backdrop and pitch/DPI plumbing
(`explorer_browser.rs`, `portal_drop.rs` wrapper, folder-gate-host-spec 2.1-2.4 and 7) are removed. A Gate is one Corral
window that paints its body with the same paint path as a Corral (corral-painting-spec 3.1), and its items with the same item
painter, in the view of section 3. The scrollbar is the Corral overlay (scrollbar-spec, ordinary internal mode).

Kept from folder-gate-host-spec: navigation and history (`GateNav`, 4.1-4.4), Allow navigation, the error bodies and probe
(3.3), the Gate menu (5), hiding the installed product's Shell menu entry (6), "This folder is empty." (3.2, now drawn in
the label style, centred).

### 2.2 Items
- Enumeration: `IShellFolder::EnumObjects` of the current folder (`SHCONTF_FOLDERS | SHCONTF_NONFOLDERS`, plus
  `SHCONTF_INCLUDEHIDDEN` only when Explorer's "Show hidden files" is on), run on a worker after the probe succeeds. The
  existing `shell::enumerate_folder` already does this for the preview fixture. Results reach the UI thread as one batch.
  Until the first batch arrives the body shows "Opening folder..." after 1000 ms (host spec 3.1 timing).
- Each item is the same `ShellItem` a Corral holds (parsing name, display name, attributes), so open, context menu, rename,
  delete, cut/copy/paste and drag use the existing Corral code paths unchanged (`open_items`, `show_item_menu`,
  `begin_rename`).
- Changes: `SHChangeNotifyRegister` on the current folder (not recursive; create, delete, rename, update, attributes,
  folder events), as Corral already does for the desktop. Notifications are coalesced for 100 ms, then the folder is
  re-enumerated on the worker and the result diffed by parsing name: kept items keep their selection and position; new
  items appear in sort order; a renamed item keeps its selection. Navigating unregisters the old folder first.
- Large folders: only rows/cells in or near the visible area are laid out with text and icons; icons load lazily (visible
  first) on the existing icon worker; a placeholder icon shows until the real one arrives. Target: a 5000-item folder opens
  without blocking input for more than 50 ms on the UI thread.
- Order: a Gate is always sorted (Explorer keeps no manual order in a folder): by its sort (4.4); default Name ascending,
  folders first (motion-sorting-spec 2).

### 2.3 Input inside a Gate
Everything a Corral does with its items, plus:
- Double-click (or Enter) on a folder: navigates inside the Gate when Allow navigation is on and the folder is inside the
  root; otherwise opens it in Explorer (host spec 4.3 decision unchanged). Files open as Windows does.
- Double-click on empty body: Open folder (host spec 2.5, unchanged).
- Back/Forward/Up keys and mouse side buttons as host spec 2.5. Backspace = Back.
- Drop onto the Gate (empty space): the folder's own drop target (`IShellFolder::GetUIObjectOf` / `CreateViewObject` with
  `IID_IDropTarget`), so move/copy/link and the Shift/Ctrl/Alt rules are Windows'. Drop onto a folder item inside the Gate:
  that folder's drop target. A drop of items that already live in the shown folder onto empty space does nothing (no tip;
  views-spec 6 is dropped because the order visibly stays the sort order).
- Drag out: the same data object a Corral builds for its items.
- Right-click empty body: the Gate menu (host spec 5) followed by the folder's background menu items that make sense here
  (New, Paste, Paste shortcut, Properties) from `CreateViewObject(IID_IContextMenu)`; Windows' View, Sort by, Group by and
  Refresh entries are replaced by Corral's View and Sort menus (section 5).
- Type-ahead: typing selects the next item whose name starts with the typed text (500 ms reset), in all three views.

### 2.4 What stays different on purpose
The folder glyph before the title, the Up button while not at the root, the error bodies, and "This folder is empty."
Nothing else in the title strip or body differs from a Corral with the same style.

## 3. Views

One pure layout function per view: `layout_view(view, body_px, items, metrics, scroll) -> Vec<Slot>` giving each item's
icon, label and hit rectangles; the painter, hit testing, selection, rubber band and drag all work on `Slot`s, so they are
shared by the three views. Vertical scrolling only.

### 3.1 Icons (today's Corral grid)
Unchanged for Corrals (grid-snapping-spec 2, icon zoom). Gates use exactly the same cell policy and label rules, so a Gate and
a Corral side by side line up and wrap names identically.

### 3.2 List
Rows of small icon + name, one item per row.

| Item | DIP |
|---|---|
| Row height | 24 |
| Icon | 16 (system small icon size, scaled with DPI) |
| Left inset, icon to text gap | 8, 8 |
| Right inset | 8 (the overlay scrollbar floats over it) |

- Name: label style, one line, end ellipsis; the full name shows in the existing tooltip when ellipsised.
- Always one column of rows at any width (user decision, question 1).
- Hover, selection and focus look: the Corral's own highlight (the same fill and outline it uses around icons today), as a
  full-row rounded rectangle (radius 4 DIP, inset 2 DIP).

### 3.3 Details
List rows plus a header and extra columns.

- Header: 24 DIP high, pinned at the top of the body (it does not scroll). Text in the label style at 85 % alpha, Caption
  size; a 1 px separator in the label colour at 25 % alpha under the header and between header cells only. The header has no
  background of its own (the body shows through), so it never reads as a Windows control.
- Sort indicator: a small chevron (Segoe Fluent Icons U+E70E up / U+E70D down, 8 DIP) centred above the sorted column's
  header text, as in the reference screenshot.
- Columns (section 4.3): Name is first and always shown, left-aligned, and takes the remaining width (minimum 96 DIP);
  Size is right-aligned; Type and Date modified left-aligned. Default visible columns: Name, Size.
- Widths: stored in DIPs per column; Name is not stored (it flexes). If the body is too narrow for Name's minimum, the
  rightmost columns are hidden for display until it fits (never horizontal scroll, never stored as hidden).
- Size text: `StrFormatKBSize` (Explorer's Details format, "1 KB", "639 KB"); folders and items without a size show nothing.
  Date modified: short date + short time in the user's locale (`GetDateFormatEx`/`GetTimeFormatEx`). Type: the type text
  of motion-sorting-spec 2.
- Header input: click = sort by that column (same column again = reverse); drag a separator = resize (minimum 40 DIP,
  live, stored on release); right-click = column menu (4.3). Hover over a header cell shows the label colour at 10 % alpha.

### 3.4 Shared rules
- Labels never use Windows' text drawing in a list view; they are drawn by Corral's text renderer with the Corral's label
  style in all views (so Auto text, shadow and Camouflage text effects apply).
- Icons in all views go through the Corral icon path (Camouflage tint, link-arrow setting, `hide_link_overlays`).
- Rubber-band selection, Ctrl/Shift selection, drag reordering (Corrals in Icons, and in List/Details only while unsorted)
  and drop-position insertion use `Slot`s. In List/Details an insertion point is a horizontal line between rows.
- Empty Corral/Gate: the view draws only the empty text (Gate) or nothing (Corral).
- Switching views keeps the selection and scrolls the first selected item (or the first visible one) into view. No
  animation (motion-sorting-spec 1 rule: only gestures animate; a view switch is a command but re-lays everything).

## 4. View state and storage

### 4.1 Corrals
New optional keys on the Corral entry in the layout (absent = default; older builds carry them as unknown keys):
- `view`: `"icons"` (default, not written), `"list"`, `"details"`.
- `columns`: ordered list of `{ "key": "size" | "type" | "date_modified", "width": DIP }` for Details; absent = Size only at
  80 DIP. Name is implicit.
A Corral's sort stays where it is today (`sort_method`, `sort_flags`). Clicking a Details header sets that sort, exactly as
the Sort menu would; "unsorted" remains possible from the menu.

### 4.2 Gates
Gates keep the per-folder view map of folder-gate-views-spec (same keys, inheritance from parent folders, save-on-user-change
only). Stored `view_mode` maps to the three views: 1, 5, 6 -> Icons; 2, 3 -> List; 4, 8 -> Details; absent -> Icons.
Writing uses 1 / 3 / 4. `sort_mode` keeps its meaning (+-1..5: Name, Size, Type, Date modified, Date created); a stored
Date created sort still sorts, but it has no column and the menus do not offer it. `col_info` becomes the Details column list
of 4.1 (parsed tolerantly; unknown columns kept on rewrite, not shown). `icon_size` is no longer applied: Gates follow the
Corral icon size and per-Corral icon zoom like Corrals.

The views-spec machinery for reading back a live Windows view (baseline, apply/verify, 300 ms checks, DPI suppression)
disappears: a view change is a Corral command, so it is saved immediately as a patch to that folder's entry.

### 4.3 Columns offered
Name (always), Size, Type, Date modified. Column menu (right-click on the header, and View > Columns): the three optional
columns as check items, then "Reset columns".

### 4.4 Sort
Corrals: existing sort keys. Gates: the folder's `sort_mode`; default Name ascending. Both use motion-sorting-spec 2 order
(folders first).

## 5. Menus

The Corral menu and the Gate menu gain a **View** submenu: Icons / List / Details (radio), a separator, Columns (only in
Details; the 4.3 items). The existing Sort menu is shared; for Gates it lists Name, Size, Type, Date modified, Ascending,
Descending (no "unsorted"). Wording follows Corral naming (English UI).

## 6. Migration and compatibility

- Existing Gates open in the view their folder entry resolves to (4.2), so an imported layout that stored Details keeps it.
- Layouts from older builds have no `view` keys: every Corral stays Icons.
- An older build reading a new layout ignores `view`/`columns` (carried as unknown Corral keys, tabs-spec 8).
- The `hide_link_overlays` per-Gate flag keeps working (it now applies to Corral-drawn icons).

## 7. Removed or changed behaviour (user-visible)

- Gates lose Windows' Tiles, Content and custom icon sizes, Windows' own View/Sort/Group menus inside the view, and
  Windows' column set (only the four of 4.3). Anything stored for those is kept but not shown.
- A Gate's items no longer use Windows' blue selection: they use the Corral highlight.
- The same-folder drop tip (views-spec 6) is dropped.
- The sorted-column shade, dark/overlay scrollbar patches and the inset frame disappear with the host.

## 8. Acceptance tests

Pure (no window):
- `layout_view` for each view: slot rectangles at 96/120/144/240 DPI; List stays one column at any width;
  Details header pinned under scroll; Name minimum hides rightmost columns, restores them when wider; no slot is wider than
  the body; hit testing returns the item under a point in every view and nothing in gaps.
- View state: `view` absent/unknown -> Icons; Gate `view_mode` mapping table both ways; `columns` parse/round-trip; unknown
  column keys kept; a view change of a Gate writes only that folder's style group.
- Gate item diff: create/delete/rename batches keep selection and order; 100 ms coalescing; navigation unregisters.
- Size and date formatting via injected formatters; sort header click toggles direction and updates the stored sort.
Live (paired screenshots at matched size and DPI):
- A Corral and a Gate with the same items, same style, side by side in each view: no visible difference except the glyph.
- Gate: open folder, Up/Back/Forward, Allow navigation off opens Explorer, drop in/out, rename, delete, paste, background
  menu, type-ahead, change notifications from Explorer (create/rename/delete), offline/missing folder errors.
- Details: header sort, column resize, column menu, narrow body hides columns; List stays one column when wide.
- 5000-item folder opens without stalling input; scroll stays smooth.

## 9. Decisions (user, 2026-10-28)

1. List view is always one column at any width.
2. Default Details columns: Name + Size.
3. Corrals and Gates offer the same four columns (as written).
4. Order of work: phase 1 = Gates drawn by Corral in Icons view (the Windows view removed); phase 2 = List and Details for
   Corrals and Gates.
