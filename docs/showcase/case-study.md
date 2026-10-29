# Case study: test files escaped the preview onto the real desktop

**Date:** 13 October 2026 (private development, before the public snapshot)
**Found by:** me, during a hands-on test session in Corral's preview window
**Fixed by:** a Codex worker, reviewed by a separate read-only agent
**Re-tested by:** me, the same day

## Why this one

Corral's main promise is that organising icons never moves, renames or deletes your files. This
bug broke that promise inside the one mode that exists to make testing safe, so it mattered more
than its size suggests. It is also a good example of how the project handled problems: reproduce,
check for damage first, find the cause, fix it narrowly, review it, then test it again by hand.

## Background

Corral has a **preview mode**: instead of taking over the Windows desktop, it opens a window that
pretends to be one. For testing, the preview can be pointed at a scratch folder of throwaway files
(via the `CORRAL_QA_ITEMS_DIR` environment variable), so it never shows or touches the real
desktop. My test session used such a scratch setup: a few Corrals, a Folder Gate on a scratch
folder with six `.txt` files, and several Ranges (desktop pages).

## What happened

Steps, as I ran them:

1. Start the preview on the prepared scratch session.
2. In the Folder Gate, select its six text files.
3. Drag them onto an ordinary Corral on another Range.

**Expected:** the six files move into the preview's scratch folder and appear in the Corral.
**Actual:** they vanished from the preview and appeared on my real Windows desktop.

## Investigation

The first question was whether anything had been lost or changed. The scratch session had
recorded a SHA-256 hash of every test file when it was created. A read-only check compared the six
files now on the desktop with that list: all six hashes matched exactly. The files had been moved,
not deleted or altered. Nothing on the desktop was touched by the agents. I deleted the six
throwaway files myself.

Then the cause. A drag out of a Folder Gate is a native Windows Shell drag, so it does not go
through Corral's internal drag code; it arrives at Corral's drop target like a drag from Explorer
would. That drop target always sent files to the **Desktop folder**. In desktop mode that is
correct, because Corrals hold desktop items. In the preview, though, the "desktop" on screen is the
scratch folder, and the drop target did not know that.

## Constraints on the fix

- Desktop-mode behaviour had to stay exactly as it was.
- The preview must never fall back to the real desktop, even if the scratch path is wrong or
  missing.
- The project rule for any code path that can move files: an independent review before merging.

## Fix

When the preview runs with a scratch folder, the drop target now uses that folder as its
destination. The scratch folder is only accepted if it really sits inside the test area, after
resolving the path; if it does not, external drops are refused instead of going to the desktop.
Desktop mode is unchanged.

In today's code this is in `src/app/item_drag.rs` (`reject_external` and `default_drop_destination`
where the drop target is registered) and `src/app/shell_items.rs` (`qa_fixture_folder`). Later
work applied the same rule to dropping onto a Folder Gate and to creating a Gate by Alt-dropping a
folder (`move_into_gate` and `safe_qa_folder` in `item_drag.rs`).

## Verification

- **Automated:** the full offline test suite passed after the fix (319 tests at the time).
- **Independent review:** a separate read-only agent reviewed the change and accepted it. It found
  no path that moves files outside the intended folder and desktop-mode routing unchanged, and it
  noted a few low-severity points; the first two below come from it.
- **By hand:** I ran the same drag again in a fresh scratch session. The files moved into the
  Corral correctly, with a slight delay before they appeared. Their placement also survived a
  restart.

## What it did not fix

- A preview started **without** the scratch variable still uses the real desktop. That is
  intended (it is how you try Corral without the desktop takeover), but it means every test
  launcher must set it.
- Dragging a scratch icon out of the preview onto Explorer or the real desktop is an ordinary
  Windows move. The test instructions were updated to warn about it.
- The slight delay before moved files appear was noted but not investigated here.

## What I took from it

Check for damage before chasing the cause, and say exactly what was verified: here, "the six files
are byte-identical to the originals" rather than "the files look fine". And keep "fixed" and
"re-tested" apart: the fix passed tests and review first, but I only called it done after
repeating the original steps myself.
