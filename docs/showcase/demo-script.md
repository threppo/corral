# Demo recording plan (45–60 seconds)

A short screen recording of Corral's preview window with throwaway sample files. Nothing on the
real desktop is touched: the preview shows only the scratch folder.

**Recorded on 29 October 2026:** [corral-demo.mp4](https://github.com/Jiigsi/corral/releases/download/v0.1.0-alpha/corral-demo.mp4)
(57 seconds), also shown as a GIF in the README. Claude Code rehearsed each step and then ran it
with the guarded test driver, using the v0.1.0-alpha `corral.exe`, and recorded only Corral's own
windows with ffmpeg. Two things changed from the first draft of this plan:

- Renaming goes through the Corral menu. The rehearsal showed that clicking the title does not
  start a rename, although the Welcome page says it does (see Known issues in the README).
- The Settings window was opened on its own (`corral.exe --settings --settings-path <file>`)
  instead of from a Corral's menu, so the Personalization page, which shows the real wallpaper,
  never appears.

## Before recording

1. Build: `cargo build --release` (or use `corral.exe` from the release page).
2. In the folder you run Corral from, create the scratch area. The preview only accepts a scratch
   folder inside `analysis\qa` under the current folder:

   ```powershell
   $demo = "$PWD\analysis\qa\demo"
   New-Item -ItemType Directory -Force "$demo\items", "$demo\home\Holiday 2026", "$demo\home\Recipes" | Out-Null
   'Meeting notes','Packing list','Trip plan','Reading list','Budget','To do' | ForEach-Object { Set-Content "$demo\items\$_.txt" 'Sample file' }
   'Shopping','Plan' | ForEach-Object { Set-Content "$demo\home\$_.txt" 'Sample file' }
   Set-Content "$demo\home\Recipes\Pancakes.txt" 'Sample file'
   # One Folder Gate on demo\home, made in advance so no folder picker (showing your own
   # folders) appears in the recording.
   $gate = @{ id = 1; title = 'Home folder'; monitor = ''; content = @{ kind = 'portal'; path = "$demo\home" }
              rect = @{ left = 600; top = 60; right = 1020; bottom = 310 }; style = @{}; swipe_index = 0; all_pages = $false }
   $layout = @{ version = 1; next_id = 2; groups = @($gate); loose = @{}; seen_initialized = $true }
   [IO.File]::WriteAllText("$demo\layout.json", ($layout | ConvertTo-Json -Depth 6))
   # Blur off, so the wallpaper never shows through the Corrals.
   [IO.File]::WriteAllText("$demo\settings.json", '{ "BackgroundBlurEnabled": false }')
   ```

3. Start the preview on it:

   ```powershell
   $env:CORRAL_QA_ITEMS_DIR = "$demo\items"
   .\corral.exe --preview --layout "$demo\layout.json" --settings-path "$demo\settings.json"
   ```

4. Make the recording clean: close other windows and turn on Do Not Disturb.
5. Never drag an icon **out** of the preview window onto Explorer or the real desktop: that is an
   ordinary Windows move of the sample file.
6. Record the preview window with the Snipping Tool (Win+Shift+R, then select the window area).

## Shot list

As recorded:

| Time | Action | Caption |
| --- | --- | --- |
| 0–4 s | Preview with the sample icons loose on the "desktop" and the Home folder Gate. | "Corral: a desktop icon organizer for Windows" |
| 4–14 s | Drag across empty space, choose **Create Corral here**; open the Corral's menu (☰), choose **Rename Corral**, type "Documents", Enter. | "Draw a Corral, then name it" |
| 14–28 s | Drag four sample files into it; drag its right edge to make it wider. | "Drag icons in, resize to fit" |
| 28–35 s | In the Home folder Gate, double-click "Recipes", then click the Up arrow in its title. | "Folder Gates: browse a folder right on the desktop" |
| 35–41 s | Double-click the Documents title to fold it, then again to unfold. | "Fold a Corral down to its title bar" |
| 41–52 s | Settings window: Welcome, then Round-up, then Glance. | "Settings: rules for new icons, Glance shortcuts" |
| 52–57 s | Back on the tidy preview. | `github.com/Jiigsi/corral` |

If a step does not behave as described, keep it out of the recording and note it as an issue
rather than re-taking until it works by luck.

## Optional, desktop mode only

Glance (Win+Space brings Corrals above open windows) and Clear view (double-click the desktop to
hide everything) only make sense on the real desktop. Record them only on a desktop prepared for
it: sample icons only, no personal files or windows visible.

## Afterwards

Delete `analysis\qa\demo` and clear the variable (`Remove-Item Env:CORRAL_QA_ITEMS_DIR`).
