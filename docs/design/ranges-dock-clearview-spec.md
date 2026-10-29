# Ranges gestures, Fold docking and Clear view rules (design spec, area D4)

Status: approved by the user 2026-10-22 (open questions resolved as the spec recommends, except as amended below)
Date: 2026-10-22
Author: Claude subagent (clean spec writer), inputs: spec-inputs-wave-d-2026-10-22.md + approved Corral design docs + public docs only

Terms: "Corral" includes Folder Gates and tab groups (one window). DIP = 1/96 inch; `px(d) = floor(d * dpi / 96 + 0.5)` at the DPI of the monitor
the pointer is on. "Free edge" = a left or right monitor edge with no other monitor touching that part of it (edges between monitors never
count). "Desktop surface" = empty desktop, a loose icon or any part of a Corral. Every number is a proposal with a one-line reason.

## 1. Goal and non-goals

Goal: three small rule sets that are predictable, cannot be triggered by ordinary desktop use, never trap or lose anything and are pure enough
to test without a window: how Ranges are switched, filled and cleaned up; when a drop docks a Corral and how a folded one stays open; when
Clear view hides and shows and what it never hides.

Non-goals: slide/bounce, fold and fade timing (motion-sorting-spec 3, 4, 5); snapping and the title strip (grid-snapping-spec); Windows virtual
desktops (untouched); moving or deleting files; new settings; interactive drag-following swipes; making loose desktop icons belong to a Range.

## 2. Input model and DPI

The recogniser is a pure state machine fed by the engine; it never reads the OS. Pointer event `{t_ms, pos_px, buttons, mods, target}` with
`target` in `EmptyDesktop | LooseIcon | Corral | Other` (any other window, taskbar, menu). Wheel event `{t_ms, pos_px, mods, dx, dy}` in wheel
units (120 = one notch; finer values accumulate). Key event `{t_ms, vk, down, mods, repeat}`. Verdict per event: `Pass` or `Swallow`.
- Invariant (as peek-hotkeys-spec 4): a swallowed down has its repeats and up swallowed; a passed down has its up passed.
- Distances are DIPs converted with the DPI of the monitor under the press point and fixed for that gesture; times never scale. Edge zones are
  `px(zone)` from the monitor's outer edge, so mixed-DPI monitors feel alike.
- Common gate for every Ranges and Clear view gesture: Corral on, session unlocked, no full-screen or presentation app foreground
  (`SHQueryUserNotificationState`, as peek-hotkeys-spec 7), no menu in menu mode, no modal Corral dialog, per-gesture setting on. Gate closed:
  every event passes and recogniser state resets.

## 3. Ranges

### 3.1 Model and which items move

Ranges are numbered 0..n-1 for the whole desktop (all monitors switch together, motion-sorting-spec 5). A Corral has one Range number (absent
= 0) or is pinned by "Show on every Range". On a switch a to b: Corrals numbered a slide out, those numbered b slide in, pinned ones stay. A tab
group is one item (all tabs share the number). Fold and dock state are kept. Loose desktop icons belong to Explorer and appear on every Range
(paging them would rewrite positions); to put an icon on a Range, drop it into a Corral of that Range. Carrying a Corral changes only its Range
number, never its rect. The app always starts on Range 0 (predictable, no state). Switching writes no positions. Limit `MAX_RANGES = 8`
(enough for themes; a held drag cannot create Ranges endlessly).

### 3.2 Constants

| Name | Value | Reason |
|---|---|---|
| `PULL_ZONE` | 6 DIP | Hit by slamming the pointer into the edge, too narrow to steal real work. |
| `PAN_T` | `clamp(0.10 x monitor width DIP, 120, 320)` | A deliberate stroke is a tenth of the screen; floor for small screens, cap for ultrawide. |
| `DOM` | travel >= 2 x vertical travel | Clearly sideways: diagonals and shaky drift never fire. |
| `TP_UNITS`, `TP_QUIET` | 360 units (3 notches); 200 ms reset, 300 ms lockout | A short deliberate swipe; lockout swallows touchpad inertia, so one swipe is one switch. |
| `KEY_MIN` | 150 ms between key or wheel steps | Fast repeat, but slower than a retargeted 240 ms slide can follow. |
| `CARRY_ZONE` | 24 DIP | A dragged pointer is less exact; the screen edge pins it, so it cannot jitter out. |
| `CARRY_DWELL`, `CARRY_REPEAT`, `ADD_DWELL` | 600, 900, 1000 ms | Passing by does not trigger; repeats are slower so the user can stop; adding commits most. |
| `DOTS_MS` | 1200 ms | Long enough to read the indicator (3.7). |

### 3.3 Switching methods

All methods: a higher number is to the right; no wrap-around (Alt+Right on the last Range or a pull toward a missing Range does nothing);
a switch to the same Range is dropped.
1. Edge pull (`SwipeDragEdge`). Left button down, `target = EmptyDesktop`, press inside `PULL_ZONE` of a free left or right edge. Press, motion and
   release are swallowed (the strip is reserved while on, so no marquee or Corral rectangle starts there; both can start 7 DIP in). With `d` =
   inward horizontal travel and `dy` vertical travel, fire when `d >= PAN_T` and `d >= 2|dy|`: the left edge gives the previous Range, the right
   edge the next (pulling a page toward you shows what is behind it). One switch per press, no timeout, nothing cancels it.
2. Two-finger swipe (`SwipeTouch`). Precision touchpads send two-finger pans as public `WM_MOUSEHWHEEL` / `WM_MOUSEWHEEL`. With no modifier and a
   desktop surface under the pointer, accumulate `h` and `v` (reset after `TP_QUIET` of silence); fire when `|h| >= TP_UNITS` and `|h| >= 2|v|`:
   positive `h` (Windows' horizontal scroll sign, so it follows the user's scroll-direction choice) is next, negative previous. After a fire, ignore
   wheel input until 300 ms of silence. Observed only, never swallowed. A touch-screen two-finger pan feeds the same machine with centroid travel
   in DIPs (`PAN_T`, `DOM`, one switch per pan) if the engine can see it (question 5).
3. Alt+Left / Alt+Right (`SwipeKeyboardWheel`). Exactly Alt held; foreground is the shell desktop or a Corral; no rename box or text edit has focus.
   Down fires (previous / next); key, repeats and up are swallowed; repeats limited to `KEY_MIN`. If live tests show Alt alone then cues a menu,
   reuse the mask-key tap of peek-hotkeys-spec 4 (question 7).
4. Alt+wheel (same setting). Exactly Alt held, pointer over a desktop surface (the wheel follows the pointer, no foreground rule). Accumulate
   `dy`; each 120 units is one step, upward previous, downward next (down advances a page, as in lists), at most one per `KEY_MIN`. Swallow it;
   with the gate closed, pass it (Alt+wheel in other programs is theirs).
5. Middle drag (`SwipeMiddleDrag`). Middle down on a desktop surface; the press passes (it does nothing there). Fire when `|dx| >= PAN_T` and
   `|dx| >= 2|dy|` from the press: dragging right pulls the desktop right, so previous; left, next. After a fire the anchor moves to the current
   x, so a long stroke passes several Ranges and a reversal goes back. A click under the system drag distance does nothing.
6. Both-buttons drag (`SwipeDualDrag`). Left and right both down (either order) on a desktop surface. The second button's down and up are
   swallowed, the first passes. Measured like the middle drag from the moment the second button goes down. Drawing a Corral needs exactly one
   button, so the second cancels a rectangle in progress. A chord must not leave a context menu: after a fired chord the engine closes any shell
   context menu that opens within 250 ms (question 8). A chord that fired changes nothing else.

### 3.4 Carrying an item and adding a Range

Active during any drag of a Corral (by its title) or of icons, needing only `SwipeEnabled` (not `SwipeDragEdge`: carrying is how Ranges get filled).
The engine polls the pointer every 50 ms during a drag. Zone = `CARRY_ZONE` along a free left or right edge.
- Carry: pointer continuously in the zone for `CARRY_DWELL` and a neighbour Range exists that way: the view switches (normal transition) and the drag
  continues. The next carry needs `CARRY_REPEAT` still in the zone (or leave, re-enter, wait `CARRY_DWELL`). Leaving the zone resets the dwell.
- Add: only a dragged Corral, on the last Range at its right edge, below `MAX_RANGES`, after `ADD_DWELL`: a new empty Range is appended and
  shown. A dragged icon never adds one (an empty Range could not take it); the left edge never adds. Progress is a 4 DIP accent bar along the
  edge growing over the dwell; for adding it ends in a "+" chip at the pointer's y.
- What moves: a dragged Corral takes the number of the Range it is dropped on; all selected icons travel together under the usual drop rules
  (join a Corral of the shown Range; on empty desktop the existing desktop rule); nothing is copied.
- Provisional: until the drop nothing is stored. Esc or a refused drop returns the view to the starting Range and the Corral to its start rect;
  an added Range is then empty and removed by 3.5. The Range holding the in-flight Corral counts as non-empty.

### 3.5 Removing empty Ranges

A Range is empty when no Corral has its number (pinned ones do not count; the in-flight payload does). After a transition settles and after any
drop, delete or move that empties one: delete every empty Range except the current one; leaving the current empty Range deletes it. Range 0 is
kept only if it would leave no Range. Deleting Range k decrements every stored number above k and the current index in one step, and remaps the
transition position `s` the same way, so nothing jumps. The shown Range is never deleted under the user. Only numbers change, never Corrals,
positions or files.

### 3.6 Settings and indicator

`SwipeEnabled` off: no recogniser, no carry, nothing renumbered. `SwipeShowIcons` on: any switch first ends Clear view (5.4), so its fade-in
runs with the slide; off: Corrals stay hidden through the switch. A new Corral gets the shown Range's number. After any switch, add or removal a
pill with one dot per Range (current one accent) shows near the bottom centre of the primary work area for `DOTS_MS`: 10 DIP dots, 8 DIP apart,
`bg/surface` with a 1 DIP `border/default` line, click-through, fading with the Clear view curves (instant under reduced motion). It is no
setting; it keeps anyone from getting lost on identical-looking Ranges (question 1).

## 4. Fold docking

`gap_top = outer.top - work.top`, `gap_bottom = work.bottom - outer.bottom` in DIPs (monitor work area; the folded rect when folded, grid spec 4.1).
Flags are the stored `dock_top` / `dock_bottom`; a user drop sets at most one.
- Docks, at the end of a title drag: `gap <= DOCK_IN = 8 DIP`, Shift not held. Both qualify: the smaller, ties to the top. The rect is moved flush
  even with `SnapEnabled` off. 8 equals the snap capture distance, so a snapped drop (gap 0) always docks and the rules agree; Shift means "skip
  snapping" and also skips docking. While a drop would dock, the 2 DIP alignment guide is drawn along that edge across the Corral's width.
- Applies to Corrals, Gates and tab groups whatever `RollupEnabled` says (the flag also drives snapping and the title side). With `RollupEnabled`
  and `RollupOnDock` on, the derived fold of motion-sorting-spec 3 follows from the flag.
- Undocks: a docked Corral sticks while the raw outer edge is within `DOCK_OUT = 12 DIP` of the edge (hysteresis like snapping's release distance);
  sliding along the edge keeps it docked. A drop with a larger gap clears the flag; the derived fold ends by itself and the Corral opens with the
  open animation. Dropping on the opposite edge docks there. Resizing never changes the flag.
- Follows the work area: the flag, not the old y, is the truth. A work-area change (taskbar moved, auto-hide, DPI, monitor) re-flushes a docked
  Corral; that is recomputed, not stored (`LayoutPreserve = 0` still moves nothing on load).
- Keep-open area: a hover-opened Corral closes 300 ms after the pointer leaves it (motion-sorting-spec 3 owns the 300 ms and its three
  exclusions). Area = the target expanded outer rect, clamped to the work area as drawn, grown by `KEEP_MARGIN = 12 DIP` on every side except
  along a docked edge, where the screen edge ends it. Reason: reaching a scrollbar or a tab, or overshooting the border, should not close it;
  12 DIP is about 3 mm. Points inside another Corral's outer rect are never in the margin (that Corral gets the pointer). The margin never opens
  a folded Corral (opening uses the folded rect only). The strip belongs to the expanded rect, so moving into the body never leaves the area.
- Nearly zero body: the expanded display body height is `max(stored body height, MIN_OPEN_BODY = 48 DIP)` (grid spec 3.3's minimum), for temporary
  and persistent unfold alike. The lifted size is drawn and hit-tested only; the stored rect and `preferred_size_dip` stay untouched until the
  user resizes. Reason: an open that reveals a few DIPs flickers and is unusable. Larger bodies are unchanged.

## 5. Clear view

### 5.1 State and hide set

State: `Shown`, `Hidden(Manual)`, `Hidden(Idle)`, plus the overlays `drop_peek` (5.5) and Glance (question 9). Hide set by `QuickHideWhat`:
0 Corrals and loose icons, 1 loose icons, 2 Corrals. It is evaluated live per item, so something that appears while hidden is hidden at once. Never
in the set: Corrals marked Keep visible (a tab group is kept when any member tab is marked; the menu command View > Keep visible in Clear view
sets or clears all members and shows checked when any is set); loose items on the list of 5.6; everything while `QuickHideEnabled` is off (the
state resets to `Shown` with a fade-in; `QuickHideWhat` stays editable). Hiding is the fade of motion-sorting-spec 4; it moves, resizes and deletes
nothing and writes no layout.

### 5.2 Double-click toggle

All must hold: `QuickHideEnabled` on; common gate (section 2); both clicks left button, no Ctrl, Shift, Alt or Win; both presses and releases on
`EmptyDesktop`; the second press within `GetDoubleClickTime()` and the system double-click rectangle of the first; less than the drag distance of
movement during each click (a marquee or Corral rectangle is not a click); no drag in progress. A double-click on an icon, a Corral (including the
empty space of a Gate, which opens Explorer), a window or the taskbar never counts. Then `Shown -> Hidden(Manual)` and `Hidden(any) -> Shown`.
Three quick clicks toggle once. A toggle during a fade retargets (motion spec).

### 5.3 Idle auto-hide (`AutoHideIdle`, `AutoHideIdleChoice`)

Idle time = time since the last keyboard or mouse input anywhere (`GetLastInputInfo`, plus touch and pen the engine sees). Threshold = the choice
in ms; the stored 1 ("Immediately") is read as `IDLE_MIN = 500 ms`, a pause the eye notices, so the desktop is not hidden between two mouse
nudges; other stored values as they are, clamped to 500..3,600,000. At the threshold, `Shown` becomes `Hidden(Idle)`. Input never hides and never
shows by itself. The timer is suspended (restarting at the next input) during full-screen or presentation use, a locked session, a visible Glance,
an open Corral menu or dialog, and any drag; resume and unlock count as activity. `Hidden(Manual)` is never changed by idle.

### 5.4 Showing again

- A double-click (5.2) always shows.
- `AutoShow`: only from `Hidden(Idle)`: one left click on the empty desktop, or the desktop becoming the foreground window (Win+D, taskbar corner).
  Never from `Hidden(Manual)`, because the first click of a double-click would undo its own hide. If that click opened a pair, the second click is
  dropped from the double-click detector. With `AutoShow` off, `Hidden(Idle)` is left only by the double-click.
- Creating a Corral or Gate (drawing, Alt-drop) and a Range switch with `SwipeShowIcons` end any hidden state first; `QuickHideEnabled` off or Corral
  off ends it at once.

### 5.5 Show while dropping (`AutoShowOnDrag`, effective only with `AutoShow`)

While a file drag is in progress and the pointer is over a desktop surface, a hidden state shows (`drop_peek`, normal fade-in) so the user sees the
landing place. Leaving the desktop mid-drag does not hide again (no flicker). 800 ms after the drag ends (drop or cancel) the hide set fades out
again unless a click, toggle or new drag intervened. The underlying state is unchanged by the peek. A drag the engine cannot detect is question 10.

### 5.6 Default never-hide list (loose items)

Recycle Bin, This PC and the user's profile folder: the three icons of a fresh Windows desktop, anchors the user reaches for while the rest is
hidden (the bin is also a drop target), not clutter. Matched by shell identity (parsing names `::{645FF040-5081-101B-9F08-00AA002F954E}`,
`::{20D04FE0-3AEA-1069-A2D8-08002B30309D}`, `::{59031A47-3F72-44A7-89C5-5595FE6B30EE}`), never by display name, so a renamed icon stays exempt and a
file called "Recycle Bin" does not. Fixed list, no UI (question 11). A copy held inside a Corral hides with its Corral.

### 5.7 Start-up and crashes

One persisted boolean `clear_view_hidden` in the runtime state record (not settings, not layout; question 12), written when `Hidden(Manual)` is
entered or left. `Hidden(Idle)` and peeks are never persisted. At start (no fade, motion-sorting-spec 1) the state is `Shown` if `AutoShowOnBoot`
is on, or idle hiding is on (the timer will hide soon; this is why that row is disabled, and its stored value is not touched), or
`clear_view_hidden` is false, or `QuickHideEnabled` is off; otherwise `Hidden(Manual)` is restored. The first start-up action makes loose icons
visible unless the restored state hides them, so a crashed run never leaves them hidden. Exit and crash handling restore icon visibility as the
project rule demands; the persisted value is kept on exit as the user's last choice.

## 6. Stored keys

Per-Corral Range number, "Show on every Range" and Keep visible use keys already stored (names: question 2). No key is renamed or removed; the
5.7 boolean is the only new stored value; unreadable values never block loading.

| Key | Use | Absent means (fresh install only; present values always win) |
|---|---|---|
| `SwipeEnabled` | master for section 3, carry and add | off (it takes Alt+wheel, edges, middle drag: opt in) |
| `SwipeDragEdge`, `SwipeTouch`, `SwipeKeyboardWheel`, `SwipeMiddleDrag` | methods 1, 2, 3+4, 5 | on |
| `SwipeDualDrag` | method 6 | off (easy to hit by accident) |
| `swipe_animation_style`, `SwipeShowIcons` | motion spec 5; 3.6 | 1 Slide; off |
| `RollupEnabled`, `RollupOnDock`, `dock_top`, `dock_bottom`, roll, `Rollup*` | section 4, motion spec 3 | as that spec |
| `QuickHideEnabled`, `QuickHideWhat` | 5.1, 5.2 | on; 0 |
| `AutoHideIdle`, `AutoHideIdleChoice` | 5.3 | off; 30000 |
| `AutoShow`, `AutoShowOnDrag`, `AutoShowOnBoot` | 5.4, 5.5, 5.7 | on, on, on |

## 7. Acceptance tests (pure; fake clock, fake monitors, literal event lists)

1. Edge pull: on a 1200 DIP monitor (`PAN_T` 120) a left-edge press at x 0..5 DIP dragged 130 right with dy 40 fires previous; dy 80 does not; a
   press at 7 DIP or inside a shared edge is `Pass` and ignored; press, motion, release are `Swallow`. `PAN_T` for 600/1920/3840/7680 DIP wide =
   120/192/320/320; zones and distances equal `px(d)` at 96/120/144/192/240 dpi; a press on a 100 % monitor keeps its px after entering a 250 % one.
2. Touchpad: 359 units no fire, 360 fires; sign gives next/previous; `h` 400 with `v` 250 no fire; 200 ms silence resets; inertia inside 300 ms is
   ignored; any modifier disables; never swallowed.
3. Alt+arrows: gate (desktop or Corral foreground, Alt exactly, no text edit); Right on the last Range does nothing; repeats limited to `KEY_MIN`;
   up and repeats swallowed iff the down was; Ctrl+Alt+Right passes. Alt+wheel: 119+1 units steps once, 15-unit events accumulate, direction as
   in 3.3.4, Alt off or gate closed passes.
4. Middle/dual: fire at `PAN_T`, chained steps at 2 x `PAN_T`, reversal steps back, a short click does nothing; dual: either order, second button
   swallowed with its up, first passes with its up, a rectangle in progress is cancelled, left alone never fires.
5. Carry: 599 ms none, 600 ms switches, repeat needs 900, leaving at 599 resets; an icon drag at the last Range does not add; a Corral drag adds at
   1000 ms only at the right edge of the last Range and never beyond `MAX_RANGES`; Esc restores the Range and rect and the added Range goes;
   pinned stays pinned; nothing is stored before the drop.
6. Removal: an empty middle Range goes after leaving it, higher numbers drop by one, `s` remaps without a jump; the current and the last
   remaining Range are never removed; a pinned-only Range counts as empty; the in-flight drag keeps its Range. `SwipeEnabled` off passes
   everything and renumbers nothing. `SwipeShowIcons` ends Clear view once per switch.
7. Docking: gap 8.0 DIP docks, 8.1 does not; both edges: smaller gap, tie top; Shift never docks; `SnapEnabled` off still docks flush; sticks to 12.0
   and releases at 12.1; opposite edge re-docks; derived fold follows the flag; work-area change re-flushes without a stored move; both flags
   loaded from a file are left as read.
8. Keep-open: margin points keep open, 12.1 DIP out closes; no margin along the docked edge; a point inside another Corral is outside; the margin
   never opens; body 10 DIP displays 48 DIP with the stored rect byte-identical after open/close; body 80 DIP unchanged.
9. Toggle gating: each failing condition of 5.2 (button, modifier, target, time, rectangle, movement, drag, menu, dialog, full-screen, disabled)
   gives no toggle; three quick clicks toggle once; the same clicks on an icon do nothing.
10. Idle: threshold per choice, 1 acts as 500 ms, input resets and never shows, suspension cases restart afterwards, `Hidden(Manual)` unaffected,
    unknown stored values clamp. Show rules: `AutoShow` shows `Hidden(Idle)` on one click and never `Hidden(Manual)`; the paired second click cannot
    re-hide; creation, Range switch (option on) and master off end hiding. Drop peek: shows on a drag over a desktop surface, stays while the pointer
    leaves, hides 800 ms after the end, cancelled by a click or new drag, inert without `AutoShow`, underlying state unchanged.
11. Hide set: modes 0/1/2 select the right sets; Keep visible on any tab exempts the group; the three identities are exempt, a file named "Recycle
    Bin" is not, a renamed This PC stays exempt; items added while hidden are hidden.
12. Start-up: the full table of `AutoShowOnBoot` x idle x `clear_view_hidden` x `QuickHideEnabled`; `Hidden(Idle)` never persisted; stale hidden
    loose icons with state `Shown` become visible first.
Live checklist: touchpad inertia, Alt+wheel in a browser (untouched), both-buttons context menu, edge slam on two monitors, hover-open near
screen edges, a browser drop while hidden, Win+D with idle hiding, live-wallpaper host windows classified as `Other`.

## 8. Open questions
1. Keep the position-dots pill (3.6)? Recommendation: keep.
2. Lead: stored key names for a Corral's Range number, "Show on every Range" and Keep visible; where the Range count lives.
3. Should saved desk setups carry Range numbers? Recommendation: no (a property of the Corral, not of a monitor setup).
4. Adding Ranges on the left, wrapping around, or paging loose icons? Recommendation: none of them (renumbering, surprise, rewritten positions).
5. Can the engine observe a touch-screen two-finger pan on the desktop? If not, drop that line of 3.3.2.
6. Is `AutoHideIdleChoice` stored in milliseconds (assumed from the redesign table)?
7. Does Alt-tap menu activation matter after a swallowed Alt+arrow on the desktop? Test live.
8. How to close the context menu after a right-first chord (close it afterwards, or hold the right button's down)?
9. Glance while Clear view is hidden: recommendation, Glance wins (its Corrals show while open, then hide again).
10. How does the engine detect a foreign drag over a hidden desktop (hidden Corral windows are not drop targets)? Needs the lead's design.
11. Make the never-hide list editable? Recommendation: no.
12. Where does the runtime state record live (beside the layout, outside backups)?
13. Confirm the fresh-install defaults of section 6 (Ranges master off, Clear view master on).
14. If the desktop-icon hiding mechanism cannot hide single loose icons, the never-hide list cannot be honoured; the lead must say what it can do.

## Amendment (2026-10-22, user decision)

Loose desktop icons keep their own Range, as Corral already stores it per icon; a loose icon dragged to a screen edge and held is carried to the next Range like a Corral. No Range-position dots (question 1).
