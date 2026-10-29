//! Pure keyboard model. Callers translate returned actions through existing save/reload paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Space,
    Enter,
    Escape,
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
    AltDown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlKind {
    Nav,
    Toggle,
    Checkbox,
    Radio,
    Button,
    Link,
    Dropdown,
    Menu,
    Segmented,
    Slider,
    Swatch,
    Stepper,
    Expander,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Activate,
    Step(i32),
    Page(i32),
    First,
    Last,
    Open,
    Close,
}
pub fn keyboard_action(kind: ControlKind, key: Key) -> Action {
    use Action::*;
    use ControlKind::*;
    use Key::*;
    match (kind, key) {
        (Menu, Escape) => Close,
        (Dropdown, AltDown | Enter | Space) => Open,
        (Toggle | Checkbox | Radio | Button | Link | Swatch | Expander | Menu, Enter | Space) => {
            Activate
        }
        (Nav | Radio | Dropdown | Menu, Up) => Step(-1),
        (Nav | Radio | Dropdown | Menu, Down) => Step(1),
        (Stepper, Up) => Step(1),
        (Stepper, Down) => Step(-1),
        (Segmented | Slider | Swatch, Left) => Step(-1),
        (Segmented | Slider | Swatch, Right) => Step(1),
        (Slider | Stepper, PageUp) => Page(1),
        (Slider | Stepper, PageDown) => Page(-1),
        (Nav | Menu | Slider, Home) => First,
        (Nav | Menu | Slider, End) => Last,
        _ => None,
    }
}
/// Focus eligibility is independent of keyboard-only ring visibility.
pub fn active_index(focused: Option<usize>, count: usize) -> Option<usize> {
    focused.filter(|index| *index < count)
}

#[derive(Default, Clone, Debug)]
pub struct FocusModel {
    pub focused: Option<usize>,
    pub keyboard_visible: bool,
}
impl FocusModel {
    /// Supply one entry for the nav (true), then master and interactive page controls in reading order.
    /// Hidden/disabled controls are false and cannot receive focus.
    pub fn advance(&mut self, enabled: &[bool], reverse: bool) -> Option<usize> {
        self.keyboard_visible = true;
        if enabled.is_empty() {
            self.focused = None;
            return None;
        }
        let len = enabled.len();
        let start = self
            .focused
            .filter(|i| *i < len)
            .unwrap_or(if reverse { 0 } else { len - 1 });
        self.focused = (1..=len)
            .map(|offset| {
                if reverse {
                    (start + len - offset % len) % len
                } else {
                    (start + offset) % len
                }
            })
            .find(|i| enabled[*i]);
        self.focused
    }
    pub fn mouse(&mut self) {
        self.keyboard_visible = false;
    }
    pub fn key(&mut self) {
        self.keyboard_visible = true;
    }
    pub fn reconcile(&mut self, enabled: &[bool]) {
        if !self.focused.is_some_and(|i| enabled.get(i) == Some(&true)) {
            self.focused = None;
        }
    }
}
pub fn switch_page(index: usize, count: usize, delta: i32) -> usize {
    if count == 0 {
        0
    } else {
        (index as i64 + delta as i64).rem_euclid(count as i64) as usize
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tab_skips_inert_and_wraps_in_both_directions() {
        let mut f = FocusModel::default();
        let on = [true, false, true, false, true];
        assert_eq!(f.advance(&on, false), Some(0));
        assert_eq!(f.advance(&on, false), Some(2));
        assert_eq!(f.advance(&on, true), Some(0));
        assert_eq!(f.advance(&on, true), Some(4));
        f.mouse();
        assert!(!f.keyboard_visible);
        f.key();
        assert!(f.keyboard_visible);
    }
    #[test]
    fn u1_fix_mouse_focus_remains_keyboard_active_without_visible_ring() {
        let mut focus = FocusModel {
            focused: Some(2),
            keyboard_visible: true,
        };
        focus.mouse();
        assert!(!focus.keyboard_visible);
        assert_eq!(active_index(focus.focused, 4), Some(2));
        assert_eq!(
            keyboard_action(ControlKind::Toggle, Key::Space),
            Action::Activate
        );
        assert_eq!(active_index(None, 4), None);
        assert_eq!(active_index(Some(4), 4), None);
        assert_eq!(focus.advance(&[true, true, true, true], false), Some(3));
    }

    #[test]
    fn no_focus_when_everything_is_gated() {
        let mut f = FocusModel::default();
        assert_eq!(f.advance(&[false, false], false), None);
        assert_eq!(f.advance(&[], true), None);
    }
    #[test]
    fn dropdown_opens_and_closed_arrow_applies() {
        assert_eq!(
            keyboard_action(ControlKind::Dropdown, Key::Enter),
            Action::Open
        );
        assert_eq!(
            keyboard_action(ControlKind::Dropdown, Key::Down),
            Action::Step(1)
        );
    }
    #[test]
    fn escape_is_not_window_close() {
        assert_eq!(
            keyboard_action(ControlKind::Button, Key::Escape),
            Action::None
        );
        assert_eq!(
            keyboard_action(ControlKind::Menu, Key::Escape),
            Action::Close
        );
    }
    #[test]
    fn pages_cycle_with_control_tab() {
        assert_eq!(switch_page(0, 13, -1), 12);
        assert_eq!(switch_page(12, 13, 1), 0);
    }
}
