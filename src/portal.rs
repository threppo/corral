//! Folder Gate navigation, history, errors and off-thread folder probes.

use std::cell::Cell;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::HSTRING;

use std::cmp::Ordering;
use std::time::Duration;

use crate::geometry::Rect;
use crate::view_state::{folder_key, parent_key};

/// Longest trail a Gate keeps (folder-gate-host-spec 4.1): a useful trail, a few kilobytes of
/// layout at most; Back is a convenience, not an archive.
pub const HISTORY_CAP: usize = 20;

/// Whether `path` is the Gate's `root` or lies inside it (folder-gate-host-spec 4.1): same key, or
/// the key of `path` starts with the root key plus `\` (a root that ends in `\` matches by prefix).
pub fn in_root(root: &str, path: &str) -> bool {
    let (r, p) = (folder_key(root), folder_key(path));
    if r.is_empty() || p.is_empty() {
        return false;
    }
    if p == r {
        return true;
    }
    if r.ends_with('\\') { p.starts_with(&r) } else { p.starts_with(&format!("{r}\\")) }
}

/// Whether two paths name the same folder (key equality, folder-gate-views-spec 3.1).
pub fn same_folder(a: &str, b: &str) -> bool {
    folder_key(a) == folder_key(b)
}

/// The folder above `path` with the caller's spelling kept: the last component and trailing
/// separators dropped (folder-gate-host-spec 4.2). A drive root, a share root and a path without a
/// file-system parent have none.
pub fn parent(path: &str) -> Option<String> {
    parent_key(path)?;
    let trimmed = path.trim_end_matches(['\\', '/']);
    let cut = trimmed.rfind(['\\', '/'])?;
    let mut above = trimmed[..cut].to_string();
    if above.ends_with(':') {
        above.push('\\');
    }
    Some(above)
}

/// Where a navigation request came from (folder-gate-host-spec 4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavOrigin {
    /// A Gate command, Back, Forward, Up, root or restore: Corral started it and carries a token.
    Corral,
    /// The user opened a folder item inside the Gate.
    View,
}

/// What the Gate does with a navigation (folder-gate-host-spec 4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavDecision {
    Allow,
    /// Open the target in a normal window instead of navigating inside the Gate.
    OpenExternally,
    /// Refuse without opening anything (the Gate is going away).
    Cancel,
}

/// The ordered history of a Gate (folder-gate-host-spec 4.1, 4.2). `entries` are oldest first;
/// `entries[cursor]` is the folder shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateNav {
    root: String,
    entries: Vec<String>,
    cursor: usize,
    allow: bool,
}

/// A Back or Forward step already taken on the cursor, kept so a failed browse can undo it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub target: String,
    from: usize,
}

impl GateNav {
    #[cfg(test)]
    /// A new Gate: history `[root]`, navigation as given (new Gates default to on, 4.5).
    pub fn new(root: &str, allow: bool) -> GateNav {
        GateNav { root: root.to_string(), entries: vec![root.to_string()], cursor: 0, allow }
    }

    /// Read the stored shape: `nav_history`, newest first, entry 0 = the folder shown. Entries
    /// outside the root are dropped, repeats merged, a list over [`HISTORY_CAP`] keeps the entries
    /// ending at the cursor (the last one), an empty list means the root (folder-gate-host-spec 4.4).
    pub fn from_stored(root: &str, stored: &[String], allow: bool) -> GateNav {
        let mut nav = GateNav { root: root.to_string(), entries: Vec::new(), cursor: 0, allow };
        for entry in stored.iter().rev() {
            if in_root(root, entry) && nav.entries.last().is_none_or(|last| !same_folder(last, entry)) {
                nav.entries.push(entry.clone());
            }
        }
        if nav.entries.is_empty() {
            nav.entries.push(root.to_string());
        }
        let excess = nav.entries.len().saturating_sub(HISTORY_CAP);
        nav.entries.drain(..excess);
        nav.cursor = nav.entries.len() - 1;
        nav
    }

    /// The stored shape for the layout: the trail up to the cursor, newest first. A Gate that never
    /// left its root stores nothing. Forward entries are not stored.
    pub fn to_stored(&self) -> Vec<String> {
        if self.cursor == 0 && same_folder(&self.entries[0], &self.root) {
            return Vec::new();
        }
        self.entries[..=self.cursor].iter().rev().cloned().collect()
    }

    pub fn root(&self) -> &str {
        &self.root
    }
    pub fn current(&self) -> &str {
        &self.entries[self.cursor]
    }
    pub fn allow(&self) -> bool {
        self.allow
    }
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    #[cfg(test)]
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn at_root(&self) -> bool {
        same_folder(self.current(), &self.root)
    }
    pub fn can_back(&self) -> bool {
        self.cursor > 0
    }
    pub fn can_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }
    /// The Up button and command are available (folder-gate-host-spec 4.5).
    pub fn can_up(&self) -> bool {
        self.allow && !self.at_root() && parent(self.current()).is_some()
    }

    /// Record a successful navigation (`OnNavigationComplete`): nothing for the folder already
    /// shown, otherwise the entries after the cursor go, the folder is appended and the oldest
    /// entry drops past the cap. Returns whether the history changed.
    pub fn visit(&mut self, path: &str) -> bool {
        if same_folder(self.current(), path) || !in_root(&self.root, path) {
            return false;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(path.to_string());
        if self.entries.len() > HISTORY_CAP {
            self.entries.remove(0);
        }
        self.cursor = self.entries.len() - 1;
        true
    }

    /// Move the cursor one entry back and return the folder to browse (the caller browses; on a
    /// failed browse it calls [`GateNav::step_failed`]).
    pub fn back(&mut self) -> Option<Step> {
        if !self.can_back() {
            return None;
        }
        let from = self.cursor;
        self.cursor -= 1;
        Some(Step { target: self.entries[self.cursor].clone(), from })
    }

    pub fn forward(&mut self) -> Option<Step> {
        if !self.can_forward() {
            return None;
        }
        let from = self.cursor;
        self.cursor += 1;
        Some(Step { target: self.entries[self.cursor].clone(), from })
    }

    /// The browse of a Back/Forward step failed: the entry goes (no dead ends) and the cursor stays
    /// on the folder it came from.
    pub fn step_failed(&mut self, step: &Step) {
        let moved_to = self.cursor;
        if moved_to >= self.entries.len() || !same_folder(&self.entries[moved_to], &step.target) {
            return;
        }
        self.entries.remove(moved_to);
        self.cursor = if moved_to < step.from { step.from - 1 } else { step.from };
        self.cursor = self.cursor.min(self.entries.len() - 1);
        // Removing an entry can leave the same folder twice in a row: merge.
        let mut i = 1;
        while i < self.entries.len() {
            if same_folder(&self.entries[i - 1], &self.entries[i]) {
                self.entries.remove(i);
                if self.cursor >= i {
                    self.cursor -= 1;
                }
            } else {
                i += 1;
            }
        }
    }

    /// The folder Up goes to, when Up is available. The history changes when the navigation
    /// completes ([`GateNav::visit`]).
    pub fn up_target(&self) -> Option<String> {
        if !self.can_up() {
            return None;
        }
        parent(self.current()).filter(|p| in_root(&self.root, p))
    }

    #[cfg(test)]
    /// Up as a model operation: `visit(parent(current))`.
    pub fn up(&mut self) -> Option<String> {
        let target = self.up_target()?;
        self.visit(&target);
        Some(target)
    }

    /// The root, unless the Gate already shows it (or cannot navigate).
    pub fn root_target(&self) -> Option<String> {
        (self.allow && !self.at_root()).then(|| self.root.clone())
    }

    #[cfg(test)]
    pub fn to_root(&mut self) -> Option<String> {
        let target = self.root_target()?;
        self.visit(&target);
        Some(target)
    }

    /// Allow navigation changed. Turning it off sends the Gate to its root now and clears the
    /// trail, so a Gate that cannot navigate never sits in a subfolder; the root to browse is
    /// returned when the Gate was elsewhere. Turning it on keeps `[root]`.
    pub fn set_allow(&mut self, allow: bool) -> Option<String> {
        let elsewhere = !self.at_root();
        self.allow = allow;
        if allow {
            return None;
        }
        self.entries = vec![self.root.clone()];
        self.cursor = 0;
        elsewhere.then(|| self.root.clone())
    }

    /// "Choose a different folder...": a new root with a fresh trail (the title is not touched).
    pub fn choose_root(&mut self, folder: &str) {
        self.root = folder.to_string();
        self.entries = vec![folder.to_string()];
        self.cursor = 0;
    }
}

/// Decide what a navigation may do (folder-gate-host-spec 4.3). `target` is the file-system path
/// of the destination, `None` for a location without one. `closing` is set once the Gate is being
/// torn down.
pub fn decide_nav(nav: &GateNav, target: Option<&str>, origin: NavOrigin, closing: bool) -> NavDecision {
    if closing {
        return NavDecision::Cancel;
    }
    let Some(target) = target else { return NavDecision::OpenExternally };
    if !in_root(nav.root(), target) {
        return NavDecision::OpenExternally;
    }
    if origin == NavOrigin::View && !nav.allow() && !same_folder(nav.current(), target) {
        return NavDecision::OpenExternally;
    }
    NavDecision::Allow
}

/// What a Gate shows on start (folder-gate-host-spec 4.4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Start {
    /// Browse this folder: the stored current, or the nearest ancestor inside the root that exists
    /// (`replaced` tells the caller to record it as a visit once the browse completes).
    Browse { path: String, replaced: bool },
    /// The root itself is unavailable: the Gate starts in the Error body with its stored state
    /// untouched, so Retry resumes exactly where it was.
    Error,
}

/// Pure: never changes `nav`, so loading never rewrites the file.
pub fn resolve_start(nav: &GateNav, exists: impl Fn(&str) -> bool) -> Start {
    let mut candidate = nav.current().to_string();
    let mut replaced = false;
    loop {
        if exists(&candidate) {
            return Start::Browse { path: candidate, replaced };
        }
        if same_folder(&candidate, nav.root()) {
            return Start::Error;
        }
        match parent(&candidate).filter(|p| in_root(nav.root(), p)) {
            Some(above) => {
                candidate = above;
                replaced = true;
            }
            None => return Start::Error,
        }
    }
}

/// Result of the folder probe (folder-gate-host-spec 3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Probe {
    Ok,
    Missing,
    Denied,
    Offline,
    Other(u32),
}

/// Longest the worker waits for `GetFileAttributesW` before the folder counts as offline.
pub const PROBE_LIMIT: Duration = Duration::from_millis(3000);
/// Offline Gates retry this often while visible.
pub const OFFLINE_RETRY: Duration = Duration::from_secs(60);

/// Classify a probe by the Win32 error code; `None` means the probe timed out (folder-gate-host-spec 3.3).
pub fn classify_probe(code: Option<u32>) -> Probe {
    match code {
        None => Probe::Offline,
        Some(0) => Probe::Ok,
        Some(2 | 3 | 267) => Probe::Missing,
        Some(21 | 53 | 59 | 64 | 67 | 121 | 1167 | 1201 | 1222 | 1231 | 2250) => Probe::Offline,
        Some(5 | 1219 | 1326) => Probe::Denied,
        Some(other) => Probe::Other(other),
    }
}

/// Title and detail of the error body (folder-gate-host-spec 3.3). `Ok` has no text.
pub fn error_text(probe: Probe) -> Option<(String, String)> {
    let (title, detail) = match probe {
        Probe::Ok => return None,
        Probe::Missing => ("This folder isn't here".to_string(), "It may have been moved, renamed or deleted.".to_string()),
        Probe::Offline => ("Can't reach this folder".to_string(), "Check the network connection or the drive, then try again.".to_string()),
        Probe::Denied => ("No access".to_string(), "You don't have permission to open this folder.".to_string()),
        Probe::Other(code) => ("Can't open this folder".to_string(), format!("Windows reported error 0x{code:08X}.")),
    };
    Some((title, detail))
}

/// Automatic retry interval of an error kind: only Offline retries by itself (a VPN or share can
/// return without an event; a missing folder will not).
pub fn auto_retry(probe: Probe) -> Option<Duration> {
    matches!(probe, Probe::Offline).then_some(OFFLINE_RETRY)
}

/// Title while navigated: when the user title still equals the root folder's display name
/// and the shown folder's name differs, the strip shows the shown folder's name; a custom title
/// stays.
pub fn navigated_title(user_title: &str, root_name: &str, target_name: &str) -> Option<String> {
    (user_title == root_name && target_name != root_name).then(|| target_name.to_string())
}

/// Open the glyph menu at the button's bottom-left corner, left/top aligned.
pub fn glyph_menu_anchor(glyph: Rect) -> (i32, i32) {
    (glyph.left, glyph.bottom)
}

/// Last path component (fallback display name; the app prefers the shell display name).
pub fn last_component(path: &str) -> &str {
    path.trim_end_matches('\\').rsplit('\\').next().filter(|s| !s.is_empty()).unwrap_or(path)
}

/// Explorer's default "Name" order: folders first, then a logical (digit-aware), case-insensitive
/// name comparison (StrCmpLogicalW-like). Used by the headless `--probe-portal`.
pub fn explorer_order(a_folder: bool, a_name: &str, b_folder: bool, b_name: &str) -> Ordering {
    b_folder.cmp(&a_folder).then_with(|| logical_cmp(a_name, b_name))
}

fn logical_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        it.next();
                    }
                    digits
                };
                let (da, db) = (take(&mut a), take(&mut b));
                let (ta, tb) = (da.trim_start_matches('0'), db.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// Window-relative buttons below the three-line Gate error (folder-gate-host-spec 3.3).
pub fn error_buttons(body: crate::geometry::Rect, dpi: u32) -> [crate::geometry::Rect; 2] {
    use crate::geometry::Rect;
    use crate::icon_metrics::dip_to_px;
    let px = |d| dip_to_px(d, dpi);
    let gap = px(8.0).min(body.height().max(0) / 3);
    let height = px(30.0).min((body.height().max(0) - gap) / 2);
    let total = height * 2 + gap;
    let text = px(22.0) * 3 + px(12.0);
    let top = (body.top + (body.height() - text - total) / 2 + text)
        .clamp(body.top, (body.bottom - total).max(body.top));
    let mut buttons = [96.0, 220.0].map(|width| {
        let width = px(width).min((body.width() - px(24.0)).max(0));
        let left = body.left + (body.width() - width) / 2;
        Rect::new(left, top, left + width, top + height)
    });
    buttons[1] = buttons[1].offset(0, height + gap);
    buttons
}

pub const WM_PORTAL_PROBE: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 0x3d;

/// What the worker found; `code` is the Win32 error (0: the folder is there).
pub struct ProbeResult {
    /// The folder to browse (the nearest existing one after a start-up climb), or the one that failed.
    pub path: String,
    pub code: u32,
    /// The probe this answers (a newer probe supersedes it).
    pub seq: u64,
    /// The start-up climb found an ancestor instead of the stored folder.
    pub replaced: bool,
}

fn attributes_error(path: &str) -> u32 {
    use windows::Win32::Foundation::GetLastError;
    use windows::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES};
    let attributes = unsafe { GetFileAttributesW(&HSTRING::from(path)) };
    if attributes == INVALID_FILE_ATTRIBUTES { unsafe { GetLastError() }.0 } else { 0 }
}

fn post_probe(host_bits: isize, result: ProbeResult) {
    let boxed = Box::into_raw(Box::new(result));
    let host = HWND(host_bits as *mut _);
    if unsafe { PostMessageW(host, WM_PORTAL_PROBE, WPARAM(0), LPARAM(boxed as isize)) }.is_err() {
        drop(unsafe { Box::from_raw(boxed) });
    }
}

/// A request number that is never reused, across every Gate and every replacement.
pub fn next_probe_seq() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// Probe `path` on a worker thread; the answer is posted to `host` as [`WM_PORTAL_PROBE`]. The UI
/// thread never waits on a dead share: the caller arms the 3000 ms probe-limit timer.
pub fn probe_folder(host: HWND, path: &str, seq: u64) {
    let path = path.to_string();
    let host_bits = host.0 as isize;
    std::thread::spawn(move || {
        let code = attributes_error(&path);
        post_probe(host_bits, ProbeResult { path, code, seq, replaced: false });
    });
}

/// Probe the start of a Gate (folder-gate-host-spec 4.4) on a worker: the stored current folder, or
/// the nearest existing ancestor inside the root, or the failure of the root itself.
pub fn probe_start(host: HWND, nav: GateNav, seq: u64) {
    let host_bits = host.0 as isize;
    std::thread::spawn(move || {
        let last_error = Cell::new(0u32);
        let start = resolve_start(&nav, |p| {
            let code = attributes_error(p);
            last_error.set(code);
            code == 0
        });
        let result = match start {
            Start::Browse { path, replaced } => ProbeResult { path, code: 0, seq, replaced },
            Start::Error => ProbeResult { path: nav.current().to_string(), code: last_error.get().max(1), seq, replaced: false },
        };
        post_probe(host_bits, result);
    });
}

/// Take ownership of a [`WM_PORTAL_PROBE`] result.
pub fn take_probe(lparam: LPARAM) -> Option<ProbeResult> {
    (lparam.0 != 0).then(|| *unsafe { Box::from_raw(lparam.0 as *mut ProbeResult) })
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_buttons_fit_the_body_without_overlap() {
        use crate::geometry::Rect;
        for dpi in [96, 240] {
            for (width, height) in [(320, 240), (140, 100), (20, 20)] {
                let px = |d| crate::icon_metrics::dip_to_px(d as f64, dpi);
                let body = Rect::new(13, 37, 13 + px(width), 37 + px(height));
                let buttons = error_buttons(body, dpi);
                for r in buttons {
                    assert!(r.left >= body.left && r.right <= body.right);
                    assert!(r.top >= body.top && r.bottom <= body.bottom);
                    assert_eq!(r.left - body.left, body.right - r.right);
                }
                assert_eq!(buttons[0].overlap_area(&buttons[1]), 0);
            }
        }
    }


    const ROOT: &str = r"C:\Data\Docs";

    fn sub(n: usize) -> String {
        format!(r"{ROOT}\s{n}")
    }

    fn assert_invariants(nav: &GateNav) {
        assert!((1..=HISTORY_CAP).contains(&nav.len()), "length {}", nav.len());
        assert!(nav.cursor() < nav.len());
        for e in &nav.entries {
            assert!(in_root(nav.root(), e), "{e} outside {}", nav.root());
        }
        for pair in nav.entries.windows(2) {
            assert!(!same_folder(&pair[0], &pair[1]), "repeat {}", pair[0]);
        }
    }

    #[test]
    fn glyph_menu_opens_at_button_bottom_left() {
        for glyph in [Rect::new(100, 20, 120, 40), Rect::new(250, 50, 300, 100), Rect::new(-90, -40, -50, 0)] {
            assert_eq!(glyph_menu_anchor(glyph), (glyph.left, glyph.bottom));
        }
    }

    #[test]
    fn title_follows_navigation_only_without_custom_title() {
        assert_eq!(navigated_title("Docs", "Docs", "Work").as_deref(), Some("Work"));
        assert_eq!(navigated_title("My stuff", "Docs", "Work"), None);
        assert_eq!(navigated_title("Docs", "Docs", "Docs"), None);
        assert_eq!(last_component(r"C:\Data\Docs\"), "Docs");
    }

    #[test]
    fn folders_first_then_logical_names() {
        let mut names = vec![(false, "file10.txt"), (true, "zeta"), (false, "File2.txt"), (true, "Alpha"), (false, "file1.txt")];
        names.sort_by(|a, b| explorer_order(a.0, a.1, b.0, b.1));
        let order: Vec<&str> = names.iter().map(|n| n.1).collect();
        assert_eq!(order, ["Alpha", "zeta", "file1.txt", "File2.txt", "file10.txt"]);
    }

    // folder-gate-host-spec 8, test 3
    #[test]
    fn in_root_is_a_component_test_not_a_prefix_test() {
        assert!(in_root(r"C:\a", r"C:\a"));
        assert!(in_root(r"C:\a", r"C:\a\b"));
        assert!(!in_root(r"C:\a", r"C:\ab"));
        assert!(!in_root(r"C:\a\b", r"C:\a"));
        // trailing separators and case
        assert!(in_root(r"C:\a\", r"c:\A\B\"));
        assert!(in_root(r"C:\a", r"C:\a\"));
        // verbatim forms
        assert!(in_root(r"\\?\C:\a", r"C:\a\b"));
        assert!(in_root(r"C:\a", r"\\?\C:\a\b"));
        assert!(in_root(r"\\?\UNC\srv\share\x", r"\\srv\share\x\y"));
        // share roots
        assert!(in_root(r"\\srv\share", r"\\srv\share\dir"));
        assert!(!in_root(r"\\srv\share", r"\\srv\share2\dir"));
        assert!(!in_root(r"\\srv\share", r"\\srv\other"));
        // a drive root matches by prefix
        assert!(in_root(r"C:\", r"C:\Users"));
        assert!(in_root(r"C:\", r"C:\"));
        assert!(!in_root(r"C:\", r"D:\Users"));
        // no file-system path
        assert!(!in_root(r"C:\a", ""));
        assert!(!in_root("", r"C:\a"));
    }

    #[test]
    fn parent_keeps_spelling_and_stops_at_roots() {
        assert_eq!(parent(r"C:\Data\Docs\Work").as_deref(), Some(r"C:\Data\Docs"));
        assert_eq!(parent(r"C:\Data\Docs\Work\").as_deref(), Some(r"C:\Data\Docs"));
        assert_eq!(parent(r"C:\Data").as_deref(), Some(r"C:\"));
        assert_eq!(parent(r"C:\"), None);
        assert_eq!(parent(r"\\srv\share\dir").as_deref(), Some(r"\\srv\share"));
        assert_eq!(parent(r"\\srv\share"), None);
        assert_eq!(parent(r"\\?\C:\a\b").as_deref(), Some(r"\\?\C:\a"));
        assert_eq!(parent("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}"), None);
    }

    // test 2
    #[test]
    fn visit_dedupes_and_truncates_forward_entries() {
        let mut nav = GateNav::new(ROOT, true);
        assert!(nav.visit(&sub(1)));
        assert!(!nav.visit(&sub(1).to_uppercase()), "same folder, other case");
        assert!(nav.visit(&sub(2)));
        assert_eq!(nav.len(), 3);
        let back = nav.back().unwrap();
        assert_eq!(back.target, sub(1));
        assert!(nav.can_forward());
        assert!(nav.visit(&sub(3)));
        assert_eq!(nav.entries, [ROOT.to_string(), sub(1), sub(3)]);
        assert!(!nav.can_forward());
        assert!(!nav.visit(r"C:\Elsewhere"), "outside the root never enters the trail");
    }

    #[test]
    fn the_21st_visit_drops_the_oldest() {
        let mut nav = GateNav::new(ROOT, true);
        for n in 1..=19 {
            nav.visit(&sub(n));
        }
        assert_eq!(nav.len(), 20);
        assert_eq!(nav.entries[0], ROOT);
        nav.visit(&sub(20));
        assert_eq!(nav.len(), 20);
        assert_eq!(nav.entries[0], sub(1));
        assert_eq!(nav.current(), sub(20));
        assert_eq!(nav.cursor(), 19);
    }

    #[test]
    fn back_and_forward_stop_at_the_ends() {
        let mut nav = GateNav::new(ROOT, true);
        assert!(nav.back().is_none() && nav.forward().is_none());
        nav.visit(&sub(1));
        assert_eq!(nav.back().unwrap().target, ROOT);
        assert!(nav.back().is_none());
        assert_eq!(nav.forward().unwrap().target, sub(1));
        assert!(nav.forward().is_none());
    }

    #[test]
    fn up_stops_at_the_root_and_never_leaves_it() {
        let mut nav = GateNav::new(ROOT, true);
        assert_eq!(nav.up(), None);
        nav.visit(&format!(r"{ROOT}\a\b"));
        assert_eq!(nav.up().as_deref(), Some(format!(r"{ROOT}\a").as_str()));
        assert_eq!(nav.up().as_deref(), Some(ROOT));
        assert_eq!(nav.up(), None, "at the root");
        assert!(!nav.can_up());
        let mut off = GateNav::new(ROOT, false);
        off.entries.push(sub(1));
        off.cursor = 1;
        assert!(!off.can_up(), "Up needs Allow navigation");
    }

    #[test]
    fn to_root_at_the_root_is_a_no_op() {
        let mut nav = GateNav::new(ROOT, true);
        assert_eq!(nav.to_root(), None);
        nav.visit(&sub(1));
        assert_eq!(nav.to_root().as_deref(), Some(ROOT));
        assert_eq!(nav.current(), ROOT);
        assert_eq!(nav.to_root(), None);
        assert!(nav.can_back(), "the way back to the subfolder stays");
    }

    #[test]
    fn a_failed_back_removes_its_entry_and_the_cursor_stays() {
        let mut nav = GateNav::new(ROOT, true);
        nav.visit(&sub(1));
        nav.visit(&sub(2));
        let step = nav.back().unwrap();
        assert_eq!(step.target, sub(1));
        nav.step_failed(&step);
        assert_eq!(nav.entries, [ROOT.to_string(), sub(2)]);
        assert_eq!(nav.current(), sub(2));
        // a failed forward
        let mut nav = GateNav::new(ROOT, true);
        nav.visit(&sub(1));
        nav.visit(&sub(2));
        nav.back();
        nav.back();
        let step = nav.forward().unwrap();
        nav.step_failed(&step);
        assert_eq!(nav.entries, [ROOT.to_string(), sub(2)]);
        assert_eq!(nav.current(), ROOT);
        assert_invariants(&nav);
    }

    #[test]
    fn a_failed_step_merges_the_neighbours_it_leaves_equal() {
        let mut nav = GateNav::new(ROOT, true);
        nav.visit(&sub(1));
        nav.visit(ROOT);
        assert_eq!(nav.entries, [ROOT.to_string(), sub(1), ROOT.to_string()]);
        let step = nav.back().unwrap();
        assert_eq!(step.target, sub(1));
        nav.step_failed(&step);
        assert_eq!(nav.entries, [ROOT.to_string()], "the two roots merge");
        assert_eq!(nav.cursor(), 0);
        assert_invariants(&nav);
        // a failed forward step in the middle of a longer trail
        let mut nav = GateNav::new(ROOT, true);
        for n in 1..=3 {
            nav.visit(&sub(n));
        }
        nav.back();
        nav.back();
        let step = nav.forward().unwrap();
        assert_eq!(step.target, sub(2));
        nav.step_failed(&step);
        assert_eq!(nav.entries, [ROOT.to_string(), sub(1), sub(3)]);
        assert_eq!(nav.current(), sub(1));
        assert_invariants(&nav);
    }

    #[test]
    fn turning_navigation_off_goes_to_the_root_with_a_one_entry_trail() {
        let mut nav = GateNav::new(ROOT, true);
        nav.visit(&sub(1));
        nav.visit(&sub(2));
        assert_eq!(nav.set_allow(false).as_deref(), Some(ROOT));
        assert_eq!(nav.entries, [ROOT.to_string()]);
        assert!(!nav.allow());
        assert_eq!(nav.set_allow(true), None);
        assert_eq!(nav.entries, [ROOT.to_string()]);
        // already at the root: nothing to browse
        assert_eq!(nav.set_allow(false), None);
    }

    #[test]
    fn choose_root_resets_the_trail() {
        let mut nav = GateNav::new(ROOT, true);
        nav.visit(&sub(1));
        nav.choose_root(r"D:\Other");
        assert_eq!(nav.root(), r"D:\Other");
        assert_eq!(nav.entries, [r"D:\Other".to_string()]);
        assert_eq!(nav.cursor(), 0);
        assert!(nav.at_root());
    }

    #[test]
    fn invariants_hold_under_10000_random_operations() {
        let mut state = 0x2545_F491u32;
        let mut next = move |n: u32| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state % n
        };
        let mut nav = GateNav::new(ROOT, true);
        for _ in 0..10_000 {
            match next(11) {
                0..=3 => {
                    let depth = 1 + next(3);
                    let mut p = ROOT.to_string();
                    for _ in 0..depth {
                        p.push_str(&format!(r"\d{}", next(4)));
                    }
                    nav.visit(&p);
                }
                4 => {
                    nav.visit(r"C:\Outside");
                }
                5 => {
                    let _ = nav.back();
                }
                6 => {
                    let _ = nav.forward();
                }
                7 => {
                    let _ = nav.up();
                }
                8 => {
                    let _ = nav.to_root();
                }
                9 => {
                    let step = if next(2) == 0 { nav.back() } else { nav.forward() };
                    if let Some(step) = step {
                        nav.step_failed(&step);
                    }
                }
                _ => {
                    let _ = nav.set_allow(next(2) == 0);
                }
            }
            assert_invariants(&nav);
        }
    }

    #[test]
    fn the_stored_shape_is_newest_first_and_empty_at_a_fresh_root() {
        let mut nav = GateNav::new(ROOT, true);
        assert!(nav.to_stored().is_empty());
        nav.visit(&sub(1));
        nav.visit(&sub(2));
        assert_eq!(nav.to_stored(), [sub(2), sub(1), ROOT.to_string()]);
        let again = GateNav::from_stored(ROOT, &nav.to_stored(), true);
        assert_eq!(again.entries, nav.entries);
        assert_eq!(again.current(), sub(2));
        // forward entries are not stored: the trail restarts at the cursor
        nav.back();
        assert_eq!(nav.to_stored(), [sub(1), ROOT.to_string()]);
    }

    #[test]
    fn from_stored_drops_outsiders_repeats_and_trims_around_the_cursor() {
        let stored = vec![sub(2), r"C:\Outside".to_string(), sub(2).to_lowercase(), sub(1), ROOT.to_string()];
        let nav = GateNav::from_stored(ROOT, &stored, true);
        let keys: Vec<String> = nav.entries.iter().map(|e| e.to_lowercase()).collect();
        assert_eq!(keys, [ROOT.to_lowercase(), sub(1).to_lowercase(), sub(2).to_lowercase()]);
        assert_eq!(GateNav::from_stored(ROOT, &[], true).entries, [ROOT.to_string()]);
        assert_eq!(GateNav::from_stored(ROOT, &[r"D:\Gone".to_string()], true).entries, [ROOT.to_string()]);
        let long: Vec<String> = (1..=30).rev().map(sub).collect();
        let nav = GateNav::from_stored(ROOT, &long, true);
        assert_eq!(nav.len(), HISTORY_CAP);
        assert_eq!(nav.current(), sub(30));
        assert_eq!(nav.entries[0], sub(11));
        assert_invariants(&nav);
    }

    // test 4
    #[test]
    fn decide_nav_keeps_the_gate_inside_its_root() {
        let mut nav = GateNav::new(ROOT, true);
        nav.visit(&sub(1));
        for origin in [NavOrigin::Corral, NavOrigin::View] {
            assert_eq!(decide_nav(&nav, Some(r"C:\Elsewhere"), origin, false), NavDecision::OpenExternally);
            assert_eq!(decide_nav(&nav, Some(r"C:\Data\Docs2"), origin, false), NavDecision::OpenExternally);
            assert_eq!(decide_nav(&nav, None, origin, false), NavDecision::OpenExternally, "no file-system path");
        }
        assert_eq!(decide_nav(&nav, Some(&sub(2)), NavOrigin::Corral, false), NavDecision::Allow);
        assert_eq!(decide_nav(&nav, Some(&sub(2)), NavOrigin::View, false), NavDecision::Allow);
    }

    #[test]
    fn decide_nav_with_navigation_off_sends_view_folders_out_except_the_current_one() {
        let mut nav = GateNav::new(ROOT, false);
        assert_eq!(decide_nav(&nav, Some(&sub(1)), NavOrigin::View, false), NavDecision::OpenExternally);
        assert_eq!(decide_nav(&nav, Some(ROOT), NavOrigin::View, false), NavDecision::Allow, "the folder shown");
        assert_eq!(decide_nav(&nav, Some(&ROOT.to_uppercase()), NavOrigin::View, false), NavDecision::Allow);
        assert_eq!(decide_nav(&nav, Some(&sub(1)), NavOrigin::Corral, false), NavDecision::Allow, "Corral's own navigation is never judged as the view's");
        nav.set_allow(true);
        assert_eq!(decide_nav(&nav, Some(&sub(1)), NavOrigin::View, false), NavDecision::Allow);
    }

    #[test]
    fn decide_nav_refuses_while_the_gate_is_going_away() {
        let nav = GateNav::new(ROOT, true);
        assert_eq!(decide_nav(&nav, Some(&sub(1)), NavOrigin::View, true), NavDecision::Cancel);
    }

    // test 5
    #[test]
    fn resolve_start_browses_an_existing_current_unchanged() {
        let mut nav = GateNav::new(ROOT, true);
        nav.visit(&sub(1));
        assert_eq!(resolve_start(&nav, |_| true), Start::Browse { path: sub(1), replaced: false });
    }

    #[test]
    fn resolve_start_climbs_to_the_nearest_existing_ancestor_inside_the_root() {
        let deep = format!(r"{ROOT}\a\b\c");
        let mut nav = GateNav::new(ROOT, true);
        nav.visit(&deep);
        let present = |p: &str| p.eq_ignore_ascii_case(ROOT) || p.eq_ignore_ascii_case(&format!(r"{ROOT}\a"));
        assert_eq!(resolve_start(&nav, present), Start::Browse { path: format!(r"{ROOT}\a"), replaced: true });
        assert_eq!(resolve_start(&nav, |p| p.eq_ignore_ascii_case(ROOT)), Start::Browse { path: ROOT.to_string(), replaced: true });
    }

    #[test]
    fn resolve_start_with_the_root_gone_errors_and_leaves_the_state_alone() {
        let mut nav = GateNav::new(ROOT, true);
        nav.visit(&sub(1));
        let before = nav.clone();
        assert_eq!(resolve_start(&nav, |_| false), Start::Error);
        assert_eq!(nav, before);
        assert_eq!(nav.to_stored(), before.to_stored(), "loading never rewrites the file");
        // a root that exists while the current is gone still browses (the root is the ceiling)
        assert_eq!(resolve_start(&nav, |p| p == ROOT), Start::Browse { path: ROOT.to_string(), replaced: true });
    }

    // test 6
    #[test]
    fn classify_probe_covers_the_table() {
        for code in [2, 3, 267] {
            assert_eq!(classify_probe(Some(code)), Probe::Missing);
        }
        for code in [21, 53, 59, 64, 67, 121, 1167, 1201, 1222, 1231, 2250] {
            assert_eq!(classify_probe(Some(code)), Probe::Offline);
        }
        for code in [5, 1219, 1326] {
            assert_eq!(classify_probe(Some(code)), Probe::Denied);
        }
        assert_eq!(classify_probe(None), Probe::Offline, "timeout");
        assert_eq!(classify_probe(Some(0)), Probe::Ok);
        assert_eq!(classify_probe(Some(1784)), Probe::Other(1784));
    }

    #[test]
    fn error_text_matches_the_spec_table() {
        assert_eq!(error_text(Probe::Missing).unwrap().0, "This folder isn't here");
        assert_eq!(error_text(Probe::Missing).unwrap().1, "It may have been moved, renamed or deleted.");
        assert_eq!(error_text(Probe::Offline).unwrap().0, "Can't reach this folder");
        assert_eq!(error_text(Probe::Offline).unwrap().1, "Check the network connection or the drive, then try again.");
        assert_eq!(error_text(Probe::Denied).unwrap().0, "No access");
        assert_eq!(error_text(Probe::Denied).unwrap().1, "You don't have permission to open this folder.");
        let other = error_text(Probe::Other(0x80070057)).unwrap();
        assert_eq!(other.0, "Can't open this folder");
        assert_eq!(other.1, "Windows reported error 0x80070057.");
        assert_eq!(error_text(Probe::Other(5000)).unwrap().1, "Windows reported error 0x00001388.");
        assert!(error_text(Probe::Ok).is_none());
    }

    #[test]
    fn only_offline_retries_by_itself() {
        assert_eq!(auto_retry(Probe::Offline), Some(Duration::from_secs(60)));
        for probe in [Probe::Missing, Probe::Denied, Probe::Other(1), Probe::Ok] {
            assert_eq!(auto_retry(probe), None);
        }
        assert_eq!(PROBE_LIMIT, Duration::from_millis(3000));
    }
}
