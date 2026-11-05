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
        assert!(f.keyboard_visible);
    }
    #[test]
    fn u1_fix_mouse_focus_remains_keyboard_active_without_visible_ring() {
        let mut focus = FocusModel {
            focused: Some(2),
            keyboard_visible: true,
        };
        focus.keyboard_visible = false;
        assert!(!focus.keyboard_visible);
        assert_eq!(active_index(focus.focused, 4), Some(2));
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
}
