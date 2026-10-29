//! Popup geometry is computed at the destination monitor's DPI, never the owner's DPI.
use super::{
    focus::{Action, ControlKind, Key, keyboard_action},
    layout::{contains, snap},
};
use crate::render::RectF;
#[derive(Clone, Copy, Debug)]
pub struct PopupGeometry {
    pub rect: RectF,
    pub dpi: u32,
    pub visible_rows: usize,
    pub row_height: f32,
    pub padding: f32,
}
impl PopupGeometry {
    /// anchor/work are screen device pixels. Returns screen device pixels and surface pixel dimensions.
    pub fn new(anchor: RectF, work: RectF, dpi: u32, width_dip: f32, count: usize) -> Self {
        let dpi = dpi.max(1);
        let padding = snap(4., dpi);
        let row_height = snap(34., dpi);
        let width = snap(width_dip, dpi).min((work.right - work.left).max(0.));
        let available = (work.bottom - work.top - 2. * padding).max(0.);
        let visible_rows = count.min((available / row_height).floor() as usize);
        let height = (visible_rows as f32 * row_height + 2. * padding).min(work.bottom - work.top);
        let left = anchor
            .left
            .clamp(work.left, (work.right - width).max(work.left));
        let preferred = if anchor.bottom + height <= work.bottom {
            anchor.bottom
        } else {
            anchor.top - height
        };
        let top = preferred.clamp(work.top, (work.bottom - height).max(work.top));
        Self {
            rect: RectF::new(left, top, left + width, top + height),
            dpi,
            visible_rows,
            row_height,
            padding,
        }
    }
    pub fn surface_size(self) -> (u32, u32) {
        (
            (self.rect.right - self.rect.left).round() as u32,
            (self.rect.bottom - self.rect.top).round() as u32,
        )
    }
    pub fn row(self, visible_index: usize) -> RectF {
        let top = self.padding + visible_index as f32 * self.row_height;
        RectF::new(
            self.padding,
            top,
            self.rect.right - self.rect.left - self.padding,
            top + snap(32., self.dpi),
        )
    }
    pub fn hit(self, x: f32, y: f32) -> Option<usize> {
        (0..self.visible_rows).find(|i| contains(self.row(*i), x, y))
    }
}
#[derive(Clone, Debug, Default)]
pub struct MenuState {
    pub selected: usize,
    pub first_visible: usize,
    pub typeahead: String,
}
impl MenuState {
    pub fn handle(&mut self, key: Key, count: usize, visible: usize) -> Action {
        let action = keyboard_action(ControlKind::Menu, key);
        if count == 0 {
            return action;
        }
        match action {
            Action::Step(d) => {
                self.selected = (self.selected as i64 + d as i64).rem_euclid(count as i64) as usize
            }
            Action::First => self.selected = 0,
            Action::Last => self.selected = count - 1,
            _ => {}
        }
        self.selected = self.selected.min(count - 1);
        if self.selected < self.first_visible {
            self.first_visible = self.selected;
        }
        if self.selected >= self.first_visible + visible.max(1) {
            self.first_visible = self.selected + 1 - visible.max(1);
        }
        action
    }
    /// Caller clears the prefix on its type-ahead timer (typically 1 second).
    pub fn type_char(&mut self, ch: char, labels: &[&str]) -> Option<usize> {
        self.typeahead.extend(ch.to_lowercase());
        let find = |prefix: &str| {
            labels
                .iter()
                .position(|v| v.to_lowercase().starts_with(prefix))
        };
        let found = find(&self.typeahead).or_else(|| {
            self.typeahead = ch.to_lowercase().collect();
            find(&self.typeahead)
        });
        if let Some(i) = found {
            self.selected = i;
        }
        found
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn popup_uses_destination_dpi_and_stays_in_work_area() {
        for dpi in [96, 120, 144, 240] {
            let p = PopupGeometry::new(
                RectF::new(1880., 950., 1920., 980.),
                RectF::new(1000., 0., 2920., 1080.),
                dpi,
                220.,
                15,
            );
            assert!(
                p.rect.left >= 1000.
                    && p.rect.right <= 2920.
                    && p.rect.bottom <= 1080.
                    && p.rect.top >= 0.
            );
            assert_eq!(p.surface_size().0, snap(220., dpi) as u32);
            assert_eq!(p.row_height, snap(34., dpi));
            assert_eq!(p.hit(p.padding + 1., p.padding + 1.), Some(0));
            assert_eq!(p.hit(0., 0.), None);
        }
    }
    #[test]
    fn dpi_change_recreates_surface_geometry() {
        let anchor = RectF::new(50., 50., 100., 80.);
        let work = RectF::new(0., 0., 3840., 2160.);
        let a = PopupGeometry::new(anchor, work, 96, 220., 3);
        let b = PopupGeometry::new(anchor, work, 240, 220., 3);
        assert_eq!(b.surface_size().0, 550);
        assert_ne!(a.surface_size(), b.surface_size());
    }
    #[test]
    fn keyboard_selection_scrolls_with_typeahead() {
        let mut m = MenuState::default();
        m.handle(Key::End, 20, 5);
        assert_eq!((m.selected, m.first_visible), (19, 15));
        m.handle(Key::Home, 20, 5);
        assert_eq!((m.selected, m.first_visible), (0, 0));
        assert_eq!(m.type_char('B', &["Amber", "Backups", "Fold"]), Some(1));
    }
}
