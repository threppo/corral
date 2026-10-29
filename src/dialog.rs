//! Shared Corral modal dialogs. Geometry and drawing are shared by production and offscreen QA.
//! No settings, snapshot or desktop operations are performed here.
#![allow(dead_code)]

use crate::render::{self, Ctx, RectF, Surface};
use crate::settings_ui::kit::{
    controls::{self, ButtonStyle, Control, VisualState},
    theme::{self, Theme, Type},
};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint, GetMonitorInfoW,
    HBRUSH, HDC, HFONT, InvalidateRect, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    PAINTSTRUCT, SRCCOPY, SetBkColor, SetTextColor,
};
use windows::Win32::UI::Controls::EM_SETSEL;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetKeyState, IsWindowEnabled, SetFocus, TME_LEAVE, TRACKMOUSEEVENT,
    TrackMouseEvent,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, Result, w};

#[derive(Clone, Debug)]
pub struct Checkbox {
    pub label: String,
    pub checked: bool,
}
impl Checkbox {
    pub fn new(label: impl Into<String>, checked: bool) -> Self {
        Self {
            label: label.into(),
            checked,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Field {
    pub label: String,
    pub value: String,
    pub hint: String,
    pub required: bool,
    pub max_chars: usize,
}
#[derive(Clone, Debug)]
pub struct Button {
    pub label: String,
    pub danger: bool,
}
#[derive(Clone, Debug)]
pub struct DialogSpec {
    pub title: String,
    pub body: String,
    pub width: f32,
    pub checks: Vec<Checkbox>,
    pub field: Option<Field>,
    pub primary: Button,
    pub secondary: Option<String>,
    pub left_action: Option<String>,
    /// Notices fill the footer; live choosers can use a compact action at the right.
    pub single_button_full_width: bool,
    /// Optional list card under the body (the Round-up preview): headings, items, captions.
    pub rows: Vec<DialogRow>,
}
/// How a row of the list card is set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Heading,
    Item,
    Caption,
}
impl RowKind {
    fn ty(self) -> Type {
        match self {
            Self::Heading => Type::BodyStrong,
            Self::Item => Type::Body,
            Self::Caption => Type::Caption,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogRow {
    pub text: String,
    pub kind: RowKind,
}
impl DialogRow {
    pub fn new(kind: RowKind, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }
}
impl DialogSpec {
    pub fn new(
        title: impl Into<String>,
        body: impl Into<String>,
        primary: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            width: 440.,
            checks: vec![],
            field: None,
            primary: Button {
                label: primary.into(),
                danger: false,
            },
            secondary: Some("Cancel".into()),
            left_action: None,
            single_button_full_width: true,
            rows: Vec::new(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Primary,
    Secondary,
    Left,
    Closed,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DialogResult {
    pub action: Action,
    pub checks: Vec<bool>,
    pub text: String,
}

/// Validation counts Unicode scalar values, matching the persisted name rule.
pub fn validation(field: &Field, value: &str) -> Option<&'static str> {
    if field.required && value.trim().is_empty() {
        Some("Enter a name.")
    } else if field.max_chars > 0 && value.chars().count() > field.max_chars {
        Some("Use 255 characters or fewer.")
    } else {
        None
    }
}

fn wrap(text: &str, width: f32, ty: Type) -> Vec<String> {
    let (size, _, weight) = ty.metrics();
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if render::measure_text(&candidate, theme::font_family(), size, weight)
                .map(|m| m.0 <= width)
                .unwrap_or(true)
            {
                line = candidate;
            } else {
                if !line.is_empty() {
                    lines.push(std::mem::take(&mut line));
                }
                // Very long unbroken paths/names must wrap rather than disappear behind ellipsis.
                for ch in word.chars() {
                    let candidate = format!("{line}{ch}");
                    if !line.is_empty()
                        && render::measure_text(&candidate, theme::font_family(), size, weight)
                            .map(|m| m.0 > width)
                            .unwrap_or(false)
                    {
                        lines.push(std::mem::take(&mut line));
                    }
                    line.push(ch);
                }
            }
        }
        lines.push(line);
    }
    lines
}

#[derive(Clone, Debug)]
pub struct DialogLayout {
    pub width: f32,
    pub height: f32,
    pub title: RectF,
    pub body: RectF,
    /// The list card and its rows (empty without `DialogSpec::rows`).
    pub list: Option<RectF>,
    pub rows: Vec<RectF>,
    pub checks: Vec<RectF>,
    pub field_label: Option<RectF>,
    pub field: Option<RectF>,
    pub hint: Option<RectF>,
    pub footer_top: f32,
}
#[derive(Clone, Copy, Debug)]
pub struct FooterLayout {
    pub primary: RectF,
    pub secondary: Option<RectF>,
    pub left: Option<RectF>,
}
const LIST_PAD: f32 = 16.;
/// All public layout coordinates are DIPs; drawing snaps them once at its API boundary.
pub fn layout(spec: &DialogSpec, width: f32) -> DialogLayout {
    let width = width.max(160.);
    let inner = (width - 50.).max(1.);
    let title_h = wrap(&spec.title, inner, Type::DialogTitle).len() as f32 * 28.;
    let title = RectF::new(25., 25., width - 25., 25. + title_h);
    let mut y = title.bottom;
    let body = if spec.body.is_empty() {
        RectF::new(25., y, width - 25., y)
    } else {
        y += 12.;
        let top = y;
        y += wrap(&spec.body, inner, Type::Body).len() as f32 * 20.;
        RectF::new(25., top, width - 25., y)
    };
    let (mut list, mut rows) = (None, Vec::new());
    if !spec.rows.is_empty() {
        y += 12.;
        let top = y;
        y += LIST_PAD;
        let mut previous: Option<RowKind> = None;
        for row in &spec.rows {
            y += match (previous, row.kind) {
                (None, _) => 0.,
                (Some(_), RowKind::Heading) => 14.,
                (Some(RowKind::Caption), RowKind::Item) => 8.,
                (Some(RowKind::Item), RowKind::Item) => 8.,
                _ => 0.,
            };
            let line = row.kind.ty().metrics().1;
            let lines = wrap(&row.text, inner - 2. * LIST_PAD, row.kind.ty()).len() as f32;
            rows.push(RectF::new(25. + LIST_PAD, y, width - 25. - LIST_PAD, y + lines * line));
            y += lines * line;
            previous = Some(row.kind);
        }
        y += LIST_PAD;
        list = Some(RectF::new(25., top, width - 25., y));
    }
    let mut checks = Vec::new();
    for (index, check) in spec.checks.iter().enumerate() {
        y += if index == 0 { 16. } else { 12. };
        let top = y;
        y += (wrap(&check.label, inner - 30., Type::Body).len() as f32 * 20.).max(20.);
        checks.push(RectF::new(25., top, width - 25., y));
    }
    let (mut field_label, mut field, mut hint) = (None, None, None);
    if let Some(f) = &spec.field {
        y += 12.;
        if !f.label.is_empty() {
            field_label = Some(RectF::new(25., y, width - 25., y + 20.));
            y += 28.;
        }
        field = Some(RectF::new(25., y, width - 25., y + 32.));
        y += 44.;
        let h = (wrap(&f.hint, inner, Type::Caption).len() as f32 * 16.).max(16.);
        hint = Some(RectF::new(25., y, width - 25., y + h));
        y += h;
    }
    let footer_top = y + 24.;
    DialogLayout {
        width,
        height: footer_top + 74.,
        title,
        body,
        list,
        rows,
        checks,
        field_label,
        field,
        hint,
        footer_top,
    }
}
pub fn footer_layout(width: f32, height: f32, secondary: bool, left: bool) -> FooterLayout {
    footer_layout_with_width(width, height, secondary, left, None)
}
fn footer_layout_with_width(
    width: f32,
    height: f32,
    secondary: bool,
    left: bool,
    single_width: Option<f32>,
) -> FooterLayout {
    let y = height - 53.;
    let button_w = if left {
        100.
    } else if secondary {
        ((width - 58.) / 2.).max(1.)
    } else {
        single_width
            .unwrap_or(width - 50.)
            .min((width - 50.).max(1.))
    };
    let primary = RectF::new(width - 25. - button_w, y, width - 25., y + 32.);
    let secondary =
        secondary.then(|| RectF::new(primary.left - 8. - button_w, y, primary.left - 8., y + 32.));
    let left = left.then(|| {
        RectF::new(
            25.,
            y,
            secondary.map_or(primary.left, |r| r.left) - 8.,
            y + 32.,
        )
    });
    FooterLayout {
        primary,
        secondary,
        left,
    }
}
fn single_button_width(spec: &DialogSpec) -> Option<f32> {
    (!spec.single_button_full_width && spec.secondary.is_none() && spec.left_action.is_none()).then(
        || {
            render::measure_text(&spec.primary.label, theme::font_family(), 14., 400)
                .map(|m| (m.0 + 24.).max(80.))
                .unwrap_or(80.)
        },
    )
}
pub fn footer_for_spec(spec: &DialogSpec, width: f32, height: f32) -> FooterLayout {
    footer_layout_with_width(
        width,
        height,
        spec.secondary.is_some(),
        spec.left_action.is_some(),
        single_button_width(spec),
    )
}
fn px(r: RectF, dpi: u32) -> RectF {
    crate::settings_ui::kit::layout::snap_rect(r, dpi)
}
fn translate(r: RectF, dy: f32) -> RectF {
    RectF::new(r.left, r.top + dy, r.right, r.bottom + dy)
}
fn contains(r: RectF, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}
fn lines(ctx: &Ctx, text: &str, r: RectF, ty: Type, color: render::Rgba, dpi: u32) -> Result<()> {
    let line_h = ty.metrics().1;
    for (i, line) in wrap(text, r.right - r.left, ty).iter().enumerate() {
        ctx.draw_text_cached(
            line,
            px(
                RectF::new(
                    r.left,
                    r.top + i as f32 * line_h,
                    r.right,
                    r.top + (i + 1) as f32 * line_h,
                ),
                dpi,
            ),
            &ty.style(dpi, color),
        )?;
    }
    Ok(())
}
/// Shared frame for complex dialog content, including the rule editor.
pub fn draw_frame(
    ctx: &Ctx,
    width: f32,
    height: f32,
    footer_top: f32,
    theme: &Theme,
    dpi: u32,
) -> Result<()> {
    let s = dpi as f32 / 96.;
    ctx.clear(render::Rgba::TRANSPARENT);
    ctx.fill_rounded_rect(
        px(RectF::new(0., 0., width, height), dpi),
        8. * s,
        theme.surface,
    )?;
    ctx.fill_rounded_rect(
        px(RectF::new(0., footer_top, width, height), dpi),
        8. * s,
        theme.surface_raised,
    )?;
    ctx.fill_rect(
        px(
            RectF::new(0., footer_top, width, (height - 8.).max(footer_top)),
            dpi,
        ),
        theme.surface_raised,
    )?;
    ctx.fill_rect(
        px(RectF::new(0., footer_top, width, footer_top + 1.), dpi),
        theme.border,
    )?;
    ctx.stroke_rounded_rect(
        px(RectF::new(0.5, 0.5, width - 0.5, height - 0.5), dpi),
        8. * s,
        s,
        theme.border,
    )
}
/// Footer geometry matches `footer_layout`; focus indices are secondary=0, primary=1, left=2.
pub fn draw_footer(
    ctx: &Ctx,
    width: f32,
    height: f32,
    primary: &Button,
    secondary: Option<&str>,
    left: Option<&str>,
    focused: Option<usize>,
    theme: &Theme,
    dpi: u32,
) -> Result<()> {
    draw_footer_state(
        ctx, width, height, primary, secondary, left, focused, None, None, theme, dpi,
    )
}
fn draw_footer_state(
    ctx: &Ctx,
    width: f32,
    height: f32,
    primary: &Button,
    secondary: Option<&str>,
    left: Option<&str>,
    focused: Option<usize>,
    hovered: Option<usize>,
    single_width: Option<f32>,
    theme: &Theme,
    dpi: u32,
) -> Result<()> {
    let f = footer_layout_with_width(
        width,
        height,
        secondary.is_some(),
        left.is_some(),
        single_width,
    );
    for (r, label, style, index) in [
        Some((
            f.primary,
            primary.label.as_str(),
            if primary.danger {
                ButtonStyle::Danger
            } else {
                ButtonStyle::Primary
            },
            1,
        )),
        f.secondary
            .zip(secondary)
            .map(|(r, t)| (r, t, ButtonStyle::Secondary, 0)),
        f.left
            .zip(left)
            .map(|(r, t)| (r, t, ButtonStyle::Subtle, 2)),
    ]
    .into_iter()
    .flatten()
    {
        controls::draw_control(
            ctx,
            Control::Button(style),
            r,
            label,
            VisualState {
                focused: focused == Some(index),
                hovered: hovered == Some(index),
                ..Default::default()
            },
            theme,
            dpi,
            0.,
        )?;
    }
    Ok(())
}
/// Render a dialog without creating a window. Used by adapter goldens and production painting.
pub fn draw_spec(ctx: &Ctx, spec: &DialogSpec, dpi: u32, light: bool) -> Result<()> {
    let l = layout(spec, spec.width);
    draw_contents(
        ctx,
        spec,
        &l,
        l.height,
        0.,
        None,
        false,
        None,
        None,
        dpi,
        &Theme::new(light),
        true,
    )
}
/// Preview the same inline validation and focus state used by the modal renderer.
pub fn draw_spec_state(
    ctx: &Ctx,
    spec: &DialogSpec,
    dpi: u32,
    light: bool,
    error: Option<&str>,
    focus: Option<Focus>,
    focus_visible: bool,
) -> Result<()> {
    let l = layout(spec, spec.width);
    draw_contents(
        ctx,
        spec,
        &l,
        l.height,
        0.,
        focus,
        focus_visible,
        None,
        error,
        dpi,
        &Theme::new(light),
        true,
    )
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Field,
    Check(usize),
    Secondary,
    Primary,
    Left,
}
/// Destructive confirmations never begin on their destructive action.
pub fn initial_focus(spec: &DialogSpec) -> Focus {
    if spec.field.is_some() {
        Focus::Field
    } else if spec.primary.danger && spec.secondary.is_some() {
        Focus::Secondary
    } else {
        Focus::Primary
    }
}
pub fn focus_order(spec: &DialogSpec) -> Vec<Focus> {
    let mut v = Vec::new();
    for i in 0..spec.checks.len() {
        v.push(Focus::Check(i));
    }
    if spec.field.is_some() {
        v.push(Focus::Field);
    }
    if spec.left_action.is_some() {
        v.push(Focus::Left);
    }
    if spec.secondary.is_some() {
        v.push(Focus::Secondary);
    }
    v.push(Focus::Primary);
    v
}
pub fn advance_focus(spec: &DialogSpec, current: Focus, reverse: bool) -> Focus {
    let order = focus_order(spec);
    let i = order.iter().position(|v| *v == current).unwrap_or(0);
    order[(i + order.len() + if reverse { order.len() - 1 } else { 1 }) % order.len()]
}
fn draw_contents(
    ctx: &Ctx,
    spec: &DialogSpec,
    l: &DialogLayout,
    height: f32,
    scroll: f32,
    focus: Option<Focus>,
    focus_visible: bool,
    hover: Option<Focus>,
    error: Option<&str>,
    dpi: u32,
    theme: &Theme,
    paint_edit: bool,
) -> Result<()> {
    let footer_top = height - 74.;
    draw_frame(ctx, l.width, height, footer_top, theme, dpi)?;
    ctx.with_clip(
        px(RectF::new(1., 1., l.width - 1., footer_top), dpi),
        |ctx| {
            lines(
                ctx,
                &spec.title,
                translate(l.title, -scroll),
                Type::DialogTitle,
                theme.text,
                dpi,
            )?;
            lines(
                ctx,
                &spec.body,
                translate(l.body, -scroll),
                Type::Body,
                theme.text,
                dpi,
            )?;
            if let Some(card) = l.list {
                let s = dpi as f32 / 96.;
                let card = px(translate(card, -scroll), dpi);
                ctx.fill_rounded_rect(card, 8. * s, theme.surface_raised)?;
                ctx.stroke_rounded_rect(card, 8. * s, s, theme.border_subtle)?;
                for (row, r) in spec.rows.iter().zip(&l.rows) {
                    let color = if row.kind == RowKind::Caption {
                        theme.secondary
                    } else {
                        theme.text
                    };
                    lines(ctx, &row.text, translate(*r, -scroll), row.kind.ty(), color, dpi)?;
                }
            }
            for (i, (check, r)) in spec.checks.iter().zip(&l.checks).enumerate() {
                let r = translate(*r, -scroll);
                controls::draw_control(
                    ctx,
                    Control::Checkbox,
                    RectF::new(r.left, r.top, r.left + 20., r.top + 20.),
                    "",
                    VisualState {
                        selected: check.checked,
                        focused: focus_visible && focus == Some(Focus::Check(i)),
                        hovered: hover == Some(Focus::Check(i)),
                        ..Default::default()
                    },
                    theme,
                    dpi,
                    0.,
                )?;
                lines(
                    ctx,
                    &check.label,
                    RectF::new(r.left + 30., r.top, r.right, r.bottom),
                    Type::Body,
                    theme.text,
                    dpi,
                )?;
            }
            if let Some(field) = &spec.field {
                if let Some(r) = l.field_label {
                    lines(
                        ctx,
                        &field.label,
                        translate(r, -scroll),
                        Type::Body,
                        theme.text,
                        dpi,
                    )?;
                }
                if let Some(r) = l.field {
                    let r = translate(r, -scroll);
                    let p = px(r, dpi);
                    let s = dpi as f32 / 96.;
                    ctx.fill_rounded_rect(p, 4. * s, theme.control)?;
                    ctx.stroke_rounded_rect(p, 4. * s, s, theme.border)?;
                    if focus == Some(Focus::Field) {
                        ctx.fill_rect(
                            px(
                                RectF::new(r.left + 1., r.bottom - 2., r.right - 1., r.bottom),
                                dpi,
                            ),
                            theme.accent,
                        )?;
                    }
                    if paint_edit {
                        ctx.draw_text_cached(
                            &field.value,
                            px(
                                RectF::new(r.left + 10., r.top + 6., r.right - 10., r.top + 26.),
                                dpi,
                            ),
                            &Type::Body.style(dpi, theme.text),
                        )?;
                    }
                }
                if let Some(r) = l.hint {
                    lines(
                        ctx,
                        error.unwrap_or(&field.hint),
                        translate(r, -scroll),
                        Type::Caption,
                        if error.is_some() {
                            theme.danger
                        } else {
                            theme.secondary
                        },
                        dpi,
                    )?;
                }
            }
            Ok(())
        },
    )?;
    let footer_focus = if focus_visible {
        match focus {
            Some(Focus::Secondary) => Some(0),
            Some(Focus::Primary) => Some(1),
            Some(Focus::Left) => Some(2),
            _ => None,
        }
    } else {
        None
    };
    draw_footer_state(
        ctx,
        l.width,
        height,
        &spec.primary,
        spec.secondary.as_deref(),
        spec.left_action.as_deref(),
        footer_focus,
        hover.and_then(|h| match h {
            Focus::Secondary => Some(0),
            Focus::Primary => Some(1),
            Focus::Left => Some(2),
            _ => None,
        }),
        single_button_width(spec),
        theme,
        dpi,
    )
}

struct Runtime<'a> {
    spec: DialogSpec,
    result: Option<DialogResult>,
    dpi: u32,
    theme: Theme,
    layout: DialogLayout,
    width: f32,
    height: f32,
    scroll: f32,
    focus: Focus,
    visible: bool,
    hover: Option<Focus>,
    error: Option<&'static str>,
    surface: Option<Surface>,
    edit: HWND,
    font: HFONT,
    brush: HBRUSH,
    callback: Option<&'a mut dyn FnMut(usize, bool)>,
    /// Rebuilds the whole spec when a checkbox changes (the Round-up preview).
    refresh: Option<&'a mut dyn FnMut(&[bool]) -> DialogSpec>,
}
fn state(hwnd: HWND) -> Option<&'static mut Runtime<'static>> {
    // The synchronous modal loop owns this allocation until WM_NCDESTROY has cleared the pointer.
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut Runtime<'static>;
    unsafe { p.as_mut() }
}
fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}
fn read_edit(edit: HWND) -> String {
    unsafe {
        let mut buf = vec![0u16; GetWindowTextLengthW(edit).max(0) as usize + 1];
        let n = GetWindowTextW(edit, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }
}
fn remember_text(st: &mut Runtime<'_>) {
    if let Some(f) = &mut st.spec.field {
        if !st.edit.is_invalid() {
            f.value = read_edit(st.edit);
        }
    }
}
fn finish(hwnd: HWND, action: Action) {
    let Some(st) = state(hwnd) else { return };
    remember_text(st);
    if action == Action::Primary {
        if let Some(f) = &st.spec.field {
            st.error = validation(f, &f.value);
            if st.error.is_some() {
                focus_to(hwnd, Focus::Field, true);
                return;
            }
        }
    }
    st.result = Some(DialogResult {
        action,
        checks: st.spec.checks.iter().map(|c| c.checked).collect(),
        text: st
            .spec
            .field
            .as_ref()
            .map(|f| f.value.clone())
            .unwrap_or_default(),
    });
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}
pub fn hit(
    spec: &DialogSpec,
    l: &DialogLayout,
    height: f32,
    scroll: f32,
    x: f32,
    y: f32,
) -> Option<Focus> {
    let f = footer_for_spec(spec, l.width, height);
    if contains(f.primary, x, y) {
        return Some(Focus::Primary);
    }
    if f.secondary.is_some_and(|r| contains(r, x, y)) {
        return Some(Focus::Secondary);
    }
    if f.left.is_some_and(|r| contains(r, x, y)) {
        return Some(Focus::Left);
    }
    if y >= height - 74. {
        return None;
    }
    if let Some(i) = l
        .checks
        .iter()
        .position(|r| contains(translate(*r, -scroll), x, y))
    {
        return Some(Focus::Check(i));
    }
    l.field
        .filter(|r| contains(translate(*r, -scroll), x, y))
        .map(|_| Focus::Field)
}
fn max_scroll(st: &Runtime<'_>) -> f32 {
    (st.layout.footer_top - (st.height - 74.)).max(0.)
}
fn position_edit(hwnd: HWND) {
    let Some(st) = state(hwnd) else { return };
    if !st.edit.is_invalid() {
        if let Some(r) = st.layout.field {
            let r = translate(r, -st.scroll);
            let r = px(
                RectF::new(r.left + 10., r.top + 6., r.right - 10., r.bottom - 6.),
                st.dpi,
            );
            unsafe {
                let _ = SetWindowPos(
                    st.edit,
                    None,
                    r.left as i32,
                    r.top as i32,
                    (r.right - r.left) as i32,
                    (r.bottom - r.top) as i32,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
                let _ = ShowWindow(
                    st.edit,
                    if r.top >= 0. && r.bottom < ((st.height - 74.) * st.dpi as f32 / 96.) {
                        SW_SHOW
                    } else {
                        SW_HIDE
                    },
                );
            }
        }
    }
}
fn focus_to(hwnd: HWND, focus: Focus, visible: bool) {
    let Some(st) = state(hwnd) else { return };
    st.focus = focus;
    st.visible = visible;
    let bounds = match focus {
        Focus::Field => st.layout.field.map(|r| {
            // An inline error belongs to the field's focus target, including when the work area
            // forces this dialog to scroll. Keep the correction and its explanation together.
            if st.error.is_some() {
                RectF::new(
                    r.left,
                    r.top,
                    r.right,
                    st.layout.hint.map_or(r.bottom, |h| h.bottom),
                )
            } else {
                r
            }
        }),
        Focus::Check(i) => st.layout.checks.get(i).copied(),
        _ => None,
    };
    if let Some(r) = bounds {
        let viewport = st.height - 74.;
        if r.top - st.scroll < 16. {
            st.scroll = (r.top - 16.).max(0.);
        } else if r.bottom - st.scroll > viewport - 16. {
            st.scroll = (r.bottom - viewport + 16.).min(max_scroll(st));
        }
    }
    unsafe {
        let _ = SetFocus(if focus == Focus::Field && !st.edit.is_invalid() {
            st.edit
        } else {
            hwnd
        });
    }
    position_edit(hwnd);
    invalidate(hwnd);
}
fn toggle(hwnd: HWND, index: usize) {
    let Some(st) = state(hwnd) else { return };
    if let Some(c) = st.spec.checks.get_mut(index) {
        c.checked = !c.checked;
        if let Some(cb) = &mut st.callback {
            cb(index, c.checked);
        }
    }
    if st.refresh.is_some() {
        rebuild_spec(hwnd);
    }
    invalidate(hwnd);
}
/// Ask the refresh callback for the next spec and make the window fit it.
fn rebuild_spec(hwnd: HWND) {
    let Some(st) = state(hwnd) else { return };
    let checks: Vec<bool> = st.spec.checks.iter().map(|c| c.checked).collect();
    let Some(next) = st.refresh.as_mut().map(|refresh| refresh(&checks)) else {
        return;
    };
    st.spec = next;
    if !focus_order(&st.spec).contains(&st.focus) {
        st.focus = Focus::Primary;
    }
    let l = layout(&st.spec, st.width);
    let mut r = RECT::default();
    unsafe {
        let _ = GetWindowRect(hwnd, &mut r);
    }
    let center = POINT {
        x: (r.left + r.right) / 2,
        y: (r.top + r.bottom) / 2,
    };
    let (work, _) = monitor_at(center);
    let bounds = opening_bounds(work, st.dpi, l.width, l.height, center);
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            bounds.left,
            bounds.top,
            bounds.right - bounds.left,
            bounds.bottom - bounds.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
    resize(hwnd);
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Focus(Focus),
    Activate(Action),
    Toggle(usize),
    Ignore,
}
pub fn keyboard_action(spec: &DialogSpec, focus: Focus, key: usize, reverse: bool) -> KeyAction {
    match key {
        9 => KeyAction::Focus(advance_focus(spec, focus, reverse)),
        27 => KeyAction::Activate(if spec.secondary.is_some() {
            Action::Secondary
        } else {
            Action::Closed
        }),
        13 => KeyAction::Activate(if spec.primary.danger && focus != Focus::Primary {
            if spec.secondary.is_some() {
                Action::Secondary
            } else {
                Action::Closed
            }
        } else {
            Action::Primary
        }),
        32 => match focus {
            Focus::Check(i) => KeyAction::Toggle(i),
            Focus::Primary => KeyAction::Activate(Action::Primary),
            Focus::Secondary => KeyAction::Activate(Action::Secondary),
            Focus::Left => KeyAction::Activate(Action::Left),
            Focus::Field => KeyAction::Ignore,
        },
        _ => KeyAction::Ignore,
    }
}
fn key(hwnd: HWND, key: usize) -> bool {
    let Some(st) = state(hwnd) else { return false };
    let action = keyboard_action(&st.spec, st.focus, key, unsafe { GetKeyState(0x10) } < 0);
    match action {
        KeyAction::Focus(f) => focus_to(hwnd, f, true),
        KeyAction::Activate(a) => finish(hwnd, a),
        KeyAction::Toggle(i) => {
            st.visible = true;
            toggle(hwnd, i);
        }
        KeyAction::Ignore => return false,
    }
    true
}

fn recolor(st: &mut Runtime<'_>) {
    unsafe {
        if !st.brush.is_invalid() {
            let _ = DeleteObject(st.brush);
        }
        let c = st.theme.control;
        st.brush = CreateSolidBrush(COLORREF(
            c.r as u32 | (c.g as u32) << 8 | (c.b as u32) << 16,
        ));
    }
}
fn font(st: &mut Runtime<'_>) {
    unsafe {
        if !st.font.is_invalid() {
            let _ = DeleteObject(st.font);
        }
        st.font = CreateFontW(
            -(14. * st.dpi as f32 / 96.).round() as i32,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            &HSTRING::from(theme::font_family()),
        );
        if !st.edit.is_invalid() {
            SendMessageW(st.edit, WM_SETFONT, WPARAM(st.font.0 as usize), LPARAM(1));
        }
    }
}
/// Window bounds in screen device pixels. Destination monitor DPI is supplied before creation.
pub fn opening_bounds(work: RECT, dpi: u32, width: f32, height: f32, center: POINT) -> RECT {
    let s = dpi as f32 / 96.;
    let w = (width * s).round() as i32;
    let h = (height * s).round() as i32;
    let w = w.min((work.right - work.left).max(1));
    let h = h.min((work.bottom - work.top).max(1));
    let x = (center.x - w / 2).clamp(work.left, work.right - w);
    let y = (center.y - h / 2).clamp(work.top, work.bottom - h);
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}
pub fn monitor_at(point: POINT) -> (RECT, u32) {
    unsafe {
        let m = MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let _ = GetMonitorInfoW(m, &mut info);
        let (mut x, mut y) = (96, 96);
        let _ = GetDpiForMonitor(m, MDT_EFFECTIVE_DPI, &mut x, &mut y);
        (info.rcWork, x.max(96))
    }
}
fn resize(hwnd: HWND) {
    let Some(st) = state(hwnd) else { return };
    let mut r = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut r);
    }
    let scale = st.dpi as f32 / 96.;
    st.width = (r.right - r.left).max(1) as f32 / scale;
    st.height = (r.bottom - r.top).max(1) as f32 / scale;
    st.layout = layout(&st.spec, st.width);
    st.scroll = st.scroll.min(max_scroll(st));
    st.surface = None;
    position_edit(hwnd);
    invalidate(hwnd);
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        let cs = unsafe { &*(lp.0 as *const CREATESTRUCTW) };
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        }
    }
    match msg {
        WM_PAINT => {
            if let Some(st) = state(hwnd) {
                let mut ps = PAINTSTRUCT::default();
                let dc = unsafe { BeginPaint(hwnd, &mut ps) };
                let s = st.dpi as f32 / 96.;
                let (w, h) = (
                    (st.width * s).round() as u32,
                    (st.height * s).round() as u32,
                );
                if st.surface.is_none() {
                    st.surface = Surface::new(w, h).ok();
                }
                if let Some(surface) = &mut st.surface {
                    let _ = surface.draw(|ctx| {
                        draw_contents(
                            ctx,
                            &st.spec,
                            &st.layout,
                            st.height,
                            st.scroll,
                            Some(st.focus),
                            st.visible,
                            st.hover,
                            st.error,
                            st.dpi,
                            &st.theme,
                            false,
                        )
                    });
                    unsafe {
                        let _ = BitBlt(dc, 0, 0, w as i32, h as i32, surface.dc(), 0, 0, SRCCOPY);
                    }
                }
                unsafe {
                    let _ = EndPaint(hwnd, &ps);
                }
            }
            return LRESULT(0);
        }
        WM_NCHITTEST => {
            if let Some(st) = state(hwnd) {
                let mut r = RECT::default();
                unsafe {
                    let _ = GetWindowRect(hwnd, &mut r);
                }
                let scale = st.dpi as f32 / 96.;
                let x = ((lp.0 & 0xffff) as i16 as i32 - r.left) as f32 / scale;
                let y = (((lp.0 >> 16) & 0xffff) as i16 as i32 - r.top) as f32 / scale;
                if contains(translate(st.layout.title, -st.scroll), x, y) && y < st.height - 74. {
                    return LRESULT(HTCAPTION as isize);
                }
            }
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_SIZE => {
            resize(hwnd);
            return LRESULT(0);
        }
        WM_CLOSE => {
            finish(hwnd, Action::Closed);
            return LRESULT(0);
        }
        WM_NCDESTROY => unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        },
        WM_KEYDOWN => {
            if key(hwnd, wp.0) {
                return LRESULT(0);
            }
        }
        WM_LBUTTONDOWN => {
            if let Some(st) = state(hwnd) {
                let s = st.dpi as f32 / 96.;
                let x = (lp.0 & 0xffff) as i16 as f32 / s;
                let y = ((lp.0 >> 16) & 0xffff) as i16 as f32 / s;
                let f = footer_for_spec(&st.spec, st.width, st.height);
                if contains(f.primary, x, y) {
                    focus_to(hwnd, Focus::Primary, false);
                    finish(hwnd, Action::Primary);
                } else if f.secondary.is_some_and(|r| contains(r, x, y)) {
                    focus_to(hwnd, Focus::Secondary, false);
                    finish(hwnd, Action::Secondary);
                } else if f.left.is_some_and(|r| contains(r, x, y)) {
                    focus_to(hwnd, Focus::Left, false);
                    finish(hwnd, Action::Left);
                } else if y < st.height - 74. {
                    if let Some(i) = st
                        .layout
                        .checks
                        .iter()
                        .position(|r| contains(translate(*r, -st.scroll), x, y))
                    {
                        focus_to(hwnd, Focus::Check(i), false);
                        toggle(hwnd, i);
                    } else if st
                        .layout
                        .field
                        .is_some_and(|r| contains(translate(r, -st.scroll), x, y))
                    {
                        focus_to(hwnd, Focus::Field, false);
                    }
                }
            }
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            if let Some(st) = state(hwnd) {
                let scale = st.dpi as f32 / 96.;
                let x = (lp.0 & 0xffff) as i16 as f32 / scale;
                let y = ((lp.0 >> 16) & 0xffff) as i16 as f32 / scale;
                let hover = hit(&st.spec, &st.layout, st.height, st.scroll, x, y);
                if st.hover != hover {
                    st.hover = hover;
                    invalidate(hwnd);
                }
                let mut tracking = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    ..Default::default()
                };
                unsafe {
                    let _ = TrackMouseEvent(&mut tracking);
                }
            }
            return LRESULT(0);
        }
        0x02A3 => {
            if let Some(st) = state(hwnd) {
                st.hover = None;
                invalidate(hwnd);
            }
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            if let Some(st) = state(hwnd) {
                let delta = ((wp.0 >> 16) & 0xffff) as i16 as f32;
                st.scroll = (st.scroll - delta / 120. * 48.).clamp(0., max_scroll(st));
                position_edit(hwnd);
                invalidate(hwnd);
            }
            return LRESULT(0);
        }
        WM_COMMAND => {
            if ((wp.0 >> 16) & 0xffff) as u32 == EN_SETFOCUS && lp.0 != 0 {
                if let Some(st) = state(hwnd) {
                    st.focus = Focus::Field;
                    invalidate(hwnd);
                }
            }
            if ((wp.0 >> 16) & 0xffff) as u32 == EN_CHANGE && lp.0 != 0 {
                if let Some(st) = state(hwnd) {
                    remember_text(st);
                    if st.error.is_some() {
                        st.error = st.spec.field.as_ref().and_then(|f| validation(f, &f.value));
                    }
                    invalidate(hwnd);
                }
            }
            return LRESULT(0);
        }
        WM_CTLCOLOREDIT => {
            if let Some(st) = state(hwnd) {
                let dc = HDC(wp.0 as *mut _);
                let c = st.theme.text;
                let bg = st.theme.control;
                unsafe {
                    SetTextColor(
                        dc,
                        COLORREF(c.r as u32 | (c.g as u32) << 8 | (c.b as u32) << 16),
                    );
                    SetBkColor(
                        dc,
                        COLORREF(bg.r as u32 | (bg.g as u32) << 8 | (bg.b as u32) << 16),
                    );
                }
                return LRESULT(st.brush.0 as isize);
            }
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            if let Some(st) = state(hwnd) {
                st.theme = Theme::new(system_light());
                recolor(st);
                let dark = i32::from(!st.theme.light);
                unsafe {
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        DWMWINDOWATTRIBUTE(20),
                        (&dark as *const i32).cast(),
                        4,
                    );
                }
                invalidate(hwnd);
                if !st.edit.is_invalid() {
                    unsafe {
                        let _ = InvalidateRect(st.edit, None, true);
                    }
                }
            }
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            if let Some(st) = state(hwnd) {
                remember_text(st);
                st.dpi = (wp.0 & 0xffff) as u32;
                font(st);
                let suggested = unsafe { *(lp.0 as *const RECT) };
                let center = POINT {
                    x: (suggested.left + suggested.right) / 2,
                    y: (suggested.top + suggested.bottom) / 2,
                };
                let (work, _) = monitor_at(center);
                let maxw = (work.right - work.left) as f32 * 96. / st.dpi as f32;
                let l = layout(&st.spec, st.spec.width.min(maxw));
                let r = opening_bounds(work, st.dpi, l.width, l.height, center);
                unsafe {
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                resize(hwnd);
            }
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// Read only Windows app-theme preference; the user's settings file is never changed.
pub fn system_light() -> bool {
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    let mut value = 1u32;
    let mut size = 4u32;
    unsafe {
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        );
    }
    value != 0
}

pub fn show(owner: HWND, light: bool, spec: DialogSpec) -> DialogResult {
    run(owner, light, spec, None, None)
}
/// Like [`show`], but whenever a checkbox changes `refresh` gets all checkbox states and returns
/// the spec to show next (new text, rows, buttons). The window follows the new height.
pub fn show_refreshing(
    owner: HWND,
    light: bool,
    spec: DialogSpec,
    refresh: &mut dyn FnMut(&[bool]) -> DialogSpec,
) -> DialogResult {
    run(owner, light, spec, None, Some(refresh))
}
pub fn show_live(
    owner: HWND,
    light: bool,
    spec: DialogSpec,
    callback: &mut dyn FnMut(usize, bool),
) -> DialogResult {
    run(owner, light, spec, Some(callback), None)
}
fn run<'a>(
    owner: HWND,
    light: bool,
    spec: DialogSpec,
    callback: Option<&'a mut dyn FnMut(usize, bool)>,
    refresh: Option<&'a mut dyn FnMut(&[bool]) -> DialogSpec>,
) -> DialogResult {
    let mut center = POINT::default();
    unsafe {
        let mut r = RECT::default();
        if !owner.is_invalid() && GetWindowRect(owner, &mut r).is_ok() {
            center = POINT {
                x: (r.left + r.right) / 2,
                y: (r.top + r.bottom) / 2,
            };
        } else {
            let _ = GetCursorPos(&mut center);
        }
    }
    let (work, dpi) = monitor_at(center);
    let width = spec
        .width
        .min((work.right - work.left).max(1) as f32 * 96. / dpi as f32);
    let l = layout(&spec, width);
    let r = opening_bounds(work, dpi, l.width, l.height, center);
    let initial = initial_focus(&spec);
    let mut st = Box::new(Runtime {
        spec,
        result: None,
        dpi,
        theme: Theme::new(light),
        layout: l,
        width,
        height: (r.bottom - r.top) as f32 * 96. / dpi as f32,
        scroll: 0.,
        focus: initial,
        visible: false,
        hover: None,
        error: None,
        surface: None,
        edit: HWND::default(),
        font: HFONT::default(),
        brush: HBRUSH::default(),
        callback,
        refresh,
    });
    recolor(&mut st);
    font(&mut st);
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map(|h| HINSTANCE(h.0))
            .unwrap_or_default();
        let class = WNDCLASSW {
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: crate::app_icon::class_icons().0,
            lpszClassName: w!("CorralDialog"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let created = CreateWindowExW(
            if owner.is_invalid() {
                WS_EX_APPWINDOW
            } else {
                WS_EX_TOOLWINDOW
            },
            w!("CorralDialog"),
            &HSTRING::from(&st.spec.title),
            WS_POPUP | WS_SYSMENU | WS_CLIPCHILDREN,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            owner,
            None,
            instance,
            Some((&mut *st as *mut Runtime<'_>).cast()),
        );
        if let Ok(hwnd) = created {
            let dark = i32::from(!light);
            let corner = 2i32;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWINDOWATTRIBUTE(20),
                (&dark as *const i32).cast(),
                4,
            );
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWINDOWATTRIBUTE(33),
                (&corner as *const i32).cast(),
                4,
            );
            if let Some(f) = &st.spec.field {
                st.edit = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("EDIT"),
                    &HSTRING::from(&f.value),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                    0,
                    0,
                    10,
                    10,
                    hwnd,
                    HMENU(10usize as *mut _),
                    instance,
                    None,
                )
                .unwrap_or_default();
                SendMessageW(st.edit, WM_SETFONT, WPARAM(st.font.0 as usize), LPARAM(1));
                SendMessageW(st.edit, EM_SETSEL, WPARAM(0), LPARAM(-1));
            }
            resize(hwnd);
            let owner_enabled = !owner.is_invalid() && IsWindowEnabled(owner).as_bool();
            if owner_enabled {
                let _ = EnableWindow(owner, false);
            }
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            focus_to(hwnd, initial, false);
            let mut msg = MSG::default();
            while IsWindow(hwnd).as_bool() {
                let status = GetMessageW(&mut msg, None, 0, 0);
                if status.0 <= 0 {
                    if status.0 == 0 {
                        PostQuitMessage(msg.wParam.0 as i32);
                    }
                    break;
                }
                if msg.message == WM_LBUTTONDOWN && msg.hwnd == st.edit {
                    focus_to(hwnd, Focus::Field, false);
                }
                if msg.message == WM_KEYDOWN
                    && (msg.hwnd == hwnd || msg.hwnd == st.edit)
                    && key(hwnd, msg.wParam.0)
                {
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            if IsWindow(hwnd).as_bool() {
                let _ = DestroyWindow(hwnd);
            }
            if owner_enabled {
                let _ = EnableWindow(owner, true);
                let _ = SetForegroundWindow(owner);
                let _ = SetFocus(owner);
            }
        }
        let _ = DeleteObject(st.font);
        let _ = DeleteObject(st.brush);
    }
    st.result.take().unwrap_or(DialogResult {
        action: Action::Closed,
        checks: vec![],
        text: String::new(),
    })
}
pub fn confirm(
    owner: HWND,
    light: bool,
    title: &str,
    body: &str,
    primary: &str,
    danger: bool,
) -> bool {
    let mut spec = DialogSpec::new(title, body, primary);
    spec.primary.danger = danger;
    show(owner, light, spec).action == Action::Primary
}
pub fn notice(owner: HWND, light: bool, title: &str, body: &str) {
    let mut spec = DialogSpec::new(title, body, "OK");
    spec.secondary = None;
    let _ = show(owner, light, spec);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_name_validation_and_boundaries() {
        let f = Field {
            label: String::new(),
            value: String::new(),
            hint: String::new(),
            required: true,
            max_chars: 255,
        };
        assert!(validation(&f, " \n ").is_some());
        assert_eq!(validation(&f, &"🙂".repeat(255)), None);
        assert!(validation(&f, &"🙂".repeat(256)).is_some());
    }
    #[test]
    fn footer_has_approved_padding_and_equal_confirm_buttons() {
        for width in [440., 680.] {
            let f = footer_layout(width, 300., true, false);
            let s = f.secondary.unwrap();
            assert_eq!(s.left, 25.);
            assert_eq!(f.primary.right, width - 25.);
            assert_eq!(f.primary.left - s.right, 8.);
            assert_eq!(s.right - s.left, f.primary.right - f.primary.left);
            assert_eq!(f.primary.bottom, 279.);
        }
    }
    #[test]
    fn focus_wraps_inside_dialog() {
        let mut spec = DialogSpec::new("Test", "Body", "Save");
        spec.checks.push(Checkbox::new("Check", false));
        spec.field = Some(Field {
            label: String::new(),
            value: String::new(),
            hint: String::new(),
            required: false,
            max_chars: 255,
        });
        assert_eq!(advance_focus(&spec, Focus::Primary, false), Focus::Check(0));
        assert_eq!(advance_focus(&spec, Focus::Check(0), true), Focus::Primary);
        assert_eq!(advance_focus(&spec, Focus::Field, false), Focus::Secondary);
    }
    #[test]
    fn destination_monitor_bounds_and_surfaces_scale_once() {
        for dpi in [96, 120, 144, 240] {
            let work = RECT {
                left: -1920,
                top: 0,
                right: 0,
                bottom: 1080,
            };
            let r = opening_bounds(work, dpi, 440., 280., POINT { x: -10, y: 100 });
            assert!(
                r.left >= work.left
                    && r.right <= work.right
                    && r.top >= work.top
                    && r.bottom <= work.bottom
            );
            assert_eq!(r.right - r.left, (440. * dpi as f32 / 96.).round() as i32);
        }
        let r = opening_bounds(
            RECT {
                left: 0,
                top: 0,
                right: 320,
                bottom: 200,
            },
            240,
            680.,
            600.,
            POINT { x: 160, y: 100 },
        );
        assert_eq!((r.right - r.left, r.bottom - r.top), (320, 200));
    }
    #[test]
    fn wrapping_preserves_every_long_name_character() {
        let text = "a".repeat(300);
        let lines = wrap(&text, 392., Type::Body);
        assert!(lines.len() > 1);
        assert_eq!(lines.join(""), text);
        for line in lines {
            assert!(
                render::measure_text(&line, theme::font_family(), 14., 400)
                    .unwrap()
                    .0
                    <= 392.
            );
        }
    }
    #[test]
    fn enter_escape_space_and_tab_have_modal_semantics() {
        let mut spec = DialogSpec::new("Test", "Body", "Save");
        spec.checks.push(Checkbox::new("Check", false));
        assert_eq!(
            keyboard_action(&spec, Focus::Check(0), 13, false),
            KeyAction::Activate(Action::Primary)
        );
        assert_eq!(
            keyboard_action(&spec, Focus::Primary, 27, false),
            KeyAction::Activate(Action::Secondary)
        );
        assert_eq!(
            keyboard_action(&spec, Focus::Check(0), 32, false),
            KeyAction::Toggle(0)
        );
        assert_eq!(
            keyboard_action(&spec, Focus::Field, 32, false),
            KeyAction::Ignore
        );
        assert_eq!(
            keyboard_action(&spec, Focus::Primary, 9, false),
            KeyAction::Focus(Focus::Check(0))
        );
        spec.secondary = None;
        assert_eq!(
            keyboard_action(&spec, Focus::Primary, 27, false),
            KeyAction::Activate(Action::Closed)
        );
    }
    #[test]
    fn hits_share_wrapped_checkbox_and_footer_geometry_at_all_dpis() {
        let mut spec = DialogSpec::new("Test", "Body", "Save");
        spec.checks
            .push(Checkbox::new("Back up the current layout first", true));
        let l = layout(&spec, 440.);
        let c = l.checks[0];
        for dpi in [96, 120, 144, 240] {
            let scale = dpi as f32 / 96.;
            let pointer = px(
                RectF::new(c.right - 10., c.top + 4., c.right - 9., c.top + 5.),
                dpi,
            );
            assert_eq!(
                hit(
                    &spec,
                    &l,
                    l.height,
                    0.,
                    pointer.left / scale,
                    pointer.top / scale
                ),
                Some(Focus::Check(0))
            );
        }
        let f = footer_layout(440., l.height, true, false);
        assert_eq!(
            hit(
                &spec,
                &l,
                l.height,
                0.,
                f.primary.left + 4.,
                f.primary.top + 4.
            ),
            Some(Focus::Primary)
        );
        assert_eq!(
            hit(
                &spec,
                &l,
                l.height,
                100.,
                f.primary.left + 4.,
                f.primary.top + 4.
            ),
            Some(Focus::Primary)
        );
    }
    #[test]
    fn matched_figma_dialog_frames_include_outer_and_footer_borders() {
        let mut name = DialogSpec::new("Name this backup", "", "Save");
        name.field = Some(Field {
            label: String::new(),
            value: String::new(),
            hint: "Up to 255 characters.".into(),
            required: true,
            max_chars: 255,
        });
        let l = layout(&name, 440.);
        assert_eq!(l.title.left, 25.);
        assert_eq!(l.title.top, 25.);
        assert_eq!(l.field.unwrap().top, 65.);
        assert_eq!(l.hint.unwrap().top, 109.);
        assert_eq!(l.footer_top, 149.);
        assert_eq!(l.height, 223.);
        let mut restore = DialogSpec::new(
            "Restore backup?",
            "Your Corrals go back to how they were yesterday. Files on the desktop are not moved or deleted.",
            "Restore",
        );
        restore.checks = vec![
            Checkbox::new("Back up the current layout first", true),
            Checkbox::new("Don't ask again", false),
        ];
        let l = layout(&restore, 440.);
        assert_eq!(l.checks[0].top, 121.);
        assert_eq!(l.checks[1].top, 153.);
        assert_eq!(l.height, 271.);
    }
    #[test]
    fn danger_is_not_default_but_explicit_keyboard_selection_can_accept() {
        let mut spec = DialogSpec::new("Remove rule?", "Remove this Round-up rule?", "Remove");
        spec.primary.danger = true;
        assert_eq!(initial_focus(&spec), Focus::Secondary);
        assert_eq!(
            keyboard_action(&spec, initial_focus(&spec), 13, false),
            KeyAction::Activate(Action::Secondary)
        );
        let selected = advance_focus(&spec, initial_focus(&spec), false);
        assert_eq!(selected, Focus::Primary);
        assert_eq!(
            keyboard_action(&spec, selected, 13, false),
            KeyAction::Activate(Action::Primary)
        );
    }
    #[test]
    fn shortcut_done_is_compact_at_right_but_notice_ok_stays_full_width() {
        let mut spec = DialogSpec::new("Shortcut arrows", "Body", "Done");
        spec.secondary = None;
        spec.single_button_full_width = false;
        let l = layout(&spec, 440.);
        let f = footer_for_spec(&spec, l.width, l.height);
        assert_eq!(f.primary.right, 415.);
        assert_eq!(f.primary.right - f.primary.left, 80.);
        assert_eq!(hit(&spec, &l, l.height, 0., 30., f.primary.top + 10.), None);
        assert_eq!(
            hit(
                &spec,
                &l,
                l.height,
                0.,
                f.primary.left + 10.,
                f.primary.top + 10.
            ),
            Some(Focus::Primary)
        );
        spec.single_button_full_width = true;
        let f = footer_for_spec(&spec, 440., l.height);
        assert_eq!(f.primary.left, 25.);
        assert_eq!(f.primary.right - f.primary.left, 390.);
    }
    #[test]
    fn shared_dialog_templates_golden_renders() {
        let dir = std::path::Path::new("target/qa/settings-redesign/u4");
        std::fs::create_dir_all(dir).unwrap();
        let mut check = DialogSpec::new(
            "Shortcut arrows",
            "Hide the small arrow on these kinds of shortcuts inside Corrals. Icons on the desktop itself keep their arrows.",
            "Done",
        );
        check.secondary = None;
        check.single_button_full_width = false;
        check.checks = [
            "Web links",
            "Program shortcuts",
            "Folder shortcuts",
            "Document shortcuts",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, s)| Checkbox::new(s, index < 2))
        .collect();
        let mut name = DialogSpec::new("Name this backup", "", "Save");
        name.field = Some(Field {
            label: String::new(),
            value: "My layout".into(),
            hint: "Up to 255 characters.".into(),
            required: true,
            max_chars: 255,
        });
        let mut danger = DialogSpec::new("Remove rule?", "Remove this Round-up rule?", "Remove");
        danger.primary.danger = true;
        let mut notice = DialogSpec::new(
            "Titlebar buttons are hidden",
            "You can still open a Corral's options: right-click its titlebar.",
            "OK",
        );
        notice.secondary = None;
        for (id, spec) in [
            ("shortcut-arrows", check),
            ("name-field", name),
            ("remove-rule", danger),
            ("notice", notice),
        ] {
            for light in [false, true] {
                for dpi in [96, 120, 144, 240] {
                    let l = layout(&spec, spec.width);
                    let s = dpi as f32 / 96.;
                    let mut surface =
                        Surface::new((l.width * s).round() as u32, (l.height * s).round() as u32)
                            .unwrap();
                    surface
                        .draw(|ctx| draw_spec(ctx, &spec, dpi, light))
                        .unwrap();
                    surface
                        .save_png(&dir.join(format!(
                            "dialog-{id}-{}-{dpi}.png",
                            if light { "light" } else { "dark" }
                        )))
                        .unwrap();
                }
            }
        }
    }
}
