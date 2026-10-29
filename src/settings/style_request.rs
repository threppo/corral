//! Corral-only styling protocol: the settings process reads group listings, while the
//! engine alone edits/saves its current layout. No icon or portal file operations.
//!
//! Colour semantics follow `docs/design/colour-state-spec.md`: a patch field that is present sets
//! an own value (also when equal to the shared one); nothing here removes a per-Corral key except
//! the three reset actions of spec 4.
use super::Settings;
use crate::color_state::{self, Globals, SEE_THROUGH_WIRE_MAX, TintSource};
use crate::model::{self, GroupStyle, Layout};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Unix milliseconds, strictly increasing within the settings process.
    pub nonce: u64,
    pub layout_path: PathBuf,
    pub action: Action,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Edit { id: u64, patch: ColorPatch },
    EditAll { patch: ColorPatch },
    /// A pending StyleRequest carrying this action is unreadable by older builds (downgrade only).
    SetCamouflage { id: u64, value: Option<bool> },
    /// colour-state-spec 4: one Corral loses every own value of the five resettable groups.
    ResetGroup { id: u64 },
    /// colour-state-spec 4: the shared look goes back to the defaults; no Corral is touched.
    ResetBase,
    /// colour-state-spec 4: `ResetBase`, then every Corral loses its own look.
    ResetAll,
}

/// Fields that are present set an own value; absent fields are untouched (including font and tab
/// settings the colour page does not edit). Unknown fields are ignored so a request written by
/// another build can never make the settings file unreadable.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorPatch {
    pub color: Option<u32>,
    pub transparency_factor: Option<u32>,
    pub blend_icons: Option<bool>,
    pub blend_background: Option<bool>,
    pub tint_icons: Option<bool>,
    pub tint_icons_color: Option<u32>,
    /// camouflage-spec 5.2: explicit source, always written together with `tint_icons`.
    pub tint_icons_color_source: Option<String>,
    /// Titlebar text colour.
    pub text_color: Option<u32>,
    /// colour-state-spec 2.1: Auto is an explicit mode, never a colour value.
    pub text_color_auto: Option<bool>,
}
impl ColorPatch {
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
    pub fn merge(&mut self, newer: &Self) {
        macro_rules! merge { ($($f:ident),*) => { $(if newer.$f.is_some() { self.$f = newer.$f.clone(); })* }; }
        merge!(color, transparency_factor, blend_icons, blend_background, tint_icons, tint_icons_color, tint_icons_color_source, text_color, text_color_auto);
    }
    /// Set the patched fields as this Corral's own values.
    pub fn apply_to_style(&self, dst: &mut GroupStyle) {
        if let Some(v) = self.color { dst.color = Some(v); }
        if let Some(v) = self.text_color { dst.text_color = Some(v); }
        if let Some(v) = self.text_color_auto { dst.text_color_auto = Some(v); }
        if let Some(v) = self.transparency_factor { dst.transparency_factor = Some(v); }
        if let Some(v) = self.blend_icons { dst.blend_icons = Some(u32::from(v)); }
        if let Some(v) = self.blend_background { dst.blend_background = Some(u32::from(v)); }
        if let Some(v) = self.tint_icons { dst.tint_icons = Some(u32::from(v)); }
        if let Some(v) = self.tint_icons_color { dst.tint_icons_color = Some(v); }
        if let Some(v) = &self.tint_icons_color_source { dst.tint_icons_color_source = Some(v.clone()); }
    }
    /// Set the patched fields as the shared values.
    pub fn apply_to_settings(&self, s: &mut Settings) {
        if let Some(v) = self.color { s.color = v; }
        if let Some(v) = self.text_color { s.text_color = v; }
        if let Some(v) = self.text_color_auto { s.text_color_auto = v; }
        if let Some(v) = self.transparency_factor { s.transparency_factor = v; }
        if let Some(v) = self.blend_icons { s.desktop_blend_icons = v; }
        if let Some(v) = self.blend_background { s.desktop_blend_corral_backgrounds = v; }
        if let Some(v) = self.tint_icons { s.desktop_tint_icons = v; }
        if let Some(v) = self.tint_icons_color { s.desktop_tint_icons_color = v; }
        if let Some(v) = &self.tint_icons_color_source { s.desktop_tint_icons_color_source = v.clone(); }
    }
    pub fn apply_to_globals(&self, g: &mut Globals) {
        if let Some(v) = self.color { g.color = v; }
        if let Some(v) = self.text_color { g.text_color = v; }
        if let Some(v) = self.text_color_auto { g.text_auto = v; }
        if let Some(v) = self.transparency_factor { g.see_through = v; }
        if let Some(v) = self.blend_icons { g.blend_icons = v; }
        if let Some(v) = self.blend_background { g.blend_background = v; }
        if let Some(v) = self.tint_icons { g.tint_icons = v; }
        if let Some(v) = self.tint_icons_color { g.tint_color = v; }
        if let Some(v) = self.tint_icons_color_source.as_deref().and_then(TintSource::parse) { g.tint_source = v; }
    }
    /// Put the patched fields of `dst` back to `old` (exactly, including absent keys).
    pub fn restore_style(&self, dst: &mut GroupStyle, old: &GroupStyle) {
        macro_rules! restore { ($($f:ident),*) => { $(if self.$f.is_some() { dst.$f = old.$f.clone(); })* }; }
        restore!(color, transparency_factor, blend_icons, blend_background, tint_icons, tint_icons_color, tint_icons_color_source, text_color, text_color_auto);
    }
    /// Put the patched shared fields back to `before`.
    pub fn restore_settings(&self, s: &mut Settings, before: &Globals) {
        if self.color.is_some() { s.color = before.color; }
        if self.text_color.is_some() { s.text_color = before.text_color; }
        if self.text_color_auto.is_some() { s.text_color_auto = before.text_auto; }
        if self.transparency_factor.is_some() { s.transparency_factor = before.see_through; }
        if self.blend_icons.is_some() { s.desktop_blend_icons = before.blend_icons; }
        if self.blend_background.is_some() { s.desktop_blend_corral_backgrounds = before.blend_background; }
        if self.tint_icons.is_some() { s.desktop_tint_icons = before.tint_icons; }
        if self.tint_icons_color.is_some() { s.desktop_tint_icons_color = before.tint_color; }
        if self.tint_icons_color_source.is_some() { s.desktop_tint_icons_color_source = before.tint_source.as_str().into(); }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestResult {
    pub nonce: u64,
    pub ok: bool,
    pub message: String,
}

/// Preserve newer engine acknowledgements/sender fallbacks while still allowing a freshly
/// issued page request to replace the older disk command.
pub fn merge_protocol(edited: &mut Settings, stored: &Settings) {
    let disk_nonce = stored.style_request.as_ref().map(|r| r.nonce)
        .into_iter().chain(stored.style_result.as_ref().map(|r| r.nonce)).max();
    if !edited.style_request.as_ref().is_some_and(|r| disk_nonce.is_none_or(|n| r.nonce > n)) {
        edited.style_request = stored.style_request.clone();
    }
    if edited.style_result.as_ref().map(|r| r.nonce) <= stored.style_result.as_ref().map(|r| r.nonce) {
        edited.style_result = stored.style_result.clone();
    }
}

/// A Corral as the settings process lists it.
pub type Target = crate::color_state::page::CorralInfo;

/// Read only, validated layout, backup recovery follows the engine's loader.
pub fn targets(path: &Path) -> Result<Vec<Target>, model::LoadError> {
    let layout = model::load_with_backup(path)?;
    let mut targets: Vec<_> = layout.groups.into_iter().map(|g| Target {
        id: g.id, title: if g.title.is_empty() { "Corral".into() } else { g.title }, style: g.style,
    }).collect();
    targets.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()).then(a.id.cmp(&b.id)));
    Ok(targets)
}

pub fn same_layout(a: &Path, b: &Path) -> bool {
    let normalize = |p: &Path| std::fs::canonicalize(p)
        .or_else(|_| std::path::absolute(p)).unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy().to_lowercase();
    normalize(a) == normalize(b)
}

/// Mutates layout metadata only. Caller works on a clone and saves it before publishing it.
pub fn apply(layout: &mut Layout, settings: &mut Settings, action: &Action) -> Result<(), &'static str> {
    const MISSING: &str = "This Corral group no longer exists";
    match action {
        // colour-state-spec 2.3: scope All writes only shared keys; Corrals keep their own values.
        Action::EditAll { patch } => {
            validate_patch(patch)?;
            patch.apply_to_settings(settings);
        }
        Action::Edit { id, patch } => {
            validate_patch(patch)?;
            let group = layout.group_mut(*id).ok_or(MISSING)?;
            patch.apply_to_style(&mut group.style);
        }
        Action::SetCamouflage { id, value } => {
            // camouflage-spec 5.4: an explicit On/Off is retained even when equal to global.
            layout.group_mut(*id).ok_or(MISSING)?.style.blend_icons = value.map(u32::from);
        }
        Action::ResetGroup { id } => color_state::reset_group(&mut layout.group_mut(*id).ok_or(MISSING)?.style),
        Action::ResetBase => color_state::reset_base(settings),
        Action::ResetAll => color_state::reset_all(layout, settings),
    }
    Ok(())
}

pub fn validate_patch(p: &ColorPatch) -> Result<(), &'static str> {
    if p.tint_icons_color_source.as_deref().is_some_and(|v| TintSource::parse(v).is_none()) { return Err("Invalid tint colour source"); }
    // colour-state-spec 2.1: tint on/off and its source are always one edit.
    if p.tint_icons.is_some() != p.tint_icons_color_source.is_some() { return Err("Tint on/off and source must be set together"); }
    if p.color.is_some_and(|v| v > 0xffffff) || p.tint_icons_color.is_some_and(|v| v > 0xffffff) || p.text_color.is_some_and(|v| v > 0xffffff)
        || p.transparency_factor.is_some_and(|v| v > SEE_THROUGH_WIRE_MAX) { Err("Invalid group colour value") } else { Ok(()) }
}

/// Preview runs coalesce; final/cancel requests are ordered barriers. IPC acceptance is not
/// an apply acknowledgement, so a new gesture must never overwrite an accepted final value.
#[derive(Default)]
pub struct PendingColors { pub values: std::collections::VecDeque<LiveRequest> }
impl PendingColors {
    /// Most queued requests the engine holds; a sender beyond this is refused (WM_COPYDATA returns 2).
    pub const MAX: usize = 64;

    /// `false` when the queue is full (the request is dropped).
    pub fn push(&mut self, mut request: LiveRequest) -> bool {
        // Only draft frames of the same session replace each other; commits and cancels are
        // barriers that keep their acceptance order.
        if !request.persist && !request.cancel {
            let index = self.values.iter().enumerate().rev()
                .take_while(|(_, old)| !old.persist && !old.cancel)
                .find_map(|(i, old)| (old.session == request.session && old.target == request.target).then_some(i));
            if let Some(i) = index {
                let old = self.values.remove(i).unwrap();
                let mut union = old.patch.clone();
                union.merge(&request.patch);
                request.patch = union;
            }
        }
        if self.values.len() >= Self::MAX { return false; }
        self.values.push_back(request);
        true
    }
}

// Bounded, versioned Corral-only WM_COPYDATA colour channel.
pub const LIVE_MAGIC: usize = 0x4352_434c;
pub const LIVE_MAX_BYTES: usize = 16 * 1024;
/// Corral-owned surface geometry changed: update thumbnail placement without polling or hooks.
pub const WM_PREVIEW_CHANGED: u32 = 0x8000 + 0x172;
/// Engine to settings window: the outcome of a colour request (`status`, lparam = session id).
pub const WM_COLOR_STATUS: u32 = 0x8000 + 133;
pub mod status {
    /// The engine took the request.
    pub const APPLIED: usize = 0;
    /// The engine was unreachable; the change was queued in the settings file.
    pub const QUEUED_IN_FILE: usize = 1;
    /// The engine was unreachable and the settings file could not be written.
    pub const FILE_SAVE_FAILED: usize = 2;
    /// The engine could not store a commit; it put the stored look back (colour-state-spec 3.4).
    pub const ENGINE_SAVE_FAILED: usize = 4;
}

/// One message of the live colour channel (colour-state-spec 3.2). A draft frame carries the
/// session's cumulative patch; `persist` stores it; `cancel` drops the session's overlay.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveRequest {
    pub version: u32,
    pub layout_path: PathBuf,
    pub target: Option<u64>,
    /// Draft overlay id: a message with another id ends the previous overlay.
    pub session: u64,
    #[serde(default)]
    pub patch: ColorPatch,
    pub persist: bool,
    #[serde(default)]
    pub cancel: bool,
}
impl LiveRequest {
    pub const VERSION: u32 = 2;
    pub fn new(layout_path: PathBuf, target: Option<u64>, session: u64, patch: ColorPatch, persist: bool) -> Self {
        Self { version: Self::VERSION, layout_path, target, session, patch, persist, cancel: false }
    }
    pub fn cancel(layout_path: PathBuf, target: Option<u64>, session: u64) -> Self {
        Self { version: Self::VERSION, layout_path, target, session, patch: ColorPatch::default(), persist: false, cancel: true }
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != Self::VERSION { return Err("unsupported version"); }
        if self.layout_path.as_os_str().is_empty() || self.layout_path.as_os_str().len() > 4096 { return Err("invalid layout path"); }
        if self.persist && self.cancel { return Err("a request cannot both store and cancel"); }
        validate_patch(&self.patch)
    }
    pub fn encode(&self) -> Option<Vec<u8>> {
        self.validate().ok()?;
        let bytes = serde_json::to_vec(self).ok()?;
        (bytes.len() <= LIVE_MAX_BYTES).then_some(bytes)
    }
    pub fn decode(magic: usize, bytes: &[u8]) -> Result<Self, &'static str> {
        if magic != LIVE_MAGIC || bytes.is_empty() || bytes.len() > LIVE_MAX_BYTES { return Err("invalid payload envelope"); }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| "invalid payload")?;
        value.validate()?;
        Ok(value)
    }
}
/// Pure destination policy apart from read-only path normalization. The receiver never opens
/// the sender's layout; the running engine's in-memory IDs are authoritative.
pub fn live_guard(request: &LiveRequest, engine_path: &Path, layout: &Layout, isolated: bool, _busy: bool) -> Result<(), &'static str> {
    request.validate()?;
    if !same_layout(&request.layout_path, engine_path) { return Err("layout path mismatch"); }
    if isolated { return Err("preview/real-settings isolation"); }
    if request.target.is_some_and(|id| !layout.groups.iter().any(|g| g.id == id)) { return Err("missing Corral target"); }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveOutcome { Applied, Rejected, Unavailable }
/// CorralMessage is a hidden top-level popup, not HWND_MESSAGE. Bound the entire discovery/send.
pub fn send_live(request: &LiveRequest) -> LiveOutcome {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::core::w;
    let Some(bytes) = request.encode() else { return LiveOutcome::Rejected };
    let data = COPYDATASTRUCT { dwData: LIVE_MAGIC, cbData: bytes.len() as u32, lpData: bytes.as_ptr() as *mut _ };
    let start = std::time::Instant::now();
    let mut after = HWND::default();
    let mut rejected = false;
    for _ in 0..32 {
        let Ok(hwnd) = (unsafe { FindWindowExW(HWND::default(), after, w!("CorralMessage"), None) }) else { break };
        if hwnd.is_invalid() { break; }
        after = hwnd;
        let left = 200u128.saturating_sub(start.elapsed().as_millis());
        if left == 0 { break; }
        let mut result = 0usize;
        let answered = unsafe { SendMessageTimeoutW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&data as *const _ as isize), SMTO_ABORTIFHUNG | SMTO_BLOCK, left as u32, Some(&mut result)) };
        if answered.0 != 0 {
            if result == 1 { return LiveOutcome::Applied; }
            if result == 2 { rejected = true; }
        }
    }
    if rejected { LiveOutcome::Rejected } else { LiveOutcome::Unavailable }
}
pub fn target_for_choice(choice: usize, targets: &[Target]) -> Option<Option<u64>> {
    match choice { 1 => Some(None), n if n >= 2 => targets.get(n - 2).map(|t| Some(t.id)), _ => None }
}

// "Reload settings now": the settings file is already saved; this only skips the 2 s poll.
pub const RELOAD_MAGIC: usize = 0x4352_5253;
pub const RELOAD_MAX_BYTES: usize = 4096 + 128;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReloadRequest { pub version: u32, pub layout_path: PathBuf }
impl ReloadRequest {
    pub fn new(layout_path: PathBuf) -> Self { Self { version: 1, layout_path } }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 { return Err("unsupported version"); }
        if self.layout_path.as_os_str().is_empty() || self.layout_path.as_os_str().len() > 4096 { return Err("invalid layout path"); }
        Ok(())
    }
    pub fn encode(&self) -> Option<Vec<u8>> {
        self.validate().ok()?;
        let bytes = serde_json::to_vec(self).ok()?;
        (bytes.len() <= RELOAD_MAX_BYTES * 4).then_some(bytes)
    }
    pub fn decode(magic: usize, bytes: &[u8]) -> Result<Self, &'static str> {
        if magic != RELOAD_MAGIC || bytes.is_empty() || bytes.len() > RELOAD_MAX_BYTES * 4 { return Err("invalid payload envelope"); }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| "invalid payload")?;
        value.validate()?;
        Ok(value)
    }
}
/// Bounded (200 ms total) best effort; the saved file and the 2 s poll remain the fallback.
pub fn send_reload(request: &ReloadRequest) -> LiveOutcome {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::core::w;
    let Some(bytes) = request.encode() else { return LiveOutcome::Rejected };
    let data = COPYDATASTRUCT { dwData: RELOAD_MAGIC, cbData: bytes.len() as u32, lpData: bytes.as_ptr() as *mut _ };
    let start = std::time::Instant::now();
    let mut after = HWND::default();
    for _ in 0..32 {
        let Ok(hwnd) = (unsafe { FindWindowExW(HWND::default(), after, w!("CorralMessage"), None) }) else { break };
        if hwnd.is_invalid() { break; }
        after = hwnd;
        let left = 200u128.saturating_sub(start.elapsed().as_millis());
        if left == 0 { break; }
        let mut result = 0usize;
        let answered = unsafe { SendMessageTimeoutW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&data as *const _ as isize), SMTO_ABORTIFHUNG | SMTO_BLOCK, left as u32, Some(&mut result)) };
        if answered.0 != 0 && result == 1 { return LiveOutcome::Applied; }
    }
    LiveOutcome::Unavailable
}

// colour-state-spec 3.1: the Corral menu's "Colors..." command asks the open settings window to show
// the Personalization page for that Corral and open its Color popover.
pub const FOCUS_MAGIC: usize = 0x4352_4643;
pub const FOCUS_MAX_BYTES: usize = 4096 + 128;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FocusRequest { pub version: u32, pub layout_path: PathBuf, pub corral: u64 }
impl FocusRequest {
    pub fn new(layout_path: PathBuf, corral: u64) -> Self { Self { version: 1, layout_path, corral } }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1 { return Err("unsupported version"); }
        if self.layout_path.as_os_str().is_empty() || self.layout_path.as_os_str().len() > 4096 { return Err("invalid layout path"); }
        Ok(())
    }
    pub fn encode(&self) -> Option<Vec<u8>> {
        self.validate().ok()?;
        let bytes = serde_json::to_vec(self).ok()?;
        (bytes.len() <= FOCUS_MAX_BYTES * 4).then_some(bytes)
    }
    pub fn decode(magic: usize, bytes: &[u8]) -> Result<Self, &'static str> {
        if magic != FOCUS_MAGIC || bytes.is_empty() || bytes.len() > FOCUS_MAX_BYTES * 4 { return Err("invalid payload envelope"); }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| "invalid payload")?;
        value.validate()?;
        Ok(value)
    }
}
/// Ask an open settings window (class `CorralSettings`) to show `request.corral`; `Unavailable`
/// when none answers, so the caller starts one.
pub fn send_focus(request: &FocusRequest) -> LiveOutcome {
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::Win32::System::DataExchange::COPYDATASTRUCT;
    use windows::core::w;
    let Some(bytes) = request.encode() else { return LiveOutcome::Rejected };
    let data = COPYDATASTRUCT { dwData: FOCUS_MAGIC, cbData: bytes.len() as u32, lpData: bytes.as_ptr() as *mut _ };
    let start = std::time::Instant::now();
    let mut after = HWND::default();
    for _ in 0..16 {
        let Ok(hwnd) = (unsafe { FindWindowExW(HWND::default(), after, w!("CorralSettings"), None) }) else { break };
        if hwnd.is_invalid() { break; }
        after = hwnd;
        let left = 500u128.saturating_sub(start.elapsed().as_millis());
        if left == 0 { break; }
        let mut result = 0usize;
        let answered = unsafe { SendMessageTimeoutW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&data as *const _ as isize), SMTO_ABORTIFHUNG | SMTO_BLOCK, left as u32, Some(&mut result)) };
        if answered.0 != 0 && result == 1 { return LiveOutcome::Applied; }
    }
    LiveOutcome::Unavailable
}
/// True while a settings window of the process `owner` (low 16 bits of its id, see
/// `color_state::session_owner`) exists: the engine drops a draft overlay when its window is gone.
pub fn settings_window_of(owner: u32) -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowExW, GetWindowThreadProcessId};
    use windows::core::w;
    let mut after = HWND::default();
    for _ in 0..32 {
        let Ok(hwnd) = (unsafe { FindWindowExW(HWND::default(), after, w!("CorralSettings"), None) }) else { return false };
        if hwnd.is_invalid() { return false; }
        after = hwnd;
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid & 0xffff == owner { return true; }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Layout, Settings, u64) {
        let mut l = Layout::new();
        let id = l.add_group("QA".into(), "m".into(), Default::default(), model::GroupContent::Items { items: vec!["scratch.txt".into()] });
        l.groups[0].style = GroupStyle { color: Some(123), font_name: Some("Test".into()), text_color: Some(9), text_color_auto: Some(false), ..Default::default() };
        (l, Settings::default(), id)
    }
    fn tint_pair(on: bool, source: &str) -> ColorPatch {
        ColorPatch { tint_icons: Some(on), tint_icons_color_source: Some(source.into()), ..Default::default() }
    }

    #[test]
    fn a_corral_edit_sets_an_own_value_even_when_it_equals_the_shared_one() {
        let (mut l, mut s, id) = fixture();
        s.color = 456;
        apply(&mut l, &mut s, &Action::Edit { id, patch: ColorPatch { color: Some(456), ..Default::default() } }).unwrap();
        assert_eq!(l.groups[0].style.color, Some(456), "equal-to-shared stays own (colour-state-spec 5)");
        assert_eq!(s.color, 456);
        // an unedited field and the font keep their values
        assert_eq!(l.groups[0].style.text_color, Some(9));
        assert_eq!(l.groups[0].style.font_name.as_deref(), Some("Test"));
    }

    #[test]
    fn edit_all_writes_only_shared_keys_and_leaves_every_corral_alone() {
        let (mut l, mut s, _) = fixture();
        let before = l.clone();
        let patch = ColorPatch { color: Some(0x112233), transparency_factor: Some(900), text_color: Some(5), text_color_auto: Some(false), ..tint_pair(true, "custom") };
        apply(&mut l, &mut s, &Action::EditAll { patch }).unwrap();
        assert_eq!(l, before);
        assert_eq!((s.color, s.transparency_factor, s.text_color, s.text_color_auto), (0x112233, 900, 5, false));
        assert!(s.desktop_tint_icons);
        assert_eq!(s.desktop_tint_icons_color_source, "custom");
    }

    #[test]
    fn tint_on_off_and_source_travel_as_one_edit() {
        let (mut l, mut s, id) = fixture();
        for patch in [ColorPatch { tint_icons: Some(true), ..Default::default() }, ColorPatch { tint_icons_color_source: Some("custom".into()), ..Default::default() }] {
            let before = (l.clone(), s.clone());
            assert!(apply(&mut l, &mut s, &Action::Edit { id, patch: patch.clone() }).is_err());
            assert!(apply(&mut l, &mut s, &Action::EditAll { patch }).is_err());
            assert_eq!((l.clone(), s.clone()), before, "a refused edit changes nothing");
        }
        apply(&mut l, &mut s, &Action::Edit { id, patch: tint_pair(false, "custom") }).unwrap();
        assert_eq!((l.groups[0].style.tint_icons, l.groups[0].style.tint_icons_color_source.as_deref()), (Some(0), Some("custom")));
        let bad = ColorPatch { tint_icons: Some(true), tint_icons_color_source: Some("invalid".into()), ..Default::default() };
        assert!(apply(&mut l, &mut s, &Action::Edit { id, patch: bad }).is_err());
    }

    #[test]
    fn auto_text_is_a_mode_not_a_colour() {
        let (mut l, mut s, id) = fixture();
        apply(&mut l, &mut s, &Action::Edit { id, patch: ColorPatch { text_color_auto: Some(true), ..Default::default() } }).unwrap();
        assert_eq!(l.groups[0].style.text_color_auto, Some(true));
        assert_eq!(l.groups[0].style.text_color, Some(9), "Auto does not overwrite the custom colour");
    }

    #[test]
    fn camouflage_override_retains_explicit_global_match_and_inherit_clears_it() {
        let (mut l,mut s,id)=fixture();
        for global in [false,true] {
            s.desktop_blend_icons=global;
            apply(&mut l,&mut s,&Action::SetCamouflage{id,value:Some(global)}).unwrap();
            assert_eq!(l.groups[0].style.blend_icons,Some(u32::from(global)));
            s.desktop_blend_icons=!global;
            assert_eq!(l.groups[0].style.blend_icons,Some(u32::from(global)));
            apply(&mut l,&mut s,&Action::SetCamouflage{id,value:None}).unwrap();
            assert_eq!(l.groups[0].style.blend_icons,None);
        }
    }

    #[test]
    fn missing_group_and_invalid_values_do_not_mutate() {
        let (mut l, mut s, id) = fixture();
        let before = (l.clone(), s.clone());
        assert!(apply(&mut l, &mut s, &Action::ResetGroup { id: id + 1 }).is_err());
        assert!(apply(&mut l, &mut s, &Action::Edit { id: id + 1, patch: ColorPatch { color: Some(1), ..Default::default() } }).is_err());
        assert!(apply(&mut l, &mut s, &Action::Edit { id, patch: ColorPatch { color: Some(u32::MAX), ..Default::default() } }).is_err());
        assert!(apply(&mut l, &mut s, &Action::Edit { id, patch: ColorPatch { text_color: Some(0x1000000), ..Default::default() } }).is_err());
        assert!(apply(&mut l, &mut s, &Action::EditAll { patch: ColorPatch { transparency_factor: Some(SEE_THROUGH_WIRE_MAX + 1), ..Default::default() } }).is_err());
        assert_eq!((l, s), before);
    }

    #[test]
    fn patches_roundtrip_ignore_unknown_keys_and_merge() {
        let old: ColorPatch = serde_json::from_str(r#"{"color":5,"tint_icons_color_use":7}"#).unwrap();
        assert_eq!(old, ColorPatch { color: Some(5), ..Default::default() });
        let new = ColorPatch { text_color: Some(9), text_color_auto: Some(false), ..Default::default() };
        assert_eq!(serde_json::from_str::<ColorPatch>(&serde_json::to_string(&new).unwrap()).unwrap(), new);
        let mut merged = ColorPatch { color: Some(1), ..Default::default() };
        merged.merge(&new);
        assert_eq!((merged.color, merged.text_color), (Some(1), Some(9)));
        assert!(ColorPatch::default().is_empty() && !merged.is_empty());
    }

    #[test]
    fn restore_puts_back_exactly_the_patched_fields() {
        let mut style = GroupStyle { color: Some(1), text_color: Some(2), font_name: Some("F".into()), ..Default::default() };
        let old = style.clone();
        let patch = ColorPatch { color: Some(7), text_color_auto: Some(true), blend_icons: Some(true), ..Default::default() };
        patch.apply_to_style(&mut style);
        style.text_color = Some(99); // not part of the patch: untouched by restore
        patch.restore_style(&mut style, &old);
        assert_eq!((style.color, style.text_color_auto, style.blend_icons), (Some(1), None, None));
        assert_eq!(style.text_color, Some(99));
        let mut s = Settings::default();
        let before = Globals::from_settings(&s);
        patch.apply_to_settings(&mut s);
        assert_eq!((s.color, s.text_color_auto, s.desktop_blend_icons), (7, true, true));
        patch.restore_settings(&mut s, &before);
        assert_eq!(Globals::from_settings(&s), before);
    }

    #[test]
    fn requests_roundtrip_and_never_replay_from_snapshots() {
        let (_, mut s, _) = fixture();
        s.style_request = Some(Request { nonce: 123, layout_path: "QA/layout.json".into(), action: Action::ResetAll });
        s.style_result = Some(RequestResult { nonce: 122, ok: true, message: "done".into() });
        assert_eq!(serde_json::from_str::<Settings>(&serde_json::to_string(&s).unwrap()).unwrap(), s);
        let clean = crate::snapshot::for_capture(&s);
        assert!(clean.style_request.is_none() && clean.style_result.is_none());
    }

    #[test]
    fn page_save_keeps_new_reset_request_and_does_not_resurrect_acknowledged_request() {
        let mut stored = Settings::default();
        stored.style_result = Some(RequestResult {nonce:10,ok:true,message:"done".into()});
        let mut edited = stored.clone();
        edited.style_request = Some(Request {nonce:11,layout_path:"QA/layout.json".into(),action:Action::ResetAll});
        merge_protocol(&mut edited,&stored);
        assert_eq!(edited.style_request.as_ref().unwrap().nonce,11);
        stored.style_result = Some(RequestResult {nonce:11,ok:true,message:"done".into()});
        merge_protocol(&mut edited,&stored);
        assert!(edited.style_request.is_none());
        stored.style_request = Some(Request {nonce:12,layout_path:"QA/layout.json".into(),action:Action::EditAll {patch:ColorPatch {color:Some(13),..Default::default()}}});
        merge_protocol(&mut edited,&stored);
        assert_eq!(edited.style_request,stored.style_request);
        assert_eq!(edited.style_result,stored.style_result);
    }

    fn request() -> LiveRequest { LiveRequest::new("QA/layout.json".into(), Some(991), 7, ColorPatch { color: Some(5), ..Default::default() }, false) }
    #[test]
    fn wire_roundtrip_all_targets_and_kinds() {
        for target in [None, Some(991)] { for persist in [false, true] {
            let mut r = request(); r.target = target; r.persist = persist;
            assert_eq!(LiveRequest::decode(LIVE_MAGIC, &r.encode().unwrap()).unwrap(), r);
        }}
        let cancel = LiveRequest::cancel("QA/layout.json".into(), Some(3), 9);
        assert_eq!(LiveRequest::decode(LIVE_MAGIC, &cancel.encode().unwrap()).unwrap(), cancel);
    }
    #[test]
    fn rejects_bad_envelopes_versions_and_values() {
        let r = request(); let bytes = r.encode().unwrap();
        assert!(LiveRequest::decode(0, &bytes).is_err());
        assert!(LiveRequest::decode(LIVE_MAGIC, &[]).is_err());
        assert!(LiveRequest::decode(LIVE_MAGIC, &vec![0; LIVE_MAX_BYTES + 1]).is_err());
        assert!(LiveRequest::decode(LIVE_MAGIC, b"{}").is_err());
        let mut bad = r.clone(); bad.version = 1; assert!(bad.encode().is_none());
        bad = r.clone(); bad.patch.color = Some(0x1000000); assert!(bad.encode().is_none());
        bad = r.clone(); bad.patch.transparency_factor = Some(SEE_THROUGH_WIRE_MAX + 1); assert!(bad.encode().is_none());
        bad = r.clone(); bad.persist = true; bad.cancel = true; assert!(bad.encode().is_none());
        bad = r; bad.layout_path = PathBuf::new(); assert!(bad.encode().is_none());
    }
    #[test]
    fn destination_rejects_other_layout_preview_and_deleted_target() {
        let mut r = request(); let layout = Layout::new();
        let path = r.layout_path.clone(); r.target = None;
        assert!(live_guard(&r, &path, &layout, false, false).is_ok());
        assert!(live_guard(&r, Path::new("QA/other.json"), &layout, false, false).is_err());
        assert!(live_guard(&r, &path, &layout, true, false).is_err());
        assert!(live_guard(&r, &path, &layout, false, true).is_ok());
        r.target = Some(991);
        assert!(live_guard(&r, &path, &layout, false, false).is_err());
    }
    #[test]
    fn menu_maps_stable_ids_not_indices_and_cancel() {
        let targets = vec![Target { id: 42, title: "Z".into(), style: Default::default() }, Target { id: 7, title: "A".into(), style: Default::default() }];
        assert_eq!(target_for_choice(0, &targets), None);
        assert_eq!(target_for_choice(1, &targets), Some(None));
        assert_eq!(target_for_choice(2, &targets), Some(Some(42)));
        assert_eq!(target_for_choice(3, &targets), Some(Some(7)));
        assert_eq!(target_for_choice(4, &targets), None);
    }

    #[test]
    fn draft_frames_of_one_session_coalesce_but_commits_cancels_and_other_sessions_are_barriers() {
        let mut q = PendingColors::default();
        for i in 1..100 {
            let mut r = LiveRequest::new("QA/layout.json".into(), None, 1, ColorPatch { color: Some(i), ..Default::default() }, false);
            r.version = LiveRequest::VERSION;
            q.push(r);
        }
        assert_eq!(q.values.len(), 1);
        assert_eq!(q.values[0].patch.color, Some(99));
        // another session is never merged into the first
        q.push(LiveRequest::new("QA/layout.json".into(), None, 2, ColorPatch { color: Some(7), ..Default::default() }, false));
        assert_eq!(q.values.iter().map(|r| r.session).collect::<Vec<_>>(), [1, 2]);
        // a commit is a barrier: later drafts of the same session do not slip before it
        q.push(LiveRequest::new("QA/layout.json".into(), None, 2, ColorPatch { color: Some(8), ..Default::default() }, true));
        q.push(LiveRequest::new("QA/layout.json".into(), None, 2, ColorPatch { color: Some(9), ..Default::default() }, false));
        assert_eq!(q.values.len(), 4);
        assert_eq!(q.values[2].patch.color, Some(8));
        assert!(q.values[2].persist && !q.values[3].persist);
        q.push(LiveRequest::cancel("QA/layout.json".into(), None, 2));
        assert!(q.values.back().unwrap().cancel);
    }
    #[test]
    fn pending_queue_refuses_commits_beyond_its_cap() {
        let mut q = PendingColors::default();
        for _ in 0..PendingColors::MAX {
            assert!(q.push(LiveRequest::new("QA/layout.json".into(), None, 1, ColorPatch::default(), true)));
        }
        assert!(!q.push(LiveRequest::new("QA/layout.json".into(), None, 1, ColorPatch::default(), true)));
        assert_eq!(q.values.len(), PendingColors::MAX);
    }

    #[test]
    fn reload_payload_validation() {
        let r = ReloadRequest::new("QA/layout.json".into());
        let bytes = r.encode().unwrap();
        assert_eq!(ReloadRequest::decode(RELOAD_MAGIC, &bytes).unwrap(), r);
        assert!(ReloadRequest::decode(LIVE_MAGIC, &bytes).is_err());
        assert!(ReloadRequest::decode(0, &bytes).is_err());
        assert!(ReloadRequest::decode(RELOAD_MAGIC, &[]).is_err());
        assert!(ReloadRequest::decode(RELOAD_MAGIC, &vec![b' '; RELOAD_MAX_BYTES * 4 + 1]).is_err());
        assert!(ReloadRequest::decode(RELOAD_MAGIC, b"{}").is_err());
        assert!(ReloadRequest::decode(RELOAD_MAGIC, br#"{"version":1,"layout_path":"a","x":1}"#).is_err());
        assert!(ReloadRequest::decode(RELOAD_MAGIC, br#"{"version":2,"layout_path":"a"}"#).is_err());
        assert!(ReloadRequest::decode(RELOAD_MAGIC, br#"{"version":1,"layout_path":""}"#).is_err());
        assert!(ReloadRequest { version: 1, layout_path: PathBuf::new() }.encode().is_none());
        assert!(LiveRequest::decode(LIVE_MAGIC, &bytes).is_err());
    }

    #[test]
    fn focus_payload_validation() {
        let r = FocusRequest::new("QA/layout.json".into(), 42);
        let bytes = r.encode().unwrap();
        assert_eq!(FocusRequest::decode(FOCUS_MAGIC, &bytes).unwrap(), r);
        assert!(FocusRequest::decode(RELOAD_MAGIC, &bytes).is_err());
        assert!(FocusRequest::decode(FOCUS_MAGIC, &[]).is_err());
        assert!(FocusRequest::decode(FOCUS_MAGIC, b"{}").is_err());
        assert!(FocusRequest::decode(FOCUS_MAGIC, br#"{"version":1,"layout_path":"a","corral":1,"x":1}"#).is_err());
        assert!(FocusRequest { version: 1, layout_path: PathBuf::new(), corral: 1 }.encode().is_none());
        assert!(ReloadRequest::decode(RELOAD_MAGIC, &bytes).is_err());
    }
}
