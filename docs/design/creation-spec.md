# Creating Corrals and the quick Folder Gate: design spec (area D2)

Status: approved by the user 2026-10-22 (open questions resolved as the spec recommends, except as amended below)
Date: 2026-10-22
Author: Claude subagent (clean spec writer), inputs: spec-inputs-wave-d-2026-10-22.md + approved Corral design docs + public docs only

Terms: "Corral" includes Folder Gates. DIP = 1/96 inch; `px(d) = floor(d * dpi / 96 + 0.5)` at the DPI of the monitor under the
press or drop point. "Empty desktop" = a point on a monitor's work area where neither a Corral (that takes input) nor a desktop item
is under the pointer. "Outer rect" and the cell helpers are those of grid-snapping-spec. Every number below is a proposal with a reason.

## 1. Goal and non-goals
Goal: two quick ways to make a new Corral or Folder Gate that cannot be started by accident, never touch a file, and always end with
a Corral that is fully on screen at the size the user can see in the preview.

Non-goals:
- Sizing, snapping, whole-cell rounding and on-screen rescue are grid-snapping-spec 3.4, 3.5, 5 and 6; this spec only calls them.
- Creating by menu, keyboard or tray (question 8), touch and pen input, moving selected icons into the new Corral.
- What a Gate shows or how it navigates (folder-gate-host-spec, folder-gate-views-spec).
- No new setting. `CreateCorralMode` (0 either button, 1 left, 2 right) keeps its meaning; absent means 0.

## 2. Drawing a rectangle
### 2.1 What may start a draw

A press starts a candidate ("pending") only when all hold:
1. Mouse input. Messages carrying the touch/pen extra-info signature (`GetMessageExtraInfo() & 0xFFFFFF00 == 0xFF515700`) never start
   one. Reason: single-finger drags belong to scrolling and Range gestures.
2. The button is allowed by `CreateCorralMode`: left button or right button; the middle button never draws (Ranges own it).
3. The press is on empty desktop (above). A press on a desktop item or on a Corral (title, body, edge, scrollbar, hosted Gate view) is
   that object's gesture. The item test is the desktop list view's own hit test (`LVM_HITTEST`, "nowhere" = empty, a label counts as item).
4. No modifier is down at press time: Shift, Ctrl, Alt and Win all up. Ctrl or Shift plus a left drag stays Explorer's selection
   gesture, and Alt stays free. Later modifier changes do nothing, except Shift, which keeps its grid-snapping meaning (snapping off).
5. No other button is down, the desktop is not in Clear view (a cleared desktop first has to be shown), and no Range transition
   is running. Reason: creating something the user cannot see, or on a page that is sliding away, is a surprise.

A plain left drag therefore draws instead of rubber-band selecting; users who want selection by drag set the dropdown to "Right
button" or hold Shift/Ctrl (question 1). Requirement either way: while Drawing, no Explorer selection rectangle is visible, and when
the gesture ends the desktop selection equals the selection at press.
### 2.2 States and transitions (pure reducer `draw_step(state, event) -> (state, effects)`)
| State | Event | Result |
|---|---|---|
| Idle | qualifying press (2.1) | Pending(button, start_px, monitor) |
| Pending | move, larger axis offset below `ARM` | stay Pending |
| Pending | move, larger axis offset at or above `ARM` | Drawing |
| Pending | release | Idle; the press-release is an ordinary click (desktop click, double-click counting, and for the right button the normal desktop context menu) |
| Pending / Drawing | another button down, Esc, `WM_CANCELMODE`, capture lost, `WM_DISPLAYCHANGE`, DPI change of the press monitor, Clear view or Range change | Idle; nothing is created, no click is generated |
| Drawing | move | recompute the preview (grid-snapping-spec 6: clipped to the press monitor's work area, minimum size, whole cells or edge snapping, Shift = raw) |
| Drawing | release, `creation_rect` is None (under 16 DIP on both axes) | Idle; nothing created; the right button's context menu is suppressed because the gesture was a drag |
| Drawing | release, rect valid | Chooser(rect, monitor) |

`ARM` = the larger of 16 DIP and the system drag size (`GetSystemMetricsForDpi(SM_CXDRAG / SM_CYDRAG, dpi)`, default 4 px, so users with a
larger accessibility setting get a larger zone). Reason: 16 DIP is the creation cancel limit of grid-snapping-spec 6, so a drag either
never became a draw or is big enough to be a rectangle; shrinking the rectangle back below 16 DIP before release cancels it.
Pointer capture is taken on arming, not on press, so a plain click or double-click is untouched.
A draw that armed is not a click: it does not feed the double-click detector that toggles Clear view.
### 2.3 Preview
A topmost, click-through, non-activating layered window (created at the DPI of the press monitor) shows the outer rect of what will be
created: rounded rect with the Corral radius (8 DIP, square when `SquareEdges`), fill `accent/default` at 14 % alpha, 2 DIP inner
`accent/default` stroke, and a 1 px outer hairline in `text/on-accent` at 60 % alpha so the shape reads on dark and light wallpapers
(settings-redesign tokens, light/dark aware). The size tooltip of grid-snapping-spec 7 is shown. The preview always shows the
snapped result, never the raw pointer rectangle. While the Chooser is open the preview stays visible and frozen.

## 3. The chooser
A small popup menu (the shared Corral popup component: `bg/surface`, radius 8, DWM shadow, 32 DIP items, per-monitor DPI-aware) with
exactly two items, in this order:
1. `Create Corral here`   (mnemonic C)
2. `Create Folder Gate here`   (mnemonic F)

- Opens with its top-left 4 DIP right of and below the release point, flipped to stay inside the work area (right and bottom).
- Mouse: hover highlights, click chooses. For a right-button draw the menu opens after the release, so the release can never choose.
- Keyboard: it takes focus when shown; Up/Down/Home/End move (nothing is highlighted until the first key or hover), Enter chooses, C/F
  choose directly, Esc closes. Focus-loss, a click elsewhere, a display change or the Range changing closes it; closing never creates.
- Choosing removes the preview. Closing without choosing removes it too and leaves the desktop exactly as before the press.

## 4. What is created
- **Corral**: outer rect from grid-snapping-spec 6; stored `rect` = the body, `preferred_size_dip` = the created size; on the Range that
  is active and the monitor under the press; default style, default title as for any new Corral; empty; no rename box opens (open
  question 9). It is saved with the normal layout save immediately.
- **Folder Gate**: the chooser item first opens the system folder picker (`IFileOpenDialog`, `FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM`,
  owned by no Corral window, starting in the user's Documents folder; reason: a common place that exists on every account). Cancel
  creates nothing. A chosen folder creates the Gate at the previewed rect with the Gate defaults of folder-gate-host-spec 4.5; the
  preview is hidden while the picker is open. Only file-system folders (local, mapped or UNC) can be chosen; shell-only folders
  (question 6) cannot.
Both paths put the new Corral on top of the Corral stack and never move, resize or hide any other Corral. Overlap with existing
Corrals is allowed (the user drew it); snapping to neighbours already helps tidy placement (question 10).

## 5. Alt-drop: a folder dropped with Alt becomes a Gate
### 5.1 Where it applies

The drag source is anything that offers file-system items (Explorer, the desktop, a Corral, another app). The drop point must be empty
desktop, a Corral's title strip, or a Corral's empty body area (not over one of its items). Over a desktop folder icon, over a Corral
item that accepts drops, and inside a Gate's hosted view the ordinary drop target wins and Alt does nothing special.
### 5.2 Modifier rules (decided at the moment of the drop, using the key state the drop target is given)
An Alt-drop is: left-button drag (`MK_LBUTTON`), `MK_ALT` set, `MK_SHIFT` and `MK_CONTROL` both clear. Rules:
- Alt is sampled on every `DragOver` and at `Drop`, so the user may press or release Alt mid-drag; the last state wins.
- Alt plus Ctrl or Shift is not an Alt-drop (the ordinary handler gets the drop unchanged). Right-button drags are never Alt-drops
  (the source's own right-drag menu stays).
- AltGr (Right Alt) counts as Alt: the Ctrl that layouts with AltGr inject (a left Ctrl with scan code 0x21D immediately before Right Alt)
  is ignored, using the same rule as the Glance shortcut spec (question 7). Reason: the user's Polish layout has no other Alt on the right.
### 5.3 Which dropped items count
Items come from the data object via `SHCreateShellItemArrayFromDataObject` (works for `CFSTR_SHELLIDLIST` and `CF_HDROP` / `DragQueryFile`).
An item is a **folder** when `IShellItem::GetAttributes` reports `SFGAO_FOLDER | SFGAO_FILESYSTEM` and not `SFGAO_STREAM` (a .zip is a
file) and it has a file-system path (`SIGDN_FILESYSPATH`). Drive roots, UNC paths, mapped drives and junctions count; shortcuts to
folders, Libraries, This PC, Recycle Bin and Control Panel do not (they are files or shell-only; questions 5 and 6).
Classification runs once per drag, on a worker, from the attributes the source supplied, with a 1000 ms budget (reason: a dead network
path must not freeze the desktop). Before it finishes `DragOver` shows the ordinary feedback; if it has not finished at `Drop`, the drop
is not an Alt-drop.
### 5.4 Several items and non-folders
- No folder among the items: not an Alt-drop at all; the ordinary handler gets the data object unchanged.
- At least one folder: each folder becomes one Gate; files in the same drop are ignored (never copied, moved or linked).
- More than 6 folders (reason: a Select-All slip would otherwise open dozens of windows): the drop is refused with
  `DROPEFFECT_NONE` and the drop tip "Drop up to 6 folders at once"; nothing is created.
- Two items with the same folder path make one Gate. A folder that already has a Gate gets another one (views-spec: Gates may share).
### 5.5 Feedback and effect: nothing moves
While an Alt-drop would be accepted, `DragOver` returns `DROPEFFECT_LINK` only (never `COPY` or `MOVE`, so a source that deletes on
`MOVE` can never delete anything) and sets the drop description (`DROPIMAGE_LABEL`, text "Create Folder Gate here" or
"Create %d Folder Gates here") when the source supports it. At `Drop` the files are not read, written, moved or touched; the effect
returned is `DROPEFFECT_LINK`. The only change is new Corrals in the layout.

## 6. Quick Gate size and position
### 6.1 Size

Default body = 5 columns x 4 rows of cells: `body_size_for(5, 4)` from `cell_metrics` at the DPI of the monitor under the drop point (96 dpi,
Windows spacing 75, cell 75 x 88 DIP: body 391 x 368 DIP, outer 391 x 392 with the 24 DIP strip). Reason: about 20 icons in a squarish
shape, roughly a seventh of a 1366 x 768 desktop, and exactly whole cells. It is clamped per axis to the work area (grid-snapping-spec 3.3, 5) and never below the
minimum. The size is stored as `preferred_size_dip`, as for a drawn Corral.
### 6.2 Position (`quick_gate_rects(drop_pt, count, work, dpi, gap_px, strip_px, sizes) -> Vec<Rect>`)
- The first Gate is centred horizontally on the drop point, with its top edge `strip_px / 2` above it, so the pointer rests on the title
  and the user can drag the Gate straight away.
- Then the grid-snapping-spec 4 move snapping runs (work area and neighbours, Shift ignored) and `place_on_screen` (section 5 there),
  so the result is on one monitor and fully inside its work area. The governing monitor is the one under the drop point.
- Several Gates tile left to right in drop order; the gap is `max(gap_px, 4 DIP)` (reason: a batch must read as separate Gates even with
  the gap toggle off). A row that would pass the work-area right edge wraps to a new row below, back at the first Gate's left edge.
  Each rect then goes through `place_on_screen` separately; a batch taller than the screen overlaps rather than leaves the screen.
- The Range is the one active at the drop; the Gates are saved at once.

## 7. DPI
All thresholds and sizes above are DIPs converted with `px()` at the monitor under the press (draw) or drop point (Alt-drop). The preview,
tooltip and chooser are created at that DPI and rebuilt if the pointer enters a monitor with another DPI; the draw rectangle itself stays
in press-monitor pixels and is clipped to that monitor (grid-snapping-spec 6). A DPI change of the press monitor during a draw cancels it
(2.2). Created rects are stored in DIPs by the exact ratio of grid-snapping-spec 8.

## 8. Acceptance tests (pure unless noted; DPIs 96, 120, 144, 192, 240; negative monitor origins as in grid-snapping-spec test 11)
1. `qualifies_press`: left/right per `CreateCorralMode` 0/1/2 (absent = 0); middle never; any of Shift, Ctrl, Alt, Win blocks; touch/pen
   signature blocks; press on item or Corral blocks; Clear view or Range transition blocks; second button blocks.
2. `draw_step`: table walk of 2.2. Move of 15 DIP stays Pending, 16 DIP arms (and at dpi 240 `ARM` is 40 px); a larger `SM_CXDRAG` raises
   `ARM`; release in Pending is a click; Esc / second button / capture loss in Drawing creates nothing and no click.
3. Shrink back: arm at 40 DIP, return to 10 DIP x 10 DIP, release: cancelled, no context menu; 17 DIP x 2 DIP release: Chooser with the
   minimum size (matches grid-snapping-spec test 14).
4. Up-left, up-right and down-left drags keep the anchor fixed; the rect is clipped to the press monitor; Shift gives raw rects.
5. Click counting: a drawn gesture between two clicks breaks the double-click; two plain clicks still count.
6. Chooser: placement flips at right and bottom work-area edges; Up/Down/Home/End/Enter/Esc/C/F; focus loss closes without creating.
7. Cancel leaves the layout byte-identical; a created rect and `preferred_size_dip` equal the previewed size and save, load, save bit-identical.
8. `alt_drop_decision(key_state, buttons, items, target)`: Alt only = yes; Alt+Ctrl, Alt+Shift, no Alt, right button = no; AltGr
   (fake LCtrl 0x21D + RAlt) = yes; real Ctrl + Alt = no; Alt released before drop = no; target over desktop folder icon / item / Gate view = no.
9. Items: a zip, a .lnk, a library, a missing path, a drive root, a UNC dir and a junction classified as above; none: ordinary; 1 folder +
   3 files: one Gate; 6 folders: six; 7 folders: refused with `DROPEFFECT_NONE`; duplicates collapse; classifier not finished: ordinary.
10. Effect: `DragOver`/`Drop` return `DROPEFFECT_LINK` on accept, `NONE` on refuse, never `COPY` or `MOVE`; no file API is called with a
    write flag (fake file layer asserts zero writes).
11. `quick_gate_rects`: size equals `body_size_for(5, 4)` at every DPI (body 391 x 368 px at 96 dpi, 490 x 460 at 120 dpi: cells 94 x 110, pad 10);
    clamped on a 640 x 360 DIP work area; pointer lands on the title strip; snapping and `place_on_screen` applied; tiles wrap; every rect
    inside its work area, also at a negative origin and at the bottom-right corner; the total is never over 6.
12. Live checklist: no Explorer rubber band while drawing; right-drag leaves no stray context menu; Alt-drop from Explorer, from a Corral
    and from a network share; preview crisp on a mixed-DPI pair; chooser keyboard use; Alt-drop of a dead network folder does not hang.

## 9. Open questions
1. Does a plain left drag losing rubber-band selection (when `CreateCorralMode` is 0 or 1) suit the owner, or should drawing need a modifier?
2. Which mechanism lets the engine own presses on the desktop list view, hide Explorer's own selection rectangle and swallow the right-button menu?
3. Which mechanism delivers Alt-drops over empty desktop (Explorer owns that drop target), and does returning `DROPEFFECT_LINK` there suppress Explorer's shortcut creation?
4. Are 5 x 4 cells, pointer-on-title placement and a cap of 6 Gates per drop (refusal beyond it) right?
5. Should a shortcut to a folder (.lnk) count as that folder for Alt-drop?
6. Should shell-only folders (This PC, Libraries, Recycle Bin) be allowed as Gate roots? Stored keys are paths, so they are excluded here.
7. Is the AltGr rule of the Glance spec right here, and is `MK_ALT` set for Right Alt on the user's layouts?
8. Is a keyboard or tray route for creating a Corral wanted (accessibility), since drawing needs a mouse?
9. Should a new Corral start in rename mode, and what is its default title?
10. Should a created Corral avoid overlapping existing Corrals? Is there a better public signal than source attributes for slow shares?
