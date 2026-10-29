# Demo recording plan (45–60 seconds)

A short screen recording of Corral's preview window with throwaway sample files. Nothing on the
real desktop is touched: the preview shows only the scratch folder.

This plan has not been rehearsed yet. The steps follow the current code and earlier test runs, so
try them once before recording.

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
   ```

3. Start the preview on it:

   ```powershell
   $env:CORRAL_QA_ITEMS_DIR = "$demo\items"
   .\corral.exe --preview --layout "$demo\layout.json" --settings-path "$demo\settings.json"
   ```

4. Make the recording clean: close other windows, turn on Do Not Disturb, and use a plain
   wallpaper (Corral's blur and Camouflage pick up colours from it).
5. Never drag an icon **out** of the preview window onto Explorer or the real desktop: that is an
   ordinary Windows move of the sample file.
6. Record the preview window with the Snipping Tool (Win+Shift+R, then select the window area).

## Shot list

| Time | Action | On screen / caption |
| --- | --- | --- |
| 0–5 s | Preview with the sample icons loose on the "desktop" and the Home folder Gate. | "Corral: keep desktop icons organised" |
| 5–14 s | Drag across empty space, choose **Create Corral here**, type "Documents". | A Corral appears and gets its name. |
| 14–22 s | Drag four sample files into it; drag its edge to resize. | Icons snap into the Corral's grid. |
| 22–32 s | In the Home folder Gate, double-click "Recipes", then click the Up arrow in its title. | "Folder Gates: browse a folder right on the desktop" |
| 32–38 s | Double-click the Documents title to fold it, then again to unfold. | "Fold: roll a Corral up to its title" |
| 38–52 s | Right-click a Corral title, **Configure Corral...**; show Welcome, then Round-up, then Glance. | "Settings, with rules for new icons" |
| 52–60 s | Close Settings; end on the tidy preview. | `github.com/Jiigsi/corral` |

If a step does not behave as described, keep it out of the recording and note it as an issue
rather than re-taking until it works by luck.

## Optional, desktop mode only

Glance (Win+Space brings Corrals above open windows) and Clear view (double-click the desktop to
hide everything) only make sense on the real desktop. Record them only on a desktop prepared for
it: sample icons only, no personal files or windows visible.

## Afterwards

Delete `analysis\qa\demo` and clear the variable (`Remove-Item Env:CORRAL_QA_ITEMS_DIR`).
