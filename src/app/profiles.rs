//! Display events are classified against coherent snapshots before adoption.
//! Corral choice: Preserve the baseline through a rearmed
//! 2-second settle; work-only changes refresh and rescue under the same key.

use crate::profile_store::{self, Switch};
use crate::dpi_transition::{self as dpi, Snapshot, EventKind, TitleReachability};
use crate::model::Layout;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};
use super::{App, Mode, TIMER_PROFILE, log};
use super::hosting::snapshot_monitors;

const PROFILE_SETTLE_MS: u32 = 2000;

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub(super) struct Pending {
    baseline: Layout,
    old: Snapshot,
    latest: Snapshot,
}

impl Pending {
    fn new(layout: &Layout, old: Snapshot, latest: Snapshot, _now: i64) -> Self {
        Self { baseline: layout.clone(), old, latest }
    }

    fn observe(&mut self, live: Snapshot) -> bool {
        if dpi::classify(&self.latest, &live).kind() == EventKind::Unchanged { return false; }
        self.latest = live;
        true
    }

    #[cfg(test)]
    fn select(self, layout: &mut Layout, live: &Snapshot, now: i64) -> Switch {
        let states = crate::desk_setup::fallback_monitors(&snapshot_monitors(live));
        self.select_states(layout, live, &states, now)
    }

    fn select_states(self, layout: &mut Layout, live: &Snapshot, states: &[crate::desk_setup::MonitorState], now: i64) -> Switch {
        // desk-setups-spec 1 non-goals: keep user edits made while the settle timer ran.
        let changed: Vec<_> = layout.groups.iter().filter(|g| {
            self.baseline.groups.iter().find(|b|b.id==g.id).is_none_or(|b|
                (b.monitor.as_str(),b.rect,b.roll,b.dock_top,b.dock_bottom,b.preferred_size_dip)
                != (g.monitor.as_str(),g.rect,g.roll,g.dock_top,g.dock_bottom,g.preferred_size_dip))
        }).cloned().collect();
        let loose_changed=layout.loose!=self.baseline.loose;
        let loose=layout.loose.clone();
        let deleted=self.baseline.groups.iter().any(|b|!layout.groups.iter().any(|g|g.id==b.id));
        let outcome=profile_store::switch_states(layout, states, now);
        for edit in &changed {
            if let Some(g)=layout.group_mut(edit.id) {
                g.monitor=edit.monitor.clone();g.rect=edit.rect;g.roll=edit.roll;
                g.dock_top=edit.dock_top;g.dock_bottom=edit.dock_bottom;g.preferred_size_dip=edit.preferred_size_dip;
                if live.find(&g.monitor).is_none() {if let Some(d)=live.displays().iter().find(|d|d.primary).or(live.displays().first()){g.monitor=d.device.clone();}}
            }
        }
        if loose_changed {layout.loose=loose;}
        if !changed.is_empty()||loose_changed||deleted {profile_store::edited(layout,now,false);}
        outcome
    }
}

/// desk-setups-spec 3.2: setups are read or written only for `LayoutPreserve` 2.
fn desk_setups_enabled(desktop: bool, layout_preserve: i64) -> bool { desktop && layout_preserve == 2 }

impl App {
    pub(super) fn profiles_enabled(&self) -> bool {
        desk_setups_enabled(self.opts.mode == Mode::Desktop || std::env::var("CORRAL_QA_PROFILES").is_ok_and(|v| v == "1"),
            self.settings.get(crate::settings::Field::LayoutPreserve))
    }

    /// Records `LayoutPreserve` in the layout (desktop engine only) and returns the value saved earlier.
    fn note_desk_mode(&mut self) -> Option<i64> {
        let mode = self.settings.get(crate::settings::Field::LayoutPreserve);
        let desktop = self.opts.mode == Mode::Desktop;
        let was = self.layout.desk_setups.mode;
        if desktop { self.layout.desk_setups.mode = Some(mode); }
        was
    }

    /// `adopt_if_mode_changed`: a start with setups on after they were off adopts the live layout (spec 3.2);
    /// a snapshot restore passes false and evaluates normally.
    pub(super) fn profile_startup(&mut self, adopt_if_mode_changed: bool) {
        self.remember_preferred_sizes();
        let was = self.note_desk_mode();
        let transition = was.is_some_and(|m| Some(m) != self.layout.desk_setups.mode);
        if !self.profiles_enabled() || self.monitors.is_empty() { if transition { self.save(); } return; }
        let states=crate::desk_setup::read_monitors(&self.monitors);
        if adopt_if_mode_changed && was.is_some_and(|m| m != 2) && !self.layout.groups.is_empty() {
            let saved = crate::desk_setup::adopt(&mut self.layout, &states, now_ms());
            log(&format!("desk setup at start: adopted the live layout (saved a setup: {saved})"));
            self.save();
        } else {
            let outcome = profile_store::switch_states(&mut self.layout, &states, now_ms());
            log(&format!("desk setup at start: {outcome:?}"));
            if transition { self.save(); }
        }
        self.publish_desk_view();
    }

    /// Settings were reloaded; `was_2` = `LayoutPreserve` was 2 before. Turning setups back on keeps the
    /// arrangement made meanwhile (spec 3.2).
    pub(super) fn desk_mode_reloaded(&mut self, was_2: bool) {
        let marker = self.note_desk_mode();
        let transition = marker.is_some_and(|m| Some(m) != self.layout.desk_setups.mode);
        if self.profiles_enabled() && !was_2 && !self.layout.groups.is_empty() && !self.monitors.is_empty() {
            self.remember_preferred_sizes();
            let states = crate::desk_setup::read_monitors(&self.monitors);
            let saved = crate::desk_setup::adopt(&mut self.layout, &states, now_ms());
            log(&format!("desk setup enabled: adopted the live layout (saved a setup: {saved})"));
            self.runtime_bodies.clear(); self.rebuild_runtime_bodies(); self.clamp_profile_titles(); self.acknowledge_desk_display();
            self.save();
        } else {
            if !self.profiles_enabled() && self.layout.desk_setups.runtime.edit_pending { self.desk_save_timer(); }
            if transition { self.save(); }
        }
    }

    pub(super) fn profile_displays_changed(&mut self) {
        let Some(live) = self.current_display_snapshot() else { return };
        let Some(old) = self.display_snapshot.clone() else {
            self.monitors = snapshot_monitors(&live); self.display_snapshot = Some(live); return;
        };
        let changes = dpi::classify(&old, &live);
        log(&format!("display transition: {:?} flags={changes:?} pending={}", changes.kind(), self.profile_pending.is_some()));
        if let Some(pending) = &mut self.profile_pending {
            if pending.observe(live) { self.arm_profile_settle(); }
            return;
        }
        if changes.kind() == EventKind::Unchanged {
            // A group's visible HWND can send WM_DPICHANGED while the global
            // display configuration is unchanged. Ignore its suggested RECT.
            if let super::Drag::MoveGroup { id, .. } | super::Drag::ResizeGroup { id, .. } = self.drag {
                if let Some(g) = self.layout.groups.iter().find(|g| g.id == id) {
                    if let Some(r) = self.requested_body(g) { self.apply_body_placement(id, r); }
                }
            }
            return;
        }
        if self.layout.desk_setups.runtime.edit_pending {self.desk_save_timer();}
        self.remember_preferred_sizes();
        self.cancel_group_for_display();
        self.page_cancel_motion();
        if self.profiles_enabled() && (changes.topology || changes.dpi) {
            self.abandon_color_preview(); // neither replay nor persist a pre-transition colour preview
            self.profile_pending = Some(Pending::new(&self.layout, old, live, now_ms()));
            self.arm_profile_settle();
            return;
        }
        let work_bodies: Vec<_> = if changes.kind() == EventKind::WorkAreaOnly {
            self.layout.groups.iter().filter(|g| crate::tabs::body_visible(&self.layout.groups, g))
                .filter_map(|g| Some((g.id, self.requested_body(g)?))).collect()
        } else { Vec::new() };
        if changes.kind() == EventKind::WorkAreaOnly {
            {
                // Preview without profiles, or an uninitialised profile: rebase
                // directly, then record the final coherent work snapshot below.
                for g in &mut self.layout.groups {
                    if let Some((a, b)) = old.find(&g.monitor).zip(live.find(&g.monitor)) {
                        if let Some(rect) = dpi::rebase_work_origin(g.rect, a, b) { g.rect = rect; }
                    }
                }
                for p in self.layout.loose.values_mut() {
                    if let Some((a, b)) = old.find(&p.monitor).zip(live.find(&p.monitor)) {
                        if let Some(r) = dpi::rebase_work_origin(crate::geometry::Rect::new(p.x, p.y, p.x, p.y), a, b) { p.x = r.left; p.y = r.top; }
                    }
                }
            }
        }
        self.monitors = snapshot_monitors(&live); self.display_snapshot = Some(live);
        self.rebuild_runtime_bodies();
        for (id, r) in work_bodies { self.apply_body_placement(id, r); }
        self.clamp_profile_titles();
        if changes.dpi || changes.topology { self.load_items(); }
        else if self.refresh_icon_metrics() { self.load_items(); }
        self.acknowledge_desk_display();
        self.refresh_all();
    }

    fn arm_profile_settle(&self) {
        unsafe { SetTimer(self.message_window, TIMER_PROFILE, PROFILE_SETTLE_MS, None); }
    }

    pub(super) fn profile_timer(&mut self) {
        unsafe { let _ = KillTimer(self.message_window, TIMER_PROFILE); }
        let Some(live) = self.current_display_snapshot() else { self.arm_profile_settle(); return };
        if live.displays().is_empty() { self.arm_profile_settle(); return; }
        if !self.profiles_enabled() {
            self.profile_pending=None; self.monitors=snapshot_monitors(&live);self.display_snapshot=Some(live);
            self.rebuild_runtime_bodies();self.refresh_all();
            // Setups were switched off during the settle: do not leave a pending edit blocking saves.
            if self.layout.desk_setups.runtime.edit_pending { self.desk_save_timer(); }
            return;
        }
        let Some(pending) = &mut self.profile_pending else { return };
        if pending.observe(live.clone()) { self.arm_profile_settle(); return; }
        let pending = self.profile_pending.take().unwrap();
        let states=crate::desk_setup::read_monitors(&snapshot_monitors(&live));
        let active_before=self.layout.desk_setups.active.clone();
        let outcome = pending.select_states(&mut self.layout, &live, &states, now_ms());
        self.monitors = snapshot_monitors(&live); self.display_snapshot = Some(live);
        self.runtime_bodies.clear();
        self.load_items();
        self.rebuild_runtime_bodies();
        self.clamp_profile_titles();
        log(&format!("desk setup switch: {outcome:?}"));
        self.acknowledge_desk_display();
        self.refresh_all();
        if self.layout.desk_setups.runtime.edit_pending {self.desk_save_timer();}
        // desk-setups-spec 4.3: `active` is the only value a restore or recovery persists.
        else if self.layout.desk_setups.active != active_before {self.save();}
    }

    /// Only temporarily fitted groups get full-envelope containment.
    pub(super) fn contain_fitted_group(&mut self, id: u64) {
        let Some(g) = self.layout.groups.iter().find(|g| g.id == id) else { return };
        if self.preferred_size(id) == (g.rect.width(), g.rect.height()) { return; }
        let Some(body) = self.requested_body(g) else { return };
        let geo = self.group_geometry_stored(g);
        let metrics = self.group_title_metrics(g, geo.dpi);
        let (ox, oy) = self.origin();
        let title = crate::group_paint::title_rect_on(geo.full_body, geo.below, &metrics);
        let envelope = crate::group_paint::union_rect(geo.full_body, title, false).offset(ox, oy);
        let Some(d) = self.display_snapshot.as_ref().and_then(|s| s.find(&g.monitor)) else { return };
        if let Some(body) = dpi::contain_fitted(body, envelope, d) { self.apply_body_placement(id, body); }
    }

    /// Mapping already happened; rescue with destination title metrics.
    pub(super) fn clamp_profile_titles(&mut self) {
        let ids: Vec<_> = self.layout.groups.iter().filter(|g| crate::tabs::body_visible(&self.layout.groups, g)).map(|g| g.id).collect();
        for id in ids {
            let edge = self.layout.groups.iter().find(|g| g.id == id).and_then(|g| {
                g.dock_edge().or_else(|| {
                    (g.roll == crate::model::RollState::Docked && self.group_is_docked(id))
                        .then(|| if self.group_geometry(g).below { crate::rollup::DockEdge::Top } else { crate::rollup::DockEdge::Bottom })
                })
            });
            if let Some(edge) = edge {
                let g = self.layout.groups.iter().find(|g| g.id == id).unwrap();
                let geo = self.group_geometry_stored(g);
                let (ox,oy) = self.origin();
                let work = self.monitor_for(&g.monitor).map_or(geo.full_body.offset(ox,oy), |m| m.work);
                let (_,_,body) = edge.layout(geo.full_body.offset(ox,oy), work,
                    geo.metrics.title_h, geo.metrics.gap, 0, self.settings.title_edge_auto_adjust);
                if let Some(g) = self.layout.group_mut(id) {
                    g.set_dock_edge(Some(edge)); g.title_side = -1;
                    g.title_side_last = i32::from(edge.title_below(self.settings.title_edge_auto_adjust));
                }
                if let Some(body) = dpi::RequestedBody::from_physical(body) { self.apply_body_placement(id,body); }
                let _ = self.layout.sync_tab_common(id);
            }
            self.contain_fitted_group(id);
            for _ in 0..4 {
                let Some(g) = self.layout.groups.iter().find(|g| g.id == id) else { continue };
                let Some(body) = self.requested_body(g) else { continue };
                let geo = self.group_geometry_stored(g);
                let (ox, oy) = self.origin();
                // Measure the expanded strip even for rolled/docked groups and for
                // a hidden label (a reachable title is needed when it is revealed).
                let metrics = self.group_title_metrics(g, geo.dpi);
                let title = crate::group_paint::title_rect_on(geo.full_body, geo.below, &metrics).offset(ox, oy);
                let r = body.physical();
                let reach = TitleReachability { relative: title.offset(-r.left, -r.top), visible_width: super::scale(64, geo.dpi) };
                let Some(d) = self.display_snapshot.as_ref().and_then(|s| s.find(&g.monitor)) else { continue };
                let Some(clamped) = dpi::clamp_title(body, d, reach) else { continue };
                self.apply_body_placement(id, clamped);
                if clamped == body { break; }
            }
        }
        self.layout.sync_all_tab_sets();
    }

    /// The layout as it is stored. An unfinished colour draft is never part of it (colour-state-spec 3.2).
    pub(super) fn layout_to_save(&self) -> std::borrow::Cow<'_, Layout> {
        let base = match &self.profile_pending {
            Some(pending) => &pending.baseline,
            None => &self.layout,
        };
        match self.color_overlay.current() {
            Some(overlay) => {
                let mut layout = base.clone();
                overlay.restore(&mut layout, &mut self.settings.clone());
                std::borrow::Cow::Owned(layout)
            }
            None => std::borrow::Cow::Borrowed(base),
        }
    }

    /// The settings as they are stored (no colour draft); every engine write or backup of
    /// `self.settings` goes through this.
    pub(super) fn settings_to_save(&self) -> std::borrow::Cow<'_, crate::settings::Settings> {
        match self.color_overlay.current() {
            Some(overlay) => {
                let mut settings = self.settings.clone();
                overlay.restore(&mut self.layout.clone(), &mut settings);
                std::borrow::Cow::Owned(settings)
            }
            None => std::borrow::Cow::Borrowed(&self.settings),
        }
    }

    pub(super) fn publish_desk_view(&self) {
        if let Err(e)=crate::desk_setup::publish_view(&self.opts.layout_path,&self.layout.desk_setups){log(&format!("desk setup view: {e}"));}
    }
    pub(super) fn acknowledge_desk_display(&mut self) {
        if self.profiles_enabled(){crate::desk_setup::acknowledge_display(&mut self.layout);self.publish_desk_view();}
    }
    /// A drag or title release that moved a Corral; not recorded when a display change cancelled it.
    pub(super) fn record_drag_edit(&mut self, changed: bool) {
        if crate::desk_setup::records_drag_end(changed, self.layout.desk_setups.runtime.display_cancel) { self.record_desk_edit(); }
    }
    pub(super) fn record_desk_edit(&mut self) {
        if !self.profiles_enabled(){return}
        self.layout.desk_setups.runtime.edit_pending=true;
        unsafe{SetTimer(self.message_window,super::TIMER_DESK_SAVE,500,None);}
    }
    pub(super) fn desk_save_timer(&mut self) {
        unsafe{let _=KillTimer(self.message_window,super::TIMER_DESK_SAVE);}
        if self.profile_pending.is_some() {return}
        if !self.layout.desk_setups.runtime.edit_pending{return}
        self.layout.desk_setups.runtime.edit_pending=false;
        if self.profiles_enabled() {profile_store::edited(&mut self.layout,now_ms(),false);}
        self.save(); self.publish_desk_view();
    }

    pub(super) fn consume_desk_request(&mut self) {
        use crate::desk_setup::{Action,RequestResult};
        let Some(request)=self.settings.desk_setup_request.take()else{return};
        let recent=now_ms().max(0) as u64;
        let valid=crate::desk_setup::normalized_path(std::path::Path::new(&request.layout_path))==crate::desk_setup::normalized_path(&self.opts.layout_path)
            && recent.abs_diff(request.nonce)<=90_000 && self.profiles_enabled() && self.profile_pending.is_none()
            && self.settings.desk_setup_result.as_ref().is_none_or(|r|r.nonce!=request.nonce);
        let mut changed=false;
        let ok=if !valid {false}else{match request.action {
            Action::Forget{id}=>{changed=self.layout.desk_setups.forget(&id);changed},
            Action::SwapMonitors=>{
                let ms=&self.layout.desk_setups.runtime.current;
                let mut rank:Vec<_>=ms.iter().collect();rank.sort_by_key(|m|m.offset_px);
                if rank.len()<2 {false}else{
                    let a=rank[0].gdi_name.clone();let b=rank[1].gdi_name.clone();
                    for g in &mut self.layout.groups {if g.monitor==a{g.monitor=b.clone();changed=true}else if g.monitor==b{g.monitor=a.clone();changed=true}}
                    if changed {self.runtime_bodies.clear();self.rebuild_runtime_bodies();self.clamp_profile_titles();self.record_desk_edit();self.refresh_all();}true
                }
            }
        }};
        self.settings.desk_setup_result=Some(RequestResult{nonce:request.nonce,ok,message:if ok{String::new()}else{"This request could not be applied. The active setup cannot be forgotten; try again after monitor changes settle.".into()}});
        if let Err(e)=crate::settings::save(&self.opts.settings_path,&self.settings_to_save()){log(&format!("desk setup response: {e}"));}
        if changed{if self.layout.desk_setups.runtime.edit_pending{self.desk_save_timer();}else{self.save();}self.publish_desk_view();}
    }
}

#[cfg(test)]
mod display_settle_tests {
    use super::*;
    use crate::{desk_setup as ds, geometry::Rect, model::GroupContent};
    use crate::app::dpi_wiring_tests::{displays as two_displays, layout as two_layout};
    fn evaluate(l:&mut Layout,s:&Snapshot,now:i64)->Switch {profile_store::switch_states(l,&ds::fallback_monitors(&snapshot_monitors(s)),now)}
    fn displays(dpi:u32)->Snapshot {
        Snapshot::new(vec![dpi::DisplaySnapshot::from_monitor(&crate::geometry::Monitor{name:"A".into(),bounds:Rect::new(0,0,2560,1440),work:Rect::new(0,0,2560,1400),dpi},true,None)]).unwrap()
    }
    fn recorded()->(Layout,Snapshot) {
        let old=displays(96);let mut l=Layout::new();l.add_group("one".into(),"A".into(),Rect::new(100,100,500,400),GroupContent::Items{items:vec![]});
        evaluate(&mut l,&old,1);l.groups[0].rect=l.groups[0].rect.offset(1,0);ds::user_edit(&mut l,2,false);(l,old)
    }
    #[test] fn setups_are_read_and_written_only_for_remembered_layouts_on_the_desktop() {
        assert!(desk_setups_enabled(true,2));
        for mode in [0,1] {assert!(!desk_setups_enabled(true,mode));}
        assert!(!desk_setups_enabled(false,2));
    }
    #[test] fn rapid_changes_rearm_without_overwriting_old_profile() {
        let (mut l,old)=recorded();let bank=l.desk_setups.setups.clone();let mut p=Pending::new(&l,old.clone(),displays(144),3);
        assert!(p.observe(displays(192)));assert!(!p.observe(displays(192)));assert_eq!(p.old,old);
        p.select(&mut l,&displays(192),4);assert_eq!(l.desk_setups.setups,bank);
    }
    #[test] fn returning_to_original_before_settle_keeps_baseline() {
        let (mut l,old)=recorded();let rect=l.groups[0].rect;let mut p=Pending::new(&l,old.clone(),displays(144),3);assert!(p.observe(old.clone()));p.select(&mut l,&old,4);assert_eq!(l.groups[0].rect,rect);
    }
    #[test] fn settle_preserves_shared_content_and_geometry_edits() {
        let (mut l,old)=recorded();let bank=l.desk_setups.setups[0].clone();let p=Pending::new(&l,old,displays(144),3);
        l.groups[0].title="edited during settle".into();l.groups[0].rect=Rect::new(20,30,420,330);l.groups[0].dock_top=true;
        p.select(&mut l,&displays(144),4);assert_eq!(l.groups[0].title,"edited during settle");assert_eq!(l.groups[0].rect,Rect::new(20,30,420,330));assert!(l.groups[0].dock_top);assert_eq!(l.desk_setups.setups[0],bank);assert_eq!(l.desk_setups.setups.len(),2);
    }
    #[test] fn measured_title_rescue_after_mapping_is_reachable_at_destination_dpi() {
        let live = two_displays(false);
        let dest = live.find("Monitor A").unwrap();
        let r = dpi::RequestedBody::from_physical(Rect::new(100, 2150, 701, 2453)).unwrap();
        let metrics = crate::group_paint::metrics(dest.dpi);
        let title = crate::group_paint::title_rect_on(r.physical(), true, &metrics);
        let reach = TitleReachability { relative: title.offset(-100, -2150), visible_width: 160 };
        let rescued = dpi::clamp_title(r, dest, reach).unwrap();
        let t = crate::group_paint::title_rect_on(rescued.physical(), true, &metrics);
        assert!(t.top >= dest.work.top && t.bottom <= dest.work.bottom);
        assert_eq!((rescued.physical().width(), rescued.physical().height()), (601, 303));
        assert_eq!(dpi::place(rescued, rescued.physical(), &live).unwrap().monitor, "Monitor A");
    }
    #[test] fn removed_monitor_recovery_keeps_preference_then_fits_destination_and_returns_home() {
        let old=two_displays(false);let mut l=two_layout();
        l.groups[0].rect=Rect::new(100,0,1380,1400);l.groups[0].preferred_size_dip=Some((1280,1400));
        evaluate(&mut l,&old,1);
        l.groups[0].rect=l.groups[0].rect.offset(1,0);
        assert!(ds::user_edit(&mut l,2,false));
        let saved=serde_json::to_vec(&l.desk_setups.setups[0]).unwrap();let saved_rect=l.groups[0].rect;
        let mut mon_a=old.find("Monitor A").unwrap().clone(); mon_a.primary=true;
        let live=Snapshot::new(vec![mon_a]).unwrap();
        Pending::new(&l,old.clone(),live.clone(),4).select(&mut l,&live,5);
        let g=&l.groups[0];
        assert_eq!(g.monitor,"Monitor A");
        assert_eq!(g.preferred_size_dip,Some((1280,1400)));
        let d=live.find("Monitor A").unwrap();
        let requested=dpi::RequestedBody::from_dips(g.rect,d).unwrap();
        let m=crate::group_paint::metrics(d.dpi);
        let (p,_)=super::super::fitted_body_placement(requested,&live,d,g.preferred_size_dip.unwrap(),
            (328,258),m.title_h+m.gap).unwrap();
        assert!(p.rect_dip.height()<864);
        let id=g.id;
        super::super::store_body_placement(&mut l,&mut std::collections::HashMap::new(),id,p);
        // The fit is temporary: the saved setup is untouched and the way back restores the saved placement.
        assert_eq!(serde_json::to_vec(&l.desk_setups.setups[0]).unwrap(),saved);
        evaluate(&mut l,&old,6);
        assert_eq!(l.groups[0].preferred_size_dip,Some((1280,1400)));
        assert_eq!((l.groups[0].monitor.as_str(),l.groups[0].rect),("Monitor B",saved_rect));
        let d=old.find("Monitor B").unwrap();
        let requested=dpi::RequestedBody::from_dips(l.groups[0].rect,d).unwrap();
        let (p,_)=super::super::fitted_body_placement(requested,&old,d,(1280,1400),crate::snap::MIN_BODY_DIP,crate::group_paint::metrics(96).title_h).unwrap();
        assert_eq!((p.rect_dip.width(),p.rect_dip.height()),(1280,1400));
    }
    #[test] fn empty_burst_does_not_destroy_original_baseline() {
        let (l,old)=recorded();let empty=Snapshot::new(vec![]).unwrap();let mut p=Pending::new(&l,old.clone(),empty,3);assert!(p.observe(old.clone()));assert_eq!(p.baseline.groups,l.groups);assert_eq!(p.old,old);
    }
    #[test] fn layout_saves_during_settle_use_the_baseline() {
        let (mut l,old)=recorded();let p=Pending::new(&l,old,displays(144),3);l.groups[0].rect=Rect::new(1,2,30,40);assert_ne!(p.baseline.groups,l.groups);assert_eq!(p.baseline.desk_setups.setups,l.desk_setups.setups);
    }
}
