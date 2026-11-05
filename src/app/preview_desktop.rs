//! Preview-only handover to a separate desktop process.

use super::*;
use crate::dialog;
use std::cell::Cell;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use windows::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

pub(super) const USE_DESKTOP_ID: usize = 0x7e01;

pub(super) fn attach_menu(hwnd: HWND) -> windows::core::Result<()> {
    unsafe {
        let menu = CreateMenu()?;
        if let Err(error) = AppendMenuW(menu, MF_STRING, USE_DESKTOP_ID, w!("Use on my desktop"))
            .and_then(|_| SetMenu(hwnd, menu))
        {
            let _ = DestroyMenu(menu);
            let _ = DestroyWindow(hwnd);
            return Err(error);
        }
        // The window owns the menu now. Attach before measuring its client area or creating layers.
        let _ = DrawMenuBar(hwnd);
        Ok(())
    }
}

fn desktop_command(exe: &Path, layout: &Path, settings: &Path) -> Command {
    let mut command = Command::new(exe);
    command.arg("--desktop").arg("--autostart")
        .arg("--settings-path").arg(settings).arg("--layout").arg(layout)
        .creation_flags(DETACHED_PROCESS.0 | CREATE_NEW_PROCESS_GROUP.0)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    command
}

#[derive(Debug, PartialEq, Eq)]
enum Handover {
    AlreadyRunning,
    Started,
    StartFailed(String),
}

/// Keep the decision independent of mutexes, processes and window messages.
fn handover(already_running: bool, start: impl FnOnce() -> Result<(), String>) -> Handover {
    if already_running { return Handover::AlreadyRunning; }
    match start() {
        Ok(()) => Handover::Started,
        Err(error) => Handover::StartFailed(error),
    }
}

thread_local! { static CONFIRMING: Cell<bool> = const { Cell::new(false) }; }

pub(super) fn dispatch(owner: HWND) -> bool {
    let preview = APP.with(|cell| cell.try_borrow().ok().is_some_and(|guard|
        guard.as_ref().is_some_and(|app| app.preview_host == Some(owner))));
    if !preview { return false; }
    if CONFIRMING.with(|busy| busy.replace(true)) { return true; }
    // No APP borrow survives any dialog's nested message loop.
    if dialog::confirm(owner, dialog::system_light(), "Use Corral on your desktop?",
        "Corral will replace the Windows desktop icons with your Corrals. Your files stay where they are. You can turn Corral off at any time in Settings, and you can make it start with Windows there too.",
        "Use on my desktop", false)
    {
        // Drop our probe before spawning: the desktop child must acquire the lock itself.
        let lock = desktop::take_desktop_lock();
        let running = matches!(lock, desktop::LockOutcome::Taken);
        let unavailable = matches!(lock, desktop::LockOutcome::Unavailable);
        drop(lock);
        let outcome = handover(running, || {
            if unavailable { return Err("Couldn't check whether Corral is already running. Please try again.".into()); }
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let command = APP.with(|cell| {
                let guard = cell.borrow();
                let app = guard.as_ref().filter(|app| app.preview_host == Some(owner))?;
                app.save();
                Some(desktop_command(&exe, &app.opts.layout_path, &app.opts.settings_path))
            });
            // Dropping Child closes our handles; it does not terminate the detached process.
            command.ok_or_else(|| "The preview has closed.".to_string())?
                .spawn().map(drop).map_err(|e| e.to_string())
        });
        match outcome {
            Handover::AlreadyRunning => dialog::notice(owner, dialog::system_light(), "Corral",
                "Corral is already running on the desktop."),
            Handover::StartFailed(error) => dialog::notice(owner, dialog::system_light(),
                "Couldn't start Corral on the desktop", &error),
            Handover::Started => unsafe {
                // The same quit path as the preview's close button: run() saves and shuts down.
                let _ = PostMessageW(owner, WM_CLOSE, WPARAM(0), LPARAM(0));
            },
        }
    }
    CONFIRMING.with(|busy| busy.set(false));
    true
}

/// Decode just as requested_body does, then use placement_in's nearest-monitor fallback.
fn preview_destination<'a>(group: &model::Group, live: &'a Snapshot)
    -> Option<&'a dpi_transition::DisplaySnapshot> {
    let fallback = live.displays().first()?;
    let r = RequestedBody::from_dips(group.rect, fallback)?.physical();
    live.nearest((r.left + r.width() / 2, r.top + r.height() / 2))
}

/// PREVIEW is a synthetic workspace, never a monitor to return to. Fit and contain the
/// whole expanded envelope on its permanent destination, retaining the preferred DIP size.
fn adopt_preview_group(group: &model::Group, live: &Snapshot,
    d: &dpi_transition::DisplaySnapshot, minimum: (i32, i32), metrics: group_paint::Metrics,
    auto_adjust: bool) -> Option<dpi_transition::PlacementDecision> {
    let preferred = group.preferred_size_dip.filter(|(w, h)| *w > 0 && *h > 0)
        .unwrap_or((group.rect.width(), group.rect.height()));
    let requested = RequestedBody::from_dips(group.rect, d)?;
    let (mut p, _) = fitted_body_placement(requested, live, d, preferred, minimum, metrics.title_h + metrics.gap)?;
    for _ in 0..4 {
        let body = p.requested.physical();
        let below = title_below_on_screen(group.title_side, group.title_side_last,
            auto_adjust, false, body, d.work.top, &metrics, false);
        let title = group_paint::title_rect_on(body, below, &metrics);
        let envelope = group_paint::union_rect(body, title, false);
        let contained = dpi_transition::contain_fitted(p.requested, envelope, d)?;
        if contained == p.requested { break; }
        p = fitted_body_placement(contained, live, d, preferred, minimum, metrics.title_h + metrics.gap)?.0;
    }
    Some(p)
}

fn forget_preview_home(group: &mut model::Group) {
    if group.rescued_home.as_ref().is_some_and(|h| h.monitor.eq_ignore_ascii_case("PREVIEW")) {
        group.rescued_home = None;
    }
}

impl App {
    pub(super) fn adopt_preview_groups(&mut self) {
        if self.opts.mode != Mode::Desktop { return; }
        let Some(live) = self.display_snapshot.as_ref() else { return };
        let placements: Vec<_> = self.layout.groups.iter()
            .filter(|g| g.monitor.eq_ignore_ascii_case("PREVIEW"))
            .filter_map(|g| {
                let d = preview_destination(g, live)?;
                let metrics = self.group_title_metrics(g, d.dpi);
                let requested = RequestedBody::from_dips(g.rect, d)?.physical();
                let min = self.min_body_px(d.dpi, Some((requested.width(), requested.height())));
                Some((g.id, adopt_preview_group(g, live, d, min, metrics, self.settings.title_edge_auto_adjust)?))
            }).collect();
        for (id, p) in placements {
            store_body_placement(&mut self.layout, &mut self.runtime_bodies, id, p);
        }
        for g in &mut self.layout.groups {
            forget_preview_home(g);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn already_running_does_not_save_or_spawn() {
        assert_eq!(handover(true, || panic!("must not start")), Handover::AlreadyRunning);
    }

    #[test]
    fn started_quits_and_failed_start_stays_in_preview() {
        assert_eq!(handover(false, || Ok(())), Handover::Started);
        assert_eq!(handover(false, || Err("spawn failed".into())), Handover::StartFailed("spawn failed".into()));
    }

    #[test]
    fn command_preserves_paths_with_spaces_as_separate_arguments() {
        let exe = Path::new(r"C:\Downloads folder\corral.exe");
        let layout = Path::new(r"C:\My Corrals\layout file.json");
        let settings = Path::new(r"C:\My Corrals\settings file.json");
        let cmd = desktop_command(exe, layout, settings);
        assert_eq!(cmd.get_program(), exe.as_os_str());
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(args, [std::ffi::OsStr::new("--desktop"), std::ffi::OsStr::new("--autostart"),
            std::ffi::OsStr::new("--settings-path"), settings.as_os_str(),
            std::ffi::OsStr::new("--layout"), layout.as_os_str()]);
    }

    #[test]
    fn saved_preview_groups_fit_real_work_areas_and_never_return_to_preview() {
        for dpi in [96, 120, 144, 192, 240] {
            let monitor = Monitor { name: "DISPLAY".into(), dpi,
                bounds: Rect::new(-1600, -900, 0, 0), work: Rect::new(-1552, -860, 0, -48) };
            let live = Snapshot::new(vec![dpi_transition::DisplaySnapshot::from_monitor(&monitor, true, None)]).unwrap();
            for rect in [Rect::new(50, 60, 450, 360), Rect::new(1100, 700, 2000, 1700),
                Rect::new(-500, -500, 1800, 1500)] {
                for auto in [false, true] {
                    for side in [-1, 0, 1] {
                        let mut saved = Layout::new();
                        let id = saved.add_group("Preview group".into(), "PREVIEW".into(), rect,
                            model::GroupContent::Items { items: vec!["fixture-item".into()] });
                        saved.group_mut(id).unwrap().title_side = side;
                        let mut layout: Layout = serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
                        let g = &layout.groups[0];
                        let d = preview_destination(g, &live).unwrap();
                        let metrics = group_paint::metrics(dpi);
                        let p = adopt_preview_group(g, &live, d, (to_px(100, dpi), to_px(80, dpi)), metrics, auto).unwrap();
                        let body = p.requested.physical();
                        let below = title_below_on_screen(side, -1, auto, false, body, d.work.top, &metrics, false);
                        let envelope = group_paint::union_rect(body, group_paint::title_rect_on(body, below, &metrics), false);
                        assert!(envelope.left >= d.work.left && envelope.top >= d.work.top
                            && envelope.right <= d.work.right && envelope.bottom <= d.work.bottom,
                            "dpi={dpi}, rect={rect:?}, side={side}, auto={auto}, envelope={envelope:?}");
                        store_body_placement(&mut layout, &mut HashMap::new(), id, p);
                        let g = layout.group_mut(id).unwrap();
                        forget_preview_home(g);
                        // The normal startup rescue no longer regards its permanent display as missing.
                        assert!(live.find(&g.monitor).is_some());
                        assert!(!g.return_home(|name| live.find(name).is_some()));
                        assert_eq!(g.rescued_home, None);
                        assert_eq!(g.monitor, "DISPLAY");
                        assert_eq!(g.preferred_size_dip, Some((rect.width(), rect.height())));
                        assert_eq!(g.content, saved.groups[0].content);
                        // Save/restart, then even opening a preview cannot return this group to PREVIEW.
                        let mut restarted: Layout = serde_json::from_str(&serde_json::to_string(&layout).unwrap()).unwrap();
                        assert!(!restarted.groups[0].return_home(|_| true));
                        assert_eq!(restarted.groups[0].monitor, "DISPLAY");
                        let decoded = RequestedBody::from_dips(restarted.groups[0].rect, d).unwrap().physical();
                        // Persistence rounds onto the DIP grid; visibility must survive that rounding.
                        assert!(decoded.left < d.work.right && decoded.right > d.work.left
                            && decoded.top < d.work.bottom && decoded.bottom > d.work.top);
                    }
                }
            }
        }
    }

    #[test]
    fn old_preview_rescue_is_forgotten_but_real_monitor_rescue_is_preserved() {
        let mut layout = Layout::new();
        let id = layout.add_group("Group".into(), "PREVIEW".into(), Rect::new(10, 50, 410, 350),
            model::GroupContent::Items { items: vec![] });
        let g = layout.group_mut(id).unwrap();
        g.note_monitor_gone();
        assert_eq!(g.rescued_home.as_ref().unwrap().monitor, "PREVIEW");
        g.monitor = "DISPLAY".into();
        forget_preview_home(g);
        assert_eq!(g.rescued_home, None);
        g.note_monitor_gone();
        forget_preview_home(g);
        assert_eq!(g.rescued_home.as_ref().unwrap().monitor, "DISPLAY");
    }
}
