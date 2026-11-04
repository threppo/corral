//! "Corral is off" (Settings > Welcome, stored as `Disabled`): Corrals, Gates, loose icons, Glance and Rules stop
//! and Windows shows its own desktop icons again. Nothing is stored differently: the layout stays as it is and
//! returns when Corral is turned on. The process keeps running so the tray icon is there as the way back, also
//! when `ShowInTaskbar` is off (`taskbar_item::tray_wanted`). A Corral that starts while off stays off.
//!
//! What keeps the windows hidden is in the painters: `render_surface`, `render_group_inner` and
//! `page_layer_visible` treat everything as not shown, so no refresh can bring a window back. The page checks
//! (`page_group_visible`, `page_name_visible`) stay true on purpose: new icons are placed around the hidden
//! Corrals, as they are when Corral is on.

use super::{App, desktop, log, peek};
use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow};

impl App {
    /// After a settings load: the part of turning off or on that does not paint. Every caller of
    /// `reload_settings` refreshes afterwards, which hides the groups or brings them back.
    pub(super) fn corral_off_changed(&mut self, was_off: bool) {
        let off = self.settings.disabled;
        if off == was_off {
            return;
        }
        log(&format!("corral {}", if off { "off: desktop icons back to Windows" } else { "on" }));
        self.peek_exit(peek::Cause::Settings); // Glance entered from the tray has no shortcut hook to stop it
        self.taskbar_off_changed();
        if off {
            self.page_cancel_motion();
            self.hide_item_tip();
            self.hide_tab_tip();
            super::zguard::set_active(false); // `restack_for` arms it again when Corral is on
            unsafe {
                let _ = ShowWindow(self.surface_hwnd, SW_HIDE);
            }
            if let Some(host) = self.host {
                desktop::restore_listview(host.listview, host.explorer_pid, host.listview_was_visible);
            }
        } else {
            if let Some(host) = self.host {
                desktop::hide_listview(&host);
            }
            unsafe {
                let _ = ShowWindow(self.surface_hwnd, SW_SHOWNOACTIVATE);
            }
        }
    }
}
