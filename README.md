# Corral

**Status: alpha.** The features below work and have been tested live on Windows 11, but expect
rough edges; see [Known issues](#known-issues).

Corral is an independent desktop icon organizer for Windows. It keeps the desktop tidy
with a set of building blocks you arrange yourself:

- **Corrals**: movable, resizable containers that hold desktop items.
- **Folder Gates**: a corral that shows a folder and lets you browse it without opening a window.
- **Camouflage**: icons take on the colours of the wallpaper behind them, for a quieter desktop.
- **Round-up**: rules that send new desktop items to the right corral, with a preview before sorting.
- **Saved desk setups**: corral placements remembered per monitor setup and restored when it returns.
- **Clear view**: double-click the desktop to hide corrals and icons, and again to bring them back.
- **Fold**: roll a corral up to just its title bar.
- **Ranges**: separate desktop pages you switch between.
- **Glance**: bring all corrals above your windows with a hotkey.

Arranging items in corrals and Round-up never move, rename or delete your files; corrals only
reference items on your desktop. Dropping files onto a Folder Gate is an ordinary Windows move
into that folder.

## Screenshots

Two Corrals and a Folder Gate (top right):

![Two Corrals and a Folder Gate](assets/screenshots/corrals.png)

The Corral menu:

![The Corral menu with its View submenu open](assets/screenshots/menu.png)

The Work corral folded to its title bar:

![The Work corral folded](assets/screenshots/fold.png)

## Requirements

- Windows 10 or Windows 11, 64-bit (x64).

## Building

You need a Rust toolchain targeting `x86_64-pc-windows-gnu`.

```powershell
cargo build --release
```

The executable is written to `target\release\corral.exe`.

Optionally, `scripts\build.ps1` wraps `cargo` (`-Release`, `-Test`, `-Check`). It uses a
portable Rust toolchain if one is found at the path given by the `CORRAL_PORTABLE_RUST`
environment variable, and otherwise uses `cargo` from your `PATH`.

## Running

`corral.exe` opens in a preview window by default, so you can try it without touching your
desktop. Start it with `--desktop` to organise the real desktop.

## Known issues

- List and Details views are not available yet; corrals show icons only.
- Desktop icons that are not in a corral stay visible through a Corral or Gate placed on top
  of them.
- In a Folder Gate, **New > Folder** creates the folder but does not start renaming it.
- In a Folder Gate, **Paste shortcut** does nothing for some clipboard contents (for example a
  file list copied by some programs).
- Opening a folder with thousands of files in a Gate for the first time froze the window for
  a few seconds once in testing. It has not happened again since.

## How this was built

Corral was built with AI coding agents, with a person deciding what to build and approving
every change that landed.

- **Claude Code as lead.** One Claude Code session plans the work, splits it into tasks and
  reviews and merges the results.
- **Codex workers on written briefs.** Each task is a short written brief that lists the files
  it may change, what "done" means and the tests to run. A Codex worker takes one brief and works
  on its own branch in a separate git worktree, so parallel tasks never edit the same files.
- **Guarded live testing.** A test driver script clicks, types and takes screenshots in a running
  Corral. It refuses any click or capture that would land on a window Corral does not own, and it
  works only with scratch files in a disposable test folder, never real desktop items.
- **Independent reviews before merging.** Branches get a read-only review by a second agent
  before they are merged; any change that can move or delete files always gets one. Nothing is
  merged until the full test suite passes.
- **Results logged per live pass.** Every live test session is written up: what passed, what
  failed, which branch fixed it and what is still open. The known issues above come from
  the latest pass.

## Settings and layouts

Settings and corral layouts are stored in a `Corral` folder under `%APPDATA%`
(`settings.json`, `layout.json`, backups). The About page in the settings window shows the
exact locations in use.

## Licence

Corral is free software under the GNU General Public License, version 3 or (at your
option) any later version. See [LICENSE](LICENSE). Licences of the third-party components
it uses are listed in [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md).
