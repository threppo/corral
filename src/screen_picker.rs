//! Screen-wide colour picker. A nearly invisible layered input surface consumes clicks.
//! Sampling uses a pre-overlay snapshot, avoiding both overlay colour contamination and feedback
//! from the live Corral preview. No full-screen snapshot is ever displayed.
//! The transient bitmap stays in memory and is discarded when the picker closes.

use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetCapture, IsWindowEnabled, ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

#[derive(Clone, Copy)]
struct Bounds {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

impl Bounds {
    fn local(self, x: i32, y: i32) -> Option<(i32, i32)> {
        let (x, y) = (x.checked_sub(self.x)?, y.checked_sub(self.y)?);
        (x >= 0 && y >= 0 && x < self.width && y < self.height).then_some((x, y))
    }
}

struct Snapshot {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}

impl Snapshot {
    unsafe fn capture(bounds: Bounds) -> Option<Self> {
        unsafe {
            let screen = GetDC(HWND::default());
            if screen.is_invalid() {
                return None;
            }
            let dc = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, bounds.width, bounds.height);
            if dc.is_invalid() || bitmap.is_invalid() {
                if !dc.is_invalid() {
                    let _ = DeleteDC(dc);
                }
                if !bitmap.is_invalid() {
                    let _ = DeleteObject(bitmap);
                }
                ReleaseDC(HWND::default(), screen);
                return None;
            }
            let previous = SelectObject(dc, bitmap);
            if previous.is_invalid() {
                let _ = DeleteObject(bitmap);
                let _ = DeleteDC(dc);
                ReleaseDC(HWND::default(), screen);
                return None;
            }
            let ok = BitBlt(
                dc,
                0,
                0,
                bounds.width,
                bounds.height,
                screen,
                bounds.x,
                bounds.y,
                SRCCOPY | CAPTUREBLT,
            )
            .is_ok();
            ReleaseDC(HWND::default(), screen);
            let snapshot = Self {
                dc,
                bitmap,
                previous,
            };
            ok.then_some(snapshot)
        }
    }
}

impl Drop for Snapshot {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap);
            let _ = DeleteDC(self.dc);
        }
    }
}

struct State {
    snapshot: Snapshot,
    bounds: Bounds,
    cursor: HCURSOR,
    hovered: Option<u32>,
    selected: Option<u32>,
    pressed: bool,
    right_pressed: bool,
    done: bool,
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let create = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        // Do not keep a borrow of caller state across a dispatch: Win32 may reenter this procedure.
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut ps);
                // Alpha 1/255 keeps hit testing enabled without showing an opaque snapshot.
                let _ = FillRect(dc, &ps.rcPaint, HBRUSH(GetStockObject(BLACK_BRUSH).0));
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SETCURSOR => {
                SetCursor((*ptr).cursor);
                LRESULT(1)
            }
            WM_MOUSEMOVE => {
                (*ptr).hovered = sample(ptr);
                LRESULT(0)
            }
            WM_LBUTTONDOWN => {
                (*ptr).selected = sample(ptr);
                (*ptr).hovered = (*ptr).selected;
                (*ptr).pressed = true;
                LRESULT(0)
            }
            // Keep the overlay until release, consuming both halves of the click.
            WM_LBUTTONUP if (*ptr).pressed => {
                (*ptr).selected = sample(ptr);
                (*ptr).hovered = (*ptr).selected;
                (*ptr).done = true;
                LRESULT(0)
            }
            WM_RBUTTONDOWN => {
                (*ptr).selected = None;
                (*ptr).right_pressed = true;
                LRESULT(0)
            }
            WM_RBUTTONUP if (*ptr).right_pressed => {
                (*ptr).selected = None;
                (*ptr).done = true;
                LRESULT(0)
            }
            WM_CANCELMODE | WM_CLOSE => {
                (*ptr).selected = None;
                (*ptr).done = true;
                LRESULT(0)
            }
            WM_KEYDOWN | WM_SYSKEYDOWN if wp.0 == VK_ESCAPE.0 as usize => {
                (*ptr).selected = None;
                (*ptr).done = true;
                LRESULT(0)
            }
            WM_CAPTURECHANGED if lp.0 != hwnd.0 as isize => {
                (*ptr).selected = None;
                (*ptr).done = true;
                LRESULT(0)
            }
            WM_ACTIVATEAPP if wp.0 == 0 => {
                (*ptr).selected = None;
                (*ptr).done = true;
                LRESULT(0)
            }
            WM_DESTROY => {
                (*ptr).selected = None;
                (*ptr).done = true;
                LRESULT(0)
            }
            WM_NCDESTROY => {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                DefWindowProcW(hwnd, msg, wp, lp)
            }
            // A single physical-pixel surface spans all monitors. A DPI seam must not resize it.
            WM_DPICHANGED => LRESULT(0),
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}

unsafe fn sample(state: *const State) -> Option<u32> {
    unsafe {
        let mut pt = POINT::default();
        GetCursorPos(&mut pt).ok()?;
        let (x, y) = (*state).bounds.local(pt.x, pt.y)?;
        let colour = GetPixel((*state).snapshot.dc, x, y).0;
        (colour != u32::MAX).then_some(colour)
    }
}

// Independent monochrome eyedropper artwork; no installed DLL/resource is loaded.
// A monochrome cursor stores AND rows followed by XOR rows, MSB first, WORD aligned.
fn eyedropper_mask() -> [u8; 256] {
    let mut bits = [0u8; 256];
    bits[..128].fill(255);
    for y in 0i32..32 {
        for x in 0i32..32 {
            let u = x + y;
            let v = x - y;
            let body = (28..=34).contains(&u) && (-19..=12).contains(&v);
            let tip = (30..=32).contains(&u) && (-23..=-19).contains(&v);
            let collar = (25..=37).contains(&u) && (12..=18).contains(&v);
            let bulb = (27..=35).contains(&u) && (18..=26).contains(&v);
            if body || tip || collar || bulb {
                let byte = y as usize * 4 + x as usize / 8;
                let bit = 0x80 >> (x as usize % 8);
                bits[byte] &= !bit;
                if body && (30..=32).contains(&u) && (-17..=10).contains(&v) {
                    bits[128 + byte] |= bit;
                }
            }
        }
    }
    bits
}

struct PickerCursor(HCURSOR);
impl PickerCursor {
    unsafe fn create() -> Option<Self> {
        unsafe {
            let bits = eyedropper_mask();
            let mask = CreateBitmap(32, 64, 1, 1, Some(bits.as_ptr().cast()));
            if mask.is_invalid() { return None; }
            let result = CreateIconIndirect(&ICONINFO {
                fIcon: false.into(), xHotspot: 4, yHotspot: 27,
                hbmMask: mask, hbmColor: HBITMAP::default(),
            }).ok().map(|icon| Self(HCURSOR(icon.0)));
            let _ = DeleteObject(mask);
            result
        }
    }
}
impl Drop for PickerCursor {
    fn drop(&mut self) { unsafe { let _ = DestroyCursor(self.0); } }
}

struct DpiGuard(DPI_AWARENESS_CONTEXT);
impl Drop for DpiGuard {
    fn drop(&mut self) {
        unsafe {
            SetThreadDpiAwarenessContext(self.0);
        }
    }
}

/// Pick a COLORREF (0x00BBGGRR) from any monitor. Esc, right click, lost capture, or failure cancels.
/// Must run on the owner's UI thread, without holding a borrow of settings/editor window state:
/// this function dispatches a nested modal message loop. The owner is disabled for its duration.
pub fn pick(owner: HWND) -> Option<u32> {
    pick_with_preview(owner, |_| {})
}

/// Report each changed hovered colour without holding picker state across the callback.
/// The caller restores its previous model when this returns None, and persists only on Some.
/// Sampling stays frozen at activation, so recolouring a Corral does not feed into subsequent picks.
pub fn pick_with_preview(owner: HWND, mut preview: impl FnMut(u32)) -> Option<u32> {
    unsafe {
        let _dpi = DpiGuard(SetThreadDpiAwarenessContext(
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        ));
        let bounds = Bounds {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN),
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN),
        };
        if bounds.width <= 0 || bounds.height <= 0 {
            return None;
        }
        let cursor = PickerCursor::create()?;
        let mut state = State {
            cursor: cursor.0,
            hovered: None,
            snapshot: Snapshot::capture(bounds)?,
            bounds,
            selected: None,
            pressed: false,
            right_pressed: false,
            done: false,
        };
        let instance = HINSTANCE(GetModuleHandleW(None).ok()?.0);
        let class = w!("CorralScreenColourPicker");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: instance,
            // Per-pick cursor lives in State; a registered class must not retain a stale handle.
            hCursor: HCURSOR::default(),
            lpszClassName: class,
            ..Default::default()
        };
        // The class can already exist after a previous pick; its procedure and module are identical.
        RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
            class,
            w!("Pick a colour — Esc to cancel"),
            WS_POPUP,
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
            owner,
            None,
            instance,
            Some(&mut state as *mut State as *const std::ffi::c_void),
        )
        .ok()?;
        if SetLayeredWindowAttributes(hwnd, windows::Win32::Foundation::COLORREF(0), 1, LWA_ALPHA).is_err() {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(hwnd);
            return None;
        }
        let was_enabled = IsWindowEnabled(owner).as_bool();
        if was_enabled {
            let _ = EnableWindow(owner, false);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
        SetCapture(hwnd);
        SetCursor(cursor.0);
        if GetCapture() != hwnd {
            state.done = true;
        }
        let _ = UpdateWindow(hwnd);
        let mut message = MSG::default();
        let mut quit = None;
        let mut last_preview = None;
        while !state.done {
            let result = GetMessageW(&mut message, HWND::default(), 0, 0).0;
            if result <= 0 {
                state.selected = None;
                if result == 0 {
                    quit = Some(message.wParam.0 as i32);
                }
                break;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
            if state.hovered != last_preview {
                last_preview = state.hovered;
                if let Some(colour) = last_preview { preview(colour); }
            }
        }
        let result = state.selected;
        if GetCapture() == hwnd {
            let _ = ReleaseCapture();
        }
        // Clear the pointer before destroying the window so cancellation does not change the result.
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let _ = DestroyWindow(hwnd);
        if let Ok(arrow) = LoadCursorW(None, IDC_ARROW) { SetCursor(arrow); }
        if was_enabled {
            let _ = EnableWindow(owner, true);
            if IsWindow(owner).as_bool() {
                let _ = SetForegroundWindow(owner);
                let _ = SetFocus(owner);
            }
        }
        if let Some(code) = quit {
            PostQuitMessage(code);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn virtual_desktop_coordinates_include_negative_monitors() {
        let b = Bounds {
            x: -2560,
            y: -400,
            width: 6400,
            height: 2560,
        };
        assert_eq!(b.local(-2560, -400), Some((0, 0)));
        assert_eq!(b.local(0, 0), Some((2560, 400)));
        assert_eq!(b.local(3839, 2159), Some((6399, 2559)));
        assert_eq!(b.local(-2561, 0), None);
        assert_eq!(b.local(3840, 0), None);
        assert_eq!(b.local(0, 2160), None);
    }
    #[test]
    fn invalid_or_overflowing_screen_positions_are_rejected() {
        let b = Bounds {
            x: -2560,
            y: 0,
            width: 6400,
            height: 2160,
        };
        assert_eq!(b.local(i32::MAX, 0), None);
        assert_eq!(b.local(0, -1), None);
    }
}
