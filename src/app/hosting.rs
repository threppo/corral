//! Hosting: monitor list, desktop/preview parenting, layer windows and z-order, shell change
//! notifications, re-attachment to a restarted Explorer, shutdown.

use crate::desktop;
use crate::geometry::{Monitor, Rect};
use crate::watchdog;
use windows::Win32::Foundation::{HINSTANCE, HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{ClientToScreen, DeleteObject, HGDIOBJ};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};
use super::{App, Mode, TIMER_REATTACH, WM_SHELL_CHANGE, log};

/// What a restack did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Restack {
    /// Preview mode or no desktop host yet.
    Skipped,
    /// Peek is on: only `peek_reapply` ran.
    Peek,
    /// The stack was already in order; nothing was touched.
    Ordered,
    /// The layers were moved back above Progman.
    Fixed,
}

impl App {
    pub(super) fn current_monitors(&self) -> Vec<Monitor> {
        self.current_display_snapshot().map(|s| snapshot_monitors(&s)).unwrap_or_default()
    }

    pub(super) fn current_display_snapshot(&self) -> Option<crate::dpi_transition::Snapshot> {
        match self.opts.mode {
            Mode::Desktop => desktop::display_snapshot(),
            Mode::Preview => {
                let (w, h) = client_size(self.parent);
                let (x, y) = self.origin();
                let dpi = unsafe { GetDpiForWindow(self.parent) }.max(96);
                let rect = Rect::new(x, y, x + w, y + h);
                let m = Monitor { name: "PREVIEW".into(), bounds: rect, work: rect, dpi };
                crate::dpi_transition::Snapshot::new(vec![
                    crate::dpi_transition::DisplaySnapshot::from_monitor(&m, true, None)])
            }
        }
    }

    /// Desktop mode: Corral's layers are top-level popups (children of Progman are not composed on
    /// Windows 11 24H2+, where Progman has WS_EX_NOREDIRECTIONBITMAP
    /// 2026-10-11). Preview mode keeps child windows of the preview host.
    pub(super) fn detached(&self) -> bool {
        self.preview_host.is_none()
    }

    /// Window position for a parent-client point: screen coordinates for popups.
    pub(super) fn window_xy(&self, client: (i32, i32)) -> (i32, i32) {
        if self.detached() {
            let (ox, oy) = self.origin();
            (client.0 + ox, client.1 + oy)
        } else {
            client
        }
    }

    /// One of Corral's own desktop windows (surface, group, scrollbar).
    pub(super) fn is_layer(&self, hwnd: HWND) -> bool {
        hwnd == self.surface_hwnd
            || self.groups.iter().any(|g| g.hwnd == hwnd)
            || self.scrollbars.values().any(|b| b.hwnd() == hwnd)
    }

    /// `restacking` and its atomic mirror for the z-order guard (`zguard`), which runs before the
    /// `APP` borrow and must know that a z change comes from Corral itself.
    pub(super) fn set_restacking(&mut self, on: bool) {
        self.restacking = on;
        super::zguard::set_own_restack(on);
    }

    /// Keep the desktop stack directly above the desktop host, below every other window:
    /// Progman < surface < groups < first foreign window.
    pub(super) fn restack(&mut self) {
        self.restack_for("call");
    }

    /// `restack` with the cause for the log and the outcome. An already ordered stack is left alone
    /// (no `SetWindowPos`, so no needless recomposition and no reorder events), which makes repeated
    /// restacks after a show-desktop press free (`showdesk.rs`).
    pub(super) fn restack_for(&mut self, cause: &str) -> Restack {
        if !self.detached() {
            return Restack::Skipped;
        }
        if self.peek.is_on() {
            self.peek_reapply(); // the band stays lifted; only windows that lost it are lifted again
            return Restack::Peek;
        }
        let Some(base) = self.host.map(|h| h.progman) else { return Restack::Skipped };
        super::zguard::set_active(true); // the stack exists and is ordered from here on
        let mut order = vec![self.surface_hwnd];
        order.extend(self.groups.iter().map(|g| g.hwnd));
        let mut run: Vec<isize> = Vec::new(); // our managed layers directly above Progman, lowest first
        let mut above = unsafe { GetWindow(base, GW_HWNDPREV) }.unwrap_or_default();
        while !above.0.is_null() && (self.is_layer(above) || self.renaming.as_ref().is_some_and(|r| r.0 == above) || self.naming.as_ref().is_some_and(|n| n.0 == above)) {
            if order.contains(&above) {
                run.push(above.0 as isize);
            }
            above = unsafe { GetWindow(above, GW_HWNDPREV) }.unwrap_or_default();
        }
        let wanted: Vec<isize> = order.iter().map(|h| h.0 as isize).collect();
        if super::showdesk::stack_is_ordered(&run, &wanted) {
            return Restack::Ordered;
        }
        let after = if above.0.is_null() { HWND_TOP } else { above };
        self.set_restacking(true);
        for hwnd in order {
            unsafe {
                let _ = SetWindowPos(hwnd, after, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
            }
        }
        self.set_restacking(false);
        log(&format!("restack: order restored ({cause}; {} of {} layers were directly above Progman in order)", run.len(), wanted.len()));
        Restack::Fixed
    }

    /// Retry re-attachment every 500 ms until Explorer's new desktop host is ready.
    pub(super) fn schedule_reattach(&mut self) {
        if self.opts.mode != Mode::Desktop {
            return;
        }
        log("reattach: scheduled");
        self.peek_exit(super::peek::Cause::Reattach); // the lifted windows are about to be destroyed
        unsafe {
            SetTimer(self.message_window, TIMER_REATTACH, 500, None);
        }
    }

    /// Rebuild hosting after an Explorer restart: new host, new watchdog, new child windows, list
    /// view hidden again, items and monitors reloaded. Returns false while the host is not ready.
    pub(super) fn reattach(&mut self) -> bool {
        let Some(host) = desktop::find_host() else { return false };
        if Some(host.listview) == self.host.map(|h| h.listview) && desktop::is_same_listview(host.listview, host.explorer_pid) {
            return true; // still attached to a live host
        }
        log(&format!("reattach: new host {}", desktop::describe(&host)));
        super::zguard::set_active(false); // the windows below are destroyed and rebuilt; restack re-arms it
        if let Some(mut old) = self.watchdog.take() {
            let _ = old.kill(); // its list view handle belongs to the dead Explorer
        }
        let args = watchdog::Args {
            parent_pid: std::process::id(),
            listview: host.listview.0 as usize,
            explorer_pid: host.explorer_pid,
            was_visible: host.listview_was_visible,
        };
        match watchdog::spawn(&args) {
            Ok(child) => self.watchdog = Some(child),
            Err(e) => {
                log(&format!("reattach: watchdog spawn failed: {e}; not hiding the list view"));
                return false;
            }
        }
        self.drop_registrations.clear();
        // Unregister Gate folder notifications before rebuilding the windows.
        self.portals.clear();
        for group in self.groups.drain(..) {
            unsafe {
                let _ = DestroyWindow(group.hwnd);
            }
        }
        unsafe {
            let _ = DestroyWindow(self.surface_hwnd);
        }
        self.parent = host.progman;
        self.host = Some(host);
        let (width, height) = client_size(self.parent);
        let Ok(surface_hwnd) = create_layer(w!("CorralSurface"), self.parent, true, Rect::new(0, 0, width, height), HWND::default(), self.instance) else {
            return false;
        };
        self.surface_hwnd = surface_hwnd;
        desktop::hide_listview(&host);
        self.restack();
        self.profile_displays_changed();
        self.load_items();
        self.refresh_all();
        true
    }

    pub(super) fn shutdown(mut self) {
        self.desk_save_timer();
        super::zguard::set_active(false);
        self.peek_shutdown();
        self.taskbar_shutdown();
        self.drop_registrations.clear();
        self.portals.clear();
        if let Some((edit, _, font)) = self.naming {
            unsafe {
                let _ = DestroyWindow(edit);
                let _ = DeleteObject(HGDIOBJ(font.0));
            }
        }
        for group in &self.groups {
            unsafe {
                let _ = DestroyWindow(group.hwnd);
            }
        }
        unsafe {
            let _ = DestroyWindow(self.surface_hwnd);
            let _ = DestroyWindow(self.message_window);
        }
        if let Some(host) = self.host {
            desktop::restore_listview(host.listview, host.explorer_pid, host.listview_was_visible);
        }
    }
}

pub(super) fn snapshot_monitors(snapshot: &crate::dpi_transition::Snapshot) -> Vec<Monitor> {
    snapshot.displays().iter().map(|d| Monitor {
        name: d.device.clone(), bounds: d.bounds, work: d.work, dpi: d.dpi,
    }).collect()
}

/// Register for desktop namespace changes (create/delete/rename/update, recursive off).
pub(super) fn register_desktop_notifications(hwnd: HWND) {
    use windows::Win32::UI::Shell::{
        SHCNE_CREATE, SHCNE_DELETE, SHCNE_MKDIR, SHCNE_RENAMEFOLDER, SHCNE_RENAMEITEM, SHCNE_RMDIR, SHCNE_UPDATEDIR,
        SHCNE_UPDATEITEM, SHCNRF_InterruptLevel, SHCNRF_ShellLevel, SHChangeNotifyEntry, SHChangeNotifyRegister,
        SHGetKnownFolderIDList, FOLDERID_Desktop, KF_FLAG_DEFAULT,
    };
    unsafe {
        let Ok(pidl) = SHGetKnownFolderIDList(&FOLDERID_Desktop, KF_FLAG_DEFAULT.0 as u32, None) else { return };
        let entry = SHChangeNotifyEntry { pidl, fRecursive: false.into() };
        let events = SHCNE_CREATE.0 | SHCNE_DELETE.0 | SHCNE_MKDIR.0 | SHCNE_RMDIR.0 | SHCNE_RENAMEITEM.0
            | SHCNE_RENAMEFOLDER.0 | SHCNE_UPDATEDIR.0 | SHCNE_UPDATEITEM.0;
        let id = SHChangeNotifyRegister(hwnd, SHCNRF_ShellLevel | SHCNRF_InterruptLevel, events as i32, WM_SHELL_CHANGE, 1, &entry);
        log(&format!("desktop change notifications registered: id={id}"));
        CoTaskMemFree(Some(pidl as *const _));
    }
}

/// The same notifications for the folder a Gate shows (corral-views-spec 2.2); 0 when it cannot be watched.
pub(super) fn register_folder_notifications(hwnd: HWND, path: &str) -> u32 {
    use windows::Win32::UI::Shell::{
        ILCreateFromPathW, ILFree, SHCNE_CREATE, SHCNE_DELETE, SHCNE_MKDIR, SHCNE_RENAMEFOLDER, SHCNE_RENAMEITEM,
        SHCNE_RMDIR, SHCNE_UPDATEDIR, SHCNE_UPDATEITEM, SHCNRF_InterruptLevel, SHCNRF_ShellLevel, SHChangeNotifyEntry,
        SHChangeNotifyRegister,
    };
    unsafe {
        let pidl = ILCreateFromPathW(&windows::core::HSTRING::from(path));
        if pidl.is_null() { return 0; }
        let entry = SHChangeNotifyEntry { pidl, fRecursive: false.into() };
        let events = SHCNE_CREATE.0 | SHCNE_DELETE.0 | SHCNE_MKDIR.0 | SHCNE_RMDIR.0 | SHCNE_RENAMEITEM.0
            | SHCNE_RENAMEFOLDER.0 | SHCNE_UPDATEDIR.0 | SHCNE_UPDATEITEM.0;
        let id = SHChangeNotifyRegister(hwnd, SHCNRF_ShellLevel | SHCNRF_InterruptLevel, events as i32, WM_SHELL_CHANGE, 1, &entry);
        ILFree(Some(pidl));
        id
    }
}

pub(super) fn deregister_notifications(id: u32) {
    if id != 0 {
        unsafe { let _ = windows::Win32::UI::Shell::SHChangeNotifyDeregister(id); }
    }
}

/// A layered Corral window at a parent-client rect: a top-level popup owned by `owner` (desktop
/// mode) or a child of `parent` (preview).
pub(super) fn create_layer(class: PCWSTR, parent: HWND, detached: bool, rect: Rect, owner: HWND, instance: HINSTANCE) -> windows::core::Result<HWND> {
    let (w, h) = (rect.width().max(1), rect.height().max(1));
    unsafe {
        if detached {
            let mut origin = POINT::default();
            let _ = ClientToScreen(parent, &mut origin);
            CreateWindowExW(WS_EX_LAYERED | WS_EX_TOOLWINDOW, class, PCWSTR::null(), WS_POPUP | WS_VISIBLE,
                rect.left + origin.x, rect.top + origin.y, w, h, owner, HMENU::default(), instance, None)
        } else {
            CreateWindowExW(WS_EX_LAYERED, class, PCWSTR::null(), WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS,
                rect.left, rect.top, w, h, parent, HMENU::default(), instance, None)
        }
    }
}

pub(super) fn client_size(hwnd: HWND) -> (i32, i32) {
    let mut rect = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
    }
    (rect.right - rect.left, rect.bottom - rect.top)
}

#[cfg(test)]
mod snapshot_adapter_tests {
    use super::*;
    #[test]
    fn adapter_retains_negative_origin_work_dpi_and_explicit_primary() {
        let snapshot=crate::app::dpi_wiring_tests::displays(false);
        let monitors=snapshot_monitors(&snapshot);
        assert_eq!(monitors[0].bounds.left,-2560);
        assert_eq!(monitors[1].dpi,240);
        assert!(snapshot.displays()[0].primary); // primary is not at origin (0,0)
        assert!(!snapshot.displays()[1].primary);
        for (m,d) in monitors.iter().zip(snapshot.displays()) {
            assert_eq!((&m.name,m.bounds,m.work,m.dpi),(&d.device,d.bounds,d.work,d.dpi));
        }
    }
}
