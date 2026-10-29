#[path = "../src/dragdrop/feedback.rs"]
mod feedback;

#[test]
fn layered_cursor_decision_table() {
    for layered in [None, Some(0), Some(1), Some(2)] {
        for cursor_set in [false, true] {
            assert_eq!(feedback::uses_arrow(layered, cursor_set),
                layered.is_some_and(|n| n != 0) && cursor_set);
        }
    }
}

#[test]
fn refresh_initial_change_clear_and_new_window_but_not_unchanged_state() {
    let description = Some((1, "Move to %1", "Desktop"));
    assert_eq!(feedback::refresh(None, &description, Some(42)), Some(42));
    assert_eq!(feedback::refresh(Some(&description), &description, Some(42)), None);
    assert_eq!(feedback::refresh(Some(&description), &None, Some(42)), Some(42));
    assert_eq!(feedback::refresh(None, &description, None), None);
    assert_eq!(feedback::refresh(None, &description, Some(0)), None);
    // The production state includes window/effect/layered/text as well as description.
    assert_eq!(feedback::refresh(Some(&(42, 1)), &(43, 1), Some(43)), Some(43));
}
