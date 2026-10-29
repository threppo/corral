# Tabs: stacking Corrals in one place (design spec, D1)

Status: approved by the user 2026-10-22 (open questions resolved as the spec recommends, except as amended below)
Date: 2026-10-22
Author: Claude subagent (clean spec writer), inputs: spec-inputs-wave-d-2026-10-22.md + approved Corral design docs + public docs only

Terms: "Corral" includes Folder Gates. A **tab set** is two or more Corrals that share one rectangle; each member is a **tab**.
DIP = 1/96 inch; `px(d) = floor(d * dpi / 96 + 0.5)` at the DPI of the monitor that governs the set (grid-snapping-spec).
"Strip" = the title strip (corral-painting-spec 2.2), which holds the tab strip (corral-painting-spec 7). Every number is a
proposal for review, with its reason. Pointer positions are physical pixels (per-monitor-aware v2).

## 1. Goal and non-goals

Goal: one pure, testable model of how Corrals join a set, switch, reorder, leave and merge, where a Corral and its items can
never be lost, and where stored data is read exactly as written.

Non-goals:
- Tab strip drawing and cell layout (corral-painting-spec 7, 2.6). This spec only consumes `tab_layout`.
- New settings, new stored keys or new settings UI. `TabsStyle`, `TabsCreate`, `TabsConfirm`, `TabsMouseover`,
  `TabsMouseoverDelay`, `TabsColors` keep their approved pages and meanings.
- Nested sets, sets spanning monitors or Ranges, tab previews, tab drag between windows of other applications.
- Wheel or keyboard tab switching (a Corral has no keyboard focus; the Tabs submenu covers non-pointer use).
- Item handling. Items belong to a Corral id; this spec never touches them.

## 2. Model

### 2.1 Stored facts (logical names; the parser keeps the stored key names and types)

Per Corral: `tab_set` (set id, or none), `tab_index`, `tab_selected`, `keep_visible_in_clear_view` (the per-tab Clear view
exclusion). Per layout: `next_tab_set` (counter for new set ids). Nothing else is added.

### 2.2 In-memory model, built once from the stored values

`TabSet { id, tabs: Vec<CorralId> in display order, selected: usize }`. A set always has 2+ tabs; a Corral is in at most one set.
Normalisation on read is in memory only (section 8): sort by `(tab_index, file order)`, pick the selected tab, drop one-member
sets to plain Corrals. Display order is the vector order; `tab_index` is written back as 0..n-1 only when a set is next modified.

### 2.3 Shared by the set (one value for all tabs)

Outer rect and `preferred_size_dip`, monitor, Range, roll (Fold) state, dock flags, title side. Reason:
the set is one window; two rectangles could disagree. The live value is the **selected tab's stored value** (fallback: lowest
index). Whenever the set's geometry changes, the same value is written to every member in the same save, so a hand-edited or
older file with mismatching members heals on its next geometry change and never at load.

### 2.4 Kept per tab (travels with the Corral id)

Title, items and their cells, sort mode, Round-up destination, Gate folder and view, icon zoom, scroll offset (in memory only;
scrollbar-spec question 5), and all colour state (colour-state-spec 2.3: the body paints with the selected tab's look; with
`TabsColors` each tab header uses its own). Joining, leaving or reordering changes none of these.

### 2.5 Clear view exclusion

Each tab keeps its own flag. The set stays visible in Clear view when **any** tab is flagged. Reason: switching to an unflagged
tab must not make the Corral fade away under the pointer. The menu command "Keep visible in Clear view" on a set shows checked
when the set is kept and writes the chosen value to **all** tabs, so the check mark never lies. Leaving a set keeps the tab's own flag.

### 2.6 Set id allocation

`new_id = max(next_tab_set, 1 + max id of any set in the layout)`; then `next_tab_set = new_id + 1`. Ids are never reused within a
layout, even after a set dissolves, so a stale reference cannot attach to a new set. Merging keeps the target's id and retires
the source's. The counter is written in the same save as the membership that used it, and never decreases.

## 3. Joining

### 3.1 Drop target zones

During a Corral move drag (title drag, also a tab dragged out, section 5), the candidate target is the visible Corral whose
**strip** (full strip rect including buttons and resize bands) contains the pointer; the dragged Corral's own rect is irrelevant.
The pointer is tested, not the dragged window, because the user aims with the pointer. Only topmost-first (z-order) Corral counts.
A target must be: on the same Range as the pointer sees (hidden Corrals are not visible, so never targets), not the source itself,
not a tab of the source's own set, and have its title shown (`LabelMode` Always, or On hover where the reserved strip counts).
With `LabelMode` Never there is no zone: the drop is a plain move.

On a set target with a visible strip, the **insertion index** is chosen by the pointer x: before tab `i` when x is left of tab
`i`'s centre, else after it. A plain target, a set in style 0 (Menu only), or a pointer on empty strip area gives "append".

### 3.2 Arming and feedback

- The zone **arms** after the pointer has stayed in it for 120 ms. Reason: sweeping across a 24 DIP strip at normal speed takes
  well under that, so passing over titles never prompts; a deliberate stop is longer. Leaving the zone disarms at once.
- Armed feedback, drawn by the target's window at its DPI: 2 DIP accent outline around the strip (radius as the strip), accent plate
  at 18 % alpha inside; on a set also a 2 DIP accent insertion mark (tab height inset 4 DIP) at the insertion index. The dragged
  window drops to 70 % opacity and a size-tooltip-style label (grid-spec 7) near the pointer says "Combine as tabs" (or
  "Add to tabs" for a set, "Merge tabs" for set onto set). Release while armed combines; release unarmed or Esc is a plain
  move or cancel. High contrast: the accent colours become `COLOR_HIGHLIGHT`.
- No jump on arm: nothing but the overlay changes. Reduced motion: the overlay appears without fade.

### 3.3 Refusals (the zone simply never arms, with no message)

`TabsCreate` off (all joining by drag and by menu, including into existing sets; leaving, reordering, switching and separating
always work). Target folded (persistent or dock-derived; a closed strip cannot show what joined it). Target title Never. Source is a
tab of the target. Allowed on purpose: different monitors (the source takes the target's monitor and DPI; the dropped Corral's old
rect is simply replaced), a folded source (its fold flag is dropped, the set's state governs), Folder Gates in either role
(each tab keeps its folder; Up and navigation buttons belong to the selected tab).

### 3.4 Confirmation

`TabsConfirm` on: on release the "Combine into tabs?" dialog appears (text and buttons from settings-redesign-spec; "Don't ask
again" turns `TabsConfirm` off). For a set, the sentence becomes "\"<A>\" (3 tabs) and \"<B>\" will share one place, as 5 tabs. Hold
Shift and drag a tab away to separate them again." with `<A>`/`<B>` the selected tabs' titles. Enter = Combine, Esc = Cancel.
- The data is untouched until Combine is pressed; then the pure join is re-validated against the live layout (the target may be
  gone) and silently abandoned if invalid.
- Cancel (or Esc) restores the whole gesture: the dragged Corral glides back (180 ms OUT, 0 with reduced motion) to the rect it had
  when the drag began. Reason: a drop on a title was a combine attempt; a cancelled attempt leaves nothing changed.
- `TabsConfirm` off: combine on release.

### 3.5 Result of a join

Source plain onto target plain: new set `[target, source]`, source selected. Onto a set: the source (or all tabs of a source
set, in their order) inserted at the insertion index; the dropped selected tab becomes the set's selected tab. The target's place always
wins: the set keeps the target's rect, monitor, Range, roll and dock state; the source window fades out over 140 ms (IN, 0 reduced
motion) and the strip re-lays out with a 120 ms OUT glide. Reason: the user sees what they just dropped, and the stationary
Corral does not move.

## 4. Switching

- **Click**: left press on an inactive tab and release on the same tab, before the drag threshold (5.1), selects it. Selecting
  is instant: the body shows the new tab's icons at their cells with no glide (first-paint rule of motion-sorting-spec 6).
  The selection is saved with the normal debounced layout save, all members in one snapshot.
- **Hit boxes** come from `tab_layout`, clipped by the resize bands of grid-snapping-spec 3.1 (the outer 8 DIP of the strip is
  not a tab). A plain left drag on a tab moves the whole set like any title; only Shift starts tab reorder (section 5).
- **Hover switching** (`TabsMouseover` on): the pointer *entering* an inactive tab starts a timer; it selects when the timer
  ends. `TabsMouseoverDelay` on: 300 ms; off: at once. Reason for 300: a pass over a tab lasts about 100-150 ms, a deliberate
  stop longer; the same value as the Fold leave grace. The timer is cancelled by leaving the tab, any button press, a menu or
  tooltip opening, and a Corral move or tab drag. A tab under a stationary pointer after any other switch never re-arms; the pointer must
  leave and re-enter. Hover switching ignores a set that is faded by Clear view.
- **Spring while dragging icons**: an icon (file) drag held over an inactive tab selects it after 600 ms even if `TabsMouseover`
  is off, so an item can be dropped into another tab (question 4). Reason: otherwise the drop is impossible, and 600 ms is slow
  enough never to fire by accident.
- Style 0 (Menu only) has no strip: switching is only via the Tabs submenu (section 6).
- A trimmed tab title shows its full text as a tooltip after the system hover time.

## 5. Reordering and tearing off

### 5.1 Starting

Shift held at left-press on a tab, then movement beyond the drag threshold: `max(GetSystemMetricsForDpi(SM_CXDRAG/SM_CYDRAG,
dpi) / 2, px(4))` in each axis around the press point (the system value is the full rectangle; half is the radius, so the
threshold equals the system's own). Shift is read only at the press; releasing it later does not end the gesture, because a
mode flip mid-drag is confusing. Below the threshold the press is a click (4).

### 5.2 Reorder

While the pointer stays within 32 DIP of the strip rect (5.3), the dragged tab follows the pointer x, clamped to the strip.
The target index is the slot whose centre the dragged tab's centre has most recently crossed; neighbours glide 120 ms OUT
(reason: shorter than an icon glide, strip changes are small), 0 with reduced motion. Release commits that order; Esc restores.
When the strip scrolls (tabs at minimum width), holding the pointer within 24 DIP of either strip end scrolls one tab per
200 ms. Nothing changes in the body during reorder (the selected tab is unchanged).

### 5.3 Tear-off

When the pointer's distance from the strip rect exceeds **32 DIP** (Euclidean, measured outside the rect), the tab tears off.
Reason: more than the strip height (24 DIP), so a sloppy horizontal reorder with 10-20 DIP of vertical drift never tears, yet
the user does not have to leave the Corral's whole body.
- A drag that is only a gesture: the set is not changed until release. At tear the tab becomes a ghost Corral window with the
  set's body size, positioned so the pointer keeps the same offset inside its title strip as it had inside the tab (x offset
  clamped to the strip width); it then follows the pointer as an ordinary Corral move drag, including snapping,
  Shift-to-raw and keep-on-screen on release. It lands unfolded and undocked whatever the set's state.
- Once torn, the user can still drop it on any Corral's strip (section 3): the net result is a move between sets. The same
  confirmation rule applies; Cancel restores the original set and index (the transaction is the whole gesture).
- Drop on empty desktop: the Corral keeps its items, title and colours (2.4), gets the new position and stores its own rect;
  `preferred_size_dip` is the set's.
- The set after tear: remaining tabs keep their order; if the torn tab was selected, the tab now at the same index (else the
  previous one) is selected; with one tab left the set dissolves (its membership fields are cleared, the Corral stays where it is).
- Esc at any point cancels the whole gesture and puts the tab back at its original index.

## 6. Merging sets and the Tabs submenu

Merging is the join of 3.5 with a source set; the source's tabs keep their relative order; the source set id is retired.

The Corral menu (and the menu button) has **Tabs** submenu. Plain Corral: only "Combine with". Set: all commands.

| Label | Effect | Enabled when |
|---|---|---|
| Show tab ▸ (list of tab titles, checked = selected) | select that tab | set; also the only way for style 0 |
| Move tab left / Move tab right | reorder the selected tab by one | set, not at that end |
| Remove from tabs | the selected tab leaves; new Corral at the set's rect offset by +24 DIP right and down (clamped on screen), unfolded, undocked | set |
| Separate all tabs | every tab becomes its own Corral; the selected tab keeps the set's place, the others cascade +24 DIP per tab in display order | set |
| Combine with ▸ (up to 24 other Corrals and sets on this Range and monitor, visible, not folded, ordered by title) | join / merge as in 3.5; confirmation per 3.4; "Merge tabs" for set onto set | `TabsCreate` on, at least one candidate |

Each of these is one atomic edit (7). "Remove from tabs" and "Separate all tabs" use the same code as tear-off. Labels in the
menu list use the tab title, trimmed to 40 characters.

## 7. Data safety

- Invariant after every operation (a pure `validate(layout)` run before saving): each Corral id appears exactly once in the
  layout; each set has 2+ tabs, contiguous indices 0..n-1 and exactly one selected tab; `next_tab_set` is greater than every set
  id; the multiset of (Corral id, its items, title, colour slots, Gate folder) is identical to the input. An operation that
  fails validation is refused as a whole and the live state stays as it was.
- Operations never delete or rewrite a Corral or an item. A set that shrinks to one tab dissolves into the remaining Corral; a
  set never "owns" items. Deleting one tab's Corral (the existing delete command) follows the existing delete behaviour for
  that Corral, then the set shrinks by the same rules.
- Atomic edit: build the new layout, validate, write with temp file plus rename (as colour-state-spec 3.4), then swap it in
  and animate. If the write fails nothing is swapped, the gesture visuals are reverted and a notice "Couldn't save the layout, so
  nothing was changed." is shown. The file therefore holds either the old or the new complete state, also across a crash.
- Reversibility replaces a general Undo (the product has none): join is reversed by Remove from tabs or tear-off, merge by tear-off
  or Separate all tabs; both keep every item and colour. The lost information is only a joined Corral's previous position,
  which is not data. Layout Backups remain the way back from anything else.
- Desk setups (desk-setups-spec) store placement per Corral id; restoring one writes each member's placement and the set's
  live geometry is then read from the selected tab (2.3).
- Round-up and Peek address items and Corrals by id, so hidden tabs stay valid targets; a hidden tab receiving items shows
  them when selected.

## 8. Migration of stored keys

- Everything is **read as stored**. No renumbering, re-indexing, compaction or counter change happens at load, and a load/save
  cycle with no tab edit is bit-identical for these fields.
- In-memory repairs (never written until the set is next modified by the user): duplicate or gapped indices sort by
  `(tab_index, file order)`; zero or several selected tabs resolve to the first selected in order, else index 0; a set with one
  member behaves as a plain Corral; members with different rects, monitors or Ranges follow the selected tab (2.3); a counter at
  or below an existing set id is raised in memory only. Unknown keys on any Corral are preserved on write.
- When a set is modified, the keys of all its members are rewritten with the same key names and value types the file already
  used. The sentinel for "no set", and whether the counter is a plain integer, are read as the existing parser does (question 1).
- Files written by this build must open in earlier builds that read the same keys (no new keys, no reordering of meaning).

## 9. DPI and mixed monitors

- Thresholds are DIPs converted at the DPI of the monitor that governs the set (5.1: system drag metrics for that DPI,
  `GetSystemMetricsForDpi`). Tear distance and arming zones use the strip rect in physical px of that monitor.
- A target on another monitor is drawn and hit-tested by its own window at its own DPI; the pointer is in virtual-screen px,
  mapped to each monitor's px. The arming timer is in ms and DPI independent.
- The torn ghost window is created at the DPI of the monitor under the pointer and follows the existing governing-monitor
  hysteresis when the pointer crosses monitors; rect is stored in DIPs relative to the work-area origin (grid-spec 8).
- Join across monitors stores the set's monitor and DIP rect on the source (round-trip exact, grid-spec 8). Insertion index,
  tab hit boxes and painting all call the same `tab_layout` with the same inputs, so they agree at 96-240 dpi and with negative
  virtual-screen origins.
- The "Combine as tabs" label and the dialog are created at the DPI of the monitor under the pointer (rebuilt on crossing).

## 10. Pure acceptance tests

Functions: `normalize_sets`, `next_set_id`, `join`, `merge`, `remove_tab`, `separate_all`, `reorder`, `drop_zone`,
`insertion_index`, `tear_distance`, `hover_switch_step`, `validate`. All with no windows; DPIs 96, 120, 144, 192, 240; origins
shifted by (-1920, -200) and (-3840, 0) give identical results after subtracting the origin.

1. `join(plain, plain)`: set of two, dropped selected, target's rect/monitor/Range/roll/dock kept; items, titles, colour slots
   of both unchanged (multiset equal); counter = used id + 1; ids never repeat across 1000 random join/leave sequences.
2. `join` into a set at each insertion index and append; `merge(set, set)`: order preserved, source id retired, target id kept,
   selected = source's selected.
3. Refusals: `TabsCreate` off, folded target (persistent and dock-derived), title Never, own set, self; allowed: other monitor,
   folded source, Gate in either role.
4. `drop_zone`: pointer in strip but dragged rect elsewhere arms; pointer on the strip edge band arms; z-order picks the top
   Corral; arming needs 120 ms in zone, exact at 119/120; leaving resets.
5. `insertion_index`: pointer left/right of each tab centre; empty strip area and style 0 append.
6. `reorder`: centre crossing exact at the boundary at every DPI; Esc leaves the vector unchanged; autoscroll steps.
7. `tear_distance`: 31.9 DIP stays, 32.1 DIP tears at every DPI (px at the set's DPI); drift of 20 DIP vertical during a long
   horizontal reorder does not tear; thresholds follow `SM_CXDRAG` values 4, 8, 16.
8. `remove_tab` / tear: selected re-chosen (same index, else previous); two-tab set dissolves and clears membership; the new
   Corral is unfolded and undocked and keeps its items.
9. `separate_all`: n Corrals, cascade +24 DIP per tab, selected keeps place, all clamped on screen, all items unchanged.
10. Hover step machine: enter starts timer, 299 ms no switch, 300 ms switches; delay flag off switches at once; leave, press,
    menu, drag cancel; stationary pointer after another switch does not re-arm; icon drag springs at 600 ms with the setting off.
11. Clear view: set kept when any tab is flagged; toggling writes the value to all tabs; tear-off keeps each flag.
12. Normalisation: gapped/duplicate indices, zero or two selected, one-member set, mismatched member rects, counter below an
    existing id: all repaired in memory; a load-then-save without edits is bit-identical; unknown keys survive; the first edit writes
    contiguous indices.
13. `validate` rejects: duplicate Corral, set of one, no or two selected, counter not above ids, lost item. Every operation above
    passes it on 10000 random sequences.
14. Persistence: write-failure leaves live state and file unchanged (injected failure); the file at any interruption point is
    a complete old or new layout.
15. Live checklist: highlight and insertion mark crisp across a mixed-DPI pair; tear-off ghost follows across monitors;
    dialog and tooltip at the right DPI; hover switching feels right with the pointer passing over tabs.

## 11. Open questions for the product owner

1. What are the stored names, types and "no set" sentinel of the membership and counter keys, and is `TabsMouseoverDelay` a
   flag or a number of ms? Recommendation: keep what the existing parser reads; treat a flag as "wait 300 ms", and a stored
   number between 100 and 2000 as that many ms.
2. Should `TabsCreate` off also block joining into existing sets and the Combine menu? Recommendation: yes (3.3).
3. Folder Gates as tabs, and tab sets mixing Gates and Corrals? Recommendation: allowed, nothing refused (3.3).
4. Spring-load a tab during an icon drag even when hover switching is off? Recommendation: yes, 600 ms (section 4).
5. Does Cancel in the confirmation dialog send the dragged Corral back (3.4) or leave it where dropped? Recommendation: send back.
6. Clear view: a set stays visible when any tab is exempt (2.5)? Recommendation: yes, and the command writes all tabs.
7. Is "Combine with" in the Tabs submenu (not asked for) acceptable as the non-drag and keyboard-friendly route? Recommendation:
   yes; it costs one command and fixes overlapping/hidden-title cases.
8. Tear distance 32 DIP, arming 120 ms, hover wait 300 ms: acceptable starting points to tune in live trials? Recommendation:
   yes; they are constants in one place.
9. A folded set: should its strip tabs stay clickable? Recommendation: yes; a click selects without changing the fold state.
10. Is the empty-title placeholder for a tab defined elsewhere? Recommendation: reuse whatever the Corral title shows for empty titles.

## Amendment (2026-10-22, user decision)

No "Combine with" menu command: joining tab sets stays drag-and-drop only.
