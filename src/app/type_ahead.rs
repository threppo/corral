//! Pure incremental display-name search (corral-views-spec 2.3).

const RESET_MS: u64 = 500;

/// Whether a WM_CHAR unit under the given modifier state is text rather than an accelerator.
/// AltGr reports as Ctrl + right Alt, so that pair alone still counts as text entry.
pub(super) fn is_text_char(unit: u16, ctrl: bool, lalt: bool, ralt: bool) -> bool {
    if unit < 0x20 || unit == 0x7f { return false; }
    let altgr = ctrl && ralt && !lalt;
    altgr || (!ctrl && !lalt && !ralt)
}

#[derive(Default)]
pub(super) struct TypeAhead {
    buffer: String,
    last_key: Option<u64>,
    high_surrogate: Option<(u16, u64)>,
}

impl TypeAhead {
    pub fn clear(&mut self) { *self = Self::default(); }

    pub fn active(&self, now: u64) -> bool {
        !self.buffer.is_empty() && self.last_key.is_some_and(|last| now.saturating_sub(last) < RESET_MS)
    }

    /// WM_CHAR delivers UTF-16 code units, including surrogate pairs for non-BMP names.
    pub fn input_utf16(&mut self, unit: u16, now: u64, names: &[&str], focus: Option<usize>) -> Option<usize> {
        let high = self.high_surrogate.take();
        if (0xd800..=0xdbff).contains(&unit) {
            self.high_surrogate = Some((unit, now));
            return None;
        }
        let ch = if (0xdc00..=0xdfff).contains(&unit) {
            let (high, _) = high.filter(|(_, time)| now.saturating_sub(*time) < RESET_MS)?;
            char::decode_utf16([high, unit]).next()?.ok()?
        } else { char::from_u32(u32::from(unit))? };
        self.input(ch, now, names, focus)
    }

    fn input(&mut self, ch: char, now: u64, names: &[&str], focus: Option<usize>) -> Option<usize> {
        if ch.is_control() { return None; }
        if !self.active(now) { self.buffer.clear(); }
        if ch == ' ' && self.buffer.is_empty() { return None; }
        let key = ch.to_lowercase().collect::<String>();
        // Keeping a repeated single character as one character makes successive presses cycle.
        let extending = !self.buffer.is_empty() && self.buffer != key;
        if !extending { self.buffer = key; } else { self.buffer.push_str(&key); }
        self.last_key = Some(now);
        let focus = focus.filter(|&i| i < names.len());
        let start = focus.map_or(0, |i| if extending { i } else { (i + 1) % names.len() });
        (0..names.len()).map(|offset| (start + offset) % names.len())
            .find(|&i| names[i].to_lowercase().starts_with(&self.buffer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn altgr_characters_are_text_but_chords_and_controls_are_not() {
        assert!(is_text_char('ł' as u16, true, false, true));
        assert!(is_text_char('a' as u16, false, false, false));
        assert!(!is_text_char('a' as u16, true, false, false));
        assert!(!is_text_char('a' as u16, true, true, false));
        assert!(!is_text_char('a' as u16, false, true, false));
        assert!(!is_text_char(1, true, false, true));
        assert!(!is_text_char(0x7f, false, false, false));
    }

    #[test]
    fn resets_at_500_ms_of_inactivity() {
        let mut search = TypeAhead::default();
        let names = ["ab", "bc"];
        assert_eq!(search.input('a', 0, &names, None), Some(0));
        assert!(search.active(499));
        assert!(!search.active(500));
        assert_eq!(search.input('b', 500, &names, Some(0)), Some(1));
        // The timeout is measured from the most recent character.
        assert_eq!(search.input('c', 999, &names, Some(1)), Some(1));
    }

    #[test]
    fn repeated_letter_cycles_and_wraps() {
        let mut search = TypeAhead::default();
        let names = ["Alpha", "Beta", "Apple", "Atlas"];
        assert_eq!(search.input('a', 0, &names, None), Some(0));
        assert_eq!(search.input('A', 100, &names, Some(0)), Some(2));
        assert_eq!(search.input('a', 200, &names, Some(2)), Some(3));
        assert_eq!(search.input('a', 300, &names, Some(3)), Some(0));
    }

    #[test]
    fn extending_starts_at_focus_then_searches_in_display_order() {
        let mut search = TypeAhead::default();
        let names = ["Apple", "Alpine", "Apricot"];
        assert_eq!(search.input('a', 0, &names, None), Some(0));
        assert_eq!(search.input('p', 100, &names, Some(0)), Some(0));
        assert_eq!(search.input('r', 200, &names, Some(0)), Some(2));
    }

    #[test]
    fn extending_wraps_and_keeps_repeated_letters_in_longer_text() {
        let mut search = TypeAhead::default();
        let names = ["Abbey", "Berry", "Apple"];
        assert_eq!(search.input('a', 0, &names, Some(1)), Some(2));
        assert_eq!(search.input('b', 100, &names, Some(2)), Some(0));
        assert_eq!(search.input('b', 200, &names, Some(0)), Some(0));
        assert_eq!(search.buffer, "abb");
    }

    #[test]
    fn no_match_keeps_the_query_for_the_next_character() {
        let mut search = TypeAhead::default();
        assert_eq!(search.input('z', 0, &["Alpha"], Some(0)), None);
        assert_eq!(search.input('a', 100, &["Zamek"], Some(0)), Some(0));
    }

    #[test]
    fn polish_lowercase_without_diacritic_folding() {
        let mut search = TypeAhead::default();
        let names = ["Lodz", "Łódź", "Łąka"];
        assert_eq!(search.input('ł', 0, &names, None), Some(1));
        assert_eq!(search.input('Ó', 100, &names, Some(1)), Some(1));
        assert_eq!(search.input('l', 600, &names, None), Some(0));
        assert_eq!(search.input('o', 700, &["Łódź"], None), None);
    }

    #[test]
    fn empty_list_and_stale_focus_are_safe() {
        let mut search = TypeAhead::default();
        assert_eq!(search.input('a', 0, &[], Some(7)), None);
        assert_eq!(search.input('a', 100, &["Alpha"], Some(7)), Some(0));
    }

    #[test]
    fn ignores_controls_and_leading_space_but_accepts_spaces_in_names() {
        let mut search = TypeAhead::default();
        let names = ["A file", "Bee"];
        for ch in [' ', '\r', '\t', '\u{8}', '\u{1b}'] {
            assert_eq!(search.input(ch, 0, &names, None), None);
        }
        assert!(!search.active(0));
        assert_eq!(search.input('a', 1, &names, None), Some(0));
        assert_eq!(search.input(' ', 2, &names, Some(0)), Some(0));
        assert_eq!(search.input('f', 3, &names, Some(0)), Some(0));
        assert_eq!(search.input(' ', 503, &names, Some(0)), None);
    }

    #[test]
    fn unicode_lowercase_can_expand_one_character() {
        let mut search = TypeAhead::default();
        assert_eq!(search.input('İ', 0, &["İzmir", "Other", "İstanbul"], None), Some(0));
        assert_eq!(search.input('İ', 1, &["İzmir", "Other", "İstanbul"], Some(0)), Some(2));
    }

    #[test]
    fn decodes_wm_char_surrogates_and_rejects_incomplete_pairs() {
        let mut search = TypeAhead::default();
        let names = ["𐐨 name"];
        let units: Vec<u16> = "𐐀".encode_utf16().collect();
        assert_eq!(search.input_utf16(units[0], 0, &names, None), None);
        assert_eq!(search.input_utf16(units[1], 1, &names, None), Some(0));
        search.clear();
        assert_eq!(search.input_utf16(units[1], 2, &names, None), None);
        assert_eq!(search.input_utf16(units[0], 3, &names, None), None);
        assert_eq!(search.input_utf16(units[1], 503, &names, None), None);
        assert!(!search.active(503));
    }
}
