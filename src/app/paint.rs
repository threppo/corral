//! Painting and layout of the icons: cell/label styles, surface and group rendering, window sync,
//! loose-icon arrangement, scrolling.

use crate::camouflage::runtime as camouflage;
use crate::geometry::{Rect, to_dip, to_px};
use crate::group_paint;
use crate::model::{self, GroupContent, RollState};
use crate::render::{Align, RectF, Rgba, Surface, TextStyle, present_layered};
use crate::scrollbar;
use crate::style;
use crate::tint;
use crate::tabs;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::ClientToScreen;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;
use super::{App, Drag, DropSite, GroupGeometry, GroupWindow, Interaction, Item, TitleButton, log, qa_hover};
use super::hosting::{client_size, create_layer};
use super::selection;
use super::metrics::Grid;

impl GroupGeometry {
    /// Body-relative placement also keeps the bar above a moving bottom title.
    fn scrollbar_rect(&self, thin: bool) -> Rect {
        let width = scrollbar::window_width(self.dpi, thin);
        Rect::new(self.body.right - width, self.body.top,
            self.body.right, self.body.bottom)
    }
}

/// Cache key of one member's title: id, title text, a Gate's current folder, weight, italic.
type TitleKey = (u64, String, String, u16, bool);

/// Measured titles of one tab set (corral-painting-spec 7), cached per window.
pub(super) struct TitleMetrics {
    key: Vec<TitleKey>,
    dpi: u32,
    pub ids: Vec<u64>,
    pub titles: Vec<String>,
    pub widths: Vec<i32>,
}

/// The cached metrics of window `id`, rebuilt (with `build`) only when the key or the DPI changed.
fn cached_title_metrics(
    cache: &std::cell::RefCell<std::collections::HashMap<u64, std::rc::Rc<TitleMetrics>>>,
    id: u64, dpi: u32, key: Vec<TitleKey>, ids: Vec<u64>, build: impl FnOnce() -> (Vec<String>, Vec<i32>),
) -> std::rc::Rc<TitleMetrics> {
    if let Some(hit) = cache.borrow().get(&id).filter(|c| c.dpi == dpi && c.key == key) {
        return hit.clone();
    }
    let (titles, widths) = build();
    let entry = std::rc::Rc::new(TitleMetrics { key, dpi, titles, widths, ids });
    cache.borrow_mut().insert(id, entry.clone());
    entry
}

impl App {
    /// Which title buttons a Corral has (corral-painting-spec 2.6): the menu and close buttons exist
    /// while "Titlebar buttons on hover" is on and the desktop is unlocked. A Gate with navigation
    /// allowed always has Up, whatever that setting says (lead decision 2026-10-21: it is navigation,
    /// not a hover control).
    fn title_buttons_of(&self, group: &model::Group) -> group_paint::ButtonSet {
        let on = self.settings.use_titlebar_controls && !self.layout.desktop_locked;
        let gate = self.portals.contains_key(&group.id) && group.view_flags & model::VIEW_FLAG_NO_NAVIGATION == 0;
        group_paint::ButtonSet { menu: on, close: on, up: gate }
    }

    /// The strip's layout (button rectangles, folder glyph, title run) in `strip`'s coordinates.
    pub(super) fn title_layout_for(&self, group: &model::Group, strip: Rect, dpi: u32, text_w: i32, run: bool) -> group_paint::TitleLayout {
        group_paint::layout_title(&group_paint::TitleLayoutInput {
            strip, dpi, want: self.title_buttons_of(group),
            folder: run && self.portals.contains_key(&group.id), text_w,
        })
    }

    /// The titles and measured widths of `group`'s tab set (or of the single Corral), cached per window:
    /// pointer moves and repaints reuse them. The cache key is everything a title or its width depends
    /// on (title text, a Gate's current folder, font weight and italic, DPI), so any change measures again.
    pub(super) fn title_metrics_of(&self, group: &model::Group, dpi: u32) -> std::rc::Rc<TitleMetrics> {
        let members = tabs::members(&self.layout.groups, group);
        let list: Vec<&model::Group> = if members.is_empty() { vec![group] } else { members };
        let italic = |m: &model::Group| m.style.font_italics.is_some_and(|i| i != 0);
        let key: Vec<TitleKey> = list.iter().map(|m| (
            m.id, m.title.clone(), self.portals.get(&m.id).map(|v| v.current.clone()).unwrap_or_default(),
            group_paint::title_weight(m.style.font_weight), italic(m),
        )).collect();
        let ids: Vec<u64> = list.iter().map(|m| m.id).collect();
        cached_title_metrics(&self.title_cache, group.id, dpi, key, ids, || {
            let titles: Vec<String> = list.iter().map(|m| self.portal_title(m)).collect();
            let widths = list.iter().zip(&titles)
                .map(|(m, t)| group_paint::measure_title(t, dpi, group_paint::title_weight(m.style.font_weight), italic(m))).collect();
            (titles, widths)
        })
    }

    /// The visible tab cells of `group`'s tab set in `strip` (window coordinates): tab widths follow the
    /// measured titles (spec 7). Empty for a single Corral, Menu only, or a strip too narrow for a tab.
    pub(super) fn tab_cells_of(&self, group: &model::Group, strip: Rect, dpi: u32) -> Vec<tabs::TabCell> {
        let members = tabs::members(&self.layout.groups, group);
        if members.len() < 2 { return Vec::new() }
        let style = tabs::TabSettings::from(&self.settings).effective_style(group.style.tabs_style);
        let metrics = self.title_metrics_of(group, dpi);
        let shown = members.iter().position(|m| m.id == group.id).unwrap_or(0);
        // While a tab is dragged near an end of a crowded strip, the strip follows the drag (tabs-spec 5.2).
        let active = self.tab_gesture.as_ref().filter(|_| self.tab_press.is_some_and(|p| p.owner == group.id))
            .and_then(|g| g.scroll_anchor).unwrap_or(shown);
        let span = self.title_layout_for(group, strip, dpi, 0, false).span;
        tabs::tab_cells(strip, span, &metrics.widths, active, style, dpi)
    }

    /// Visual state of a title button of the window at `index` (spec 6): Disabled when it cannot act,
    /// Pressed while the mouse is down on it, Hover under the pointer.
    fn title_button_state(&self, index: usize, button: TitleButton, enabled: bool) -> group_paint::ButtonState {
        use group_paint::ButtonState;
        if !enabled { return ButtonState::Disabled }
        let id = self.groups[index].id;
        let hot = self.groups[index].hot_button == Some(button);
        let down = match button {
            TitleButton::Menu => self.control_press == Some((id, false)),
            TitleButton::Close => self.control_press == Some((id, true)),
            TitleButton::Up => self.tab_press.is_some_and(|p| p.owner == id && p.portal_glyph && p.portal_up),
        };
        match (down, hot) {
            (true, true) => ButtonState::Pressed,
            (_, true) => ButtonState::Hover,
            _ => ButtonState::Rest,
        }
    }

    /// Wallpaper sampling uses screen pixels, so moving the preview or a new palette changes
    /// every Corral (camouflage-spec 4).
    pub(super) fn rerender_camouflage(&mut self) {
        let indices: Vec<usize> = self.groups.iter().enumerate().filter_map(|(i, window)| {
            let group = self.layout.groups.iter().find(|g| g.id == window.id)?;
            let fx = self.group_effects(group);
            // Only Corrals whose pixels depend on the wallpaper: Camouflage, blur backing, Auto text.
            let auto = crate::color_state::effective_of(&self.settings, &group.style).text_auto;
            (fx.body > 0.0 || fx.icons > 0.0 || tint::blur_enabled() || auto || self.scrollbars.contains_key(&group.id)).then_some(i)
        }).collect();
        for index in indices { self.render_group(index); }
    }

    /// Items drawn by Corral in a group: members of a Corral, or the items a Gate read from its folder (in
    /// its sort order, corral-views-spec 2.2).
    pub(super) fn group_members(&self, group: &model::Group) -> Vec<&Item> {
        match &group.content {
            GroupContent::Items { items } => items
                .iter()
                .filter_map(|name| self.items.iter().find(|i| i.shell.parsing_name.eq_ignore_ascii_case(name)))
                .collect(),
            GroupContent::Portal { .. } => self.items.iter().filter(|i| i.gate == Some(group.id)).collect(),
        }
    }

    /// What a Gate's body says instead of items (folder-gate-host-spec 3): the error, "Opening folder..." until
    /// the first read, or the empty text; (text, strong). Nothing for a Corral or a Gate with items.
    fn gate_body_lines(&self, id: u64) -> Vec<(String, bool)> {
        let Some(view) = self.portals.get(&id) else { return Vec::new() };
        if let Some((kind, path)) = &view.error {
            let Some((title, detail)) = crate::portal::error_text(*kind) else { return Vec::new() };
            return vec![(title, true), (detail, false), (path.clone(), false)];
        }
        if !view.loaded { return vec![("Opening folder...".to_string(), false)]; }
        if self.items.iter().any(|i| i.gate == Some(id)) { return Vec::new(); }
        vec![("This folder is empty.".to_string(), false)]
    }

    /// The Corral or Gate an item belongs to: a Corral by its stored list, a Gate by the folder it shows.
    pub(super) fn group_of(&self, name: &str) -> Option<u64> {
        self.layout.group_of(name).or_else(|| self.gate_index.get(&name.to_lowercase()).copied())
    }

    /// Rebuild `gate_index` after Gate items changed.
    pub(super) fn index_gate_items(&mut self) {
        self.gate_index = self.items.iter().filter_map(|i| i.gate.map(|g| (i.shell.parsing_name.to_lowercase(), g))).collect();
    }

    /// The shortcut-arrow policy of a group's icons: the global setting, or a Gate's own "Hide shortcut arrows".
    pub(super) fn icon_policy_of(&self, group_id: u64) -> crate::icon_overlay::Policy {
        let gate_hides = self.layout.groups.iter().any(|g| g.id == group_id && g.hide_link_overlays && matches!(g.content, GroupContent::Portal { .. }));
        use crate::icon_overlay::{DOCUMENT, ENABLED, FOLDER, PROGRAM, Policy, WEB};
        Policy(if gate_hides { ENABLED | WEB | PROGRAM | FOLDER | DOCUMENT } else { self.settings.hide_shortcut_overlays })
    }

    /// Parent-client position of the desktop list view's client origin.
    pub(super) fn listview_origin(&self) -> (i32, i32) {
        let Some(host) = self.host else { return (0, 0) };
        let mut point = POINT::default();
        unsafe {
            let _ = ClientToScreen(host.listview, &mut point);
        }
        let (ox, oy) = self.origin();
        (point.x - ox, point.y - oy)
    }

    /// Place loose items that have no Explorer position (column-major grid, skipping groups).
    pub(super) fn arrange_loose(&mut self) {
        let Some(m) = self.monitors.first().cloned() else { return };
        let (ox, oy) = self.origin();
        let (cw, ch) = self.loose_cell();
        let group_rects: Vec<Rect> = self.layout.groups.iter().filter(|g| self.page_group_visible(g)).map(|g| self.group_client_rect(g)).collect();
        let occupied: Vec<Rect> = self.items.iter().filter(|i| i.placed && self.page_name_visible(&i.shell.parsing_name)).map(|i| Rect::new(i.pos.0, i.pos.1, i.pos.0 + cw, i.pos.1 + ch)).collect();
        let area = m.work.offset(-ox, -oy);
        let blocked: Vec<Rect> = group_rects.into_iter().chain(occupied).collect();
        let mut slot = 0;
        for index in 0..self.items.len() {
            if self.items[index].placed || self.group_of(&self.items[index].shell.parsing_name).is_some() {
                continue;
            }
            let (pos, next) = selection::first_free_grid_cell(&area, (cw, ch), &blocked, slot, 10_000);
            slot = next;
            self.items[index].pos = pos;
            self.items[index].placed = true;
        }
    }

    pub(super) fn refresh_all(&mut self) {
        self.refresh_item_icon_policy();
        if self.apply_sorting() {
            self.save();
        }
        self.sync_group_windows();
        self.render_surface();
        for index in 0..self.groups.len() {
            self.render_group(index);
        }
    }

    pub(super) fn sync_group_windows(&mut self) {
        self.apply_tab_exclusions();
        self.ensure_drop_target(self.surface_hwnd, DropSite::Surface);
        self.sync_portals();
        let wanted: Vec<u64> = self.layout.groups.iter().map(|g| g.id).collect();
        camouflage::prune(&wanted);
        self.scrollbars.retain(|id, _| wanted.contains(id));
        let registrations = &mut self.drop_registrations;
        self.groups.retain(|gw| {
            let keep = wanted.contains(&gw.id);
            if !keep {
                registrations.remove(&(gw.hwnd.0 as isize));
                unsafe {
                    let _ = DestroyWindow(gw.hwnd);
                }
            }
            keep
        });
        for group in self.layout.groups.clone() {
            let rect = self.group_client_rect(&group);
            if let Some(gw) = self.groups.iter_mut().find(|gw| gw.id == group.id) {
                gw.rect = rect;
                let hwnd = gw.hwnd;
                if tabs::body_visible(&self.layout.groups, &group) {
                    self.ensure_drop_target(hwnd, DropSite::Group(group.id));
                } else {
                    self.drop_registrations.remove(&(hwnd.0 as isize));
                    unsafe { let _ = ShowWindow(hwnd, SW_HIDE); }
                }
                continue;
            }
            let hwnd = create_layer(w!("CorralGroup"), self.parent, self.detached(), rect, self.surface_hwnd, self.instance);
            let (Ok(hwnd), Ok(surface)) = (hwnd, Surface::new(rect.width().max(1) as u32, rect.height().max(1) as u32)) else { continue };
            if !self.detached() {
                unsafe {
                    let _ = SetWindowPos(hwnd, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                }
            }
            super::pages_glue::register_touch(hwnd);
            self.groups.push(GroupWindow { id: group.id, hwnd, surface, rect, title: Rect::default(), body: Rect::default(), glyph: Rect::default(), menu_glyph: Rect::default(), close_glyph: Rect::default(), up_glyph: Rect::default(), tab_hover: None, strip_hot: false, hot_button: None });
            if tabs::body_visible(&self.layout.groups, &group) {
                self.ensure_drop_target(hwnd, DropSite::Group(group.id));
            } else {
                unsafe { let _ = ShowWindow(hwnd, SW_HIDE); }
            }
        }
        self.restack();
    }

    fn label_style(&self, dpi: u32) -> TextStyle<'static> {
        TextStyle {
            family: style::FONT,
            size_px: style::LABEL_FONT_PX * dpi as f32 / 96.0,
            weight: 400,
            color: style::LABEL_TEXT,
            align: Align::Center,
            shadow: Some((style::LABEL_SHADOW, 1.0, 1.0)),
        }
    }

    /// Draw one icon cell (icon + label) with its top-left at (x, y).
    pub(super) fn draw_cell(&self, ctx: &crate::render::Ctx, item: &Item, x: f32, y: f32, cell: (i32, i32)) -> windows::core::Result<()> {
        self.draw_cell_fx(ctx, item, x, y, cell, None)
    }

    /// Draw icons through camouflage-spec 3.3; glyph labels retain their text colour.
    pub(super) fn draw_cell_fx(&self, ctx: &crate::render::Ctx, item: &Item, x: f32, y: f32, cell: (i32, i32), fx: Option<&CellFx>) -> windows::core::Result<()> {
        self.draw_cell_raster(ctx, item, x, y, cell, self.dpi(), self.icon_px as f32, item.icon.as_ref(), fx)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_group_cell_fx(&self, ctx: &crate::render::Ctx, item: &Item, x: f32, y: f32, cell: (i32, i32), dpi: u32, group_id: u64, fx: Option<&CellFx>) -> windows::core::Result<()> {
        let bitmap = self.group_icon_bitmap(item, dpi, group_id);
        self.draw_cell_raster(ctx, item, x, y, cell, dpi, self.group_icon_px_of(group_id, dpi) as f32, bitmap.as_deref(), fx)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_cell_raster(&self, ctx: &crate::render::Ctx, item: &Item, x: f32, y: f32, cell: (i32, i32), dpi: u32, icon: f32, bitmap: Option<&crate::shell::IconBitmap>, fx: Option<&CellFx>) -> windows::core::Result<()> {
        let (cw, ch) = (cell.0 as f32, cell.1 as f32);
        if let Some(bitmap) = bitmap {
            let left = x + (cw - icon) / 2.0;
            let top = y + 4.0 * dpi as f32 / 96.0;
            let dest = RectF::new(left, top, left + icon, top + icon);
            match fx.filter(|f| f.effects.icons > 0.0) {
                Some(f) => {
                    let pixels = camouflage::cached_pixels(&bitmap.bgra_premultiplied, f.effects);
                    ctx.draw_bitmap(&pixels, bitmap.width, bitmap.height, dest, 1.0)?;
                }
                None => ctx.draw_bitmap(&bitmap.bgra_premultiplied, bitmap.width, bitmap.height, dest, 1.0)?,
            }
        }
        let label_top = y + icon + 8.0 * dpi as f32 / 96.0;
        let label_rect = RectF::new(x + 2.0, label_top, x + cw - 2.0, y + ch);
        // Explorer shows up to two lines (full text when selected).
        let lines = if item.selected { 6 } else { 2 };
        let mut text_style=self.label_style(dpi);
        // camouflage-spec 3.5: only the Auto choice changes label colour; glyphs are never recoloured.
        if let Some(f)=fx { let c=camouflage::colorref_rgb(f.effects.label);text_style.color=Rgba::new(c[0],c[1],c[2],255); }
        ctx.draw_label(&item.shell.display_name,label_rect,&text_style,lines).map(|_| ())
    }

    /// Resolve per Corral from camouflage-spec 5.3, before any pixel source is drawn.
    pub(super) fn group_effects(&self, group: &model::Group) -> camouflage::Effects {
        let geo=self.group_geometry(group);
        let origin=self.to_screen((geo.union.left,geo.union.top));
        let mut pointer=POINT::default();
        let hovered=unsafe {GetCursorPos(&mut pointer)}.is_ok() && pointer.x>=origin.0 && pointer.x<origin.0+geo.union.width() && pointer.y>=origin.1 && pointer.y<origin.1+geo.union.height();
        camouflage::resolve(&self.settings,group,camouflage::sampling_region(
            [origin.0,origin.1,origin.0+geo.union.width(),origin.1+geo.union.height()],
            &self.monitors.iter().map(|m|[m.bounds.left,m.bounds.top,m.bounds.right,m.bounds.bottom]).collect::<Vec<_>>()),
            hovered,
            self.drag.interaction(group.id)!=Interaction::Rest||self.preview_camouflage_moving,
            self.message_window.0 as isize)
    }

    pub(super) fn sync_desktop_active(&mut self) {
        let now=camouflage::desktop_active();
        if self.color_desktop_active==now {return}
        self.color_desktop_active=now;
        if self.settings.desktop_blend_color_on_active {self.rerender_camouflage();}
    }

    /// The approved spec defines a palette per Corral; loose desktop icons keep their own colours.
    pub(super) fn loose_fx(&self) -> Option<CellFx> { None }

    pub(super) fn render_surface(&mut self) {
        if self.pages.moving() && !self.pages.snapshotting { return; }
        let (w, h) = client_size(self.parent);
        let (sx, sy) = self.window_xy((0, 0));
        unsafe {
            let _ = SetWindowPos(self.surface_hwnd, HWND::default(), sx, sy, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        let _ = self.surface.resize(w.max(1) as u32, h.max(1) as u32);
        let preview = self.preview_host.is_some();
        let band = match self.drag {
            // a band started inside a group is drawn by that group (its points are group-local)
            Drag::Band { start, current } if self.selection.band_group.is_none() => Some(Rect::from_points(start.0, start.1, current.0, current.1)),
            _ => None,
        };
        let origin = self.origin();
        let mut surface = std::mem::replace(&mut self.surface, Surface::new(1, 1).expect("surface"));
        let loose_fx = self.loose_fx();
        let result = surface.draw(|ctx| {
            if preview {
                ctx.fill_linear_gradient(RectF::new(0.0, 0.0, w as f32, h as f32), Rgba::new(28, 64, 110, 255), Rgba::new(10, 22, 40, 255))?;
            } else {
                ctx.clear(Rgba::new(0, 0, 0, 1)); // nearly transparent but hit-testable
            }
            for item in self.items.iter().filter(|i| self.group_of(&i.shell.parsing_name).is_none() && self.page_name_visible(&i.shell.parsing_name)) {
                // quick-hide fade of loose icons (exempt icons stay at 255)
                let qh_alpha = self.qh_loose_alpha(item);
                if qh_alpha == 0 {
                    continue;
                }
                if item.selected || super::item_drag::item_drop_highlight(&item.shell.parsing_name) {
                    let (cw, ch) = self.loose_cell();
                    let r = RectF::new(item.pos.0 as f32 + 1.0, item.pos.1 as f32 + 1.0, (item.pos.0 + cw) as f32 - 1.0, (item.pos.1 + ch) as f32 - 1.0);
                    ctx.fill_rect(r, Self::qh_scaled(style::SELECTION_FILL, qh_alpha))?;
                    ctx.stroke_rounded_rect(r, 0.0, 1.0, Self::qh_scaled(style::SELECTION_BORDER, qh_alpha))?;
                }
                if qh_alpha == 255 {
                    self.draw_cell_fx(ctx, item, item.pos.0 as f32, item.pos.1 as f32, self.loose_cell(), loose_fx.as_ref())?;
                } else {
                    self.draw_cell_alpha(ctx, item, item.pos.0 as f32, item.pos.1 as f32, self.loose_cell(), qh_alpha)?;
                }
            }
            if let Some(r) = band {
                let rect = RectF::new(r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
                ctx.fill_rect(rect, style::BAND_FILL)?;
                ctx.stroke_rounded_rect(rect, 0.0, 1.0, style::BAND_BORDER)?;
            }
            // creation-spec 2.3: the Corral a draw will create, as it will be created (snapped, never the raw
            // pointer rectangle): accent fill at 14 %, a 2 DIP inner accent stroke, a 1 px outer hairline
            if let Some(p) = &self.creation_preview {
                let dpi = self.monitors.iter().find(|m| m.name == p.monitor).map_or(96, |m| m.dpi) as f32;
                let theme = crate::settings_ui::kit::theme::Theme::new(crate::menu_theme::system_uses_light_theme());
                let o = p.rect.outer.offset(-origin.0, -origin.1);
                let radius = if self.settings.square_edges { 0.0 } else { 8.0 * dpi / 96.0 };
                let (l, t, r, b) = (o.left as f32, o.top as f32, o.right as f32, o.bottom as f32);
                let stroke = (2.0 * dpi / 96.0).max(1.0);
                let accent = theme.accent;
                ctx.fill_rounded_rect(RectF::new(l, t, r, b), radius, Rgba { a: 36, ..accent })?;
                ctx.stroke_rounded_rect(RectF::new(l + stroke / 2.0, t + stroke / 2.0, r - stroke / 2.0, b - stroke / 2.0), (radius - stroke / 2.0).max(0.0), stroke, accent)?;
                ctx.stroke_rounded_rect(RectF::new(l - 0.5, t - 0.5, r + 0.5, b + 0.5), radius + 0.5, 1.0, Rgba { a: 153, ..theme.on_accent })?;
            }
            // grid-snapping-spec 4.2: a guide line along each aligned edge while the snap holds
            for g in &self.snap_guides {
                let line = if g.vertical {
                    RectF::new((g.at - origin.0) as f32 - 0.5, (g.from - origin.1) as f32, (g.at - origin.0) as f32 + 0.5, (g.to - origin.1) as f32)
                } else {
                    RectF::new((g.from - origin.0) as f32, (g.at - origin.1) as f32 - 0.5, (g.to - origin.0) as f32, (g.at - origin.1) as f32 + 0.5)
                };
                ctx.fill_rect(line, style::BAND_BORDER)?;
            }
            Ok(())
        });
        if result.is_ok() && !self.pages.snapshotting {
            let _ = present_layered(self.surface_hwnd, &surface, self.window_xy((0, 0)), 255);
        }
        self.surface = surface;
        if !self.pages.snapshotting { self.quick_hide_dump_surface(); }
    }

    /// Re-place overlay scrollbars after their group windows move.
    pub(super) fn layout_portal_popups(&mut self) {
        self.layout_scrollbars();
    }

    /// Publish the interaction surface once, then move windows only. Release/capture
    /// loss restores exact wallpaper sampling; ordinary moves queue no pixel work.
    pub(super) fn drag_render_group(&mut self, index: usize) {
        let Some(group) = self.layout.groups.iter().find(|g| g.id == self.groups[index].id).cloned() else { return };
        if !self.page_group_visible(&group) || self.pages.snapshotting {
            self.render_group(index);
            return;
        }
        let started = std::env::var_os("CORRAL_QA_CAMOUFLAGE_TRACE").map(|_| std::time::Instant::now());
        let geo = self.group_geometry(&group);
        // Publish the wallpaper-independent surface once (Camouflage overlay, portal ghost; a
        // Tint-only or plain Corral just renders normally). Subsequent moves are position only;
        // DWM composites the live wallpaper. A different shape or title side (the strip flips
        // near the monitor top) is not a pure move and rebuilds the surface. Release restores
        // the exact surface.
        if tint::blur_enabled()
            || self.drag_overlay != Some(group.id)
            || !position_only_ok(self.groups[index].rect, geo.union, group.title_side_last, geo.below) {
            self.render_group(index);
            self.drag_overlay = Some(group.id);
            return;
        }
        let (wx, wy) = self.window_xy((geo.union.left, geo.union.top));
        unsafe {
            let _ = SetWindowPos(self.groups[index].hwnd, HWND::default(), wx, wy, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
        self.groups[index].rect = geo.union;
        self.layout_portal_popups();
        if let Some(started) = started {
            log(&format!("camouflage-trace: position+portals group={} elapsed_ms={:.3}", group.id,
                started.elapsed().as_secs_f64()*1000.0));
        }
    }

    /// One render per live size. The palette stays frozen while the resize runs (camouflage-spec 4),
    /// and icons come from the cache, so a step costs the same as an ordinary Corral.
    pub(super) fn drag_render_resize(&mut self, index: usize) {
        self.render_group(index);
    }

    /// Timer tick: render the latest dragged position once (frames in between were dropped).
    pub(super) fn drag_render_tick(&mut self) {
        let Some(id) = self.drag_render_pending.take() else {
            self.drag_render_stop();
            return;
        };
        if let Some(i) = self.groups.iter().position(|g| g.id == id) {
            self.render_group(i);
        }
    }

    /// End of the drag: stop the timer and forget the owed render (the caller renders the final frame).
    pub(super) fn drag_render_stop(&mut self) {
        self.drag_render_pending = None;
        self.drag_overlay = None;
        unsafe { let _ = KillTimer(self.message_window, super::TIMER_DRAG_RENDER); }
    }

    /// Paint one group (default style, no blur yet): rounded
    /// union filled with Color at body alpha, title strip overlaid with the darken alpha, title text
    /// centred with end ellipsis, member icons in the icon-area grid.
    pub(super) fn render_group(&mut self, index: usize) {
        let trace = std::env::var_os("CORRAL_QA_CAMOUFLAGE_TRACE").is_some()
            && self.groups.get(index).and_then(|w| self.layout.groups.iter().find(|g| g.id == w.id))
                .is_some_and(|g| { let fx = self.group_effects(g); fx.body > 0.0 || fx.icons > 0.0 });
        if trace {
            let start = std::time::Instant::now();
            self.render_group_inner(index);
            log(&format!("camouflage-trace: render+present index={index} interactive={} elapsed_ms={:.3}",
                self.groups.get(index).is_some_and(|g| self.drag.surface_interactive(g.id)) || self.preview_camouflage_moving, start.elapsed().as_secs_f64()*1000.0));
            return;
        }
        // QA: CORRAL_QA_PAINT_TIMING=1 logs the duration of every group render (drag smoothness work).
        if std::env::var_os("CORRAL_QA_PAINT_TIMING").is_some() {
            let start = std::time::Instant::now();
            self.render_group_inner(index);
            log(&format!("paint timing: group index {index} {:.1} ms", start.elapsed().as_secs_f64() * 1000.0));
            return;
        }
        self.render_group_inner(index);
    }

    fn render_group_inner(&mut self, index: usize) {
        self.retain_group_icon_rasters();
        if std::env::var_os("CORRAL_QA_COLOR_EDITOR").is_some() {
            log(&format!("color editor: render group={}", self.groups[index].id));
        }
        let Some(group) = self.layout.groups.iter().find(|g| g.id == self.groups[index].id).cloned() else { return };
        if !tabs::body_visible(&self.layout.groups, &group) || !self.page_group_visible(&group) {
            if !tabs::body_visible(&self.layout.groups, &group) {
                // Discard old visible pixels too: page animation composites stored surfaces.
                let _ = self.groups[index].surface.draw(|ctx| { ctx.clear(Rgba::TRANSPARENT); Ok(()) });
                self.groups[index].rect = self.group_geometry(&group).union;
                self.groups[index].tab_hover = None;
            }
            if !self.pages.snapshotting {
                unsafe { let _ = ShowWindow(self.groups[index].hwnd, SW_HIDE); }
                if let Some(bar) = self.scrollbars.get(&group.id) { bar.hide(); }
            }
            return;
        }
        let geo = self.group_geometry(&group);
        // every layout stores the title side it used (Groups field 18)
        if group.title_side_last != geo.below as i32 {
            if let Some(g) = self.layout.group_mut(group.id) {
                g.title_side_last = geo.below as i32;
            }
            let _ = self.layout.sync_tab_common(group.id);
        }
        let union = geo.union;
        let (w, h) = (union.width().max(1), union.height().max(1));
        let local = |r: Rect| r.offset(-union.left, -union.top);
        let (title, body) = (local(geo.title), local(geo.body));
        let full_body = local(geo.full_body);
        let member_count = self.group_members(&group).len();
        let hwnd = self.groups[index].hwnd;
        let (wx, wy) = self.window_xy((union.left, union.top));
        unsafe {
            let _ = SetWindowPos(hwnd, HWND::default(), wx, wy, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        let mut surface = std::mem::replace(&mut self.groups[index].surface, Surface::new(1, 1).expect("surface"));
        let _ = surface.resize(w as u32, h as u32);
        let colorref = |c: u32| ((c & 0xff) as u8, ((c >> 8) & 0xff) as u8, ((c >> 16) & 0xff) as u8);
        // Colour state, Camouflage and the painter's inputs (corral-painting-spec 3, 4).
        let fx = self.group_effects(&group);
        let look = crate::color_state::effective_of(&self.settings, &group.style);
        let base_color = camouflage::recolor_colorref(look.color, fx);
        // QA hook CORRAL_QA_HOVER=1 pins every group at full hover.
        let hover_level = if qa_hover() { 250.0 } else { self.title_hover.get(&group.id).copied().unwrap_or(0.0) };
        // Eased in over 120 ms (out curve) and out over 200 ms (in curve), spec 2.4; the level is stepped
        // by `step_title_hover`.
        let showing = self.title_hot == Some(group.id) || qa_hover();
        let progress = (hover_level / 250.0).clamp(0.0, 1.0);
        let hover = if showing { crate::motion::ease(crate::motion::Curve::Out, progress) }
            else { 1.0 - crate::motion::ease(crate::motion::Curve::In, 1.0 - progress) };
        // View > Opacity: the group fade scales every alpha the painter draws (spec 3.5).
        let is_portal = self.portals.contains_key(&group.id);
        let fade = model::group_fade(group.opacity, hover, self.transparency_disabled(), is_portal);
        let draw_title = title.height() > 0;
        let folded = geo.rolled && draw_title;
        // A Corral that is not fully open always shows its strip, so it can be unfolded (spec 2.5).
        let reveal = group_paint::reveal_for(self.settings.label_mode, group.roll != RollState::Expanded, hover);
        let title_metrics = self.title_metrics_of(&group, geo.dpi);
        let me = title_metrics.ids.iter().position(|id| *id == group.id).unwrap_or(0);
        let title_text = title_metrics.titles.get(me).cloned().unwrap_or_else(|| group.title.clone());
        let weight = group_paint::title_weight(group.style.font_weight);
        let italic = group.style.font_italics.is_some_and(|i| i != 0);
        let tab_settings = tabs::TabSettings::from(&self.settings);
        let tab_style = tab_settings.effective_style(group.style.tabs_style);
        let tab_members = tabs::members(&self.layout.groups, &group);
        let tabbed = tab_members.len() > 1 && (1..=3).contains(&tab_style);
        let tab_cells = if tabbed && draw_title { self.tab_cells_of(&group, title, geo.dpi) } else { Vec::new() };
        let tabbed = tabbed && !tab_cells.is_empty(); // Narrow-strip fallback: the selected title.
        let hovered_tab = self.groups[index].tab_hover;
        let up_enabled = is_portal && self.portals.get(&group.id)
            .is_some_and(|view| view.core.try_borrow().is_ok_and(|core| core.nav.can_up()));
        let text_w = if tabbed || !draw_title { 0 } else { title_metrics.widths.get(me).copied().unwrap_or(0) };
        let layout = if draw_title { self.title_layout_for(&group, title, geo.dpi, text_w, !tabbed) } else { group_paint::TitleLayout::default() };
        let (menu_state, up_state, close_state) = (
            self.title_button_state(index, TitleButton::Menu, true),
            self.title_button_state(index, TitleButton::Up, up_enabled),
            self.title_button_state(index, TitleButton::Close, true),
        );
        let colors_on = self.settings.tabs_always_show_colors || self.settings.tabs_colored_per_tab_in_segment_view;
        let tab_items: Vec<group_paint::TabItem> = tab_cells.iter().map(|cell| {
            let member = tab_members[cell.index as usize];
            let member_look = crate::color_state::effective_of(&self.settings, &member.style);
            let own = colors_on.then(|| group_paint::colorref_rgb(camouflage::recolor_colorref(member_look.color, self.group_effects(member))));
            group_paint::TabItem {
                title: title_metrics.titles.get(cell.index as usize).cloned().unwrap_or_default(), hit: cell.hit, active: member.id == group.id,
                hovered: hovered_tab == Some(cell.index), own_color: own,
                custom_text: (!member_look.text_auto).then(|| group_paint::colorref_rgb(member_look.text_color)),
                weight: group_paint::title_weight(member.style.font_weight),
                italic: member.style.font_italics.is_some_and(|i| i != 0),
            }
        }).collect();
        let high_contrast = group_paint::high_contrast();
        let paint = group_paint::PaintInputs {
            outer: Rect::new(0, 0, w, h),
            strip: draw_title.then_some(title),
            strip_above: !geo.below,
            folded,
            dpi: geo.dpi,
            square_edges: self.settings.square_edges,
            body_rgb: group_paint::colorref_rgb(base_color),
            fill_alpha: group_paint::see_through_alpha(look.see_through as i64),
            shade: self.settings.darken_titlebars,
            outline: self.settings.outline,
            fade: fade as f64 / 255.0,
            reveal,
            strip_hover: if self.groups[index].strip_hot { hover } else { 0.0 },
            text: if look.text_auto { group_paint::TextMode::Auto } else { group_paint::TextMode::Custom(group_paint::colorref_rgb(fx.text)) },
            backdrop_mean: fx.mean,
            title: &title_text,
            weight,
            italic,
            layout,
            menu: menu_state,
            up: up_state,
            close: close_state,
            buttons: hover,
            tabs: tabbed.then(|| group_paint::TabsPaint { style: tab_style, tabs: &tab_items }),
            high_contrast,
        };

        // One grid for painting, hit testing, drops and scrolling (grid-snapping-spec 2.1); its content
        // height feeds the scrollbar model (scrollbar-spec 4: fractional runtime offset).
        let grid = {
            let flat = self.group_grid(group.id, geo.dpi, full_body, 0);
            let content = flat.content_px(member_count);
            let scroll_px = self.scrollbars.get(&group.id).map_or(0, |b|
                b.offset_px(geo.full_body.height(), content, flat.m.cell_h_px, geo.dpi));
            Grid { scroll_px: scroll_px.max(0), ..flat }
        };
        let scroll_px = grid.scroll_px;
        // Hit rectangles of the title buttons (relative to the window); the cluster only takes
        // the pointer while it is shown on hover.
        let shown = draw_title && hover > 0.0 && fade > 0 && high_contrast.is_none() || draw_title && high_contrast.is_some();
        let menu_rect = layout.menu.filter(|_| shown);
        let close_rect = layout.close.filter(|_| shown);
        // Up takes the pointer whenever the strip is shown (it is not a hover control).
        let up_rect = layout.up.filter(|_| draw_title && up_enabled && reveal > 0.0);
        // Camouflage (camouflage-spec 3.5): body, strip and scrollbar colours were mapped above; icons
        // go through the palette one by one in `draw_cell_fx`; text keeps its colour.
        let origin = self.to_screen((union.left, union.top));
        let cell_fx = Some(CellFx { effects:fx });
        let mut result = surface.draw(|ctx| {
            ctx.clear(Rgba::TRANSPARENT);
            group_paint::paint_fills(&mut group_paint::CtxSink(ctx), &paint)
        });
        if result.is_ok() && tint::blur_enabled() && high_contrast.is_none() {
            // wallpaper-blur-spec 8: the backdrop is the blurred wallpaper picture Windows reports,
            // never a copy of the composed desktop with its icons and windows.
            if let Ok(mut mask) = Surface::new(w as u32, h as u32) {
                let shape = RectF::new(0.0, 0.0, w as f32, h as f32);
                let radius = paint.radius();
                let drawn = mask.draw(|ctx| {
                    ctx.clear(Rgba::TRANSPARENT);
                    ctx.fill_corner_shape(shape, [radius; 4], Rgba::new(255, 255, 255, 255))
                });
                if drawn.is_ok() {
                    tint::composite_blurred_backdrop(surface.pixels_mut(), mask.pixels(), w as usize, h as usize, origin, fade, 1.0);
                }
            }
        }
        if result.is_ok() {
            // Icons between the fills and the chrome: the outline, glyphs and text never lie under them.
            result = surface.draw(|ctx| {
            if !geo.rolled {
                let clip = RectF::new(body.left as f32, body.top as f32, body.right as f32, body.bottom as f32);
                let members = self.group_members(&group);
                ctx.with_clip(clip, |ctx| {
                    for (slot, item) in members.iter().enumerate() {
                        let slot_rect = grid.slot_rect(slot);
                        let (x, y, cell) = (slot_rect.left, slot_rect.top, (slot_rect.width(), slot_rect.height()));
                        if y + cell.1 <= body.top || y >= body.bottom {
                            continue;
                        }
                        if item.selected || super::item_drag::item_drop_highlight(&item.shell.parsing_name) {
                            let r = RectF::new(x as f32 + 1.0, y as f32 + 1.0, (x + cell.0) as f32 - 1.0, (y + cell.1) as f32 - 1.0);
                            ctx.fill_rect(r, style::SELECTION_FILL)?;
                            ctx.stroke_rounded_rect(r, 0.0, 1.0, style::SELECTION_BORDER)?;
                        }
                        self.draw_group_cell_fx(ctx, item, x as f32, y as f32, cell, geo.dpi, group.id, cell_fx.as_ref())?;
                    }
                    // A Gate's empty, loading or error text, centred in the body in the label colour.
                    let lines = self.gate_body_lines(group.id);
                    let line_h = 22.0 * geo.dpi as f32 / 96.0;
                    let buttons = self.portals.get(&group.id).filter(|v| v.error.is_some())
                        .map(|_| crate::portal::error_buttons(body, geo.dpi));
                    let mut y = buttons.map_or((body.top + body.bottom) as f32 / 2.0 - line_h * lines.len() as f32 / 2.0,
                        |b| b[0].top as f32 - 12.0 * geo.dpi as f32 / 96.0 - line_h * lines.len() as f32);
                    let (r, g, b) = colorref(fx.label);
                    for (text, strong) in &lines {
                        let style = crate::render::TextStyle { family: style::FONT, size_px: (if *strong { 14.0 } else { 12.0 }) * geo.dpi as f32 / 96.0,
                            weight: if *strong { 600 } else { 400 }, color: Rgba::new(r, g, b, 255), align: crate::render::Align::Center,
                            shadow: Some((Rgba::new(0, 0, 0, 140), 1.0, 1.0)) };
                        let inset = 12.0 * geo.dpi as f32 / 96.0;
                        ctx.draw_text(text, RectF::new(body.left as f32 + inset, y, body.right as f32 - inset, y + line_h), &style)?;
                        y += line_h;
                    }
                    if let Some(buttons) = buttons {
                        let scale = geo.dpi as f32 / 96.0;
                        let style = crate::render::TextStyle { family: style::FONT, size_px: 12.0 * scale,
                            weight: 400, color: Rgba::new(r, g, b, 255), align: crate::render::Align::Center,
                            shadow: Some((Rgba::new(0, 0, 0, 140), 1.0, 1.0)) };
                        for (button, text) in buttons.into_iter().zip(["Try again", "Choose a different folder..."]) {
                            let rect = RectF::new(button.left as f32, button.top as f32, button.right as f32, button.bottom as f32);
                            ctx.stroke_rounded_rect(rect, 4.0 * scale, 1.0, style.color)?;
                            ctx.draw_text(text, rect, &style)?;
                        }
                    }
                    if let (Drag::Band { start, current }, Some(band_group)) = (&self.drag, self.selection.band_group) {
                        if band_group == group.id {
                            let r = Rect::from_points(start.0, start.1, current.0, current.1);
                            let rect = RectF::new(r.left as f32, r.top as f32, r.right as f32, r.bottom as f32);
                            ctx.fill_rect(rect, style::BAND_FILL)?;
                            ctx.stroke_rounded_rect(rect, 0.0, 1.0, style::BAND_BORDER)?;
                        }
                    }
                    Ok(())
                })?;
            }
            // Outline after every fill and after Camouflage, then glyphs and title text (spec 3.1).
            group_paint::paint_chrome(&mut group_paint::CtxSink(ctx), &paint)
            });
        }
        if result.is_ok() && draw_title {
            // An armed drop zone: accent outline, plate and insertion mark (tabs-spec 3.2).
            if let Some((_, insertion)) = self.tab_overlay.filter(|(target, _)| *target == group.id) {
                let mark = insertion.and_then(|i| tabs::insertion_mark(&tab_cells, i, tab_members.len()));
                result = surface.draw(|ctx| draw_join_overlay(ctx, title, paint.radius(), mark, geo.dpi, high_contrast));
            }
        }
        if result.is_ok() && !self.pages.snapshotting && self.page_layer_visible(&group) {
            // quick-hide fade (non-exempt groups); a fully faded group window stays hidden
            let mut qh_alpha = self.qh_group_alpha(group.id);
            // The Corral dragged onto an armed drop zone shows at 70 % (tabs-spec 3.2).
            if self.tab_fade == Some(group.id) { qh_alpha = (u32::from(qh_alpha) * 70 / 100) as u8; }
            if qh_alpha > 0 {
                let _ = present_layered(hwnd, &surface, self.window_xy((union.left, union.top)), qh_alpha);
                if !unsafe { IsWindowVisible(hwnd) }.as_bool() { unsafe { let _ = SetWindowPos(hwnd, None, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW); } }
            } else { unsafe { let _ = ShowWindow(hwnd, SW_HIDE); } }
            // QA hook: CORRAL_QA_DUMP_DIR=<dir> writes each rendered group surface (no screen capture)
            if let (Ok(dir), true) = (std::env::var("CORRAL_QA_DUMP_DIR"), qh_alpha > 0) {
                let _ = surface.save_png(&std::path::Path::new(&dir).join(format!("group-{}.png", group.id)));
            }
        }
        let glyph = layout.folder.filter(|_| is_portal).unwrap_or_default();
        if glyph != self.groups[index].glyph && glyph.width() > 0 {
            // QA aid: where the folder glyph is on screen (posted-click trials)
            let (ox, oy) = self.origin();
            let (x, y) = (glyph.left + union.left + ox, glyph.top + union.top + oy);
            log(&format!("portal {}: glyph at screen {x},{y} {}x{}", self.groups[index].id, glyph.width(), glyph.height()));
        }
        let gw = &mut self.groups[index];
        gw.surface = surface;
        gw.rect = union;
        gw.title = title;
        gw.body = body;
        gw.glyph = glyph;
        gw.menu_glyph = menu_rect.unwrap_or_default();
        gw.close_glyph = close_rect.unwrap_or_default();
        gw.up_glyph = up_rect.unwrap_or_default();
        if let Some(g) = self.layout.group_mut(group.id) { g.scroll = to_dip(scroll_px, geo.dpi); }
        if !self.pages.snapshotting { self.sync_scrollbar(&group, &geo, member_count, scroll_px, colorref(look.color), colorref(fx.label), fade, hover); }
    }

    /// scrollbar-spec 2: overlay lane entirely inside the body, with no grid reservation.
    fn bar_rect(&self, geo: &GroupGeometry) -> Rect {
        geo.scrollbar_rect(self.settings.extra_thin_scrollbars)
    }

    /// scrollbar-spec 2/3/5: synchronize the overlay and its independent show/hide state.
    #[allow(clippy::too_many_arguments)]
    fn sync_scrollbar(&mut self, group: &model::Group, geo: &GroupGeometry, count: usize, scroll_px: i32, _track: (u8, u8, u8), thumb: (u8, u8, u8), _fade: u8, _hover: f64) {
        let id = group.id;
        let grid = self.group_grid(id, geo.dpi, geo.full_body, 0);
        let area = geo.full_body.height();
        let content = grid.content_px(count);
        // A Gate scrolls exactly like a Corral (corral-views-spec 2.1): same content, same bar.
        let range = if content as f64 > area as f64 + 0.5 * geo.dpi as f64 / 96.0 {
            Some((scrollbar::PortalRange { max: content, page: area, pos: scroll_px, min: 0 }, scrollbar::BASE_OPACITY))
        } else { None };
        let wanted = tabs::body_visible(&self.layout.groups, group) && self.page_layer_visible(group) && range.is_some() && !geo.rolled;
        let owner = match self.preview_host {
            Some(host) => host,
            None => self.groups.iter().find(|g| g.id == id).map(|g| g.hwnd).unwrap_or(self.surface_hwnd),
        };
        if self.scrollbars.get(&id).is_some_and(|b| !b.alive() || b.owner() != owner) {
            self.scrollbars.remove(&id);
        }
        if !wanted {
            crate::tint::set_scrollbar_user(id, false);
            if let Some(bar) = self.scrollbars.get(&id) {
                bar.hide();
            }
            return;
        }
        if !self.scrollbars.contains_key(&id) {
            match scrollbar::Bar::create(id, owner, scrollbar_notify) {
                Ok(bar) => {
                    self.scrollbars.insert(id, bar);
                }
                Err(e) => {
                    log(&format!("scrollbar {id}: window failed: {e}"));
                    return;
                }
            }
        }
        let Some((range, _base_opacity)) = range else { return };
        let rect = self.bar_rect(geo);
        let (sx, sy) = self.to_screen((rect.left, rect.top));
        let effects = self.group_effects(group);
        let (gx, gy) = self.to_screen((geo.union.left, geo.union.top));
        let parent = [gx, gy, gx + geo.union.width(), gy + geo.union.height()];
        // scrollbar-spec 5: the effective backdrop includes the untreated wallpaper.
        let wallpaper = crate::camouflage::mean_color(&{
            crate::tint::set_scrollbar_user(id, true);
            crate::tint::backdrop_source().sample_rect(parent, 8)
        });
        let body = camouflage::colorref_rgb(camouflage::recolor_colorref(
            group.style.color.unwrap_or(self.settings.color), effects));
        let fill_alpha = group_paint::see_through_alpha(group.style.transparency_factor.unwrap_or(self.settings.transparency_factor) as i64);
        let backdrop = crate::camouflage::composite_backdrop(body, wallpaper, 255 - fill_alpha);
        let params = scrollbar::Params {
            x: sx,
            y: sy,
            w: rect.width(),
            h: rect.height(),
            dpi: geo.dpi,
            thin: self.settings.extra_thin_scrollbars,
            corner: if self.settings.square_edges { 0.0 } else { geo.metrics.radius as f64 * 96.0 / geo.dpi as f64 },
            max: range.max,
            page: range.page,
            thumb,
            backdrop,
            pitch: grid.m.cell_h_px,
            parent,
            input: self.groups.iter().find(|g| g.id == id).map_or(owner, |g| g.hwnd),
            hide_mode: self.settings.scrollbar_mode == 1,
            opacity: self.qh_group_alpha(id),
        };
        let Some(bar) = self.scrollbars.get(&id) else { return };
        let before = bar.rect();
        bar.set_camouflage(effects);
        bar.update(&params);
        if before != (sx, sy, rect.width(), rect.height()) {
            log(&format!("scrollbar {id}: rect screen {sx},{sy} {}x{} content={} page={} pos={} opacity={}", rect.width(), rect.height(), range.max, range.page, range.pos, params.opacity));
        }
    }

    /// Follow a moved parent (preview window) without repainting.
    fn layout_scrollbars(&mut self) {
        for group in self.layout.groups.clone() {
            if let Some(bar) = self.scrollbars.get(&group.id) {
                let rect = self.bar_rect(&self.group_geometry(&group));
                let (sx, sy) = self.to_screen((rect.left, rect.top));
                bar.move_to(sx, sy);
            }
        }
    }

    /// A scroll request from a bar window (wheel, track click, thumb drag).
    pub(super) fn scrollbar_request(&mut self, id: u64, request: scrollbar::Request) {
        let Some(index) = self.groups.iter().position(|g| g.id == id) else { return };
        let scrollbar::Request::Thumb { pos } = request;
        self.scroll_with(index, |_, _, _, max| pos.clamp(0, max));
    }

    /// Wheel deltas and per-group targets, with pass-through at the ends (scrollbar-spec 4).
    pub(super) fn scroll_group(&mut self, index: usize, wheel_delta: i32) -> bool {
        let id = self.groups[index].id;
        self.scrollbars.get(&id).is_some_and(|bar| bar.wheel(wheel_delta))
    }

    /// Apply a scroll operation `(pos, row, page, max_pos) -> new pos` (all px) to a group that has
    /// scrollbar-spec 4: a scrollable, unfolded body.
    fn scroll_with(&mut self, index: usize, op: impl Fn(i32, i32, i32, i32) -> i32) {
        let id = self.groups[index].id;
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id).cloned() else { return };
        let count = self.group_members(&group).len();
        let geo = self.group_geometry(&group);
        let grid = self.group_grid(id, geo.dpi, geo.full_body, 0);
        let row = grid.m.cell_h_px;
        let area = geo.full_body.height();
        let content = grid.content_px(count);
        if content as f64 <= area as f64 + 0.5 * geo.dpi as f64 / 96.0 || geo.rolled {
            return;
        }
        let max = grid.max_scroll_px(count);
        let current = to_px(group.scroll, geo.dpi).clamp(0, max);
        let next = op(current, row, area, max).clamp(0, max);
        log(&format!("scroll id={id} items={count} area={area} content={content} row={row} max={max} current={current} next={next}"));
        if let Some(g) = self.layout.group_mut(id) {g.scroll = to_dip(next, geo.dpi);}
        // scrollbar-spec 4: a fractional offset can cross a device pixel without crossing a whole DIP.
        self.render_group(index);
    }
}

/// Callback of the scrollbar windows: forwards the request to the application state.
fn scrollbar_notify(id: u64, request: scrollbar::Request) {
    super::APP.with(|cell| {
        if let Ok(mut guard) = cell.try_borrow_mut() {
            if let Some(app) = guard.as_mut() {
                app.scrollbar_request(id, request);
            }
        }
    });
}

/// A window-only move keeps the rendered pixels valid while the surface keeps its size and the
/// title side it was drawn with.
pub(super) fn position_only_ok(rendered: Rect, now: Rect, side_last: i32, below: bool) -> bool {
    rendered.width() == now.width() && rendered.height() == now.height() && side_last == below as i32
}

/// camouflage-spec 3.5: icon treatment and independently resolved label colour.
#[derive(Clone, Copy, Debug)]
pub(super) struct CellFx { pub effects:camouflage::Effects }

#[cfg(test)]
mod tests {
    #[test]
    fn title_metrics_are_measured_once_until_something_they_depend_on_changes() {
        use super::*;
        use std::cell::{Cell, RefCell};
        let cache = RefCell::new(std::collections::HashMap::new());
        let calls = Cell::new(0);
        let key = |title: &str, current: &str, weight: u16, italic: bool| -> Vec<TitleKey> {
            vec![(7, title.into(), current.into(), weight, italic)]
        };
        let get = |dpi: u32, key: Vec<TitleKey>| {
            cached_title_metrics(&cache, 7, dpi, key, vec![7], || { calls.set(calls.get() + 1); (vec!["T".into()], vec![40]) })
        };
        get(96, key("Docs", "C:\\a", 600, false));
        get(96, key("Docs", "C:\\a", 600, false));
        get(96, key("Docs", "C:\\a", 600, false));
        assert_eq!(calls.get(), 1, "pointer moves reuse the measurement");
        get(96, key("Renamed", "C:\\a", 600, false));
        assert_eq!(calls.get(), 2, "title changed");
        get(96, key("Renamed", "C:\\a\\b", 600, false));
        assert_eq!(calls.get(), 3, "navigation changed the shown folder");
        get(96, key("Renamed", "C:\\a\\b", 700, false));
        assert_eq!(calls.get(), 4, "weight changed");
        get(96, key("Renamed", "C:\\a\\b", 700, true));
        assert_eq!(calls.get(), 5, "italic changed");
        get(144, key("Renamed", "C:\\a\\b", 700, true));
        assert_eq!(calls.get(), 6, "DPI changed");
        get(144, key("Renamed", "C:\\a\\b", 700, true));
        assert_eq!(calls.get(), 6);
    }

    #[test]
    fn q33n_gate_and_scrollbar_follow_bottom_reveal_through_reversal() {
        use super::*;
        for dpi in [96,120,144,192,240] {
            let metrics=group_paint::metrics(dpi);
            let work=Rect::new(-1920,-1400,0,0);
            let body=Rect::new(-800,-750,-200,0);
            for auto in [false,true] {
                for progress in [0.0,0.24,0.5,1.0,0.72,0.32,0.5,0.0] {
                    let geo=GroupGeometry::edge_docked(body,work,metrics,dpi,progress,crate::rollup::DockEdge::Bottom,auto);
                    let origin=(-1920,1400);
                    let gate=geo.body.offset(origin.0,origin.1);
                    assert_eq!(gate.height(),geo.body.height());
                    assert_eq!(gate.bottom,work.bottom+origin.1);
                    assert!(!geo.below);
                    for thin in [false,true] {
                        let bar=geo.scrollbar_rect(thin);
                        assert_eq!(bar.right,geo.body.right);
                        assert_eq!(bar.bottom,work.bottom);
                        assert_eq!(bar.top,geo.body.top);
                        if !geo.rolled { assert!(bar.top>=geo.title.bottom); }
                    }
                    if progress==1.0 { assert_eq!(geo.body,geo.full_body); }
                    if progress==0.0 {
                        assert!(geo.rolled,"Gate and scrollbar visibility gates hide zero-height body");
                        assert_eq!(geo.title.bottom,work.bottom);
                    }
                }
            }
        }
    }

    #[test]
    fn q33m_gate_and_scrollbar_bounds_stay_in_the_revealed_body() {
        use super::*;
        for dpi in [96,120,144,192,240] {
            let metrics=group_paint::metrics(dpi);
            let body=Rect::new(-800,100,-200,850);
            for auto in [false,true] {
                for progress in [0.0,0.24,0.5,1.0,0.5,0.0] {
                    let geo=GroupGeometry::top_docked(body,-1400,metrics,dpi,progress,auto);
                    let origin=(-1920,1400);
                    let gate=geo.body.offset(origin.0,origin.1);
                    assert_eq!(gate.height(),geo.body.height());
                    assert_eq!(gate.top,if auto { 0 } else { metrics.title_h+metrics.gap });
                    for thin in [false,true] {
                        let bar=geo.scrollbar_rect(thin);
                        assert_eq!(bar.right,geo.body.right);
                        assert_eq!(bar.bottom,geo.body.bottom);
                        assert_eq!(bar.top,geo.body.top);
                        if !geo.rolled {
                            if auto { assert!(bar.bottom<=geo.title.top); }
                            else { assert!(bar.top>=geo.title.bottom); }
                        }
                    }
                    if progress==1.0 { assert_eq!(geo.body,geo.full_body); }
                    if progress==0.0 { assert!(geo.rolled,"Gate and scrollbar visibility gates hide zero-height body"); }
                }
            }
        }
    }



    #[test]
    fn q36b_press_is_rest_until_the_geometry_really_changes_then_release_is_exact() {
        use super::{Drag, Interaction};
        use crate::geometry::Rect;
        let grab = crate::dpi_transition::Grab { px: (0, 0), dpi: 96, size_dip: (400, 300) };
        for resize in [false, true] {
            let state = |moved| if resize {
                Drag::ResizeGroup { id: 42, zone: crate::snap::EdgeZone::RIGHT, pointer: (0, 0), start: Rect::new(0, 0, 400, 300), moved }
            } else { Drag::MoveGroup { id: 42, grab, moved, undock_below: false } };
            let want = if resize { Interaction::Resize } else { Interaction::Move };
            // a click that never moves must not swap in the approximation or freeze a portal
            assert_eq!(state(false).interaction(42), Interaction::Rest, "press only, resize={resize}");
            assert!(!state(false).surface_interactive(42));
            // the first step that changes the model rectangle starts the overlay
            assert_eq!(state(true).interaction(42), want);
            assert!(state(true).surface_interactive(42));
            // another Corral is never affected
            assert_eq!(state(true).interaction(43), Interaction::Rest);
        }
        // Release, WM_CAPTURECHANGED/WM_CANCELMODE and display cancellation all end in
        // `end_group_drag_with_rescue`, which sets Drag::None before the final render.
        for drag in [Drag::None, Drag::Pending { start: (0, 0) }, Drag::Band { start: (0, 0), current: (1, 1) }] {
            assert_eq!(drag.interaction(42), Interaction::Rest);
        }
    }




    #[test]
    fn q36b_window_only_moves_need_the_same_shape_and_title_side() {
        use super::position_only_ok;
        use crate::geometry::Rect;
        let drawn = Rect::new(10, 10, 410, 330);
        assert!(position_only_ok(drawn, Rect::new(500, 80, 900, 400), 0, false));
        assert!(position_only_ok(drawn, Rect::new(500, 80, 900, 400), 1, true));
        assert!(!position_only_ok(drawn, Rect::new(500, 80, 900, 401), 0, false), "size changed");
        assert!(!position_only_ok(drawn, Rect::new(500, 80, 901, 400), 0, false), "size changed");
        assert!(!position_only_ok(drawn, Rect::new(500, 80, 900, 400), 0, true), "title flipped below near the monitor top");
        assert!(!position_only_ok(drawn, Rect::new(500, 80, 900, 400), 1, false), "title flipped above");
    }



}

/// The armed drop zone of a dragged Corral (tabs-spec 3.2): a 2 DIP accent outline around the strip, an
/// accent plate at 18 % inside and, on a set, a 2 DIP insertion mark (tab height inset 4 DIP). High
/// contrast uses the system highlight colour.
fn draw_join_overlay(ctx: &crate::render::Ctx, strip: Rect, radius: f32, mark: Option<i32>, dpi: u32,
    high_contrast: Option<group_paint::HighContrast>) -> windows::core::Result<()> {
    let accent = high_contrast.map_or(crate::settings_ui::Theme::FALLBACK_ACCENT, |h| Rgba::new(h.highlight[0], h.highlight[1], h.highlight[2], 255));
    let line = group_paint::px(2.0, dpi) as f32;
    let rect = RectF::new(strip.left as f32, strip.top as f32, strip.right as f32, strip.bottom as f32);
    ctx.fill_rounded_rect(rect, radius, Rgba::new(accent.r, accent.g, accent.b, 46))?;
    let inner = RectF::new(rect.left + line / 2.0, rect.top + line / 2.0, rect.right - line / 2.0, rect.bottom - line / 2.0);
    ctx.stroke_rounded_rect(inner, (radius - line / 2.0).max(0.0), line, accent)?;
    if let Some(x) = mark {
        let inset = group_paint::px(4.0, dpi) as f32;
        ctx.fill_rect(RectF::new(x as f32 - line / 2.0, rect.top + inset, x as f32 + line / 2.0, rect.bottom - inset), accent)?;
    }
    Ok(())
}
