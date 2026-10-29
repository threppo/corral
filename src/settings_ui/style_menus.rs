//! Settings command semantics reused by U3 inline appearance controls.
//! Legacy popup UI was removed; persisted fields, command IDs and notices remain compatible.
use crate::settings::Settings;

/// A command a menu item stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    RoundCorners,
    Blur,
    Outline,
    BackgroundColor,
    TextColor,
    Darken(u32),
    ButtonsOnHover,
    TitleBelowNearTop,
}

impl Cmd {
    /// Menu id of the command (stable, non-zero).
    pub fn id(self) -> usize {
        match self {
            Cmd::RoundCorners => 1,
            Cmd::Blur => 2,
            Cmd::Outline => 3,
            Cmd::BackgroundColor => 4,
            Cmd::TextColor => 5,
            Cmd::Darken(n) => 10 + n.min(2) as usize,
            Cmd::ButtonsOnHover => 20,
            Cmd::TitleBelowNearTop => 22,
        }
    }
    pub fn from_id(id: usize) -> Option<Cmd> {
        Some(match id {
            1 => Cmd::RoundCorners,
            2 => Cmd::Blur,
            3 => Cmd::Outline,
            4 => Cmd::BackgroundColor,
            5 => Cmd::TextColor,
            10..=12 => Cmd::Darken((id - 10) as u32),
            20 => Cmd::ButtonsOnHover,
            22 => Cmd::TitleBelowNearTop,
            _ => return None,
        })
    }
}

/// What the window must do after a settings-only command was applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    None,
    /// Titlebar buttons were switched off: say where the Corral options went.
    ButtonsHidden,
    /// Moving the titlebar below was switched off: docked groups keep their side.
    TitleStaysBelow,
}

/// Apply a settings-only command (the colour commands are handled by the window).
/// Returns `None` for the colour commands and for a disabled item.
pub fn apply(s: &mut Settings, cmd: Cmd) -> Option<Notice> {
    match cmd {
        Cmd::RoundCorners => s.square_edges = !s.square_edges,
        Cmd::Blur => s.set(
            crate::settings::Field::BlurSource,
            i64::from(!s.background_blur_enabled),
        ),
        Cmd::Outline => s.outline = !s.outline,
        Cmd::Darken(n) => s.darken_titlebars = n.min(2),
        Cmd::ButtonsOnHover => {
            s.use_titlebar_controls = !s.use_titlebar_controls;
            return Some(if s.use_titlebar_controls {
                Notice::None
            } else {
                Notice::ButtonsHidden
            });
        }
        Cmd::TitleBelowNearTop => {
            s.title_edge_auto_adjust = !s.title_edge_auto_adjust;
            return Some(if s.title_edge_auto_adjust {
                Notice::None
            } else {
                Notice::TitleStaysBelow
            });
        }
        Cmd::BackgroundColor | Cmd::TextColor => return None,
    }
    Some(Notice::None)
}

pub const BUTTONS_HIDDEN_TEXT: &str = "Titlebar buttons are now hidden.\n\nThe options of a Corral are still available by right-clicking its titlebar.";
pub const TITLE_STAYS_BELOW_TEXT: &str = "Done. Titlebars no longer move underneath a Corral when it is placed near the top of the screen.\n\nCorrals already docked at the top keep their titlebar underneath until you move them. Pull such a Corral away from the edge to release it.";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip_through_ids() {
        for cmd in [
            Cmd::RoundCorners,
            Cmd::Blur,
            Cmd::Outline,
            Cmd::BackgroundColor,
            Cmd::TextColor,
            Cmd::Darken(0),
            Cmd::Darken(1),
            Cmd::Darken(2),
            Cmd::ButtonsOnHover,
            Cmd::TitleBelowNearTop,
        ] {
            assert_eq!(Cmd::from_id(cmd.id()), Some(cmd));
        }
        assert_eq!(Cmd::from_id(21), None, "the retired id stays unused");
        assert_eq!(Cmd::from_id(0), None);
        assert_eq!(Cmd::from_id(99), None);
    }

    #[test]
    fn every_toggle_changes_its_stored_field_and_back() {
        let mut s = Settings::default();
        let before = s.clone();
        for cmd in [
            Cmd::RoundCorners,
            Cmd::Blur,
            Cmd::Outline,
            Cmd::TitleBelowNearTop,
            Cmd::ButtonsOnHover,
        ] {
            assert!(apply(&mut s, cmd).is_some());
            assert_ne!(s, before, "{cmd:?} must change a field");
            let mut again = s.clone();
            apply(&mut again, cmd);
            assert_ne!(again, s);
        }
        let mut s = Settings::default();
        assert_eq!(apply(&mut s, Cmd::RoundCorners), Some(Notice::None));
        assert!(s.square_edges);
        apply(&mut s, Cmd::Blur);
        assert!(s.background_blur_enabled);
        apply(&mut s, Cmd::Outline);
        assert!(s.outline);
        apply(&mut s, Cmd::Darken(2));
        assert_eq!(s.darken_titlebars, 2);
        apply(&mut s, Cmd::Darken(0));
        assert_eq!(s.darken_titlebars, 0);
    }

    #[test]
    fn notices_fire_on_switch_off() {
        let mut s = Settings::default();
        assert_eq!(
            apply(&mut s, Cmd::ButtonsOnHover),
            Some(Notice::ButtonsHidden)
        );
        assert!(!s.use_titlebar_controls);
        assert_eq!(apply(&mut s, Cmd::ButtonsOnHover), Some(Notice::None));
        assert_eq!(
            apply(&mut s, Cmd::TitleBelowNearTop),
            Some(Notice::TitleStaysBelow)
        );
        assert_eq!(apply(&mut s, Cmd::TitleBelowNearTop), Some(Notice::None));
    }

    #[test]
    fn colour_commands_are_not_settings_only() {
        let mut s = Settings::default();
        assert_eq!(apply(&mut s, Cmd::BackgroundColor), None);
        assert_eq!(apply(&mut s, Cmd::TextColor), None);
        assert_eq!(s, Settings::default());
    }

    #[test]
    fn new_settings_round_trip_and_old_files_still_load() {
        let mut s = Settings::default();
        s.outline = true;
        s.square_edges = true;
        s.background_blur_enabled = true;
        s.use_titlebar_controls = false;
        s.darken_titlebars = 2;
        s.title_edge_auto_adjust = false;
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        // Settings files without "Outline" use the default.
        let mut value = serde_json::to_value(Settings::default()).unwrap();
        value.as_object_mut().unwrap().remove("Outline");
        let old: Settings = serde_json::from_value(value).unwrap();
        assert!(!old.outline);
        assert_eq!(old, Settings::default());
        let empty: Settings = serde_json::from_str("{}").unwrap();
        assert!(!empty.outline);
    }
}
