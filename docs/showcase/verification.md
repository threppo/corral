# Verification record

What has been checked, when, by whom and on which code. Corral was developed privately from
10 to 29 October 2026; the public repository starts with a single snapshot commit (`72d75fc`), so
older results refer to private commits that are not published. A result is only listed against
code it was actually run on.

Environment for everything below: Windows 11 Pro, x64, Rust 1.99.0 (`x86_64-pc-windows-gnu`).
Monitors for the live tests: 2560×1440 at 100 % and 3840×2160 at 250 %.

## Fresh checks on the public code (29 October 2026)

Run by Claude Code in this repository, at the commit named.

| Check | Code | Result |
| --- | --- | --- |
| `cargo test --offline` on a clean copy of the repository | `72d75fc` | 1247 unit tests passed, 3 ignored; 14 integration tests passed (`tests/`) |
| Same, on this documentation branch | see the commit that adds this file | 1247 + 14 passed, 3 ignored (re-run after the documentation changes; no code changed) |
| Release build of `corral.exe` (v0.1.0-alpha) | `72d75fc` | builds; local folder paths rewritten at build time; a byte scan of the exe found no user name, home-folder path or private project name |
| The release exe started in preview mode on scratch files for 4 seconds | `72d75fc` | started, ran and exited with code 0 |
| Release asset downloaded back from GitHub | v0.1.0-alpha | SHA-256 identical to the local build |
| Text scan of every published file for personal paths, names and internal-only markers | `72d75fc` | 0 hits (the scan script lives in the private repository) |

## Screenshots

All screenshots were taken by Claude Code on 29 October 2026 with a guarded test driver that
refuses to click or capture anything that is not a Corral window. They use throwaway sample files
and a scratch settings file, not real desktop items or settings.

- `corrals.png`, `menu.png`, `fold.png`: preview window, built from the private code a few hours
  before the public snapshot. The changes between that build and `72d75fc` were internal renames,
  removal of a layout importer and a new About-page notice; none of them is visible in these three
  images.
- `settings-welcome.png`, `settings-glance.png`, `settings-round-up.png`: Settings window, built
  from the same source as `72d75fc`. The Personalization page was left out because its preview
  shows the real wallpaper.

## Demo recording (29 October 2026)

Driven by Claude Code with the same guarded driver, on the v0.1.0-alpha `corral.exe` (`72d75fc`),
in preview mode with sample files and blur off. Every step was rehearsed first and checked with
screenshots, then recorded: drawing and naming a Corral, dragging four files in, resizing, opening
a folder inside a Folder Gate and going back up, folding and unfolding, and three Settings pages.
All of them behaved as expected, with one exception: the rehearsal showed that clicking a Corral's
title does not start a rename, although the Settings Welcome page says it does. Renaming works
through the Corral menu. This is listed under Known issues; the code was not changed.

## Historical: live tests driven by the lead agent (private history)

**29 October 2026, Folder Gates pass.** Claude Code drove a running Corral with the guarded
driver, in the preview and in desktop mode on both monitors. Passed: Gate keyboard navigation,
switching tabs and folding with a Gate tab, sorting a Gate (stored per folder), hiding shortcut
arrows, copy and paste inside a Gate, drops onto a Gate from a Corral and from Explorer, creating a
Gate by Alt-dropping a folder, a 5000-file folder opening and scrolling, Gate menus while a large
folder loads, a Gate moved to the 250 % monitor without a message flood, Glance and Clear view with
a Gate. Three failures were found and fixed on branches, each re-checked live on its branch build
before merging: no rubber-band selection in Gates, double-click on empty Gate space did nothing,
and the empty-space menu was the folder's full Shell menu instead of Corral's. What stayed open is
listed under Known issues in the README.

That pass ran on private code from before the public snapshot. Since then, settings keys and names
were renamed, two interop features became settings-driven and a layout importer was removed. The
unit tests were re-run on the new code (above); the live pass was **not** repeated.

## Historical: my own hands-on tests (private history)

These are tests I ran myself, by hand, on much older code (two to three weeks before the public
snapshot). The results were recorded at the time as "user-reported" because I described what I saw
and the agents logged it.

- **13 October, preview.** Resizing: outer margin, cursors, raise on press and losing the mouse
  mid-resize all passed. I reported that resizing a Folder Gate felt laggier than a Corral, that
  Gates had no scrollbar yet, and that the size tooltip showed on every resize. Moving between
  Ranges, showing a Corral on all Ranges and Camouflage colours worked. I found the drop bug in the
  [case study](case-study.md) and re-tested its fix the same day; I also reported that moving
  Corrals was slightly choppy, with the wallpaper trailing by a few frames.
- **14 October, desktop mode, both monitors.** Saved desk setups: switching between one screen
  and two screens with Win+P (twice, moving Corrals in between) and changing the main monitor from
  2560×1440 to 1920×1080 and back. Every setup came back with its own positions. The final "stop"
  step failed because the test script also ended Corral's watchdog, which left the desktop icons
  hidden; the script was fixed.
- **14 October, desktop mode.** First Glance experiments. Several variants behaved the same; I
  asked the lead agent to choose one, and it picked the one that changed the least.
- **16 October.** Docking a Corral's title at the top of the screen: passed.

## Not covered

- No continuous integration; every result above was produced on one Windows 11 machine.
- Windows 10 has not been tested, although the code targets it.
- Touch and pen input have never been tested.
- The UI tests rely on test tooling kept in the private repository; the public repository contains
  the unit and integration tests only.
- The release exe is not code-signed.
