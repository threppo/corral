//! Shared Camouflage settings mutations. U3 draws all sliders with the Corral kit.
use crate::settings::{Field, Settings};

pub enum Change {
    Field(Field, i64),
}

pub fn effective_radius(settings: &Settings) -> i32 {
    if settings.background_blur_enabled {
        radius_position(settings.background_blur_radius)
    } else {
        0
    }
}
pub fn radius_position(radius: f32) -> i32 {
    if radius.is_finite() {
        radius.round().clamp(0.0, 255.0) as i32
    } else {
        60
    }
}

/// Shared settings mutation for the inline controls and their tests.
pub fn apply(settings: &mut Settings, change: Change) {
    match change {
        Change::Field(field, value) => settings.set(field, value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn field_changes_use_settings_mutations() {
        let mut s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(radius_position(f32::NAN), 60);
        for f in [
            Field::BlendColorOnMouseover,
            Field::BlendColorOnActive,
            Field::BlendUsesTintColor,
        ] {
            apply(&mut s, Change::Field(f, 1));
            assert!(s.is_on(f));
        }
    }
}
