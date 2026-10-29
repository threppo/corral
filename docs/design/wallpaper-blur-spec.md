# Wallpaper blur (design spec, area D5)

Status: approved by the user 2026-10-22 (open questions resolved as the spec recommends, except as amended below)
Date: 2026-10-22
Author: Claude subagent (clean spec writer), inputs: spec-inputs-wave-d-2026-10-22.md + approved Corral design docs + public docs only

Terms: "Corral" includes Folder Gates and tab groups. "Canvas" = one monitor's rectangle in physical pixels (after rotation). "Working size" =
canvas divided by an integer `k`. "B" = blurred copy, "S" = unblurred working copy. All numbers are proposals with a reason; none is measured.

## 1. Goal and non-goals

Goal: give every monitor a blurred copy of the wallpaper with no icons, windows or Corrals in it, aligned with the real wallpaper, cheap to keep and
testable without a window. Corral bodies draw it as frosted glass (corral-painting-spec 3.4), Camouflage samples it (camouflage-spec 3.2), a generated
noise tile hides banding. Non-goals: blurring what is on screen; changing the wallpaper or other programs' settings; HDR or colour management (sources
are sRGB); animating the blur; new settings (only the Blur steps, `BackgroundBlurEnabled`, `BackgroundBlurRadius`).

## 2. Settings mapping

The Blur slider (Personalization and Camouflage share it) offers Off 0, Subtle 15, Soft 30, Medium 60, Strong 120, stored in
`BackgroundBlurRadius`, with `BackgroundBlurEnabled` true iff non-zero (settings-redesign-spec 7). Reading: effective radius `R` = 0 if
`BackgroundBlurEnabled` is present and false, else `clamp(stored, 0, 255)`. A value between steps is honoured; the UI shows the nearest step and
does not rewrite it until the slider moves.

    sigma_dip = R / 2          sigma_px = sigma_dip x dpi / 96      (dpi of the monitor being blurred)

The four steps are sigma 7.5, 15, 30, 60 DIP. Reason: Medium lands near the softness of Windows' own acrylic (about 30 DIP) and each step
doubles, so neighbours are clearly different. Sigma is in DIPs, so a 250 % monitor looks as soft as a 100 % one.

## 3. Capturing the wallpaper per monitor

Decision: build the picture from the wallpaper source Windows reports, never from a screen capture. Windows.Graphics.Capture and DXGI desktop
duplication show the composited desktop (icons, windows, files, other programs), can return black for protected content, change every frame and may
add a capture border. Excluding Corral's own windows (`SetWindowDisplayAffinity` with `WDA_EXCLUDEFROMCAPTURE`) removes none of the rest.

### 3.1 Sources, in order, per monitor
1. `IDesktopWallpaper` (public COM, STA thread): `GetMonitorDevicePathAt` joined to the monitor, `GetMonitorRECT`, `GetWallpaper(monitorId)`
   (that monitor's current picture; a slideshow reports its current slide), `GetPosition()`, `GetBackgroundColor()`.
2. No usable path: `SystemParametersInfoW(SPI_GETDESKWALLPAPER)` (a `MAX_PATH` UTF-16 buffer; one picture for all monitors).
3. Still nothing, the file cannot be decoded, or it exceeds 128 megapixels: a solid image of the background colour (`COLOR_DESKTOP` if the COM call
   fails), logged once per source. A solid source blurs to itself.
Decoding is by WIC (JPEG, PNG, BMP, TIFF, installed codecs; GIF uses its first frame), scaled straight to the working size with the Fant filter, so a
large picture never exists at full size in memory.

### 3.2 Placement (pure `placement(mode, image_px, canvas_px, virtual_rect_px) -> {src_rect, dest_rect}`)

`GetPosition()` gives one mode for the whole desktop. Fill: uniform scale to cover, centred, overflow cropped. Fit: uniform scale to contain,
centred, bars in the background colour. Stretch: non-uniform to the canvas. Center: native size centred. Tile: native size repeated from the canvas
top-left. Span: one Fill-style placement over the bounding rectangle of all monitors (physical pixels of the virtual screen); each monitor takes its
crop, so neighbouring crops meet exactly, negative origins included. Small scaler differences vanish in the blur; a wrong mode does not (section 9).

### 3.3 Several monitors, slideshows, live wallpapers

Each monitor has its own source, canvas, `k`, S and B; nothing is shared across different pictures, DPIs or rotations.
- Slideshow: Windows broadcasts `WM_SETTINGCHANGE` with `SPI_SETDESKWALLPAPER` on a change; handle it after a 250 ms debounce (the file may still be
  written). While `IDesktopWallpaper::GetStatus` reports a slideshow, also compare (path, mtime, size) of each source every 10 s; no timer otherwise.
- Live wallpapers (Wallpaper Engine style hosts render into a window under the icons): Corral does not capture them. It blurs the still picture
  Windows reports. A host is recognised by a visible window under the shell's wallpaper window owned by another process; that only adds a Caption
  under the Blur rows: "With a live wallpaper, blur uses the still picture Windows reports." Such tools often can write a chosen still for Windows;
  whether that fixes the match is unverified (question 2).
- Laziness: build only while needed (effective radius > 0, or Camouflage on with the wallpaper as its source, or the Camouflage preview or the
  Personalization sample is visible); otherwise nothing is decoded or kept.

## 4. Blur method

Pure CPU on the working size, in linear light; no GPU effect. Reason: at working size the cost is small, the result is the same on every machine
(Remote Desktop, no GPU, device loss), and one function serves the engine, the live sample and the tests.
1. `k = clamp(2^floor(log2(sigma_px / 3)), 1, 8)` (1 when `sigma_px < 6`); `sigma_w = sigma_px / k`; working size `ceil(canvas / k)` per axis. The kernel
   stays at least 3 px wide, so cost does not grow with the radius, and the detail dropped is detail the blur would remove. At 96 dpi: Subtle k 2, Soft 4,
   Medium 8, Strong 8 (sigma_w 7.5); Subtle at 192 dpi k 4.
2. Convert the 8-bit sRGB working image to linear f32.
3. Three box blurs per axis (the standard three-box Gaussian approximation): ideal width `w = sqrt(4 sigma_w^2 + 1)`, `wl` the largest odd integer
   `<= w`, `wu = wl + 2`, `m = round((12 sigma_w^2 - 3 wl^2 - 12 wl - 9) / (-4 wl - 4))`; the first `m` passes use `wl`, the rest `wu`. Edges repeat the
   border pixel (clamp), so no darkening at the border. Running sums make each pass O(pixels).
4. Encode to sRGB 8-bit, `floor(x x 255 + 0.5)` clamped. B is opaque BGRX at working size.
Linear light keeps bright areas bright (a black/white checker blurs to sRGB 188, not 128), as frosted glass does. Drawing scales B to the canvas with
linear interpolation (`D2D1_INTERPOLATION_MODE_LINEAR`), and the Corral's screen position selects the source region, so the frosted area lines up with
the real wallpaper around it.

## 5. Noise texture

A fixed generated tile; no file, no third-party asset.
- 128 x 128 texels, drawn 1:1 in device pixels (never scaled by DPI), tiled from the monitor's top-left (not the Corral's), so it does not crawl when a
  Corral moves. White noise tiles without seams.
- Texel `i`: `u1 = fmix32(0x5EED0001 + 2i) / 2^32`, `u2 = fmix32(0x5EED0001 + 2i + 1) / 2^32` (`fmix32` = murmur3 finaliser: `h ^= h>>16; h *= 0x85EBCA6B;
  h ^= h>>13; h *= 0xC2B2AE35; h ^= h>>16`, wrapping), `n = u1 + u2 - 1` (triangular, mean 0). `n >= 0`: white, alpha `round(6n)`; `n < 0`: black, alpha
  `round(-6n)`; premultiplied BGRA.
- Effect: over mid-grey at most about +-3 levels (rms about 1.2), none at pure black or white; it breaks the 1-level steps of a smooth gradient without
  visible grain (strength to confirm by eye, question 4).
- Drawn only when the effective radius is > 0, right after B and before the body fill, clipped to the Corral shape, alpha times `fade` like B. Not drawn
  in high contrast; never part of what Camouflage samples; the Personalization sample draws it too.

## 6. Caching and invalidation

- S key: (source identity {path, mtime, size} or solid colour, background colour, mode, canvas rect, virtual rect for Span, `k`). B key: (S key, `sigma_w`
  rounded to 1/16). A blur-step change with the same `k` reuses S and reruns only steps 2-4; a `k` change re-decodes.
- One global `wallpaper_generation` is incremented when a rebuilt S or B differs in content (64-bit pixel hash) from the one it replaces; an identical
  re-read changes nothing. Camouflage keys its palettes on it with `sigma_dip` (camouflage-spec 4).
- Triggers: `SPI_SETDESKWALLPAPER` and theme changes (250 ms debounce); slideshow polling (3.3); display, DPI and monitor changes after the engine's
  settle period; resume and unlock; a change of blur step or `BackgroundBlurEnabled`; a need turning on (3.3). Moves, scrolling, hover, fade never recompute.
- One background worker; the newest request wins and older queued ones are dropped. The old B stays in use until the new one is ready and then swaps
  atomically (no flash of sharp wallpaper). A failure keeps the old B and logs once.
- Swap look: a wallpaper change cross-fades old to new in 400 ms (OUT, motion-sorting-spec 1.1; about the length of Windows' own change); blur-step
  changes, first load, DPI and monitor changes swap at once (the slider stays live); reduced motion swaps at once.

## 7. Performance budget (mid-range laptop CPU, per 4K monitor, release)

- Decode and scale of a 12-megapixel JPEG to working size <= 120 ms on the worker; compose <= 5 ms; linearise, blur and encode at Medium <= 15 ms (the
  slider can drive about 30 coalesced rebuilds a second).
- Memory: S at k 4 about 2 MB; B about 8 MB at Subtle (k 2), <= 2 MB at Soft and above; one S and one B per monitor plus one transient copy; 3 x 4K at
  Subtle <= 30 MB. UI thread: no decoding or blur; steady state is one cached texture draw plus one noise draw per Corral; no timer without a slideshow.

## 8. Interactions

- Corral body (corral-painting-spec 3.1): backdrop = B for the outer rect at opacity `fade`, then noise at the same `fade`, then the body fill. A Corral
  straddling two monitors during a drag draws each part from its own monitor's B, split at the seam. Body alpha and Camouflage backgrounds are unchanged.
- Camouflage (camouflage-spec 3.2): `sample(rect_px)` reads B when the effective radius is > 0, otherwise S (built at k 4 only when Camouflage needs it).
  A cell's 4 x 4 mean comes from the working pixels around the mapped centre (`x / k`, bilinear), averaged in linear light; monitor origins are
  subtracted; noise is never included. During a rebuild the previous palette stays on screen.
- Personalization sample: the same `blur(image, sigma_px)` on its wallpaper crop, sigma scaled by the preview scale. Clear view and Range switches
  never touch B. Glance draws the same B.

## 9. Acceptance tests (pure; small synthetic images, no window)

1. Mapping: stored 0/15/30/60/120 give sigma_dip 0/7.5/15/30/60; enabled false gives R 0; 300 clamps to 255; between steps is kept; `sigma_px` and `k`
   for every step at 96/120/144/192/240 dpi (Subtle 96: 2, Soft 4, Medium 8, Strong 8; Subtle 192: 4); `k` within 1..8; `sigma_w >= 3` when `k > 1`.
2. Blur: box-width kernel variance within 3 % of sigma_w^2 for sigma_w 1..12 (step 0.25); a constant image is unchanged (edges included); the mean is preserved within 1e-4; an impulse gives a symmetric kernel summing to 1; a step edge
   is monotone, no over- or undershoot; mirror symmetry; 1 x 1 and 3 x 3 inputs do not panic; a 1-pixel black/white checker at strong
   blur gives 188 +- 1 (linear light); a 256-level ramp stays monotone.
3. Placement: Fill, Fit, Stretch, Center, Tile for 16:9 and 4:3 images on 16:10, 21:9 and portrait canvases; Span over two differently sized monitors at
   origins (0,0), (-1920,-200), (0,-1080): crops abut exactly and their union equals the cover placement of the virtual rect.
4. Sources: empty per-monitor path falls back to the SPI path, then to solid; an unreadable file and a 129-megapixel header give solid; solid blurs to
   itself within 1 level.
5. Cache: identical inputs give identical keys; each trigger of section 6 changes the right key (S or B); a step change with the same `k` runs no decode
   (counter); `wallpaper_generation` moves only on content change.
6. Noise: byte-identical across runs; mean of `n` within 0.02, variance within 5 % of 1/6; max alpha 6; no two rows identical; wrap neighbours uncorrelated
   (|r| < 0.05); noise at screen pixel (x, y) equals `tile[(x - mon_x) mod 128, (y - mon_y) mod 128]` whatever the Corral position; a 1000 x 1 ramp 100..120
   after noise has no run of identical values longer than 12 px (the bands were 50).
7. Cross-fade: 400 ms OUT, weights sum to 1, zero under reduced motion, a retarget continues from the shown mix. Laziness truth table (3.3).
8. Sampling: a flat B returns the flat colour; a half-black/half-white B at k 4 gives samples within 1 level of the full-size computation (negative
    origin, rect partly outside, mixed-DPI pair). Budget smoke (logged, not a gate): 960 x 540 blur and tile build within section 7.
Live checklist: slideshow change, span over two monitors, wallpaper-change flash, a live-wallpaper host (caption, no capture), 100/150/250 % and a
mixed-DPI pair, a Corral dragged over a seam, slider smoothness, banding on a dark gradient with and without noise.

## 10. Open questions
1. Is sigma = R / 2 DIP right per step (tune by eye)? Existing files may hold radii from an older scale; any 0..255 value is honoured but may look different.
2. Which still does a live-wallpaper host expose to Windows, and is a Caption enough? Recommendation: Caption only; a snapshot mode is later work.
3. Does `GetWallpaper` return the current slide on this Windows 11 build, or only a transcoded cache (SPI path is the fallback)? There is no
   per-monitor `GetPosition`: is one fit mode always right on the user's setups?
4. Noise strength 6/255 and the 400 ms cross-fade: confirm by eye.
5. Is the 128-megapixel decode limit acceptable? Spotlight and theme wallpapers are assumed to behave like any picture path; verify live.
