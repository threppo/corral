# Folder Gate views: which view a Gate shows, and when Corral remembers it (design spec)

Status: approved by the user 2026-10-20 (open questions resolved as the spec recommends)
Date: 2026-10-20
Author: Claude subagent (clean spec writer), inputs: spec-inputs-wave-b-2026-10-20.md + approved Corral design docs + public docs only

Terms: "folder key" = a folder path normalised as in 3.1. "Live view" = the Windows view hosted by the Gate
(`IExplorerBrowser` -> `IFolderView2`, columns through `IColumnManager`). "Baseline" = the view state read back right
after Corral applied a preference (or after Windows built the view when nothing was applied).

## 1. Goal and non-goals

Goal: a Gate shows each folder the way the user last set it, a folder the user never touched looks calm and the
same everywhere, and Corral never records a view the user did not choose.

Non-goals:
- The Windows view control stays. Corral does not draw items, columns or headers, and does not reorder files.
- No new view features (grouping, filters, saved named views, per-Gate overrides of a folder's view).
- No change to what "Allow navigation" or the first folder (`nav_history`) mean. This spec decides only the
  preference policy: resolve, apply, save, migrate.
- No file or folder is touched. A view preference is only a record about how to display a path.

## 2. What a view preference is

One entry per folder key, in three independent **groups**. A group is either present in the entry or absent
("inherit/default"). Groups are resolved separately (section 3) and written whole.

| Group | Fields | Public API used to apply and read |
|---|---|---|
| Style | mode, icon size, folder flags | `SetViewModeAndIconSize`, `SetCurrentFolderFlags` (and getters) |
| Sort | column + direction | `SetSortColumns` / `GetSortColumns` |
| Columns | ordered visible columns, each with a width in DIPs | `IColumnManager::SetColumns`, `SetColumnInfo`, `GetColumnInfo` |

- **Mode**: a `FOLDERVIEWMODE` value. Accepted: 1 icon, 2 small icon, 3 list, 4 details, 5 thumbnail, 6 tile,
  8 content. Anything else (0, -1 auto, 7 thumbnail strip, unknown) means the style group is absent.
- **Icon size**: `Follow` (the folder uses the current desktop icon size), `Fixed(px)` (16..=256, clamped on read), or
  absent. It matters only in icon-type modes (1 and 5); in other modes it is carried unchanged, never applied.
  Stored: negative = Follow (the magnitude is ignored on read; on write it is the pixel size resolved at that moment, so
  an older build still shows something close), positive = Fixed, 0 = absent (then Follow).
- **Folder flags**: the per-folder `view_flags` bits (header hidden and similar). Distinct from the per-Gate
  `view_flags` (Allow navigation); the names collide in storage, so code calls them `folder_flags` and `gate_flags`.
  Only the bits whose meaning is confirmed (open question 2) are applied; unknown bits are preserved on rewrite and
  never interpreted. Flags belong to the style group: they are meaningful only together with a mode.
- **Sort**: signed 1-based column, 1 Name, 2 Size, 3 Type, 4 Date modified, 5 Date created, negative = descending. These
  map to the canonical properties `System.ItemNameDisplay`, `System.Size`, `System.ItemTypeText`, `System.DateModified`,
  `System.DateCreated`. Stored 0 = "no preference" (Windows decides); it is never applied as a sort.
  A live sort on any other property is not representable: the sort group is then not written (open question 10).
- **Columns**: list of (property key, width in DIPs). The Name column is always visible and always first: it is inserted
  when missing. Width 0 or absent = the column's default width. A width is clamped to 24..=1200 DIPs on read.
- **`set`**: true when the user explicitly configured the folder. An entry whose `set` is false is ignored by the resolver
  (it is kept in the file), because it can only have come from something other than a user choice (open question 6).

## 3. Resolution for a Gate showing folder F

`resolve_view(F, store, inherit, ctx) -> ResolvedView`, pure. For each group, in this order, the first source that has
the group wins; the result also records the source (`Own`, `Inherited(from key)`, `Default`) for the tests and logs.

1. **Own**: the entry for F, when `set` and the group is present.
2. **Inherited**, only when `FolderPortalsInheritViewStateFromParent` is on: the same group of the *nearest ancestor of F*
   whose entry is `set` and has that group. Groups are looked up separately, so a child that only changed its sort still
   inherits its parent's style and columns, and keeps following later changes to the parent in those groups.
3. **Default** (a Gate that never had a preference): Style = icon view (mode 1), icon size `Follow`, no flags.
   Sort = Name ascending (1). Columns = absent (Windows' default columns for that folder type).

Why these defaults: a Gate sits among desktop icons, so an icon grid at the desktop icon size looks like the rest of the
desktop and reads best in a small rectangle; Name ascending is predictable; Details is wide and needs the user to size
columns, so it is something the user picks. Corral defaults win over Windows' own remembered view for the folder
(`IExplorerBrowser` otherwise lets the shell's saved state decide, which differs by folder type and flickers). Explorer's
saved state is neither read nor written by this feature.

3.1 **Ancestors and keys.** The key is the path with `\\?\` and `\\?\UNC\` prefixes mapped to plain form, `/` turned into
`\`, trailing separators removed (except for a root), compared by simple one-to-one Unicode case folding, ordinal. Parents
are removed one component at a time and stop at the root: `C:\`, or `\\server\share` (a share root has no parent; `\\server`
is never a key). Paths are not resolved against the disk (no symlink, junction, 8.3 or existence lookups), so a missing or
disconnected folder behaves the same. Non-filesystem locations (a shell namespace path) have no ancestors.

3.2 **Apply.** The resolved view is applied in the order Style, Flags, Sort, Columns, as early as `OnViewCreated`, with
the initial mode also passed through `SetFolderSettings` so the first paint is already right. Each call is verified by
reading the live view back. A failed call leaves that group at what Windows built; the others still apply. The baseline
is whatever was read back. Stored Fixed icon size scales with the Gate's monitor DPI (DIPs, see section 5 and question 3);
Follow resolves to `ctx.desktop_icon_px` at apply time and is re-applied when the desktop icon size changes.

## 4. When preferences are saved

**Save only a user change, as a patch.** A save writes the groups that changed, sets `set = true`, and leaves every other
field of the entry (and unknown fields) untouched. Nothing is saved because a folder was displayed, inherited, defaulted,
navigated to, or re-laid-out by a DPI change.

State per Gate: `Navigating` -> `Applying` -> `Settled(F, baseline)`. Only in `Settled` may a change be recorded.

- Triggers for a check (idempotent compare of the live view with the baseline, per group): immediately after a Corral
  view command (mode, size, sort, header, columns); 300 ms after the last pointer-up, key-up or Windows menu command
  inside the view (this catches header clicks, column drags, Windows' own View and Sort menus, Ctrl+wheel); and the
  flushes below. There is no periodic timer while idle.
- A group is written only if the live value differs from the baseline: size differs, sort differs, column list or any
  width moved by 2 device pixels or more. A `Follow` size stays `Follow` unless the user picked another size. After a
  write the baseline is updated, so the same change is never saved twice.
- **Flush before navigating away.** The check runs, and its patch is written, before the next folder is resolved:
  in `OnNavigationPending` for navigations started inside the view (opening a subfolder), at the start of Corral's own
  Navigate/Back/Forward/Up, and when the Gate is closed or removed, the layout reloads, or Corral exits. A flush that finds
  no difference does nothing. Because the flush happens first, a subfolder resolving by inheritance sees the parent's
  newest preference.
- **Never from a transient Windows default.** While `Navigating` or `Applying`, and until the apply read-back is stored as
  the baseline, no check runs. A folder that has no preference shows Windows' state for a moment before apply; that is
  never recorded. If an apply failed, the baseline is the read-back, not the preference, so no echo is saved. A failed
  navigation (`OnNavigationFailed`) keeps the previous `Settled` state.
- For 500 ms after a DPI change of the Gate's monitor, a re-apply, or a Gate resize that triggers a re-layout, checks are
  suppressed; the baseline is retaken afterwards (section 5).

**Two Gates on one folder** share the entry, so they share the view:
- Writes are per-group patches, so Gate A changing the sort and Gate B changing the mode never overwrite each other with
  stale values. If both change the same group, the later write wins.
- When the store changes for the folder a Gate is showing, the Gate re-applies the changed groups (debounced 150 ms) and
  takes the read-back as its baseline, which cannot cause a save. A Gate with its own unsaved change (pointer down in the
  header, an open Windows menu) skips the update and wins when it saves.
- A Gate showing a *subfolder* is not updated by a change to its parent unless it inherits that group.

A Corral-side command "Reset view" (open question 9) deletes the entry's groups, so the folder falls back to inheritance
or the defaults.

## 5. Column widths across DPI

- Storage unit is the DIP (1/96 inch). `px = round(dip * dpi / 96)` when applying; `dip = round(px * 96 / dpi)` when saving.
  For dpi >= 96 the round trip `dip -> px -> dip` is exact for every integer DIP (test it at 96, 120, 144, 168, 192, 240, 288, 384).
- A column whose live width is within 1 device pixel of what Corral applied keeps its stored DIP value exactly. Only moved
  columns are converted. This removes rounding drift from repeated load/save at different scales.
- Moving a Gate to a monitor with another DPI re-applies widths from the stored DIPs (the view is not trusted to scale
  them), then retakes the baseline. The widths read during that transition are never saved.
- Fixed-width columns (`CM_STATE_FIXEDWIDTH`) are not resized. A width the view refuses falls back to the column's ideal or
  default width and counts as "not applied" (baseline = read-back).
- Columns that the folder does not offer (a stored `Dimensions` in a folder type without it) are skipped at apply and kept
  in storage until the user changes the columns of that folder; the new list then replaces it. If the folder offers none of
  the stored columns except Name, Windows' default columns are used and the column group is not applied.
- The first-column Name and a minimum of one visible column are always kept.

## 6. Same-folder drop tip

Problem: dragging items inside a Gate and dropping them back into the same folder does nothing visible, because the view
is sorted and Windows does not store a manual order.

Recommendation: a short, non-modal tip, shown rarely.
- Trigger (all must hold): a drag that started inside this Gate ended inside the same Gate; the dragged items already live
  in the folder shown; the drop produced no file operation (no copy, move or new item); and a sort is active.
  A drop onto a subfolder, onto another Gate, or with Ctrl (copy) never shows the tip.
- Show: a one-line label at the bottom of the Gate body for 5 seconds, no button, no focus change, dismissed by any click.
  At most once per Gate per app run. No new setting and no stored counter.
- Wording (English UI): "Icons here follow the folder's sort order. Use the Gate menu to change how they are sorted."
- If the drag cannot be recognised reliably enough to meet these conditions, show nothing (open question 8). A wrong tip
  is worse than none.

## 7. Storage and migration

The per-folder entries keep their current keys: `view_mode`, `icon_size`, `sort_mode`, `view_flags`, `set`, `col_info`, in
the map keyed by folder path. The per-Gate keys (`nav_history`, `view_flags` for Allow navigation) and the global
`FolderPortalsInheritViewStateFromParent` are unchanged in name and meaning. No new key is needed.

Load (never fails, never rewrites):
- Each field is parsed on its own. A bad value (wrong type, out of range, unknown mode, sort outside +-1..5) makes that
  field absent and the group falls back as in section 3; the rest of the entry is kept. Icon size is clamped to 16..=256.
  A negative icon size is `Follow`. Mode 0 absent. `set` missing = false.
- `col_info` is parsed tolerantly into (property, width). Columns whose property cannot be recognised are dropped; widths
  are read as DIPs (open questions 1 and 3). A parse that yields no columns means the column group is absent.
- Keys that differ only by case are merged for lookup: prefer the entry with `set`, then the one with more groups present,
  then the one later in the file. The file keeps both spellings until the folder is next saved.
- Unknown fields and unknown flag bits are preserved on rewrite. An entry that is not an object is skipped but its raw
  JSON is kept in `view_unparsed` (by key) so nothing is lost.
- Entries for folders that no longer exist are kept. There is no pruning.
- Entries are rewritten only when a user change patches them (section 4); unchanged entries stay byte-identical. No backup
  file is needed because migration changes no bytes.

Interpretation of legacy values: a positive `icon_size` is Fixed, `set = true` with partial fields is a valid partial entry
(for example only a sort), and `set = false` entries are ignored by the resolver. `view_flags` bits not named in the code are
carried verbatim. Gates created by older builds keep their `nav_history` and Allow-navigation bits untouched.

## 8. Edge cases and acceptance tests

All resolver tests are pure: a fake store (map of key -> entry), a fake `ctx.desktop_icon_px`, no window, no COM.

Resolver
- Own wins over inherited and default; inherited only when the option is on; off gives Default even with a configured parent.
- Nearest ancestor wins: entries at `D:\a`, `D:\a\b`; folder `D:\a\b\c` uses `D:\a\b`; groups resolved separately
  (parent has style+columns, child has sort only: child gets own sort, parent's style and columns).
- Roots: `C:\` has no parent; `\\srv\share\x` ends at `\\srv\share`; `\\?\C:\a` equals `C:\a`; trailing `\` and `/` ignored;
  a long path, a path with a trailing dot or space is not altered further; a non-filesystem key has no ancestors.
- Case: `c:\USERS\Me` matches `C:\Users\me`; non-ASCII letters; two entries differing by case merge as in section 7.
- `set = false` ignored; `set = true` with no groups behaves as absent; mode 7, 0, -1 and 99 give an absent style group.
- Icon size: negative = Follow regardless of magnitude; 0 = absent; 5 clamps to 16; 4000 clamps to 256; a Fixed size is
  carried but not applied in modes 3, 4, 6, 8 (apply plan omits it).
- Sort: each of +-1..5 maps to the right property and direction; 0 and 6 are no preference; the five-column property list.
- Columns: Name inserted first when missing; unknown columns skipped; none available except Name gives "not applied";
  widths clamped; duplicates removed; a column the folder lacks is kept in storage until the list is rewritten.
- Mode switches: Details -> List -> Icon -> Details keeps icon size and columns (carried); switching mode writes the whole
  style group; a Follow size stays Follow across a mode switch.

Saving (pure decision function `plan_save(state, baseline, live) -> Option<Patch>`)
- No patch while `Navigating`/`Applying`; a Windows default read at `OnViewCreated` never produces a patch.
- A failed apply followed by an identical read-back produces no patch; the same read-back twice produces one patch at most.
- Flush at `OnNavigationPending` writes the old folder's patch before the new folder is resolved (sequence test with two folders).
- Patch contains only changed groups; unknown fields of the entry survive; `set` becomes true; a second identical check
  produces nothing.
- Width change of 1 px produces nothing, 2 px produces a patch; unchanged columns keep their stored DIPs byte for byte.
- DPI change: widths after a 100% -> 150% move produce no patch; round trip exact for the DPI list in section 5.
- Two Gates, same folder: A sorts, B changes mode, both patches survive; B re-applies A's sort without saving; same-group
  conflict resolves to the later write; a Gate with a pending change is not overwritten by an update.
- Tip trigger: all four conditions required, each omitted in turn gives no tip; once per Gate per run.

Snapshot roundtrip
- Load -> save with no user change: output bytes equal input bytes. Load the full matrix of values (valid, partial, invalid,
  unknown fields, differing key cases, non-object entry) and save after a patch: only the patched entry changes; unparsed
  and unknown data are present; loading the result gives the same resolver output.

Live checklist (not unit-testable): no flash of Windows' view before the preference; column drag persists; Gate moved
between 100% and 150% monitors keeps column widths; Ctrl+wheel; Windows' own View menu inside the Gate; two Gates on one
folder stay in step; folder on a disconnected drive; a Gate removed while a menu is open still flushes.

## 9. Open questions

For the lead (storage facts the spec writer could not see):
1. What exactly is the stored `col_info` format (a property identity per column, a width unit, an order)? Recommendation:
   keep reading it tolerantly into (property, DIP width); decide the parser from one real sample.
2. Which bits of the per-folder `view_flags` and the per-Gate `view_flags` are used, and what are their values?
   Recommendation: name only the confirmed ones (header hidden, Allow navigation), preserve every other bit.
3. Is the stored `icon_size` pixels at 96 DPI or device pixels at the DPI when saved? Recommendation: treat as DIPs, check
   with one live save at 150% scaling; if device pixels, convert on read and write DIPs only after the next change.
4. What does `nav_history` hold (a list, a single path, which entry is "shown first"), and does a Gate with navigation
   allowed reopen at the last subfolder? Recommendation: unchanged here; resolution runs for whichever folder opens.

For the product owner:
5. Fresh-install default for "Subfolders use the parent folder view" (`FolderPortalsInheritViewStateFromParent`)?
   Recommendation: on; an existing file keeps its stored value.
6. Entries with `set = false` but filled fields: ignore them (recommended; they cannot be told from Windows defaults) or
   honour them (keeps more of an older setup but can bring back the very defaults this spec refuses to save)?
7. Defaults for a Gate that never had a view: icon view, size follows the desktop, Name ascending, Windows' default columns.
   Recommendation: confirm; the alternative is Windows' own remembered view per folder type.
8. Same-folder drop tip: show it under the strict conditions of section 6 (recommended), or never?
9. Add a "Reset view" command to the Gate menu that removes a folder's own preference? Recommendation: yes, it is the only
   way back to inheritance or defaults after the user changed something.
10. Sort by a column other than the five stored ones (for example Authors): not persisted in this version (recommended);
    a later `sort_property` field could add it. Accept?
11. Live sync of two Gates on one folder (recommended) or each Gate keeps the view it had until it navigates?
12. Does `FWF_NOBROWSERVIEWSTATE`-style behaviour keep Windows from restoring or saving its own state for Gates? Needs one
    live check; until then Corral applies its preference after `OnViewCreated` and verifies by read-back.

## Amendment (2026-10-20, user decision "approve leaner")

Not in scope for this version: the "Reset view" Gate command and live syncing of two Gates that show the same
folder. Two such Gates read the shared preference when they open or navigate; the last saved change wins.
