//! Semantic tokens and type ramp from settings-redesign-spec sections 1-2.
use crate::render::{Align, Rgba, TextStyle};

const fn rgb(hex: u32) -> Rgba {
    Rgba::new((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, 255)
}
const fn alpha(hex: u32, a: u8) -> Rgba {
    Rgba::new((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, a)
}
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub light: bool,
    pub window: Rgba,
    pub surface: Rgba,
    pub surface_raised: Rgba,
    pub control: Rgba,
    pub control_hover: Rgba,
    pub subtle_hover: Rgba,
    pub subtle_selected: Rgba,
    pub border: Rgba,
    pub border_strong: Rgba,
    pub border_subtle: Rgba,
    pub control_strong: Rgba,
    pub control_track: Rgba,
    pub text: Rgba,
    pub secondary: Rgba,
    pub disabled: Rgba,
    pub link: Rgba,
    pub accent: Rgba,
    pub accent_hover: Rgba,
    pub accent_subtle: Rgba,
    pub on_accent: Rgba,
    pub focus: Rgba,
    pub danger: Rgba,
    /// Explicit semantic aliases retained for the window adapter.
    pub text_primary: Rgba,
    pub text_secondary: Rgba,
    pub border_default: Rgba,
    pub text_link: Rgba,
}
impl Theme {
    pub const fn new(light: bool) -> Self {
        if light {
            Self {
                light,
                window: rgb(0xF6F7F8),
                surface: rgb(0xFFFFFF),
                surface_raised: rgb(0xECEEF1),
                control: rgb(0xFFFFFF),
                control_hover: rgb(0xECEEF1),
                subtle_hover: alpha(0, 10),
                subtle_selected: alpha(0, 18),
                border: rgb(0xD7DADF),
                border_strong: rgb(0xB4B9C2),
                border_subtle: rgb(0xECEEF1),
                control_strong: rgb(0x6B7280),
                control_track: rgb(0xD7DADF),
                text: rgb(0x14161A),
                secondary: rgb(0x494E58),
                disabled: rgb(0x8B919C),
                link: rgb(0x8F5B22),
                accent: rgb(0x8F5B22),
                accent_hover: rgb(0xB97A33),
                accent_subtle: alpha(0x8F5B22, 26),
                on_accent: rgb(0xFFFFFF),
                focus: rgb(0x14161A),
                danger: rgb(0xC4382E),
                text_primary: rgb(0x14161A),
                text_secondary: rgb(0x494E58),
                border_default: rgb(0xD7DADF),
                text_link: rgb(0x8F5B22),
            }
        } else {
            Self {
                light,
                window: rgb(0x14161A),
                surface: rgb(0x1B1D22),
                surface_raised: rgb(0x222429),
                control: rgb(0x292C32),
                control_hover: rgb(0x33373F),
                subtle_hover: alpha(0xFFFFFF, 13),
                subtle_selected: alpha(0xFFFFFF, 20),
                border: rgb(0x33373F),
                border_strong: rgb(0x494E58),
                border_subtle: rgb(0x292C32),
                control_strong: rgb(0x8B919C),
                control_track: rgb(0x33373F),
                text: rgb(0xF6F7F8),
                secondary: rgb(0xB4B9C2),
                disabled: rgb(0x6B7280),
                link: rgb(0xEAB477),
                accent: rgb(0xEAB477),
                accent_hover: rgb(0xF3CD9C),
                accent_subtle: alpha(0xEAB477, 36),
                on_accent: rgb(0x14161A),
                focus: rgb(0xFFFFFF),
                danger: rgb(0xF08A80),
                text_primary: rgb(0xF6F7F8),
                text_secondary: rgb(0xB4B9C2),
                border_default: rgb(0x33373F),
                text_link: rgb(0xEAB477),
            }
        }
    }
    pub fn color(&self, color: Rgba, enabled: bool) -> Rgba {
        if enabled {
            color
        } else {
            Rgba {
                a: (color.a as f32 * 0.4).round() as u8,
                ..color
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    PageTitle,
    DialogTitle,
    Section,
    Body,
    BodyStrong,
    Caption,
    Group,
}
impl Type {
    pub fn metrics(self) -> (f32, f32, u16) {
        match self {
            Self::PageTitle => (28., 36., 600),
            Self::DialogTitle => (20., 28., 600),
            Self::Section | Self::BodyStrong => (14., 20., 600),
            Self::Body => (14., 20., 400),
            Self::Caption => (12., 16., 400),
            Self::Group => (12., 16., 600),
        }
    }
    pub fn style(self, dpi: u32, color: Rgba) -> TextStyle<'static> {
        TextStyle {
            family: font_family(),
            size_px: self.metrics().0 * dpi as f32 / 96.,
            weight: self.metrics().2,
            color,
            align: Align::Left,
            shadow: None,
        }
    }
}
pub fn font_family() -> &'static str {
    static FONT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *FONT.get_or_init(|| {
        let root = std::env::var_os("WINDIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "C:\\Windows".into());
        root.join("Fonts/SegUIVar.ttf").is_file()
    }) {
        "Segoe UI Variable"
    } else {
        "Segoe UI"
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn approved_tokens_are_amber_in_both_modes() {
        assert_eq!(Theme::new(true).accent, rgb(0x8F5B22));
        assert_eq!(Theme::new(false).accent, rgb(0xEAB477));
        assert_eq!(Theme::new(false).window, rgb(0x14161A));
        assert_eq!(Theme::new(true).danger, rgb(0xC4382E));
    }
    #[test]
    fn ramp_scales_without_rounding_type_sizes() {
        for dpi in [96, 120, 144, 240] {
            assert_eq!(
                Type::PageTitle.style(dpi, rgb(0)).size_px,
                28. * dpi as f32 / 96.
            );
            assert_eq!(Type::Caption.metrics(), (12., 16., 400));
        }
    }
}
