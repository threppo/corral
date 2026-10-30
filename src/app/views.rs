//! Commands and input for the Corral-drawn views. Geometry lives in metrics::Grid.
use super::{App, Drag};
use crate::geometry::to_dip;
use crate::model::GroupContent;
use crate::view_state::{self, View, ColumnKey, DetailsColumns};
use super::metrics::HeaderHit;

pub(super) struct HeaderPress {
    id: u64, key: ColumnKey, x: i32, width: i32, resize: bool, left_edge: bool,
    columns: DetailsColumns,
}
/// A new column keeps the menu's direction; clicking the sorted column reverses it.
fn gate_header_sort(old: i32, key: ColumnKey) -> i32 { if old.abs() == key.sort() { -old } else if old < 0 { -key.sort() } else { key.sort() } }
fn header_sort(method: u32, flags: u32, key: ColumnKey) -> (u32, u32) {
    let same = method == key.method() && crate::sorting::is_maintained(method, flags);
    (key.method(), (if same { flags ^ crate::sorting::FLAG_REVERSE } else { flags }) & !crate::sorting::FLAG_NOT_MAINTAINED)
}


impl App {
    pub(super) fn group_columns(&self, id: u64) -> DetailsColumns {
        if let Some(p) = self.header_press.as_ref().filter(|p| p.id == id && p.resize) { return p.columns.clone(); }
        let Some(g) = self.layout.groups.iter().find(|g| g.id == id) else { return DetailsColumns::default() };
        match &g.content {
            GroupContent::Items { .. } => g.columns(),
            GroupContent::Portal { path } => view_state::details_columns(self.portal_current_path(id).unwrap_or(path),
                &self.layout.view_states, self.settings.folder_portals_inherit_view),
        }
    }
    fn store_columns(&mut self, id: u64, columns: Option<&DetailsColumns>) {
        let Some(g) = self.layout.groups.iter().find(|g| g.id == id) else { return };
        match &g.content {
            GroupContent::Items { .. } => {
                let group = self.layout.group_mut(id).unwrap();
                if let Some(columns) = columns { group.set_columns(columns); } else { group.reset_columns(); }
            }
            GroupContent::Portal { path } => {
                let folder = self.portal_current_path(id).unwrap_or(path).to_string();
                for (path, raw) in view_state::unparsed_for_folder(&self.layout.view_states, &folder) {
                    self.layout.view_unparsed.entry(path).or_insert(raw);
                }
                if let Some(columns) = columns { view_state::set_details_columns(&mut self.layout.view_states, &folder, columns); }
                else { view_state::reset_details_columns(&mut self.layout.view_states, &folder); }
            }
        }
        self.save();
        self.refresh_all();
    }
    pub(super) fn column_command(&mut self, id: u64, command: u32) -> bool {
        use crate::portal_menu::id as ids;
        let mut columns = self.group_columns(id);
        match command {
            ids::COLUMN_SIZE => columns.toggle(ColumnKey::Size),
            ids::COLUMN_TYPE => columns.toggle(ColumnKey::Type),
            ids::COLUMN_DATE => columns.toggle(ColumnKey::DateModified),
            ids::COLUMN_RESET => {
                if self.group_view(id) == View::Details { self.store_columns(id, None); }
                return true;
            }
            _ => return false,
        }
        if self.group_view(id) == View::Details { self.store_columns(id, Some(&columns)); }
        true
    }
    pub(super) fn append_columns_menu(&self, menu: windows::Win32::UI::WindowsAndMessaging::HMENU, id: u64) {
        unsafe { super::portals::append_gate_items(menu, &crate::portal_menu::column_menu(&self.group_columns(id))); }
    }
    fn show_columns_menu(&mut self, index: usize, point: (i32, i32)) {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let id = self.groups[index].id;
        let origin = self.to_screen((self.groups[index].rect.left, self.groups[index].rect.top));
        let owner = self.groups[index].hwnd;
        let choice = unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            self.append_columns_menu(menu, id);
            let _ = SetForegroundWindow(owner);
            let choice = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, origin.0 + point.0, origin.1 + point.1, 0, owner, None).0;
            let _ = DestroyMenu(menu);
            choice
        };
        self.column_command(id, choice as u32);
    }
    pub(super) fn group_sort_indicator(&self, id: u64) -> (u32, bool) {
        if self.portals.contains_key(&id) {
            let sort = self.gate_sort_of(id);
            let key = match sort.abs() { 1 => ColumnKey::Name, 2 => ColumnKey::Size, 3 => ColumnKey::Type,
                4 => ColumnKey::DateModified, _ => return (crate::sorting::METHOD_CREATED, sort < 0) };
            return (key.method(), sort < 0);
        }
        self.layout.groups.iter().find(|g| g.id == id).map_or((0, false), |g|
            (if crate::sorting::is_maintained(g.sort_method, g.sort_flags) { g.sort_method } else { 0 }, g.sort_flags & crate::sorting::FLAG_REVERSE != 0))
    }
    fn click_header(&mut self, id: u64, key: ColumnKey) {
        if self.portals.contains_key(&id) {
            let old = self.gate_sort_of(id);
            self.portal_sort(id, gate_header_sort(old, key));
        } else {
            let Some(g) = self.layout.groups.iter().find(|g| g.id == id) else { return };
            let (method, flags) = header_sort(g.sort_method, g.sort_flags, key);
            let toggles = flags ^ g.sort_flags;
            self.set_group_sort(id, (method != g.sort_method).then_some(method), toggles);
        }
    }
    pub(super) fn end_header_press(&mut self, commit: bool) {
        use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
        let Some(p) = self.header_press.take() else { return };
        unsafe { let _ = ReleaseCapture(); }
        if commit && p.resize && p.columns != self.group_columns(p.id) { self.store_columns(p.id, Some(&p.columns)); }
        else { self.rerender_group(p.id); }
    }
    /// Header input consumes presses before item selection, bands or group resizing can see them.
    pub(super) fn header_input(&mut self, index: usize, msg: u32, keys: usize, point: (i32, i32)) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::*;
        use windows::Win32::UI::Input::KeyboardAndMouse::{SetCapture, GetAsyncKeyState, VK_ESCAPE, TRACKMOUSEEVENT, TrackMouseEvent, TME_LEAVE};
        let id = self.groups[index].id;
        let dpi = self.group_dpi(id);
        if self.header_press.as_ref().is_some_and(|p| p.id == id) {
            if matches!(msg, WM_CANCELMODE | WM_CAPTURECHANGED) || (msg == WM_MOUSEMOVE && unsafe { GetAsyncKeyState(VK_ESCAPE.0 as i32) } < 0) {
                self.end_header_press(false); return true;
            }
            if msg == WM_MOUSEMOVE {
                if keys & 1 == 0 { self.end_header_press(false); return true; }
                let p = self.header_press.as_mut().unwrap();
                if p.resize {
                    let delta = crate::geometry::to_dip(point.0 - p.x, dpi);
                    p.columns.resize(p.key, p.width.saturating_add(if p.left_edge { -delta } else { delta }));
                    self.render_group(index);
                }
                return true;
            }
            if msg == WM_LBUTTONUP {
                let p = self.header_press.take().unwrap();
                unsafe { let _ = windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture(); }
                if p.resize {
                    let mut columns = p.columns;
                    let delta = crate::geometry::to_dip(point.0 - p.x, dpi);
                    columns.resize(p.key, p.width.saturating_add(if p.left_edge { -delta } else { delta }));
                    if columns != self.group_columns(id) { self.store_columns(id, Some(&columns)); } else { self.render_group(index); }
                }
                else {
                    let g = self.layout.groups.iter().find(|g| g.id == id).unwrap();
                    let grid = self.group_grid(id, dpi, self.groups[index].body, g.scroll);
                    if grid.header_hit(point) == Some(HeaderHit::Cell(p.key)) { self.click_header(id, p.key); }
                }
                return true;
            }
            if matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_RBUTTONDBLCLK) { return true; }
        }
        let Some(g) = self.layout.groups.iter().find(|g| g.id == id) else { return false };
        let grid = self.group_grid(id, dpi, self.groups[index].body, g.scroll);
        let hit = grid.header_hit(point);
        if msg == WM_MOUSEMOVE || msg == 0x02a3 {
            let hot = if msg == WM_MOUSEMOVE { hit.map(|hit| (id, match hit { HeaderHit::Cell(k) | HeaderHit::Separator(k) => k })) } else { None };
            if hot != self.header_hover { self.header_hover = hot; self.render_group(index); }
            if hot.is_some() {
                let mut track = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: self.groups[index].hwnd, dwHoverTime: 0 };
                unsafe { let _ = TrackMouseEvent(&mut track); }
            }
        }
        if msg == WM_SETCURSOR {
            let mut pointer = windows::Win32::Foundation::POINT::default();
            unsafe { let _ = GetCursorPos(&mut pointer); let _ = windows::Win32::Graphics::Gdi::ScreenToClient(self.groups[index].hwnd, &mut pointer); }
            if self.header_press.as_ref().is_some_and(|p| p.resize) || matches!(grid.header_hit((pointer.x, pointer.y)), Some(HeaderHit::Separator(_))) {
                unsafe { SetCursor(LoadCursorW(None, IDC_SIZEWE).unwrap_or_default()); }
                return true;
            }
        }
        if !grid.header.contains(point.0, point.1) { return false; }
        // An existing gesture still receives moves/releases, but a fresh header press
        // must never reach the body's selection or band-start path.
        if !matches!(self.drag, Drag::None) {
            return matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONDBLCLK);
        }
        let Some(hit) = hit else {
            return matches!(msg, WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_RBUTTONDBLCLK);
        };
        match msg {
            WM_LBUTTONDOWN => {
                let (key, resize) = match hit { HeaderHit::Cell(k) => (k, false), HeaderHit::Separator(k) => (k, true) };
                let columns = self.group_columns(id);
                let width = columns.visible().iter().find(|(k, _)| *k == key).map_or(80, |(_, w)| *w);
                let cell = grid.headers.iter().flatten().find(|c| c.key == key);
                let left_edge = cell.is_some_and(|c| (point.0 - c.rect.left).abs() < (point.0 - c.rect.right).abs());
                unsafe { SetCapture(self.groups[index].hwnd); }
                self.header_press = Some(HeaderPress { id, key, x: point.0, width, resize, left_edge, columns });
                true
            }
            WM_RBUTTONUP => { self.show_columns_menu(index, point); true }
            WM_RBUTTONDOWN | WM_RBUTTONDBLCLK | WM_LBUTTONUP | WM_LBUTTONDBLCLK => true,
            _ => false,
        }
    }

    pub(super) fn group_view(&self, id: u64) -> View {
        let Some(g) = self.layout.groups.iter().find(|g| g.id == id) else { return View::Icons };
        match &g.content {
            GroupContent::Items { .. } => g.view(),
            GroupContent::Portal { path } => {
                let current = self.portal_current_path(id).unwrap_or(path);
                View::from_mode(view_state::resolve_view(current, &self.layout.view_states,
                    self.settings.folder_portals_inherit_view, view_state::Context { desktop_icon_size: 32 }).style.mode)
            }
        }
    }

    pub(super) fn set_group_view(&mut self, id: u64, view: View) {
        if self.group_view(id) == view { return; }
        self.end_header_press(false);
        self.header_hover = None;
        let anchors: Vec<_> = self.layout.groups.iter().map(|g| {
            let geo = self.group_geometry(g);
            let grid = self.group_grid(g.id, geo.dpi, geo.full_body, g.scroll);
            let members = self.group_members(g);
            let anchor = members.iter().position(|i| i.selected).unwrap_or_else(|| grid.first_visible(members.len()));
            (g.id, self.group_view(g.id), anchor)
        }).collect();
        let Some(g) = self.layout.groups.iter().find(|g| g.id == id) else { return };
        match &g.content {
            GroupContent::Items { .. } => self.layout.group_mut(id).unwrap().set_view(view),
            GroupContent::Portal { path } => {
                let current = self.portal_current_path(id).unwrap_or(path).to_string();
                view_state::set_view(&mut self.layout.view_states, &current, view);
            }
        }
        self.hide_item_tip();
        self.item_hover = None;
        // Other Gates showing or inheriting this folder also resolve the patched view immediately.
        for (member, old, anchor) in anchors {
            if self.group_view(member) == old { continue; }
            let g = self.layout.groups.iter().find(|g| g.id == member).unwrap();
            let geo = self.group_geometry(g);
            let grid = self.group_grid(member, geo.dpi, geo.full_body, 0);
            let scroll = grid.scroll_to(anchor, self.group_members(g).len());
            self.layout.group_mut(member).unwrap().scroll = to_dip(scroll, geo.dpi);
            if let Some(bar) = self.scrollbars.get(&member) { bar.seed(scroll, geo.dpi); }
        }
        self.retain_group_icon_rasters();
        self.save();
        self.refresh_all();
    }

    pub(super) fn update_item_hover(&mut self, index: usize, point: Option<(i32, i32)>) {
        let id = self.groups[index].id;
        let hit = point.filter(|_| self.group_view(id) != View::Icons && self.header_press.is_none() && matches!(self.drag, Drag::None))
            .and_then(|p| self.item_in_group(index, p)).map(|name| (id, name));
        if self.item_hover != hit {
            let old = self.item_hover.take().map(|h| h.0);
            self.hide_item_tip();
            self.item_hover = hit;
            if let Some(old_index) = old.filter(|old| *old != id).and_then(|old| self.groups.iter().position(|g| g.id == old)) {
                self.render_group(old_index);
            }
            self.render_group(index);
        }
        if self.item_hover.is_some() {
            use windows::Win32::UI::Input::KeyboardAndMouse::{TME_HOVER, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
            let mut track = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_HOVER | TME_LEAVE, hwndTrack: self.groups[index].hwnd, dwHoverTime: 0xFFFF_FFFF };
            unsafe { let _ = TrackMouseEvent(&mut track); }
        }
    }

    /// Group `id`'s rows moved or changed under a still pointer: its hover row and label tip no longer apply.
    pub(super) fn drop_item_hover(&mut self, id: u64) {
        if self.item_hover.as_ref().is_some_and(|(g, _)| *g == id) {
            self.item_hover = None;
            self.hide_item_tip();
        }
    }

    pub(super) fn hide_item_tip(&mut self) {
        if self.item_tip { crate::size_tip::hide(); self.item_tip = false; }
    }

    pub(super) fn item_label_tip(&mut self, index: usize, point: (i32, i32)) {
        if !matches!(self.drag, Drag::None) { return; }
        let id = self.groups[index].id;
        if self.group_view(id) == View::Icons || self.header_press.is_some() { return; }
        let Some(name) = self.item_in_group(index, point) else { return };
        let g = self.layout.groups.iter().find(|g| g.id == id).unwrap();
        let dpi = self.group_dpi(id);
        let grid = self.group_grid(id, dpi, self.groups[index].body, g.scroll);
        let Some(slot) = grid.slot_at(point) else { return };
        let Some(item) = self.items.iter().find(|i| i.shell.parsing_name.eq_ignore_ascii_case(&name)) else { return };
        let width = crate::render::measure_text(&item.shell.display_name, crate::style::FONT,
            crate::style::LABEL_FONT_PX * dpi as f32 / 96.0, 400).map_or(0.0, |m| m.0);
        if width > grid.slot(slot).label.width() as f32 {
            crate::size_tip::show(self.groups[index].hwnd, &item.shell.display_name);
            self.item_tip = true;
        }
    }

    pub(super) fn list_insertion(&self, id: u64, grid: &super::metrics::Grid, count: usize) -> Option<i32> {
        if !grid.is_list() { return None; }
        let g = self.layout.groups.iter().find(|g| g.id == id)?;
        if !matches!(g.content, GroupContent::Items { .. }) || crate::sorting::is_maintained(g.sort_method, g.sort_flags) { return None; }
        if let Some((_, slot)) = self.list_drop_mark.filter(|(target, _)| *target == id) {
            return Some(grid.insertion_y(slot));
        }
        let Drag::Item { current, active: true, .. } = self.drag else { return None };
        let gw = self.groups.iter().find(|g| g.id == id)?;
        let origin = self.to_screen((gw.rect.left, gw.rect.top));
        let point = (current.0 - origin.0, current.1 - origin.1);
        grid.body.contains(point.0, point.1).then(|| grid.insertion_y(grid.drop_slot(point, count)))
    }

    pub(super) fn scroll_item_into_view(&mut self, id: u64, slot: usize) {
        let Some(g) = self.layout.groups.iter().find(|g| g.id == id) else { return };
        let geo = self.group_geometry(g);
        let grid = self.group_grid(id, geo.dpi, geo.full_body, g.scroll);
        let px = grid.scroll_to(slot, self.group_members(g).len());
        let next = to_dip(px, geo.dpi);
        if next != g.scroll {
            self.layout.group_mut(id).unwrap().scroll = next;
            if let Some(bar) = self.scrollbars.get(&id) { bar.seed(px, geo.dpi); }
            self.drop_item_hover(id);
        }
    }

    pub(super) fn update_list_drop_mark(&mut self, screen: Option<(i32, i32)>) {
        let mark = screen.and_then(|screen| {
            let origin = self.origin();
            let client = (screen.0 - origin.0, screen.1 - origin.1);
            let gw = self.groups.iter().find(|gw| {
                self.layout.groups.iter().find(|g| g.id == gw.id).is_some_and(|g|
                    crate::tabs::body_visible(&self.layout.groups, g) && self.page_group_visible(g))
                    && gw.body.offset(gw.rect.left, gw.rect.top).contains(client.0, client.1)
            })?;
            let g = self.layout.groups.iter().find(|g| g.id == gw.id)?;
            if !matches!(g.content, GroupContent::Items { .. }) || crate::sorting::is_maintained(g.sort_method, g.sort_flags) { return None; }
            let grid = self.group_grid(g.id, self.group_dpi(g.id), gw.body, g.scroll);
            if !grid.is_list() { return None; }
            let point = (client.0 - gw.rect.left, client.1 - gw.rect.top);
            grid.body.contains(point.0, point.1).then(|| (g.id, grid.drop_slot(point, self.group_members(g).len())))
        });
        if mark == self.list_drop_mark { return; }
        let old = self.list_drop_mark;
        self.list_drop_mark = mark;
        let mut ids = Vec::new();
        for (id, _) in old.into_iter().chain(mark) { if !ids.contains(&id) { ids.push(id); } }
        for id in ids { if let Some(i) = self.groups.iter().position(|g| g.id == id) { self.render_group(i); } }
    }
}

#[cfg(test)]
mod details_tests {
    use super::*;
    #[test]
    fn header_click_changes_persisted_method_and_toggles_direction() {
        let mut layout = crate::model::Layout::new();
        let id = layout.add_group("test".into(),"monitor".into(),crate::geometry::Rect::new(0,0,300,200),GroupContent::Items { items: vec![] });
        for key in [ColumnKey::Name,ColumnKey::Size,ColumnKey::Type,ColumnKey::DateModified] {
            let g = layout.group_mut(id).unwrap();
            g.sort_flags = 0;
            let (method,flags) = header_sort(g.sort_method,g.sort_flags,key);
            g.sort_method=method; g.sort_flags=flags;
            assert_eq!(g.sort_method,key.method()); assert_eq!(g.sort_flags & crate::sorting::FLAG_REVERSE,0);
            let (method,flags) = header_sort(g.sort_method,g.sort_flags,key);
            g.sort_method=method; g.sort_flags=flags;
            let reload: crate::model::Layout = serde_json::from_str(&serde_json::to_string(&layout).unwrap()).unwrap();
            assert_eq!(reload.groups[0].sort_method,key.method());
            assert_ne!(reload.groups[0].sort_flags & crate::sorting::FLAG_REVERSE,0);
        }
    }
}

#[cfg(test)]
mod header_state_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn changing_header_column_keeps_direction_for_corrals_and_gates() {
        use crate::sorting::*;
        for reverse in [false, true] {
            for key in [ColumnKey::Name, ColumnKey::Size, ColumnKey::Type, ColumnKey::DateModified] {
                let other = if key == ColumnKey::Name { ColumnKey::Size } else { ColumnKey::Name };
                let flags = 0x80 | if reverse { FLAG_REVERSE } else { 0 };
                assert_eq!(header_sort(other.method(), flags, key), (key.method(), flags));
                assert_eq!(header_sort(key.method(), flags, key), (key.method(), flags ^ FLAG_REVERSE));
                let sign = if reverse { -1 } else { 1 };
                assert_eq!(gate_header_sort(sign * other.sort(), key), sign * key.sort());
                assert_eq!(gate_header_sort(sign * key.sort(), key), -sign * key.sort());
            }
        }
    }
    #[test]
    fn gate_header_click_patches_only_folder_sort_and_survives_reload() {
        let mut store: view_state::ViewStates = serde_json::from_value(json!({"folder":{"set":true,"view_mode":4,"col_info":[{"key":"future","width":88}],"extra":12},"other":{"set":true,"sort_mode":-2}})).unwrap();
        let original = serde_json::to_value(&store).unwrap();
        for key in [ColumnKey::Name, ColumnKey::Size, ColumnKey::Type, ColumnKey::DateModified] {
            let old = view_state::resolve_view("folder",&store,false,view_state::Context { desktop_icon_size:32 }).sort;
            let sort = gate_header_sort(old,key);
            view_state::apply_patch(&mut store,"folder",view_state::Patch { sort:Some(sort), ..Default::default() },view_state::Context { desktop_icon_size:32 });
            let reverse = gate_header_sort(sort,key);
            assert_eq!(reverse,-sort);
            view_state::apply_patch(&mut store,"folder",view_state::Patch { sort:Some(reverse), ..Default::default() },view_state::Context { desktop_icon_size:32 });
            let saved=serde_json::to_value(&store).unwrap();
            assert_eq!(saved["folder"]["sort_mode"],reverse);
            assert_eq!(saved["other"],original["other"]);
            assert_eq!(saved["folder"]["col_info"],original["folder"]["col_info"]);
            assert_eq!(saved["folder"]["view_mode"],4);
            store=serde_json::from_value(saved).unwrap();
        }
    }
    #[test]
    fn header_enables_sort_after_manual_order_and_keeps_unknown_flags() {
        use crate::sorting::*;
        for method in [METHOD_NONE,METHOD_SIZE] {
            let (method,flags)=header_sort(method,FLAG_NOT_MAINTAINED | 0x80,ColumnKey::Size);
            assert_eq!(method,METHOD_SIZE);
            assert_eq!(flags & (FLAG_NOT_MAINTAINED | FLAG_REVERSE),0);
            assert_ne!(flags & 0x80,0);
        }
    }
}
