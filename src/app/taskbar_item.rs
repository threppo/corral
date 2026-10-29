//! "Show Corrals" icon: with `ShowInTaskbar` on, Corral shows a notification-area (tray) icon; a click on it
//! flips Glance (Peek) like the toggle shortcut.
//!
//! [Corral policy, user decision 2026-10-15]: a taskbar window button behaves like an application (thumbnail
//! flyout, an "active" state in which a click minimizes instead of activating, so clicks were lost), and the
//! user wanted a shell-like button. Windows 11 has no API for a button next to Start/Search; a tray icon (like
//! Wallpaper Engine's) reports every click directly (`NIN_SELECT`). Not implemented: a pinned shortcut, the
//! "Customize icon" link and `ShowInTaskbarAsRunning`.

use super::{App, log};
use crate::peek_keys;
use crate::quick_hide::{self, FgClass};
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject, HGDIOBJ, MONITOR_DEFAULTTOPRIMARY, MonitorFromPoint};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, GetDpiForSystem, GetSystemMetricsForDpi, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

/// Posted to the message window when the icon was clicked (wParam/lParam unused).
pub(super) const WM_TASKBAR_FLIP: u32 = WM_APP + 0x41;
/// The icon's callback message, sent to the (hidden) owner window.
const WM_TRAY_CALLBACK: u32 = WM_APP + 0x42;
const TRAY_ID: u32 = 1;
const NIN_SELECT: u32 = WM_USER;
const NIN_KEYSELECT: u32 = WM_USER + 1;
const MENU_SHOW: usize = 1;
const MENU_SETTINGS: usize = 2;
const MENU_HIDE: usize = 3;

/// The last activated window that is not Corral's and is a real application window: what the
/// foreground returns to when the click on the icon left the taskbar in front (written by the
/// foreground hook).
static LAST_FOREIGN_FG: AtomicIsize = AtomicIsize::new(0);

// ---------------------------------------------------------------- pure decisions

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Presence {
    Create,
    Destroy,
    Keep,
}

/// The icon exists exactly while desktop mode runs and `ShowInTaskbar` is on.
pub(super) fn presence_plan(desktop_mode: bool, show_in_taskbar: bool, exists: bool) -> Presence {
    match (desktop_mode && show_in_taskbar, exists) {
        (true, false) => Presence::Create,
        (false, true) => Presence::Destroy,
        _ => Presence::Keep,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TrayAction {
    Flip,
    Menu,
    Ignore,
}

/// The icon's version-4 callback event (low word of lParam). Every left click is a flip: `NIN_SELECT`
/// arrives once per click, for both clicks of a double click too [unverified: the QA log shows
/// each click]; `NIN_KEYSELECT` is Enter/Space on the focused icon.
pub(super) fn tray_action(event: u32) -> TrayAction {
    match event {
        NIN_SELECT | NIN_KEYSELECT => TrayAction::Flip,
        WM_CONTEXTMENU => TrayAction::Menu,
        _ => TrayAction::Ignore,
    }
}

/// Whether the foreground window can be given the foreground back later: a normal application window
/// (not the desktop, taskbar, shell flyouts or Corral's own windows).
pub(super) fn restorable_foreground(class: &str, own_process: bool, visible: bool, tool_window: bool) -> bool {
    !own_process && quick_hide::classify_foreground(class, false, false, visible, tool_window) == FgClass::Normal
}

// ---------------------------------------------------------------- the emblem
//
// The tray shows Corral's emblem: the app icon's mark without its graphite tile (Figma "Corral — Brand
// & Theme", page "App icon · FINAL", components "Corral / Emblem amber / 16|24|32|256"): a rounded
// enclosure whose right rail opens at the lower right, the gate leaf swung into the yard, and one item.
// [Corral policy, own artwork]
//
// Drawn here rather than embedded as a second icon group: the tray needs `SM_CXSMICON` at the taskbar's
// DPI (16, 20, 24, 28, 32, 40, 48 ...), a palette per taskbar theme, and a rebuild when either changes.
// The hand-drawn sizes keep their own pixel grids as numbers taken from the SVG exports; every other
// size is the 256 master scaled and snapped to whole pixels. No exported PNG pipeline, no extra resources.

/// One drawing of the emblem on its own grid (`grid` x `grid` px), numbers from the Figma SVG export.
#[derive(Clone, Copy)]
struct Emblem {
    grid: f32,
    stroke: f32,
    /// Outline centre line: left, top, right, bottom.
    rail: [f32; 4],
    /// Corners are quadratic curves with the control point on the rectangle corner, this far from it.
    corner: f32,
    /// The right rail runs down from the top corner to here; below it the gate is open down to the
    /// bottom corner, where the rail ends.
    gate_hinge: f32,
    /// The leaf: from (right, `gate_hinge`) into the yard to this point.
    leaf_end: (f32, f32),
    /// The item: left, top, right, bottom, corner radius.
    item: [f32; 5],
}

/// The hand-drawn 16/24/32 and the 256 master are the Figma emblem components. Figma has no 20 px
/// emblem: it is the hand-drawn 20 px app icon (`assets/icon/svg/corral-20.svg`) without its tile and
/// slate blocks, which is how every emblem relates to its app icon.
const EMBLEMS: [Emblem; 5] = [
    Emblem { grid: 16.0, stroke: 2.0, rail: [3.0, 3.0, 13.0, 13.0], corner: 2.0, gate_hinge: 6.0, leaf_end: (11.0, 8.0), item: [5.0, 8.0, 8.0, 11.0, 0.6] },
    Emblem { grid: 20.0, stroke: 2.0, rail: [4.0, 4.0, 16.0, 16.0], corner: 3.0, gate_hinge: 8.0, leaf_end: (14.3, 10.0), item: [9.0, 11.0, 12.0, 14.0, 0.6] },
    Emblem { grid: 24.0, stroke: 2.0, rail: [5.0, 5.0, 19.0, 19.0], corner: 3.0, gate_hinge: 9.0, leaf_end: (16.7, 11.75), item: [11.0, 13.0, 14.0, 16.0, 0.75] },
    Emblem { grid: 32.0, stroke: 2.0, rail: [6.0, 7.0, 26.0, 25.0], corner: 4.0, gate_hinge: 13.0, leaf_end: (23.2, 16.8), item: [18.0, 19.0, 22.0, 23.0, 1.0] },
    Emblem { grid: 256.0, stroke: 16.0, rail: [48.0, 56.0, 208.0, 200.0], corner: 32.0, gate_hinge: 108.0, leaf_end: (185.5, 134.8), item: [150.0, 148.0, 182.0, 180.0, 8.0] },
];

/// Vertical gradient, top to bottom of each part (sRGB, as in the SVG).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Palette {
    top: [u8; 3],
    bottom: [u8; 3],
}

/// The brand amber (#F0C48E to #E2A35C) for a dark taskbar.
pub(super) const AMBER: Palette = Palette { top: [0xF0, 0xC4, 0x8E], bottom: [0xE2, 0xA3, 0x5C] };
/// A light taskbar (#EEEEEE to #F3F3F3) leaves the brand amber at 1.4-1.9:1, too faint for a 16 px
/// mark: the same hues darkened to 3.3-4.7:1 against it (the brand amber has 7-12:1 on a dark one). [Corral policy, own choice 2026-10-16;
/// the Figma file's light-background emblem is "Emblem black" #14161A, which drops the brand colour]
pub(super) const AMBER_ON_LIGHT: Palette = Palette { top: [0xB0, 0x76, 0x30], bottom: [0x9A, 0x60, 0x1E] };

/// The palette for the Windows mode the taskbar follows (`SystemUsesLightTheme`).
pub(super) fn palette_for(light_taskbar: bool) -> Palette {
    if light_taskbar { AMBER_ON_LIGHT } else { AMBER }
}

/// The drawing for `size` px: the hand-drawn grid when one matches, else the master scaled with a whole
/// pixel stroke whose edges sit on pixel boundaries and a pixel-aligned item, so 28 or 40 px stay crisp.
fn emblem_for(size: usize) -> Emblem {
    if let Some(emblem) = EMBLEMS.iter().find(|e| e.grid as usize == size) {
        return *emblem;
    }
    let master = EMBLEMS[EMBLEMS.len() - 1];
    let k = size as f32 / master.grid;
    let stroke = (master.stroke * k).round().max(1.0);
    let rail = master.rail.map(|v| (v * k - stroke / 2.0).round() + stroke / 2.0);
    let [il, it, ir, ib, ic] = master.item;
    Emblem {
        grid: size as f32,
        stroke,
        rail,
        corner: master.corner * k,
        gate_hinge: master.gate_hinge * k,
        leaf_end: (master.leaf_end.0 * k, master.leaf_end.1 * k),
        item: [(il * k).round(), (it * k).round(), (ir * k).round(), (ib * k).round(), ic * k],
    }
}

/// The open outline as a polyline: from the gate hinge up the right rail, round the top, left and
/// bottom, ending at the bottom right corner.
fn rail_points(e: &Emblem) -> Vec<(f32, f32)> {
    let [l, t, r, b] = e.rail;
    let c = e.corner;
    let mut points = vec![(r, e.gate_hinge)];
    let corners = [((r, t + c), (r, t), (r - c, t)), ((l + c, t), (l, t), (l, t + c)), ((l, b - c), (l, b), (l + c, b)), ((r - c, b), (r, b), (r, b - c))];
    for (from, control, to) in corners {
        points.push(from);
        for step in 1..=12 {
            let s = step as f32 / 12.0;
            let (u, v, w) = ((1.0 - s) * (1.0 - s), 2.0 * s * (1.0 - s), s * s);
            points.push((u * from.0 + v * control.0 + w * to.0, u * from.1 + v * control.1 + w * to.1));
        }
    }
    points
}

fn segment_distance(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = dx * dx + dy * dy;
    let s = if length > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / length).clamp(0.0, 1.0) } else { 0.0 };
    ((p.0 - a.0 - s * dx).powi(2) + (p.1 - a.1 - s * dy).powi(2)).sqrt()
}

/// Signed distance to a rounded box (centre, half extents, corner radius).
fn rounded_box(x: f32, y: f32, cx: f32, cy: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let (qx, qy) = ((x - cx).abs() - (hx - r), (y - cy).abs() - (hy - r));
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
}

/// BGRA, premultiplied, `size` x `size`: the emblem on a transparent background, 8x8 samples per pixel.
pub(super) fn emblem_pixels(size: usize, palette: Palette) -> Vec<u8> {
    const SAMPLES: usize = 8;
    let e = &emblem_for(size);
    let rail = rail_points(e);
    let half = e.stroke / 2.0;
    let hinge = (e.rail[2], e.gate_hinge);
    let [il, it, ir, ib, ic] = e.item;
    let shade = |y: f32, top: f32, bottom: f32| {
        let s = ((y - top) / (bottom - top)).clamp(0.0, 1.0);
        let mix = |i: usize| palette.top[i] as f32 * (1.0 - s) + palette.bottom[i] as f32 * s;
        [mix(0), mix(1), mix(2)]
    };
    let mut out = Vec::with_capacity(size * size * 4);
    for py in 0..size {
        for px in 0..size {
            let (mut sum, mut hits) = ([0.0f32; 3], 0usize);
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let p = (px as f32 + (sx as f32 + 0.5) / SAMPLES as f32, py as f32 + (sy as f32 + 0.5) / SAMPLES as f32);
                    // painted in SVG order: rail, leaf, item on top
                    let color = if rounded_box(p.0, p.1, (il + ir) / 2.0, (it + ib) / 2.0, (ir - il) / 2.0, (ib - it) / 2.0, ic) <= 0.0 {
                        shade(p.1, it, ib)
                    } else if segment_distance(p, hinge, e.leaf_end) <= half {
                        shade(p.1, e.gate_hinge, e.leaf_end.1)
                    } else if rail.windows(2).any(|w| segment_distance(p, w[0], w[1]) <= half) {
                        shade(p.1, e.rail[1], e.rail[3])
                    } else {
                        continue;
                    };
                    hits += 1;
                    for i in 0..3 {
                        sum[i] += color[i];
                    }
                }
            }
            let n = (SAMPLES * SAMPLES) as f32;
            let channel = |v: f32| (v / n).round().min(255.0) as u8;
            out.extend_from_slice(&[channel(sum[2]), channel(sum[1]), channel(sum[0]), channel(hits as f32 * 255.0)]);
        }
    }
    out
}

// ---------------------------------------------------------------- Win32

/// Called from the foreground hook (`fg_proc`, runs on the main thread): remember the last real
/// application window.
pub(super) fn note_foreground(hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }
    let class = super::peek::class_name(hwnd);
    let ex = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    let tool = ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0;
    if restorable_foreground(&class, super::peek::is_own_process(hwnd), unsafe { IsWindowVisible(hwnd) }.as_bool(), tool) {
        LAST_FOREIGN_FG.store(hwnd.0 as isize, Ordering::Relaxed);
    }
}

/// The window to give the foreground back to (null when gone or never seen).
pub(super) fn last_foreign_foreground() -> HWND {
    let hwnd = HWND(LAST_FOREIGN_FG.load(Ordering::Relaxed) as *mut core::ffi::c_void);
    if !hwnd.0.is_null() && unsafe { IsWindow(hwnd) }.as_bool() { hwnd } else { HWND::default() }
}

/// What the tray icon looks like now: `SM_CXSMICON` at the primary monitor's DPI (the notification area
/// is on the primary taskbar; `GetSystemMetrics` would keep the DPI Corral started with) and whether the
/// taskbar is light.
fn tray_icon_spec() -> (i32, bool) {
    let dpi = unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let (mut x, mut y) = (0u32, 0u32);
        if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y).is_ok() { x } else { GetDpiForSystem() }
    };
    let size = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, dpi.max(96)) }.max(16);
    (size, crate::menu_theme::system_uses_light_theme())
}

fn make_icon((size, light_taskbar): (i32, bool)) -> Option<HICON> {
    let pixels = emblem_pixels(size as usize, palette_for(light_taskbar));
    let mask = vec![0u8; (size as usize).div_ceil(16) * 2 * size as usize];
    unsafe {
        let color = CreateBitmap(size, size, 1, 32, Some(pixels.as_ptr() as *const _));
        let monochrome = CreateBitmap(size, size, 1, 1, Some(mask.as_ptr() as *const _));
        let info = ICONINFO { fIcon: BOOL(1), xHotspot: 0, yHotspot: 0, hbmMask: monochrome, hbmColor: color };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(monochrome.0));
        icon
    }
}

fn taskbar_created_message() -> u32 {
    unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) }
}

/// Taskbar item state kept in `App`.
#[derive(Default)]
pub(super) struct State {
    /// Hidden owner of the icon: receives its callbacks and owns its menu.
    hwnd: Option<HWND>,
    icon: Option<HICON>,
    /// Size and taskbar theme `icon` was drawn for.
    icon_spec: Option<(i32, bool)>,
    /// While a click on the icon is being handled (the taskbar is foreground then).
    clicking: bool,
}

impl App {
    /// Create or destroy the icon after a settings load (and at start).
    pub(super) fn taskbar_configure(&mut self) {
        match presence_plan(self.detached(), self.settings.show_in_taskbar, self.taskbar.hwnd.is_some()) {
            Presence::Create => self.taskbar_create(),
            Presence::Destroy => self.taskbar_destroy(),
            Presence::Keep => {}
        }
    }

    fn taskbar_create(&mut self) {
        // a hidden top-level tool window: no taskbar button, but it gets the TaskbarCreated broadcast
        let created = unsafe {
            CreateWindowExW(WS_EX_TOOLWINDOW, w!("CorralTaskbarItem"), w!("Show Corrals"), WS_POPUP,
                0, 0, 0, 0, HWND::default(), HMENU::default(), self.instance, None)
        };
        let hwnd = match created {
            Ok(hwnd) => hwnd,
            Err(e) => {
                log(&format!("taskbar item: window creation failed: {e}"));
                return;
            }
        };
        self.taskbar.hwnd = Some(hwnd);
        let spec = tray_icon_spec();
        self.taskbar.icon = make_icon(spec);
        self.taskbar.icon_spec = Some(spec);
        self.taskbar_add_icon();
    }

    /// Redraw the icon when the taskbar's DPI or light/dark mode changed since it was drawn.
    fn taskbar_refresh_icon(&mut self) {
        let Some(hwnd) = self.taskbar.hwnd else { return };
        let spec = tray_icon_spec();
        if self.taskbar.icon_spec == Some(spec) {
            return;
        }
        let Some(icon) = make_icon(spec) else { return };
        let old = self.taskbar.icon.replace(icon);
        self.taskbar.icon_spec = Some(spec);
        let mut data = self.taskbar_icon_data(hwnd);
        data.uFlags = NIF_ICON;
        let modified = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool();
        log(&format!("taskbar item: icon redrawn at {} px, light taskbar {}: {modified}", spec.0, spec.1));
        if let Some(old) = old {
            unsafe {
                let _ = DestroyIcon(old);
            }
        }
    }

    fn taskbar_icon_data(&self, hwnd: HWND) -> NOTIFYICONDATAW {
        let mut data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
            uCallbackMessage: WM_TRAY_CALLBACK,
            hIcon: self.taskbar.icon.unwrap_or_default(),
            ..Default::default()
        };
        for (slot, unit) in data.szTip.iter_mut().zip("Show Corrals".encode_utf16()) {
            *slot = unit;
        }
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        data
    }

    /// Add the icon (also again after Explorer restarted and broadcast TaskbarCreated).
    fn taskbar_add_icon(&mut self) {
        let Some(hwnd) = self.taskbar.hwnd else { return };
        let data = self.taskbar_icon_data(hwnd);
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool();
        let versioned = added && unsafe { Shell_NotifyIconW(NIM_SETVERSION, &data) }.as_bool();
        log(&format!("taskbar item: tray icon added {added}, version 4 {versioned}"));
    }

    fn taskbar_destroy(&mut self) {
        if let Some(hwnd) = self.taskbar.hwnd.take() {
            let data = self.taskbar_icon_data(hwnd);
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                let _ = DestroyWindow(hwnd);
            }
            log("taskbar item: tray icon removed");
        }
        self.taskbar.icon_spec = None;
        if let Some(icon) = self.taskbar.icon.take() {
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    }

    pub(super) fn taskbar_shutdown(&mut self) {
        self.taskbar_destroy();
    }

    pub(super) fn taskbar_owns(&self, hwnd: HWND) -> bool {
        self.taskbar.hwnd == Some(hwnd)
    }

    /// Peek is entered by a click on the icon and `fg` (the taskbar or Corral's menu owner) is not an
    /// application to give the foreground back to.
    pub(super) fn taskbar_click_foreground(&self, fg: HWND) -> bool {
        if !self.taskbar.clicking {
            return false;
        }
        let ex = unsafe { GetWindowLongW(fg, GWL_EXSTYLE) } as u32;
        let tool = ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0;
        !restorable_foreground(&super::peek::class_name(fg), super::peek::is_own_process(fg),
            unsafe { IsWindowVisible(fg) }.as_bool(), tool)
    }

    /// Messages of the icon's owner window. `None` = default processing.
    pub(super) fn taskbar_message(&mut self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if msg == WM_TRAY_CALLBACK {
            let event = (lparam.0 as u32) & 0xffff;
            match tray_action(event) {
                TrayAction::Flip => {
                    log(&format!("taskbar item: click ({event:#x})"));
                    unsafe {
                        let _ = PostMessageW(self.message_window, WM_TASKBAR_FLIP, WPARAM(0), LPARAM(0));
                    }
                }
                TrayAction::Menu => {
                    // version 4: the anchor point is in wParam (screen coordinates)
                    let at = POINT { x: (wparam.0 & 0xffff) as i16 as i32, y: ((wparam.0 >> 16) & 0xffff) as i16 as i32 };
                    self.taskbar_menu(at);
                }
                TrayAction::Ignore => {}
            }
            return Some(LRESULT(0));
        }
        if msg == taskbar_created_message() {
            log("taskbar item: TaskbarCreated (Explorer restarted), adding the icon again");
            self.taskbar_refresh_icon();
            self.taskbar_add_icon();
            return Some(LRESULT(0));
        }
        // light/dark mode ("ImmersiveColorSet"), scaling or display changes reach this hidden top-level
        // window too; redraw only when the size or theme actually changed
        if matches!(msg, WM_SETTINGCHANGE | WM_DISPLAYCHANGE | WM_DPICHANGED) {
            self.taskbar_refresh_icon();
        }
        None
    }

    fn taskbar_menu(&mut self, at: POINT) {
        let Some(hwnd) = self.taskbar.hwnd else { return };
        let choice = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let show = if self.peek.is_on() { w!("Hide Corrals") } else { w!("Show Corrals") };
            let _ = AppendMenuW(menu, MF_STRING, MENU_SHOW, show);
            let _ = AppendMenuW(menu, MF_STRING, MENU_SETTINGS, w!("Configure Corral..."));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, MENU_HIDE, w!("Hide this icon"));
            let _ = SetMenuDefaultItem(menu, MENU_SHOW as u32, 0);
            // the documented tray-menu dance: owner foreground before, a WM_NULL after
            let _ = SetForegroundWindow(hwnd);
            let choice = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RIGHTALIGN,
                at.x, at.y, 0, hwnd, None);
            let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
            choice.0 as usize
        };
        log(&format!("taskbar item: menu choice {choice}"));
        match choice {
            MENU_SHOW => self.taskbar_flip(),
            MENU_SETTINGS => self.open_settings_window(),
            MENU_HIDE => self.taskbar_closed(),
            _ => {}
        }
    }

    /// "Hide this icon": the icon is removed and the setting is switched off and saved, so the checkbox
    /// in Peek > Taskbar Icon agrees.
    fn taskbar_closed(&mut self) {
        log("taskbar item: hidden from its menu -> ShowInTaskbar = false");
        let path = self.opts.settings_path.clone();
        let mut stored = super::settings_store::load_with_backup(&path);
        stored.show_in_taskbar = false;
        match super::settings_store::save(&path, &stored) {
            Ok(()) => self.settings_stamp = self.settings_file_stamp(),
            Err(e) => log(&format!("taskbar item: saving ShowInTaskbar failed: {e}")),
        }
        self.settings.show_in_taskbar = false;
        self.taskbar_configure();
    }

    /// `WM_TASKBAR_FLIP`: the icon was clicked. Same path as the toggle hotkey; when Peek did not come on
    /// (refused or just ended), the application the user worked in gets the foreground back from the
    /// taskbar.
    pub(super) fn taskbar_flip(&mut self) {
        log("peek: taskbar item activated");
        self.taskbar.clicking = true;
        self.peek_key_event(peek_keys::EVENT_TOGGLE);
        self.taskbar.clicking = false;
        let fg = unsafe { GetForegroundWindow() };
        let left_on_taskbar = !self.peek.is_on() && {
            let ex = unsafe { GetWindowLongW(fg, GWL_EXSTYLE) } as u32;
            let tool = ex & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0;
            !restorable_foreground(&super::peek::class_name(fg), super::peek::is_own_process(fg),
                unsafe { IsWindowVisible(fg) }.as_bool(), tool)
        };
        if left_on_taskbar {
            let back = last_foreign_foreground();
            if !back.0.is_null() && back != fg {
                let ok = unsafe { SetForegroundWindow(back) }.as_bool();
                log(&format!("taskbar item: foreground given back to {:#x}: {}", back.0 as isize, if ok { "OK" } else { "refused by the system" }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_follows_desktop_mode_and_the_setting() {
        assert_eq!(presence_plan(true, true, false), Presence::Create);
        assert_eq!(presence_plan(true, true, true), Presence::Keep);
        assert_eq!(presence_plan(true, false, true), Presence::Destroy);
        assert_eq!(presence_plan(true, false, false), Presence::Keep);
        // preview mode never has one, even when the setting is on
        assert_eq!(presence_plan(false, true, false), Presence::Keep);
        assert_eq!(presence_plan(false, true, true), Presence::Destroy);
        assert_eq!(presence_plan(false, false, false), Presence::Keep);
    }

    #[test]
    fn every_left_click_flips_and_the_right_click_opens_the_menu() {
        assert_eq!(tray_action(NIN_SELECT), TrayAction::Flip);
        assert_eq!(tray_action(NIN_KEYSELECT), TrayAction::Flip);
        assert_eq!(tray_action(WM_CONTEXTMENU), TrayAction::Menu);
        for event in [WM_MOUSEMOVE, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_LBUTTONDBLCLK, WM_RBUTTONUP, WM_USER + 2] {
            assert_eq!(tray_action(event), TrayAction::Ignore, "{event:#x}");
        }
    }

    #[test]
    fn only_real_application_windows_are_remembered_for_the_foreground() {
        assert!(restorable_foreground("Notepad", false, true, false));
        assert!(!restorable_foreground("Notepad", true, true, false), "Corral's own window");
        assert!(!restorable_foreground("Notepad", false, false, false), "hidden");
        assert!(!restorable_foreground("Notepad", false, true, true), "tool window");
        for class in ["Shell_TrayWnd", "Progman", "WorkerW", "#32768", "Windows.UI.Core.CoreWindow"] {
            assert!(!restorable_foreground(class, false, true, false), "{class}");
        }
    }

    fn pixel(px: &[u8], size: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * size + x) * 4;
        [px[i], px[i + 1], px[i + 2], px[i + 3]]
    }

    #[test]
    fn the_emblem_has_no_tile_at_any_tray_size() {
        // SM_CXSMICON at 100 % to 300 %
        for size in [16usize, 20, 24, 28, 32, 36, 40, 48] {
            let px = emblem_pixels(size, AMBER);
            assert_eq!(px.len(), size * size * 4);
            for (x, y) in [(0, 0), (size - 1, 0), (0, size - 1), (size - 1, size - 1), (size / 2, size / 2 - 1)] {
                assert_eq!(pixel(&px, size, x, y)[3], 0, "transparent at ({x}, {y}), {size} px: no tile, empty yard");
            }
            assert!(px.chunks(4).any(|p| p[3] == 255), "{size} px has solid strokes");
            assert!(px.chunks(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]), "premultiplied at {size} px");
        }
    }

    #[test]
    fn the_hand_drawn_16_px_emblem_keeps_its_pixel_grid() {
        let px = emblem_pixels(16, AMBER);
        // the top rail's straight run covers rows 2 and 3 exactly, in the brand amber (BGRA)
        for x in 5..11 {
            assert_eq!(pixel(&px, 16, x, 1)[3], 0, "above the rail at x {x}");
            for y in [2, 3] {
                let p = pixel(&px, 16, x, y);
                assert_eq!(p[3], 255, "rail at ({x}, {y})");
                assert!(p[2] > p[1] && p[1] > p[0], "amber at ({x}, {y}): {p:?}");
            }
            assert_eq!(pixel(&px, 16, x, 4)[3], 0, "below the rail at x {x}");
        }
        // the item fills (5..8, 8..11); the gate is open on the right rail below the leaf
        assert_eq!(pixel(&px, 16, 6, 9)[3], 255, "the item");
        assert_eq!(pixel(&px, 16, 13, 9)[3], 0, "the gate opening");
        assert_eq!(pixel(&px, 16, 13, 5)[3], 255, "the right rail above the gate");
    }

    /// WCAG relative luminance of an sRGB colour.
    fn luminance(c: [u8; 3]) -> f32 {
        let lin = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
    }

    fn contrast(a: [u8; 3], b: [u8; 3]) -> f32 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn the_emblem_stays_readable_on_light_and_dark_taskbars() {
        // Windows 11 taskbar fills, solid and Mica-tinted
        let dark = [[0x1C, 0x1C, 0x1C], [0x20, 0x20, 0x20], [0x2C, 0x2C, 0x2C]];
        let light = [[0xEE, 0xEE, 0xEE], [0xF3, 0xF3, 0xF3], [0xF9, 0xF9, 0xF9]];
        for (taskbars, light_taskbar) in [(dark, false), (light, true)] {
            let palette = palette_for(light_taskbar);
            for taskbar in taskbars {
                for stop in [palette.top, palette.bottom] {
                    let ratio = contrast(stop, taskbar);
                    assert!(ratio >= 3.0, "{stop:?} on {taskbar:?}: {ratio:.2}:1 (non-text minimum 3:1)");
                }
            }
        }
        // the brand amber itself would not be
        assert!(contrast(AMBER.top, [0xEE, 0xEE, 0xEE]) < 2.0);
    }

    /// Writes `emblem-<dark|light>-<size>.bgra` (premultiplied BGRA rows) into the directory named by
    /// `CORRAL_EMBLEM_DUMP`, for visual checks: `cargo test emblem_dump -- --ignored`.
    #[test]
    #[ignore]
    fn emblem_dump() {
        let dir = std::path::PathBuf::from(std::env::var("CORRAL_EMBLEM_DUMP").expect("CORRAL_EMBLEM_DUMP"));
        for (name, light) in [("dark", false), ("light", true)] {
            for size in [16usize, 20, 24, 28, 32, 40, 48] {
                std::fs::write(dir.join(format!("emblem-{name}-{size}.bgra")), emblem_pixels(size, palette_for(light))).unwrap();
            }
        }
    }
}
