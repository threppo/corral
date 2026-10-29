//! Shared Camouflage settings mutations. U3 draws all sliders with the Corral kit.
use crate::settings::{Field, Settings};

pub enum Change {
    Field(Field, i64),
    Blur(i32),
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

pub const BLUR_RANGE: (i32, i32) = (0, 255);
pub const BLUR_PAGE_STEP: i32 = 10;

/// Shared settings mutation for the inline controls and their tests.
pub fn apply(settings: &mut Settings, change: Change) {
    match change {
        Change::Field(field, value) => settings.set(field, value),
        Change::Blur(radius) => {
            let radius = radius.clamp(BLUR_RANGE.0, BLUR_RANGE.1);
            settings.background_blur_radius = radius as f32;
            settings.background_blur_enabled = radius > 0;
        }
    }
}

pub fn opacity_percent(value: u8) -> i32 {
    (i32::from(value) * 100 + 127) / 255
}
pub fn opacity_value(percent: i32) -> i64 {
    i64::from(percent.clamp(0, 100) * 255 / 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stored_values_and_legacy_percent_mapping_round_trip() {
        let mut s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(radius_position(f32::NAN), 60);
        for radius in [0, 15, 30, 60, 120, 173, 255] {
            apply(&mut s, Change::Blur(radius));
            assert_eq!(effective_radius(&s), radius);
            assert_eq!(s.background_blur_enabled, radius > 0);
            let restored: Settings =
                serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
            assert_eq!(restored, s);
        }
        for (p, v) in [
            (0, 0),
            (50, 127),
            (60, 153),
            (70, 178),
            (80, 204),
            (90, 229),
            (100, 255),
        ] {
            assert_eq!(opacity_value(p), v);
            assert_eq!(opacity_percent(v as u8), p);
        }
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
