# Colour state: global and per-Corral colours (design spec)

Status: approved by the user 2026-10-20 (open questions resolved as the spec recommends)
Date: 2026-10-20
Author: Claude subagent (clean spec writer), inputs: spec-inputs-wave-b-2026-10-20.md + approved Corral design docs + public docs only

Terms: "Corral" includes Folder Gates. "Look" = the colour-state fields listed in section 2.1. "Own" = a Corral has its
own stored value; "inherit" = it follows the global value. UI strings are US English ("color"), prose is not.

## 1. Goal and non-goals

Goal: one explicit, testable model for every colour-like setting that exists both globally and per Corral, so that the
settings colour popover, the live preview, the Reset dialogs and the file format all agree on what "own" and
"inherited" mean, and no edit, cancel, failed save, deleted Corral or restore can leave a half-written or stuck look.

Non-goals:
- No separate "Edit Background Color" window (retired). The Corral menu command opens the Personalization page.
- No new colour features, no per-field "use shared" controls, no per-Corral Camouflage strength (camouflage-spec, open question 4).
- No change to how Camouflage derives palettes (camouflage-spec 3), only to which inputs it receives.
- Font weight, italics and tab style get a state model here (2.1) but their editing UI is outside this spec.

## 2. Model

### 2.1 Fields

Every field is a `Slot<T>` per Corral: `Inherit` or `Own(value)`. Globals are plain values with built-in defaults.

| Field (logical) | Global key | Per-Corral | In popover/Reset |
|---|---|---|---|
| background colour | `Color` | `color` | yes |
| text colour: `Auto` or `Custom(rgb)` | `TextColor` + new `TextColorMode` | `text_color` + `text_color_mode` | yes |
| see-through | `TransparencyFactor` | `see_through` | yes |
| tint on/off, tint source, tint colour | `DesktopTintIcons`, `...ColorSource`, `...Color` | `tint_icons`, `tint_icons_color_source`, `tint_icons_color` | yes |
| Camouflage icons / backgrounds | `DesktopBlendIcons`, `DesktopBlendCorralBackgrounds` | `blend_icons`, `blend_background` | yes |
| font weight, italics | (existing) | optional | no |
| tab style | `TabsStyle` | optional | no |

"Existing key names stay" applies to every per-Corral field above whose current name differs from the logical one
(open question L1). Only `TextColorMode` and `text_color_mode` are new keys. Everything else on the Personalization
page (blur, corners, outline, titlebar visibility and shade, Advanced) is global only; with one Corral selected those
rows stay disabled with "Applies to all Corrals." as already specified.

Explicit modes, never colour tricks: text colour Auto is `text_color_mode = "auto"`, not a special colour value;
"inherit" is an absent key, never a sentinel such as 0, -1 or a transparent colour; tint source is the string key of
camouflage-spec 5.2. Tint Off / Match Corral / Custom is a view over two fields (`tint_icons`, source) and is always
written as that pair in one edit.

### 2.2 Resolution and "own look"

    effective(corral) = for each field: corral.slot is Own(v) ? v : global
    Match-Corral tint colour = effective(corral).background colour       (not the raw global)
    has_own_look(corral) = any field of the five resettable groups (rows 1-5) is Own

`has_own_look` is the single source for the "own look" menu suffix, the scope caption (N Corrals, titles, "and N
more") and the Reset dialog. After Reset "<title>" it is false for that Corral (invariant, tested).

Resolution is one pure function `resolve(&Globals, &Overrides) -> Effective` (also reporting Own/Inherit per field),
called by the engine, the live sample and Camouflage; nobody re-implements the fallback.

### 2.3 Scope and tabs

The settings page edits one **scope**: All Corrals, or one Corral identified by its stable id (never by title or
index). Scope All writes only global keys. A Corral scope writes only that Corral's slots. Neither scope ever writes
the other's keys, except Reset (section 4).

A tab is a Corral with its own slots. Joining or leaving a tab group moves no colour state: state travels with the
Corral id. A tab group's shared body paints with the effective look of the active tab; with `TabsColors` on each tab
header uses its own Corral's effective colour. The scope picker lists every Corral, including hidden tabs; editing a
hidden tab changes its slots and the sample, and the desktop shows it when that tab becomes active.

## 3. Editing flow

### 3.1 Entry points

- Corral menu, its colour command (label "Colors…", also on Folder Gates): bring the settings window forward (create it
  if needed), select Personalization, set the scope to that Corral, scroll the Background "Color" row into view and open
  the Color popover under its Custom swatch. If a popover session is already open it is committed first (3.4), then the
  scope changes.
- Personalization page directly: scope picker, swatches, popover as in the redesign spec section 4.

### 3.2 Preview

Every edit is previewed before it is stored. During a gesture (slider drag, popover session, eyedropper move) the page
sends a **draft overlay** `{session id, scope, field, value}` through the existing 16 ms live colour sender. The engine
and the live sample resolve with the overlay on top of stored data: scope All replaces the global value (inheriting
Corrals follow, Corrals with an own value do not); a Corral scope replaces only that Corral's slot. Match-Corral tint
and Auto text colour are recomputed from the drafted background. The overlay is never persisted, one overlay exists at a
time, and the engine drops it when its session ends, when the settings window closes or its channel drops, or when a
message with a different session id arrives. Dropped live frames are harmless: commit uses the page's latest draft
value, never the last value that was sent.

### 3.3 The Color popover session

- Opening writes nothing. The draft starts at the effective value; the slot state at open (`Inherit` or `Own(v)`) is
  remembered.
- The slot becomes `Own` on the first **explicit choice**: any slider move that changed the value, a valid hex entered
  and confirmed with Enter or focus loss, a preset swatch click, or an eyedropper pick. Returning the draft to its
  starting value afterwards does not turn it back into inherit.
- Hex field: accepts `RRGGBB` with or without `#`; an invalid string keeps the last good draft, marks the field, and
  is never stored.
- Eyedropper (already in the redesign spec): a pick is one explicit choice. Esc while picking ends the pick and restores
  the draft from before it; the popover stays open.
- In a Corral scope with an inherited field, a Caption under the popover header reads "Using the color all Corrals
  share. Changing it gives this Corral its own." (hidden once the field is own).
- Text colour: the Auto swatch is an explicit choice of `Auto`; for a Corral it stores `text_color_mode = "auto"` as an
  own value, which differs from inheriting a global Custom colour.

### 3.4 Commit and cancel

There is no OK/Cancel (redesign spec). Rules:
- **Close** (click outside, close button, opening another popup, window close, menu retarget): commit the draft if the
  session had an explicit choice, otherwise write nothing and do not touch any file.
- **Esc**: cancel. Discard the draft, clear the overlay, and leave stored data untouched. The slot is exactly what it was
  at open, so a cancelled session on an inherited field leaves it inherited, not "own with the old colour".
- Non-popover rows (swatch click, slider, segmented, eyedropper button on the row): preview during the gesture, commit
  once at its end (release, click, Enter).
- Commit is one atomic edit: build the new settings copy, write with temp file plus rename, then swap it in and send
  the reload request. If the write fails, keep the previous in-memory state, clear the overlay so the desktop shows the
  stored look again, and show a Caption in status/danger on the page: "Couldn't save. Your previous colors are back."
  No message box. A later edit tries again normally.

## 4. Reset scopes

These match the redesign spec's two dialogs; nothing resets without one.

| Action | Dialog | Effect |
|---|---|---|
| `ResetBase` | "Reset all Corrals…", default option "Keep Corrals with their own look" | Global `Color`, `TextColor`/`TextColorMode`, `TransparencyFactor` and `DesktopBlendCorralBackgrounds` return to built-in defaults. `DesktopTintIcons` is turned off only if the global tint source is `"corral"`. No Corral slot is touched. |
| `ResetAll` | same dialog, option "Reset every Corral" | `ResetBase`, then every Corral loses all slots in rows 1-5 of 2.1. |
| `ResetGroup` | "Reset \"<title>\"…" | That Corral loses all slots in rows 1-5 (color, text colour with mode, see-through, the three tint keys, `blend_icons`, `blend_background`). Globals and other Corrals untouched. |

Never reset by any action: blur, corners, outline, titlebar options, Advanced settings, Camouflage master switch and
strength, font weight, italics, tab style, geometry, membership. With no Corral having an own look the dialog shows no
choice and the action is `ResetBase` (equal to `ResetAll`). Each action is one atomic edit as in 3.4; `ResetAll`
touches two stores if the lead's layout puts per-Corral data in a second file (L3): write both, and if the second fails
write the first back from the in-memory pre-image, then show the 3.4 error. Reset closes any open popover session
without committing it (the user confirmed the reset after the session ended, so the order is: commit session, then
dialog). Reset cannot be undone in the app; Backups is the way back (open question 6).

## 5. Storage and migration

- **Absent = inherit; present = own.** Per-Corral keys are written only when the user makes an explicit choice (3.3) and
  removed only by a Reset. Opening, viewing, switching scope, loading and saving unrelated settings never add or
  remove a per-Corral colour key.
- **A value equal to the inherited one stays own** if the user set it. Nothing normalises, prunes or deduplicates on
  load or save. Consequence, stated in the release notes: a Corral with an own colour does not follow later changes
  to the shared colour, even if the two once matched.
- **Global keys**: absent = built-in default (one table in code). Setting a global to its default value writes the key
  (the value is the user's choice); only Reset removes it.
- **Text colour**: on load, if `TextColorMode` (global) or `text_color_mode` (per Corral) is present it decides. If
  absent: per Corral, a present `text_color` is `Custom` and absent is inherit; global, a present `TextColor` is `Custom`
  and absent is `Auto`, except values the lead lists as legacy "auto" encodings (L2), which migrate to `Auto`. The new
  key is written on the next normal save of that scope; no legacy sentinel is ever written again.
- **Tint source** migrates exactly as camouflage-spec 5.2 (including its amendment), comparing the stored tint
  colour with the Corral's effective background colour. Per-Corral values migrate independently; absent inherits.
- Saves preserve unknown fields and current colour encodings; files are not rewritten at load. Older builds ignore the
  new keys (Auto text colour for a build that expects a sentinel: L2).

## 6. Edge cases and acceptance tests

Tests are pure (settings structs and the resolver; no window). Each bullet is at least one test.

**Resolution and storage**
- Absent slot resolves to global and reports `Inherit`; present slot equal to global resolves identically and reports
  `Own`. Load then save leaves absent keys absent and equal present keys present, byte-identical when nothing changed.
- Own `Auto` text beats a global `Custom`; inherited text follows a global change; own colour ignores a global change.
- Match-Corral tint uses the Corral's effective background (own or global), and updates when either changes.
- `has_own_look` is true for any one of rows 1-5, false for font/tab-only overrides; caption text lists at most 3 titles.

**Editing**
- Open then Esc, open then close, and open then slide back to the start without choosing: no write, settings
  byte-identical. A slider move then Esc on an inherited field leaves it `Inherit`.
- Explicit choice of a value equal to the inherited one (swatch click, hex Enter) writes `Own`; the slot stays `Own`
  after a later global change; nothing converts it back.
- Scope All edits only global keys; scope Corral edits only that Corral. Tint mode Off / Match / Custom writes
  `tint_icons` plus source together or not at all.
- Live frames: 100 drafts at 1 ms spacing through the throttle; commit stores the final draft even if the last frame
  was coalesced away. Hex parsing accepts `RRGGBB` / `#RRGGBB` in any case; `#GGG`, empty, 7 digits are rejected.

**Failure and lifecycle**
- Failed save (injected write error): stored and in-memory settings equal the pre-edit state, overlay cleared, error
  flag set, next edit succeeds. A multi-key edit is never half applied; `ResetAll` over two stores restores the first
  on a second-store failure.
- Target Corral deleted mid-session (popover open or mid-drag): session cancelled, no write to any Corral or to the
  globals, overlay cleared, scope falls back to All Corrals with the Caption "\"<title>\" was removed. Showing all
  Corrals.". A commit that arrives for a missing id is a no-op and does not create a record.
- Switching target (scope picker, or the menu command of another Corral) commits the current session to its own target
  first; the draft never moves to the new target and never leaks into the globals. If that commit fails, the error
  shows and the scope still changes.
- Snapshot or backup restore, or a settings reload from disk, during a session: the session is cancelled without
  commit and the overlay cleared before the new state is applied. The scope is kept if the Corral id still exists in the
  restored data, otherwise it becomes All Corrals with the same Caption. Restored data always wins; no draft is written
  over it.
- Tabs: joining a group, leaving it, or reordering tabs leaves every Corral's slots unchanged; the group body resolves
  from the active tab; editing a hidden tab changes only its slots; deleting the target tab follows the deleted-target
  rule; merging an own-look Corral with an inheriting one keeps both unchanged.
- Overlay never sticks: ended by commit, cancel, window close, channel drop, stale session id (a test per path).
- Reset: `ResetBase` keeps every Corral slot and applies the tint rule (source corral turns tint off; source custom keeps
  it); `ResetAll` clears rows 1-5 on every Corral and keeps font, tab style, geometry; `ResetGroup` leaves globals and
  other Corrals byte-identical; all three are idempotent; with zero own looks `ResetBase` equals `ResetAll`.
- Migration: legacy absent / present / unknown text values; present per-Corral values stay `Own` even when equal to the
  global; unknown keys preserved; fresh install writes no per-Corral colour key.

**Live checklist**: menu command opens the popover on the right swatch at 100/150/250 % and on a second monitor;
sample, desktop and Camouflage agree during a drag; no stuck overlay after killing the settings process.

## 7. Open questions

For the product owner:
1. Should a single field be revertible to the shared look without a full Reset (per-row "Use shared" action)?
   Recommendation: no for now; Reset "<title>" is the one way back, and own values are visible through the menu suffix.
2. Where are per-Corral font weight, italics and tab style edited, given the approved pages show neither?
   Recommendation: leave them where they are edited today and exclude them from Reset (matches the approved dialog text).
3. Should Reset "<title>" also clear font weight, italics and tab style? Recommendation: no; the approved dialog names
   only color, see-through, text color, tint and Camouflage choice.
4. Does `blend_background` per Corral get UI? Recommendation: no; "Blend Corral backgrounds" stays global, the key stays
   readable and is cleared by Reset.
5. Should the Caption inside the popover for inherited fields (3.3) ship? Recommendation: yes, it is the only place
   the user learns that editing creates an own value.
6. Take an automatic backup (tagged Automatic) before `ResetAll`, since the dialog says it cannot be undone?
   Recommendation: yes, same mechanism as the Round-up proposal; no Undo button.
7. Menu label "Colors…" for the Corral command (and the same on Folder Gates)? Recommendation: yes.
8. A later change to the shared colour does not move Corrals with an own colour, even a coincidentally equal one.
   Recommendation: accept; the scope caption names those Corrals.

For the lead (engineering, I could not read the code):
- L1. Current per-Corral key names and encodings for background colour, text colour and see-through.
- L2. How legacy stores "Auto" text colour (absent, a sentinel value, or both) and the TransparencyFactor range, so
  migration can map it to `text_color_mode = "auto"` without ever writing the sentinel again.
- L3. Are per-Corral slots in the layout file and globals in the settings file? This decides the two-store behaviour
  of `ResetAll` and of snapshot restore (section 4 and 6).
