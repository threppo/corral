//! Window integration: desktop/preview hosting, group windows, creation by drawing, naming,
//! persistence. Coordinates of child windows are parent-client physical pixels; the model stores
//! group rectangles in DIPs relative to the monitor work area (see model.rs).

mod creation_glue;
mod group_menu;
mod hosting;
mod input;
mod item_drag;
mod metrics;
mod paint;
mod pages_glue;
mod peek;
mod portals;
mod profiles;
mod quick_hide_glue;
mod round_up_glue;
mod selection;
mod showdesk;
mod shell_items;
mod snapshot_glue;
mod style_glue;
mod taskbar_item;
mod zguard;

use crate::creation;
use crate::dpi_transition::{self as dpi_transition, RequestedBody, Snapshot};
use crate::desktop::{self, DesktopHost};
use crate::geometry::{Monitor, Rect, to_px};
use crate::group_paint;
use crate::model::{self, Layout, RollState};
use crate::render::Surface;
use crate::settings::{self as settings_store, Settings};
use crate::shell::{IconBitmap, ShellItem};
use crate::shell_menu::ShellMenu;
use crate::dragdrop;
use crate::gate_host;
use crate::portal;
use crate::rollup;
use crate::style;
use crate::watchdog;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Child;
use std::rc::Rc;
use std::time::Instant;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, HFONT};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_ALT, MOD_CONTROL, MOD_SHIFT, RegisterHotKey};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};
use self::hosting::{client_size, create_layer, register_desktop_notifications};
use self::item_drag::{OleDrag, OleDragRequest};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Top-level window simulating one monitor; never touches Explorer.
    Preview,
    /// Children of Explorer's desktop host; hides the desktop list view while running.
    Desktop,
}

pub struct Options {
    pub mode: Mode,
    pub layout_path: PathBuf,
    /// Auto-exit after this many seconds (disposable trials).
    pub seconds: Option<u32>,
    /// Settings store (JSON written by the settings window).
    pub settings_path: PathBuf,
}

const TIMER_REASSERT: usize = 1;
const TIMER_EXIT: usize = 2;
const TIMER_REATTACH: usize = 3;
const TIMER_SETTINGS: usize = 4;
const TIMER_RELOAD_ITEMS: usize = 5;
const TIMER_ANIMATE: usize = 6;
const TIMER_HOVER_POLL: usize = 7;
const TIMER_HOVER_DELAY: usize = 8;
const TIMER_TITLE_HOVER: usize = 9;
/// Existing 33 ms coalescing for exact-only resize modes; overlays render at each size.
const TIMER_DRAG_RENDER: usize = 10;
/// 3 s debounced layout save after a changed move/resize event.
const TIMER_LAYOUT_SAVE: usize = 11;
/// Monitor profile switch after the displays settled (`profiles.rs`).
const TIMER_PROFILE: usize = 13;
const TIMER_TAB_HOVER: usize = 14;
const TIMER_TAB_SAVE: usize = 15;
/// The drop zone under a dragged Corral arms 120 ms after the pointer entered it (tabs-spec 3.2).
const TIMER_TAB_ARM: usize = 18;
/// Scrolls a crowded tab strip while a tab is dragged near its end (tabs-spec 5.2).
const TIMER_TAB_SCROLL: usize = 19;
const TIMER_DESK_SAVE: usize = 17;
/// Deferred drop actions (tab selection, Alt portal creation) run right after the OLE callback.
const TIMER_DEFERRED_DROPS: usize = 16;
/// Shell change notifications for the desktop arrive as this message on the message window.
const WM_SHELL_CHANGE: u32 = WM_APP + 0x20;
/// Delay between a desktop change notice and the re-read it triggers (throttle period).
const SHELL_CHANGE_MS: u32 = 60;
/// Minimum gap between change-notice re-reads while notices keep arriving (follow-up review, finding 3).
const SHELL_CHANGE_GAP_MS: u32 = 300;
/// Posted when one of Corral's desktop layers changed z-order; re-establishes the desktop stack.
const WM_RESTACK: u32 = WM_APP + 0x21;

const HOTKEY_QUIT: i32 = 1;
const EM_SETSEL: u32 = 0x00B1;

struct Item {
    shell: ShellItem,
    icon: Option<IconBitmap>,
    /// Loose icon cell origin in parent-client pixels.
    pos: (i32, i32),
    /// True once `pos` is meaningful (from Explorer or assigned by `arrange_loose`).
    placed: bool,
    selected: bool,
    /// The Gate whose folder this item was read from (corral-views-spec 2.2); `None` for desktop items.
    gate: Option<u64>,
}

struct GroupWindow {
    id: u64,
    hwnd: HWND,
    surface: Surface,
    /// Current window rectangle in parent-client pixels.
    rect: Rect,
    /// Title strip and icon area relative to the window.
    title: Rect,
    body: Rect,
    /// A Gate's inline folder glyph relative to the window; empty otherwise.
    glyph: Rect,
    /// The menu, Up and close buttons (corral-painting-spec 6); empty while not shown.
    menu_glyph: Rect,
    close_glyph: Rect,
    up_glyph: Rect,
    /// Visual hover only; it never selects a member.
    tab_hover: Option<u32>,
    /// The pointer is over the title strip (hover mix) and which title button it is over.
    strip_hot: bool,
    hot_button: Option<TitleButton>,
}

/// A hit-tested element of the title strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TitleButton {
    Menu,
    Up,
    Close,
}

#[derive(Clone, Copy, Debug)]
struct GroupGeometry {
    /// Visible icon area (animated for rolled groups).
    body: Rect,
    /// Full icon area (icon grid origin).
    full_body: Rect,
    title: Rect,
    union: Rect,
    #[allow(dead_code)]
    below: bool,
    rolled: bool,
    metrics: group_paint::Metrics,
    dpi: u32,
}

impl GroupGeometry {
    /// Pure adapter: paint, Gate child windows, scrollbars and input share these
    /// current rectangles, including explicit fold/unfold and setting changes.
    #[cfg(test)]
    fn top_docked(body: Rect, work_top: i32, metrics: group_paint::Metrics,
        dpi: u32, progress: f64, below: bool) -> Self {
        Self::edge_docked(body, Rect::new(body.left, work_top, body.right, body.bottom),
            metrics, dpi, progress, rollup::DockEdge::Top, below)
    }

    fn edge_docked(body: Rect, work: Rect, metrics: group_paint::Metrics,
        dpi: u32, progress: f64, edge: rollup::DockEdge, auto: bool) -> Self {
        let visible = rollup::visible_body_height(body.height(), progress);
        let (title, shown, full_body) = edge.layout(body, work, metrics.title_h, metrics.gap, visible, auto);
        let union = if visible == 0 { title } else { group_paint::union_rect(shown, title, false) };
        Self { body: shown, full_body, title, union, below: edge.title_below(auto), rolled: visible == 0, metrics, dpi }
    }
}

/// Corral choice: Exact pixels survive lossy model projections. The
/// stamp invalidates this cache on intentional geometry edits by other features.
#[derive(Clone, Debug)]
struct RuntimeBody {
    requested: RequestedBody,
    centre_dpi: u32,
    monitor: String,
    projected: Rect,
}

impl RuntimeBody {
    fn matches(&self, group: &model::Group) -> bool {
        self.monitor.eq_ignore_ascii_case(&group.monitor) && self.projected == group.rect
    }
}

/// Metric DPI of one group: the interaction's governing DPI, else the one geometry resolves at rest
/// (the stored governing monitor's: user decision 2026-10-14, size/title/icons follow it together),
/// else the stored monitor's.
fn pick_metric_dpi(centre: Option<u32>, resolved: Option<u32>, identity: u32) -> u32 {
    centre.or(resolved).unwrap_or(identity)
}

/// Placement of a body under its GOVERNING monitor (user decision 2026-10-14: a Corral keeps its
/// DIP size across DPIs, so the stored monitor is the one whose DPI scales the whole rect).
/// `governing` names it (the stored monitor, or the one `dpi_transition::govern` picked during
/// an interaction); an unknown name falls back to the monitor under the body's centre. The
/// effective body adds the minimum at the governing DPI.
fn body_placement(requested: RequestedBody, live: &Snapshot, governing: Option<&str>,
    size_dip: Option<(i32, i32)>, minimum: impl Fn(u32) -> (i32, i32))
    -> Option<(dpi_transition::PlacementDecision, Rect)> {
    let r = requested.physical();
    let g = governing.and_then(|n| live.find(n))
        .or_else(|| live.nearest((r.left + r.width() / 2, r.top + r.height() / 2)))?;
    let effective = metrics::effective_body(r, minimum(g.dpi));
    Some((dpi_transition::place_governed(requested, effective, g, live, size_dip)?, effective))
}

/// Pure fitting adapter shared by runtime placement and offline tests.
fn fitted_body_placement(requested: RequestedBody, live: &Snapshot,
    d: &dpi_transition::DisplaySnapshot, preferred: (i32, i32), min: (i32, i32), title_px: i32)
    -> Option<(dpi_transition::PlacementDecision, Rect)> {
    let shown = dpi_transition::fit_size(preferred, d, min, title_px)?;
    let requested = dpi_transition::fit_body(requested, shown, d)?;
    body_placement(requested, live, Some(&d.device), Some(shown), |_| dpi_transition::fitting_minimum(d, min, title_px))
}

/// Corral policy: Enter below at the full-title boundary; leave only after
/// another full title height plus two measured gaps (normally 8 DIP). Outline does not change this boundary.
#[allow(clippy::too_many_arguments)]
fn title_below_for_body(pref: i32, last: i32, auto_adjust: bool, _outline: bool,
    body: Rect, monitor_top: i32, full_metrics: &group_paint::Metrics, dragging: bool) -> bool {
    if dragging && (0..=1).contains(&last) { return last == 1; }
    let threshold = i64::from(full_metrics.title_h) + i64::from(full_metrics.gap)
        + if last == 1 { i64::from(full_metrics.title_h) + 2 * i64::from(full_metrics.gap.max(1)) } else { 0 };
    let near_top = i64::from(body.top) - i64::from(monitor_top) <= threshold;
    model::title_below(pref, last, auto_adjust, near_top)
}

/// The side a Corral is drawn on: `title_below_for_body`, except that a strip pinned above a body with no
/// room for it above (the body at the top of the work area) is drawn below instead of off-screen,
/// whatever `TitleEdgeAutoAdjust` says (corral-painting-spec 2.4). The release path still pushes such a
/// body down to make room, so the stored side is unaffected.
#[allow(clippy::too_many_arguments)]
fn title_below_on_screen(pref: i32, last: i32, auto_adjust: bool, outline: bool,
    body: Rect, monitor_top: i32, full_metrics: &group_paint::Metrics, dragging: bool) -> bool {
    let no_room_above = i64::from(body.top) - i64::from(monitor_top)
        < i64::from(full_metrics.title_h) + i64::from(full_metrics.gap);
    title_below_for_body(pref, last, auto_adjust, outline, body, monitor_top, full_metrics, dragging)
        || (!dragging && no_room_above)
}

/// Corral policy: Latch the release side before DIP-grid rounding can
/// move the top-clamped body one pixel beyond its entry threshold. If the title
/// stays above, round its contained top inward onto the same persistence grid.
fn settle_title_release(requested: RequestedBody, display: &dpi_transition::DisplaySnapshot,
    size_dip: Option<(i32, i32)>, pref: i32, last: i32, auto_adjust: bool,
    metrics: &group_paint::Metrics) -> Option<(RequestedBody, bool)> {
    let below = title_below_for_body(pref, last, auto_adjust, false,
        requested.physical(), display.work.top, metrics, false);
    let mut settled = dpi_transition::settle_on_grid(requested, display, size_dip)?;
    let reserve = if below { 0 } else { metrics.title_h.max(0).checked_add(metrics.gap.max(0))? };
    if settled.physical().top < display.work.top.checked_add(reserve)? {
        let dip = dpi_transition::physical_to_dips(settled.physical(), display)?;
        let top = i32::try_from((i64::from(reserve) * 96 + i64::from(display.dpi) - 1)
            / i64::from(display.dpi)).ok()?;
        let delta = top.checked_sub(dip.top)?;
        settled = RequestedBody::from_dips(Rect::new(dip.left, top, dip.right, dip.bottom.checked_add(delta)?), display)?;
    }
    Some((settled, below))
}

#[cfg(test)]
mod title_side_tests {
    use super::*;

    #[test]
    fn a_strip_pinned_above_never_goes_off_screen_at_the_work_area_top() {
        for dpi in [96, 120, 144, 192, 240] {
            let m = group_paint::metrics(dpi);
            let top = -1080;
            for auto in [false, true] {
                // pinned above (pref 0), body flush with the top: the strip goes below
                assert!(title_below_on_screen(0, 0, auto, false, Rect::new(0, top, 400, top + 300), top, &m, false), "dpi {dpi}");
                assert!(title_below_on_screen(0, 0, auto, false, Rect::new(0, top + m.title_h - 1, 400, top + 300), top, &m, false));
                // with room for the whole strip it stays above
                assert!(!title_below_on_screen(0, 0, auto, false, Rect::new(0, top + m.title_h + m.gap, 400, top + 300), top, &m, false));
                // the release path is unchanged: it pushes the body down instead
                assert!(!title_below_for_body(0, 0, auto, false, Rect::new(0, top, 400, top + 300), top, &m, false));
            }
        }
    }

    #[test]
    fn q33n_bottom_restart_reanchors_on_each_monitors_work_area_and_dpi() {
        use dpi_transition::DisplaySnapshot;
        for dpi in [96,120,144,192,240] {
            for work in [Rect::new(-1920,-1080,0,-48),Rect::new(-1920,-1040,0,0),
                Rect::new(-1872,-1080,0,0),Rect::new(-1920,-1080,-48,0),Rect::new(-1920,-1080,0,0)] {
                let monitor=Monitor { name:"QA".into(),bounds:Rect::new(-1920,-1080,0,0),work,dpi };
                let display=DisplaySnapshot::from_monitor(&monitor,false,None);
                let metrics=group_paint::metrics(dpi);
                for auto in [false,true] {
                    for roll in [RollState::Docked,RollState::Manual,RollState::Expanded] {
                        for content in [model::GroupContent::Items { items:vec![] },model::GroupContent::Portal { path:"QA".into() }] {
                            let mut layout=Layout::new();
                            let id=layout.add_group("QA".into(),"QA".into(),Rect::new(10,34,210,234),content);
                            let g=layout.group_mut(id).unwrap();
                            g.set_dock_edge(Some(rollup::DockEdge::Bottom));g.roll=roll;
                            let physical=RequestedBody::from_dips(g.rect,&display).unwrap().physical();
                            let (_,_,body)=rollup::DockEdge::Bottom.layout(physical,work,metrics.title_h,metrics.gap,0,auto);
                            // Compose the real release grid projection and restart decode;
                            // work.bottom need not lie on a whole DIP coordinate.
                            let (settled,_)=settle_title_release(RequestedBody::from_physical(body).unwrap(),
                                &display,Some((200,200)),0,0,auto,&metrics).unwrap();
                            g.rect=dpi_transition::physical_to_dips(settled.physical(),&display).unwrap();
                            let loaded:Layout=serde_json::from_str(&serde_json::to_string(&layout).unwrap()).unwrap();
                            let g=&loaded.groups[0];
                            assert_eq!(g.dock_edge(),Some(rollup::DockEdge::Bottom));
                            let decoded=RequestedBody::from_dips(g.rect,&display).unwrap().physical();
                            let p=if roll==RollState::Expanded { rollup::OPEN } else { 0.0 };
                            let geo=GroupGeometry::edge_docked(decoded,work,metrics,dpi,p,g.dock_edge().unwrap(),auto);
                            assert_eq!(geo.full_body.height(),to_px(200,dpi));
                            assert_eq!(geo.full_body.bottom,work.bottom);
                            assert!(!geo.below);
                            if p==0.0 { assert_eq!(geo.union,geo.title);assert_eq!(geo.title.bottom,work.bottom); }
                            else { assert_eq!(geo.body,geo.full_body);assert_eq!(geo.title.bottom,geo.body.top-metrics.gap); }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn q33m_docked_setting_switches_layout_without_changing_progress_or_size() {
        let metrics = group_paint::metrics(144);
        let body = Rect::new(-800, 91, -200, 841);
        for progress in [0.0, 0.5, 0.32, 1.0, 0.8, 0.0] {
            let visible = rollup::visible_body_height(body.height(), progress);
            for auto in [true, false, true, false] {
                let geo = GroupGeometry::top_docked(body, 40, metrics, 144, progress, auto);
                assert_eq!(geo.below, auto);
                assert_eq!(geo.rolled, visible == 0);
                assert_eq!(geo.body.height(), visible);
                assert_eq!(geo.full_body.height(), body.height());
                assert_eq!(geo.union.top, 40);
                if auto {
                    assert_eq!((geo.body.top, geo.full_body.top), (40,40));
                    assert_eq!(geo.title.top, if visible == 0 { 40 } else { geo.body.bottom + metrics.gap });
                } else {
                    assert_eq!(geo.title.top, 40);
                    assert_eq!((geo.body.top, geo.full_body.top), (geo.title.bottom + metrics.gap, geo.title.bottom + metrics.gap));
                }
                if visible == 0 { assert_eq!(geo.union, geo.title); }
                else { assert_eq!(geo.union.bottom, geo.body.bottom.max(geo.title.bottom)); }
            }
        }
    }

    #[test]
    fn q33m_restart_uses_saved_fold_state_and_current_setting_for_items_and_gates() {
        for dpi in [96,120,144,192,240] {
            let metrics = group_paint::metrics(dpi);
            for auto in [false,true] {
                let settings = Settings { title_edge_auto_adjust: auto, ..Settings::default() };
                let settings: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
                let mut layout = Layout::new();
                let id = layout.add_group("QA".into(), "QA".into(), Rect::new(10,34,410,334),
                    model::GroupContent::Items { items: vec![] });
                for content in [model::GroupContent::Items { items: vec![] }, model::GroupContent::Portal { path: "QA".into() }] {
                    for roll in [RollState::Docked, RollState::Manual, RollState::Expanded] {
                        let group = layout.group_mut(id).unwrap();
                        group.content = content.clone(); group.dock_top = true; group.roll = roll;
                        // An older record can have an above-title body origin and last side.
                        group.title_side_last = 0;
                        let loaded: Layout = serde_json::from_str(&serde_json::to_string(&layout).unwrap()).unwrap();
                        let group = loaded.groups.iter().find(|g| g.id == id).unwrap();
                        assert!(group.dock_top);
                        let body = Rect::new(to_px(group.rect.left,dpi),to_px(group.rect.top,dpi),
                            to_px(group.rect.right,dpi),to_px(group.rect.bottom,dpi));
                        let progress = if group.roll == RollState::Expanded { rollup::OPEN } else { 0.0 };
                        let geo = GroupGeometry::top_docked(body, -1400, metrics, dpi, progress, settings.title_edge_auto_adjust);
                        assert_eq!(geo.below, auto);
                        assert_eq!(geo.full_body.height(),to_px(300,dpi));
                        if roll == RollState::Expanded {
                            assert_eq!(geo.body,geo.full_body);
                            assert_eq!(geo.body.top,if auto { -1400 } else { -1400+metrics.title_h+metrics.gap });
                            if auto { assert_eq!(geo.title.top,geo.body.bottom+metrics.gap); }
                        } else {
                            assert_eq!(geo.union,geo.title);
                            assert_eq!(geo.title.top,-1400);
                            assert_eq!(geo.body.height(),0);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn q33m_undocking_resumes_normal_title_hysteresis_for_both_settings() {
        for dpi in [96,120,144,192,240] {
            let metrics=group_paint::metrics(dpi);
            let top=-1400;
            let body=Rect::new(-800,top,-200,top+750);
            let enter=metrics.title_h+metrics.gap;
            let exit=enter+metrics.title_h+2*metrics.gap.max(1);
            for auto in [false,true] {
                for progress in [0.0,0.5,1.0] {
                    let geo=GroupGeometry::top_docked(body,top,metrics,dpi,progress,auto);
                    let (drag,below)=rollup::top_drag_body(geo.full_body,geo.title,auto,metrics.gap,top);
                    let moved=drag.offset(0,2);
                    // Once unlatched, keep the captured expanded-body side until release.
                    assert_eq!(title_below_for_body(-1,i32::from(below),auto,false,moved,top,&metrics,true),below);
                    let released=title_below_for_body(-1,i32::from(below),auto,false,moved,top,&metrics,false);
                    if !auto { assert!(!released); }
                    if auto && below {
                        assert!(released,"a fully opened bottom title stays below just after undock");
                        let band=body.offset(0,enter+1);
                        assert!(title_below_for_body(-1,1,true,false,band,top,&metrics,false));
                    }
                    let far=body.offset(0,exit+1);
                    assert!(!title_below_for_body(-1,i32::from(released),auto,false,far,top,&metrics,false));
                    // After the above transition, returning into the exit band cannot oscillate.
                    assert!(!title_below_for_body(-1,0,auto,false,body.offset(0,exit),top,&metrics,false));
                }
            }
        }
    }

    #[test]
    fn full_title_height_decides_side_at_each_dpi_and_monitor_origin() {
        for dpi in [96, 120, 144, 192, 240] {
            let full = group_paint::metrics(dpi);
            for monitor_top in [-1440, 0, 2160] {
                for outline in [false, true] {
                    let threshold = full.title_h + full.gap;
                    let body = Rect::new(100, monitor_top + threshold, 500, monitor_top + threshold + 200);
                    assert!(title_below_for_body(-1, 0, true, outline, body, monitor_top, &full, false));
                    assert!(!title_below_for_body(-1, 0, true, outline, body.offset(0, 1), monitor_top, &full, false));
                    // A hidden/hover-only title still uses the full-height threshold, rather than
                    // the zero-height rectangle which the resting painter exposes.
                    let hidden = group_paint::Metrics { title_h: 0, ..full };
                    assert!(!group_paint::near_monitor_top(body, monitor_top, &hidden));
                }
            }
        }
    }

    #[test]
    fn switching_off_keeps_an_existing_bottom_title_until_moved_away() {
        let m = group_paint::metrics(96);
        let top = Rect::new(100, 0, 500, 200);
        assert!(title_below_for_body(-1, 0, true, false, top, 0, &m, false));
        assert!(title_below_for_body(-1, 1, false, false, top, 0, &m, false));
        assert!(!title_below_for_body(-1, 1, false, false, top.offset(0, 100), 0, &m, false));
        assert!(!title_below_for_body(-1, 0, false, false, top, 0, &m, false));
        assert!(!title_below_for_body(0, 1, true, false, top, 0, &m, false));
        assert!(title_below_for_body(1, 0, false, false, top.offset(0, 100), 0, &m, false));
    }

    #[test]
    fn title_side_has_full_strip_hysteresis_and_waits_for_drag_end() {
        for dpi in [96, 120, 144, 192, 240] {
            let m = group_paint::metrics(dpi);
            for work_top in [-1440, 40, 2160] {
                let enter = m.title_h + m.gap;
                let exit = enter + m.title_h + 2 * m.gap.max(1);
                let body = |distance| Rect::new(100, work_top + distance, 500, work_top + distance + 200);
                // The above-title clamp itself is enough to reach the below transition.
                assert!(!title_below_for_body(-1, 0, true, false, body(enter), work_top, &m, true));
                assert!(title_below_for_body(-1, 0, true, false, body(enter), work_top, &m, false));
                for distance in enter..=exit {
                    assert!(title_below_for_body(-1, 1, true, false, body(distance), work_top, &m, false));
                }
                assert!(title_below_for_body(-1, 1, true, false, body(exit + 1), work_top, &m, true));
                assert!(!title_below_for_body(-1, 1, true, false, body(exit + 1), work_top, &m, false));
                // Returning into the hysteresis band after release stays above; no oscillation.
                assert!(!title_below_for_body(-1, 0, true, false, body(exit), work_top, &m, false));
            }
        }
    }

    #[test]
    fn top_clamp_release_settlement_keeps_side_reachable_and_title_contained() {
        for dpi in [96, 120, 144, 192, 240] {
            for work_top in [-1440, 40, 2160] {
                let d = dpi_transition::DisplaySnapshot { device: "QA".into(),
                    bounds: Rect::new(-1920, work_top, 1920, work_top + 2160),
                    work: Rect::new(-1920, work_top, 1920, work_top + 2160), dpi,
                    primary: true, hardware: Default::default() };
                let metrics = group_paint::metrics(dpi);
                let body = RequestedBody::from_dips(Rect::new(100, 100, 500, 300), &d).unwrap().physical();
                for resizing_top in [false, true] {
                    let candidate = if resizing_top { Rect::new(body.left, work_top - 200, body.right, body.bottom) }
                        else { body.offset(0, -2000) };
                    let clamped = rollup::clamp_top(candidate, false,
                        metrics.title_h, metrics.gap, work_top, resizing_top);
                    if resizing_top { assert_eq!(clamped.bottom, candidate.bottom); }
                    let request = RequestedBody::from_physical(clamped).unwrap();
                    let dip = dpi_transition::physical_to_dips(clamped, &d).unwrap();
                    for auto in [false, true] {
                        let (settled, below) = settle_title_release(request, &d, Some((dip.width(),dip.height())), -1, 0, auto, &metrics).unwrap();
                        assert_eq!(below, auto, "entry at {dpi} DPI, work top {work_top}, resize={resizing_top}");
                        assert_eq!(title_below_for_body(-1, i32::from(below), auto, false,
                            settled.physical(), work_top, &metrics, false), below, "settled side stays latched");
                        let title = group_paint::title_rect_on(settled.physical(), below, &metrics);
                        assert!(settled.physical().top >= work_top && title.top >= work_top);
                        let stored = dpi_transition::physical_to_dips(settled.physical(), &d).unwrap();
                        assert_eq!(RequestedBody::from_dips(stored, &d).unwrap(), settled, "restart reproduces contained pixels");
                    }
                }
            }
        }
    }

    #[test]
    fn release_side_latch_propagates_to_tab_common_placement() {
        let mut layout = Layout::new();
        let first = layout.add_group("First".into(), "QA".into(), Rect::new(100,100,500,300), model::GroupContent::Items { items: vec![] });
        let second = layout.add_group("Second".into(), "QA".into(), Rect::new(600,100,1000,300), model::GroupContent::Items { items: vec![] });
        layout.join_tabs(first, second).unwrap();
        let active = crate::tabs::collect(&layout.groups, first).unwrap().active;
        layout.group_mut(active).unwrap().title_side_last = 1;
        layout.sync_tab_common(active).unwrap();
        assert!(layout.groups.iter().all(|g| g.title_side_last == 1));
        layout.group_mut(active).unwrap().title_side_last = 0;
        layout.sync_tab_common(active).unwrap();
        assert!(layout.groups.iter().all(|g| g.title_side_last == 0));
    }
}

/// A cursor-defined user resize chooses a new preference, even when
/// fitting produces the same visible rectangle. Monitor-only moves never call it.
fn user_resize_preference(layout: &mut Layout, id: u64, body: RequestedBody,
    d: &dpi_transition::DisplaySnapshot) -> Option<((i32, i32), bool)> {
    let rect = dpi_transition::physical_to_dips(body.physical(), d)?;
    let preferred = (rect.width().max(1), rect.height().max(1));
    let g = layout.group_mut(id)?;
    let changed = g.preferred_size_dip != Some(preferred);
    g.preferred_size_dip = Some(preferred);
    Some((preferred, changed))
}

/// Positions and repeated runtime rebuilds do not change the fit log context.
fn record_fit_context(states: &mut HashMap<u64, (String, (i32, i32))>, id: u64,
    monitor: &str, shown: (i32, i32), preferred: (i32, i32)) -> bool {
    let context = (monitor.to_string(), shown);
    let changed = states.get(&id) != Some(&context);
    states.insert(id, context);
    changed && shown != preferred
}

fn store_body_placement(layout: &mut Layout, states: &mut HashMap<u64, RuntimeBody>, id: u64,
    p: dpi_transition::PlacementDecision) {
    let state = RuntimeBody { requested: p.requested, centre_dpi: p.effective_dpi,
        monitor: p.monitor.clone(), projected: p.rect_dip };
    let active = crate::tabs::collect(&layout.groups, id).map_or(id, |v| v.active);
    if let Some(g) = layout.group_mut(active) {
        g.preferred_size_dip.get_or_insert((g.rect.width().max(1), g.rect.height().max(1)));
        g.monitor = p.monitor; g.rect = p.rect_dip;
    }
    let _ = layout.sync_tab_common(active);
    let ids = crate::tabs::collect(&layout.groups, id).map(|v| v.members).unwrap_or_else(|| vec![id]);
    for member in ids { states.insert(member, state.clone()); }
}

/// The Corral a drawing drag would create, as its preview showed it (grid-snapping-spec 6).
struct CreationPreview {
    start: (i32, i32),
    end: (i32, i32),
    monitor: String,
    rect: creation::CreationRect,
}

/// One tab member as it was at the press of a resize: what Esc restores (grid-snapping-spec 3.1).
#[derive(Clone, Debug)]
struct MemberState {
    id: u64,
    rect: Rect,
    monitor: String,
    preferred: Option<(i32, i32)>,
    runtime: Option<RuntimeBody>,
}

/// The press-time model and runtime state of the resized Corral and its tab siblings.
struct ResizeUndo {
    id: u64,
    members: Vec<MemberState>,
}

fn capture_resize_state(layout: &Layout, states: &HashMap<u64, RuntimeBody>, id: u64) -> ResizeUndo {
    let ids = crate::tabs::collect(&layout.groups, id).map(|v| v.members).unwrap_or_else(|| vec![id]);
    let members = ids.into_iter().filter_map(|member| {
        let g = layout.groups.iter().find(|g| g.id == member)?;
        Some(MemberState { id: member, rect: g.rect, monitor: g.monitor.clone(), preferred: g.preferred_size_dip, runtime: states.get(&member).cloned() })
    }).collect();
    ResizeUndo { id, members }
}

/// Puts `rect`, `monitor`, `preferred_size_dip` and the exact runtime pixels back, bit for bit.
fn restore_resize_state(layout: &mut Layout, states: &mut HashMap<u64, RuntimeBody>, undo: &ResizeUndo) {
    for m in &undo.members {
        if let Some(g) = layout.group_mut(m.id) {
            g.rect = m.rect;
            g.monitor = m.monitor.clone();
            g.preferred_size_dip = m.preferred;
        }
        match &m.runtime {
            Some(state) => { states.insert(m.id, state.clone()); }
            None => { states.remove(&m.id); }
        }
    }
}

enum Drag {
    None,
    Pending { start: (i32, i32) },
    Band { start: (i32, i32), current: (i32, i32) },
    /// Title drag. `grab` = pointer offset from the icon-area origin at the press (parent-client
    /// px); `moved` = the model rect really changed.
    MoveGroup { id: u64, grab: dpi_transition::Grab, moved: bool,
        /// Side of the expanded drag body, independent of a docked animation's moving title.
        undock_below: bool },
    /// Edge/corner drag (grid-snapping-spec 3). `start` = icon area (screen px) and `pointer` = the
    /// pointer (screen px) at the press; every step is computed from these two, never from the last step.
    ResizeGroup { id: u64, zone: crate::snap::EdgeZone, pointer: (i32, i32), start: Rect, moved: bool },
    /// Dragging a desktop item; points in screen pixels. `grab` = cursor offset inside the cell.
    Item { name: String, start: (i32, i32), current: (i32, i32), grab: (i32, i32), active: bool, owner: HWND },
}

/// What kind of live gesture owns a Corral's surface right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Interaction {
    /// At rest (also a title/edge press that has not changed the geometry yet).
    Rest,
    /// The Corral window really moved.
    Move,
    /// The Corral really changed size.
    Resize,
    /// The preview window itself is being moved/sized by the system.
    PreviewMove,
}

impl Drag {
    /// Interaction state of group `group`. A press that never changed the model rectangle stays
    /// `Rest`: a plain click must neither flip the Corral to its approximation nor freeze a portal.
    fn interaction(&self, group: u64) -> Interaction {
        match self {
            Self::MoveGroup { id, moved: true, .. } if *id == group => Interaction::Move,
            Self::ResizeGroup { id, moved: true, .. } if *id == group => Interaction::Resize,
            _ => Interaction::Rest,
        }
    }

    fn surface_interactive(&self, group: u64) -> bool {
        self.interaction(group) != Interaction::Rest
    }
}

/// Stable Corral identities and parent-client physical pixels, frozen at title press.
#[derive(Clone, Copy, Debug)]
struct TabPress {
    /// The window that holds the capture; messages of the press are routed by it.
    owner: u64,
    /// The tab under the press (the owner when the press is not on a tab).
    member: u64,
    /// The Corral the move drag acts on: the owner, or the torn-off tab after a tear.
    moving: u64,
    press: (i32, i32),
    /// Shift was held at the press on a tab: reorder / tear-off (tabs-spec 5).
    shift_tab: bool,
    on_tab: bool,
    /// Left edge, centre and strip width of the pressed tab (parent-client px).
    tab_left: i32,
    tab_centre: i32,
    strip_width: i32,
    /// Movement counts only beyond the system drag threshold (presses on a tab, a folded Corral or a glyph).
    gated: bool,
    started: bool,
    page: i32,
    /// The press began on the Gate's Up button (with `portal_glyph`) rather than its folder glyph.
    portal_up: bool,
    portal_glyph: bool,
}

/// One title press as a transaction (tabs-spec 3.4, 5.3): what Esc or Cancel puts back.
struct TabGesture {
    undo: ResizeUndo,
    doc: crate::tabs::TabDoc,
    arm: crate::tabs::ArmState,
    /// The drop zone that has armed (feedback is up); a release combines into it.
    armed: Option<crate::tabs::ZoneHit>,
    /// Last pointer position (parent-client px), for the arming timer.
    client: (i32, i32),
    torn: bool,
    reordered: bool,
    /// The Corrals of the pressed set at the press: the only ones an undo touches.
    ids: Vec<u64>,
    /// The tab the crowded strip keeps in view while it is scrolled by a drag near its end.
    scroll_anchor: Option<usize>,
    last_scroll: Option<u64>,
}

struct App {
    opts: Options,
    pages: pages_glue::State,
    instance: HINSTANCE,
    parent: HWND,
    preview_host: Option<HWND>,
    /// Quick-hide state machine.
    quick_hide: crate::quick_hide::State,
    /// Clear view's state (hidden kind, click detector, idle watch, drop peek) and the foreground WinEvent
    /// hook (`app/quick_hide_glue.rs`).
    qh: crate::quick_hide::Machine,
    qh_fg_hook: Option<quick_hide_glue::FgHook>,
    /// Show-desktop hooks, settle schedule and QA trace (`app/showdesk.rs`).
    showdesk: showdesk::State,
    message_window: HWND,
    surface_hwnd: HWND,
    surface: Surface,
    host: Option<DesktopHost>,
    watchdog: Option<Child>,
    taskbar_created: u32,
    layout: Layout,
    monitors: Vec<Monitor>,
    display_snapshot: Option<Snapshot>,
    runtime_bodies: HashMap<u64, RuntimeBody>,
    fit_log_state: HashMap<u64, (String, (i32, i32))>,
    profile_pending: Option<profiles::Pending>,
    items: Vec<Item>,
    groups: Vec<GroupWindow>,
    drag: Drag,
    tab_press: Option<TabPress>,
    tab_gesture: Option<TabGesture>,
    /// The tab whose full title is shown as a tooltip (tabs-spec 4).
    tab_tip: Option<u64>,
    /// `save` is held back while a tear-off is written through `commit_tab_edit`.
    tab_defer_save: bool,
    /// Armed drop zone feedback: (target Corral, insertion index among its tabs for a set).
    tab_overlay: Option<(u64, Option<usize>)>,
    /// The dragged Corral shown at 70 % while a drop zone is armed.
    tab_fade: Option<u64>,
    tab_hover_state: crate::tabs::HoverState,
    tab_save_pending: bool,
    /// Unmodified settings exclusions; tab-inherited exemptions are a layout-owned overlay.
    tab_base_exclusions: Option<String>,
    /// The item drag while OLE owns it (between the hand-over and `finish_ole_drag`).
    ole_drag: Option<OleDrag>,
    naming: Option<(HWND, u64, HFONT)>,
    /// In-place rename of a desktop item: (edit window, parsing name, font).
    renaming: Option<(HWND, String, HFONT)>,
    /// Icon bitmap size and loose-icon spacing from Explorer's view (physical px).
    icon_px: i32,
    spacing: (i32, i32),
    /// Read-only desktop spacing and icon size in DIPs (grid-snapping-spec 2.2); cell inputs.
    desktop_metrics: crate::icon_metrics::DesktopMetrics,
    /// Snap targets held by the running move, resize or creation (grid-snapping-spec 4.3) and the
    /// alignment guide lines to draw while they hold.
    snap_lock: crate::snap::SnapLock,
    snap_guides: Vec<crate::snap::Guide>,
    creation_preview: Option<CreationPreview>,
    resize_undo: Option<ResizeUndo>,
    /// `CreateCorralWithMouseMode` (defaults to both buttons until the settings model exists).
    mouse_mode: creation::MouseMode,
    /// Right-button press point for the right-drag creation path.
    right_start: Option<(i32, i32)>,
    /// The draw gesture on the desktop surface (creation-spec 2.2) and the double-click bookkeeping around it.
    draw: creation::DrawState,
    settings: Settings,
    settings_stamp: Option<std::time::SystemTime>,
    /// Translucent drag image window (top-level, created on demand).
    ghost: Option<(HWND, Surface)>,
    /// Fold openness shown now (0 = title strip, 1 = open), the animation target (true = opening),
    /// the running tween and its timer gate per rolled group; the hot (hover-open) group; the
    /// 300 ms leave rule (motion-sorting-spec 3, 8).
    roll_progress: HashMap<u64, f64>,
    roll_target: HashMap<u64, bool>,
    roll_tween: HashMap<u64, crate::motion::Tween>,
    roll_gate: crate::motion::TimerGate,
    roll_leave: rollup::LeaveGrace,
    hot_group: Option<u64>,
    /// Group just rolled up under the pointer: no hover expand until the pointer has left it once
    /// (a rolled group opens on pointer *entry*).
    roll_hover_suppressed: Option<u64>,
    pending_hover: Option<u64>,
    /// Portal whose title glyph received the left-button press (acts on release).
    glyph_press: Option<u64>,
    /// Hover level per group (0..250 over 250 ms): drives the
    /// title darkening and the title-bar control alpha. `title_hot` = group under the pointer.
    title_hover: HashMap<u64, f64>,
    /// Measured titles per window (see `title_metrics_of`).
    title_cache: std::cell::RefCell<HashMap<u64, std::rc::Rc<paint::TitleMetrics>>>,
    title_hot: Option<u64>,
    title_clock: Option<Instant>,
    /// Title-bar control pressed (group id, true = close) - acts on release.
    control_press: Option<(u64, bool)>,
    /// Group whose full render is owed by the coalescing drag timer (`TIMER_DRAG_RENDER`).
    drag_render_pending: Option<u64>,
    /// Wallpaper-independent move surface already presented; reset on release/capture loss.
    drag_overlay: Option<u64>,
    preview_camouflage_moving: bool,
    /// OLE drop-target registrations per window (dropped before the window is destroyed).
    drop_registrations: HashMap<isize, dragdrop::Registration>,
    /// Where items dropped from Explorer should go once they appear (lower-case parsing name).
    pending_drops: HashMap<String, (PendingPlacement, Instant)>,
    /// Further item-reload delays (ms) queued after a Shell hand-over.
    quick_reloads: Vec<u32>,
    /// A desktop change notice armed the item-reload timer (throttle, see WM_SHELL_CHANGE).
    shell_change_armed: bool,
    /// When the item-reload timer last re-read the desktop (throttle gap for change notices).
    last_item_reload: Option<Instant>,
    /// When each desktop item last changed, for Round-up's two-second settle (round-up-spec 3).
    round_up_tracking: crate::round_up::RunTracker,
    /// The layout was copied before Round-up's first start with it (round-up-spec 6).
    round_up_layout_copied: bool,
    /// A background-menu command chosen inside a Corral body (New, Paste): the Corral, the items that
    /// existed before it, and when. The items that appear next go into that Corral.
    menu_new_target: Option<(u64, std::collections::HashSet<String>, Instant)>,
    /// Folder Portal groups: navigation state and the items of the folder currently shown.
    portals: HashMap<u64, PortalView>,
    /// Lower-case parsing name -> Gate, for the items Gates read from their folders (`App::group_of`).
    gate_index: HashMap<String, u64>,
    /// Scrollbar popup per scrollable Corral.
    scrollbars: HashMap<u64, crate::scrollbar::Bar>,
    /// True while `restack` runs (its own z changes must not re-trigger it).
    restacking: bool,
    /// Basic Peek ("Glance" mode): state, hotkey listener (`app/peek.rs`).
    peek: peek::Peek,
    /// Peek's "Show Corrals" taskbar button (`app/taskbar_item.rs`).
    taskbar: taskbar_item::State,
    /// Multi-selection bookkeeping (anchor, focus, deferred release effect, band origin).
    selection: selection::State,
    /// Backups engine (`app/snapshot_glue.rs`): store, cursor gate, last daily-capture error.
    snap: snapshot_glue::State,
    style_last_nonce: Option<u64>,
    /// The draft overlay of the settings window's open colour session (colour-state-spec 3.2).
    color_overlay: crate::color_state::overlay::OverlayHost,
    color_preview_fast: bool,
    color_desktop_active: bool,
    /// Ctrl + wheel icon zoom: wheel delta carried between messages (high-resolution wheels).
    zoom_wheel_carry: i32,
}

/// A drawn Folder Gate's listing and navigation state (folder-gate-host-spec 4).
struct PortalView {
    root: String,
    /// Folder shown (mirror of the core's current entry).
    current: String,
    /// Navigation and pending probe target.
    core: Rc<RefCell<gate_host::Core>>,
    /// A Back or Forward step taken on the cursor whose browse is in flight (undone if it fails).
    step: Option<portal::Step>,
    /// Sequence number of the latest folder probe, and whether an answer is still awaited.
    probe_seq: u64,
    probe_pending: bool,
    /// Latest background folder read; invalidated before every navigation or probe.
    read_seq: u64,
    /// The folder could not be shown: the error body (folder-gate-host-spec 3.3) and the folder it names.
    error: Option<(portal::Probe, String)>,
    /// The folder's items have been read at least once (until then the body shows "Opening folder...").
    loaded: bool,
    /// Change notification registration for the folder shown (0 = none).
    notify: u32,
}

/// Placement requested by an external drop, applied when the shell has created the item.
#[derive(Clone, Copy, Debug)]
enum PendingPlacement {
    Group(u64),
    /// Parent-client cell origin for a loose item.
    Loose((i32, i32), i32),
}

/// Which Corral window received an external drop.
#[derive(Clone, Copy)]
enum DropSite {
    Surface,
    Group(u64),
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    /// Set by the item-drag hand-over, consumed by `window_proc` after the `APP` borrow ended.
    static PENDING_OLE_DRAG: RefCell<Option<OleDragRequest>> = const { RefCell::new(None) };
    /// `DoDragDrop` of a handed-over icon drag is running (blocks a second hand-over even if a
    /// stale `App::ole_drag` was left behind).
    static OLE_DRAG_RUNNING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The shell menu currently shown; its owner window forwards menu messages to it while APP is
    /// borrowed by the modal TrackPopupMenuEx loop.
    static ACTIVE_MENU: RefCell<Option<Rc<ShellMenu>>> = const { RefCell::new(None) };
}

/// Opt-in diagnostics: appends to the file named by `CORRAL_LOG`.
pub fn log(message: &str) {
    if let Some(path) = std::env::var_os("CORRAL_LOG") {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{message}");
        }
    }
}

fn qa_hover() -> bool {
    std::env::var("CORRAL_QA_HOVER").is_ok_and(|v| v == "1")
}

fn lparam_point(lparam: LPARAM) -> (i32, i32) {
    ((lparam.0 & 0xffff) as i16 as i32, ((lparam.0 >> 16) & 0xffff) as i16 as i32)
}

/// Debounce of the desktop re-enumeration after a shell command or change notice. While a drop waits for
/// its item to appear (e.g. portal -> Corral), re-read sooner: the fixed 1 s made such drops land visibly
/// late. Display timing only; no file operation depends on it.
fn reload_delay_ms(drop_pending: bool) -> u32 {
    if drop_pending { 150 } else { 1000 }
}

fn scale(value: i32, dpi: u32) -> i32 {
    to_px(value, dpi)
}

fn dip_to_px_i32(dip: i32, dpi: u32) -> i32 {
    to_px(dip, dpi)
}

unsafe extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_COPYDATA { return style_glue::receive(hwnd, lparam); }
    if msg == style_glue::WM_APPLY_COLOR { style_glue::dispatch(); return LRESULT(0); }
    style_glue::preview_changed(hwnd, msg, lparam);
    if matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
        let forwarded = ACTIVE_MENU.with(|cell| cell.borrow().as_ref().and_then(|m| m.handle_menu_msg(msg, wparam, lparam)));
        if let Some(result) = forwarded {
            return result;
        }
    }
    if msg == WM_WINDOWPOSCHANGING {
        peek::guard_surface_zorder(hwnd, lparam); // before the APP borrow; no-op unless Peek is on
        zguard::guard(hwnd, lparam); // desktop mode: activation must not lift the surface/groups/portals
    }
    let handled = APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().and_then(|app| {
            let result = app.handle(hwnd, msg, wparam, lparam);
            style_glue::context(app);
            result
        }),
        Err(_) => None,
    });
    // A Gate's folder listing or probe answer arriving inside a modal loop (a menu) while `APP` is borrowed
    // owns a boxed payload: keep it and hand it over on a short timer instead of dropping it.
    if handled.is_none() && matches!(msg, portals::WM_GATE_ITEMS | crate::portal::WM_PORTAL_PROBE) && lparam.0 != 0
        && APP.with(|cell| cell.try_borrow_mut().is_err()) {
        portals::defer_gate_message(hwnd, msg, lparam);
        return LRESULT(0);
    }
    style_glue::post_pending(true);
    run_pending_ole_drag();
    match handled {
        Some(result) => result,
        None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Runs the OLE drag recorded by the item-drag hand-over, outside any `APP` borrow. The left
/// button is still held (the hand-over checked it in the same `WM_MOUSEMOVE`), as `DoDragDrop`
/// requires. The modal loop dispatches window messages, timers and the drop-target callbacks back
/// into `window_proc`, which work normally because `APP` is free and `App::drag` is `Drag::None`
/// (nothing in those paths starts a drag; `App::ole_drag` also blocks a second hand-over).
fn run_pending_ole_drag() {
    // only from the outermost `window_proc` frame: a nested call (e.g. WM_CAPTURECHANGED while the
    // outer handler still holds `APP`) must leave the request for the outer one
    if APP.with(|cell| cell.try_borrow_mut().is_err()) {
        return;
    }
    let Some(request) = PENDING_OLE_DRAG.with(|cell| cell.borrow_mut().take()) else { return };
    log(&format!("ole drag: begin {:?}", request.names));
    OLE_DRAG_RUNNING.with(|c| c.set(true));
    let result = dragdrop::begin_drag_as(request.owner, &request.names, request.image, request.internal);
    OLE_DRAG_RUNNING.with(|c| c.set(false));
    APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => {
            if let Some(app) = guard.as_mut() {
                app.finish_ole_drag(result);
            }
        }
        Err(_) => log("ole drag: finished while the app state was borrowed; outcome dropped"),
    });
}

fn register_class(instance: HINSTANCE, name: PCWSTR) {
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS,
        lpfnWndProc: Some(window_proc),
        hInstance: instance,
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
        lpszClassName: name,
        ..Default::default()
    };
    unsafe { RegisterClassExW(&class) };
}

/// Offer Gate navigation keys before Range switching and Corral accelerators.
fn pre_translate(msg: &MSG) -> bool {
    // Never feed input to a hidden tab.
    if APP.with(|cell| cell.try_borrow().ok().is_some_and(|g| g.as_ref().is_some_and(|app| app.inactive_tab_window(msg.hwnd))))
        && matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP | WM_CHAR | WM_MOUSEWHEEL | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_RBUTTONDOWN | WM_RBUTTONUP) { return true }
    // Alt+Left/Right while a Gate has focus belong to the Gate, before Range switching (folder-gate-host-spec 2.5).
    if matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_APPCOMMAND)
        && APP.with(|cell| cell.try_borrow_mut().ok().is_some_and(|mut g| g.as_mut().is_some_and(|app| app.portal_key(msg)))) {
        return true;
    }
    if APP.with(|cell| cell.try_borrow_mut().ok().and_then(|mut g| g.as_mut().and_then(|app| app.handle_pages_input(msg.hwnd, msg.message, msg.wParam, msg.lParam))).is_some()) { return true; }
    if matches!(msg.message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
        // The wheel goes to the focus window when Windows' "Scroll inactive windows" is off
        // (MouseWheelRouting 0, the user's machine): re-target it to the Corral window under the
        // cursor, e.g. a portal list the user has not clicked into (live 2026-10-13). Horizontal
        // too: touchpad Range swipes are counted only on the copy under the cursor (live 2026-10-28).
        let pt = POINT { x: (msg.lParam.0 & 0xffff) as i16 as i32, y: ((msg.lParam.0 >> 16) & 0xffff) as i16 as i32 };
        let under = unsafe { WindowFromPoint(pt) };
        if !under.is_invalid() && under != msg.hwnd && unsafe { GetWindowThreadProcessId(under, None) == windows::Win32::System::Threading::GetCurrentThreadId() } {
            return unsafe { PostMessageW(under, msg.message, msg.wParam, msg.lParam) }.is_ok();
        }
        // The group window scrolls and zooms its items; Alt+wheel belongs to Range switching.
        return false;
    }
    if !matches!(msg.message, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP | WM_CHAR) {
        return false;
    }
    if msg.message == WM_KEYDOWN && msg.wParam.0 == windows::Win32::UI::Input::KeyboardAndMouse::VK_ESCAPE.0 as usize
        && APP.with(|cell| cell.try_borrow_mut().ok().is_some_and(|mut g| g.as_mut().is_some_and(|app| app.peek_escape(msg.hwnd)))) {
        return true;
    }
    false
}

pub fn run(opts: Options) -> i32 {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
    if let Err(e) = dragdrop::init_ole() {
        log(&format!("OleInitialize failed: {e}"));
    }
    let stored = crate::settings::load_with_backup(&opts.settings_path);
    crate::portal_menu::set_hidden_entries(&stored.hidden_shell_menu_entries);
    let blocked = crate::shell_guard::block_extensions(&stored.blocked_shell_extensions);
    log(&format!("shell extensions refused in-process: {blocked:?}"));
    log(&format!("menu theme (SetPreferredAppMode): {:?}", crate::menu_theme::apply()));
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None) }.map(|m| HINSTANCE(m.0)).unwrap_or_default();
    for class in [w!("CorralPreview"), w!("CorralSurface"), w!("CorralGroup"), w!("CorralMessage"), w!("CorralGhost"), w!("CorralTaskbarItem")] {
        register_class(instance, class);
    }
    let app = match App::create(opts, instance) {
        Ok(app) => app,
        Err(message) => {
            eprintln!("corral: {message}");
            return 1;
        }
    };
    style_glue::context(&app);
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    APP.with(|cell| {
        if let Some(app) = cell.borrow_mut().as_mut() {
            app.refresh_all();
            app.pages_init();
        }
    });
    let mut msg = MSG::default();
    log("run: message loop start");
    unsafe {
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            if pre_translate(&msg) {
                continue;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    log(&format!("run: message loop end wparam={}", msg.wParam.0));
    // The wallpaper worker must not post to the message window once it is going away.
    crate::tint::set_notify_window(0);
    let app = APP.with(|cell| cell.borrow_mut().take());
    if let Some(app) = app {
        // Flush selection/order/membership even when orderly exit beats the 2 s debounce.
        app.save();
        app.shutdown();
    }
    crate::render::release_factories();
    log("run: shutdown complete");
    0
}

impl App {
    fn create(opts: Options, instance: HINSTANCE) -> Result<App, String> {
        let (parent, preview_host, host) = match opts.mode {
            Mode::Preview => {
                // Preview simulates a 1280x760-DIP monitor at the primary monitor's scale.
                let primary = desktop::monitors().into_iter().next();
                let dpi = primary.as_ref().map(|m| m.dpi).unwrap_or(96);
                let (max_w, max_h) = primary.map(|m| (m.work.width(), m.work.height())).unwrap_or((1280, 760));
                let (pw, ph) = (to_px(1280, dpi).min(max_w), to_px(760, dpi).min(max_h));
                let hwnd = unsafe {
                    CreateWindowExW(WINDOW_EX_STYLE(0), w!("CorralPreview"), w!("Corral preview"), WS_OVERLAPPEDWINDOW | WS_VISIBLE | WS_CLIPCHILDREN,
                        CW_USEDEFAULT, CW_USEDEFAULT, pw, ph, HWND::default(), HMENU::default(), instance, None)
                }
                .map_err(|e| format!("preview window: {e}"))?;
                crate::app_icon::set_for_dpi(hwnd, unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) });
                (hwnd, Some(hwnd), None)
            }
            Mode::Desktop => {
                let host = desktop::find_host().ok_or("Explorer desktop host not found")?;
                (host.progman, None, Some(host))
            }
        };
        let message_window = unsafe {
            CreateWindowExW(WS_EX_TOOLWINDOW, w!("CorralMessage"), w!("Corral"), WS_POPUP, 0, 0, 0, 0, HWND::default(), HMENU::default(), instance, None)
        }
        .map_err(|e| format!("message window: {e}"))?;
        // wallpaper-blur-spec 6: the wallpaper worker repaints through this window.
        crate::tint::set_notify_window(message_window.0 as isize);
        let (width, height) = client_size(parent);
        let surface_hwnd = create_layer(w!("CorralSurface"), parent, host.is_some(), Rect::new(0, 0, width, height), HWND::default(), instance)
            .map_err(|e| format!("surface window: {e}"))?;
        let surface = Surface::new(width.max(1) as u32, height.max(1) as u32).map_err(|e| format!("surface: {e}"))?;
        let watchdog = match &host {
            Some(h) => {
                let args = watchdog::Args {
                    parent_pid: std::process::id(),
                    listview: h.listview.0 as usize,
                    explorer_pid: h.explorer_pid,
                    was_visible: h.listview_was_visible,
                };
                let child = watchdog::spawn(&args).map_err(|e| format!("watchdog: {e}"))?;
                desktop::hide_listview(h);
                Some(child)
            }
            None => None,
        };
        let taskbar_created = unsafe { RegisterWindowMessageW(w!("TaskbarCreated")) };
        unsafe {
            let _ = RegisterHotKey(message_window, HOTKEY_QUIT, MOD_CONTROL | MOD_ALT | MOD_SHIFT, 'Q' as u32);
            if host.is_some() {
                SetTimer(message_window, TIMER_REASSERT, 2000, None);
            }
            SetTimer(message_window, TIMER_SETTINGS, 2000, None);
            register_desktop_notifications(message_window);
            if let Some(seconds) = opts.seconds {
                SetTimer(message_window, TIMER_EXIT, seconds * 1000, None);
            }
            let _ = SetWindowPos(surface_hwnd, HWND_TOP, 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }
        let mut app = App {
            pages: pages_glue::State::default(),
            opts,
            instance,
            parent,
            preview_host,
            quick_hide: crate::quick_hide::State::new(false),
            qh: crate::quick_hide::Machine::default(),
            qh_fg_hook: None,
            showdesk: showdesk::State::default(),
            message_window,
            surface_hwnd,
            surface,
            host,
            watchdog,
            taskbar_created,
            layout: Layout::new(),
            monitors: Vec::new(),
            display_snapshot: None,
            runtime_bodies: HashMap::new(),
            fit_log_state: HashMap::new(),
            profile_pending: None,
            items: Vec::new(),
            groups: Vec::new(),
            drag: Drag::None,
            tab_press: None,
            tab_gesture: None,
            tab_tip: None,
            tab_defer_save: false,
            tab_overlay: None,
            tab_fade: None,
            tab_hover_state: crate::tabs::HoverState::default(),
            tab_save_pending: false,
            tab_base_exclusions: None,
            ole_drag: None,
            naming: None,
            renaming: None,
            icon_px: 0,
            spacing: (0, 0),
            desktop_metrics: Default::default(),
            snap_lock: Default::default(),
            snap_guides: Vec::new(),
            creation_preview: None,
            resize_undo: None,
            mouse_mode: creation::MouseMode::default(),
            right_start: None,
            draw: creation::DrawState::Idle,
            settings: Settings::default(),
            settings_stamp: None,
            ghost: None,
            roll_progress: HashMap::new(),
            roll_target: HashMap::new(),
            roll_tween: HashMap::new(),
            roll_gate: crate::motion::TimerGate::new(TIMER_ANIMATE),
            roll_leave: rollup::LeaveGrace::default(),
            hot_group: None,
            roll_hover_suppressed: None,
            pending_hover: None,
            glyph_press: None,
            title_hover: HashMap::new(),
            title_cache: Default::default(),
            title_hot: None,
            title_clock: None,
            control_press: None,
            drag_render_pending: None,
            drag_overlay: None,
            preview_camouflage_moving: false,
            drop_registrations: HashMap::new(),
            pending_drops: HashMap::new(),
            quick_reloads: Vec::new(),
            shell_change_armed: false,
            last_item_reload: None,
            round_up_tracking: Default::default(),
            round_up_layout_copied: false,
            menu_new_target: None,
            portals: HashMap::new(),
            gate_index: HashMap::new(),
            scrollbars: HashMap::new(),
            restacking: false,
            peek: peek::Peek::default(),
            taskbar: taskbar_item::State::default(),
            selection: selection::State::default(),
            snap: snapshot_glue::State::default(),
            style_last_nonce: None,
            color_overlay: Default::default(),
            color_preview_fast: false,
            color_desktop_active: false,
            zoom_wheel_carry: 0,
        };
        app.snapshot_init(); // before the loads: finishes an interrupted restore on disk
        app.display_snapshot = app.current_display_snapshot();
        app.monitors = app.display_snapshot.as_ref().map(hosting::snapshot_monitors).unwrap_or_default();
        app.icon_px = scale(style::ICON_PX, app.dpi());
        app.reload_settings();
        app.layout = app.load_layout();
        app.prepare_round_up();
        app.profile_startup(true);
        app.load_items();
        app.rebuild_runtime_bodies();
        app.clamp_profile_titles();
        app.acknowledge_desk_display();
        if std::env::var("CORRAL_QA_APPLY_RULES").is_ok_and(|v| v == "1") {
            app.apply_rules_now(); // QA trigger of "Apply rules now"
        }
        app.consume_rules_requests();
        app.apply_tab_exclusions();
        app.consume_snapshot_request(true);
        app.consume_style_request(true);
        crate::motion::refresh_system();
        app.quick_hide_init();
        app.showdesk_init();
        app.peek_init();
        Ok(app)
    }

    fn load_layout(&self) -> Layout {
        if let Ok(mut layout) = model::load_with_backup(&self.opts.layout_path) {
            for g in &mut layout.groups {g.style.migrate_tint_source(self.settings.color,self.settings.desktop_tint_icons_color);}
            return layout;
        }
        Layout::new()
    }

    fn save(&self) {
        // desk-setups-spec 3.2: publish a user gesture once after its quiet period.
        // this deferral can lose one edit if the process dies inside the 500 ms quiet period.
        if crate::desk_setup::defers_save(self.layout.desk_setups.runtime.edit_pending, self.profile_pending.is_some(), self.profiles_enabled()) { return; }
        if matches!(self.drag, Drag::MoveGroup { .. } | Drag::ResizeGroup { .. }) || self.tab_defer_save { return; }
        if !self.snapshot_may_save_layout() {
            return;
        }
        // `layout_to_save` never contains an unfinished colour draft.
        let mut layout = self.layout_to_save().into_owned();
        for g in &mut layout.groups {g.style.migrate_tint_source(self.settings.color,self.settings.desktop_tint_icons_color);}
        if let Err(e) = model::save(&self.opts.layout_path, &layout) {
            eprintln!("corral: save failed: {e}");
        }
    }

    /// Screen position of the parent's client origin.
    fn origin(&self) -> (i32, i32) {
        let mut point = POINT::default();
        unsafe {
            let _ = ClientToScreen(self.parent, &mut point);
        }
        (point.x, point.y)
    }

    fn dpi(&self) -> u32 {
        self.monitors.first().map(|m| m.dpi).unwrap_or(96)
    }

    fn monitor_for(&self, name: &str) -> Option<&Monitor> {
        self.monitors.iter().find(|m| m.name.eq_ignore_ascii_case(name)).or(self.monitors.first())
    }

    fn runtime_body(&self, group: &model::Group) -> Option<&RuntimeBody> {
        self.runtime_bodies.get(&group.id).filter(|r| r.matches(group))
    }

    /// Interaction-only governing DPI of the dragged tab set. `group_metrics_dpi` prefers it
    /// and otherwise resolves the same effective-body centre DPI that geometry uses at rest.
    fn group_centre_dpi(&self, id: u64) -> Option<u32> {
        let owner = match self.drag {
            Drag::MoveGroup { id, .. } | Drag::ResizeGroup { id, .. } => id,
            _ => return None,
        };
        let group = self.layout.groups.iter().find(|g| g.id == id)?;
        let set = crate::tabs::collect(&self.layout.groups, owner)?;
        if !set.members.contains(&id) { return None; }
        Some(self.runtime_body(group)?.centre_dpi)
    }

    fn requested_body(&self, group: &model::Group) -> Option<RequestedBody> {
        self.runtime_body(group).map(|r| r.requested).or_else(|| {
            let snapshot = self.display_snapshot.as_ref()?;
            let d = snapshot.find(&group.monitor).or_else(|| snapshot.displays().first())?;
            RequestedBody::from_dips(group.rect, d)
        })
    }

    fn placement_for(&self, id: u64, requested: RequestedBody)
        -> Option<(dpi_transition::PlacementDecision, Rect)> {
        self.placement_in(id, requested, None, None)
    }

    /// `governing` None = the group's stored monitor. `size_dip` fixes the persisted DIP size.
    fn placement_in(&self, id: u64, requested: RequestedBody, governing: Option<&str>,
        size_dip: Option<(i32, i32)>) -> Option<(dpi_transition::PlacementDecision, Rect)> {
        let group = self.layout.groups.iter().find(|g| g.id == id)?;
        let live = self.display_snapshot.as_ref()?;
        let r = requested.physical();
        let d = governing.or(Some(group.monitor.as_str())).and_then(|n| live.find(n))
            .or_else(|| live.nearest((r.left + r.width()/2, r.top + r.height()/2)))?;
        let preferred = size_dip.unwrap_or_else(|| self.preferred_size(id));
        let metrics = self.group_title_metrics(group, d.dpi);
        let r = requested.physical();
        let min = self.min_body_px(d.dpi, Some((r.width(), r.height())));
        fitted_body_placement(requested, live, d, preferred, min, metrics.title_h + metrics.gap)
    }

    fn preferred_size(&self, id: u64) -> (i32, i32) {
        let g = self.layout.groups.iter().find(|g| g.id == id).unwrap();
        g.preferred_size_dip.filter(|(w,h)| *w > 0 && *h > 0)
            .unwrap_or((g.rect.width().max(1), g.rect.height().max(1)))
    }

    fn fitted_size(&self, id: u64, preferred: (i32, i32), d: &dpi_transition::DisplaySnapshot)
        -> Option<(i32, i32)> {
        let g = self.layout.groups.iter().find(|g| g.id == id)?;
        let metrics = self.group_title_metrics(g, d.dpi);
        let min = self.min_body_px(d.dpi, Some((dip_to_px_i32(preferred.0, d.dpi), dip_to_px_i32(preferred.1, d.dpi))));
        dpi_transition::fit_size(preferred, d, min, metrics.title_h + metrics.gap)
    }

    /// Capture absent preferences before profile transforms overwrite displayed rects.
    fn remember_preferred_sizes(&mut self) {
        for g in &mut self.layout.groups {
            if g.preferred_size_dip.is_none_or(|(w,h)| w <= 0 || h <= 0) {
                g.preferred_size_dip = Some((g.rect.width().max(1), g.rect.height().max(1)));
            }
        }
    }

    /// Write the governing monitor and all four projected edges together; never carry the
    /// old monitor's DIP extent into a rebase. Tab members share exact pixels.
    fn apply_body_placement(&mut self, id: u64, requested: RequestedBody) -> bool {
        self.apply_placement(id, requested, None, None)
    }

    fn apply_placement(&mut self, id: u64, requested: RequestedBody, governing: Option<&str>,
        size_dip: Option<(i32, i32)>) -> bool {
        let Some((p, effective)) = self.placement_in(id, requested, governing, size_dip) else { return false };
        let Some(group) = self.layout.groups.iter().find(|g| g.id == id) else { return false };
        let changed = self.requested_body(group) != Some(p.requested)
            || group.monitor != p.monitor || group.rect != p.rect_dip
            || self.runtime_body(group).is_none_or(|r| r.centre_dpi != p.effective_dpi);
        if changed { log(&format!("dpi placement id={id} governing={} centre={} dpi={} requested={:?} effective={effective:?} dip={:?}",
            p.monitor, p.centre_monitor, p.effective_dpi, requested.physical(), p.rect_dip)); }
        let preferred = size_dip.unwrap_or_else(|| self.preferred_size(id));
        let shown = (p.rect_dip.width(), p.rect_dip.height());
        if record_fit_context(&mut self.fit_log_state, id, &p.monitor, shown, preferred) {
            log(&format!("fit: group {id} preferred=({},{})dip shown=({},{})dip on {}",
                preferred.0, preferred.1, shown.0, shown.1, p.monitor));
        }
        store_body_placement(&mut self.layout, &mut self.runtime_bodies, id, p);
        changed
    }

    fn rebuild_runtime_bodies(&mut self) {
        self.remember_preferred_sizes();
        self.runtime_bodies.clear();
        let bodies: Vec<_> = self.layout.groups.iter().filter(|g| crate::tabs::body_visible(&self.layout.groups, g))
            .filter_map(|g| Some((g.id, self.requested_body(g)?))).collect();
        for (id, requested) in bodies { self.apply_body_placement(id, requested); }
    }

    /// The single metric-DPI decision shared by geometry, hit-testing, painting and rasters.
    /// `resolved` is the effective-body centre DPI from `placement_for`; `identity` is the
    /// stored monitor's DPI, used only when no placement could be resolved.
    fn metric_dpi(&self, id: u64, resolved: Option<u32>, identity: u32) -> u32 {
        pick_metric_dpi(self.group_centre_dpi(id), resolved, identity)
    }

    /// Title strip metrics: one height for ordinary titles and tab strips (corral-painting-spec 2.2).
    fn group_title_metrics(&self, _group: &model::Group, dpi: u32) -> group_paint::Metrics {
        group_paint::metrics(dpi)
    }

    /// Group geometry in parent-client pixels. The model rect is the icon area;
    /// the title strip sits above it, or below near the monitor top.
    fn group_geometry(&self, group: &model::Group) -> GroupGeometry {
        self.group_geometry_with(group, true)
    }

    /// The geometry of the stored body, without the display lift of a nearly empty body (ranges-dock-clearview-spec
    /// 4). Everything that writes a body back to the layout measures with this one.
    fn group_geometry_stored(&self, group: &model::Group) -> GroupGeometry {
        self.group_geometry_with(group, false)
    }

    fn group_geometry_with(&self, group: &model::Group, lift: bool) -> GroupGeometry {
        let fallback = self.monitors.first().cloned();
        let m = self.monitor_for(&group.monitor).cloned().or(fallback);
        let (ox, oy) = self.origin();
        let (identity_dpi, work_left, work_top, _monitor_top) = match &m {
            Some(m) => (m.dpi, m.work.left, m.work.top, m.bounds.top),
            None => (96, 0, 0, 0),
        };
        let mut body = self.requested_body(group).map(|r| r.physical().offset(-ox, -oy)).unwrap_or_else(|| Rect::new(
            work_left + to_px(group.rect.left, identity_dpi) - ox,
            work_top + to_px(group.rect.top, identity_dpi) - oy,
            work_left + to_px(group.rect.right, identity_dpi) - ox,
            work_top + to_px(group.rect.bottom, identity_dpi) - oy,
        ));
        let resolved = self.requested_body(group).and_then(|r| self.placement_for(group.id, r));
        let dpi = self.metric_dpi(group.id, resolved.as_ref().map(|(p, _)| p.effective_dpi), identity_dpi);
        if let Some((_, effective)) = resolved { body = effective.offset(-ox, -oy); }
        if !crate::tabs::body_visible(&self.layout.groups, group) {
            // Keep the saved full body, but expose no visible geometry. Page animation's
            // compositor also uses these rectangles, including when capturing a portal view.
            let empty = Rect::new(body.left, body.top, body.left, body.top);
            return GroupGeometry { body: empty, full_body: body, title: empty, union: empty,
                below: false, rolled: true, metrics: group_paint::metrics(dpi), dpi };
        }
        let mut metrics = self.group_title_metrics(group, dpi);
        // Corral policy: Preserve the press-time side throughout a captured move/
        // resize. Re-evaluate on release against the work area, using the full measured strip.
        let dragging = matches!(self.drag, Drag::MoveGroup { id, .. } | Drag::ResizeGroup { id, .. } if id == group.id);
        let below = if let Some(edge) = group.dock_edge() { edge.title_below(self.settings.title_edge_auto_adjust) } else {
            title_below_on_screen(group.title_side, group.title_side_last,
                self.settings.title_edge_auto_adjust, self.settings.outline, body, work_top - oy, &metrics, dragging)
        };
        // A nearly empty body is shown at the minimum open height (ranges-dock-clearview-spec 4): the picture
        // and hit-testing only, the stored rect and preferred size stay as they are.
        let body = if dragging || !lift { body } else { rollup::lifted_body(body, below, dpi) };
        let rolled = group.roll != RollState::Expanded;
        // The strip is reserved for Always and On hover (nothing shifts when it appears); Never has none,
        // but a folded or docked Corral always has its strip (corral-painting-spec 2.4, 2.5).
        let show_label = group_paint::strip_reserved(self.settings.label_mode, rolled) || group.dock_edge().is_some();
        if !show_label {
            metrics.title_h = 0;
        }
        if self.settings.square_edges {
            metrics.radius = 0;
        }
        if let Some(edge) = group.dock_edge() {
            let progress = self.roll_progress.get(&group.id).copied()
                .unwrap_or(if rolled { 0.0 } else { rollup::OPEN });
            let work = m.as_ref().map_or(body, |m| m.work.offset(-ox, -oy));
            return GroupGeometry::edge_docked(body, work, metrics, dpi, progress, edge, self.settings.title_edge_auto_adjust);
        }
        let title = group_paint::title_rect_on(body, below, &metrics);
        if rolled {
            // Rolled: body height animates with the hover progress, anchored to the title side;
            // a docked strip sits flush with the screen edge.
            let work = match &m {
                Some(m) => m.work.offset(-ox, -oy),
                None => body,
            };
            let progress = self.roll_progress.get(&group.id).copied().unwrap_or(0.0);
            let visible = rollup::visible_body_height(body.height(), progress);
            let docked = group.roll == RollState::Docked;
            let (strip, body_vis) = rollup::rolled_layout(body, title, !below, docked, work, metrics.gap, visible);
            let union = if visible == 0 { strip } else { group_paint::union_rect(body_vis, strip, false) };
            return GroupGeometry { body: body_vis, full_body: body, title: strip, union, below, rolled: visible == 0, metrics, dpi };
        }
        let union = if !show_label && self.settings.label_mode == 0 { body } else { group_paint::union_rect(body, title, rolled) };
        GroupGeometry { body, full_body: body, title, union, below, rolled, metrics, dpi }
    }

    /// CORRAL_LOG: every input of each Corral's title-side decision (`group_geometry`).
    fn log_title_sides(&self) {
        let (_, oy) = self.origin();
        for g in &self.layout.groups {
            let geo = self.group_geometry(g);
            let Some(m) = self.monitor_for(&g.monitor).or(self.monitors.first()) else { continue };
            let full = self.group_title_metrics(g, geo.dpi);
            let gap_to_top = geo.full_body.top - (m.work.top - oy);
            let threshold = full.title_h + full.gap
                + if g.title_side_last == 1 { full.title_h + 2 * full.gap.max(1) } else { 0 };
            log(&format!("title side: group={} pref={} last={} auto={} body_to_work_top={gap_to_top} threshold={threshold} near_top={} below={}",
                g.id, g.title_side, g.title_side_last, self.settings.title_edge_auto_adjust, gap_to_top <= threshold, geo.below));
        }
    }

    /// Window rectangle (union of title strip and icon area) in parent-client pixels.
    fn group_client_rect(&self, group: &model::Group) -> Rect {
        self.group_geometry(group).union
    }

    fn handle(&mut self, hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        if msg==crate::camouflage::runtime::WM_READY && hwnd==self.message_window {
            // camouflage-spec 4: lparam names the one Corral whose palette or fade moved; 0 = all.
            match lparam.0 as u64 { 0 => self.rerender_camouflage(), id => self.rerender_group(id) }
            return Some(LRESULT(0));
        }
        if self.taskbar_owns(hwnd) {
            return self.taskbar_message(msg, wparam, lparam);
        }
        // Central show guard: quick-hide and page animation must never resurrect inactive members.
        let member_id = self.groups.iter().find(|g| g.hwnd == hwnd).map(|g| g.id);
        let inactive = member_id.and_then(|id| self.layout.groups.iter().find(|g| g.id == id))
            .is_some_and(|g| !crate::tabs::body_visible(&self.layout.groups, g));
        if inactive && msg == WM_WINDOWPOSCHANGING && lparam.0 != 0 {
            let pos = unsafe { &mut *(lparam.0 as *mut WINDOWPOS) };
            if pos.flags.contains(SWP_SHOWWINDOW) {
                pos.flags = (pos.flags & !SWP_SHOWWINDOW) | SWP_HIDEWINDOW;
            }
        }
        if inactive && matches!(msg, WM_MOUSEMOVE | WM_MOUSEWHEEL | WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_KEYDOWN | WM_CONTEXTMENU) {
            return Some(LRESULT(0));
        }
        if msg == WM_DISPLAYCHANGE { self.draw_cancel(creation::CancelReason::DisplayChange); }
        if matches!(msg, WM_DISPLAYCHANGE | WM_SETTINGCHANGE) && hwnd != self.message_window {
            crate::tint::invalidate_wallpaper();
            if msg == WM_SETTINGCHANGE { crate::menu_theme::apply(); crate::motion::refresh_system(); }
            self.profile_displays_changed();
            if msg == WM_SETTINGCHANGE && self.profile_pending.is_none() {
                if self.refresh_icon_metrics() { self.load_items(); }
                self.refresh_all();
            }
            return Some(LRESULT(0));
        }
        if msg == WM_DPICHANGED {
            self.draw_cancel(creation::CancelReason::DpiChange);
        }
        if msg == WM_DPICHANGED && (Some(hwnd) == self.preview_host || self.is_layer(hwnd) || hwnd == self.message_window) {
            if Some(hwnd) == self.preview_host && lparam.0 != 0 {
                let suggested = unsafe { &*(lparam.0 as *const windows::Win32::Foundation::RECT) };
                unsafe { let _ = SetWindowPos(hwnd, HWND::default(), suggested.left, suggested.top,
                    suggested.right - suggested.left, suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE); }
            }
            // wallpaper-blur-spec 6: a DPI change moves the working size of that monitor's copies.
            crate::tint::invalidate_wallpaper();
            self.profile_displays_changed();
            return Some(LRESULT(0));
        }
        if let Some(result) = self.handle_pages_input(hwnd, msg, wparam, lparam) { return Some(result); }
        if msg == WM_WINDOWPOSCHANGED && self.detached() && !self.restacking {
            let pos = unsafe { &*(lparam.0 as *const WINDOWPOS) };
            if !pos.flags.contains(SWP_NOZORDER) && self.is_layer(hwnd) {
                unsafe {
                    let _ = PostMessageW(self.message_window, WM_RESTACK, WPARAM(0), LPARAM(0));
                }
            }
        }
        if hwnd == self.message_window && msg == WM_RESTACK {
            self.restack_for("WM_RESTACK");
            return Some(LRESULT(0));
        }
        if hwnd == self.message_window && msg == showdesk::WM_SHOWDESK {
            self.showdesk_event();
            return Some(LRESULT(0));
        }
        if hwnd == self.message_window {
            if let Some(result) = self.gate_window_message(msg, wparam, lparam) { return Some(result); }
            return self.handle_message_window(msg, wparam);
        }
        if Some(hwnd) == self.preview_host {
            match msg {
                WM_ENTERSIZEMOVE => {
                    self.preview_camouflage_moving = true;
                    self.rerender_camouflage();
                    return None;
                }
                WM_EXITSIZEMOVE => {
                    self.preview_camouflage_moving = false;
                    self.rerender_camouflage();
                    return None;
                }
                WM_SIZE => {
                    self.profile_displays_changed();
                    return Some(LRESULT(0));
                }
                // Portal popups are owned top-level windows placed in screen pixels: follow the
                // preview window (groups are its children and move with it). The simulated monitor
                // is the client rect in screen pixels, so it moves too.
                WM_MOVE => {
                    if self.profile_pending.is_some() { self.profile_displays_changed(); return None; }
                    if self.pages.moving() { self.page_cancel_motion(); self.page_refresh(); }
                    let live = self.current_display_snapshot();
                    if let Some((old, new)) = self.display_snapshot.as_ref().and_then(|s| s.displays().first())
                        .zip(live.as_ref().and_then(|s| s.displays().first())) {
                        let (dx, dy) = (new.work.left - old.work.left, new.work.top - old.work.top);
                        for state in self.runtime_bodies.values_mut() {
                            if let Some(r) = state.requested.offset(dx, dy) { state.requested = r; }
                        }
                    }
                    self.monitors = live.as_ref().map(hosting::snapshot_monitors).unwrap_or_default();
                    self.display_snapshot = live;
                    self.layout_portal_popups();
                    if !self.preview_camouflage_moving || crate::tint::blur_enabled() { self.rerender_camouflage(); }
                    return None;
                }
                WM_CLOSE => {
                    unsafe { PostQuitMessage(0) };
                    return Some(LRESULT(0));
                }
                _ => return None,
            }
        }
        if hwnd == self.surface_hwnd {
            return self.handle_surface(msg, wparam, lparam);
        }
        if let Some(index) = self.group_index(hwnd) {
            if msg == WM_MOUSEMOVE {
                self.update_strip_hover(index, Some(lparam_point(lparam)));
            } else if msg == 0x02A3 {
                self.update_strip_hover(index, None);
            }
            // Visual hover and input share the same cells. The dwell timer re-hits the cursor.
            if matches!(msg, WM_MOUSEMOVE | 0x02A3) {
                if let Some(group) = self.layout.groups.iter().find(|g| g.id == self.groups[index].id && g.tab.is_some()) {
                    let cells = self.tab_cells_of(group, self.groups[index].title, self.group_geometry(group).dpi);
                    let hit = if msg == WM_MOUSEMOVE { crate::tabs::hit_tab(&cells, lparam_point(lparam)) } else { None };
                    if self.groups[index].tab_hover != hit {
                        self.groups[index].tab_hover = hit;
                        self.hide_tab_tip();
                        self.render_group(index);
                    }
                    if hit.is_some() {
                        // One WM_MOUSEHOVER after the system hover time: the full title of a trimmed tab.
                        use windows::Win32::UI::Input::KeyboardAndMouse::{TME_HOVER, TRACKMOUSEEVENT, TrackMouseEvent};
                        const HOVER_DEFAULT: u32 = 0xFFFF_FFFF; // the system hover time
                        let mut track = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                            dwFlags: TME_HOVER, hwndTrack: hwnd, dwHoverTime: HOVER_DEFAULT };
                        unsafe { let _ = TrackMouseEvent(&mut track); }
                    }
                }
            }
            if msg == 0x02A3 { self.hide_tab_tip(); }
            if msg == 0x02A1 /* WM_MOUSEHOVER */ {
                self.tab_title_tip(index, lparam_point(lparam));
                return Some(LRESULT(0));
            }
            return self.handle_group(index, msg, wparam, lparam);
        }
        None
    }

    fn handle_message_window(&mut self, msg: u32, wparam: WPARAM) -> Option<LRESULT> {
        if msg == self.taskbar_created && self.taskbar_created != 0 {
            // Explorer restarted: our children died with the old Progman. Re-attach to the new host.
            self.schedule_reattach();
            return Some(LRESULT(0));
        }
        // folder-gate-host-spec 3.3: a drive that arrives, or a resume from sleep, retries the Gates
        // that show an error (the messages still go on to their other handlers).
        const WM_DEVICECHANGE: u32 = 0x0219;
        const DBT_DEVICEARRIVAL: usize = 0x8000;
        const PBT_APMRESUMEAUTOMATIC: usize = 0x0012;
        if (msg == WM_POWERBROADCAST && wparam.0 == PBT_APMRESUMEAUTOMATIC) || (msg == WM_DEVICECHANGE && wparam.0 == DBT_DEVICEARRIVAL) {
            self.retry_portal_errors();
        }
        if peek::is_peek_message(msg) {
            self.peek_message(msg, wparam);
            return Some(LRESULT(0));
        }
        if msg == WM_TIMER && self.pages_timer(wparam.0) { return Some(LRESULT(0)); }
        if msg == WM_TIMER && self.snapshot_timer(wparam.0) { return Some(LRESULT(0)); }
        match msg {
            WM_TIMER if wparam.0 == TIMER_TAB_HOVER => {
                self.poll_tab_hover();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_TAB_SCROLL => {
                self.tab_scroll_poll();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_TAB_ARM => {
                self.tab_zone_poll();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_DESK_SAVE => {
                self.desk_save_timer(); Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_TAB_SAVE => {
                unsafe { let _ = KillTimer(self.message_window, TIMER_TAB_SAVE); }
                self.tab_save_pending = false;
                self.save();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_REASSERT => {
                self.restack_for("reassert tick");
                if let Some(host) = self.host {
                    if !desktop::is_same_listview(host.listview, host.explorer_pid) {
                        self.schedule_reattach();
                    } else if unsafe { IsWindowVisible(host.listview) }.as_bool() {
                        desktop::hide_listview(&host);
                    }
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_REATTACH => {
                if self.reattach() {
                    unsafe {
                        let _ = KillTimer(self.message_window, TIMER_REATTACH);
                    }
                }
                Some(LRESULT(0))
            }
            WM_SHELL_CHANGE => {
                // Throttled, not debounced: the first notice of a burst re-reads after a short delay
                // and later notices ride along, so a delete/move shows at once (the old 1 s debounce
                // left dropped-away icons visible, live 2026-10-15) and a long copy re-reads at most
                // every SHELL_CHANGE_MS.
                if !self.shell_change_armed {
                    self.shell_change_armed = true;
                    // a burst (long copy) re-reads at most every SHELL_CHANGE_GAP_MS
                    let since = self.last_item_reload.map_or(u32::MAX, |t| t.elapsed().as_millis().min(u32::MAX as u128) as u32);
                    let delay = SHELL_CHANGE_MS.max(SHELL_CHANGE_GAP_MS.saturating_sub(since));
                    unsafe { SetTimer(self.message_window, TIMER_RELOAD_ITEMS, delay, None); }
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_SETTINGS => {
                self.sync_desktop_active();
                self.color_overlay_housekeeping();
                if self.settings_file_stamp() != self.settings_stamp {
                    self.reload_settings();
                    self.consume_rules_requests();
                    self.consume_snapshot_request(false);
                    self.consume_style_request(false);
                    if !self.settings.swipe_enabled { self.page_cancel_motion(); }
                    self.page_refresh();
                } else if self.refresh_icon_metrics() {
                    // Explorer icon-size changes need not change our JSON settings.
                    self.load_items();
                    self.refresh_all();
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_DEFERRED_DROPS => {
                unsafe {
                    let _ = KillTimer(self.message_window, TIMER_DEFERRED_DROPS);
                }
                if self.run_deferred_drops() {
                    self.refresh_all();
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_RELOAD_ITEMS => {
                unsafe {
                    let _ = KillTimer(self.message_window, TIMER_RELOAD_ITEMS);
                }
                self.shell_change_armed = false;
                self.last_item_reload = Some(Instant::now());
                self.load_items();
                self.refresh_all();
                // after a hand-over the Shell's operation may still be running: look again soon
                if let Some(&next) = self.quick_reloads.first() {
                    self.quick_reloads.remove(0);
                    unsafe { SetTimer(self.message_window, TIMER_RELOAD_ITEMS, next, None); }
                }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_ANIMATE => {
                self.step_roll_animation();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_LAYOUT_SAVE => {
                unsafe {
                    let _ = KillTimer(self.message_window, TIMER_LAYOUT_SAVE);
                }
                self.save();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_PROFILE => {
                self.profile_timer();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_DRAG_RENDER => {
                self.drag_render_tick();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_TITLE_HOVER => {
                self.step_title_hover();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_HOVER_POLL => {
                self.poll_hover_leave();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_HOVER_DELAY => {
                unsafe {
                    let _ = KillTimer(self.message_window, TIMER_HOVER_DELAY);
                }
                self.hover_delay_elapsed();
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == crate::camouflage::runtime::FADE_TIMER => {
                // One timer steps every running real-colour fade (camouflage-spec 2, 6).
                let (ids, running) = crate::camouflage::runtime::fade_tick();
                if !running { unsafe { let _ = KillTimer(self.message_window, wparam.0); } }
                for id in ids { self.rerender_group(id); }
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == peek::TIMER_PEEK => {
                self.peek_timer();
                Some(LRESULT(0))
            }
            WM_TIMER if showdesk::is_timer(wparam.0) => {
                self.showdesk_timer(wparam.0);
                Some(LRESULT(0))
            }
            WM_TIMER if quick_hide_glue::is_timer(wparam.0) => {
                self.quick_hide_timer(wparam.0);
                Some(LRESULT(0))
            }
            WM_TIMER if wparam.0 == TIMER_EXIT => {
                unsafe { PostQuitMessage(0) };
                Some(LRESULT(0))
            }
            WM_HOTKEY if wparam.0 == HOTKEY_QUIT as usize => {
                unsafe { PostQuitMessage(0) };
                Some(LRESULT(0))
            }
            WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
                if msg == WM_DISPLAYCHANGE { self.draw_cancel(creation::CancelReason::DisplayChange); }
                crate::tint::invalidate_wallpaper();
                if msg == WM_SETTINGCHANGE {
                    crate::menu_theme::apply();
                    crate::motion::refresh_system();
                } else {
                    self.peek_fullscreen_refresh();
                }
                self.profile_displays_changed();
                if msg == WM_SETTINGCHANGE && self.profile_pending.is_none() {
                    if self.refresh_icon_metrics() { self.load_items(); }
                    self.refresh_all();
                }
                Some(LRESULT(0))
            }
            _ => None,
        }
    }

    fn settings_file_stamp(&self) -> Option<std::time::SystemTime> {
        std::fs::metadata(&self.opts.settings_path).and_then(|m| m.modified()).ok()
    }

    /// Load settings (falls back to .bak, then defaults) and apply the values the engine uses.
    fn reload_settings(&mut self) {
        let title_edge_was = self.settings.title_edge_auto_adjust;
        let desk_was=self.settings.get(crate::settings::Field::LayoutPreserve);
        self.settings = settings_store::load_with_backup(&self.opts.settings_path);
        self.desk_mode_reloaded(desk_was==2);
        self.consume_desk_request();
        self.title_edge_changed(title_edge_was);
        self.tab_base_exclusions = self.settings.quick_hide_exclusions.clone();
        self.apply_tab_exclusions();
        self.settings_stamp = self.settings_file_stamp();
        self.mouse_mode = creation::MouseMode::from_value(self.settings.create_corral_with_mouse_mode);
        self.refresh_icon_metrics();
        self.peek_configure();
        self.taskbar_configure();
        // colour-state-spec 6: a settings reload cancels the draft. The freshly loaded settings hold
        // the stored look already; the Corral part of the draft is taken off the layout.
        self.color_overlay.cancel_for_reload(&mut self.layout);
        log(&format!("settings loaded from {}", self.opts.settings_path.display()));
    }

    /// Show `menu` modally with menu-message forwarding active.
    fn track(menu: &Rc<ShellMenu>, owner: HWND, screen: (i32, i32)) -> Option<u32> {
        ACTIVE_MENU.with(|cell| *cell.borrow_mut() = Some(menu.clone()));
        let choice = menu.show(owner, screen);
        ACTIVE_MENU.with(|cell| *cell.borrow_mut() = None);
        choice
    }

    fn to_screen(&self, client_point: (i32, i32)) -> (i32, i32) {
        let (ox, oy) = self.origin();
        (client_point.0 + ox, client_point.1 + oy)
    }

    /// After a drop handed to a Shell target (Recycle Bin, folder): re-read at once and a few times
    /// more while the Shell's (possibly background) operation finishes, instead of one read after 1 s
    /// (live 2026-10-15: the icon left the desktop visibly late).
    fn reload_soon_after_handover(&mut self) {
        self.quick_reloads = vec![300, 700];
        unsafe {
            SetTimer(self.message_window, TIMER_RELOAD_ITEMS, 100, None);
        }
    }

    /// Re-enumerate desktop items shortly after a shell command (no change notifications yet).
    fn schedule_item_reload(&mut self) {
        unsafe {
            // a later request must not push a hand-over's quick re-read (or an armed change-notice
            // re-read) back to the 1 s debounce
            if self.shell_change_armed { return; }
            let delay = if self.quick_reloads.is_empty() { reload_delay_ms(!self.pending_drops.is_empty()) } else { 100 };
            SetTimer(self.message_window, TIMER_RELOAD_ITEMS, delay, None);
        }
    }

    fn open_settings_window(&self) {
        if let Ok(exe) = std::env::current_exe() {
            // the Rules page lists this app's Corrals as destinations
            let _ = std::process::Command::new(exe)
                .arg("--settings")
                .arg("--settings-path")
                .arg(&self.opts.settings_path)
                .arg("--layout")
                .arg(&self.opts.layout_path)
                .spawn();
        }
    }
}

/// Monotonic milliseconds since Corral started (used for the portal
/// view seed; only differences matter).
pub fn tick_ms() -> u64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

#[cfg(test)]
mod dpi_wiring_tests {
    use super::*;
    use crate::dpi_transition::DisplaySnapshot;

    pub(super) fn displays(stacked: bool) -> Snapshot {
        let monitors = if stacked {
            vec![Monitor { name: "Monitor B".into(), bounds: Rect::new(-2560, -1440, 0, 0), work: Rect::new(-2560, -1440, 0, 0), dpi: 96 },
                Monitor { name: "Monitor A".into(), bounds: Rect::new(-2560, 0, 1280, 2160), work: Rect::new(-2560, 0, 1280, 2160), dpi: 240 }]
        } else {
            vec![Monitor { name: "Monitor B".into(), bounds: Rect::new(-2560, 0, 0, 1440), work: Rect::new(-2560, 0, 0, 1440), dpi: 96 },
                Monitor { name: "Monitor A".into(), bounds: Rect::new(0, 0, 3840, 2160), work: Rect::new(0, 0, 3840, 2160), dpi: 240 }]
        };
        Snapshot::new(monitors.iter().enumerate().map(|(i,m)| DisplaySnapshot::from_monitor(m, i == 0, None)).collect()).unwrap()
    }

    pub(super) fn layout() -> Layout {
        let mut l = Layout::new();
        l.add_group("A".into(), "Monitor B".into(), Rect::new(2100, 200, 2701, 503), model::GroupContent::Items { items: vec![] });
        l.add_group("B".into(), "Monitor B".into(), Rect::new(400, 200, 1001, 503), model::GroupContent::Items { items: vec![] });
        l
    }

    #[test]
    fn straddling_body_is_governed_by_the_chosen_monitor_not_the_top_left() {
        // Top-left on Monitor B (96 dpi), centre on Monitor A (240 dpi). The governing monitor is whichever the
        // interaction picked (stored in the group), and it scales the whole rect.
        let r = RequestedBody::from_physical(Rect::new(-100, 200, 501, 503)).unwrap();
        for (gov, dpi) in [("Monitor B", 96), ("Monitor A", 240)] {
            let (p,e) = body_placement(r, &displays(false), Some(gov), None, |_| (50,50)).unwrap();
            assert_eq!((p.monitor.as_str(), p.effective_dpi), (gov, dpi));
            assert_eq!(e,r.physical());
        }
        // an unknown stored monitor falls back to the monitor under the body's centre
        let (p,_) = body_placement(r, &displays(false), Some("gone"), None, |_| (50,50)).unwrap();
        assert_eq!(p.monitor, "Monitor A");
    }

    #[test]
    fn metric_dpi_follows_the_governing_monitor_during_interaction_and_at_rest() {
        let r = RequestedBody::from_physical(Rect::new(-100, 200, 501, 503)).unwrap();
        let (p, _) = body_placement(r, &displays(false), Some("Monitor A"), None, |_| (50, 50)).unwrap();
        assert_eq!((p.monitor.as_str(), p.effective_dpi), ("Monitor A", 240));
        let identity = displays(false).find(&p.monitor).unwrap().dpi;
        assert_eq!(identity, 240);
        // interaction: the tracked governing DPI wins
        assert_eq!(pick_metric_dpi(Some(p.effective_dpi), None, identity), 240);
        assert_eq!(pick_metric_dpi(Some(240), Some(96), identity), 240);
        // at rest after release: the geometry-resolved DPI (equal to the stored monitor's)
        assert_eq!(pick_metric_dpi(None, Some(p.effective_dpi), identity), 240);
        // unresolved placement: only then the identity monitor
        assert_eq!(pick_metric_dpi(None, None, identity), 240);
        let (p, _) = body_placement(r, &displays(false), Some("Monitor B"), None, |_| (50, 50)).unwrap();
        assert_eq!(pick_metric_dpi(None, Some(p.effective_dpi), 240), 96);
    }

    #[test]
    fn effective_minimum_follows_the_governing_dpi_without_scaling_requested() {
        let r = RequestedBody::from_physical(Rect::new(-20, 200, -9, 211)).unwrap();
        let min = |dpi| (to_px(100,dpi),to_px(100,dpi));
        let (p,e) = body_placement(r, &displays(false), Some("Monitor A"), None, min).unwrap();
        assert_eq!(p.effective_dpi,240); assert_eq!(p.requested,r);
        assert_eq!((e.width(),e.height()),(250,250));
        let (p,e) = body_placement(r, &displays(false), Some("Monitor B"), None, min).unwrap();
        assert_eq!(p.effective_dpi,96); assert_eq!(p.requested,r);
        assert_eq!((e.width(),e.height()),(100,100));
    }

    #[test]
    fn placement_is_deterministic_and_keeps_requested_extent() {
        let r = RequestedBody::from_physical(Rect::new(-200, 200, -190, 210)).unwrap();
        let live = displays(false);
        let minimum = |dpi| if dpi == 96 { (500, 50) } else { (100, 50) };
        let a = body_placement(r, &live, Some("Monitor B"), None, minimum).unwrap();
        let b = body_placement(r, &live, Some("Monitor B"), None, minimum).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.0.requested, r);
        assert_eq!(a.1.width(), 500);
        assert_eq!(a.0.effective_dpi, 96);
    }

    #[test]
    fn model_projection_does_not_change_runtime_odd_pixels() {
        let mut l = layout(); let id=l.groups[0].id;
        let mut states=HashMap::new();
        let r=RequestedBody::from_physical(Rect::new(201, 203, 802, 506)).unwrap();
        let (p,_)=body_placement(r,&displays(false), Some("Monitor A"), None, |_|(1,1)).unwrap();
        store_body_placement(&mut l,&mut states,id,p);
        assert_eq!(states[&id].requested,r);
        assert!(states[&id].matches(&l.groups[0]));
        let roundtrip=RequestedBody::from_dips(l.groups[0].rect,displays(false).find("Monitor A").unwrap()).unwrap();
        assert_ne!(roundtrip,r);
    }

    #[test]
    fn tab_set_propagates_identity_extents_and_exact_runtime() {
        let mut l=layout(); let id=l.groups[0].id; let member=l.groups[1].id;
        l.join_tabs(id,member).unwrap();
        let mut states=HashMap::new();
        let r=RequestedBody::from_physical(Rect::new(201,203,802,506)).unwrap();
        let (p,_)=body_placement(r,&displays(false), Some("Monitor A"), None, |_|(1,1)).unwrap();
        store_body_placement(&mut l,&mut states,id,p);
        assert_eq!(l.groups[0].rect,l.groups[1].rect);
        assert!(l.groups.iter().all(|g|g.monitor=="Monitor A" && states[&g.id].requested==r && states[&g.id].centre_dpi==240));
    }

    #[test]
    fn intentional_model_edit_invalidates_runtime_stamp() {
        let mut l=layout(); let id=l.groups[0].id; let mut states=HashMap::new();
        let r=RequestedBody::from_physical(Rect::new(-200,100,401,403)).unwrap();
        let (p,_)=body_placement(r,&displays(false), Some("Monitor A"), None, |_|(1,1)).unwrap();
        store_body_placement(&mut l,&mut states,id,p);
        l.groups[0].rect=l.groups[0].rect.offset(1,0);
        assert!(!states[&id].matches(&l.groups[0]));
    }

    #[test]
    fn capture_completion_retains_last_pixels_after_lossy_projection() {
        let mut l=layout(); let id=l.groups[0].id; let mut states=HashMap::new();
        let r=RequestedBody::from_physical(Rect::new(201,203,802,506)).unwrap();
        let (p,e)=body_placement(r,&displays(false), Some("Monitor A"), None, |_|(1,1)).unwrap();
        store_body_placement(&mut l,&mut states,id,p);
        // The completion path stores the effective rectangle, then clears Drag;
        // geometry uses this valid state independently of capture ownership.
        let (p,_)=body_placement(RequestedBody::from_physical(e).unwrap(),&displays(false), Some("Monitor A"), None, |_|(1,1)).unwrap();
        store_body_placement(&mut l,&mut states,id,p);
        assert_eq!(states[&id].requested,r);
        assert!(states[&id].matches(&l.groups[0]));
    }

    /// Monitor B -> Monitor A -> Monitor B through the App's storing path (monitor, DIP rect, runtime pixels):
    /// the persisted DIP size never changes and a restart (decode of the saved rect) gives
    /// exactly the settled on-screen pixels.
    #[test]
    fn move_round_trip_keeps_dip_size_and_restart_reproduces_the_pixels() {
        let live = displays(false); // Monitor B (96 dpi) left of x=0, Monitor A (240 dpi) right of it
        let mut l = layout(); let id = l.groups[0].id;
        let mut states = HashMap::new();
        let size = (l.groups[0].rect.width(), l.groups[0].rect.height());
        assert_eq!(size, (601, 303));
        let start = dpi_transition::DisplaySnapshot::clone(live.find("Monitor B").unwrap());
        let r0 = RequestedBody::from_dips(l.groups[0].rect, &start).unwrap();
        let grab = dpi_transition::Grab { px: (200, -17), dpi: 96, size_dip: size };
        let mut gov = "Monitor B".to_string();
        let min = |dpi: u32| (to_px(crate::snap::MIN_BODY_DIP.0, dpi), to_px(crate::snap::MIN_BODY_DIP.1, dpi));
        let step = |l: &mut Layout, states: &mut HashMap<u64, RuntimeBody>, gov: &mut String, x: i32| {
            let g = dpi_transition::govern(&live, gov, |m| {
                let b = dpi_transition::move_layout(grab, (x, 400), m.dpi)?.physical();
                Some(metrics::effective_body(b, min(m.dpi)))
            }).unwrap().device.clone();
            let d = live.find(&g).unwrap().dpi;
            let req = dpi_transition::move_layout(grab, (x, 400), d).unwrap();
            let (p, _) = body_placement(req, &live, Some(&g), Some(size), |dpi| min(dpi)).unwrap();
            store_body_placement(l, states, id, p);
            *gov = g;
        };
        let _ = r0;
        for round in 0..20 {
            let path: Vec<i32> = (-1800..1800).step_by(37).collect();
            for &x in &path { step(&mut l, &mut states, &mut gov, x); }
            assert_eq!(gov, "Monitor A", "round {round}");
            assert_eq!((l.groups[0].rect.width(), l.groups[0].rect.height()), size);
            let on_mon_a = states[&id].requested.physical();
            assert_eq!((on_mon_a.width(), on_mon_a.height()), (1503, 758));
            for &x in path.iter().rev() { step(&mut l, &mut states, &mut gov, x); }
            assert_eq!(gov, "Monitor B", "round {round}");
            assert_eq!((l.groups[0].rect.width(), l.groups[0].rect.height()), size);
            assert_eq!(states[&id].requested.physical().width(), 601);
        }
        // release: settle onto the grid, store, then "restart" = decode of the saved rect
        let d = live.find(&gov).unwrap();
        let settled = dpi_transition::settle_on_grid(states[&id].requested, d, Some(size)).unwrap();
        let (p, _) = body_placement(settled, &live, Some(&gov), Some(size), |dpi| min(dpi)).unwrap();
        store_body_placement(&mut l, &mut states, id, p);
        let reloaded = RequestedBody::from_dips(l.groups[0].rect, live.find(&l.groups[0].monitor).unwrap()).unwrap();
        assert_eq!(reloaded, settled);
        assert_eq!(states[&id].requested, settled);
    }
}

#[cfg(test)]
mod reload_delay_tests {
    #[test]
    fn a_pending_drop_reloads_sooner() {
        assert_eq!(super::reload_delay_ms(true), 150);
        assert_eq!(super::reload_delay_ms(false), 1000);
    }
}

#[cfg(test)]
mod q26_fit_tests {
    use super::*;
    use super::dpi_wiring_tests::{displays, layout};

    fn fit(l: &mut Layout, monitor: &str) -> (Rect, Rect) {
        let live = displays(false);
        let d = live.find(monitor).unwrap();
        let g = &l.groups[0];
        let preferred = g.preferred_size_dip.unwrap_or((g.rect.width(), g.rect.height()));
        let rect = if g.monitor == monitor { g.rect } else { Rect::new(0,0,preferred.0,preferred.1) };
        let body = RequestedBody::from_dips(rect, d).unwrap();
        let m = group_paint::metrics(d.dpi);
        let (p, effective) = fitted_body_placement(body, &live, d, preferred,
            (to_px(crate::snap::MIN_BODY_DIP.0, d.dpi), to_px(crate::snap::MIN_BODY_DIP.1, d.dpi)), m.title_h + m.gap).unwrap();
        let title = group_paint::title_rect_on(effective, true, &m);
        let envelope = group_paint::union_rect(effective, title, false);
        let requested = dpi_transition::contain_fitted(p.requested, envelope, d).unwrap();
        let shown = (p.rect_dip.width(), p.rect_dip.height());
        let settled = dpi_transition::settle_on_grid(requested, d, Some(shown)).unwrap();
        let (p, _) = fitted_body_placement(settled, &live, d, preferred,
            (to_px(crate::snap::MIN_BODY_DIP.0, d.dpi), to_px(crate::snap::MIN_BODY_DIP.1, d.dpi)), m.title_h + m.gap).unwrap();
        let full = p.requested.physical();
        let title = group_paint::title_rect_on(full, true, &m);
        let id = g.id;
        store_body_placement(l, &mut HashMap::new(), id, p);
        (full, title)
    }

    fn large() -> Layout {
        let mut l = layout();
        l.groups[0].rect = Rect::new(0,0,1280,1400);
        l
    }

    #[test]
    fn half_mon_b_fits_mon_a_with_aspect_and_title() {
        let mut l = large();
        let (body, title) = fit(&mut l, "Monitor A");
        let live = displays(false); let work = live.find("Monitor A").unwrap().work;
        assert!(body.left >= work.left && body.right <= work.right);
        assert!(body.top >= work.top && title.bottom <= work.bottom);
        let shown = l.groups[0].rect;
        assert!((shown.width() as f64 / shown.height() as f64 - 1280.0/1400.0).abs() < 0.002);
        assert_eq!(l.groups[0].preferred_size_dip, Some((1280,1400)));
    }

    #[test]
    fn twenty_round_trips_restore_exact_preferred_without_drift() {
        let mut l = large();
        for _ in 0..20 {
            fit(&mut l, "Monitor A"); fit(&mut l, "Monitor B");
            assert_eq!((l.groups[0].rect.width(), l.groups[0].rect.height()), (1280,1400));
            assert_eq!(l.groups[0].preferred_size_dip, Some((1280,1400)));
        }
    }

    #[test]
    fn restart_on_mon_a_preserves_fitted_display_and_preference() {
        let mut l = large(); fit(&mut l, "Monitor A");
        l.groups[0].rect = l.groups[0].rect.offset(13,0);
        let (pixels,_) = fit(&mut l,"Monitor A");
        let live=displays(false); let d=live.find("Monitor A").unwrap();
        assert_eq!(RequestedBody::from_dips(l.groups[0].rect,d).unwrap().physical(),pixels);
        let rect = l.groups[0].rect; let version = l.version;
        let mut restored: Layout = serde_json::from_str(&serde_json::to_string(&l).unwrap()).unwrap();
        fit(&mut restored, "Monitor A");
        assert_eq!(restored.groups[0].rect, rect);
        assert_eq!(restored.version, version);
        assert_eq!(restored.groups[0].preferred_size_dip, Some((1280,1400)));
        fit(&mut restored, "Monitor B");
        assert_eq!(restored.groups[0].rect.height(),1400);
    }

    #[test]
    fn user_resize_while_fitted_replaces_preference() {
        let mut l = large(); fit(&mut l, "Monitor A");
        // The resize path projects cursor-defined edges and explicitly replaces preference.
        let live = displays(false); let d = live.find("Monitor A").unwrap();
        let raw = RequestedBody::from_dips(Rect::new(0,0,500,600),d).unwrap();
        let id = l.groups[0].id;
        assert!(user_resize_preference(&mut l,id,raw,d).unwrap().1);
        fit(&mut l, "Monitor A"); fit(&mut l, "Monitor B");
        assert_eq!((l.groups[0].rect.width(),l.groups[0].rect.height()),(500,600));
    }

    #[test]
    fn already_small_group_retains_size() {
        let mut l = layout(); fit(&mut l,"Monitor A");
        assert_eq!((l.groups[0].rect.width(),l.groups[0].rect.height()),(601,303));
    }

    #[test]
    fn tab_set_shares_fitting_preference_selection_and_restoration() {
        let mut l = large(); let id = l.groups[0].id; let other = l.groups[1].id;
        l.join_tabs(id,other).unwrap(); fit(&mut l,"Monitor A");
        assert_eq!(l.groups[0].rect,l.groups[1].rect);
        assert_eq!(l.groups[0].preferred_size_dip,l.groups[1].preferred_size_dip);
        let doc=crate::tabs::select(&l.tab_doc(),other).unwrap(); l.apply_tab_doc(doc);
        fit(&mut l,"Monitor B");
        assert!(l.groups.iter().all(|g| g.rect.width()==1280 && g.rect.height()==1400
            && g.preferred_size_dip==Some((1280,1400))));
    }

    #[test]
    fn rolled_portal_uses_expanded_body() {
        let mut l = large(); l.groups[0].roll = RollState::Manual;
        l.groups[0].content = model::GroupContent::Portal { path: "metadata-only".into() };
        fit(&mut l,"Monitor A"); assert!(l.groups[0].rect.height()<864);
        fit(&mut l,"Monitor B"); assert_eq!(l.groups[0].rect.height(),1400);
        assert_eq!(l.groups[0].roll,RollState::Manual);
    }

    #[test]
    fn work_area_shrink_then_growth_restores_size() {
        let live = displays(false); let mut d = live.find("Monitor B").unwrap().clone();
        d.work.bottom = 700;
        let pref = (1280,1400);
        let shown = dpi_transition::fit_size(pref,&d,crate::snap::MIN_BODY_DIP,crate::group_paint::metrics(96).title_h).unwrap();
        assert!(shown.1<=700-crate::group_paint::metrics(96).title_h);
        d.work.bottom=1440;
        assert_eq!(dpi_transition::fit_size(pref,&d,crate::snap::MIN_BODY_DIP,crate::group_paint::metrics(96).title_h),Some(pref));
    }

    #[test]
    fn fitted_candidate_governing_monitor_is_stable_at_every_cursor() {
        for stacked in [false,true] {
            let live = displays(stacked);
            let grab = dpi_transition::Grab { px:(640,-17), dpi:96, size_dip:(1280,1400) };
            let mut current = "Monitor B".to_string();
            for pos in (-2500..3500).step_by(29) {
                let cursor = if stacked { (-1600,pos) } else { (pos,500) };
                let probe = |d: &dpi_transition::DisplaySnapshot| {
                    let m=group_paint::metrics(d.dpi);
                    let size=dpi_transition::fit_size(grab.size_dip,d,(to_px(crate::snap::MIN_BODY_DIP.0,d.dpi),to_px(crate::snap::MIN_BODY_DIP.1,d.dpi)),m.title_h+m.gap)?;
                    Some(dpi_transition::move_layout_fitted(grab,cursor,d,size)?.physical())
                };
                current=dpi_transition::govern(&live,&current,probe).unwrap().device.clone();
                for _ in 0..5 {
                    assert_eq!(dpi_transition::govern(&live,&current,probe).unwrap().device,current);
                }
            }
            assert_eq!(current,"Monitor A");
        }
    }
    #[test]
    fn old_layout_defaults_to_current_rect_size() {
        let mut l=large();
        let json=serde_json::to_value(&l).unwrap();
        assert!(json["groups"][0].get("preferred_size_dip").is_none());
        let old: Layout=serde_json::from_value(json).unwrap();
        assert_eq!(old.groups[0].preferred_size_dip,None);
        fit(&mut l,"Monitor A");
        assert_eq!(l.groups[0].preferred_size_dip,Some((1280,1400)));
    }

    #[test]
    fn fitted_grab_scales_horizontal_offset_but_keeps_title_height() {
        let live=displays(false); let d=live.find("Monitor A").unwrap();
        let grab=dpi_transition::Grab { px:(640,-17),dpi:96,size_dip:(1280,1400) };
        let m=group_paint::metrics(d.dpi);
        let shown=dpi_transition::fit_size(grab.size_dip,d,(328,258),m.title_h+m.gap).unwrap();
        let body=dpi_transition::move_layout_fitted(grab,(1800,200),d,shown).unwrap().physical();
        assert!((1800-body.left-body.width()/2).abs()<=1);
        assert_eq!(body.top-200,43);
    }

    #[test]
    fn work_area_wins_when_minimum_and_title_cannot_fit() {
        let live=displays(false); let mut d=live.find("Monitor A").unwrap().clone();
        d.work=Rect::new(0,0,200,200);
        let shown=dpi_transition::fit_size((1280,1400),&d,(328,258),80).unwrap();
        let px=dpi_transition::scale_size(shown,d.dpi).unwrap();
        assert!(px.0<=200 && px.1+80<=200);
        let r=RequestedBody::from_physical(Rect::new(0,80,1000,1000)).unwrap();
        let tiny=Snapshot::new(vec![d.clone()]).unwrap();
        let (_,effective)=fitted_body_placement(r,&tiny,&d,(1280,1400),(328,258),80).unwrap();
        assert!(effective.width()<=200 && effective.height()+80<=200);
        assert_eq!(dpi_transition::fit_size((0,1400),&d,(328,258),80),None);
    }

    #[test]
    fn equal_dpi_different_work_room_uses_candidate_identity_for_hysteresis() {
        let base=displays(false); let mut ds=base.displays().to_vec();
        ds[1].dpi=96; ds[1].work.bottom=800;
        let live=Snapshot::new(ds).unwrap();
        let grab=dpi_transition::Grab { px:(1000,-17),dpi:96,size_dip:(1280,1400) };
        let mut current="Monitor B".to_string();
        for x in (-2500..3500).step_by(19) {
            let probe=|d: &dpi_transition::DisplaySnapshot| {
                let shown=dpi_transition::fit_size(grab.size_dip,d,crate::snap::MIN_BODY_DIP,crate::group_paint::metrics(96).title_h)?;
                Some(dpi_transition::move_layout_fitted(grab,(x,400),d,shown)?.physical())
            };
            current=dpi_transition::govern(&live,&current,probe).unwrap().device.clone();
            assert_eq!(dpi_transition::govern(&live,&current,probe).unwrap().device,current);
        }
        assert_eq!(current,"Monitor A");
    }

    #[test]
    fn fit_log_ignores_259_identical_layouts_and_records_size_or_monitor_changes() {
        let mut states=HashMap::new();
        let pref=(408,1365); let shown=(405,1358);
        assert!(record_fit_context(&mut states,2,"A",shown,pref));
        for _ in 0..259 { assert!(!record_fit_context(&mut states,2,"A",shown,pref)); }
        assert!(record_fit_context(&mut states,2,"B",shown,pref));
        assert!(record_fit_context(&mut states,2,"B",(405,1357),pref));
        assert!(!record_fit_context(&mut states,2,"A",pref,pref));
        assert!(record_fit_context(&mut states,2,"B",shown,pref));
        assert!(record_fit_context(&mut states,3,"B",shown,pref));
    }

    #[test]
    fn common_pairs_portrait_and_third_monitor_fit_restore_model_round_trips() {
        for (a,b) in [(96,144),(120,168)] {
            for bounds in [Rect::new(3200,0,5120,1080),Rect::new(0,-1080,1920,0),
                Rect::new(0,2800,1920,3880),Rect::new(-1080,-300,0,1620)] {
                let live=Snapshot::new(vec![
                    dpi_transition::DisplaySnapshot{device:"A".into(),bounds:Rect::new(0,0,3200,2800),work:Rect::new(0,0,3200,2800),dpi:a,primary:true,hardware:Default::default()},
                    dpi_transition::DisplaySnapshot{device:"B".into(),bounds,work:bounds,dpi:b,primary:false,hardware:Default::default()},
                    dpi_transition::DisplaySnapshot{device:"C".into(),bounds:Rect::new(-1600,3000,0,3900),work:Rect::new(-1600,3000,0,3900),dpi:192,primary:false,hardware:Default::default()},
                ]).unwrap();
                let mut l=large(); let id=l.groups[0].id;
                l.groups[0].preferred_size_dip=Some((1280,1400));
                for _ in 0..20 {
                    for name in ["B","C","A"] {
                        let d=live.find(name).unwrap(); let m=group_paint::metrics(d.dpi);
                        let raw=RequestedBody::from_dips(Rect::new(13,17,1293,1417),d).unwrap();
                        let (p,effective)=fitted_body_placement(raw,&live,d,(1280,1400),
                            dpi_transition::scale_size(crate::snap::MIN_BODY_DIP,d.dpi).unwrap(),m.title_h+m.gap).unwrap();
                        assert!(effective.width()<=d.work.width() && effective.height()+m.title_h+m.gap<=d.work.height());
                        // Match release: arbitrary physical candidates settle onto the
                        // destination DIP grid before persistence/restart comparison.
                        let shown=(p.rect_dip.width(),p.rect_dip.height());
                        let settled=dpi_transition::settle_on_grid(p.requested,d,Some(shown)).unwrap();
                        let (p,_)=fitted_body_placement(settled,&live,d,(1280,1400),
                            dpi_transition::scale_size(crate::snap::MIN_BODY_DIP,d.dpi).unwrap(),m.title_h+m.gap).unwrap();
                        let pbody=p.requested;
                        store_body_placement(&mut l,&mut HashMap::new(),id,p);
                        assert_eq!(l.groups[0].preferred_size_dip,Some((1280,1400)));
                        assert_eq!(RequestedBody::from_dips(l.groups[0].rect,d),Some(pbody));
                        if name=="A" { assert_eq!((l.groups[0].rect.width(),l.groups[0].rect.height()),(1280,1400)); }
                        else { assert!(l.groups[0].rect.height()<1400); }
                    }
                }
            }
        }
    }

}

#[cfg(test)]
mod resize_undo_tests {
    use super::*;

    #[test]
    fn esc_restores_rect_monitor_preferred_size_and_runtime_pixels_exactly() {
        let mut layout = Layout::new();
        let a = layout.add_group("a".into(), "M1".into(), Rect::new(10, 20, 410, 320), model::GroupContent::Items { items: vec![] });
        let b = layout.add_group("b".into(), "M1".into(), Rect::new(500, 20, 700, 120), model::GroupContent::Items { items: vec![] });
        layout.group_mut(a).unwrap().preferred_size_dip = Some((401, 299));
        let mut states: HashMap<u64, RuntimeBody> = HashMap::new();
        states.insert(a, RuntimeBody { requested: RequestedBody::from_physical(Rect::new(11, 21, 412, 321)).unwrap(), centre_dpi: 144, monitor: "M1".into(), projected: Rect::new(10, 20, 410, 320) });
        let (rect_a, rect_b) = (layout.group_mut(a).unwrap().rect, layout.group_mut(b).unwrap().rect);
        let undo = capture_resize_state(&layout, &states, a);
        // the resize changes everything a step can touch
        let g = layout.group_mut(a).unwrap();
        g.rect = Rect::new(10, 20, 999, 888);
        g.monitor = "M2".into();
        g.preferred_size_dip = Some((7, 7));
        states.insert(a, RuntimeBody { requested: RequestedBody::from_physical(Rect::new(0, 0, 50, 50)).unwrap(), centre_dpi: 96, monitor: "M2".into(), projected: Rect::new(0, 0, 9, 9) });
        restore_resize_state(&mut layout, &mut states, &undo);
        let g = layout.group_mut(a).unwrap();
        assert_eq!((g.rect, g.monitor.as_str(), g.preferred_size_dip), (rect_a, "M1", Some((401, 299))));
        let state = states.get(&a).unwrap();
        assert_eq!((state.requested.physical(), state.centre_dpi, state.projected), (Rect::new(11, 21, 412, 321), 144, Rect::new(10, 20, 410, 320)));
        assert_eq!(layout.group_mut(b).unwrap().rect, rect_b, "other Corrals are not touched");
        // a Corral that had no runtime body gets none back; no preferred size stays none
        let mut states: HashMap<u64, RuntimeBody> = HashMap::new();
        let undo = capture_resize_state(&layout, &states, b);
        layout.group_mut(b).unwrap().preferred_size_dip = Some((1, 1));
        states.insert(b, RuntimeBody { requested: RequestedBody::from_physical(Rect::new(0, 0, 5, 5)).unwrap(), centre_dpi: 96, monitor: "M1".into(), projected: Rect::new(0, 0, 5, 5) });
        restore_resize_state(&mut layout, &mut states, &undo);
        assert!(states.is_empty() && layout.group_mut(b).unwrap().preferred_size_dip.is_none());
    }
}
