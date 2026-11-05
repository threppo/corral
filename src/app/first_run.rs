//! Optional first-run arrangement. Only layout references change; desktop files are read-only.

use super::{APP, App};
use crate::dialog::{self, Action, DialogSpec};
use crate::geometry::{Monitor, Rect, to_px};
use crate::icon_metrics::{self, CellMetrics};
use crate::item_facts::{ItemFacts, ItemType, TargetClass};
use crate::model::{GroupContent, Layout};
use crate::shell::ShellItem;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SetTimer, WM_APP};

pub(super) const WM_SORTED: u32 = WM_APP + 0x25;
pub(super) const RESULT_TIMER: usize = 0x4652_0000;

pub(super) struct Sorted {
    facts: Vec<ItemFacts>,
    downloads: Option<String>,
}

pub(super) type Receiver = std::sync::mpsc::Receiver<Result<Sorted, String>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Offer {
    #[default]
    No,
    Pending,
    Showing,
    Sorting,
    /// Nothing to sort this session, or reading the items failed. Corral runs normally, and a
    /// later start can still ask as long as nothing has been saved.
    Deferred,
}

impl Offer {
    pub fn filter_startup(
        &mut self,
        disabled: bool,
        settings_existed: bool,
        fixture: bool,
        opt_in: bool,
    ) {
        if disabled || ((settings_existed || fixture) && !opt_in) {
            *self = Self::No;
        }
    }

    /// The answer is on its way, so Round-up holds off until it arrives.
    pub fn waiting(self) -> bool {
        matches!(self, Self::Pending | Self::Showing | Self::Sorting)
    }

    /// Saving an untouched layout now would use up the question.
    pub fn keeps_question_open(self, layout: &Layout) -> bool {
        self != Self::No && untouched(layout)
    }

    fn begin(&mut self, has_real_items: bool) -> bool {
        if *self != Self::Pending {
            return false;
        }
        *self = if has_real_items { Self::Showing } else { Self::Deferred };
        has_real_items
    }
}

/// Nothing a person arranged would be lost by not saving. Compares every field, so anything added
/// later counts as worth saving. `seen` is only Round-up's memory of the items present.
fn untouched(layout: &Layout) -> bool {
    if !layout.groups.is_empty() || !layout.loose.is_empty() {
        return false;
    }
    let mut plain = layout.clone();
    plain.seen.clear();
    plain == Layout::new()
}

fn spec() -> DialogSpec {
    let mut spec = DialogSpec::new(
        "Sort your desktop?",
        "Corral can put your desktop icons into Corrals named Programs, Files and Folders, and add a Gate that shows your Downloads folder. Your files stay exactly where they are, and you can move or remove any of this later.",
        "Sort my desktop",
    );
    spec.secondary = Some("Not now".into());
    spec
}

/// Called by a posted message, after the surface is visible. No APP borrow crosses the modal loop.
pub(super) fn dispatch() {
    let owner = APP.with(|cell| {
        let mut guard = cell.borrow_mut();
        let app = guard.as_mut()?;
        let has_real_items = app
            .items
            .iter()
            .any(|item| item.gate.is_none() && !item.shell.parsing_name.starts_with("::"));
        app.layout_recovery
            .first_run
            .begin(has_real_items)
            .then_some(app.preview_host.unwrap_or_default())
    });
    let Some(owner) = owner else { return };
    let answer = dialog::show(owner, dialog::system_light(), spec()).action;
    APP.with(|cell| {
        if let Some(app) = cell.borrow_mut().as_mut() {
            // A successful restore during the modal loop cancels the original startup choice.
            if app.layout_recovery.first_run != Offer::Showing {
                return;
            }
            let shells = app
                .items
                .iter()
                .filter(|item| item.gate.is_none())
                .map(|item| item.shell.clone())
                .collect();
            if let Some(names) =
                answer_items(answer, shells, |shells| app.start_first_run_sort(shells))
            {
                app.acknowledge_first_run(&names);
            }
        }
    });
}

/// Only the positive answer can reach the injected sorting/lookup operation.
fn answer_items(
    answer: Action,
    shells: Vec<ShellItem>,
    sort: impl FnOnce(Vec<ShellItem>),
) -> Option<Vec<String>> {
    if answer == Action::Primary {
        sort(shells);
        None
    } else {
        Some(shells.into_iter().map(|item| item.parsing_name).collect())
    }
}

/// A notification carries no pointer: the receiver owns the data, even if the window goes away.
pub(super) fn complete(window: HWND) {
    APP.with(|cell| {
        let Ok(mut guard) = cell.try_borrow_mut() else {
            // A nested menu loop can dispatch this while APP is borrowed. Retry without losing data.
            unsafe {
                let _ = SetTimer(window, RESULT_TIMER, 50, None);
            }
            return;
        };
        unsafe {
            let _ = KillTimer(window, RESULT_TIMER);
        }
        let Some(app) = guard.as_mut() else { return };
        let Some(result) = app
            .first_run_result
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        else {
            return;
        };
        app.first_run_result = None;
        // A restore during the worker read cancels this startup choice.
        if app.layout_recovery.first_run != Offer::Sorting {
            return;
        }
        match result {
            Ok(mut batch) => {
                retain_present(
                    &mut batch.facts,
                    app.items
                        .iter()
                        .filter(|item| item.gate.is_none())
                        .map(|item| item.shell.parsing_name.as_str()),
                );
                app.finish_first_run(&batch.facts, batch.downloads);
            }
            Err(error) => {
                super::log(&format!("first run: {error}"));
                app.layout_recovery.first_run = Offer::Deferred;
            }
        }
    });
}

fn retain_present<'a>(facts: &mut Vec<ItemFacts>, names: impl Iterator<Item = &'a str>) {
    let present: std::collections::HashSet<_> = names.map(str::to_lowercase).collect();
    facts.retain(|fact| present.contains(&fact.name.to_lowercase()));
}

fn read_sort_items(shells: Vec<ShellItem>) -> Result<Sorted, String> {
    // Match item_facts::ensure_com: Shell links use an STA. No COM interfaces leave this thread.
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
        .ok()
        .map_err(|error| format!("could not initialize shortcut reader: {error}"))?;
    let facts = shells
        .iter()
        .map(crate::item_facts::first_run_facts_for)
        .collect();
    let downloads = downloads_folder();
    unsafe {
        CoUninitialize();
    }
    Ok(Sorted { facts, downloads })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Category {
    Programs = 0,
    Files = 1,
    Folders = 2,
}

/// All facts are supplied by the caller. Unknown shortcuts are programs, never guessed documents.
fn classify(item: &ItemFacts) -> Option<Category> {
    if item.name.starts_with("::") {
        return None;
    }
    if item.kind == ItemType::Folder {
        return Some(Category::Folders);
    }
    let ext = item
        .file_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    if item.kind == ItemType::Shortcut || ext == "lnk" {
        return Some(match item.target.as_ref() {
            Some(target) if target.class == TargetClass::Folder => Category::Folders,
            Some(target)
                if target.class == TargetClass::File
                    && !matches!(ext.as_str(), "url" | "website") =>
            {
                // Include installer/command targets beyond item_facts' narrower program list.
                let ext = target
                    .path
                    .rsplit_once('.')
                    .map(|(_, e)| e.to_ascii_lowercase())
                    .unwrap_or_default();
                if matches!(
                    ext.as_str(),
                    "exe" | "bat" | "cmd" | "msi" | "msc" | "cpl" | "com" | "scr" | "pif" | "appref-ms"
                ) {
                    Category::Programs
                } else {
                    Category::Files
                }
            }
            _ => Category::Programs,
        });
    }
    Some(
        if matches!(
            ext.as_str(),
            "exe" | "url" | "appref-ms" | "bat" | "cmd" | "msi"
        ) {
            Category::Programs
        } else {
            Category::Files
        },
    )
}

/// Body rectangles in screen pixels; the title strip is reserved above each one. Use real cells
/// for desired sizes and cap to allotted space: the normal group scrollbar handles excess items.
/// The grid fallback handles short work areas where stacking three open bodies would not fit.
fn placements(
    counts: [usize; 3],
    gate: bool,
    work: Rect,
    cell: CellMetrics,
    strip: i32,
    dpi: u32,
) -> [Option<Rect>; 4] {
    let mut out = [None; 4];
    let indices: Vec<_> = (0..4)
        .filter(|&i| if i == 3 { gate } else { counts[i] > 0 })
        .collect();
    if indices.is_empty() {
        return out;
    }
    let gap = to_px(12, dpi).max(4);
    let min_h = to_px(crate::rollup::MIN_OPEN_BODY_DIP, dpi).max(1);
    let inner = Rect::new(
        work.left + gap,
        work.top + gap,
        work.right - gap,
        work.bottom - gap,
    );
    if inner.width() < 1 || inner.height() < strip + min_h {
        return out;
    }
    let categories = indices.iter().filter(|&&i| i != 3).count();
    let stacked_h = if categories == 0 {
        0
    } else {
        (inner.height() - (categories as i32 - 1) * gap) / categories as i32 - strip
    };
    let normal = stacked_h >= min_h
        && inner.width()
            >= cell.body_size_for(2, 1).0 * if gate && categories > 0 { 2 } else { 1 } + gap;
    let columns = if normal {
        if gate && categories > 0 { 2 } else { 1 }
    } else {
        let rows = ((inner.height() + gap) / (strip + min_h + gap)).max(1) as usize;
        indices.len().div_ceil(rows) as i32
    };
    let column_w = (inner.width() - (columns - 1) * gap) / columns;
    if column_w < 1 {
        return out;
    }
    let rows = if normal {
        categories.max(1) as i32
    } else {
        indices.len().div_ceil(columns as usize) as i32
    };
    let row_h = (inner.height() - (rows - 1) * gap) / rows;
    let four_w = cell.body_size_for(4, 1).0;
    let gate_w = four_w.min(column_w);
    // Normally the Corrals share one width, wide enough to show the fullest without scrolling if
    // the screen allows, and the Gate sits right beside them.
    let corral_w = if normal {
        let cap = if gate && categories > 0 { inner.width() - gate_w - gap } else { inner.width() };
        let rows_fit = cell.cells_for_size(1, row_h - strip).1.max(1) as usize;
        let fullest = counts.into_iter().max().unwrap_or(0);
        let max_cols = cell.cells_for_size(cap, 1).0.max(1);
        let cols = fullest.div_ceil(rows_fit).min(max_cols as usize) as i32;
        cell.body_size_for(cols.max(4).min(max_cols), 1).0.min(cap)
    } else {
        column_w.min(four_w)
    };
    let mut next_y = inner.top;
    for (slot, &index) in indices.iter().enumerate() {
        let (x, y, width, height) = match (normal, index) {
            (true, 3) => {
                let x = if categories > 0 { inner.left + corral_w + gap } else { inner.left };
                (x, inner.top, gate_w, inner.height())
            }
            // Height a shorter Corral above didn't use goes to the ones below.
            (true, _) => {
                let left = (categories - slot) as i32;
                let share = (inner.bottom - next_y - (left - 1) * gap) / left;
                (inner.left, next_y, corral_w, share)
            }
            _ => {
                let (col, row) = (slot as i32 % columns, slot as i32 / columns);
                let x = inner.left + col * (column_w + gap);
                (x, inner.top + row * (row_h + gap), corral_w, row_h)
            }
        };
        let cols = cell.cols_for(width);
        let needed_rows = if index == 3 {
            4
        } else {
            cell.rows_for(counts[index], cols).max(1)
        };
        let wanted = cell.body_size_for(cols, needed_rows).1.max(min_h);
        let body_h = wanted.min(height - strip);
        out[index] = Some(Rect::new(x, y + strip, x + width, y + strip + body_h));
        next_y = y + strip + body_h + gap;
    }
    out
}

/// Pure layout mutation; no Shell or file operations. Empty categories do not create Corrals.
fn sort(
    layout: &mut Layout,
    facts: &[ItemFacts],
    monitor: &Monitor,
    rects: [Option<Rect>; 4],
    downloads: Option<String>,
    page: i32,
    now: u64,
) {
    let categories = [Category::Programs, Category::Files, Category::Folders];
    for (index, (category, title)) in categories
        .into_iter()
        .zip(["Programs", "Files", "Folders"])
        .enumerate()
    {
        let names: Vec<_> = facts
            .iter()
            .filter(|f| classify(f) == Some(category))
            .map(|f| &f.name)
            .collect();
        let Some(body) = rects[index].filter(|_| !names.is_empty()) else {
            continue;
        };
        let id = add_group(
            layout,
            monitor,
            title,
            body,
            GroupContent::Items { items: Vec::new() },
            page,
        );
        for name in names {
            layout.assign_item(name, Some(id));
            layout.mark_manual(name, now);
        }
    }
    if let (Some(body), Some(path)) = (rects[3], downloads) {
        add_group(
            layout,
            monitor,
            "Downloads",
            body,
            GroupContent::Portal { path },
            page,
        );
    }
}

fn add_group(
    layout: &mut Layout,
    monitor: &Monitor,
    title: &str,
    body: Rect,
    content: GroupContent,
    page: i32,
) -> u64 {
    let rect = crate::creation::body_to_dip(body, monitor.work, monitor.dpi);
    let id = layout.add_group(title.into(), monitor.name.clone(), rect, content);
    let group = layout.group_mut(id).unwrap();
    group.preferred_size_dip = Some((rect.width().max(1), rect.height().max(1)));
    group.swipe_index = page;
    id
}

/// Both answers establish the existing desktop as seen. Layout::new already has both initialization
/// flags true; do not reset them and accidentally invoke the old-layout migration pass.
fn acknowledge<'a>(layout: &mut Layout, names: impl IntoIterator<Item = &'a str>) {
    for name in names {
        layout.mark_seen(name);
    }
}

fn downloads_folder() -> Option<String> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, KF_FLAG_DEFAULT, SHGetKnownFolderPath};
    unsafe {
        let known = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
        let path = known.to_string().ok();
        CoTaskMemFree(Some(known.0 as *const _));
        path.filter(|p| std::path::Path::new(p).is_dir())
    }
}

impl App {
    fn start_first_run_sort(&mut self, shells: Vec<ShellItem>) {
        let (sender, receiver) = std::sync::mpsc::channel();
        self.first_run_result = Some(receiver);
        self.layout_recovery.first_run = Offer::Sorting;
        let window = self.message_window.0 as isize;
        let spawned = std::thread::Builder::new()
            .name("first-run-sort".into())
            .spawn(move || {
                // A panic must still report back, or the offer would wait for this thread forever.
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    read_sort_items(shells)
                }))
                .unwrap_or_else(|_| Err("shortcut reader stopped unexpectedly".into()));
                deliver(result, &sender, || {
                    // Like Gate reads, wake the message window. All data remains owned by the channel.
                    let _ = unsafe {
                        PostMessageW(HWND(window as *mut _), WM_SORTED, WPARAM(0), LPARAM(0))
                    };
                });
            });
        if let Err(error) = spawned {
            super::log(&format!(
                "first run: could not start shortcut reader: {error}"
            ));
            self.first_run_result = None;
            self.layout_recovery.first_run = Offer::Deferred;
        }
    }

    fn finish_first_run(&mut self, facts: &[ItemFacts], downloads: Option<String>) {
        let mut cursor = windows::Win32::Foundation::POINT::default();
        let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut cursor) };
        let monitor = self
            .monitors
            .iter()
            .find(|m| m.bounds.contains(cursor.x, cursor.y))
            .or(self.monitors.first())
            .cloned();
        if let Some(monitor) = monitor {
            let cell = icon_metrics::cell_metrics(&self.cell_inputs(
                monitor.dpi,
                icon_metrics::ZOOM_DEFAULT,
                self.settings.group_spacing_matches_desktop,
            ));
            let metrics = crate::group_paint::metrics(monitor.dpi);
            // Reserve the separated-tab gap as well, so every title style stays on screen.
            let strip = if self.settings.label_mode >= 1 {
                metrics.title_h
                    + metrics.gap
                    + crate::group_paint::px(crate::group_paint::SEGMENT_BODY_GAP, monitor.dpi)
            } else {
                0
            };
            let mut counts = [0; 3];
            for item in facts {
                if let Some(category) = classify(item) {
                    counts[category as usize] += 1;
                }
            }
            let rects = placements(
                counts,
                downloads.is_some(),
                monitor.work,
                cell,
                strip,
                monitor.dpi,
            );
            let page = if self.settings.swipe_enabled {
                self.pages.current
            } else {
                0
            };
            self.qh_end_hidden();
            sort(
                &mut self.layout,
                facts,
                &monitor,
                rects,
                downloads,
                page,
                crate::round_up_time::Clock::now().now_ms.max(0) as u64,
            );
            self.rebuild_runtime_bodies();
            self.record_desk_edit();
        }
        let names: Vec<_> = facts.iter().map(|fact| fact.name.clone()).collect();
        self.acknowledge_first_run(&names);
    }

    fn acknowledge_first_run(&mut self, names: &[String]) {
        acknowledge(&mut self.layout, names.iter().map(String::as_str));
        self.layout_recovery.first_run = Offer::No;
        self.save();
        self.arrange_loose();
        self.refresh_all();
    }
}

fn deliver(
    result: Result<Sorted, String>,
    sender: &std::sync::mpsc::Sender<Result<Sorted, String>>,
    notify: impl FnOnce(),
) {
    // Quitting drops the receiver. A late worker then discards its owned data without posting.
    if sender.send(result).is_ok() {
        notify();
    }
}

#[cfg(test)]
mod tests;
