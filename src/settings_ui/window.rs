//! The top-level "Corral Settings" window: a thin Win32 shell around the off-screen renderer.
//!
//! All painting is `render_settings_state` into a [`Surface`] followed by a `BitBlt`; input is turned
//! into [`Hit`]s by the pure layout code in the parent module. Settings are saved after every change
//! (no Apply button). The window is per-monitor-v2 DPI aware (process setting made in
//! `main`), re-renders at the new DPI on `WM_DPICHANGED`, and has a 760x520 DIP minimum client size.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, EndPaint, InvalidateRect, PAINTSTRUCT, SRCCOPY,
};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, w};

use super::backup_dialog::{self, Chosen};
use super::backups;
use super::color_popover;
use super::content::combo_choices;
use super::look_pages::{self, Action as LookAction, ColorChannel, ColorChoice, SliderKind};
use super::rules_dialog::{self, EditOutcome};
use super::rules_page::{self, Action, GroupInfo};
use super::{
    Hit, Page, Theme, View, apply_choice, apply_click, hit_test, max_scroll_with, nav_width, sc,
};
use crate::color_state::{
    self, ColorEdit, TintMode,
    page::{Caption, ColorPage, Commit, CorralInfo, Ended, Field as ColorField, Scope, Step, Throttle},
};
use crate::render::{RectF, Rgba, Surface};
use crate::settings::{self, Field, Settings};
use crate::snapshot::Action as BackupAction;

const MIN_CLIENT: (f32, f32) = (760.0, 520.0);
const INITIAL_CLIENT: (f32, f32) = (1000.0, 720.0);
const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_OVERLAPPEDWINDOW.0 | WS_CLIPCHILDREN.0);
const WM_MOUSELEAVE: u32 = 0x02A3;
const WHEEL_STEP_DIP: f32 = 48.0;
static COLOR_SETTINGS_WRITE: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// The Corral menu's "Colors..." command, relayed from the WM_COPYDATA handler (colour-state-spec 3.1).
const WM_FOCUS_CORRAL: u32 = WM_APP + 134;
use settings::style_request::{WM_COLOR_STATUS, status};
const TIMER_LIVE_COLOR: usize = 0xC033;
const TIMER_LOOK: usize = 0xC034;
const TIMER_DESK_SETUP: usize = 0xC035;
const LIVE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(16);

struct State {
    settings: Settings,
    path: PathBuf,
    /// Layout file the Rules page reads Corrals from (read-only).
    layout_path: PathBuf,
    /// Corrals of the layout (reloaded whenever the Rules page opens).
    groups: Vec<GroupInfo>,
    view: View,
    dpi: u32,
    theme: Theme,
    surface: Option<Surface>,
    look_drag: Option<(SliderKind, i32, i32)>,
    look_dirty: Option<SliderKind>,
    look_last_send: Option<std::time::Instant>,
    look_pending: bool,
    pressed: bool,
    focus_index: Option<usize>,
    focus_visible: bool,
    hover_hit: Hit,
    /// Scope, popover session and stored colour state (colour-state-spec 2.3, 3).
    page: ColorPage,
    /// Bounds the draft frames of the open session to one per 16 ms.
    throttle: Throttle,
    clock: std::time::Instant,
    /// Session whose draft frames the engine currently shows (it needs a cancel when that ends).
    framed: Option<u64>,
    /// The Corral menu asked for this Corral while a popover was open.
    pending_focus: Option<u64>,
    sender: Option<ColorSender>,
    /// Transient engine status that is not an error of the colour page (e.g. the engine is not running).
    notice: String,
    tracking: bool,
    /// Tooltip control of the backup tiles and the text it is currently asked to show.
    tip: HWND,
    tip_text: Vec<u16>,
}

// One worker owns IPC and disk fallback; the window thread only replaces queued
// previews. A persisted boundary is never overwritten by a subsequent gesture's preview.
struct ColorSender {
    shared: std::sync::Arc<(std::sync::Mutex<SenderQueue>, std::sync::Condvar)>,
    worker: Option<std::thread::JoinHandle<()>>,
}
#[derive(Default)]
struct SenderQueue {
    requests: std::collections::VecDeque<settings::style_request::LiveRequest>,
    closed: bool,
}
impl SenderQueue {
    /// Draft frames of one session replace each other; a commit or cancel is a barrier that
    /// is never overwritten by a later gesture's frames.
    fn submit(&mut self, request: settings::style_request::LiveRequest) {
        if !request.persist && !request.cancel {
            if let Some(last) = self.requests.back_mut().filter(|last| {
                !last.persist && !last.cancel && last.session == request.session && last.target == request.target
            }) {
                let mut patch = last.patch.clone();
                patch.merge(&request.patch);
                *last = settings::style_request::LiveRequest { patch, ..request };
                return;
            }
        }
        self.requests.push_back(request);
    }
}
impl Drop for ColorSender {
    fn drop(&mut self) {
        {
            let (lock, wake) = &*self.shared;
            let mut q = lock.lock().unwrap();
            q.closed = true;
            wake.notify_one();
        }
        // The window is gone: drain final boundaries before this settings process exits.
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl ColorSender {
    fn new(hwnd: HWND, path: PathBuf) -> Self {
        let shared = std::sync::Arc::new((
            std::sync::Mutex::new(SenderQueue::default()),
            std::sync::Condvar::new(),
        ));
        let worker = shared.clone();
        let destination = hwnd.0 as usize;
        let handle = std::thread::spawn(move || {
            use settings::style_request::{Action, LiveOutcome, Request, send_live};
            loop {
                let request = {
                    let (lock, wake) = &*worker;
                    let mut q = lock.lock().unwrap();
                    while q.requests.is_empty() && !q.closed {
                        q = wake.wait(q).unwrap();
                    }
                    let Some(r) = q.requests.pop_front() else {
                        break;
                    };
                    r
                };
                let outcome = send_live(&request);
                let post = |code: usize| unsafe {
                    let _ = PostMessageW(
                        HWND(destination as *mut _),
                        WM_COLOR_STATUS,
                        WPARAM(code),
                        LPARAM(request.session as isize),
                    );
                };
                if !request.persist {
                    // Draft frames and cancels are best effort: with no engine there is no overlay.
                    continue;
                }
                match outcome {
                    LiveOutcome::Applied => post(status::APPLIED),
                    // The engine refused it (target gone, preview instance, queue full): nothing
                    // was stored, the page puts the stored look back.
                    LiveOutcome::Rejected => post(status::ENGINE_SAVE_FAILED),
                    LiveOutcome::PreviewOnly => post(status::PREVIEW_ONLY),
                    LiveOutcome::Unavailable => {
                        // Quiet asynchronous file fallback: engine remains the sole layout writer.
                        // Serialize our own page saves with the worker so neither loses protocol state.
                        let _write = COLOR_SETTINGS_WRITE.lock().unwrap();
                        let mut latest = settings::load_with_backup(&path);
                        let previous = latest
                            .style_request
                            .as_ref()
                            .map(|r| r.nonce)
                            .max(latest.style_result.as_ref().map(|r| r.nonce));
                        let nonce = backups::issue_nonce(previous, backups::now_ms());
                        let mut patch = request.patch.clone();
                        // An unavailable engine has only one file slot. Preserve independent
                        // fields from a queued edit of the same target before replacing it.
                        if let Some(pending) = latest
                            .style_request
                            .as_ref()
                            .filter(|p| p.layout_path == request.layout_path)
                        {
                            let previous_patch = match (&pending.action, request.target) {
                                (Action::Edit { id, patch }, Some(target)) if *id == target => {
                                    Some(patch)
                                }
                                (Action::EditAll { patch }, None) => Some(patch),
                                _ => None,
                            };
                            if let Some(previous_patch) = previous_patch {
                                let mut merged = previous_patch.clone();
                                merged.merge(&patch);
                                patch = merged;
                            }
                        }
                        latest.style_request = Some(Request {
                            nonce,
                            layout_path: request.layout_path.clone(),
                            action: match request.target {
                                Some(id) => Action::Edit { id, patch },
                                None => Action::EditAll { patch },
                            },
                        });
                        latest.style_result = None;
                        post(if settings::save(&path, &latest).is_ok() {
                            status::QUEUED_IN_FILE
                        } else {
                            status::FILE_SAVE_FAILED
                        });
                    }
                }
            }
        });
        Self {
            shared,
            worker: Some(handle),
        }
    }
    fn submit(&self, request: settings::style_request::LiveRequest) {
        let (lock, wake) = &*self.shared;
        let mut q = lock.lock().unwrap();
        q.submit(request);
        wake.notify_one();
    }
}

fn read_dword(subkey: windows::core::PCWSTR, name: windows::core::PCWSTR) -> Option<u32> {
    let mut value = 0u32;
    let mut size = 4u32;
    // SAFETY: `value` / `size` describe a DWORD buffer.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            subkey,
            name,
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };
    status.is_ok().then_some(value)
}

/// Corral amber is fixed; Windows controls only the light/dark selection.
fn system_theme() -> Theme {
    let light = read_dword(
        w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
        w!("AppsUseLightTheme"),
    ) == Some(1);
    if light { Theme::light() } else { Theme::dark() }
}

fn caption_theme(hwnd: HWND, theme: &Theme) {
    use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
    let colorref = |c: Rgba| u32::from(c.r) | (u32::from(c.g) << 8) | (u32::from(c.b) << 16);
    let dark = i32::from(!theme.light);
    let background = colorref(theme.panel_bg);
    let foreground = colorref(theme.header_text);
    // Unsupported attributes fail harmlessly on Windows versions before 11.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(20),
            (&dark as *const i32).cast(),
            4,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(35),
            (&background as *const u32).cast(),
            4,
        );
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(36),
            (&foreground as *const u32).cast(),
            4,
        );
    }
}

/// Pure work-area policy, including the non-client frame. Small work areas win over minimums.
fn opening_bounds(work: (i32, i32, i32, i32), desired: (i32, i32)) -> (i32, i32, i32, i32) {
    let (left, top, right, bottom) = work;
    let width = (right - left).max(1);
    let height = (bottom - top).max(1);
    let (w, h) = if desired.0 > width || desired.1 > height {
        (
            desired.0.min((width as f32 * 0.9).floor() as i32),
            desired.1.min((height as f32 * 0.9).floor() as i32),
        )
    } else {
        desired
    };
    (
        left + (width - w) / 2,
        top + (height - h) / 2,
        w.max(1),
        h.max(1),
    )
}

fn monitor_work(hwnd: HWND) -> RECT {
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
    };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        let _ = GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut info);
    }
    info.rcWork
}

fn outer_size(client: (f32, f32), dpi: u32) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: sc(client.0, dpi),
        bottom: sc(client.1, dpi),
    };
    // SAFETY: plain geometry call on a local RECT.
    let _ = unsafe { AdjustWindowRectExForDpi(&mut r, STYLE, false, WINDOW_EX_STYLE(0), dpi) };
    (r.right - r.left, r.bottom - r.top)
}

fn state_ptr(hwnd: HWND) -> *mut State {
    // SAFETY: reads the pointer stored by WM_NCCREATE (null before / after).
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State }
}

fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut r = RECT::default();
    // SAFETY: valid window and out pointer.
    let _ = unsafe { GetClientRect(hwnd, &mut r) };
    (r.right - r.left, r.bottom - r.top)
}

fn point_of(l: LPARAM) -> (i32, i32) {
    (
        (l.0 & 0xFFFF) as i16 as i32,
        ((l.0 >> 16) & 0xFFFF) as i16 as i32,
    )
}

fn invalidate(hwnd: HWND) {
    // SAFETY: valid window; whole client.
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn key_down(key: i32) -> bool {
    unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(key) < 0 }
}
fn keyboard_page(hwnd: HWND, page: Page) {
    let dpi = state_mut(hwnd).map(|s| s.dpi).unwrap_or(96);
    let (_, height) = client_size(hwnd);
    let (x, y, w, h) = super::nav_item_rect(page.index(), dpi, height);
    // Use the existing navigation action so enter/leave and backing data remain identical.
    on_click(hwnd, (x + w / 2, y + h / 2));
}

/// Only Alt+Down belongs to settings among system keys; leave Alt+F4/Space to Windows.
fn dispatch_settings_key(message: u32, key: usize, alt: bool) -> bool {
    message == WM_KEYDOWN || (message == WM_SYSKEYDOWN && alt && key == 0x28)
}

/// Alt+Down opens dropdowns only; it must not become ordinary Down on another control.
fn focused_key_allowed(hit: Hit, key: usize, alt: bool) -> bool {
    !alt || key != 0x28 || matches!(hit, Hit::Combo { .. } | Hit::ColorTarget { .. } | Hit::RuleAction(_))
}

/// Production focus reveal, with content-space bounds and a special case for page masters.
fn focused_scroll(
    page: Page,
    hit: Hit,
    rect: RectF,
    dpi: u32,
    height: i32,
    scroll: i32,
    max_scroll: i32,
) -> i32 {
    if matches!(hit, Hit::Nav(_)) {
        return scroll;
    }
    let scale = dpi as f32 / 96.0;
    let master = super::content::content(page)
        .master
        .is_some_and(|f| hit == Hit::Toggle(f));
    let world = RectF::new(
        rect.left / scale,
        (rect.top + scroll as f32) / scale,
        rect.right / scale,
        (rect.bottom + scroll as f32) / scale,
    );
    (super::kit::layout::reveal_focus(
        scroll as f32 / scale,
        world,
        height as f32 / scale,
        max_scroll as f32 / scale,
        master,
    ) * scale)
        .round() as i32
}

fn keyboard(hwnd: HWND, key: usize) -> bool {
    let reverse = key_down(0x10);
    if key == 9 && key_down(0x11) {
        let page = state_mut(hwnd)
            .map(|s| s.view.page)
            .unwrap_or(Page::Welcome);
        let index = Page::ALL.iter().position(|p| *p == page).unwrap_or(0);
        let n = Page::ALL.len();
        keyboard_page(
            hwnd,
            Page::ALL[(index + n + if reverse { n - 1 } else { 1 }) % n],
        );
        if let Some(st) = state_mut(hwnd) {
            st.focus_index = Some(0);
            st.focus_visible = true;
        }
        focus_refresh(hwnd);
        return true;
    }
    let Some(st) = state_mut(hwnd) else {
        return false;
    };
    let (w, h) = client_size(hwnd);
    let targets = super::focus_targets_with(
        &display_settings(st),
        &st.groups,
        st.view.page,
        st.dpi,
        w,
        h,
        st.view.scroll,
    )
    .unwrap_or_default();
    if targets.is_empty() {
        return false;
    }
    if key == 9 {
        let mut focus = super::kit::focus::FocusModel {
            focused: st.focus_index,
            keyboard_visible: st.focus_visible,
        };
        st.focus_index = focus.advance(&vec![true; targets.len()], reverse);
        st.focus_visible = focus.keyboard_visible;
        focus_refresh(hwnd);
        return true;
    }
    // Mouse focus remains active even while its keyboard-only ring is hidden.
    let Some(current) = super::kit::focus::active_index(st.focus_index, targets.len()) else {
        return false;
    };
    let (hit, r) = targets[current];
    if !focused_key_allowed(hit, key, key_down(0x12)) {
        return false;
    }
    if let Hit::LookAction(action) = hit {
        if let LookAction::Slider { kind, .. } = action {
            if matches!(key, 0x20 | 0x0d) {
                return true;
            }
            if matches!(key,0x25..=0x28|0x21..=0x24) {
                let old = look_slider_value(st, kind);
                let max = if kind == SliderKind::Blur { 4 } else { 100 };
                let step = if kind == SliderKind::Blur { 1 } else { 10 };
                let value = match key {
                    0x24 => 0,
                    0x23 => max,
                    0x21 => old + step,
                    0x22 => old - step,
                    0x25 | 0x28 => old - 1,
                    _ => old + 1,
                };
                st.focus_visible = true;
                set_look_slider(hwnd, kind, value, true);
                focus_refresh(hwnd);
                return true;
            }
        }
        if let LookAction::Color { channel, .. } = action {
            if matches!(key, 0x25 | 0x27) {
                let next = if key == 0x25 {
                    current.saturating_sub(1)
                } else {
                    (current + 1).min(targets.len() - 1)
                };
                if matches!(targets[next].0,Hit::LookAction(LookAction::Color{channel:c,..}) if c==channel)
                {
                    st.focus_index = Some(next);
                    st.focus_visible = true;
                    focus_refresh(hwnd);
                }
                return true;
            }
        }
        if matches!(key, 0x25 | 0x27) {
            let delta = if key == 0x25 { -1 } else { 1 };
            let next = match action {
                LookAction::CamouflageMode(_) => Some(LookAction::CamouflageMode((look_pages::camouflage_mode() + delta).clamp(0, 2))),
                LookAction::TintMode(_) => Some(LookAction::TintMode(
                    (look_pages::tint_mode(&st.page.effective()) + delta).clamp(0, 2),
                )),
                LookAction::Style(super::style_menus::Cmd::Darken(_)) => {
                    Some(LookAction::Style(super::style_menus::Cmd::Darken(
                        (st.settings.darken_titlebars as i64 + delta).clamp(0, 2) as u32,
                    )))
                }
                _ => None,
            };
            if let Some(next) = next {
                st.focus_visible = true;
                look_action(hwnd, next, (r.left as i32, r.bottom as i32));
                focus_refresh(hwnd);
                return true;
            }
        }
        if matches!(key, 0x20 | 0x0d) {
            look_action(
                hwnd,
                action,
                ((r.left + r.right) as i32 / 2, (r.top + r.bottom) as i32 / 2),
            );
            focus_refresh(hwnd);
            return true;
        }
    }
    if let Hit::ColorTarget { anchor } = hit {
        if key == 0x28 && key_down(0x12) {
            select_color_target(hwnd, anchor);
            return true;
        }
    }
    if let Hit::RuleAction(action) = hit {
        if matches!(action, Action::DefaultDest { .. })
            && matches!(key, 0x26 | 0x28)
            && !key_down(0x12)
        {
            use crate::round_up::Destination;
            let options: Vec<Destination> = std::iter::once(Destination::Desktop)
                .chain(
                    rules_page::pickable(&st.groups)
                        .iter()
                        .map(|g| Destination::Corral { id: g.id as u64 }),
                )
                .collect();
            let index = options
                .iter()
                .position(|v| *v == st.settings.round_up.default_destination)
                .unwrap_or(0);
            let next = if key == 0x26 {
                index.saturating_sub(1)
            } else {
                (index + 1).min(options.len() - 1)
            };
            st.settings.round_up.default_destination = options[next].clone();
            st.focus_visible = true;
            save(st);
            invalidate(hwnd);
            return true;
        }
        // Alt+Up / Alt+Down move the focused rule (round-up-spec 5).
        if key_down(0x12) && matches!(key, 0x26 | 0x28) {
            if let Action::ToggleRule(i) | Action::EditRule(i) | Action::RuleDest { index: i, .. } = action {
                let up = key == 0x26;
                let moved = if up { i.checked_sub(1) } else { Some(i + 1) };
                let last = st.settings.round_up.rules.len();
                if let Some(to) = moved.filter(|to| *to < last) {
                    rules_action(hwnd, Action::MoveRule { index: i, up });
                    // Keep the focus on the same rule at its new place.
                    if let Some(st) = state_mut(hwnd) {
                        let (w, h) = client_size(hwnd);
                        let again = super::focus_targets_with(
                            &display_settings(st),
                            &st.groups,
                            st.view.page,
                            st.dpi,
                            w,
                            h,
                            st.view.scroll,
                        )
                        .unwrap_or_default();
                        let same_kind = |hit: &Hit| match (action, hit) {
                            (Action::ToggleRule(_), Hit::RuleAction(Action::ToggleRule(n))) => *n == to,
                            (Action::EditRule(_), Hit::RuleAction(Action::EditRule(n))) => *n == to,
                            (Action::RuleDest { .. }, Hit::RuleAction(Action::RuleDest { index: n, .. })) => *n == to,
                            _ => false,
                        };
                        if let Some(at) = again.iter().position(|(hit, _)| same_kind(hit)) {
                            st.focus_index = Some(at);
                            st.focus_visible = true;
                        }
                    }
                    focus_refresh(hwnd);
                }
                return true;
            }
        }
        if key == 0x79 && reverse {
            let context = match action {
                Action::ToggleRule(i) | Action::EditRule(i) | Action::RuleDest { index: i, .. } => {
                    Some(i)
                }
                _ => None,
            };
            if let Some(index) = context {
                rules_action(
                    hwnd,
                    Action::RuleContext {
                        index,
                        anchor: (r.left as i32, r.bottom as i32),
                    },
                );
                return true;
            }
        }
        if matches!(key, 0x20 | 0x0d)
            || (key == 0x28
                && key_down(0x12)
                && matches!(action, Action::DefaultDest { .. } | Action::RuleDest { .. }))
        {
            st.focus_visible = true;
            rules_action(hwnd, action);
            focus_refresh(hwnd);
            return true;
        }
    }
    if !focused_key_allowed(hit, key, key_down(0x12)) {
        return false;
    }
    if matches!(key, 0x20 | 0x0d | 0x21..=0x28) {
        st.focus_visible = true;
    }
    if matches!(hit, Hit::Nav(_)) && matches!(key, 0x26 | 0x28 | 0x24 | 0x23) {
        let index = Page::ALL
            .iter()
            .position(|p| *p == st.view.page)
            .unwrap_or(0);
        let n = Page::ALL.len();
        let next = match key {
            0x24 => 0,
            0x23 => n - 1,
            0x26 => (index + n - 1) % n,
            _ => (index + 1) % n,
        };
        keyboard_page(hwnd, Page::ALL[next]);
        focus_refresh(hwnd);
        return true;
    }
    if hit == Hit::Spacing && matches!(key, 0x26 | 0x28 | 0x21 | 0x22 | 0x24 | 0x23) {
        let value = st.settings.snap_padding as i32;
        let desired = match key {
            0x24 => 0,
            0x23 => 100,
            0x26 => value + 1,
            0x28 => value - 1,
            0x21 => value + 10,
            _ => value - 10,
        };
        super::adjust_spacing(&mut st.settings, desired - value);
        save(st);
        invalidate(hwnd);
        return true;
    }
    if let Some(value) = super::desktop_pages::segment_arrow(hit, key, &st.settings) {
        super::desktop_pages::apply_segment(&mut st.settings, hit, value);
        save(st);
        invalidate(hwnd);
        return true;
    }
    if let Hit::Radio(field, _) = hit {
        if matches!(key, 0x26 | 0x28) || (field == Field::TabsStyle && matches!(key, 0x25 | 0x27)) {
            let group: Vec<_> = targets
                .iter()
                .enumerate()
                .filter(|(_, (h, _))| matches!(h,Hit::Radio(f,_)if *f==field))
                .collect();
            if let Some(position) = group.iter().position(|(i, _)| *i == current) {
                let next = super::desktop_pages::radio_step(key, position, group.len());
                let (index, (_, rect)) = group[next];
                st.focus_index = Some(index);
                on_click(
                    hwnd,
                    (
                        ((rect.left + rect.right) / 2.0) as i32,
                        ((rect.top + rect.bottom) / 2.0) as i32,
                    ),
                );
                focus_refresh(hwnd);
                return true;
            }
        }
    }
    if let Hit::FoldMode(_) = hit {
        if matches!(key, 0x25 | 0x27) {
            let mode = (super::content::fold_open_mode(&st.settings)
                + if key == 0x25 { -1 } else { 1 })
            .clamp(0, 2);
            super::content::apply_fold_open_mode(&mut st.settings, mode);
            save(st);
            invalidate(hwnd);
            return true;
        }
    }
    if let Hit::Combo { field, .. } = hit {
        if matches!(key, 0x26 | 0x28) && !key_down(0x12) {
            let choices = combo_choices(field, super::content::choice_value(&st.settings, field));
            if !choices.is_empty() {
                let index = choices
                    .iter()
                    .position(|(_, v)| *v == super::content::choice_value(&st.settings, field))
                    .unwrap_or(0);
                let next = if key == 0x26 {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(choices.len() - 1)
                };
                let value = choices[next].1;
                if field == Field::BlendIconOpacity {
                    apply_camouflage(st, super::camouflage_options::Change::Field(field, value));
                } else {
                    apply_choice(&mut st.settings, field, value);
                    save(st);
                }
                invalidate(hwnd);
            }
            return true;
        }
    }
    if matches!(
        hit,
        Hit::FoldMode(_) | Hit::AnimationMode(_) | Hit::Radio(Field::QuickHideWhat, _)
    ) && matches!(key, 0x20 | 0x0d)
    {
        return true;
    }
    if matches!(key, 0x20 | 0x0d)
        || (key == 0x28 && key_down(0x12) && matches!(hit, Hit::Combo { .. }))
    {
        on_click(
            hwnd,
            (
                ((r.left + r.right) / 2.0) as i32,
                ((r.top + r.bottom) / 2.0) as i32,
            ),
        );
        focus_refresh(hwnd);
        return true;
    }
    false
}
fn focus_refresh(hwnd: HWND) {
    let Some(st) = state_mut(hwnd) else { return };
    let (w, h) = client_size(hwnd);
    let mut targets = super::focus_targets_with(
        &display_settings(st),
        &st.groups,
        st.view.page,
        st.dpi,
        w,
        h,
        st.view.scroll,
    )
    .unwrap_or_default();
    if let Some(index) = st.focus_index.filter(|i| *i < targets.len()) {
        let (hit, r) = targets[index];
        if !matches!(hit, Hit::Nav(_)) {
            let max =
                max_scroll_with(&st.settings, &st.groups, st.view.page, st.dpi, w, h).unwrap_or(0);
            st.view.scroll = focused_scroll(st.view.page, hit, r, st.dpi, h, st.view.scroll, max);
            clamp_scroll(st, hwnd);
            targets = super::focus_targets_with(
                &display_settings(st),
                &st.groups,
                st.view.page,
                st.dpi,
                w,
                h,
                st.view.scroll,
            )
            .unwrap_or_default();
        }
        super::set_focus_ring(
            st.focus_visible
                .then(|| targets.get(index).map(|v| v.1))
                .flatten(),
        );
    } else {
        super::set_focus_ring(None);
    }
    invalidate(hwnd);
}

fn clamp_scroll(st: &mut State, hwnd: HWND) {
    sync_look_state(st);
    let (w, h) = client_size(hwnd);
    let max = max_scroll_with(&st.settings, &st.groups, st.view.page, st.dpi, w, h).unwrap_or(0);
    st.view.scroll = st.view.scroll.clamp(0, max);
}

fn save(st: &State) {
    let _write = COLOR_SETTINGS_WRITE.lock().unwrap();
    let latest = settings::load_with_backup(&st.path);
    let mut updated = st.settings.clone();
    // Appearance IPC acknowledgements/file fallback belong to the engine/sender, not
    // the page's cached Settings snapshot.
    settings::style_request::merge_protocol(&mut updated, &latest);
    updated.desk_setup_request = latest.desk_setup_request;
    updated.desk_setup_result = latest.desk_setup_result;
    if let Err(e) = settings::save(&st.path, &updated) {
        eprintln!("corral: cannot save settings: {e}");
    }
}

fn now_ms(st: &State) -> u64 {
    st.clock.elapsed().as_millis() as u64
}
fn scope_target(scope: Scope) -> Option<u64> {
    match scope {
        Scope::All => None,
        Scope::Corral(id) => Some(id),
    }
}
fn color_field(channel: ColorChannel) -> ColorField {
    match channel {
        ColorChannel::Background => ColorField::Background,
        ColorChannel::Text => ColorField::Text,
        ColorChannel::Tint => ColorField::TintColor,
    }
}

/// The colour state the page works on: shared values from the settings this window edits, the
/// Corrals from the layout (legacy tint choices migrated as on load, camouflage-spec 5.2).
fn read_color_data(st: &State) -> (color_state::Globals, Vec<CorralInfo>) {
    let mut corrals = settings::style_request::targets(&st.layout_path).unwrap_or_default();
    for corral in &mut corrals {
        corral.style.migrate_tint_source(st.settings.color, st.settings.desktop_tint_icons_color);
    }
    (color_state::Globals::from_settings(&st.settings), corrals)
}

fn submit_live(st: &mut State, hwnd: HWND, request: settings::style_request::LiveRequest) {
    if st.sender.is_none() {
        st.sender = Some(ColorSender::new(hwnd, st.path.clone()));
    }
    if let Some(sender) = &st.sender {
        sender.submit(request);
    }
}

/// What the colour rows, the sample and the scope header show (stored values plus any draft).
fn sync_look_state(st: &State) {
    let e = st.page.effective();
    let corral = matches!(st.page.scope(), Scope::Corral(_));
    look_pages::set_scope(
        corral.then(|| st.page.title()),
        st.page.own_titles(),
        st.page.scope_has_own_look(),
    );
    look_pages::set_camouflage_override((corral && e.own.blend_icons).then_some(e.blend_icons));
    look_pages::set_effective(&e);
}
fn send_look_reload(st: &State) {
    let request = settings::style_request::ReloadRequest::new(st.layout_path.clone());
    std::thread::spawn(move || {
        let _ = settings::style_request::send_reload(&request);
    });
}
fn flush_look_globals(st: &mut State) {
    st.look_pending = false;
    st.look_last_send = Some(std::time::Instant::now());
    save(st);
    crate::tint::set_blur(
        st.settings.background_blur_enabled,
        st.settings.background_blur_radius,
    );
    super::camouflage_preview::refresh_blur_sample();
    send_look_reload(st);
}
fn look_slider_value(st: &State, kind: SliderKind) -> i32 {
    match kind {
        SliderKind::SeeThrough => look_pages::see_through_percent(st.page.effective().see_through),
        SliderKind::Blur => {
            if st.settings.background_blur_enabled {
                look_pages::nearest_blur(st.settings.background_blur_radius) as i32
            } else {
                0
            }
        }
        SliderKind::BlendStrength => {
            look_pages::blend_percent(st.settings.desktop_blend_icon_opacity)
        }
    }
}

// ---- draft frames, commits and cancels (colour-state-spec 3.2, 3.4) ---------------------------

/// Send the open session's latest draft now.
fn send_frame(hwnd: HWND) {
    let Some(st) = state_mut(hwnd) else { return };
    let Some(frame) = st.page.frame() else { return };
    let request = settings::style_request::LiveRequest::new(
        st.layout_path.clone(),
        scope_target(frame.scope),
        frame.session,
        frame.patch,
        false,
    );
    st.framed = Some(frame.session);
    submit_live(st, hwnd, request);
}

/// A new draft value is on the page: repaint, and send a frame at most every 16 ms. Frames the
/// throttle skips are harmless; a commit always carries the page's latest draft.
fn live_frame(hwnd: HWND) {
    let Some(st) = state_mut(hwnd) else { return };
    if st.page.frame().is_none() {
        return;
    }
    let step = st.throttle.offer(now_ms(st));
    match step {
        Step::Send => send_frame(hwnd),
        Step::Wait(ms) => unsafe {
            SetTimer(hwnd, TIMER_LIVE_COLOR, ms.max(1) as u32, None);
        },
    }
}

/// Tell the engine to drop the draft of a session that ends without a commit.
fn cancel_overlay(hwnd: HWND, session: u64, scope: Scope) {
    unsafe {
        let _ = KillTimer(hwnd, TIMER_LIVE_COLOR);
    }
    let Some(st) = state_mut(hwnd) else { return };
    if st.framed == Some(session) {
        st.framed = None;
        let request = settings::style_request::LiveRequest::cancel(st.layout_path.clone(), scope_target(scope), session);
        submit_live(st, hwnd, request);
    }
}

/// Store a commit: the page builds the new copy and hands it to the engine, which writes the
/// layout and settings atomically. A failure comes back as `status::ENGINE_SAVE_FAILED`.
fn send_commit(hwnd: HWND, commit: Commit) {
    let Some(st) = state_mut(hwnd) else { return };
    let layout_path = st.layout_path.clone();
    let mut request = None;
    let outcome = st.page.apply(&commit, &mut |c| {
        request = Some(settings::style_request::LiveRequest::new(
            layout_path.clone(),
            scope_target(c.scope),
            c.session,
            c.patch.clone(),
            true,
        ));
        Ok(())
    });
    unsafe {
        let _ = KillTimer(hwnd, TIMER_LIVE_COLOR);
    }
    let now = now_ms(st);
    st.throttle.flushed(now);
    match (outcome, request) {
        (Ok(()), Some(request)) => {
            if st.framed == Some(commit.session) {
                st.framed = None;
            }
            // `save` writes this copy back: keep its shared colours equal to the page's.
            st.page.globals().write_to(&mut st.settings);
            submit_live(st, hwnd, request);
        }
        _ => {
            // The Corral vanished (or the write was refused): show what is stored.
            reload_color_target(st);
        }
    }
    sync_look_state(st);
}

/// One explicit edit: close any open session first, then store the edit.
fn commit_edit_now(hwnd: HWND, edit: ColorEdit) {
    let commits = match state_mut(hwnd) {
        Some(st) => st.page.commit_edit(edit),
        None => return,
    };
    for commit in commits {
        send_commit(hwnd, commit);
    }
    invalidate(hwnd);
}

/// End the open session: Esc discards, anything else commits when it had an explicit choice.
fn finish_session(hwnd: HWND, cancel: bool) {
    let (session, ended) = {
        let Some(st) = state_mut(hwnd) else { return };
        let session = st.page.session().cloned();
        (session, st.page.end(cancel))
    };
    match ended {
        Ended::Commit(commit) => send_commit(hwnd, commit),
        Ended::Discarded => {
            if let Some(s) = session {
                cancel_overlay(hwnd, s.id, s.scope);
            }
        }
        Ended::Nothing => {}
    }
    if let Some(st) = state_mut(hwnd) {
        sync_look_state(st);
    }
    invalidate(hwnd);
}

/// Open a session for `field` (a session still open commits to its own target first).
fn begin_session(hwnd: HWND, field: ColorField) {
    let closing = match state_mut(hwnd) {
        Some(st) => st.page.begin(field).1,
        None => return,
    };
    if let Some(commit) = closing {
        send_commit(hwnd, commit);
        if let Some(st) = state_mut(hwnd) {
            st.page.begin(field);
        }
    }
}

fn set_look_slider(hwnd: HWND, kind: SliderKind, value: i32, persist: bool) {
    let Some(st) = state_mut(hwnd) else { return };
    if kind == SliderKind::SeeThrough {
        let raw = look_pages::see_through_raw(value);
        if st.page.is_editing() {
            st.page.slide(raw);
            sync_look_state(st);
            if persist {
                finish_session(hwnd, false);
            } else {
                live_frame(hwnd);
            }
        } else if persist {
            // A key step or a plain click: one explicit edit.
            commit_edit_now(hwnd, ColorEdit::SeeThrough(raw));
        } else {
            begin_session(hwnd, ColorField::SeeThrough);
            if let Some(st) = state_mut(hwnd) {
                st.page.slide(raw);
                sync_look_state(st);
            }
            live_frame(hwnd);
        }
    } else {
        if matches!(st.page.scope(), Scope::Corral(_)) && st.view.page == Page::Personalization {
            return;
        }
        if kind == SliderKind::Blur && !persist {
            // Re-blurring the wallpaper on every move froze the window; blur applies on release.
            invalidate_look_slider(hwnd, kind);
            return;
        }
        match kind {
            SliderKind::Blur => {
                let radius = look_pages::blur_radius(value.clamp(0, 4) as usize);
                st.settings.background_blur_radius = radius as f32;
                st.settings.background_blur_enabled = radius > 0;
            }
            SliderKind::BlendStrength => {
                st.settings.desktop_blend_icon_opacity = look_pages::blend_raw(value)
            }
            _ => {}
        }
        let elapsed = st
            .look_last_send
            .map(|t| t.elapsed())
            .unwrap_or(LIVE_INTERVAL);
        if persist || elapsed >= LIVE_INTERVAL {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_LOOK);
            }
            flush_look_globals(st);
        } else {
            st.look_pending = true;
            unsafe {
                SetTimer(
                    hwnd,
                    TIMER_LOOK,
                    (LIVE_INTERVAL - elapsed).as_millis().max(1) as u32,
                    None,
                );
            }
        }
    }
    invalidate(hwnd);
}
fn invalidate_look_slider(hwnd: HWND, kind: SliderKind) {
    if let Some(st) = state_mut(hwnd) {
        st.look_dirty = Some(kind);
    }
    invalidate(hwnd);
}

/// Colour swatch picks: presets and Auto are one explicit edit; Custom opens the popover; the
/// eyedropper previews on the desktop and stores one choice when the pick ends.
fn color_choice(hwnd: HWND, channel: ColorChannel, choice: ColorChoice, anchor: (i32, i32)) {
    match choice {
        ColorChoice::Auto => commit_edit_now(hwnd, ColorEdit::TextAuto),
        ColorChoice::Preset(index) => {
            if let Some(color) = look_pages::preset(channel, index) {
                commit_edit_now(
                    hwnd,
                    match channel {
                        ColorChannel::Background => ColorEdit::Background(color),
                        ColorChannel::Text => ColorEdit::TextCustom(color),
                        ColorChannel::Tint => ColorEdit::TintColor(color),
                    },
                );
            }
        }
        ColorChoice::Custom => {
            let mut screen = windows::Win32::Foundation::POINT { x: anchor.0, y: anchor.1 };
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut screen);
            }
            open_popover(hwnd, channel, screen);
        }
        ColorChoice::Eyedropper => {
            begin_session(hwnd, color_field(channel));
            let picked = crate::screen_picker::pick_with_preview(hwnd, |color| {
                if let Some(st) = state_mut(hwnd) {
                    st.page.preview(color);
                    sync_look_state(st);
                }
                live_frame(hwnd);
                invalidate(hwnd);
            });
            if let Some(color) = picked {
                if let Some(st) = state_mut(hwnd) {
                    st.page.choose(color);
                }
            }
            finish_session(hwnd, picked.is_none());
        }
    }
}

/// The Color popover under the Custom swatch (colour-state-spec 3.3): opening writes nothing,
/// Esc discards, closing commits only after an explicit choice.
fn open_popover(hwnd: HWND, channel: ColorChannel, screen: windows::Win32::Foundation::POINT) {
    begin_session(hwnd, color_field(channel));
    let opened = state_mut(hwnd).and_then(|st| {
        let s = st.page.session()?;
        Some((s.start, st.theme.light, matches!(s.scope, Scope::Corral(_)) && !s.was_own))
    });
    let Some((opening, light, inherited)) = opened else { return };
    let mut live = |event: color_popover::Live| {
        if let Some(st) = state_mut(hwnd) {
            match event {
                color_popover::Live::Preview(color) => st.page.preview(color),
                color_popover::Live::Choose(color) => st.page.choose(color),
            };
            sync_look_state(st);
        }
        live_frame(hwnd);
        invalidate(hwnd);
    };
    let result = color_popover::show(
        hwnd,
        RECT { left: screen.x, top: screen.y, right: screen.x + 1, bottom: screen.y + 1 },
        opening,
        light,
        inherited.then_some(color_popover::INHERITED_CAPTION),
        &mut live,
    );
    finish_session(hwnd, result.cancelled);
    run_pending_focus(hwnd);
}

/// Put the shared colours of `settings` back to what is stored (after a failed commit).
fn restore_shared_colors(settings: &mut Settings, stored: &Settings) {
    color_state::Globals::from_settings(stored).write_to(settings);
}

/// A save did not go through: show what is stored and the error caption (colour-state-spec 3.4).
fn caption_failed(st: &mut State, hwnd: HWND, caption: Caption) {
    // `send_commit` copied the failed shared colours into this window's settings; put the stored
    // ones back before anything saves them.
    let disk = settings::load_with_backup(&st.path);
    restore_shared_colors(&mut st.settings, &disk);
    let (globals, corrals) = read_color_data(st);
    st.page.save_failed(globals, corrals, caption);
    sync_look_state(st);
    invalidate(hwnd);
}

fn reset_appearance(hwnd: HWND) {
    let (light, target, own) = match state_mut(hwnd) {
        Some(st) => (
            st.theme.light,
            match st.page.scope() {
                Scope::Corral(id) => Some((id, st.page.title())),
                Scope::All => None,
            },
            st.page.own_titles(),
        ),
        None => return,
    };
    let Some(action) = super::appearance_reset::show(hwnd, light, target, &own) else {
        return;
    };
    // The user confirmed after any session ended; a session cannot be open behind the dialog.
    finish_session(hwnd, false);
    let sender = state_mut(hwnd).and_then(|st| st.sender.take());
    drop(sender);
    match run_style_request(hwnd, action) {
        Ok(()) => {
            if let Some(st) = state_mut(hwnd) {
                st.page.clear_caption();
                reload_color_target(st);
            }
        }
        Err(_) => {
            if let Some(st) = state_mut(hwnd) {
                caption_failed(st, hwnd, Caption::SaveFailed);
            }
        }
    }
    invalidate(hwnd);
}
fn look_action(hwnd: HWND, action: LookAction, point: (i32, i32)) {
    match action {
        LookAction::Style(cmd) => {
            if state_mut(hwnd).is_some_and(|st| st.page.scope() == Scope::All) {
                apply_style_command(hwnd, cmd);
            }
        }
        LookAction::CamouflageMode(value) => {
            if let Some(Scope::Corral(id)) = state_mut(hwnd).map(|st| st.page.scope()) {
                let value = match value {
                    0 => None,
                    1 => Some(true),
                    2 => Some(false),
                    _ => return,
                };
                match run_style_request(hwnd, settings::style_request::Action::SetCamouflage { id, value }) {
                    Ok(()) => {
                        if let Some(st) = state_mut(hwnd) {
                            reload_color_target(st);
                        }
                    }
                    Err(_) => {
                        if let Some(st) = state_mut(hwnd) {
                            caption_failed(st, hwnd, Caption::SaveFailed);
                        }
                    }
                }
            }
        }
        LookAction::TintMode(value) => {
            if let Some(mode) = TintMode::from_index(value) {
                commit_edit_now(hwnd, ColorEdit::TintMode(mode));
            }
        }
        LookAction::Slider { kind, left, right } => set_look_slider(
            hwnd,
            kind,
            look_pages::slider_value_at(kind, point.0, left, right),
            true,
        ),
        LookAction::Color { channel, choice, anchor } => color_choice(hwnd, channel, choice, anchor),
    }
    invalidate(hwnd);
}

fn display_settings(st: &State) -> Settings {
    sync_look_state(st);
    let mut s = st.settings.clone();
    if st.view.page == Page::Camouflage {
        return s;
    }
    // The rows and the sample read the scope's effective values, draft included.
    let e = st.page.effective();
    s.color = e.color;
    s.text_color = e.text_color;
    s.text_color_auto = e.text_auto;
    s.transparency_factor = e.see_through;
    s.desktop_blend_icons = e.blend_icons;
    s.desktop_blend_corral_backgrounds = e.blend_background;
    s.desktop_tint_icons = e.tint_icons;
    s.desktop_tint_icons_color = e.tint_color;
    s.desktop_tint_icons_color_source = e.tint_source.as_str().into();
    s
}

fn paint(hwnd: HWND, st: &mut State) {
    let (w, h) = client_size(hwnd);
    if w <= 0 || h <= 0 {
        return;
    }
    let size_ok = st
        .surface
        .as_ref()
        .is_some_and(|s| s.width() == w as u32 && s.height() == h as u32);
    if !size_ok {
        st.surface = Surface::new(w as u32, h as u32).ok();
    }
    if st.view.page == Page::Camouflage {
        super::camouflage_preview::enter();
    }
    let shown = display_settings(st);
    if st.focus_visible {
        let targets = super::focus_targets_with(
            &shown,
            &st.groups,
            st.view.page,
            st.dpi,
            w,
            h,
            st.view.scroll,
        )
        .unwrap_or_default();
        super::set_focus_ring(st.focus_index.and_then(|i| targets.get(i).map(|v| v.1)));
    }
    let Some(surface) = st.surface.as_mut() else {
        return;
    };
    let shown_look = st.page.effective();
    let partial = if let Some(kind) = st.look_dirty.take().filter(|_| size_ok) {
        matches!(
            super::render_look_slider_row(
                surface,
                &st.view,
                &shown,
                &st.theme,
                st.dpi,
                Some(&shown_look),
                kind
            ),
            Ok(Some(_))
        )
    } else {
        false
    };
    if !partial {
        let _ = super::render_settings_editor(
            surface,
            &st.view,
            &shown,
            &st.groups,
            &st.theme,
            st.dpi,
            Some(&shown_look),
        );
    }
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: standard BeginPaint/EndPaint pair; the surface DC is a live memory DC of the same size.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let r = ps.rcPaint;
        let _ = BitBlt(
            hdc,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            surface.dc(),
            r.left,
            r.top,
            SRCCOPY,
        );
        // The colour page's caption (colour-state-spec 3.4, 6): the error in the danger colour.
        let caption = matches!(st.view.page, Page::Personalization | Page::Camouflage)
            .then(|| st.page.caption().map(|c| (c.text(), c.is_error())))
            .flatten()
            .or_else(|| (!st.notice.is_empty()).then(|| (st.notice.clone(), false)));
        if let Some((caption, error)) = caption {
            use windows::Win32::Graphics::Gdi::{
                DT_END_ELLIPSIS, DT_LEFT, DT_SINGLELINE, DrawTextW, SetBkMode, SetTextColor,
                TRANSPARENT,
            };
            let _ = SetBkMode(hdc, TRANSPARENT);
            let danger = super::kit::theme::Theme::new(st.theme.light).danger;
            SetTextColor(
                hdc,
                windows::Win32::Foundation::COLORREF(if error {
                    u32::from(danger.r) | (u32::from(danger.g) << 8) | (u32::from(danger.b) << 16)
                } else {
                    0x0080_8080
                }),
            );
            let mut text: Vec<u16> = caption.encode_utf16().collect();
            let mut rect = RECT {
                left: nav_width(st.dpi) + sc(16.0, st.dpi),
                top: h - sc(26.0, st.dpi),
                right: w - sc(12.0, st.dpi),
                bottom: h,
            };
            DrawTextW(
                hdc,
                &mut text,
                &mut rect,
                DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS,
            );
        }
        let _ = EndPaint(hwnd, &ps);
    }
    let lay = if st.view.page == Page::Camouflage {
        super::layout_page(
            st.view.page,
            &shown,
            st.dpi,
            w as f32,
            h as f32,
            st.view.scroll,
        )
        .ok()
    } else {
        None
    };
    super::camouflage_preview::sync(hwnd, lay.as_ref(), st.dpi);
}

/// Settings choices use a D2D surface owned by the popup monitor, never a stretched menu bitmap.
struct ChoicePopup {
    labels: Vec<String>,
    values: Vec<i64>,
    enabled: Vec<bool>,
    checked: Vec<bool>,
    width_dip: f32,
    keyboard_visible: bool,
    selected: usize,
    hover: Option<usize>,
    result: Option<i64>,
    done: bool,
    dpi: u32,
    theme: Theme,
    surface: Option<Surface>,
    search: String,
    last_key: Option<std::time::Instant>,
    first: usize,
}
fn popup_geometry(
    anchor: (i32, i32),
    work: RECT,
    dpi: u32,
    width: f32,
    count: usize,
) -> (i32, i32, i32, i32) {
    let w = sc(width, dpi).min((work.right - work.left).max(1));
    let h = sc(8.0 + count as f32 * 34.0, dpi).min((work.bottom - work.top).max(1));
    (
        anchor.0.clamp(work.left, (work.right - w).max(work.left)),
        anchor.1.clamp(work.top, (work.bottom - h).max(work.top)),
        w,
        h,
    )
}
fn popup_row_rect(
    index: usize,
    first: usize,
    dpi: u32,
    w: i32,
    h: i32,
) -> Option<crate::render::RectF> {
    let row = index.checked_sub(first)?;
    let y = sc(4.0 + row as f32 * 34.0, dpi);
    let bottom = y + sc(32.0, dpi);
    (bottom <= h).then(|| {
        crate::render::RectF::new(
            sc(4.0, dpi) as f32,
            y as f32,
            (w - sc(4.0, dpi)) as f32,
            bottom as f32,
        )
    })
}
fn popup_row_at(
    enabled: &[bool],
    first: usize,
    dpi: u32,
    point: (i32, i32),
    w: i32,
    h: i32,
) -> Option<usize> {
    (first..enabled.len()).find(|&i| {
        enabled[i]
            && popup_row_rect(i, first, dpi, w, h).is_some_and(|r| {
                point.0 as f32 >= r.left
                    && (point.0 as f32) < r.right
                    && point.1 as f32 >= r.top
                    && (point.1 as f32) < r.bottom
            })
    })
}
fn draw_choice_popup(
    ctx: &crate::render::Ctx,
    st: &ChoicePopup,
    w: i32,
    h: i32,
) -> windows::core::Result<()> {
    use crate::render::{Align, RectF, TextStyle};
    let scale = st.dpi as f32 / 96.0;

    let r = RectF::new(0.0, 0.0, w as f32, h as f32);
    ctx.clear(st.theme.group_fill);
    ctx.fill_rounded_rect(r, 8.0 * scale, st.theme.group_fill)?;
    let stroke = sc(1.0, st.dpi) as f32;
    ctx.stroke_rounded_rect(
        RectF::new(
            stroke / 2.0,
            stroke / 2.0,
            w as f32 - stroke / 2.0,
            h as f32 - stroke / 2.0,
        ),
        8.0 * scale,
        stroke,
        st.theme.group_outline,
    )?;
    for (i, label) in st.labels.iter().enumerate().skip(st.first) {
        let Some(r) = popup_row_rect(i, st.first, st.dpi, w, h) else {
            break;
        };
        if st.enabled[i] && st.checked[i] {
            ctx.fill_rounded_rect(r, 4.0 * scale, st.theme.nav_selected)?;
        } else if st.enabled[i]
            && (st.hover == Some(i) || (st.keyboard_visible && i == st.selected))
        {
            ctx.fill_rounded_rect(r, 4.0 * scale, st.theme.nav_hover)?;
        }
        if st.checked[i] {
            ctx.fill_rounded_rect(
                RectF::new(
                    r.left + 8.0 * scale,
                    r.top + 8.0 * scale,
                    r.left + 11.0 * scale,
                    r.top + 24.0 * scale,
                ),
                1.5 * scale,
                st.theme.accent,
            )?;
        }
        ctx.draw_text_cached(
            label,
            RectF::new(
                r.left + 23.0 * scale,
                r.top,
                r.right - 12.0 * scale,
                r.bottom,
            ),
            &TextStyle {
                family: super::kit::theme::font_family(),
                size_px: 14.0 * scale,
                weight: 400,
                color: if !st.enabled[i] {
                    st.theme.nav_text_disabled
                } else if label.starts_with("Delete") || label.starts_with("Remove") {
                    super::kit::theme::Theme::new(st.theme.light).danger
                } else {
                    st.theme.header_text
                },
                align: Align::Left,
                shadow: None,
            },
        )?;
    }
    Ok(())
}

fn popup_paint(hwnd: HWND, st: &mut ChoicePopup) {
    let (w, h) = client_size(hwnd);
    if !st
        .surface
        .as_ref()
        .is_some_and(|s| s.width() == w as u32 && s.height() == h as u32)
    {
        st.surface = Surface::new(w as u32, h as u32).ok();
    }
    if let Some(mut surface) = st.surface.take() {
        let _ = surface.draw(|ctx| draw_choice_popup(ctx, st, w, h));
        let mut ps = PAINTSTRUCT::default();
        unsafe {
            let dc = BeginPaint(hwnd, &mut ps);
            let _ = BitBlt(dc, 0, 0, w, h, surface.dc(), 0, 0, SRCCOPY);
            let _ = EndPaint(hwnd, &ps);
        }
        st.surface = Some(surface);
    }
}
extern "system" fn choice_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        unsafe {
            let cs = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
        }
        return LRESULT(1);
    }
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut ChoicePopup;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    let st = unsafe { &mut *ptr };
    match msg {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            popup_paint(hwnd, st);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            st.keyboard_visible = false;
            st.hover = {
                let (w, h) = client_size(hwnd);
                popup_row_at(&st.enabled, st.first, st.dpi, point_of(lp), w, h)
            };
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            if let Some(i) = {
                let (w, h) = client_size(hwnd);
                popup_row_at(&st.enabled, st.first, st.dpi, point_of(lp), w, h)
            } {
                st.result = Some(st.values[i]);
                st.done = true;
            }
            return LRESULT(0);
        }
        WM_KEYDOWN => {
            st.keyboard_visible = true;
            match wp.0 {
                0x1b => st.done = true,
                0x0d | 0x20 if st.enabled[st.selected] => {
                    st.result = Some(st.values[st.selected]);
                    st.done = true;
                }
                0x26 => st.selected = popup_next_enabled(&st.enabled, st.selected, false),
                0x28 => st.selected = popup_next_enabled(&st.enabled, st.selected, true),
                0x24 => st.selected = st.enabled.iter().position(|v| *v).unwrap_or(st.selected),
                0x23 => st.selected = st.enabled.iter().rposition(|v| *v).unwrap_or(st.selected),
                _ => {}
            }
            let rows =
                ((client_size(hwnd).1 - sc(8.0, st.dpi)) / sc(34.0, st.dpi).max(1)).max(1) as usize;
            if st.selected < st.first {
                st.first = st.selected;
            } else if st.selected >= st.first + rows {
                st.first = st.selected + 1 - rows;
            }
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_CHAR => {
            st.keyboard_visible = true;
            if let Some(c) = char::from_u32(wp.0 as u32).filter(|c| !c.is_control()) {
                if st
                    .last_key
                    .is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(900))
                {
                    st.search.clear();
                }
                st.last_key = Some(std::time::Instant::now());
                st.search.push(c);
                if let Some(i) = st.labels.iter().enumerate().position(|(i, l)| {
                    st.enabled[i] && l.to_lowercase().starts_with(&st.search.to_lowercase())
                }) {
                    st.selected = i;
                    st.first = i;
                    invalidate(hwnd);
                }
            }
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let delta = ((wp.0 >> 16) & 0xffff) as i16;
            st.first = if delta > 0 {
                st.first.saturating_sub(3)
            } else {
                (st.first + 3).min(st.labels.len().saturating_sub(1))
            };
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            st.theme = system_theme();
            st.surface = None;
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_KILLFOCUS | WM_CLOSE => {
            st.done = true;
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            st.dpi = (wp.0 & 0xffff) as u32;
            st.surface = None;
            let r = unsafe { &*(lp.0 as *const RECT) };
            let work = monitor_work(hwnd);
            let (x, y, w, h) =
                popup_geometry((r.left, r.top), work, st.dpi, st.width_dip, st.labels.len());
            unsafe {
                let _ = SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            invalidate(hwnd);
            return LRESULT(0);
        }
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
fn popup_next_enabled(enabled: &[bool], current: usize, forward: bool) -> usize {
    if forward {
        (current + 1..enabled.len())
            .find(|i| enabled[*i])
            .unwrap_or(current)
    } else {
        (0..current).rev().find(|i| enabled[*i]).unwrap_or(current)
    }
}

fn pick_choice(hwnd: HWND, field: Field, current: i64, anchor_client: (i32, i32)) -> Option<i64> {
    let choices = combo_choices(field, current);
    let enabled = vec![true; choices.len()];
    let checked = choices.iter().map(|(_, v)| *v == current).collect();
    let theme = state_mut(hwnd)
        .map(|s| s.theme)
        .unwrap_or_else(system_theme);
    show_choice_items(hwnd, choices, enabled, checked, anchor_client, theme)
}

fn show_choice_items(
    hwnd: HWND,
    choices: Vec<(String, i64)>,
    enabled: Vec<bool>,
    checked: Vec<bool>,
    anchor_client: (i32, i32),
    theme: Theme,
) -> Option<i64> {
    use windows::Win32::Graphics::Gdi::{
        ClientToScreen, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    };
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    if choices.is_empty() || !enabled.iter().any(|v| *v) {
        return None;
    }
    let mut point = windows::Win32::Foundation::POINT {
        x: anchor_client.0,
        y: anchor_client.1,
    };
    unsafe {
        let _ = ClientToScreen(hwnd, &mut point);
    }
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST) };
    let mut work = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let (mut dpi, mut ydpi) = (96, 96);
    unsafe {
        let _ = GetMonitorInfoW(monitor, &mut work);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut ydpi);
    }
    let width = choices
        .iter()
        .map(|(label, _)| {
            crate::render::measure_text(label, super::kit::theme::font_family(), 14.0, 400)
                .map(|v| v.0 + 48.0)
                .unwrap_or(220.0)
        })
        .fold(220.0, f32::max);
    let (x, y, w, h) = popup_geometry((point.x, point.y), work.rcWork, dpi, width, choices.len());
    let mut state = Box::new(ChoicePopup {
        selected: checked
            .iter()
            .enumerate()
            .position(|(i, v)| *v && enabled[i])
            .unwrap_or_else(|| enabled.iter().position(|v| *v).unwrap()),
        enabled,
        checked,
        width_dip: width,
        keyboard_visible: false,
        labels: choices.iter().map(|(s, _)| s.clone()).collect(),
        values: choices.iter().map(|(_, v)| *v).collect(),
        hover: None,
        result: None,
        done: false,
        dpi,
        theme,
        surface: None,
        search: String::new(),
        last_key: None,
        first: 0,
    });
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or_default();
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_DROPSHADOW,
            lpfnWndProc: Some(choice_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("CorralSettingsChoices"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let popup = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("CorralSettingsChoices"),
            w!(""),
            WS_POPUP,
            x,
            y,
            w,
            h,
            hwnd,
            None,
            instance,
            Some((&mut *state as *mut ChoicePopup).cast()),
        )
        .ok()?;
        let corner = 2i32;
        let _ = windows::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            popup,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(33),
            (&corner as *const i32).cast(),
            4,
        );
        let _ = ShowWindow(popup, SW_SHOW);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(popup);
        let mut msg = MSG::default();
        while !state.done {
            let status = GetMessageW(&mut msg, None, 0, 0);
            if status.0 == -1 {
                break;
            }
            if status.0 == 0 || msg.message == WM_QUIT {
                PostQuitMessage(msg.wParam.0 as i32);
                break;
            }
            if matches!(msg.message, WM_LBUTTONDOWN | WM_RBUTTONDOWN) && msg.hwnd != popup {
                state.done = true;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = DestroyWindow(popup);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(hwnd);
    }
    state.result
}

fn spacing_spec(value: u32) -> crate::dialog::DialogSpec {
    let mut spec = crate::dialog::DialogSpec::new(
        "Corral spacing",
        "Enter a whole number from 0 to 100.",
        "Save",
    );
    spec.field = Some(crate::dialog::Field {
        label: "Pixels apart".into(),
        value: value.to_string(),
        hint: "0–100 pixels".into(),
        required: false,
        max_chars: 0,
    });
    spec
}
fn disable_corral_spec() -> crate::dialog::DialogSpec {
    crate::dialog::DialogSpec::new(
        "Turn off Corral?",
        "Your Corrals and Gates disappear and Windows shows the desktop icons. When you turn Corral back on, it puts every icon back in place. Changes made to the desktop meanwhile may be undone.",
        "Turn off",
    )
}
fn confirm_disable(hwnd: HWND) -> bool {
    let light = state_mut(hwnd)
        .map(|st| st.theme.light)
        .unwrap_or_else(|| system_theme().light);
    crate::dialog::show(hwnd, light, disable_corral_spec()).action == crate::dialog::Action::Primary
}

fn on_click(hwnd: HWND, point: (i32, i32)) {
    // SAFETY: the pointer is valid while the window exists; no reference is held across the modal calls below.
    let (hit, dpi, width, height, scroll, page) = {
        let st = unsafe { &*state_ptr(hwnd) };
        let (w, h) = client_size(hwnd);
        let hit = hit_test(
            &display_settings(st),
            st.view.page,
            point,
            st.dpi,
            w,
            h,
            st.view.scroll,
        )
        .unwrap_or(Hit::Nothing);
        (hit, st.dpi, w, h, st.view.scroll, st.view.page)
    };
    if page == Page::Rules && point.0 >= nav_width(dpi) {
        rules_click(hwnd, point);
        return;
    }
    match hit {
        Hit::OpenSettingsFolder => {
            if let Some(st) = state_mut(hwnd) {
                if let Some(folder) = st.path.parent() {
                    unsafe {
                        let _ = windows::Win32::UI::Shell::ShellExecuteW(
                            hwnd,
                            w!("open"),
                            &HSTRING::from(folder.as_os_str()),
                            None,
                            None,
                            SW_SHOWNORMAL,
                        );
                    }
                }
            }
        }
        Hit::AnimationMode(mode) => {
            if let Some(st) = state_mut(hwnd) {
                super::content::apply_animation(&mut st.settings, mode);
                save(st);
                invalidate(hwnd);
            }
        }
        Hit::FoldMode(mode) => {
            if let Some(st) = state_mut(hwnd) {
                super::content::apply_fold_open_mode(&mut st.settings, mode);
                save(st);
                invalidate(hwnd);
            }
        }
        Hit::Expander(page, title) => {
            super::toggle_expander(page, title);
            if let Some(st) = state_mut(hwnd) {
                if !super::desktop_pages::owns(page) {
                    clamp_scroll(st, hwnd);
                }
                let (w, h) = client_size(hwnd);
                if let Ok(lay) = super::layout_page(
                    page,
                    &st.settings,
                    st.dpi,
                    w as f32,
                    h as f32,
                    st.view.scroll,
                ) {
                    if let Some(row) = lay.sections.iter().flat_map(|s| &s.rows).find(|r| {
                        r.row.text == title && r.row.kind == super::content::RowKind::Expander
                    }) {
                        if super::expander_open(page, &row.row) {
                            let scale = st.dpi as f32 / 96.0;
                            let offset = st.view.scroll as f32;
                            let header = crate::render::RectF::new(
                                row.x / scale,
                                (row.y + offset) / scale,
                                (row.x + row.w) / scale,
                                (row.y + sc(48.0, st.dpi) as f32 + offset) / scale,
                            );
                            let first = crate::render::RectF::new(
                                header.left,
                                header.bottom,
                                header.right,
                                header.bottom + 16.0,
                            );
                            let max = max_scroll_with(&st.settings, &st.groups, page, st.dpi, w, h)
                                .unwrap_or(0) as f32
                                / scale;
                            st.view.scroll = (super::kit::layout::expand_scroll(
                                offset / scale,
                                header,
                                first,
                                h as f32 / scale,
                                max,
                            ) * scale)
                                .round() as i32;
                        }
                    }
                }
            }
            invalidate(hwnd);
        }
        Hit::SpacingStep(delta) => {
            if let Some(st) = state_mut(hwnd) {
                if super::adjust_spacing(&mut st.settings, delta) {
                    save(st);
                }
            }
            invalidate(hwnd);
        }
        Hit::Spacing => {
            let (theme, value) = {
                let st = unsafe { &*state_ptr(hwnd) };
                (st.theme, st.settings.snap_padding)
            };
            let result = crate::dialog::show(hwnd, theme.light, spacing_spec(value));
            if result.action == crate::dialog::Action::Primary {
                let text = result.text;
                if let Some(n) = super::parse_spacing(&text) {
                    if let Some(st) = state_mut(hwnd) {
                        st.settings.snap_padding = n;
                        st.settings.snap_should_scale = true;
                        save(st);
                        invalidate(hwnd);
                    }
                } else {
                    notify(
                        hwnd,
                        "Enter a whole number from 0 to 100. Spacing was not changed.",
                        false,
                    );
                }
            }
        }
        Hit::SwapMonitors => {
            if let Err(message) = run_desk_request(hwnd, crate::desk_setup::Action::SwapMonitors) {
                notify(hwnd, &message, false);
            }
            invalidate(hwnd);
        }
        Hit::SavedDeskSetups => show_desk_setups(hwnd),
        Hit::ColorTarget { anchor } => select_color_target(hwnd, anchor),
        Hit::StyleMenu { .. } => {} // Legacy IDs are no longer reachable from the redesigned pages.
        Hit::Help(label) => {
            if let Some(help) = super::help::lookup(label) {
                let light = state_mut(hwnd)
                    .map(|s| s.theme.light)
                    .unwrap_or_else(|| system_theme().light);
                crate::dialog::notice(hwnd, light, help.title, help.body);
            }
        }
        Hit::ResetAppearance => reset_appearance(hwnd),
        Hit::LookAction(action) => look_action(hwnd, action, point),
        Hit::Nothing => {}
        Hit::Nav(p) | Hit::Goto(p) => {
            // SAFETY: see above.
            let st = unsafe { &mut *state_ptr(hwnd) };
            if st.view.page != p {
                st.view.page = p;
                st.view.scroll = 0;
                if p == Page::Backups {
                    backups::expand(backups::load_tiles(&st.layout_path));
                    backups::set_notice(backups::load_notice(&st.layout_path));
                } else {
                    backups::collapse();
                }
                if p == Page::Rules {
                    enter_rules(st);
                }
                if p == Page::Layout && st.settings.get(Field::LayoutPreserve) == 2 {
                    super::desk_setups::refresh(&st.layout_path);
                }
                if p == Page::Personalization {
                    reload_color_target(st);
                }
                if p == Page::Camouflage {
                    super::camouflage_preview::enter();
                } else {
                    super::camouflage_preview::leave();
                }
                if matches!(p, Page::Tabs | Page::Camouflage | Page::Personalization) {
                    refresh_layout_flags(&st.layout_path);
                }
                invalidate(hwnd);
            }
        }
        Hit::Toggle(field) => {
            if matches!(
                field,
                Field::BlendColorOnMouseover
                    | Field::BlendColorOnActive
                    | Field::BlendUsesTintColor
            ) {
                if let Some(st) = state_mut(hwnd) {
                    let value = i64::from(!st.settings.is_on(field));
                    apply_camouflage(st, super::camouflage_options::Change::Field(field, value));
                }
                invalidate(hwnd);
                return;
            }
            if field == Field::BlurSource {
                if let Some(st) = state_mut(hwnd) {
                    let value = i64::from(!st.settings.background_blur_enabled);
                    apply_camouflage(st, super::camouflage_options::Change::Field(field, value));
                }
                invalidate(hwnd);
                return;
            }
            if field == Field::BlendIcons || field == Field::BlendBackgrounds {
                if let Some(st) = state_mut(hwnd) {
                    let on = !st.settings.is_on(field);
                    st.settings.set(field, i64::from(on));
                    flush_look_globals(st);
                    reload_color_target(st);
                }
                invalidate(hwnd);
                return;
            }
            if field == Field::EnableCorral {
                let enabled = unsafe { &*state_ptr(hwnd) }.settings.is_on(field);
                if enabled && !confirm_disable(hwnd) {
                    return;
                }
            }
            // SAFETY: see above.
            let st = unsafe { &mut *state_ptr(hwnd) };
            if apply_click(&mut st.settings, page, point, dpi, width, height, scroll).is_some() {
                save(st);
                if matches!(page, Page::Personalization | Page::Camouflage) {
                    send_look_reload(st);
                }
                if !super::desktop_pages::owns(page) {
                    clamp_scroll(st, hwnd);
                }
                invalidate(hwnd);
            }
        }
        Hit::Radio(..) => {
            // SAFETY: see above.
            let st = unsafe { &mut *state_ptr(hwnd) };
            if apply_click(&mut st.settings, page, point, dpi, width, height, scroll).is_some() {
                save(st);
                if matches!(page, Page::Personalization | Page::Camouflage) {
                    send_look_reload(st);
                }
                if !super::desktop_pages::owns(page) {
                    clamp_scroll(st, hwnd);
                }
                invalidate(hwnd);
            }
        }
        Hit::BackupLink => {
            // SAFETY: see above.
            let st = unsafe { &mut *state_ptr(hwnd) };
            backups::expand(backups::load_tiles(&st.layout_path));
            backups::set_notice(backups::load_notice(&st.layout_path));
            clamp_scroll(st, hwnd);
            invalidate(hwnd);
        }
        Hit::SaveBackup => save_backup(hwnd),
        Hit::ShortcutArrows { anchor } => shortcut_arrows_menu(hwnd, anchor),
        Hit::BackupTile(index) | Hit::BackupRestore(index) => open_tile(hwnd, index),
        Hit::BackupMore(index) => backup_more(hwnd, index, point),
        Hit::RuleAction(action) => rules_action(hwnd, action),
        Hit::Combo { field, rect } => {
            let current =
                super::content::choice_value(&unsafe { &*state_ptr(hwnd) }.settings, field);
            // The menu runs a nested message loop; no reference into the state is alive here.
            if let Some(value) = pick_choice(hwnd, field, current, (rect.0, rect.3)) {
                if field == Field::BlendIconOpacity {
                    if let Some(st) = state_mut(hwnd) {
                        apply_camouflage(
                            st,
                            super::camouflage_options::Change::Field(field, value),
                        );
                    }
                    invalidate(hwnd);
                    return;
                }
                // SAFETY: see above.
                let st = unsafe { &mut *state_ptr(hwnd) };
                if value != current {
                    apply_choice(&mut st.settings, field, value);
                    save(st);
                    if !super::desktop_pages::owns(page) {
                        clamp_scroll(st, hwnd);
                    }
                }
                invalidate(hwnd);
            }
        }
    }
}

// Layout styling is serialized through the same engine-owned settings-file boundary
// as backups. No UI process writes the layout, and no state borrow crosses a message pump.
fn run_style_request(hwnd: HWND, action: settings::style_request::Action) -> Result<(), String> {
    use settings::style_request::Request;
    let prepared = (|| -> Result<_, String> {
        let Some(st) = state_mut(hwnd) else {
            return Err("The settings window was closed.".into());
        };
        st.settings = settings::load_with_backup(&st.path);
        let previous = st
            .settings
            .style_request
            .as_ref()
            .map(|r| r.nonce)
            .max(st.settings.style_result.as_ref().map(|r| r.nonce));
        let nonce = backups::issue_nonce(previous, backups::now_ms());
        st.settings.style_request = Some(Request {
            nonce,
            layout_path: st.layout_path.clone(),
            action,
        });
        st.settings.style_result = None;
        settings::save(&st.path, &st.settings)
            .map_err(|e| format!("Cannot request appearance change: {e}"))?;
        Ok((st.path.clone(), nonce))
    })();
    let (path, nonce) = prepared?;
    unsafe {
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(hwnd, false);
    }
    let start = std::time::Instant::now();
    let mut result = None;
    loop {
        if !pump_messages() || !unsafe { IsWindow(hwnd) }.as_bool() {
            let mut latest = settings::load_with_backup(&path);
            if latest
                .style_request
                .as_ref()
                .is_some_and(|r| r.nonce == nonce)
            {
                latest.style_request = None;
                let _ = settings::save(&path, &latest);
            }
            if unsafe { IsWindow(hwnd) }.as_bool() {
                unsafe {
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(hwnd, true);
                }
            }
            return Err("The settings window was closed.".into());
        }
        let latest = settings::load_with_backup(&path);
        if let Some(answer) = latest.style_result.as_ref().filter(|r| r.nonce == nonce) {
            result = Some((answer.ok, answer.message.clone()));
            if let Some(st) = state_mut(hwnd) {
                st.settings = latest;
            }
            break;
        }
        if start.elapsed().as_millis() >= 5000 {
            let mut latest = latest;
            // Withdraw only our still-pending request. Never erase another writer's request.
            if latest
                .style_request
                .as_ref()
                .is_some_and(|r| r.nonce == nonce)
            {
                latest.style_request = None;
                let _ = settings::save(&path, &latest);
            }
            if let Some(st) = state_mut(hwnd) {
                st.settings = latest;
            }
            break;
        }
        unsafe {
            MsgWaitForMultipleObjects(None, false, 50, QS_ALLINPUT);
        }
    }
    unsafe {
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(hwnd, true);
    }
    match result {
        Some((true, _)) => Ok(()),
        Some((false, message)) => Err(message),
        None => Err("The engine did not confirm this appearance change.".into()),
    }
}

/// The stored data changed under the page (Backups restore, a reset, a removed Corral, an
/// asynchronous save failure): an open session is cancelled without a commit, the scope stays
/// when its Corral still exists, and the rows show what is stored (colour-state-spec 6).
fn reload_color_target(st: &mut State) {
    if let Some(session) = st.framed.take() {
        let scope = st.page.session().map_or_else(|| st.page.scope(), |s| s.scope);
        if let Some(sender) = &st.sender {
            sender.submit(settings::style_request::LiveRequest::cancel(
                st.layout_path.clone(),
                scope_target(scope),
                session,
            ));
        }
    }
    let (globals, corrals) = read_color_data(st);
    let was_editing = st.page.is_editing();
    st.page.reload(globals, corrals);
    if was_editing {
        color_popover::request_close(true);
    }
    sync_look_state(st);
}

// Corral places these controls inline beneath the blur slider.
fn apply_camouflage(st: &mut State, change: super::camouflage_options::Change) {
    // Apply to the window's own copy: re-reading the file dropped unsaved in-memory changes such as the
    // Camouflage master switch (persisted by the engine), so any click here turned Camouflage off.
    // `save` already merges the engine-owned fields from disk.
    super::camouflage_options::apply(&mut st.settings, change);
    save(st);
    crate::tint::set_blur(
        st.settings.background_blur_enabled,
        st.settings.background_blur_radius,
    );
    super::camouflage_preview::refresh_blur_sample();
    let request = settings::style_request::ReloadRequest::new(st.layout_path.clone());
    std::thread::spawn(move || {
        let _ = settings::style_request::send_reload(&request);
    });
}

fn apply_style_command(hwnd: HWND, cmd: super::style_menus::Cmd) {
    use super::style_menus::{self as menus, Notice};
    if cmd == super::style_menus::Cmd::Blur {
        if let Some(st) = state_mut(hwnd) {
            let on = i64::from(!st.settings.background_blur_enabled);
            apply_camouflage(
                st,
                super::camouflage_options::Change::Field(
                    Field::BlurSource,
                    on,
                ),
            );
        }
        return;
    }
    let notice = {
        let Some(st) = state_mut(hwnd) else { return };
        // Same as apply_camouflage: edit the window's copy so engine-persisted switches are not reverted.
        let Some(notice) = menus::apply(&mut st.settings, cmd) else {
            return;
        };
        save(st);
        // Immediate apply: the file stays the record, the engine skips its 2 s poll.
        let request = settings::style_request::ReloadRequest::new(st.layout_path.clone());
        std::thread::spawn(move || {
            let _ = settings::style_request::send_reload(&request);
        });
        match notice {
            Notice::TitleStaysBelow => {
                // The notice is only shown when a group already sits with its titlebar below.
                let below = crate::model::load_with_backup(&st.layout_path)
                    .map(|l| l.groups.iter().any(|g| g.title_side_last == 1))
                    .unwrap_or(false);
                if below {
                    Notice::TitleStaysBelow
                } else {
                    Notice::None
                }
            }
            other => other,
        }
    };
    match notice {
        Notice::ButtonsHidden => {
            let light = state_mut(hwnd)
                .map(|st| st.theme.light)
                .unwrap_or_else(|| system_theme().light);
            crate::dialog::notice(
                hwnd,
                light,
                "Titlebar buttons are hidden",
                "You can still open a Corral's options: right-click its titlebar.",
            );
        }
        Notice::TitleStaysBelow => notify(hwnd, menus::TITLE_STAYS_BELOW_TEXT, true),
        Notice::None => {}
    }
}

fn select_color_target(hwnd: HWND, anchor: (i32, i32)) {
    // Fresh list: a Corral may have been added or removed since the page was opened.
    if let Some(st) = state_mut(hwnd) {
        if !st.page.is_editing() {
            reload_color_target(st);
        }
    }
    let (selected, targets) = {
        let Some(st) = state_mut(hwnd) else { return };
        (st.page.scope(), st.page.corrals().to_vec())
    };
    let mut entries = vec![Entry::Item {
        id: 1,
        text: "All Corrals".into(),
        checked: selected == Scope::All,
        enabled: true,
    }];
    entries.extend(targets.iter().enumerate().map(|(i, t)| Entry::Item {
        id: i + 2,
        text: format!(
            "{}{}",
            t.title,
            if color_state::has_own_look(&t.style) {
                " · own look"
            } else {
                ""
            }
        ),
        checked: selected == Scope::Corral(t.id),
        enabled: true,
    }));
    let Some(choice) = show_corral_menu(hwnd, &entries, anchor) else {
        return;
    };
    let Some(target) = settings::style_request::target_for_choice(choice, &targets) else {
        return;
    };
    let scope = target.map_or(Scope::All, Scope::Corral);
    // The open session (none behind a menu) commits to its own target before the scope moves.
    let closing = state_mut(hwnd).and_then(|st| st.page.set_scope(scope));
    if let Some(commit) = closing {
        send_commit(hwnd, commit);
    }
    if let Some(st) = state_mut(hwnd) {
        clamp_scroll(st, hwnd);
        sync_look_state(st);
    }
    invalidate(hwnd);
}

// ---- the Corral menu's "Colors..." command (colour-state-spec 3.1) -------------------------------

/// Show Personalization for the Corral the menu named and open its Color popover under the
/// Custom swatch. A popover that is still open is closed (committing an explicit choice) first.
fn run_pending_focus(hwnd: HWND) {
    if color_popover::is_open() {
        return;
    }
    let Some(id) = state_mut(hwnd).and_then(|st| st.pending_focus.take()) else {
        return;
    };
    // The command may arrive for a Corral created after this window opened.
    if let Some(st) = state_mut(hwnd) {
        let (globals, corrals) = read_color_data(st);
        if !st.page.is_editing() {
            st.page.reload(globals, corrals);
        }
    }
    let closing = state_mut(hwnd).and_then(|st| st.page.set_scope(Scope::Corral(id)));
    if let Some(commit) = closing {
        send_commit(hwnd, commit);
    }
    let anchor = {
        let Some(st) = state_mut(hwnd) else { return };
        if st.page.scope() != Scope::Corral(id) {
            sync_look_state(st);
            invalidate(hwnd);
            return;
        }
        st.view.page = Page::Personalization;
        st.view.scroll = 0;
        backups::collapse();
        super::camouflage_preview::leave();
        refresh_layout_flags(&st.layout_path);
        clamp_scroll(st, hwnd);
        sync_look_state(st);
        custom_swatch_anchor(st, hwnd)
    };
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let _ = SetForegroundWindow(hwnd);
    }
    invalidate(hwnd);
    if let Some(anchor) = anchor {
        look_action(
            hwnd,
            LookAction::Color { channel: ColorChannel::Background, choice: ColorChoice::Custom, anchor },
            anchor,
        );
    }
}

/// Bottom-left of the Background "Color" row's Custom swatch in client pixels, after scrolling the
/// row into view.
fn custom_swatch_anchor(st: &mut State, hwnd: HWND) -> Option<(i32, i32)> {
    let (w, h) = client_size(hwnd);
    for _ in 0..2 {
        let lay = super::layout_page(
            Page::Personalization,
            &display_settings(st),
            st.dpi,
            w as f32,
            h as f32,
            st.view.scroll,
        )
        .ok()?;
        let row = lay
            .sections
            .iter()
            .flat_map(|section| &section.rows)
            .find(|row| look_pages::channel(&row.row) == Some(ColorChannel::Background))?;
        let swatch = look_pages::custom_swatch(row, st.dpi).ok()??;
        let margin = sc(12.0, st.dpi) as f32;
        if swatch.top >= margin && swatch.bottom <= h as f32 - margin {
            return Some((swatch.left as i32, swatch.bottom as i32));
        }
        st.view.scroll = (st.view.scroll as f32 + swatch.top - margin).max(0.0) as i32;
        clamp_scroll(st, hwnd);
    }
    None
}

/// The scope's Corral was removed, or the page's data was replaced: look for it once a second.
fn check_scope_alive(hwnd: HWND) {
    let Some(st) = state_mut(hwnd) else { return };
    let Scope::Corral(id) = st.page.scope() else { return };
    let alive = crate::model::load_with_backup(&st.layout_path)
        .map(|layout| layout.groups.iter().any(|g| g.id == id))
        .unwrap_or(true);
    if !alive {
        reload_color_target(st);
        invalidate(hwnd);
    }
}

// ---------------------------------------------------------------------------------------------
// Layout backups: the window only reads the store; every change
// is a request in the settings file that the running app executes and answers.

struct Answer {
    ok: bool,
    message: String,
}

fn notify(hwnd: HWND, text: &str, _ok: bool) {
    if !unsafe { IsWindow(hwnd) }.as_bool() {
        return;
    }
    let light = state_mut(hwnd)
        .map(|s| s.theme.light)
        .unwrap_or_else(|| system_theme().light);
    crate::dialog::notice(hwnd, light, "Corral", text);
}

/// Pump pending messages; false when WM_QUIT was seen (it is re-posted for the outer loop).
fn pump_messages() -> bool {
    // SAFETY: standard non-blocking message pump on this thread.
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, HWND::default(), 0, 0, PM_REMOVE).as_bool() {
            if msg.message == WM_QUIT {
                PostQuitMessage(msg.wParam.0 as i32);
                return false;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    true
}

/// The window state, or None once the window is destroyed (the pointer is null then).
fn state_mut<'a>(hwnd: HWND) -> Option<&'a mut State> {
    let ptr = state_ptr(hwnd);
    // SAFETY: non-null pointers are the live state stored by WM_NCCREATE; single threaded.
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &mut *ptr })
    }
}

fn refresh_tiles(hwnd: HWND) {
    let Some(st) = state_mut(hwnd) else { return };
    if backups::expanded() {
        backups::set_tiles(backups::load_tiles(&st.layout_path));
        backups::set_notice(backups::load_notice(&st.layout_path));
    }
    clamp_scroll(st, hwnd);
    invalidate(hwnd);
}

fn show_desk_setups(hwnd: HWND) {
    loop {
        let (layout_path, light) = {
            let Some(st) = state_mut(hwnd) else { return };
            (st.layout_path.clone(), st.theme.light)
        };
        let bank = super::desk_setups::refresh(&layout_path);
        let Some(id) = super::desk_setups::show(hwnd, light, super::desk_setups::rows(&bank))
        else {
            break;
        };
        if let Err(message) = run_desk_request(hwnd, crate::desk_setup::Action::Forget { id }) {
            notify(hwnd, &message, false);
        }
    }
    invalidate(hwnd);
}

/// desk-setups-spec 6: the engine is the sole writer; settings carries a nonce-checked request.
fn run_desk_request(
    hwnd: HWND,
    action: crate::desk_setup::Action,
) -> std::result::Result<(), String> {
    struct DisableOwner(HWND);
    impl Drop for DisableOwner {
        fn drop(&mut self) {
            unsafe {
                if IsWindow(self.0).as_bool() {
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(self.0, true);
                }
            }
        }
    }
    let (settings_path, layout_path, nonce) = {
        let Some(st) = state_mut(hwnd) else {
            return Err("The settings window was closed.".into());
        };
        let _write = COLOR_SETTINGS_WRITE.lock().unwrap();
        let mut latest = settings::load_with_backup(&st.path);
        if latest.desk_setup_request.is_some() {
            return Err(
                "Corral is still processing a saved setup request. Try again in a moment.".into(),
            );
        }
        let nonce = backups::issue_nonce(
            latest.desk_setup_result.as_ref().map(|r| r.nonce),
            backups::now_ms(),
        );
        latest.desk_setup_request = Some(crate::desk_setup::Request {
            nonce,
            layout_path: crate::desk_setup::normalized_path(&st.layout_path),
            action,
        });
        latest.desk_setup_result = None;
        settings::save(&st.path, &latest).map_err(|e| e.to_string())?;
        st.settings = latest;
        send_look_reload(st);
        (st.path.clone(), st.layout_path.clone(), nonce)
    };
    unsafe {
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(hwnd, false);
    }
    let _disabled = DisableOwner(hwnd);
    let started = std::time::Instant::now();
    loop {
        if !pump_messages() || !unsafe { IsWindow(hwnd) }.as_bool() {
            return Err("The settings window was closed.".into());
        }
        let latest = settings::load_with_backup(&settings_path);
        if let Some(result) = latest
            .desk_setup_result
            .as_ref()
            .filter(|r| r.nonce == nonce)
        {
            let answer = if result.ok {
                Ok(())
            } else {
                Err(result.message.clone())
            };
            if let Some(st) = state_mut(hwnd) {
                st.settings = latest;
            }
            super::desk_setups::refresh(&layout_path);
            return answer;
        }
        if started.elapsed() >= std::time::Duration::from_secs(10) {
            let _write = COLOR_SETTINGS_WRITE.lock().unwrap();
            let mut latest = settings::load_with_backup(&settings_path);
            if latest
                .desk_setup_request
                .as_ref()
                .is_some_and(|r| r.nonce == nonce)
            {
                latest.desk_setup_request = None;
                settings::save(&settings_path, &latest).map_err(|e| e.to_string())?;
                if let Some(st) = state_mut(hwnd) {
                    st.settings = latest;
                }
                return Err("Corral did not answer. The saved setup request was withdrawn. Make sure Corral is running and try again.".into());
            }
            return Err("Corral is still processing the saved setup request. Check the list again in a moment.".into());
        }
        unsafe {
            MsgWaitForMultipleObjects(None, false, 100, QS_ALLINPUT);
        }
    }
}

/// Write `action` as a request, then poll the settings file until the app answers with the same nonce
/// or [`backups::TIMEOUT_MS`] pass. The window is disabled while waiting.
fn run_request(hwnd: HWND, action: BackupAction) -> Answer {
    let gone = || Answer {
        ok: false,
        message: "The settings window was closed.".to_string(),
    };
    let (path, nonce) = {
        let Some(st) = state_mut(hwnd) else {
            return gone();
        };
        // Start from the file: the app may have changed settings since this window loaded them.
        st.settings = settings::load_with_backup(&st.path);
        let previous = st
            .settings
            .snapshot_request
            .as_ref()
            .map(|r| r.nonce)
            .max(st.settings.snapshot_result.as_ref().map(|r| r.nonce));
        let nonce = backups::issue_nonce(previous, backups::now_ms());
        st.settings.snapshot_request = Some(backups::new_request(nonce, action));
        st.settings.snapshot_result = None;
        save(st);
        (st.path.clone(), nonce)
    };
    // SAFETY: plain enable call on our window.
    unsafe {
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(hwnd, false);
    }
    let start = std::time::Instant::now();
    let answer = loop {
        // The window may be destroyed while we wait: leave on WM_QUIT or a dead window, and touch nothing.
        // SAFETY: plain window query.
        if !pump_messages() || !unsafe { IsWindow(hwnd) }.as_bool() {
            return gone();
        }
        let latest = settings::load_with_backup(&path);
        let elapsed = start.elapsed().as_millis() as u64;
        let pending = latest
            .snapshot_request
            .as_ref()
            .is_some_and(|r| r.nonce == nonce);
        match backups::poll_decision(latest.snapshot_result.as_ref(), nonce, elapsed, pending) {
            backups::Poll::Done => {
                let result = latest
                    .snapshot_result
                    .clone()
                    .expect("poll_decision saw the result");
                if let Some(st) = state_mut(hwnd) {
                    st.settings = latest;
                }
                break Answer {
                    ok: result.ok,
                    message: result.message,
                };
            }
            backups::Poll::Withdraw => {
                // The app never took it: withdraw it so it cannot run later, when the app starts.
                let mut latest = latest;
                latest.snapshot_request = None;
                if let Err(e) = settings::save(&path, &latest) {
                    eprintln!("corral: cannot save settings: {e}");
                }
                if let Some(st) = state_mut(hwnd) {
                    st.settings = latest;
                }
                break Answer {
                    ok: false,
                    message: backups::TIMEOUT_MESSAGE.to_string(),
                };
            }
            backups::Poll::StillWorking => {
                // The app took the request and is busy: it may still finish; do not suggest a retry.
                if let Some(st) = state_mut(hwnd) {
                    st.settings = latest;
                }
                break Answer {
                    ok: true,
                    message: backups::STILL_WORKING_MESSAGE.to_string(),
                };
            }
            backups::Poll::Pending => {
                // SAFETY: wait for input or 100 ms.
                unsafe {
                    MsgWaitForMultipleObjects(None, false, 100, QS_ALLINPUT);
                }
            }
        }
    };
    // SAFETY: re-enable before any further dialog.
    unsafe {
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow(hwnd, true);
    }
    refresh_tiles(hwnd);
    answer
}

fn shift_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_SHIFT};
    // SAFETY: plain key state query.
    unsafe { GetKeyState(VK_SHIFT.0 as i32) < 0 }
}

/// Saving always asks for a name; only an accepted dialog creates the existing capture request.
fn save_backup(hwnd: HWND) {
    let (theme, dpi) = {
        let st = unsafe { &*state_ptr(hwnd) };
        (st.theme, st.dpi)
    };
    let default = backups::format_taken(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    );
    let Some(title) = backup_dialog::name_dialog(hwnd, &theme, dpi, "Name this backup", &default)
    else {
        return;
    };
    let answer = run_request(hwnd, BackupAction::Capture { title: Some(title) });
    notify(hwnd, &answer.message, answer.ok);
}

fn backup_more(hwnd: HWND, index: usize, anchor: (i32, i32)) {
    let Some(tile) = backups::with_view(|v| v.tiles.get(index).cloned()) else {
        return;
    };
    let (theme, dpi) = {
        let st = unsafe { &*state_ptr(hwnd) };
        (st.theme, st.dpi)
    };
    let entries: Vec<_> = backups::MENU_LABELS
        .iter()
        .enumerate()
        .map(|(i, text)| Entry::Item {
            id: i + 1,
            text: (*text).into(),
            checked: false,
            enabled: true,
        })
        .collect();
    let Some(choice) = show_corral_menu(hwnd, &entries, anchor) else {
        return;
    };
    let Some(action) = backups::menu_action(choice - 1) else {
        return;
    };
    let request = match action {
        backups::MenuAction::Rename => {
            let Some(title) =
                backup_dialog::name_dialog(hwnd, &theme, dpi, "Rename backup", &tile.title)
            else {
                return;
            };
            if title == tile.title {
                return;
            }
            Some(BackupAction::Rename {
                id: tile.id.clone(),
                title,
            })
        }
        backups::MenuAction::Update => backup_dialog::confirm_update(hwnd, &theme, &tile.title)
            .then(|| BackupAction::Recapture {
                id: tile.id.clone(),
            }),
        backups::MenuAction::Delete => backup_dialog::confirm_delete(hwnd, &theme, &tile.title)
            .then(|| BackupAction::Delete {
                id: tile.id.clone(),
            }),
    };
    if let Some(action) = request {
        let answer = run_request(hwnd, action);
        notify(hwnd, &answer.message, answer.ok);
    }
}

/// A click on a tile opens the Restore Backup dialog (Shift suppresses it).
fn open_tile(hwnd: HWND, index: usize) {
    let Some(tile) = backups::with_view(|v| v.tiles.get(index).cloned()) else {
        return;
    };
    let (theme, dpi, skip) = {
        // SAFETY: short-lived read.
        let st = unsafe { &*state_ptr(hwnd) };
        (
            st.theme,
            st.dpi,
            !backups::restore_prompt_needed(&st.settings),
        )
    };
    let outcome = if skip {
        let backup_first =
            state_mut(hwnd).is_some_and(|st| backups::remembered_backup_first(&st.settings));
        backup_dialog::RestoreOutcome {
            renamed: None,
            chosen: Some(Chosen::Restore {
                backup_first,
                never_ask: false,
            }),
        }
    } else {
        backup_dialog::restore_dialog(
            hwnd,
            &theme,
            dpi,
            &tile.title,
            &backups::format_taken(tile.created_ms),
            false,
        )
    };
    if let Some(title) = outcome.renamed {
        let answer = run_request(
            hwnd,
            BackupAction::Rename {
                id: tile.id.clone(),
                title,
            },
        );
        if !answer.ok {
            notify(hwnd, &answer.message, false);
            return;
        }
    }
    let action = match outcome.chosen {
        None => return,
        Some(Chosen::Recapture) => BackupAction::Recapture {
            id: tile.id.clone(),
        },
        Some(Chosen::Delete) => BackupAction::Delete {
            id: tile.id.clone(),
        },
        Some(Chosen::Restore {
            backup_first,
            never_ask,
        }) => {
            if never_ask {
                // SAFETY: short-lived access; saved before the request reloads the file.
                let st = unsafe { &mut *state_ptr(hwnd) };
                backups::remember_restore_choice(&mut st.settings, backup_first, never_ask);
                save(st);
            }
            BackupAction::Restore {
                id: tile.id.clone(),
                backup_first,
            }
        }
    };
    let answer = run_request(hwnd, action);
    notify(hwnd, &answer.message, answer.ok);
}

/// Tile under the cursor (client point) on the revealed Layout page.
fn tile_under(st: &State, hwnd: HWND, point: (i32, i32)) -> Option<usize> {
    if st.view.page != Page::Backups || !backups::expanded() {
        return None;
    }
    let (w, h) = client_size(hwnd);
    match hit_test(
        &display_settings(st),
        st.view.page,
        point,
        st.dpi,
        w,
        h,
        st.view.scroll,
    ) {
        Ok(Hit::BackupTile(i) | Hit::BackupRestore(i) | Hit::BackupMore(i)) => Some(i),
        _ => None,
    }
}

fn create_tooltip(hwnd: HWND, instance: HINSTANCE) -> HWND {
    use windows::Win32::UI::Controls::{
        TOOLTIPS_CLASSW, TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW, TTS_ALWAYSTIP, TTS_NOPREFIX,
        TTTOOLINFOW,
    };
    // SAFETY: tooltip control owned by `hwnd`; the tool info only has to live during the add call.
    unsafe {
        let tip = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASSW,
            None,
            WINDOW_STYLE(WS_POPUP.0 | TTS_ALWAYSTIP | TTS_NOPREFIX),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            hwnd,
            HMENU::default(),
            instance,
            None,
        )
        .unwrap_or_default();
        if !tip.is_invalid() {
            let info = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                hwnd,
                uId: hwnd.0 as usize,
                lpszText: windows::core::PWSTR(-1isize as *mut u16), // LPSTR_TEXTCALLBACKW
                ..Default::default()
            };
            SendMessageW(
                tip,
                TTM_ADDTOOLW,
                WPARAM(0),
                LPARAM(&info as *const _ as isize),
            );
        }
        tip
    }
}

/// Answer to the tooltip's text request: `Taken: <date>` for the hovered tile, empty otherwise.
fn tooltip_text_for(st: &mut State, owner: HWND) -> *mut u16 {
    let mut text = backups::with_view(|v| {
        v.hover
            .and_then(|i| v.tiles.get(i))
            .map(|t| backups::tooltip_text(&backups::format_taken(t.created_ms)))
    })
    .unwrap_or_default();
    if st.view.page == Page::Backups {
        unsafe {
            let mut p = windows::Win32::Foundation::POINT::default();
            if GetCursorPos(&mut p).is_ok() {
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(owner, &mut p);
                let (w, h) = client_size(owner);
                if matches!(
                    hit_test(
                        &st.settings,
                        Page::Backups,
                        (p.x, p.y),
                        st.dpi,
                        w,
                        h,
                        st.view.scroll
                    ),
                    Ok(Hit::BackupMore(_))
                ) {
                    text = "More".into();
                }
            }
        }
    }
    if matches!(st.view.page, Page::Personalization | Page::Camouflage) {
        unsafe {
            let mut p = windows::Win32::Foundation::POINT::default();
            if GetCursorPos(&mut p).is_ok() {
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(owner, &mut p);
                let (w, h) = client_size(owner);
                text = match hit_test(
                    &display_settings(st),
                    st.view.page,
                    (p.x, p.y),
                    st.dpi,
                    w,
                    h,
                    st.view.scroll,
                ) {
                    Ok(Hit::ResetAppearance) => look_pages::reset_label(),
                    Ok(Hit::ColorTarget { .. }) => st.page.title(),
                    Ok(Hit::LookAction(LookAction::Color {
                        choice: ColorChoice::Eyedropper,
                        ..
                    })) => "Eyedropper".into(),
                    Ok(Hit::LookAction(LookAction::Color {
                        choice: ColorChoice::Custom,
                        ..
                    })) => "Custom color".into(),
                    Ok(Hit::LookAction(LookAction::Color {
                        choice: ColorChoice::Auto,
                        ..
                    })) => "Automatic text color".into(),
                    _ => String::new(),
                };
            }
        }
    }
    st.tip_text = text.encode_utf16().chain(std::iter::once(0)).collect();
    st.tip_text.as_mut_ptr()
}

// ---------------------------------------------------------------------------------------------
// Rules page

/// Corrals of the layout file for the pickers (read-only; an unreadable layout gives none).
fn load_groups(layout_path: &Path) -> Vec<GroupInfo> {
    let Ok(layout) = crate::model::load_with_backup(layout_path) else {
        return Vec::new();
    };
    layout
        .groups
        .iter()
        .map(|g| match &g.content {
            crate::model::GroupContent::Items { items } => GroupInfo {
                id: g.id as i32,
                title: g.title.clone(),
                portal: false,
                items: items.len(),
            },
            crate::model::GroupContent::Portal { .. } => GroupInfo {
                id: g.id as i32,
                title: g.title.clone(),
                portal: true,
                items: 0,
            },
        })
        .collect()
}

/// Facts about the layout's Corrals that gate rows of the Tabs and Camouflage pages (read-only).
fn refresh_layout_flags(layout_path: &Path) {
    let flags = crate::model::load_with_backup(layout_path)
        .map(|l| settings::LayoutFlags::from_groups(&l.groups))
        .unwrap_or_default();
    settings::set_layout_flags(flags);
}

thread_local! {
    /// The desktop's facts, read once when the Round-up page opens; commits only re-plan.
    static DESKTOP_FACTS: std::cell::RefCell<Option<Vec<crate::item_facts::ItemFacts>>> =
        const { std::cell::RefCell::new(None) };
}

/// Opening the Round-up page: refresh the Corrals, read the desktop's facts once, and count the
/// icons that cannot be checked.
fn enter_rules(st: &mut State) {
    st.groups = load_groups(&st.layout_path);
    let facts = crate::item_facts::desktop_facts().ok();
    DESKTOP_FACTS.with(|cache| *cache.borrow_mut() = facts);
    rules_page::set_unchecked(count_unchecked(st));
}

/// Icons today's rules cannot decide: a plan over the facts read on page entry (read-only; the
/// layout file is only read, the desktop is not read again).
fn count_unchecked(st: &State) -> usize {
    if !st.settings.round_up.has_work() {
        return 0;
    }
    let Ok(layout) = crate::model::load_with_backup(&st.layout_path) else {
        return 0;
    };
    let Some(zone) = crate::round_up_time::LocalTimeZone::capture() else {
        return 0;
    };
    let clock = crate::round_up_time::Clock::now();
    DESKTOP_FACTS.with(|cache| {
        cache.borrow().as_ref().map_or(0, |facts| {
            crate::round_up::plan(&st.settings.round_up, &layout, facts, clock, &zone, false).unchecked_count()
        })
    })
}

pub(super) enum Entry {
    Item {
        id: usize,
        text: String,
        checked: bool,
        enabled: bool,
    },
    Sep,
    /// Cascading submenu (its items carry the ids).
    Sub {
        text: String,
        items: Vec<Entry>,
    },
}

/// Build the shell menu tree of `entries`.
fn build_menu(entries: &[Entry]) -> Option<windows::Win32::UI::WindowsAndMessaging::HMENU> {
    // SAFETY: `AppendMenuW` copies the strings; the handles are owned by the returned menu tree.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        for e in entries {
            match e {
                Entry::Item {
                    id,
                    text,
                    checked,
                    enabled,
                } => {
                    let mut flags = MF_STRING;
                    if *checked {
                        flags |= MF_CHECKED;
                    }
                    if !*enabled {
                        flags |= MF_GRAYED;
                    }
                    let _ = AppendMenuW(menu, flags, *id, &HSTRING::from(text.as_str()));
                }
                Entry::Sep => {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                }
                Entry::Sub { text, items } => {
                    if let Some(sub) = build_menu(items) {
                        let _ = AppendMenuW(
                            menu,
                            MF_STRING | MF_POPUP,
                            sub.0 as usize,
                            &HSTRING::from(text.as_str()),
                        );
                    }
                }
            }
        }
        Some(menu)
    }
}

/// Popup menu at a client point; returns the chosen item id.
fn show_menu(hwnd: HWND, entries: &[Entry], anchor: (i32, i32)) -> Option<usize> {
    let menu = build_menu(entries)?;
    // SAFETY: the menu tree is local; DestroyMenu also frees its submenus.
    unsafe {
        let mut pt = windows::Win32::Foundation::POINT {
            x: anchor.0,
            y: anchor.1,
        };
        let _ = windows::Win32::Graphics::Gdi::ClientToScreen(hwnd, &mut pt);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN,
            pt.x,
            pt.y,
            0,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        (cmd.0 > 0).then_some(cmd.0 as usize)
    }
}

/// U4 menus reuse the destination-DPI Direct2D choices shell. No settings-state cast: owners may be dialogs.
pub(super) fn show_corral_menu(hwnd: HWND, entries: &[Entry], anchor: (i32, i32)) -> Option<usize> {
    let mut choices = Vec::new();
    let mut enabled = Vec::new();
    let mut checked = Vec::new();
    for entry in entries {
        match entry {
            Entry::Item {
                id,
                text,
                checked: mark,
                enabled: active,
            } => {
                choices.push((text.clone(), *id as i64));
                enabled.push(*active && *id != 0);
                checked.push(*mark);
            }
            Entry::Sep => {}
            Entry::Sub { text, .. } => {
                choices.push((text.clone(), 0));
                enabled.push(false);
                checked.push(false);
            }
        }
    }
    show_choice_items(hwnd, choices, enabled, checked, anchor, system_theme()).map(|v| v as usize)
}

/// Shortcut-arrow menu.
fn shortcut_arrows_menu(hwnd: HWND, _anchor: (i32, i32)) {
    use crate::icon_overlay::{CHOICES, Policy};
    let (raw, light) = {
        let st = unsafe { &*state_ptr(hwnd) };
        (st.settings.hide_shortcut_overlays, st.theme.light)
    };
    let mut spec = crate::dialog::DialogSpec::new(
        "Shortcut arrows",
        "Hide the small arrow on these kinds of shortcuts inside Corrals. Icons on the desktop itself keep their arrows.",
        "Done",
    );
    spec.secondary = None;
    spec.single_button_full_width = false;
    spec.checks = CHOICES
        .iter()
        .map(|(label, bit)| crate::dialog::Checkbox::new(*label, raw & bit != 0))
        .collect();
    crate::dialog::show_live(hwnd, light, spec, &mut |index, _checked| {
        if let Some((_, bit)) = CHOICES.get(index) {
            if let Some(st) = state_mut(hwnd) {
                st.settings.hide_shortcut_overlays =
                    Policy(st.settings.hide_shortcut_overlays).toggle(*bit).0;
                save(st);
                invalidate(hwnd);
            }
        }
    });
}

const ID_FIRST_ICON: usize = 2;
const ID_GROUP_BASE: usize = 100;

/// Corral choices of a destination menu; `current` is the checked Corral.
fn group_entries(groups: &[GroupInfo], current: Option<u64>) -> Vec<Entry> {
    let pickable = rules_page::pickable(groups);
    if pickable.is_empty() {
        let only_portals = !groups.is_empty();
        let mut v = vec![Entry::Item {
            id: 0,
            text: if only_portals {
                "Only Folder Gate groups found."
            } else {
                "No groups found"
            }
            .into(),
            checked: false,
            enabled: false,
        }];
        if only_portals {
            v.push(Entry::Item {
                id: 0,
                text: "Create an ordinary Corral to use Round-up.".into(),
                checked: false,
                enabled: false,
            });
        }
        return v;
    }
    pickable
        .iter()
        .enumerate()
        .map(|(k, g)| Entry::Item {
            id: ID_GROUP_BASE + k,
            text: g.picker_text(),
            checked: current == Some(g.id as u64),
            enabled: true,
        })
        .collect()
}

fn warn(hwnd: HWND, text: &str, yes_no: bool) -> bool {
    let light = state_mut(hwnd)
        .map(|s| s.theme.light)
        .unwrap_or_else(|| system_theme().light);
    if yes_no {
        crate::dialog::confirm(hwnd, light, "Corral", text, "Continue", false)
    } else {
        crate::dialog::notice(hwnd, light, "Corral", text);
        true
    }
}

/// Everything a handler needs from the state, taken before any nested loop starts.
fn snapshot(hwnd: HWND) -> (crate::round_up::RoundUp, Vec<GroupInfo>, Theme) {
    // SAFETY: short-lived read of the live state.
    let st = unsafe { &*state_ptr(hwnd) };
    (st.settings.round_up.clone(), st.groups.clone(), st.theme)
}

/// Change the settings, save, recount the icons that cannot be checked, and repaint.
fn commit_rules(hwnd: HWND, change: impl FnOnce(&mut Settings)) {
    // SAFETY: no nested loop is running; the state is valid.
    let st = unsafe { &mut *state_ptr(hwnd) };
    change(&mut st.settings);
    save(st);
    rules_page::set_unchecked(count_unchecked(st));
    clamp_scroll(st, hwnd);
    invalidate(hwnd);
}

fn rules_click(hwnd: HWND, point: (i32, i32)) {
    let action = {
        // SAFETY: short-lived read.
        let st = unsafe { &*state_ptr(hwnd) };
        let (w, h) = client_size(hwnd);
        rules_page::layout(&st.settings, &st.groups, st.dpi, w as f32, h as f32, st.view.scroll)
            .ok()
            .and_then(|lay| rules_page::hit(&lay, point))
    };
    let Some(action) = action else { return };
    rules_action(hwnd, action);
}

/// Ask where matching icons go: the desktop or one of the Corrals. `None` when cancelled.
fn pick_destination(
    hwnd: HWND,
    groups: &[GroupInfo],
    current: Option<&crate::round_up::Destination>,
    desktop_label: &str,
    anchor: (i32, i32),
) -> Option<crate::round_up::Destination> {
    use crate::round_up::Destination;
    let mut entries = vec![
        Entry::Item {
            id: 1,
            text: desktop_label.into(),
            checked: current == Some(&Destination::Desktop),
            enabled: true,
        },
        Entry::Sep,
    ];
    let current_id = match current {
        Some(Destination::Corral { id }) => Some(*id),
        _ => None,
    };
    entries.extend(group_entries(groups, current_id));
    match show_corral_menu(hwnd, &entries, anchor)? {
        1 => Some(Destination::Desktop),
        id if id >= ID_GROUP_BASE => rules_page::pickable(groups)
            .get(id - ID_GROUP_BASE)
            .map(|g| Destination::Corral { id: g.id as u64 }),
        _ => None,
    }
}

/// Remove a user rule, or put a built-in rule back to its default conditions.
fn remove_or_reset(hwnd: HWND, ru: &crate::round_up::RoundUp, index: usize, light: bool) {
    let Some(rule) = ru.rules.get(index) else {
        return;
    };
    let preset = rule.preset.clone();
    let reset = preset.is_some();
    if !crate::dialog::confirm(
        hwnd,
        light,
        if reset { "Reset rule?" } else { "Remove rule?" },
        if reset {
            "Reset this rule to its default conditions?"
        } else {
            "Remove this Round-up rule?"
        },
        if reset { "Reset" } else { "Remove" },
        !reset,
    ) {
        return;
    }
    commit_rules(hwnd, |s| {
        let rules = &mut s.round_up.rules;
        if index >= rules.len() {
            return;
        }
        match preset.as_deref().and_then(crate::round_up::preset_rule) {
            Some(default) => {
                let (enabled, destination) = (rules[index].enabled, rules[index].destination.clone());
                rules[index] = crate::round_up::Rule {
                    enabled,
                    destination,
                    ..default
                };
            }
            None => {
                rules.remove(index);
            }
        }
    });
}

fn rules_action(hwnd: HWND, action: Action) {
    let (ru, groups, theme) = snapshot(hwnd);
    if ru.paused_reason.is_some() {
        return;
    }
    match action {
        Action::RuleContext { index, anchor } => {
            let Some(rule) = ru.rules.get(index) else {
                return;
            };
            let item = |id: usize, text: &str, enabled: bool| Entry::Item {
                id,
                text: text.into(),
                checked: false,
                enabled,
            };
            let entries = vec![
                item(1, "Move up", index > 0),
                item(2, "Move down", index + 1 < ru.rules.len()),
                Entry::Sep,
                item(3, "Edit\u{2026}", true),
                item(4, "Rename\u{2026}", true),
                item(
                    5,
                    if rule.preset.is_none() {
                        "Delete\u{2026}"
                    } else {
                        "Reset to default\u{2026}"
                    },
                    true,
                ),
            ];
            match show_corral_menu(hwnd, &entries, anchor) {
                Some(1) => rules_action(hwnd, Action::MoveRule { index, up: true }),
                Some(2) => rules_action(hwnd, Action::MoveRule { index, up: false }),
                Some(3) => rules_action(hwnd, Action::EditRule(index)),
                Some(4) => {
                    let mut renamed = rule.clone();
                    if rules_dialog::rename_rule(hwnd, &mut renamed, &theme) {
                        commit_rules(hwnd, |s| {
                            if let Some(slot) = s.round_up.rules.get_mut(index) {
                                slot.name = renamed.name;
                            }
                        });
                    }
                }
                Some(5) => remove_or_reset(hwnd, &ru, index, theme.light),
                _ => {}
            }
        }
        Action::MoveRule { index, up } => {
            if index < ru.rules.len() {
                commit_rules(hwnd, |s| {
                    s.round_up.move_rule(index, up);
                });
            }
        }
        Action::ToggleRule(index) => {
            let Some(rule) = ru.rules.get(index) else {
                return;
            };
            if rule.enabled {
                commit_rules(hwnd, |s| s.round_up.rules[index].enabled = false);
                return;
            }
            // Switching a rule on needs a Corral that exists (or the desktop): ask first.
            let mut rule = rule.clone();
            let usable = rule.destination.is_some()
                && rules_page::destination_text(rule.destination.as_ref(), &groups) != "Corral missing";
            if !usable {
                let anchor = (nav_width(unsafe { &*state_ptr(hwnd) }.dpi) + 48, 160);
                let current = rule.destination.as_ref();
                let Some(chosen) =
                    pick_destination(hwnd, &groups, current, "Leave on the desktop", anchor)
                else {
                    return;
                };
                rule.destination = Some(chosen);
            }
            rule.enabled = true;
            if rule.validate().is_err() {
                // Something else is incomplete: let the user finish the rule instead.
                match rules_dialog::edit_rule(hwnd, &rule, false, &groups, &ru.rules[..index], &theme) {
                    EditOutcome::Ok(edited) => {
                        rule = crate::round_up::Rule {
                            enabled: true,
                            ..edited
                        }
                    }
                    _ => return,
                }
            }
            commit_rules(hwnd, |s| s.round_up.rules[index] = rule);
        }
        Action::EditRule(index) => {
            let Some(rule) = ru.rules.get(index) else {
                return;
            };
            match rules_dialog::edit_rule(hwnd, rule, false, &groups, &ru.rules[..index], &theme) {
                EditOutcome::Ok(edited) => commit_rules(hwnd, |s| {
                    if index < s.round_up.rules.len() {
                        s.round_up.rules[index] = edited;
                    }
                }),
                EditOutcome::Delete => commit_rules(hwnd, |s| {
                    if index < s.round_up.rules.len() {
                        s.round_up.rules.remove(index);
                    }
                }),
                EditOutcome::Cancel => {}
            }
        }
        Action::RuleDest { index, anchor } => {
            let Some(rule) = ru.rules.get(index) else {
                return;
            };
            let current = rule.destination.as_ref();
            if let Some(chosen) =
                pick_destination(hwnd, &groups, current, "Leave on the desktop", anchor)
            {
                commit_rules(hwnd, |s| s.round_up.rules[index].destination = Some(chosen));
            }
        }
        Action::DefaultDest { anchor } => {
            let current = Some(&ru.default_destination);
            if let Some(chosen) =
                pick_destination(hwnd, &groups, current, "Leave it on the desktop", anchor)
            {
                commit_rules(hwnd, |s| s.round_up.default_destination = chosen);
            }
        }
        Action::AddRule { .. } => {
            let fresh = ru.new_user_rule();
            if let EditOutcome::Ok(rule) =
                rules_dialog::edit_rule(hwnd, &fresh, true, &groups, &ru.rules, &theme)
            {
                commit_rules(hwnd, |s| {
                    s.round_up.insert_rule(rule);
                });
            }
        }
        Action::RewriteReview(index) => {
            let Some(item) = ru.needs_review.get(index) else {
                return;
            };
            let prefill = item.prefill(ru.new_rule_id());
            // The old position, counted among the converted rules: review items are not in the
            // list, so those still waiting before this one shift its slot.
            let waiting_before = ru
                .needs_review
                .iter()
                .filter(|r| r.position < item.position)
                .count();
            let slot = item
                .position
                .saturating_sub(waiting_before)
                .min(ru.rules.len());
            if let EditOutcome::Ok(rule) =
                rules_dialog::edit_rule(hwnd, &prefill, true, &groups, &ru.rules[..slot], &theme)
            {
                commit_rules(hwnd, |s| {
                    if index < s.round_up.needs_review.len() {
                        s.round_up.needs_review.remove(index);
                    }
                    let slot = slot.min(s.round_up.rules.len());
                    s.round_up.rules.insert(slot, rule);
                });
            }
        }
        Action::DismissReview(index) => {
            if crate::dialog::confirm(
                hwnd,
                theme.light,
                "Dismiss this rule?",
                "It will not be converted. A copy of your old settings stays next to your settings file.",
                "Dismiss",
                true,
            ) {
                commit_rules(hwnd, |s| {
                    if index < s.round_up.needs_review.len() {
                        s.round_up.needs_review.remove(index);
                    }
                });
            }
        }
        Action::ApplyNow => {
            // The running app shows the preview; let it come to the front.
            // SAFETY: plain call, no handles involved.
            unsafe {
                let _ = AllowSetForegroundWindow(ASFW_ANY);
            }
            commit_rules(hwnd, |s| s.apply_rules_now_requested = true);
        }
        Action::KeepApplied => commit_rules(hwnd, |s| s.always_apply_rules = !s.always_apply_rules),
    }
}

extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: lparam is the CREATESTRUCTW of this message; lpCreateParams is our boxed State.
        unsafe {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
    }
    let ptr = state_ptr(hwnd);
    if ptr.is_null() {
        // SAFETY: default processing.
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }
    match msg {
        WM_CONTEXTMENU => {
            let st = unsafe { &*ptr };
            if st.view.page == Page::Rules {
                let (w, h) = client_size(hwnd);
                let action = if lparam.0 == -1 {
                    super::focus_targets_with(
                        &st.settings,
                        &st.groups,
                        Page::Rules,
                        st.dpi,
                        w,
                        h,
                        st.view.scroll,
                    )
                    .ok()
                    .and_then(|targets| st.focus_index.and_then(|i| targets.get(i).copied()))
                    .and_then(|(hit, r)| match hit {
                        Hit::RuleAction(
                            Action::ToggleRule(index)
                            | Action::EditRule(index)
                            | Action::RuleDest { index, .. },
                        ) => Some(Action::RuleContext {
                            index,
                            anchor: (r.left as i32, r.bottom as i32),
                        }),
                        _ => None,
                    })
                } else {
                    let mut p = windows::Win32::Foundation::POINT {
                        x: point_of(lparam).0,
                        y: point_of(lparam).1,
                    };
                    unsafe {
                        let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut p);
                    }
                    rules_page::layout(
                        &st.settings,
                        &st.groups,
                        st.dpi,
                        w as f32,
                        h as f32,
                        st.view.scroll,
                    )
                    .ok()
                    .and_then(|lay| rules_page::context_at(&lay, (p.x, p.y)))
                };
                if let Some(action) = action {
                    rules_action(hwnd, action);
                    return LRESULT(0);
                }
            }
        }
        WM_KEYDOWN | WM_SYSKEYDOWN
            if dispatch_settings_key(msg, wparam.0, key_down(0x12)) && keyboard(hwnd, wparam.0) =>
        {
            return LRESULT(0);
        }
        settings::style_request::WM_PREVIEW_CHANGED => {
            if unsafe { (*ptr).view.page == Page::Camouflage } {
                invalidate(hwnd);
            }
            return LRESULT(0);
        }
        WM_COLOR_STATUS => {
            let st = unsafe { &mut *ptr };
            // The engine tells every settings window; only the one that owns the session shows it.
            if lparam.0 != 0 && color_state::session_owner(lparam.0 as u64) != std::process::id() & 0xffff {
                return LRESULT(0);
            }
            match wparam.0 {
                status::APPLIED => st.notice.clear(),
                status::QUEUED_IN_FILE => {
                    st.notice =
                        "Engine confirmation unavailable. Appearance change queued in settings.".into()
                }
                // colour-state-spec 3.4: the previous colors are back, with a caption.
                status::FILE_SAVE_FAILED | status::ENGINE_SAVE_FAILED => {
                    st.notice.clear();
                    caption_failed(st, hwnd, Caption::SaveFailed);
                }
                status::PREVIEW_ONLY => {
                    st.notice.clear();
                    caption_failed(st, hwnd, Caption::PreviewOnly);
                }
                _ => {}
            }
            invalidate(hwnd);
            return LRESULT(0);
        }
        crate::camouflage::runtime::WM_READY => {
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_FOCUS_CORRAL => {
            if color_popover::is_open() {
                // The popover commits an explicit choice to its own Corral, then the focus follows.
                color_popover::request_close(false);
            } else {
                run_pending_focus(hwnd);
            }
            return LRESULT(0);
        }
        WM_COPYDATA => {
            use windows::Win32::System::DataExchange::COPYDATASTRUCT;
            if lparam.0 != 0 {
                let data = unsafe { std::ptr::read_unaligned(lparam.0 as *const COPYDATASTRUCT) };
                if data.dwData == settings::style_request::FOCUS_MAGIC {
                    let valid = data.cbData != 0
                        && data.cbData as usize <= settings::style_request::FOCUS_MAX_BYTES * 4
                        && !data.lpData.is_null();
                    let request = valid
                        .then(|| unsafe {
                            std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize)
                        })
                        .and_then(|bytes| settings::style_request::FocusRequest::decode(data.dwData, bytes).ok());
                    let st = unsafe { &mut *ptr };
                    return match request {
                        Some(r) if settings::style_request::same_layout(&r.layout_path, &st.layout_path) => {
                            st.pending_focus = Some(r.corral);
                            unsafe {
                                let _ = PostMessageW(hwnd, WM_FOCUS_CORRAL, WPARAM(0), LPARAM(0));
                            }
                            LRESULT(1)
                        }
                        Some(_) => LRESULT(0),
                        None => LRESULT(2),
                    };
                }
            }
        }
        WM_TIMER if wparam.0 == TIMER_DESK_SETUP => {
            if let Some(st) = state_mut(hwnd) {
                if st.view.page == Page::Layout && st.settings.get(Field::LayoutPreserve) == 2 {
                    let previous = super::desk_setups::status();
                    super::desk_setups::refresh(&st.layout_path);
                    if previous != super::desk_setups::status() {
                        clamp_scroll(st, hwnd);
                        invalidate(hwnd);
                    }
                }
            }
            check_scope_alive(hwnd);
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == TIMER_LOOK => {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_LOOK);
            }
            if let Some(st) = state_mut(hwnd) {
                if st.look_pending {
                    flush_look_globals(st);
                }
            }
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == TIMER_LIVE_COLOR => {
            unsafe {
                let _ = KillTimer(hwnd, TIMER_LIVE_COLOR);
            }
            let due = state_mut(hwnd).is_some_and(|st| {
                let now = now_ms(st);
                st.throttle.fire(now)
            });
            if due {
                send_frame(hwnd);
            }
            return LRESULT(0);
        }
        WM_KEYDOWN if wparam.0 == 0x1b => {
            let look = unsafe { (*ptr).look_drag.take() };
            if let Some((kind, _, _)) = look {
                look_pages::set_slider_pointer(None);
                if kind == SliderKind::SeeThrough {
                    // Esc: discard the draft, stored data untouched (colour-state-spec 3.4).
                    finish_session(hwnd, true);
                } else if let Some(st) = state_mut(hwnd) {
                    flush_look_globals(st);
                }
                unsafe {
                    (*ptr).pressed = false;
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
                }
                invalidate(hwnd);
                return LRESULT(0);
            }
        }
        WM_CAPTURECHANGED | WM_CANCELMODE => {
            super::set_control_pointer(None, false);
            let st = unsafe { &mut *ptr };
            st.pressed = false;
            let look = st.look_drag.take();
            if let Some((kind, _, _)) = look {
                look_pages::set_slider_pointer(None);
                if kind == SliderKind::SeeThrough {
                    finish_session(hwnd, false);
                } else {
                    flush_look_globals(st);
                }
                invalidate(hwnd);
            }
            if msg == WM_CANCELMODE {
                unsafe {
                    if windows::Win32::UI::Input::KeyboardAndMouse::GetCapture() == hwnd {
                        let _ = windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
                    }
                }
            }
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            // SAFETY: pointer valid while the window exists.
            paint(hwnd, unsafe { &mut *ptr });
            return LRESULT(0);
        }
        WM_SIZE => {
            // SAFETY: as above.
            clamp_scroll(unsafe { &mut *ptr }, hwnd);
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_GETMINMAXINFO => {
            // SAFETY: lparam points at a MINMAXINFO.
            unsafe {
                let dpi = GetDpiForWindow(hwnd).max(96);
                let (w, h) = outer_size(MIN_CLIENT, dpi);
                let info = &mut *(lparam.0 as *mut MINMAXINFO);
                info.ptMinTrackSize.x = w;
                info.ptMinTrackSize.y = h;
            }
            return LRESULT(0);
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED => {
            let st = unsafe { &mut *ptr };
            st.theme = system_theme();
            caption_theme(hwnd, &st.theme);
            st.surface = None;
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_DISPLAYCHANGE => {
            super::camouflage_preview::reset();
            super::sample_corral::reset();
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            super::camouflage_preview::reset();
            super::sample_corral::reset();
            // SAFETY: lparam points at the suggested RECT; state pointer valid.
            unsafe {
                let st = &mut *ptr;
                st.dpi = (wparam.0 & 0xFFFF) as u32;
                crate::app_icon::set_for_dpi(hwnd, st.dpi);
                caption_theme(hwnd, &st.theme);
                let r = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                st.surface = None;
                clamp_scroll(st, hwnd);
            }
            invalidate(hwnd);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            // SAFETY: state pointer valid; no nested calls hold references.
            let st = unsafe { &mut *ptr };
            let p = point_of(lparam);
            if let Some((kind, left, right)) = st.look_drag {
                let value = look_pages::slider_value_at(kind, p.0, left, right);
                look_pages::set_slider_pointer(Some((kind, p.0.clamp(left, right) as f32)));
                set_look_slider(hwnd, kind, value, false);
                if let Some(st) = state_mut(hwnd) {
                    st.look_dirty = Some(kind);
                }
                invalidate_look_slider(hwnd, kind);
                return LRESULT(0);
            }
            super::set_control_pointer(Some((p.0 as f32, p.1 as f32)), st.pressed);
            if matches!(
                st.view.page,
                Page::Rules | Page::Personalization | Page::Camouflage
            ) {
                invalidate(hwnd);
            }
            let (w, h) = client_size(hwnd);
            let hovered = hit_test(
                &display_settings(st),
                st.view.page,
                p,
                st.dpi,
                w,
                h,
                st.view.scroll,
            )
            .unwrap_or(Hit::Nothing);
            if hovered != st.hover_hit {
                st.hover_hit = hovered;
                invalidate(hwnd);
            }
            if !st.tracking {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                // SAFETY: valid TRACKMOUSEEVENT.
                st.tracking = unsafe { TrackMouseEvent(&mut tme) }.is_ok();
            }
            let hover = super::nav_hit_test(p, st.dpi, h);
            if hover != st.view.hover {
                st.view.hover = hover;
                invalidate(hwnd);
            }
            if st.view.page == Page::Backups && backups::expanded() {
                let tile = tile_under(st, hwnd, p);
                if tile != backups::with_view(|v| v.hover) {
                    backups::with_view(|v| v.hover = tile);
                    if !st.tip.is_invalid() {
                        // SAFETY: pop a visible tooltip so the next tile asks for its own text.
                        unsafe {
                            SendMessageW(
                                st.tip,
                                windows::Win32::UI::Controls::TTM_POP,
                                WPARAM(0),
                                LPARAM(0),
                            )
                        };
                    }
                    invalidate(hwnd);
                }
            }
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            super::set_control_pointer(None, false);
            // SAFETY: as above.
            let st = unsafe { &mut *ptr };
            st.tracking = false;
            if st.hover_hit != Hit::Nothing {
                st.hover_hit = Hit::Nothing;
                invalidate(hwnd);
            }
            let nav_hover = st.view.hover.take().is_some();
            let tile_hover = backups::with_view(|v| v.hover.take()).is_some();
            if nav_hover
                || tile_hover
                || matches!(
                    st.view.page,
                    Page::Rules | Page::Personalization | Page::Camouflage
                )
            {
                invalidate(hwnd);
            }
            return LRESULT(0);
        }
        WM_LBUTTONDOWN => {
            let point = point_of(lparam);
            super::set_control_pointer(Some((point.0 as f32, point.1 as f32)), true);
            invalidate(hwnd);
            super::set_focus_ring(None);
            let st = unsafe { &mut *ptr };
            st.focus_visible = false;
            let (w, h) = client_size(hwnd);
            let targets = super::focus_targets_with(
                &display_settings(st),
                &st.groups,
                st.view.page,
                st.dpi,
                w,
                h,
                st.view.scroll,
            )
            .unwrap_or_default();
            let click = point_of(lparam);
            st.focus_index = targets.iter().position(|(_, r)| {
                click.0 as f32 >= r.left
                    && (click.0 as f32) < r.right
                    && click.1 as f32 >= r.top
                    && (click.1 as f32) < r.bottom
            });
            if let Ok(Hit::LookAction(LookAction::Slider { kind, left, right })) = hit_test(
                &display_settings(st),
                st.view.page,
                point,
                st.dpi,
                w,
                h,
                st.view.scroll,
            ) {
                st.look_drag = Some((kind, left, right));
                st.pressed = true;
                look_pages::set_slider_pointer(Some((kind, point.0.clamp(left, right) as f32)));
                unsafe {
                    windows::Win32::UI::Input::KeyboardAndMouse::SetCapture(hwnd);
                }
                set_look_slider(
                    hwnd,
                    kind,
                    look_pages::slider_value_at(kind, point.0, left, right),
                    false,
                );
                invalidate(hwnd);
                return LRESULT(0);
            }
            // SAFETY: as above.
            unsafe {
                (*ptr).pressed = true;
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let point = point_of(lparam);
            super::set_control_pointer(Some((point.0 as f32, point.1 as f32)), false);
            if let Some((kind, left, right)) = unsafe { (*ptr).look_drag.take() } {
                unsafe {
                    (*ptr).pressed = false;
                    let _ = windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture();
                }
                look_pages::set_slider_pointer(None);
                set_look_slider(
                    hwnd,
                    kind,
                    look_pages::slider_value_at(kind, point.0, left, right),
                    true,
                );
                invalidate(hwnd);
                return LRESULT(0);
            }
            // SAFETY: as above.
            let was = unsafe { std::mem::replace(&mut (*ptr).pressed, false) };
            if was {
                on_click(hwnd, point_of(lparam));
                invalidate(hwnd);
            }
            return LRESULT(0);
        }
        WM_NOTIFY => {
            // SAFETY: lparam points at an NMHDR; for TTN_GETDISPINFOW it is an NMTTDISPINFOW.
            unsafe {
                let hdr = &*(lparam.0 as *const windows::Win32::UI::Controls::NMHDR);
                if hdr.code == windows::Win32::UI::Controls::TTN_GETDISPINFOW {
                    let info = &mut *(lparam.0 as *mut windows::Win32::UI::Controls::NMTTDISPINFOW);
                    info.lpszText = windows::core::PWSTR(tooltip_text_for(&mut *ptr, hwnd));
                    return LRESULT(0);
                }
            }
        }
        WM_MOUSEWHEEL => {
            // SAFETY: as above.
            let st = unsafe { &mut *ptr };
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16 as i32;
            let step =
                (delta as f32 / 120.0 * WHEEL_STEP_DIP * st.dpi as f32 / 96.0).round() as i32;
            let before = st.view.scroll;
            st.view.scroll -= step;
            clamp_scroll(st, hwnd);
            if st.view.scroll != before {
                invalidate(hwnd);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            if let Some(st) = state_mut(hwnd) {
                if st.look_pending {
                    flush_look_globals(st);
                }
            }
            unsafe {
                let _ = KillTimer(hwnd, TIMER_LOOK);
            }
            super::camouflage_preview::reset();
            super::sample_corral::reset();
            crate::tint::set_notify_window(0);
            // SAFETY: ends the message loop.
            unsafe { PostQuitMessage(0) };
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            // SAFETY: reclaim the box leaked in `run_settings_window`; the pointer is cleared first.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                drop(Box::from_raw(ptr));
            }
            return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
        }
        _ => {}
    }
    // SAFETY: default processing.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The colour page at start: shared values from the settings, Corrals from the layout, scope
/// All Corrals until the Corral menu names one.
fn initial_page(settings: &Settings, layout_path: &Path) -> ColorPage {
    let mut corrals = settings::style_request::targets(layout_path).unwrap_or_default();
    for corral in &mut corrals {
        corral.style.migrate_tint_source(settings.color, settings.desktop_tint_icons_color);
    }
    let seed = color_state::session_seed(std::process::id(), backups::now_ms());
    ColorPage::new(color_state::Globals::from_settings(settings), corrals, Scope::All, seed)
}

/// Open the settings window and run its message loop until it is closed. Returns the exit code.
pub fn run_settings_window(
    settings_path: &Path,
    layout_path: &Path,
    start_page: Option<Page>,
) -> i32 {
    // SAFETY: standard window class registration / creation on this thread.
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(
            None,
            windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
        );
        let instance: HINSTANCE = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or_default();
        let (icon, icon_small) = crate::app_icon::class_icons();
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hIcon: icon,
            hIconSm: icon_small,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("CorralSettings"),
            ..Default::default()
        };
        RegisterClassExW(&class);

        let dpi = GetDpiForSystem().max(96);
        let loaded = settings::load_with_backup(settings_path);
        let page = initial_page(&loaded, layout_path);
        let state = Box::new(State {
            settings: loaded,
            path: settings_path.to_path_buf(),
            layout_path: layout_path.to_path_buf(),
            groups: Vec::new(),
            view: View {
                page: start_page.unwrap_or(Page::Welcome),
                hover: None,
                scroll: 0,
            },
            dpi,
            theme: system_theme(),
            surface: None,
            look_drag: None,
            look_dirty: None,
            look_last_send: None,
            look_pending: false,
            pressed: false,
            focus_index: Some(0),
            focus_visible: false,
            hover_hit: Hit::Nothing,
            page,
            throttle: Throttle::new(LIVE_INTERVAL.as_millis() as u64),
            clock: std::time::Instant::now(),
            framed: None,
            pending_focus: None,
            sender: None,
            notice: String::new(),
            tracking: false,
            tip: HWND::default(),
            tip_text: vec![0],
        });
        let raw = Box::into_raw(state);
        // Only a settings process started by the Corral menu's "Colors..." command receives this
        // target (colour-state-spec 3.1); a missing or deleted Corral is ignored when it runs.
        let menu_target = std::env::var("CORRAL_SETTINGS_COLOR_TARGET")
            .ok()
            .and_then(|target| target.parse::<u64>().ok());
        sync_look_state(&*raw);
        super::camouflage_preview::reset();
        super::sample_corral::reset();
        if (*raw).view.page == Page::Camouflage {
            super::camouflage_preview::enter();
        }
        crate::menu_theme::apply();
        if (*raw).view.page == Page::Rules {
            enter_rules(&mut *raw);
        }
        if matches!(
            (*raw).view.page,
            Page::Tabs | Page::Camouflage | Page::Personalization
        ) {
            refresh_layout_flags(&(*raw).layout_path);
        }
        let (ow, oh) = outer_size(INITIAL_CLIENT, dpi);
        let hwnd = match CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("CorralSettings"),
            w!("Corral Settings"),
            STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            ow,
            oh,
            HWND::default(),
            HMENU::default(),
            instance,
            Some(raw as *const std::ffi::c_void),
        ) {
            Ok(h) => h,
            Err(e) => {
                super::camouflage_preview::reset();
                super::sample_corral::reset();
                eprintln!("corral: settings window: {e}");
                drop(Box::from_raw(raw));
                return 1;
            }
        };
        // Real DPI of the monitor the window landed on: resize and centre on the primary screen.
        let wdpi = GetDpiForWindow(hwnd).max(96);
        (*raw).dpi = wdpi;
        crate::app_icon::set_for_dpi(hwnd, wdpi);
        let work = monitor_work(hwnd);
        let (x, y, ow, oh) = opening_bounds(
            (work.left, work.top, work.right, work.bottom),
            outer_size(INITIAL_CLIENT, wdpi),
        );
        let _ = SetWindowPos(hwnd, None, x, y, ow, oh, SWP_NOZORDER | SWP_NOACTIVATE);
        // wallpaper-blur-spec 6: the engine asks for a repaint when the preview picture is ready.
        crate::tint::set_notify_window(hwnd.0 as isize);
        caption_theme(hwnd, &(*raw).theme);
        if (*raw).view.page == Page::Backups {
            backups::expand(backups::load_tiles(&(*raw).layout_path));
            backups::set_notice(backups::load_notice(&(*raw).layout_path));
        }
        if (*raw).view.page == Page::Layout && (*raw).settings.get(Field::LayoutPreserve) == 2 {
            super::desk_setups::refresh(&(*raw).layout_path);
        }
        SetTimer(hwnd, TIMER_DESK_SETUP, 1000, None);
        (*raw).tip = create_tooltip(hwnd, instance);
        let _ = ShowWindow(hwnd, SW_SHOW);
        if let Some(corral) = menu_target {
            // Runs inside the message loop, once the window is up.
            (*raw).pending_focus = Some(corral);
            let _ = PostMessageW(hwnd, WM_FOCUS_CORRAL, WPARAM(0), LPARAM(0));
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        crate::render::release_factories();
        0
    }
}

#[cfg(test)]
mod q33e_tests {
    use super::*;
    #[test]
    fn u1_fix_native_alt_down_is_routed_without_stealing_system_shortcuts() {
        assert!(dispatch_settings_key(WM_SYSKEYDOWN, 0x28, true));
        assert!(dispatch_settings_key(WM_KEYDOWN, 0x28, false));
        assert!(!dispatch_settings_key(WM_SYSKEYDOWN, 0x28, false));
        assert!(!dispatch_settings_key(WM_SYSKEYDOWN, 0x73, true)); // Alt+F4
        assert!(!dispatch_settings_key(WM_SYSKEYDOWN, 0x20, true)); // Alt+Space
        assert!(!dispatch_settings_key(WM_KEYUP, 0x28, true));
        for hit in [
            Hit::Nav(Page::Welcome),
            Hit::Spacing,
            Hit::Toggle(Field::RollupEnabled),
            Hit::FoldMode(0),
        ] {
            assert!(
                !focused_key_allowed(hit, 0x28, true),
                "Alt+Down changed {hit:?}"
            );
            assert!(
                focused_key_allowed(hit, 0x28, false),
                "ordinary Down stopped working"
            );
        }
        assert!(focused_key_allowed(
            Hit::Combo {
                field: Field::CreateCorralMode,
                rect: (0, 0, 220, 32)
            },
            0x28,
            true
        ));
    }

    #[test]
    fn u1_fix_production_focus_reveal_returns_master_to_top() {
        let settings = Settings::default();
        for dpi in [96, 120, 144, 240] {
            let (w, h, scroll) = (sc(760.0, dpi), sc(520.0, dpi), sc(200.0, dpi));
            let targets =
                super::super::focus_targets(&settings, Page::Rollup, dpi, w, h, scroll).unwrap();
            let (hit, r) = targets
                .iter()
                .find(|(hit, _)| *hit == Hit::Toggle(Field::RollupEnabled))
                .copied()
                .unwrap();
            assert_eq!(
                focused_scroll(Page::Rollup, hit, r, dpi, h, scroll, sc(600.0, dpi)),
                0
            );
            let (nav, r) = targets[0];
            assert_eq!(
                focused_scroll(Page::Rollup, nav, r, dpi, h, scroll, sc(600.0, dpi)),
                scroll
            );
            let bottom = RectF::new(0.0, sc(500.0, dpi) as f32, 40.0, sc(532.0, dpi) as f32);
            assert_eq!(
                focused_scroll(
                    Page::Rollup,
                    Hit::Spacing,
                    bottom,
                    dpi,
                    h,
                    scroll,
                    sc(600.0, dpi)
                ),
                sc(228.0, dpi)
            );
        }
    }

    #[test]
    fn opening_size_uses_work_area_and_small_monitors_remain_bounded() {
        assert_eq!(
            opening_bounds((0, 0, 1920, 1040), (1000, 760)),
            (460, 140, 1000, 760)
        );
        for work in [
            (0, 0, 1366, 728),
            (1920, 0, 3286, 728),
            (-1280, -100, 0, 620),
        ] {
            let (x, y, w, h) = opening_bounds(work, (2500, 1900));
            assert!(x >= work.0 && y >= work.1 && x + w <= work.2 && y + h <= work.3);
            assert!(w as f32 <= (work.2 - work.0) as f32 * 0.9);
            assert!(h as f32 <= (work.3 - work.1) as f32 * 0.9);
        }
    }
    #[test]
    fn choice_popup_uses_its_own_monitor_dpi_and_clamps_both_axes() {
        for dpi in [96, 120, 144, 240] {
            let work = RECT {
                left: -1920,
                top: 50,
                right: 0,
                bottom: 1490,
            };
            let (x, y, w, h) = popup_geometry((-20, 1470), work, dpi, 220.0, 6);
            assert_eq!(w, sc(220.0, dpi));
            assert_eq!(h, sc(212.0, dpi));
            assert!(x >= work.left && y >= work.top && x + w <= work.right && y + h <= work.bottom);
        }
    }
    #[test]
    fn sender_coalesces_previews_but_keeps_final_before_next_gesture() {
        use settings::style_request::{ColorPatch, LiveRequest};
        let mut queue = SenderQueue::default();
        let patch = |color| ColorPatch { color: Some(color), ..Default::default() };
        let mut request = LiveRequest::new("QA/layout.json".into(), None, 1, patch(1), false);
        queue.submit(request.clone());
        request.patch = patch(123);
        queue.submit(request.clone());
        assert_eq!(queue.requests.len(), 1);
        request.persist = true;
        queue.submit(request.clone());
        request.persist = false;
        request.patch = patch(456);
        queue.submit(request);
        assert_eq!(queue.requests.len(), 3, "a frame never overwrites a commit that went before it");
        assert!(queue.requests[1].persist);
        assert_eq!(queue.requests[1].patch.color, Some(123));
        assert_eq!(queue.requests[2].patch.color, Some(456));
        // a cancel is a barrier too
        queue.submit(LiveRequest::cancel("QA/layout.json".into(), None, 1));
        assert!(queue.requests.back().unwrap().cancel);
    }
    #[test]
    fn a_failed_commit_puts_the_stored_shared_colours_back_before_anything_saves_them() {
        // `send_commit` copies the page's shared colours into the window's settings at once.
        let stored = Settings::default();
        let mut window = stored.clone();
        window.snap_padding = 33; // an unrelated, already saved page value
        let failed = ColorEdit::Background(0x123456).patch(&color_state::effective_of(&window, &Default::default()));
        failed.apply_to_settings(&mut window);
        assert_eq!(window.color, 0x123456);
        restore_shared_colors(&mut window, &stored);
        assert_eq!(window.color, stored.color, "a later unrelated save cannot persist the failed colour");
        assert_eq!(window.snap_padding, 33, "other settings stay");
    }
    #[test]
    fn sender_merges_the_fields_of_one_session_and_keeps_sessions_apart() {
        use settings::style_request::{ColorPatch, LiveRequest};
        let mut queue = SenderQueue::default();
        queue.submit(LiveRequest::new(
            "QA/layout.json".into(), None, 1,
            ColorPatch { color: Some(99), ..Default::default() }, false));
        queue.submit(LiveRequest::new(
            "QA/layout.json".into(), None, 1,
            ColorPatch { transparency_factor: Some(800), ..Default::default() }, false));
        assert_eq!(queue.requests.len(), 1);
        let patch = &queue.requests[0].patch;
        assert_eq!((patch.color, patch.transparency_factor, patch.tint_icons), (Some(99), Some(800), None));
        queue.submit(LiveRequest::new(
            "QA/layout.json".into(), None, 2,
            ColorPatch { color: Some(5), ..Default::default() }, false));
        assert_eq!(queue.requests.len(), 2, "another session starts its own overlay");
    }
}

#[cfg(test)]
mod u4_integrated_menu_tests {
    use super::*;
    #[test]
    fn popup_keyboard_skips_disabled_rows_and_keeps_boundaries() {
        let enabled = [false, true, false, true, false];
        assert_eq!(popup_next_enabled(&enabled, 1, true), 3);
        assert_eq!(popup_next_enabled(&enabled, 3, true), 3);
        assert_eq!(popup_next_enabled(&enabled, 3, false), 1);
        assert_eq!(popup_next_enabled(&enabled, 1, false), 1);
    }
    #[test]
    fn popup_mouse_matches_drawn_rows_at_fractional_dpi_and_clipping() {
        for dpi in [96, 120, 144, 240] {
            let enabled = vec![true; 30];
            let (w, h) = (sc(250.0, dpi), sc(8.0 + 20.0 * 34.0, dpi));
            for first in [0, 5] {
                for i in first..first + 20 {
                    let r = popup_row_rect(i, first, dpi, w, h).unwrap();
                    for y in [r.top as i32, r.bottom as i32 - 1] {
                        assert_eq!(popup_row_at(&enabled, first, dpi, (20, y), w, h), Some(i));
                    }
                    assert_eq!(
                        popup_row_at(&enabled, first, dpi, (20, r.bottom as i32), w, h),
                        None
                    );
                }
                assert!(popup_row_rect(first + 20, first, dpi, w, h).is_none());
                assert_eq!(popup_row_at(&enabled, first, dpi, (20, h), w, h), None);
                assert_eq!(popup_row_at(&enabled, first, dpi, (0, 20), w, h), None);
            }
        }
    }
    #[test]
    fn u4_menu_and_disable_prompt_goldens() -> windows::core::Result<()> {
        let dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/qa/settings-redesign/u4");
        std::fs::create_dir_all(&dir).unwrap();
        for light in [true, false] {
            for dpi in [96, 120, 144, 240] {
                let theme = if light { Theme::light() } else { Theme::dark() };
                for (name, labels) in [
                    (
                        "backup-more-menu",
                        vec!["Rename", "Update to current layout", "Delete"],
                    ),
                    ("rule-context-menu", vec!["Edit…", "Rename…", "Delete…"]),
                ] {
                    let state = ChoicePopup {
                        labels: labels.into_iter().map(str::to_owned).collect(),
                        values: vec![1, 2, 3],
                        enabled: vec![true; 3],
                        checked: vec![false; 3],
                        width_dip: 250.0,
                        keyboard_visible: false,
                        selected: 1,
                        hover: Some(1),
                        result: None,
                        done: false,
                        dpi,
                        theme,
                        surface: None,
                        search: String::new(),
                        last_key: None,
                        first: 0,
                    };
                    let (w, h) = (sc(250.0, dpi), sc(110.0, dpi));
                    let mut surface = Surface::new(w as u32, h as u32)?;
                    surface.draw(|ctx| draw_choice_popup(ctx, &state, w, h))?;
                    surface.save_png(&dir.join(format!(
                        "{name}-{}-{dpi}.png",
                        if light { "light" } else { "dark" }
                    )))?;
                }
                for (name, spec) in [
                    ("turn-off-corral", disable_corral_spec()),
                    ("spacing", spacing_spec(8)),
                ] {
                    let lay = crate::dialog::layout(&spec, spec.width);
                    let mut surface =
                        Surface::new(sc(lay.width, dpi) as u32, sc(lay.height, dpi) as u32)?;
                    surface.draw(|ctx| crate::dialog::draw_spec(ctx, &spec, dpi, light))?;
                    surface.save_png(&dir.join(format!(
                        "{name}-{}-{dpi}.png",
                        if light { "light" } else { "dark" }
                    )))?;
                }
            }
        }
        Ok(())
    }
}
