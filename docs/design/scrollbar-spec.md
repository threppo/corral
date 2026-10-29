# Corral scrollbar: design spec (area P7)

Status: approved by the user 2026-10-20 (open questions resolved as the spec recommends)
Date: 2026-10-20
Author: Claude subagent (clean spec writer), inputs: spec-inputs-wave-b-2026-10-20.md + approved Corral design docs + public docs only

Terms: "Corral" includes Folder Gates only where stated (section 1). DIP = 1/96 inch; device px = round(DIP x dpi / 96).
"Pitch" = the height of one icon row including its gap, taken from the Corral's shared cell policy (area P6).

## 1. Goal and non-goals

Goal: a Corral whose icons do not fit scrolls vertically, with a bar that is quiet at rest, easy to hit when wanted,
readable on every background, and identical in behaviour at any DPI.

Non-goals:
- Folder Gates: an opened Gate view hosts the Explorer view, which draws its own scrollbars. This spec covers Corrals.
  Amendment 2026-10-28 (user request): a Gate shows this overlay over its view's own vertical range (native rows or
  pixels, `external`); the view's vertical bar is hidden by giving its strip to the client area. Its horizontal bar
  stays Windows' own (dark in dark mode).
- Horizontal scrolling (icons wrap; a Corral never scrolls sideways). The settings window keeps its own scrollbar.
- No new settings UI beyond the existing "Scrollbars only on hover" toggle (section 7). `ExtraThinScrollbars` stays
  read-only (no UI).
- No inertia or rubber-band overscroll. The offset is never persisted.

## 2. Geometry (DIPs, scaled per Corral monitor)

The bar is an **overlay** on the body (it never reserves width, so the grid does not reflow when it appears). It is
absent for folded Corrals and while content fits (`content_h <= view_h + 0.5 DIP`). It sits inside the body, under the
titlebar (title above or below does not matter).

| Item | Normal | Extra thin |
|---|---|---|
| Thumb width at rest | 4 | 2 |
| Thumb width when pointed at (lane hot) or dragged | 8 | 5 |
| Gap from the body's right edge to the thumb | 3 | 2 |
| Hit lane (right edge strip that takes mouse input) | 14 | 10 |
| Minimum thumb length | 28 | 20 |
| Inset of the track from the body top and bottom | max(6, corner radius) | same |

- Thumb and track are rounded to a full cap (radius = width / 2); the widened thumb grows leftwards (right edge fixed).
- Track (drawn only while widened): usable track length, widened thumb width, 12 % alpha.
- Thumb length = max(min_len, track_len x view_h / content_h), never above track_len. Thumb top =
  track_top + (track_len - thumb_len) x offset / max_offset. `max_offset = content_h - view_h` (content includes the
  bottom padding, so the last row is fully visible at the end).
- Every size is converted to whole device px (rounded, widths at least 1 px) and the rectangle is snapped to device
  pixels, so there are no half-pixel edges at 100/125/150/250 %. The bar is repositioned (not scaled) on DPI change.
- The lane is hit-tested before icons. When the bar is hidden the lane is still live: entering it shows the bar at once.

## 3. Show, hide and widen

`ScrollbarMode` = 1: **hide until pointed at**. Any other stored value: **always visible** when scrollable (rest size).

Hide mode (own timings):
- Pointer enters the Corral (any part): fade in 120 ms, ease-out.
- Pointer leaves the Corral and no drag is active: after a 800 ms linger, fade out 240 ms, ease-in.
- Any scroll (wheel, keys, paging) shows the bar and restarts a 1000 ms linger, even with the pointer elsewhere.
- A drag keeps the bar visible until release; the linger then starts.

Widen (both modes): pointer inside the lane widens 4 -> 8 DIP over 100 ms ease-out; leaving waits 250 ms, then narrows
over 150 ms. A drag keeps it wide. Hover on the thumb itself brightens it (section 5), no extra size.

Reduced motion (`SPI_GETCLIENTAREAANIMATION` false): all durations are 0; lingers stay. One 16 ms timer runs only while a
transition is active; an idle bar costs nothing.

## 4. Input

**Wheel** (`WM_MOUSEWHEEL`, pointer over the Corral, no Ctrl, no Alt; Ctrl+wheel is icon zoom and Alt+wheel is Range switching):
- Rows per notch = clamp(round(`SPI_GETWHEELSCROLLLINES` / 3), 1, 4): the Windows default of 3 gives one row per notch.
  Lines = 0 disables wheel scrolling; `WHEEL_PAGESCROLL` scrolls one page.
- A standard notch (delta a multiple of 120) moves the offset to the next row boundary(ies) and animates 90 ms ease-out
  (0 ms in reduced motion). Further notches during the animation add to the target.
- Precision touchpads (deltas not a multiple of 120, or smaller than 120): offset += delta / 120 x rows x pitch, applied
  immediately, in sub-pixel DIPs, with no row snapping. Remainders are kept, not lost.
- At either end the event is not consumed (it passes on to the next window handler). Horizontal wheel is ignored.
**Thumb drag**: press on the thumb captures the mouse with no movement threshold. The thumb keeps the grab offset (no
jump). New offset = (pointer_y - grab - track_top) / (track_len - thumb_len) x max_offset, clamped. There is no snap-back
when the pointer leaves the lane. Esc during a drag restores the offset at press; losing capture or focus ends the drag
where it stands. A press on the bar never starts a rubber-band selection, icon drag or desktop click.
**Track paging**: press on the track outside the thumb scrolls one page toward the pointer: page = view_h - pitch,
rounded down to whole rows, at least one row, animated 120 ms. Held: after 400 ms repeat every 60 ms until the thumb
reaches the pointer or the button is released. Right button on the lane: nothing (the Corral menu is not opened).
**Keyboard**: the bar is not a Tab stop. If Corral moves a keyboard selection between icons, it calls
`ensure_visible(item_rect)` (minimum scroll to show the item with a 6 DIP margin). Home/End/PageUp/PageDown scroll only
when that selection exists (open question 4).
State: `offset` is clamped to [0, max_offset] whenever the Corral resizes, icons change or zoom changes; the first
visible row is kept as the anchor where possible. Tabs each keep their own offset (open question 5).

## 5. Camouflage and backgrounds (camouflage-spec 3.5)

- The bar is a Corral surface. It is recoloured with `recolor_color` (alpha preserved) only when
  `DesktopBlendCorralBackgrounds` is on; the pair and strength are the Corral's. Off: the base colours below are drawn.
- Hover-to-real-colours fades the bar together with the Corral background and icons (same fade factor).
- The wallpaper showing through a see-through Corral is never recoloured. The bar's own alpha is not multiplied by the
  Corral's see-through factor (it must stay readable on a nearly clear Corral).
- Guard: after recolouring, if the composited rest thumb is below 3:1 contrast with the effective backdrop (the
  camouflage-spec 3.5 definition), use the un-recoloured colour instead.

Colours: base = the Corral's resolved label colour (Auto already picks white or #1B1D22). Thumb alpha: rest 0.55, hovered or
lane hot 0.80, dragging 1.00; raised (up to 1.00) until the composited thumb reaches 3:1 against the backdrop where possible.

## 6. High contrast and DPI

- `SPI_GETHIGHCONTRAST` with `HCF_HIGHCONTRASTON` (re-read on `WM_SETTINGCHANGE` and `WM_THEMECHANGED`):
  the bar is always visible when scrollable (ScrollbarMode is ignored), uses the widened size, no fades or widening
  animation, no recolour. Track opaque `COLOR_WINDOW` + 1 px `COLOR_WINDOWTEXT` outline; thumb `COLOR_WINDOWTEXT`; hot or
  dragged thumb `COLOR_HOTLIGHT` (documented to pair with `COLOR_WINDOW`).
- DPI: all values scale per section 2 using the DPI of the monitor that governs the Corral; a monitor change re-lays
  out the bar from the same DIP table. Row and page distances come from the shared cell policy, never from pixels.

## 7. Settings keys

| Key | Meaning |
|---|---|
| `ScrollbarMode` | 1 = hide until pointed at (fresh default, open question 1); other values = always visible when scrollable. |
| `ExtraThinScrollbars` | true = "Extra thin" column of the table in section 2. Read-only, no UI. |
| `DesktopBlendCorralBackgrounds` | Recolour switch for the bar (section 5). |

The existing Personalization > Advanced toggle "Scrollbars only on hover" (redesign spec 7) is the UI for `ScrollbarMode`:
on writes 1, off writes 0. No other settings UI changes.

## 8. Acceptance tests (pure, no window)

Geometry `bar_layout(view_h, content_h, offset, dpi, thin, corner_r) -> Option<Bar>` (1-4), scroll model (5-10), state (11-13), colour (14):
1. Fits (content <= view + 0.5): None; one DIP more: Some; zero view height: None, no panic.
2. Thumb length: tall content gives the minimum; content = 2 x view gives half the track; never above the track.
3. Thumb top: offset 0 -> track_top, max -> track_top + track_len - thumb_len; monotone between.
4. Rects are whole device px at 96/120/144/240 dpi, width at least 1 px, inside the body; thin < normal.
5. Scroll model, wheel: delta 120 with 3 lines = one pitch to the next boundary; lines 6 = two rows; WHEEL_PAGESCROLL = page.
6. Precision deltas (30, -45, 7) accumulate to the exact sum, no row snapping; clamped at both ends; ends not consumed.
7. Drag: thumb-position <-> offset maps invert each other within 1 device px; grab offset prevents a jump on press;
   Esc restores the press offset; pointer far outside the lane still maps by y only.
8. Paging: page = view_h - pitch in whole rows, at least one; repeat stops when the thumb passes the pointer.
9. Resize/zoom/icon-count change keeps the offset clamped and the anchor row if it still exists.
10. `ensure_visible` scrolls the minimum, 0 when visible already, clamps at the ends.
11. Hide mode (fake clock): enter -> fade-in 120 ms; leave -> bar stays 800 ms, then fades over 240 ms; re-enter mid-fade continues
    from the current alpha (no jump); wheel after leave restarts a 1000 ms linger; drag blocks hiding.
12. Widen: enter lane widens over 100 ms, leave waits 250 ms then narrows over 150 ms; drag holds wide. Always-visible mode never fades; reduced motion makes transitions instantaneous.
13. High contrast: always visible, fixed colours, no recolour, no animation.
14. Thumb contrast over black, white, mid-grey, bright red backdrops >= 3:1 where achievable; recolour keeps alpha and
    falls back when the guard fails; the hover fade factor scales the bar like the Corral.
Live checklist: 100/150/250 % and mixed DPI; touchpad feel; no flicker over icons; bright and dark wallpapers; high contrast.

## 9. Open questions

1. Fresh-install default for `ScrollbarMode`, and what stored values other than 1 mean (assumed "always visible").
2. Is `HideScrollbars` (redesign spec) the in-code name of `ScrollbarMode`? Assumed yes.
3. The overlay can cover the right edge of the rightmost label; add right content padding (P6 grid decision)?
4. Does Corral have keyboard navigation between icons? If not, section 4 "Keyboard" has nothing to do.
5. Should each tab keep its own scroll offset (assumed yes) or reset on switching?
6. Autoscroll while dragging an icon near the top/bottom edge of a Corral: in scope here or in the drag-and-drop area?
7. Touch and pen panning inside a Corral (not specified here).
8. Should `ExtraThinScrollbars` get a UI? Use the system wheel-lines setting (assumed) or one row per notch?
9. High contrast: is the Corral body itself switched to system colours elsewhere, or does the opaque track sit on a custom body?
