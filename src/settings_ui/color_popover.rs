//! Shared settings colour popover (colour-state-spec 3.3). It owns no settings and never persists
//! a value itself: it reports preview frames and deliberate choices to its host, which keeps the
//! session and decides at the end whether anything is stored.
use super::kit::{
    controls::{self, Control, VisualState},
    layout::snap_rect,
    theme::{self, Theme, Type},
};
use crate::render::{Ctx, RectF, Rgba, Surface};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint, HBRUSH, HFONT,
    InvalidateRect, PAINTSTRUCT, SRCCOPY, SetBkColor, SetTextColor,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::EM_SETLIMITTEXT;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetFocus, GetKeyState, ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, Result, w};

pub const WIDTH: f32 = 320.;
pub const HEIGHT: f32 = 252.;
/// Extra height for the caption under the header (two 12 px lines).
pub const CAPTION_H: f32 = 36.;
/// colour-state-spec 3.3: shown while a Corral's field still follows the shared value.
pub const INHERITED_CAPTION: &str = "Using the color all Corrals share. Changing it gives this Corral its own.";

/// Hue, saturation and brightness, each 0..255 (hue covers the full circle).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hsb {
    pub h: f32,
    pub s: f32,
    pub b: f32,
}

impl Hsb {
    /// Clamps every component to 0..255.
    pub fn new(h: f32, s: f32, b: f32) -> Hsb {
        Hsb { h: h.clamp(0.0, 255.0), s: s.clamp(0.0, 255.0), b: b.clamp(0.0, 255.0) }
    }
}

/// Split a COLORREF (`0x00BBGGRR`).
pub fn rgb_of(cref: u32) -> (u8, u8, u8) {
    ((cref & 0xff) as u8, ((cref >> 8) & 0xff) as u8, ((cref >> 16) & 0xff) as u8)
}

/// Build a COLORREF.
pub fn cref_of(r: u8, g: u8, b: u8) -> u32 {
    u32::from(r) | u32::from(g) << 8 | u32::from(b) << 16
}

/// RGB (0..255 channels) to HSB on the 0..255 scale (hue 255 is a full turn). Greys have hue 0.
pub fn hsb_from_rgb(r: u8, g: u8, b: u8) -> Hsb {
    let (rf, gf, bf) = (r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
    let max = rf.max(gf).max(bf);
    let min = rf.min(gf).min(bf);
    let delta = max - min;
    let hue_deg = if delta == 0.0 {
        0.0
    } else if max == rf {
        60.0 * ((gf - bf) / delta).rem_euclid(6.0)
    } else if max == gf {
        60.0 * ((bf - rf) / delta + 2.0)
    } else {
        60.0 * ((rf - gf) / delta + 4.0)
    };
    let sat = if max == 0.0 { 0.0 } else { delta / max };
    Hsb { h: (hue_deg / 360.0 * 255.0) as f32, s: (sat * 255.0) as f32, b: (max * 255.0) as f32 }
}

/// HSB (0..255 scale) to RGB; channels use ordinary rounding.
pub fn hsb_to_rgb(c: Hsb) -> (u8, u8, u8) {
    let hue_deg = (c.h as f64 / 255.0 * 360.0).rem_euclid(360.0);
    let sat = (c.s as f64 / 255.0).clamp(0.0, 1.0);
    let val = (c.b as f64 / 255.0).clamp(0.0, 1.0);
    let chroma = val * sat;
    let x = chroma * (1.0 - ((hue_deg / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = val - chroma;
    let (r, g, b) = match (hue_deg / 60.0) as u32 {
        0 => (chroma, x, 0.0),
        1 => (x, chroma, 0.0),
        2 => (0.0, chroma, x),
        3 => (0.0, x, chroma),
        4 => (x, 0.0, chroma),
        _ => (chroma, 0.0, x),
    };
    let to_u8 = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to_u8(r), to_u8(g), to_u8(b))
}

/// COLORREF of an HSB value.
pub fn cref_of_hsb(c: Hsb) -> u32 {
    let (r, g, b) = hsb_to_rgb(c);
    cref_of(r, g, b)
}

/// HSB of a COLORREF.
pub fn hsb_of_cref(cref: u32) -> Hsb {
    let (r, g, b) = rgb_of(cref);
    hsb_from_rgb(r, g, b)
}

/// What the popover tells its host while it is open (colour-state-spec 3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Live {
    /// The draft changed for display only (hex typing, an eyedropper hover, a cancelled pick).
    Preview(u32),
    /// A deliberate choice: a slider move that changed the colour, a confirmed hex, a finished pick.
    Choose(u32),
}

thread_local! {
    static OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static CLOSE_REQUEST: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// A popover is open on this thread.
pub fn is_open() -> bool {
    OPEN.with(|o| o.get())
}

/// Ask the open popover to end as if the user closed it (`cancel` false) or pressed Esc (`cancel`
/// true): used when the Corral menu retargets the page or the target Corral disappears.
pub fn request_close(cancel: bool) {
    if is_open() {
        CLOSE_REQUEST.with(|r| r.set(Some(cancel)));
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub color: u32,
    pub cancelled: bool,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Model {
    pub opening: u32,
    pub color: u32,
    pub hsb: Hsb,
}
impl Model {
    pub fn new(color: u32) -> Self {
        Self {
            opening: color,
            color,
            hsb: hsb_of_cref(color),
        }
    }
    pub fn set_color(&mut self, color: u32) {
        self.color = color;
        self.hsb = hsb_of_cref(color);
    }
    pub fn set_component(&mut self, index: usize, value: f32) {
        let value = value.clamp(0., 255.);
        match index {
            0 => self.hsb.h = value,
            1 => self.hsb.s = value,
            2 => self.hsb.b = value,
            _ => return,
        };
        self.color = cref_of_hsb(self.hsb);
    }
    pub fn finish(self, cancelled: bool) -> Outcome {
        Outcome {
            color: if cancelled { self.opening } else { self.color },
            cancelled,
        }
    }
}
pub fn format_hex(color: u32) -> String {
    let (r, g, b) = rgb_of(color);
    format!("#{r:02X}{g:02X}{b:02X}")
}
pub fn parse_hex(value: &str) -> Option<u32> {
    let value = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if value.len() != 6 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u32::from_str_radix(value, 16).ok()?;
    Some(cref_of(
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
    ))
}
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub preview: RectF,
    pub close: RectF,
    pub tracks: [RectF; 3],
    pub hex: RectF,
    pub eye: RectF,
}
/// `extra` is the height added under the header when a caption is shown.
pub fn layout(extra: f32) -> Layout {
    Layout {
        preview: RectF::new(17., 17., 41., 41.),
        close: RectF::new(271., 13., 303., 45.),
        tracks: [81., 129., 177.].map(|y| RectF::new(17., y + extra, 303., y + extra + 12.)),
        hex: RectF::new(49., 203. + extra, 169., 235. + extra),
        eye: RectF::new(271., 203. + extra, 303., 235. + extra),
    }
}
fn contains(r: RectF, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}
fn component(m: &Model, index: usize) -> f32 {
    [m.hsb.h, m.hsb.s, m.hsb.b][index]
}
fn color(cref: u32) -> Rgba {
    let (r, g, b) = rgb_of(cref);
    Rgba::new(r, g, b, 255)
}
fn stops(model: &Model, index: usize) -> Vec<(f32, Rgba)> {
    let at = |h, s, b| color(cref_of_hsb(Hsb::new(h, s, b)));
    match index {
        0 => (0..=6)
            .map(|i| (i as f32 / 6., at(i as f32 / 6. * 255., 255., 255.)))
            .collect(),
        1 => vec![
            (0., at(model.hsb.h, 0., model.hsb.b)),
            (1., at(model.hsb.h, 255., model.hsb.b)),
        ],
        _ => vec![
            (0., Rgba::new(0, 0, 0, 255)),
            (0.5, color(model.color)),
            (1., Rgba::new(255, 255, 255, 255)),
        ],
    }
}
/// Shared production and offscreen renderer. Native EDIT supplies the hex text on a live window.
pub fn draw(
    ctx: &Ctx,
    model: &Model,
    dpi: u32,
    light: bool,
    focus: Option<usize>,
    hover: Option<usize>,
    hex_error: bool,
    draw_hex: bool,
    extra: f32,
    caption: Option<&str>,
) -> Result<()> {
    let t = Theme::new(light);
    let s = dpi as f32 / 96.;
    let l = layout(extra);
    let px = |r| snap_rect(r, dpi);
    ctx.clear(t.surface);
    ctx.stroke_rounded_rect(px(RectF::new(0.5, 0.5, 319.5, 251.5 + extra)), 8. * s, s, t.border)?;
    if let Some(caption) = caption {
        ctx.draw_text(
            caption,
            px(RectF::new(17., 47., 303., 47. + CAPTION_H - 4.)),
            &Type::Caption.style(dpi, t.secondary),
        )?;
    }
    ctx.fill_rounded_rect(px(l.preview), 12. * s, color(model.color))?;
    ctx.draw_text(
        "Custom color",
        px(RectF::new(49., 13., 263., 45.)),
        &Type::BodyStrong.style(dpi, t.text),
    )?;
    for (id, r, glyph) in [(0, l.close, "\u{E8BB}"), (5, l.eye, "\u{EF3C}")] {
        if id == 5 {
            ctx.fill_rounded_rect(px(r), 4. * s, t.control)?;
            ctx.stroke_rounded_rect(px(r), 4. * s, s, t.border)?;
        }
        controls::draw_control(
            ctx,
            Control::IconButton,
            r,
            "",
            VisualState {
                hovered: hover == Some(id),
                focused: focus == Some(id),
                ..Default::default()
            },
            &t,
            dpi,
            0.,
        )?;
        let mut style = Type::Body.style(dpi, t.text);
        style.family = super::icon_family();
        style.size_px = 16. * s;
        style.align = crate::render::Align::Center;
        ctx.draw_text(glyph, px(r), &style)?;
    }
    for (index, label) in ["Hue", "Saturation", "Brightness"].iter().enumerate() {
        let r = l.tracks[index];
        ctx.draw_text(
            label,
            px(RectF::new(r.left, r.top - 22., r.right, r.top - 6.)),
            &Type::Caption.style(dpi, t.secondary),
        )?;
        ctx.fill_horizontal_gradient(px(r), &stops(model, index), 6. * s)?;
        let x = r.left + (r.right - r.left) * component(model, index) / 255.;
        let y = (r.top + r.bottom) / 2.;
        // Strokes are centred by D2D: inset the white centreline by 1.5 so its
        // outer diameter stays 16 DIPs and leaves the 1-DIP dark halo visible.
        let halo = px(RectF::new(x - 8.5, y - 8.5, x + 8.5, y + 8.5));
        let disk = px(RectF::new(x - 8., y - 8., x + 8., y + 8.));
        let ring = px(RectF::new(x - 6.5, y - 6.5, x + 6.5, y + 6.5));
        ctx.stroke_rounded_rect(halo, 8.5 * s, s, Rgba::new(27, 29, 34, 255))?;
        ctx.fill_rounded_rect(disk, 8. * s, color(model.color))?;
        ctx.stroke_rounded_rect(ring, 6.5 * s, 3. * s, Rgba::new(255, 255, 255, 255))?;
        if focus == Some(index + 1) {
            ctx.stroke_rounded_rect(
                px(RectF::new(
                    r.left - 4.,
                    r.top - 5.,
                    r.right + 4.,
                    r.bottom + 5.,
                )),
                9. * s,
                2. * s,
                t.focus,
            )?;
        }
    }
    ctx.draw_text(
        "Hex",
        px(RectF::new(17., 203. + extra, 41., 235. + extra)),
        &Type::Caption.style(dpi, t.secondary),
    )?;
    ctx.fill_rounded_rect(px(l.hex), 4. * s, t.control)?;
    ctx.stroke_rounded_rect(
        px(l.hex),
        4. * s,
        s,
        if hex_error { t.danger } else { t.border },
    )?;
    if focus == Some(4) {
        ctx.fill_rect(
            px(RectF::new(
                l.hex.left + 2.,
                l.hex.bottom - 2.,
                l.hex.right - 2.,
                l.hex.bottom,
            )),
            t.accent,
        )?;
    }
    if draw_hex {
        ctx.draw_text(
            &format_hex(model.color),
            px(RectF::new(
                l.hex.left + 10.,
                l.hex.top,
                l.hex.right - 6.,
                l.hex.bottom,
            )),
            &Type::Body.style(dpi, t.text),
        )?;
    }
    if let Some((label, bounds)) = match hover {
        Some(0) => Some(("Close", RectF::new(249., 49., 303., 73.))),
        Some(5) => Some(("Pick color from screen", RectF::new(153., 173. + extra, 303., 197. + extra))),
        _ => None,
    } {
        ctx.fill_rounded_rect(px(bounds), 4. * s, t.surface_raised)?;
        ctx.stroke_rounded_rect(px(bounds), 4. * s, s, t.border)?;
        ctx.draw_text(
            label,
            px(RectF::new(
                bounds.left + 8.,
                bounds.top,
                bounds.right - 8.,
                bounds.bottom,
            )),
            &Type::Caption.style(dpi, t.text),
        )?;
    }
    Ok(())
}
/// Position below the Custom swatch; flip above if necessary and clamp to the destination monitor.
pub fn opening_bounds(anchor: RECT, work: RECT, dpi: u32, extra: f32) -> RECT {
    let s = dpi as f32 / 96.;
    let w = (WIDTH * s).round() as i32;
    let h = ((HEIGHT + extra) * s).round() as i32;
    let w = w.min((work.right - work.left).max(1));
    let h = h.min((work.bottom - work.top).max(1));
    let x = anchor.left.clamp(work.left, work.right - w);
    let below = anchor.bottom + (4. * s).round() as i32;
    let y = if below + h <= work.bottom {
        below
    } else {
        anchor.top - h - (4. * s).round() as i32
    }
    .clamp(work.top, work.bottom - h);
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}
struct State {
    model: Model,
    dpi: u32,
    light: bool,
    edit: HWND,
    font: HFONT,
    brush: HBRUSH,
    surface: Option<Surface>,
    focus: usize,
    keyboard: bool,
    hover: Option<usize>,
    drag: Option<usize>,
    done: bool,
    cancelled: bool,
    pending: Vec<Live>,
    /// The hex text was edited by the user since it was last shown from the draft.
    edited: bool,
    pick: bool,
    picking: bool,
    syncing: bool,
    error: bool,
    /// Height added for the caption, and whether it is still shown (it ends with the first
    /// explicit choice, when the field becomes the Corral's own).
    extra: f32,
    caption: Option<&'static str>,
    chosen: bool,
}
unsafe fn queue(p: *mut State, event: Live) {
    unsafe {
        if matches!(event, Live::Choose(_)) {
            (*p).chosen = true;
        }
        (*p).pending.push(event);
    }
}
fn ptr(hwnd: HWND) -> *mut State {
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State }
}
fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}
unsafe fn edit_text(edit: HWND) -> String {
    unsafe {
        let n = GetWindowTextLengthW(edit).max(0) as usize;
        let mut b = vec![0u16; n + 1];
        let used = GetWindowTextW(edit, &mut b).max(0) as usize;
        String::from_utf16_lossy(&b[..used])
    }
}
unsafe fn sync_edit(hwnd: HWND) {
    unsafe {
        let p = ptr(hwnd);
        if p.is_null() {
            return;
        }
        (*p).syncing = true;
        let edit = (*p).edit;
        let text = HSTRING::from(format_hex((*p).model.color));
        let _ = SetWindowTextW(edit, &text);
        (*p).syncing = false;
        (*p).error = false;
        (*p).edited = false;
    }
}
/// A valid hex string the user typed counts as a choice when it is confirmed by Enter or by
/// leaving the field; an invalid one is dropped and the last good draft stays.
unsafe fn confirm_hex(hwnd: HWND) {
    unsafe {
        let p = ptr(hwnd);
        if p.is_null() || !(*p).edited || (*p).picking {
            return;
        }
        (*p).edited = false;
        if let Some(color) = parse_hex(&edit_text((*p).edit)) {
            (*p).model.set_color(color);
            queue(p, Live::Choose(color));
        }
    }
}
unsafe fn set_color(hwnd: HWND, color: u32) {
    unsafe {
        let p = ptr(hwnd);
        (*p).model.set_color(color);
        queue(p, Live::Choose(color));
        sync_edit(hwnd);
        invalidate(hwnd);
    }
}
unsafe fn slider(hwnd: HWND, index: usize, x: f32) {
    unsafe {
        let p = ptr(hwnd);
        let r = layout((*p).extra).tracks[index];
        let value = (x - r.left) / (r.right - r.left) * 255.;
        let before = (*p).model.color;
        (*p).model.set_component(index, value);
        let after = (*p).model.color;
        queue(p, if after != before { Live::Choose(after) } else { Live::Preview(after) });
        sync_edit(hwnd);
        invalidate(hwnd);
    }
}
unsafe fn fonts(hwnd: HWND) {
    unsafe {
        let p = ptr(hwnd);
        if p.is_null() {
            return;
        }
        let old = (*p).font;
        let size = (14. * (*p).dpi as f32 / 96.).round() as i32;
        (*p).font = CreateFontW(
            -size,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            0,
            &HSTRING::from(theme::font_family()),
        );
        if !(*p).edit.is_invalid() {
            SendMessageW(
                (*p).edit,
                WM_SETFONT,
                WPARAM((*p).font.0 as usize),
                LPARAM(1),
            );
        }
        if !old.is_invalid() {
            let _ = DeleteObject(old);
        }
        let s = (*p).dpi as f32 / 96.;
        let r = layout((*p).extra).hex;
        let _ = SetWindowPos(
            (*p).edit,
            None,
            ((r.left + 10.) * s).round() as i32,
            ((r.top + 6.) * s).round() as i32,
            ((r.right - r.left - 20.) * s).round() as i32,
            (20. * s).round() as i32,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        (*p).surface = None;
    }
}
unsafe fn focus_to(hwnd: HWND, index: usize, keyboard: bool) {
    unsafe {
        let p = ptr(hwnd);
        (*p).focus = index;
        (*p).keyboard = keyboard;
        let target = if index == 4 { (*p).edit } else { hwnd };
        let _ = SetFocus(target);
        invalidate(hwnd);
    }
}
unsafe fn key(hwnd: HWND, key: usize) -> bool {
    unsafe {
        let p = ptr(hwnd);
        if p.is_null() {
            return false;
        }
        match key {
            27 => {
                (*p).cancelled = true;
                (*p).done = true;
                true
            }
            9 => {
                let backwards = GetKeyState(0x10) < 0;
                let next = ((*p).focus + if backwards { 5 } else { 1 }) % 6;
                focus_to(hwnd, next, true);
                true
            }
            13 => {
                if (*p).focus == 4 {
                    if let Some(color) = parse_hex(&edit_text((*p).edit)) {
                        set_color(hwnd, color);
                        (*p).done = true;
                    } else {
                        (*p).error = true;
                        invalidate(hwnd);
                    }
                } else if (*p).focus == 5 {
                    (*p).pick = true;
                } else {
                    (*p).done = true;
                }
                true
            }
            32 if (*p).focus == 0 || (*p).focus == 5 => {
                if (*p).focus == 0 {
                    (*p).done = true;
                } else {
                    (*p).pick = true;
                }
                true
            }
            37 | 38 | 39 | 40 | 33 | 34 | 36 | 35 if (1..=3).contains(&(*p).focus) => {
                let i = (*p).focus - 1;
                let old = component(&(*p).model, i);
                let value = match key {
                    36 => 0.,
                    35 => 255.,
                    33 => old + 10.,
                    34 => old - 10.,
                    37 | 40 => old - 1.,
                    _ => old + 1.,
                };
                let before = (*p).model.color;
                (*p).model.set_component(i, value);
                let after = (*p).model.color;
                queue(p, if after != before { Live::Choose(after) } else { Live::Preview(after) });
                sync_edit(hwnd);
                (*p).keyboard = true;
                invalidate(hwnd);
                true
            }
            _ => false,
        }
    }
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        }
        let p = ptr(hwnd);
        if p.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                let mut r = RECT::default();
                let _ = GetClientRect(hwnd, &mut r);
                let mut surface = (*p).surface.take();
                if surface.is_none() {
                    surface = Surface::new(r.right.max(1) as u32, r.bottom.max(1) as u32).ok();
                }
                if let Some(s) = &mut surface {
                    let model = (*p).model;
                    let dpi = (*p).dpi;
                    let light = (*p).light;
                    let focus = if (*p).keyboard || GetFocus() == (*p).edit {
                        Some((*p).focus)
                    } else {
                        None
                    };
                    let hover = (*p).hover;
                    let error = (*p).error;
                    let _ = s.draw(|ctx| draw(ctx, &model, dpi, light, focus, hover, error, false, (*p).extra, (*p).caption.filter(|_| !(*p).chosen)));
                    let _ = BitBlt(dc, 0, 0, r.right, r.bottom, s.dc(), 0, 0, SRCCOPY);
                }
                (*p).surface = surface;
                let _ = EndPaint(hwnd, &ps);
                return LRESULT(0);
            }
            WM_ERASEBKGND => return LRESULT(1),
            WM_CLOSE => {
                (*p).done = true;
                return LRESULT(0);
            }
            WM_DESTROY => {
                (*p).done = true;
                return LRESULT(0);
            }
            WM_ACTIVATE if wp.0 & 0xffff == 0 && !(*p).picking => {
                (*p).done = true;
                return LRESULT(0);
            }
            WM_KEYDOWN => {
                if key(hwnd, wp.0) {
                    return LRESULT(0);
                }
            }
            WM_LBUTTONDOWN => {
                let s = (*p).dpi as f32 / 96.;
                let x = (lp.0 & 0xffff) as i16 as f32 / s;
                let y = ((lp.0 >> 16) & 0xffff) as i16 as f32 / s;
                let l = layout((*p).extra);
                (*p).keyboard = false;
                for (i, r) in l.tracks.iter().enumerate() {
                    let hit = RectF::new(r.left - 8., r.top - 8., r.right + 8., r.bottom + 8.);
                    if contains(hit, x, y) {
                        focus_to(hwnd, i + 1, false);
                        (*p).drag = Some(i);
                        SetCapture(hwnd);
                        slider(hwnd, i, x);
                        return LRESULT(0);
                    }
                }
                if contains(l.close, x, y) {
                    focus_to(hwnd, 0, false);
                    (*p).done = true;
                } else if contains(l.eye, x, y) {
                    focus_to(hwnd, 5, false);
                    (*p).pick = true;
                } else if contains(l.hex, x, y) {
                    focus_to(hwnd, 4, false);
                }
                return LRESULT(0);
            }
            WM_MOUSEMOVE => {
                let s = (*p).dpi as f32 / 96.;
                let x = (lp.0 & 0xffff) as i16 as f32 / s;
                let y = ((lp.0 >> 16) & 0xffff) as i16 as f32 / s;
                if let Some(i) = (*p).drag {
                    slider(hwnd, i, x);
                } else {
                    let l = layout((*p).extra);
                    let hover = if contains(l.close, x, y) {
                        Some(0)
                    } else if contains(l.eye, x, y) {
                        Some(5)
                    } else {
                        None
                    };
                    if hover != (*p).hover {
                        (*p).hover = hover;
                        invalidate(hwnd);
                    }
                }
                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    ..Default::default()
                };
                let _ = TrackMouseEvent(&mut track);
                return LRESULT(0);
            }
            0x02A3 => { // WM_MOUSELEAVE (not exported by WindowsAndMessaging).
                (*p).hover = None;
                invalidate(hwnd);
                return LRESULT(0);
            }
            WM_LBUTTONUP => {
                if (*p).drag.take().is_some() {
                    let _ = ReleaseCapture();
                }
                return LRESULT(0);
            }
            WM_CAPTURECHANGED => {
                (*p).drag = None;
                return LRESULT(0);
            }
            WM_COMMAND if HWND(lp.0 as *mut _) == (*p).edit => {
                let notification = (wp.0 >> 16) as u32;
                if notification == EN_CHANGE && !(*p).syncing {
                    // Typing previews a valid colour; an invalid string keeps the last good draft and is
                    // only marked (colour-state-spec 3.3).
                    if let Some(color) = parse_hex(&edit_text((*p).edit)) {
                        (*p).model.set_color(color);
                        queue(p, Live::Preview(color));
                        (*p).error = false;
                    } else {
                        (*p).error = true;
                    }
                    (*p).edited = true;
                    invalidate(hwnd);
                } else if notification == EN_SETFOCUS {
                    (*p).focus = 4;
                    invalidate(hwnd);
                } else if notification == EN_KILLFOCUS {
                    confirm_hex(hwnd);
                    sync_edit(hwnd);
                    invalidate(hwnd);
                }
                return LRESULT(0);
            }
            WM_CTLCOLOREDIT => {
                let dc = windows::Win32::Graphics::Gdi::HDC(wp.0 as *mut _);
                let t = Theme::new((*p).light);
                SetBkColor(dc, COLORREF(cref_of(t.control.r, t.control.g, t.control.b)));
                SetTextColor(dc, COLORREF(cref_of(t.text.r, t.text.g, t.text.b)));
                return LRESULT((*p).brush.0 as isize);
            }
            WM_DPICHANGED => {
                (*p).dpi = (wp.0 & 0xffff) as u32;
                let suggested = *(lp.0 as *const RECT);
                let point = POINT {
                    x: suggested.left,
                    y: suggested.top,
                };
                let (work, _) = crate::dialog::monitor_at(point);
                let scale = (*p).dpi as f32 / 96.;
                let width = ((WIDTH * scale).round() as i32).min((work.right - work.left).max(1));
                let height = (((HEIGHT + (*p).extra) * scale).round() as i32).min((work.bottom - work.top).max(1));
                let x = suggested.left.clamp(work.left, work.right - width);
                let y = suggested.top.clamp(work.top, work.bottom - height);
                let r = RECT {
                    left: x,
                    top: y,
                    right: x + width,
                    bottom: y + height,
                };
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
                fonts(hwnd);
                invalidate(hwnd);
                return LRESULT(0);
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            _ => {}
        }
        DefWindowProcW(hwnd, msg, wp, lp)
    }
}
/// Owned popup on the caller's UI thread. The parent must not hold its window-state borrow here:
/// native messages and the screen picker dispatch a nested loop. Callbacks run between dispatches.
/// Anchor is the Custom swatch's screen rectangle in physical pixels.
pub fn show(
    owner: HWND,
    anchor: RECT,
    opening: u32,
    light: bool,
    caption: Option<&'static str>,
    live: &mut dyn FnMut(Live),
) -> Outcome {
    unsafe {
        let old_dpi = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        struct Dpi(windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT);
        impl Drop for Dpi {
            fn drop(&mut self) {
                unsafe {
                    SetThreadDpiAwarenessContext(self.0);
                }
            }
        }
        let _dpi = Dpi(old_dpi);
        let (work, dpi) = crate::dialog::monitor_at(POINT {
            x: anchor.left,
            y: anchor.bottom,
        });
        let extra = if caption.is_some() { CAPTION_H } else { 0. };
        let r = opening_bounds(anchor, work, dpi, extra);
        let t = Theme::new(light);
        let mut state = Box::new(State {
            model: Model::new(opening),
            dpi,
            light,
            edit: HWND::default(),
            font: HFONT::default(),
            brush: CreateSolidBrush(COLORREF(cref_of(t.control.r, t.control.g, t.control.b))),
            surface: None,
            focus: 1,
            keyboard: false,
            hover: None,
            drag: None,
            done: false,
            cancelled: false,
            pending: Vec::new(),
            edited: false,
            pick: false,
            picking: false,
            syncing: false,
            error: false,
            extra,
            caption,
            chosen: false,
        });
        let instance = match GetModuleHandleW(None) {
            Ok(m) => HINSTANCE(m.0),
            Err(_) => {
                let _ = DeleteObject(state.brush);
                return state.model.finish(true);
            }
        };
        let class = w!("CorralColorPopover");
        RegisterClassW(&WNDCLASSW {
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            lpszClassName: class,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            ..Default::default()
        });
        let hwnd = match CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class,
            w!("Custom color"),
            WS_POPUP | WS_CLIPCHILDREN,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            owner,
            None,
            instance,
            Some((&mut *state as *mut State).cast()),
        ) {
            Ok(h) => h,
            Err(_) => {
                let _ = DeleteObject(state.brush);
                return state.model.finish(true);
            }
        };
        let dark = i32::from(!light);
        let corners = 2i32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(20),
            (&dark as *const i32).cast(),
            4,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(33),
            (&corners as *const i32).cast(),
            4,
        );
        state.edit = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            &HSTRING::from(format_hex(opening)),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            0,
            0,
            1,
            1,
            hwnd,
            HMENU(1usize as *mut _),
            instance,
            None,
        )
        .unwrap_or_default();
        if state.edit.is_invalid() {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(hwnd);
            let _ = DeleteObject(state.brush);
            return state.model.finish(true);
        }
        SendMessageW(state.edit, EM_SETLIMITTEXT, WPARAM(7), LPARAM(0));
        fonts(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        focus_to(hwnd, 1, false);
        struct OpenFlag;
        impl Drop for OpenFlag {
            fn drop(&mut self) {
                OPEN.with(|o| o.set(false));
                CLOSE_REQUEST.with(|r| r.set(None));
            }
        }
        OPEN.with(|o| o.set(true));
        CLOSE_REQUEST.with(|r| r.set(None));
        let _open = OpenFlag;
        let mut quit = None;
        let mut message = MSG::default();
        while !state.done && IsWindow(hwnd).as_bool() {
            let status = GetMessageW(&mut message, None, 0, 0).0;
            if status <= 0 {
                if status == 0 {
                    quit = Some(message.wParam.0 as i32);
                }
                state.cancelled = true;
                break;
            }
            if message.message == WM_KEYDOWN
                && (message.hwnd == hwnd || message.hwnd == state.edit)
                && key(hwnd, message.wParam.0)
            {
            } else {
                // Dismiss before dispatching an outside click; its owner still receives the click normally.
                if matches!(
                    message.message,
                    WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_NCLBUTTONDOWN
                ) && message.hwnd != hwnd
                    && message.hwnd != state.edit
                {
                    state.done = true;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            for event in std::mem::take(&mut state.pending) {
                live(event);
            }
            if let Some(cancel) = CLOSE_REQUEST.with(|r| r.take()) {
                state.done = true;
                state.cancelled = cancel;
            }
            if state.pick && !state.done {
                state.pick = false;
                state.picking = true;
                let before = state.model.color;
                let _ = ShowWindow(hwnd, SW_HIDE);
                // No State borrow survives this nested loop or a host callback.
                let picked = crate::screen_picker::pick_with_preview(hwnd, |color| {
                    live(Live::Preview(color));
                });
                state.picking = false;
                if !IsWindow(hwnd).as_bool() {
                    state.done = true;
                    break;
                }
                // A finished pick is one deliberate choice; Esc while picking puts the draft from
                // before the pick back and the popover stays open (colour-state-spec 3.3).
                let color = picked.unwrap_or(before);
                state.model.set_color(color);
                state.pending.clear();
                state.chosen |= picked.is_some();
                live(if picked.is_some() { Live::Choose(color) } else { Live::Preview(color) });
                sync_edit(hwnd);
                invalidate(hwnd);
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
                focus_to(hwnd, 5, true);
            }
        }
        if !state.cancelled {
            // Closing while the hex field holds an unconfirmed valid string confirms it.
            confirm_hex(hwnd);
            for event in std::mem::take(&mut state.pending) {
                live(event);
            }
        }
        let outcome = state.model.finish(state.cancelled);
        let restore_focus = GetForegroundWindow() == hwnd;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        if IsWindow(hwnd).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
        let _ = DeleteObject(state.font);
        let _ = DeleteObject(state.brush);
        if restore_focus && IsWindow(owner).as_bool() {
            let _ = SetFocus(owner);
        }
        if let Some(code) = quit {
            PostQuitMessage(code);
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_round_trip_is_rgb_not_colorref_order() {
        for color in [0, 0xffffff, 0x123456, 0xB56F3F, 0x010203] {
            assert_eq!(parse_hex(&format_hex(color)), Some(color));
        }
        assert_eq!(parse_hex(" #3f6fb5 "), Some(0xB56F3F));
        for bad in ["", "#12345", "#1234567", "#GG0011", "ééé"] {
            assert_eq!(parse_hex(bad), None);
        }
    }
    #[test]
    fn hex_accepts_rrggbb_with_or_without_hash_in_any_case_and_rejects_the_rest() {
        // colour-state-spec 6
        assert_eq!(parse_hex("B56F3F"), parse_hex("#b56f3f"));
        assert_eq!(parse_hex("AbCdEf"), Some(cref_of(0xAB, 0xCD, 0xEF)));
        assert_eq!(parse_hex("#AbCdEf"), Some(cref_of(0xAB, 0xCD, 0xEF)));
        for bad in ["#GGG", "", "#", "1234567", "#1234567", "12 456", "#12345", "0x1234"] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
    }
    #[test]
    fn hsv_sector_colours_have_textbook_values() {
        // One colour per 60 degree sector border (hue scale 0..255 = 360 degrees).
        let cases: [((u8, u8, u8), f32); 6] = [
            ((255, 0, 0), 0.0),
            ((255, 255, 0), 42.5),
            ((0, 255, 0), 85.0),
            ((0, 255, 255), 127.5),
            ((0, 0, 255), 170.0),
            ((255, 0, 255), 212.5),
        ];
        for ((r, g, b), hue) in cases {
            let c = hsb_from_rgb(r, g, b);
            assert!((c.h - hue).abs() < 0.01 && c.s == 255.0 && c.b == 255.0, "{r},{g},{b} -> {c:?}");
            assert_eq!(hsb_to_rgb(c), (r, g, b));
        }
        // Mid-sector colours (sectors 0..5) round-trip.
        for (r, g, b) in [(255, 128, 0), (128, 255, 0), (0, 255, 128), (0, 128, 255), (128, 0, 255), (255, 0, 128)] {
            assert_eq!(hsb_to_rgb(hsb_from_rgb(r, g, b)), (r, g, b));
        }
        // Hue 255 is a full turn, i.e. the same as 0.
        assert_eq!(hsb_to_rgb(Hsb::new(255.0, 255.0, 255.0)), (255, 0, 0));
    }
    #[test]
    fn hsv_greys_black_white_and_extremes() {
        for v in [0u8, 1, 17, 128, 254, 255] {
            let c = hsb_from_rgb(v, v, v);
            assert_eq!((c.h, c.s, c.b), (0.0, 0.0, v as f32));
            assert_eq!(hsb_to_rgb(c), (v, v, v));
        }
        // Zero saturation is grey whatever the hue; zero brightness is black whatever hue and saturation.
        assert_eq!(hsb_to_rgb(Hsb::new(100.0, 0.0, 200.0)), (200, 200, 200));
        assert_eq!(hsb_to_rgb(Hsb::new(100.0, 255.0, 0.0)), (0, 0, 0));
        // Near-grey and single-channel extremes.
        for (r, g, b) in [(1, 0, 0), (0, 1, 0), (0, 0, 1), (255, 254, 254), (254, 255, 255), (1, 1, 0)] {
            assert_eq!(hsb_to_rgb(hsb_from_rgb(r, g, b)), (r, g, b));
        }
    }
    #[test]
    fn hsv_round_trips_a_lattice_of_rgb_values() {
        for r in (0..=255u16).step_by(15) {
            for g in (0..=255u16).step_by(15) {
                for b in (0..=255u16).step_by(15) {
                    let (r, g, b) = (r as u8, g as u8, b as u8);
                    assert_eq!(hsb_to_rgb(hsb_from_rgb(r, g, b)), (r, g, b), "{r},{g},{b}");
                }
            }
        }
    }
    #[test]
    fn primaries_round_trip_through_hsb() {
        for (r, g, b) in [(255, 0, 0), (0, 255, 0), (0, 0, 255), (255, 255, 255), (0, 0, 0), (63, 111, 181)] {
            let c = cref_of(r, g, b);
            let back = cref_of_hsb(hsb_of_cref(c));
            let (r2, g2, b2) = rgb_of(back);
            assert!(r.abs_diff(r2) <= 1 && g.abs_diff(g2) <= 1 && b.abs_diff(b2) <= 1, "{c:#08x} -> {back:#08x}");
        }
        assert_eq!(Hsb::new(-5.0, 300.0, 12.0), Hsb { h: 0.0, s: 255.0, b: 12.0 });
    }
    #[test]
    fn escape_restores_exact_opening_not_an_hsb_round_trip() {
        let mut m = Model::new(0xA81237);
        m.set_component(1, 64.3);
        assert_eq!(
            m.finish(true),
            Outcome {
                color: 0xA81237,
                cancelled: true
            }
        );
        assert_eq!(m.finish(false).color, m.color);
    }
    #[test]
    fn gradient_drag_retains_fractional_position_separate_from_rgb() {
        let mut m = Model::new(0xB56F3F);
        m.set_component(0, 103.125);
        assert_eq!(m.hsb.h, 103.125);
        m.set_component(2, 300.);
        assert_eq!(m.hsb.b, 255.);
        m.set_component(1, -5.);
        assert_eq!(m.hsb.s, 0.);
    }
    #[test]
    fn popup_scales_on_destination_monitor_and_stays_in_work_area() {
        for (dpi, extra) in [96, 120, 144, 240].into_iter().flat_map(|d| [(d, 0.), (d, CAPTION_H)]) {
            let work = RECT {
                left: -3840,
                top: -200,
                right: 0,
                bottom: 1960,
            };
            let anchor = RECT {
                left: -50,
                top: 1900,
                right: -18,
                bottom: 1932,
            };
            let r = opening_bounds(anchor, work, dpi, extra);
            assert!(
                r.left >= work.left
                    && r.right <= work.right
                    && r.top >= work.top
                    && r.bottom <= work.bottom
            );
            assert_eq!(r.right - r.left, (WIDTH * dpi as f32 / 96.).round() as i32);
            assert_eq!(r.bottom - r.top, ((HEIGHT + extra) * dpi as f32 / 96.).round() as i32);
        }
    }
    #[test]
    fn fields_and_tracks_fit_all_required_dpi() {
        for (dpi, extra) in [96, 120, 144, 240].into_iter().flat_map(|d| [(d, 0.), (d, CAPTION_H)]) {
            let l = layout(extra);
            let s = dpi as f32 / 96.;
            for r in [
                l.preview,
                l.close,
                l.hex,
                l.eye,
                l.tracks[0],
                l.tracks[1],
                l.tracks[2],
            ] {
                let p = snap_rect(r, dpi);
                assert!(
                    p.left >= 0. && p.top >= 0. && p.right <= WIDTH * s && p.bottom <= (HEIGHT + extra) * s
                );
                assert_eq!(p.left, p.left.round());
            }
            assert_eq!(l.hex.right - l.hex.left, 120.);
        }
    }
    #[test]
    fn popover_goldens() {
        let dir = std::path::Path::new("target/qa/settings-redesign/u3");
        std::fs::create_dir_all(dir).unwrap();
        let m = Model::new(0xB56F3F);
        for light in [false, true] {
            for dpi in [96, 120, 144, 240] {
                for (name, extra, caption) in [
                    ("", 0., None),
                    ("-caption", CAPTION_H, Some(INHERITED_CAPTION)),
                ] {
                    let s = dpi as f32 / 96.;
                    let mut surface = Surface::new(
                        (WIDTH * s).round() as u32,
                        ((HEIGHT + extra) * s).round() as u32,
                    )
                    .unwrap();
                    surface
                        .draw(|ctx| draw(ctx, &m, dpi, light, None, None, false, true, extra, caption))
                        .unwrap();
                    surface
                        .save_png(&dir.join(format!(
                            "color-popover{name}-{}-{dpi}.png",
                            if light { "light" } else { "dark" }
                        )))
                        .unwrap();
                }
            }
        }
    }
}
