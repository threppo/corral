//! Colour state: global and per-Corral colours (colour-state-spec).
//!
//! One explicit model for every colour-like setting that exists both globally and per Corral:
//! a per-Corral field is `Inherit` (key absent) or `Own(value)` (key present, even when equal to
//! the shared value). Resolution, edits, resets and the draft overlay are pure functions over the
//! settings and layout structs; the settings window ([`page`]) and the engine
//! (`app/style_glue.rs`) both call them, so nobody re-implements the fallback (spec 2.2).
//!
//! Colours are COLORREF (`0x00BBGGRR`) throughout, as in `Settings::color`.

use crate::model::{GroupStyle, Layout};
use crate::settings::{Settings, style_request::ColorPatch};

pub mod overlay;
pub mod page;

/// Shared tint colour used until the user picks one (amber).
pub const TINT_COLOR_DEFAULT: u32 = 0x00E4_913F;
/// `TransparencyFactor` is an integer scale, not a percent (spec 2.1; brief: keep the stored scale).
pub const SEE_THROUGH_DEFAULT: u32 = 1380;
/// Largest value any reader accepts on the wire or from a file.
pub const SEE_THROUGH_WIRE_MAX: u32 = 2550;

/// First draft-overlay id of a settings process: the low 16 bits of its process id in the top bits,
/// the clock below. The ids a page hands out count up from it, so the engine and the other
/// settings windows can tell whose draft or answer a message belongs to.
pub fn session_seed(pid: u32, now_ms: u64) -> u64 {
    (u64::from(pid & 0xffff) << 48) | (now_ms & 0x0000_ffff_ffff_ffff)
}
/// The process (low 16 bits of its id) that owns `session`.
pub fn session_owner(session: u64) -> u32 {
    (session >> 48) as u32
}

/// One per-Corral field: follow the shared value, or hold an own value (spec 2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot<T> {
    Inherit,
    Own(T),
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Slot::Inherit
    }
}

impl<T: Copy> Slot<T> {
    pub fn from_option(value: Option<T>) -> Self {
        value.map_or(Slot::Inherit, Slot::Own)
    }
    pub fn own(self) -> Option<T> {
        match self {
            Slot::Inherit => None,
            Slot::Own(v) => Some(v),
        }
    }
    pub fn is_own(self) -> bool {
        matches!(self, Slot::Own(_))
    }
    /// The own value, else the shared one.
    pub fn or(self, shared: T) -> T {
        self.own().unwrap_or(shared)
    }
}

/// Where the tint colour comes from (camouflage-spec 5.2 string keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TintSource {
    /// "corral": the Corral's own effective background colour.
    Corral,
    /// "custom": the stored tint colour.
    Custom,
}

impl TintSource {
    pub fn as_str(self) -> &'static str {
        match self {
            TintSource::Corral => "corral",
            TintSource::Custom => "custom",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "corral" => Some(TintSource::Corral),
            "custom" => Some(TintSource::Custom),
            _ => None,
        }
    }
}

/// The three-way "Tint icons" choice: a view over `tint_icons` plus the source (spec 2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TintMode {
    Off,
    Match,
    Custom,
}

impl TintMode {
    pub fn index(self) -> i64 {
        match self {
            TintMode::Off => 0,
            TintMode::Match => 1,
            TintMode::Custom => 2,
        }
    }
    pub fn from_index(index: i64) -> Option<Self> {
        match index {
            0 => Some(TintMode::Off),
            1 => Some(TintMode::Match),
            2 => Some(TintMode::Custom),
            _ => None,
        }
    }
}

/// The shared (global) values; built-in defaults are the `Settings` defaults (spec 5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Globals {
    pub color: u32,
    pub text_auto: bool,
    /// The custom text colour; kept while `text_auto` is on.
    pub text_color: u32,
    pub see_through: u32,
    pub tint_icons: bool,
    pub tint_source: TintSource,
    pub tint_color: u32,
    pub blend_icons: bool,
    pub blend_background: bool,
}

impl Globals {
    pub fn from_settings(s: &Settings) -> Self {
        Globals {
            color: s.color,
            text_auto: s.text_color_auto,
            text_color: s.text_color,
            see_through: s.transparency_factor,
            tint_icons: s.desktop_tint_icons,
            tint_source: TintSource::parse(&s.desktop_tint_icons_color_source).unwrap_or(TintSource::Corral),
            tint_color: s.desktop_tint_icons_color,
            blend_icons: s.desktop_blend_icons,
            blend_background: s.desktop_blend_corral_backgrounds,
        }
    }
    /// Write every shared colour field back (the other settings are untouched).
    pub fn write_to(&self, s: &mut Settings) {
        s.color = self.color;
        s.text_color_auto = self.text_auto;
        s.text_color = self.text_color;
        s.transparency_factor = self.see_through;
        s.desktop_tint_icons = self.tint_icons;
        s.desktop_tint_icons_color_source = self.tint_source.as_str().into();
        s.desktop_tint_icons_color = self.tint_color;
        s.desktop_blend_icons = self.blend_icons;
        s.desktop_blend_corral_backgrounds = self.blend_background;
    }
}

/// The per-Corral view of a stored [`GroupStyle`] (read only; edits go through [`ColorPatch`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    pub color: Slot<u32>,
    pub text_auto: Slot<bool>,
    pub text_color: Slot<u32>,
    pub see_through: Slot<u32>,
    pub tint_icons: Slot<bool>,
    pub tint_source: Slot<TintSource>,
    pub tint_color: Slot<u32>,
    pub blend_icons: Slot<bool>,
    pub blend_background: Slot<bool>,
}

fn is_white(color: u32) -> bool {
    color & 0x00ff_ffff == 0x00ff_ffff
}

impl Overrides {
    /// Absent key = inherit, present key = own, whatever its value (spec 5). One legacy rule
    /// (spec 5, text colour; same as `GroupStyle::migrate_tint_source`): a stored text colour with
    /// no `text_color_auto` key is that Corral's own Auto when white, an own custom colour otherwise.
    pub fn of(style: &GroupStyle) -> Self {
        let mut text_auto = Slot::from_option(style.text_color_auto);
        let text_color = Slot::from_option(style.text_color);
        if style.text_color_auto.is_none() {
            if let Some(color) = style.text_color {
                text_auto = Slot::Own(is_white(color));
            }
        }
        Overrides {
            color: Slot::from_option(style.color),
            text_auto,
            text_color,
            see_through: Slot::from_option(style.transparency_factor),
            tint_icons: Slot::from_option(style.tint_icons.map(|v| v != 0)),
            tint_source: Slot::from_option(style.tint_icons_color_source.as_deref().and_then(TintSource::parse)),
            tint_color: Slot::from_option(style.tint_icons_color),
            blend_icons: Slot::from_option(style.blend_icons.map(|v| v != 0)),
            blend_background: Slot::from_option(style.blend_background.map(|v| v != 0)),
        }
    }
}

impl Overrides {
    pub fn own(&self) -> Own {
        Own {
            color: self.color.is_own(),
            text: self.text_auto.is_own() || self.text_color.is_own(),
            see_through: self.see_through.is_own(),
            tint: self.tint_icons.is_own() || self.tint_source.is_own(),
            tint_color: self.tint_color.is_own(),
            blend_icons: self.blend_icons.is_own(),
            blend_background: self.blend_background.is_own(),
        }
    }
}

/// Which of the resettable groups (spec 2.1 rows 1-5) hold an own value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Own {
    pub color: bool,
    pub text: bool,
    pub see_through: bool,
    pub tint: bool,
    pub tint_color: bool,
    pub blend_icons: bool,
    pub blend_background: bool,
}

impl Own {
    pub fn any(&self) -> bool {
        self.color || self.text || self.see_through || self.tint || self.tint_color || self.blend_icons || self.blend_background
    }
}

/// Resolved values of one Corral (or of "All Corrals") plus which of them are its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Effective {
    pub color: u32,
    pub text_auto: bool,
    pub text_color: u32,
    pub see_through: u32,
    pub tint_icons: bool,
    pub tint_source: TintSource,
    pub tint_color: u32,
    pub blend_icons: bool,
    pub blend_background: bool,
    pub own: Own,
}

impl Effective {
    pub fn tint_mode(&self) -> TintMode {
        match (self.tint_icons, self.tint_source) {
            (false, _) => TintMode::Off,
            (true, TintSource::Corral) => TintMode::Match,
            (true, TintSource::Custom) => TintMode::Custom,
        }
    }
    /// The colour the icons are tinted with: the Corral's effective background in Match mode
    /// (not the raw shared value, spec 2.2), else the stored tint colour.
    pub fn tint_in_use(&self) -> u32 {
        match self.tint_source {
            TintSource::Corral => self.color,
            TintSource::Custom => self.tint_color,
        }
    }
}

/// The single resolver (spec 2.2): each field is the Corral's own value when it has one, else the
/// shared one.
pub fn resolve(g: &Globals, o: &Overrides) -> Effective {
    Effective {
        color: o.color.or(g.color),
        text_auto: o.text_auto.or(g.text_auto),
        text_color: o.text_color.or(g.text_color),
        see_through: o.see_through.or(g.see_through),
        tint_icons: o.tint_icons.or(g.tint_icons),
        tint_source: o.tint_source.or(g.tint_source),
        tint_color: o.tint_color.or(g.tint_color),
        blend_icons: o.blend_icons.or(g.blend_icons),
        blend_background: o.blend_background.or(g.blend_background),
        own: o.own(),
    }
}

/// Effective values of one stored Corral style under the shared settings.
pub fn effective_of(settings: &Settings, style: &GroupStyle) -> Effective {
    resolve(&Globals::from_settings(settings), &Overrides::of(style))
}

/// Does this Corral hold an own value in any of the five resettable groups? The single source of
/// the "own look" suffix, the scope caption and the Reset dialog (spec 2.2). Font weight,
/// italics and tab style do not count.
pub fn has_own_look(style: &GroupStyle) -> bool {
    Overrides::of(style).own().any()
}

// ---------------------------------------------------------------------------------------------
// Edits

/// One explicit choice of the user (spec 3.3). Turned into a [`ColorPatch`] that sets own values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorEdit {
    Background(u32),
    TextAuto,
    TextCustom(u32),
    SeeThrough(u32),
    /// Writes `tint_icons` and the source together (spec 2.1).
    TintMode(TintMode),
    TintColor(u32),
}

impl ColorEdit {
    /// `current` supplies the source that "Off" keeps next to `tint_icons = false`.
    pub fn patch(&self, current: &Effective) -> ColorPatch {
        let mut p = ColorPatch::default();
        match *self {
            ColorEdit::Background(c) => p.color = Some(c),
            ColorEdit::TextAuto => p.text_color_auto = Some(true),
            ColorEdit::TextCustom(c) => {
                p.text_color = Some(c);
                p.text_color_auto = Some(false);
            }
            ColorEdit::SeeThrough(v) => p.transparency_factor = Some(v),
            ColorEdit::TintMode(mode) => {
                p.tint_icons = Some(mode != TintMode::Off);
                p.tint_icons_color_source = Some(
                    match mode {
                        TintMode::Off => current.tint_source,
                        TintMode::Match => TintSource::Corral,
                        TintMode::Custom => TintSource::Custom,
                    }
                    .as_str()
                    .into(),
                );
            }
            ColorEdit::TintColor(c) => p.tint_icons_color = Some(c),
        }
        p
    }
}

// ---------------------------------------------------------------------------------------------
// Resets (spec 4). Nothing else ever removes a per-Corral colour key.

/// `ResetBase`: shared colour, text colour and mode, see-through and Blend Corral backgrounds go
/// back to the built-in defaults; tint is turned off only when its source is "corral". No Corral
/// is touched.
pub fn reset_base(s: &mut Settings) {
    let defaults = Settings::default();
    if s.desktop_tint_icons_color_source == "corral" {
        s.desktop_tint_icons = false;
    }
    s.color = defaults.color;
    s.text_color = defaults.text_color;
    s.text_color_auto = defaults.text_color_auto;
    s.transparency_factor = defaults.transparency_factor;
    s.desktop_blend_corral_backgrounds = defaults.desktop_blend_corral_backgrounds;
}

/// `ResetGroup`: the Corral loses every own value of the five resettable groups. Font weight,
/// italics, tab style and the other style keys stay.
pub fn reset_group(style: &mut GroupStyle) {
    style.color = None;
    style.text_color = None;
    style.text_color_auto = None;
    style.transparency_factor = None;
    style.tint_icons = None;
    style.tint_icons_color = None;
    style.tint_icons_color_source = None;
    style.tint_icons_color_use = None;
    style.blend_icons = None;
    style.blend_background = None;
}

/// `ResetAll`: `ResetBase`, then every Corral loses its own look.
pub fn reset_all(layout: &mut Layout, s: &mut Settings) {
    reset_base(s);
    for group in &mut layout.groups {
        reset_group(&mut group.style);
    }
}

/// True when any Corral holds an own value (decides whether the Reset dialog offers a choice).
pub fn any_own_look(layout: &Layout) -> bool {
    layout.groups.iter().any(|g| has_own_look(&g.style))
}

// ---------------------------------------------------------------------------------------------
// Two-store writes (spec 4)

/// Write the layout, then the settings. When the settings write fails the layout is written back
/// from its pre-image, so a multi-file edit is never half applied. `settings` is `None` for edits
/// that only touch the layout.
pub fn write_pair(
    prior_layout: &Layout,
    next_layout: &Layout,
    next_settings: Option<&Settings>,
    write_layout: &mut dyn FnMut(&Layout) -> Result<(), String>,
    write_settings: &mut dyn FnMut(&Settings) -> Result<(), String>,
) -> Result<(), String> {
    write_layout(next_layout)?;
    let Some(settings) = next_settings else { return Ok(()) };
    if let Err(e) = write_settings(settings) {
        return match write_layout(prior_layout) {
            Ok(()) => Err(e),
            Err(rollback) => Err(format!("{e}; layout rollback failed: {rollback}")),
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests;
