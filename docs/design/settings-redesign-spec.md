# Corral settings redesign: implementation spec (approved by the user 2026-10-16)

This spec fixes every value the settings window needs. The pages were designed in Figma first; where a design and this
spec disagree, this spec wins.

All sizes are DIPs (1 DIP = 1 px at 96 dpi); scale by dpi/96 and snap rectangles to whole device pixels. Every design
must work at 100 %, 150 % and 250 % and in light and dark mode. Draw with Direct2D/DirectWrite only (solid fills,
strokes, linear gradients, text); no native common controls except EDIT boxes for text entry.

## 1. Tokens

Primitives:

| name | hex | name | hex |
|---|---|---|---|
| gray/white | #FFFFFF | gray/700 | #33373F |
| gray/50 | #F6F7F8 | gray/800 | #292C32 |
| gray/100 | #ECEEF1 | gray/850 | #222429 |
| gray/200 | #D7DADF | gray/900 | #1B1D22 |
| gray/300 | #B4B9C2 | gray/950 | #14161A |
| gray/400 | #8B919C | amber/300 | #F3CD9C |
| gray/500 | #6B7280 | amber/400 | #EAB477 |
| gray/600 | #494E58 | amber/600 | #B97A33 |
| red/400 | #F08A80 | amber/700 | #8F5B22 |
| red/600 | #C4382E | | |

Semantic colours (Light / Dark):

| token | Light | Dark | use |
|---|---|---|---|
| bg/window | gray/50 | gray/950 | window, nav, page background; DWM caption colour |
| bg/surface | white | gray/900 | cards, dialogs, menus |
| bg/surface-raised | gray/100 | gray/850 | segmented track, dialog footer, tip box |
| bg/control | white | gray/800 | buttons, dropdowns, fields, selected segment, slider thumb |
| bg/control-hover | gray/100 | gray/700 | hovered control |
| bg/subtle-hover | #000000 4 % | #FFFFFF 5 % | hovered nav item / row / subtle button |
| bg/subtle-selected | #000000 7 % | #FFFFFF 8 % | selected nav item, value chip, selected menu item |
| border/default | gray/200 | gray/700 | card, control and dialog outlines |
| border/strong | gray/300 | gray/600 | swatch edge, hover ring |
| border/subtle | gray/100 | gray/800 | dividers inside cards |
| control/strong | gray/500 | gray/400 | toggle-off outline and knob, checkbox/radio outline, slider ticks |
| control/track | gray/200 | gray/700 | slider track |
| text/primary | gray/950 | gray/50 | |
| text/secondary | gray/600 | gray/300 | descriptions, captions, chevrons |
| text/disabled | gray/400 | gray/500 | |
| text/link | amber/700 | amber/400 | links |
| accent/default | amber/700 | amber/400 | toggle on, checked boxes, slider fill, selection pills, primary button |
| accent/hover | amber/600 | amber/300 | hovered accent |
| accent/subtle | #8F5B22 10 % | #EAB477 14 % | icon badges, warning box, step numbers |
| text/on-accent | white | gray/950 | text/glyphs on accent fills |
| focus/ring | gray/950 | white | keyboard focus ring |
| status/danger | red/600 | red/400 | Danger button, "Delete rule" text |

The Windows accent colour is no longer used (user decision: Corral amber). Light/dark follows
`AppsUseLightTheme` as today.

## 2. Type

Segoe UI Variable (fall back to Segoe UI when absent). Figma used Inter as a stand-in; use these sizes:

| style | size / line | weight |
|---|---|---|
| Page title | 28 / 36 | Semibold |
| Dialog title | 20 / 28 | Semibold |
| Section label | 14 / 20 | Semibold |
| Body | 14 / 20 | Regular |
| Body strong | 14 / 20 | Semibold |
| Caption | 12 / 16 | Regular |
| Group label (nav) | 12 / 16 | Semibold |

Glyphs: Segoe Fluent Icons (fallback Segoe MDL2 Assets) at 16 DIP, coloured text/primary unless stated.

## 3. Window

- Standard Windows caption kept. On Windows 11 set `DWMWA_CAPTION_COLOR` = bg/window, `DWMWA_TEXT_COLOR` =
  text/primary, and `DWMWA_USE_IMMERSIVE_DARK_MODE` in dark mode (also on WM_SETTINGCHANGE theme changes).
- Default client size 1000 x 720; minimum 760 x 520. Keep the per-monitor DPI handling.
- Navigation column 256 wide (padding 16 left/right, 8 top), never scrolls. Content column: padding 24 left, 40 right,
  8 top, 40 bottom; it scrolls (overflow-only scrollbar as today, wheel step 48 DIP). Content width = available width,
  max 960; in wider windows the column is centred in the space beside the nav (user, 2026-10-20). When the content width is under 520, a setting row stacks its control under the text.
- Navigation order with group labels: Welcome · **Look**: Personalization, Camouflage · **Desktop**: Folder Gates,
  Tabs, Fold, Clear view, Glance, Ranges · **Organize**: Round-up, Layout, Backups · (12 DIP gap) About.
  Backups is a new page. Glyphs: Welcome U+E80F, Personalization U+E771, Camouflage U+E790, Folder Gates U+E838,
  Tabs U+E8A1, Fold U+E70E, Clear view U+ED1A, Glance U+E890, Ranges U+E7C4, Round-up U+E8CB, Layout U+E80A,
  Backups History (U+E81C), About U+E946.
- Glyph codepoints marked "name" below are the Segoe Fluent Icons names; verify each codepoint in the font file (as the
  existing nav table in `src/settings_ui.rs` does) and record it in that table.

## 4. Components (sizes, states, keyboard)

States everywhere: Rest, Hover, Pressed (where noted), Focus (keyboard only, see 5), Disabled = whole element at 40 %
opacity. Corner radius 4 for controls, 8 for cards/dialogs/menus/tiles.

- **Nav item** 224 x 36, radius 4, padding 12, glyph 16 + gap 12 + Body label. Hover: bg/subtle-hover. Selected:
  bg/subtle-selected, Body strong label, accent pill 3 x 16 (radius 1.5) at x 0 vertically centred.
  **Nav group label**: Group label style, text/secondary, padding 16 top / 4 bottom / 12 left, not focusable.
- **Page header**: Page title, 4 gap, Subtitle (Body, text/secondary, wraps). Optional master switch at top-right
  (10 DIP down): Body "On"/"Off" + 12 gap + Toggle. Master switch off: cards below it at 40 % and inert, except
  controls the original never disabled (stated per page). Header bottom padding 8.
- **Section label**: Section style, text/primary, 20 above, 8 below, 2 left inset.
- **Card**: bg/surface fill, 1 DIP border/default, radius 8, clips content. Rows inside are separated by 1 DIP
  border/subtle dividers inset 16 left/right; no divider after the last row.
- **Setting row**: padding 16 horizontal, 10 vertical, min height 56 (48 allowed without description), gap 16.
  Optional leading glyph 16. Text column fills: Title (Body), optional Description (Caption, text/secondary, wraps).
  Control on the right, vertically centred. For Toggle and Navigate rows the whole row is the hit target and shows
  bg/subtle-hover on hover. Navigate row control = "ChevronRight" (U+E76C) 16 in text/secondary.
- **Toggle** 40 x 20, radius 10. On: accent fill, knob 12 text/on-accent at right inset 4. Off: no fill,
  1 DIP control/strong outline, knob 12 control/strong at left inset 4. Hover: knob 14, On fill accent/hover, Off fill
  bg/subtle-hover. Space toggles.
- **Checkbox** 20 x 20, radius 4. Checked: accent fill + check glyph (text/on-accent). Unchecked: bg/control fill,
  1 DIP control/strong. Label 10 to the right; label click toggles. Only for multi-select lists.
- **Radio** 20 circle; selected = accent fill + 8 dot text/on-accent; unselected bg/control + 1 DIP control/strong.
  Only inside Radio rows.
- **Radio row**: as Setting row but radio at the left (gap 12), padding 12 vertical, title + description; whole row
  clickable; Up/Down move the selection inside the group.
- **Check row**: min height 44, padding 8 vertical; checkbox, optional glyph, Body label filling, optional
  destination chip at the right: bg/subtle-selected radius 4, padding 8/2, chevron-right glyph text/secondary +
  Body text (text/primary when checked, text/secondary otherwise); clicking the chip opens the Corral picker menu.
- **Button** height 32, padding 12, radius 4, Body label, min width 80. Primary: accent fill, text/on-accent.
  Secondary: bg/control + 1 DIP border/default, text/primary. Subtle: no fill (bg/subtle-hover on hover).
  Danger: status/danger fill, text/on-accent; only for destructive confirmations, never the default button.
- **Icon button** 32 x 32, radius 4, glyph 16; Subtle or Secondary styles; needs a tooltip.
- **Link**: Body, text/link, underline on hover. Only for navigation, never to change a setting.
- **Dropdown** 32 high, default width 220 (row dropdowns), radius 4, bg/control + 1 DIP border/default, padding 12
  left / 8 right, value text (truncated with ellipsis) + chevron-down glyph text/secondary. Open: border accent and the
  Menu below. Closed + focused: Up/Down change the value directly; Alt+Down, Enter or Space opens.
- **Menu** (popup window with DWM shadow): bg/surface, 1 DIP border/default, radius 8, padding 4, item gap 2.
  **Menu item** 32 high, radius 4, padding 8 left / 12 right: 3 x 16 accent bar slot (visible when selected) + 8 gap
  + Body label. Hover bg/subtle-hover; selected bg/subtle-selected. Up/Down/Home/End/Enter/Esc; type-ahead.
- **Segmented**: track bg/surface-raised + 1 DIP border/default, radius 6, padding 2, gap 2. Segment 28 high, equal
  width (default 96, wider for longer labels; never truncate: measure and grow the whole control), radius 4.
  Selected: bg/control + 1 DIP border/default, Body strong text/primary, accent indicator 16 x 3 (radius 1.5) centred
  at the bottom (2 DIP up). Unselected: Body text/secondary, hover bg/subtle-hover. Left/Right move and apply.
- **Slider** 240 wide, 24 high (32 with ticks). Track 4 high radius 2 control/track, inset 10 each side; fill
  accent from start to value; thumb 20 circle bg/control + 1 DIP border/default with inner dot 12 accent (14 hover,
  10 pressed). Steps type: 5 ticks 2 x 6 control/strong under the track at equal spacing; thumb snaps to a step on
  release (and while dragging shows the nearest step name). Keyboard: Left/Right one step (continuous: 1 %),
  PageUp/PageDown 10 %, Home/End.
- **Slider row**: Setting row with slider (240) + 16 gap + **value chip** (72 wide, bg/subtle-selected radius 4,
  padding 8/2, Body text/secondary, right-aligned text): "55 %" or the step name. The chip updates while dragging.
- **Swatch** 28 x 28 hit box: 20 circle colour fill with 1 DIP border/strong edge, centred. Hover: 28 ring 1 DIP
  border/strong. Selected: 28 ring 2 DIP text/primary. **Custom swatch**: 20 circle bg/control, dashed 1 DIP
  control/strong outline (2 on / 2 off), plus glyph 12; when the stored colour is not a preset, the Custom swatch
  shows that colour and the selected ring. **Auto swatch** (text colour only): circle split vertically, left half
  #FFFFFF, right half #1B1D22. Arrow keys move between swatches; Enter/Space selects.
- **Color row**: Setting row whose control is swatches (gap 4) + 16 gap + 1 x 20 divider + Icon button (eyedropper
  "Eyedropper") that picks a colour from the screen (reuse the existing colour-picker capture).
- **Color popover** (popup, anchored under the Custom swatch, 320 wide): padding 16 sides, 12 top, 16 bottom, gap 14.
  Header: 24 preview circle + "Custom color" (Body strong) + close icon button. Three gradient sliders labelled
  (Caption, text/secondary) Hue, Saturation, Brightness: track 12 high radius 6 with a linear gradient (hue: red,
  yellow, green, cyan, blue, magenta, red; saturation: grey to the full colour; brightness: black, colour, white),
  thumb 16 ring 3 DIP white with a 1 DIP dark halo. Hex row: "Hex" Caption + EDIT field 120 x 32 + eyedropper icon
  button (Secondary) at the right. Live-applies while dragging; Esc restores the colour it opened with; no OK/Cancel.
  Reuse `color_editor` models and slider maths (hue 0..255).
- **Text field**: native EDIT child in a 32-high frame, bg/control + 1 DIP border/default, radius 4, padding 10;
  focused = 2 DIP accent underline.
- **Number stepper**: bg/control + 1 DIP border/default radius 4: minus icon button, value (Body, 40 wide centred),
  plus icon button. Up/Down step 1, PageUp/PageDown 10.
- **Info box**: radius 8, padding 16/12, gap 12, glyph 16 at the top-left. Tip: bg/surface-raised, lightbulb
  ("Lightbulb") in text/secondary. Warning: accent/subtle, "Warning" (U+E7BA) in accent. Title Body strong + body Caption.
- **Expander** (card): header 48 high (padding 16/14): optional lightbulb glyph (text/secondary) + Title (Body) +
  chevron-down (rotated 180 when open). Content: padding 44 left (aligned with the title), 16 right, 14 bottom; bullet
  lines "•  text" in Caption text/secondary, gap 6. Enter/Space toggles. "How it works" expanders start collapsed
  except where a page says open; the open state is remembered per page until the window closes. The "Advanced"
  expander has no glyph and contains setting rows separated from the header by a divider.
- **Dialog** (modal popup owned by the settings window, also used by the desktop app for its prompts): width 440
  (rule editor 680), bg/surface, 1 DIP border/default, radius 8, DWM shadow. Content padding 24, gap 12: Dialog
  title, body (Body, wraps), optional checkbox lines (gap 12). Footer: bg/surface-raised, 1 DIP top border/default,
  padding 24/20, gap 8; confirmation dialogs use two equal-width buttons (secondary left, primary right); dialogs with
  a left action (Delete rule) right-align Cancel/Save. Enter = primary, Esc = secondary/close. Must follow dark mode.

## 5. Keyboard and focus

- Tab / Shift+Tab walk: navigation list (one stop; Up/Down/Home/End move the selected page, which switches the page
  immediately), the page master switch, then every interactive element of the page in reading order. Focus wraps.
- Focus ring: 2 DIP focus/ring stroke drawn 3 DIP outside the element bounds, radius = element radius + 3. Show it
  only after keyboard navigation (hide again on the next mouse click), like `:focus-visible`.
- Scroll the content so the focused element is visible. Ctrl+Tab / Ctrl+Shift+Tab switch pages.
- Esc closes an open menu/popover first; with nothing open it does nothing (the window does not close).

## 6. Behaviour rules

- Every control live-applies through the existing paths (`save` + reload request, live colour sender with its 16 ms
  throttle, style requests). No new persisted fields unless this spec says so; old settings files load unchanged.
- **Sliders feel smooth**: thumb position follows the cursor in sub-pixel DIPs; the stored value is rounded
  separately. While dragging, repaint only the slider row (and the value chip) instead of re-laying out the page; text
  layouts are cached per page/DPI/width. Measure before/after (frame time per WM_MOUSEMOVE at 100 % and 250 %) and
  report the numbers.
- Merged controls write exactly the fields listed in the page table and read them back for display.
- Text: Corral names and wording only (tests/ui_wording.rs must pass); no other product's names or (TM).
  The About notice names the licence (GPL v3 or later).

## 7. Page content (exact copy; field bindings)

Copy in quotes is the UI text. Controls: T = toggle, D = dropdown, S = segmented, SL = slider, CR = color row,
N = navigate row, B = button row, R = radio row, C = check row, X = expander.

### Welcome (no master switch)
Title "Welcome to Corral"; subtitle "Keep desktop icons in Corrals and browse folders through Gates."
- Status card (padding 16, gap 16): app icon 32 + "Corral is on" (Body strong) / "Your Corrals, Gates and shortcuts
  are active. Turn Corral off to show the plain desktop." (Caption) + T `EnableCorral` (title becomes "Corral is
  off" when off). Turning off asks with the "Turn off Corral?" dialog (U4; MessageBox until then).
- "Get started" card: three rows with a 24 circle number badge (accent/subtle, Body strong): "Create a Corral" /
  "Drag across an empty part of the desktop, then choose Create Corral here."; "Name it" / "Click the Corral title and
  type a name."; "Move and resize" / "Drag the title to move a Corral. Drag an edge to resize it."
- "Explore": tiles in 2 columns, gap 12; tile = card radius 8, padding 14/12, 36 icon badge (accent/subtle, glyph
  20) + title (Body strong) + description (Caption) + chevron; click opens the page. Personalization "Colors,
  see-through and titlebars."; Camouflage "Icons that blend into the wallpaper."; Folder Gates "Browse a folder right
  on the desktop."; Tabs "Stack Corrals in one place as tabs."; Fold "Fold Corrals down to their titles."; Clear view
  "Temporarily hide desktop icons and Corrals."; Glance "Bring Corrals above your windows."; Ranges "Organize icons across
  separate desktop pages."; Round-up "Send new icons to the right Corral."; Backups "Save and restore your layout."
- "General" card: D "Create Corrals with" / "Which mouse button draws a new Corral on the desktop." → `CreateCorralMode`
  choices "Either button" (0), "Left button" (1), "Right button" (2).

### Personalization (no master switch)
Subtitle "Choose how your Corrals look. Changes show on the desktop right away."
- Scope bar (padding 12 top / 4 bottom): "Apply changes to" + D (220) "All Corrals" + each Corral by title (today's
  `select_color_target` list; Corrals with their own look show a Caption suffix "own look" in the menu) + flexible
  gap + Subtle reset button whose label always names its scope: **"Reset all Corrals…"** while All Corrals is selected,
  **"Reset \"<title>\"…"** while one Corral is selected (truncate long titles with an ellipsis, full title in the
  tooltip). The button always opens a Reset dialog (§8); nothing resets without it.
- Scope caption under the bar (Caption, text/secondary, 14 bottom): with All Corrals selected and N > 0 Corrals that
  have their own look: "N Corral(s) have their own look: <titles, max 3, then "and N more">. Pick one above to edit
  only that Corral."; N = 0: "All Corrals share this look."; one Corral selected: "Changes apply only to \"<title>\"."
  (plus " It has its own look." when it does).
- Rows whose setting has no per-Corral value are shown disabled with the description replaced by "Applies to all
  Corrals." while a single Corral is selected.
- **Live sample** (full content width x 200, radius 8, 1 DIP border): the current wallpaper (crop centred on the
  primary monitor, scaled to cover) with one sample Corral (320 x 150, centred, title "Downloads", four sample icons
  with labels) painted by the real Corral paint code (`group_paint`) using the current scope's style: colour,
  see-through, blur, rounded corners, outline, title visibility (On hover shows the title), title text colour, shade
  and tint. Repaints within 16 ms of any change on this page. Caption under it (8 gap): "Your wallpaper with a sample
  Corral. It follows every change on this page."
- "Background" card:
  - CR "Color" / "Behind the icons and the titlebar." presets #292C32 #494E58 #3F6FB5 #2E8B8B #3D8B57 #B97A33 #B8505F
    #7A5BA8 + Custom (popover) + eyedropper → background colour model (today's Bg sliders).
  - SL "See-through" / "How much of the wallpaper shows through." → `BgTransparency`, shown as % of 0..2000.
  - SL steps "Blur" / "Softens the wallpaper behind Corrals. Also used by Camouflage." → steps Off 0, Subtle 15,
    Soft 30, Medium 60, Strong 120 (`background_blur_radius`; blur enabled iff non-zero). A stored
    value between steps shows the nearest step name and is not rewritten until the user moves the slider.
  - T "Rounded corners" → `!square_edges`. T "Outline" / "A thin line, lighter or darker than the Corral, around each
    Corral." → `outline`.
- "Titlebar" card: S "Show the titlebar" / "On hover keeps the desktop calmer." Always (LabelMode 2) / On hover (1) /
  Never (0). CR "Text color" / "Auto picks light or dark text for contrast." Auto (= today's reset to automatic) +
  #FFFFFF #D7DADF #8B919C #1B1D22 #F3CD9C #9DB8E2 #F08A80 + Custom + eyedropper. S "Titlebar shade" / "Darken the
  titlebar so the name stands out." None / Dark / Darker → `darken_titlebars` 0/1/2.
- "Icons" card: S "Tint icons" / "Color the icons inside Corrals." Off (`TintIcons` off) / Match Corral (on, mode 0)
  / Custom (on, mode 1). CR "Tint color" / "Used when Tint icons is Custom." (Corral palette; disabled unless
  Custom). N "Shortcut arrows" / "Choose which shortcut arrows to hide on icons in Corrals." → Shortcut arrows dialog.
- X "Advanced" (collapsed by default), toggles: "Titlebar buttons on hover" / "Show the menu and close buttons when you
  point at a titlebar." (`use_titlebar_controls`, keeps the ButtonsHidden notice); "Titlebar buttons on inactive tabs"
  / "Also show them on tabs that are not selected." (enabled only when the previous is on); "Titlebar below near the
  screen top" / "When a Corral touches the top edge, its titlebar moves underneath." (`title_edge_auto_adjust`, same
  release semantics and notice as today's style menu command); "Animate icon movement" / "Icons glide to their new
  place when Corrals are sorted or resized." (`AnimateIcons`); "Scrollbars only on hover" / "Hide Corral scrollbars
  until you point at the Corral." (`HideScrollbars`); "Restore see-through on hover" / "A faded Corral becomes solid
  while you point at it." (`FadeInOpacity`, keeps its show gate `FadedGroupsVisible`).
- Removed: both style popup menus, the "Color picker" button, the HSB slider rows, Tab Style (moved to Tabs), the
  "Pick background color" path (already removed).

### Camouflage (master switch = `BlendIcons`)
Subtitle "Icons take on the colors of the wallpaper behind them, for a quieter desktop."
- Live desktop preview (today's Camouflage preview, monitor-shaped box: full content width up to 45 % of the window
  height, then narrower and centred - never letterbox bars; radius 8, 1 DIP border); caption "Live view of your main desktop, with your real Corrals and Folder Gates."
- "Blending" card: SL "Blend strength" / "How strongly icon colors follow the wallpaper." → `BlendIconOpacity` as
  0..100 % (0 % keeps original colours). SL steps "Wallpaper blur" / "Smooths the colors Camouflage
  picks up. Same setting as Blur in Personalization." (same field and steps as Personalization Blur). T "Use the icon
  tint color" / "Blend icons toward your tint color from Personalization instead of the wallpaper." →
  `BlendUsesTintColor`.
- "Show real colors" card: T "When you point at a Corral" / "Its icons show their real colors while the pointer is
  over it." → `BlendColorOnMouseover`; T "While the desktop is active" / "All icons show their real colors after you
  click the desktop, until you switch to a window." → `BlendColorOnActive`.
- X "How it works" (collapsed): "Camouflage samples the wallpaper behind each icon and shifts the icon's colors toward
  it." / "A Corral can have its own Camouflage setting in its color editor." / "Point at a Corral to see the real
  colors when that option is on."
- Keep the existing applicability gate (`BlendOptionsVisible`: global or any per-Corral Camouflage). Removed: the
  Options popup, the native trackbars, "0 = sharp" and "Suggested" notes.

### Folder Gates (no master switch)
Subtitle "Show a folder on the desktop and browse it without opening a window."
- X "How it works" (open): "Create a Gate: drag across the desktop and choose Create Folder Gate here, or hold Alt
  while you drop a folder on the desktop." / "Change its view, icon size or sort order from the menu in the top-left
  corner; Ctrl + mouse wheel resizes icons." / "Removing a Gate never deletes the folder. Double-click empty space
  inside a Gate to open the folder in Explorer."
- "Settings" card: T "Subfolders use the parent folder view" / "When you open a subfolder in a Gate, it keeps the view
  of the folder above it." → `InheritViewFromParent`.

### Tabs (no master switch)
Subtitle "Stack several Corrals in one place and switch between them with tabs."
- "Tab style": four selectable tiles in one row (gap 12, equal width): card radius 8 (selected: 2 DIP accent border),
  padding 10, a 64-high preview (bg/surface-raised radius 6) drawing a tab strip in that style, then Radio + label:
  "Menu only" (0), "Flat" (1), "Segmented" (2), "Rounded" (3) → `TabsStyle`. Left/Right move the selection.
- "Behavior" card: T "Make tabs by dropping one Corral onto another" / "Turn off to stop new tab groups. Existing tabs
  stay." (`TabsCreate`); T "Ask before combining Corrals" / "Confirm each time you drop a Corral onto another one."
  (`TabsConfirm`); T "Switch tabs on hover" / "Point at a tab to show it, without clicking." (`TabsMouseover`);
  T "Wait briefly before switching" / "Avoids switching when the pointer just passes over a tab."
  (`TabsMouseoverDelay`, shown while hover switching is on); T "Color each tab like its Corral" / "Shown when at least
  one Corral has its own color." (`TabsColors`, existing show gate).
- X "How it works" (collapsed): "Drop one Corral onto another to stack them as tabs." / "Hold Shift and drag a tab to
  reorder it, or drag it away to make it a separate Corral again."

### Fold (master switch = `RollupEnabled`)
Subtitle "Fold a Corral down to its title and open it only when you need it."
- "Folding" card: T "Double-click a title to fold or unfold" / "The quickest way to fold a single Corral."
  (`RollupDoubleClick`); T "Fold Corrals docked at a screen edge" / "Corrals pushed against the top or bottom edge
  fold automatically." (`RollupOnDock`).
- "Opening" card: S "Open a folded Corral" / "On hover is fastest; On click never opens by accident." On hover
  (RequireClick off, RequireDelay off) / After a pause (RequireClick off, RequireDelay on) / On click (RequireClick
  on; RequireDelay left unchanged).
- X "How it works" (collapsed): "Double-click a Corral title to fold it, and again to unfold it." / "Point at a folded
  title to see its icons; move away and it folds again." / "Drag a Corral to the top or bottom edge of the screen to
  dock it there."

### Clear view (master switch = `QuickHideEnabled`)
Subtitle "Double-click an empty part of the desktop to hide your icons. Double-click again to bring them back."
- X "How it works" (open): "Double-click an empty spot on the desktop: icons fade out. Double-click again to show
  them." / "To keep a Corral visible, right-click inside it and choose View > Keep visible in Clear view." / "Clear
  view only hides. It never moves or deletes files."
- "What to hide" card: S "Hide" / "Everything means icons and Corrals. Loose icons are the ones outside any Corral."
  Everything (0) / Loose icons (1) / Corrals (2) → `QuickHideWhat`. Stays enabled when the master switch is off (as
  the original).
- "Automatically" card: D "Hide when the desktop is idle" / "Icons fade out after this long without activity." Never
  (`AutoHideIdle` off, choice kept) / Immediately (1) / After 1 second (1000) / After 30 seconds (30000) / After 2
  minutes (120000) / After 10 minutes (600000) (on + `AutoHideIdleChoice`). T "Show icons when you click the desktop"
  / "Hidden icons come back as soon as the desktop is active." (`AutoShow`). T "Show icons while you drop files on
  the desktop" / "So you can see where a dragged item will land." (`AutoShowOnDrag`, enabled while AutoShow is on).
  T "Start with icons visible" (`AutoShowOnBoot`; disabled while idle hiding is on, then its description reads "Not
  available while idle hiding is on.").

### Glance (no master switch)
Subtitle "Bring your Corrals above open windows with a shortcut, the tray icon or a screen corner."
- "Shortcuts" card: T "Glance shortcuts" / "Use the keys below anywhere in Windows." (`HotkeysGlance`); D "Show Corrals
  until you press again" / "Press once to show, again to hide." (`HotkeyGlanceToggle`); D "Show Corrals while held" /
  "Corrals stay up only while you hold the keys." (`HotkeyGlanceHold`); T "Keep Glance open when you click other
  windows" / "Otherwise a click elsewhere ends Glance." (`PeekKeepOpen`). Dropdowns and Keep open disabled while
  shortcuts are off. Hotkey choice labels: as `HOTKEY_CHOICES`, with "Control" written "Ctrl".
- On builds below 26100 only, same card adds: T "Speed up Show desktop (Win + D)" (`HotkeysDesktop`), D "Show the
  desktop until you press again" (`HotkeyDesktopToggle`), D "Show the desktop while held" (`HotkeyDesktopHold`).
- "Tray icon" card: T "Show the Corral icon in the notification area" / "Click it to Glance at your Corrals."
  (`ShowInTaskbar`). The dead "Customize icon" link is removed.
- "Screen corner" card (Windows 11 only, existing gate): T "Use the bottom-right corner" / "Click the far corner of the
  taskbar." (`HotCornerEnabled`); D "Left click" and D "Right click" without descriptions (`HotCornerLeft/Right`,
  existing choice lists), shown while the corner is on.

### Ranges (master switch = `SwipeEnabled`)
Subtitle "Organize icons and Corrals across separate desktop pages and slide between them. Not related to Windows
virtual desktops." (Ranges must never read as Windows virtual desktops.)
- X "How it works" (collapsed): "Pull the left or right edge of the desktop sideways to move to the next Range." /
  "To add a Range or carry an item over, drag it to the screen edge and hold it there for a moment." / "With the
  keyboard: click the desktop, then press Alt + Left or Alt + Right."
- "Switch Ranges by" card, check rows: "Pulling a screen edge" (`SwipeDragEdge`), "Swiping with two fingers"
  (`SwipeTouch`), "Alt + arrow keys or Alt + mouse wheel" (`SwipeKeyboardWheel`), "Dragging with the middle button"
  (`SwipeMiddleDrag`), "Dragging with both buttons" (`SwipeDualDrag`).
- "Animation" card: S "Animation" / "How the desktop moves when you change Range." Off / Slide / Bounce →
  `swipe_animation_style` 0/1/2. T "Show hidden icons when you change Range" / "Brings back icons hidden by Clear
  view." (`SwipeShowIcons`).

### Round-up (no master switch)
Subtitle "Send new desktop icons to the right Corral automatically."
- "New icons" card: D "When no rule matches" / "Where a new icon goes if none of the rules below applies." → the
  default destination ("Leave it on the desktop" for the general desktop, then each Corral; today's "By default..."
  link).
- "By file type" card: check rows for the seven presets (Folders, Documents, Images, Music and video, Downloads, Web
  links, Program shortcuts) with destination chips ("Choose a Corral" when unset).
- "Your rules" card: every stored name / shortcut-target / time / custom rule as a check row with its description and
  destination chip; last row a Subtle button "+  Add a rule…" opening the rule editor; row context menu (right-click
  or Shift+F10): "Edit…", "Rename…", "Delete…".
- "Running" card: T "Keep Round-up active" / "Also re-sort icons already on the desktop whenever they change."; B
  "Sort the desktop now" / "Apply every rule to the icons that are on the desktop right now." button "Run now"
  (today's "Run Round-up now"); N "Rule priority" / "Choose which rule wins when several match the same icon." (today's
  order dialog).

### Layout (no master switch)
Subtitle "How Corrals line up, and what happens to them when your monitors change."
- "When monitors change" card (existing `LayoutAuto` gate): R "Remember a layout for each monitor setup" / "Works with
  one or more monitors. Recommended." (2); R "Scale Corrals to fit the display" / "For a single monitor whose
  resolution changes." (1); R "Don't adjust" / "Corrals keep their pixel positions and can end up off screen." (0)
  → `LayoutPreserve`. B "Swap monitors" / "Move the Corrals of your first two monitors to the other screen." button
  "Swap" (existing `MultiMonitor` gate).
- "Snapping" card: T "Snap Corrals while moving" / "Corrals jump to neighbouring edges and to each other."
  (`SnapEnabled`); T "Keep a gap between Corrals" / "Snapped Corrals stay a little apart." (`SnapSpaced`); stepper row
  "Gap" / "In pixels at 100 % scaling." (existing spacing value 0..100); T "Resize in whole icon cells" / "Edges stop
  where a full row or column of icons fits." (`SnapSizes`); T "Align edges with nearby Corrals" / "Guides appear when
  edges line up." (`SnapLines`). Gates as today.
- Tip box (12 above): "Hold Shift to skip snapping" / "While you drag or resize a Corral, Shift turns snapping off for
  that move."
- "Icon spacing" card: T "Match the desktop icon spacing" / "Corrals use the same spacing as the desktop. Windows' own
  spacing is never changed." (`GroupSpacingMatchesDesktop`).

### Backups (new page, no master switch)
Subtitle "Save your Corral layout and bring it back later."
- Card: B "Save a backup now" / "Includes every Corral, Gate and tab on all monitors." with a **Primary** button
  "Save backup" (asks for a name with the name dialog, default name = date); T "Save a backup every day" / "Corral
  keeps the daily backups of the last seven days." (`DailyBackups`).
- "Saved backups" card: one row per backup, newest first: 32 badge (accent/subtle, History glyph) + name (Body) +
  "<date> · Automatic" or "<date> · Saved by you" (Caption) + Secondary "Restore" + Subtle icon button "More" (U+E712)
  opening a Menu: "Rename", "Update to current layout" (re-capture), "Delete". Empty list: Caption "No backups yet.
  Save one to keep your current layout."
- Tip box: "Restoring is safe to try" / "Before a restore, Corral offers to back up the current layout first. Files on
  the desktop are never moved or deleted."

### About
- Hero (16 top / 8 bottom, gap 20): app icon 64 + "Corral" (Page title) + "Version <CARGO_PKG_VERSION>" (Body,
  text/secondary).
- Card (padding 16/14, gap 6): "An independent desktop organizer for Windows: Corrals, Folder Gates, Tabs, Ranges and
  more." (Body) / "Free software under the GNU General Public License, version 3 or later." (Caption).
- "Files" card: B "Settings and layouts" / the settings folder path (e.g. `%APPDATA%\Corral`, shown expanded) button
  "Open folder" (opens Explorer on that folder; new Corral extra).
- Removed: the inert "Use system language" dropdown and "Troubleshooting" link.

## 8. Dialogs (copy)

- **Shortcut arrows** (440): "Shortcut arrows" / "Hide the small arrow on these kinds of shortcuts inside Corrals.
  Icons on the desktop itself keep their arrows." checkboxes "Web links", "Program shortcuts", "Folder shortcuts",
  "Document shortcuts" (icon_overlay flags, live-apply); footer right: Primary "Done".
- **Round-up rule** (680): "Round-up rule" / "Icons that match these conditions go to the Corral you pick."
  Condition rows (gap 8): first row label "Where" (Body text/secondary, 72 wide), later rows D and/or (72); D kind
  (170); D operator (140); value field or dropdown (fills); remove icon button (×) when more than one condition.
  Subtle "+  Add a condition" aligned with the kind column. Divider. Field rows (label 180 wide + field): "Send
  matching icons to" D (240) Corral picker; "Rule name" text field with Caption "Leave it to name the rule
  automatically.". Footer: Subtle "Delete rule" with status/danger text (left, only for existing user rules; "Reset to
  default" for preset rules), then Secondary "Cancel" and Primary "Save" (right).
- **Restore "<name>"?** (440): "Your Corrals go back to how they were <date>. Files on the desktop are not moved or
  deleted." checkboxes "Back up the current layout first" (default on) and "Don't ask again" (the
  `SnapshotBeforeRestore` tip; when it is set, restore without asking, honouring the remembered backup choice);
  buttons "Cancel" / Primary "Restore".
- **Rename backup** / **Name this backup** (440): name field + Caption "Up to 255 characters."; "Cancel" / Primary
  "Save". Empty or too-long names show the error as Caption in status/danger under the field (no message box).
- **Delete "<name>"?**: "This backup is removed for good. Your current layout isn't affected." "Cancel" / Danger
  "Delete".
- **Update "<name>"?**: "Replace this backup with the current layout?" "Cancel" / Primary "Update".
- **Turn off Corral?**: "Your Corrals and Gates disappear and Windows shows the desktop icons. When you turn Corral back
  on, it puts every icon back in place. Changes made to the desktop meanwhile may be undone." "Cancel" / Primary
  "Turn off". (Turning on needs no dialog.)
- **Combine into tabs?**: "\"<A>\" and \"<B>\" will share one place, as two tabs. Hold Shift and drag a tab away to
  separate them again." checkbox "Don't ask again" (turns `TabsConfirm` off); "Cancel" / Primary "Combine".
- **Stop sorting this Corral?**: "You moved an icon inside a sorted Corral. Stop sorting it so you can arrange its
  icons by hand?" checkbox "Always use this answer"; "Keep sorting" / Primary "Stop sorting". Desktop variant keeps the
  existing desktop wording.
- **Remove rule?** "Remove this Round-up rule?" "Cancel" / Danger "Remove". **Reset rule?** "Reset this rule to its
  default conditions?" "Cancel" / Primary "Reset".
- **Reset the look of all Corrals?** (480): "The color, see-through and text color that all Corrals share go back to
  the defaults." Two radio lines (only when at least one Corral has its own look; otherwise no choice is shown):
  "Keep Corrals with their own look" / "<titles> keep their colors. Recommended." (default; = `ResetBase`) and "Reset
  every Corral" / "<titles> lose their own look too. This cannot be undone." (= `ResetAll`). Caption: "Blur,
  corners, outline, titlebar options and Advanced settings are not changed." Buttons "Cancel" / Primary "Reset".
  With no own looks the action is `ResetBase` (identical to `ResetAll` then).
- **Reset "<title>"?** (440): "Its own color, see-through, text color, tint and Camouflage choice are removed, so it
  uses the look all Corrals share. Other Corrals are not changed." (= `ResetGroup`). "Cancel" / Primary "Reset".
  These replace today's implicit three-way "Reset to default"/"Reset all to default" link and its message boxes.
  [observed in code] `ResetBase` resets the shared colour, text colour, see-through and Camouflage backgrounds, and
  turns off a tint only when it followed the Corral colour (`style_request::reset_base`).
- **Notices** (one Primary "OK", full width): "Titlebar buttons are hidden" / "You can still open a Corral's options:
  right-click its titlebar."; the title-underneath notice and the backup timeout / still-working messages keep their
  current text in this layout.
- **Rule priority**: "Rule priority" / "Rules higher in the list win when several match the same icon." list (card
  with rows, selected row bg/subtle-selected + accent bar) + Up/Down icon buttons at the right; "Cancel" / Primary
  "Save". Alt+Up/Alt+Down move the selected rule.

## 9. Acceptance checks (user, 2026-10-16) - every screen must meet these

Every screen is covered, with offline tests where possible and a live checklist entry for
the rest. Golden renders include **120 dpi (125 %)** and 144 dpi (150 %) besides 96 and 240.

1. **Navigation stays usable while content scrolls**: the nav column never scrolls or moves; nav hit-testing and
   keyboard selection are independent of the content scroll offset (test: hit-test nav items at several scroll
   offsets). The content scrollbar never overlaps nav or the caption.
2. **No awkward clipping**: at the minimum client size (760 x 520) and at 96/120/144/240 dpi, the page title, subtitle,
   master switch, every row title and every control are fully inside the content column; text wraps instead of being
   cut; segmented controls and dropdowns are never narrower than their measured text; the stacked row layout kicks in
   under 520 DIP content width. Test by laying out every page at those sizes and asserting that no element rectangle
   exceeds the content bounds and no single-line label is narrower than its measured text.
3. **Expanding "Advanced" (or any expander) does not jump**: the expander header stays at the same on-screen position
   when it opens or closes; content below it moves, content above it does not; if the opened content would be off
   screen, scroll just enough to show its first row (never more). Same for gate changes that show/hide rows (e.g. the
   hover-delay toggle): rows above the changed control keep their screen position. Test the scroll anchoring maths.
4. **Keyboard focus scrolls into view**: moving focus to an element outside the viewport scrolls the minimum amount to
   show it fully with 16 DIP margin, including its focus ring; Shift+Tab upward likewise; the page header master switch
   scrolls to the top. Test with a tall page (Personalization, Round-up).
5. **Comfortable at 125-150 %**: at 120 and 144 dpi the default window (1000 x 720 DIP) fits a 1920 x 1080 work area
   at 125 % and 1366 x 768 at 100 %; if the work area is smaller, open at 90 % of it (never larger than the monitor);
   no 1-px seams, blurry text or half-pixel borders (rectangles snapped to device pixels; DirectWrite grayscale
   antialiasing as today).
6. **Menus, popovers and dialogs are crisp in every window state**, including a maximized settings window on a
   high-DPI monitor (e.g. 3840 x 2160 at 250 %) and on a second monitor with a different DPI: every popup is created
   per-monitor-DPI-aware at the DPI of the monitor it appears on and rendered at device pixels (no 96-dpi bitmap
   stretched by DWM), re-rendered on WM_DPICHANGED, and kept inside that monitor's work area. Maximized: the content
   column grows to 960 DIP and is centred beside the nav; nothing stretches. Test that popup surfaces are
   sized from the popup's own DPI; live-check maximized at 100 % and 250 % and across both monitors.
