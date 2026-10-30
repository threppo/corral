# Corral painting: body, title strip, outline, shades, tabs, folded look and title glyphs (design spec, P8 + P9)

Status: approved by the user 2026-10-21 (open questions resolved as the spec recommends, except question 9: no Back/Forward buttons)
Date: 2026-10-21
Author: Claude subagent (clean spec writer), inputs: spec-inputs-wave-c-2026-10-21.md + approved Corral design docs + public docs only

Terms: "Corral" includes Folder Gates and tab groups (one rectangle). DIP = 1/96 inch. `px(d) = floor(d * dpi / 96 + 0.5)`
(round half up, as in the grid spec). "Outer rect" = body plus title strip. All colours are straight (not premultiplied) 8-bit
sRGB unless stated. Every size is a proposal for review, not a measured value.

## 1. Goal and non-goals

Goal: one pure, testable description of how a Corral is drawn, so that the desktop, the Personalization live sample and the
Tabs preview tiles show the same pixels, and so that titles, glyphs and outlines stay readable on any wallpaper, at any DPI
from 96 to 240, in high contrast, and while faded.

Non-goals:
- Icons, labels, the scrollbar and Camouflage maths (camouflage-spec, scrollbar-spec). Painting only calls `recolor_color`.
- Hit testing zones (grid-spec 3.1), tab drag and drop, menus, tooltips, animation of icon positions.
- Bitmaps for chrome. Everything here is Direct2D fills and strokes plus DirectWrite text.
- New settings. Every key is already stored (packet B); no UI changes.

Entry point: `paint_corral(&mut Target, &PaintInputs)`, plus pure helpers `strip_px`, `layout_title`, `see_through_alpha`,
`title_overlay`, `outline_color`, `auto_text`, `tab_layout`. `PaintInputs` carries: outer rect (px), dpi, `TitleSide`,
`LabelMode`, `reveal` (0..1), `folded`, `square_edges`, resolved `Color`, `TextMode` (Auto or Custom), `see_through` (stored
value), `shade` (0/1/2), `outline`, optional blurred backdrop, optional `(Palette, strength)` for Camouflage backgrounds,
`fade` (0..1), `hover` state, tab list and `TabsStyle`, button flags, `high_contrast`.

## 2. Geometry

### 2.1 Constants (DIPs, scaled with `px`, each rectangle edge snapped to whole device pixels)

| Name | DIP | Reason |
|---|---|---|
| `STRIP_MIN` | 24 | grid-spec `TITLE_H`; also the 24 x 24 minimum comfortable pointer target |
| `STRIP_PAD_V` | 4 | text line (16) plus 2 x 4 gives exactly 24 with the default font |
| `TITLE_FONT` | 12 | settings Caption size; small enough for a 24 DIP strip |
| `RADIUS` | 8 | same as cards and dialogs in the settings kit |
| `EDGE_PAD` | 8 | text start/end distance from the strip edge when no button is there |
| `BTN_W` | 24 | button hit box width (height is the strip height) |
| `GLYPH` | 14 | leaves 5 DIP around the glyph inside the 24 DIP box |
| `PLATE_INSET`, `PLATE_R` | 2, 4 | hover plate inside the box, same radius as settings controls |
| `TEXT_GAP` | 4 | clear space between a button and the title text |
| `TEXT_MIN` | 24 | narrowest title span worth drawing (about two letters plus an ellipsis) |
| `ALPHA_FLOOR` | 20 | a 100 % see-through Corral stays visible (3.3) |

### 2.2 Title strip height

`strip_px(dpi, ascent_em, descent_em, line_gap_em) = max(px(24), line_px + 2 * px(4))` where
`line_px = ceil((ascent + descent + gap) * px_of_title_font)` and the title font is 12 DIP at the Corral's governing monitor
DPI. Segoe UI / Segoe UI Variable at 12 DIP have a line of about 16 DIP, so the strip is 24 DIP exactly; a fallback font with a
taller line grows the strip instead of clipping. Text does not follow the Windows text-size slider (question 6): Corral
geometry must not move when an accessibility setting changes. The painter exports `strip_px` to the grid code (grid-spec 3.6);
the stored body rect never changes with it.

### 2.3 Shape, corners, edges

- The outer rect is one rounded shape, radius `px(8)`, clamped to `min(w, h) / 2` and to half the strip height when folded.
  `SquareEdges` = radius 0. Body and strip share the outer corners; the seam between them is always square.
- Title above: the strip owns the top two corners and the body the bottom two. Title below: the reverse. Title hidden: the
  body owns all four.
- All fills are clipped to this shape with anti-aliasing; the outline (section 5) follows the same path.
- Radius at 96/120/144/192/240 dpi: 8/10/12/16/20 px. The scrollbar inset `max(6, radius)` (scrollbar-spec 2) stays valid.

### 2.4 Title visibility and side

- `LabelMode` 2 Always: strip shown. 1 On hover: strip is reserved but not shown until the pointer is over the Corral;
  `reveal` animates 0 -> 1 in 120 ms (ease-out) and back in 200 ms (ease-in). 0 Never: strip height 0, nothing drawn.
  Reduced motion (`SPI_GETCLIENTAREAANIMATION` false): both 0 ms.
- While a reserved strip is not shown, the body fill continues under it (body fill always covers the whole outer rect), so the
  Corral looks like one plain rectangle; the strip's overlay, text and glyphs fade in with `reveal`.
- Side (above or below) comes from the engine (`TitleEdgeAutoAdjust` is its decision). Painting only mirrors the layout; a flip
  changes no colour.

### 2.5 Folded look (Fold)

The outer rect is the strip alone, all four corners rounded (or square), body fill and shade overlay as for a normal strip,
outline around it. A folded Corral always shows its strip, whatever `LabelMode` says, because it is otherwise invisible and
cannot be unfolded (question 5 reconciles this with grid-spec 3.6, which gives Never height 0). Hover-unfold drawing is the
normal Corral drawing clamped by the engine.

### 2.6 Layout inside the strip (pure `layout_title`)

Left to right: leading cluster, title run, trailing cluster; mirror nothing (English UI).
- Leading cluster from x = 4 DIP: menu button, then (Gates showing a subfolder, navigation allowed) the Up button. Trailing cluster ends at
  W - 4 DIP: close/collapse button. Each button is `BTN_W` wide and as tall as the strip. A button group exists only when the
  Advanced toggle "Titlebar buttons on hover" is on; the room is reserved even while the buttons are not hovered, so the title
  never shifts.
- Title run: optional 14 DIP folder glyph (Gates), 4 DIP gap, text. Span = from `lead_end + TEXT_GAP` (or `EDGE_PAD`) to
  `trail_start - TEXT_GAP` (or `W - EDGE_PAD`). The run is centred in that span (not forced to the strip centre, so a
  one-sided cluster shifts it by half a button) and left-aligned when wider.
- Narrow strips: if the span is under `TEXT_MIN`, drop clusters in this order until it fits: close, Up, menu. The right-click
  menu keeps every command reachable. At the 64 DIP minimum body this keeps the menu button and about 24 DIP of title.
- Vertical: text and glyphs are centred on the strip's line box; the baseline is snapped to a whole device pixel.

## 3. Body

### 3.1 Paint order (one function, same for desktop and sample)

1. Blurred backdrop clipped to the outer shape (only when blur is on and the Corral is not in high contrast).
2. Body fill over the whole outer rect (3.2, 3.3).
3. Title overlay on the strip: shade (4.2) and hover mix (4.3), scaled by `reveal`.
4. Tab strip fills (7) in the strip.
5. Outline (5). It comes after every title fill and after Camouflage recolouring, as chosen.
6. Glyphs and title text (4.4, 6) in the resolved text colour.

Icons and the scrollbar are drawn by their own areas between steps 2 and 6 as today; they never draw over the outline.

### 3.2 Body colour

Fill RGB = the resolved `Color` (colour-state-spec). If `DesktopBlendCorralBackgrounds` is on, the RGB first goes through
`recolor_color(Palette, strength, rgba)`, alpha preserved; `strength` already includes the real-colours hover fade. Shade
scaling, the outline mix and Auto text all use this recoloured RGB, so every surface agrees with what the user sees.

### 3.3 See-through mapping (stored `TransparencyFactor`, scale 0..2000, default 1380)

Direction (question 1): higher = more of the wallpaper shows (settings spec: "See-through", shown as % of 0..2000).

    v      = clamp(TransparencyFactor, 0, 2000)
    alpha  = 20 + ((2000 - v) * 235 + 1000) / 2000          (integer division)

| v | 0 | 500 | 1000 | 1380 (default) | 2000 |
|---|---|---|---|---|---|
| body alpha / 255 | 255 | 196 | 138 | 93 | 20 |

Linear in alpha because the slider is a percentage and users expect it to be proportional; the floor of 20 (about 8 %) keeps a
fully see-through Corral findable (with blur on it reads as frosted glass). The slider's stored value is never rewritten.
Hit testing does not depend on alpha.

### 3.4 Blur interplay

Blur only changes what is behind the fill: the engine hands over an already blurred wallpaper copy for the outer rect; the fill
keeps the same alpha with or without blur. The blurred copy is drawn at opacity `fade`, so a fading Corral shows less
frosted glass and more of the live desktop. Blur radius, steps and capture belong to the blur design.

### 3.5 Faded state

`fade` (0..1) is supplied by the engine (idle fading, Clear view, Camouflage-independent). Painting multiplies the alpha of
every element it draws (blur copy, body, overlays, tab fills, outline, glyphs, text) by `fade` using `floor(a * fade + 0.5)`;
colours are unchanged and Auto text is computed without `fade`, so nothing flips while it animates.
With `FadeInOpacityOnMouseover` on, the engine drives `fade` to 1 while the pointer is over the Corral (120 ms in, 200 ms out;
timing is P11). "Opaque" means the full configured look, not body alpha 255 (question 3).

## 4. Title

### 4.1 Base fill

The strip shows the body fill (3.1 step 2); there is no separate fill. Text legibility comes from the shade overlay and from
Auto text colour, not from a second layer.

### 4.2 Shades (`DarkenTitlebars`: 0 None, 1 Dark, 2 Darker), as chosen by Corral

`title_overlay(rgb, shade, body_alpha) -> Option<Rgba>`:
- None: no overlay.
- Dark: RGB scaled to 75 %, Darker: 50 %, per channel `(c * pct + 50) / 100`; overlay alpha = `max(body_alpha, 112)` for Dark,
  `max(body_alpha, 128)` for Darker; both before `fade`. The overlay is drawn over the strip rect only (the body fill remains
  underneath, so the strip is darker than the body at every see-through value).

### 4.3 Hover mix (kept)

While the pointer is over the strip, one more overlay is drawn: white at alpha 28 on dark colours, black at alpha 28 on light
ones (dark/light as in 5), eased in 100 ms (0 in reduced motion). 28/255 (11 %) is noticeable but does not change contrast
class. Multiplied by `fade` and `reveal`.

### 4.4 Text

- Font: Segoe UI Variable (Text optical size), fallback Segoe UI; size 12 DIP; weight = the stored per-Corral/global font
  weight, default Semibold (600); italics from the stored flag. No shadow or glow.
- DirectWrite layout per title: no wrap, single line, `DWRITE_TRIMMING_GRANULARITY_CHARACTER` with an ellipsis trimming sign
  ("..." U+2026) when wider than the span; if the span is under `TEXT_MIN` nothing is drawn. Grayscale anti-aliasing (the
  window is layered), text origin snapped to device pixels. A trimmed title shows its full text in the Corral's tooltip.
- Colour: `Custom(rgb)` as stored. `Auto` = `auto_text(backdrop)`: white `#FFFFFF` when its WCAG contrast with the backdrop is
  at least that of `#1B1D22`, else `#1B1D22` (camouflage-spec 3.5). The backdrop for title text is the strip's final colour
  (body fill plus shade overlay) composited over the sampled region's mean wallpaper colour, using the real alpha; the body label
  text uses the body fill alone. Hence a Darker strip can take white text while the labels below take dark text.
- The same helper `prefers_light_text(rgb)` (= Auto picks white) defines "dark" for the outline and hover mix.

## 5. Outline (as chosen by Corral)

`Outline` on: a stroke of 1 physical pixel (never scaled with DPI) around the whole outer shape (body and strip together, or
the strip alone when folded), drawn inside the shape edge (path inset 0.5 px) so that it never exceeds the outer rect.
Colour = final body RGB mixed 35 % toward white when `prefers_light_text(rgb)` (dark colour), else 35 % toward black; alpha
`floor(180 * fade + 0.5)`. Mix per channel: toward white `c + ((255 - c) * 35 + 50) / 100`, toward black
`c - (c * 35 + 50) / 100`. Drawn after title fills and Camouflage (3.1). No divider is drawn between strip and body.

## 6. Glyphs (P9)

Font: Segoe Fluent Icons, fallback Segoe MDL2 Assets (same codepoints for these six), chosen once per process with
`IDWriteFontCollection::FindFamilyName` and verified per codepoint with `IDWriteFontFace::GetGlyphIndices` (index 0 = missing).
If neither font has a glyph, a built-in vector shape is drawn instead (three lines, an X, chevron or arrow strokes, a folder
outline), still not a bitmap. Size 14 DIP, regular weight, one UTF-16 code unit per glyph, centred in the box, origin snapped.

| Role | Name | Codepoint | Notes |
|---|---|---|---|
| Menu button | GlobalNavButton | U+E700 | opens the Corral menu |
| Collapse (Fold on, Corral open) | ChevronUp | U+E70E | click folds; same glyph as the Fold page |
| Expand (folded) | ChevronDown | U+E70D | click unfolds |
| Close | ChromeClose | U+E8BB | used when the engine reports a close action (question 7) |
| Folder (Gates) | Folder | U+E8B7 | inline before the name, not a button |
| Up (Gates) | Up | U+E74A | navigates to the parent; hidden at the root (user decision 2026-10-30) |

States (button hit box 24 DIP wide x strip height; plate = box inset 2 DIP, radius 4), colours from the resolved text colour:
Rest: glyph 100 %, no plate. Hover: plate 14 % alpha. Pressed (mouse down on the button, capture): plate 24 %. Disabled: glyph
at 40 % (settings-kit rule), no plate, no hover. All times `fade`. Hover and pressed switch instantly (no animation). Buttons
are hit-tested before the strip's move zone; they are not keyboard stops (as the strip itself).

## 7. Tab strip (`TabsStyle`, shown in the strip of a tabbed Corral)

Common: tabs sit left to right in the title span (2.6), width = text + 2 x 10 DIP, clamped to 48..160 DIP, text 12 DIP, trimmed with
an ellipsis; active tab at 100 % text alpha, inactive 70 %, hover 100 % plus plate. Plates and fills use the text colour with the
alpha shown. Empty strip stays a move zone. When tabs exceed the span, widths shrink equally down to 48 DIP; beyond that the
strip scrolls by whole tabs so the active tab is fully visible, and the Corral menu lists all tabs (question 8). The tab strip
shows only when the title shows (`LabelMode`).

| Value | Name | Look |
|---|---|---|
| 0 | Menu only | No strip. The strip shows the active tab's name as an ordinary title; tabs are chosen from the menu |
| 1 | Flat | Text only; the active tab has a 2 DIP bar of the text colour along the edge facing the body; hover plate 10 % |
| 2 | Segmented | One track (strip inset 2 DIP, radius 6, 10 % fill) holding all tabs side by side; the active tab is a segment inset 2 DIP in the track (radius 4, 22 % fill) |
| 3 | Rounded | Separate pills 4 DIP apart, height strip - 8 DIP, fully rounded; active 22 % fill, inactive 8 % fill |

Rounded with room (user decisions 2026-10-29/30): one rounded title bar per tab, equal widths, 4 DIP apart, each with its
own menu, Up and close buttons. A bar that cannot fit its buttons plus 48 DIP of title drops close, then Up, then menu;
only when a bare 48 DIP title does not fit either does the set use the pills above.

`TabsColors` on: each tab uses its own Corral's effective colour (colour-state-spec 2.3). Flat bar, Segmented segment and Rounded
pill take that colour at alpha 255 (inactive 120) with a 1 px edge in its outline mix; the tab's text colour is `auto_text` of
that fill (or its Custom colour). The strip's own overlay and shade are unchanged. Drawn by the same functions in the Tabs
settings preview tiles.

## 8. High contrast and DPI

High contrast (`SPI_GETHIGHCONTRAST` with `HCF_HIGHCONTRASTON`; re-read on `WM_SETTINGCHANGE` and `WM_THEMECHANGED`):
- Body and strip fill `COLOR_WINDOW`, alpha 255; see-through, blur, Camouflage recolour, shade, hover mix and `fade` (except 0)
  are ignored. Text and glyphs `COLOR_WINDOWTEXT`; Auto/Custom text colours are ignored.
- Outline always on: `COLOR_WINDOWTEXT`, `max(2, px(1))` px. Corner shape still follows `SquareEdges`.
- Button hover and pressed: plate `COLOR_HIGHLIGHT`, glyph `COLOR_HIGHLIGHTTEXT`. Tabs: active tab filled `COLOR_HIGHLIGHT` with
  `COLOR_HIGHLIGHTTEXT`, inactive plain `COLOR_WINDOWTEXT`; Flat bar `COLOR_WINDOWTEXT`; no 70 % alpha.

DPI: governing-monitor DPI, 96 to 240 and anything between. All metrics come from the DIP table of 2.1 through `px`; a monitor or
DPI change re-derives `strip_px` and re-lays out from the same DIPs (nothing is scaled as a bitmap).

| dpi | 96 | 120 | 144 | 192 | 240 |
|---|---|---|---|---|---|
| strip px (default font) | 24 | 30 | 36 | 48 | 60 |
| corner radius | 8 | 10 | 12 | 16 | 20 |
| button width | 24 | 30 | 36 | 48 | 60 |
| title font px / glyph font px | 12 / 14 | 15 / 17.5 | 18 / 21 | 24 / 28 | 30 / 35 |
| tab minimum width | 48 | 60 | 72 | 96 | 120 |
| outline px | 1 | 1 | 1 | 1 | 1 |

## 9. Acceptance tests (pure; DirectWrite metrics are supplied as plain numbers)

1. `strip_px` with Segoe-like metrics (1.079 / 0.251 / 0) at 96, 120, 144, 192, 240 dpi gives the table row; a taller font
   (ascent+descent 1.6 em) gives `ceil(1.6 * 12 * dpi / 96) + 2 * px(4)`; never below `px(24)`.
2. Corner radius table; clamp: a 30 x 24 DIP outer rect keeps 8, a 20 x 10 DIP one gets 5; `SquareEdges` gives 0; folded clamps
   to half the strip.
3. See-through endpoints and samples: v 0 -> 255, 500 -> 196, 1000 -> 138, 1380 -> 93, 2000 -> 20, -50 -> 255, 2500 -> 20;
   monotone non-increasing for v = 0..2000.
4. Shade: colour (200,100,50) Dark -> (150,75,38); Darker -> (100,50,25). Overlay alpha: body 93 Dark -> 112, Darker -> 128;
   body 200 -> 200 both; body 255 -> 255; None -> no overlay. With fade 0.5, alpha 112 -> 56.
5. Outline: dark (30,40,60) -> (109,115,128) alpha 180; light (240,240,240) -> (156,156,156) alpha 180; fade 0.5 -> alpha 90;
   (0,0,0) mixes toward white, (255,255,255) toward black; Outline off draws nothing; stroke width is 1 px at every DPI.
6. `prefers_light_text` / `auto_text`: black -> white, white -> `#1B1D22`, (128,128,128) -> `#1B1D22` (about 4.27:1 against
   3.95:1); a Darker strip over a mid-grey body can flip to white; fade does not change the result.
7. `layout_title`: widths 64, 96, 160, 320 DIP at all five DPIs with each button combination: no overlap, text span >= `TEXT_MIN`
   or no text, drop order close -> Up -> menu, reserved room constant when only hover state changes, left-aligned when wider.
8. Ellipsis: a long title is trimmed with U+2026 inside the span; a title that fits is untouched; empty title draws nothing;
   a span under `TEXT_MIN` draws no text.
9. Glyph table: the six codepoints above; `pick_icon_font(available, has_glyph)` returns Fluent, else MDL2, else the vector fallback;
   a missing single codepoint in Fluent falls back for that glyph only.
10. Buttons: hit box 24 DIP x strip height at each DPI; hover/pressed/disabled alphas 14 %, 24 %, 40 %; Up at the root is
    hidden (user decision 2026-10-30).
11. Visibility: Always, On hover (reveal 0 draws no strip overlay/text, body fill continues), Never (strip 0), folded + Never
    shows the strip; reveal timing 120/200 ms, 0 with reduced motion.
12. Tabs: widths clamp 48..160 DIP; shrink then scroll keeps the active tab visible; each of the four styles produces the
    documented primitives; `TabsColors` uses each tab's own colour; tab strip hidden with the title.
13. Paint order: recorded draw calls arrive in the order of 3.1; the outline is after the title overlay and after
    Camouflage-recoloured fills; `fade` multiplies every alpha and leaves RGB unchanged.
14. High contrast: all alphas 255, system colours only, outline `max(2, px(1))`, shade/see-through/blur ignored.
15. Golden images (offline renderer, as for the settings kit): rounded and square, title above/below/hidden/folded, shades 0/1/2,
    outline on/off, four tab styles, at 96/120/144/192/240 dpi on dark, light and flat-grey backdrops; reviewed by eye.
    Live checklist: mixed-DPI drag, Camouflage backgrounds on, high contrast themes, wallpaper with fine detail behind a
    see-through Corral, reduced motion.

## 10. Open questions for the product owner

1. Is higher `TransparencyFactor` = more see-through, 0..2000? Recommendation: yes (matches the settings page). If reversed,
   only `see_through_alpha` changes.
2. Alpha floor 20 at 100 % see-through? Recommendation: keep; without it the body vanishes and only the outline shows.
3. What produces the faded state, and does "opaque while pointed at" mean full configured look (recommended) or body alpha 255?
4. The packet keeps a title "hover mix" without describing it. Recommendation: my 11 % white/black overlay (4.3); lead to
   adjust the alpha only after a live look.
5. Reconcile grid-spec 3.6 (Never = strip height 0) with a folded Corral: recommendation: folded always shows the standard strip.
6. Should title text follow the Windows text-size slider? Recommendation: no; Corral geometry must not move with it.
7. What does the close/collapse button do, and when is it an X versus a chevron? Recommendation: chevrons when Fold is enabled
   for the Corral, X only where the engine defines a close action. Painting accepts either.
8. Overflow of many tabs: shrink then scroll with the menu as the full list (recommended), or an overflow button (U+E712)?
9. Back/Forward glyphs (U+E72B, U+E72A) for Gates with navigation allowed are not in the packet. Recommendation: add them as two
   more 24 DIP buttons in the leading cluster, same states, dropped by the narrow-strip rule after Up.
10. Title default weight Semibold when no weight is stored, with the stored weight winning. Recommendation: confirm.
11. Outline colour is mixed from the final (recoloured) body colour. Recommendation: confirm; the alternative (unrecoloured
    `Color`) would make the outline disagree with the Camouflage look.
12. High contrast forces the Corral opaque and system-coloured. Recommendation: yes; users who run high contrast asked for it.

## Amendment (2026-10-21, user decision)

Question 9: no Back/Forward buttons on Folder Gates; Up is the only navigation button in the title strip.

## Amendment 2 (2026-10-21, user decision)
Section 3.3 and question 1 are reversed: the stored `TransparencyFactor` (0..2000) is how solid a Corral is, as Corral has always drawn it (higher = more opaque). `see_through_alpha` is two monotone straight segments: 0 gives the floor of 20, 1380 (default) gives 138 (about 54 %), 2000 and above give 255. The "See-through" slider shows 100 % minus the stored value / 20 (`see_through_percent(raw) = 100 - raw / 20`). A Corral whose strip is pinned above but whose body sits at the top of the work area draws the strip below (2.4).
