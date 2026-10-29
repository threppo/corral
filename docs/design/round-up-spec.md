# Round-up: rules that send desktop items into Corrals

Status: approved by the user 2026-10-20 (open questions resolved as the spec recommends)
Date: 2026-10-20
Author: Claude subagent (clean spec writer), inputs: spec-inputs-2026-10-20.md + settings redesign docs + public docs only

## 1. Goal and non-goals

Goal: let the user describe, in sentences, which desktop items belong in which Corral, and have Corral keep to that
without surprises. Every automatic move can be previewed first, explained afterwards, and is never applied to an icon
the user placed by hand.

**Hard invariant: Round-up changes only which Corral an item belongs to.** It never moves, copies, renames, deletes or
writes any file or folder. The evaluator and planner are pure (facts in, plan out); only the existing Corral
membership API applies a plan.

Non-goals: content inspection (file text, metadata, creating program, times opened: not available), regex, nested
groups, scheduled/timer-driven sorting, a location condition (the facts have no parent folder), destinations other than
a Corral or "the desktop", any file operation.

## 2. Rule model

A rule reads as one sentence: **When an item {is a PDF} and {its name contains "invoice"}, put it in {Accounts}.**

Stored fields: `id` (stable), `name` (optional; empty = use the generated sentence), `enabled`, `match` (`all` or
`any`), `conditions` (flat list, at least one), `destination`, optional `preset` id.

**Grouping: one level only.** A rule has one All/Any switch over a flat list. Anything like "(A and B) or C" is two
rules with the same destination, which top-to-bottom order already expresses. Nesting adds a second dimension to the
editor for no real need. With one condition the switch is hidden.

**Destination:** a Corral (by its stable id, so renaming a Corral changes nothing) or **Leave on the desktop**, which
means "this item belongs outside every Corral" and therefore also ends evaluation (an exemption rule). Folder Gates are
not destinations. A destination may be empty only on a disabled rule (an unconfigured preset). Enabling needs one.

### Condition types

All text comparison is ordinal and case-insensitive (simple case folding), with no Unicode normalisation: Windows
treats names the same way. Wildcards exist only in the "matches pattern" operator: `*` = any run, `?` = one character,
everything else literal. There is no case option. Empty text values are rejected by the editor.

| Condition | Operators | Value | Evaluated from |
|---|---|---|---|
| Item type | is, is not | Folder, File, Shortcut, System item | folder / file / virtual / is-shortcut facts. Exactly one type per item, so "File" excludes shortcuts, folders and system items. |
| Extension | is one of, is none of, has none | list of extensions without dot | The text after the last dot of the file name (".gitignore" has "gitignore"; no dot or trailing dot = none). Folders and system items always have none. |
| Name | is, contains, does not contain, starts with, ends with, matches pattern | text | The display name **without** its extension for files and shortcuts; the whole name for folders and system items. Extensions are matched by Extension, so "contains pdf" never matches by accident. |
| Size | is at least, is less than | number + KB / MB / GB (1024-based, as Explorer shows) | The file's size fact. Folders and system items have no size: result **No** (not applicable), reason shown. A shortcut's size is that of the shortcut file itself. |
| Date modified | is newer than, is older than | number + hours / days / weeks / months | Last-write time fact (a UTC instant). See "Time" below. |
| Shortcut points to | is, is not | Program, Folder, File | Program: the existing "target is a program" fact. Folder / File need one extra fact, "target is a folder", from a single attribute lookup of the target path (see open questions). |
| Target extension | same as Extension | same | The target's extension fact. |
| Target path | same operators as Name | text | The target path fact, whole string. |

Applicability: the three Target conditions apply only to shortcuts. On a non-shortcut they evaluate to **No** (even
for "is not" / "is none of"), so "target is not a program" never grabs ordinary files.

**Time.** The clock is read once at the start of a run. "Newer than N" is true when modified >= threshold; "older than
N" when modified < threshold (exact complements). Hours subtract an exact duration. Days, weeks and months subtract
calendar units in the local time zone and then convert to an instant (a non-existent local time moves forward, an
ambiguous one takes the earlier instant; a month step clamps to the last day of a shorter month). A modified time in the
future counts as newer. Date rules are re-checked only when a run happens, never on a timer; the editor says so.

### Three-valued results

Each condition returns **Yes**, **No** or **Could not evaluate**. "Could not evaluate" is returned only when the fact
should exist but could not be obtained: unreadable or unset modified time, size lookup failed, shortcut target missing
or unresolvable (offline share, broken link), "target is a folder" lookup failed. "Not applicable" is a definite No and
is explained in the preview, but is not an error. Negative operators never turn Could-not-evaluate into Yes.

## 3. Evaluation

**Combining.** All: any No gives No; else any Could-not-evaluate gives Could-not-evaluate; else Yes. Any: any Yes gives
Yes; else any Could-not-evaluate gives Could-not-evaluate; else No. A rule's result is its combined result, so one
No inside an All settles it even if another condition is unknown.

**Rule order.** Enabled rules run top to bottom per item; the first rule that returns Yes decides. If a rule returns
Could-not-evaluate **before** any rule said Yes, evaluation stops for that item: the item is not moved, not marked
seen, and is reported as "could not be checked" with the reason (e.g. "shortcut target could not be read"). It is
retried on the next run. Rationale: skipping would drop the item into a lower rule's Corral only because a fact was
missing. If no rule says Yes and none is unknown, the **default destination** applies when it is a Corral; when it is
"Leave it on the desktop" the item is simply not touched (no opinion), so nothing is pulled out of a Corral just
because it matches nothing.

**Plan.** The result per item is a plan entry: from, to, deciding rule, and the satisfied conditions as text ("is a PDF;
name contains invoice"). For Any, every Yes condition is listed. Applying a plan is a no-op for items already in their
destination, so running twice changes nothing.

**When rules run**
1. **New item** (not yet seen): after it has been unchanged for about 2 seconds (so a download in progress is not
   judged half-written), and only if there is an enabled rule or a Corral default.
2. **Keep Round-up active** (`AlwaysApplyRules`): when a non-manual item changes (renamed, content changed, target
   changed) that single item is re-evaluated. Editing rules never re-sorts anything by itself.
3. **Sort the desktop now**: every non-manual item, through the preview in section 4.
Round-up does nothing while Corral itself is off.

**Item identity.** Items are identified by their desktop name, like the layout already does. A renamed file therefore
counts as a new item. Accepted: it is the honest limit of the identity we have.

**Manual placement.** An item is *manual* when the user changed its membership: dragging it into a Corral, between
Corrals, or out onto the desktop, or choosing a "move to" command. Round-up's own moves, undo of them, backup restores
and Corral resizing are not manual. Manual items are remembered in the layout file (`manual`: item name -> `placed_ms`,
`last_seen_ms`), are skipped by every automatic run and by Sort now unless the preview's checkbox **"Include icons I
placed myself"** (off by default) is ticked. Ticking it and applying clears their manual mark. An entry is pruned when
the item has been absent for 30 days. Context menu on an icon: "Let Round-up decide" clears the mark.

**Seen.** An item is *seen* once Round-up reached a decision for it (moved, or left in place because nothing matched).
Could-not-evaluate items stay unseen. Seen items are re-examined only by Keep Round-up active (on change) or by Sort now.
**First start with an older layout** (`seen_initialized` absent): every item present is marked seen, nothing is moved,
and every item that is currently inside a Corral is marked manual (its membership came from the user or an older
version). Loose items are seen but not manual. After that, only new items are acted on.

**Destination missing.** A rule whose Corral no longer exists is suspended (skipped as if disabled; its row shows
"Corral missing"); the Round-up page shows a warning box ("2 rules point to a Corral that no longer exists"). A missing
default Corral behaves as "Leave it on the desktop" with the same warning. Nothing is deleted; restoring the Corral
(same id) revives the rule.

## 4. Preview

"Run now" opens a dialog (680 wide, same Dialog kit) titled **Sort the desktop now**. It computes the plan with the
rules as saved and shows, as lists inside one card:
- **Will move (N)**: grouped by destination Corral; each row `item name -> Corral` and a Caption line with the deciding
  rule and satisfied conditions: `Accounts: is a PDF; name contains "invoice"`.
- **Could not be checked (N)**: item, rule, reason. They will stay where they are.
- **Skipped, placed by you (N)** and **No change (N)**: counts only; the first has the "Include icons I placed myself"
  checkbox, which refreshes the list.
Footer: Cancel / Primary **"Move N icons"** (when N = 0 the body says "Nothing to move" and the only button is Close).
Applying writes one Corral membership change per item and finishes with a notice if any items could not be checked.
There are no per-row checkboxes: to exclude an item, cancel and adjust a rule.

In the rule dialog a single live Caption under "Send matching icons to" shows how the rule alone would act on today's
desktop: `Matches 4 icons now: a.pdf, b.pdf, c.pdf and 1 more. 2 are taken by rules above. 1 could not be checked.`
It recomputes as conditions change (debounced) and never applies anything.

## 5. UI changes to the approved Round-up page and rule dialog

Page (cards in this order), changes against the redesign spec:
- "New icons" card: unchanged ("When no rule matches").
- **Needs review** card (only when non-empty; section 6).
- **Your rules** replaces both "By file type" and "Your rules": **one ordered list**, check rows with sentence
  description and destination chip. Presets are ordinary rules in this list with a "Built in" Caption; they can be
  edited, reordered, and use "Reset to default" instead of "Delete". Reason: two cards plus a separate priority dialog
  hide the real evaluation order; one list makes top-to-bottom visible. Seeded order: Program shortcuts, Web links,
  Folders, Documents, Images, Music and video, Downloads. A new rule is inserted **above the first built-in rule**, so it
  can win. Unchecking is "enabled = off". Checking a rule with no destination opens the Corral picker first.
- Order is changed with the row context menu "Move up" / "Move down", Alt+Up / Alt+Down on the focused row, and two
  small icon buttons (up/down) on the hovered or focused row. **The "Rule priority" navigate row and dialog are
  removed.** Context menu otherwise as designed: Edit..., Rename..., Delete... (Reset... for presets).
- "Running" card: "Keep Round-up active", "Sort the desktop now" ("Run now"); the priority row is gone. A Warning info
  box appears above it when icons could not be checked or a Corral is missing.

Rule dialog (680): the per-row and/or dropdown becomes a read-only word.
- Line: "An icon matches when" + Segmented **All | Any** + "of these are true:" (with one condition: "this is true:").
- Condition rows: label column shows "Where" on row one and plain text "and" / "or" afterwards (follows the switch,
  not editable); D kind (170) with the eight kinds above; D operator (140); value field / dropdown / number + unit /
  chips for lists; remove (x) when more than one condition. "+ Add a condition" unchanged.
- Under the divider, a live **sentence** (Body): `When an item is a PDF and its name contains "invoice", put it in
  Accounts.` It is also the automatic rule name. Then "Send matching icons to", "Rule name", the match Caption from
  section 4, and the footer exactly as in the redesign spec. No Enabled switch in the dialog (the list has it).
- Invalid input (empty text, missing number) shows Caption errors in status/danger under the field and disables Save.

## 6. Storage and migration

**New typed block** in the settings file, beside the old keys (names readable, `version` first):
```
"RoundUp": { "version": 1, "migrated_from_legacy": true,
  "default_destination": { "kind": "desktop" } | { "kind": "corral", "id": <corral id> },
  "rules": [ { "id": "r-0f3a", "name": "", "enabled": true, "preset": "images" | null, "match": "all",
      "conditions": [ { "type": "extension", "op": "is_one_of", "value": ["pdf"] },
                      { "type": "name", "op": "contains", "value": "invoice" },
                      { "type": "size", "op": "at_least", "value": { "n": 10, "unit": "MB" } },
                      { "type": "modified", "op": "newer_than", "value": { "n": 7, "unit": "days" } } ],
      "destination": { "kind": "corral", "id": 12 } | { "kind": "desktop" } | null,
      "legacy": { "place_at_end": true, "auto_id": "..." } } ],
  "needs_review": [ { "position": 3, "name": "...", "data": "<original>", "target": "<original>", "reason": "..." } ] }
```
`AlwaysApplyRules` stays as the only key for "Keep Round-up active". The layout file gains `manual` (section 3); `seen` and
`seen_initialized` are unchanged. Corral ids use the layout's id type. A block with a newer `version` than the build
knows is kept byte-for-byte, Round-up is paused, and the page says why. Unknown condition types inside a known version
disable that rule and show it in the list as "needs an update".

**Migration policy** (runs once when `RoundUp` is absent; idempotent via `migrated_from_legacy`):
1. Parse each old rule by its outer shape only. Convert it **only if** (a) it parses, (b) every criterion maps to
   exactly one new condition with the same meaning under the definitions in section 2, (c) the criteria are all
   AND-joined or all OR-joined (-> All or Any; a single criterion is trivial), and (d) the target parses. A mixed AND/OR
   chain is never guessed: precedence is unknown, so it is not converted.
2. Converted rules keep their name, enabled flag, order and destination (target -1 = desktop, otherwise the Corral id;
   an id with no Corral stays and shows "Corral missing"). `PlaceAtEnd` is dropped from behaviour (items are added where
   the destination Corral would normally put a new icon) but its value is kept in `legacy` verbatim. A rule whose
   `AutoGeneratedID` is recognised as one of the seven presets becomes that preset with the converted conditions.
3. Everything else (unknown method or operator, mixed joins, zero criteria, a criterion whose old meaning differs from the
   new definition, malformed shape) goes to `needs_review` with the original text verbatim, **disabled and not in the
   evaluation order**, plus a one-line reason and its old position.
4. Missing presets are added in default form, disabled, without a destination, below the user's rules. The existing
   default destination is copied to `default_destination`.
5. The old `rules` array and old default-destination field are left in the file **untouched** while `needs_review` is
   non-empty. The new build only reads them for migration.
6. **Before the first migrated save** the settings file and layout file are copied to
   `<name>.before-round-up.<timestamp>.json` next to them, never overwritten. If the copy fails, the migration does not
   save and Round-up is paused with a visible message.

**Needs review card:** each row shows the old name, the original text as plain Caption, the reason, and buttons
"Rewrite..." (opens the rule dialog prefilled with whatever mapped; Save inserts it at the old position and removes the
entry) and "Dismiss" (confirm; removes the entry). When the list becomes empty the legacy `rules` key is removed; the
timestamped copy remains.

Presets, defaults (editable, shown in the editor; extension lists are suggestions): Program shortcuts (type Shortcut,
target is a program); Web links (extension url); Folders (type Folder); Documents (pdf, doc, docx, xls, xlsx, ppt, pptx,
txt, rtf, odt, ods, csv, md); Images (jpg, jpeg, png, gif, bmp, webp, tif, tiff, heic, svg); Music and video (mp3, wav,
flac, m4a, aac, ogg, mp4, mkv, mov, avi, wmv, webm); Downloads (zip, 7z, rar, iso, msi, torrent).

## 7. Edge cases and acceptance tests

Pure tests with fake `ItemFacts` and a fake clock and time zone; the file-system trait in tests panics on any call.
- **Truth tables** for All and Any over every combination of Yes / No / Could-not-evaluate for 1, 2 and 3 conditions
  (including No plus unknown in All = No; Yes plus unknown in Any = Yes), and negative operators over unknown.
- **First-match order**: a lower rule is not consulted after a Yes; an unknown rule above a Yes halts the item (reported,
  unseen); an unknown rule below a Yes is irrelevant; disabled and suspended rules are skipped; reordering changes the
  outcome.
- **Extension**: `PDF`, `Pdf`, `pdf` all match; multi-dot `a.tar.gz` gives `gz`; `.gitignore`, no dot, trailing dot;
  empty extension with "has none" and "is none of"; folders always none.
- **Name**: stem used for files, whole name for folders; case folding with non-ASCII; wildcard `*` and `?`; literal
  `[` and `.`; empty name stem (`.gitignore`).
- **Shortcuts**: to file, folder, program; unresolvable target gives Could-not-evaluate for all Target conditions;
  non-shortcut gives No on Target conditions even with negative operators; item type Shortcut vs File exclusivity.
- **Size**: exactly at the boundary (at-least true, less-than false), 0 bytes, 1 KB = 1024, folder = No (not
  applicable), lookup failure = unknown.
- **Date**: boundary instant (newer true, older false), 1 ms either side, future timestamps, unset timestamp = unknown,
  month clamp (Mar 31 minus 1 month), a spring-forward gap and an autumn overlap in a DST zone, a UTC-only zone and a
  zone with a positive offset, hours vs days across a DST change.
- **Destination**: deleted Corral suspends the rule and warns; revive on same id; deleted default Corral; destination
  "desktop" pulls a non-manual item out of a Corral and leaves a loose item alone; missing destination blocks enabling.
- **Manual placement**: dragged-in item stays after Run now and Keep active; the checkbox includes it and clears the
  mark; pruning after 30 days; Round-up's own move does not mark manual.
- **Seen / first start**: old layout marks everything seen, Corral members manual, moves nothing; a new item afterwards is
  evaluated; unknown results stay unseen; a rename is a new item.
- **Idempotence**: apply a plan, re-plan: empty plan; Keep active on an unchanged item does nothing.
- **Roundtrip**: save, reload, save yields identical JSON for every condition type; unknown newer `version` preserved
  byte-for-byte; unknown condition type preserved.
- **Migration**: simple single criterion; all-AND; all-OR; mixed AND/OR (-> review, text verbatim); unknown method;
  zero criteria; malformed outer shape and missing fields; target -1 and a missing Corral id; a recognised preset;
  idempotent second run; old keys untouched while review items exist; backup file written before the first save and
  save aborted when the copy fails.
- **Never a file operation**: the planner/applier crates have no file-system write access (test double plus a source
  review rule); a full Sort now over a fake desktop leaves the fake file tree equal.
- **Preview**: the plan shown equals the plan applied; counts add up (move + unchecked + skipped + unchanged = items).

## 8. Open questions for the product owner

1. Halt an item at a Could-not-evaluate rule (recommended) or skip that rule and continue?
2. New rules go above built-in rules (recommended), or at the end of the list?
3. Take an automatic layout backup (Backups page, tagged Automatic) before an applied Sort now? Recommend yes; it also
   serves as undo, so no separate Undo button.
4. Add the one extra fact "shortcut target is a folder" so that Folder / File targets work? Recommend yes; without it only
   Program is offered and "Shortcut points to" shrinks to a program yes/no.
5. Does the existing "is a shortcut" fact cover internet shortcuts (.url)? If not, "Web links" matches by extension and
   type Shortcut covers only .lnk. Recommend leaving as facts report it.
6. For migration: did old name criteria see the extension? If they did, those criteria go to review (recommended)
   unless equivalent under the new stem-only definition.
7. Drop the "place at end" choice (recommended) and keep its value only in `legacy`?
8. Confirm the preset extension lists and the seeded preset order in section 6.
9. Mark all items already inside Corrals as manual on first start with an older layout (recommended), or leave them
   open to Sort now?
10. Accept that a renamed file counts as a new item (recommended), or invest in a stronger item identity later?
11. Offer a location condition (user desktop vs shared desktop) later? Recommend not now; the facts do not include it.
12. Date rules checked only on runs, never on a timer (recommended)? A daily check at midnight can be added later.
