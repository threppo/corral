//! Scoped appearance reset: the U4 Corral frame with the U3 exclusive reset choices.
use super::kit::{
    controls::{self, Control, VisualState},
    theme::{Theme, Type},
};
use crate::{
    dialog,
    render::{self, RectF, Surface},
    settings::style_request::Action,
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM},
        Graphics::{
            Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute},
            Gdi::{BeginPaint, BitBlt, EndPaint, InvalidateRect, PAINTSTRUCT, SRCCOPY},
        },
        UI::{
            Input::KeyboardAndMouse::{EnableWindow, GetKeyState, IsWindowEnabled},
            WindowsAndMessaging::*,
        },
    },
    core::{Result, w},
};

#[derive(Clone)]
struct Spec {
    title: String,
    body: String,
    caption: String,
    choices: Vec<(String, String)>,
    target: Option<u64>,
    width: f32,
}
fn names(titles: &[String]) -> String {
    super::look_pages::title_list(titles)
}
fn spec(target: Option<(u64, String)>, own: &[String]) -> Spec {
    if let Some((id, title)) = target {
        return Spec { title:format!("Reset \"{title}\"?"),
        body:"Its own color, see-through, text color, tint and Camouflage choice are removed, so it uses the look all Corrals share. Other Corrals are not changed.".into(),
        caption:String::new(),choices:vec![],target:Some(id),width:440. };
    }
    let titles = names(own);
    Spec {
        title: "Reset the look of all Corrals?".into(),
        body:
            "The color, see-through and text color that all Corrals share go back to the defaults."
                .into(),
        caption: "Blur, corners, outline, titlebar options and Advanced settings are not changed."
            .into(),
        choices: if own.is_empty() {
            vec![]
        } else {
            vec![
                (
                    "Keep Corrals with their own look".into(),
                    format!("{titles} keep their colors. Recommended."),
                ),
                (
                    "Reset every Corral".into(),
                    format!("{titles} lose their own look too. This cannot be undone."),
                ),
            ]
        },
        target: None,
        width: 480.,
    }
}
fn result(spec: &Spec, accepted: bool, selected: usize) -> Option<Action> {
    accepted.then(|| match spec.target {
        Some(id) => Action::ResetGroup { id },
        None if spec.choices.is_empty() || selected == 0 => Action::ResetBase,
        None => Action::ResetAll,
    })
}
struct Layout {
    title: RectF,
    body: RectF,
    choices: Vec<RectF>,
    caption: RectF,
    footer: f32,
    height: f32,
}
fn wrapped(text: &str, width: f32, ty: Type) -> Vec<String> {
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
            if render::measure_text(&candidate, super::kit::theme::font_family(), size, weight)
                .is_ok_and(|m| m.0 <= width)
            {
                line = candidate;
                continue;
            }
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            for ch in word.chars() {
                let candidate = format!("{line}{ch}");
                if !line.is_empty()
                    && render::measure_text(
                        &candidate,
                        super::kit::theme::font_family(),
                        size,
                        weight,
                    )
                    .is_ok_and(|m| m.0 > width)
                {
                    lines.push(std::mem::take(&mut line));
                }
                line.push(ch);
            }
        }
        if !line.is_empty() {
            lines.push(line);
        }
    }
    lines
}
fn text_height(text: &str, width: f32, ty: Type) -> f32 {
    wrapped(text, width, ty).len() as f32 * ty.metrics().1
}
fn layout(s: &Spec) -> Layout {
    let w = s.width;
    let mut y = 24.;
    let title_h = text_height(&s.title, w - 48., Type::DialogTitle);
    let title = RectF::new(24., y, w - 24., y + title_h);
    y += title_h + 12.;
    let h = text_height(&s.body, w - 48., Type::Body);
    let body = RectF::new(24., y, w - 24., y + h);
    y += h + 12.;
    let mut choices = vec![];
    for (label, desc) in &s.choices {
        let h = (text_height(label, w - 78., Type::Body)
            + 2.
            + text_height(desc, w - 78., Type::Caption))
        .max(32.);
        choices.push(RectF::new(24., y, w - 24., y + h));
        y += h + 12.;
    }
    let h = text_height(&s.caption, w - 48., Type::Caption);
    let caption = RectF::new(24., y, w - 24., y + h);
    y += h + 24.;
    Layout {
        title,
        body,
        choices,
        caption,
        footer: y,
        height: y + 72.,
    }
}
fn text(
    ctx: &render::Ctx,
    label: &str,
    r: RectF,
    ty: Type,
    color: render::Rgba,
    dpi: u32,
) -> Result<()> {
    let scale = dpi as f32 / 96.;
    for (i, line) in wrapped(label, r.right - r.left, ty).iter().enumerate() {
        let y = r.top + i as f32 * ty.metrics().1;
        ctx.draw_text_cached(
            line,
            RectF::new(
                r.left * scale,
                y * scale,
                r.right * scale,
                (y + ty.metrics().1) * scale,
            ),
            &ty.style(dpi, color),
        )?;
    }
    Ok(())
}
fn draw(
    ctx: &render::Ctx,
    s: &Spec,
    l: &Layout,
    selected: usize,
    focus: Option<usize>,
    hover: Option<usize>,
    theme: &Theme,
    dpi: u32,
) -> Result<()> {
    draw_view(ctx, s, l, selected, focus, hover, theme, dpi, l.height, 0.)
}
fn draw_view(
    ctx: &render::Ctx,
    s: &Spec,
    l: &Layout,
    selected: usize,
    focus: Option<usize>,
    hover: Option<usize>,
    theme: &Theme,
    dpi: u32,
    height: f32,
    scroll: f32,
) -> Result<()> {
    let shift = |r: RectF| RectF::new(r.left, r.top - scroll, r.right, r.bottom - scroll);
    let scale = dpi as f32 / 96.;
    dialog::draw_frame(ctx, s.width, height, (height - 72.).max(0.), theme, dpi)?;
    ctx.with_clip(
        RectF::new(
            scale,
            scale,
            (s.width - 1.) * scale,
            (height - 72.).max(1.) * scale,
        ),
        |ctx| {
            text(
                ctx,
                &s.title,
                shift(l.title),
                Type::DialogTitle,
                theme.text,
                dpi,
            )?;
            text(ctx, &s.body, shift(l.body), Type::Body, theme.text, dpi)?;
            for (i, ((label, desc), r)) in s.choices.iter().zip(&l.choices).enumerate() {
                let r = shift(*r);
                controls::draw_control(
                    ctx,
                    Control::Radio,
                    RectF::new(r.left, r.top + 1., r.left + 20., r.top + 21.),
                    "",
                    VisualState {
                        selected: i == selected,
                        focused: focus == Some(i),
                        hovered: hover == Some(i),
                        ..Default::default()
                    },
                    theme,
                    dpi,
                    0.,
                )?;
                let left = r.left + 30.;
                let h = text_height(label, r.right - left, Type::Body);
                text(
                    ctx,
                    label,
                    RectF::new(left, r.top, r.right, r.top + h),
                    Type::Body,
                    theme.text,
                    dpi,
                )?;
                text(
                    ctx,
                    desc,
                    RectF::new(left, r.top + h + 2., r.right, r.bottom),
                    Type::Caption,
                    theme.secondary,
                    dpi,
                )?;
            }
            text(
                ctx,
                &s.caption,
                shift(l.caption),
                Type::Caption,
                theme.secondary,
                dpi,
            )?;
            Ok(())
        },
    )?;
    let offset = s.choices.len();
    dialog::draw_footer(
        ctx,
        s.width,
        height,
        &dialog::Button {
            label: "Reset".into(),
            danger: false,
        },
        Some("Cancel"),
        None,
        focus.and_then(|i| i.checked_sub(offset)),
        theme,
        dpi,
    )
}
struct State {
    spec: Spec,
    lay: Layout,
    theme: Theme,
    dpi: u32,
    selected: usize,
    focus: usize,
    visible: bool,
    hover: Option<usize>,
    done: bool,
    accepted: bool,
    surface: Option<Surface>,
    viewport_height: f32,
    scroll: f32,
    pressed: Option<usize>,
}
fn contains(r: RectF, p: (f32, f32)) -> bool {
    p.0 >= r.left && p.0 < r.right && p.1 >= r.top && p.1 < r.bottom
}
fn hit(st: &State, lp: LPARAM) -> Option<usize> {
    let scale = st.dpi as f32 / 96.;
    let p = (
        (lp.0 as i16) as f32 / scale,
        ((lp.0 >> 16) as i16) as f32 / scale,
    );
    let f = dialog::footer_layout(st.spec.width, st.viewport_height, true, false);
    st.lay
        .choices
        .iter()
        .position(|r| {
            p.1 < st.viewport_height - 72.
                && contains(
                    RectF::new(r.left, r.top - st.scroll, r.right, r.bottom - st.scroll),
                    p,
                )
        })
        .or_else(|| {
            f.secondary
                .filter(|r| contains(*r, p))
                .map(|_| st.spec.choices.len())
        })
        .or_else(|| contains(f.primary, p).then_some(st.spec.choices.len() + 1))
}
fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}
fn fit(st: &mut State, work: RECT, dpi: u32) {
    let scale = dpi as f32 / 96.;
    st.dpi = dpi;
    st.spec.width = (if st.spec.target.is_some() {
        440.0f32
    } else {
        480.0f32
    })
    .min((work.right - work.left) as f32 / scale);
    st.lay = layout(&st.spec);
    st.viewport_height = st.lay.height.min((work.bottom - work.top) as f32 / scale);
    st.scroll = st
        .scroll
        .clamp(0., (st.lay.footer - (st.viewport_height - 72.)).max(0.));
    st.surface = None;
}
fn reveal(st: &mut State) {
    if let Some(r) = st.lay.choices.get(st.focus) {
        let bottom = st.viewport_height - 84.;
        if r.bottom - r.top > (bottom - 12.).max(1.) {
            st.scroll = (r.top - 12.).max(0.);
        } else if r.top - st.scroll < 12. {
            st.scroll = (r.top - 12.).max(0.);
        } else if r.bottom - st.scroll > bottom {
            st.scroll = r.bottom - bottom;
        }
    }
}
extern "system" fn proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        unsafe {
            let c = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, c.lpCreateParams as isize);
        }
        return LRESULT(1);
    }
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_CLOSE => unsafe {
            (*ptr).done = true;
        },
        WM_MOUSEMOVE => {
            let st = unsafe { &mut *ptr };
            st.hover = hit(st, lp);
            invalidate(hwnd);
            let mut track = windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<
                    windows::Win32::UI::Input::KeyboardAndMouse::TRACKMOUSEEVENT,
                >() as u32,
                dwFlags: windows::Win32::UI::Input::KeyboardAndMouse::TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            unsafe {
                let _ = windows::Win32::UI::Input::KeyboardAndMouse::TrackMouseEvent(&mut track);
            }
        }
        0x02a3 => {
            unsafe {
                (*ptr).hover = None;
            }
            invalidate(hwnd);
        }
        WM_LBUTTONDOWN => {
            let st = unsafe { &mut *ptr };
            st.pressed = hit(st, lp);
            st.visible = false;
        }
        WM_LBUTTONUP => {
            let st = unsafe { &mut *ptr };
            let hit = hit(st, lp);
            let pressed = st.pressed.take();
            if hit == pressed {
                if let Some(i) = hit {
                    st.focus = i;
                    let n = st.spec.choices.len();
                    if i < n {
                        st.selected = i;
                    } else {
                        st.accepted = i == n + 1;
                        st.done = true;
                    }
                    invalidate(hwnd);
                }
            }
        }
        WM_MOUSEWHEEL => {
            let st = unsafe { &mut *ptr };
            let delta = ((wp.0 >> 16) as i16) as f32 / 120.;
            st.scroll = (st.scroll - delta * 48.)
                .clamp(0., (st.lay.footer - (st.viewport_height - 72.)).max(0.));
            invalidate(hwnd);
        }
        WM_KEYDOWN => {
            let reverse = unsafe { GetKeyState(0x10) } < 0;
            let st = unsafe { &mut *ptr };
            let n = st.spec.choices.len();
            st.visible = true;
            match wp.0 {
                0x1b => st.done = true,
                9 => st.focus = (st.focus + if reverse { n + 1 } else { 1 }) % (n + 2),
                0x25 | 0x26 | 0x27 | 0x28 if st.focus < n => {
                    st.selected = 1 - st.selected;
                    st.focus = st.selected;
                }
                0x20 if st.focus < n => st.selected = st.focus,
                0x20 => {
                    st.accepted = st.focus == n + 1;
                    st.done = true;
                }
                0x0d => {
                    st.accepted = st.focus != n;
                    st.done = true;
                }
                _ => {}
            }
            reveal(st);
            invalidate(hwnd);
        }
        WM_DPICHANGED => {
            let center = unsafe {
                let r = &*(lp.0 as *const RECT);
                POINT {
                    x: (r.left + r.right) / 2,
                    y: (r.top + r.bottom) / 2,
                }
            };
            let (work, _) = dialog::monitor_at(center);
            let r = {
                let st = unsafe { &mut *ptr };
                fit(st, work, (wp.0 & 0xffff) as u32);
                dialog::opening_bounds(work, st.dpi, st.spec.width, st.viewport_height, center)
            };
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
            invalidate(hwnd);
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let dc = unsafe { BeginPaint(hwnd, &mut ps) };
            {
                let st = unsafe { &mut *ptr };
                let scale = st.dpi as f32 / 96.;
                let (w, h) = (
                    (st.spec.width * scale).round() as u32,
                    (st.viewport_height * scale).round() as u32,
                );
                if st
                    .surface
                    .as_ref()
                    .is_none_or(|s| s.width() != w || s.height() != h)
                {
                    st.surface = Surface::new(w, h).ok();
                }
                if let Some(mut surface) = st.surface.take() {
                    let _ = surface.draw(|ctx| {
                        draw_view(
                            ctx,
                            &st.spec,
                            &st.lay,
                            st.selected,
                            st.visible.then_some(st.focus),
                            st.hover,
                            &st.theme,
                            st.dpi,
                            st.viewport_height,
                            st.scroll,
                        )
                    });
                    unsafe {
                        let _ = BitBlt(dc, 0, 0, w as i32, h as i32, surface.dc(), 0, 0, SRCCOPY);
                    }
                    st.surface = Some(surface);
                }
            }
            unsafe {
                let _ = EndPaint(hwnd, &ps);
            }
        }
        WM_NCDESTROY => {
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
        }
        _ => return unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
    LRESULT(0)
}
pub fn show(
    owner: HWND,
    light: bool,
    target: Option<(u64, String)>,
    own_titles: &[String],
) -> Option<Action> {
    unsafe {
        let mut owner_rect = RECT::default();
        let _ = GetWindowRect(owner, &mut owner_rect);
        let center = POINT {
            x: (owner_rect.left + owner_rect.right) / 2,
            y: (owner_rect.top + owner_rect.bottom) / 2,
        };
        let (work, dpi) = dialog::monitor_at(center);
        let spec = spec(target, own_titles);
        let lay = layout(&spec);

        let initial_focus = if spec.choices.is_empty() { 1 } else { 0 };
        let mut state = Box::new(State {
            spec,
            lay,
            theme: Theme::new(light),
            dpi,
            selected: 0,
            focus: initial_focus,
            visible: false,
            hover: None,
            done: false,
            accepted: false,
            surface: None,
            viewport_height: 0.,
            scroll: 0.,
            pressed: None,
        });
        fit(&mut state, work, dpi);
        let r = dialog::opening_bounds(work, dpi, state.spec.width, state.viewport_height, center);
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).ok()?;
        let class = WNDCLASSW {
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("CorralAppearanceReset"),
            ..Default::default()
        };
        RegisterClassW(&class);
        let hwnd = CreateWindowExW(
            WS_EX_DLGMODALFRAME | WS_EX_TOOLWINDOW,
            w!("CorralAppearanceReset"),
            w!("Reset appearance"),
            WS_POPUP,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            owner,
            None,
            Some(&windows::Win32::Foundation::HINSTANCE(instance.0)),
            Some((&mut *state as *mut State).cast()),
        )
        .ok()?;
        let dark = i32::from(!light);
        let corner = 2i32;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(20),
            &dark as *const _ as *const _,
            4,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(33),
            &corner as *const _ as *const _,
            4,
        );
        let enabled = IsWindowEnabled(owner).as_bool();
        if enabled {
            let _ = EnableWindow(owner, false);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let mut quit = None;
        let mut message = MSG::default();
        while !state.done && IsWindow(hwnd).as_bool() {
            let status = GetMessageW(&mut message, None, 0, 0).0;
            if status <= 0 {
                if status == 0 {
                    quit = Some(message.wParam.0 as i32);
                }
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        if IsWindow(hwnd).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
        if enabled && IsWindow(owner).as_bool() {
            let _ = EnableWindow(owner, true);
            let _ = SetForegroundWindow(owner);
        }
        if let Some(code) = quit {
            PostQuitMessage(code);
        }
        result(&state.spec, state.accepted, state.selected)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmed_scope_and_radio_map_to_existing_reset_actions() {
        for n in [0, 1, 4] {
            let own = (0..n).map(|i| format!("Corral {i}")).collect::<Vec<_>>();
            let s = spec(None, &own);
            assert_eq!(s.choices.len(), if n == 0 { 0 } else { 2 });
            assert!(result(&s, false, 1).is_none());
            assert!(matches!(result(&s, true, 0), Some(Action::ResetBase)));
            assert!(matches!(result(&s, true, 1), Some(Action::ResetBase)) == (n == 0));
        }
        let s = spec(Some((7, "Downloads".into())), &[]);
        assert!(matches!(
            result(&s, true, 0),
            Some(Action::ResetGroup { id: 7 })
        ));
    }
    #[test]
    fn reset_goldens() -> Result<()> {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/qa/settings-redesign/u3");
        std::fs::create_dir_all(&dir).unwrap();
        for light in [false, true] {
            for dpi in [96, 120, 144, 240] {
                for (n, target, own) in [
                    ("all-shared", None, vec![]),
                    ("all-own", None, vec!["Downloads".into(), "Games".into()]),
                    ("group", Some((7, "Downloads".into())), vec![]),
                ] {
                    let s = spec(target, &own);
                    let l = layout(&s);
                    let scale = dpi as f32 / 96.;
                    let mut surface = Surface::new(
                        (s.width * scale).round() as u32,
                        (l.height * scale).round() as u32,
                    )?;
                    surface
                        .draw(|ctx| draw(ctx, &s, &l, 0, None, None, &Theme::new(light), dpi))?;
                    surface.save_png(&dir.join(format!(
                        "reset-{n}-{}-{dpi}.png",
                        if light { "light" } else { "dark" }
                    )))?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod viewport_tests {
    use super::*;
    #[test]
    fn long_unbroken_titles_wrap_and_small_work_area_keeps_reset_accessible() {
        let name = "W".repeat(255);
        let lines = wrapped(&name, 160., Type::Body);
        assert!(lines.len() > 1);
        assert_eq!(lines.concat(), name);
        let spec = spec(Some((7, name)), &[]);
        let lay = layout(&spec);
        let mut state = State {
            spec,
            lay,
            theme: Theme::new(false),
            dpi: 240,
            selected: 0,
            focus: 1,
            visible: false,
            hover: None,
            done: false,
            accepted: false,
            surface: None,
            viewport_height: 0.,
            scroll: 0.,
            pressed: None,
        };
        fit(
            &mut state,
            RECT {
                left: 0,
                top: 0,
                right: 800,
                bottom: 600,
            },
            240,
        );
        assert_eq!(state.spec.width, 320.);
        assert!(state.lay.height > state.viewport_height);
        let footer = dialog::footer_layout(state.spec.width, state.viewport_height, true, false);
        let x = ((footer.primary.left + footer.primary.right) * 1.25) as i32;
        let y = ((footer.primary.top + footer.primary.bottom) * 1.25) as i32;
        assert_eq!(
            hit(&state, LPARAM(((y << 16) | (x & 0xffff)) as isize)),
            Some(1)
        );
        assert!(footer.primary.bottom <= state.viewport_height);
    }
}
