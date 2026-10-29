//! Saved desk setups UI, from desk-setups-spec 6. Layout files are read-only here;
//! accepted Forget actions are returned to the settings shell for the engine to execute.

use std::cell::RefCell;
use std::path::Path;

use super::kit::{
    controls::{self, Control, VisualState},
    layout::{contains, snap_rect},
    theme::{Theme, Type},
};
use crate::desk_setup::DeskSetups;
use crate::dialog::{self, DialogSpec};
use crate::render::{Ctx, RectF, Surface};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, EndPaint, InvalidateRect, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetKeyState, IsWindowEnabled, SetFocus,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{Result, w};

pub const EMPTY: &str = "No saved setups yet. Move a Corral to save one.";
const UNKNOWN: &str = "New monitor setup. Move a Corral to save a layout for it.";
const FORGET_TEXT: &str = "Corral stops remembering where Corrals sit for this monitor setup. Corrals and files are not changed.";

thread_local! {
    static STATUS: RefCell<String> = RefCell::new(UNKNOWN.into());
}

pub fn status() -> String {
    STATUS.with(|v| v.borrow().clone())
}

/// The live read-only view carries temporary recovery state that never belongs in a saved setup.
pub fn refresh(path: &Path) -> DeskSetups {
    let live = crate::desk_setup::read_view(path);
    let status = live
        .as_ref()
        .map(DeskSetups::status)
        .unwrap_or_else(|| UNKNOWN.into());
    STATUS.with(|v| *v.borrow_mut() = status);
    live.unwrap_or_else(|| {
        crate::model::load_with_backup(path)
            .map(|l| l.desk_setups)
            .unwrap_or_default()
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub caption: String,
    pub active: bool,
}

pub fn rows(bank: &DeskSetups) -> Vec<Row> {
    bank.visible()
        .into_iter()
        .map(|setup| {
            let active = bank.active.as_ref() == Some(&setup.id);
            let suffix = if active {
                ", this setup"
            } else if setup.origin == "legacy" {
                ", from an earlier version"
            } else {
                ""
            };
            Row {
                id: setup.id.clone(),
                name: setup.name.clone(),
                caption: format!(
                    "{} Corrals · last used {}{}",
                    setup.groups.len(),
                    super::backups::format_taken(setup.last_used_ms.max(0) as u64),
                    suffix
                ),
                active,
            }
        })
        .collect()
}

pub fn forget_spec(name: &str) -> DialogSpec {
    let mut spec = DialogSpec::new(format!("Forget \"{name}\"?"), FORGET_TEXT, "Forget");
    spec.primary.danger = true;
    spec
}

fn wrapped(text: &str, width: f32, ty: Type) -> Vec<String> {
    let (size, _, weight) = ty.metrics();
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let candidate = if line.is_empty() {
            word.to_string()
        } else {
            format!("{line} {word}")
        };
        if crate::render::measure_text(&candidate, super::kit::theme::font_family(), size, weight)
            .map(|m| m.0 <= width)
            .unwrap_or(true)
        {
            line = candidate;
        } else {
            if !line.is_empty() {
                out.push(std::mem::take(&mut line));
            }
            for c in word.chars() {
                let candidate = format!("{line}{c}");
                if !line.is_empty()
                    && crate::render::measure_text(
                        &candidate,
                        super::kit::theme::font_family(),
                        size,
                        weight,
                    )
                    .map(|m| m.0 > width)
                    .unwrap_or(false)
                {
                    out.push(std::mem::take(&mut line));
                }
                line.push(c);
            }
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

#[derive(Clone, Debug)]
struct RowLayout {
    top: f32,
    height: f32,
    title: Vec<String>,
    caption: Vec<String>,
}
fn geometry(rows: &[Row], width: f32) -> Vec<RowLayout> {
    let text_width = (width - 152.).max(20.);
    let mut y = 0.;
    rows.iter()
        .map(|r| {
            let title = wrapped(&r.name, text_width, Type::Body);
            let caption = wrapped(&r.caption, text_width, Type::Caption);
            let height =
                (24. + title.len().max(1) as f32 * 20. + caption.len().max(1) as f32 * 16.)
                    .max(72.);
            let item = RowLayout {
                top: y,
                height,
                title,
                caption,
            };
            y += height;
            item
        })
        .collect()
}
fn content_height(rows: &[RowLayout]) -> f32 {
    rows.last().map_or(68., |r| r.top + r.height)
}
fn viewport(height: f32, width: f32) -> RectF {
    RectF::new(25., 70., width - 25., (height - 90.).max(70.))
}
fn button(width: f32, row: &RowLayout, scroll: f32) -> RectF {
    let top = 70. + row.top - scroll + (row.height - 32.) / 2.;
    RectF::new(width - 69., top, width - 37., top + 32.)
}

/// Shared offscreen and production drawing; every rectangle is in DIPs until snapped once.
fn draw(
    ctx: &Ctx,
    rows: &[Row],
    lay: &[RowLayout],
    width: f32,
    height: f32,
    scroll: f32,
    focus: usize,
    hover: Option<usize>,
    dpi: u32,
    theme: &Theme,
) -> Result<()> {
    dialog::draw_frame(ctx, width, height, height - 74., theme, dpi)?;
    ctx.draw_text_cached(
        "Saved desk setups",
        snap_rect(RectF::new(25., 25., width - 25., 53.), dpi),
        &Type::DialogTitle.style(dpi, theme.text),
    )?;
    let view = viewport(height, width);
    ctx.with_clip(snap_rect(view, dpi), |ctx| {
        ctx.fill_rounded_rect(snap_rect(view, dpi), 8. * dpi as f32 / 96., theme.surface)?;
        ctx.stroke_rounded_rect(
            snap_rect(view, dpi),
            8. * dpi as f32 / 96.,
            dpi as f32 / 96.,
            theme.border,
        )?;
        if rows.is_empty() {
            for (i, text) in wrapped(EMPTY, width - 74., Type::Caption)
                .iter()
                .enumerate()
            {
                ctx.draw_text_cached(
                    text,
                    snap_rect(
                        RectF::new(37., 82. + i as f32 * 16., width - 37., 98. + i as f32 * 16.),
                        dpi,
                    ),
                    &Type::Caption.style(dpi, theme.secondary),
                )?;
            }
        }
        for (i, (row, l)) in rows.iter().zip(lay).enumerate() {
            let y = 70. + l.top - scroll;
            if y + l.height < view.top || y > view.bottom {
                continue;
            }
            let badge = RectF::new(37., y + 16., 69., y + 48.);
            ctx.fill_rounded_rect(
                snap_rect(badge, dpi),
                8. * dpi as f32 / 96.,
                theme.accent_subtle,
            )?;
            let mut glyph = Type::Body.style(dpi, theme.accent);
            glyph.family = super::icon_family().into();
            glyph.size_px = 16. * dpi as f32 / 96.;
            glyph.align = crate::render::Align::Center;
            ctx.draw_text_cached("\u{E7F4}", snap_rect(badge, dpi), &glyph)?;
            let mut ty = y + 12.;
            for text in &l.title {
                ctx.draw_text_cached(
                    text,
                    snap_rect(RectF::new(81., ty, width - 81., ty + 20.), dpi),
                    &Type::Body.style(dpi, theme.text),
                )?;
                ty += 20.;
            }
            for text in &l.caption {
                ctx.draw_text_cached(
                    text,
                    snap_rect(RectF::new(81., ty, width - 81., ty + 16.), dpi),
                    &Type::Caption.style(dpi, theme.secondary),
                )?;
                ty += 16.;
            }
            // desk-setups-spec 6: the active setup can never be forgotten, including via keyboard.
            controls::draw_control(
                ctx,
                Control::IconButton,
                button(width, l, scroll),
                "",
                VisualState {
                    enabled: !row.active,
                    focused: focus == i,
                    hovered: hover == Some(i),
                    ..Default::default()
                },
                theme,
                dpi,
                0.,
            )?;
            let mut forget_glyph = Type::Body.style(dpi, theme.color(theme.text, !row.active));
            forget_glyph.family = super::icon_family();
            forget_glyph.size_px = 16. * dpi as f32 / 96.;
            forget_glyph.align = crate::render::Align::Center;
            ctx.draw_text_cached("\u{E74D}", snap_rect(button(width, l, scroll), dpi), &forget_glyph)?;
            if i + 1 < rows.len() {
                ctx.fill_rect(
                    snap_rect(
                        RectF::new(37., y + l.height - 1., width - 37., y + l.height),
                        dpi,
                    ),
                    theme.border_subtle,
                )?;
            }
        }
        Ok(())
    })?;
    let max = (content_height(lay) - (view.bottom - view.top)).max(0.);
    if max > 0. {
        let track = view.bottom - view.top;
        let h = (track * track / content_height(lay)).max(20.);
        let y = view.top + (track - h) * scroll / max;
        ctx.fill_rounded_rect(
            snap_rect(RectF::new(width - 21., y, width - 18., y + h), dpi),
            2. * dpi as f32 / 96.,
            theme.control_strong,
        )?;
    }
    dialog::draw_footer(
        ctx,
        width,
        height,
        &dialog::Button {
            label: "Done".into(),
            danger: false,
        },
        None,
        None,
        (focus == rows.len()).then_some(1),
        theme,
        dpi,
    )
}

struct State {
    rows: Vec<Row>,
    lay: Vec<RowLayout>,
    width: f32,
    height: f32,
    dpi: u32,
    theme: Theme,
    scroll: f32,
    focus: usize,
    focus_visible: bool,
    pressed: Option<usize>,
    hover: Option<usize>,
    surface: Option<Surface>,
    result: Option<String>,
    tip: HWND,
    tip_text: Vec<u16>,
}
impl State {
    fn scroll_to_focus(&mut self) {
        if let Some(row) = self.lay.get(self.focus) {
            let view = viewport(self.height, self.width);
            let available = view.bottom - view.top;
            if row.top < self.scroll {
                self.scroll = row.top;
            }
            if row.top + row.height > self.scroll + available {
                self.scroll = row.top + row.height - available;
            }
        }
        self.clamp();
    }
    fn clamp(&mut self) {
        let v = viewport(self.height, self.width);
        self.scroll = self
            .scroll
            .clamp(0., (content_height(&self.lay) - (v.bottom - v.top)).max(0.));
    }
    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        if !contains(viewport(self.height, self.width), x, y) {
            return None;
        }
        self.lay
            .iter()
            .position(|r| contains(button(self.width, r, self.scroll), x, y))
    }
}
fn state(hwnd: HWND) -> Option<&'static mut State> {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    unsafe { ptr.as_mut() }
}
fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}
fn activate(hwnd: HWND, index: usize) {
    let Some(st) = state(hwnd) else { return };
    if index == st.rows.len() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
        return;
    }
    let Some(row) = st.rows.get(index).filter(|r| !r.active).cloned() else {
        return;
    };
    let light = st.theme.light;
    if dialog::show(hwnd, light, forget_spec(&row.name)).action == dialog::Action::Primary {
        if let Some(st) = state(hwnd) {
            st.result = Some(row.id);
        }
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let create = unsafe { &*(lp.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
    }
    let Some(st) = state(hwnd) else {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut ps) };
            let width = (st.width * st.dpi as f32 / 96.).round().max(1.) as u32;
            let height = (st.height * st.dpi as f32 / 96.).round().max(1.) as u32;
            if let Ok(mut surface) = Surface::new(width, height) {
                let _ = surface.draw(|ctx| {
                    draw(
                        ctx,
                        &st.rows,
                        &st.lay,
                        st.width,
                        st.height,
                        st.scroll,
                        if st.focus_visible {
                            st.focus
                        } else {
                            usize::MAX
                        },
                        st.hover,
                        st.dpi,
                        &st.theme,
                    )
                });
                unsafe {
                    let _ = BitBlt(
                        dc,
                        0,
                        0,
                        width as i32,
                        height as i32,
                        surface.dc(),
                        0,
                        0,
                        SRCCOPY,
                    );
                }
                st.surface = Some(surface);
            }
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEMOVE | WM_LBUTTONDOWN | WM_LBUTTONUP => {
            let scale = 96. / st.dpi as f32;
            let x = lp.0 as i16 as f32 * scale;
            let y = (lp.0 >> 16) as i16 as f32 * scale;
            st.hover = st.hit(x, y);
            let target = st.hover.or_else(|| {
                contains(
                    dialog::footer_layout(st.width, st.height, false, false).primary,
                    x,
                    y,
                )
                .then_some(st.rows.len())
            });
            if msg == WM_LBUTTONDOWN {
                st.focus_visible = false;
                st.pressed = target;
                if let Some(i) = target.filter(|i| *i == st.rows.len() || !st.rows[*i].active) {
                    st.focus = i;
                }
            } else if msg == WM_LBUTTONUP {
                let pressed = st.pressed.take();
                if pressed == target {
                    if let Some(i) = target {
                        activate(hwnd, i);
                    }
                }
            }
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            st.scroll -= (wp.0 >> 16) as i16 as f32 / 120. * 48.;
            st.clamp();
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            st.focus_visible = true;
            match wp.0 {
                0x1B => unsafe {
                    let _ = DestroyWindow(hwnd);
                },
                0x0D | 0x20 => activate(hwnd, st.focus),
                0x09 | 0x26 | 0x28 => {
                    let reverse =
                        wp.0 == 0x26 || (wp.0 == 0x09 && unsafe { GetKeyState(0x10) } < 0);
                    let mut order: Vec<usize> = st
                        .rows
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| !r.active)
                        .map(|(i, _)| i)
                        .collect();
                    order.push(st.rows.len());
                    let at = order
                        .iter()
                        .position(|i| *i == st.focus)
                        .unwrap_or(order.len() - 1);
                    st.focus =
                        order[(at + if reverse { order.len() - 1 } else { 1 }) % order.len()];
                    st.scroll_to_focus();
                    invalidate(hwnd);
                }
                0x21 | 0x22 => {
                    st.scroll += if wp.0 == 0x21 { -200. } else { 200. };
                    st.clamp();
                    invalidate(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_NOTIFY => {
            let header = unsafe { &*(lp.0 as *const NMHDR) };
            if header.code == TTN_GETDISPINFOW {
                let text = st
                    .hover
                    .map(|i| {
                        if st.rows[i].active {
                            "In use"
                        } else {
                            "Forget this setup"
                        }
                    })
                    .unwrap_or("");
                st.tip_text = text.encode_utf16().chain(Some(0)).collect();
                unsafe {
                    (*(lp.0 as *mut NMTTDISPINFOW)).lpszText =
                        windows::core::PWSTR(st.tip_text.as_mut_ptr());
                }
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            st.dpi = (wp.0 as u32 & 0xffff).max(1);
            let suggested = unsafe { &*(lp.0 as *const RECT) };
            let center = POINT {
                x: (suggested.left + suggested.right) / 2,
                y: (suggested.top + suggested.bottom) / 2,
            };
            let (work, _) = dialog::monitor_at(center);
            st.width = 440f32.min((work.right - work.left) as f32 * 96. / st.dpi as f32);
            st.lay = geometry(&st.rows, st.width);
            st.height = (70. + content_height(&st.lay) + 90.)
                .min(600.)
                .min((work.bottom - work.top) as f32 * 96. / st.dpi as f32);
            let rect = dialog::opening_bounds(work, st.dpi, st.width, st.height, center);
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            st.clamp();
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_NCDESTROY => {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// Returns only a confirmed non-active id; the engine rechecks active state before forgetting.
pub fn show(owner: HWND, light: bool, rows: Vec<Row>) -> Option<String> {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetWindowRect(owner, &mut rect);
    }
    let center = POINT {
        x: (rect.left + rect.right) / 2,
        y: (rect.top + rect.bottom) / 2,
    };
    let (work, dpi) = dialog::monitor_at(center);
    let width = 440f32.min((work.right - work.left) as f32 * 96. / dpi as f32);
    let lay = geometry(&rows, width);
    let height = (70. + content_height(&lay) + 90.)
        .min(600.)
        .min((work.bottom - work.top) as f32 * 96. / dpi as f32);
    let r = dialog::opening_bounds(work, dpi, width, height, center);
    let focus = rows.len();
    let mut st = Box::new(State {
        rows,
        lay,
        width,
        height,
        dpi,
        theme: Theme::new(light),
        scroll: 0.,
        focus,
        focus_visible: false,
        pressed: None,
        hover: None,
        surface: None,
        result: None,
        tip: HWND::default(),
        tip_text: vec![0],
    });
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map(|h| HINSTANCE(h.0))
            .unwrap_or_default();
        RegisterClassW(&WNDCLASSW {
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("CorralSavedDeskSetups"),
            ..Default::default()
        });
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("CorralSavedDeskSetups"),
            w!("Saved desk setups"),
            WS_POPUP | WS_SYSMENU,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            owner,
            None,
            instance,
            Some((&mut *st as *mut State).cast()),
        ) else {
            return None;
        };
        st.tip = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            None,
            WINDOW_STYLE(WS_POPUP.0 | TTS_ALWAYSTIP | TTS_NOPREFIX),
            0,
            0,
            0,
            0,
            hwnd,
            None,
            instance,
            None,
        )
        .unwrap_or_default();
        let info = TTTOOLINFOW {
            cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_IDISHWND | TTF_SUBCLASS,
            hwnd,
            uId: hwnd.0 as usize,
            lpszText: windows::core::PWSTR(-1isize as *mut u16),
            ..Default::default()
        };
        SendMessageW(
            st.tip,
            TTM_ADDTOOLW,
            WPARAM(0),
            LPARAM(&info as *const _ as isize),
        );
        let was_enabled = IsWindowEnabled(owner).as_bool();
        if was_enabled {
            let _ = EnableWindow(owner, false);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
        let mut msg = MSG::default();
        while IsWindow(hwnd).as_bool() {
            let status = GetMessageW(&mut msg, None, 0, 0);
            if status.0 <= 0 {
                if status.0 == 0 {
                    PostQuitMessage(msg.wParam.0 as i32);
                }
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if IsWindow(hwnd).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
        if was_enabled {
            let _ = EnableWindow(owner, true);
            let _ = SetForegroundWindow(owner);
            let _ = SetFocus(owner);
        }
    }
    st.result.take()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn list_is_recent_first_omits_archived_and_labels_active_and_legacy() {
        use crate::desk_setup::DeskSetup;
        let mut bank = DeskSetups::default();
        bank.active = Some("active".into());
        bank.setups = vec![
            DeskSetup {
                id: "old".into(),
                name: "Earlier layout".into(),
                origin: "legacy".into(),
                last_used_ms: 1,
                ..Default::default()
            },
            DeskSetup {
                id: "archived".into(),
                last_used_ms: 100,
                legacy: Some(serde_json::json!({"obsolete":true})),
                ..Default::default()
            },
            DeskSetup {
                id: "active".into(),
                name: "Monitor B + Monitor A".into(),
                origin: "learned".into(),
                last_used_ms: 2,
                ..Default::default()
            },
        ];
        let visible = rows(&bank);
        assert_eq!(
            visible.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["active", "old"]
        );
        assert!(visible[0].active);
        assert!(visible[0].caption.ends_with(", this setup"));
        assert!(!visible[1].active);
        assert!(visible[1].caption.ends_with(", from an earlier version"));
        assert!(!bank.forget("active"));
        assert!(bank.forget("old"));
    }
    #[test]
    fn forget_uses_the_approved_confirmation_without_a_name_field() {
        let spec = forget_spec("Monitor B + Monitor A");
        assert_eq!(spec.title, "Forget \"Monitor B + Monitor A\"?");
        assert_eq!(spec.body, FORGET_TEXT);
        assert!(spec.primary.danger);
        assert_eq!(spec.primary.label, "Forget");
        assert_eq!(spec.secondary.as_deref(), Some("Cancel"));
        assert!(spec.field.is_none());
    }
    #[test]
    fn list_wraps_and_keyboard_scroll_reaches_last_row_at_all_supported_dpis() -> Result<()> {
        let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/qa/c2-ui");
        std::fs::create_dir_all(&output).unwrap();
        let rows = (0..16)
            .map(|i| Row {
                id: i.to_string(),
                name: "Monitor B 2K + Monitor A 4K + a long automatic display name".into(),
                caption: "42 Corrals · last used 2026-10-20 12:30, from an earlier version".into(),
                active: i == 0,
            })
            .collect::<Vec<_>>();
        let lay = geometry(&rows, 440.);
        assert!(lay[0].title.len() > 1);
        for light in [true, false] {
            for dpi in [96, 120, 144, 240] {
                let mut st = State {
                    rows: rows.clone(),
                    lay: lay.clone(),
                    width: 440.,
                    height: 600.,
                    dpi,
                    theme: Theme::new(light),
                    scroll: 0.,
                    focus: 15,
                    focus_visible: true,
                    pressed: None,
                    hover: None,
                    surface: None,
                    result: None,
                    tip: HWND::default(),
                    tip_text: vec![0],
                };
                st.scroll_to_focus();
                let b = button(st.width, &st.lay[15], st.scroll);
                assert!(b.top >= 70. && b.bottom <= 510.);
                assert_eq!(
                    st.hit((b.left + b.right) / 2., (b.top + b.bottom) / 2.),
                    Some(15)
                );
                let mut surface = Surface::new(
                    (440. * dpi as f32 / 96.) as u32,
                    (600. * dpi as f32 / 96.) as u32,
                )?;
                surface.draw(|ctx| {
                    draw(
                        ctx, &st.rows, &st.lay, 440., 600., st.scroll, st.focus, None, dpi,
                        &st.theme,
                    )
                })?;
                surface.save_png(&output.join(format!(
                    "setups-{}-{dpi}-scrolled.png",
                    if light { "light" } else { "dark" }
                )))?;
                surface.draw(|ctx| {
                    draw(
                        ctx,
                        &st.rows,
                        &st.lay,
                        440.,
                        600.,
                        0.,
                        usize::MAX,
                        None,
                        dpi,
                        &st.theme,
                    )
                })?;
                surface.save_png(&output.join(format!(
                    "setups-{}-{dpi}-top.png",
                    if light { "light" } else { "dark" }
                )))?;
            }
        }
        Ok(())
    }
}
