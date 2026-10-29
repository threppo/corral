# Motion and sorting: design spec (area P11)

Status: approved by the user 2026-10-21 (open questions resolved as the spec recommends)
Date: 2026-10-21
Author: Claude subagent (clean spec writer), inputs: spec-inputs-wave-c-2026-10-21.md + approved Corral design docs + public docs only

Terms: "Corral" includes Folder Gates only where a line says so. "Progress" p is time elapsed / duration, clamped to 0..1.
"Target" = the state an animation is heading for; input and hit tests always follow the target, never the in-flight picture.
All durations, curves and delays below are proposals for review, not measured values.

## 1. Goal and non-goals

Goal: one small motion vocabulary (three curves, a handful of durations) and one deterministic item order, so that sorting
matches what Windows users expect, every animation is short, interruptible without jumps, cheap, and off when Windows
animations are off.

Non-goals: Folder Gates (the hosted Windows view sorts and animates itself); the menu wording and the storage of the sort
choice; interactive drag-following swipes for Ranges; animation of loading, DPI, monitor or backup-restore changes; new settings
(none are added; section 6 reuses `IconAnimationDisabled`, section 5 reuses the Ranges Animation choice).

Rule for every animation: only changes caused by a live gesture, timer or command in this session animate. State applied by
loading a layout, a desk-setup recovery, a DPI change or a restore is applied instantly (first paint never animates).

### 1.1 Shared curves and durations

Curves are cubic Beziers from (0,0) to (1,1) with control points (x1,y1,x2,y2); `ease(curve, p)` solves x(t)=p (Newton, bisection
fallback, tolerance 1e-5) and returns y(t). x1,x2 are within 0..1; for the first three, y1,y2 are too, so they are monotone.

| Name | Control points | Used for | Reason |
|---|---|---|---|
| OUT | 0.10, 0.50, 0.30, 1.00 | things arriving, opening, settling | Starts fast so the response feels instant, ends soft. |
| IN | 0.70, 0.00, 0.90, 0.50 (mirror of OUT) | things leaving, closing | A leaving object speeds up and is gone, matching Windows motion guidance. |
| INOUT | 0.40, 0.00, 0.60, 1.00 | reserved (no use in this spec) | Kept symmetric for later moves between two resting places. |
| BOUNCE | 0.34, 1.30, 0.64, 1.00 | Ranges "Bounce" only | y1 above 1 gives a single overshoot of about 3 % of travel at roughly 68 % of the time. |

Durations: Fold open 180 ms (OUT), Fold close 140 ms (IN), Clear view out 150 ms (IN), Clear view in 220 ms (OUT), Ranges Slide
240 ms (OUT), Ranges Bounce 360 ms (BOUNCE), icon glide 180 ms (OUT). Reasons are given in each section. The scrollbar spec's
"ease-out" and "ease-in" mean OUT and IN here; Camouflage's 120 ms / 200 ms real-colour fades keep their durations and use OUT
(to real colours) and IN (back to the effect). Durations never depend on DPI; distances do (they are DIPs).

## 2. Sorting

A sorted Corral lays its icons out in the sorted order, row by row, left to right. This section defines only the order.

- Keys: Name, Type, Size, Date (date modified, UTC instant). Direction: ascending or descending, applied to the primary key only.
- Group first: folders, including virtual and system items (This PC, Recycle Bin), come before files for every key and in both
  directions. A shortcut to a folder is a file. Reason: matches Explorer, and descending never buries folders at the bottom.
- Name: the label text the user sees (hidden extensions stay hidden, so the visible order is the order). Compare with
  `CompareStringEx(user default locale, NORM_IGNORECASE | SORT_DIGITSASNUMBERS | SORT_STRINGSORT)`. Digit runs anywhere count as
  numbers ("file2" before "file10", "v1.9" before "v1.10"); case is ignored; accents are not (culture decides, see below);
  punctuation is never skipped, so "a-b" and "ab" do not interleave. `StrCmpLogicalW` is the same idea without a locale argument; we
  take `CompareStringEx` because the locale makes tests reproducible. A call that returns 0 (error) falls back to an ordinal
  case-folded compare and logs once.
- Culture: the user's locale (`LOCALE_NAME_USER_DEFAULT`), read at startup and on `WM_SETTINGCHANGE`. A Polish user sees "Ala" before
  "Ąbc"; an English user sees "Ąbc" first. Both are correct for their user. The collator is injected (`Collator` trait) so logic
  tests do not depend on Windows' NLS tables.
- Type: the type text Explorer shows ("PDF Document", "File folder"), compared with the same collator. No registered description:
  the upper-cased extension plus " File" ("XYZ File"); no extension: "File". Look-ups are cached per lower-cased extension.
- Size: bytes, numerically. Folders and system items have no size and tie with each other. Zero-byte files are ordinary.
- Date: last-write instant. Folders use their own last-write time.
- Unknown values (failed size or date look-up): after every known value, in both directions, then by the tie-breaks below.
- Ties, always ascending whatever the direction: (1) Name natural order, skipped when Name is the key; (2) ordinal compare of the
  name, case-sensitive by UTF-16 code unit ("Apple" before "apple"; only possible when the user and public desktops share a name);
  (3) ordinal full path; (4) original index. The result is a total order, so the sort is stable and the same input in any
  order gives the same output.
- Re-sorting runs when the item list or a key fact of an item changes, at most once per message-loop turn. The icons then glide to
  their new cells (section 6).

## 3. Fold

Folded = effective fold = stored roll state, or (`RollupOnDock` on and the Corral is docked top or bottom). The second part is
derived, never stored, so dragging a Corral off the edge unfolds it by itself and nothing needs to be written. A folded Corral
shows only its title strip (grid spec 3.6); its stored height and `preferred_size_dip` are never touched by any animation.

Opening (all temporary; the stored state does not change, and nothing is written to a layout or a desk setup):
- `RollupEnabled` off: no folding at all. Otherwise hover starts the open, unless `RollupRequireClick` is on.
- Hover mode, no delay (`RollupRequireDelay` off): the pointer entering the folded rect opens it at once.
- Hover with delay (`RollupRequireDelay` on): the pointer must stay inside the folded rect for `RollupRequireDelayTime` ms (400 ms if
  absent, clamped 100..2000; the unit is milliseconds, open question 4). Leaving earlier cancels the wait.
- Click mode (`RollupRequireClick` on): hover does nothing; a single left click on the folded title opens it. The delay key is
  ignored in this mode (the redesign merges the keys into one choice).
- `RollupDoubleClick` on: a double-click on the title toggles the stored roll state (fold or unfold for good). The first click of
  that double-click may already have opened it temporarily; the persistent unfold simply takes over. A persistent fold started
  from a temporarily open Corral collapses it with the close animation and ignores the pointer until it has left the title once, so
  it does not pop back open. On a dock-derived fold a double-click has no persistent effect and opens it temporarily.
- Closing a temporary open: 300 ms after the pointer leaves the outer rect (it returns before that: cancelled). Never while a
  mouse button is down, an icon or Corral drag that started here is running, or one of its menus, tooltips or rename boxes is open;
  the 300 ms starts when those end. Reason: crossing a gap between icons or reaching a menu must not flicker.
- Animation: the outer rect goes from the folded rect to the expanded rect (both computed by the normal layout, including the dock
  and work-area clamp of grid spec 3.6; the expanded rect grows away from the docked edge, else away from the title strip).
  Open 180 ms OUT, close 140 ms IN, every edge using the same eased progress. Content is laid out at its final place and clipped
  to the animated rect, never scaled. Shadow, blur, outline and the scrollbar follow the animated rect. Reasons: opening is the
  answer to the user's act, so it is short but readable; closing is a dismissal, so it is faster.
- Hit tests and the leave rule use the target rect (expanded while opening, folded while closing), so a closing Corral does not
  catch the pointer and an opening one is usable at once.
- Interrupting (reverse mid-flight): the new tween starts at the current progress, duration = base x the remaining distance
  (at least one frame), so the picture never jumps.
- After dragging a folded Corral: drag start collapses a temporarily open Corral at once (140 ms IN) and a drag never changes the
  stored roll state. After the drop the Corral stays folded and hover or click opening is disarmed until the pointer has left its
  outer rect once, or any click happened. Reason: the pointer is still on the title where the user dropped it, so the Corral
  would otherwise spring open under their hand. If the drop made it dock and `RollupOnDock` is on, it folds with the close
  animation; if the drag took a dock-folded Corral away from the edge, it unfolds with the open animation.
- Snapping and neighbour tests use the folded rect (grid spec 4.1), never the animated one.

## 4. Clear view fade

Applies to a Clear view toggle (double-click on the empty desktop), the idle auto-hide, and the auto-show cases (`AutoShow`,
`AutoShowOnDrag`). `QuickHideWhat` decides who is affected: Corrals, loose icons, or both. Corrals marked "Keep visible in
Clear view" are never faded. Start with icons visible (`AutoShowOnBoot`) and the load state apply instantly.
- Out: 150 ms IN. In: 220 ms OUT. Reason: the user asked for a clear desktop, so it clears quickly; the return is gentler and gives
  the eye time to find things.
- The fade is one opacity factor on the whole Corral window (title, body, icons, scrollbar). It multiplies the Corral's own
  see-through and Camouflage factors; none of them is changed. At 0 the window is hidden (`SW_HIDE`) to stop compositing; before
  fading in it is shown at 0 first.
- Loose icons belong to the desktop and may not be fadable through any public API. Recommendation: they switch at the end of the
  fade-out and at the start of the fade-in, so the Corrals always fade over a state that has no gap. Question 7.
- Input: a fading-out Corral is already non-interactive, and clicks fall through to the desktop, so the user can double-click
  again to bring everything back. A fading-in Corral takes input from its first frame. A second toggle during a fade retargets from
  the current opacity (no jump, duration scaled by the remaining distance, as in section 3).
- Hover-open of Fold, hover effects and tooltips are suppressed while the target is hidden.

## 5. Ranges transition

Setting: the existing Animation choice, `swipe_animation_style` 0 Off, 1 Slide, 2 Bounce.
- Model: one scroll position s (in Ranges, a real number). Range i is drawn at x = (i - s) x W, where W is the work-area width of
  each monitor; only Ranges with |i - s| < 1 are drawn, the rest are hidden. Switching from Range a to b animates s from the
  current value to b. Each monitor's Corrals are clipped to that monitor, so nothing shows on a neighbour screen.
- Direction: a higher Range number is to the right, so the content travels left (the old page leaves left, the new one enters
  from the right); a lower number is the mirror. The UI is not mirrored for any language. A jump over several Ranges slides once.
  Reason: it matches pulling the desktop sideways.
- Slide: 240 ms OUT. Bounce: 360 ms BOUNCE (the new page overshoots by about 3 % of W and settles back). Reasons: a full-width
  travel needs more time than a local move, but staying under 300 ms keeps repeated Alt+Arrow usable; Bounce needs time for the
  overshoot to be seen.
- Interrupting: a new switch retargets s from where it is. Duration = base x clamp(|distance|, 0.5, 1.0), so a long retarget is
  not slower than a normal one, and a tiny reversal is not a snap. Position is continuous; only velocity may change.
- Input: during the transition hit tests use the target Range only; the leaving Range is click-through. The next switch gesture
  is accepted at once.
- Off, or reduced motion: swap instantly (the new Range appears at s = target in one frame).

## 6. Icon position animation

Setting: `IconAnimationDisabled`. Absent or 0 = animate (fresh default). The Personalization toggle "Animate icon movement" writes
its inverse. Global only.
- When it applies: an icon changes cell because of a sort, a reflow after a Corral resize ends, an item added to or removed from
  a Corral (the others close ranks), a Round-up move, a drop that rearranges, or an icon released after a drag (it glides from the
  drop point to its cell).
- When it does not: first paint, a new Corral, scrolling, Ctrl+wheel zoom, DPI or monitor changes, restores, the Corral being
  moved as a whole, and every frame of a live resize drag (icons follow the reflow directly). Reason: continuous input must not lag.
- Timing: 180 ms OUT for every icon, all together, no stagger. Reason: long enough to follow where an icon went, short enough that a
  200-icon sort is not slow; stagger would make large sorts take seconds.
- Only icons whose old or new cell is in the visible area glide; the others jump. An icon that moves again mid-glide starts from
  its current drawn position (section 3 retarget rule). Labels travel with their icons. Hit tests use the target cell.
- Effective on = `IconAnimationDisabled` is 0 and Windows animations are on (section 7).

## 7. Reduced motion

Read `SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION)` at startup and on `WM_SETTINGCHANGE` with `SPI_SETCLIENTAREAANIMATION`;
keep it in an atomic. False means "no animation". A motion service answers `duration(base) -> 0` and running animations finish
immediately at their target.

| Item | With Windows animations off |
|---|---|
| Fold open/close | The rect switches in one frame; hover delay, 300 ms leave rule and disarm-after-drag stay. |
| Clear view | Opacity switches to 0 or 1 in one frame. |
| Ranges | Instant swap, even if the choice says Slide or Bounce; the Animation row shows a Caption "Windows animations are off." |
| Icon glide | Icons jump to their cells. |
| Camouflage real-colour fade, scrollbar fades | Instant (the scrollbar spec already says so; Camouflage 2 follows it). |

Setting values are never rewritten. Everything that is not motion (delays, state, order, hit tests) is unchanged.

## 8. Frame pacing

- Progress is always computed from a monotonic clock (`QueryPerformanceCounter`) and the animation's start time, never from tick
  counts, so a late frame only costs smoothness, never duration. Late frames are skipped, not replayed.
- Pacing is a `WM_TIMER` of 16 ms. A window owns at most one animation timer (one id), started when its first animator becomes
  active and killed when none is. That timer advances all animators of the window (`AnimationSet::tick`): fold, icon glide, hover
  fades, scrollbar fades, and pending deadlines (hover delay, leave grace). One repaint per tick.
- Animations that span windows (Clear view, Ranges) run on one coordinator timer on the engine's message window. In a tick it
  updates every affected window from the same clock value and moves them in one `BeginDeferWindowPos` batch, so Corrals never move
  out of step. Those windows start no timer of their own for that animation.
- No timer runs when nothing animates; hidden windows and a locked session finish at once.
- Why a timer, not vsync: it matches the scrollbar spec, needs no change to the paint path, and costs nothing when idle. Cost:
  about 60 updates a second even on 120 or 144 Hz screens, and `WM_TIMER` is low priority, so a tick handler stays under about
  4 ms. A vsync-paced replacement (swap-chain present or `DwmFlush`) can slot in behind `AnimationSet` later. Question 12.

## 9. Pure acceptance tests

Sorting (fake collator unless marked platform; `CompareStringEx` cases run on Windows only and log, not fail, on NLS differences).
1. Natural: [file10, file2, file1] gives file1, file2, file10; descending the reverse; [v1.10, v1.9] gives v1.9, v1.10;
   [file02, file2] ties and the ordinal tie-break gives file02, file2; a 40-digit run does not panic and is antisymmetric.
2. Case: [banana, Apple, cherry, apple] gives Apple, apple, banana, cherry in both directions of Name (ties stay ascending).
3. Non-ASCII (platform): en-US [Ala, Ąbc] gives Ąbc, Ala and [Zebra, Żaba] gives Żaba, Zebra; pl-PL gives Ala, Ąbc and
   Zebra, Żaba; [Lato, Łódź] gives Lato, Łódź in both; the same inputs through the fake collator are locale independent.
4. Symbols: [.env, 1.txt, a.txt, _x] puts punctuation first, digits next, letters last (platform).
5. Groups: folders and system items first for every key and both directions; folders sort among themselves by the key.
6. Size: [10, 2, 0, unknown] ascending 0, 2, 10, unknown; descending 10, 2, 0, unknown; equal sizes by name ascending.
7. Date: boundary instants, equal instants, unknown last; descending newest first.
8. Type: group order, then collator on type text, extension fallback ("XYZ File"), no-extension "File", cache hit equal to miss.
9. Total order: for random lists, `cmp` is reflexive, antisymmetric and transitive; any shuffle of one list yields identical output.

Easing and tweens.
10. Every curve: `ease(0) == 0` and `ease(1) == 1` exactly; input is clamped outside 0..1; OUT, IN and INOUT are monotone
    non-decreasing over 1001 samples; OUT(0.5) > 0.5, IN(0.5) < 0.5, INOUT(0.5) == 0.5 (within 1e-4);
    IN(p) == 1 - OUT(1 - p) within 1e-4. BOUNCE: ends at 1, maximum between 1.02 and 1.04, reached between 60 % and 75 % of time.
11. Duration scaling: a zero-duration tween is at its target on the first read; retarget duration = base x remaining distance.
12. Interruption continuity: retarget at p = 0.0, 0.4, 0.9 (fake clock) for Fold, Clear view, Ranges (clamp rule) and icon glide;
    the value just before and just after the retarget differ by under 1e-6; retargeting every frame converges to the target and
    stays within 0..1 for monotone curves (BOUNCE: within 1.05).
13. Fold: effective-fold derivation (roll, `RollupOnDock` x dock flags); dwell-delay cancel; leave grace restart and cancel;
    the three exclusions of the grace rule; disarm after drag until pointer-out; double-click persistent fold does not re-open;
    animation never writes: stored roll and sizes byte-identical after open/close cycles.
14. Clear view: out then in at 0.5 of the out time lands at the current opacity first; the input state follows the target at each
    instant; exempt Corrals never leave 1.0.
15. Ranges: s model positions for i - s in -1..1; direction signs; the clamp 0.5..1.0 duration; hidden when |i - s| >= 1.
16. Icon glide: displayed = lerp(from, target, ease(p)); a second move continues from the displayed position; resize-drag and
    scroll produce no glide; `IconAnimationDisabled` 1 produces none.
17. Reduced motion: with the flag off every `duration` is 0, tweens read their target at once, delays still apply, and flipping the
    flag mid-animation finishes it; no stored value changes.
18. Pacing: one timer id per window regardless of animator count; the timer is requested when the first animator starts and
    released when the last ends (fake clock and fake timer API).
Live checklist: 60/144 Hz feel, mixed-DPI clip during Ranges, Clear view over a busy wallpaper, hover Fold near screen edges,
sort of 200 icons, Windows "Show animations" toggled while running.

## 10. Open questions for the product owner

1. "Date" means date modified; offer created or accessed? Recommendation: modified only.
2. Folders always first, both directions, no option? Recommendation: yes, matches Explorer.
3. Natural order and case-insensitivity follow the user's locale; exact `StrCmpLogicalW` parity instead? Recommendation: locale
   with `CompareStringEx`, accept rare edge differences.
4. Hover-delay default 400 ms when `RollupRequireDelayTime` is absent. The lead must confirm the unit (ms assumed). Recommendation: 400.
5. Leave grace of 300 ms before a temporary open closes. Recommendation: 300.
6. A fold caused only by `RollupOnDock` is derived and undone by undocking. Recommendation: yes; no extra stored state.
7. Loose desktop icons cannot be faded; switch at the fade edges (recommended) or do not fade them at all?
8. Ranges travel one monitor width, clipped per monitor. Recommendation: yes; confirm Ranges are per desktop, not per monitor.
9. Bounce overshoot of about 3 %: right strength? Recommendation: keep; tune after a live look.
10. Show the Caption "Windows animations are off." on the Ranges Animation row? Recommendation: yes.
11. Lead: is `IconAnimationDisabled` the same key as the redesign's `AnimateIcons`? Assumed yes, inverted. In click-open Fold mode,
    a title click also means rename (Welcome text): recommendation, a click on a folded title only opens; rename works when open.
    (Resolved 2026-10-29: a title click never renamed; the Welcome text now points to Rename Corral in the Corral menu.)
12. 16 ms `WM_TIMER` now, vsync-paced later? Recommendation: timer for this version.
