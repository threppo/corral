//! OLE drag and drop between Explorer (or any OLE application) and Corral.
//!
//! Corral draws the desktop items itself, so it has to speak OLE drag and drop for Explorer's
//! stand-in. Both directions delegate every file operation to the Shell; Corral never copies,
//! moves, deletes or renames a file in this module.
//!
//! * **Drop target** ([`DropTarget`], an `IDropTarget`): the real work is done by the Shell's own
//!   drop target of the destination folder (`IShellFolder::CreateViewObject(IID_IDropTarget)` for
//!   the desktop or a folder path). Move/copy/link selection by drive and modifier keys, conflict
//!   dialogs, right-button "Copy here / Move here" menus and undo are therefore Explorer's.
//!   Corral only (a) lets the parent veto or take over a drop ([`DropHint`]), (b) reads the source
//!   names from the data object *before* delegating, and (c) reports them afterwards
//!   ([`Dropped`]).
//! * **Drag source** ([`begin_drag`]): builds the Shell data object of the items
//!   (`GetUIObjectOf(IDataObject)` of their common parent folder) and runs `DoDragDrop` with an
//!   `IDropSource` implemented here. The outcome reports what the target did.
//!
//! # Parent integration
//!
//! **Thread and apartment.** Everything here runs on the UI thread that owns the window, which
//! must be an STA with *OLE* initialised: call [`init_ole`] (`OleInitialize`) once on that thread
//! before [`register`] or [`begin_drag`]. `OleInitialize` after an earlier
//! `CoInitializeEx(COINIT_APARTMENTTHREADED)` on the same thread is fine (it returns success and
//! adds the OLE services); `CoInitializeEx` alone is *not* enough - `RegisterDragDrop` fails with
//! `CO_E_NOTINITIALIZED` and `DoDragDrop` too. `RegisterDragDrop`, `RevokeDragDrop` (dropping
//! [`Registration`]) and every callback run on that thread; none of the types are `Send`.
//!
//! **When to register.** Create the host window, then `init_ole()`, build the target with
//! [`DropTarget::new`], `register(hwnd, target)` and keep the returned [`Registration`] alive as
//! long as the window should accept drops. Register the window that actually receives the mouse:
//! Corral's layered group windows (or the one host window), not `Progman`/the list view. Drop the
//! `Registration` (or call [`Registration::revoke`]) before `DestroyWindow`, and on Explorer
//! restart re-register after re-creating the window.
//!
//! **Callbacks** ([`DropCallbacks`]) are invoked synchronously from inside OLE calls, on the UI
//! thread, while OLE (or `DoDragDrop`'s modal loop for Corral's own drags) holds the stack. They
//! must be quick, must not show UI, must not start a nested drag and must not call back into the
//! `DropTarget`; a re-entrant call is skipped rather than deadlocking. A callback that touches
//! shared application state should use `try_borrow_mut` and fall back to doing nothing. All points
//! are **screen coordinates** (`ScreenToClient` them for hit-testing).
//!
//! **Choosing the target group.** `on_hover` receives the screen point on every `DragOver`: hit-test
//! the groups, remember/highlight the group under the cursor and return a [`DropHint`]. Return
//! [`DropHint::Shell`] to let the Shell perform the drop into the desktop (the default
//! destination), [`DropHint::ShellTo`] to drop into a specific folder (e.g. a folder-portal group
//! mirroring a directory) or [`DropHint::Reject`] for "no drop here". `on_leave` clears the
//! highlight. `on_dropped` is then called once with the sources and the point:
//!
//! * "insert into group X": the parent remembers the group it returned the hint for (or hit-tests
//!   `Dropped::point` again) and records a *pending placement* for each of
//!   `Dropped::expected_destinations` (a lower-cased path -> group id map). When the desktop item
//!   appears (the Shell change notification / the next desktop enumeration) it is added to that
//!   group instead of being left loose.
//! * "loose at point": same, but the placement is a position (the screen point converted to the
//!   desktop-surface point) instead of a group id.
//!
//! The Shell may perform the operation asynchronously (background copy thread, progress dialog,
//! conflict prompt, right-button menu) and may rename on conflict ("name - Copy"), so the expected
//! names are predictions: the complete way to attribute new items is to snapshot the known desktop
//! item names when `on_dropped` fires and treat names that newly appear within a few seconds as
//! belonging to the drop; names matching `expected_destinations` are the exact case. Data without
//! files (text, URLs, browser images, virtual files) produces empty `sources` and still needs the
//! snapshot diff. `on_dropped` means "the Shell target accepted a drop with effect X", not "files arrived":
//! a cancelled right-button menu or a declined conflict prompt still reports it, so the snapshot diff
//! (empty = nothing happened) is what decides. An optimised move returns `DROPEFFECT_NONE` from the
//! Shell's `Drop` (so the source never deletes); `Dropped::effect` still says MOVE and
//! `Dropped::target_effect` carries the returned NONE. Never remove or move items from the layout
//! based on `on_dropped`.
//!
//! **Corral's own drags.** [`begin_drag`] sets a thread-local marker for the lifetime of its
//! `DoDragDrop`; `DragInfo::internal` / `Dropped::internal` are true for drops that originate
//! from it (the modal loop runs on the same thread, so no other drag can be in flight). For these,
//! return [`DropHint::Internal`] from `on_hover` (typically `DROPEFFECT_MOVE`): the Shell is *not*
//! consulted, no file operation happens, and `on_dropped` fires so the parent can re-group the
//! items (group-to-group moves, or "move loose at point"). Returning [`DropHint::Shell`] for an
//! internal drag would make the Shell move the items onto the desktop they already are on, which
//! for a copy-modifier drag creates "- Copy" files; do not do that.
//!
//! **After `begin_drag`.** The returned [`DragOutcome`] is informational. The authoritative state
//! comes from the Shell change notifications (items that were moved away disappear from the
//! desktop enumeration). Do not delete anything yourself. When
//! [`DragOutcome::source_should_delete`] is true the target asked the *source* to remove the
//! originals (an "unoptimized move", e.g. into an archive). This is informational: never follow
//! an item hand-over with a second delete/move. Corral refreshes; the Shell owns the operation.
//!
//! **Not covered here** (parent/other modules): the drag threshold and starting a drag on a mouse
//! move, hit-testing, and drawing highlights. `DoDragDrop` is modal: it returns only when the drag
//! ends, so call it from the mouse-move handler that detected the threshold, with the group state
//! stored where the callbacks can reach it.
#![allow(dead_code)]

#[path = "dragdrop/bitmap.rs"]
mod bitmap;
#[path = "dragdrop/feedback.rs"]
mod feedback;

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::Path;

use windows::Win32::Foundation::{GlobalFree,
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, E_FAIL, E_INVALIDARG, HGLOBAL, HWND, POINT,
    POINTL, S_OK, SIZE,
};
use windows::Win32::Graphics::Gdi::{BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, DeleteObject, CreateDIBSection, HBITMAP};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, DVASPECT_CONTENT, FORMATETC, IBindCtx, IDataObject, STGMEDIUM,
    TYMED_HGLOBAL,
};
use windows::Win32::System::DataExchange::RegisterClipboardFormatW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE, DROPEFFECT_NONE, DoDragDrop, IDropSource,
    IDropSource_Impl, IDropTarget, IDropTarget_Impl, OleGetClipboard, OleInitialize, RegisterDragDrop, ReleaseStgMedium,
    RevokeDragDrop,
};
use windows::Win32::System::SystemServices::{MK_CONTROL, MK_LBUTTON, MK_RBUTTON, MK_SHIFT, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VIRTUAL_KEY, VK_LBUTTON, VK_LCONTROL, VK_LMENU, VK_RBUTTON, VK_RCONTROL, VK_RMENU};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_SFObject, CLSID_DragDropHelper, DragQueryFileW, FOLDERID_Desktop, HDROP, ILClone, ILCombine, ILFindLastID, ILFree,
    ILIsEqual, ILRemoveLastID, IDragSourceHelper, IDragSourceHelper2, IDropTargetHelper, IShellFolder, IShellItem, KF_FLAG_DEFAULT, SHBindToParent,
    SHCreateItemFromParsingName, SHDRAGIMAGE, SHGetDesktopFolder, SHGetKnownFolderPath, SHGetNameFromIDList, SHParseDisplayName,
    SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, IDC_ARROW, LoadCursorW, SM_SWAPBUTTON, SetCursor};
use windows::core::{Error, HSTRING, Interface, PWSTR, Result, implement};

const CFSTR_SHELLIDLIST: &str = "Shell IDList Array";
const CFSTR_PERFORMEDDROPEFFECT: &str = "Performed DropEffect";
const CFSTR_LOGICALPERFORMEDDROPEFFECT: &str = "Logical Performed DropEffect";
/// All effects Corral offers as a drag source.
const ALL_EFFECTS: DROPEFFECT = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0 | DROPEFFECT_LINK.0);
/// `CLR_NONE`: no colour key (the drag image carries per-pixel alpha).
const CLR_NONE: u32 = bitmap::COLOR_KEY;

fn err(message: &str) -> Error {
    Error::new(E_FAIL, message)
}

fn invalid(message: &str) -> Error {
    Error::new(E_INVALIDARG, message)
}

/// OLE initialisation for the calling thread (an STA with the OLE services that
/// `RegisterDragDrop` and `DoDragDrop` need). Safe to call after `CoInitializeEx(STA)` on the same
/// thread. There is no matching uninitialise: the UI thread keeps OLE until the process exits.
pub fn init_ole() -> Result<()> {
    unsafe { OleInitialize(None) }
}

// ---------------------------------------------------------------------------------------------
// Data object helpers
// ---------------------------------------------------------------------------------------------

fn clipboard_format(name: &str) -> u16 {
    unsafe { RegisterClipboardFormatW(&HSTRING::from(name)) as u16 }
}

fn hglobal_format(cf: u16) -> FORMATETC {
    FORMATETC { cfFormat: cf, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
}

/// Releases an `STGMEDIUM` obtained from `IDataObject::GetData`.
struct Medium(STGMEDIUM);

impl Medium {
    fn hglobal(&self) -> HGLOBAL {
        unsafe { self.0.u.hGlobal }
    }
}

impl Drop for Medium {
    fn drop(&mut self) {
        unsafe { ReleaseStgMedium(&mut self.0) };
    }
}

fn get_hglobal(data: &IDataObject, cf: u16) -> Result<Medium> {
    let format = hglobal_format(cf);
    let medium = Medium(unsafe { data.GetData(&format)? });
    if medium.0.tymed != TYMED_HGLOBAL.0 as u32 || medium.hglobal().is_invalid() {
        return Err(err("data object returned a medium that is not an HGLOBAL"));
    }
    Ok(medium)
}

/// Reads a DWORD format such as `Performed DropEffect`. `None` when the format is absent.
fn read_dword(data: &IDataObject, cf: u16) -> Option<u32> {
    let medium = get_hglobal(data, cf).ok()?;
    let hglobal = medium.hglobal();
    if unsafe { GlobalSize(hglobal) } < 4 {
        return None;
    }
    let ptr = unsafe { GlobalLock(hglobal) } as *const u32;
    if ptr.is_null() {
        return None;
    }
    let value = unsafe { ptr.read_unaligned() };
    unsafe {
        let _ = GlobalUnlock(hglobal);
    }
    Some(value)
}

fn abs_pidl_name(pidl: *const ITEMIDLIST) -> Option<String> {
    let name: PWSTR = unsafe { SHGetNameFromIDList(pidl, SIGDN_DESKTOPABSOLUTEPARSING) }.ok()?;
    let text = unsafe { name.to_string() }.ok();
    unsafe { CoTaskMemFree(Some(name.0 as *const c_void)) };
    text
}

/// Upper bound on the item count accepted from a `CIDA` (the buffer-size check is the real limit).
const MAX_CIDA_ITEMS: usize = 1_000_000;

fn read_u32(buf: &[u8], at: usize) -> Option<u32> {
    let bytes = buf.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// Length in bytes (terminator included) of the PIDL starting at `start`, walking the `SHITEMID`
/// `cb` chain strictly inside `buf`. `None` when an item is malformed (`cb` of 1, past the end) or
/// the 2-byte zero terminator is not inside the buffer.
fn pidl_extent(buf: &[u8], start: usize) -> Option<usize> {
    let mut at = start;
    loop {
        let cb = u16::from_le_bytes([*buf.get(at)?, *buf.get(at.checked_add(1)?)?]) as usize;
        if cb == 0 {
            return Some(at + 2 - start);
        }
        if cb < 2 {
            return None;
        }
        let next = at.checked_add(cb)?;
        if next > buf.len() {
            return None;
        }
        at = next;
    }
}

/// Validated `(parent, children)` byte ranges of a `CIDA` buffer. Never reads outside `buf`.
/// Returns `None` when the header is inconsistent; individual children whose PIDL is malformed
/// are omitted (and so is everything if the parent PIDL is malformed).
fn parse_cida(buf: &[u8]) -> Option<(std::ops::Range<usize>, Vec<std::ops::Range<usize>>)> {
    if buf.len() < 4 {
        return None;
    }
    let count = read_u32(buf, 0)? as usize;
    if count == 0 || count > MAX_CIDA_ITEMS {
        return None;
    }
    let table_end = count.checked_add(1)?.checked_mul(4)?.checked_add(4)?;
    if table_end > buf.len() {
        return None;
    }
    let range_at = |index: usize| -> Option<std::ops::Range<usize>> {
        let offset = read_u32(buf, 4 + 4 * index)? as usize;
        if offset >= buf.len() {
            return None;
        }
        Some(offset..offset + pidl_extent(buf, offset)?)
    };
    let parent = range_at(0)?;
    let children = (0..count).filter_map(|i| range_at(i + 1)).collect();
    Some((parent, children))
}

/// Copies a validated PIDL range into 2-byte-aligned storage.
fn aligned_copy(bytes: &[u8]) -> Vec<u16> {
    let mut words = vec![0u16; bytes.len().div_ceil(2)];
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), words.as_mut_ptr() as *mut u8, bytes.len()) };
    words
}

/// Absolute parsing names for every child of a `CIDA` buffer (untrusted: fully bounds-checked).
fn names_from_cida(buf: &[u8]) -> Vec<String> {
    let Some((parent, children)) = parse_cida(buf) else { return Vec::new() };
    let parent = aligned_copy(&buf[parent]);
    let mut names = Vec::new();
    for child in children {
        let child = aligned_copy(&buf[child]);
        let absolute = unsafe {
            ILCombine(Some(parent.as_ptr() as *const ITEMIDLIST), Some(child.as_ptr() as *const ITEMIDLIST))
        };
        if absolute.is_null() {
            continue;
        }
        if let Some(name) = abs_pidl_name(absolute) {
            names.push(name);
        }
        unsafe { ILFree(Some(absolute as *const ITEMIDLIST)) };
    }
    names
}

/// Parsing names from `CFSTR_SHELLIDLIST` (a `CIDA`: count, offsets, then a parent PIDL and one
/// child PIDL per item). Works for virtual items too. The buffer is validated against
/// `GlobalSize` before any PIDL is handed to the Shell.
fn names_from_idlist(data: &IDataObject) -> Result<Vec<String>> {
    let medium = get_hglobal(data, clipboard_format(CFSTR_SHELLIDLIST))?;
    let hglobal = medium.hglobal();
    let size = unsafe { GlobalSize(hglobal) };
    if size < 4 {
        return Err(err("Shell ID list is too small"));
    }
    let base = unsafe { GlobalLock(hglobal) } as *const u8;
    if base.is_null() {
        return Err(Error::from_win32());
    }
    // Copy first so the lock is not held across Shell calls and the slice cannot outlive it.
    let copy = unsafe { std::slice::from_raw_parts(base, size) }.to_vec();
    unsafe {
        let _ = GlobalUnlock(hglobal);
    }
    Ok(names_from_cida(&copy))
}

/// File paths from `CF_HDROP`.
fn names_from_hdrop(data: &IDataObject) -> Result<Vec<String>> {
    let medium = get_hglobal(data, CF_HDROP.0)?;
    let hdrop = HDROP(medium.hglobal().0);
    let count = unsafe { DragQueryFileW(hdrop, u32::MAX, None) };
    let mut names = Vec::new();
    for i in 0..count {
        let len = unsafe { DragQueryFileW(hdrop, i, None) } as usize;
        let mut buffer = vec![0u16; len + 1];
        let copied = unsafe { DragQueryFileW(hdrop, i, Some(&mut buffer)) } as usize;
        names.push(String::from_utf16_lossy(&buffer[..copied.min(buffer.len())]));
    }
    Ok(names)
}

/// Parsing names (`SIGDN_DESKTOPABSOLUTEPARSING`) of the items in a data object: the Shell ID
/// list when present, otherwise `CF_HDROP`. Empty when the data carries no files (text, URLs,
/// virtual files). Read-only.
pub fn read_source_names(data: &IDataObject) -> Vec<String> {
    match names_from_idlist(data) {
        Ok(names) if !names.is_empty() => names,
        _ => names_from_hdrop(data).unwrap_or_default(),
    }
}

/// Owns an absolute PIDL allocated by the Shell (`CoTaskMemFree`).
struct Pidl(*mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CoTaskMemFree(Some(self.0 as *const c_void)) };
        }
    }
}

/// Owns a PIDL cloned with `ILClone` (freed with `ILFree`).
struct ClonedPidl(*mut ITEMIDLIST);

impl Drop for ClonedPidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { ILFree(Some(self.0 as *const ITEMIDLIST)) };
        }
    }
}

fn parse(name: &str) -> Result<Pidl> {
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    unsafe { SHParseDisplayName(&HSTRING::from(name), None::<&IBindCtx>, &mut pidl, 0, None)? };
    if pidl.is_null() {
        return Err(err("SHParseDisplayName returned no item"));
    }
    Ok(Pidl(pidl))
}

fn parent_pidl(pidl: &Pidl) -> Result<ClonedPidl> {
    let clone = unsafe { ILClone(pidl.0 as *const ITEMIDLIST) };
    if clone.is_null() {
        return Err(err("ILClone failed"));
    }
    let clone = ClonedPidl(clone);
    unsafe {
        let _ = ILRemoveLastID(Some(clone.0));
    }
    Ok(clone)
}

/// The Shell data object (`CFSTR_SHELLIDLIST`, `CF_HDROP`, ...) for items that share one parent
/// folder, exactly what Explorer drags. `Err` for an empty list, an unparsable name or items from
/// different parents (user and public desktop items resolve to different parents).
pub fn data_object_for(owner: HWND, parsing_names: &[String]) -> Result<IDataObject> {
    if parsing_names.is_empty() {
        return Err(invalid("no items given to drag"));
    }
    let pidls = parsing_names.iter().map(|n| parse(n)).collect::<Result<Vec<_>>>()?;
    let first_parent = parent_pidl(&pidls[0])?;
    for (i, pidl) in pidls.iter().enumerate().skip(1) {
        let parent = parent_pidl(pidl)?;
        if !unsafe { ILIsEqual(first_parent.0 as *const ITEMIDLIST, parent.0 as *const ITEMIDLIST) }.as_bool() {
            return Err(invalid(&format!(
                "items do not share a parent folder (\"{}\" differs from \"{}\"); one data object is not possible",
                parsing_names[i], parsing_names[0]
            )));
        }
    }
    let folder: IShellFolder = unsafe { SHBindToParent::<IShellFolder>(pidls[0].0 as *const ITEMIDLIST, None)? };
    let children: Vec<*const ITEMIDLIST> =
        pidls.iter().map(|p| unsafe { ILFindLastID(p.0 as *const ITEMIDLIST) } as *const ITEMIDLIST).collect();
    if children.iter().any(|c| c.is_null()) {
        return Err(err("could not determine child item identifiers"));
    }
    unsafe { folder.GetUIObjectOf(owner, &children, None) }
}

// ---------------------------------------------------------------------------------------------
// Drop target
// ---------------------------------------------------------------------------------------------

/// Where the Shell should put dropped items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Destination {
    /// The desktop folder (the user's desktop directory; Explorer's behaviour for the desktop).
    Desktop,
    /// A folder given by any parsing name (normally a file-system path).
    Folder(String),
    /// A single shell item, obtained from its parent's GetUIObjectOf, never a folder background.
    Item(String),
}

/// Conservative lexical exclusions before asking the Shell. PIDL identity/ancestry is checked
/// as well by `item_drop_target` (aliases must not bypass the safety boundary).
pub fn item_target_eligible(target: &str, sources: &[String]) -> bool {
    fn normalized(s: &str) -> String { s.replace('/', "\\").trim_end_matches('\\').to_lowercase() }
    let target = normalized(target);
    !target.is_empty() && !sources.is_empty() && sources.iter().all(|source| {
        let source = normalized(source);
        let parent = source.rsplit_once('\\').map(|(p, _)| p);
        !source.is_empty() && target != source && parent != Some(target.as_str())
            && !target.starts_with(&(source + "\\"))
    })
}

/// The item is the dragged item itself, its parent folder or one of its descendants (or a name
/// cannot be parsed): an external drop on it must be refused, never fall back to the Desktop
/// (file-safety review 2026-10-15, finding 2: a file from `Desktop\X` dropped on the `X` icon was
/// moved to the Desktop).
pub fn item_target_excluded(name: &str, sources: &[String]) -> bool {
    // Only a PROVEN self/parent/descendant relation refuses; data without readable file names
    // (browser images, mail attachments) keeps the background route (follow-up review, finding 1).
    if sources.is_empty() { return false }
    if !item_target_eligible(name, sources) { return true }
    let Ok(target) = parse(name) else { return false };
    sources.iter().any(|source| {
        let Ok(source) = parse(source) else { return false };
        let Ok(parent) = parent_pidl(&source) else { return false };
        unsafe {
            use windows::Win32::UI::Shell::ILIsParent;
            ILIsEqual(source.0, target.0).as_bool() || ILIsEqual(parent.0, target.0).as_bool()
                || ILIsParent(source.0, target.0, false).as_bool()
        }
    })
}

/// Read-only capability/identity lookup. No DragEnter or file operation is performed here.
pub fn item_drop_target(owner: HWND, name: &str, sources: &[String]) -> Result<IDropTarget> {
    if !item_target_eligible(name, sources) { return Err(invalid("self/parent/descendant item target")) }
    let target = parse(name)?;
    for source in sources {
        let source = parse(source)?;
        let parent = parent_pidl(&source)?;
        unsafe {
            use windows::Win32::UI::Shell::ILIsParent;
            if ILIsEqual(source.0, target.0).as_bool() || ILIsEqual(parent.0, target.0).as_bool()
                || ILIsParent(source.0, target.0, false).as_bool() {
                return Err(invalid("self/parent/descendant PIDL target"));
            }
        }
    }
    let folder: IShellFolder = unsafe { SHBindToParent(target.0, None)? };
    let child = unsafe { ILFindLastID(target.0) };
    if child.is_null() { return Err(invalid("missing target child PIDL")) }
    unsafe { folder.GetUIObjectOf(owner, &[child], None) }
}

/// What `on_hover` tells the target to do with the drag at the reported point.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropHint {
    /// Let the Shell drop into the target's default destination (the usual answer).
    Shell,
    /// Let the Shell drop into this destination instead of the default (e.g. a folder-portal group).
    ShellTo(Destination),
    /// Corral handles the drop itself (its own internal drags): the Shell is not consulted and no
    /// file operation happens. The effect is reported to the source (clamped to what it allows).
    Internal(DROPEFFECT),
    /// No drop here.
    Reject,
}

/// Information passed to `on_hover`.
pub struct DragInfo<'a> {
    /// Final routing at Drop; hover must not eagerly route an inactive portal label.
    pub dropping: bool,
    /// Screen coordinates of the cursor.
    pub point: POINT,
    /// `MK_*` key/button state.
    pub keys: u32,
    /// Effects the drag source allows.
    pub allowed: DROPEFFECT,
    /// Parsing names of the dragged items (empty when the data carries no files).
    pub sources: &'a [String],
    /// True when the drag was started by [`begin_drag`] on this thread.
    pub internal: bool,
    /// True for the first call of a drag (`DragEnter`).
    pub entering: bool,
}

/// Reported to `on_dropped` after the Shell's drop returned successfully.
#[derive(Clone, Debug)]
pub struct Dropped {
    /// Parsing names of the dropped items as read from the data object before the drop.
    pub sources: Vec<String>,
    /// Predicted parsing names of the items in the destination folder (destination directory plus
    /// the source's leaf name) for COPY and MOVE of file-system items; empty otherwise. The Shell
    /// may rename on conflict or finish asynchronously, so these are predictions, not proof.
    pub expected_destinations: Vec<String>,
    /// The destination the Shell dropped into (`None` for internal positional drops).
    /// `Item` is reload-only even on failure/refusal; no expected placement may be recorded.
    pub destination: Option<Destination>,
    /// Destination directory path when it is a file-system folder.
    pub destination_dir: Option<String>,
    /// Screen coordinates of the drop.
    pub point: POINT,
    /// Effect negotiated for the drop (what the last `DragOver` offered the user). NONE is
    /// possible for an item refusal reported solely to trigger a reload. The Shell chooses.
    pub effect: DROPEFFECT,
    /// Effect the Shell's `Drop` returned to the source. An optimised move (the Shell moved the
    /// files itself) returns `DROPEFFECT_NONE` so the source does not delete anything; copies return
    /// the copy effect. `None` for internal drops.
    pub target_effect: Option<DROPEFFECT>,
    /// True for Corral's own OLE source, including Shell item hand-overs. Do not interpret this
    /// alone as a request for positional placement: first inspect `destination`.
    pub internal: bool,
}

/// Callbacks the parent supplies. See the module documentation for the threading rules.
pub struct DropCallbacks {
    /// Called on `DragEnter` and every `DragOver` (and before a `Drop`).
    pub on_hover: Box<dyn FnMut(&DragInfo<'_>) -> DropHint>,
    /// Called when the drag leaves or is cancelled; clear highlights here.
    pub on_leave: Box<dyn FnMut()>,
    /// Called once after a drop was performed.
    pub on_dropped: Box<dyn FnMut(&Dropped)>,
    quick_create: Option<QuickCreateCallbacks>,
    item_feedback: Box<dyn FnMut(Option<&str>)>,
}

struct QuickCreateCallbacks {
    /// Whether an Alt-drop may start at this screen point (setting, lock, and where the point is).
    enabled: Box<dyn Fn(POINT) -> bool>,
    enqueue: Box<dyn FnMut(Vec<String>, POINT)>,
}

impl DropCallbacks {
    pub fn new(
        on_hover: impl FnMut(&DragInfo<'_>) -> DropHint + 'static,
        on_dropped: impl FnMut(&Dropped) + 'static,
    ) -> DropCallbacks {
        DropCallbacks { on_hover: Box::new(on_hover), on_leave: Box::new(|| {}), on_dropped: Box::new(on_dropped), quick_create: None, item_feedback: Box::new(|_| {}) }
    }

    pub fn with_leave(mut self, on_leave: impl FnMut() + 'static) -> DropCallbacks {
        self.on_leave = Box::new(on_leave);
        self
    }

    pub fn with_item_feedback(mut self, feedback: impl FnMut(Option<&str>) + 'static) -> Self {
        self.item_feedback = Box::new(feedback);
        self
    }

    /// Desktop route only (Alt-drop, creation-spec 5). `enqueue` gets the folders and must only queue work:
    /// never construct a Gate or re-enter App.
    pub fn with_quick_create(mut self, enabled: impl Fn(POINT) -> bool + 'static, enqueue: impl FnMut(Vec<String>, POINT) + 'static) -> Self {
        self.quick_create = Some(QuickCreateCallbacks { enabled: Box::new(enabled), enqueue: Box::new(enqueue) });
        self
    }
}

thread_local! {
    /// Names being dragged by an in-progress [`begin_drag`] on this thread.
    static INTERNAL_DRAG: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
    static INTERNAL_ACTIVE: Cell<bool> = const { Cell::new(false) };
}

/// True while a Corral-initiated drag runs on this thread.
pub fn internal_drag_active() -> bool {
    INTERNAL_ACTIVE.with(|a| a.get())
}

/// Marks a Corral-initiated drag for the lifetime of the guard.
struct InternalDragGuard;

impl InternalDragGuard {
    fn enter(names: &[String]) -> InternalDragGuard {
        INTERNAL_DRAG.with(|d| *d.borrow_mut() = Some(names.to_vec()));
        INTERNAL_ACTIVE.with(|a| a.set(true));
        InternalDragGuard
    }
}

impl Drop for InternalDragGuard {
    fn drop(&mut self) {
        INTERNAL_ACTIVE.with(|a| a.set(false));
        INTERNAL_DRAG.with(|d| *d.borrow_mut() = None);
    }
}

/// Creates the Shell's drop target for a destination, plus the destination directory path when it
/// is a file-system folder.
fn shell_drop_target(dest: &Destination, owner: HWND) -> Result<(IDropTarget, Option<String>)> {
    match dest {
        Destination::Desktop => {
            let folder = unsafe { SHGetDesktopFolder()? };
            let target: IDropTarget = unsafe { folder.CreateViewObject(owner)? };
            let dir = unsafe { SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None) }.ok().and_then(|p| {
                let text = unsafe { p.to_string() }.ok();
                unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
                text
            });
            Ok((target, dir))
        }
        Destination::Folder(name) => {
            let item: IShellItem = unsafe { SHCreateItemFromParsingName(&HSTRING::from(name.as_str()), None::<&IBindCtx>)? };
            let folder: IShellFolder = unsafe { item.BindToHandler(None::<&IBindCtx>, &BHID_SFObject)? };
            let target: IDropTarget = unsafe { folder.CreateViewObject(owner)? };
            let dir = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok().and_then(|p| {
                let text = unsafe { p.to_string() }.ok();
                unsafe { CoTaskMemFree(Some(p.0 as *const c_void)) };
                text
            });
            Ok((target, dir))
        }
        Destination::Item(_) => Err(invalid("item targets require source identity checks")),
    }
}

/// "Paste shortcut" (Explorer does it the same way): the clipboard's items dropped on `folder` with only
/// LINK allowed, so the folder's own drop target makes the shortcuts and nothing is copied or moved.
pub fn paste_link_into(owner: HWND, folder: &str) -> Result<()> {
    let data = unsafe { OleGetClipboard()? };
    let (target, _) = shell_drop_target(&Destination::Folder(folder.to_string()), owner)?;
    // Ctrl+Shift is the Shell's "make a link" key state; without it the folder's default (move/copy) is masked to none
    let keys = MODIFIERKEYS_FLAGS(MK_LBUTTON.0 | MK_CONTROL.0 | MK_SHIFT.0);
    let mut effect = DROPEFFECT_LINK;
    unsafe { target.DragEnter(&data, keys, POINTL::default(), &mut effect)? };
    if effect.0 & DROPEFFECT_LINK.0 == 0 {
        let _ = unsafe { target.DragLeave() };
        return Err(Error::new(E_FAIL, "the folder makes no shortcuts to these items"));
    }
    let mut effect = DROPEFFECT_LINK;
    unsafe { target.Drop(&data, keys, POINTL::default(), &mut effect) }
}

/// Expected names of the dropped items inside `dir` (see [`Dropped::expected_destinations`]).
fn expected_destinations(sources: &[String], dir: Option<&str>, effect: DROPEFFECT) -> Vec<String> {
    let Some(dir) = dir else { return Vec::new() };
    if effect.0 & (DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0) == 0 {
        return Vec::new();
    }
    sources
        .iter()
        .filter(|s| !s.starts_with("::"))
        .filter_map(|s| Path::new(s).file_name().map(|leaf| Path::new(dir).join(leaf).to_string_lossy().into_owned()))
        .collect()
}

struct Delegate {
    dest: Destination,
    dir: Option<String>,
    target: IDropTarget,
    /// The effect the delegate answered last (DragEnter/DragOver): what Drop then works with.
    effect: DROPEFFECT,
}

#[derive(Default)]
struct State {
    data: Option<IDataObject>,
    sources: Vec<String>,
    internal: bool,
    delegate: Option<Delegate>,
    helper: Option<IDropTargetHelper>,
    /// Folder classification of the dragged items, started at `DragEnter` (creation-spec 5.3).
    classifier: Option<crate::creation::FolderClassifier>,
    saved_description: Option<Option<Vec<u8>>>,
    /// The drop point is empty desktop or a Corral's empty area: the hover did not hand it to an item or a folder.
    alt_target_ok: bool,
    /// The last `DragEnter`/`DragOver` answered an Alt-drop (accepted or refused it): a `Drop` that decides
    /// otherwise must not reach the Shell.
    last_alt: bool,
    /// Screen point of the last hover.
    last_point: POINT,
    /// Key state of the last DragEnter/DragOver, mouse buttons included (Drop's key state has none).
    last_keys: u32,
}

/// Outcome of routing one drag event.
enum Routed {
    Rejected,
    Internal(DROPEFFECT),
    Shell,
    /// An Alt-drop answered here (accepted, or refused with `NONE`): the Shell is not involved.
    Alt(crate::creation::AltDrop),
}

/// `IDropTarget` that delegates the real work to the Shell's drop target of a destination folder.
/// Build with [`DropTarget::new`], register with [`register`].
#[implement(IDropTarget)]
pub struct DropTarget {
    hwnd: HWND,
    default_destination: Destination,
    callbacks: RefCell<DropCallbacks>,
    state: RefCell<State>,
}

impl DropTarget {
    /// `hwnd` is the window the target is registered for: it owns Shell dialogs and receives the
    /// drag-image feedback (`IDropTargetHelper`); pass `HWND::default()` for neither.
    /// `default_destination` is where `DropHint::Shell` drops go.
    pub fn new(hwnd: HWND, default_destination: Destination, callbacks: DropCallbacks) -> DropTarget {
        DropTarget { hwnd, default_destination, callbacks: RefCell::new(callbacks), state: RefCell::new(State::default()) }
    }

    fn hover(&self, point: POINT, keys: u32, allowed: DROPEFFECT, entering: bool, dropping: bool) -> DropHint {
        let (sources, internal) = {
            let state = self.state.borrow();
            (state.sources.clone(), state.internal)
        };
        let info = DragInfo { point, keys, allowed, sources: &sources, internal, entering, dropping };
        match self.callbacks.try_borrow_mut() {
            Ok(mut callbacks) => (callbacks.on_hover)(&info),
            Err(_) => DropHint::Reject, // re-entrant call from a callback: refuse rather than panic
        }
    }

    /// Leaves the current Shell delegate, if any.
    fn drop_delegate(&self) {
        let delegate = self.state.borrow_mut().delegate.take();
        if let Some(delegate) = delegate {
            let _ = unsafe { delegate.target.DragLeave() };
        }
    }

    /// Decide what happens for this event and drive the delegate's DragEnter/DragOver.
    fn route(&self, keys: MODIFIERKEYS_FLAGS, pt: POINTL, effect: &mut DROPEFFECT, entering: bool, dropping: bool) -> Routed {
        let routed = self.route_inner(keys, pt, effect, entering, dropping);
        let name = self.state.borrow().delegate.as_ref().and_then(|d| match &d.dest {
            Destination::Item(name) if *effect != DROPEFFECT_NONE => Some(name.clone()), _ => None,
        });
        self.item_feedback(name.as_deref());
        routed
    }

    fn route_inner(&self, keys: MODIFIERKEYS_FLAGS, pt: POINTL, effect: &mut DROPEFFECT, entering: bool, dropping: bool) -> Routed {
        use crate::creation::{AltDrop, Refusal};
        self.restore_description();
        let point = POINT { x: pt.x, y: pt.y };
        let hint = self.hover(point, keys.0, *effect, entering, dropping);
        {
            let mut state = self.state.borrow_mut();
            state.alt_target_ok = matches!(hint, DropHint::Shell | DropHint::Internal(_));
            state.last_point = point;
        }
        // An Alt-drop is decided here, before any Shell target is bound, so a hover that is answered as
        // an Alt-drop never re-binds the Shell delegate (creation-spec 5.5).
        let decision = self.alt_decision(keys.0, *effect, dropping);
        if !dropping {
            self.state.borrow_mut().last_alt = matches!(decision, AltDrop::Accept(_) | AltDrop::TooMany | AltDrop::Refused(Refusal::NoLink));
        }
        if decision != AltDrop::Ordinary {
            self.alt_answer(decision, effect, dropping);
            return Routed::Alt(decision);
        }
        let destination = match hint {
            DropHint::Reject => {
                self.drop_delegate();
                *effect = DROPEFFECT_NONE;
                return Routed::Rejected;
            }
            DropHint::Internal(wanted) => {
                self.drop_delegate();
                *effect = if effect.0 & wanted.0 != 0 { DROPEFFECT(wanted.0 & effect.0) } else { DROPEFFECT_NONE };
                return Routed::Internal(*effect);
            }
            DropHint::Shell if self.state.borrow().internal => {
                // Corral's own drag must never reach the default Shell target (a Ctrl-copy onto the
                // desktop folder the items already live in would create "- Copy" files).
                self.drop_delegate();
                *effect = DROPEFFECT_NONE;
                return Routed::Rejected;
            }
            DropHint::Shell => self.default_destination.clone(),
            DropHint::ShellTo(dest) => dest,
        };

        let same = self.state.borrow().delegate.as_ref().is_some_and(|d| d.dest == destination);
        if !same {
            self.drop_delegate();
            let sources = self.state.borrow().sources.clone();
            let resolved = match &destination {
                Destination::Item(name) => item_drop_target(self.hwnd, name, &sources).map(|t| (t, None)),
                _ => shell_drop_target(&destination, self.hwnd),
            };
            match resolved {
                Ok((target, dir)) => {
                    self.state.borrow_mut().delegate = Some(Delegate { dest: destination, dir, target, effect: DROPEFFECT_NONE });
                }
                Err(_) => {
                    *effect = DROPEFFECT_NONE;
                    return Routed::Rejected;
                }
            }
            let (target, data) = {
                let state = self.state.borrow();
                (state.delegate.as_ref().map(|d| d.target.clone()), state.data.clone())
            };
            if let (Some(target), Some(data)) = (target, data) {
                // A delegate first met at the drop point still learns which button drags: Drop's own
                // key state carries no mouse button, and the Shell decides its right-drag menu from
                // the key state it saw in DragEnter/DragOver.
                let enter_keys = if dropping { MODIFIERKEYS_FLAGS(keys.0 | (self.state.borrow().last_keys & 0x13)) } else { keys };
                if unsafe { target.DragEnter(&data, enter_keys, pt, effect) }.is_err() {
                    self.drop_delegate();
                    *effect = DROPEFFECT_NONE;
                    return Routed::Rejected;
                }
                if let Some(d) = self.state.borrow_mut().delegate.as_mut() { d.effect = *effect; }
            }
        } else if dropping {
            // OLE semantics: Drop follows the last DragOver; no extra DragOver with button-less keys
            // (it turned a right-drag onto a folder into a plain move without the Shell's menu,
            // live 2026-10-15). The delegate's last answer stands.
            *effect = self.state.borrow().delegate.as_ref().map_or(DROPEFFECT_NONE, |d| d.effect);
        } else {
            let target = self.state.borrow().delegate.as_ref().map(|d| d.target.clone());
            if let Some(target) = target {
                if unsafe { target.DragOver(keys, pt, effect) }.is_err() {
                    *effect = DROPEFFECT_NONE;
                }
                if let Some(d) = self.state.borrow_mut().delegate.as_mut() { d.effect = *effect; }
            }
        }
        Routed::Shell
    }

    fn item_feedback(&self, name: Option<&str>) {
        if let Ok(mut callbacks) = self.callbacks.try_borrow_mut() { (callbacks.item_feedback)(name); }
    }

    fn helper(&self) -> Option<IDropTargetHelper> {
        self.state.borrow().helper.clone()
    }

    /// The Alt-drop decision for the current drag (creation-spec 5): `keys` is the key state of this call,
    /// `allowed` the effects the source allows, `dropping` true at `Drop`. The mouse buttons come from the
    /// last `DragEnter`/`DragOver` because `Drop` carries none. The same function answers at hover and at
    /// drop, so the two cannot disagree about the point.
    fn alt_decision(&self, keys: u32, allowed: DROPEFFECT, dropping: bool) -> crate::creation::AltDrop {
        use crate::creation::{self, AltDrop, AltTarget};
        let (folders, target, last, point) = {
            let state = self.state.borrow();
            if state.internal { return AltDrop::Ordinary }
            // no classifier: nothing was classified (no file items); a classifier that has not finished is None
            let folders = match &state.classifier {
                Some(classifier) => classifier.result(),
                None => Some(Vec::new()),
            };
            (folders, if state.alt_target_ok { AltTarget::Empty } else { AltTarget::Other }, state.last_keys, state.last_point)
        };
        let enabled = self.callbacks.try_borrow().ok().is_some_and(|c| c.quick_create.as_ref().is_some_and(|q| (q.enabled)(point)));
        if !enabled { return AltDrop::Ordinary }
        let buttons = last & (creation::MK_LBUTTON | creation::MK_RBUTTON | creation::MK_MBUTTON);
        let down = |key: VIRTUAL_KEY| unsafe { GetAsyncKeyState(key.0 as i32) } < 0;
        let dk = creation::DropKeys {
            mk: keys & !(creation::MK_LBUTTON | creation::MK_RBUTTON | creation::MK_MBUTTON),
            lmenu: down(VK_LMENU), rmenu: down(VK_RMENU), lctrl: down(VK_LCONTROL), rctrl: down(VK_RCONTROL),
        };
        creation::alt_drop_decision(&dk, buttons, folders.as_deref(), target, allowed.0 & DROPEFFECT_LINK.0 != 0, dropping)
    }

    fn restore_description(&self) {
        let (data, saved) = {
            let mut state = self.state.borrow_mut();
            (state.data.clone(), state.saved_description.take())
        };
        if let (Some(data), Some(saved)) = (data, saved) {
            // An INVALID description clears our feedback when no earlier format was present.
            let bytes = saved.unwrap_or_else(|| drop_description(-1, "", ""));
            let _ = set_bytes_format(&data, CFSTR_DROPDESCRIPTION, &bytes);
        }
    }

    /// Applies an Alt-drop decision to one answer (creation-spec 5.5): the Shell target is left, the effect
    /// becomes `LINK` only (or `NONE` when refused) and, while dragging, the drop description says what
    /// happens.
    fn alt_answer(&self, decision: crate::creation::AltDrop, effect: &mut DROPEFFECT, dropping: bool) {
        use crate::creation::{self, AltDrop, Refusal};
        self.drop_delegate();
        *effect = if matches!(decision, AltDrop::Accept(_)) { DROPEFFECT_LINK } else { DROPEFFECT_NONE };
        let (kind, text) = match decision {
            AltDrop::Accept(count) => (6, creation::alt_drop_tip(count)),
            AltDrop::TooMany => (0, creation::TOO_MANY_TIP.to_string()),
            AltDrop::Refused(Refusal::NoLink) => (0, creation::NO_LINK_TIP.to_string()),
            AltDrop::Refused(Refusal::NotClassified) | AltDrop::Ordinary => return,
        };
        if dropping { return }
        let data = self.state.borrow().data.clone();
        if let Some(data) = data {
            let previous = format_payload(&data, CFSTR_DROPDESCRIPTION);
            if set_bytes_format(&data, CFSTR_DROPDESCRIPTION, &drop_description(kind, &text, "")).is_ok() {
                self.state.borrow_mut().saved_description = Some(previous);
            }
        }
    }
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: Option<&IDataObject>,
        grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        if pdweffect.is_null() {
            return Err(invalid("null effect pointer"));
        }
        let effect = unsafe { &mut *pdweffect };
        let Some(data) = pdataobj else {
            *effect = DROPEFFECT_NONE;
            let _ = self.DragLeave();
            return Ok(());
        };
        // A new drag starts: discard anything left from an interrupted previous one.
        self.drop_delegate();
        self.restore_description();
        *self.state.borrow_mut() = State::default();
        let sources = read_source_names(data);
        let internal = internal_drag_active();
        let quick = self.callbacks.try_borrow().ok().is_some_and(|c| c.quick_create.as_ref().is_some_and(|q| (q.enabled)(POINT { x: pt.x, y: pt.y })));
        // Folder classification runs once per drag on a worker, never on this thread (creation-spec 5.3).
        let classifier = (quick && !internal && !sources.is_empty())
            .then(|| crate::creation::FolderClassifier::spawn(sources.clone(), crate::creation::fs_is_folder));
        let helper = if self.hwnd.is_invalid() {
            None
        } else {
            unsafe { CoCreateInstance::<_, IDropTargetHelper>(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER) }.ok()
        };
        {
            let mut state = self.state.borrow_mut();
            state.data = Some(data.clone());
            state.sources = sources;
            state.internal = internal;
            state.helper = helper;
            state.classifier = classifier;
            state.last_keys = grfkeystate.0;
        }
        self.route(grfkeystate, *pt, effect, true, false);
        if let Some(helper) = self.helper() {
            let point = POINT { x: pt.x, y: pt.y };
            let _ = unsafe { helper.DragEnter(self.hwnd, data, &point, *effect) };
        }
        Ok(())
    }

    fn DragOver(&self, grfkeystate: MODIFIERKEYS_FLAGS, pt: &POINTL, pdweffect: *mut DROPEFFECT) -> Result<()> {
        if pdweffect.is_null() {
            return Err(invalid("null effect pointer"));
        }
        let effect = unsafe { &mut *pdweffect };
        if self.state.borrow().data.is_none() {
            *effect = DROPEFFECT_NONE;
            return Ok(());
        }
        self.state.borrow_mut().last_keys = grfkeystate.0;
        self.route(grfkeystate, *pt, effect, false, false);
        if let Some(helper) = self.helper() {
            let point = POINT { x: pt.x, y: pt.y };
            let _ = unsafe { helper.DragOver(&point, *effect) };
        }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        self.item_feedback(None);
        self.restore_description();
        if let Some(helper) = self.helper() {
            let _ = unsafe { helper.DragLeave() };
        }
        self.drop_delegate();
        *self.state.borrow_mut() = State::default();
        if let Ok(mut callbacks) = self.callbacks.try_borrow_mut() {
            (callbacks.on_leave)();
        }
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: Option<&IDataObject>,
        grfkeystate: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> Result<()> {
        if pdweffect.is_null() {
            return Err(invalid("null effect pointer"));
        }
        let effect = unsafe { &mut *pdweffect };
        let point = POINT { x: pt.x, y: pt.y };
        let Some(data) = pdataobj else {
            *effect = DROPEFFECT_NONE;
            let _ = self.DragLeave();
            return Ok(());
        };
        if let Some(helper) = self.helper() {
            let _ = unsafe { helper.Drop(data, &point, *effect) };
        }
        // Names are read from the data object before the Shell gets it (a move may invalidate it).
        let (mut sources, internal) = {
            let state = self.state.borrow();
            (state.sources.clone(), state.internal)
        };
        if sources.is_empty() {
            sources = read_source_names(data);
        }
        {
            let mut state = self.state.borrow_mut();
            state.data = Some(data.clone());
            state.helper = None;
        }

        // Re-evaluate at the drop point (an unchanged delegate keeps its last answer, see route_inner).
        let allowed = *effect; // what the source allows: the delegate's Drop chooses from this
        let hovered_alt = self.state.borrow().last_alt;
        let routed = self.route(grfkeystate, *pt, effect, false, true);
        match routed {
            // Matched Leave, zero Shell Drop calls, the source is told LINK and nothing else
            // (creation-spec 5.5). Creation is deferred to the app.
            Routed::Alt(crate::creation::AltDrop::Accept(_)) => {
                let folders = self.state.borrow().classifier.as_ref().and_then(|c| c.result()).unwrap_or_default();
                let _ = self.DragLeave();
                *effect = DROPEFFECT_LINK;
                if let Ok(mut callbacks) = self.callbacks.try_borrow_mut() {
                    if let Some(quick) = callbacks.quick_create.as_mut() { (quick.enqueue)(folders, point); }
                }
                return Ok(());
            }
            // refused: too many folders, no LINK, or not classified in time
            Routed::Alt(_) => {
                let _ = self.DragLeave();
                *effect = DROPEFFECT_NONE;
                return Ok(());
            }
            // the last hover offered or refused an Alt-drop but the drop point decided otherwise: the user
            // saw a Gate being created, so the Shell must not get this drop
            _ if hovered_alt => {
                let _ = self.DragLeave();
                *effect = DROPEFFECT_NONE;
                return Ok(());
            }
            _ => {}
        }
        let negotiated = *effect;
        let result: Result<()>;
        let mut returned_effect = None;
        let mut destination = None;
        let mut destination_dir = None;
        match routed {
            Routed::Rejected | Routed::Alt(_) => {
                *effect = DROPEFFECT_NONE;
                result = Ok(());
            }
            Routed::Internal(_) => {
                result = Ok(());
            }
            Routed::Shell => {
                let delegate = self.state.borrow().delegate.as_ref().map(|d| (d.target.clone(), d.dest.clone(), d.dir.clone()));
                match delegate {
                    Some((target, dest, dir)) => {
                        destination = Some(dest.clone());
                        destination_dir = dir;
                        // The Shell may run a modal dialog or a background operation here.
                        result = if negotiated == DROPEFFECT_NONE {
                            let _ = unsafe { target.DragLeave() };
                            Ok(())
                        } else {
                            // Drop gets the source's allowed effects like any OLE target (the right-drag
                            // menu offers Copy/Move/Link from them); it answers the effect it performed.
                            *effect = allowed;
                            unsafe { target.Drop(data, grfkeystate, *pt, effect) }
                        };
                        if let Destination::Item(name) = &dest {
                            crate::app::log(&format!("item drop hand-over: target={name:?} negotiated={} returned={} result={result:?}", negotiated.0, effect.0));
                        }
                        if result.is_err() {
                            *effect = DROPEFFECT_NONE;
                        }
                        returned_effect = Some(*effect);
                    }
                    None => {
                        *effect = DROPEFFECT_NONE;
                        result = Ok(());
                    }
                }
            }
        }

        // The delegate's drop completed; end the drag state before reporting.
        self.state.borrow_mut().delegate = None;
        let expected = match routed {
            Routed::Shell => expected_destinations(&sources, destination_dir.as_deref(), negotiated),
            _ => Vec::new(),
        };
        *self.state.borrow_mut() = State::default();
        self.item_feedback(None);

        // `negotiated` (not the returned effect) gates the report: an optimised move returns NONE.
        if (result.is_ok() && negotiated != DROPEFFECT_NONE) || matches!(destination, Some(Destination::Item(_))) {
            let dropped = Dropped {
                sources,
                expected_destinations: expected,
                destination,
                destination_dir,
                point,
                effect: negotiated,
                target_effect: returned_effect,
                internal,
            };
            if let Ok(mut callbacks) = self.callbacks.try_borrow_mut() {
                (callbacks.on_dropped)(&dropped);
            }
        } else if let Ok(mut callbacks) = self.callbacks.try_borrow_mut() {
            (callbacks.on_leave)();
        }
        result
    }
}

/// A live `RegisterDragDrop` registration; dropping it revokes the registration.
pub struct Registration {
    hwnd: HWND,
    _target: IDropTarget,
}

impl Registration {
    /// Revoke now (equivalent to dropping).
    pub fn revoke(self) {}
}

impl Drop for Registration {
    fn drop(&mut self) {
        // Fails harmlessly when the window was already destroyed.
        let _ = unsafe { RevokeDragDrop(self.hwnd) };
    }
}

/// Registers `target` as the drop target of `hwnd` (`RegisterDragDrop`). The calling thread must be
/// the window's thread and must have called [`init_ole`]. Fails with `DRAGDROP_E_ALREADYREGISTERED`
/// if the window already has a target.
pub fn register(hwnd: HWND, target: DropTarget) -> Result<Registration> {
    let target: IDropTarget = target.into();
    unsafe { RegisterDragDrop(hwnd, &target)? };
    Ok(Registration { hwnd, _target: target })
}

// ---------------------------------------------------------------------------------------------
// Drag source
// ---------------------------------------------------------------------------------------------

/// A drag image: an HBITMAP (32 bpp straight-alpha BGRA) and the cursor hot spot inside it.
/// The bitmap is owned: it is deleted on drop unless [`begin_drag`] handed it to the Shell.
pub struct DragImage {
    bitmap: HBITMAP,
    size: SIZE,
    hotspot: POINT,
}

impl DragImage {
    /// Takes ownership of `bitmap`. Its pixels must have straight (not premultiplied) alpha:
    /// `IDragSourceHelper::InitializeFromBitmap` performs the multiplication itself.
    pub fn from_hbitmap(bitmap: HBITMAP, width: i32, height: i32, hotspot: (i32, i32)) -> DragImage {
        DragImage { bitmap, size: SIZE { cx: width, cy: height }, hotspot: POINT { x: hotspot.0, y: hotspot.1 } }
    }

    /// Builds a straight-alpha 32 bpp top-down DIB from premultiplied BGRA pixels
    /// (`width * height * 4` bytes, the format of `shell::IconBitmap` and `render::Surface`).
    /// Only this Shell-bound copy is unpremultiplied; the caller's layered-window pixels stay
    /// premultiplied. `hotspot` is the cursor position relative to the image's top-left corner.
    pub fn from_bgra_premultiplied(width: u32, height: u32, pixels: &[u8], hotspot: (i32, i32)) -> Result<DragImage> {
        let pixels = bitmap::straight_bgra(width, height, pixels)
            .ok_or_else(|| invalid("drag image pixel buffer does not match its size"))?;
        let mut info = BITMAPINFO::default();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)? };
        if bits.is_null() {
            unsafe {
                let _ = DeleteObject(bitmap);
            }
            return Err(err("CreateDIBSection returned no pixel buffer"));
        }
        unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len()) };
        Ok(DragImage::from_hbitmap(bitmap, width as i32, height as i32, hotspot))
    }

    pub fn size(&self) -> (i32, i32) {
        (self.size.cx, self.size.cy)
    }

    pub fn hbitmap(&self) -> HBITMAP {
        self.bitmap
    }
}

impl Drop for DragImage {
    fn drop(&mut self) {
        if !self.bitmap.is_invalid() {
            unsafe {
                let _ = DeleteObject(self.bitmap);
            }
        }
    }
}

/// `DSH_ALLOWDROPDESCRIPTIONTEXT`: the source lets the targets show their drop-description text
/// ("Move to <folder>"). Observed (offline probe, Windows 11 26200): the Shell folder targets write
/// `DropDescription` into the data object either way; the flag decides whether the target-side
/// drag-image window turns the text on (`IsShowingText` 1 with the flag, absent without). The flag is
/// delivered through `IDragSourceHelper2::SetFlags`, which is one-shot process-wide state consumed by
/// the next `Initialize*` call (see [`ensure_drop_description_flag`]).
const DSH_ALLOWDROPDESCRIPTIONTEXT: u32 = 0x1;

/// The shell drag-drop helper of a drag source with drop descriptions switched on (before any
/// `Initialize*` call, as the documentation requires). A shell older than `IDragSourceHelper2`
/// (pre-Vista) still gets the plain helper.
fn drag_source_helper() -> Result<IDragSourceHelper> {
    drag_source_helper_with(true)
}

/// [`drag_source_helper`] with the drop-description flag optional (the offline probe compares both).
fn drag_source_helper_with(allow_drop_description: bool) -> Result<IDragSourceHelper> {
    let helper: IDragSourceHelper = unsafe { CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER)? };
    if allow_drop_description {
        match helper.cast::<IDragSourceHelper2>() {
            Ok(helper2) => {
                if let Err(e) = unsafe { helper2.SetFlags(DSH_ALLOWDROPDESCRIPTIONTEXT) } {
                    crate::app::log(&format!("drag source helper: SetFlags failed: {e}"));
                }
            }
            Err(e) => crate::app::log(&format!("drag source helper: no IDragSourceHelper2: {e}")),
        }
    }
    Ok(helper)
}

/// Stores a DWORD private format in `data` (`IDataObject::SetData`, the data object takes the
/// `HGLOBAL`). The Shell data objects accept arbitrary formats.
fn set_dword_format(data: &IDataObject, name: &str, value: u32) -> Result<()> {
    let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, 4)? };
    let ptr = unsafe { GlobalLock(hglobal) } as *mut u32;
    if ptr.is_null() {
        let error = Error::from_win32();
        unsafe {
            let _ = GlobalFree(hglobal);
        }
        return Err(error);
    }
    unsafe {
        ptr.write_unaligned(value);
        let _ = GlobalUnlock(hglobal);
    }
    let mut medium = STGMEDIUM::default();
    medium.tymed = TYMED_HGLOBAL.0 as u32;
    medium.u.hGlobal = hglobal;
    let format = hglobal_format(clipboard_format(name));
    let result = unsafe { data.SetData(&format, &medium, true) };
    if result.is_err() {
        // the data object did not take ownership
        unsafe {
            let _ = GlobalFree(hglobal);
        }
    }
    result
}

/// Makes sure the data object carries `DragSourceHelperFlags = DSH_ALLOWDROPDESCRIPTIONTEXT` after an
/// `Initialize*` call. Observed (offline probe): `IDragSourceHelper2::SetFlags` is *one-shot,
/// process-wide* state that the next `Initialize*` call on any helper consumes and writes into its
/// data object, so another `Initialize*` (another thread, another shell component) between our two
/// calls steals it and the data object ends up without the flag - and then the targets' helper shows
/// no drop-description text. Writing the format ourselves when it is missing closes that window.
fn ensure_drop_description_flag(data: &IDataObject) {
    let present = format_payload(data, CFSTR_DRAGSOURCEHELPERFLAGS).and_then(payload_u32);
    if present.is_some_and(|f| f & DSH_ALLOWDROPDESCRIPTIONTEXT != 0) {
        return;
    }
    let result = set_dword_format(data, CFSTR_DRAGSOURCEHELPERFLAGS, DSH_ALLOWDROPDESCRIPTIONTEXT);
    crate::app::log(&format!(
        "drag source helper: DragSourceHelperFlags was {present:?} after Initialize; wrote {DSH_ALLOWDROPDESCRIPTIONTEXT} -> {:?}",
        result.as_ref().map_err(|e| e.code())
    ));
}

/// Prepares a drag without an image for drop descriptions: the helper still registers the drag
/// context (`InitializeFromWindow(NULL, NULL, data)`, the documented "no image" form), so shell
/// targets can show "Move to <folder>" next to the plain cursor.
pub fn set_no_drag_image(data: &IDataObject) -> Result<()> {
    let helper = drag_source_helper()?;
    unsafe { helper.InitializeFromWindow(HWND::default(), None, data)? };
    ensure_drop_description_flag(data);
    Ok(())
}

/// Attaches `image` to `data` through `IDragSourceHelper::InitializeFromBitmap`, with drop
/// descriptions allowed (see [`DSH_ALLOWDROPDESCRIPTIONTEXT`]). On success the Shell owns the
/// bitmap; on failure it is deleted with `image`.
pub fn set_drag_image(data: &IDataObject, image: DragImage) -> Result<()> {
    let helper = drag_source_helper()?;
    let shdi = SHDRAGIMAGE {
        sizeDragImage: image.size,
        ptOffset: image.hotspot,
        hbmpDragImage: image.bitmap,
        crColorKey: windows::Win32::Foundation::COLORREF(CLR_NONE),
    };
    unsafe { helper.InitializeFromBitmap(&shdi, data)? };
    // The drag-image manager owns the bitmap now.
    let mut image = image;
    image.bitmap = HBITMAP::default();
    ensure_drop_description_flag(data);
    Ok(())
}

/// What a finished drag reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DragOutcome {
    /// Effect the target reported (`DROPEFFECT_NONE` when cancelled or not dropped).
    pub effect: DROPEFFECT,
    /// True when the user cancelled (Esc / released over nothing is `effect == NONE` without cancel).
    pub cancelled: bool,
    /// `CFSTR_PERFORMEDDROPEFFECT` as set by the target on the data object, when present.
    pub performed_effect: Option<DROPEFFECT>,
    /// `CFSTR_LOGICALPERFORMEDDROPEFFECT`, when present.
    pub logical_performed_effect: Option<DROPEFFECT>,
}

impl DragOutcome {
    /// The drag ended in a move (the Shell moved the files, or asked the source to remove them).
    /// Informational: the desktop enumeration/change notifications are authoritative.
    pub fn moved(&self) -> bool {
        self.effect == DROPEFFECT_MOVE
    }

    pub fn copied(&self) -> bool {
        self.effect == DROPEFFECT_COPY
    }

    pub fn linked(&self) -> bool {
        self.effect == DROPEFFECT_LINK
    }

    /// True when the target left the originals for the *source* to remove (an unoptimised move: the
    /// target set `Performed DropEffect` to MOVE). An optimised move sets it to NONE and moves the
    /// files itself. Explorer deletes only on this explicit signal; Corral never deletes here -
    /// see the module documentation.
    pub fn source_should_delete(&self) -> bool {
        self.effect == DROPEFFECT_MOVE && self.performed_effect == Some(DROPEFFECT_MOVE)
    }
}

const CFSTR_ISSHOWINGLAYERED: &str = "IsShowingLayered";
const CFSTR_ISSHOWINGTEXT: &str = "IsShowingText";
const CFSTR_DRAGWINDOW: &str = "DragWindow";
const CFSTR_DROPDESCRIPTION: &str = "DropDescription";

fn drop_description(kind: i32, message: &str, insert: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(1044);
    bytes.extend_from_slice(&kind.to_le_bytes());
    for text in [message, insert] {
        let mut wide = [0u16; 260];
        for (slot, value) in wide[..259].iter_mut().zip(text.encode_utf16()) { *slot = value; }
        for value in wide { bytes.extend_from_slice(&value.to_le_bytes()); }
    }
    bytes
}

fn set_bytes_format(data: &IDataObject, name: &str, bytes: &[u8]) -> Result<()> {
    let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len())? };
    let ptr = unsafe { GlobalLock(hglobal) } as *mut u8;
    if ptr.is_null() {
        unsafe { let _ = GlobalFree(hglobal); }
        return Err(Error::from_win32());
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
        let _ = GlobalUnlock(hglobal);
    }
    let mut medium = STGMEDIUM::default();
    medium.tymed = TYMED_HGLOBAL.0 as u32;
    medium.u.hGlobal = hglobal;
    let result = unsafe { data.SetData(&hglobal_format(clipboard_format(name)), &medium, true) };
    if result.is_err() { unsafe { let _ = GlobalFree(hglobal); } }
    result
}
/// Written by the drag helper when `SetFlags` was pending at an `Initialize*` call.
const CFSTR_DRAGSOURCEHELPERFLAGS: &str = "DragSourceHelperFlags";
/// `CFSTR_INDRAGLOOP`: nonzero while the data object is inside a drag-and-drop loop.
const CFSTR_INDRAGLOOP: &str = "InShellDragLoop";
/// `DDWM_SETCURSOR` / `DDWM_UPDATEWINDOW` (`WM_USER + 2` / `WM_USER + 3`, shlobj_core.h and the
/// DDWM_UPDATEWINDOW documentation): messages the drag-image window of the drag helper answers.
const DDWM_SETCURSOR: u32 = 0x0400 + 2;
const DDWM_UPDATEWINDOW: u32 = 0x0400 + 3;

/// The drop-target feedback the Shell keeps in the drag data object: what a target wrote
/// (`DropDescription`) and what the drag helper reports back (`IsShowingLayered`, `IsShowingText`,
/// `DragWindow`). `None` fields were absent from the data object.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FeedbackState {
    effect: u32,
    layered: Option<u32>,
    text: Option<u32>,
    drag_window: Option<u32>,
    /// `DragSourceHelperFlags` (1 = the source allows drop-description text).
    flags: Option<u32>,
    /// `InShellDragLoop`.
    in_loop: Option<u32>,
    /// `DROPDESCRIPTION`: type, message, insert.
    description: Option<(i32, String, String)>,
}

impl FeedbackState {
    fn describe(&self) -> String {
        let flag = |v: Option<u32>| v.map_or("absent".to_string(), |v| v.to_string());
        let description = match &self.description {
            Some((kind, message, insert)) => format!("type={kind} message={message:?} insert={insert:?}"),
            None => "absent".to_string(),
        };
        format!(
            "effect={} IsShowingLayered={} IsShowingText={} DragSourceHelperFlags={} InShellDragLoop={} DragWindow={} DropDescription: {description}",
            self.effect,
            flag(self.layered),
            flag(self.text),
            flag(self.flags),
            flag(self.in_loop),
            self.drag_window.map_or("absent".to_string(), |w| format!("{w:#x}")),
        )
    }
}

/// UTF-16 text up to the first NUL.
fn wide_text(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    String::from_utf16_lossy(&units)
}

/// Bytes of a private clipboard format of `data` (`None` when absent). Read-only.
fn format_payload(data: &IDataObject, name: &str) -> Option<Vec<u8>> {
    let medium = get_hglobal(data, clipboard_format(name)).ok()?;
    let hglobal = medium.hglobal();
    let size = unsafe { GlobalSize(hglobal) };
    let ptr = unsafe { GlobalLock(hglobal) } as *const u8;
    if ptr.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr, size) }.to_vec();
    unsafe {
        let _ = GlobalUnlock(hglobal);
    }
    Some(bytes)
}

fn payload_u32(bytes: Vec<u8>) -> Option<u32> {
    read_u32(&bytes, 0)
}

/// Reads the feedback formats of `data`. `DROPDESCRIPTION` is `{ int type; WCHAR szMessage[260];
/// WCHAR szInsert[260] }` = 1044 bytes.
fn feedback_state(data: &IDataObject, effect: DROPEFFECT) -> FeedbackState {
    let description = format_payload(data, CFSTR_DROPDESCRIPTION).and_then(|b| {
        if b.len() < 4 + 520 + 520 {
            return None;
        }
        let kind = i32::from_le_bytes([b[0], b[1], b[2], b[3]]);
        Some((kind, wide_text(&b[4..524]), wide_text(&b[524..1044])))
    });
    FeedbackState {
        effect: effect.0,
        layered: format_payload(data, CFSTR_ISSHOWINGLAYERED).and_then(payload_u32),
        text: format_payload(data, CFSTR_ISSHOWINGTEXT).and_then(payload_u32),
        drag_window: format_payload(data, CFSTR_DRAGWINDOW).and_then(payload_u32),
        flags: format_payload(data, CFSTR_DRAGSOURCEHELPERFLAGS).and_then(payload_u32),
        in_loop: format_payload(data, CFSTR_INDRAGLOOP).and_then(payload_u32),
        description,
    }
}

/// `IDropSource`: ends on button release, cancels on Esc or when the other mouse button goes down
/// (the standard OLE source behaviour: right button during a left drag = cancel), default cursors.
#[implement(IDropSource)]
struct DropSource {
    button_mask: u32,
    /// The drag's data object: the Shell targets and the drag helper keep their feedback state in it.
    data: IDataObject,
    /// Last logged state (a line is written only when it changes).
    last: RefCell<Option<FeedbackState>>,
    /// Refresh state is independent of optional diagnostic logging.
    feedback: RefCell<Option<FeedbackState>>,
}

impl DropSource {
    fn new(button_mask: u32, data: IDataObject) -> DropSource {
        DropSource {
            button_mask,
            data,
            last: RefCell::new(None),
            feedback: RefCell::new(None),
        }
    }

    /// One log line per change of the target state (never per mouse move).
    fn log_change(&self, origin: &str, effect: DROPEFFECT) {
        if std::env::var_os("CORRAL_LOG").is_none() {
            return;
        }
        let state = feedback_state(&self.data, effect);
        let mut last = self.last.borrow_mut();
        if last.as_ref() != Some(&state) {
            crate::app::log(&format!("drag source {origin}: {}", state.describe()));
            *last = Some(state);
        }
    }
}

/// Decision of `IDropSource::QueryContinueDrag` for the dragging button `button_mask`
/// (`MK_LBUTTON` or `MK_RBUTTON`) and the current `MK_*` state.
fn continue_drag(escape_pressed: bool, keys: u32, button_mask: u32) -> windows::core::HRESULT {
    let other_button = (MK_LBUTTON.0 | MK_RBUTTON.0) & !button_mask;
    if escape_pressed || keys & other_button != 0 {
        DRAGDROP_S_CANCEL
    } else if keys & button_mask == 0 {
        DRAGDROP_S_DROP
    } else {
        S_OK
    }
}

/// The Shell's drag-image window (class `SysDragImage`) created by the drag-source helper in this
/// process. Anything else named by `DragWindow` is not ours to message.
fn is_own_drag_window(hwnd: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowThreadProcessId, IsWindow};
    if hwnd.0.is_null() || !unsafe { IsWindow(hwnd) }.as_bool() {
        return false;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    if pid != std::process::id() {
        return false;
    }
    let mut class = [0u16; 32];
    let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
    String::from_utf16_lossy(&class[..len.min(class.len())]) == "SysDragImage"
}

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: windows::Win32::Foundation::BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> windows::core::HRESULT {
        let decision = continue_drag(fescapepressed.as_bool(), grfkeystate.0, self.button_mask);
        if decision != S_OK {
            // the final state before the drag ends (drop or cancel)
            let effect = self.last.borrow().as_ref().map_or(DROPEFFECT_NONE.0, |s| s.effect);
            self.log_change(if decision == DRAGDROP_S_DROP { "drop" } else { "cancel" }, DROPEFFECT(effect));
        }
        decision
    }

    fn GiveFeedback(&self, dweffect: DROPEFFECT) -> windows::core::HRESULT {
        self.log_change("feedback", dweffect);
        let state = feedback_state(&self.data, dweffect);
        // DragWindow contains a DWORD HWND, including on 64-bit Windows.
        if let Some(window) = feedback::refresh(self.feedback.borrow().as_ref(), &state, state.drag_window) {
            // Drop targets may write DragWindow: post only to the Shell's own drag-image window.
            let hwnd = HWND(window as usize as *mut c_void);
            if is_own_drag_window(hwnd) {
                let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                    hwnd, DDWM_UPDATEWINDOW,
                    windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0),
                ) };
            } else {
                crate::app::log(&format!("drag feedback: DragWindow {window:#x} is not this process's SysDragImage; no refresh"));
            }
        }
        let layered = state.layered.unwrap_or(0) != 0;
        *self.feedback.borrow_mut() = Some(state);
        if layered {
            if let Ok(arrow) = unsafe { LoadCursorW(None, IDC_ARROW) } {
                unsafe { SetCursor(arrow) };
                return feedback_result(true, true);
            }
        }
        feedback_result(layered, false)
    }
}

fn feedback_result(layered: bool, cursor_set: bool) -> windows::core::HRESULT {
    if feedback::uses_arrow(Some(u32::from(layered)), cursor_set) { S_OK } else { DRAGDROP_S_USEDEFAULTCURSORS }
}

/// `MK_LBUTTON` or `MK_RBUTTON`, whichever logical button is held at the start of the drag.
fn initial_button_mask() -> u32 {
    unsafe {
        let down = |vk: i32| GetAsyncKeyState(vk) as u16 & 0x8000 != 0;
        let (physical_left, physical_right) = (down(VK_LBUTTON.0 as i32), down(VK_RBUTTON.0 as i32));
        // GetAsyncKeyState reports physical buttons; MK_* flags are logical.
        let (left, right) =
            if GetSystemMetrics(SM_SWAPBUTTON) != 0 { (physical_right, physical_left) } else { (physical_left, physical_right) };
        if right && !left { MK_RBUTTON.0 } else { MK_LBUTTON.0 }
    }
}

/// Runs a modal OLE drag of the given Shell items (all with the same parent folder) offering
/// COPY, MOVE and LINK, and reports what the target did. `hwnd` owns the Shell objects.
///
/// Must be called on the UI thread after [`init_ole`], from a mouse handler with a button held
/// (the drag ends when that button is released). The call returns when the drag ends. While it
/// runs, drops on Corral's own windows are marked `internal` (see the module documentation).
/// `drag_image` is optional; it is consumed either way.
///
/// Nothing is copied, moved or deleted here: the Shell targets do that, and a failure to build the
/// data object returns `Err` before any drag starts.
pub fn begin_drag(hwnd: HWND, parsing_names: &[String], drag_image: Option<DragImage>) -> Result<DragOutcome> {
    begin_drag_as(hwnd, parsing_names, drag_image, true)
}

/// [`begin_drag`]; `internal` false makes Corral's own windows treat the drop as one from Explorer (a Shell
/// file operation), as files a Gate shows need: they are not Corral references.
pub fn begin_drag_as(hwnd: HWND, parsing_names: &[String], drag_image: Option<DragImage>, internal: bool) -> Result<DragOutcome> {
    let data = data_object_for(hwnd, parsing_names)?;
    match drag_image {
        // A missing drag image only costs the picture; the drag proceeds.
        Some(image) => {
            let _ = set_drag_image(&data, image);
        }
        // No image: still register the drag context so targets can show drop descriptions.
        None => {
            let _ = set_no_drag_image(&data);
        }
    }
    // CFSTR_INDRAGLOOP: tell the targets the data object is inside a drag loop (the Shell's own loop
    // does; a missing value means "not in a loop"). Reset below.
    if let Err(e) = set_dword_format(&data, CFSTR_INDRAGLOOP, 1) {
        crate::app::log(&format!("drag source: could not set {CFSTR_INDRAGLOOP}: {e}"));
    }
    if std::env::var_os("CORRAL_LOG").is_some() {
        crate::app::log(&format!("drag source begin: {}", feedback_state(&data, DROPEFFECT_NONE).describe()));
    }
    let source: IDropSource = DropSource::new(initial_button_mask(), data.clone()).into();
    let guard = internal.then(|| InternalDragGuard::enter(parsing_names));
    let mut effect = DROPEFFECT_NONE;
    let hr = unsafe { DoDragDrop(&data, &source, ALL_EFFECTS, &mut effect) };
    drop(guard);
    let _ = set_dword_format(&data, CFSTR_INDRAGLOOP, 0);
    crate::app::log(&format!("drag source end: hr={:#x} effect={}", hr.0, effect.0));
    if hr.is_err() {
        return Err(Error::from(hr));
    }
    let cancelled = hr == DRAGDROP_S_CANCEL;
    if cancelled {
        effect = DROPEFFECT_NONE;
    }
    let performed_effect = read_dword(&data, clipboard_format(CFSTR_PERFORMEDDROPEFFECT)).map(DROPEFFECT);
    let logical_performed_effect = read_dword(&data, clipboard_format(CFSTR_LOGICALPERFORMEDDROPEFFECT)).map(DROPEFFECT);
    Ok(DragOutcome { effect, cancelled, performed_effect, logical_performed_effect })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::DRAGDROP_E_ALREADYREGISTERED;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, IAdviseSink};
    use windows::Win32::System::Memory::{GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc};
    use windows::Win32::UI::Shell::DROPFILES;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, DispatchMessageW, HWND_MESSAGE, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
        WINDOW_EX_STYLE, WS_OVERLAPPED,
    };
    use windows::core::{AsImpl, w};

    #[test]
    fn q38b_shell_store_round_trips_arbitrary_private_formats_without_touching_file_formats() {
        let _ole = init_ole().expect("OLE");
        // Existing tracked input, read only; no scratch files or file operations required.
        let name = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let data = data_object_for(HWND::default(), &[name.to_string_lossy().into_owned()]).unwrap();
        let names_before = read_source_names(&data);
        let pidls_before = format_payload(&data, CFSTR_SHELLIDLIST).unwrap();
        let hdrop_before = names_from_hdrop(&data).unwrap();
        for (format, bytes) in [
            ("Corral.Test.Arbitrary.Private", vec![0, 255, 13, 0, 42]),
            (CFSTR_ISSHOWINGLAYERED, 1u32.to_le_bytes().to_vec()),
            (CFSTR_ISSHOWINGTEXT, 1u32.to_le_bytes().to_vec()),
            (CFSTR_DRAGWINDOW, 42u32.to_le_bytes().to_vec()),
            ("IsComputingImage", 0u32.to_le_bytes().to_vec()),
            ("UsingDefaultDragImage", 0u32.to_le_bytes().to_vec()),
            ("DisableDragText", 0u32.to_le_bytes().to_vec()),
            (CFSTR_DROPDESCRIPTION, drop_description(2, "Move to %1", "Desktop")),
        ] {
            set_bytes_format(&data, format, &bytes).unwrap();
            let returned = format_payload(&data, format).unwrap();
            // GlobalAlloc may round the allocation size up; logical bytes must survive exactly.
            assert_eq!(&returned[..bytes.len()], bytes.as_slice(), "{format}");
        }
        set_bytes_format(&data, "Corral.Test.Arbitrary.Private", &[9, 8, 7, 6]).unwrap();
        assert_eq!(&format_payload(&data, "Corral.Test.Arbitrary.Private").unwrap()[..4], &[9,8,7,6]);
        assert_eq!(read_source_names(&data), names_before);
        assert_eq!(format_payload(&data, CFSTR_SHELLIDLIST).unwrap(), pidls_before);
        assert_eq!(names_from_hdrop(&data).unwrap(), hdrop_before);
    }

    /// The Shell's drag-image helper keeps process-wide state: two threads inside
    /// `SetFlags`/`Initialize*` at once lose the "DragSourceHelperFlags" write on one data object
    /// (observed: 17 of 20 parallel runs of the module failed, 0 of 20 serial). A real drag has
    /// one UI thread and one drag at a time, so only these tests need to serialise.
    fn helper_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[implement(IDropTarget)]
    struct CountingTarget {
        drops: std::rc::Rc<Cell<usize>>,
        leaves: std::rc::Rc<Cell<usize>>,
    }

    #[implement(IDropTarget)]
    struct ItemTestTarget {
        calls: Rc<RefCell<Vec<(&'static str, u32, u32, usize)>>>,
        refuse: bool,
        fail: bool,
    }

    impl IDropTarget_Impl for ItemTestTarget_Impl {
        fn DragEnter(&self, data: Option<&IDataObject>, keys: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            self.calls.borrow_mut().push(("enter", keys.0, unsafe { (*effect).0 }, data.map_or(0, |d| d.as_raw() as usize)));
            Ok(())
        }
        fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            self.calls.borrow_mut().push(("over", keys.0, unsafe { (*effect).0 }, 0));
            if self.refuse { unsafe { *effect = DROPEFFECT_NONE; } }
            Ok(())
        }
        fn DragLeave(&self) -> Result<()> {
            self.calls.borrow_mut().push(("leave", 0, 0, 0));
            Ok(())
        }
        fn Drop(&self, data: Option<&IDataObject>, keys: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
            self.calls.borrow_mut().push(("drop", keys.0, unsafe { (*effect).0 }, data.map_or(0, |d| d.as_raw() as usize)));
            unsafe { *effect = DROPEFFECT_NONE; } // optimized move / dismissed Shell menu
            if self.fail { Err(err("mock failure")) } else { Ok(()) }
        }
    }

    /// In-memory vtables only: no Shell, OleInitialize, registration, windows or file writes.
    #[test]
    fn q35_no_shell_item_forwards_original_data_keys_and_never_predicts_placement() {
        for (internal, keys, refuse, fail) in [(true, 0, false, false), (false, 8, false, false),
            (false, 4, false, false), (true, 0x20, false, false), (true, 2, false, false),
            (false, 0, true, false), (true, 0, false, true)] {
            let calls = Rc::new(RefCell::new(Vec::new()));
            let reports = Rc::new(RefCell::new(Vec::new()));
            let reported = reports.clone();
            let dest = Destination::Item("mock-item-never-bound".into());
            let hover_dest = dest.clone();
            let callbacks = DropCallbacks::new(move |_| DropHint::ShellTo(hover_dest.clone()), move |d| reported.borrow_mut().push(d.clone()))
                .with_quick_create(|_| true, |_, _| panic!("Alt over item must not create a Gate"));
            let data: IDataObject = HdropOnly { paths: vec![] }.into();
            let fake: IDropTarget = ItemTestTarget { calls: calls.clone(), refuse, fail }.into();
            let pt = POINTL { x: 10, y: 20 };
            let mut effect = ALL_EFFECTS;
            unsafe { fake.DragEnter(&data, MODIFIERKEYS_FLAGS(keys), pt, &mut effect) }.unwrap();
            unsafe { fake.DragOver(MODIFIERKEYS_FLAGS(keys), pt, &mut effect) }.unwrap(); // the last hover
            let object = DropTarget::new(HWND::default(), Destination::Desktop, callbacks);
            *object.state.borrow_mut() = State { data: Some(data.clone()), sources: vec!["source".into()], internal,
                classifier: Some(crate::creation::FolderClassifier::finished(vec!["C:\\cached-folder".into()])), last_keys: 1,
                delegate: Some(Delegate { dest: dest.clone(), dir: None, target: fake, effect }), ..State::default() };
            let target: IDropTarget = object.into();
            let result = unsafe { target.Drop(&data, MODIFIERKEYS_FLAGS(keys), pt, &mut effect) };
            assert_eq!(result.is_err(), fail);
            assert_eq!(effect, DROPEFFECT_NONE);
            let calls = calls.borrow();
            assert_eq!(calls[0], ("enter", keys, ALL_EFFECTS.0, data.as_raw() as usize));
            // the hover's DragOver (above), then no extra DragOver at Drop (OLE semantics); Drop gets
            // the source's allowed effects
            assert_eq!(calls[1], ("over", keys, ALL_EFFECTS.0, 0));
            assert_eq!(calls[2].0, if refuse { "leave" } else { "drop" });
            if !refuse { assert_eq!(calls[2], ("drop", keys, ALL_EFFECTS.0, data.as_raw() as usize)); }
            assert_eq!(calls.len(), 3);
            let reports = reports.borrow();
            assert_eq!(reports.len(), 1); // item failure / refusal also requests reload only
            assert_eq!(reports[0].destination, Some(dest));
            assert!(reports[0].expected_destinations.is_empty());
        }
    }

    #[test]
    fn q35_no_shell_leaving_item_restores_internal_route_and_clears_feedback() {
        let leaves = Rc::new(Cell::new(0));
        let highlights = Rc::new(RefCell::new(Vec::new()));
        let feedback = highlights.clone();
        let object = DropTarget::new(HWND::default(), Destination::Desktop,
            DropCallbacks::new(|_| DropHint::Internal(DROPEFFECT_MOVE), |_| panic!("hover cannot place items"))
                .with_item_feedback(move |n| feedback.borrow_mut().push(n.map(str::to_owned))));
        let fake: IDropTarget = CountingTarget { drops: Rc::new(Cell::new(0)), leaves: leaves.clone() }.into();
        object.state.borrow_mut().delegate = Some(Delegate { dest: Destination::Item("mock".into()), dir: None, target: fake, effect: DROPEFFECT_NONE });
        let mut effect = ALL_EFFECTS;
        assert!(matches!(object.route(MODIFIERKEYS_FLAGS(8), POINTL::default(), &mut effect, false, false), Routed::Internal(_)));
        assert_eq!(effect, DROPEFFECT_MOVE);
        assert_eq!(leaves.get(), 1);
        assert_eq!(&*highlights.borrow(), &[None]);
        let target: IDropTarget = object.into();
        unsafe { target.DragLeave() }.unwrap(); // Esc / leaving window: no Drop
        assert_eq!(leaves.get(), 1);
    }

    impl IDropTarget_Impl for CountingTarget_Impl {
        fn DragEnter(&self, _: Option<&IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, _: *mut DROPEFFECT) -> Result<()> { Ok(()) }
        fn DragOver(&self, _: MODIFIERKEYS_FLAGS, _: &POINTL, _: *mut DROPEFFECT) -> Result<()> { Ok(()) }
        fn DragLeave(&self) -> Result<()> { self.leaves.set(self.leaves.get() + 1); Ok(()) }
        fn Drop(&self, _: Option<&IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, _: *mut DROPEFFECT) -> Result<()> {
            self.drops.set(self.drops.get() + 1); Ok(())
        }
    }

    /// What the classifier of a test drag looks like.
    enum Cls {
        Absent,
        Pending,
        Done(Vec<String>),
    }

    impl Cls {
        fn build(self) -> Option<crate::creation::FolderClassifier> {
            match self {
                Cls::Absent => None,
                Cls::Pending => Some(crate::creation::FolderClassifier::pending()),
                Cls::Done(list) => Some(crate::creation::FolderClassifier::finished(list)),
            }
        }
    }

    /// In-memory COM vtables only: no CoCreateInstance, Shell target, filesystem, or window. The Alt-drop
    /// (creation-spec 5, 8.8 to 8.10) never reaches the Shell's `Drop`, answers `LINK` and nothing else,
    /// and refuses with `NONE`.
    #[test]
    fn q18_no_shell_alt_drop_never_drops_answers_link_and_ordinary_drops_stay_ordinary() {
        let link_only = DROPEFFECT(DROPEFFECT_COPY.0 | DROPEFFECT_MOVE.0);
        let folders = |n: usize| (0..n).map(|i| format!("C:\\F{i}")).collect::<Vec<_>>();
        // (keys, reject, buttons, classifier, allowed, expected shell drops, expected created batches, effect when no Shell drop)
        let rows: Vec<(u32, bool, u32, Cls, DROPEFFECT, usize, Vec<usize>, DROPEFFECT)> = vec![
            (0x20, false, 1, Cls::Done(folders(1)), ALL_EFFECTS, 0, vec![1], DROPEFFECT_LINK),
            (0x20, false, 1, Cls::Done(folders(3)), ALL_EFFECTS, 0, vec![3], DROPEFFECT_LINK),
            (0x20, false, 1, Cls::Done(folders(6)), ALL_EFFECTS, 0, vec![6], DROPEFFECT_LINK),
            (0x20, false, 1, Cls::Done(folders(7)), ALL_EFFECTS, 0, vec![], DROPEFFECT_NONE),
            (0x20, false, 1, Cls::Done(folders(7)), link_only, 0, vec![], DROPEFFECT_NONE),
            (0x20, true, 1, Cls::Done(folders(1)), ALL_EFFECTS, 0, vec![], DROPEFFECT_NONE),
            (0x20, false, 2, Cls::Done(folders(1)), ALL_EFFECTS, 1, vec![], DROPEFFECT_NONE),
            (0x20 | 0x8, false, 1, Cls::Done(folders(1)), ALL_EFFECTS, 1, vec![], DROPEFFECT_NONE),
            (0x20 | 0x4, false, 1, Cls::Done(folders(1)), ALL_EFFECTS, 1, vec![], DROPEFFECT_NONE),
            (0, false, 1, Cls::Done(folders(1)), ALL_EFFECTS, 1, vec![], DROPEFFECT_NONE),
            // a clear Alt-drop whose classifier has not finished is refused, not handed to the Shell
            (0x20, false, 1, Cls::Pending, ALL_EFFECTS, 0, vec![], DROPEFFECT_NONE),
            // no Alt: a pending classifier changes nothing
            (0, false, 1, Cls::Pending, ALL_EFFECTS, 1, vec![], DROPEFFECT_NONE),
            // nothing was classified (no file items) or no folder among them: the ordinary handler
            (0x20, false, 1, Cls::Absent, ALL_EFFECTS, 1, vec![], DROPEFFECT_NONE),
            (0x20, false, 1, Cls::Done(vec![]), ALL_EFFECTS, 1, vec![], DROPEFFECT_NONE),
            // a clear Alt-drop from a source without LINK is refused, never handed to the Shell
            (0x20, false, 1, Cls::Done(folders(1)), link_only, 0, vec![], DROPEFFECT_NONE),
            // ... but without the Alt intent a missing LINK changes nothing
            (0, false, 1, Cls::Done(folders(1)), link_only, 1, vec![], DROPEFFECT_NONE),
        ];
        for (index, (keys, reject, buttons, classifier, allowed, expected_drops, expected_batches, expected_effect)) in rows.into_iter().enumerate() {
            let drops = std::rc::Rc::new(Cell::new(0));
            let leaves = std::rc::Rc::new(Cell::new(0));
            let batches = std::rc::Rc::new(RefCell::new(Vec::new()));
            let created = batches.clone();
            let callbacks = DropCallbacks::new(move |_| if reject { DropHint::Reject } else { DropHint::Shell }, |_| {})
                .with_quick_create(|_| true, move |paths, point| {
                    assert_eq!(point.x, 10);
                    created.borrow_mut().push(paths.len());
                });
            let data: IDataObject = HdropOnly { paths: vec![] }.into();
            let fake: IDropTarget = CountingTarget { drops: drops.clone(), leaves: leaves.clone() }.into();
            let dest = Destination::Folder("never-bound".into());
            let object = DropTarget::new(HWND::default(), dest.clone(), callbacks);
            *object.state.borrow_mut() = State {
                data: Some(data.clone()), sources: vec!["source".into()], last_keys: buttons,
                classifier: classifier.build(),
                delegate: Some(Delegate { dest, dir: None, target: fake, effect: DROPEFFECT_COPY }), ..State::default()
            };
            let target: IDropTarget = object.into();
            let mut effect = allowed;
            unsafe { target.Drop(&data, MODIFIERKEYS_FLAGS(keys), POINTL { x: 10, y: 20 }, &mut effect) }.unwrap();
            assert_eq!(drops.get(), expected_drops, "row {index}");
            assert_eq!(*batches.borrow(), expected_batches, "row {index}");
            assert_eq!(leaves.get(), usize::from(expected_drops == 0), "row {index}");
            if expected_drops == 0 { assert_eq!(effect, expected_effect, "row {index}"); }
            assert_ne!(effect, DROPEFFECT_COPY, "row {index}: an Alt-drop never copies");
            assert_ne!(effect, DROPEFFECT_MOVE, "row {index}: an Alt-drop never moves");
            let state = unsafe { AsImpl::<DropTarget>::as_impl(&target) }.state.borrow();
            assert!(state.classifier.is_none() && state.delegate.is_none() && state.data.is_none(), "row {index}");
        }
    }

    /// A hover that was answered as an Alt-drop and a drop point that decides otherwise (a Gate's tab label
    /// hovers as the desktop and drops into the folder): the Shell must not get the drop.
    #[test]
    fn q18_no_shell_a_drop_that_disagrees_with_an_accepted_hover_never_reaches_the_shell() {
        for (hovered_alt, expected_drops) in [(true, 0), (false, 1)] {
            let drops = std::rc::Rc::new(Cell::new(0));
            let leaves = std::rc::Rc::new(Cell::new(0));
            let creates = std::rc::Rc::new(Cell::new(0));
            let created = creates.clone();
            let folder = Destination::Folder("tab-folder".into());
            let hover_dest = folder.clone();
            let callbacks = DropCallbacks::new(move |info| if info.dropping { DropHint::ShellTo(hover_dest.clone()) } else { DropHint::Shell }, |_| {})
                .with_quick_create(|_| true, move |_, _| created.set(created.get() + 1));
            let data: IDataObject = HdropOnly { paths: vec![] }.into();
            let fake: IDropTarget = CountingTarget { drops: drops.clone(), leaves: leaves.clone() }.into();
            let object = DropTarget::new(HWND::default(), Destination::Desktop, callbacks);
            *object.state.borrow_mut() = State {
                data: Some(data.clone()), sources: vec!["source".into()], last_keys: 1, last_alt: hovered_alt,
                classifier: Cls::Done(vec!["C:\\F".into()]).build(),
                delegate: Some(Delegate { dest: folder, dir: None, target: fake, effect: DROPEFFECT_COPY }), ..State::default()
            };
            let target: IDropTarget = object.into();
            let mut effect = ALL_EFFECTS;
            unsafe { target.Drop(&data, MODIFIERKEYS_FLAGS(0x20), POINTL { x: 10, y: 20 }, &mut effect) }.unwrap();
            assert_eq!(drops.get(), expected_drops, "hovered_alt={hovered_alt}");
            assert_eq!(creates.get(), 0, "the drop point is not an Alt-drop target");
            if hovered_alt {
                assert_eq!(effect, DROPEFFECT_NONE);
                assert_eq!(leaves.get(), 1);
            }
        }
    }

    /// Where an Alt-drop may start is asked at hover and at drop with the same point.
    #[test]
    fn q18_no_shell_the_app_can_veto_a_point_for_the_alt_drop() {
        for (x, expected_creates, expected_drops) in [(10, 1, 0), (99, 0, 1)] {
            let drops = std::rc::Rc::new(Cell::new(0));
            let leaves = std::rc::Rc::new(Cell::new(0));
            let creates = std::rc::Rc::new(Cell::new(0));
            let created = creates.clone();
            let callbacks = DropCallbacks::new(|_| DropHint::Shell, |_| {})
                .with_quick_create(|p| p.x != 99, move |_, _| created.set(created.get() + 1));
            let data: IDataObject = HdropOnly { paths: vec![] }.into();
            let fake: IDropTarget = CountingTarget { drops: drops.clone(), leaves: leaves.clone() }.into();
            let dest = Destination::Folder("never-bound".into());
            let object = DropTarget::new(HWND::default(), dest.clone(), callbacks);
            *object.state.borrow_mut() = State {
                data: Some(data.clone()), sources: vec!["source".into()], last_keys: 1,
                classifier: Cls::Done(vec!["C:\\F".into()]).build(),
                delegate: Some(Delegate { dest, dir: None, target: fake, effect: DROPEFFECT_COPY }), ..State::default()
            };
            let target: IDropTarget = object.into();
            let mut effect = ALL_EFFECTS;
            unsafe { target.Drop(&data, MODIFIERKEYS_FLAGS(0x20), POINTL { x, y: 20 }, &mut effect) }.unwrap();
            assert_eq!((creates.get(), drops.get()), (expected_creates, expected_drops), "x={x}");
        }
    }

    #[test]
    fn q18_no_shell_description_layout_and_unsupported_data_object() {
        let bytes = drop_description(6, &crate::creation::alt_drop_tip(2), "");
        assert_eq!(bytes.len(), 1044);
        assert_eq!(i32::from_le_bytes(bytes[..4].try_into().unwrap()), 6);
        let text = |range: std::ops::Range<usize>| {
            let wide: Vec<_> = bytes[range].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).take_while(|v| *v != 0).collect();
            String::from_utf16_lossy(&wide)
        };
        assert_eq!(text(4..524), "Create 2 Folder Gates here");
        assert_eq!(text(524..1044), "");
        let data: IDataObject = HdropOnly { paths: vec![] }.into();
        assert!(set_bytes_format(&data, CFSTR_DROPDESCRIPTION, &bytes).is_err());
        let leaves = std::rc::Rc::new(Cell::new(0));
        let dest = Destination::Folder("never-bound".into());
        let object = DropTarget::new(HWND::default(), dest.clone(),
            DropCallbacks::new(|_| DropHint::Shell, |_| {}).with_quick_create(|_| true, |_, _| {}));
        let fake: IDropTarget = CountingTarget { drops: std::rc::Rc::new(Cell::new(0)), leaves: leaves.clone() }.into();
        *object.state.borrow_mut() = State {
            data: Some(data), last_keys: 1, classifier: Cls::Done(vec!["C:\\folder".into()]).build(),
            delegate: Some(Delegate { dest, dir: None, target: fake, effect: DROPEFFECT_COPY }), ..State::default()
        };
        let pt = POINTL { x: 10, y: 20 };
        // unsupported SetData must not prevent the answer or retain stale feedback; the Shell target is left
        let mut effect = ALL_EFFECTS;
        let routed = object.route(MODIFIERKEYS_FLAGS(0x20), pt, &mut effect, false, false);
        assert!(matches!(routed, Routed::Alt(crate::creation::AltDrop::Accept(1))));
        assert_eq!(effect, DROPEFFECT_LINK);
        assert_eq!(leaves.get(), 1);
        assert!(object.state.borrow().saved_description.is_none());
        assert!(object.state.borrow().last_alt);
        // no link among the allowed effects: refused with NONE while dragging too
        let mut effect = DROPEFFECT_COPY;
        let routed = object.route(MODIFIERKEYS_FLAGS(0x20), pt, &mut effect, false, false);
        assert!(matches!(routed, Routed::Alt(crate::creation::AltDrop::Refused(crate::creation::Refusal::NoLink))));
        assert_eq!(effect, DROPEFFECT_NONE);
        assert!(object.state.borrow().last_alt);
        // Alt+Shift is no Alt-drop: the ordinary answer comes back and the memory of the Alt answer goes
        let dest = Destination::Folder("never-bound".into());
        let fake: IDropTarget = CountingTarget { drops: std::rc::Rc::new(Cell::new(0)), leaves: leaves.clone() }.into();
        object.state.borrow_mut().delegate = Some(Delegate { dest, dir: None, target: fake, effect: DROPEFFECT_COPY });
        let mut effect = ALL_EFFECTS;
        assert!(matches!(object.route(MODIFIERKEYS_FLAGS(0x24), pt, &mut effect, false, false), Routed::Shell));
        assert_eq!(effect, ALL_EFFECTS);
        assert!(!object.state.borrow().last_alt);
        assert_eq!(object.alt_decision(0x20, ALL_EFFECTS, false), crate::creation::AltDrop::Accept(1));
    }

    fn init() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            OleInitialize(None).expect("OleInitialize");
        }
    }

    /// Temp directory pair that is always removed.
    struct Sandbox {
        root: std::path::PathBuf,
        a: std::path::PathBuf,
        b: std::path::PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Sandbox {
            let root = std::env::temp_dir().join(format!("corral_dragdrop_{}_{}", std::process::id(), tag));
            let _ = std::fs::remove_dir_all(&root);
            let a = root.join("A");
            let b = root.join("B");
            std::fs::create_dir_all(&a).unwrap();
            std::fs::create_dir_all(&b).unwrap();
            Sandbox { root, a, b }
        }
        fn file_in_a(&self, name: &str) -> String {
            let p = self.a.join(name);
            std::fs::write(&p, name.as_bytes()).unwrap();
            p.to_string_lossy().into_owned()
        }
        fn b_string(&self) -> String {
            self.b.to_string_lossy().into_owned()
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn pump_until(timeout: Duration, mut done: impl FnMut() -> bool) -> (bool, Duration) {
        let start = Instant::now();
        loop {
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if done() {
                return (true, start.elapsed());
            }
            if start.elapsed() > timeout {
                return (false, start.elapsed());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    type Recorded = Rc<RefCell<Vec<Dropped>>>;

    fn make_target(default: Destination, hint: DropHint) -> (IDropTarget, Recorded, Rc<RefCell<Vec<(bool, bool)>>>) {
        let recorded: Recorded = Rc::new(RefCell::new(Vec::new()));
        let hovers = Rc::new(RefCell::new(Vec::new()));
        let (r, h) = (recorded.clone(), hovers.clone());
        let callbacks = DropCallbacks::new(
            move |info| {
                h.borrow_mut().push((info.entering, info.internal));
                hint.clone()
            },
            move |d| r.borrow_mut().push(d.clone()),
        );
        let target: IDropTarget = DropTarget::new(HWND::default(), default, callbacks).into();
        (target, recorded, hovers)
    }

    /// Runs DragEnter / DragOver / Drop with the given allowed effect. Returns the effects after
    /// each call and the Drop result.
    fn run_drop(target: &IDropTarget, data: &IDataObject, allowed: DROPEFFECT, keys: u32) -> (u32, u32, u32, Result<()>) {
        let pt = POINTL { x: 40, y: 40 };
        let keys = MODIFIERKEYS_FLAGS(keys);
        let mut effect = allowed;
        unsafe { target.DragEnter(data, keys, pt, &mut effect) }.expect("DragEnter");
        let after_enter = effect.0;
        effect = allowed;
        unsafe { target.DragOver(keys, pt, &mut effect) }.expect("DragOver");
        let after_over = effect.0;
        effect = allowed;
        let result = unsafe { target.Drop(data, keys, pt, &mut effect) };
        (after_enter, after_over, effect.0, result)
    }

    fn same_path(a: &str, b: &str) -> bool {
        a.eq_ignore_ascii_case(b)
    }

    #[test]
    fn drop_copy_into_folder_keeps_source() {
        init();
        let sb = Sandbox::new("copy");
        let source = sb.file_in_a("one.txt");
        let data = data_object_for(HWND::default(), &[source.clone()]).expect("data object");
        let (target, recorded, hovers) = make_target(Destination::Folder(sb.b_string()), DropHint::Shell);
        let (enter, over, finished, result) = run_drop(&target, &data, DROPEFFECT_COPY, 0);
        let dest = sb.b.join("one.txt");
        let (appeared, waited) = pump_until(Duration::from_secs(10), || dest.exists());
        println!(
            "  [copy] DragEnter effect={enter} DragOver effect={over} Drop effect={finished} result={result:?}; \
             B\\one.txt exists={appeared} after {waited:?}; A\\one.txt exists={}; on_dropped calls={}; hover calls={:?}",
            std::path::Path::new(&source).exists(),
            recorded.borrow().len(),
            hovers.borrow()
        );
        result.expect("Drop");
        assert!(appeared, "file did not appear in B within 10 s");
        assert!(std::path::Path::new(&source).exists(), "COPY must keep the source");
        assert_eq!(finished, DROPEFFECT_COPY.0);
        let recorded = recorded.borrow();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].sources.len(), 1);
        assert!(same_path(&recorded[0].sources[0], &source), "reported {:?}", recorded[0].sources);
        assert_eq!(recorded[0].effect, DROPEFFECT_COPY);
        assert!(!recorded[0].internal);
        assert_eq!(recorded[0].expected_destinations.len(), 1);
        assert!(same_path(&recorded[0].expected_destinations[0], &dest.to_string_lossy()), "{:?}", recorded[0].expected_destinations);
        assert_eq!(std::fs::read(&dest).unwrap(), b"one.txt");
        assert!(hovers.borrow().first().is_some_and(|h| h.0), "first hover must be the entering call");
    }

    #[test]
    fn drop_move_into_folder_moves_source() {
        init();
        let sb = Sandbox::new("move");
        let source = sb.file_in_a("two.txt");
        let data = data_object_for(HWND::default(), &[source.clone()]).expect("data object");
        let (target, recorded, _) = make_target(Destination::Folder(sb.b_string()), DropHint::Shell);
        let (enter, over, finished, result) = run_drop(&target, &data, DROPEFFECT_MOVE, 0);
        let dest = sb.b.join("two.txt");
        // The Shell performs the move itself (optimised move) and returns NONE from Drop so that the
        // source does not delete the originals; the negotiated effect is reported as MOVE.
        let (appeared, waited) = pump_until(Duration::from_secs(10), || dest.exists() && !std::path::Path::new(&source).exists());
        println!(
            "  [move] DragEnter effect={enter} DragOver effect={over} Drop effect={finished} result={result:?}; \
             B\\two.txt exists={} A\\two.txt exists={} (settled={appeared} after {waited:?}); on_dropped calls={}",
            dest.exists(),
            std::path::Path::new(&source).exists(),
            recorded.borrow().len()
        );
        result.expect("Drop");
        assert!(appeared, "move did not complete within 10 s");
        assert!(dest.exists() && !std::path::Path::new(&source).exists());
        assert_eq!(finished, DROPEFFECT_NONE.0, "optimised move returns NONE to the source");
        assert_eq!(recorded.borrow().len(), 1);
        assert_eq!(recorded.borrow()[0].effect, DROPEFFECT_MOVE);
        assert_eq!(recorded.borrow()[0].target_effect, Some(DROPEFFECT_NONE));
    }

    #[test]
    fn two_files_report_both_names() {
        init();
        let sb = Sandbox::new("two");
        let s1 = sb.file_in_a("alpha.txt");
        let s2 = sb.file_in_a("beta.txt");
        let data = data_object_for(HWND::default(), &[s1.clone(), s2.clone()]).expect("data object");
        let direct = read_source_names(&data);
        let (target, recorded, _) = make_target(Destination::Folder(sb.b_string()), DropHint::Shell);
        let (_, _, _, result) = run_drop(&target, &data, DROPEFFECT_COPY, 0);
        let (b1, b2) = (sb.b.join("alpha.txt"), sb.b.join("beta.txt"));
        let (appeared, waited) = pump_until(Duration::from_secs(10), || b1.exists() && b2.exists());
        println!(
            "  [two] read_source_names={direct:?}; on_dropped sources={:?} expected={:?}; both in B={appeared} after {waited:?}",
            recorded.borrow().first().map(|d| d.sources.clone()),
            recorded.borrow().first().map(|d| d.expected_destinations.clone())
        );
        result.expect("Drop");
        assert_eq!(direct.len(), 2);
        assert!(direct.iter().any(|n| same_path(n, &s1)) && direct.iter().any(|n| same_path(n, &s2)));
        assert!(appeared, "both files should reach B");
        let recorded = recorded.borrow();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].sources.len(), 2);
        assert!(recorded[0].sources.iter().any(|n| same_path(n, &s1)) && recorded[0].sources.iter().any(|n| same_path(n, &s2)));
        assert_eq!(recorded[0].expected_destinations.len(), 2);
    }

    #[test]
    fn shell_to_overrides_default_destination() {
        init();
        let sb = Sandbox::new("shellto");
        let source = sb.file_in_a("three.txt");
        let data = data_object_for(HWND::default(), &[source.clone()]).expect("data object");
        // Default destination is the real Desktop; the hint redirects every event to B, so the
        // desktop's drop target is never even created.
        let (target, recorded, _) =
            make_target(Destination::Desktop, DropHint::ShellTo(Destination::Folder(sb.b_string())));
        let (_, _, _, result) = run_drop(&target, &data, DROPEFFECT_COPY, 0);
        let dest = sb.b.join("three.txt");
        let (appeared, _) = pump_until(Duration::from_secs(10), || dest.exists());
        println!("  [shell_to] result={result:?} B\\three.txt exists={appeared}; destination reported={:?}", recorded.borrow().first().map(|d| d.destination.clone()));
        result.expect("Drop");
        assert!(appeared);
        assert_eq!(recorded.borrow()[0].destination, Some(Destination::Folder(sb.b_string())));
    }

    #[test]
    fn reject_and_internal_do_not_touch_files() {
        init();
        let sb = Sandbox::new("veto");
        let source = sb.file_in_a("four.txt");
        let data = data_object_for(HWND::default(), &[source.clone()]).expect("data object");

        let (target, recorded, _) = make_target(Destination::Folder(sb.b_string()), DropHint::Reject);
        let (enter, over, finished, result) = run_drop(&target, &data, DROPEFFECT_COPY | DROPEFFECT_MOVE, 0);
        std::thread::sleep(Duration::from_millis(300));
        println!("  [reject] effects {enter}/{over}/{finished} result={result:?} B entries={} on_dropped={}", std::fs::read_dir(&sb.b).unwrap().count(), recorded.borrow().len());
        result.expect("Drop");
        assert_eq!((enter, over, finished), (0, 0, 0));
        assert!(recorded.borrow().is_empty());
        assert_eq!(std::fs::read_dir(&sb.b).unwrap().count(), 0);

        let _guard = InternalDragGuard::enter(&[source.clone()]);
        let (target, recorded, hovers) = make_target(Destination::Folder(sb.b_string()), DropHint::Internal(DROPEFFECT_MOVE));
        let (enter, over, finished, result) = run_drop(&target, &data, ALL_EFFECTS, 0);
        std::thread::sleep(Duration::from_millis(300));
        println!(
            "  [internal] effects {enter}/{over}/{finished} result={result:?} B entries={} A\\four.txt exists={} on_dropped internal={:?} hover internal flags={:?}",
            std::fs::read_dir(&sb.b).unwrap().count(),
            std::path::Path::new(&source).exists(),
            recorded.borrow().first().map(|d| d.internal),
            hovers.borrow()
        );
        result.expect("Drop");
        assert_eq!((enter, over, finished), (DROPEFFECT_MOVE.0, DROPEFFECT_MOVE.0, DROPEFFECT_MOVE.0));
        assert_eq!(std::fs::read_dir(&sb.b).unwrap().count(), 0, "internal drop must not run a file operation");
        assert!(std::path::Path::new(&source).exists());
        let recorded = recorded.borrow();
        assert_eq!(recorded.len(), 1);
        assert!(recorded[0].internal && recorded[0].destination.is_none() && recorded[0].expected_destinations.is_empty());
        assert!(hovers.borrow().iter().all(|h| h.1), "hover must see the internal flag");
    }

    #[test]
    fn drag_leave_reports_and_resets() {
        init();
        let sb = Sandbox::new("leave");
        let source = sb.file_in_a("five.txt");
        let data = data_object_for(HWND::default(), &[source]).expect("data object");
        let left = Rc::new(Cell::new(0));
        let l = left.clone();
        let callbacks = DropCallbacks::new(|_| DropHint::Shell, |_| {}).with_leave(move || l.set(l.get() + 1));
        let target: IDropTarget = DropTarget::new(HWND::default(), Destination::Folder(sb.b_string()), callbacks).into();
        let mut effect = DROPEFFECT_COPY;
        unsafe {
            target.DragEnter(&data, MODIFIERKEYS_FLAGS(0), POINTL { x: 1, y: 1 }, &mut effect).unwrap();
            target.DragLeave().unwrap();
        }
        println!("  [leave] on_leave calls={} B entries={}", left.get(), std::fs::read_dir(&sb.b).unwrap().count());
        assert_eq!(left.get(), 1);
        assert_eq!(std::fs::read_dir(&sb.b).unwrap().count(), 0);
        // DragOver after leave is refused.
        let mut effect = DROPEFFECT_COPY;
        unsafe { target.DragOver(MODIFIERKEYS_FLAGS(0), POINTL { x: 1, y: 1 }, &mut effect) }.unwrap();
        assert_eq!(effect, DROPEFFECT_NONE);
    }

    #[test]
    fn desktop_drop_target_resolves_without_dropping() {
        init();
        // Read-only: only creates the Shell's drop target for the desktop; nothing is dropped.
        let (_target, dir) = shell_drop_target(&Destination::Desktop, HWND::default()).expect("desktop drop target");
        println!("  [desktop] drop target created; desktop dir = {dir:?}");
        let dir = dir.expect("desktop directory");
        assert!(Path::new(&dir).is_dir());
        assert!(shell_drop_target(&Destination::Folder(r"C:\definitely\not\here\corral-xyz".into()), HWND::default()).is_err());
    }

    #[test]
    fn register_and_revoke_on_message_window() {
        init();
        let hwnd = unsafe {
            CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("corral-dd-test"), WS_OVERLAPPED, 0, 0, 0, 0, HWND_MESSAGE, None, None, None)
        }
        .expect("message-only window");
        let new_target = || DropTarget::new(hwnd, Destination::Desktop, DropCallbacks::new(|_| DropHint::Reject, |_| {}));
        let registration = register(hwnd, new_target()).expect("RegisterDragDrop on a message-only window");
        let second = register(hwnd, new_target());
        println!("  [register] first ok; second registration -> {:?}", second.as_ref().err().map(|e| e.code()));
        assert_eq!(second.err().map(|e| e.code()), Some(DRAGDROP_E_ALREADYREGISTERED));
        drop(registration);
        let again = register(hwnd, new_target()).expect("re-register after revoke");
        again.revoke();
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }

    #[test]
    fn query_continue_drag_decisions() {
        let (l, r) = (MK_LBUTTON.0, MK_RBUTTON.0);
        // left drag: keep going while held, drop on release, cancel on Esc or on the right button
        assert_eq!(continue_drag(false, l, l), S_OK);
        assert_eq!(continue_drag(false, 0, l), DRAGDROP_S_DROP);
        assert_eq!(continue_drag(true, l, l), DRAGDROP_S_CANCEL);
        assert_eq!(continue_drag(false, l | r, l), DRAGDROP_S_CANCEL);
        assert_eq!(continue_drag(false, r, l), DRAGDROP_S_CANCEL);
        // right drag mirrors it
        assert_eq!(continue_drag(false, r, r), S_OK);
        assert_eq!(continue_drag(false, 0, r), DRAGDROP_S_DROP);
        assert_eq!(continue_drag(false, l | r, r), DRAGDROP_S_CANCEL);
        // modifier keys (Ctrl/Shift) do not interfere
        assert_eq!(continue_drag(false, l | 0x0008 | 0x0004, l), S_OK);
    }

    #[test]
    fn begin_drag_rejects_bad_input() {
        init();
        assert!(begin_drag(HWND::default(), &[], None).is_err());
        let sb1 = Sandbox::new("bad1");
        let sb2 = Sandbox::new("bad2");
        let (f1, f2) = (sb1.file_in_a("x.txt"), sb2.file_in_a("y.txt"));
        let mixed = begin_drag(HWND::default(), &[f1, f2], None);
        println!("  [begin_drag] mixed parents -> {:?}", mixed.as_ref().err().map(|e| e.message()));
        assert!(mixed.is_err());
        let missing = std::env::temp_dir().join("corral_dd_does_not_exist_77").join("nope.txt");
        assert!(begin_drag(HWND::default(), &[missing.to_string_lossy().into_owned()], None).is_err());
    }

    #[test]
    fn drag_image_attaches_and_buffer_is_validated() {
        let _serial = helper_lock();
        init();
        assert!(DragImage::from_bgra_premultiplied(4, 4, &[0u8; 10], (0, 0)).is_err());
        let sb = Sandbox::new("img");
        let source = sb.file_in_a("six.txt");
        let data = data_object_for(HWND::default(), &[source]).expect("data object");
        let pixels = vec![0x80u8; 16 * 16 * 4];
        let image = DragImage::from_bgra_premultiplied(16, 16, &pixels, (8, 8)).expect("drag image");
        assert_eq!(image.size(), (16, 16));
        let result = set_drag_image(&data, image);
        println!("  [drag image] InitializeFromBitmap on the Shell data object -> {:?}", result.as_ref().err().map(|e| e.code()));
        result.expect("InitializeFromBitmap");
    }

    /// Reads the bytes of a private clipboard format from `data`, or the failing HRESULT.
    fn format_bytes(data: &IDataObject, name: &str) -> std::result::Result<Vec<u8>, windows::core::HRESULT> {
        let medium = get_hglobal(data, clipboard_format(name)).map_err(|e| e.code())?;
        unsafe {
            let size = GlobalSize(medium.hglobal());
            let ptr = GlobalLock(medium.hglobal()) as *const u8;
            let bytes = if ptr.is_null() { Vec::new() } else { std::slice::from_raw_parts(ptr, size).to_vec() };
            let _ = GlobalUnlock(medium.hglobal());
            Ok(bytes)
        }
    }

    #[test]
    fn drag_source_allows_drop_description_text() {
        let _serial = helper_lock();
        init();
        let sb = Sandbox::new("dsh");
        let source = sb.file_in_a("seven.txt");
        for (label, with_image) in [("bitmap", true), ("no image", false)] {
            let data = data_object_for(HWND::default(), &[source.clone()]).expect("data object");
            if with_image {
                let image = DragImage::from_bgra_premultiplied(16, 16, &vec![0x80u8; 16 * 16 * 4], (8, 8)).expect("drag image");
                set_drag_image(&data, image).expect("InitializeFromBitmap");
            } else {
                let result = set_no_drag_image(&data);
                println!("  [drag source] InitializeFromWindow(NULL) -> {:?}", result.as_ref().err().map(|e| e.code()));
                result.expect("InitializeFromWindow without an image");
            }
            // observed: the helper stores the flags as the "DragSourceHelperFlags" DWORD (1 = allowed)
            let flags_now = format_bytes(&data, "DragSourceHelperFlags");
            if flags_now.is_err() {
                std::thread::sleep(Duration::from_millis(500));
                println!("  [drag source] {label}: flags missing; after 500 ms: {:?}", format_bytes(&data, "DragSourceHelperFlags"));
            }
            assert_eq!(
                format_bytes(&data, "DragSourceHelperFlags").ok().and_then(|b| b.get(..4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))),
                Some(DSH_ALLOWDROPDESCRIPTIONTEXT),
                "{label}: drop descriptions must be allowed by the source"
            );
            for name in ["DragContext", "DragSourceHelperFlags", "IsShowingText", "DragWindow", "DropDescription", "IsComputingImage", "UsingDefaultDragImage"] {
                println!("  [drag source] {label}: {name}: {:?}", format_bytes(&data, name).map(|b| b.into_iter().take(16).collect::<Vec<u8>>()));
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // Offline probe: what do the Shell's own folder drop targets write into our data object?
    // -----------------------------------------------------------------------------------------

    /// Shell drop target of the folder `folder` as Explorer's DefView gets it for a folder icon
    /// under the cursor (`GetUIObjectOf(IDropTarget)` of the parent), or the folder's background
    /// target (`CreateViewObject`) when `background`.
    fn folder_target(folder: &str, background: bool) -> IDropTarget {
        if background {
            return shell_drop_target(&Destination::Folder(folder.to_string()), HWND::default()).expect("background target").0;
        }
        let pidl = parse(folder).expect("parse folder");
        let parent: IShellFolder = unsafe { SHBindToParent::<IShellFolder>(pidl.0 as *const ITEMIDLIST, None) }.expect("parent");
        let child = unsafe { ILFindLastID(pidl.0 as *const ITEMIDLIST) } as *const ITEMIDLIST;
        let target: IDropTarget = unsafe { parent.GetUIObjectOf(HWND::default(), &[child], None) }.expect("GetUIObjectOf(IDropTarget)");
        target
    }

    /// One probe scenario: DragEnter, DragOver x2, DragLeave on a Shell folder target (never Drop),
    /// printing the feedback state in the data object after each step.
    fn probe_scenario(label: &str, image: bool, flags: bool, background: bool, target_helper_window: Option<HWND>, keys: u32) -> Vec<FeedbackState> {
        let sb = Sandbox::new(&format!("probe{}", label.len() * 7 + image as usize + 3 * flags as usize + 5 * background as usize));
        let source = sb.file_in_a("probe.txt");
        let dest = sb.b_string();
        let data = data_object_for(HWND::default(), &[source.clone()]).expect("data object");
        let helper = drag_source_helper_with(flags).expect("source helper");
        if image {
            let pixels = vec![0x80u8; 32 * 32 * 4];
            let mut img = DragImage::from_bgra_premultiplied(32, 32, &pixels, (16, 16)).expect("image");
            let shdi = SHDRAGIMAGE {
                sizeDragImage: img.size,
                ptOffset: img.hotspot,
                hbmpDragImage: img.bitmap,
                crColorKey: windows::Win32::Foundation::COLORREF(CLR_NONE),
            };
            unsafe { helper.InitializeFromBitmap(&shdi, &data) }.expect("InitializeFromBitmap");
            img.bitmap = HBITMAP::default();
        } else {
            unsafe { helper.InitializeFromWindow(HWND::default(), None, &data) }.expect("InitializeFromWindow");
        }
        let target = folder_target(&dest, background);
        let target_helper = target_helper_window
            .and_then(|_| unsafe { CoCreateInstance::<_, IDropTargetHelper>(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER) }.ok());
        let pt = POINTL { x: 300, y: 300 };
        let point = POINT { x: 300, y: 300 };
        let keys = MODIFIERKEYS_FLAGS(keys);
        let mut states = Vec::new();
        let mut effect = ALL_EFFECTS;
        let enter = unsafe { target.DragEnter(&data, keys, pt, &mut effect) };
        if let (Some(h), Some(w)) = (&target_helper, target_helper_window) {
            let _ = unsafe { h.DragEnter(w, &data, &point, effect) };
        }
        states.push(feedback_state(&data, effect));
        println!("  [probe {label}] DragEnter -> {enter:?}; {}", states[0].describe());
        for step in 1..=2 {
            effect = ALL_EFFECTS;
            let over = unsafe { target.DragOver(keys, pt, &mut effect) };
            if let Some(h) = &target_helper {
                let _ = unsafe { h.DragOver(&point, effect) };
            }
            let state = feedback_state(&data, effect);
            println!("  [probe {label}] DragOver #{step} -> {over:?}; {}", state.describe());
            states.push(state);
        }
        if target_helper.is_some() {
            let (_, waited) = pump_until(Duration::from_millis(300), || false);
            let after_pump = feedback_state(&data, effect);
            println!("  [probe {label}] after pumping messages {waited:?}; {}", after_pump.describe());
            if let Some(window) = after_pump.drag_window {
                let hwnd = HWND(window as isize as *mut c_void);
                let reply = unsafe {
                    windows::Win32::UI::WindowsAndMessaging::SendMessageW(
                        hwnd,
                        DDWM_UPDATEWINDOW,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    )
                };
                let after_update = feedback_state(&data, effect);
                println!("  [probe {label}] after DDWM_UPDATEWINDOW (reply {}); {}", reply.0, after_update.describe());
            }
        }
        if let Some(h) = &target_helper {
            let _ = unsafe { h.DragLeave() };
        }
        let leave = unsafe { target.DragLeave() };
        let state = feedback_state(&data, DROPEFFECT_NONE);
        println!("  [probe {label}] DragLeave -> {leave:?}; {}", state.describe());
        states.push(state);
        let untouched = std::fs::read_dir(&sb.b).unwrap().count() == 0 && Path::new(&source).exists();
        assert!(untouched, "{label}: the probe must not move or copy anything");
        states
    }

    #[test]
    fn probe_shell_folder_targets_drop_description() {
        let _serial = helper_lock();
        init();
        let window = unsafe {
            CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("corral-probe"), WS_OVERLAPPED, 0, 0, 100, 100, None, None, None, None)
        }
        .ok();
        let hwnd = window.unwrap_or_default();
        let lbutton = MK_LBUTTON.0;
        for (label, image, flags, background, helper_window) in [
            ("child folder, bitmap, flags, no target helper", true, true, false, None),
            ("child folder, bitmap, NO flags, no target helper", true, false, false, None),
            ("child folder, no image, flags, no target helper", false, true, false, None),
            ("background (CreateViewObject), bitmap, flags", true, true, true, None),
            ("child folder, bitmap, flags, with target helper", true, true, false, Some(hwnd)),
        ] {
            probe_scenario(label, image, flags, background, helper_window, lbutton);
        }
        if window.is_some() {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
    }

    /// A target in another apartment (a second STA thread that only holds a COM proxy of our data
    /// object, exactly what Explorer holds during a real out-of-process drag): does the Shell
    /// folder target's `DropDescription` reach the source's own data object, and does the helper
    /// turn on `IsShowingText`? The source thread pumps messages (as `DoDragDrop` does).
    #[test]
    fn probe_cross_apartment_target_writes_drop_description() {
        cross_apartment_run(true);
        cross_apartment_run(false);
    }

    fn cross_apartment_run(flags: bool) {
        use std::sync::mpsc;
        use windows::Win32::System::Com::IGlobalInterfaceTable;
        const CLSID_STD_GIT: windows::core::GUID = windows::core::GUID::from_u128(0x00000323_0000_0000_c000_000000000046);
        let _serial = helper_lock();
        init();
        let sb = Sandbox::new("xapt");
        let source = sb.file_in_a("xapt.txt");
        let dest = sb.b_string();
        let data = data_object_for(HWND::default(), &[source.clone()]).expect("data object");
        let helper = drag_source_helper_with(flags).expect("helper");
        println!("  [probe cross-apartment] ---- SetFlags(DSH_ALLOWDROPDESCRIPTIONTEXT) = {flags}");
        let mut img = DragImage::from_bgra_premultiplied(32, 32, &vec![0x80u8; 32 * 32 * 4], (16, 16)).expect("image");
        let shdi = SHDRAGIMAGE {
            sizeDragImage: img.size,
            ptOffset: img.hotspot,
            hbmpDragImage: img.bitmap,
            crColorKey: windows::Win32::Foundation::COLORREF(CLR_NONE),
        };
        unsafe { helper.InitializeFromBitmap(&shdi, &data) }.expect("InitializeFromBitmap");
        img.bitmap = HBITMAP::default();

        let table: IGlobalInterfaceTable =
            unsafe { CoCreateInstance(&CLSID_STD_GIT, None, CLSCTX_INPROC_SERVER) }.expect("interface table");
        let cookie = unsafe { table.RegisterInterfaceInGlobal(&data, &IDataObject::IID) }.expect("register in table");
        let (ready_tx, ready_rx) = mpsc::channel::<Vec<String>>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let dest_for_thread = dest.clone();
        let source_thread = unsafe { windows::Win32::System::Threading::GetCurrentThreadId() };
        let worker = std::thread::spawn(move || {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                OleInitialize(None).expect("OleInitialize (target)");
            }
            let table: IGlobalInterfaceTable =
                unsafe { CoCreateInstance(&CLSID_STD_GIT, None, CLSCTX_INPROC_SERVER) }.expect("table (target)");
            let proxy: IDataObject = unsafe {
                let mut raw: *mut c_void = std::ptr::null_mut();
                table.GetInterfaceFromGlobal(cookie, &IDataObject::IID, &mut raw).expect("proxy of the data object");
                IDataObject::from_raw(raw)
            };
            let window = unsafe {
                CreateWindowExW(WINDOW_EX_STYLE(0), w!("STATIC"), w!("corral-xapt-target"), WS_OVERLAPPED, 0, 0, 100, 100, None, None, None, None)
            }
            .unwrap_or_default();
            let target = folder_target(&dest_for_thread, false);
            let target_helper: IDropTargetHelper =
                unsafe { CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER) }.expect("target helper");
            let (pt, point, keys) = (POINTL { x: 300, y: 300 }, POINT { x: 300, y: 300 }, MODIFIERKEYS_FLAGS(MK_LBUTTON.0));
            let mut log = Vec::new();
            let mut effect = ALL_EFFECTS;
            let r = unsafe { target.DragEnter(&proxy, keys, pt, &mut effect) };
            let h = unsafe { target_helper.DragEnter(window, &proxy, &point, effect) };
            log.push(format!("target DragEnter {r:?} helper {h:?} effect={}", effect.0));
            for i in 0..2 {
                effect = ALL_EFFECTS;
                let r = unsafe { target.DragOver(keys, pt, &mut effect) };
                let h = unsafe { target_helper.DragOver(&point, effect) };
                log.push(format!("target DragOver#{i} {r:?} helper {h:?} effect={}", effect.0));
            }
            // pump this thread's messages (the drag window lives here) for a moment
            let _ = pump_until(Duration::from_millis(400), || false);
            let state = feedback_state(&proxy, effect);
            log.push(format!("via proxy: {}", state.describe()));
            if let Some(w) = state.drag_window {
                use windows::Win32::UI::WindowsAndMessaging::{GetWindowLongW, GetWindowThreadProcessId, IsWindowVisible, GWL_EXSTYLE};
                let hwnd = HWND(w as isize as *mut c_void);
                let thread = unsafe { GetWindowThreadProcessId(hwnd, None) };
                log.push(format!(
                    "DragWindow owner thread={thread} (target thread={}, source thread={}) visible={} exstyle={:#x}",
                    unsafe { windows::Win32::System::Threading::GetCurrentThreadId() },
                    source_thread,
                    unsafe { IsWindowVisible(hwnd) }.as_bool(),
                    unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) }
                ));
            }
            let _ = ready_tx.send(log);
            let _ = go_rx.recv_timeout(Duration::from_secs(10));
            let _ = unsafe { target_helper.DragLeave() };
            let _ = unsafe { target.DragLeave() };
            unsafe {
                let _ = DestroyWindow(window);
            }
        });
        // source thread: serve incoming proxy calls like DoDragDrop's loop
        let mut log = None;
        let _ = pump_until(Duration::from_secs(15), || {
            if let Ok(l) = ready_rx.try_recv() {
                log = Some(l);
                true
            } else {
                false
            }
        });
        let log = log.expect("target thread did not report in time");
        let direct = feedback_state(&data, ALL_EFFECTS);
        for line in &log {
            println!("  [probe cross-apartment] {line}");
        }
        println!("  [probe cross-apartment] source side (direct data object): {}", direct.describe());
        let _ = go_tx.send(());
        let _ = pump_until(Duration::from_secs(5), || worker.is_finished());
        worker.join().expect("target thread");
        unsafe {
            let _ = table.RevokeInterfaceFromGlobal(cookie);
        }
        assert!(std::fs::read_dir(&sb.b).unwrap().count() == 0 && Path::new(&source).exists(), "nothing may be moved or copied");
    }

    /// Regression: when another `Initialize*` consumed the pending `SetFlags`, our data object ends
    /// up without the flag; `ensure_drop_description_flag` writes it, and `set_dword_format` works on
    /// the Shell data object (also used for `InShellDragLoop`).
    #[test]
    fn stolen_helper_flag_is_backfilled() {
        let _serial = helper_lock();
        init();
        let sb = Sandbox::new("backfill");
        let source = sb.file_in_a("backfill.txt");
        let data = data_object_for(HWND::default(), &[source]).expect("data object");
        // a flag-less helper (nothing pending) initialises the data object: no flag, as if stolen
        let helper = drag_source_helper_with(false).expect("helper");
        unsafe { helper.InitializeFromWindow(HWND::default(), None, &data) }.expect("InitializeFromWindow");
        let flags = |d: &IDataObject| format_payload(d, CFSTR_DRAGSOURCEHELPERFLAGS).and_then(payload_u32);
        let before = flags(&data);
        ensure_drop_description_flag(&data);
        let after = flags(&data);
        println!("  [backfill] flag before={before:?} after={after:?}");
        assert_eq!(after, Some(DSH_ALLOWDROPDESCRIPTIONTEXT));
        // idempotent
        ensure_drop_description_flag(&data);
        assert_eq!(flags(&data), Some(DSH_ALLOWDROPDESCRIPTIONTEXT));
        set_dword_format(&data, CFSTR_INDRAGLOOP, 1).expect("InShellDragLoop");
        assert_eq!(format_payload(&data, CFSTR_INDRAGLOOP).and_then(payload_u32), Some(1));
        set_dword_format(&data, CFSTR_INDRAGLOOP, 0).expect("InShellDragLoop reset");
        assert_eq!(format_payload(&data, CFSTR_INDRAGLOOP).and_then(payload_u32), Some(0));
    }

    /// Where does `DragSourceHelperFlags` come from: the helper instance, or process-wide state?
    #[test]
    fn probe_helper_flag_scope() {
        let _serial = helper_lock();
        init();
        let sb = Sandbox::new("flagscope");
        let source = sb.file_in_a("scope.txt");
        let new_data = || data_object_for(HWND::default(), &[source.clone()]).expect("data object");
        let flags_of = |d: &IDataObject| format_payload(d, "DragSourceHelperFlags").and_then(payload_u32);
        let init_bitmap = |h: &IDragSourceHelper, d: &IDataObject| {
            let mut img = DragImage::from_bgra_premultiplied(8, 8, &[0x80u8; 8 * 8 * 4], (4, 4)).unwrap();
            let shdi = SHDRAGIMAGE {
                sizeDragImage: img.size,
                ptOffset: img.hotspot,
                hbmpDragImage: img.bitmap,
                crColorKey: windows::Win32::Foundation::COLORREF(CLR_NONE),
            };
            unsafe { h.InitializeFromBitmap(&shdi, d) }.expect("InitializeFromBitmap");
            img.bitmap = HBITMAP::default();
        };
        // (1) flags set on helper A, data initialised through helper B
        let a = drag_source_helper_with(true).unwrap();
        let b = drag_source_helper_with(false).unwrap();
        let d1 = new_data();
        init_bitmap(&b, &d1);
        println!("  [probe flag scope] flags on A, Initialize via B -> {:?}", flags_of(&d1));
        // (2) the same helper A initialises two data objects in a row
        let (d2, d3) = (new_data(), new_data());
        init_bitmap(&a, &d2);
        init_bitmap(&a, &d3);
        println!("  [probe flag scope] helper A, first data -> {:?}, second data -> {:?}", flags_of(&d2), flags_of(&d3));
        // (3) a second Initialize on the same data object with a flag-less helper
        let d4 = new_data();
        init_bitmap(&a, &d4);
        let first = flags_of(&d4);
        init_bitmap(&b, &d4);
        println!("  [probe flag scope] same data: via A -> {first:?}, then via flag-less B -> {:?}", flags_of(&d4));
        // (4) SetFlags called after Initialize
        let c = drag_source_helper_with(false).unwrap();
        let d5 = new_data();
        init_bitmap(&c, &d5);
        let before = flags_of(&d5);
        if let Ok(c2) = c.cast::<IDragSourceHelper2>() {
            let _ = unsafe { c2.SetFlags(DSH_ALLOWDROPDESCRIPTIONTEXT) };
        }
        println!("  [probe flag scope] SetFlags after Initialize: before -> {before:?}, after -> {:?}", flags_of(&d5));
    }

    /// Data object that only offers `CF_HDROP`, to test the fallback path.
    #[implement(windows::Win32::System::Com::IDataObject)]
    struct HdropOnly {
        paths: Vec<String>,
    }

    impl IDataObject_Impl for HdropOnly_Impl {
        fn GetData(&self, pformatetcin: *const FORMATETC) -> Result<STGMEDIUM> {
            let wanted = unsafe { &*pformatetcin };
            if wanted.cfFormat != CF_HDROP.0 {
                return Err(Error::from(windows::Win32::Foundation::DV_E_FORMATETC));
            }
            let mut wide: Vec<u16> = Vec::new();
            for p in &self.paths {
                wide.extend(p.encode_utf16());
                wide.push(0);
            }
            wide.push(0);
            let header = std::mem::size_of::<DROPFILES>();
            let total = header + wide.len() * 2;
            unsafe {
                let hglobal = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, total)?;
                let ptr = GlobalLock(hglobal) as *mut u8;
                let drop_files = DROPFILES { pFiles: header as u32, pt: POINT::default(), fNC: false.into(), fWide: true.into() };
                std::ptr::copy_nonoverlapping(&drop_files as *const DROPFILES as *const u8, ptr, header);
                std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, ptr.add(header), wide.len() * 2);
                let _ = GlobalUnlock(hglobal);
                let mut medium = STGMEDIUM::default();
                medium.tymed = TYMED_HGLOBAL.0 as u32;
                medium.u.hGlobal = hglobal;
                Ok(medium)
            }
        }
        fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> Result<()> {
            Err(Error::from(E_FAIL))
        }
        fn QueryGetData(&self, pformatetc: *const FORMATETC) -> windows::core::HRESULT {
            if unsafe { (*pformatetc).cfFormat } == CF_HDROP.0 { S_OK } else { windows::Win32::Foundation::DV_E_FORMATETC }
        }
        fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, _: *mut FORMATETC) -> windows::core::HRESULT {
            E_FAIL
        }
        fn SetData(&self, _: *const FORMATETC, _: *const STGMEDIUM, _: windows::Win32::Foundation::BOOL) -> Result<()> {
            Err(Error::from(E_FAIL))
        }
        fn EnumFormatEtc(&self, direction: u32) -> Result<IEnumFORMATETC> {
            if direction != 1 {
                return Err(Error::from(windows::Win32::Foundation::E_NOTIMPL));
            }
            unsafe { windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc(&[hglobal_format(CF_HDROP.0)]) }
        }
        fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Option<&IAdviseSink>) -> Result<u32> {
            Err(Error::from(windows::Win32::Foundation::E_NOTIMPL))
        }
        fn DUnadvise(&self, _: u32) -> Result<()> {
            Err(Error::from(windows::Win32::Foundation::E_NOTIMPL))
        }
        fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
            Err(Error::from(windows::Win32::Foundation::E_NOTIMPL))
        }
    }

    /// Data object that serves one `HGLOBAL` format with arbitrary (possibly hostile) bytes.
    #[implement(windows::Win32::System::Com::IDataObject)]
    struct RawFormat {
        cf: u16,
        bytes: Vec<u8>,
    }

    impl IDataObject_Impl for RawFormat_Impl {
        fn GetData(&self, pformatetcin: *const FORMATETC) -> Result<STGMEDIUM> {
            let wanted = unsafe { &*pformatetcin };
            if wanted.cfFormat != self.cf {
                return Err(Error::from(windows::Win32::Foundation::DV_E_FORMATETC));
            }
            unsafe {
                // GlobalAlloc may round the block up (GlobalSize >= requested); the parsers must cope
                // with the reported size, which is what these tests exercise.
                let hglobal = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, self.bytes.len())?;
                let ptr = GlobalLock(hglobal) as *mut u8;
                std::ptr::copy_nonoverlapping(self.bytes.as_ptr(), ptr, self.bytes.len());
                let _ = GlobalUnlock(hglobal);
                let mut medium = STGMEDIUM::default();
                medium.tymed = TYMED_HGLOBAL.0 as u32;
                medium.u.hGlobal = hglobal;
                Ok(medium)
            }
        }
        fn GetDataHere(&self, _: *const FORMATETC, _: *mut STGMEDIUM) -> Result<()> {
            Err(Error::from(E_FAIL))
        }
        fn QueryGetData(&self, pformatetc: *const FORMATETC) -> windows::core::HRESULT {
            if unsafe { (*pformatetc).cfFormat } == self.cf { S_OK } else { windows::Win32::Foundation::DV_E_FORMATETC }
        }
        fn GetCanonicalFormatEtc(&self, _: *const FORMATETC, _: *mut FORMATETC) -> windows::core::HRESULT {
            E_FAIL
        }
        fn SetData(&self, _: *const FORMATETC, _: *const STGMEDIUM, _: windows::Win32::Foundation::BOOL) -> Result<()> {
            Err(Error::from(E_FAIL))
        }
        fn EnumFormatEtc(&self, direction: u32) -> Result<IEnumFORMATETC> {
            if direction != 1 {
                return Err(Error::from(windows::Win32::Foundation::E_NOTIMPL));
            }
            unsafe { windows::Win32::UI::Shell::SHCreateStdEnumFmtEtc(&[hglobal_format(self.cf)]) }
        }
        fn DAdvise(&self, _: *const FORMATETC, _: u32, _: Option<&IAdviseSink>) -> Result<u32> {
            Err(Error::from(windows::Win32::Foundation::E_NOTIMPL))
        }
        fn DUnadvise(&self, _: u32) -> Result<()> {
            Err(Error::from(windows::Win32::Foundation::E_NOTIMPL))
        }
        fn EnumDAdvise(&self) -> Result<IEnumSTATDATA> {
            Err(Error::from(windows::Win32::Foundation::E_NOTIMPL))
        }
    }

    fn cida_object(bytes: Vec<u8>) -> IDataObject {
        RawFormat { cf: clipboard_format(CFSTR_SHELLIDLIST), bytes }.into()
    }

    fn le32(values: &[u32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn malformed_cida_yields_no_names_and_no_crash() {
        init();
        let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
        cases.push(("empty buffer", vec![]));
        cases.push(("3 bytes (< header)", vec![1, 0, 0]));
        cases.push(("4 bytes, count 1, no offset table", le32(&[1])));
        // count claims 1000 items but the buffer holds 16 bytes.
        cases.push(("count larger than buffer", le32(&[1000, 12, 14, 14])));
        cases.push(("count overflow-sized", le32(&[u32::MAX, 0, 0, 0])));
        cases.push(("count zero", le32(&[0, 8])));
        // Offsets pointing far outside the buffer.
        cases.push(("offsets out of range", le32(&[1, 0x00FF_FFFF, 0xFFFF_FFF0])));
        cases.push(("child offset == len", {
            let mut b = le32(&[1, 12, 14]);
            b.extend_from_slice(&[0, 0]);
            b
        }));
        // Parent is an empty PIDL (00 00) at 12; child at 14 has cb=4 and runs to the end without a terminator.
        cases.push(("unterminated child PIDL", {
            let mut b = le32(&[1, 12, 14]);
            b.extend_from_slice(&[0, 0, 4, 0, b'A', b'A']);
            b
        }));
        // Parent PIDL whose item claims to extend past the buffer.
        cases.push(("unterminated parent PIDL", {
            let mut b = le32(&[1, 12, 12]);
            b.extend_from_slice(&[0xFF, 0x7F, 1, 2]);
            b
        }));
        // cb == 1 is never valid and would otherwise loop/overrun.
        cases.push(("item with cb of 1", {
            let mut b = le32(&[1, 12, 14]);
            b.extend_from_slice(&[0, 0, 1, 0, 0, 0]);
            b
        }));
        // Child offset is the last byte of the buffer (a cb needs 2 bytes).
        cases.push(("child at last byte", {
            let mut b = le32(&[1, 12, 13]);
            b.extend_from_slice(&[0, 0]);
            b
        }));
        for (label, bytes) in cases {
            let direct = names_from_cida(&bytes);
            let data = cida_object(bytes.clone());
            let via_object = read_source_names(&data);
            let via_idlist = names_from_idlist(&data).map_err(|e| e.code());
            println!(
                "  [malformed cida] {label}: {} bytes -> names_from_cida={direct:?} read_source_names={via_object:?} names_from_idlist={via_idlist:?}",
                bytes.len()
            );
            assert!(direct.is_empty() && via_object.is_empty(), "{label}: must yield no names");
            assert!(via_idlist.map(|v| v.is_empty()).unwrap_or(true), "{label}: names_from_idlist must be empty or Err");
        }
    }

    #[test]
    fn well_formed_cida_ranges_are_found_and_partial_children_survive() {
        // header(count=2) + 3 offsets = 16 bytes; parent empty PIDL at 16; child1 valid at 18; child2 unterminated at 26.
        let mut b = le32(&[2, 16, 18, 26]);
        b.extend_from_slice(&[0, 0]); // parent terminator
        b.extend_from_slice(&[6, 0, 1, 2, 3, 4, 0, 0]); // child 1: cb=6 + terminator
        b.extend_from_slice(&[8, 0, 9, 9]); // child 2 runs off the end
        let (parent, children) = parse_cida(&b).expect("header is valid");
        println!("  [cida ranges] parent={parent:?} children={children:?}");
        assert_eq!(parent, 16..18);
        assert_eq!(children, vec![18..26]);
        assert_eq!(pidl_extent(&b, 26), None);
    }

    #[test]
    fn read_dword_requires_four_bytes() {
        init();
        let cf = clipboard_format(CFSTR_PERFORMEDDROPEFFECT);
        let short: IDataObject = RawFormat { cf, bytes: vec![2, 0] }.into();
        let exact: IDataObject = RawFormat { cf, bytes: 2u32.to_le_bytes().to_vec() }.into();
        let (a, b) = (read_dword(&short, cf), read_dword(&exact, cf));
        println!("  [read_dword] 2-byte buffer -> {a:?}; 4-byte buffer -> {b:?}");
        // GlobalAlloc may round small blocks up, so the short case is only guaranteed to be None
        // when GlobalSize reports the requested size; the exact case must always read.
        assert_eq!(b, Some(2));
        let _ = a;
    }

    #[test]
    fn internal_drag_with_shell_hint_never_reaches_the_shell_target() {
        init();
        let sb = Sandbox::new("internal_shell");
        let source = sb.file_in_a("seven.txt");
        let data = data_object_for(HWND::default(), &[source.clone()]).expect("data object");
        let _guard = InternalDragGuard::enter(&[source.clone()]);
        // Hint is Shell with the default destination = B and Ctrl held: without the guard this would copy.
        let (target, recorded, hovers) = make_target(Destination::Folder(sb.b_string()), DropHint::Shell);
        let keys = MODIFIERKEYS_FLAGS(8 /* MK_CONTROL */ | 1 /* MK_LBUTTON */);
        let pt = POINTL { x: 5, y: 5 };
        let mut effect = ALL_EFFECTS;
        unsafe { target.DragEnter(&data, keys, pt, &mut effect) }.expect("DragEnter");
        let after_enter = effect.0;
        let delegate_after_enter = unsafe { AsImpl::<DropTarget>::as_impl(&target) }.state.borrow().delegate.is_some();
        effect = ALL_EFFECTS;
        unsafe { target.DragOver(keys, pt, &mut effect) }.expect("DragOver");
        let delegate_after_over = unsafe { AsImpl::<DropTarget>::as_impl(&target) }.state.borrow().delegate.is_some();
        effect = ALL_EFFECTS;
        let result = unsafe { target.Drop(&data, keys, pt, &mut effect) };
        std::thread::sleep(Duration::from_millis(300));
        let b_entries: Vec<_> = std::fs::read_dir(&sb.b).unwrap().flatten().map(|e| e.file_name()).collect();
        let a_entries: Vec<_> = std::fs::read_dir(&sb.a).unwrap().flatten().map(|e| e.file_name()).collect();
        println!(
            "  [internal+Shell hint, Ctrl] effect after enter={after_enter} delegate created={delegate_after_enter}/{delegate_after_over} drop effect={} result={result:?}; A={a_entries:?} B={b_entries:?} on_dropped={} hovers={:?}",
            effect.0,
            recorded.borrow().len(),
            hovers.borrow()
        );
        result.expect("Drop");
        assert_eq!(after_enter, DROPEFFECT_NONE.0);
        assert!(!delegate_after_enter && !delegate_after_over, "the default Shell target must not be created");
        assert_eq!(effect, DROPEFFECT_NONE);
        assert!(b_entries.is_empty());
        assert_eq!(a_entries.len(), 1, "no copy may appear in A");
        assert!(recorded.borrow().is_empty());
        assert!(hovers.borrow().iter().all(|h| h.1));
    }

    #[test]
    fn internal_drag_with_shell_to_folder_is_still_allowed() {
        init();
        let sb = Sandbox::new("internal_shellto");
        let source = sb.file_in_a("eight.txt");
        let data = data_object_for(HWND::default(), &[source]).expect("data object");
        let _guard = InternalDragGuard::enter(&[]);
        let (target, recorded, _) =
            make_target(Destination::Desktop, DropHint::ShellTo(Destination::Folder(sb.b_string())));
        let (_, _, _, result) = run_drop(&target, &data, DROPEFFECT_COPY, 0);
        let dest = sb.b.join("eight.txt");
        let (appeared, _) = pump_until(Duration::from_secs(10), || dest.exists());
        println!("  [internal+ShellTo] result={result:?} copied into B={appeared} on_dropped={}", recorded.borrow().len());
        result.expect("Drop");
        assert!(appeared);
        assert_eq!(recorded.borrow().len(), 1);
        assert!(recorded.borrow()[0].internal);
    }

    #[test]
    fn hdrop_fallback_names_and_copy_through_shell() {
        init();
        let sb = Sandbox::new("hdrop");
        let s1 = sb.file_in_a("h1.txt");
        let s2 = sb.file_in_a("h2.txt");
        let data: IDataObject = HdropOnly { paths: vec![s1.clone(), s2.clone()] }.into();
        let names = read_source_names(&data);
        println!("  [hdrop] names read from a CF_HDROP-only data object: {names:?}");
        assert_eq!(names, vec![s1.clone(), s2.clone()]);

        // A CF_HDROP-only object has no Shell ID list: the Shell's drop target still handles it.
        let (target, recorded, _) = make_target(Destination::Folder(sb.b_string()), DropHint::Shell);
        let (enter, over, finished, result) = run_drop(&target, &data, DROPEFFECT_COPY, 0);
        let (b1, b2) = (sb.b.join("h1.txt"), sb.b.join("h2.txt"));
        let (appeared, waited) = pump_until(Duration::from_secs(10), || b1.exists() && b2.exists());
        println!("  [hdrop] DragEnter effect={enter} DragOver effect={over} Drop effect={finished} result={result:?}; both copied={appeared} after {waited:?}; reported sources={:?}", recorded.borrow().first().map(|d| d.sources.clone()));
        result.expect("Drop");
        assert!(appeared);
        assert_eq!(recorded.borrow()[0].sources, vec![s1, s2]);
    }
}
