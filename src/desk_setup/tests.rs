//! Pure acceptance coverage from desk-setups-spec section 7.
use super::*;
use crate::model::GroupContent;
use serde_json::json;

fn monitor(name: &str, product: u16, x: i32) -> MonitorState {
    MonitorState {
        id: MonitorIdentity {
            model: Some(Model { mfr: "DEL".into(), product }),
            name: Some(name.into()),
            ..Default::default()
        },
        size_px: (2560, 1440), scale_pct: 100, offset_px: (x, 0),
        primary: x == 0, gdi_name: name.into(),
        work: Rect::new(x, 0, x + 2560, 1400), ..Default::default()
    }
}
fn serial(mut m: MonitorState, value: &str) -> MonitorState {
    m.id.serial_hash = Some(value.into()); m
}
fn connector(mut m: MonitorState, instance: u32) -> MonitorState {
    m.id.connector = Some(Connector { tech: 10, instance }); m
}
fn setup(id: &str, monitors: Vec<MonitorState>, used: i64) -> DeskSetup {
    DeskSetup { id: id.into(), name: id.into(), origin: "learned".into(),
        monitors, last_used_ms: used, ..Default::default() }
}
fn learned() -> (Layout, Vec<MonitorState>) {
    let monitors = vec![monitor("laptop", 1, 0), monitor("external", 2, 2560)];
    let mut layout = Layout::new();
    for (i, m) in monitors.iter().enumerate() {
        layout.add_group(format!("Corral {i}"), m.gdi_name.clone(),
            Rect::new(40 + i as i32, 80, 540 + i as i32, 380),
            GroupContent::Items { items: vec![format!("owned-{i}")] });
    }
    evaluate(&mut layout, &monitors, 1);
    layout.groups[0].rect.left += 1;
    layout.groups[0].preferred_size_dip = Some((500, 300));
    layout.groups[1].roll = RollState::Docked;
    layout.groups[1].dock_top = true;
    layout.groups[1].dock_bottom = true;
    layout.groups[1].preferred_size_dip = Some((900, 700));
    assert!(user_edit(&mut layout, 2, false));
    (layout, monitors)
}
fn saved_bytes(layout: &Layout, index: usize) -> Vec<u8> {
    serde_json::to_vec(&layout.desk_setups.setups[index]).unwrap()
}

#[test]
fn dock_undock_recovery_leaves_saved_setup_byte_identical_and_restores_flags() {
    let (mut layout, monitors) = learned();
    let base = layout.desk_setups.setups[0].clone();
    let bytes = saved_bytes(&layout, 0);
    assert_eq!(evaluate(&mut layout, &monitors[..1], 10).case, Case::Missing);
    assert_eq!(saved_bytes(&layout, 0), bytes);
    assert_eq!(layout.groups[1].monitor, "laptop");
    assert_eq!(layout.groups[1].rect, base.groups[&2].rect);
    assert_eq!(layout.groups[1].preferred_size_dip, Some((900, 700)));
    assert_eq!(layout.groups[1].roll, RollState::Docked);
    assert!(layout.groups[1].dock_top && layout.groups[1].dock_bottom);
    for now in 11..16 {
        assert_eq!(evaluate(&mut layout, &monitors[..1], now).case, Case::Missing);
        assert!(!user_edit(&mut layout, now, false));
    }
    assert_eq!(saved_bytes(&layout, 0), bytes);
    assert_eq!(evaluate(&mut layout, &monitors, 20).case, Case::Exact);
    for g in &layout.groups {
        let p = &base.groups[&g.id];
        assert_eq!(g.rect, p.rect);
        assert_eq!(g.roll, p.roll);
        assert_eq!((g.dock_top, g.dock_bottom), (p.dock_top, p.dock_bottom));
        assert_eq!(g.preferred_size_dip, p.preferred_size_dip);
    }
}

#[test]
fn first_user_edit_during_recovery_creates_new_setup_and_preserves_original() {
    let (mut layout, monitors) = learned();
    let bytes = saved_bytes(&layout, 0);
    evaluate(&mut layout, &monitors[..1], 10);
    layout.groups[1].roll = RollState::Manual;
    layout.groups[1].rect = Rect::new(300, 200, 800, 500);
    assert!(user_edit(&mut layout, 11, false));
    assert_eq!(layout.desk_setups.setups.len(), 2);
    assert_eq!(saved_bytes(&layout, 0), bytes);
    assert_ne!(layout.desk_setups.active, Some(layout.desk_setups.setups[0].id.clone()));
    assert_eq!(layout.desk_setups.setups[1].monitors.len(), 1);
    assert_eq!(layout.desk_setups.setups[1].groups[&2].preferred_size_dip, Some((900, 700)));
    assert_eq!(evaluate(&mut layout, &monitors, 12).case, Case::Exact);
    assert_eq!(layout.groups[1].rect, layout.desk_setups.setups[0].groups[&2].rect);
    assert_eq!(layout.groups[1].roll, RollState::Docked);
}

#[test]
fn restart_during_recovery_recomputes_same_positions_and_returns_home() {
    let (mut layout, monitors) = learned();
    evaluate(&mut layout, &monitors[..1], 10);
    let bytes = serde_json::to_vec(&layout).unwrap();
    let mut restarted: Layout = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(evaluate(&mut restarted, &monitors[..1], 11).case, Case::Missing);
    assert_eq!(restarted.groups, layout.groups);
    assert_eq!(evaluate(&mut restarted, &monitors, 12).case, Case::Exact);
    assert_eq!(restarted.groups[1].monitor, "external");
}

#[test]
fn new_third_monitor_does_not_move_anything_until_an_edit() {
    let (mut layout, mut monitors) = learned();
    let before = layout.groups.clone();
    let bytes = saved_bytes(&layout, 0);
    monitors.push(monitor("new", 3, 5120));
    assert_eq!(evaluate(&mut layout, &monitors, 10).case, Case::New);
    assert_eq!(layout.groups, before);
    assert_eq!(saved_bytes(&layout, 0), bytes);
    layout.groups[0].monitor = "new".into();
    assert!(user_edit(&mut layout, 11, false));
    assert_eq!(layout.desk_setups.setups[1].monitors.len(), 3);
    assert_eq!(saved_bytes(&layout, 0), bytes);
}

#[test]
fn identical_monitors_follow_serials_after_connector_changes() {
    let a = connector(serial(monitor("a", 7, 0), "1111"), 1);
    let b = connector(serial(monitor("b", 7, 2560), "2222"), 2);
    let current = vec![connector(b.clone(), 1), connector(a.clone(), 2)];
    let (pairs, positional) = pair(&[a.clone(), b.clone()], &current);
    assert_eq!(pairs.iter().find(|(s, _)| *s == 0).unwrap().1, 1);
    assert_eq!(pairs.iter().find(|(s, _)| *s == 1).unwrap().1, 0);
    assert!(!positional);
    assert_eq!(decide(&current, &[setup("s", vec![a, b], 0)], None).case, Case::Exact);
}

#[test]
fn identical_monitors_use_connector_then_rank_with_a_visible_status() {
    let a = connector(monitor("a", 7, 0), 1);
    let b = connector(monitor("b", 7, 2560), 2);
    let current = vec![b.clone(), a.clone()];
    let (pairs, positional) = pair(&[a, b], &current);
    assert_eq!(pairs, vec![(0, 1), (1, 0)]);
    assert!(!positional);
    let (mut layout, mut monitors) = learned();
    monitors[1].id.model = monitors[0].id.model.clone();
    layout.desk_setups.setups[0].monitors = monitors.clone();
    monitors.reverse();
    let decision = evaluate(&mut layout, &monitors, 3);
    assert!(decision.positional);
    assert!(layout.desk_setups.status().contains("Identical monitors are matched by position"));
    assert_eq!(layout.groups[0].monitor, "laptop");
    assert_eq!(layout.groups[1].monitor, "external");
}

#[test]
fn duplicated_fake_serials_are_discarded_and_rank_pairs_the_remaining_monitor() {
    let a = serial(monitor("a", 7, 0), "fake");
    let b = serial(monitor("b", 7, 2560), "fake");
    assert!(normalize(vec![a.clone(), b.clone()]).iter().all(|m|m.id.serial_hash.is_none()));
    let saved = normalize(vec![a, b]);
    let mut remaining = saved[1].clone(); remaining.offset_px = (0, 0);
    let decision = decide(&[remaining], &[setup("s", saved, 0)], None);
    assert_eq!(decision.case, Case::Missing);
    assert_eq!(decision.pairs, vec![(0, 0)]);
    assert!(decision.positional);
}

#[test]
fn known_different_serials_never_pair_even_with_the_same_connector() {
    let a = connector(serial(monitor("a", 7, 0), "111"), 1);
    let b = connector(serial(monitor("b", 7, 0), "222"), 1);
    assert!(pair(&[a], &[b]).0.is_empty());
}

#[test]
fn port_change_without_serial_still_pairs_by_model() {
    let saved = connector(monitor("old-name", 7, 0), 1);
    let current = connector(monitor("renumbered", 7, 0), 9);
    assert_eq!(decide(&[current], &[setup("s", vec![saved], 0)], None).case, Case::Exact);
}

#[test]
fn missing_edid_pairs_equal_sizes_by_rank_but_not_distinct_known_models() {
    let mut a = monitor("a", 1, 0); a.id.model = None;
    let mut b = monitor("b", 2, 2560); b.id.model = None;
    assert_eq!(pair(&[a.clone(), b.clone()], &[b, a]).0, vec![(0, 1), (1, 0)]);
    let mut different_size = monitor("different", 1, 0); different_size.id.model = None;
    different_size.size_px = (1920, 1080);
    assert!(pair(&[MonitorState { id: MonitorIdentity::default(), ..monitor("saved", 1, 0) }], &[different_size]).0.is_empty());
    assert!(pair(&[monitor("a", 1, 0)], &[monitor("b", 2, 0)]).0.is_empty());
}

#[test]
fn primary_and_work_area_changes_remain_exact_and_do_not_rewrite_saved_content() {
    let (mut layout, mut monitors) = learned();
    let bytes = saved_bytes(&layout, 0);
    monitors[0].primary = false; monitors[1].primary = true;
    monitors[0].work = Rect::new(-400, -900, 2160, 500);
    assert_eq!(evaluate(&mut layout, &monitors, 3).case, Case::Exact);
    assert_eq!(saved_bytes(&layout, 0), bytes);
    assert!(!user_edit(&mut layout, 4, false));
    assert_eq!(layout.desk_setups.setups.len(), 1);
}

#[test]
fn resolution_scale_and_arrangement_changes_are_temporary_and_keep_preferred_size() {
    for change in 0..3 {
        let (mut layout, original) = learned();
        let bytes = saved_bytes(&layout, 0);
        let mut current = original.clone();
        match change { 0 => current[1].size_px = (1280, 720),
                       1 => current[1].scale_pct = 250,
                       _ => current[1].offset_px = (0, 1440) }
        assert_eq!(evaluate(&mut layout, &current, 3).case, Case::Adjusted);
        assert_eq!(saved_bytes(&layout, 0), bytes);
        // Existing fit machinery reports the displayed baseline; a fit is not an edit.
        layout.groups[1].rect = Rect::new(40, 80, 340, 280);
        acknowledge_display(&mut layout);
        assert!(!user_edit(&mut layout, 4, false));
        assert_eq!(saved_bytes(&layout, 0), bytes);
        assert_eq!(evaluate(&mut layout, &original, 5).case, Case::Exact);
        assert_eq!(layout.groups[1].preferred_size_dip, Some((900, 700)));
        assert_eq!(layout.groups[1].rect, layout.desk_setups.setups[0].groups[&2].rect);
    }
}

#[test]
fn first_edit_in_an_adjusted_state_creates_a_setup_and_leaves_the_base_unchanged() {
    let (mut layout, original) = learned();
    let bytes = saved_bytes(&layout, 0);
    let mut changed = original.clone(); changed[1].scale_pct = 150;
    assert_eq!(evaluate(&mut layout, &changed, 3).case, Case::Adjusted);
    layout.groups[0].rect.left += 9;
    assert!(user_edit(&mut layout, 4, false));
    assert_eq!(layout.desk_setups.setups.len(), 2);
    assert_eq!(saved_bytes(&layout, 0), bytes);
    assert_eq!(layout.desk_setups.active.as_deref(), Some(layout.desk_setups.setups[1].id.as_str()));
    assert_eq!(evaluate(&mut layout, &changed, 5).case, Case::Exact);
    assert_eq!(evaluate(&mut layout, &original, 6).case, Case::Exact);
    assert_eq!(layout.desk_setups.active.as_deref(), Some("s1"));
}

#[test]
fn negative_origins_are_normalized_before_exact_comparison() {
    let make = |dx, dy| vec![
        Monitor { name: "left".into(), bounds: Rect::new(-2560+dx, -1440+dy, dx, dy), work: Rect::new(-2560+dx, -1440+dy, dx, dy), dpi: 96 },
        Monitor { name: "right".into(), bounds: Rect::new(dx, dy, 2560+dx, 1440+dy), work: Rect::new(dx, dy, 2560+dx, 1440+dy), dpi: 96 }];
    let saved = fallback_monitors(&make(0, 0));
    let current = fallback_monitors(&make(500, -1000));
    assert_eq!(decide(&current, &[setup("s", saved, 0)], None).case, Case::Exact);
}

#[test]
fn recovery_uses_saved_centres_and_rank_even_if_current_arrangement_changes() {
    let saved = vec![monitor("left", 1, 0), monitor("missing", 2, 2560), monitor("right", 3, 5120)];
    let current = vec![monitor("right", 3, 0), monitor("left", 1, 2560)];
    assert_eq!(recovery_target(1, &saved, &current, &[(0, 1), (2, 0)]), Some(1));
}

#[test]
fn repeated_evaluations_and_settling_do_not_write_and_each_edit_is_saved_once() {
    let (mut layout, monitors) = learned();
    let bytes = saved_bytes(&layout, 0);
    for now in 3..20 {
        evaluate(&mut layout, &monitors, now);
        assert!(!user_edit(&mut layout, now, false));
    }
    assert_eq!(saved_bytes(&layout, 0), bytes);
    layout.groups[0].rect.left += 7;
    assert!(!user_edit(&mut layout, 21, true));
    assert_eq!(saved_bytes(&layout, 0), bytes);
    assert!(user_edit(&mut layout, 22, false));
    let once = saved_bytes(&layout, 0);
    assert!(!user_edit(&mut layout, 23, false));
    assert_eq!(saved_bytes(&layout, 0), once);
}

#[test]
fn seventeenth_setup_forgets_least_recent_nonactive_and_keeps_active_base() {
    let (mut layout, original) = learned();
    for n in 1..16 {
        let mut s = setup(&format!("extra-{n}"), vec![monitor("other", n + 10, 0)], n as i64 + 10);
        s.groups = layout.desk_setups.setups[0].groups.clone();
        layout.desk_setups.setups.push(s);
    }
    // The active base is oldest and must survive the LRU eviction.
    let mut changed = original.clone(); changed[0].scale_pct = 125;
    evaluate(&mut layout, &changed, 50);
    layout.groups[0].rect.left += 3;
    assert!(user_edit(&mut layout, 51, false));
    assert_eq!(layout.desk_setups.setups.len(), 16);
    assert!(layout.desk_setups.setups.iter().any(|s|s.id == "s1"));
    assert!(!layout.desk_setups.setups.iter().any(|s|s.id == "extra-1"));
}

fn legacy_entry() -> Value {
    json!({"displays":[{"device":"legacy","bounds":[0,0,2560,1440],"dpi":96}],
           "groups":{"1":{"monitor":"legacy","rect":[40,80,540,380],"roll":"Manual","dock_top":true,"dock_bottom":false}},
           "loose":{},"last_used_ms":10,"modified_ms":9,"established":{"opaque":true},"obsolete":false})
}

#[test]
fn migration_preserves_flags_extensions_unknown_monitor_and_invalid_entries() {
    let mut layout = Layout::new();
    let mut good = legacy_entry();
    good["future"] = json!({"keep":1});
    good["groups"]["1"]["monitor"] = json!("missing-name");
    good["groups"]["1"]["future_placement"] = json!([1,2]);
    let mut zero = legacy_entry(); zero["displays"][0]["bounds"] = json!([0,0,0,100]);
    let mut zero_scale = legacy_entry(); zero_scale["displays"][0]["dpi"] = json!(0);
    layout.profiles = json!({"good":good,"wrong_type":[1,2],"zero":zero,"zero_scale":zero_scale,"empty":{"displays":[]}});
    layout.active_profile = "nonexistent".into();
    migrate(&mut layout);
    assert_eq!(layout.desk_setups.setups.len(), 1);
    let s = &layout.desk_setups.setups[0];
    assert_eq!(s.origin, "legacy");
    assert_eq!(s.legacy_key.as_deref(), Some("good"));
    assert_eq!(s.legacy.as_ref().unwrap()["established"], json!({"opaque":true}));
    assert_eq!(s.groups[&1].monitor, None);
    assert_eq!(s.extra["future"], json!({"keep":1}));
    assert_eq!(s.groups[&1].extra["future_placement"], json!([1,2]));
    assert_eq!(layout.desk_setups.legacy_unparsed.len(), 4);
    assert_eq!(layout.desk_setups.legacy_unparsed["zero"], zero);
    assert_eq!(layout.desk_setups.active, None);
    assert!(layout.profiles.is_null()); assert!(layout.active_profile.is_empty());
}

#[test]
fn migration_cycles_self_references_and_archives_do_not_drive_matching() {
    let mut a = legacy_entry(); a["downsized_to"] = json!("b");
    let mut b = legacy_entry(); b["downsized_to"] = json!("a");
    let mut own = legacy_entry(); own["downsized_to"] = json!("own");
    let mut archived = legacy_entry(); archived["obsolete"] = json!(true);
    let mut layout = Layout::new();
    layout.profiles = json!({"a":a,"b":b,"own":own,"archived":archived});
    migrate(&mut layout);
    assert_eq!(layout.desk_setups.setups.len(), 4);
    for s in layout.desk_setups.setups.iter().filter(|s|!s.archived()) {
        assert!(s.legacy.as_ref().unwrap().get("downsized_to").is_none());
    }
    assert_eq!(layout.desk_setups.visible().len(), 3);
    let archived = layout.desk_setups.setups.iter().find(|s|s.archived()).unwrap().clone();
    assert_eq!(decide(&[monitor("legacy", 7, 0)], &[archived], None).case, Case::Unknown);
}

#[test]
fn missing_empty_and_wrong_type_legacy_tables_are_safe_and_idempotent() {
    for raw in [Value::Null, json!({}), json!([1]), json!("broken")] {
        let mut layout = Layout::new(); layout.profiles = raw.clone();
        migrate(&mut layout);
        assert!(layout.desk_setups.setups.is_empty());
        if !raw.is_null() && !raw.is_object() {
            assert_eq!(layout.desk_setups.legacy_unparsed["profiles"], raw);
        }
        let before = serde_json::to_vec(&layout.desk_setups).unwrap();
        migrate(&mut layout);
        assert_eq!(serde_json::to_vec(&layout.desk_setups).unwrap(), before);
    }
}

#[test]
fn legacy_identity_upgrade_requires_an_exact_settled_match() {
    let mut layout = Layout::new();
    layout.profiles = json!({"1:[2560x1440@100]+":legacy_entry()});
    layout.active_profile = "1:[2560x1440@100]+".into();
    migrate(&mut layout);
    let mut wrong_mode = monitor("legacy", 7, 0); wrong_mode.size_px = (1920,1080);
    assert_ne!(evaluate(&mut layout, &[wrong_mode], 20).case, Case::Exact);
    assert_eq!(layout.desk_setups.setups[0].origin, "legacy");
    let renamed = monitor("renamed", 7, 0);
    assert_eq!(evaluate(&mut layout, &[renamed], 21).case, Case::Exact);
    assert_eq!(layout.desk_setups.setups[0].origin, "learned");
    assert_eq!(layout.desk_setups.setups[0].monitors[0].id.model.as_ref().unwrap().product, 7);
}

#[test]
fn backup_old_shape_once_preserves_original_bytes_and_restored_backup_migrates() {
    let root = std::env::current_dir().unwrap().join("target").join("qa").join("c2-tests")
        .join(format!("backup-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("layout.json");
    let mut layout = Layout::new(); layout.profiles = json!({"old":legacy_entry(),"bad":false});
    let original = serde_json::to_vec_pretty(&layout).unwrap();
    std::fs::write(&path, &original).unwrap();
    ensure_migration_backup(&path).unwrap();
    let backup = migration_backup_path(&path);
    assert_eq!(std::fs::read(&backup).unwrap(), original);
    let mut restored: Layout = serde_json::from_slice(&std::fs::read(&backup).unwrap()).unwrap();
    migrate(&mut restored);
    assert_eq!(restored.desk_setups.setups.len(), 1);
    assert_eq!(restored.desk_setups.legacy_unparsed["bad"], json!(false));
    std::fs::write(&path, b"{\"profiles\":{}}").unwrap();
    ensure_migration_backup(&path).unwrap();
    assert_eq!(std::fs::read(&backup).unwrap(), original);
    // Scratch is kept for review; this test never removes a user path.
}

#[test]
fn learned_identityless_setup_can_gain_identity_after_restart_without_guessing() {
    let mut current = vec![monitor("remote-display", 7, 0)];
    current[0].id.model = None;
    current[0].id.serial_hash = None;
    let mut layout = Layout::new();
    layout.add_group("Remote".into(), "remote-display".into(), Rect::new(50,80,550,380),
        GroupContent::Items { items: vec![] });
    evaluate(&mut layout, &current, 1);
    layout.groups[0].rect.left += 1;
    assert!(user_edit(&mut layout, 2, false));
    let mut restarted: Layout = serde_json::from_slice(&serde_json::to_vec(&layout).unwrap()).unwrap();
    let physical = vec![serial(monitor("remote-display", 7, 0), "hashed-device")];
    assert_eq!(evaluate(&mut restarted, &physical, 3).case, Case::Exact);
    assert_eq!(restarted.desk_setups.setups[0].monitors[0].id, physical[0].id);
}

#[test]
fn edit_in_a_permuted_exact_match_is_stored_against_the_saved_monitor_order() {
    let a = serial(monitor("a", 7, 0), "1111");
    let b = serial(monitor("b", 7, 2560), "2222");
    let mut layout = Layout::new();
    layout.add_group("on-a".into(), "a".into(), Rect::new(40, 80, 540, 380), GroupContent::Items { items: vec![] });
    layout.add_group("on-b".into(), "b".into(), Rect::new(40, 80, 540, 380), GroupContent::Items { items: vec![] });
    evaluate(&mut layout, &[a.clone(), b.clone()], 1);
    layout.groups[0].rect.left += 1;
    assert!(user_edit(&mut layout, 2, false));
    // The same two monitors now enumerate in the other order and under other GDI names.
    let mut b2 = b.clone(); b2.gdi_name = "x".into();
    let mut a2 = a.clone(); a2.gdi_name = "y".into();
    assert_eq!(evaluate(&mut layout, &[b2.clone(), a2.clone()], 3).case, Case::Exact);
    assert_eq!((layout.groups[0].monitor.as_str(), layout.groups[1].monitor.as_str()), ("y", "x"));
    layout.groups[1].rect.left += 5;
    assert!(user_edit(&mut layout, 4, false));
    let stored = &layout.desk_setups.setups[0];
    assert_eq!((stored.groups[&1].monitor, stored.groups[&2].monitor), (Some(0), Some(1)));
    assert_eq!(evaluate(&mut layout, &[a, b], 5).case, Case::Exact);
    assert_eq!((layout.groups[0].monitor.as_str(), layout.groups[1].monitor.as_str()), ("a", "b"));
}

#[test]
fn deleting_the_last_corral_in_recovery_still_creates_a_setup_for_the_current_monitors() {
    let (mut layout, monitors) = learned();
    evaluate(&mut layout, &monitors[..1], 10);
    layout.groups.clear();
    assert!(user_edit(&mut layout, 11, false));
    assert_eq!(layout.desk_setups.setups.len(), 2);
    assert_eq!(layout.desk_setups.setups[1].monitors.len(), 1);
}

fn scratch(name: &str) -> PathBuf {
    let root = std::env::current_dir().unwrap().join("target").join("qa").join("c2-tests")
        .join(format!("{name}-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn old_shape_file_keeps_its_keys_until_the_backup_exists_and_is_dropped_only_afterwards() {
    let path = scratch("model-save").join("layout.json");
    let mut old = Layout::new();
    old.profiles = json!({"old": legacy_entry(), "bad": false});
    old.active_profile = "old".into();
    std::fs::write(&path, serde_json::to_vec_pretty(&old).unwrap()).unwrap();
    let original = std::fs::read(&path).unwrap();
    let loaded = crate::model::load(&path).unwrap();
    assert_eq!(loaded.desk_setups.setups.len(), 1);
    assert!(loaded.desk_setups.active.is_some());
    // A blocked backup refuses the write and leaves the old file untouched.
    std::fs::create_dir(migration_backup_path(&path)).unwrap();
    assert!(crate::model::save(&path, &loaded).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    std::fs::remove_dir(migration_backup_path(&path)).unwrap();
    crate::model::save(&path, &loaded).unwrap();
    assert_eq!(std::fs::read(migration_backup_path(&path)).unwrap(), original);
    let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(written.get("profiles").is_none() && written.get("active_profile").is_none());
    assert_eq!(written["desk_setups"]["setups"].as_array().unwrap().len(), 1);
    assert_eq!(written["desk_setups"]["legacy_unparsed"]["bad"], json!(false));
    // Later saves neither touch the one-time copy nor migrate again.
    crate::model::save(&path, &loaded).unwrap();
    assert_eq!(std::fs::read(migration_backup_path(&path)).unwrap(), original);
    assert_eq!(crate::model::load(&path).unwrap().desk_setups.setups, loaded.desk_setups.setups);
}

#[test]
fn a_corrupt_existing_file_does_not_block_saving() {
    let path = scratch("corrupt").join("layout.json");
    std::fs::write(&path, b"{ not json").unwrap();
    crate::model::save(&path, &Layout::new()).unwrap();
    assert!(!migration_backup_path(&path).exists());
    assert!(crate::model::load(&path).is_ok());
}

#[test]
fn drag_end_without_rescue_is_a_user_edit_that_survives_the_next_exact_restore() {
    // Escape and capture loss end a drag with the geometry already moved; only a display-change cancel is not an edit.
    assert!(records_drag_end(true, false));
    assert!(!records_drag_end(false, false));
    assert!(!records_drag_end(true, true));
    let (mut layout, monitors) = learned();
    layout.groups[0].rect = Rect::new(300, 300, 800, 600);
    assert!(user_edit(&mut layout, 5, false));
    assert_eq!(evaluate(&mut layout, &monitors, 6).case, Case::Exact);
    assert_eq!(layout.groups[0].rect, Rect::new(300, 300, 800, 600));
    assert_eq!(layout.desk_setups.setups.len(), 1);
}

#[test]
fn display_cancelled_drag_is_not_saved_into_any_setup() {
    let (mut layout, monitors) = learned();
    let bytes = saved_bytes(&layout, 0);
    // The cancel happens during the settle, whose baseline is the pre-change layout.
    layout.groups[0].rect = Rect::new(300, 300, 800, 600);
    assert!(!user_edit(&mut layout, 5, true));
    assert_eq!(saved_bytes(&layout, 0), bytes);
    assert_eq!(evaluate(&mut layout, &monitors, 6).case, Case::Exact);
    assert_eq!(layout.groups[0].rect, layout.desk_setups.setups[0].groups[&1].rect);
}

#[test]
fn layout_made_while_setups_were_off_is_adopted_not_overwritten_and_survives_restart() {
    let (mut layout, monitors) = learned();
    // LayoutPreserve 0/1: the user rearranges; the engine neither reads nor writes setups.
    layout.groups[0].rect = Rect::new(500, 400, 900, 700);
    layout.groups[1].monitor = "laptop".into();
    layout.groups[1].roll = RollState::Expanded;
    let live: Vec<_> = layout.groups.iter().map(|g| (g.monitor.clone(), g.rect, g.roll)).collect();
    // Turned on again: the live arrangement is saved into the Exact setup, never replaced by the old one.
    assert!(adopt(&mut layout, &monitors, 10));
    assert_eq!(layout.desk_setups.setups.len(), 1);
    assert_eq!(layout.groups.iter().map(|g| (g.monitor.clone(), g.rect, g.roll)).collect::<Vec<_>>(), live);
    let mut restarted: Layout = serde_json::from_slice(&serde_json::to_vec(&layout).unwrap()).unwrap();
    for g in &mut restarted.groups { g.rect = Rect::default(); }
    assert_eq!(evaluate(&mut restarted, &monitors, 11).case, Case::Exact);
    assert_eq!(restarted.groups.iter().map(|g| (g.monitor.clone(), g.rect, g.roll)).collect::<Vec<_>>(), live);
    // Nothing differs: adopting again changes nothing.
    assert!(!adopt(&mut layout, &monitors, 12));
}

#[test]
fn adopting_with_other_monitors_creates_a_setup_and_keeps_the_old_one() {
    let (mut layout, monitors) = learned();
    let bytes = saved_bytes(&layout, 0);
    layout.groups[1].rect = Rect::new(60, 60, 360, 260);
    assert!(adopt(&mut layout, &monitors[..1], 10));
    assert_eq!(layout.desk_setups.setups.len(), 2);
    assert_eq!(saved_bytes(&layout, 0), bytes);
    assert_eq!(layout.groups[1].rect, Rect::new(60, 60, 360, 260));
    // an empty layout adopts nothing
    let mut empty = Layout::new();
    assert!(!adopt(&mut empty, &monitors, 1));
    assert!(empty.desk_setups.setups.is_empty());
}

#[test]
fn a_pending_edit_defers_saves_only_while_setups_are_on_and_not_settling() {
    assert!(defers_save(true, false, true));
    assert!(!defers_save(false, false, true));
    assert!(!defers_save(true, true, true));
    assert!(!defers_save(true, false, false));
}

#[test]
fn legacy_backup_is_taken_from_bak_when_the_primary_file_does_not_parse() {
    let path = scratch("bak-fallback").join("layout.json");
    let mut old = Layout::new();
    old.profiles = json!({"old": legacy_entry()});
    let bytes = serde_json::to_vec_pretty(&old).unwrap();
    std::fs::write(crate::model::backup_path(&path), &bytes).unwrap();
    std::fs::write(&path, b"{ broken").unwrap();
    let loaded = crate::model::load_with_backup(&path).unwrap();
    assert_eq!(loaded.desk_setups.setups.len(), 1);
    crate::model::save(&path, &loaded).unwrap();
    assert_eq!(std::fs::read(migration_backup_path(&path)).unwrap(), bytes);
    // the save replaced .bak with the broken primary, but the byte copy of the legacy data survives
    assert_eq!(std::fs::read(crate::model::backup_path(&path)).unwrap(), b"{ broken");
}

#[test]
fn migration_keeps_the_stored_work_area_of_each_display() {
    let mut entry = legacy_entry();
    entry["displays"][0]["work"] = json!([0, 0, 2560, 1400]);
    let mut layout = Layout::new();
    layout.profiles = json!({"k": entry});
    migrate(&mut layout);
    let m = &layout.desk_setups.setups[0].monitors[0];
    assert_eq!(m.legacy_work, Some(Rect::new(0, 0, 2560, 1400)));
    let again: DeskSetups = serde_json::from_slice(&serde_json::to_vec(&layout.desk_setups).unwrap()).unwrap();
    assert_eq!(again.setups[0].monitors[0].legacy_work, Some(Rect::new(0, 0, 2560, 1400)));
}

#[test]
fn legacy_identity_follows_the_pairing_not_the_gdi_names() {
    let display = |device: &str, left: i32| json!({"device": device, "bounds": [left, 0, left + 2560, 1440], "dpi": 96});
    let entry = json!({"displays": [display("L", 0), display("R", 2560)], "groups": {}, "loose": {}});
    let key = "2:[2560x1440@100]+[2560x1440@100]";
    let mut layout = Layout::new();
    layout.profiles = json!({ key: entry });
    migrate(&mut layout);
    // Same two equal-size monitors, but Windows names the left one "R" and the right one "L".
    let left = monitor("R", 1, 0);
    let right = monitor("L", 2, 2560);
    let current = vec![right.clone(), left.clone()];
    assert_eq!(evaluate(&mut layout, &current, 5).case, Case::Exact);
    let s = &layout.desk_setups.setups[0];
    assert_eq!(s.origin, "learned");
    assert_eq!(s.monitors[0].id, left.id);
    assert_eq!(s.monitors[1].id, right.id);
}

#[test]
fn request_paths_compare_equal_across_spelling() {
    let a = normalized_path(Path::new(r"C:\Users\user\Layouts\layout.json"));
    assert_eq!(a, normalized_path(Path::new("c:/users/user/layouts/LAYOUT.json")));
    assert_eq!(a, normalized_path(Path::new(r"\\?\C:\Users\user\Layouts\layout.json")));
    assert_ne!(a, normalized_path(Path::new(r"C:\Users\user\Layouts\other.json")));
}

#[test]
fn desk_mode_marker_round_trips_and_is_absent_by_default() {
    let mut layout = Layout::new();
    assert!(!serde_json::to_string(&layout).unwrap().contains("\"mode\""));
    layout.desk_setups.mode = Some(0);
    let back: Layout = serde_json::from_slice(&serde_json::to_vec(&layout).unwrap()).unwrap();
    assert_eq!(back.desk_setups.mode, Some(0));
}

#[test]
fn failed_backup_does_not_count_an_existing_directory_as_a_successful_copy() {
    let root = std::env::current_dir().unwrap().join("target").join("qa").join("c2-tests")
        .join(format!("failed-backup-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("layout.json");
    std::fs::write(&path, b"{\"profiles\":{}}").unwrap();
    std::fs::create_dir(migration_backup_path(&path)).unwrap();
    assert!(ensure_migration_backup(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"profiles\":{}}");
}
