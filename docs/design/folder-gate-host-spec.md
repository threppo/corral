# Folder Gate hosting, history and menus: design spec (area D3)

Status: approved by the user 2026-10-22 (open questions resolved as the spec recommends, except as amended below)
Date: 2026-10-22
Author: Claude subagent (clean spec writer), inputs: spec-inputs-wave-d-2026-10-22.md + approved Corral design docs + public docs only

Terms: "Gate" = a Folder Gate. "Root" = the folder the Gate was made for (the ceiling of navigation). "Current" = the folder shown now.
"Host" = the window that carries the Windows folder view (`IExplorerBrowser`). "Key" = a path normalised as in folder-gate-views-spec 3.1.
DIP = 1/96 inch, `px(d) = floor(d * dpi / 96 + 0.5)` at the Gate's monitor DPI. `pad_px` and `strip_px` are the values of grid-snapping-spec 2.2
and corral-painting-spec 2.2. Public API names are from Microsoft's documentation; every number is a proposal with a reason.

## 1. Goal and non-goals
Goal: a Gate looks like part of its Corral (same background, same text colour, no white box), behaves like Windows' own folder view for
everything inside the body, never shows a stale or blank rectangle, and never leaves its root unless the user asks Windows to open
the target in a normal window.

Non-goals:
- Which view, sort and columns a folder uses and when they are saved: folder-gate-views-spec. This spec only says when the host raises its events.
- Title strip look, Up button and glyphs: corral-painting-spec. No Back/Forward buttons (an amendment to that spec).
- Drawing items, columns, headers or scrollbars: Windows draws them. Camouflage of hosted icons: camouflage-spec.
- Moving, resizing, snapping, folding and docking rules: grid-snapping-spec and motion-sorting-spec; this spec only says how the host follows them.
- Creating a Gate: creation-spec.

## 2. Hosting
### 2.1 Layers

A Gate is two windows on the UI thread (one STA for all Gates; `IExplorerBrowser` is apartment-bound):
- **Corral window** (existing, layered): paints the title strip, the outline, the outer ring of the body and, while the host is shown, leaves
  the host rectangle fully transparent (alpha 0) so two translucent layers never stack (a stack would look brighter and fade wrongly).
- **Host window**: a `WS_POPUP`, `WS_EX_LAYERED | WS_EX_TOOLWINDOW` window owned by the Corral window (it follows its owner's z-order, no taskbar
  button), class-registered once. Its client area is exactly the host rectangle. It paints its own backdrop (2.4) and the Loading/Error body (3).
  The browser's child window lives inside it. Its title text is the Corral title (for screen readers). It is not `WS_EX_NOACTIVATE`: the user
  must be able to type in it.
Fade (Clear view, idle fade, Camouflage-independent) sets the host's whole-window alpha with `SetLayeredWindowAttributes(LWA_ALPHA)` to
`floor(255 * fade + 0.5)`, the same factor the Corral window uses, so both layers fade in step.
### 2.2 Creation sequence
1. Create the host hidden. 2. `CoCreateInstance(CLSID_ExplorerBrowser)`, `Initialize(host, rect, FOLDERSETTINGS)` with the initial view mode
resolved by folder-gate-views-spec 3.2. 3. `SetOptions(EBO_NOBORDER | EBO_NOTRAVELLOG | EBO_NOPERSISTVIEWSTATE | EBO_ALWAYSNAVIGATE)`:
no border (the Corral draws the frame); no travel log (Corral keeps its own history, 4); no Windows-saved view state (views-spec 3);
always navigate (so Refresh and re-entry work). `EBO_SHOWFRAMES` stays off: no navigation, details or preview panes. 4. `SetEmptyText` (3.2).
5. `Advise` an `IExplorerBrowserEvents` sink; implement `IServiceProvider` and `IInputObjectSite` on the site so keys and focus work (2.5).
6. Probe the folder on a worker (3.3) and parse its path to a PIDL there (`SHParseDisplayName` is free-threaded). 7. `BrowseToIDList` on the UI thread.
The host stays hidden until `OnViewCreated` has run and the backdrop and text colours are applied (no white flash). If that has not happened
after 1000 ms the host is shown anyway with the Loading body (3.1). A Gate is created the first time its Corral becomes visible and lives
until removed (hidden Ranges keep their browsers; open question 9).
### 2.3 Size and insets
`host_rect(body_px, dpi) = body_px` inset by `pad_px` (8 DIP: 8/10/12/16/20 px at 96/120/144/192/240 dpi) on all four sides.
Reasons: it is exactly the inner area grid-snapping-spec 2.2 gives icons, so whole-cell sizing and "cells that fit" agree for Corrals and Gates;
the 8 DIP ring keeps the whole resize band (grid 3.1) and the rounded corners (a rectangle inset 2.4 DIP clears an 8 DIP radius) free of the
view; and the view's own scrollbar sits 8 DIP inside the right edge, as the Corral scrollbar rule requires. The title strip is outside the body.
If either inner side is below 32 DIP the host is hidden and the Corral window paints the body normally (a Gate that small is a handle).
`BeginDeferWindowPos` moves the Corral window and the host in one step. After a move or resize the browser gets `SetRect(0, 0, w, h)`; during an
interactive resize `SetRect` runs at most once per 16 ms with the newest size and always once more on release. Folded or being animated (Fold open
180 ms, close 140 ms): the host is hidden and the Corral window paints the body, then the host is shown again when the animation ends
(order: show host, then repaint the Corral window with the hole; hide in the reverse order). Hover-unfold works the same.
### 2.4 Backdrop and translucency
The Windows view cannot draw alpha, so the Gate bakes the Corral look under it:
- `gate_backdrop(PaintInputs, host_rect) -> Bitmap` runs corral-painting-spec 3.1 steps 1-2 only (blurred wallpaper crop, body fill with the
  see-through alpha, Camouflage backgrounds when on; high contrast = opaque `COLOR_WINDOW`) for the host rectangle, with `fade` = 1. It is the
  same pure code the Corral window uses, so ring and host match without a seam. Rebuilt (throttled to 16 ms) when the look, blur, wallpaper, size
  or DPI changes; never per frame while idle.
- The host paints it in `WM_ERASEBKGND`, and installs it on the view's list-view child (found by walking the browser window's children for
  class `SysListView32`) with `LVM_SETBKIMAGE` (`LVBKIF_SOURCE_HBITMAP`, `LVBKIF_STYLE_NORMAL`), `LVM_SETTEXTBKCOLOR = CLR_NONE` and
  `LVM_SETTEXTCOLOR` = the body label colour (`auto_text` of the body fill alone, painting spec 4.4). All of this is re-applied in every
  `OnViewCreated` (a view mode change creates a new view) and after every backdrop rebuild.
- If no `SysListView32` is found or the view refuses the image, the host still shows the baked backdrop behind the view and the view keeps
  Windows' own colours; Gate content is then opaque. This is an accepted degradation, logged once per Gate (question 2).
Selection highlight, focus rectangle and scrollbars stay Windows' own (they follow the system theme and dark mode of the process).
### 2.5 Focus, keyboard, z-order
- A click in the view activates the host (`WM_MOUSEACTIVATE` returns `MA_ACTIVATE`); the host's `WM_WINDOWPOSCHANGING` removes any z-order
  change that would lift the Gate above other applications' windows, so keyboard focus never raises a desktop-level Corral (Glance raises
  the whole stack through its owner, and the host follows).
- The message loop offers key messages to the browser through `IInputObject::TranslateAcceleratorIO` before dispatching (Tab, arrows,
  F2, Delete, Ctrl+C/V, Enter and type-ahead are Windows' own). Corral claims only these, before the view sees them (`route_key`):
  Alt+Left = Back, Alt+Right = Forward, Alt+Up = Up, Backspace = Back and `WM_APPCOMMAND` browser back/forward (mouse side buttons), all only
  when Allow navigation is on and no rename edit has focus. Alt+Left/Right while focus is inside a Gate belong to the Gate; Range switching with
  those keys works when the desktop has focus. Alt+wheel is never consumed by the host (it forwards it to Range switching). Esc is not used.
- Ctrl+wheel (icon size) and F5 are Windows' own; the resulting view change is picked up by folder-gate-views-spec triggers.
- Double-click on empty space in the view (a subclass on the list view: `WM_LBUTTONDBLCLK` and `LVM_HITTEST` "nowhere") runs Open folder.
- The host reports `busy` to the engine while a menu is open (`WM_INITMENUPOPUP` to `WM_EXITMENULOOP`), a rename edit is active or a mouse
  button is down, so hover-fold does not close the Gate under the user's hand (motion-sorting-spec 3).

## 3. Body states, empty text, errors
### 3.1 States

`Creating -> Loading -> Ready`, with `Error(kind)` reachable from Loading and Ready, `Hidden` (folded, too small, Range hidden) orthogonal.
Loading: the browser window stays hidden; the host shows the plain backdrop; after 1000 ms it also shows "Opening folder..." (Caption, label
colour, centred). Error replaces the browser by the error body (3.3). Retry returns to Loading.
### 3.2 Empty folder
`SetEmptyText("This folder is empty.")` (Corral wording, no trailing hint, no Windows name). The view draws it in its own colour; Corral cannot
recolour it (question 3).
### 3.3 Missing, offline, denied
Before every navigation that starts from a stored or chosen path, a worker probes `GetFileAttributesW` with a 3000 ms limit (reason: SMB
failures normally return within a few seconds, and the UI thread must never wait on a dead share). Result `Probe::{Ok, Missing, Denied, Offline, Other(code)}`:

| Kind | Win32 codes (or) | Title | Detail |
|---|---|---|---|
| Missing | 2, 3, 267 | This folder isn't here | It may have been moved, renamed or deleted. |
| Offline | 21, 53, 59, 64, 67, 121, 1167, 1201, 1222, 1231, 2250, probe timeout | Can't reach this folder | Check the network connection or the drive, then try again. |
| Denied | 5, 1219, 1326 | No access | You don't have permission to open this folder. |
| Other | any other failure, `OnNavigationFailed` | Can't open this folder | Windows reported error 0x%08X. |

The error body is drawn by the host in DIPs: Warning glyph U+E7BA 20 DIP, title (Body strong), detail (Caption, text/secondary tone of the label
colour), the path (Caption, middle-ellipsised, tooltip shows it whole), then two buttons in the settings kit style: Secondary "Try again" and
Subtle "Choose a different folder...". Text and buttons use the label colour of the body, never a fixed colour. At narrow widths text wraps; below
the minimum for the glyph and title only the title shows. The title strip is untouched, so the Gate can still be moved, folded and given a menu.
Retries: the "Try again" button; the Gate menu's Refresh; `DBT_DEVICEARRIVAL` and `PBT_APMRESUMEAUTOMATIC`; for Offline only, every 60 s while the
Gate is visible (reason: a VPN or share can return with no event; a missing folder will not). A late `Ok` from a probe that had timed out recovers
the Gate by itself. A Gate sitting in a subfolder that disappeared climbs to the nearest existing ancestor inside the root (4.4) before it errors.

## 4. Navigation and history
### 4.1 Model (pure `GateNav`)

`GateNav { root, entries: Vec<String> (1..=20), cursor, allow }`, `current = entries[cursor]`. Invariants: every entry is `root` or inside it
(`in_root(root, p)`: same key, or the key of `p` starts with the root key plus `\`; a root that ends in `\` matches by prefix); entries never
repeat consecutively; at most 20. Reason for 20: a useful trail, a few kilobytes of layout at most, and Back is a convenience, not an archive.
### 4.2 Operations
- `visit(path)` (after a successful `OnNavigationComplete`): same key as current = nothing; otherwise drop the entries after `cursor`, append,
  and if the list exceeds 20 drop the oldest entry. Failed navigations (`OnNavigationFailed`) never reach history. Refresh does not record.
- `back` / `forward`: move the cursor, then browse; if the browse fails, the entry is removed from the list (no dead ends) and the cursor stays.
- `up`: if `current` is not `root` (by key), `visit(parent(current))`, where parent drops the last path component and trailing separators.
  At the root it does nothing (button disabled, 4.5). Up never leaves the root.
- `to_root`: `visit(root)` unless already there. `set_allow(false)`: browse to the root now, `entries = [root]`, so a Gate that cannot navigate
  never sits in a subfolder. `set_allow(true)`: entries stay `[root]`. `choose_root(folder)`: new root, `entries = [folder]`, `cursor = 0`; the
  Corral's title is not changed (question 6).
### 4.3 Where a navigation may go (`decide_nav(state, target, origin) -> Allow | OpenExternally | Cancel`)
`origin` is `Corral` (a Gate command, Back, Forward, Up, root, restore) or `View` (the user opened something in the view, reported by
`OnNavigationPending`, which Corral answers with a failure `HRESULT` to cancel; the documentation says any failure code cancels).
- Target without a file-system path, or outside the root: `OpenExternally` (`ShellExecuteEx`, `SEE_MASK_IDLIST`, verb "open", normal window), and
  the view is cancelled. This covers folder shortcuts that point elsewhere and links inside files.
- `origin == View` and Allow navigation is off: `OpenExternally` for any folder that is not the current one. Files always open as Windows does.
- Otherwise `Allow`. A navigation started by Corral carries a token so its own `OnNavigationPending` is never judged as `View`.
Before `Allow` is returned the views-spec flush runs (4.6).
### 4.4 Restart
Persisted (inside the existing Gate entry; no new key): the root, the history list and the Allow-navigation flag. The cursor is persisted only if
the existing `nav_history` format has room for it; otherwise it restarts at the last entry, so forward entries are lost, nothing else (question 1).
Not persisted: scroll offset, selection, error state, view preferences (those live in the per-folder map). On start, `resolve_start(nav, exists)`:
current is browsed if it exists; if not, the nearest existing ancestor that is still inside the root replaces it (appended as a visit); if the root
itself is unavailable the Gate starts in `Error` with the stored state untouched, so Retry resumes exactly where it was. Entries are written with
the layout on each `visit`/`choose_root`/`set_allow`, never on Refresh or on a failed navigation. A stored list longer than 20 keeps the 20 entries
ending at the cursor; a list with entries outside the root drops them. Loading never rewrites the file.
### 4.5 New Gate defaults
Allow navigation on (reason: the Up button and the settings copy about opening subfolders only make sense then); history `[root]`; view
preferences as folder-gate-views-spec 3 (icon view, desktop icon size, Name ascending); Opacity, colours and the rest as for a new Corral. Up is
shown only when Allow navigation is on and disabled at the root; "Navigate up" and "Navigate to root" are hidden when it is off and disabled at the root.
### 4.6 Events to the views state machine
`OnNavigationPending` -> flush (views 4) then `decide_nav`; `OnViewCreated` -> apply preference, backdrop and text colour (2.4); `OnNavigationComplete`
-> `visit`, take the baseline, Loading to Ready; `OnNavigationFailed` -> keep the previous Settled state, classify (3.3), keep the old view if there
is one. Closing, removing or tearing down the Gate flushes first, then `Unadvise`, `IExplorerBrowser::Destroy`, destroy the host, release on the UI thread.

## 5. The Gate menu
Opened by the strip's menu button and by right-click on the strip, drawn by the shared Corral popup component. Right-click inside the view is
Windows' own item or background menu (copy, delete, properties and the rest must stay available); only section 6 touches it. Commands are internal ids
(0x6101-0x6124 = Gate family; 0x60xx stays with the general Corral menu; no relation to Windows' ids). Structure:

| Id | Label | Kind and rule |
|---|---|---|
| 0x6101 | Open folder | command: opens `current` in a normal Explorer window (`ShellExecuteEx` "open") |
| 0x6102 | Choose a different folder... | command: folder picker as in creation-spec 4; sets the root (4.2) |
| 0x6103 | Configure folder | command: Windows Properties of `current` (`SHObjectProperties`); question 4 |
| | separator | |
| 0x6104 | Refresh | command: `IShellView::Refresh` on the current view, and retry in an error state |
| 0x6105 | Navigate up | command; hidden if Allow navigation is off, disabled at the root |
| 0x6106 | Navigate to root | command; same rule |
| 0x6107 | Allow navigation | check item; toggling runs `set_allow` |
| | separator | |
| | View (submenu) | |
| 0x6110-0x6113 | Extra large icons / Large icons / Medium icons / Classic icons | radio: icon view at 256 / 96 / 48 / 32 px (question 5) |
| 0x6114, 0x6115 | List / Details | radio: view modes 3 and 4 |
| 0x6116 | Hide details header | check; enabled only in Details; writes the folder flag "header hidden" |
| 0x6117 | Hide shortcut arrows | check; bound to the shortcut-arrow flags of the settings dialog (question 7) |
| | Sort by (submenu) | |
| 0x6120-0x6123 | Name / Size / Item type / Date modified | radio, the first four stored sort columns of views 2 |
| 0x6124 | Reverse order | check; flips the direction |
| | separator, then the shared Corral section | Fold, Opacity, Keep visible in Clear view, Show on every Range, Titlebar position, Colors..., Copy color, Paste color, Reset color, Rename Corral, Adjust spacing... (owned by the Corral menu design, appended unchanged) |

Radio state comes from the resolved view: the icon radios are checked when the mode is icon view and the effective pixel size equals the entry
(a Follow size equal to 48 shows Medium); other sizes check none; Date created or any non-stored sort checks none. Each view or sort command builds
a views-spec patch (mode/size/flags/sort), applies it to the live view and triggers the immediate check of views-spec 4. The menu model is a pure function
`gate_menu(GateMenuState) -> Vec<Item>` (ids, labels, enabled, checked, hidden); label text is English and never contains another product's name.

## 6. Hiding listed Shell entries (interop)
If an installed program adds a Shell entry to the folder-background menu that is listed in the `HiddenShellMenuEntries`
setting (no UI; empty by default), Gates must not show it. Rule: the host subclasses the folder view window and, for every `WM_INITMENUPOPUP` the view receives (after the default handler has let
the Shell extensions fill the menu), walks the `HMENU` with `GetMenuItemInfo(MIIM_STRING | MIIM_ID | MIIM_FTYPE | MIIM_SUBMENU)`. An item is
recognised by its label, normalised: remove `&`, trim, remove a trailing `...` or U+2026, case-fold, collapse spaces, and compare for equality with
each listed label, normalised the same way (the full string, so with "Configure Widgets" listed, "Configure Widgets Pro" or "Configure folder" are
untouched). Matching items are removed with
`RemoveMenu(MF_BYPOSITION)`; a separator left leading, trailing or doubled by the removal is removed too, and only then. Corral never executes the
entry, never sends its id and never edits another product's registration; recognition by handler is not possible from a menu handle. The same walk
covers submenus and the classic menu behind "Show more options". The Gate's own menu never contains the entry. With an empty list nothing is removed.

## 7. DPI and mixed monitors
`pad_px`, strip, error-body metrics and the backdrop are re-derived from DIPs on `WM_DPICHANGED`; the host moves with the Corral window, the browser gets
`SetRect`, the backdrop and list-view image are rebuilt, and view icon size and column widths are re-applied by folder-gate-views-spec 5. The host
window is per-monitor DPI-aware (v2) like the process. The popup menu is created at the DPI of the monitor it opens on.

## 8. Acceptance tests (pure unless stated)
1. `host_rect`: inset equals 8/10/12/16/20 px at 96/120/144/192/240 dpi on all sides; zero-size and 40 DIP bodies hide the host; a corner point of the
   host rect lies inside the rounded shape for radii 0 to 8 DIP; host rect + ring tile the body with no gap.
2. `GateNav`: visit/dedupe/forward-truncation; cap 20 (21st visit drops the oldest); back/forward bounds; `up` stops at root; `to_root` at root is a no-op;
   a failed back removes its entry; `set_allow(false)` yields `[root]`; `choose_root` resets; invariants hold under 10000 random operations.
3. `in_root`: `C:\a` vs `C:\ab` (not inside), `C:\a\b` (inside), trailing separators, case, `\\?\` forms, UNC share roots, a root `C:\`.
4. `decide_nav`: outside root and non-file-system targets go external in both origins; View with Allow off goes external except the same folder;
   Corral origin always allowed inside root; own navigation token not judged as View; files untouched.
5. `resolve_start`: existing current; deleted subfolder climbs to the nearest existing ancestor; root missing gives Error with state unchanged; entries
   outside root dropped; list over 20 trimmed around the cursor; load never rewrites.
6. `classify_probe`: every code in the table, the timeout, an unknown code; late Ok recovers; Offline schedules a 60 s retry, Missing does not.
7. `gate_menu`: item set, enabled/hidden/checked for Allow on/off, at root/not, each view mode and size, sort states, reverse; ids unique and inside
   0x6101-0x6124; labels equal the table; no label contains a product name.
8. `filter_shell_menu(items, hidden)` with "Configure Widgets" listed: removes "Configure Widgets", "&Configure Widgets...", "CONFIGURE  WIDGETS" and the
   U+2026 form; leaves "Configure Widgets Pro",
   "Configure folder", "Properties"; fixes separators (leading, trailing, double) only around a removal; idempotent; submenus walked; the removed id is
   recorded and never invoked.
9. `route_key`: Alt+Left/Right/Up, Backspace, `WM_APPCOMMAND` back/forward claimed only with Allow on and no rename edit; Alt+wheel never claimed; every other key
   passes to the view.
10. Resize coalescing: a burst of N size events produces at most one `SetRect` per 16 ms and the last one carries the final size.
11. Event sequence with fakes: Pending (flush before decide), ViewCreated (apply once), Complete (visit once), Failed (history untouched), teardown order.
12. Backdrop: `gate_backdrop` pixels equal `paint_corral` steps 1-2 for the same rect; high contrast gives opaque `COLOR_WINDOW`; fade is not baked in.
13. Live checklist: no white flash on open; text colour on dark/light/see-through Corrals; empty text legible; drag and drop into the view; rename; the Windows
    context menu (classic and Win11 "more options") with the product's entry installed; Alt+Left in a Gate versus on the desktop; offline share and unplugged
    drive then reconnect; folder shortcut to elsewhere; mixed-DPI move; Fold hover over a Gate; z-order stays under apps after typing in a Gate.

## 9. Open questions
1. What exactly does the stored `nav_history` hold (list vs single path, where the root and cursor live)? The logical model is fixed here; the mapping needs a real sample.
2. Does `LVM_SETBKIMAGE` plus `CLR_NONE` text background work on the hosted `SysListView32` in every view mode, after navigation and on Windows 10 and 11?
3. Can the empty-folder text be recoloured, or must Corral draw it itself (poll `IFolderView2::ItemCount` after complete)?
4. What does the current "Configure folder" command do? Properties of the folder is my reading.
5. Is "Classic icons" the 32 px icon view? 256/96/48 for the other sizes follow Windows' own steps.
6. Should the Gate title follow the folder name after "Choose a different folder..." when the user never renamed it?
7. How is "Hide shortcut arrows" realised inside a hosted Windows view, per Gate or global (it is global in the Personalization dialog), and is it feasible?
8. Does Windows' folder background menu appear classic or modern in a hosted view, and does `WM_INITMENUPOPUP` reach the subclass in both?
9. Memory: is one live browser per Gate acceptable with many Gates, or should hidden Ranges' browsers be released and recreated?
10. Should a failed navigation inside a Gate show a notice (Windows may show its own)?
11. Is a 3000 ms probe and a 60 s offline retry right for slow networks and VPNs?
12. Should Allow navigation default to on for a Gate that an older layout stores without the flag?

## Amendment (2026-10-22, user decision)

Translucency: implement the baked backdrop first. If it does not work live (view modes, navigation, Windows 10/11), fall back to a standard Windows translucency method for the host window (for example DWM blur-behind or a layered host with per-pixel alpha), chosen by the implementer and recorded in the report.

## Amendment 2 (2026-10-27, user decision)

Camouflage stays on Folder Gates (camouflage-spec covers Gate icons): Windows still hosts and draws the view, but
Corral recolours each item icon with the Camouflage effect after Windows paints it (list-view custom draw,
post-paint of the icon rectangle), using the same per-Corral palette, strength, hover/active restore and caches as
ordinary Corrals. Labels follow the Auto text colour rule. Scrollbar and rename box stay Windows-drawn.

## Amendment 3 (2026-10-28, live check answer to open question 2)
On Windows 11 the Explorer browser's default view is not a `SysListView32`, so the backdrop, label colour and icon
Camouflage could not reach it. Gates therefore ask the browser for the classic list view (`IFolderViewOptions`,
`FVO_VISTALAYOUT`) before `Initialize`, on every Gate browser and on the hidden icon-spacing reference view. The
backdrop goes in as a list-view watermark image of the list's client size (fixed, never scrolled) instead of a normal
background image. The browser control's border is removed before the browser is first sized. Column headers are
hidden outside Details (`FWF_NOHEADERINALLVIEWS`); Details keeps its header unless the user hides it. The list never
shows a selected (shaded) sort column, so labels stay readable on the backdrop; sorting is unchanged.
