# Corral Peek (Glance) hotkeys: design spec (area P12)

Status: approved by the user 2026-10-20 (open questions resolved as the spec recommends)
Date: 2026-10-20
Author: Claude subagent (clean spec writer), inputs: spec-inputs-wave-b-2026-10-20.md + approved Corral design docs + public docs only

Terms: Peek = the feature the UI calls "Glance" (Corrals above all windows). "Toggle combo" shows it until pressed again,
"hold combo" shows it while held. "Consume" = the hook returns non-zero, Windows never sees the event.

## 1. Goal and non-goals

Goal: Win+Space (toggle) and Win+Ctrl+Space (hold) work as the defaults, together with any other valid combo, and they
never cost a user anything: Win+Space still switches the input language for anyone who does not use Peek or whose
combo is not completed, no key is ever stuck, the Start menu does not pop up after a consumed Win combo, and a
slow or lost hook cannot hurt typing.

Non-goals: desktop-mode Peek (`HotkeysDesktopEnabled`, `HotkeysComboDesktop*`: keys are stored untouched, nothing
reads them); chords of several non-modifier keys; combos without a modifier; system-wide shortcuts for other Corral
features; swallowing mouse events (never done).

## 2. Combo model and stored encoding

A combo is `{ mods: Win|Ctrl|Alt|Shift subset (left and right keys merged), key: trigger vk }`.
Valid when: the key is Space, A-Z, 0-9 or F1-F12 (not a modifier, Esc, Tab, Delete, Backspace, Enter); and `mods` is
non-empty and either contains Win or has at least two members (so Ctrl+K, Alt+F, Shift+A are refused: they collide with
typing and apps); and it is not Win+L (cannot be intercepted) or Alt+F4. A combo for toggle and one for hold must differ.
Validation lives in one pure `validate_combo`; the settings UI and the loader both call it.

**Stored u32** (new, Corral-defined): `0x4000_0000 | (mods << 16) | vk`, with mods using the public `RegisterHotKey`
values Alt = 1, Ctrl = 2, Shift = 4, Win = 8 and `vk` the Windows virtual-key code. Value 0 = none (that gesture off).
Examples: Win+Space `0x4008_0020`; Win+Ctrl+Space `0x400A_0020`; Ctrl+Alt+Space `0x4003_0020`.

Reading `HotkeysComboGlanceToggle` / `HotkeysComboGlanceHold`:
- absent -> default (Win+Space / Win+Ctrl+Space); 0 -> none;
- bit 30 set -> decode; if reserved bits are non-zero or `validate_combo` fails -> invalid, run with the default;
- bit 30 clear and non-zero -> **legacy value**: decode through `legacy_combo(u32) -> Option<Combo>`, a table the lead
  fills from the choices Corral's settings already offer (open question 1). No match -> unrecognised: run with the default.
Nothing is rewritten at load. A value is written only when the user picks a combo (always in the new encoding), so
unrecognised legacy values survive until then. Keys `HotkeysGlanceEnabled` (bool) and `CorralPeekKeepOpen` (bool, UI name
`PeekKeepOpen`) are unchanged.

## 3. When a combo starts and ends

Modifier tracking: the engine tracks the four merged modifiers from key events it sees. A fake Ctrl with scan code 0x21D
sent right before Right Alt on AltGr layouts is not counted (so AltGr+Space typed on a Polish or German layout is never
mistaken for Ctrl+Alt+Space; open question 6).

- **Start**: key-down of the combo's trigger while the held modifiers are exactly the combo's `mods`. Exact match is
  required: Win+Shift+Space (reverse language switch) does not start Win+Space, and Win+Space does not start
  Win+Ctrl+Space. Auto-repeat downs of a consumed trigger are consumed and ignored.
- **Toggle**: emit `Toggle` at start. The UI thread decides show or hide. The combo is "over" when the trigger is released.
- **Hold**: emit `HoldStart` at start. Ends at the first of: trigger key-up; any required modifier released; hold watchdog
  mismatch (section 6); Esc; click elsewhere (unless `CorralPeekKeepOpen`); config change; session lock; a fullscreen
  app becoming foreground. Emits one `HoldEnd`. The trigger stays "consumed" until it is physically released; auto-repeat
  does not restart the hold. If Peek was already open from a toggle, hold neither closes nor re-opens it (the UI thread
  records `opened_by_hold`); `CorralPeekKeepOpen` never prolongs a hold, it only governs click dismissal.
- Pressing a different key first (Win, X, Space) does not start anything: the combo must be built modifiers first.

## 4. What is consumed and what passes

Pass (return `CallNextHookEx`): every modifier key down and up, always, with no exceptions. Windows therefore always sees
a matching up for each modifier down, which makes a Corral-caused stuck modifier impossible.

Consume, only when all of: Peek hotkeys enabled, Corral is on, no fullscreen refusal (section 7), event not tagged with
Corral's own marker:
1. Trigger key-down that starts a combo (section 3) -> consumed, key added to the `consumed` set.
2. Auto-repeat downs and the key-up of any vk in `consumed` -> consumed, then removed on key-up. An up is never consumed
   without a consumed down, and a consumed down always gets its up consumed (even if settings changed in between).
3. Esc key-down (no modifiers) while `peek_visible` -> consumed with its up, emits `Dismiss`. Otherwise Esc passes.

Everything else passes untouched: Win alone, Win+Shift+Space, Ctrl+Space, Win+Left, typing, etc. A half-built combo (Win
down, no trigger) consumes nothing. With Peek hotkeys off, Corral off, or no combo configured, the hook is **not installed**
at all, so Win+Space is entirely Windows'.

**Start menu suppression.** After a consumed trigger whose `mods` include Win or Alt, Windows would see Win (or Alt)
pressed and released with nothing between, which opens the Start menu (or highlights the menu bar). So the engine asks
for one **mask key**: the hook thread injects, via `SendInput`, a down and an up of the unassigned virtual key 0xE8
carrying Corral's marker in `dwExtraInfo`, at once after the callback returns (self-posted message, within about 1 ms,
before a human can release Win). The marker makes the hook ignore its own event. One mask per consumed start, none when
the combo has neither Win nor Alt. `SendInput` is subject to UIPI: with an elevated foreground window the mask may be
dropped and Start may open (open question 4). [unverified on live Windows: the mask is believed to work as in other tools.]

## 5. Injected input

Events with `LLKHF_INJECTED` from other processes (remapping tools, on-screen keyboard, accessibility, remote sessions)
are treated like physical keys: they update modifier tracking, can start a combo and can be consumed. Reason: the
people who remap Caps Lock to Win or use the on-screen keyboard are the ones who need the shortcut. Events carrying
Corral's own marker are dropped from tracking and never consumed. One stream rule: an injected up whose down was not
consumed is never consumed. Whether to ignore other injected input is open question 5.

## 6. Timing, budget and liveness

- **Hook budget**: the callback only reads the event, steps the pure engine, `PostMessage`s at most one event and
  returns. Target under 200 microseconds, hard limit 1 ms measured with a monotonic clock. It never allocates, locks
  (config is read with `try_lock`; on failure it keeps the previous copy), touches files, the registry, COM, UI or logs.
  Windows silently removes a low-level hook that exceeds `LowLevelHooksTimeout`, which would also end Win+Space for Peek users.
- **Liveness**: the engine records each callback's duration; over 150 ms sets a `slow` flag and the hook thread reinstalls the hook. The hook is also reinstalled
  after resume from sleep and session unlock (and tracking is reset). Reinstall failure shows the warning box in section 8.
- **Modifier resync**: before consuming a trigger, the engine compares its tracked modifiers with `GetAsyncKeyState`
  for the other keys; a mismatch means a missed key-up (Win+L, secure desktop, Ctrl+Alt+Del). The truth wins: tracking is
  corrected and the combo is evaluated again, so a stale "Win down" can never swallow a lone Space.
- **Hold watchdog**: while a hold is active the hook thread runs a 250 ms timer that checks the physical state of the
  required keys; any released key ends the hold. Session lock, desktop switch and power events clear tracking and end a hold.
- There is no debounce; only auto-repeat suppression. Peek has no timeout while open from a toggle.

## 7. Fullscreen, dismissal, mouse

- **Refusal**: the UI thread keeps an atomic `fullscreen` flag, refreshed on foreground change, on display change and
  every 2 s, from `SHQueryUserNotificationState` (`QUNS_BUSY`, `QUNS_RUNNING_D3D_FULL_SCREEN`, `QUNS_PRESENTATION_MODE`
  mean refuse). While it is set the engine consumes nothing, so the combo (for example the language switch) belongs to
  Windows. If the flag was stale and `Show` is refused after consumption, nothing appears (a rare lost key).
  A fullscreen app taking the foreground while Peek is open dismisses it.
- **Click elsewhere**: while Peek is visible the hook thread also installs `WH_MOUSE_LL`. On any button-down it posts the
  point to the UI thread and passes the click through (never consumed). The UI thread dismisses unless the point is inside a
  Corral or a Corral popup or `CorralPeekKeepOpen` is on. The mouse hook is removed on hide.
- **Esc** always dismisses (consumed once; the next Esc reaches the app). Dismiss ends a hold, and the remaining trigger
  events stay consumed until released.

## 8. Threading and settings UI

Threading: one **hook thread**, started when the feature turns on, owns both hooks and the pure `HotkeyEngine`. It runs a
`GetMessage` loop and nothing else. It posts `Toggle`, `HoldStart`, `HoldEnd`, `Dismiss`, `ClickAt(x, y)` to the UI window
with `PostMessage` (non-blocking). The UI thread publishes configuration as a `Mutex<Config>` (combos, enabled) plus atomics
`peek_visible`, `fullscreen`. Config changes mid-gesture end the hold and keep `consumed`. Shutdown unhooks before the
process exits; crash safety needs no state because modifiers are never consumed.

Settings UI (smallest change to redesign spec section 7, Glance, "Shortcuts" card):
- The two existing dropdowns keep their rows and descriptions. Each lists the existing choices, plus "None" and a last item
  "Custom...". Choices already used by the other dropdown are disabled with the suffix "In use". Labels read "Win + Space".
- "Custom..." opens a Dialog (440): title "Custom shortcut", body "Pick the keys to use together.", a row of four checkboxes
  Win, Ctrl, Alt, Shift, a Dropdown "Key" (Space, A-Z, 0-9, F1-F12), a live preview chip, a Caption error in status/danger
  from `validate_combo` ("Add Win, or at least two of Ctrl, Alt and Shift.", "This shortcut is reserved by Windows."),
  "Cancel" / Primary "Use shortcut". No key capture: Windows would act on Win combos before the dialog saw them.
- A stored value that is not a listed choice appears as an extra checked item with its label.
- When the toggle combo is Win+Space, a Caption under the row: "Windows also uses Win + Space to switch the input language.
  While shortcuts are on, Corral takes it." Nothing else changes.
- Hook failed or lost: a Warning info box at the top of the card, "Shortcuts are not available" / "Corral could not listen for
  keys. Try turning the shortcuts off and on." Dropdowns stay editable.

## 9. Acceptance tests (pure, fake event stream and clock)

`HotkeyEngine::on_key(KeyEvent{vk, down, injected, marker, scan}, now, &oracle) -> Out{consume, emit, mask}` and
`on_tick(now, &oracle)`. The oracle is a fake `GetAsyncKeyState`. Streams are literal lists with timestamps.
1. Win+Space toggle: LWin down passes; Space down consumed + `Toggle` + mask; Space up consumed; LWin up passes. RWin same.
2. Off: enabled false, Corral off, or combo none: every event passes, nothing emitted, no mask.
3. Exactness: Win alone, Win+Shift+Space, Ctrl+Space, Win+Ctrl+Space with only the toggle set, Space then Win: all pass.
4. Hold: Win+Ctrl+Space gives `HoldStart` at Space down and `HoldEnd` at Space up; releasing Win first ends the hold and the
   later Space up is still consumed; releasing Ctrl first likewise; auto-repeat does not restart; toggle open + hold = no change.
5. Auto-repeat of a toggle trigger: one `Toggle`, repeats consumed.
6. Esc consumed with its up only while `peek_visible`; otherwise passes. Esc ends a hold and the trigger up is consumed.
7. AltGr: fake LCtrl (scan 0x21D) + RAlt + Space does not start Ctrl+Alt+Space; real Ctrl+Alt+Space does.
8. Missed key-up: tracked Win down, oracle says up: a lone Space passes and tracking is repaired.
9. Hold watchdog: oracle reports Win up at t+250 ms -> `HoldEnd` from `on_tick`; no events means no change.
10. Fullscreen flag: nothing consumed; a stream that started before the flag still gets its ups consumed.
11. Config change mid-hold: `HoldEnd`, trigger up still consumed, tracking intact.
12. Injected: Corral-marker events leave tracking and consumption unchanged; other injected events behave as physical;
    no up is consumed without its consumed down.
13. Invariants over generated streams: every modifier event passes; each consumed up has a consumed down and vice versa;
    after all keys are released the consumed set and tracked modifiers are empty; mask count equals consumed starts with
    Win or Alt, zero otherwise.
14. Encoding: round trip for every valid combo; the three examples in section 2; 0 -> none; reserved bits -> invalid ->
    default; legacy table hit/miss; bit 30 clear is never read as the new format; `validate_combo` boundary cases;
    toggle equal to hold refused.
15. Dismissal: pure `should_dismiss(point, corral_rects, popup_rects, keep_open)`; Esc and hold-end ignore `keep_open`.
16. Budget: 1,000,000 mixed events finish in under 1 s in release with no allocation in the loop; a fake clock reporting a
    slow callback sets `slow`.
Live checklist: Win+Space language switch with Peek off and with the combo unfinished; Start menu after Win+Space; no stuck
Win after Win+L and after UAC; AltGr typing; remappers; elevated foreground window; fullscreen game; RDP session.

## 10. Open questions

1. Lead: supply the existing `HOTKEY_CHOICES` table (label -> stored u32) so `legacy_combo` can read current values.
2. Do any current choices fail `validate_combo` (for example a Shift-only or key-less entry)? How are they displayed?
3. Is an unrecognised legacy value acceptable to run as the default silently, or should the dropdown say so?
4. Does the 0xE8 mask key suppress Start/menu activation reliably on current Windows 11, and how to handle an elevated foreground window?
5. Should injected input (remappers, on-screen keyboard) be honoured (assumed) or ignored?
6. Confirm the AltGr fake-Ctrl signature (scan code 0x21D) on the user's layouts.
7. Should hold on a toggled-open Peek (section 3) do nothing (assumed) or close it on release?
8. Is passing the combo to Windows during a fullscreen refusal desirable, or should Peek always consume?
9. Hook liveness: is reinstall-after-slow-callback enough, or add a periodic check? Foreground-change-only fullscreen refresh instead of 2 s?

## Amendment (2026-10-20, user decision "approve leaner")

Keep Corral's current stored combo format instead of the new encoding in section 2: `(mods << 16) | vk` with
mods Win=1, Shift=2, Ctrl=4, Alt=8, and vk 0 for a modifier-only combo; 0 = disabled. Keep the existing 12-choice
dropdown and add no "Custom..." dialog. For a modifier-only choice the trigger is the last of its modifiers to go
down; if that cannot meet the swallowing rules reliably, drop those choices from the dropdown (stored values then
read as the default, shown with a note). Everything else in this spec applies unchanged.

## Amendment 2 (2026-10-20, user decision)

Dismissal: a toggled Peek ends when another application's window becomes active (click, Alt+Tab, Win+number), unless `CorralPeekKeepOpen` is on; this replaces the click-elsewhere mouse hook of section 7. Esc and the hotkey still end it. Hold Peek ends on release.
