//! Backups page: pure model of the backup list, request construction, title
//! validation, list geometry and the request/result polling decision.
//!
//! The settings window only READS the snapshot store; every change is a [`Request`] written into the
//! settings file for the running app, which answers with a [`RequestResult`] carrying the same nonce.
//! The page view state (the current list, notice and hovered row) lives in a thread-local
//! so the shared page layout / hit-test / draw code can read it without new parameters (the
//! settings window is single threaded).

use std::cell::RefCell;
use std::path::Path;

use crate::render::RectF;
use crate::settings::Settings;
use crate::snapshot::{self, Action, Request, RequestResult};

/// Text of the link that reveals the list.
pub const LINK_TEXT: &str = "Save / Restore a backup";
/// Text of the capture button.
pub const SAVE_TEXT: &str = "Save backup";
/// Section header that hosts the list.
pub const SECTION: &str = "Saved backups";
/// Key of the "never ask again" tip in `Settings::tips` (stored as `Tips\SnapshotBeforeRestore`).
pub const TIP_KEY: &str = "SnapshotBeforeRestore";
/// Value stored for it.
pub const TIP_VALUE: u32 = 7;
/// Independent Corral-only preference; legacy tip=7 alone keeps the historical no-backup choice.
pub const BACKUP_FIRST_KEY: &str = "CorralSnapshotBackupFirst";
/// How long the window waits for the app to TAKE the request; an untaken request is withdrawn then.
pub const TIMEOUT_MS: u64 = 10_000;
/// Longest total wait, when the app took the request but is still busy (a restore rebuilds portals).
pub const MAX_WAIT_MS: u64 = 60_000;
pub const STILL_WORKING_MESSAGE: &str =
    "Corral is still working on the request. Check the backup list in a moment.";
pub const TIMEOUT_MESSAGE: &str = "Corral did not answer within 10 seconds. The backup request was not completed. Make sure Corral is running and try again.";
pub const EMPTY_TEXT: &str = "No backups yet. Save one to keep your current layout.";
pub const NAME_EMPTY: &str = "Enter a name.";
pub const NAME_TOO_LONG: &str = "Use 255 characters or fewer.";

/// One backup as shown in the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    pub id: String,
    pub title: String,
    pub created_ms: u64,
    /// Snapshot kind, independent of the editable title.
    pub automatic: bool,
}

/// Newest first; the id breaks ties deterministically.
pub fn tiles_from(list: Vec<snapshot::Snapshot>) -> Vec<Tile> {
    let mut tiles: Vec<Tile> = list
        .into_iter()
        .map(|s| Tile {
            id: s.id,
            title: s.title,
            created_ms: s.created_ms,
            automatic: matches!(s.kind, snapshot::Kind::Daily { .. } | snapshot::Kind::Automatic),
        })
        .collect();
    tiles.sort_by(|a, b| {
        b.created_ms
            .cmp(&a.created_ms)
            .then_with(|| a.id.cmp(&b.id))
    });
    tiles
}

/// Read the store of `layout_path` (read only). A missing folder or an unreadable store is an empty list.
pub fn load_tiles(layout_path: &Path) -> Vec<Tile> {
    let layout_path =
        std::path::absolute(layout_path).unwrap_or_else(|_| layout_path.to_path_buf());
    let dir = snapshot::store_dir_for(&layout_path);
    if !dir.is_dir() {
        return Vec::new();
    }
    match snapshot::Store::open(&dir).and_then(|store| store.list()) {
        Ok(list) => tiles_from(list),
        Err(_) => Vec::new(),
    }
}

/// `Taken: <date and time>` tooltip.
pub fn tooltip_text(taken: &str) -> String {
    format!("Taken: {taken}")
}

/// `yyyy-MM-dd HH:mm` from local calendar parts.
pub fn format_taken_parts(year: u16, month: u16, day: u16, hour: u16, minute: u16) -> String {
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct RawSystemTime {
    year: u16,
    month: u16,
    day_of_week: u16,
    day: u16,
    hour: u16,
    minute: u16,
    second: u16,
    millis: u16,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn FileTimeToSystemTime(file_time: *const u64, system_time: *mut RawSystemTime) -> i32;
    fn SystemTimeToTzSpecificLocalTime(
        zone: *const u8,
        universal: *const RawSystemTime,
        local: *mut RawSystemTime,
    ) -> i32;
}

/// Unix milliseconds as local date and time text.
pub fn format_taken(created_ms: u64) -> String {
    // FILETIME = 100 ns ticks since 1601-01-01 (little endian u64 = the two DWORDs); the Unix epoch is 11_644_473_600 s later.
    let ticks = (created_ms + 11_644_473_600_000) * 10_000;
    let (mut utc, mut local) = (RawSystemTime::default(), RawSystemTime::default());
    // SAFETY: plain conversions on local structs of the documented layout.
    let ok = unsafe {
        FileTimeToSystemTime(&ticks, &mut utc) != 0
            && SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) != 0
    };
    if !ok {
        return format!("{created_ms}");
    }
    format_taken_parts(local.year, local.month, local.day, local.hour, local.minute)
}

/// Verdict of the rename / naming dialog on the typed text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TitleCheck {
    Valid,
    /// Same raw title as before: accepted, nothing to rename.
    Unchanged,
    Invalid(&'static str),
}

pub fn validate_title(raw: &str, original: &str) -> TitleCheck {
    if raw.trim().is_empty() {
        return TitleCheck::Invalid(NAME_EMPTY);
    }
    if raw.chars().count() > 255 {
        return TitleCheck::Invalid(NAME_TOO_LONG);
    }
    if raw == original {
        TitleCheck::Unchanged
    } else {
        TitleCheck::Valid
    }
}

pub fn new_request(nonce: u64, action: Action) -> Request {
    Request { nonce, action }
}

/// Current Unix time in milliseconds, used as the request nonce.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A nonce that is not equal to the one of the previous request even within the same millisecond.
pub fn next_nonce(previous: Option<u64>, now: u64) -> u64 {
    match previous {
        Some(p) if p >= now => p + 1,
        _ => now,
    }
}

static LAST_NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The next request nonce: a Unix-ms timestamp, strictly greater than every nonce this process
/// issued and than `previous` (the nonces found in the settings file).
pub fn issue_nonce(previous: Option<u64>, now: u64) -> u64 {
    use std::sync::atomic::Ordering;
    let floor = previous.max(Some(LAST_NONCE.load(Ordering::Relaxed)).filter(|&n| n > 0));
    let nonce = next_nonce(floor, now);
    LAST_NONCE.store(nonce, Ordering::Relaxed);
    nonce
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Poll {
    Pending,
    /// The app answered this request.
    Done,
    /// The app never took the request within [`TIMEOUT_MS`]: withdraw it and report "not completed".
    Withdraw,
    /// The app took the request (it is gone from the file) but did not answer within [`MAX_WAIT_MS`].
    StillWorking,
}

/// Whether the wait for the app's answer is over: the result with the same nonce wins over the
/// timeouts. `request_pending` is whether the request with our nonce is still in the settings file.
pub fn poll_decision(
    result: Option<&RequestResult>,
    nonce: u64,
    elapsed_ms: u64,
    request_pending: bool,
) -> Poll {
    if result.is_some_and(|r| r.nonce == nonce) {
        Poll::Done
    } else if request_pending && elapsed_ms >= TIMEOUT_MS {
        Poll::Withdraw
    } else if !request_pending && elapsed_ms >= MAX_WAIT_MS {
        Poll::StillWorking
    } else {
        Poll::Pending
    }
}

/// The daily backup's last error, if any (read only, one line).
pub fn load_notice(layout_path: &Path) -> Option<String> {
    let layout_path =
        std::path::absolute(layout_path).unwrap_or_else(|_| layout_path.to_path_buf());
    let dir = snapshot::store_dir_for(&layout_path);
    if !dir.is_dir() {
        return None;
    }
    let message = snapshot::Store::open(&dir).ok()?.last_error()?;
    Some(one_line(&message))
}

fn one_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether "Restore Backup" shows the take-a-backup-first prompt (not after "never ask again").
pub fn restore_prompt_needed(settings: &Settings) -> bool {
    settings.tips.get(TIP_KEY).copied().unwrap_or(0) == 0
}

/// "Never ask me to make a backup again" together with No persists the tip.
pub fn remember_never_ask(settings: &mut Settings) {
    settings.tips.insert(TIP_KEY.to_string(), TIP_VALUE);
}

/// Remember both accepted checkboxes without changing the snapshot payload or request protocol.
pub fn remember_restore_choice(settings: &mut Settings, backup_first: bool, never_ask: bool) {
    if never_ask {
        remember_never_ask(settings);
        settings
            .tips
            .insert(BACKUP_FIRST_KEY.into(), u32::from(backup_first));
    }
}
pub fn remembered_backup_first(settings: &Settings) -> bool {
    settings.tips.get(BACKUP_FIRST_KEY).copied().unwrap_or(0) != 0
}

// ---------------------------------------------------------------------------------------------
// Page view state

#[derive(Default)]
pub struct BackupView {
    /// The list is revealed (the link is hidden meanwhile).
    pub expanded: bool,
    pub tiles: Vec<Tile>,
    pub hover: Option<usize>,
    /// One-line notice above the list (the daily backup's last error).
    pub notice: Option<String>,
    /// Invalidated only by the list setters; hover and notice do not affect row metrics.
    metrics: Option<ListMetrics>,
}

thread_local! {
    static VIEW: RefCell<BackupView> = RefCell::new(BackupView::default());
}

pub fn with_view<R>(f: impl FnOnce(&mut BackupView) -> R) -> R {
    VIEW.with(|v| f(&mut v.borrow_mut()))
}

pub fn expanded() -> bool {
    with_view(|v| v.expanded)
}

pub fn tile_count() -> usize {
    with_view(|v| v.tiles.len())
}

pub fn set_notice(notice: Option<String>) {
    with_view(|v| v.notice = notice);
}

/// Height of the notice line above the tiles (0 without a notice).
pub fn notice_height(dpi: u32) -> f32 {
    with_view(|v| notice_height_inner(v, dpi))
}

pub fn expand(tiles: Vec<Tile>) {
    with_view(|v| {
        v.expanded = true;
        v.tiles = tiles;
        v.metrics = None;
        v.hover = None;
    });
}

/// The list collapses again on a later page visit.
pub fn collapse() {
    with_view(|v| *v = BackupView::default());
}

pub fn set_tiles(tiles: Vec<Tile>) {
    with_view(|v| {
        v.tiles = tiles;
        v.metrics = None;
        v.hover = None;
    });
}

// ---------------------------------------------------------------------------------------------
// List geometry. All public bounds and pointer coordinates here are device pixels.
// The caller supplies the standard card's inner content bounds (16 DIP inset).
const ROW_H: f32 = 58.0;
const EMPTY_H: f32 = 56.0;
const NOTICE_H: f32 = 40.0;

fn sc(v: f32, dpi: u32) -> f32 {
    (v * dpi as f32 / 96.0).round()
}

/// A list has one row per backup at every supported width.
pub fn columns(_inner_w: f32, _dpi: u32) -> usize {
    1
}

#[derive(Clone, Debug)]
struct RowMetrics {
    /// Unscaled cumulative offset and height; snap the final boundaries once.
    top: f32,
    height: f32,
    title_lines: Vec<String>,
}
#[derive(Clone, Debug)]
struct ListMetrics {
    inner_w: f32,
    dpi: u32,
    rows: Vec<RowMetrics>,
}
#[derive(Clone, Copy, Debug)]
struct RowParts {
    badge: RectF,
    text_left: f32,
    text_right: f32,
    restore: RectF,
    more: RectF,
}

/// One source of horizontal geometry for wrapping, painting, hit-testing and keyboard focus.
fn row_parts(row: RectF, dpi: u32) -> RowParts {
    // Recover the integral DIP row height before snapping its centered control. This keeps
    // the 13-DIP offset for every ordinary 58-DIP row, including 125% rounding.
    let height_dip = ((row.bottom - row.top) * 96.0 / dpi as f32).round();
    let top = row.top + sc((height_dip - 32.0) / 2.0, dpi);
    let more_right = row.right - sc(12.0, dpi);
    let more = RectF::new(
        more_right - sc(32.0, dpi),
        top,
        more_right,
        top + sc(32.0, dpi),
    );
    let right = more.left - sc(12.0, dpi);
    let restore = RectF::new(right - sc(77.0, dpi), top, right, top + sc(32.0, dpi));
    let badge = RectF::new(
        row.left + sc(16.0, dpi),
        top,
        row.left + sc(48.0, dpi),
        top + sc(32.0, dpi),
    );
    RowParts {
        badge,
        text_left: badge.right + sc(12.0, dpi),
        text_right: restore.left - sc(12.0, dpi),
        restore,
        more,
    }
}

/// Keep all entered characters, including whitespace. Prefer word boundaries, but split a long
/// unbroken word at Unicode scalar boundaries so no accepted 255-character title is clipped.
fn title_lines(title: &str, width: f32, dpi: u32) -> Vec<String> {
    let measure = |text: &str| {
        crate::render::measure_text(
            text,
            super::kit::theme::font_family(),
            14.0 * dpi as f32 / 96.0,
            400,
        )
        .map(|m| m.0)
        .unwrap_or(0.0)
    };
    // One device pixel of spare width avoids DirectWrite trimming a line on a fractional advance.
    let width = (width - 1.0).max(1.0);
    let mut lines = Vec::new();
    let mut current = String::new();
    for ch in title.chars() {
        let previous = current.len();
        current.push(ch);
        if previous > 0 && measure(&current) > width {
            let word_break = current[..previous]
                .char_indices()
                .rev()
                .find_map(|(i, c)| c.is_whitespace().then_some(i + c.len_utf8()))
                .filter(|end| *end > 0 && measure(&current[..*end]) <= width);
            let split = word_break.unwrap_or(previous);
            lines.push(current[..split].to_owned());
            current = current[split..].to_owned();
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

fn ensure_metrics(view: &mut BackupView, count: usize, inner_w: f32, dpi: u32) {
    let count = count.max(view.tiles.len());
    if view
        .metrics
        .as_ref()
        .is_some_and(|m| m.inner_w == inner_w && m.dpi == dpi && m.rows.len() >= count)
    {
        return;
    }
    let sample = RectF::new(0.0, 0.0, inner_w + sc(32.0, dpi), sc(ROW_H, dpi));
    let parts = row_parts(sample, dpi);
    let width = (parts.text_right - parts.text_left).max(1.0);
    let mut top = 0.0;
    let rows = (0..count)
        .map(|i| {
            let title = view.tiles.get(i).map_or("", |t| t.title.as_str());
            let title_lines = title_lines(title, width, dpi);
            let height = ROW_H + (title_lines.len().saturating_sub(1) as f32) * 20.0;
            let row = RowMetrics {
                top,
                height,
                title_lines,
            };
            top += height + 1.0;
            row
        })
        .collect();
    view.metrics = Some(ListMetrics { inner_w, dpi, rows });
}
fn row_rect(metrics: &ListMetrics, index: usize, x: f32, y: f32, inner_w: f32, dpi: u32) -> RectF {
    let row = &metrics.rows[index];
    RectF::new(
        x - sc(16.0, dpi),
        y + sc(row.top, dpi),
        x + inner_w + sc(16.0, dpi),
        y + sc(row.top + row.height, dpi),
    )
}
pub fn list_height(count: usize, inner_w: f32, dpi: u32) -> f32 {
    with_view(|view| {
        let notice = notice_height_inner(view, dpi);
        if count == 0 {
            return notice + sc(EMPTY_H, dpi);
        }
        ensure_metrics(view, count, inner_w, dpi);
        let row = &view.metrics.as_ref().unwrap().rows[count - 1];
        notice + sc(row.top + row.height, dpi)
    })
}
pub fn tile_rect(index: usize, x: f32, y: f32, inner_w: f32, dpi: u32) -> RectF {
    with_view(|view| {
        ensure_metrics(view, index + 1, inner_w, dpi);
        row_rect(view.metrics.as_ref().unwrap(), index, x, y, inner_w, dpi)
    })
}

pub fn tile_at(
    point: (f32, f32),
    count: usize,
    x: f32,
    y: f32,
    inner_w: f32,
    dpi: u32,
) -> Option<usize> {
    (0..count).find(|&i| contains(tile_rect(i, x, y, inner_w, dpi), point))
}
fn contains(r: RectF, p: (f32, f32)) -> bool {
    p.0 >= r.left && p.0 < r.right && p.1 >= r.top && p.1 < r.bottom
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListAction {
    Restore(usize),
    More(usize),
}
impl ListAction {
    pub fn index(self) -> usize {
        match self {
            Self::Restore(i) | Self::More(i) => i,
        }
    }
    pub fn part(self) -> usize {
        usize::from(matches!(self, Self::More(_)))
    }
}

/// part 0 = Restore; part 1 = More. No list-body click triggers a restore.
pub fn action_rect(index: usize, part: usize, x: f32, y: f32, inner_w: f32, dpi: u32) -> RectF {
    let parts = row_parts(tile_rect(index, x, y, inner_w, dpi), dpi);
    if part == 1 { parts.more } else { parts.restore }
}
pub fn list_action_at(
    point: (f32, f32),
    count: usize,
    x: f32,
    y: f32,
    inner_w: f32,
    dpi: u32,
) -> Option<ListAction> {
    with_view(|view| {
        ensure_metrics(view, count, inner_w, dpi);
        let model = view.metrics.as_ref().unwrap();
        for index in 0..count {
            let row = row_rect(model, index, x, y, inner_w, dpi);
            if !contains(row, point) {
                continue;
            }
            let parts = row_parts(row, dpi);
            if contains(parts.restore, point) {
                return Some(ListAction::Restore(index));
            }
            if contains(parts.more, point) {
                return Some(ListAction::More(index));
            }
            return None;
        }
        None
    })
}

pub fn focus_actions(count: usize) -> Vec<ListAction> {
    (0..count)
        .flat_map(|i| [ListAction::Restore(i), ListAction::More(i)])
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuAction {
    Rename,
    Update,
    Delete,
}
pub const MENU_LABELS: [&str; 3] = ["Rename", "Update to current layout", "Delete"];
pub fn menu_action(index: usize) -> Option<MenuAction> {
    [MenuAction::Rename, MenuAction::Update, MenuAction::Delete]
        .get(index)
        .copied()
}
impl MenuAction {
    /// Rename requires a separately validated title. No store is opened or mutated here.
    pub fn request(self, id: &str, title: Option<String>) -> Option<Action> {
        match self {
            Self::Rename => title
                .filter(|v| matches!(validate_title(v, ""), TitleCheck::Valid))
                .map(|title| Action::Rename {
                    id: id.into(),
                    title,
                }),
            Self::Update => Some(Action::Recapture { id: id.into() }),
            Self::Delete => Some(Action::Delete { id: id.into() }),
        }
    }
}
pub fn subtitle(tile: &Tile) -> String {
    format!(
        "{} · {}",
        format_taken(tile.created_ms),
        if tile.automatic {
            "Automatic"
        } else {
            "Saved by you"
        }
    )
}

/// Draw the real list through the shared kit. Hover/focus identifies an actual button, never a tile.
pub fn draw_list(
    ctx: &crate::render::Ctx,
    x: f32,
    y: f32,
    inner_w: f32,
    dpi: u32,
    light: bool,
    enabled: bool,
    hover: Option<ListAction>,
    focused: Option<ListAction>,
) -> windows::core::Result<()> {
    use super::kit::{
        controls::{self, ButtonStyle, Control, VisualState},
        theme::{Theme, Type},
    };
    use crate::render::{Align, TextStyle};
    let theme = Theme::new(light);
    let dip = |r: RectF| {
        let s = dpi as f32 / 96.0;
        RectF::new(r.left / s, r.top / s, r.right / s, r.bottom / s)
    };
    with_view(|view| {
        let count = view.tiles.len();
        ensure_metrics(view, count, inner_w, dpi);
        let model = view.metrics.as_ref().unwrap();
        let top = y + notice_height_inner(view, dpi);
        if let Some(notice) = &view.notice {
            ctx.draw_text_cached(
                notice,
                RectF::new(x, y, x + inner_w, top),
                &Type::Caption.style(dpi, theme.color(theme.secondary, enabled)),
            )?;
        }
        if view.tiles.is_empty() {
            return ctx.draw_text_cached(
                EMPTY_TEXT,
                RectF::new(x, top + sc(10.0, dpi), x + inner_w, top + sc(46.0, dpi)),
                &Type::Caption.style(dpi, theme.color(theme.secondary, enabled)),
            );
        }
        for (i, tile) in view.tiles.iter().enumerate() {
            let row = row_rect(model, i, x, top, inner_w, dpi);
            let parts = row_parts(row, dpi);
            if i > 0 {
                ctx.fill_rect(
                    RectF::new(row.left, row.top - sc(1.0, dpi), row.right, row.top),
                    theme.border_subtle,
                )?;
            }
            let badge = parts.badge;
            ctx.fill_rounded_rect(
                badge,
                sc(8.0, dpi),
                theme.color(theme.accent_subtle, enabled),
            )?;
            ctx.draw_text_cached(
                "\u{E81C}",
                badge,
                &TextStyle {
                    family: super::icon_family(),
                    size_px: sc(16.0, dpi),
                    weight: 400,
                    color: theme.color(theme.text, enabled),
                    align: Align::Center,
                    shadow: None,
                },
            )?;
            let metric = &model.rows[i];
            for (line_index, line) in metric.title_lines.iter().enumerate() {
                let line_y = row.top + sc(10.0 + line_index as f32 * 20.0, dpi);
                ctx.draw_text_cached(
                    line,
                    RectF::new(
                        parts.text_left,
                        line_y,
                        parts.text_right,
                        line_y + sc(20.0, dpi),
                    ),
                    &Type::Body.style(dpi, theme.color(theme.text, enabled)),
                )?;
            }
            let caption_y = row.top + sc(12.0 + metric.title_lines.len() as f32 * 20.0, dpi);
            ctx.draw_text_cached(
                &subtitle(tile),
                RectF::new(
                    parts.text_left,
                    caption_y,
                    parts.text_right,
                    caption_y + sc(16.0, dpi),
                ),
                &Type::Caption.style(dpi, theme.color(theme.secondary, enabled)),
            )?;
            for action in [ListAction::Restore(i), ListAction::More(i)] {
                let state = VisualState {
                    enabled,
                    hovered: hover == Some(action),
                    focused: focused == Some(action),
                    ..VisualState::default()
                };
                let kind = if action.part() == 0 {
                    Control::Button(ButtonStyle::Secondary)
                } else {
                    Control::IconButton
                };
                let rect = if action.part() == 0 {
                    parts.restore
                } else {
                    parts.more
                };
                controls::draw_control(
                    ctx,
                    kind,
                    dip(rect),
                    if action.part() == 0 { "Restore" } else { "" },
                    state,
                    &theme,
                    dpi,
                    0.0,
                )?;
                if action.part() == 1 {
                    ctx.draw_text_cached(
                        "\u{E712}",
                        rect,
                        &TextStyle {
                            family: super::icon_family(),
                            size_px: sc(16.0, dpi),
                            weight: 400,
                            color: theme.color(theme.text, enabled),
                            align: Align::Center,
                            shadow: None,
                        },
                    )?;
                }
            }
        }
        Ok(())
    })
}
fn notice_height_inner(view: &BackupView, dpi: u32) -> f32 {
    if view.notice.is_some() {
        sc(NOTICE_H, dpi)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    fn result(nonce: u64) -> RequestResult {
        RequestResult {
            nonce,
            ok: true,
            message: "Backup captured".into(),
            id: None,
        }
    }

    #[test]
    fn request_is_built_with_nonce_and_action() {
        let r = new_request(42, Action::Capture { title: None });
        assert_eq!(r.nonce, 42);
        assert_eq!(r.action, Action::Capture { title: None });
        let r = new_request(
            7,
            Action::Restore {
                id: "ab".into(),
                backup_first: true,
            },
        );
        assert_eq!(
            r.action,
            Action::Restore {
                id: "ab".into(),
                backup_first: true
            }
        );
    }

    #[test]
    fn nonce_never_repeats_inside_a_millisecond() {
        assert_eq!(next_nonce(None, 100), 100);
        assert_eq!(next_nonce(Some(100), 100), 101);
        assert_eq!(next_nonce(Some(100), 99), 101);
        assert_eq!(next_nonce(Some(90), 100), 100);
    }

    #[test]
    fn title_validation_uses_the_expected_texts() {
        assert_eq!(validate_title("  ", "Old"), TitleCheck::Invalid(NAME_EMPTY));
        assert_eq!(validate_title("", ""), TitleCheck::Invalid(NAME_EMPTY));
        assert_eq!(
            validate_title(&"a".repeat(256), "Old"),
            TitleCheck::Invalid(NAME_TOO_LONG)
        );
        assert_eq!(validate_title(&"a".repeat(254), "Old"), TitleCheck::Valid);
        assert_eq!(validate_title("Old", "Old"), TitleCheck::Unchanged);
        // Raw text is kept verbatim: surrounding blanks make it a different title.
        assert_eq!(validate_title(" Old", "Old"), TitleCheck::Valid);
        // Count Unicode characters, not UTF-16 surrogate units.
        assert_eq!(
            validate_title(&"\u{1F600}".repeat(255), "x"),
            TitleCheck::Valid
        );
        assert_eq!(
            validate_title(&"\u{1F600}".repeat(256), "x"),
            TitleCheck::Invalid(NAME_TOO_LONG)
        );
    }

    #[test]
    fn tiles_sort_newest_first_by_timestamp() {
        let mk = |id: &str, ms: u64, kind: snapshot::Kind| snapshot::Snapshot {
            version: snapshot::VERSION,
            id: id.into(),
            title: id.into(),
            created_ms: ms,
            kind,
            layout: crate::model::Layout::new(),
            settings: Settings::default(),
        };
        let tiles = tiles_from(vec![
            mk("c", 30, snapshot::Kind::Manual),
            mk("a", 10, snapshot::Kind::Manual),
            mk("b2", 20, snapshot::Kind::Manual),
            mk("b1", 20, snapshot::Kind::Daily { slot: 1 }),
        ]);
        assert!(tiles[1].automatic);
        assert!(!tiles[0].automatic);
        let order: Vec<&str> = tiles.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(order, ["c", "b1", "b2", "a"]);
    }

    #[test]
    fn tooltip_and_date_text() {
        assert_eq!(tooltip_text("2026-10-13 14:05"), "Taken: 2026-10-13 14:05");
        assert_eq!(format_taken_parts(2026, 3, 4, 5, 6), "2026-03-04 05:06");
        assert!(!format_taken(1_760_000_000_000).is_empty());
    }

    #[test]
    fn polling_decision() {
        assert_eq!(poll_decision(None, 5, 0, true), Poll::Pending);
        assert_eq!(poll_decision(None, 5, 9_999, true), Poll::Pending);
        // Never taken: withdrawn after 10 s.
        assert_eq!(poll_decision(None, 5, 10_000, true), Poll::Withdraw);
        assert_eq!(poll_decision(Some(&result(5)), 5, 100, true), Poll::Done);
        // A stale answer of an older request is not ours.
        assert_eq!(poll_decision(Some(&result(4)), 5, 100, true), Poll::Pending);
        assert_eq!(
            poll_decision(Some(&result(4)), 5, 10_001, true),
            Poll::Withdraw
        );
        // The answer wins over the timeouts when both are due.
        assert_eq!(
            poll_decision(Some(&result(5)), 5, 70_000, false),
            Poll::Done
        );
        assert_eq!(poll_decision(Some(&result(5)), 5, 20_000, true), Poll::Done);
    }

    #[test]
    fn a_taken_request_keeps_waiting_up_to_the_long_cap() {
        // The app took the request (gone from the file): not withdrawn, not "not completed".
        assert_eq!(poll_decision(None, 5, 10_000, false), Poll::Pending);
        assert_eq!(poll_decision(None, 5, 59_999, false), Poll::Pending);
        assert_eq!(
            poll_decision(None, 5, MAX_WAIT_MS, false),
            Poll::StillWorking
        );
        assert!(STILL_WORKING_MESSAGE.starts_with("Corral is still working"));
        assert!(MAX_WAIT_MS + 10_000 <= snapshot::REQUEST_MAX_AGE_MS);
    }

    #[test]
    fn issued_nonces_strictly_increase() {
        let a = issue_nonce(None, 1_000_000);
        let b = issue_nonce(None, 1_000_000);
        let c = issue_nonce(Some(b + 10), 5);
        let d = issue_nonce(None, 0);
        assert!(a < b && b < c && c < d);
    }

    #[test]
    fn notice_is_one_line_and_adds_height() {
        assert_eq!(
            one_line("Daily backup failed:\r\n  disk  full"),
            "Daily backup failed: disk full"
        );
        collapse();
        let plain = list_height(2, 400.0, 96);
        set_notice(Some("x".into()));
        assert!(list_height(2, 400.0, 96) > plain);
        collapse();
        assert_eq!(list_height(2, 400.0, 96), plain);
    }

    #[test]
    fn never_ask_tip_suppresses_the_restore_prompt() {
        let mut s = Settings::default();
        assert!(restore_prompt_needed(&s));
        remember_never_ask(&mut s);
        assert_eq!(s.tips.get("SnapshotBeforeRestore"), Some(&7));
        assert!(!restore_prompt_needed(&s));
    }

    #[test]
    fn list_rows_and_action_hits_match_at_all_dpis() {
        collapse();
        for dpi in [96u32, 120, 144, 240] {
            let width = sc(398.0, dpi);
            assert_eq!(columns(width, dpi), 1);
            for count in 1..50 {
                assert_eq!(
                    tile_rect(count - 1, sc(17.0, dpi), 0.0, width, dpi).bottom,
                    list_height(count, width, dpi)
                );
            }
            assert_eq!(
                action_rect(0, 0, sc(17.0, dpi), 0.0, width, dpi).top,
                sc(13.0, dpi)
            );
            let height = list_height(4, width, dpi);
            let last = tile_rect(3, sc(17.0, dpi), 0.0, width, dpi);
            assert_eq!(last.bottom, height);
            for action in focus_actions(4) {
                let r = action_rect(
                    action.index(),
                    action.part(),
                    sc(17.0, dpi),
                    0.0,
                    width,
                    dpi,
                );
                assert!(r.left >= last.left && r.right <= last.right);
                assert_eq!(
                    list_action_at(
                        ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0),
                        4,
                        sc(17.0, dpi),
                        0.0,
                        width,
                        dpi
                    ),
                    Some(action)
                );
            }
            // A click on the name is inert.
            assert_eq!(
                list_action_at(
                    (sc(70.0, dpi), sc(20.0, dpi)),
                    4,
                    sc(17.0, dpi),
                    0.0,
                    width,
                    dpi
                ),
                None
            );
        }
    }

    #[test]
    fn menu_maps_to_existing_request_variants() {
        assert_eq!(menu_action(0), Some(MenuAction::Rename));
        assert_eq!(menu_action(1), Some(MenuAction::Update));
        assert_eq!(menu_action(2), Some(MenuAction::Delete));
        assert_eq!(menu_action(3), None);
        assert_eq!(
            MenuAction::Update.request("id", None),
            Some(Action::Recapture { id: "id".into() })
        );
        assert_eq!(
            MenuAction::Delete.request("id", None),
            Some(Action::Delete { id: "id".into() })
        );
        assert!(MenuAction::Rename.request("id", Some(" ".into())).is_none());
        assert_eq!(
            MenuAction::Rename.request("id", Some("New".into())),
            Some(Action::Rename {
                id: "id".into(),
                title: "New".into()
            })
        );
        let mut tile = Tile {
            id: "x".into(),
            title: "Daily backup".into(),
            created_ms: 1,
            automatic: false,
        };
        assert!(subtitle(&tile).ends_with(" · Saved by you"));
        tile.title = "Renamed daily".into();
        tile.automatic = true;
        assert!(subtitle(&tile).ends_with(" · Automatic"));
    }

    #[test]
    fn list_goldens() -> windows::core::Result<()> {
        use super::super::kit::theme::Theme;
        use crate::render::Surface;
        let output = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/qa/settings-redesign/u4");
        std::fs::create_dir_all(&output).unwrap();
        for light in [true, false] {
            for dpi in [96u32, 120, 144, 240] {
                for width in [432.0, 678.0] {
                    for empty in [false, true] {
                        expand(if empty {
                            vec![]
                        } else {
                            (0..4)
                                .map(|i| Tile {
                                    id: i.to_string(),
                                    title: if i == 0 {
                                        "Before reorganizing".into()
                                    } else {
                                        "Daily backup".into()
                                    },
                                    created_ms: 1_760_000_000_000 - i * 86_400_000,
                                    automatic: i > 0,
                                })
                                .collect()
                        });
                        let height =
                            list_height(tile_count(), sc(width - 34.0, dpi), dpi) + sc(2.0, dpi);
                        let mut surface = Surface::new(sc(width, dpi) as u32, height as u32)?;
                        surface.draw(|ctx| {
                            let theme = Theme::new(light);
                            ctx.clear(theme.window);
                            ctx.fill_rounded_rect(
                                RectF::new(0.0, 0.0, sc(width, dpi), height),
                                sc(8.0, dpi),
                                theme.surface,
                            )?;
                            draw_list(
                                ctx,
                                sc(17.0, dpi),
                                sc(1.0, dpi),
                                sc(width - 34.0, dpi),
                                dpi,
                                light,
                                true,
                                Some(ListAction::More(0)),
                                None,
                            )?;
                            ctx.stroke_rounded_rect(
                                RectF::new(0.5, 0.5, sc(width, dpi) - 0.5, height - 0.5),
                                sc(8.0, dpi),
                                sc(1.0, dpi),
                                theme.border,
                            )
                        })?;
                        surface.save_png(&output.join(format!(
                            "backups-list-{}-{}-{}-{}.png",
                            if light { "light" } else { "dark" },
                            dpi,
                            width as u32,
                            if empty { "empty" } else { "saved" }
                        )))?;
                    }
                }
            }
        }
        collapse();
        Ok(())
    }

    fn long_title_fixture() -> Vec<Tile> {
        let unicode: String = "Zażółć gęślą jaźń 😀 ".chars().cycle().take(255).collect();
        ["Before reorganizing".to_string(), unicode, "W".repeat(255)]
            .into_iter()
            .enumerate()
            .map(|(i, title)| Tile {
                id: i.to_string(),
                title,
                created_ms: 1_760_000_000_000 - i as u64,
                automatic: i == 1,
            })
            .collect()
    }

    #[test]
    fn long_names_preserve_text_and_share_variable_geometry() -> windows::core::Result<()> {
        use super::super::kit::theme::font_family;
        for dpi in [96u32, 120, 144, 240] {
            expand(long_title_fixture());
            let inner_w = sc(398.0, dpi);
            let x = sc(17.0, dpi);
            with_view(|view| {
                ensure_metrics(view, 3, inner_w, dpi);
                let model = view.metrics.as_ref().unwrap();
                assert_eq!(
                    model.rows[0].height, 58.0,
                    "Ordinary Figma row must stay unchanged"
                );
                for index in [1, 2] {
                    let tile = &view.tiles[index];
                    let row = &model.rows[index];
                    assert_eq!(tile.title.chars().count(), 255);
                    assert_eq!(
                        row.title_lines.concat(),
                        tile.title,
                        "Wrapping must retain every character"
                    );
                    assert!(row.title_lines.len() > 1);
                    assert_eq!(row.height, 38.0 + 20.0 * row.title_lines.len() as f32);
                    let parts = row_parts(row_rect(model, index, x, 0.0, inner_w, dpi), dpi);
                    let width = parts.text_right - parts.text_left;
                    for line in &row.title_lines {
                        let measured = crate::render::measure_text(
                            line,
                            font_family(),
                            14.0 * dpi as f32 / 96.0,
                            400,
                        )?;
                        assert!(
                            measured.0 <= width,
                            "A painted line must fit without ellipsis"
                        );
                    }
                }
                Ok::<_, windows::core::Error>(())
            })?;
            let height = list_height(3, inner_w, dpi);
            assert_eq!(tile_rect(2, x, 0.0, inner_w, dpi).bottom, height);
            for index in 0..3 {
                let row = tile_rect(index, x, 0.0, inner_w, dpi);
                if index > 0 {
                    let before = tile_rect(index - 1, x, 0.0, inner_w, dpi);
                    let gap = row.top - before.bottom;
                    assert!(gap >= 1.0 && gap <= (dpi as f32 / 96.0).ceil());
                }
                for part in [0, 1] {
                    let r = action_rect(index, part, x, 0.0, inner_w, dpi);
                    assert!(
                        r.left >= row.left
                            && r.right <= row.right
                            && r.top >= row.top
                            && r.bottom <= row.bottom
                    );
                    let expected = if part == 0 {
                        ListAction::Restore(index)
                    } else {
                        ListAction::More(index)
                    };
                    assert_eq!(
                        list_action_at(
                            ((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0),
                            3,
                            x,
                            0.0,
                            inner_w,
                            dpi
                        ),
                        Some(expected)
                    );
                }
            }
            // A new list invalidates retained line breaks and height after a rename/refresh.
            set_tiles(vec![Tile {
                id: "a".into(),
                title: "Short".into(),
                created_ms: 1,
                automatic: false,
            }]);
            assert_eq!(list_height(1, inner_w, dpi), sc(58.0, dpi));
        }
        collapse();
        Ok(())
    }

    #[test]
    fn long_backup_name_goldens() -> windows::core::Result<()> {
        use super::super::kit::theme::Theme;
        use crate::render::Surface;
        let output = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/qa/settings-redesign/u4");
        std::fs::create_dir_all(&output).unwrap();
        for light in [true, false] {
            for dpi in [96u32, 120, 144, 240] {
                expand(long_title_fixture());
                let width = sc(432.0, dpi);
                let inner_w = sc(398.0, dpi);
                let height = list_height(3, inner_w, dpi) + sc(2.0, dpi);
                let mut surface = Surface::new(width as u32, height as u32)?;
                surface.draw(|ctx| {
                    let theme = Theme::new(light);
                    ctx.clear(theme.window);
                    ctx.fill_rounded_rect(
                        RectF::new(0.0, 0.0, width, height),
                        sc(8.0, dpi),
                        theme.surface,
                    )?;
                    draw_list(
                        ctx,
                        sc(17.0, dpi),
                        sc(1.0, dpi),
                        inner_w,
                        dpi,
                        light,
                        true,
                        None,
                        None,
                    )?;
                    ctx.stroke_rounded_rect(
                        RectF::new(0.5, 0.5, width - 0.5, height - 0.5),
                        sc(8.0, dpi),
                        sc(1.0, dpi),
                        theme.border,
                    )
                })?;
                surface.save_png(&output.join(format!(
                    "backups-long-names-{}-{dpi}-432.png",
                    if light { "light" } else { "dark" }
                )))?;
            }
        }
        collapse();
        Ok(())
    }

    #[test]
    fn expand_and_collapse() {
        collapse();
        assert!(!expanded());
        expand(vec![Tile {
            id: "a".into(),
            title: "A".into(),
            created_ms: 1,
            automatic: false,
        }]);
        assert!(expanded());
        assert_eq!(tile_count(), 1);
        collapse();
        assert!(!expanded());
        assert_eq!(tile_count(), 0);
    }

    #[test]
    fn remembered_restore_keeps_both_checkbox_choices_and_legacy_default() {
        let mut settings = Settings::default();
        remember_never_ask(&mut settings);
        assert!(!remembered_backup_first(&settings));
        remember_restore_choice(&mut settings, true, true);
        assert!(!restore_prompt_needed(&settings));
        assert!(remembered_backup_first(&settings));
        remember_restore_choice(&mut settings, false, true);
        assert!(!remembered_backup_first(&settings));
        let mut fresh = Settings::default();
        remember_restore_choice(&mut fresh, true, false);
        assert!(restore_prompt_needed(&fresh));
        assert!(!fresh.tips.contains_key(BACKUP_FIRST_KEY));
    }

    #[test]
    fn missing_store_folder_gives_an_empty_list() {
        let dir = std::env::temp_dir()
            .join("corral-no-such-layout-folder-xyz")
            .join("layout.json");
        assert!(load_tiles(&dir).is_empty());
    }
}
