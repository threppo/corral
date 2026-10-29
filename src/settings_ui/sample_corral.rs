//! A disposable, synchronous look preview. Reuses the engine's group_paint/tint pipeline;
//! it never hosts desktop items, mutates a layout, or polls the wallpaper while idle.
use super::{Theme, scf};
use crate::render::{Align, Ctx, RectF, Rgba, Surface, TextStyle};
use crate::settings::Settings;
use crate::{camouflage, color_state::Effective, group_paint, tint};
use std::{cell::RefCell, rc::Rc};
use windows::core::Result;

#[derive(Clone, Debug, PartialEq, Eq)]
struct StyleKey {
    /// The shown scope's resolved colour state (colour-state-spec 2.2).
    look: Effective,
    label: u32,
    shade: u32,
    square: bool,
    outline: bool,
    opacity: u8,
    uses_tint: bool,
    /// The room reserved for the hover buttons moves the title.
    controls: bool,
    /// A high-contrast theme switch changes every colour the painter uses.
    high_contrast: Option<group_paint::HighContrast>,
}
struct Prepared {
    width: u32,
    height: u32,
    dpi: u32,
    blur: u32,
    sharp: tint::Wallpaper,
    sampled_group: tint::Wallpaper,
    body: Surface,
    title: Surface,
    icons: Surface,
    labels: Surface,
    mask: Surface,
    output: Surface,
    outer_mask: Vec<u8>,
    key: Option<StyleKey>,
}
struct Cache {
    source: Rc<tint::Wallpaper>,
    blurred: Option<(u32, Rc<tint::Wallpaper>)>,
    prepared: Option<Prepared>,
}
thread_local! { static CACHE:RefCell<Option<Cache>>=const {RefCell::new(None)}; }

/// Called on display/DPI changes and settings-window destruction, never on a slider tick.
pub(super) fn reset() {
    tint::set_preview_visible(false);
    PLACEHOLDER.set(0);
    CACHE.with(|c| *c.borrow_mut() = None);
}
fn rgba(color: u32, alpha: u8) -> Rgba {
    Rgba::new(color as u8, (color >> 8) as u8, (color >> 16) as u8, alpha)
}
/// camouflage-spec 3.1: convert COLORREF only at the drawing boundary.
pub(super) fn color_rgb(c: u32) -> camouflage::Rgb { camouflage::runtime::colorref_rgb(c) }
pub(super) fn color_argb(c: u32) -> u32 { camouflage::runtime::colorref_argb(c) }
pub(super) fn mapped_color(p: &camouflage::Palette, strength: u8, c: u32) -> u32 {
    let argb = camouflage::recolor_color(p, strength, color_argb(c));
    camouflage::runtime::rgb_colorref([(argb >> 16) as u8, (argb >> 8) as u8, argb as u8])
}
pub(super) fn wallpaper_palette(w: &tint::Wallpaper, rect: [i32; 4]) -> camouflage::Palette {
    camouflage::derive_wallpaper_palette(&camouflage::sample_rect(w, rect, 32))
}
pub(super) fn apply_pixels(p: &camouflage::Palette, strength: u8, bytes: &mut [u8]) {
    camouflage::apply_duotone_bytes(p, strength, bytes);
}
/// camouflage-spec 3.5: Auto picks white or dark text against the backdrop the Corral really has:
/// its (possibly mapped) colour over the sampled mean by the see-through amount.
pub(super) fn auto_text_color(settings: &Settings, wallpaper: &tint::Wallpaper, rect: [i32; 4], palette: &camouflage::Palette, strength: u8, background: u32, alpha: u8) -> u32 {
    auto_text_color_for(settings.desktop_blend_icons && settings.desktop_blend_corral_backgrounds, wallpaper, rect, palette, strength, background, alpha)
}
/// As [`auto_text_color`], with the Corral's own `blend_body` choice (Camouflage on icons and backgrounds).
pub(super) fn auto_text_color_for(blend_body: bool, wallpaper: &tint::Wallpaper, rect: [i32; 4], palette: &camouflage::Palette, strength: u8, background: u32, alpha: u8) -> u32 {
    let mean = camouflage::mean_color(&camouflage::sample_rect(wallpaper, rect, 32));
    let colour = if blend_body {
        mapped_color(palette, strength, background)
    } else { background };
    let text = camouflage::auto_text_color(camouflage::composite_backdrop(color_rgb(colour), mean, 255 - alpha));
    camouflage::runtime::rgb_colorref([(text >> 16) as u8, (text >> 8) as u8, text as u8])
}
thread_local! {
    /// The cached source is the desktop colour because the picture was not ready: ask again on the next paint.
    static PLACEHOLDER: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}
/// Paints that may retry a missing picture before the desktop colour is accepted.
const PICTURE_RETRIES: u8 = 30;

/// The source picture and whether it is the real one (not the desktop-colour placeholder).
fn source() -> (Rc<tint::Wallpaper>, bool) {
    // Offscreen renders deliberately avoid the user's wallpaper and any desktop capture.
    #[cfg(test)]
    {
        (
            Rc::new(tint::Wallpaper {
                origin: (0, 0),
                width: 1,
                height: 1,
                bgra: vec![32, 40, 48, 255],
                solid: Some(0x202830),
            }),
            true,
        )
    }
    #[cfg(not(test))]
    {
        // wallpaper-blur-spec 8: the primary monitor's picture at the preview scale, no icons or windows.
        let matte = unsafe {
            windows::Win32::Graphics::Gdi::GetSysColor(
                windows::Win32::Graphics::Gdi::COLOR_BACKGROUND,
            )
        };
        match tint::primary_preview() {
            Some(picture) => (Rc::new(picture), true),
            None => (
                Rc::new(tint::Wallpaper {
                    origin: (0, 0),
                    width: 1,
                    height: 1,
                    bgra: color_argb(matte).to_le_bytes().to_vec(),
                    solid: Some(matte),
                }),
                false,
            ),
        }
    }
}
/// Centred cover mapping is shared by sharp and blurred copies, so sampling remains registered.
fn cover(source: &tint::Wallpaper, w: u32, h: u32) -> tint::Wallpaper {
    let sw = source.width.max(1) as f32;
    let sh = source.height.max(1) as f32;
    let scale = (w as f32 / sw).max(h as f32 / sh);
    let ox = (sw - w as f32 / scale) * 0.5;
    let oy = (sh - h as f32 / scale) * 0.5;
    let mut pixels = vec![0; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let sx = (ox + (x as f32 + 0.5) / scale).floor() as i32;
            let sy = (oy + (y as f32 + 0.5) / scale).floor() as i32;
            let p = source
                .solid
                .map(color_argb)
                .or_else(|| {
                    source.pixel(
                        sx.clamp(0, source.width.max(1) - 1),
                        sy.clamp(0, source.height.max(1) - 1),
                    )
                })
                .unwrap_or(0xff202830)
                | 0xff000000;
            pixels[((y * w + x) * 4) as usize..][..4].copy_from_slice(&p.to_le_bytes());
        }
    }
    tint::Wallpaper {
        origin: (0, 0),
        width: w as i32,
        height: h as i32,
        bgra: pixels,
        solid: source.solid,
    }
}
fn crop(source: &tint::Wallpaper, x: u32, y: u32, w: u32, h: u32) -> tint::Wallpaper {
    let mut pixels = vec![0; (w * h * 4) as usize];
    for row in 0..h {
        let from = (((y + row) * source.width as u32 + x) * 4) as usize;
        let to = (row * w * 4) as usize;
        pixels[to..to + (w * 4) as usize]
            .copy_from_slice(&source.bgra[from..from + (w * 4) as usize]);
    }
    tint::Wallpaper {
        origin: (0, 0),
        width: w as i32,
        height: h as i32,
        bgra: pixels,
        solid: source.solid,
    }
}
fn apply_mask(pixels: &mut [u8], mask: &[u8]) {
    for (p, m) in pixels.chunks_exact_mut(4).zip(mask.chunks_exact(4)) {
        for c in p {
            *c = ((*c as u32 * m[3] as u32 + 127) / 255) as u8;
        }
    }
}
fn prepare(cache: &mut Cache, w: u32, h: u32, dpi: u32, blur: u32) -> Result<()> {
    if cache
        .prepared
        .as_ref()
        .is_some_and(|p| p.width == w && p.height == h && p.dpi == dpi && p.blur == blur)
    {
        return Ok(());
    }
    let sampled = if blur == 0 || cache.source.solid.is_some() {
        cache.source.clone()
    } else {
        if !cache.blurred.as_ref().is_some_and(|(r, _)| *r == blur) {
            cache.blurred = Some((
                blur,
                tint::sample_blur(&cache.source, f32::from_bits(blur))
                    .map(Rc::new)
                    .unwrap_or_else(|| cache.source.clone()),
            ));
        }
        cache.blurred.as_ref().unwrap().1.clone()
    };
    if let Some(prepared) = cache
        .prepared
        .as_mut()
        .filter(|p| p.width == w && p.height == h && p.dpi == dpi)
    {
        let scaled = cover(&sampled, w, h);
        prepared.sampled_group = crop(
            &scaled,
            (w - prepared.body.width()) / 2,
            (h - prepared.body.height()) / 2,
            prepared.body.width(),
            prepared.body.height(),
        );
        prepared.blur = blur;
        prepared.key = None;
        return Ok(());
    }
    let gw = scf(320.0, dpi).round().min(w as f32).max(1.0) as u32;
    let gh = scf(150.0, dpi).round().min(h as f32).max(1.0) as u32;
    let mut mask = Surface::new(w, h)?;
    mask.draw(|ctx| {
        ctx.clear(Rgba::TRANSPARENT);
        ctx.fill_rounded_rect(
            RectF::new(0.0, 0.0, w as f32, h as f32),
            scf(8.0, dpi),
            Rgba::new(255, 255, 255, 255),
        )
    })?;
    let sharp = cover(&cache.source, w, h);
    let sampled = cover(&sampled, w, h);
    let mut output = Surface::new(w, h)?;
    output.pixels_mut().copy_from_slice(&sharp.bgra);
    apply_mask(output.pixels_mut(), mask.pixels());
    cache.prepared = Some(Prepared {
        width: w,
        height: h,
        dpi,
        blur,
        sharp,
        sampled_group: crop(&sampled, (w - gw) / 2, (h - gh) / 2, gw, gh),
        body: Surface::new(gw, gh)?,
        title: Surface::new(gw, gh)?,
        icons: Surface::new(gw, gh)?,
        labels: Surface::new(gw, gh)?,
        mask: Surface::new(gw, gh)?,
        output,
        outer_mask: mask.pixels().to_vec(),
        key: None,
    });
    Ok(())
}
#[cfg(test)]
thread_local! {static PROFILE:RefCell<Option<Vec<(&'static str,f64)>>> = const {RefCell::new(None)};}
struct Profile {
    #[cfg(test)]
    stamp: std::time::Instant,
}
impl Profile {
    fn new() -> Self {
        Self {
            #[cfg(test)]
            stamp: std::time::Instant::now(),
        }
    }
    fn mark(&mut self, _stage: &'static str) {
        #[cfg(test)]
        {
            let now = std::time::Instant::now();
            let elapsed = now.duration_since(self.stamp).as_secs_f64() * 1000.0;
            self.stamp = now;
            PROFILE.with(|p| {
                if let Some(profile) = p.borrow_mut().as_mut() {
                    profile.push((_stage, elapsed));
                }
            });
        }
    }
}
fn paint(p: &mut Prepared, settings: &Settings, model: &Effective) -> Result<()> {
    let high_contrast = group_paint::high_contrast();
    let key = StyleKey {
        look: model.clone(),
        label: settings.label_mode,
        shade: settings.darken_titlebars,
        square: settings.square_edges,
        outline: settings.outline,
        opacity: settings.desktop_blend_icon_opacity,
        uses_tint: settings.desktop_blend_uses_tint_icons_color,
        controls: settings.use_titlebar_controls,
        high_contrast,
    };
    if p.key.as_ref() == Some(&key) {
        return Ok(());
    }
    // camouflage-spec 4: prepared surfaces are reused until an actual appearance key changes.
    let body_blend = model.blend_icons && model.blend_background;
    let (mask_dirty, body_dirty, title_dirty, icons_dirty, labels_dirty) = (true,true,true,true,true);
    let mut profile = Profile::new();
    let dpi = p.dpi;
    let w = p.body.width();
    let h = p.body.height();
    let x = (p.width - w) / 2;
    let y = (p.height - h) / 2;
    let m = group_paint::metrics(dpi);
    let full = RectF::new(0.0, 0.0, w as f32, h as f32);
    // On-hover mode deliberately displays its title in the sample (settings-redesign-spec 7).
    let has_title = settings.label_mode != 0;
    let th = if has_title {
        m.title_h.min(h as i32)
    } else {
        0
    };
    let strip = crate::geometry::Rect::new(0, 0, w as i32, th);
    let alpha = group_paint::see_through_alpha(model.see_through as i64);
    let sampled = &p.sampled_group;
    let chosen = model.tint_in_use();
    let palette = if model.blend_icons && !settings.desktop_blend_uses_tint_icons_color {
        wallpaper_palette(sampled, [0,0,w as i32,h as i32])
    } else { camouflage::derive_tint_palette(color_rgb(chosen)) };
    let strength = if model.blend_icons {super::look_pages::blend_percent(settings.desktop_blend_icon_opacity) as u8} else {100};
    // Item labels read the body alone (corral-painting-spec 4.4); the title text is decided by the painter.
    let text_color = if model.text_auto {auto_text_color_for(body_blend,sampled,[0,0,w as i32,h as i32],&palette,strength,model.color,alpha)} else {model.text_color};
    let rendered_color=if body_blend {mapped_color(&palette,strength,model.color)} else {model.color};
    let title_text = "Downloads";
    let weight = group_paint::title_weight(None);
    let text_w = group_paint::measure_title(title_text, dpi, weight, false);
    // The room for the hover buttons is reserved, so the title sits where it does on the desktop.
    let buttons = settings.use_titlebar_controls;
    let layout = group_paint::layout_title(&group_paint::TitleLayoutInput {
        strip, dpi, want: group_paint::ButtonSet { menu: buttons, up: false, close: buttons }, folder: false, text_w,
    });
    let paint = group_paint::PaintInputs {
        outer: crate::geometry::Rect::new(0, 0, w as i32, h as i32),
        strip: has_title.then_some(strip),
        strip_above: true,
        folded: false,
        dpi,
        square_edges: settings.square_edges,
        body_rgb: group_paint::colorref_rgb(rendered_color),
        fill_alpha: alpha,
        shade: settings.darken_titlebars,
        outline: settings.outline,
        fade: 1.0,
        reveal: 1.0,
        strip_hover: 0.0,
        text: if model.text_auto { group_paint::TextMode::Auto } else { group_paint::TextMode::Custom(group_paint::colorref_rgb(model.text_color)) },
        backdrop_mean: camouflage::mean_color(&camouflage::sample_rect(sampled, [0, 0, w as i32, h as i32], 32)),
        title: title_text,
        weight,
        italic: false,
        layout,
        menu: group_paint::ButtonState::Rest,
        up: group_paint::ButtonState::Rest,
        close: group_paint::ButtonState::Rest,
        buttons: 0.0,
        tabs: None,
        high_contrast,
    };
    let radius = paint.radius();
    profile.mark("geometry");
    if mask_dirty {
        p.mask.draw(|ctx| {
            ctx.clear(Rgba::TRANSPARENT);
            ctx.fill_corner_shape(full, [radius; 4], Rgba::new(255, 255, 255, 255))
        })?;
    }
    profile.mark("mask");
    if body_dirty {
        p.body.draw(|ctx| {
            ctx.clear(Rgba::TRANSPARENT);
            group_paint::paint_fills(&mut group_paint::CtxSink(ctx), &paint)
        })?;
        if p.blur != 0 && paint.high_contrast.is_none() {
            tint::composite_backdrop_from(
                p.body.pixels_mut(),
                p.mask.pixels(),
                w as usize,
                h as usize,
                (0, 0),
                255,
                1.0,
                sampled,
            );
        }
    }
    profile.mark("body");
    if title_dirty {
        // Outline, buttons and title text last: nothing lies over them (spec 3.1).
        p.title.draw(|ctx| {
            ctx.clear(Rgba::TRANSPARENT);
            group_paint::paint_chrome(&mut group_paint::CtxSink(ctx), &paint)
        })?;
    }
    profile.mark("title");
    let icon_size = scf(40.0, dpi);
    let icon_y = th as f32 + scf(18.0, dpi);
    if icons_dirty {
        p.icons.draw(|ctx| {
            ctx.clear(Rgba::TRANSPARENT);
            for (i, color) in [0xE85D50, 0xF2C14E, 0x5BB5A2, 0xEDEFF2]
                .into_iter()
                .enumerate()
            {
                let ix = scf([51.5, 124.0, 195.5, 270.5][i] - 20.0, dpi);
                ctx.fill_rounded_rect(
                    RectF::new(ix, icon_y, ix + icon_size, icon_y + icon_size),
                    scf(6.0, dpi),
                    Rgba::new((color >> 16) as u8, (color >> 8) as u8, color as u8, 255),
                )?;
            }
            Ok(())
        })?;

    }
    profile.mark("icons");
    if labels_dirty {
        p.labels.draw(|ctx| {
            ctx.clear(Rgba::TRANSPARENT);
            for (i, label) in ["report.pdf", "photos", "setup.exe", "notes.txt"]
                .into_iter()
                .enumerate()
            {
                let cx = scf([51.5, 124.0, 195.5, 270.5][i], dpi);
                ctx.draw_text_cached(
                    label,
                    RectF::new(
                        cx - scf(32.0, dpi),
                        icon_y + icon_size + scf(6.0, dpi),
                        cx + scf(32.0, dpi),
                        icon_y + icon_size + scf(23.0, dpi),
                    ),
                    &TextStyle {
                        family: crate::style::FONT,
                        size_px: scf(11.0, dpi),
                        weight: 400,
                        color: rgba(if model.text_auto { text_color } else { 0x00ff_ffff },255),
                        align: Align::Center,
                        shadow: Some((Rgba::new(0, 0, 0, 153), scf(1.0, dpi), scf(1.0, dpi))),
                    },
                )?;
            }
            Ok(())
        })?;
    }
    profile.mark("labels");
    // camouflage-spec 3.4/3.5: icons only; text never enters the pixel mapping.
    if (model.blend_icons || model.tint_icons) && icons_dirty {
        apply_pixels(&palette, strength, p.icons.pixels_mut());
    }
    profile.mark("icon-effects");
    // Wallpaper outside the sample never changes. Source-over only the group bounds,
    // using the same premultiplied BGRA rule as the engine's snapshot composition.
    let output = p.output.pixels_mut();
    let layers = [
        p.body.pixels(),
        p.icons.pixels(),
        p.labels.pixels(),
        p.title.pixels(),
    ];
    for gy in 0..h as usize {
        for gx in 0..w as usize {
            let from = (gy * w as usize + gx) * 4;
            let to = (((y as usize + gy) * p.width as usize) + x as usize + gx) * 4;
            let mut pixel = [
                p.sharp.bgra[to],
                p.sharp.bgra[to + 1],
                p.sharp.bgra[to + 2],
                255,
            ];
            for layer in layers {
                let inverse = 255 - layer[from + 3] as u32;
                for c in 0..3 {
                    pixel[c] = (layer[from + c] as u32 + (pixel[c] as u32 * inverse + 127) / 255)
                        .min(255) as u8;
                }
            }
            let mask = p.outer_mask[to + 3] as u32;
            for c in 0..4 {
                output[to + c] = ((pixel[c] as u32 * mask + 127) / 255) as u8;
            }
        }
    }
    profile.mark("compose");
    p.key = Some(key);
    Ok(())
}

fn draw_prepared(ctx: &Ctx, prepared: &Prepared, rect: RectF, theme: &Theme) -> Result<()> {
    ctx.draw_bitmap(
        prepared.output.pixels(),
        prepared.width,
        prepared.height,
        rect,
        1.0,
    )?;
    ctx.stroke_rounded_rect(
        rect,
        scf(8.0, prepared.dpi),
        scf(1.0, prepared.dpi),
        super::kit::theme::Theme::new(theme.light).border,
    )
}

pub(super) fn draw(
    ctx: &Ctx,
    rect: RectF,
    dpi: u32,
    settings: &Settings,
    model: &Effective,
    theme: &Theme,
) -> Result<()> {
    let w = (rect.right - rect.left).round().max(1.0) as u32;
    let h = (rect.bottom - rect.top).round().max(1.0) as u32;
    CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        // A placeholder source is replaced as soon as the picture is ready (never cached for good).
        if PLACEHOLDER.get() > 0 {
            *cache = None;
        }
        let cache = cache.get_or_insert_with(|| {
            let (source, real) = source();
            PLACEHOLDER.set(if real { 0 } else { PLACEHOLDER.get().saturating_add(1).min(PICTURE_RETRIES + 1) });
            if PLACEHOLDER.get() > PICTURE_RETRIES {
                PLACEHOLDER.set(0); // give up: keep the colour
            }
            Cache { source, blurred: None, prepared: None }
        });
        let blur = tint::blur_gate(
            settings.background_blur_enabled,
            settings.background_blur_radius,
        )
        .map(f32::to_bits)
        .unwrap_or(0);
        prepare(cache, w, h, dpi, blur)?;
        let p = cache.prepared.as_mut().unwrap();
        paint(p, settings, model)?;
        draw_prepared(ctx, p, rect, theme)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> tint::Wallpaper {
        let (w, h) = (960, 540);
        let mut bgra = vec![0; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let at = (y * w + x) * 4;
                bgra[at..at + 4].copy_from_slice(&[
                    (40 + x * 130 / w) as u8,
                    (45 + y * 110 / h) as u8,
                    (70 + x * 130 / w) as u8,
                    255,
                ]);
            }
        }
        tint::Wallpaper {
            origin: (0, 0),
            width: w as i32,
            height: h as i32,
            bgra,
            solid: None,
        }
    }
    fn model(settings: &Settings) -> Effective {
        crate::color_state::effective_of(settings, &Default::default())
    }
    fn cref_of(r: u8, g: u8, b: u8) -> u32 {
        crate::settings_ui::color_popover::cref_of(r, g, b)
    }
    #[test]
    fn cover_centres_and_preserves_solid() {
        let f = fixture();
        let p = cover(&f, 200, 200);
        assert_eq!(p.pixel(0, 0), f.pixel(211, 1));
        let s = source().0;
        let p = cover(&s, 13, 11);
        assert!(p.bgra.chunks_exact(4).all(|p| p == [32, 40, 48, 255]));
    }
    #[test]
    fn scoped_styles_change_render_while_cached_wallpaper_stays_owned() {
        let mut cache = Cache {
            source: Rc::new(fixture()),
            blurred: None,
            prepared: None,
        };
        prepare(&mut cache, 678, 200, 96, 0).unwrap();
        let source = cache.source.clone();
        let settings = Settings::default();
        let mut model = model(&settings);
        let p = cache.prepared.as_mut().unwrap();
        paint(p, &settings, &model).unwrap();
        let before = p.output.pixels().to_vec();
        model.color = cref_of(63, 111, 181);
        paint(p, &settings, &model).unwrap();
        assert_ne!(before, p.output.pixels());
        assert!(Rc::ptr_eq(&source, &cache.source));
        let same = p.output.pixels().as_ptr();
        paint(p, &settings, &model).unwrap();
        assert_eq!(same, p.output.pixels().as_ptr());
        assert_eq!(
            &p.output.pixels()[..4],
            &[0, 0, 0, 0],
            "rounded preview does not leak square wallpaper corners"
        );
    }
    #[test]
    fn camouflage_preview_preserves_label_pixels() {
        let mut cache=Cache{source:Rc::new(fixture()),blurred:None,prepared:None};
        prepare(&mut cache,678,200,96,0).unwrap();
        let mut settings=Settings::default();
        settings.text_color_auto=false;
        let mut m=model(&settings);
        let p=cache.prepared.as_mut().unwrap();
        paint(p,&settings,&m).unwrap();
        let labels=p.labels.pixels().to_vec();
        let titles=p.title.pixels().to_vec();
        {
            m.blend_icons=true;
            settings.desktop_blend_icons=true;
            for strength in [0,128,255] {
                settings.desktop_blend_icon_opacity=strength;
                paint(p,&settings,&m).unwrap();
                assert_eq!(p.labels.pixels(),labels);
                assert_eq!(p.title.pixels(),titles);
            }
        }
    }
    #[test]
    fn cached_composition_matches_direct2d_source_over() {
        let mut cache = Cache {
            source: Rc::new(fixture()),
            blurred: None,
            prepared: None,
        };
        prepare(&mut cache, 678, 200, 96, 0).unwrap();
        let mut settings = Settings::default();
        settings.outline = true;
        settings.color = cref_of(63, 111, 181);
        settings.transparency_factor = 900;
        let mut model = model(&settings);
        model.tint_icons = true;
        model.tint_color = cref_of(185, 122, 51);
        let p = cache.prepared.as_mut().unwrap();
        paint(p, &settings, &model).unwrap();
        let w = p.body.width();
        let h = p.body.height();
        let x = (p.width - w) / 2;
        let y = (p.height - h) / 2;
        let mut expected = Surface::new(p.width, p.height).unwrap();
        expected
            .draw(|ctx| {
                ctx.clear(Rgba::TRANSPARENT);
                ctx.draw_bitmap(
                    &p.sharp.bgra,
                    p.width,
                    p.height,
                    RectF::new(0.0, 0.0, p.width as f32, p.height as f32),
                    1.0,
                )?;
                for layer in [&p.body, &p.title, &p.icons, &p.labels] {
                    ctx.draw_bitmap(
                        layer.pixels(),
                        w,
                        h,
                        RectF::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32),
                        1.0,
                    )?;
                }
                Ok(())
            })
            .unwrap();
        apply_mask(expected.pixels_mut(), &p.outer_mask);
        let max = expected
            .pixels()
            .iter()
            .zip(p.output.pixels())
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(
            max <= 2,
            "premultiplied source-over differs from D2D by {max} levels"
        );
    }
    #[test]
    fn every_scoped_visual_input_changes_only_the_sample_corral() {
        let mut cache = Cache {
            source: Rc::new(fixture()),
            blurred: None,
            prepared: None,
        };
        prepare(&mut cache, 678, 200, 96, 0).unwrap();
        let base = Settings::default();
        let base_model = model(&base);
        let p = cache.prepared.as_mut().unwrap();
        paint(p, &base, &base_model).unwrap();
        let before = p.output.pixels().to_vec();
        for change in 0..9 {
            let mut settings = base.clone();
            let mut model = base_model.clone();
            match change {
                0 => model.color = cref_of(63, 111, 181),
                1 => model.see_through = 250,
                2 => settings.square_edges = true,
                3 => settings.outline = !settings.outline,
                4 => settings.label_mode = 0,
                5 => {model.text_color = cref_of(240, 138, 128);model.text_auto = false;},
                6 => settings.darken_titlebars = if base.darken_titlebars == 0 { 2 } else { 0 },
                7 => {
                    model.tint_icons = true;
                    model.tint_color = cref_of(185, 122, 51);
                }
                _ => {
                    model.blend_icons = true;
                    model.blend_background = true;
                }
            }
            paint(p, &settings, &model).unwrap();
            assert_ne!(before, p.output.pixels(), "style input {change}");
            let mut fresh = Cache {
                source: Rc::new(fixture()),
                blurred: None,
                prepared: None,
            };
            prepare(&mut fresh, 678, 200, 96, 0).unwrap();
            let fresh = fresh.prepared.as_mut().unwrap();
            paint(fresh, &settings, &model).unwrap();
            assert_eq!(
                p.output.pixels(),
                fresh.output.pixels(),
                "partial sample repaint must equal fresh render for input {change}"
            );
            let outside = (15 * 678 + 25) * 4;
            assert_eq!(
                &before[outside..outside + 4],
                &p.output.pixels()[outside..outside + 4],
                "wallpaper outside Corral remains sharp"
            );
        }
    }
    #[test]
    fn sample_goldens_and_repaint_timings() {
        let root = std::path::Path::new("target/camouflage-ui-qa/sample");
        std::fs::create_dir_all(root).unwrap();
        let mut timings = String::from(
            "Offscreen synchronous sample rendering, milliseconds. Cached means unchanged image and offscreen presentation; changed means color changed with prepared wallpaper/surfaces and offscreen presentation. Cold means a fresh local preview cache, not a cold OS process. This does not measure native window present/input latency.\n",
        );
        for dpi in [96, 120, 144, 240] {
            for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
                for style in 0..2 {
                    let mut settings = Settings::default();
                    settings.color = cref_of(63, 111, 181);
                    settings.outline = true;
                    settings.label_mode = 2;
                    if style == 1 {
                        settings.square_edges = true;
                        settings.label_mode = 0;
                        settings.darken_titlebars = 2;
                        settings.desktop_tint_icons = true;
                        settings.desktop_tint_icons_color = cref_of(185, 122, 51);
                        settings.transparency_factor = 700;
                        settings.background_blur_enabled = true;
                        settings.background_blur_radius = 30.0;
                    }
                    let mut model = model(&settings);
                    let mut cache = Cache {
                        source: Rc::new(fixture()),
                        blurred: None,
                        prepared: None,
                    };
                    let w = scf(678.0, dpi) as u32;
                    let h = scf(200.0, dpi) as u32;
                    let cold = std::time::Instant::now();
                    prepare(
                        &mut cache,
                        w,
                        h,
                        dpi,
                        if style == 1 { 30.0f32.to_bits() } else { 0 },
                    )
                    .unwrap();
                    let p = cache.prepared.as_mut().unwrap();
                    paint(p, &settings, &model).unwrap();
                    let cold_ms = cold.elapsed().as_secs_f64() * 1000.0;
                    let mut out = Surface::new(w, h).unwrap();
                    out.draw(|ctx| {
                        ctx.clear(theme.panel_bg);
                        draw_prepared(ctx, p, RectF::new(0.0, 0.0, w as f32, h as f32), &theme)
                    })
                    .unwrap();
                    out.save_png(&root.join(format!("sample-{name}-{dpi}-style{style}.png")))
                        .unwrap();
                    let start = std::time::Instant::now();
                    for _ in 0..20 {
                        paint(p, &settings, &model).unwrap();
                        out.draw(|ctx| {
                            ctx.clear(theme.panel_bg);
                            draw_prepared(ctx, p, RectF::new(0.0, 0.0, w as f32, h as f32), &theme)
                        })
                        .unwrap();
                    }
                    let cached_ms = start.elapsed().as_secs_f64() * 1000.0 / 20.0;
                    PROFILE.with(|p| *p.borrow_mut() = Some(Vec::new()));
                    let mut changed = Vec::new();
                    for n in 1..=8 {
                        model.color = cref_of(63 + n, 111, 181);
                        let start = std::time::Instant::now();
                        paint(p, &settings, &model).unwrap();
                        out.draw(|ctx| {
                            ctx.clear(theme.panel_bg);
                            draw_prepared(ctx, p, RectF::new(0.0, 0.0, w as f32, h as f32), &theme)
                        })
                        .unwrap();
                        changed.push(start.elapsed().as_secs_f64() * 1000.0);
                    }
                    let profile = PROFILE.with(|p| p.borrow_mut().take().unwrap());
                    let mut stages = std::collections::BTreeMap::<&str, (f64, usize)>::new();
                    for (name, ms) in profile {
                        let stage = stages.entry(name).or_default();
                        stage.0 += ms;
                        stage.1 += 1;
                    }
                    let stage_text = stages
                        .iter()
                        .map(|(stage, (ms, n))| format!("{stage}={:.3}", ms / (*n as f64)))
                        .collect::<Vec<_>>()
                        .join(",");
                    let mean = changed.iter().sum::<f64>() / changed.len() as f64;
                    let max = changed.iter().copied().fold(0.0, f64::max);
                    timings.push_str(&format!("{name} dpi={dpi} style={style}: cold={cold_ms:.3}, cached={cached_ms:.3}, changed_mean={mean:.3}, changed_max={max:.3}; stages {stage_text}\n"));
                }
            }
        }
        std::fs::write(root.join("sample-repaint-timings.txt"), timings).unwrap();
        reset();
    }
}
