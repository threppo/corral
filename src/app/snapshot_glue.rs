//! App glue for the backups engine: the hourly daily
//! backup, the settings window's backup requests, and restore with a recovery journal.
//!
//! The pure parts (day ordinal, cursor gate, request execution, restore-to-disk, journal recovery
//! decision) have no window state and are unit-tested. Everything here writes only Corral's own
//! files: the store folder (`<layout folder>\backups`), the layout file and the settings file.
//! No path in this module moves, deletes or creates Desktop or portal files.

use super::{App, Drag, log};
use crate::model::{self, Layout};
use crate::settings::{self as settings_store, Settings};
use crate::snapshot::{self, Action, Request, RequestResult, Snapshot, Store};
use std::io;
use std::path::Path;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, KillTimer, SetTimer};

/// Hourly timer of the daily backup (3,600,000 ms).
pub(super) const TIMER_SNAPSHOT: usize = 0x6101;
const HOUR_MS: u32 = 3_600_000;

// ---- English texts; the ones marked [Corral] are Corral's own wording ---------------
const MSG_CAPTURED: &str = "Backup captured";
const MSG_DELETED: &str = "Backup deleted";
const MSG_RECAPTURED: &str = "Backup updated"; // [Corral]
const MSG_RENAMED: &str = "Backup renamed"; // [Corral]
const MSG_RESTORED: &str = "Backup restored"; // [Corral]
const MSG_EMPTY_TITLE: &str = "You must give the snapshot a name";
const MSG_LONG_TITLE: &str = "Use 255 characters or fewer.";
const MSG_NO_STORE: &str = "Saving of backup failed.\n\nUnable to access application data directory.";
const MSG_DRAG_ACTIVE: &str = "Problem restoring snapshot.\n\nAn icon drag is in progress.";
const MSG_OLD: &str = "The backup request was out of date and was not run."; // [Corral]
const MSG_INCOMPLETE: &str = "The restore will be completed the next time Corral starts."; // [Corral]
const MSG_STALE: &str = "The backup request was not run because Corral was not running."; // [Corral]

// ---- pure parts ------------------------------------------------------------------------------

/// Days since 0001-01-01 of a Gregorian date (`DateTime.Now - DateTime.MinValue` in whole days,
/// section 3). Proleptic Gregorian, civil-from-days algorithm.
pub(super) fn day_ordinal(year: i32, month: u32, day: u32) -> u32 {
    let y = i64::from(year) - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let m = i64::from(month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_since_1970 = era * 146_097 + doe - 719_468;
    (days_since_1970 + 719_162) as u32 // 719162 = days 0001-01-01 .. 1970-01-01
}

/// The sampled-cursor gate (section 3 step 1): a tick passes only when the cursor is somewhere
/// else than at the previous tick. Movement that returns to the same point is missed.
#[derive(Default)]
pub(super) struct CursorGate {
    last: Option<(i32, i32)>,
}
impl CursorGate {
    pub(super) fn moved(&mut self, position: (i32, i32)) -> bool {
        let moved = self.last != Some(position);
        self.last = Some(position);
        moved
    }
}

pub(super) fn daily_title(short_date: &str) -> String {
    format!("Daily Backup ({short_date})")
}

/// What `execute` needs from the running app.
pub(super) struct Env<'a> {
    pub layout: &'a Layout,
    /// Settings as they go into a capture (`snapshot::for_capture` already applied).
    pub settings: &'a Settings,
    pub now_ms: u64,
    /// Automatic title: "Save backup" without a name and the pre-restore capture.
    pub auto_title: &'a str,
}

pub(super) struct Executed {
    pub result: RequestResult,
    /// Set for an accepted restore: validated, pre-restore capture done, journal written. The
    /// caller replaces the layout/settings pair and clears the journal afterwards.
    pub restore: Option<Snapshot>,
}

fn title_error(title: &str) -> Option<&'static str> {
    if title.trim().is_empty() {
        Some(MSG_EMPTY_TITLE)
    } else if title.chars().count() > 255 {
        Some(MSG_LONG_TITLE)
    } else {
        None
    }
}

fn outcome(nonce: u64, ok: bool, message: impl Into<String>, id: Option<String>) -> Executed {
    Executed { result: RequestResult { nonce, ok, message: message.into(), id }, restore: None }
}

/// Execute one request against the store. Never touches the live layout/settings files.
pub(super) fn execute(store: &Store, request: &Request, env: &Env) -> Executed {
    let nonce = request.nonce;
    let fail = |prefix: &str, e: io::Error| outcome(nonce, false, format!("{prefix}\n\n{e}"), None);
    match &request.action {
        Action::Capture { title } => {
            let title = title.as_deref().unwrap_or(env.auto_title);
            if let Some(message) = title_error(title) {
                return outcome(nonce, false, message, None);
            }
            match store.capture(title, env.now_ms, env.layout, env.settings) {
                Ok(s) => outcome(nonce, true, MSG_CAPTURED, Some(s.id)),
                Err(e) => fail("Saving of backup failed.", e),
            }
        }
        Action::Recapture { id } => match store.recapture(id, env.now_ms, env.layout, env.settings) {
            Ok(s) => outcome(nonce, true, MSG_RECAPTURED, Some(s.id)),
            Err(e) => fail("Saving of backup failed.", e),
        },
        Action::Rename { id, title } => {
            if let Some(message) = title_error(title) {
                return outcome(nonce, false, message, None);
            }
            match store.rename(id, title) {
                Ok(s) => outcome(nonce, true, MSG_RENAMED, Some(s.id)),
                Err(e) => fail("Problem saving renaming.", e),
            }
        }
        Action::Delete { id } => match store.delete(id) {
            Ok(()) => outcome(nonce, true, MSG_DELETED, Some(id.clone())),
            Err(e) => fail("Problem deleting backup.", e),
        },
        Action::Restore { id, backup_first } => {
            // 1. validate the target before anything else
            let target = match store.load(id) {
                Ok(s) => s,
                Err(e) => return fail("Problem restoring snapshot.", e),
            };
            // 2. the requested safety backup; a failure stops the restore (deliberate
            //    improvement)
            if *backup_first {
                if let Err(e) = store.capture(env.auto_title, env.now_ms, env.layout, env.settings) {
                    return fail("Saving of backup failed.\n\nThe backup before restoring could not be taken; nothing was restored.", e);
                }
            }
            // 3. journal before anything is replaced (G5)
            if let Err(e) = store.write_journal(&target.id) {
                return fail("Problem restoring snapshot.", e);
            }
            let mut done = outcome(nonce, true, MSG_RESTORED, Some(target.id.clone()));
            done.restore = Some(target);
            done
        }
    }
}

/// What to do with a request found in the settings file (F3). Nonces are Unix-ms timestamps.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Gate {
    Run,
    /// Older (or from the future) than [`snapshot::REQUEST_MAX_AGE_MS`]: answer with an error.
    TooOld,
    /// Not newer than the last request already handled: ignore it silently.
    Replay,
}

pub(super) fn request_gate(nonce: u64, now_ms: u64, last_handled: Option<u64>) -> Gate {
    if last_handled.is_some_and(|last| nonce <= last) {
        Gate::Replay
    } else if now_ms.saturating_sub(nonce) > snapshot::REQUEST_MAX_AGE_MS || nonce.saturating_sub(now_ms) > snapshot::REQUEST_MAX_AGE_MS {
        Gate::TooOld
    } else {
        Gate::Run
    }
}

/// F5: a preview instance that still uses the default (real) settings file must not run requests.
pub(super) fn shares_real_settings(preview: bool, settings_path: &Path, default_path: &Path) -> bool {
    let norm = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()).to_string_lossy().to_lowercase();
    preview && norm(settings_path) == norm(default_path)
}

/// State of the restore journal as far as this session knows.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum JournalState {
    #[default]
    Clear,
    /// A restore finished in memory and on disk but deleting the journal failed: retry at the next layout save.
    ClearPending,
    /// A restore failed and could not be rolled back: the journal stays, layout autosave is off.
    Blocked,
}

/// Called before every layout save: returns whether the layout may be written and the new state.
pub(super) fn before_layout_save(state: JournalState, store: Option<&Store>) -> (bool, JournalState) {
    match state {
        JournalState::Clear => (true, state),
        JournalState::Blocked => (false, state),
        JournalState::ClearPending => match store.map(Store::clear_journal) {
            Some(Err(_)) => (true, JournalState::ClearPending),
            _ => (true, JournalState::Clear),
        },
    }
}

/// Journal state after a successful restore tried to delete its journal. A success also lifts an
/// earlier `Blocked` (review finding B: autosave stayed off for the rest of the session).
pub(super) fn after_restore_clear(cleared: bool) -> JournalState {
    if cleared { JournalState::Clear } else { JournalState::ClearPending }
}

pub(super) enum Finish {
    /// The backup is on disk; the caller installs it and clears the journal.
    Done,
    /// The write failed, the old pair was rewritten from memory and the journal cleared.
    RolledBack(String),
    /// The write failed and so did the rollback (or clearing the journal): the journal stays.
    Incomplete(String),
}

/// F1: write the restored pair; on failure put the in-memory state back on disk and drop the journal,
/// so neither autosave nor the next start-up recovery applies a backup the user was told failed.
pub(super) fn finish_restore(
    store: &Store,
    target: &Snapshot,
    memory: (&Layout, &Settings),
    paths: (&Path, &Path),
    write: impl FnOnce(&Snapshot, &Path, &Path) -> io::Result<()>,
) -> Finish {
    let Err(error) = write(target, paths.0, paths.1) else { return Finish::Done };
    let rolled = model::save(paths.0, memory.0)
        .and_then(|_| settings_store::save(paths.1, &snapshot::for_capture(memory.1)))
        .and_then(|_| store.clear_journal());
    match rolled {
        Ok(()) => Finish::RolledBack(error.to_string()),
        Err(second) => Finish::Incomplete(format!("{error} (rolling back: {second})")),
    }
}

/// Replace Corral's layout and settings files with the snapshot's (settings without the
/// request/result fields). These are the only two files written; the first failure stops.
fn preserve_restore_preferences(target: &mut Settings, current: &Settings) {
    // Restore choices belong to the prompt session, not to the captured layout.
    for key in ["SnapshotBeforeRestore", "CorralSnapshotBackupFirst"] {
        if let Some(value) = current.tips.get(key) {
            target.tips.insert(key.into(), *value);
        } else {
            target.tips.remove(key);
        }
    }
}

pub(super) fn restore_to_disk(
    snapshot: &Snapshot,
    layout_path: &Path,
    settings_path: &Path,
) -> io::Result<()> {
    let current = settings_store::load_with_backup(settings_path);
    let mut restored = snapshot::for_capture(&snapshot.settings);
    preserve_restore_preferences(&mut restored, &current);
    // Old snapshots carry the legacy packed tint choice; make it explicit before it is dropped on write.
    let mut layout = snapshot.layout.clone();
    layout.migrate_tint_sources(restored.color);
    model::save(layout_path, &layout)?;
    settings_store::save(settings_path, &restored)
}

pub(super) enum Recovery {
    Nothing,
    /// Re-apply this snapshot (forward recovery), then delete the journal.
    Apply(Snapshot),
    /// The journal cannot be used; log the reason and delete it.
    Discard(String),
}

pub(super) fn recovery_decision(store: &Store) -> Recovery {
    match store.read_journal() {
        Ok(None) => Recovery::Nothing,
        Err(e) => Recovery::Discard(format!("journal unreadable: {e}")),
        Ok(Some(journal)) => match store.load(&journal.target_id) {
            Ok(snapshot) => Recovery::Apply(snapshot),
            Err(e) => Recovery::Discard(format!("journal target {} unavailable: {e}", journal.target_id)),
        },
    }
}

/// One automatic capture; the outcome is also recorded in the store (`last_error`) so the
/// settings window can show it. The ring advances only on a successful publication.
pub(super) fn run_daily(store: &Store, day: u32, title: &str, now_ms: u64, layout: &Layout, settings: &Settings) -> io::Result<()> {
    match store.capture_daily(day, title, now_ms, layout, settings) {
        Ok(_) => {
            if let Err(e) = store.set_last_error(None) {
                log(&format!("snapshots: clearing last-error failed: {e}"));
            }
            Ok(())
        }
        Err(e) => {
            if let Err(e2) = store.set_last_error(Some(&format!("Daily backup failed: {e}"))) {
                log(&format!("snapshots: recording last-error failed: {e2}"));
            }
            Err(e)
        }
    }
}

/// Automatic Round-up safeguards kept in the store.
const AUTOMATIC_KEPT: usize = 5;

// ---- local clock (kernel32) ---------------------------------------------------------------------

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct SysTime {
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
    fn GetLocalTime(time: *mut SysTime);
    fn GetDateFormatEx(locale: *const u16, flags: u32, date: *const SysTime, format: *const u16, out: *mut u16, cch: i32, calendar: *const u16) -> i32;
    fn GetTimeFormatEx(locale: *const u16, flags: u32, time: *const SysTime, format: *const u16, out: *mut u16, cch: i32) -> i32;
}

const DATE_SHORTDATE: u32 = 1;
const TIME_NOSECONDS: u32 = 2;

fn local_now() -> SysTime {
    let mut t = SysTime::default();
    unsafe { GetLocalTime(&mut t) };
    t
}

/// Short date in the user's locale ("local short date"), falling back to ISO.
fn short_date(t: &SysTime) -> String {
    let mut buffer = [0u16; 80];
    let n = unsafe { GetDateFormatEx(std::ptr::null(), DATE_SHORTDATE, t, std::ptr::null(), buffer.as_mut_ptr(), buffer.len() as i32, std::ptr::null()) };
    if n > 1 { String::from_utf16_lossy(&buffer[..n as usize - 1]) } else { format!("{:04}-{:02}-{:02}", t.year, t.month, t.day) }
}

fn short_time(t: &SysTime) -> String {
    let mut buffer = [0u16; 80];
    let n = unsafe { GetTimeFormatEx(std::ptr::null(), TIME_NOSECONDS, t, std::ptr::null(), buffer.as_mut_ptr(), buffer.len() as i32) };
    if n > 1 { String::from_utf16_lossy(&buffer[..n as usize - 1]) } else { format!("{:02}:{:02}", t.hour, t.minute) }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---- app integration ---------------------------------------------------------------------------

#[derive(Default)]
pub(super) struct State {
    store: Option<Store>,
    gate: CursorGate,
    /// The last failed automatic capture (also stored in the store folder for the settings window).
    pub(super) last_error: Option<String>,
    /// Nonce of the last request handled in this session (F3).
    last_nonce: Option<u64>,
    /// Interior mutability: layout saves go through `&self`.
    journal: std::cell::Cell<JournalState>,
}

impl App {
    /// Start-up, before settings and layout are loaded: open the store, finish an interrupted
    /// restore (forward recovery, which only rewrites the layout/settings files), arm the timer.
    pub(super) fn snapshot_init(&mut self) {
        unsafe { SetTimer(self.message_window, TIMER_SNAPSHOT, HOUR_MS, None) };
        let dir = snapshot::store_dir_for(&std::path::absolute(&self.opts.layout_path).unwrap_or_else(|_| self.opts.layout_path.clone()));
        let opened = std::fs::create_dir_all(&dir).and_then(|_| Store::open(&dir));
        let store = match opened {
            Ok(store) => store,
            Err(e) => {
                log(&format!("snapshots: store {} unavailable, running without backups: {e}", dir.display()));
                return;
            }
        };
        match recovery_decision(&store) {
            Recovery::Nothing => {}
            Recovery::Apply(target) => match restore_to_disk(&target, &self.opts.layout_path, &self.opts.settings_path) {
                Ok(()) => {
                    log(&format!("snapshots: interrupted restore recovered, re-applied backup {}", target.id));
                    if let Err(e) = store.clear_journal() {
                        log(&format!("snapshots: deleting the journal failed, retrying at the next layout save: {e}"));
                        self.snap.journal.set(JournalState::ClearPending);
                    }
                }
                Err(e) => {
                    // The pair may be half-replaced: don't autosave over it, so the next start can finish the restore.
                    log(&format!("snapshots: recovery of backup {} failed, journal kept, layout autosave off: {e}", target.id));
                    self.snap.journal.set(JournalState::Blocked);
                }
            },
            Recovery::Discard(reason) => {
                log(&format!("snapshots: restore journal discarded: {reason}"));
                if let Err(e) = store.clear_journal() {
                    log(&format!("snapshots: deleting the journal failed: {e}"));
                }
            }
        }
        self.snap.last_error = store.last_error();
        self.snap.store = Some(store);
    }

    /// Hourly timer. Returns true when `id` was this timer.
    pub(super) fn snapshot_timer(&mut self, id: usize) -> bool {
        if id != TIMER_SNAPSHOT {
            return false;
        }
        let mut cursor = POINT::default();
        if unsafe { GetCursorPos(&mut cursor) }.is_err() {
            return true;
        }
        if !self.snap.gate.moved((cursor.x, cursor.y)) || !self.settings.auto_take_snapshots {
            return true;
        }
        if self.snap.store.is_none() {
            return true;
        }
        self.flush_portal_views();
        let Some(store) = &self.snap.store else { return true };
        let t = local_now();
        let layout = self.layout_to_save().into_owned();
        let settings = snapshot::for_capture(&self.settings_to_save());
        let day = day_ordinal(t.year as i32, t.month as u32, t.day as u32);
        match run_daily(store, day, &daily_title(&short_date(&t)), now_ms(), &layout, &settings) {
            Ok(()) => {
                log(&format!("snapshots: daily backup taken (day {day})"));
                self.snap.last_error = None;
            }
            Err(e) => {
                log(&format!("snapshots: daily backup failed: {e}"));
                self.snap.last_error = Some(format!("Daily backup failed: {e}"));
            }
        }
        true
    }

    /// The settings window's `snapshot_request`, consumed like the rules request (after every
    /// settings reload). At start-up a leftover request is discarded unrun: it belongs to a
    /// session that is gone and a restore must never run unattended.
    pub(super) fn consume_snapshot_request(&mut self, startup: bool) {
        let Some(request) = self.settings.snapshot_request.clone() else { return };
        let nonce = request.nonce;
        let preview = matches!(self.opts.mode, super::Mode::Preview);
        // F5: a preview that shares the real settings file leaves the request alone (no answer, no
        // clear): it belongs to the real desktop instance, which may be running at the same time.
        if shares_real_settings(preview, &self.opts.settings_path, &settings_store::default_path()) {
            log(&format!("snapshots: request {nonce} left for the real instance (this preview shares its settings file)"));
            return;
        }
        let answer = |message: &str| Some(RequestResult { nonce, ok: false, message: message.into(), id: None });
        let result = if startup {
            answer(MSG_STALE)
        } else {
            match request_gate(nonce, now_ms(), self.snap.last_nonce) {
                Gate::Replay => None,
                Gate::TooOld => answer(MSG_OLD),
                Gate::Run => {
                    self.acknowledge_snapshot_request(nonce, matches!(request.action, Action::Restore { .. }));
                    Some(self.run_snapshot_request(&request))
                }
            }
        };
        if result.is_some() {
            self.snap.last_nonce = Some(self.snap.last_nonce.map_or(nonce, |l| l.max(nonce)));
        }
        match &result {
            Some(r) => log(&format!("snapshots: request {nonce} -> ok={} {:?}", r.ok, r.message)),
            None => log(&format!("snapshots: request {nonce} ignored (already handled)")),
        }
        self.settings.snapshot_request = None;
        if let Some(r) = result {
            self.settings.snapshot_result = Some(r);
        }
        if let Err(e) = settings_store::save(&self.opts.settings_path, &self.settings_to_save()) {
            log(&format!("snapshots: saving the result failed: {e}"));
        }
        self.settings_stamp = self.settings_file_stamp();
    }

    /// Take the request out of the settings file before running it. The settings window withdraws a
    /// request still in the file after 10 s and reports "not completed"; once it is gone, the window
    /// waits for the answer and reports "still working" instead (`backups::poll_decision`). Without
    /// this a restore longer than 10 s was reported as not completed while it went on running.
    fn acknowledge_snapshot_request(&mut self, nonce: u64, restore: bool) {
        self.settings.snapshot_request = None;
        if let Err(e) = settings_store::save(&self.opts.settings_path, &self.settings_to_save()) {
            log(&format!("snapshots: acknowledging request {nonce} failed: {e}"));
        }
        self.settings_stamp = self.settings_file_stamp();
        // QA only: simulate a slow restore in a preview (blocks this thread like a long restore).
        if restore && matches!(self.opts.mode, super::Mode::Preview) {
            if let Some(ms) = std::env::var("CORRAL_QA_SNAPSHOT_DELAY_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
                log(&format!("snapshots: QA delay {ms} ms for request {nonce}"));
                std::thread::sleep(std::time::Duration::from_millis(ms.min(120_000)));
            }
        }
    }

    /// colour-state-spec 4 (open question 6): "Reset every Corral" cannot be undone in the app, so
    /// a backup of the current pair is saved first. Without a backup store the reset is refused.
    pub(super) fn backup_before_reset_all(&self, layout: &Layout, settings: &Settings) -> Result<(), String> {
        let Some(store) = &self.snap.store else { return Err(MSG_NO_STORE.to_string()) };
        let t = local_now();
        let title = format!("Before resetting all Corrals {} {}", short_date(&t), short_time(&t));
        store.capture(&title, now_ms(), layout, settings).map(|_| ()).map_err(|e| e.to_string())
    }

    /// Commit a pending desk edit before a capture.
    fn flush_portal_views(&mut self) {
        // A captured snapshot must contain a desk edit still waiting for its quiet period.
        self.desk_save_timer();
    }

    /// An automatic backup of the current layout and settings, taken before Round-up moves icons
    /// (round-up-spec 4). It appears on the Backups page like any other.
    pub(super) fn snapshot_capture_automatic(&mut self, title: &str) -> Result<(), String> {
        self.flush_portal_views();
        let layout = self.layout_to_save().into_owned();
        let settings = snapshot::for_capture(&self.settings_to_save());
        let Some(store) = &self.snap.store else {
            return Err(MSG_NO_STORE.to_string());
        };
        store
            .capture_automatic(title, now_ms(), &layout, &settings)
            .map_err(|e| e.to_string())?;
        // Only the five newest safeguards are kept (older ones of this kind go; others stay).
        if let Err(e) = store.prune_automatic(AUTOMATIC_KEPT) {
            log(&format!("snapshots: pruning old automatic backups failed: {e}"));
        }
        Ok(())
    }

    /// False while a failed, un-rolled-back restore is pending (the journal must stay valid); a
    /// pending journal deletion is retried here.
    pub(super) fn snapshot_may_save_layout(&self) -> bool {
        let (allowed, state) = before_layout_save(self.snap.journal.get(), self.snap.store.as_ref());
        self.snap.journal.set(state);
        allowed
    }

    fn run_snapshot_request(&mut self, request: &Request) -> RequestResult {
        let nonce = request.nonce;
        let error = |message: &str| RequestResult { nonce, ok: false, message: message.into(), id: None };
        if self.snap.store.is_none() {
            return error(MSG_NO_STORE);
        }
        if matches!(request.action, Action::Restore { .. }) && (self.ole_drag.is_some() || !matches!(self.drag, Drag::None)) {
            return error(MSG_DRAG_ACTIVE);
        }
        // current state for captures; open portal views are flushed into the layout first
        self.flush_portal_views();
        let layout = self.layout_to_save().into_owned();
        let settings = snapshot::for_capture(&self.settings_to_save());
        let t = local_now();
        let auto_title = format!("{} {}", short_date(&t), short_time(&t));
        let executed = {
            let Some(store) = &self.snap.store else { return error(MSG_NO_STORE) };
            execute(store, request, &Env { layout: &layout, settings: &settings, now_ms: now_ms(), auto_title: &auto_title })
        };
        let Some(target) = executed.restore else { return executed.result };
        // The pair on disk first (the journal is already there), then memory.
        let finish = {
            let Some(store) = &self.snap.store else { return error(MSG_NO_STORE) };
            finish_restore(store, &target, (&layout, &settings), (&self.opts.layout_path, &self.opts.settings_path), restore_to_disk)
        };
        match finish {
            Finish::Done => {}
            Finish::RolledBack(e) => {
                // The journal is gone and the old pair is back on disk: autosave may run again.
                self.snap.journal.set(JournalState::Clear);
                log(&format!("snapshots: restore of {} failed while writing, old state written back: {e}", target.id));
                return error(&format!("Problem restoring snapshot.\n\n{e}"));
            }
            Finish::Incomplete(e) => {
                log(&format!("snapshots: restore of {} failed and could not be rolled back, journal kept: {e}", target.id));
                self.snap.journal.set(JournalState::Blocked);
                return error(&format!("Problem restoring snapshot.\n\n{e}\n\n{MSG_INCOMPLETE}"));
            }
        }
        self.install_restored(target.layout);
        if let Some(store) = &self.snap.store {
            let next = after_restore_clear(store.clear_journal().map_err(|e| log(&format!("snapshots: deleting the journal failed, retrying at the next layout save: {e}"))).is_ok());
            self.snap.journal.set(next);
        }
        executed.result
    }

    /// Install a restored layout/settings pair (already on disk) and rebuild everything that
    /// derives from it. Only Corral's windows and in-memory state are touched: the rules pass in
    /// `load_items` assigns references, and nothing here moves, renames, deletes or creates a
    /// Desktop or portal file (`finish_rename` is never called, so no IFileOperation runs).
    fn install_restored(&mut self, layout: Layout) {
        // in-progress interactions are dropped without committing
        if let Some((edit, _, font)) = self.naming.take() {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(edit);
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(windows::Win32::Graphics::Gdi::HGDIOBJ(font.0));
            }
        }
        if let Some((edit, _, font)) = self.renaming.take() {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(edit);
                let _ = windows::Win32::Graphics::Gdi::DeleteObject(windows::Win32::Graphics::Gdi::HGDIOBJ(font.0));
            }
        }
        self.page_cancel_motion();
        self.pages.drag_names.clear();
        self.drag = Drag::None;
        self.selection = Default::default();
        unsafe {
            let _ = KillTimer(self.message_window, super::TIMER_DRAG_RENDER);
            let _ = KillTimer(self.message_window, super::TIMER_HOVER_DELAY);
        }
        self.drag_render_pending = None;
        self.roll_progress.clear();
        self.roll_target.clear();
        self.roll_tween.clear();
        {
            let mut api = super::input::WinTimers(self.message_window);
            self.roll_gate.clear(&mut api);
        }
        self.roll_leave.reset();
        self.hot_group = None;
        self.roll_hover_suppressed = None;
        self.pending_hover = None;
        self.glyph_press = None;
        self.title_hover.clear();
        self.title_hot = None;
        self.title_clock = None;
        self.control_press = None;
        self.pending_drops.clear();
        self.pages.current = 0;
        // Group windows, portal shell views (not saved back: the old layout is being replaced)
        // and scrollbars are rebuilt from the new layout.
        self.drop_registrations.clear();
        // Gate hosts first (not saved back: the old layout is being replaced), while their owners
        // still exist, so the teardown order of folder-gate-host-spec 4.6 holds.
        self.portals.clear();
        for group in self.groups.drain(..) {
            unsafe {
                let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(group.hwnd);
            }
        }
        self.scrollbars.clear();
        // The pair was just written: take the settings from disk through the normal load path
        // (mouse mode, label colour, tips, blur), then the layout and its profile.
        self.color_overlay.forget(); // a restore ends any in-flight colour preview; restored data wins
        self.layout = layout;
        self.reload_settings();
        self.monitors = self.current_monitors();
        self.profile_startup(false);
        if !self.settings.swipe_enabled {
            self.page_cancel_motion();
        }
        self.load_items();
        self.rebuild_runtime_bodies();
        self.clamp_profile_titles();
        self.acknowledge_desk_display();
        self.refresh_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            static N: AtomicU32 = AtomicU32::new(0);
            let root = std::env::current_dir().unwrap().join("target\\qa\\snapshot-glue-tests");
            let dir = root.join(format!("test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(dir.join("backups")).unwrap();
            Scratch(dir)
        }
        fn store(&self) -> Store {
            Store::open(&self.0.join("backups")).unwrap()
        }
        fn layout_path(&self) -> PathBuf {
            self.0.join("layout.json")
        }
        fn settings_path(&self) -> PathBuf {
            self.0.join("settings.json")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            // only this test's own unique directory
            assert!(self.0.file_name().unwrap().to_string_lossy().starts_with("test-"));
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn marked_layout(next_id: u64) -> Layout {
        let mut l = Layout::new();
        l.next_id = next_id;
        l
    }
    fn marked_settings(auto: bool) -> Settings {
        let mut s = Settings::default();
        s.auto_take_snapshots = auto;
        s
    }
    fn env<'a>(layout: &'a Layout, settings: &'a Settings, title: &'a str) -> Env<'a> {
        Env { layout, settings, now_ms: 1_000, auto_title: title }
    }
    fn req(nonce: u64, action: Action) -> Request {
        Request { nonce, action }
    }

    #[test]
    fn day_ordinals_match_dotnet_ticks() {
        assert_eq!(day_ordinal(1, 1, 1), 0);
        assert_eq!(day_ordinal(1970, 1, 1), 719_162);
        assert_eq!(day_ordinal(2000, 1, 1), 730_119);
        assert_eq!(day_ordinal(2000, 3, 1) - day_ordinal(2000, 2, 28), 2); // leap year
        assert_eq!(day_ordinal(1900, 3, 1) - day_ordinal(1900, 2, 28), 1); // not a leap year
        // stored day ordinal 0xB4A3A is 2026-10-10
        assert_eq!(day_ordinal(2026, 10, 10), 0xB4A3A);
        assert_eq!(day_ordinal(2026, 10, 11), 0xB4A3B);
    }

    #[test]
    fn daily_title_text() {
        assert_eq!(daily_title("13.10.2026"), "Daily Backup (13.10.2026)");
    }

    #[test]
    fn cursor_gate_skips_an_unmoved_cursor() {
        let mut gate = CursorGate::default();
        assert!(gate.moved((10, 10)), "first sample counts as movement");
        assert!(!gate.moved((10, 10)));
        assert!(gate.moved((11, 10)));
        assert!(gate.moved((10, 10)), "back to an earlier point is still a change from the last sample");
        assert!(!gate.moved((10, 10)));
    }

    #[test]
    fn requests_run_against_a_store() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let (layout, settings) = (marked_layout(5), marked_settings(false));
        let e = env(&layout, &settings, "13.10.2026 10:00");

        let captured = execute(&store, &req(1, Action::Capture { title: None }), &e);
        assert!(captured.result.ok && captured.result.message == "Backup captured" && captured.result.nonce == 1);
        let id = captured.result.id.clone().unwrap();
        assert_eq!(store.load(&id).unwrap().title, "13.10.2026 10:00");

        let named = execute(&store, &req(2, Action::Capture { title: Some("Mine".into()) }), &e);
        assert!(named.result.ok);
        assert_eq!(store.load(named.result.id.as_ref().unwrap()).unwrap().title, "Mine");

        let empty = execute(&store, &req(3, Action::Capture { title: Some("  ".into()) }), &e);
        assert!(!empty.result.ok && empty.result.message == "You must give the snapshot a name");
        let long = execute(&store, &req(4, Action::Rename { id: id.clone(), title: "x".repeat(256) }), &e);
        assert!(!long.result.ok && long.result.message.contains("255"));

        let renamed = execute(&store, &req(5, Action::Rename { id: id.clone(), title: "Renamed".into() }), &e);
        assert!(renamed.result.ok);
        let newer_layout = marked_layout(9);
        let e2 = Env { layout: &newer_layout, settings: &settings, now_ms: 2_000, auto_title: "t" };
        let again = execute(&store, &req(6, Action::Recapture { id: id.clone() }), &e2);
        assert!(again.result.ok);
        let s = store.load(&id).unwrap();
        assert_eq!((s.title.as_str(), s.created_ms, s.layout.next_id), ("Renamed", 2_000, 9));

        let deleted = execute(&store, &req(7, Action::Delete { id: id.clone() }), &e);
        assert!(deleted.result.ok && deleted.result.message == "Backup deleted");
        assert!(store.load(&id).is_err());
        let missing = execute(&store, &req(8, Action::Delete { id: id.clone() }), &e);
        assert!(!missing.result.ok && missing.result.message.starts_with("Problem deleting backup."));
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn restore_writes_the_pair_and_the_journal_lifecycle() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let old = (marked_layout(3), marked_settings(false));
        let target = store.capture("Old", 1, &old.0, &old.1).unwrap();
        // the live state differs; a request is pending inside the live settings
        let live = (marked_layout(8), marked_settings(true));
        model::save(&scratch.layout_path(), &live.0).unwrap();
        settings_store::save(&scratch.settings_path(), &live.1).unwrap();

        let e = env(&live.0, &live.1, "before");
        let done = execute(&store, &req(9, Action::Restore { id: target.id.clone(), backup_first: true }), &e);
        assert!(done.result.ok && done.result.nonce == 9);
        assert_eq!(store.list().unwrap().len(), 2, "the pre-restore backup was taken");
        assert_eq!(store.read_journal().unwrap().unwrap().target_id, target.id);
        // nothing replaced yet
        assert_eq!(model::load(&scratch.layout_path()).unwrap().next_id, 8);

        let snapshot = done.restore.unwrap();
        restore_to_disk(&snapshot, &scratch.layout_path(), &scratch.settings_path()).unwrap();
        assert_eq!(model::load(&scratch.layout_path()).unwrap().next_id, 3);
        let back = settings_store::load_with_backup(&scratch.settings_path());
        assert!(!back.auto_take_snapshots && back.snapshot_request.is_none() && back.snapshot_result.is_none());
        store.clear_journal().unwrap();
        assert!(store.read_journal().unwrap().is_none());
        store.clear_journal().unwrap(); // absent is fine
    }

    #[test]
    fn restore_keeps_legacy_per_corral_tint_choices_as_explicit_sources() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let old = (marked_layout(3), marked_settings(false));
        let target = store.capture("Old", 1, &old.0, &old.1).unwrap();
        let live = (marked_layout(8), marked_settings(true));
        let e = env(&live.0, &live.1, "before");
        let done = execute(&store, &req(9, Action::Restore { id: target.id.clone(), backup_first: false }), &e);
        let mut snapshot = done.restore.unwrap();
        let colour = snapshot.settings.color;
        let same = snapshot.layout.add_group("A".into(), "m".into(), Default::default(), crate::model::GroupContent::Items { items: vec![] });
        let other = snapshot.layout.add_group("B".into(), "m".into(), Default::default(), crate::model::GroupContent::Items { items: vec![] });
        snapshot.layout.group_mut(same).unwrap().style.tint_icons_color_use = Some(0xff00_0000 | colour);
        snapshot.layout.group_mut(other).unwrap().style.tint_icons_color_use = Some(0xff00_0000 | (colour ^ 0x0010_2030));
        restore_to_disk(&snapshot, &scratch.layout_path(), &scratch.settings_path()).unwrap();
        let back = model::load(&scratch.layout_path()).unwrap();
        let source = |id| back.groups.iter().find(|g| g.id == id).unwrap().style.tint_icons_color_source.clone();
        assert_eq!(source(same).as_deref(), Some("corral"));
        assert_eq!(source(other).as_deref(), Some("custom"));
    }

    #[test]
    fn failed_pre_restore_capture_restores_nothing() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let target = store.capture("Old", 1, &marked_layout(3), &marked_settings(false)).unwrap();
        let live = (marked_layout(8), marked_settings(true));
        // a blank automatic title makes the safety capture fail
        let e = env(&live.0, &live.1, "  ");
        let done = execute(&store, &req(1, Action::Restore { id: target.id.clone(), backup_first: true }), &e);
        assert!(!done.result.ok);
        assert!(done.restore.is_none());
        assert!(store.read_journal().unwrap().is_none(), "no journal when nothing will be replaced");
        assert_eq!(store.list().unwrap().len(), 1, "no extra backup");
        // without the safety backup the same request is accepted
        let no_backup = execute(&store, &req(2, Action::Restore { id: target.id.clone(), backup_first: false }), &e);
        assert!(no_backup.result.ok && no_backup.restore.is_some());
        assert!(store.read_journal().unwrap().is_some());
    }

    #[test]
    fn restore_of_an_unknown_target_is_rejected_before_anything_happens() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let (layout, settings) = (marked_layout(1), marked_settings(true));
        let e = env(&layout, &settings, "t");
        for id in ["0".repeat(32), "../x".to_string()] {
            let done = execute(&store, &req(1, Action::Restore { id, backup_first: true }), &e);
            assert!(!done.result.ok && done.restore.is_none());
        }
        assert!(store.list().unwrap().is_empty() && store.read_journal().unwrap().is_none());
    }

    #[test]
    fn journal_recovery_decision() {
        let scratch = Scratch::new();
        let store = scratch.store();
        assert!(matches!(recovery_decision(&store), Recovery::Nothing));
        let target = store.capture("T", 1, &marked_layout(4), &marked_settings(false)).unwrap();
        store.write_journal(&target.id).unwrap();
        match recovery_decision(&store) {
            Recovery::Apply(s) => assert_eq!(s.id, target.id),
            _ => panic!("expected Apply"),
        }
        // the target vanished: unusable journal
        store.delete(&target.id).unwrap();
        assert!(matches!(recovery_decision(&store), Recovery::Discard(_)));
        // a corrupt journal is unusable too
        fs::write(scratch.0.join("backups").join("restore-journal.json"), "{ nope").unwrap();
        assert!(matches!(recovery_decision(&store), Recovery::Discard(_)));
        store.clear_journal().unwrap();
        assert!(matches!(recovery_decision(&store), Recovery::Nothing));
    }

    #[test]
    fn forward_recovery_reapplies_the_target_over_a_half_replaced_pair() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let target = store.capture("T", 1, &marked_layout(4), &marked_settings(false)).unwrap();
        // crash state: new layout written, old settings still there
        model::save(&scratch.layout_path(), &marked_layout(4)).unwrap();
        settings_store::save(&scratch.settings_path(), &marked_settings(true)).unwrap();
        store.write_journal(&target.id).unwrap();
        let Recovery::Apply(s) = recovery_decision(&store) else { panic!("expected Apply") };
        restore_to_disk(&s, &scratch.layout_path(), &scratch.settings_path()).unwrap();
        store.clear_journal().unwrap();
        assert!(!settings_store::load_with_backup(&scratch.settings_path()).auto_take_snapshots);
        assert_eq!(model::load(&scratch.layout_path()).unwrap().next_id, 4);
    }

    #[test]
    fn daily_backup_records_and_clears_its_last_error() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let (layout, settings) = (marked_layout(1), marked_settings(true));
        assert!(run_daily(&store, 100, "  ", 1, &layout, &settings).is_err());
        assert!(store.last_error().unwrap().starts_with("Daily backup failed"));
        run_daily(&store, 100, "Daily Backup (x)", 2, &layout, &settings).unwrap();
        assert!(store.last_error().is_none());
        // same day overwrites the slot; a new day advances to another
        run_daily(&store, 100, "Daily Backup (x)", 3, &layout, &settings).unwrap();
        assert_eq!(store.list().unwrap().len(), 1);
        run_daily(&store, 101, "Daily Backup (y)", 4, &layout, &settings).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);
    }

    // ---- review fixes F1, F3, F4, F5 ----

    #[test]
    fn failed_restore_write_rolls_back_to_the_memory_state_and_clears_the_journal() {
        let scratch = Scratch::new();
        let store = scratch.store();
        let target = store.capture("Old", 1, &marked_layout(3), &marked_settings(false)).unwrap();
        let live = (marked_layout(8), marked_settings(true));
        model::save(&scratch.layout_path(), &live.0).unwrap();
        settings_store::save(&scratch.settings_path(), &live.1).unwrap();
        let e = env(&live.0, &live.1, "before");
        let done = execute(&store, &req(1, Action::Restore { id: target.id.clone(), backup_first: false }), &e);
        let snapshot = done.restore.unwrap();
        assert!(store.read_journal().unwrap().is_some());
        // the layout is replaced, then the settings write fails: a half-replaced pair
        let half = |s: &Snapshot, l: &Path, _p: &Path| -> io::Result<()> {
            model::save(l, &s.layout)?;
            Err(io::Error::other("injected settings failure"))
        };
        let finish = finish_restore(&store, &snapshot, (&live.0, &live.1), (&scratch.layout_path(), &scratch.settings_path()), half);
        assert!(matches!(finish, Finish::RolledBack(ref m) if m.contains("injected")));
        assert!(store.read_journal().unwrap().is_none(), "no journal: start-up recovery must not apply the backup");
        assert_eq!(model::load(&scratch.layout_path()).unwrap().next_id, 8, "old layout back");
        assert!(settings_store::load_with_backup(&scratch.settings_path()).auto_take_snapshots, "old settings intact");
    }

    #[test]
    fn restore_with_a_real_write_failure_keeps_the_old_pair() {
        // settings path is a directory: the real restore_to_disk fails after the layout was written
        let scratch = Scratch::new();
        let store = scratch.store();
        let target = store.capture("Old", 1, &marked_layout(3), &marked_settings(false)).unwrap();
        let live = (marked_layout(8), marked_settings(true));
        let bad_settings = scratch.0.join("settings-is-a-directory");
        fs::create_dir_all(&bad_settings).unwrap();
        model::save(&scratch.layout_path(), &live.0).unwrap();
        store.write_journal(&target.id).unwrap();
        let finish = finish_restore(&store, &target, (&live.0, &live.1), (&scratch.layout_path(), &bad_settings), restore_to_disk);
        // the rollback cannot write the settings either: the journal stays and the caller blocks autosave
        assert!(matches!(finish, Finish::Incomplete(_)));
        assert!(store.read_journal().unwrap().is_some());
    }

    #[test]
    fn blocked_journal_suppresses_layout_autosave_and_pending_clear_is_retried() {
        let scratch = Scratch::new();
        let store = scratch.store();
        assert_eq!(before_layout_save(JournalState::Clear, Some(&store)), (true, JournalState::Clear));
        assert_eq!(before_layout_save(JournalState::Blocked, Some(&store)), (false, JournalState::Blocked));
        // F4: the journal cannot be deleted (a directory stands in its place), then it can
        let journal = scratch.0.join("backups").join("restore-journal.json");
        fs::create_dir_all(&journal).unwrap();
        assert!(store.clear_journal().is_err());
        assert_eq!(before_layout_save(JournalState::ClearPending, Some(&store)), (true, JournalState::ClearPending));
        fs::remove_dir(&journal).unwrap();
        let target = store.capture("T", 1, &marked_layout(1), &marked_settings(false)).unwrap();
        store.write_journal(&target.id).unwrap();
        assert_eq!(before_layout_save(JournalState::ClearPending, Some(&store)), (true, JournalState::Clear));
        assert!(store.read_journal().unwrap().is_none(), "the next layout save deleted the journal");
    }

    #[test]
    fn request_gate_rejects_old_future_and_replayed_nonces() {
        let now = 10_000_000u64;
        assert_eq!(request_gate(now - 1_000, now, None), Gate::Run);
        assert_eq!(request_gate(now - snapshot::REQUEST_MAX_AGE_MS, now, None), Gate::Run);
        assert_eq!(request_gate(now - snapshot::REQUEST_MAX_AGE_MS - 1, now, None), Gate::TooOld);
        assert_eq!(request_gate(now + snapshot::REQUEST_MAX_AGE_MS + 1, now, None), Gate::TooOld);
        // replays: not newer than the last handled nonce
        assert_eq!(request_gate(now - 500, now, Some(now - 500)), Gate::Replay);
        assert_eq!(request_gate(now - 600, now, Some(now - 500)), Gate::Replay);
        assert_eq!(request_gate(now - 400, now, Some(now - 500)), Gate::Run);
        // an old replay is a replay first (ignored silently)
        assert_eq!(request_gate(5, now, Some(7)), Gate::Replay);
    }

    #[test]
    fn a_successful_restore_after_a_blocked_one_lifts_the_block() {
        assert_eq!(after_restore_clear(true), JournalState::Clear);
        assert_eq!(after_restore_clear(false), JournalState::ClearPending);
        assert_eq!(before_layout_save(after_restore_clear(true), None), (true, JournalState::Clear));
    }

    #[test]
    fn preview_with_the_default_settings_path_refuses_requests() {
        let default = Path::new(r"C:\Users\x\AppData\Roaming\Corral\settings.json");
        assert!(shares_real_settings(true, default, default));
        assert!(shares_real_settings(true, Path::new(r"c:\users\X\appdata\roaming\corral\settings.json"), default));
        assert!(!shares_real_settings(true, Path::new(r"C:\qa\settings.json"), default));
        assert!(!shares_real_settings(false, default, default), "desktop mode is the real instance");
    }
}

#[cfg(test)]
mod u4_restore_preferences {
    use super::*;
    #[test]
    fn restore_keeps_current_prompt_choices_and_does_not_resurrect_old_ones() {
        let mut captured = Settings::default();
        captured.tips.insert("Unrelated".into(), 99);
        let mut current = Settings::default();
        current.tips.insert("SnapshotBeforeRestore".into(), 7);
        current.tips.insert("CorralSnapshotBackupFirst".into(), 1);
        preserve_restore_preferences(&mut captured, &current);
        assert_eq!(captured.tips.get("SnapshotBeforeRestore"), Some(&7));
        assert_eq!(captured.tips.get("CorralSnapshotBackupFirst"), Some(&1));
        assert_eq!(captured.tips.get("Unrelated"), Some(&99));
        preserve_restore_preferences(&mut captured, &Settings::default());
        assert!(!captured.tips.contains_key("SnapshotBeforeRestore"));
        assert!(!captured.tips.contains_key("CorralSnapshotBackupFirst"));
    }
    #[test]
    fn engine_name_validation_accepts_exactly_255_unicode_characters() {
        for name in ["a".repeat(255), "😀".repeat(255)] {
            assert_eq!(title_error(&name), None);
        }
        for name in ["a".repeat(256), "😀".repeat(256)] {
            assert!(title_error(&name).is_some());
        }
    }
}
