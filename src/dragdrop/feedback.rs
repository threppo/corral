//! Pure policy for the Shell layered drag window. No effect negotiation happens here.
pub fn uses_arrow(layered: Option<u32>, cursor_set: bool) -> bool {
    layered.unwrap_or(0) != 0 && cursor_set
}

pub fn refresh<T: PartialEq>(previous: Option<&T>, current: &T, window: Option<u32>) -> Option<u32> {
    window.filter(|w| *w != 0 && previous != Some(current))
}
