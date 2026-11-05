# How the demo was recorded

[corral-demo.mp4](../../assets/screenshots/corral-demo.mp4) (56 seconds, 1264 × 700), also shown as
a GIF in the README. Recorded on 5 November 2026. No person touched the mouse: Claude Code wrote the
flow, a script drove it, and it ran in real time, with nothing sped up.

## What it shows

| Time | What happens | Caption |
| --- | --- | --- |
| 0–5 s | A scattered desktop and Corral's first-start question. | "A messy desktop. Corral asks before it touches anything." |
| 5–12 s | **Sort my desktop**: Programs, Files, Folders and a Downloads Gate. | "One click: Programs, Files, Folders and a live Downloads Gate." / "Your files stay exactly where they are. Only the view changes." |
| 12–21 s | Drawing a Corral on empty space, then naming it from its menu. | "Draw your own Corral anywhere." / "Name it." |
| 21–29 s | Ctrl-clicking three images and dragging them in. | "Select a few icons and drag them in." |
| 29–36 s | A new image appears and goes to Photos by a rule. A new file also shows up in the Gate. | "New photo on the desktop? Round-up files it by your rules." |
| 36–41 s | Opening a folder inside the Gate and going back up. | "A Gate is a real folder, live on your desktop. Browse it right there." |
| 41–47 s | Folding three Corrals. | "Fold what you don't need down to its title bar." |
| 47–52 s | Clear view: hiding everything and bringing it back. | "Double-click the desktop and everything hides." / "Double-click again and it's all back." |
| 52–56 s | End card. | "Corral: a desktop icon organizer for Windows · github.com/threppo/corral" |

## What was set up in advance

The features in the video are real, but it is a prepared demo, not a recording of someone's desktop:

- It runs in Corral's **preview window**, with only the inside of the window recorded. All files are
  one-byte samples in a scratch folder.
- The scattered layout was written in advance with a fixed seed.
- The **Round-up rule** ("Images go to Photos") was already in the settings file. The video does
  not show it being created, and the script creates the new image.
- In the sample world Corral only watches Gate folders, so the script also adds a file to Downloads,
  whose change notice makes Corral re-read the sample desktop. On a real desktop the notice comes
  from the desktop folder itself.
- The build is v0.2.1 plus two test-only hooks. They let a sample-file preview ask the first-start
  question over a placed layout and show a sample Downloads folder. They do nothing without their
  test variables.
- Captions and the rings marking clicks were added afterwards with ffmpeg, timed from the script's log.

## How it was driven

Maestro-style tools find controls through the accessibility tree. The first step was to check
whether that could work here, so `windows-gui-mcp` 0.1.0 was installed in a throwaway environment
and asked, read-only, to dump Corral's windows:

| Window | What UI Automation sees |
| --- | --- |
| Settings | Named controls with stable ids, such as `Navigation.Rules` (53 nodes) |
| Preview, Corrals and Gates | Blank panes: no names, no ids, nothing inside them |

Corral draws its own Corrals, so there is nothing to select. That tool also has no drag action, and
its fallback click reads the whole screen and clicks matching text without checking which window it
belongs to. It was not used to drive anything.

The flow instead goes through Corral's own guarded test driver, using coordinates inside the
preview. Before every pointer move it checks that the window under the pointer is Corral's. Once
Corral has had the foreground, any other app taking it stops the run, and so does anything covering
the recorded area. ffmpeg's `gdigrab` records only that rectangle, and it stops before any other
window comes back.

## Results

See [Demo recording (5 November 2026)](verification.md#demo-recording-5-november-2026) in the
verification record.
