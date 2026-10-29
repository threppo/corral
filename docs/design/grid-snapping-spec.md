# Grid, sizing and snapping: design spec

Status: approved by the user 2026-10-20 (open questions resolved as the spec recommends)
Date: 2026-10-20
Author: Claude subagent (clean spec writer), inputs: spec-inputs-wave-b-2026-10-20.md + approved Corral design docs + public docs only

Terms: "Corral" includes Folder Gates and tab groups (one rectangle). DIP = 1/96 inch. "px" = device pixels of the Corral's
governing monitor. "Outer rect" = body plus the title strip; "body" = the stored `rect`. "Neighbour" = another Corral that
snapping may use (section 4.4). All rounding is round-half-up, `floor(x + 0.5)`, for negative values too, so results never
depend on where the virtual-screen origin is. Every size below is a proposal for review, not a measured value.

## 1. Goal and non-goals

Goal: one grid, one set of sizing rules and one snapping model, so that icons, hit tests, drops, scrolling and zoom always
agree, a Corral feels the same at any DPI, and a stored size is exactly what the user last chose.

Non-goals:
- Corral never changes Explorer's icon spacing or the icon size of the desktop. It only reads them.
- No free-form icon positions inside a Corral beyond the grid (placing in cells is the whole model).
- No new settings and no new settings UI. Snapping keys keep their names and meanings (section 8).
- Docking, folding and desk setups keep their own specs; this spec only says how they interact with sizing and snapping.
- Scrollbar look and timing belong to the scrollbar area (P7); this spec only reserves room for it (3.1).

## 2. Cell policy

### 2.1 One function

`cell_metrics(CellInputs) -> CellMetrics` is the only place a cell size is computed. Painting, hit testing, drop slots,
scroll limits, zoom and whole-cell sizing all take their numbers from the returned value; none of them multiplies by a
spacing constant on its own.

    CellInputs  { dpi, zoom_pct, windows_spacing_dip: Option<(f32, f32)>, desktop_icon_dip: Option<f32>,
                  match_desktop: bool /* GroupSpacingMatchesDesktop */ }
    CellMetrics { cell_w_px, cell_h_px, icon_px, label_lines, pad_px, cols_for(inner_w_px), rows_for(count, cols),
                  cell_rect(index, cols) -> Rect, cell_at(point, cols, scroll_px) -> Option<index>,
                  drop_slot(point, cols, scroll_px, count) -> index, max_scroll_px(count, cols, inner_h_px) }

Other helpers: `body_size_for(cols, rows) = (cols * cell_w_px + 2 * pad_px, rows * cell_h_px + 2 * pad_px)` and its
inverse `cells_for_size`. Pure, no window, no globals; the caller supplies the inputs and caches the result keyed by the
whole `CellInputs` value (recompute on DPI change, zoom change, setting change and `WM_SETTINGCHANGE`).

### 2.2 Inputs and formula (all in DIPs first, rounded to px once)

1. Windows spacing (read only): at startup and on `WM_SETTINGCHANGE`, call `GetSystemMetricsForDpi(SM_CXICONSPACING /
   SM_CYICONSPACING, GetDpiForSystem())` (public API; falls back to `SystemParametersInfo(SPI_GETICONMETRICS)` which returns
   `iHorzSpacing` / `iVertSpacing`). Divide by `system_dpi / 96` to get DIPs. The DIP value is then valid for any monitor.
2. Icon edge: the desktop icon size in DIPs if the engine knows it, else 48 DIP (Windows' default medium icons).
   `icon_dip = icon_edge * zoom_pct / 100`.
3. Content minimum (label lines): `LABEL_LINES = 2`, `LABEL_LINE_H = 16 DIP` (labels do not follow zoom).
   `min_w = max(icon_dip + 16, 64)`, `min_h = icon_dip + 4 + LABEL_LINES * 16 + 4`.
4. Cell in DIPs: with `match_desktop` on, `w = max(win_w * zoom / 100, min_w)`, `h = max(win_h * zoom / 100, min_h)`;
   with it off, `w = min_w`, `h = min_h` (compact cell). The maximum with the content minimum guarantees that an icon and its
   two label lines never overlap the next cell, even for a very tight Windows spacing.
5. Pixels: `cell_w_px = floor(w * dpi / 96 + 0.5)`, same for height, each at least 1. Everything inside the Corral is an
   integer multiple of these (`x = pad + col * cell_w_px`), never `index * float`, so there is no accumulated error.
   The icon is centred in the cell with `floor((cell - icon_px) / 2)`.
6. Body padding: `pad = 8 DIP` on all four sides (`pad_px` rounded once). The overlay scrollbar lives inside the right
   padding (constraint for P7: its hit area ends at least 8 DIP from the body's right edge, 3.1).
7. Zoom: `icon_zoom` is an integer percent clamped to 50..300; absent means 100. Ctrl+wheel changes it by 10 per notch.

Zoom never changes the Corral's size. Cells reflow; the first visible row is kept (`scroll_px` is recomputed so that the
item at the top-left stays there). Whole-cell sizing (3.4) applies only to later resizes, never as a result of zooming.

### 2.3 Windows metrics unavailable

If the API fails, returns 0 or leaves the clamp range 40..160 DIP: use the default 75 x 75 DIP (Windows' stock spacing) for
that axis, log once per run, and keep going. A Remote Desktop or service context therefore gives a plain, working grid.
The function never panics and never returns a zero size.

## 3. Resize

### 3.1 Hit zones (outer rect, in this priority order)

1. **Corner zone**: 16 x 16 DIP square at each corner of the outer rect (clamped to a third of the width and of the
   height; at least 8 px).
2. **Edge band**: 8 DIP wide strip along each edge, inside the outer rect (clamped to a third of the shorter side; at least
   4 px). It covers the title strip's outer edge too, so a 24 DIP title keeps a 16 DIP move area.
3. **Title strip**: move (caption) zone, except the title's own buttons, which come first when the title is shown.
4. **Body**: icon area; the scrollbar's hit area stays inside it, not closer than 8 DIP to the right edge.

The zones are answered from `WM_NCHITTEST` (`HTLEFT` ... `HTBOTTOMRIGHT`, `HTCAPTION`), so cursors and accessibility come from
Windows. The drag itself runs from our own mouse-capture loop, not from the system sizing loop, because the same pure
function must also drive creation (6), the tooltip (7) and Esc-to-cancel. Cursors: `IDC_SIZEWE`, `IDC_SIZENS`, `IDC_SIZENWSE`,
`IDC_SIZENESW`. There is no invisible margin outside the outer rect: it would overlap neighbours packed at a small gap.

Disabled zones: a folded Corral (3.6) has no top or bottom zones (corners become left/right edges). A docked edge (3.6) has no
zone. A Corral being dragged has none. Esc during a resize restores the rect the drag started with.

### 3.2 Which edge stays fixed

The edge or corner opposite to the grabbed zone is the anchor and never moves; the grabbed edge follows the pointer. This holds
when the minimum, the work-area limit or snapping stops the grabbed edge. A side that is not grabbed keeps its stored value
bit for bit (a bottom-edge drag cannot change x, y or width, not even by one rounding step).

Position is always computed from the rect and pointer at drag start (`start_rect` plus `pointer - start_pointer`, in px), never
from the previous message, so the result depends on where the pointer is and not on how many messages arrived.

### 3.3 Minimum and maximum

- Minimum body: 64 x 48 DIP (small enough to be a handle, large enough for the title text, a menu button and one icon). With
  whole-cell sizing on, the minimum is one cell: `max(64, cell_w + 2 pad)` x `max(48, cell_h + 2 pad)`.
- A Corral that is already smaller on an axis (loaded from an older file or fitted to a small monitor) may keep that size; the
  effective minimum on an axis is `min(MIN, size at drag start)`. Resizing never forces it larger.
- Maximum: the governing monitor's work area, minus the title strip on the body's vertical axis. A Corral never straddles
  monitors (its rect is relative to one work area), so growing stops at the work-area edge.

### 3.4 Whole-cell sizing (`SnapToDiscreteSizes`, needs `SnapEnabled`)

For the grabbed axis (corners treat the two axes independently): `raw_inner = raw_size - 2 pad`;
`n = clamp(floor(raw_inner / cell + 0.5), n_min, n_max)`; `size = n * cell + 2 pad`, in px. `n_max = floor((limit - 2 pad) / cell)`.
Rounding to nearest means the edge jumps at half a cell. Because `n` is computed from the raw pointer each time, there is no
drift and no hysteresis is needed. Only user resizes and creation produce whole-cell sizes. A stored size that is not whole is
left alone until the user resizes that axis (8).

### 3.5 Shift and edge snapping while resizing

- Holding Shift (sampled with `GetKeyState` on every mouse move, so it can change mid-drag) turns off both whole-cell sizing and
  edge snapping. The raw pointer position is used, still limited by the minimum, the work area and whole pixels. Releasing
  Shift snaps back at once. Shift+drag on a tab keeps its Tabs meaning (reorder) and is not a Corral resize.
- With `SnapEnabled` on and whole-cell sizing off, the grabbed edge snaps to work-area edges and to neighbours' edges, using
  the move rules of section 4 (adjacent edges with gap, aligned edges when `SnapLineUpCorrals`). With both on, whole cells win;
  a neighbour alignment would break the grid.

### 3.6 Title position, folded and docked Corrals

- **Title above or below**: the outer rect is the invariant; the body is the outer rect minus a title strip of fixed height
  `TITLE_H = 24 DIP` (rounded to px once; the painting code owns the actual value and exports it). Resizing from the title's
  outer edge moves the body's matching edge by the same amount. If the title moves to the other side (auto-adjust near the top),
  the outer rect stays and the body shifts by exactly `title_px`; the shift is exact and reversible.
- **Title hidden** (Never): strip height 0. **On hover**: the strip is reserved, so the layout never jumps when it appears.
- **Folded**: the outer rect is the title strip alone. Only width can be resized (left and right zones), with the same
  whole-cell rule for width. The stored body height and roll state are untouched, so unfolding restores the old height.
- **Docked** (`dock_top` or `dock_bottom`): the docked edge has no zone; height changes by the free edge, width by both
  sides. A hover-unfold expands away from the docked edge and is drawn clamped to the work area without writing anything.
- Folder Gates and tab groups follow the same rules; a tab group's tab strip counts as its title strip.

## 4. Move snapping

### 4.1 What it works on

The outer rect of the moving Corral, in monitor-local px (origin = the governing monitor's work-area top-left), against the
outer rects of neighbours and the work area. The two axes are solved independently. Folded Corrals use their folded rect.

### 4.2 Targets

With `SnapEnabled` on, per axis (x uses left and right edges, y uses top and bottom):
1. **Work-area edges**: my edge flush with the work-area edge (gap 0). This guarantees equality, which the docking logic needs.
2. **Adjacency**: my right edge at `neighbour.left - gap`, my left at `neighbour.right + gap` (likewise vertically). Only
   neighbours whose range on the other axis overlaps mine, grown by the capture distance, count; one far above cannot grab
   a horizontal move.
3. **Alignment** (needs `SnapLineUpCorrals`): left with left, right with right, top with top, bottom with bottom, for any
   neighbour. No centre alignment. A guide line is drawn along each aligned edge while the snap holds.

### 4.3 Distances, stickiness, gap

- Capture distance: 8 DIP (`round(8 * dpi / 96)` px). Once an axis is snapped it stays snapped until the raw edge is more than
  12 DIP away (release distance). That is what makes snapping feel sticky without flicker.
- Candidate choice: smallest absolute distance; ties go to work-area edge, then adjacency, then alignment.
- `gap` = 0 if the "Keep a gap between Corrals" toggle is off, else `SnapPadding` (0..100) converted to px:
  with `SnapShouldScale` on (default when absent) `round(SnapPadding * dpi / 96)` at the governing monitor's DPI; off:
  `SnapPadding` literal px. The gap is measured between outer rects, so titles are included.
- Shift anywhere during the drag turns all of this off immediately; position is then the raw start rect plus pointer delta.
  `SnapEnabled` off means no snapping at all, including no whole-cell sizing; keep-on-screen (5) still applies.

### 4.4 Which Corrals are neighbours

All of: same governing monitor, same Range (desktop page) as the moving Corral, currently visible (not hidden by Clear view,
not another tab of a tab group, not the moving Corral itself). Faded or folded Corrals count. Corrals of other Ranges and
other monitors never do, and the moving Corral's own tab siblings do not.

### 4.5 No drift

A move changes only `x` and `y`; width and height are copied unchanged from the start rect. A snapped position is an integer
px; when nothing snaps, it is the integer px from the pointer delta. Converting to DIPs for storage uses the exact ratio and
the stored value must map back to the same px (8, test 12).

## 5. Keep on screen and rescue

Applies after a user drop, resize end, creation, unfold and title flip; DPI and monitor changes keep going through the existing
fit machinery, which calls the same pure function. `place_on_screen(outer, monitors, preferred_monitor, pointer) -> Rect`:

1. Monitor: the governing monitor chosen by the engine's drag hysteresis; if none (the pointer or rect lies outside every
   monitor, such as the dead space between two differently sized monitors or a removed monitor), the monitor with the largest
   overlap with the outer rect, else the one nearest its centre (`MONITOR_DEFAULTTONEAREST`).
2. If the outer rect is larger than the work area on an axis, reduce that axis to the work area. Only the live size is reduced;
   `preferred_size_dip` is untouched so a later larger monitor restores it. The reduction is exact, not rounded to cells.
3. Then shift the whole rect (never resize again) so all four sides are inside the work area.
4. Result is stored in that monitor's work-area-relative DIPs.

`LayoutPreserve = 0` ("Don't adjust") is respected on load: nothing is moved when a file is read or monitors change. User
actions still rescue, so a drag can never leave a Corral where it cannot be grabbed. A hover-unroll is drawn clamped but not
stored; a real unfold near the bottom or right edge shifts and stores.

## 6. Creation by drawing a rectangle

The press point is the anchor; the opposite corner follows the pointer. The shape is an outer rect: body = rect minus the title
strip (on the side the Corral will use).
- Cancel if the drawn rectangle is under 16 DIP on both axes (a click or a slip); the menu logic ignores it.
- The rectangle is limited to the work area of the monitor under the press point; spanning monitors is clipped, never split.
- Size: `max(drawn, minimum)` per axis (3.3), growing away from the anchor. With whole-cell sizing on, each axis is rounded to
  cells as in 3.4 with the anchor fixed. Otherwise, with `SnapEnabled`, edges snap as in 3.5. Shift disables all of it.
- The preview rectangle shows the snapped result, so what the user sees is what is created. The size tooltip (7) is shown.
- The new Corral stores `rect` and sets `preferred_size_dip` to the created size.

## 7. Size tooltip

- Shown during a resize and during creation, after the drag passes the system drag threshold; hidden on release or Esc. Not
  shown while moving.
- Text: outer-rect size in px of the governing monitor, `640 x 360` (multiplication sign U+00D7 with spaces). When whole-cell
  sizing is active, append the cell count: `640 x 360 (8 x 3 icons)`. With Shift held the cell count disappears.
- Window: topmost, non-activating, click-through tool window, created at the DPI of the monitor under the pointer; updates only
  when the text changes. Text 12 DIP, padding 8 x 4 DIP, radius 4, `bg/surface` fill and 1 DIP `border/default` line, text
  `text/primary` (settings-redesign tokens), dark mode aware.
- Position: pointer + (16, 20) DIP; flipped above or left of the pointer when it would leave the monitor work area. When the
  pointer crosses to a monitor with another DPI, rebuild font and padding at the new DPI.

## 8. Storage

- `rect` (body, DIPs relative to the monitor work-area origin), `preferred_size_dip`, roll state, dock flags and `icon_zoom` are
  stored as today. **Loading never rounds, snaps, clamps to cells or rewrites them.** Only the existing fit machinery may change
  the live size, and it keeps `preferred_size_dip`.
- `preferred_size_dip` is written when the user resizes or creates a Corral (the size chosen), never by a fit, a move, a zoom
  or a rescue.
- px to DIP uses the exact ratio `px * 96 / dpi`; DIP to px uses round-half-up. Requirement: for every dpi in 96..480 and
  every |px| <= 20000, `dip_to_px(px_to_dip(px)) == px`. Stored DIP values keep at least 0.01 precision (if they are integers
  today, that is an open question, 10).
- Keys keep their names and meanings: `SnapEnabled` (master switch for all snapping and whole-cell sizing), `SnapPadding`
  (gap in pixels at 100 percent scaling, 0..100), `SnapToDiscreteSizes`, `SnapLineUpCorrals`, `SnapShouldScale` (padding follows
  DPI), `GroupSpacingMatchesDesktop`. The "Keep a gap between Corrals" toggle (named `SnapSpaced` in the approved settings
  design) gates `SnapPadding`; the key is read if present, absent means on. No key is renamed, added or removed here.
- Absent keys: `SnapEnabled` on, `SnapPadding` 8, `SnapShouldScale` on, others off except `GroupSpacingMatchesDesktop` on.

## 9. Edge cases and acceptance tests

All pure, on small inputs, no windows. DPIs: 96, 120, 144, 192, 240 (scale 1, 1.25, 1.5, 2, 2.5). Named functions:
`cell_metrics`, `resize_zone`, `resize_rect`, `snap_move`, `place_on_screen`, `creation_rect`, `px_to_dip`, `dip_to_px`.

1. Cell sizes (win 75 x 75, zoom 100, icon 48, match on): px width 75, 94, 113, 150, 188; with zoom 150: 113, 141, 169,
   225, 281 (`floor(x + 0.5)`). Match off gives the content minimum (64 wide, 88 high at zoom 100). Tight Windows spacing
   (40 x 40) is raised to the content minimum. Missing or zero metrics give 75 x 75. Zoom 50 and 300 are clamped, 49 and
   301 are clamped to those.
2. One function: a table test feeds one `CellMetrics` to `cell_rect`, `cell_at` and `drop_slot` for 0..60 items and every DPI;
   `cell_at(cell_rect(i).center) == i`, rects tile with no gap and no overlap, `drop_slot` is monotone.
3. Scroll: `max_scroll_px` is 0 when all rows fit, else `rows * cell_h + 2 pad - inner_h`; zoom keeps the top-left item.
4. Every zone: for each of 8 edges and corners, a point in the zone returns it; points on the 8 DIP band boundary (inclusive
   inner side) are exact at every DPI; corner beats edge beats title beats body; title buttons win in the title strip; a 20 x 20
   DIP outer rect still returns sane zones (third-of-side clamp).
5. Anchor: dragging each zone changes only the expected sides; the opposite side is bit-identical, including when stopped by
   the minimum, the work area or whole-cell snap; the stored size on an ungrabbed axis does not change by 1 px.
6. Whole cells: sizes are always `n * cell + 2 pad` in the grabbed axis; the half-cell jump point is exact; `n_min` and `n_max`
   hold; without `SnapToDiscreteSizes` sizes are raw px.
7. Shift: with Shift, a drag equals the raw pointer result at every DPI; pressing and releasing Shift during one drag
   toggles the result and returns to the first snapped value when released (no hysteresis residue).
8. Title and states: title above or below keeps the outer rect through a flip and restores the body exactly; hidden title uses
   height 0; folded Corral resizes width only and keeps its stored height; docked top has no top zone and keeps `y = work.top`;
   a tab group and a Folder Gate behave as Corrals.
9. Move snapping: each target kind alone (work edge, adjacency, alignment) snaps at 7.9 DIP and not at 8.1 DIP; a snapped axis
   releases beyond 12 DIP; ties follow the priority; gap is `round(SnapPadding * dpi / 96)` with `SnapShouldScale` on and
   literal with it off; gap 0 when the gap toggle is off; `SnapLineUpCorrals` off removes alignment only; `SnapEnabled` off
   removes all snapping and whole cells.
10. Neighbours: another monitor, another Range, hidden, another tab of the same group, the Corral itself: none are used. A
    folded and a faded neighbour are used. A neighbour on the other Range with a perfect alignment does not snap.
11. Negative origins: run each test with monitor-local inputs shifted by (-1920, -200), (-3840, 0) and (0, -1080); results,
    after subtracting the origin, are identical. A Corral at x = -1500 snaps to a neighbour at x = -1620 + width + gap.
12. No drift: 10000 random moves with pointer deltas summing to zero return the exact start rect; width and height never
    change in a move; `dip_to_px(px_to_dip(px)) == px` for all DPIs listed and 96..480 in steps of 12; a save, load, save
    sequence leaves the stored rect bit-identical.
13. Rescue: a rect past each edge, bigger than the work area, in the dead space between monitors, on a removed monitor and
    straddling two monitors ends fully inside one work area; `preferred_size_dip` is unchanged; `LayoutPreserve = 0` makes a
    file load move nothing; a user drag still rescues.
14. Creation: a click and a 15 DIP slip cancel; 17 DIP creates at the minimum; dragging up-left works (anchor fixed);
    whole-cell creation; clipping to the press monitor; Shift raw; created size equals `preferred_size_dip`.
15. Tooltip text and placement: text for pixel and whole-cell modes at each DPI; flips at the right and bottom work-area edge;
    never partly on another monitor.
16. Live checklist (not unit-testable): cursor shapes, tooltip crispness across a mixed-DPI pair, drag across monitors keeps
    gap and cell sizes right, the desktop's own grid and the Corral's grid agree on spacing at 125 percent and 150 percent.

## 10. Open questions for the product owner

1. Is the title strip always 24 DIP and the grab band 8 DIP, or should they follow system metrics? Recommendation: fixed
   values, scaled with DPI.
2. Is storing the Windows spacing read at the system DPI and rescaled per monitor right for mixed-DPI setups? Recommendation:
   yes, verify with a live pair (16); if wrong, read the spacing per monitor.
3. Should a drop that is flush with the top or bottom edge always dock? Recommendation: the snap makes it flush, and the dock
   logic decides (its own spec); this spec only guarantees exact equality.
4. Are `rect` values stored as integers? Recommendation: store floats with at least 0.01 precision, or integers only if the
   round trip test (12) passes at all DPIs.
5. Fresh defaults for snapping (on, gap 8 DIP, gap scaling on, whole cells off). Recommendation: as listed in 8; whole-cell
   sizing off so a plain resize is predictable.
6. Does resize snap to neighbours when whole cells are off (3.5)? Recommendation: yes, with the same capture distance.
7. Is a 64 x 48 DIP minimum body acceptable? Recommendation: yes; a smaller Corral cannot show a title and one icon.
8. Reserve the title strip when the title shows only on hover (3.6)? Recommendation: yes, so nothing shifts when it appears.
9. Should zoom ever resize the Corral to keep its cell counts? Recommendation: no; zoom only reflows, and size is only changed
   by the user.
10. Does the scrollbar fit inside the 8 DIP right padding (P7) without crowding the right resize band? Recommendation: scrollbar
    hit area ends 8 DIP inside, its drawn bar may overlay the padding.
11. Should the size tooltip report the body or the outer size? Recommendation: outer size (what the user sees), as specified.
12. Does Shift conflict with any other drag gesture on a title or body besides tab reorder? Recommendation: none known; Shift
    snapping-off wins on titles and edges, tab reorder wins on tabs.
