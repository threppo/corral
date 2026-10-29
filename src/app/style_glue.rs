//! Layout-only settings requests. The engine saves its current layout, avoiding a
//! stale settings-window read/modify/write replacing geometry, membership or portal state.
//!
//! Colour edits follow `docs/design/colour-state-spec.md`: draft frames only show on top of the
//! stored look (the draft overlay), a commit is one atomic edit, and a failed write puts the
//! stored look back and tells the settings window.
use super::{App, Drag, Mode, log};
use crate::color_state::{self, overlay::OverlayError};
use crate::model::{self, Layout};
use crate::settings::{self, Settings, style_request::{self, Action, RequestResult, status}};
use super::snapshot_glue::{request_gate, shares_real_settings, Gate};

fn color_log(message: &str) {
    if std::env::var_os("CORRAL_QA_COLOR_EDITOR").is_some() { log(&format!("color editor: {message}")); }
}

// Receive ownership is independent of APP: sent messages may reenter while APP is borrowed.
use std::cell::RefCell;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
pub(super) const WM_APPLY_COLOR: u32 = WM_APP + 0x171;
#[derive(Default)]
struct Inbox {
    hwnd: isize,
    path: std::path::PathBuf,
    isolated: bool,
    ids: Vec<u64>,
    pending: style_request::PendingColors,
    posted: bool,
    retries: u8,
    reload: bool,
}
thread_local! { static INBOX: RefCell<Inbox> = RefCell::new(Inbox::default()); }

/// Every settings window gets `message` (the engine has no handle of its own).
fn post_to_settings_windows(message: u32, wparam: usize, lparam: isize) {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowExW;
    let mut after = HWND::default();
    for _ in 0..16 {
        let Ok(settings) = (unsafe { FindWindowExW(HWND::default(), after, windows::core::w!("CorralSettings"), None) }) else { break };
        if settings.is_invalid() { break; }
        after = settings;
        unsafe { let _ = PostMessageW(settings, message, WPARAM(wparam), LPARAM(lparam)); }
    }
}

pub(super) fn preview_changed(hwnd: HWND, msg: u32, lparam: LPARAM) {
    use windows::Win32::UI::WindowsAndMessaging::*;
    if !matches!(msg, WM_WINDOWPOSCHANGED | WM_NCDESTROY) { return; }
    if msg == WM_WINDOWPOSCHANGED {
        if lparam.0 == 0 { return; }
        let pos = unsafe { &*(lparam.0 as *const WINDOWPOS) };
        if pos.flags.contains(SWP_NOMOVE | SWP_NOSIZE) { return; }
    }
    let mut name = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut name) }.max(0) as usize;
    if !matches!(String::from_utf16_lossy(&name[..len]).as_str(), "CorralGroup" | "CorralSurface") { return; }
    post_to_settings_windows(style_request::WM_PREVIEW_CHANGED, 0, 0);
}
pub(super) fn context(app: &App) {
    INBOX.with(|cell| {
        let mut i = cell.borrow_mut();
        let changed = i.hwnd != app.message_window.0 as isize || i.path != app.opts.layout_path;
        i.hwnd = app.message_window.0 as isize;
        i.ids = app.layout.groups.iter().map(|g| g.id).collect();
        if !changed { return; }
        i.path = app.opts.layout_path.clone();
        i.isolated = shares_real_settings(matches!(app.opts.mode, Mode::Preview), &app.opts.settings_path, &settings::default_path())
            || (matches!(app.opts.mode, Mode::Preview) && style_request::same_layout(&app.opts.layout_path, &crate::default_layout_path()));
    });
}
pub(super) fn post_pending(reset: bool) {
    INBOX.with(|cell| {
        let mut i = cell.borrow_mut();
        if reset { i.retries = 0; }
        if i.posted || (i.pending.values.is_empty() && !i.reload) || i.hwnd == 0 || i.retries >= 8 { return; }
        i.posted = unsafe { PostMessageW(HWND(i.hwnd as *mut _), WM_APPLY_COLOR, WPARAM(0), LPARAM(0)) }.is_ok();
        i.retries += 1;
    });
}
pub(super) fn receive(hwnd: HWND, lparam: LPARAM) -> LRESULT {
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    if lparam.0 == 0 { return LRESULT(0); }
    let data = unsafe { std::ptr::read_unaligned(lparam.0 as *const COPYDATASTRUCT) };
    if data.dwData == style_request::RELOAD_MAGIC { return receive_reload(hwnd, &data); }
    if data.dwData != style_request::LIVE_MAGIC { return LRESULT(0); }
    if data.cbData == 0 || data.cbData as usize > style_request::LIVE_MAX_BYTES || data.lpData.is_null() { return LRESULT(2); }
    let bytes = unsafe { std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize) };
    let r = match style_request::LiveRequest::decode(data.dwData, bytes) {
        Ok(r) => r, Err(e) => { color_log(&format!("reject: {e}")); return LRESULT(2); }
    };
    let result = INBOX.with(|cell| {
        let mut i = cell.borrow_mut();
        if i.hwnd != hwnd.0 as isize || !style_request::same_layout(&r.layout_path, &i.path) { return LRESULT(0); }
        if i.isolated || r.target.is_some_and(|id| !i.ids.contains(&id)) { return LRESULT(2); }
        if !i.pending.push(r) { color_log("reject: inbox full"); return LRESULT(2); } // JSON and all borrowed COPYDATA bytes now owned.
        LRESULT(1)
    });
    post_pending(true);
    result
}
fn receive_reload(hwnd: HWND, data: &windows::Win32::System::DataExchange::COPYDATASTRUCT) -> LRESULT {
    if data.cbData == 0 || data.cbData as usize > style_request::RELOAD_MAX_BYTES * 4 || data.lpData.is_null() { return LRESULT(2); }
    let bytes = unsafe { std::slice::from_raw_parts(data.lpData as *const u8, data.cbData as usize) };
    let r = match style_request::ReloadRequest::decode(data.dwData, bytes) {
        Ok(r) => r, Err(e) => { color_log(&format!("reload reject: {e}")); return LRESULT(2); }
    };
    let result = INBOX.with(|cell| {
        let mut i = cell.borrow_mut();
        if i.hwnd != hwnd.0 as isize || !style_request::same_layout(&r.layout_path, &i.path) { return LRESULT(0); }
        if i.isolated { return LRESULT(2); }
        i.reload = true; // coalesced; applied by the queued, non-reentrant handler
        LRESULT(1)
    });
    post_pending(true);
    result
}
pub(super) fn dispatch() {
    INBOX.with(|c| c.borrow_mut().posted = false);
    let free = super::APP.with(|cell| {
        let Ok(mut app) = cell.try_borrow_mut() else { return false };
        let Some(app) = app.as_mut() else { return true };
        if !matches!(app.drag, Drag::None) || !app.snapshot_may_save_layout() || app.profile_pending.is_some() { return false; }
        let reload = INBOX.with(|c| std::mem::take(&mut c.borrow_mut().reload));
        if reload {
            app.reload_settings();
            if !app.settings.swipe_enabled { app.page_cancel_motion(); }
            app.page_refresh();
        }
        let pending = INBOX.with(|c| c.borrow_mut().pending.values.pop_front());
        if let Some(r) = pending {
            if let Err(e) = app.apply_color_request(&r) {
                color_log(&format!("deferred failure: {e}"));
                // A commit that could not be stored (missing Corral, write error, ...): the page puts
                // the stored look back and says so (colour-state-spec 3.4).
                if r.persist { post_to_settings_windows(style_request::WM_COLOR_STATUS, status::ENGINE_SAVE_FAILED, r.session as isize); }
            }
        }
        context(app);
        true
    });
    post_pending(free);
}

/// One logical edit over the two stores (colour-state-spec 4). `backup` runs before anything is
/// written, only for `ResetAll`; a failing backup stops the reset. `write_layout` and
/// `write_settings` are the atomic writers; a failing second write puts the first back.
/// The settings file is built from what is on disk (`read_settings`) plus the shared colours only,
/// so runtime-only state of the engine's copy (tab-derived exclusions, an older pending request)
/// never overwrites newer disk content. Returns the new in-memory pair.
pub(super) fn plan_style_action(
    stored: (&Layout, &Settings),
    action: &Action,
    backup: &mut dyn FnMut() -> Result<(), String>,
    read_settings: &mut dyn FnMut() -> Settings,
    write_layout: &mut dyn FnMut(&Layout) -> Result<(), String>,
    write_settings: &mut dyn FnMut(&Settings) -> Result<(), String>,
) -> Result<(Layout, Settings), String> {
    let (mut layout, mut settings) = (stored.0.clone(), stored.1.clone());
    style_request::apply(&mut layout, &mut settings, action).map_err(str::to_string)?;
    if matches!(action, Action::ResetAll) {
        backup().map_err(|e| format!("Couldn't save a backup first, so nothing was reset. {e}"))?;
    }
    let shared = matches!(action, Action::ResetBase | Action::ResetAll | Action::EditAll { .. });
    let touches_layout = !matches!(action, Action::ResetBase | Action::EditAll { .. });
    let next_settings = shared.then(|| {
        let mut file = read_settings();
        color_state::Globals::from_settings(&settings).write_to(&mut file);
        file
    });
    if touches_layout {
        color_state::write_pair(stored.0, &layout, next_settings.as_ref(), write_layout, write_settings)?;
    } else if let Some(file) = &next_settings {
        write_settings(file)?;
    }
    Ok((layout, settings))
}

impl App {
    /// The draft overlay ended without a commit (cancel, window closed, stale session, channel
    /// dropped): the stored look is back and the desktop shows it again.
    pub(super) fn end_color_overlay(&mut self) {
        let target = self.color_overlay.current().map(|o| o.target);
        if self.color_overlay.end(&mut self.layout, &mut self.settings) {
            self.color_preview_fast = false;
            self.rerender_colors(target.flatten());
        }
    }

    /// Puts every unfinished appearance preview back to its committed values and forgets it, so a
    /// later save (profile transition, file style action) cannot publish preview colours.
    pub(super) fn abandon_color_preview(&mut self) {
        self.end_color_overlay();
    }

    /// colour-state-spec 3.2: without a settings window nobody can end a draft, so it ends here.
    pub(super) fn color_overlay_housekeeping(&mut self) {
        let owner = self.color_overlay.current().map(|o| color_state::session_owner(o.session));
        if owner.is_some_and(|owner| !style_request::settings_window_of(owner)) {
            color_log("overlay dropped: its settings window is gone");
            self.end_color_overlay();
        }
    }

    fn rerender_colors(&mut self, target: Option<u64>) {
        // Appearance-only preview never goes through sorting/save/shell-layout refresh.
        match target {
            Some(id) => self.rerender_group(id),
            None => {
                self.render_surface();
                for index in 0..self.groups.len() { self.render_group(index); }
            }
        }
    }

    pub(super) fn apply_color_request(&mut self, r: &style_request::LiveRequest) -> Result<(), String> {
        r.validate().map_err(str::to_string)?;
        if !matches!(self.drag, Drag::None) || !self.snapshot_may_save_layout() || self.profile_pending.is_some() {
            return Err("active drag or pending profile/snapshot transition".into());
        }
        if r.cancel {
            // Only the overlay of that session ends; a newer session is left alone.
            if self.color_overlay.current().is_some_and(|o| o.session == r.session) {
                self.end_color_overlay();
            }
            return Ok(());
        }
        if r.persist { return self.commit_colors(r); }
        match self.color_overlay.preview(&mut self.layout, &mut self.settings, r.session, r.target, &r.patch) {
            Ok(()) => {}
            Err(OverlayError::MissingCorral) => return Err("missing Corral target".into()),
        }
        self.color_preview_fast = true;
        self.rerender_colors(r.target);
        color_log(&format!("draft session={} target={:?}", r.session, r.target));
        Ok(())
    }

    /// One atomic edit: the stored look first (a draft of this session is taken off), then the
    /// patch, the layout and the settings written, and only then published (spec 3.4).
    fn commit_colors(&mut self, r: &style_request::LiveRequest) -> Result<(), String> {
        // A draft of this or another session never survives into a commit.
        self.end_color_overlay();
        if r.target.is_some_and(|id| !self.layout.groups.iter().any(|g| g.id == id)) {
            return Err("missing Corral target".into());
        }
        let action = r.target.map_or_else(|| Action::EditAll { patch: r.patch.clone() },
            |id| Action::Edit { id, patch: r.patch.clone() });
        let old = (self.layout.clone(), self.settings.clone());
        let result = self.store_colors(&action);
        match result {
            Ok(()) => {
                self.color_preview_fast = false;
                self.reload_settings();
                if !self.settings.swipe_enabled { self.page_cancel_motion(); }
                self.page_refresh();
                self.settings_stamp = self.settings_file_stamp();
                if self.settings.apply_rules_now_requested || self.settings.always_apply_rules || self.settings.snapshot_request.is_some() { self.settings_stamp = None; }
                let stored = settings::load_with_backup(&self.opts.settings_path);
                self.style_last_nonce = self.style_last_nonce.into_iter()
                    .chain(stored.style_request.as_ref().map(|r| r.nonce))
                    .chain(stored.style_result.as_ref().map(|r| r.nonce)).max();
                color_log(&format!("commit target={:?} session={}", r.target, r.session));
                Ok(())
            }
            Err(e) => {
                // The previous colours are back, on screen and in memory.
                (self.layout, self.settings) = old;
                self.color_preview_fast = false;
                self.rerender_colors(r.target);
                Err(e)
            }
        }
    }

    /// Apply `action` to the in-memory pair and write it; in-memory state is published only after
    /// the files are on disk. The caller restores its own copy when this fails.
    fn store_colors(&mut self, action: &Action) -> Result<(), String> {
        let layout_path = self.opts.layout_path.clone();
        let settings_path = self.opts.settings_path.clone();
        // What is on disk right now is the rollback copy; legacy per-Corral tint choices are made
        // explicit so a rollback does not lose them (camouflage-spec 5.2).
        let mut disk = model::load_with_backup(&layout_path).map_err(|e| format!("loading committed layout: {e}"))?;
        disk.migrate_tint_sources(self.settings.color);
        let mut to_save = self.layout_to_save().into_owned();
        let mut settings = self.settings.clone();
        style_request::apply(&mut to_save, &mut settings, action).map_err(str::to_string)?;
        for g in &mut to_save.groups { g.style.migrate_tint_source(settings.color, settings.desktop_tint_icons_color); }
        let next_settings = matches!(action, Action::EditAll { .. }).then(|| {
            let mut file = settings::load_with_backup(&settings_path);
            color_state::Globals::from_settings(&settings).write_to(&mut file);
            file
        });
        color_state::write_pair(
            &disk, &to_save, next_settings.as_ref(),
            &mut |l| model::save(&layout_path, l).map_err(|e| e.to_string()),
            &mut |s| settings::save(&settings_path, s).map_err(|e| e.to_string()),
        )?;
        // Published: the live pair takes the same edit.
        style_request::apply(&mut self.layout, &mut self.settings, action).map_err(str::to_string)?;
        Ok(())
    }

    pub(super) fn consume_style_request(&mut self, startup: bool) {
        let Some(request) = self.settings.style_request.clone() else { return };
        // A preview sharing real settings never acknowledges another engine's request.
        if shares_real_settings(matches!(self.opts.mode, Mode::Preview), &self.opts.settings_path, &settings::default_path()) {
            color_log("file reject: preview shares real settings"); return;
        }
        if matches!(self.opts.mode, Mode::Preview) && style_request::same_layout(&self.opts.layout_path, &crate::default_layout_path()) {
            color_log("file reject: preview uses real layout"); return;
        }
        if !style_request::same_layout(&request.layout_path, &self.opts.layout_path) {
            color_log("file reject: layout path mismatch"); return;
        }
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let last = self.style_last_nonce.into_iter().chain(self.settings.style_result.as_ref().map(|r| r.nonce)).max();
        let outcome = if startup {
            Err("The style request was not run because Corral was not running".to_string())
        } else {
            match request_gate(request.nonce, now, last) {
                Gate::Replay => { color_log("file reject: nonce replay"); return },
                Gate::TooOld => Err("The style request was out of date and was not run".into()),
                Gate::Run => self.run_style_action(&request.action),
            }
        };
        if let Err(e) = &outcome { color_log(&format!("file reject: {e}")); }
        self.style_last_nonce = Some(last.map_or(request.nonce, |n| n.max(request.nonce)));
        self.settings.style_request = None;
        self.settings.style_result = Some(RequestResult {
            nonce: request.nonce, ok: outcome.is_ok(),
            message: outcome.err().unwrap_or_else(|| "Group style updated".into()),
        });
        // Preserve unrelated settings and a newer pending action written while saving layout.
        // The shared colours of a successful action are already in the file (`run_style_action`).
        let mut stored = settings::load_with_backup(&self.opts.settings_path);
        if stored.style_request.as_ref().is_some_and(|r| r.nonce == request.nonce) {
            stored.style_request = None;
            stored.style_result = self.settings.style_result.clone();
            if let Err(e) = settings::save(&self.opts.settings_path, &stored) {
                log(&format!("saving style request result failed: {e}"));
            }
            self.settings_stamp = self.settings_file_stamp();
        }
    }

    fn run_style_action(&mut self, action: &Action) -> Result<(), String> {
        if !matches!(self.drag, Drag::None) || !self.snapshot_may_save_layout() || self.profile_pending.is_some() {
            return Err("Finish the current layout operation or display transition before changing group style".into());
        }
        self.abandon_color_preview();
        let layout_path = self.opts.layout_path.clone();
        let settings_path = self.opts.settings_path.clone();
        let settings_path_read = settings_path.clone();
        let stored_layout = self.layout_to_save().into_owned();
        let stored_settings = self.settings.clone();
        // "Reset every Corral" cannot be undone in the app: a backup goes first (colour-state-spec 4).
        let backup_pair = (self.layout_to_save().into_owned(), crate::snapshot::for_capture(&self.settings_to_save()));
        let mut backup = || self.backup_before_reset_all(&backup_pair.0, &backup_pair.1);
        let (live_layout, live_settings) = plan_style_action(
            (&stored_layout, &stored_settings), action, &mut backup,
            &mut || settings::load_with_backup(&settings_path_read),
            &mut |l| model::save(&layout_path, l).map_err(|e| format!("Saving group style failed: {e}")),
            &mut |s| settings::save(&settings_path, s).map_err(|e| format!("Saving group style failed: {e}")),
        )?;
        // The live pair keeps what is not saved (runtime-only layout state): apply the same action to it.
        let mut live = self.layout.clone();
        let mut settings = self.settings.clone();
        style_request::apply(&mut live, &mut settings, action).map_err(str::to_string)?;
        debug_assert_eq!(live.groups.len(), live_layout.groups.len());
        let _ = live_settings;
        self.color_preview_fast = false;
        self.layout = live;
        self.settings = settings;
        crate::tint::invalidate_wallpaper();
        crate::settings::set_layout_flags(crate::settings::LayoutFlags::from_groups(&self.layout.groups));
        self.refresh_tabs();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GroupContent, GroupStyle};
    use crate::settings::style_request::ColorPatch;

    fn fixture() -> (Layout, Settings) {
        let mut l = Layout::new();
        for title in ["A", "B"] {
            l.add_group(title.into(), "m".into(), Default::default(), GroupContent::Items { items: vec![] });
        }
        l.groups[0].style = GroupStyle { color: Some(5), text_color_auto: Some(true), font_weight: Some(700), ..Default::default() };
        let mut s = Settings::default();
        s.color = 9;
        (l, s)
    }

    #[derive(Default)]
    struct Log { events: Vec<&'static str>, layout: Option<Layout>, settings: Option<Settings> }

    /// What is on disk: newer than the engine's copy (a pending request, tab exclusions).
    fn disk() -> Settings {
        let mut d = Settings::default();
        d.quick_hide_exclusions = Some("4,5".into());
        d.style_request = Some(style_request::Request { nonce: 99, layout_path: "QA/layout.json".into(), action: Action::ResetBase });
        d
    }

    fn run(
        action: &Action,
        fail_backup: bool,
        fail_settings: bool,
    ) -> (Result<(Layout, Settings), String>, std::rc::Rc<RefCell<Log>>) {
        let (l, s) = fixture();
        let log = std::rc::Rc::new(RefCell::new(Log::default()));
        let (a, b, c) = (log.clone(), log.clone(), log.clone());
        let result = plan_style_action(
            (&l, &s), action,
            &mut || { a.borrow_mut().events.push("backup"); if fail_backup { Err("disk full".into()) } else { Ok(()) } },
            &mut disk,
            &mut |l| { let mut b = b.borrow_mut(); b.events.push("layout"); b.layout = Some(l.clone()); Ok(()) },
            &mut |s| { let mut c = c.borrow_mut(); c.events.push("settings"); if fail_settings { return Err("denied".into()); } c.settings = Some(s.clone()); Ok(()) },
        );
        (result, log)
    }

    #[test]
    fn reset_all_takes_its_backup_before_any_write_and_the_others_do_not() {
        let (result, log) = run(&Action::ResetAll, false, false);
        assert!(result.is_ok());
        assert_eq!(log.borrow().events, ["backup", "layout", "settings"]);
        for action in [Action::ResetBase, Action::ResetGroup { id: 1 }, Action::EditAll { patch: Default::default() }] {
            let (result, log) = run(&action, false, false);
            assert!(result.is_ok(), "{action:?}");
            assert!(!log.borrow().events.contains(&"backup"), "{action:?}");
        }
    }

    #[test]
    fn a_failing_backup_stops_reset_all_before_anything_is_written() {
        let (result, log) = run(&Action::ResetAll, true, false);
        assert!(result.unwrap_err().contains("nothing was reset"));
        assert_eq!(log.borrow().events, ["backup"]);
    }

    #[test]
    fn reset_all_over_two_stores_restores_the_first_when_the_second_fails() {
        let (result, log) = run(&Action::ResetAll, false, true);
        assert_eq!(result.unwrap_err(), "denied");
        let log = log.borrow();
        assert_eq!(log.events, ["backup", "layout", "settings", "layout"]);
        // The last layout write is the pre-image, untouched by the reset.
        let (before, _) = fixture();
        assert_eq!(log.layout.as_ref().unwrap(), &before);
    }

    #[test]
    fn reset_scopes_write_only_the_stores_they_touch() {
        let (_, log) = run(&Action::ResetBase, false, false);
        assert_eq!(log.borrow().events, ["settings"], "ResetBase touches only the shared settings");
        let (result, log) = run(&Action::ResetGroup { id: 1 }, false, false);
        let (layout, settings) = result.unwrap();
        assert_eq!(log.borrow().events, ["layout"], "ResetGroup touches only the layout");
        assert!(layout.groups[0].style.color.is_none() && layout.groups[0].style.font_weight == Some(700));
        assert_eq!(settings.color, 9);
    }

    #[test]
    fn the_settings_write_keeps_non_colour_disk_content_and_a_newer_request() {
        for action in [Action::ResetBase, Action::ResetAll, Action::EditAll { patch: ColorPatch { color: Some(0x123456), ..Default::default() } }] {
            let (result, log) = run(&action, false, false);
            result.unwrap();
            let log = log.borrow();
            let written = log.settings.as_ref().expect("settings written");
            assert_eq!(written.quick_hide_exclusions.as_deref(), Some("4,5"), "{action:?}: runtime-only state does not overwrite disk");
            assert_eq!(written.style_request.as_ref().map(|r| r.nonce), Some(99), "{action:?}: the newer request survives");
            let expect = if matches!(action, Action::EditAll { .. }) { 0x123456 } else { Settings::default().color };
            assert_eq!(written.color, expect, "{action:?}: the shared colours are written");
        }
    }

    #[test]
    fn a_missing_corral_is_an_error_without_a_write() {
        let (result, log) = run(&Action::ResetGroup { id: 99 }, false, false);
        assert!(result.is_err());
        assert!(log.borrow().events.is_empty());
    }
}
