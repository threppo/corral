//! Creating Corrals and Folder Gates (creation-spec): when a press may start a draw and how the draw
//! gesture ends, the chooser, which dropped items become Gates on an Alt-drop, and where a quick Gate
//! goes. Sizing a drawn rectangle is grid-snapping-spec 6. Pure functions so the window layer stays thin
//! and the rules are unit-tested.

use crate::geometry::Rect;
use crate::icon_metrics::{dip_to_px, CellMetrics};
use crate::snap::{self, EdgeZone, SnapLock, SnapSettings};

/// `CreateCorralWithMouseMode` (default 0): which mouse button draws a Corral.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    Both = 0,
    LeftOnly = 1,
    RightOnly = 2,
}

impl MouseMode {
    pub fn from_value(value: u32) -> Self {
        match value {
            1 => MouseMode::LeftOnly,
            2 => MouseMode::RightOnly,
            _ => MouseMode::Both,
        }
    }
}

/// Title given to every new Corral.
pub const DEFAULT_TITLE: &str = "New Corral";

/// Smallest drawn rectangle that still creates a Corral: a click or a slip under this on both axes
/// cancels (grid-snapping-spec 6).
pub const CANCEL_UNDER_DIP: f64 = 16.0;

pub struct CreationInput<'a> {
    /// Press point (the anchor) and current pointer, in the same pixel space as `work`.
    pub anchor: (i32, i32),
    pub pointer: (i32, i32),
    /// Work area of the monitor under the press point; a drag over other monitors is clipped to it.
    pub work: Rect,
    pub dpi: u32,
    /// Title strip on the side the Corral will use (above or below), in px.
    pub strip_top: i32,
    pub strip_bottom: i32,
    pub cell: CellMetrics,
    pub settings: SnapSettings,
    pub shift: bool,
    pub neighbours: &'a [Rect],
    pub lock: SnapLock,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreationRect {
    /// The rectangle the preview shows (body plus title strip).
    pub outer: Rect,
    /// The stored body.
    pub body: Rect,
    pub lock: SnapLock,
    pub guides: Vec<snap::Guide>,
}

/// The rectangle a drawn drag creates (grid-snapping-spec 6): the press point stays fixed, the opposite
/// corner follows the pointer; the size is `max(drawn, minimum)` per axis, growing away from the anchor,
/// rounded to whole cells when that is on, else snapped like a resize; Shift gives the raw result.
/// `None` when the drag is a click or a slip.
pub fn creation_rect(inp: &CreationInput) -> Option<CreationRect> {
    let clip = |p: (i32, i32)| (p.0.clamp(inp.work.left, inp.work.right), p.1.clamp(inp.work.top, inp.work.bottom));
    let (a, p) = (clip(inp.anchor), clip(inp.pointer));
    let slip = dip_to_px(CANCEL_UNDER_DIP, inp.dpi);
    if (p.0 - a.0).abs() < slip && (p.1 - a.1).abs() < slip {
        return None;
    }
    let down = p.1 >= a.1;
    let right = p.0 >= a.0;
    // The anchor is an outer corner: the body starts one title strip in on the side facing the anchor.
    let anchor_body_y = if down { a.1 + inp.strip_top } else { a.1 - inp.strip_bottom };
    let target_body_y = if down { p.1 - inp.strip_bottom } else { p.1 + inp.strip_top };
    let zone = (if right { EdgeZone::RIGHT } else { EdgeZone::LEFT }) | (if down { EdgeZone::BOTTOM } else { EdgeZone::TOP });
    let out = snap::resize_rect(&snap::ResizeInput {
        zone,
        start: Rect::new(a.0, anchor_body_y, a.0, anchor_body_y),
        strip_top: inp.strip_top,
        strip_bottom: inp.strip_bottom,
        start_pointer: (0, 0),
        pointer: (p.0 - a.0, target_body_y - anchor_body_y),
        dpi: inp.dpi,
        work: inp.work,
        cell: inp.cell,
        settings: inp.settings,
        shift: inp.shift,
        neighbours: inp.neighbours,
        lock: inp.lock,
        min: Some(snap::min_body_px(inp.dpi)),
    });
    let body = out.rect;
    Some(CreationRect { outer: Rect::new(body.left, body.top - inp.strip_top, body.right, body.bottom + inp.strip_bottom), body, lock: out.lock, guides: out.guides })
}

// ---------------------------------------------------------------------------------------------
// Drawing a rectangle (creation-spec 2)
// ---------------------------------------------------------------------------------------------

/// Touch and pen input carries this signature in `GetMessageExtraInfo()`; it never starts a draw
/// (creation-spec 2.1 rule 1).
const TOUCH_SIGNATURE_MASK: u32 = 0xFFFF_FF00;
const TOUCH_SIGNATURE: u32 = 0xFF51_5700;

/// True when the message came from touch or pen (`GetMessageExtraInfo` value).
pub fn is_touch_or_pen(extra_info: isize) -> bool {
    (extra_info as u32) & TOUCH_SIGNATURE_MASK == TOUCH_SIGNATURE
}

/// The mouse button of a press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawButton {
    Left,
    Right,
    Middle,
}

impl MouseMode {
    /// Whether `CreateCorralMode` lets this button draw; the middle button never does (creation-spec 2.1 rule 2).
    pub fn allows(self, button: DrawButton) -> bool {
        match button {
            DrawButton::Left => self != MouseMode::RightOnly,
            DrawButton::Right => self != MouseMode::LeftOnly,
            DrawButton::Middle => false,
        }
    }
}

/// Everything creation-spec 2.1 looks at when a button goes down.
#[derive(Clone, Copy, Debug)]
pub struct PressFacts {
    pub button: DrawButton,
    pub mode: MouseMode,
    pub touch_or_pen: bool,
    /// No Corral that takes input and no desktop item is under the pointer.
    pub on_empty_desktop: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub win: bool,
    /// Any other mouse button is already down.
    pub other_button_down: bool,
    pub clear_view: bool,
    pub range_transition: bool,
}

/// May this press become a pending draw (creation-spec 2.1)?
pub fn qualifies_press(f: &PressFacts) -> bool {
    !f.touch_or_pen
        && f.mode.allows(f.button)
        && f.on_empty_desktop
        && !(f.shift || f.ctrl || f.alt || f.win)
        && !f.other_button_down
        && !f.clear_view
        && !f.range_transition
}

/// Smallest pointer travel in px that arms a draw: the larger of 16 DIP and the system drag size
/// (creation-spec 2.2).
pub fn arm_px(dpi: u32, system_drag: (i32, i32)) -> i32 {
    dip_to_px(CANCEL_UNDER_DIP, dpi).max(system_drag.0).max(system_drag.1).max(1)
}

/// The state of a draw gesture (creation-spec 2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawState {
    Idle,
    Pending { button: DrawButton, start: (i32, i32) },
    Drawing { button: DrawButton, start: (i32, i32) },
}

/// Why a gesture ends without creating anything and without a click (creation-spec 2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelReason {
    OtherButton,
    Escape,
    CancelMode,
    CaptureLost,
    DisplayChange,
    DpiChange,
    ClearView,
    RangeChange,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawEvent {
    /// A button went down; `qualifies` is [`qualifies_press`].
    Press { button: DrawButton, at: (i32, i32), qualifies: bool },
    /// The pointer moved; `arm_px` is [`arm_px`] at the press monitor's DPI.
    Move { at: (i32, i32), arm_px: i32 },
    /// The drawing button went up; `rect_valid` = `creation_rect(..).is_some()` for the final pointer.
    Release { rect_valid: bool },
    Cancel(CancelReason),
}

/// What the host does after a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrawEffect {
    /// Nothing to do.
    None,
    /// The drag armed: show the preview.
    Armed,
    /// Recompute and show the preview.
    Preview,
    /// The press-release was an ordinary click (desktop click, double-click counting, and for the right
    /// button the desktop context menu).
    Click,
    /// The gesture ended without a result and without a click.
    Cancelled,
    /// A drag that is too small ended: nothing is created and the right button's menu stays away.
    Suppressed,
    /// Open the chooser for the previewed rectangle.
    Chooser,
}

/// The pure reducer of creation-spec 2.2.
pub fn draw_step(state: DrawState, event: DrawEvent) -> (DrawState, DrawEffect) {
    use DrawEffect as E;
    use DrawState as S;
    match (state, event) {
        (S::Idle, DrawEvent::Press { button, at, qualifies: true }) => (S::Pending { button, start: at }, E::None),
        (S::Idle, _) => (S::Idle, E::None),
        // a second press, or any cancel reason, ends a live gesture silently
        (_, DrawEvent::Press { .. }) | (_, DrawEvent::Cancel(_)) => (S::Idle, E::Cancelled),
        (S::Pending { button, start }, DrawEvent::Move { at, arm_px }) => {
            let travel = (at.0 - start.0).abs().max((at.1 - start.1).abs());
            if travel >= arm_px.max(1) { (S::Drawing { button, start }, E::Armed) } else { (state, E::None) }
        }
        (S::Pending { .. }, DrawEvent::Release { .. }) => (S::Idle, E::Click),
        (S::Drawing { .. }, DrawEvent::Move { .. }) => (state, E::Preview),
        (S::Drawing { .. }, DrawEvent::Release { rect_valid: false }) => (S::Idle, E::Suppressed),
        (S::Drawing { .. }, DrawEvent::Release { rect_valid: true }) => (S::Idle, E::Chooser),
    }
}

/// A body rect in work-area-relative DIPs, the way a created Corral stores it (creation-spec 7).
pub fn body_to_dip(body: Rect, work: Rect, dpi: u32) -> Rect {
    use crate::geometry::to_dip;
    let (left, top) = (body.left - work.left, body.top - work.top);
    Rect::new(to_dip(left, dpi), to_dip(top, dpi), to_dip(left + body.width(), dpi), to_dip(top + body.height(), dpi))
}

// ---------------------------------------------------------------------------------------------
// The chooser (creation-spec 3)
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChooserChoice {
    Corral,
    FolderGate,
}

/// The two items, in order, as menu text with the mnemonic marked (C and F).
pub const CHOOSER_ITEMS: [(ChooserChoice, &str); 2] = [
    (ChooserChoice::Corral, "&Create Corral here"),
    (ChooserChoice::FolderGate, "Create &Folder Gate here"),
];

/// Distance of the chooser from the release point (creation-spec 3).
pub const CHOOSER_OFFSET_DIP: f64 = 4.0;
/// Item height and the width the menu is planned with when its real size is not known yet.
const CHOOSER_ITEM_DIP: f64 = 32.0;
const CHOOSER_WIDTH_DIP: f64 = 240.0;
const CHOOSER_PAD_DIP: f64 = 8.0;

/// Where the chooser opens: the anchor point and which sides it hangs from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChooserPlacement {
    pub at: (i32, i32),
    /// The menu's right edge sits at `at.0` (flipped at the right work-area edge).
    pub align_right: bool,
    /// The menu's bottom edge sits at `at.1` (flipped at the bottom work-area edge).
    pub align_bottom: bool,
}

/// Planned size of the chooser in px at `dpi`.
pub fn chooser_size_px(dpi: u32) -> (i32, i32) {
    (dip_to_px(CHOOSER_WIDTH_DIP, dpi), dip_to_px(2.0 * CHOOSER_ITEM_DIP + CHOOSER_PAD_DIP, dpi))
}

/// Top-left 4 DIP right of and below `release`, flipped to stay inside `work` (creation-spec 3).
pub fn chooser_placement(release: (i32, i32), dpi: u32, work: Rect) -> ChooserPlacement {
    let (w, h) = chooser_size_px(dpi);
    let off = dip_to_px(CHOOSER_OFFSET_DIP, dpi);
    let align_right = release.0 + off + w > work.right;
    let align_bottom = release.1 + off + h > work.bottom;
    ChooserPlacement {
        at: (if align_right { release.0 - off } else { release.0 + off }, if align_bottom { release.1 - off } else { release.1 + off }),
        align_right,
        align_bottom,
    }
}

// ---------------------------------------------------------------------------------------------
// Alt-drop (creation-spec 5)
// ---------------------------------------------------------------------------------------------

pub const MK_LBUTTON: u32 = 0x01;
pub const MK_RBUTTON: u32 = 0x02;
pub const MK_SHIFT: u32 = 0x04;
pub const MK_CONTROL: u32 = 0x08;
pub const MK_MBUTTON: u32 = 0x10;
pub const MK_ALT: u32 = 0x20;

/// More folders than this in one drop are refused (creation-spec 5.4).
pub const MAX_FOLDERS_PER_DROP: usize = 6;
pub const TOO_MANY_TIP: &str = "Drop up to 6 folders at once";

/// The drop description while an Alt-drop would be accepted (creation-spec 5.5).
pub fn alt_drop_tip(count: usize) -> String {
    if count == 1 { "Create Folder Gate here".to_string() } else { format!("Create {count} Folder Gates here") }
}

/// The key state the drop target is given plus the side-specific keys that tell AltGr from Ctrl+Alt.
#[derive(Clone, Copy, Debug, Default)]
pub struct DropKeys {
    /// `grfKeyState`; the mouse buttons come from the last `DragOver` because `Drop` carries none.
    pub mk: u32,
    pub lmenu: bool,
    pub rmenu: bool,
    pub lctrl: bool,
    pub rctrl: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DropMods {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
}

/// Modifiers of a drop. AltGr counts as Alt: the left Ctrl a layout injects with Right Alt is ignored
/// (creation-spec 5.2).
pub fn drop_modifiers(k: &DropKeys) -> DropMods {
    let altgr_ctrl = k.rmenu && !k.lmenu && k.lctrl && !k.rctrl;
    DropMods { alt: k.mk & MK_ALT != 0 || k.lmenu || k.rmenu, ctrl: !altgr_ctrl && k.mk & MK_CONTROL != 0, shift: k.mk & MK_SHIFT != 0 }
}

/// Where the drop point is (creation-spec 5.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AltTarget {
    /// Empty desktop, a Corral's title strip or a Corral's empty body area.
    Empty,
    /// A desktop folder icon, a Corral item that accepts drops, or a Gate's hosted view.
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AltDrop {
    /// The ordinary handler gets the drop unchanged.
    Ordinary,
    /// Create this many Gates; the effect is `DROPEFFECT_LINK`.
    Accept(usize),
    /// More than [`MAX_FOLDERS_PER_DROP`] folders: refused with `DROPEFFECT_NONE`.
    TooMany,
    /// A clear Alt-drop that cannot be carried out: refused with `DROPEFFECT_NONE`, never handed to the
    /// Shell.
    Refused(Refusal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The source does not allow `DROPEFFECT_LINK`.
    NoLink,
    /// The classifier had not finished when the items were dropped.
    NotClassified,
}

/// Drop tip when the source cannot take part in an Alt-drop.
pub const NO_LINK_TIP: &str = "This drag can't create Folder Gates";

/// The Alt-drop decision (creation-spec 5.2 to 5.4). `buttons` is the `MK_*` button state of the drag;
/// `folders` is the classifier's result, `None` while it has not finished; `allows_link` says whether the
/// source allows `DROPEFFECT_LINK`; `dropping` is true at `Drop`, where an unfinished classification is
/// refused instead of showing the ordinary feedback. The intent (left button, Alt alone, empty target) is
/// what makes a refusal: without it the ordinary handler always gets the drop.
pub fn alt_drop_decision(keys: &DropKeys, buttons: u32, folders: Option<&[String]>, target: AltTarget, allows_link: bool, dropping: bool) -> AltDrop {
    let mods = drop_modifiers(keys);
    let left_only = buttons & MK_LBUTTON != 0 && buttons & (MK_RBUTTON | MK_MBUTTON) == 0;
    if !(left_only && mods.alt && !mods.ctrl && !mods.shift) || target != AltTarget::Empty {
        return AltDrop::Ordinary;
    }
    match folders {
        None if dropping => AltDrop::Refused(Refusal::NotClassified),
        None | Some([]) => AltDrop::Ordinary,
        Some(list) if list.len() > MAX_FOLDERS_PER_DROP => AltDrop::TooMany,
        Some(_) if !allows_link => AltDrop::Refused(Refusal::NoLink),
        Some(list) => AltDrop::Accept(list.len()),
    }
}

// ---- which dropped items count (creation-spec 5.3) ----

/// A file-system path that can be a Gate root: absolute, not a shell-only name.
fn is_filesystem_path(path: &str) -> bool {
    let b = path.as_bytes();
    !path.starts_with("::") && (path.starts_with("\\\\") || (b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic()))
}

/// Identity of a folder path for de-duplication: case-insensitive, one separator style, no trailing
/// separator (a drive root keeps it).
fn folder_key(path: &str) -> String {
    let mut key = path.replace('/', "\\").to_lowercase();
    while key.len() > 3 && key.ends_with('\\') {
        key.pop();
    }
    key
}

/// The folders among `paths`, in drop order, without duplicates (creation-spec 5.3 and 5.4). `is_folder`
/// answers from read-only attributes: a folder, not a .zip or other file, with a file-system path.
pub fn classify_folders(paths: &[String], is_folder: impl Fn(&str) -> bool) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    paths
        .iter()
        .filter(|p| is_filesystem_path(p) && is_folder(p))
        .filter(|p| seen.insert(folder_key(p)))
        .cloned()
        .collect()
}

/// Read-only folder test: file-system directories count, including drive roots, UNC directories, mapped
/// drives and junctions; files (.zip, .lnk, .library-ms) and missing paths do not.
pub fn fs_is_folder(path: &str) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_dir())
}

/// Time the classifier gets (creation-spec 5.3).
pub const CLASSIFY_BUDGET: std::time::Duration = std::time::Duration::from_millis(1000);

type ClassifySlot = std::sync::Arc<std::sync::Mutex<Option<(Vec<String>, std::time::Duration)>>>;
type ClassifyJob = Box<dyn FnOnce() + Send>;

/// The shared worker: one thread takes the jobs of all drags in turn instead of one thread per drag.
/// A worker that has been busy longer than the budget (a dead network path) is left to finish alone and
/// a fresh one takes the next jobs.
struct ClassifyWorker {
    jobs: std::sync::mpsc::Sender<ClassifyJob>,
    busy_since: std::sync::Arc<std::sync::Mutex<Option<std::time::Instant>>>,
}

impl ClassifyWorker {
    fn start() -> Option<Self> {
        let (jobs, queue) = std::sync::mpsc::channel::<ClassifyJob>();
        let busy_since = std::sync::Arc::new(std::sync::Mutex::new(None));
        let busy = busy_since.clone();
        std::thread::Builder::new().name("corral-folder-classifier".into()).spawn(move || {
            while let Ok(job) = queue.recv() {
                if let Ok(mut guard) = busy.lock() { *guard = Some(std::time::Instant::now()); }
                job();
                if let Ok(mut guard) = busy.lock() { *guard = None; }
            }
        }).ok()?;
        Some(Self { jobs, busy_since })
    }

    fn stuck(&self, budget: std::time::Duration) -> bool {
        self.busy_since.lock().ok().and_then(|g| *g).is_some_and(|since| since.elapsed() > budget)
    }
}

static CLASSIFY_WORKER: std::sync::Mutex<Option<ClassifyWorker>> = std::sync::Mutex::new(None);

/// Classification on the shared worker thread so a dead network path cannot freeze the desktop. A result
/// that arrives after the budget is never used.
pub struct FolderClassifier {
    slot: ClassifySlot,
}

impl FolderClassifier {
    pub fn spawn(paths: Vec<String>, is_folder: impl Fn(&str) -> bool + Send + 'static) -> Self {
        Self::spawn_with_budget(CLASSIFY_BUDGET, paths, is_folder)
    }

    fn spawn_with_budget(budget: std::time::Duration, paths: Vec<String>, is_folder: impl Fn(&str) -> bool + Send + 'static) -> Self {
        let slot: ClassifySlot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let started = std::time::Instant::now();
        let mine = slot.clone();
        let job: ClassifyJob = Box::new(move || {
            // nobody waits for this drag any more: skip the file access
            if std::sync::Arc::strong_count(&mine) == 1 { return; }
            let folders = classify_folders(&paths, is_folder);
            if let Ok(mut guard) = mine.lock() {
                *guard = Some((folders, started.elapsed()));
            }
        });
        if let Ok(mut worker) = CLASSIFY_WORKER.lock() {
            if worker.as_ref().is_none_or(|w| w.stuck(budget)) {
                *worker = ClassifyWorker::start();
            }
            // no worker, or it is gone: the slot stays empty and the drop is never classified
            if let Some(w) = worker.as_ref() { let _ = w.jobs.send(job); }
        }
        Self { slot }
    }

    /// The folders once classification finished within [`CLASSIFY_BUDGET`]; `None` before that.
    pub fn result(&self) -> Option<Vec<String>> {
        let guard = self.slot.lock().ok()?;
        guard.as_ref().filter(|(_, took)| *took <= CLASSIFY_BUDGET).map(|(folders, _)| folders.clone())
    }

    /// A classifier that has already finished, for tests of the code that consumes it.
    #[cfg(test)]
    pub fn finished(folders: Vec<String>) -> Self {
        Self { slot: std::sync::Arc::new(std::sync::Mutex::new(Some((folders, std::time::Duration::ZERO)))) }
    }

    /// A classifier that never finishes, for tests.
    #[cfg(test)]
    pub fn pending() -> Self {
        Self { slot: std::sync::Arc::new(std::sync::Mutex::new(None)) }
    }
}

/// Whether an item the app already knows (its Shell attributes) is a folder that can be a Gate root:
/// a folder, not a stream folder (.zip) and not a shortcut, with a file-system path (creation-spec 5.3).
pub fn known_item_is_folder(path: &str, is_folder: bool, is_link: bool) -> bool {
    is_folder && !is_link && is_filesystem_path(path) && !path.to_lowercase().ends_with(".library-ms")
}

// ---------------------------------------------------------------------------------------------
// Quick Folder Gate size and position (creation-spec 6)
// ---------------------------------------------------------------------------------------------

/// Default Gate body in whole cells (creation-spec 6.1).
pub const QUICK_GATE_CELLS: (i32, i32) = (5, 4);
/// A batch reads as separate Gates even with the gap toggle off (creation-spec 6.2).
pub const QUICK_GATE_MIN_GAP_DIP: f64 = 4.0;

pub struct QuickGateInput<'a> {
    /// Drop point in screen px.
    pub drop: (i32, i32),
    pub count: usize,
    /// Work areas of all monitors; `governing` is the one under the drop point.
    pub works: &'a [Rect],
    pub governing: usize,
    pub dpi: u32,
    /// Title strip height in px (0 when titles are hidden).
    pub strip_px: i32,
    pub cell: CellMetrics,
    pub settings: SnapSettings,
    /// Outer rects of the Corrals the move snapping may align to.
    pub neighbours: &'a [Rect],
}

/// Outer rects (screen px, body plus title strip) of the Gates an Alt-drop creates (creation-spec 6.2).
pub fn quick_gate_rects(inp: &QuickGateInput) -> Vec<Rect> {
    let Some(work) = inp.works.get(inp.governing).copied() else { return Vec::new() };
    let count = inp.count.min(MAX_FOLDERS_PER_DROP);
    if count == 0 {
        return Vec::new();
    }
    let (body_w, body_h) = inp.cell.body_size_for(QUICK_GATE_CELLS.0, QUICK_GATE_CELLS.1);
    let min = snap::min_body_px(inp.dpi);
    let (w, h) = (body_w.min(work.width()).max(min.0), (body_h + inp.strip_px).min(work.height()).max(min.1 + inp.strip_px));
    // the pointer rests on the title strip
    let raw = Rect::new(inp.drop.0 - w / 2, inp.drop.1 - inp.strip_px / 2, inp.drop.0 - w / 2 + w, inp.drop.1 - inp.strip_px / 2 + h);
    let snapped = snap::snap_move(&snap::MoveInput {
        raw, work, neighbours: inp.neighbours, settings: inp.settings, dpi: inp.dpi, shift: false, lock: SnapLock::default(),
    });
    let first = snap::place_on_screen(snapped.rect, inp.works, Some(inp.governing), Some(inp.drop)).rect;
    let gap = inp.settings.gap_px(inp.dpi).max(dip_to_px(QUICK_GATE_MIN_GAP_DIP, inp.dpi));
    let (tile_w, tile_h) = (first.width(), first.height());
    let mut out = vec![first];
    let (mut x, mut y) = (first.right + gap, first.top);
    for _ in 1..count {
        if x + tile_w > work.right && x > first.left {
            x = first.left;
            y += tile_h + gap;
        }
        let tile = Rect::new(x, y, x + tile_w, y + tile_h);
        out.push(snap::place_on_screen(tile, inp.works, Some(inp.governing), None).rect);
        x += tile_w + gap;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- grid-snapping-spec 6 (creation) ----

    const DPIS: [u32; 5] = [96, 120, 144, 192, 240];

    fn cell(dpi: u32) -> CellMetrics {
        crate::icon_metrics::cell_metrics(&crate::icon_metrics::CellInputs { dpi, zoom_pct: 100, windows_spacing_dip: Some((75.0, 75.0)), desktop_icon_dip: Some(48.0), match_desktop: true })
    }

    fn input<'a>(dpi: u32, anchor: (i32, i32), pointer: (i32, i32), settings: SnapSettings, neighbours: &'a [Rect]) -> CreationInput<'a> {
        CreationInput {
            anchor, pointer, work: Rect::new(0, 0, 3840, 2160), dpi, strip_top: dip_to_px(24.0, dpi), strip_bottom: 0, cell: cell(dpi),
            settings, shift: false, neighbours, lock: SnapLock::default(),
        }
    }

    fn off() -> SnapSettings {
        SnapSettings::new(false, 0, true, true, false)
    }

    #[test]
    fn a_click_and_a_15_dip_slip_cancel_and_17_dip_creates_at_the_minimum() {
        for dpi in DPIS {
            let d = |dip: f64| dip_to_px(dip, dpi);
            assert!(creation_rect(&input(dpi, (500, 500), (500, 500), off(), &[])).is_none(), "click");
            assert!(creation_rect(&input(dpi, (500, 500), (500 + d(15.0) - 1, 500 + d(15.0) - 1), off(), &[])).is_none(), "slip at dpi {dpi}");
            let r = creation_rect(&input(dpi, (500, 500), (500 + d(17.0), 500 + d(17.0)), off(), &[])).expect("17 DIP creates");
            let min = snap::min_body_px(dpi);
            assert_eq!((r.body.width(), r.body.height()), min, "created at the minimum, dpi {dpi}");
            // one long axis is enough
            assert!(creation_rect(&input(dpi, (500, 500), (500 + d(40.0), 500 + 2), off(), &[])).is_some());
        }
    }

    #[test]
    fn the_anchor_stays_fixed_in_every_direction_and_the_outer_rect_is_what_was_drawn() {
        for dpi in DPIS {
            let strip = dip_to_px(24.0, dpi);
            let (w, h) = (dip_to_px(300.0, dpi), dip_to_px(200.0, dpi));
            let a = (1000, 800);
            for (sx, sy) in [(1, 1), (-1, 1), (1, -1), (-1, -1)] {
                let r = creation_rect(&input(dpi, a, (a.0 + sx * w, a.1 + sy * h), off(), &[])).unwrap();
                let tag = format!("dpi {dpi} dir ({sx},{sy})");
                assert_eq!((r.outer.width(), r.outer.height()), (w, h), "{tag}");
                assert_eq!(r.body, Rect::new(r.outer.left, r.outer.top + strip, r.outer.right, r.outer.bottom), "{tag}");
                if sx > 0 { assert_eq!(r.outer.left, a.0, "{tag}"); } else { assert_eq!(r.outer.right, a.0, "{tag}"); }
                if sy > 0 { assert_eq!(r.outer.top, a.1, "{tag}"); } else { assert_eq!(r.outer.bottom, a.1, "{tag}"); }
            }
        }
    }

    #[test]
    fn whole_cell_creation_rounds_each_axis_to_cells_with_the_anchor_fixed() {
        for dpi in DPIS {
            let c = cell(dpi);
            let s = SnapSettings::new(true, 0, true, false, true);
            let (bw, bh) = c.body_size_for(4, 3);
            let r = creation_rect(&input(dpi, (1000, 800), (1000 + bw + 3, 800 + dip_to_px(24.0, dpi) + bh - 2), s, &[])).unwrap();
            assert_eq!((r.body.width(), r.body.height()), (bw, bh), "dpi {dpi}");
            assert_eq!(r.outer.left, 1000);
            assert_eq!(r.outer.top, 800);
            // dragging up-left keeps the bottom-right anchor
            let r = creation_rect(&input(dpi, (2000, 1500), (2000 - bw - 3, 1500 - bh - 2), s, &[])).unwrap();
            assert_eq!((r.body.width(), r.body.height()), (bw, bh));
            assert_eq!((r.outer.right, r.outer.bottom), (2000, 1500));
            // Shift gives the raw rectangle
            let mut raw = input(dpi, (1000, 800), (1000 + bw + 3, 800 + dip_to_px(24.0, dpi) + bh - 2), s, &[]);
            raw.shift = true;
            let r = creation_rect(&raw).unwrap();
            assert_eq!((r.body.width(), r.body.height()), (bw + 3, bh - 2));
        }
    }

    #[test]
    fn creation_is_clipped_to_the_press_monitor_and_snaps_without_whole_cells() {
        let dpi = 96;
        let mut i = input(dpi, (1800, 500), (5000, 3000), off(), &[]);
        i.work = Rect::new(0, 0, 1920, 1040);
        let r = creation_rect(&i).unwrap();
        assert_eq!((r.outer.right, r.outer.bottom), (1920, 1040), "clipped, never split");
        // edge snapping like a resize when whole cells are off
        let n = [Rect::new(1000, 300, 1400, 600)];
        let snapped = creation_rect(&input(dpi, (700, 350), (1000 - 8 - 3, 600), SnapSettings::new(true, 8, true, false, false), &n)).unwrap();
        assert_eq!(snapped.outer.right, 1000 - 8);
        assert!(snapped.guides.is_empty());
        // the stored size is the created size
        assert_eq!(snapped.body.width(), snapped.outer.width());
    }

    // ---- creation-spec 8 ----

    fn facts() -> PressFacts {
        PressFacts {
            button: DrawButton::Left, mode: MouseMode::Both, touch_or_pen: false, on_empty_desktop: true,
            shift: false, ctrl: false, alt: false, win: false, other_button_down: false, clear_view: false, range_transition: false,
        }
    }

    #[test]
    fn qualifies_press_follows_the_spec_rules() {
        assert!(qualifies_press(&facts()));
        for (mode, button, ok) in [
            (MouseMode::Both, DrawButton::Left, true), (MouseMode::Both, DrawButton::Right, true),
            (MouseMode::LeftOnly, DrawButton::Left, true), (MouseMode::LeftOnly, DrawButton::Right, false),
            (MouseMode::RightOnly, DrawButton::Left, false), (MouseMode::RightOnly, DrawButton::Right, true),
            (MouseMode::Both, DrawButton::Middle, false), (MouseMode::LeftOnly, DrawButton::Middle, false),
            (MouseMode::RightOnly, DrawButton::Middle, false),
        ] {
            assert_eq!(qualifies_press(&PressFacts { mode, button, ..facts() }), ok, "{mode:?} {button:?}");
        }
        // the setting is absent: mode 0
        assert!(qualifies_press(&PressFacts { mode: MouseMode::from_value(0), ..facts() }));
        assert_eq!(MouseMode::from_value(1), MouseMode::LeftOnly);
        assert_eq!(MouseMode::from_value(2), MouseMode::RightOnly);
        assert_eq!(MouseMode::from_value(7), MouseMode::Both);
        let blockers: [fn(&mut PressFacts); 9] = [
            |f| f.shift = true, |f| f.ctrl = true, |f| f.alt = true, |f| f.win = true,
            |f| f.touch_or_pen = true, |f| f.on_empty_desktop = false,
            |f| f.clear_view = true, |f| f.range_transition = true, |f| f.other_button_down = true,
        ];
        for block in blockers {
            let mut f = facts();
            block(&mut f);
            assert!(!qualifies_press(&f));
        }
        assert!(is_touch_or_pen(0xFF51_5700_u32 as isize));
        assert!(is_touch_or_pen(0xFF51_5780_u32 as isize));
        assert!(!is_touch_or_pen(0));
        assert!(!is_touch_or_pen(0xFF51_5600_u32 as isize));
    }

    fn walk(events: &[DrawEvent]) -> (DrawState, Vec<DrawEffect>) {
        let mut state = DrawState::Idle;
        let mut effects = Vec::new();
        for e in events {
            let (next, effect) = draw_step(state, *e);
            state = next;
            effects.push(effect);
        }
        (state, effects)
    }

    fn press(at: (i32, i32)) -> DrawEvent {
        DrawEvent::Press { button: DrawButton::Left, at, qualifies: true }
    }

    #[test]
    fn draw_step_walks_the_table() {
        // 15 DIP stays pending, 16 DIP arms; at 240 dpi ARM is 40 px
        for dpi in DPIS {
            let arm = arm_px(dpi, (4, 4));
            assert_eq!(arm, dip_to_px(16.0, dpi));
            let (state, effects) = walk(&[press((100, 100)), DrawEvent::Move { at: (100 + arm - 1, 100), arm_px: arm }]);
            assert_eq!(state, DrawState::Pending { button: DrawButton::Left, start: (100, 100) }, "dpi {dpi}");
            assert_eq!(effects, [DrawEffect::None, DrawEffect::None]);
            let (state, effects) = walk(&[press((100, 100)), DrawEvent::Move { at: (100, 100 - arm), arm_px: arm }]);
            assert_eq!(state, DrawState::Drawing { button: DrawButton::Left, start: (100, 100) }, "dpi {dpi}");
            assert_eq!(effects[1], DrawEffect::Armed);
        }
        assert_eq!(arm_px(240, (4, 4)), 40);
        assert_eq!(arm_px(96, (4, 4)), 16);
        // a larger accessibility drag size raises ARM
        assert_eq!(arm_px(96, (30, 12)), 30);
        assert_eq!(arm_px(96, (12, 30)), 30);
        // release while pending is a click
        let (state, effects) = walk(&[press((5, 5)), DrawEvent::Release { rect_valid: false }]);
        assert_eq!((state, effects[1]), (DrawState::Idle, DrawEffect::Click));
        // a press that does not qualify never starts anything
        let (state, effects) = walk(&[DrawEvent::Press { button: DrawButton::Left, at: (1, 1), qualifies: false }, DrawEvent::Release { rect_valid: true }]);
        assert_eq!((state, effects), (DrawState::Idle, vec![DrawEffect::None, DrawEffect::None]));
        // drawing: moves preview, a valid release opens the chooser, a small one is suppressed
        let arm = DrawEvent::Move { at: (200, 200), arm_px: 16 };
        let (state, effects) = walk(&[press((100, 100)), arm, DrawEvent::Move { at: (210, 210), arm_px: 16 }, DrawEvent::Release { rect_valid: true }]);
        assert_eq!(state, DrawState::Idle);
        assert_eq!(effects, [DrawEffect::None, DrawEffect::Armed, DrawEffect::Preview, DrawEffect::Chooser]);
        let (_, effects) = walk(&[press((100, 100)), arm, DrawEvent::Release { rect_valid: false }]);
        assert_eq!(effects[2], DrawEffect::Suppressed);
        // every cancel reason, pending or drawing: nothing created and no click
        for reason in [CancelReason::OtherButton, CancelReason::Escape, CancelReason::CancelMode, CancelReason::CaptureLost,
            CancelReason::DisplayChange, CancelReason::DpiChange, CancelReason::ClearView, CancelReason::RangeChange] {
            for armed in [false, true] {
                let mut events = vec![press((100, 100))];
                if armed { events.push(arm); }
                events.push(DrawEvent::Cancel(reason));
                // a release after the cancel must not click either
                events.push(DrawEvent::Release { rect_valid: true });
                let (state, effects) = walk(&events);
                assert_eq!(state, DrawState::Idle);
                assert_eq!(effects[effects.len() - 2], DrawEffect::Cancelled, "{reason:?} armed={armed}");
                assert!(!effects.contains(&DrawEffect::Click) && !effects.contains(&DrawEffect::Chooser));
            }
        }
        // a second press while a gesture is live cancels it
        let (state, effects) = walk(&[press((1, 1)), DrawEvent::Press { button: DrawButton::Right, at: (2, 2), qualifies: true }]);
        assert_eq!((state, effects[1]), (DrawState::Idle, DrawEffect::Cancelled));
    }

    #[test]
    fn shrinking_back_cancels_and_a_thin_drag_creates_the_minimum() {
        for dpi in DPIS {
            let d = |dip: f64| dip_to_px(dip, dpi);
            let anchor = (500, 500);
            let valid = |pointer: (i32, i32)| creation_rect(&input(dpi, anchor, pointer, off(), &[])).is_some();
            let arm = arm_px(dpi, (4, 4));
            // arm at 40 DIP, return to 10 DIP x 10 DIP, release: cancelled, no context menu
            let (_, effects) = walk(&[
                press(anchor),
                DrawEvent::Move { at: (500 + d(40.0), 500 + d(40.0)), arm_px: arm },
                DrawEvent::Move { at: (500 + d(10.0), 500 + d(10.0)), arm_px: arm },
                DrawEvent::Release { rect_valid: valid((500 + d(10.0), 500 + d(10.0))) },
            ]);
            assert_eq!(effects, [DrawEffect::None, DrawEffect::Armed, DrawEffect::Preview, DrawEffect::Suppressed], "dpi {dpi}");
            // 17 DIP x 2 DIP: the chooser with the minimum size
            let thin = (500 + d(17.0), 500 + d(2.0));
            assert!(valid(thin));
            let rect = creation_rect(&input(dpi, anchor, thin, off(), &[])).unwrap();
            assert_eq!((rect.body.width(), rect.body.height()), snap::min_body_px(dpi), "dpi {dpi}");
            let (_, effects) = walk(&[press(anchor), DrawEvent::Move { at: thin, arm_px: arm }, DrawEvent::Release { rect_valid: true }]);
            assert_eq!(effects[2], DrawEffect::Chooser);
        }
    }

    #[test]
    fn the_chooser_has_two_items_and_flips_at_the_work_area_edges() {
        assert_eq!(CHOOSER_ITEMS.len(), 2);
        assert_eq!(CHOOSER_ITEMS[0], (ChooserChoice::Corral, "&Create Corral here"));
        assert_eq!(CHOOSER_ITEMS[1], (ChooserChoice::FolderGate, "Create &Folder Gate here"));
        for dpi in DPIS {
            let work = Rect::new(-1920, -100, 0, 980);
            let off = dip_to_px(CHOOSER_OFFSET_DIP, dpi);
            let (w, h) = chooser_size_px(dpi);
            let mid = chooser_placement((-1000, 400), dpi, work);
            assert_eq!(mid, ChooserPlacement { at: (-1000 + off, 400 + off), align_right: false, align_bottom: false }, "dpi {dpi}");
            let right = chooser_placement((work.right - w / 2, 400), dpi, work);
            assert!(right.align_right && !right.align_bottom);
            assert_eq!(right.at.0, work.right - w / 2 - off);
            let bottom = chooser_placement((-1000, work.bottom - h / 2), dpi, work);
            assert!(bottom.align_bottom && !bottom.align_right);
            let corner = chooser_placement((work.right - 1, work.bottom - 1), dpi, work);
            assert!(corner.align_right && corner.align_bottom);
        }
    }

    fn keys(mk: u32) -> DropKeys {
        DropKeys { mk, ..DropKeys::default() }
    }

    fn one() -> Vec<String> {
        vec!["C:\\Data".to_string()]
    }

    #[test]
    fn alt_drop_decision_follows_the_modifier_rules() {
        let folders = one();
        let decide = |k: DropKeys, buttons: u32, target: AltTarget| alt_drop_decision(&k, buttons, Some(&folders), target, true, false);
        assert_eq!(decide(keys(MK_ALT), MK_LBUTTON, AltTarget::Empty), AltDrop::Accept(1));
        assert_eq!(decide(keys(MK_ALT | MK_CONTROL), MK_LBUTTON, AltTarget::Empty), AltDrop::Ordinary);
        assert_eq!(decide(keys(MK_ALT | MK_SHIFT), MK_LBUTTON, AltTarget::Empty), AltDrop::Ordinary);
        assert_eq!(decide(keys(0), MK_LBUTTON, AltTarget::Empty), AltDrop::Ordinary, "Alt released before the drop");
        assert_eq!(decide(keys(MK_ALT), MK_RBUTTON, AltTarget::Empty), AltDrop::Ordinary, "right-button drags never");
        assert_eq!(decide(keys(MK_ALT), MK_LBUTTON | MK_RBUTTON, AltTarget::Empty), AltDrop::Ordinary);
        assert_eq!(decide(keys(MK_ALT), 0, AltTarget::Empty), AltDrop::Ordinary);
        assert_eq!(decide(keys(MK_ALT), MK_LBUTTON, AltTarget::Other), AltDrop::Ordinary, "folder icon / item / Gate view");
        // AltGr: the injected left Ctrl is ignored
        let altgr = DropKeys { mk: MK_ALT | MK_CONTROL, rmenu: true, lctrl: true, ..DropKeys::default() };
        assert_eq!(decide(altgr, MK_LBUTTON, AltTarget::Empty), AltDrop::Accept(1));
        // a real Ctrl with the left Alt, or with AltGr plus the right Ctrl, is not an Alt-drop
        let real = DropKeys { mk: MK_ALT | MK_CONTROL, lmenu: true, lctrl: true, ..DropKeys::default() };
        assert_eq!(decide(real, MK_LBUTTON, AltTarget::Empty), AltDrop::Ordinary);
        let both = DropKeys { mk: MK_ALT | MK_CONTROL, rmenu: true, lctrl: true, rctrl: true, ..DropKeys::default() };
        assert_eq!(decide(both, MK_LBUTTON, AltTarget::Empty), AltDrop::Ordinary);
        // a source that does not allow DROPEFFECT_LINK is refused, never handed to the Shell
        assert_eq!(alt_drop_decision(&keys(MK_ALT), MK_LBUTTON, Some(&folders), AltTarget::Empty, false, false), AltDrop::Refused(Refusal::NoLink));
        assert_eq!(alt_drop_decision(&keys(MK_ALT), MK_LBUTTON, Some(&folders), AltTarget::Empty, false, true), AltDrop::Refused(Refusal::NoLink));
        // without the intent a missing LINK changes nothing
        assert_eq!(alt_drop_decision(&keys(0), MK_LBUTTON, Some(&folders), AltTarget::Empty, false, true), AltDrop::Ordinary);
        assert_eq!(alt_drop_decision(&keys(MK_ALT), MK_LBUTTON, Some(&folders), AltTarget::Other, false, true), AltDrop::Ordinary);
        assert_eq!(alt_drop_decision(&keys(MK_ALT | MK_SHIFT), MK_LBUTTON, Some(&folders), AltTarget::Empty, false, true), AltDrop::Ordinary);
        // classifier not finished: ordinary feedback while dragging, refused at the drop
        assert_eq!(alt_drop_decision(&keys(MK_ALT), MK_LBUTTON, None, AltTarget::Empty, true, false), AltDrop::Ordinary);
        assert_eq!(alt_drop_decision(&keys(MK_ALT), MK_LBUTTON, None, AltTarget::Empty, true, true), AltDrop::Refused(Refusal::NotClassified));
        assert_eq!(alt_drop_decision(&keys(0), MK_LBUTTON, None, AltTarget::Empty, true, true), AltDrop::Ordinary);
        // none: ordinary; six: accepted; seven: refused
        let list = |n: usize| (0..n).map(|i| format!("C:\\F{i}")).collect::<Vec<_>>();
        let by_count = |n: usize| alt_drop_decision(&keys(MK_ALT), MK_LBUTTON, Some(&list(n)), AltTarget::Empty, true, false);
        assert_eq!(by_count(0), AltDrop::Ordinary);
        assert_eq!(by_count(1), AltDrop::Accept(1));
        assert_eq!(by_count(6), AltDrop::Accept(6));
        assert_eq!(by_count(7), AltDrop::TooMany);
        assert_eq!(alt_drop_tip(1), "Create Folder Gate here");
        assert_eq!(alt_drop_tip(3), "Create 3 Folder Gates here");
        assert_eq!(TOO_MANY_TIP, "Drop up to 6 folders at once");
    }

    #[test]
    fn dropped_items_are_classified_by_read_only_attributes() {
        let known = ["C:\\", "C:\\Data", "\\\\server\\share\\dir", "D:\\Junction", "C:\\Data\\"];
        let probe = |p: &str| known.contains(&p);
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // a zip, a shortcut, a library, a missing path and a shell-only name are not folders
        let mixed = s(&["C:\\a.zip", "C:\\b.lnk", "C:\\Users\\x\\Documents.library-ms", "C:\\missing", "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", "relative"]);
        assert!(classify_folders(&mixed, probe).is_empty());
        // a drive root, a UNC directory and a junction are
        assert_eq!(classify_folders(&s(&["C:\\", "\\\\server\\share\\dir", "D:\\Junction"]), probe), s(&["C:\\", "\\\\server\\share\\dir", "D:\\Junction"]));
        // one folder and three files make one Gate
        assert_eq!(classify_folders(&s(&["C:\\x.txt", "C:\\Data", "C:\\y.txt", "C:\\z.zip"]), probe), s(&["C:\\Data"]));
        // duplicates collapse, whatever the case and the trailing separator
        assert_eq!(classify_folders(&s(&["C:\\Data", "c:\\data", "C:\\Data\\"]), probe).len(), 1);
        // the real attribute test is read-only and correct on a scratch folder
        let root = std::env::temp_dir().join(format!("corral-d2-classify-{}", std::process::id()));
        let dir = root.join("dir");
        std::fs::create_dir_all(&dir).unwrap();
        let file = root.join("a.zip");
        std::fs::write(&file, b"x").unwrap();
        let (dir_s, file_s) = (dir.to_string_lossy().into_owned(), file.to_string_lossy().into_owned());
        assert!(fs_is_folder(&dir_s) && !fs_is_folder(&file_s) && !fs_is_folder(&root.join("missing").to_string_lossy()));
        assert_eq!(classify_folders(&[file_s.clone(), dir_s.clone()], fs_is_folder), vec![dir_s]);
        assert_eq!(std::fs::read(&file).unwrap(), b"x", "classification never writes");
        let _ = std::fs::remove_dir_all(&root);
    }

    static CLASSIFIER_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn the_classifier_runs_on_a_worker_and_late_results_are_not_used() {
        let _serial = CLASSIFIER_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let slow = FolderClassifier::spawn(vec!["C:\\Data".into()], move |_| {
            let _ = gate.recv();
            true
        });
        assert!(slow.result().is_none(), "not finished: the drop stays ordinary");
        release.send(()).unwrap();
        let mut got = None;
        for _ in 0..200 {
            got = slow.result();
            if got.is_some() { break; }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(got, Some(vec!["C:\\Data".to_string()]));
        // a result slower than the budget is dropped
        let late = FolderClassifier { slot: std::sync::Arc::new(std::sync::Mutex::new(Some((one(), CLASSIFY_BUDGET + std::time::Duration::from_millis(1))))) };
        assert!(late.result().is_none());
        let on_time = FolderClassifier { slot: std::sync::Arc::new(std::sync::Mutex::new(Some((one(), CLASSIFY_BUDGET)))) };
        assert_eq!(on_time.result(), Some(one()));
    }

    #[test]
    fn drags_share_one_classifier_thread_and_a_stuck_one_is_replaced() {
        let _serial = CLASSIFIER_TESTS.lock().unwrap_or_else(|e| e.into_inner());
        let ids = |n: usize| -> Vec<std::thread::ThreadId> {
            let (tx, rx) = std::sync::mpsc::channel();
            let classifiers: Vec<_> = (0..n).map(|_| {
                let tx = tx.clone();
                FolderClassifier::spawn(vec!["C:\\x".into()], move |_| { let _ = tx.send(std::thread::current().id()); true })
            }).collect();
            let wait = std::time::Instant::now();
            while classifiers.iter().any(|c| c.result().is_none()) && wait.elapsed() < std::time::Duration::from_secs(5) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            rx.try_iter().collect()
        };
        let first = ids(3);
        assert_eq!(first.len(), 3);
        assert!(first.iter().all(|id| *id == first[0]), "one thread serves all drags");
        assert_ne!(first[0], std::thread::current().id());
        // a worker stuck on a dead path is left alone once it is over budget; the next drag gets a new one
        let budget = std::time::Duration::from_millis(400); // generous: the full suite runs tests in parallel
        let (release, gate) = std::sync::mpsc::channel::<()>();
        let stuck = FolderClassifier::spawn_with_budget(budget, vec!["C:\\dead".into()], move |_| { let _ = gate.recv(); true });
        std::thread::sleep(budget * 3);
        assert!(stuck.result().is_none());
        let (tx, rx) = std::sync::mpsc::channel();
        let next = FolderClassifier::spawn_with_budget(budget, vec!["C:\\x".into()], move |_| { let _ = tx.send(std::thread::current().id()); true });
        let replacement = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the replacement worker runs it");
        assert_ne!(replacement, first[0]);
        let wait = std::time::Instant::now(); // the worker signals before it stores the result
        while next.result().is_none() && wait.elapsed() < budget { std::thread::sleep(std::time::Duration::from_millis(5)); }
        assert_eq!(next.result(), Some(vec!["C:\\x".to_string()]));
        release.send(()).unwrap();
    }

    #[test]
    fn known_items_count_as_folders_by_their_attributes() {
        assert!(known_item_is_folder("C:\\Users\\x\\Desktop\\Docs", true, false));
        assert!(known_item_is_folder("\\\\server\\share\\dir", true, false));
        assert!(!known_item_is_folder("C:\\a.zip", false, false), "a stream folder is a file");
        assert!(!known_item_is_folder("C:\\Docs.lnk", true, true), "shortcuts do not count");
        assert!(!known_item_is_folder("::{20D04FE0-3AEA-1069-A2D8-08002B30309D}", true, false));
        assert!(!known_item_is_folder("C:\\Users\\x\\Documents.library-ms", true, false));
        assert!(!known_item_is_folder("C:\\f.txt", false, false));
    }

    fn gate_input<'a>(dpi: u32, drop: (i32, i32), count: usize, works: &'a [Rect], governing: usize, settings: SnapSettings, neighbours: &'a [Rect]) -> QuickGateInput<'a> {
        QuickGateInput { drop, count, works, governing, dpi, strip_px: dip_to_px(24.0, dpi), cell: cell(dpi), settings, neighbours }
    }

    #[test]
    fn quick_gate_rects_have_the_cell_size_and_land_inside_the_work_area() {
        // sizes from the spec at 96 and 120 dpi
        assert_eq!(cell(96).body_size_for(5, 4), (391, 368));
        assert_eq!(cell(120).body_size_for(5, 4), (490, 460));
        assert_eq!((cell(120).cell_w_px, cell(120).cell_h_px, cell(120).pad_px), (94, 110, 10));
        for dpi in DPIS {
            for origin in [(0, 0), (-1920, -200), (-3840, 0), (0, -1080)] {
                let work = Rect::new(origin.0, origin.1, origin.0 + 3200, origin.1 + 2400);
                let strip = dip_to_px(24.0, dpi);
                let (bw, bh) = cell(dpi).body_size_for(5, 4);
                let centre = (origin.0 + 1500, origin.1 + 900);
                let r = quick_gate_rects(&gate_input(dpi, centre, 1, &[work], 0, off(), &[]));
                assert_eq!(r.len(), 1);
                assert_eq!((r[0].width(), r[0].height()), (bw, bh + strip), "dpi {dpi}");
                // centred horizontally, the pointer rests on the title strip
                assert_eq!(r[0].left, centre.0 - bw / 2);
                assert!(centre.1 >= r[0].top && centre.1 <= r[0].top + strip, "pointer on the title strip: dpi {dpi} origin {origin:?} centre {centre:?} rect {:?} strip {strip}", r[0]);
                // the corners and edges keep every Gate inside the work area
                for drop in [work.left, work.right - 1, work.left + 10].into_iter().flat_map(|x| [work.top, work.bottom - 1, work.top + 10].map(|y| (x, y))) {
                    for count in [1usize, 3, 6, 9] {
                        let rects = quick_gate_rects(&gate_input(dpi, drop, count, &[work], 0, off(), &[]));
                        assert_eq!(rects.len(), count.min(MAX_FOLDERS_PER_DROP));
                        for r in &rects {
                            assert!(r.left >= work.left && r.top >= work.top && r.right <= work.right && r.bottom <= work.bottom, "{r:?} in {work:?}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn quick_gates_clamp_to_a_small_work_area_and_tile_with_a_gap() {
        for dpi in DPIS {
            let d = |dip: f64| dip_to_px(dip, dpi);
            let small = Rect::new(0, 0, d(640.0), d(360.0));
            let r = quick_gate_rects(&gate_input(dpi, (d(320.0), d(180.0)), 1, &[small], 0, off(), &[]));
            assert!(r[0].width() <= small.width() && r[0].height() <= small.height());
            assert!(r[0].width() >= snap::min_body_px(dpi).0);
            // tiles left to right in drop order with a gap of at least 4 DIP, also with the gap setting off
            let work = Rect::new(0, 0, 8000, 2000);
            let rects = quick_gate_rects(&gate_input(dpi, (100, 300), 6, &[work], 0, off(), &[]));
            let gap = d(QUICK_GATE_MIN_GAP_DIP);
            assert_eq!(rects[1].left, rects[0].right + gap, "gap of at least 4 DIP with the gap setting off");
            assert_eq!(rects[1].top, rects[0].top);
            for pair in rects.windows(2) {
                assert_eq!(pair[1].left, pair[0].right + gap);
            }
            // a row that would pass the work-area right edge wraps back to the first Gate's left edge
            let narrow = Rect::new(0, 0, rects[0].width() * 2 + 3 * gap, 6000);
            let rects = quick_gate_rects(&gate_input(dpi, (100, 300), 5, &[narrow], 0, off(), &[]));
            assert_eq!(rects.len(), 5);
            assert_eq!(rects[1].top, rects[0].top, "two fit in a row");
            assert_eq!(rects[2].left, rects[0].left, "the third starts a new row");
            assert!(rects[2].top >= rects[0].bottom + gap || rects[2].bottom == narrow.bottom || rects[2].top > rects[0].top);
            for r in &rects {
                assert!(r.right <= narrow.right && r.bottom <= narrow.bottom);
            }
            // a batch taller than the screen overlaps rather than leaves it
            let flat = Rect::new(0, 0, d(900.0), d(500.0));
            for r in quick_gate_rects(&gate_input(dpi, (d(450.0), d(250.0)), 6, &[flat], 0, off(), &[])) {
                assert!(r.left >= flat.left && r.top >= flat.top && r.right <= flat.right && r.bottom <= flat.bottom);
            }
            // the gap setting wins when it is larger
            let wide = SnapSettings::new(true, 40, true, false, false);
            let rects = quick_gate_rects(&gate_input(dpi, (100, 300), 2, &[work], 0, wide, &[]));
            assert_eq!(rects[1].left - rects[0].right, wide.gap_px(dpi));
        }
    }

    #[test]
    fn quick_gates_use_the_monitor_under_the_drop_point() {
        let (left, right) = (Rect::new(-1920, 0, 0, 1040), Rect::new(0, 0, 2560, 1400));
        let r = quick_gate_rects(&gate_input(96, (-10, 500), 1, &[left, right], 0, off(), &[]));
        assert!(r[0].left >= left.left && r[0].right <= left.right, "{:?}", r[0]);
        let r = quick_gate_rects(&gate_input(96, (10, 500), 1, &[left, right], 1, off(), &[]));
        assert!(r[0].left >= right.left && r[0].right <= right.right);
        // snapping aligns the Gate to a neighbour's edge
        let neighbour = [Rect::new(1000, 100, 1400, 500)];
        let snapped = SnapSettings::new(true, 0, true, true, false);
        let r = quick_gate_rects(&gate_input(96, (1000 + 195 + 3, 700), 1, &[left, right], 1, snapped, &neighbour));
        assert_eq!(r[0].left, 1000, "left edge snapped to the neighbour");
    }

    #[test]
    fn the_stored_rect_is_the_previewed_size_in_dips() {
        for dpi in DPIS {
            let work = Rect::new(-1920, 40, 0, 1000);
            let body = Rect::new(-1500, 200, -1500 + dip_to_px(300.0, dpi), 200 + dip_to_px(200.0, dpi));
            let stored = body_to_dip(body, work, dpi);
            assert_eq!((stored.width(), stored.height()), (300, 200), "dpi {dpi}");
            assert_eq!(body_to_dip(body, work, dpi), stored, "deterministic");
        }
    }
}
