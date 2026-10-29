//! The engine's draft overlay (colour-state-spec 3.2): the settings window sends draft frames, the
//! engine shows them on top of stored data and keeps a pre-image to put the stored look back.
//! Exactly one overlay exists at a time; it ends on commit, cancel, a stale session id, the end of
//! the settings window, or a dropped channel. It is never persisted.

use super::Globals;
use crate::model::{GroupStyle, Layout};
use crate::settings::{Settings, style_request::ColorPatch};

/// The pre-image of what a draft touches.
#[derive(Clone, Debug)]
pub struct Overlay {
    pub session: u64,
    pub target: Option<u64>,
    before: Globals,
    style: Option<(u64, GroupStyle)>,
    /// Everything the session has shown so far (cumulative; later frames replace earlier values).
    pub draft: ColorPatch,
}

impl Overlay {
    /// Show the draft: the shared look for "All Corrals", one Corral's slots otherwise. A
    /// Corral scope never writes the shared values and vice versa (spec 2.3).
    pub fn apply(&self, layout: &mut Layout, settings: &mut Settings) {
        match self.target {
            None => self.draft.apply_to_settings(settings),
            Some(id) => {
                if let Some(g) = layout.group_mut(id) {
                    self.draft.apply_to_style(&mut g.style);
                }
            }
        }
    }

    /// Put the stored look back for every field the draft touched.
    pub fn restore(&self, layout: &mut Layout, settings: &mut Settings) {
        match (&self.target, &self.style) {
            (None, _) => self.draft.restore_settings(settings, &self.before),
            (Some(_), Some((id, old))) => {
                if let Some(g) = layout.group_mut(*id) {
                    self.draft.restore_style(&mut g.style, old);
                }
            }
            _ => {}
        }
    }

}

#[derive(Debug, PartialEq, Eq)]
pub enum OverlayError {
    MissingCorral,
}

/// Holder with the lifecycle rules of spec 3.2.
#[derive(Default)]
pub struct OverlayHost {
    current: Option<Overlay>,
}

impl OverlayHost {
    pub fn current(&self) -> Option<&Overlay> {
        self.current.as_ref()
    }
    pub fn is_active(&self) -> bool {
        self.current.is_some()
    }

    /// A draft frame. A frame of a different session first puts the older overlay's stored look
    /// back (stale session id), then starts a new one.
    pub fn preview(
        &mut self,
        layout: &mut Layout,
        settings: &mut Settings,
        session: u64,
        target: Option<u64>,
        patch: &ColorPatch,
    ) -> Result<(), OverlayError> {
        if self.current.as_ref().is_some_and(|o| o.session != session || o.target != target) {
            self.end(layout, settings);
        }
        if let Some(id) = target {
            if !layout.groups.iter().any(|g| g.id == id) {
                self.end(layout, settings);
                return Err(OverlayError::MissingCorral);
            }
        }
        if self.current.is_none() {
            let style = target.and_then(|id| layout.groups.iter().find(|g| g.id == id).map(|g| (id, g.style.clone())));
            self.current = Some(Overlay { session, target, before: Globals::from_settings(settings), style, draft: ColorPatch::default() });
        }
        let overlay = self.current.as_mut().expect("overlay was just created");
        overlay.draft.merge(patch);
        overlay.apply(layout, settings);
        Ok(())
    }

    /// Cancel, window close, dropped channel, or a restore that replaced the data: put the stored
    /// look back and forget the overlay. Returns whether one was active.
    pub fn end(&mut self, layout: &mut Layout, settings: &mut Settings) -> bool {
        match self.current.take() {
            Some(o) => {
                o.restore(layout, settings);
                true
            }
            None => false,
        }
    }

    /// A commit was stored: the draft values are now the stored values, so the overlay is simply
    /// forgotten (nothing is restored).
    pub fn settle(&mut self) -> Option<Overlay> {
        self.current.take()
    }

    /// Forget the overlay without touching anything (the data it was shown on was replaced).
    pub fn forget(&mut self) {
        self.current = None;
    }

    /// The settings were reloaded from disk: they already hold the stored look, so only the Corral
    /// part of the draft is taken off the layout, and the overlay ends (colour-state-spec 6). A
    /// session that goes on simply starts a new overlay with its next frame.
    pub fn cancel_for_reload(&mut self, layout: &mut Layout) -> bool {
        match self.current.take() {
            Some(o) => {
                o.restore(layout, &mut Settings::default());
                true
            }
            None => false,
        }
    }

    /// A copy of `layout` and `settings` as they are stored, for saves that must not publish an
    /// unfinished draft.
    pub fn stored_copy(&self, layout: &Layout, settings: &Settings) -> (Layout, Settings) {
        let (mut l, mut s) = (layout.clone(), settings.clone());
        if let Some(o) = &self.current {
            o.restore(&mut l, &mut s);
        }
        (l, s)
    }
}
