# Saved desk setups: design spec

Status: approved by the user 2026-10-20 (open questions resolved as the spec recommends)
Date: 2026-10-20
Author: Claude subagent (clean spec writer), inputs: spec-inputs-2026-10-20.md + settings redesign docs + public docs only

## 1. Goal and non-goals

Goal: when the set of connected monitors changes, put every Corral where the user would expect, never lose a saved
placement by accident, and bring Corrals back to exactly where they were when a known setup returns. A "desk setup" is
one set of physical monitors plus the Corral placements that belong to it. This replaces "monitor profiles".

This spec decides only: monitor identity, matching, restore, temporary recovery, storage and migration, and the small UI.

Non-goals (kept exactly as they are today, see packet B): DIP geometry per monitor DPI, the settle period after display
changes, the pending save baseline during that period, governing-monitor hysteresis while dragging, and fitting a Corral
onto a smaller monitor with later restore of its preferred size. The setting `LayoutPreserve` keeps its three values.
No new settings are added. Loose desktop icons are not repositioned by recovery (section 4).

Two invariants drive every decision below:

1. A saved setup is written only when it is the exact match of the settled state and the change was caused by the user.
2. Everything else (restore adjustments, fitting, recovery) is computed from saved data plus the current monitor list.
   It is temporary: it can be recomputed at any time and is never written back.

## 2. Monitor identity

### 2.1 What is read for each connected monitor

Read with `QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS)`, then per path:

- `DisplayConfigGetDeviceInfo` with `DISPLAYCONFIG_SOURCE_DEVICE_NAME` gives the GDI name (`\\.\DISPLAYn`). Join it to the
  `HMONITOR` from `EnumDisplayMonitors` / `GetMonitorInfoW` (`MONITORINFOEX.szDevice`) to get bounds and work area.
- `DisplayConfigGetDeviceInfo` with `DISPLAYCONFIG_TARGET_DEVICE_NAME` gives `flags.edidIdsValid`, the EDID manufacturer id
  and product code, `connectorInstance`, `monitorFriendlyDeviceName` and `monitorDevicePath`. `path.targetInfo.outputTechnology`
  gives the connector type (HDMI, DisplayPort, internal, and so on).
- `GetDpiForMonitor(MDT_EFFECTIVE_DPI)` gives the scale (stored as a percentage, `dpi * 100 / 96`, rounded).
- Best effort, for a serial: open the monitor device interface named by `monitorDevicePath` (SetupAPI,
  `SetupDiOpenDeviceInterface` then `SetupDiOpenDevRegKey`), read the `EDID` value of "Device Parameters", and take the
  monitor serial string (EDID descriptor tag 0xFF) or, if absent, the 32-bit ID serial number. Empty, all-zero and
  `0x01010101` values mean "no serial".

### 2.2 Identity tiers, in priority order

1. Device: manufacturer id + product code + serial. Stable across reboots, ports, docks and adapters.
2. Model and connector: manufacturer id + product code + connector (output technology + connector instance). Stable
   across reboots; changes when the cable moves to another port.
3. Model: manufacturer id + product code. Survives port changes, but cannot tell identical monitors apart.
4. Nothing (EDID ids invalid, a remote session, `QueryDisplayConfig` failed): the monitor is "identity-less".

Never used for matching: adapter LUID and target id (change across reboots), the `monitorDevicePath` instance segment
(tied to the port), `HMONITOR`, and GDI names (renumbered freely). They are used only during one run to join Windows data.
The friendly name is stored for display only.

A serial is discarded as unusable when two connected monitors share the same model and serial value (some panels report a
fixed fake serial). The stored serial is a short hash (first 8 bytes of SHA-256 of `mfr:product:serial`, hex) so layout
files and backups shared in a bug report do not expose the real serial.

### 2.3 The struct

```rust
struct MonitorIdentity {
    model: Option<Model>,            // None = identity-less
    serial_hash: Option<String>,     // None = no usable serial
    connector: Option<Connector>,    // None = unknown
    name: Option<String>,            // friendly name, display only
}
struct Model { mfr: String /* 3 letters */, product: u16 }
struct Connector { tech: u32 /* output technology */, instance: u32 }

struct MonitorState {                // one connected or saved monitor
    id: MonitorIdentity,
    size_px: (u32, u32),             // physical pixels, after rotation
    scale_pct: u32,
    offset_px: (i32, i32),           // top-left minus the top-left of all monitors together
    primary: bool,                   // stored but never compared
    // runtime only, never matched: gdi_name, hmonitor, work area
}
```

`offset_px` is relative to the union of all monitors, so a changed primary monitor or a negative virtual-screen origin does
not change it. Rotated monitors compare by their rotated size.

Identical models: told apart by serial when both have one; otherwise by connector; otherwise by their rank in the
arrangement (left to right, then top to bottom, by `offset_px`) as the last step (section 4.1).

Duplicated displays (one source, several targets): treated as one monitor whose identity comes from the first target in
path order. Known uncertainty (section 8).

## 3. A saved desk setup

### 3.1 Contents

- `monitors`: list of `MonitorState` as above (identity, resolution, scale, arrangement).
- `groups`: Corral id -> placement. Same fields as today (`rect` in DIPs relative to the monitor work-area origin, `roll`,
  `dock_top`, `dock_bottom`, plus whatever the size-restore machinery already keeps per placement, such as the preferred
  size). `monitor` changes from a GDI name to an index into `monitors`.
- `loose`: item name -> position, exactly as stored today (opaque; only restored in the exact and adjusted cases).
- `id`, `name`, `origin` (`learned` or `legacy`), `last_used_ms`, `modified_ms`.

### 3.2 When it is created and updated

Only from state that is stable (the settle period has ended and the pending save baseline is cleared) and caused by the user:
dragging, resizing, rolling, docking, creating or deleting a Corral, moving a loose icon, "Swap monitors". These are not user
causes and never create or update a setup: fitting to a smaller monitor, DPI changes, recovery, Explorer re-laying out icons,
hover-unrolling of a folded Corral, and anything during the settle period.

- State is Exact: after a short quiet period, snapshot live placements and update that setup in place, only if the canonical
  content differs from what is stored (no write otherwise).
- State is anything else (adjusted, recovery, new monitor, unknown): the first such edit creates a new setup for the current
  monitor list, seeded with the live placements (each Corral's preferred size kept where the existing machinery holds one)
  with the edit applied. The setup that served as the base is not touched.
- A setup is never written because Corral merely restored or displayed something.
- Limit: 16 setups. When a 17th is created, the least recently used one that is not active is forgotten (open question 1).
- Setups are only read or written when `LayoutPreserve` is 2. With 1 or 0, the stored data stays untouched.

### 3.3 Name and display

Automatic only: the friendly names joined with " + ", for example `DELL U2720Q + LG 27UL850`. A monitor with no friendly name
is "Built-in display" (internal output technology) or "Display <n>" with its resolution. Legacy setups read
`Earlier layout: 2 monitors, 2560x1440 + 1920x1080`. No renaming (open question 8). Shown only in the Layout card (section 6).

## 4. Matching

### 4.1 Pairing monitors

Current monitors are paired with a setup's saved monitors by these passes, in order. A monitor takes part in only one pair.

1. Device: both have a usable serial and the same model and serial hash.
2. Model and connector: same model, same connector (both known), exactly one such candidate on each side. A pair whose two
   serials are both known and differ is never made.
3. Model: same model, among what is left. Candidates on each side are paired in arrangement rank order. Two serials that
   both exist and differ are never paired.
4. Identity-less: where either side has no model, pair monitors of equal `size_px` in arrangement rank order. Two monitors
   that both have a model are never paired in this pass.

What remains unpaired on the current side is "new"; what remains on the saved side is "missing".

Identical monitors with no usable serial and no distinguishing connector end up in pass 3 and are paired by their position in
the arrangement. This keeps the association the user can see (the Corrals of the left screen stay with the left screen) and
needs no prompt. It is wrong only if the user's two identical screens really exchange places, and "Swap monitors" fixes that
in one click (decision and justification in section 6).

### 4.2 Choosing the setup and the case

Over all saved setups (archived legacy ones excluded), take the first that applies:

- E. Exact: every monitor is paired, counts are equal, and for each pair `size_px`, `scale_pct` and `offset_px` are equal
  (`primary` is ignored). If several qualify, take the most recently used.
- Otherwise, compare setups by this ordered tuple, smaller first: number of new current monitors, number of missing saved
  monitors, then most recently used. The first setup that has at least one pair is the base. Then the case is:
  - A. Adjusted: nothing new, nothing missing, but resolution, scale or arrangement differs.
  - M. Missing: some saved monitor is unpaired (new monitors may also exist).
  - N. New: only new monitors exist, every saved monitor is paired.
- U. Unknown: no setup has a single pair.

### 4.3 What each case does

| Case | Live geometry | Persisted | Temporary |
|---|---|---|---|
| E | Placements exactly as saved (including roll, both dock flags, loose positions). | `active` = this setup, `last_used_ms`. Later user edits update this setup. | Nothing. |
| A | Placements from the base, kept in DIPs on their paired monitor, fitted by the existing fit machinery; preferred sizes are kept. Loose positions restored as stored where the existing logic supports it. | `active` = the base setup. Nothing else. | The fitted geometry. Returning to the old mode is Exact again. |
| M | Corrals of present monitors as saved. Corrals whose monitor is missing are moved to the nearest present monitor (by distance between monitor centres in the saved arrangement; a tie goes to the lower rank) keeping their DIP offset, clamped by the fit machinery. Roll and dock flags are kept. Loose items are not moved. | `active` = the base setup. The base is not modified. | The moved positions, and the list of "home" monitors, recomputed from the base on every evaluation. |
| N | Nothing changes. No Corral goes to the new monitor. | `active` = the base. | Nothing. |
| U | The existing `LayoutPreserve` behaviour on the live geometry. Nothing is learned yet. | `active` = none. | Nothing. |

When the missing monitor comes back, the next evaluation is Exact (or Adjusted); the Corrals return to their saved placements
because the base setup was never changed. Mixed cases (some missing and some new) behave as M for the missing monitors and as
N for the new ones. Two monitors at once missing is just M with more Corrals moved.

A Corral that exists live but has no placement in the chosen setup (created later) keeps its live position, clamped. A saved
placement for a Corral that no longer exists is ignored and dropped the next time that setup is written.

### 4.4 Editing during temporary recovery

A user edit in M (a moved, resized, rolled or docked Corral, a new or deleted Corral, "Swap monitors") belongs to the current
monitor list. After the quiet period it creates a new setup for the current monitors (section 3.2), and `active` points to it.
All live placements are copied in, so moved Corrals now live permanently on the remaining monitor in this new setup. The old
setup keeps its original placements, including the one for the Corral just edited, so reconnecting the missing monitor still
gives the full original layout. Edits to a Corral whose home monitor is present follow the same rule (they are written to the
new setup only). The same rule covers A, N and U.

## 5. Storage and migration

### 5.1 New JSON shape (layout file)

```json
"desk_setups": {
  "version": 1,
  "active": "s3",
  "next_id": 4,
  "setups": [{
    "id": "s3", "origin": "learned", "last_used_ms": 0, "modified_ms": 0,
    "monitors": [{
      "model": {"mfr": "DEL", "product": 41013}, "serial_hash": "a1b2c3d4e5f60718",
      "connector": {"tech": 10, "instance": 0}, "name": "DELL U2720Q",
      "size_px": [2560, 1440], "scale_pct": 100, "offset_px": [0, 0], "primary": true
    }],
    "groups": {"12": {"monitor": 0, "rect": [40, 40, 320, 200], "roll": 0, "dock_top": false, "dock_bottom": false}},
    "loose": {"Notes.txt": [120, 300]}
  }]
}
```

Old `profiles` and `active_profile` are read once and, after the backup below, no longer written (open question 10). Unknown fields
inside a setup are preserved on rewrite.

### 5.2 Legacy setups

Each entry of `profiles` becomes a setup with `origin: "legacy"`:

- `legacy_key`: the old key string, kept. `monitors`: one entry per old `displays` item with `model`, `serial_hash`
  and `connector` all null (never invented), `name` null, `size_px` and `scale_pct` from the stored bounds and dpi, `offset_px`
  from the stored bounds (union-relative), and the GDI name kept as `legacy_gdi_name`.
- `groups`: `monitor` becomes the index of the display with that GDI name. A placement naming an unknown display is kept
  with `monitor: null` and is placed on the primary monitor when used. `loose`, `last_used_ms`, `modified_ms` carry over.
- `active_profile` becomes `active` if that key exists; otherwise `active` is none.

Matching a legacy setup uses only what it stores: it is Exact when the current display list produces the same key string as
today (count, resolution and scale per display, same function) and pass 4 pairs every monitor. Otherwise it competes as a
normal base with identity-less monitors in cases A, M and N.

A legacy setup gains real identities the first time it is the Exact match and the state has settled: each saved monitor is
joined to the current monitor of equal size and the same GDI name, the current identity is copied in, and `origin` becomes
`learned`. This is the only way an identity is attached; if any monitor cannot be joined, the setup stays legacy. Learned setups
that were saved with identity-less monitors (for example in a remote session) upgrade the same way.

Legacy flags are kept verbatim under `legacy: {established, obsolete, downsized_to}` and are not part of matching, except:
`obsolete: true` makes the setup archived (never matched, not listed in the UI, but still stored). `downsized_to` is only a
reference for display; a key that is missing, equal to the setup's own key, or part of a cycle (checked with a visited set) is
dropped from the setup and noted in the log. Their exact old meaning is an open question (question 5).

### 5.3 Bad data

- `profiles` missing or empty: no setups, `active` none.
- `profiles` or an entry is not the expected type, or an entry fails to parse: that entry is skipped, and its raw JSON is kept
  under `legacy_unparsed` (keyed by the old key) so nothing is lost; the rest is migrated.
- Negative or zero sizes, zero scale, or no displays: treated as unparsed.
- `active_profile` naming a missing key: `active` is none.
- Duplicate keys that normalise to the same monitor list: keep both; the most recently used wins at matching.

### 5.4 Backup before the first migrated save

Before the first write of a layout file that contains `desk_setups` in place of `profiles`, copy the existing file byte for
byte to `<layout name>.before-desk-setups.bak` in the same folder, once (never overwritten). If the copy fails, do not write
the new shape; keep running from memory, log, and retry at the next save. Restoring a Backups page snapshot that has the old
shape migrates on load the same way (the file written afterwards is covered by the same one-time backup) and a restore is
followed by a normal evaluation, not by a save.

## 6. UI

Smallest change: two things in the existing Layout card "When monitors change" (existing `LayoutAuto` gate), shown only when
"Remember a layout for each monitor setup" is selected. The three radio rows and the Swap monitors row stay as they are.

1. Setting row "This desk setup": title as quoted, description (Caption) is a one-line status (the title never changes):
   - Exact: "Saved layout restored."
   - Exact, or after the first edit: "Layout saved for <name>."
   - Adjusted: "Your monitor settings changed. Corrals are fitted for now; move one to save a layout for these settings."
   - Missing: "<monitor name> is not connected. Its Corrals are shown on <other name> for now and go back when it returns."
   - New monitor: "<name> is new. Move a Corral onto it to save a layout."
   - Unknown: "New monitor setup. Move a Corral to save a layout for it."
   - Appended when pass 3 paired identical monitors by position: "Identical monitors are matched by position. Use Swap
     monitors if they are the wrong way round."
   Control: Secondary button "Saved setups…".
2. Dialog (440) "Saved desk setups": a card of rows, newest first. Row: badge (accent/subtle, glyph "Devices" or the monitor
   glyph), name (Body), Caption "<n> Corrals · last used <date>" with ", this setup" for the active one or ", from an earlier
   version" for legacy; Subtle icon button "Forget" (tooltip "Forget this setup", disabled and tooltip "In use" for the
   active one). Empty list: Caption "No saved setups yet. Move a Corral to save one." Footer: Primary "Done".
   Forget opens the standard confirmation: "Forget \"<name>\"?" / "Corral stops remembering where Corrals sit for this
   monitor setup. Corrals and files are not changed." / "Cancel", Danger "Forget".

Decision on identical monitors: keep the association by position and show the status line; no one-time prompt. Reason: the case
needs identical models without a usable serial and without a distinguishing connector, which is rare; a prompt would interrupt
at every dock or undock and still cannot know the answer; the association is by what the user can see; and the existing
Swap monitors button is a one-click fix.

There is no desktop notification or toast for recovery; the status line and the Corrals' own positions are the signal.

## 7. Edge cases and acceptance tests

The matching and decision code is pure: `read_monitors()` returns `Vec<MonitorState>`; `decide(current, setups, active) -> Decision`
takes plain values. Tests use fake monitor lists (a builder with model, serial, connector, size, scale, offset) and never touch
Windows. Rules tested at the pure level are marked (P); marked (L) need a live pass.

1. (P) Dock and undock: setup A = {laptop, external}; undock gives M with the external's Corrals on the laptop; the serialised
   bytes of A are identical afterwards; dock again gives E and every placement equals A's, including roll and both dock flags.
2. (P) Unplug and replug the same monitor into another port: serial pairs it (pass 1); a monitor with no serial pairs by model
   (pass 3) and the case is E or A, not M plus N.
3. (P) Two identical monitors: (a) both with distinct serials, swapped ports: E, Corrals follow the serial; (b) no serials,
   different connectors: pass 2; (c) no serials, same or unknown connectors: pass 3 by rank, status line appended; (d) same
   model and the same fake serial on both: serial treated as unusable, behaves as (c); (e) one of two identical monitors
   disconnected: M with the remaining one paired by rank.
4. (P) Missing EDID: identity-less monitors pair by equal `size_px` and rank, never against a monitor that has a model; a legacy
   setup of one 2560x1440 monitor is Exact only for a single 2560x1440 monitor; after settling it upgrades with the identity.
5. (P) Primary monitor change only: E (primary is not compared, offsets are union-relative); placements do not move.
6. (P) Resolution change, scale change, arrangement change: each gives A; nothing is written; changing back gives E with the
   original setup byte-identical; after a fit to a smaller monitor and back, the preferred size is the saved one.
7. (P) Work-area change only (taskbar moved, auto-hide toggled): same case as before, no write, no new setup.
8. (P) Negative virtual-screen origins (monitor left of or above the primary): offsets and rects are relative, so a primary swap
   or a shifted arrangement origin does not change exactness; a Corral keeps its place relative to its monitor.
9. (P) New third monitor: case N, `active` set, nothing else written; first edit creates a setup of three monitors and leaves the
   two-monitor setup unchanged.
10. (P) Edit during recovery: after the quiet period a new setup exists for the current monitors, `active` points to it, the base
    is byte-identical, reconnecting the monitor selects the base and the edited Corral returns to its base placement.
11. (P) Restart during recovery: reloading with the same fake monitors yields the same decision and the same moved positions;
    reloading with the missing monitor present yields E; `active` was the only extra persisted value.
12. (P) Snapshot or backup restore: loading an old-shape backup migrates (including bad entries), then evaluates; nothing is
    written until a user-caused edit; the `.before-desk-setups.bak` copy exists once and is never overwritten.
13. (P) Rolled and docked Corrals: roll state and both dock flags survive M and back (E) unchanged; a hover-unroll and an
    automatic fold on dock never mark an edit; a user roll in M creates the new setup.
14. (P) Legacy data: missing, empty, wrong-type and zero-size profiles; a `downsized_to` cycle (a -> b -> a) and a self-reference;
    `active_profile` pointing nowhere; `obsolete` entries archived; unparsed entries preserved in `legacy_unparsed`.
15. (P) No repeated saves: a sequence of identical evaluations (many display-change events for the same final state) produces
    zero writes; after an edit exactly one write; a second identical snapshot produces none; the settle period produces none.
16. (P) Limit: creating a 17th setup forgets the least recently used non-active one.
17. (P) Settings `LayoutPreserve` 1 or 0: no setup read or written, stored setups unchanged.
18. (L) Real dock with two identical external monitors; remote session where identity is unavailable; duplicated-display mode;
    a docked Corral on a monitor that disappears, then returns; checking `QueryDisplayConfig` data on the user's machines.

## 8. Open questions for the product owner

1. Is a cap of 16 saved setups (forget the least recently used) acceptable, or should nothing ever be forgotten automatically?
   Recommendation: cap at 16.
2. Where should Corrals of a missing monitor go: the nearest present monitor (recommended) or always the primary?
3. Should recovered Corrals avoid overlapping Corrals already on that monitor? Recommendation: no in version 1 (it is
   temporary and the user can see and move them).
4. May Corral read the monitor serial from EDID (stored only as a short hash, local)? Recommendation: yes, it is the only
   reliable way to tell identical monitors apart.
5. Lead to confirm the old meaning of `established`, `obsolete` and `downsized_to` (the spec writer could not look). Proposed
   handling: keep verbatim; `obsolete` makes a setup archived; the others are not used for matching.
6. In which coordinate space is the legacy `loose` position stored? Proposal: keep it opaque and unchanged, restore only in E and A.
7. Duplicated-display mode: identity from the first target only. Is that enough, or should duplicated monitors be one
   unmatched identity-less monitor?
8. Should saved setups be renameable? Recommendation: no, automatic names are enough.
9. Should recovery show any notice beyond the Layout status line? Recommendation: no.
10. After migration, drop `profiles` and `active_profile` from the file (recommended, the one-time backup keeps them) or keep
    writing them for older builds?
11. Does the existing machinery restore at once on a display change, or only after the settle period? This spec assumes
    evaluation after the settle period ends; confirm that this fits the current settle code.
12. An edit made during the settle period is treated as part of the display change and may be overwritten by the restore. Is that
    acceptable? Recommendation: yes (narrow window, existing behaviour).
