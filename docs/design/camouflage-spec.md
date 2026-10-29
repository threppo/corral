# Camouflage: wallpaper-aware icon colour effect (design spec)

Status: approved by the user 2026-10-20 (open questions resolved as the spec recommends)
Date: 2026-10-20
Author: Claude subagent (clean spec writer), inputs: spec-inputs-2026-10-20.md + settings redesign docs + public docs only

Terms: "Corral" includes Folder Gates unless a line says otherwise. "Straight" colour = not premultiplied.
All 8-bit colour values are sRGB-encoded. "Oklab" is Bjorn Ottosson's public perceptual colour space (2020), used
with its published sRGB-to-Oklab matrices.

## 1. Goal and non-goals

Goal: make icons sit quietly on the wallpaper. For each Corral, take two colours from the wallpaper behind it (a
shadow tone and a highlight tone) and redraw every icon so its dark pixels use the shadow tone, its light pixels the
highlight tone, and brightness in between runs along the gradient. A strength slider blends the original icon and
the recoloured one. Pointing at a Corral shows the real colours.

Non-goals:
- No change to icon shape, size, position, alpha or the files behind them. Camouflage only changes pixels drawn.
- No per-icon manual colours, no animation of the wallpaper, no semantic icon analysis (faces, logos).
- Labels (text) are never recoloured; they are kept readable (section 3.5).
- No new settings UI beyond the smallest changes in section 5.
- Not a security or privacy feature; the wallpaper is only read from the capture the engine already owns.

## 2. User-visible behaviour

- Off (master switch off, or strength 0 %): pixel-for-pixel the original icons. No cost beyond a flag check.
- On: every icon in a Corral or Folder Gate is drawn with the Corral's two colours. Different Corrals over different
  parts of the wallpaper get different colour pairs. A Corral over a flat dark region gets dark, low-contrast
  icons; over a bright warm region, warm light ones. Icons stay recognisable by their light/dark structure.
- Strength 0..100 %: 0 = original, 100 = pure two-colour icon, linear in between (section 3.3).
- Labels: unchanged colour and position; the automatic text colour is chosen against the final background so they
  stay readable (3.5).
- Corral backgrounds and titlebars: recoloured only when "Blend Corral backgrounds" is on (section 5), using the same
  colour pair and strength. Transparency, blur, outline and titlebar shade then apply on top, unchanged.
- Scrollbar: follows the backgrounds switch (it is a Corral surface); its alpha is preserved.
- Folder Gates: identical to Corrals; the sampled region is the Gate's rectangle. Icons in an opened Gate view too.
- Real colours: when "When you point at a Corral" is on, pointing anywhere inside a Corral (icons, titlebar, empty
  body) fades that whole Corral (icons, background, scrollbar) to original colours in 120 ms and back in 200 ms
  after the pointer leaves. When "While the desktop is active" is on, all Corrals show real colours the same way
  from the moment the desktop is clicked until a window is activated. Plain tint (3.4) is not affected by either.
- Drag and resize: the Corral keeps its colours while moving; the colour pair may update a few times a second
  (section 4). There is no flicker and no stall.
- Per-Corral override: a Corral can inherit, force Camouflage on, or force it off (section 5). Strength is global.
- Wallpaper change, monitor or DPI change: colours update once the new capture is ready; the old pair stays on
  screen until then (no flash of original colours).

## 3. The effect

### 3.1 Pipeline

For every Corral c the engine keeps one **Palette** and applies it to every pixel source of c:

    Palette = { shadow: sRGB8, highlight: sRGB8, lut: [sRGB8; 256], hash }

    effective source(c) -> palette(c)
    pixel (premultiplied BGRA) --duotone(palette)--> recoloured pixel --strength--> output

Pure functions (so they are testable without a window):
`derive_wallpaper_palette(samples) -> Palette`, `derive_tint_palette(colour) -> Palette`,
`apply_duotone(&Palette, strength_pct, &mut [u32])`, `apply_palette_blend(target, strength_pct, &mut [u32])`,
`recolor_color(&Palette, strength_pct, rgba) -> rgba`.

### 3.2 (a) Duotone: deriving the palette from the wallpaper

1. Sampling region: the Corral's whole outer rectangle (titlebar included) in physical pixels of the virtual screen,
   intersected with the captured wallpaper bounds. Use the blurred copy when wallpaper blur is on, otherwise the
   sharp copy. The capture contains no icons or Corrals, so nothing of Corral's own drawing leaks in.
2. Grid: split the clipped region into at most 32 x 32 cells (fewer when the region has fewer pixels, never
   more cells than pixels). Each cell contributes one colour: the mean of the (up to) 4 x 4 pixels
   at the cell centre, averaged in linear light. This is at most 1024 samples, independent of Corral size and DPI.
3. Colour space: decode sRGB to linear, convert to Oklab (L, a, b), f32 throughout.
4. Statistic (lightness percentiles): sort samples by L. With n samples, let
   - shadow base = mean Oklab of samples with rank in [floor(0.05 n), ceil(0.30 n));
   - highlight base = mean Oklab of samples with rank in [floor(0.70 n), ceil(0.95 n)).
   Bands (not single percentiles) average out noise; trimming 5 % at each end ignores specks and stars. If n < 16
   use the mean for both and let the guards below create separation.
5. Guards, applied in this order to (Ls, Lh) and the chroma of both colours:
   a. Chroma cap: C = sqrt(a^2 + b^2) is limited to 0.15 for both colours (keeps a vivid wallpaper from producing
      neon icons).
   b. Separation: if Lh - Ls < 0.35 (flat region), set both to M +/- 0.175, where M is their mean. This keeps the
      icon's internal contrast, i.e. readability.
   c. Range: shift both together so that Ls >= 0.10 and Lh <= 0.97 (the window 0.87 is wider than 0.35, so both
      always fit). Shift by the violated bound only.
   d. Gamut: for each colour, if its linear sRGB falls outside 0..1 at its (L, C, hue), reduce C by bisection
      (12 steps) at fixed L and hue. Hue is never changed.
6. Gradient table: for i in 0..255, t = i / 255, take the Oklab point (1-t) * shadow + t * highlight, convert to
   sRGB, clamp to 0..1, encode to 8 bits (round half up: floor(x * 255 + 0.5)). The table has 256 entries x 3
   bytes. Interpolating in Oklab avoids the muddy mid-tone of sRGB blending.

Guard results (use as test targets, tolerance 2 levels per channel): pure black wallpaper -> shadow about (3,3,3),
highlight about (85,85,85); pure white -> shadow about (134,134,134), highlight about (245,245,245); any flat grey
-> neutral pair with exactly the guaranteed separation; pure red (255,0,0) -> reddish pair, chroma capped,
lightness gap 0.35 around L = 0.63.

### 3.3 Duotone: mapping pixels and strength

Per pixel, premultiplied BGRA (B, G, R, A):
1. A = 0: output unchanged (stays 0,0,0,0).
2. Un-premultiply to straight colour: c = min(255, (p * 255 + A / 2) / A) per channel (skip when A = 255).
3. Brightness: Y' = (54 R + 183 G + 19 B + 128) >> 8, the integer Rec. 709 weighting (54 + 183 + 19 = 256) on
   the encoded values. Using encoded values, not linear light, matches perceived brightness well enough and avoids
   per-pixel linearisation.
4. Duotone colour D_straight = lut[Y'].
5. Re-premultiply: D = (D_straight * A + 127) / 255. Alpha is never changed. Because D_straight <= 255, D <= A,
   so the output is always a valid premultiplied pixel.
6. Strength s (integer percent 0..100). Because original O and D share the same alpha, interpolating the
   premultiplied channels equals interpolating straight colours and re-premultiplying, so no second division is
   needed: out = (O * (100 - s) + D * s + 50) / 100 per colour channel, alpha copied. s = 0 returns the input
   array untouched (do not run the loop). s = 100 returns D.
7. Rounding policy for the whole feature: integer paths use "+ half then divide"; float to 8-bit uses
   floor(x * 255 + 0.5) after clamping; no dithering.

Cache the pure-duotone result D (s = 100) per icon image; strength and hover fades are then one cheap lerp between
O and D (section 4).

### 3.4 Colour source: wallpaper or a chosen colour

Source per Corral (resolved in section 5.3): **wallpaper** (3.2) or **chosen colour** C, which is either the
Corral's own background colour ("Match Corral") or the custom tint colour.

Chosen colour -> palette (`derive_tint_palette`): convert C to Oklab (L, a, b). Shadow = (L - 0.25, a, b),
highlight = (L + 0.25, a, b); then apply guards b, c, d of 3.2 (no chroma cap: the user chose it) and build the
LUT as in step 6. For C with L in [0.35, 0.72] the middle of the gradient equals C, so a mid-grey icon pixel shows
the chosen colour (test: LUT[128] within 0.03 Oklab distance of C).

**Plain tint** (Personalization "Tint icons") is this effect with source = chosen colour:
- Camouflage on for the Corral: Camouflage decides. Source is the wallpaper, or the chosen colour if
  "Use the icon tint color" is on; strength is the Camouflage strength.
- Camouflage off for the Corral and Tint icons on: chosen colour, strength 100 %, always visible (not affected by
  the real-colour options).
- Both off: original icons.

### 3.5 Labels, backgrounds, scrollbar

- Labels: text glyphs keep the Corral's text colour. If the text colour is Auto, choose white or the dark text
  colour (#1B1D22) by higher WCAG contrast ratio against the effective backdrop: the Corral background after
  Camouflage, composited over the sampled region's mean colour by the Corral's see-through amount. Existing label
  shadows or outlines stay. Never recolour label text, because recoloured text loses contrast unpredictably.
  Icon bitmaps drawn inside the label row (none today) would be treated as icons.
- Backgrounds (when enabled): the background colour, and any gradient stops used for the titlebar, go through
  `recolor_color` (the pixel path of 3.3 applied to one colour, alpha preserved). The wallpaper showing through a
  see-through Corral is the real wallpaper and is never recoloured.
- Scrollbar track and thumb: `recolor_color`, same switch as backgrounds.

### 3.6 (b) Palette blend (prototype)

Simpler alternative with the same inputs and the same cache:
- Target colour T: the mean of all samples in Oklab, converted to sRGB (wallpaper source), or the chosen colour C.
- For strength s, the mix amount is m = round(65 * s / 100) percent. The 65 % ceiling keeps icon shapes visible;
  100 % toward one colour would give flat silhouettes.
- Per pixel on premultiplied data: out = (P * (100 - m) + prem(T, A) * m + 50) / 100, where
  prem(T, A) = (T * A + 127) / 255. Alpha unchanged, A = 0 untouched, s = 0 untouched.
- Backgrounds and scrollbar use the same formula on one colour.

### 3.7 QA switch and what the user compares

A hidden setting `CamouflageAlgorithm` (0 = duotone, default; 1 = palette blend) is honoured by the engine and the
settings preview. It has no UI and is deleted once the decision is made. An offline contact-sheet generator (an
ignored test that writes a PNG) renders a grid: rows = 6 wallpapers (dark, bright, flat grey, saturated, busy photo,
pastel) x both algorithms; columns = strength 25, 50, 75, 100 %; same 12 real icons in each cell.

The user judges, at 50 % strength on their own wallpaper: (1) is every icon still recognisable at arm's length?
(2) does the Corral feel like part of the wallpaper? (3) can I tell a folder from a document from an app by colour,
or only by shape? Duotone is calmer and more uniform, but removes colour identity; palette blend keeps identity but
looks muddier on busy wallpapers. Recommendation: duotone, unless (1) or (3) fails on the user's wallpapers.

## 4. When colours are recomputed, caching, budget

Triggers (each recomputes the palette of the affected Corrals only):
- wallpaper capture generation changes (all Corrals), blur setting changes (all), source or chosen colour changes
  (affected Corrals);
- Corral rectangle changes after a move or resize ends (the drop, release or settle event), a monitor layout or
  DPI change (after the engine's existing settle period).
- Not triggered by scrolling inside a Corral, icon changes, hover, or strength changes (strength never needs a new
  palette).

Cache: one Palette per Corral keyed by (Corral id, rect in virtual-screen px, wallpaper generation, blur radius,
source kind, chosen colour). A palette hash identifies it. A processed-icon cache is keyed by (icon image id, DPI
variant, palette hash) and stores D (s = 100). Evict when the palette hash changes or the icon is dropped.

Budget on a mid-range laptop CPU:
- Palette derivation <= 1 ms per Corral (1024 samples); recompute for 20 Corrals on a wallpaper change runs on the
  worker that owns the capture and publishes the palettes atomically; it must never block a paint.
- Recolouring a 48 x 48 icon <= 15 microseconds; 200 icons <= 3 ms total after a palette change, done lazily as
  icons are first drawn.
- Per-frame cost in steady state: zero extra work (cached D drawn, or one lerp of cached O and D when strength is
  between 0 and 100 and while a hover fade runs, <= 5 microseconds per icon).

Drag and resize may approximate:
- Move: recompute at most every 100 ms using a 16 x 16 grid; adopt the new palette only if it differs by more than
  0.008 Oklab distance (hysteresis); exact 32 x 32 recompute on drop.
- Resize: keep the old palette until release, then recompute exactly.
- Dragged-Corral snapshots reuse cached recoloured icons; no sampling or recolouring per frame.

## 5. Settings and storage

### 5.1 Key map (names unchanged unless stated)

| Key | New meaning |
|---|---|
| `DesktopBlendIcons` | Master switch. Fresh default: off. |
| `DesktopBlendIconOpacity` | Blend strength. Shown as 0..100 %; 0 = original colours. Fresh default: 60 %. |
| `DesktopBlendCorralBackgrounds` | Also recolour Corral background, titlebar and scrollbar (3.5). Fresh default: off. |
| `DesktopBlendColorOnMouseover` | Fade the pointed-at Corral to real colours. Fresh default: on. |
| `DesktopBlendColorOnActive` | Fade all Corrals to real colours while the desktop is active. Fresh default: off. |
| `DesktopBlendUsesTintIconsColor` | Colour source: false = wallpaper, true = chosen colour (3.4). Fresh default: false. |
| `BackgroundBlurEnabled`, `BackgroundBlurRadius` | Unchanged; shared with Personalization. Camouflage samples the blurred copy when blur is on. |
| `DesktopTintIcons` | Plain tint on/off (3.4). Unchanged. |
| `DesktopTintIconsColor` | The custom chosen colour. Unchanged. |
| `DesktopTintIconsColorUse` | Legacy; replaced by `DesktopTintIconsColorSource` (5.2). |
| `Color`, `TextColor`, `TransparencyFactor` | Unchanged. `Color` is the chosen colour for "Match Corral"; `TextColor` Auto uses 3.5. |

Per-Corral (absent = inherit global): `blend_icons` (1 = on, 0 = off), `blend_background` (same), `tint_icons`,
`tint_icons_color` (custom colour for this Corral), and `tint_icons_color_use` replaced by `tint_icons_color_source`.
`BlendOptionsVisible` (global or any per-Corral Camouflage) stays as the page gate. The stored algorithm choice
`CamouflageAlgorithm` (3.7) is temporary.

### 5.2 Migration: explicit colour source

New string key `DesktopTintIconsColorSource` with values `"corral"` (use the Corral's `Color`) and `"custom"` (use
`DesktopTintIconsColor`); per-Corral `tint_icons_color_source` likewise. On load, if the new key is absent, read the
legacy number: 0 -> `"corral"`, any other value -> `"custom"` (the redesign defines mode 0 = Match Corral, mode 1 =
Custom). If both exist, the new key wins. Files are not rewritten at load; the new key is written on the next normal
save. For one release the legacy key is also written (0/1 mapped back) so a downgrade keeps working; after that it
is read-only and then dropped. Per-Corral values migrate independently. Absent per-Corral source inherits the
global one. Fresh install: source `"corral"`.

Amendment (2026-10-20, from Corral's stored data): the legacy `...ColorUse` value is a packed colour, not a 0/1
mode. Migration therefore reads "Match Corral" as: the stored tint colour equals the Corral's `Color` within one step
per channel -> `"corral"`; otherwise `"custom"` with the stored tint colour. The legacy key is not written back.

### 5.3 Resolution for one Corral

    camouflage_on  = blend_icons ?? DesktopBlendIcons
    plain_tint     = tint_icons ?? DesktopTintIcons
    use_chosen     = DesktopBlendUsesTintIconsColor            (global only)
    chosen colour  = (tint_icons_color_source ?? DesktopTintIconsColorSource) == "corral"
                       ? Corral.Color : (tint_icons_color ?? DesktopTintIconsColor)
    if camouflage_on: source = use_chosen ? chosen : wallpaper; strength = BlendIconOpacity
    else if plain_tint: source = chosen; strength = 100; not hover-restorable
    else: none

### 5.4 Settings page changes (smallest set, relative to settings-redesign-spec.md section 7)

Camouflage page:
- Add one toggle row to the "Blending" card: "Blend Corral backgrounds" / "Also tint the Corral background,
  titlebar and scrollbar the same way." -> `DesktopBlendCorralBackgrounds`.
- "Wallpaper blur" row is disabled (not hidden) while "Use the icon tint color" is on, because the wallpaper is not
  sampled then.
- Replace the three "How it works" bullets with: "Camouflage picks a dark and a light color from the wallpaper
  behind each Corral and redraws its icons with them." / "Choose a different Corral in Personalization to give it
  its own Camouflage setting." / "Point at a Corral to see its real colors when that option is on."
- Keep: master switch, preview, strength, blur steps, tint-colour toggle, both real-colour toggles. Removed:
  nothing else; the Options popup and per-Corral Camouflage controls in the retired colour editor are already gone.

Personalization page:
- When one Corral is selected in the scope picker, the "Icons" card shows an extra Segmented row "Camouflage" /
  "Like all Corrals | On | Off" -> `blend_icons` absent / 1 / 0. Hidden when All Corrals is selected. The Reset
  dialogs already mention "Camouflage choice"; Reset removes `blend_icons`, `blend_background`.
- "Tint color" Custom swatch popover edits the colour source as specified; selecting the "Match Corral" mode writes
  `"corral"`, "Custom" writes `"custom"`.

Removed: the legacy numeric colour-use code as a UI concept; the old "toward wallpaper by strength" algorithm
(a user's existing strength value is kept; the look changes and the change is mentioned in release notes).

## 6. Edge cases and acceptance tests

All tests are pure functions on small synthetic images (for example 8 x 8 icons, 64 x 64 wallpapers), no window.

Identity and bounds
- Strength 0: output slice is bit-identical to input (also for hover fade factor 0).
- Strength 100: every pixel equals premultiplied `lut[Y']`; alpha channel identical to input for all strengths.
- Alpha 0 stays (0,0,0,0) for every strength and both algorithms; alpha 255 round-trips exactly through un/premultiply
  at s = 100 when lut is identity-grey; alpha 1 and 128 produce channels <= alpha.
- Strength 50: each channel lies between the original and the s = 100 value, rounded as in 3.3.
- Black icon pixel maps to `shadow`, white to `highlight` (within 1 level); a horizontal grey ramp maps to a
  monotonic non-decreasing lightness ramp.

Wallpapers
- Flat black, flat white, flat mid-grey: separation >= 0.35 in Oklab L, within range [0.10, 0.97]; expected values
  in 3.2.
- Fully saturated red, green, blue: chroma <= 0.15 before gamut mapping, both colours inside sRGB, same hue as the
  wallpaper (hue error < 3 degrees).
- Two-tone wallpaper (left black, right white, region covers both): shadow near black band, highlight near white band.
- Region of 1 x 1 pixel and of fewer than 16 samples: no panic, mean fallback, guards applied.
- Tint source: LUT[128] within 0.03 Oklab of the chosen colour when L is in [0.35, 0.72]; pure white and pure black
  choices obey the range guard.
- Palette blend: s = 0 identity; s = 100 gives m = 65; alpha preserved; target equals sample mean on a flat image.

Geometry
- Region partly outside the virtual screen: only the clipped part is sampled; a region fully outside samples the
  monitor nearest the region centre; result identical to the in-bounds case when the visible part is the same.
- Negative virtual-screen origin (for example origin (-1920, -200)): region coordinates are converted by subtracting
  the origin; a test places a distinctive colour in the left monitor only and a Corral at x = -1500.
- Region spanning two monitors with different DPI: rectangle is in physical pixels, grid covers both; the result is
  equal to the single-pixel-space computation of the same pixels.
- DPI change with same on-screen area: the palette hash changes only if the physical rect changes.

Behaviour
- Hover restore: effective strength = s * (1 - fade); fade 0 -> s, fade 1 -> 0 (bit-identical original); fade
  curves are monotone, and a re-entry mid-fade continues from the current value (no jump).
- Active-desktop restore sets all fades to 1 and clears on window activation.
- Per-Corral override inheritance: absent -> global; `blend_icons` 0 beats global on; `blend_icons` 1 beats global
  off; `tint_icons_color_source` absent inherits global source; plain tint when Camouflage is off for that Corral.
- Migration: legacy 0 -> `"corral"`, 1 -> `"custom"`, unknown (for example 7) -> `"custom"`; new key present
  beats legacy; load then save does not change unrelated fields; fresh install values match 5.1.
- Auto label colour: dark backdrop -> white text; light backdrop -> dark text; contrast >= 4.5:1 for the
  black/white/mid-grey wallpapers where achievable.
- Cache: same inputs give the same hash; any listed trigger changes it; scroll does not.
- Performance smoke (not a pass/fail gate on CI): a 48 x 48 recolour and a 1024-sample derivation are within the
  budgets of section 4 on the developer machine; results logged.

Live checklist (not unit-testable): preview and real Corrals agree; wallpaper change shows no flash; drag does not
stall at 144 Hz on a 4K monitor; hover fade is smooth; dark and light mode; 100/150/250 % scaling.

## 7. Open questions (product owner)

1. Fresh install default: Camouflage off, strength 60 % when turned on. Recommendation: off, because it changes the
   user's icons out of the box; the Welcome tile and the preview introduce it.
2. Plain tint without Camouflage: now a full (100 %) two-colour treatment of the chosen colour. Recommendation:
   accept; if it looks too strong in review, lower the fixed strength rather than add a slider.
3. Corral backgrounds: add the "Blend Corral backgrounds" toggle (default off) so the stored key has a UI.
   Recommendation: add it; alternative is dropping the key and never recolouring backgrounds.
4. Per-Corral strength: not offered (only on/off and colour source per Corral). Recommendation: keep it global.
5. Duotone versus palette blend: decide after the contact sheet (3.7). Recommendation: duotone.
6. Labels are never recoloured, only the automatic text colour adapts. Recommendation: confirm.

Questions for the lead (engineering, not product): (a) Is `DesktopBlendIconOpacity` stored as 0..100 or 0..255? This
spec assumes it maps to an integer percent; use round(v * 100 / 255) if it is a byte scale. (b) Are the legacy
`...ColorUse` codes only 0 and 1? (c) The old plain-tint algorithm is not described in the inputs; its on-screen
look may differ from 3.4 and needs a before/after review.
