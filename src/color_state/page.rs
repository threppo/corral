//! The colour page controller (colour-state-spec 2.3, 3 and 6): scope, popover sessions and
//! gestures, commit and cancel, and the reload rules. It is pure: the settings window feeds it
//! input and sends what it returns; tests drive it directly.

use super::{ColorEdit, Effective, Globals, Overrides, Slot, resolve};
use crate::model::GroupStyle;
use crate::settings::style_request::ColorPatch;

/// What the page edits: the shared look, or one Corral identified by its stable id (spec 2.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Corral(u64),
}

/// A Corral as the page knows it (read from the layout; Folder Gates and hidden tabs included).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorralInfo {
    pub id: u64,
    pub title: String,
    pub style: GroupStyle,
}

/// The colour-valued fields that run a popover session or a slider gesture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    Background,
    /// The custom text colour; the Auto swatch is a one-shot edit.
    Text,
    SeeThrough,
    TintColor,
}

impl Field {
    pub fn edit(self, value: u32) -> ColorEdit {
        match self {
            Field::Background => ColorEdit::Background(value),
            Field::Text => ColorEdit::TextCustom(value),
            Field::SeeThrough => ColorEdit::SeeThrough(value),
            Field::TintColor => ColorEdit::TintColor(value),
        }
    }
}

/// One popover session or gesture (spec 3.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: u64,
    pub scope: Scope,
    pub field: Field,
    /// The slot state at open: own, or following the shared value.
    pub was_own: bool,
    pub start: u32,
    pub draft: u32,
    /// An explicit choice happened (it stays set when the draft returns to its start).
    pub explicit: bool,
}

/// A change to store. `patch` sets own values only; nothing here ever writes "inherit".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub session: u64,
    pub scope: Scope,
    pub edits: Vec<ColorEdit>,
    pub patch: ColorPatch,
}

/// One live frame of the draft overlay (spec 3.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub session: u64,
    pub scope: Scope,
    pub patch: ColorPatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Caption {
    SaveFailed,
    /// Title of the Corral that disappeared while it was the scope.
    TargetRemoved(String),
}

impl Caption {
    pub fn text(&self) -> String {
        match self {
            Caption::SaveFailed => "Couldn't save. Your previous colors are back.".into(),
            Caption::TargetRemoved(title) => format!("\"{title}\" was removed. Showing all Corrals."),
        }
    }
    pub fn is_error(&self) -> bool {
        matches!(self, Caption::SaveFailed)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageError {
    /// The scope's Corral no longer exists; nothing was written.
    MissingCorral(u64),
    /// The store refused the write; the page is back to its stored state.
    SaveFailed(String),
}

/// How a session ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ended {
    /// No session was open.
    Nothing,
    /// Esc, or a close without an explicit choice: stored data untouched, overlay cleared.
    Discarded,
    Commit(Commit),
}

pub struct ColorPage {
    globals: Globals,
    corrals: Vec<CorralInfo>,
    scope: Scope,
    session: Option<Session>,
    next_session: u64,
    caption: Option<Caption>,
}

impl ColorPage {
    /// `first_session` seeds the session ids (the engine drops an overlay whose id differs).
    pub fn new(globals: Globals, corrals: Vec<CorralInfo>, scope: Scope, first_session: u64) -> Self {
        let mut page = ColorPage { globals, corrals, scope: Scope::All, session: None, next_session: first_session.max(1), caption: None };
        if let Scope::Corral(id) = scope {
            if page.corral(id).is_some() {
                page.scope = scope;
            }
        }
        page
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }
    pub fn globals(&self) -> &Globals {
        &self.globals
    }
    pub fn corrals(&self) -> &[CorralInfo] {
        &self.corrals
    }
    pub fn caption(&self) -> Option<&Caption> {
        self.caption.as_ref()
    }
    pub fn clear_caption(&mut self) {
        self.caption = None;
    }
    pub fn session(&self) -> Option<&Session> {
        self.session.as_ref()
    }
    pub fn is_editing(&self) -> bool {
        self.session.is_some()
    }

    fn corral(&self, id: u64) -> Option<&CorralInfo> {
        self.corrals.iter().find(|c| c.id == id)
    }

    pub fn title(&self) -> String {
        match self.scope {
            Scope::All => "All Corrals".into(),
            Scope::Corral(id) => self.corral(id).map_or_else(|| "All Corrals".into(), |c| c.title.clone()),
        }
    }

    /// Titles of the Corrals with an own look, for the scope caption and the Reset dialog.
    pub fn own_titles(&self) -> Vec<String> {
        self.corrals.iter().filter(|c| super::has_own_look(&c.style)).map(|c| c.title.clone()).collect()
    }

    pub fn scope_has_own_look(&self) -> bool {
        match self.scope {
            Scope::All => false,
            Scope::Corral(id) => self.corral(id).is_some_and(|c| super::has_own_look(&c.style)),
        }
    }

    fn overrides(&self, scope: Scope) -> Overrides {
        match scope {
            Scope::All => Overrides::default(),
            Scope::Corral(id) => self.corral(id).map(|c| Overrides::of(&c.style)).unwrap_or_default(),
        }
    }

    /// Effective values of the scope as stored (no draft).
    pub fn stored_effective(&self) -> Effective {
        resolve(&self.globals, &self.overrides(self.scope))
    }

    /// Effective values of the scope with the open session's draft on top (spec 3.2). Match-Corral
    /// tint and Auto text colour follow because they are derived from the resolved values.
    pub fn effective(&self) -> Effective {
        let mut g = self.globals.clone();
        let mut o = self.overrides(self.scope);
        if let Some(s) = self.session.as_ref().filter(|s| s.scope == self.scope) {
            let v = s.draft;
            match (s.scope, s.field) {
                (Scope::All, Field::Background) => g.color = v,
                (Scope::All, Field::Text) => {
                    g.text_color = v;
                    g.text_auto = false;
                }
                (Scope::All, Field::SeeThrough) => g.see_through = v,
                (Scope::All, Field::TintColor) => g.tint_color = v,
                (Scope::Corral(_), Field::Background) => o.color = Slot::Own(v),
                (Scope::Corral(_), Field::Text) => {
                    o.text_color = Slot::Own(v);
                    o.text_auto = Slot::Own(false);
                }
                (Scope::Corral(_), Field::SeeThrough) => o.see_through = Slot::Own(v),
                (Scope::Corral(_), Field::TintColor) => o.tint_color = Slot::Own(v),
            }
        }
        resolve(&g, &o)
    }

    // ---- sessions and gestures ----------------------------------------------------------------

    /// Open a session for `field` of the current scope. Opening writes nothing; the draft starts
    /// at the effective value and the slot state is remembered (spec 3.3). Any session still open
    /// is ended with close semantics first and returned.
    pub fn begin(&mut self, field: Field) -> (u64, Option<Commit>) {
        let closing = match self.end(false) {
            Ended::Commit(c) => Some(c),
            _ => None,
        };
        let e = self.stored_effective();
        let (value, was_own) = match field {
            Field::Background => (e.color, e.own.color),
            Field::Text => (e.text_color, e.own.text),
            Field::SeeThrough => (e.see_through, e.own.see_through),
            Field::TintColor => (e.tint_color, e.own.tint_color),
        };
        let id = self.next_session;
        self.next_session += 1;
        self.session = Some(Session { id, scope: self.scope, field, was_own, start: value, draft: value, explicit: false });
        (id, closing)
    }

    /// A preview frame (hex typing, an eyedropper hover, restoring the draft after a cancelled
    /// pick): the draft changes, the session does not become explicit. False without a session.
    pub fn preview(&mut self, value: u32) -> bool {
        let Some(s) = self.session.as_mut() else { return false };
        s.draft = value;
        true
    }

    /// A deliberate choice: a slider move that changed the value, a confirmed hex, a preset swatch
    /// or a finished pick. It stays explicit even when the value equals the inherited one, and
    /// when the draft later returns to its start (spec 3.3). False without a session.
    pub fn choose(&mut self, value: u32) -> bool {
        let Some(s) = self.session.as_mut() else { return false };
        s.draft = value;
        s.explicit = true;
        true
    }

    /// A slider move: a choice only when it changes the value, so grabbing a thumb without moving
    /// it writes nothing.
    pub fn slide(&mut self, value: u32) -> bool {
        match self.session.as_ref() {
            None => false,
            Some(s) if s.draft != value => self.choose(value),
            Some(_) => self.preview(value),
        }
    }

    /// The overlay frame to send while a session is open (always the latest draft).
    pub fn frame(&self) -> Option<Frame> {
        let s = self.session.as_ref()?;
        // Opening, or grabbing a thumb without moving it, shows nothing on the desktop.
        if !s.explicit && s.draft == s.start {
            return None;
        }
        let current = self.stored_effective_for(s.scope);
        Some(Frame { session: s.id, scope: s.scope, patch: s.field.edit(s.draft).patch(&current) })
    }

    fn stored_effective_for(&self, scope: Scope) -> Effective {
        resolve(&self.globals, &self.overrides(scope))
    }

    /// End the open session. `cancel` is Esc: discard. Otherwise (close) the draft is committed
    /// when the session had an explicit choice, and nothing is written when it had none.
    pub fn end(&mut self, cancel: bool) -> Ended {
        let Some(s) = self.session.take() else { return Ended::Nothing };
        if cancel || !s.explicit {
            return Ended::Discarded;
        }
        let current = self.stored_effective_for(s.scope);
        let edit = s.field.edit(s.draft);
        Ended::Commit(Commit { session: s.id, scope: s.scope, patch: edit.patch(&current), edits: vec![edit] })
    }

    /// A one-shot explicit edit (preset swatch, Auto, segmented choice, key step). Closes an open
    /// session first; the result lists the commits to send in order.
    pub fn commit_edit(&mut self, edit: ColorEdit) -> Vec<Commit> {
        let mut out = Vec::new();
        if let Ended::Commit(c) = self.end(false) {
            out.push(c);
        }
        let current = self.stored_effective();
        let id = self.next_session;
        self.next_session += 1;
        out.push(Commit { session: id, scope: self.scope, patch: edit.patch(&current), edits: vec![edit] });
        out
    }

    /// Move to another scope. The open session commits to its own target first; the draft never
    /// moves to the new scope. An unknown Corral is ignored.
    pub fn set_scope(&mut self, scope: Scope) -> Option<Commit> {
        if let Scope::Corral(id) = scope {
            if self.corral(id).is_none() {
                return None;
            }
        }
        let closing = match self.end(false) {
            Ended::Commit(c) => Some(c),
            _ => None,
        };
        self.scope = scope;
        closing
    }

    // ---- storing --------------------------------------------------------------------------------

    /// Apply a commit: build the new copy, write it through `write`, then swap it in. A write error
    /// leaves the stored state as it was, clears the draft and sets the error caption (spec 3.4).
    pub fn apply(&mut self, commit: &Commit, write: &mut dyn FnMut(&Commit) -> Result<(), String>) -> Result<(), PageError> {
        let mut globals = self.globals.clone();
        let mut corrals = self.corrals.clone();
        match commit.scope {
            Scope::All => commit.patch.apply_to_globals(&mut globals),
            Scope::Corral(id) => {
                let Some(c) = corrals.iter_mut().find(|c| c.id == id) else {
                    self.session = None;
                    return Err(PageError::MissingCorral(id));
                };
                commit.patch.apply_to_style(&mut c.style);
            }
        }
        if let Err(e) = write(commit) {
            self.session = None;
            self.caption = Some(Caption::SaveFailed);
            return Err(PageError::SaveFailed(e));
        }
        self.globals = globals;
        self.corrals = corrals;
        if self.caption == Some(Caption::SaveFailed) {
            self.caption = None;
        }
        Ok(())
    }

    /// The data on disk changed under the page (settings reload, Backups restore, a Corral
    /// removed, an asynchronous save failure): an open session is cancelled without a commit, the
    /// overlay is cleared, and the scope stays when its Corral still exists, else becomes
    /// All Corrals with a caption (spec 6). Restored data always wins.
    pub fn reload(&mut self, globals: Globals, corrals: Vec<CorralInfo>) -> bool {
        self.session = None;
        let removed = match self.scope {
            Scope::Corral(id) if !corrals.iter().any(|c| c.id == id) => Some(self.title()),
            _ => None,
        };
        self.globals = globals;
        self.corrals = corrals;
        match removed {
            Some(title) => {
                self.scope = Scope::All;
                self.caption = Some(Caption::TargetRemoved(title));
                true
            }
            None => false,
        }
    }

    /// An asynchronous save failed: reload what is stored and show the error caption.
    pub fn save_failed(&mut self, globals: Globals, corrals: Vec<CorralInfo>) {
        self.reload(globals, corrals);
        self.caption = Some(Caption::SaveFailed);
    }
}

// ---------------------------------------------------------------------------------------------
// Live frame throttle (spec 6: 100 drafts at 1 ms spacing)

/// Bounds draft frames to one per interval. The page always keeps the latest draft, so a frame
/// that is skipped is harmless: commit stores the draft, never the last value sent.
#[derive(Clone, Debug)]
pub struct Throttle {
    interval_ms: u64,
    last: Option<u64>,
    pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// Send the latest draft now.
    Send,
    /// Too soon; send the latest draft after this many milliseconds.
    Wait(u64),
}

impl Throttle {
    pub fn new(interval_ms: u64) -> Self {
        Throttle { interval_ms, last: None, pending: false }
    }
    pub fn offer(&mut self, now_ms: u64) -> Step {
        match self.last {
            Some(last) if now_ms.saturating_sub(last) < self.interval_ms => {
                self.pending = true;
                Step::Wait(self.interval_ms - now_ms.saturating_sub(last))
            }
            _ => {
                self.last = Some(now_ms);
                self.pending = false;
                Step::Send
            }
        }
    }
    /// The wait elapsed: true when a deferred frame must be sent now.
    pub fn fire(&mut self, now_ms: u64) -> bool {
        if !self.pending {
            return false;
        }
        self.pending = false;
        self.last = Some(now_ms);
        true
    }
    /// A commit or cancel goes out immediately and ends the deferral.
    pub fn flushed(&mut self, now_ms: u64) {
        self.pending = false;
        self.last = Some(now_ms);
    }
    pub fn pending(&self) -> bool {
        self.pending
    }
}
