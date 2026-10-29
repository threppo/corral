//! Pure Folder Gate key routing and pending navigation state.

use crate::portal::GateNav;

pub const VK_BACK: u16 = 0x08;
pub const VK_LEFT: u16 = 0x25;
pub const VK_UP: u16 = 0x26;
pub const VK_RIGHT: u16 = 0x27;

/// A message the Gate offers to `route_key` before ordinary Corral key handling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Key { vk: u16, alt: bool, ctrl: bool, shift: bool },
    /// `WM_APPCOMMAND` browser back (the mouse side button).
    AppBack,
    AppForward,
    /// A wheel is never claimed (Alt+wheel belongs to Range switching); listed so the rule is tested.
    #[cfg_attr(not(test), allow(dead_code))]
    Wheel { alt: bool },
}

/// A navigation the Gate claims from a key (folder-gate-host-spec 2.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    Back,
    Forward,
    Up,
}

/// Corral claims only these keys, before the view sees them, and only with Allow navigation on and
/// no rename edit holding the focus; everything else belongs to ordinary Corral input.
pub fn route_key(input: Input, allow_navigation: bool, renaming: bool) -> Option<Claim> {
    if !allow_navigation || renaming {
        return None;
    }
    match input {
        Input::Key { vk, alt: true, ctrl: false, shift: false } => match vk {
            VK_LEFT => Some(Claim::Back),
            VK_RIGHT => Some(Claim::Forward),
            VK_UP => Some(Claim::Up),
            _ => None,
        },
        Input::Key { vk: VK_BACK, alt: false, ctrl: false, shift: false } => Some(Claim::Back),
        Input::AppBack => Some(Claim::Back),
        Input::AppForward => Some(Claim::Forward),
        _ => None,
    }
}

/// Navigation state kept while an off-thread probe is pending.
#[derive(Clone, Debug)]
pub struct Core {
    pub nav: GateNav,
    target: Option<String>,
}

impl Core {
    pub fn new(nav: GateNav) -> Core { Core { nav, target: None } }

    pub fn target(&self) -> Option<&str> { self.target.as_deref() }

    pub fn begin_own_navigation(&mut self, target: &str) { self.target = Some(target.to_string()); }

    pub fn cancel_own_navigation(&mut self) { self.target = None; }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(vk: u16, alt: bool) -> Input {
        Input::Key { vk, alt, ctrl: false, shift: false }
    }

    #[test]
    fn navigation_keys_are_claimed_only_with_allow_on_and_no_rename() {
        assert_eq!(route_key(key(VK_LEFT, true), true, false), Some(Claim::Back));
        assert_eq!(route_key(key(VK_RIGHT, true), true, false), Some(Claim::Forward));
        assert_eq!(route_key(key(VK_UP, true), true, false), Some(Claim::Up));
        assert_eq!(route_key(key(VK_BACK, false), true, false), Some(Claim::Back));
        assert_eq!(route_key(Input::AppBack, true, false), Some(Claim::Back));
        assert_eq!(route_key(Input::AppForward, true, false), Some(Claim::Forward));
        for input in [key(VK_LEFT, true), key(VK_RIGHT, true), key(VK_UP, true), key(VK_BACK, false), Input::AppBack, Input::AppForward] {
            assert_eq!(route_key(input, false, false), None, "Allow navigation off: {input:?}");
            assert_eq!(route_key(input, true, true), None, "rename edit active: {input:?}");
        }
    }

    #[test]
    fn everything_else_passes_to_the_view() {
        assert_eq!(route_key(Input::Wheel { alt: true }, true, false), None);
        assert_eq!(route_key(Input::Wheel { alt: false }, true, false), None);
        for vk in [0x09u16 /* Tab */, 0x71 /* F2 */, 0x2e /* Delete */, 0x0d /* Enter */, 0x41 /* A */, 0x1b /* Esc */, 0x74 /* F5 */] {
            assert_eq!(route_key(key(vk, false), true, false), None, "{vk:#x}");
            assert_eq!(route_key(key(vk, true), true, false), None, "Alt+{vk:#x}");
        }
        // arrows without Alt, Backspace and Alt+arrows with other modifiers stay the view's
        assert_eq!(route_key(key(VK_LEFT, false), true, false), None);
        assert_eq!(route_key(key(VK_UP, false), true, false), None);
        assert_eq!(route_key(Input::Key { vk: VK_LEFT, alt: true, ctrl: true, shift: false }, true, false), None);
        assert_eq!(route_key(Input::Key { vk: VK_BACK, alt: false, ctrl: false, shift: true }, true, false), None);
        assert_eq!(route_key(Input::Key { vk: VK_BACK, alt: true, ctrl: false, shift: false }, true, false), None);
    }

}
