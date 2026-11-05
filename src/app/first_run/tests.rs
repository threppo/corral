use super::*;
use crate::item_facts::{
    TargetFacts,
    fake::{FakeTree, item},
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

fn facts(name: &str, kind: ItemType, target: Option<TargetClass>) -> ItemFacts {
    ItemFacts {
        name: name.into(),
        file_name: name.into(),
        kind,
        size: None,
        modified_ms: None,
        target: target.map(|class| TargetFacts {
            path: "C:\\target\\document.pdf".into(),
            class,
        }),
    }
}

fn monitor(dpi: u32, width: i32, height: i32) -> Monitor {
    let work = Rect::new(-1800, 40, -1800 + width, 40 + height);
    Monitor {
        name: "TEST".into(),
        bounds: work,
        work,
        dpi,
    }
}

fn cells(dpi: u32) -> CellMetrics {
    icon_metrics::cell_metrics(&icon_metrics::CellInputs {
        dpi,
        zoom_pct: 100,
        windows_spacing_dip: Some((75., 80.)),
        desktop_icon_dip: Some(48.),
        match_desktop: true,
    })
}

fn arrange(layout: &mut Layout, facts: &[ItemFacts], gate: bool, monitor: &Monitor) {
    let mut counts = [0; 3];
    for fact in facts {
        if let Some(c) = classify(fact) {
            counts[c as usize] += 1;
        }
    }
    let rects = placements(
        counts,
        gate,
        monitor.work,
        cells(monitor.dpi),
        to_px(36, monitor.dpi),
        monitor.dpi,
    );
    sort(
        layout,
        facts,
        monitor,
        rects,
        gate.then(|| "C:\\Downloads".into()),
        0,
        123,
    );
    acknowledge(layout, facts.iter().map(|fact| fact.name.as_str()));
}

#[test]
fn classifier_covers_all_kinds_and_extensions() {
    use Category::*;
    for (name, kind, target, expected) in [
        ("::{RecycleBin}", ItemType::SystemItem, None, None),
        ("::{ThisPC}", ItemType::Folder, None, None),
        ("Projects", ItemType::Folder, None, Some(Folders)),
        (
            "folder.lnk",
            ItemType::Shortcut,
            Some(TargetClass::Folder),
            Some(Folders),
        ),
        (
            "program.lnk",
            ItemType::Shortcut,
            Some(TargetClass::Program),
            Some(Programs),
        ),
        (
            "file.lnk",
            ItemType::Shortcut,
            Some(TargetClass::File),
            Some(Files),
        ),
        ("unknown.lnk", ItemType::Shortcut, None, Some(Programs)),
        ("odd.shortcut", ItemType::Shortcut, None, Some(Programs)),
        (
            "web.url",
            ItemType::Shortcut,
            Some(TargetClass::Web),
            Some(Programs),
        ),
        (
            "local.url",
            ItemType::Shortcut,
            Some(TargetClass::File),
            Some(Programs),
        ),
        ("report.pdf", ItemType::File, None, Some(Files)),
        ("README", ItemType::File, None, Some(Files)),
    ] {
        assert_eq!(classify(&facts(name, kind, target)), expected, "{name}");
    }
    for ext in ["EXE", "lnk", "url", "appref-ms", "bat", "cmd", "msi"] {
        assert_eq!(
            classify(&facts(&format!("app.{ext}"), ItemType::File, None)),
            Some(Programs)
        );
    }
    for ext in ["exe", "com", "bat", "cmd", "msi", "msc", "cpl", "appref-ms", "scr", "pif"] {
        let mut f = facts("installer.lnk", ItemType::Shortcut, Some(TargetClass::File));
        f.target.as_mut().unwrap().path = format!("C:\\target\\app.{ext}");
        assert_eq!(classify(&f), Some(Programs));
    }
}

#[test]
fn existing_read_only_facts_resolve_folder_document_program_and_missing_shortcuts() {
    let tree = FakeTree::default()
        .folder("C:\\folder")
        .file("C:\\file.pdf", 1, None)
        .file("C:\\program.exe", 1, None)
        .link("folder.lnk", "C:\\folder")
        .link("file.lnk", "C:\\file.pdf")
        .link("app.lnk", "C:\\program.exe")
        .link("unknown.lnk", "C:\\missing");
    let before = tree.fingerprint();
    for (name, category) in [
        ("folder.lnk", Category::Folders),
        ("file.lnk", Category::Files),
        ("app.lnk", Category::Programs),
        ("unknown.lnk", Category::Programs),
    ] {
        assert_eq!(
            classify(&crate::item_facts::collect_with(&item(name), &tree)),
            Some(category)
        );
    }
    assert_eq!(tree.fingerprint(), before);
}

#[test]
fn qa_requires_explicit_opt_in_and_showing_is_not_offered_twice() {
    for (fixture, opt_in, eligible) in [
        (false, false, true),
        (false, true, true),
        (true, false, false),
        (true, true, true),
    ] {
        let mut offer = Offer::Pending;
        offer.filter_startup(false, false, fixture, opt_in);
        assert_eq!(offer.waiting(), eligible);
        assert_eq!(offer.begin(true), eligible);
        assert!(!offer.begin(true));
        assert_eq!(
            offer.waiting(),
            eligible,
            "Round-up stays paused throughout the dialog"
        );
    }
    let mut offer = Offer::No;
    offer.filter_startup(false, false, true, true);
    assert!(
        !offer.begin(true),
        "QA override cannot promote a non-first run"
    );
}

#[test]
fn disabled_and_existing_settings_filter_the_offer_even_without_layouts() {
    for (disabled, settings_existed, qa, expected) in [
        (true, false, false, Offer::No),
        (true, false, true, Offer::No),
        (true, true, true, Offer::No),
        (false, true, false, Offer::No),
        (false, true, true, Offer::Pending),
        (false, false, false, Offer::Pending),
    ] {
        let mut offer = Offer::Pending;
        offer.filter_startup(disabled, settings_existed, false, qa);
        assert_eq!(offer, expected);
    }

    let dir = Scratch::new();
    let settings_path = dir.0.join("settings.json");
    std::fs::write(&settings_path, b"{}").unwrap();
    let (_, mut recovery) = super::super::layout_startup::load(&dir.0.join("layout.json"), "test");
    recovery
        .first_run
        .filter_startup(false, settings_path.exists(), false, false);
    assert_eq!(recovery.first_run, Offer::No);
}

#[test]
fn declining_or_closing_never_calls_the_injected_sort_lookup() {
    for answer in [Action::Secondary, Action::Closed] {
        let names = answer_items(
            answer,
            vec![item("unreachable.lnk"), item("keep.txt")],
            |_| panic!("declining must never start fact or target lookups"),
        )
        .unwrap();
        assert_eq!(names, ["unreachable.lnk", "keep.txt"]);
        let mut layout = Layout::new();
        acknowledge(&mut layout, names.iter().map(String::as_str));
        assert!(layout.is_seen("unreachable.lnk") && layout.is_seen("keep.txt"));
        assert!(layout.groups.is_empty());
    }
    let mut called = false;
    assert!(
        answer_items(Action::Primary, vec![item("sort.lnk")], |items| {
            assert_eq!(items[0].parsing_name, "sort.lnk");
            called = true;
        })
        .is_none()
    );
    assert!(called);
}

#[test]
fn first_run_unc_targets_fall_back_to_programs_without_metadata_lookups() {
    for target in [
        r"\\server\share\app.exe",
        r"\\server\share\folder",
        r"\\?\UNC\server\share\file.pdf",
    ] {
        let tree = FakeTree::default()
            .link("remote.lnk", target)
            .folder(target);
        let facts = crate::item_facts::collect_first_run_with(&item("remote.lnk"), &tree);
        assert_eq!(facts.target, None);
        assert_eq!(classify(&facts), Some(Category::Programs));
        assert_eq!(*tree.reads.borrow(), ["remote.lnk"]);
        // Round-up continues to read/classify the same target according to its own rules.
        let facts = crate::item_facts::collect_with(&item("remote.lnk"), &tree);
        assert_eq!(facts.target.unwrap().class, TargetClass::Folder);
        assert_eq!(*tree.reads.borrow(), ["remote.lnk", "remote.lnk", target]);
    }
}

#[test]
fn first_run_local_target_facts_match_round_up() {
    let tree = FakeTree::default()
        .folder(r"C:\folder")
        .file(r"C:\file.pdf", 1, None)
        .file(r"C:\program.exe", 1, None)
        .link("folder.lnk", r"C:\folder")
        .link("file.lnk", r"C:\file.pdf")
        .link("app.lnk", r"C:\program.exe")
        .link("unknown.lnk", r"C:\missing");
    for name in ["folder.lnk", "file.lnk", "app.lnk", "unknown.lnk"] {
        assert_eq!(
            crate::item_facts::collect_first_run_with(&item(name), &tree),
            crate::item_facts::collect_with(&item(name), &tree)
        );
    }
}

#[test]
fn delayed_sort_applies_by_name_ignoring_removed_items_and_new_arrivals() {
    let mut batch = vec![
        facts("gone.exe", ItemType::File, None),
        facts("kept.pdf", ItemType::File, None),
    ];
    let mut layout = Layout::new();
    for name in ["kept.pdf", "new.txt"] {
        layout.place_loose(
            name,
            crate::model::LoosePosition {
                monitor: "TEST".into(),
                x: 15,
                y: 35,
            },
        );
    }
    retain_present(&mut batch, ["KEPT.PDF", "new.txt"].into_iter());
    arrange(&mut layout, &batch, false, &monitor(96, 1200, 900));
    assert_eq!(
        layout.groups.len(),
        1,
        "no empty Programs Corral for the removed item"
    );
    assert_eq!(layout.groups[0].title, "Files");
    assert_eq!(layout.group_of("kept.pdf"), Some(layout.groups[0].id));
    assert_eq!(layout.group_of("gone.exe"), None);
    assert!(!layout.is_seen("gone.exe") && !layout.is_manual("gone.exe"));
    assert!(layout.loose.contains_key("new.txt"));
    assert!(!layout.is_seen("new.txt") && !layout.is_manual("new.txt"));
}

#[test]
fn worker_result_is_owned_and_quitting_discards_it_without_notifying() {
    let (sender, receiver) = std::sync::mpsc::channel();
    drop(receiver); // App has quit before the worker finishes.
    deliver(
        Ok(Sorted {
            facts: vec![],
            downloads: None,
        }),
        &sender,
        || panic!("a dropped receiver must not post to the old message window"),
    );

    let (sender, receiver) = std::sync::mpsc::channel();
    let mut notified = false;
    deliver(
        Ok(Sorted {
            facts: vec![facts("a.txt", ItemType::File, None)],
            downloads: None,
        }),
        &sender,
        || notified = true,
    );
    assert!(notified);
    assert_eq!(receiver.try_recv().unwrap().unwrap().facts[0].name, "a.txt");
    // Quitting after notification but before dispatch also safely drops the queued owned result.
    deliver(
        Ok(Sorted {
            facts: vec![],
            downloads: None,
        }),
        &sender,
        || {},
    );
    drop(receiver);

    let mut offer = Offer::Sorting;
    assert!(offer.waiting(), "Round-up stays paused until application");
    assert!(
        !offer.begin(true),
        "sorting must never open a second dialog"
    );
}

#[test]
fn no_real_items_leave_the_offer_unanswered_for_a_later_start() {
    for names in [vec![], vec!["::{RecycleBin}", "::{ThisPC}"]] {
        let dir = Scratch::new();
        let path = dir.0.join("layout.json");
        let (layout, mut recovery) = super::super::layout_startup::load(&path, "unpopulated");
        let original = layout.clone();
        let has_real_items = names.iter().any(|name| !name.starts_with("::"));
        assert!(!recovery.first_run.begin(has_real_items), "no dialog");
        assert_eq!(recovery.first_run, Offer::Deferred);
        assert!(!recovery.first_run.waiting(), "Round-up runs this session");
        assert!(!recovery.first_run.begin(true), "only one try per start");
        assert_eq!(layout, original);
        recovery.save(&path, &layout).unwrap(); // Startup autosave or orderly quit.
        assert!(!path.exists());
        let (_, mut recovery) = super::super::layout_startup::load(&path, "populated");
        assert!(
            recovery.first_run.begin(true),
            "a later start can still ask"
        );
    }
}

#[test]
fn sort_keeps_each_item_once_virtuals_loose_and_skips_empty_categories() {
    let f = vec![
        facts("app.exe", ItemType::File, None),
        facts("report.pdf", ItemType::File, None),
        facts("folder.lnk", ItemType::Shortcut, Some(TargetClass::Folder)),
        facts("::{RecycleBin}", ItemType::SystemItem, None),
    ];
    let mut layout = Layout::new();
    for fact in &f {
        layout.place_loose(
            &fact.name,
            crate::model::LoosePosition {
                monitor: "TEST".into(),
                x: 20,
                y: 30,
            },
        );
    }
    arrange(&mut layout, &f, true, &monitor(96, 1200, 900));
    assert_eq!(
        layout
            .groups
            .iter()
            .map(|g| g.title.as_str())
            .collect::<Vec<_>>(),
        ["Programs", "Files", "Folders", "Downloads"]
    );
    for fact in &f {
        let count: usize = layout
            .groups
            .iter()
            .map(|g| match &g.content {
                GroupContent::Items { items } => items
                    .iter()
                    .filter(|n| n.eq_ignore_ascii_case(&fact.name))
                    .count(),
                _ => 0,
            })
            .sum();
        let virtual_item = fact.name.starts_with("::");
        assert_eq!(count, usize::from(!virtual_item));
        assert_eq!(
            layout.loose.contains_key(&fact.name.to_lowercase()),
            virtual_item
        );
        assert!(layout.is_seen(&fact.name));
        assert_eq!(layout.is_manual(&fact.name), !virtual_item);
    }
    assert!(layout.seen_initialized && layout.round_up_initialized);
    for group in &layout.groups {
        assert_eq!(
            group.preferred_size_dip,
            Some((group.rect.width(), group.rect.height()))
        );
    }
    let mut files_only = Layout::new();
    arrange(&mut files_only, &f[1..2], false, &monitor(96, 900, 700));
    assert_eq!(files_only.groups.len(), 1);
    assert_eq!(files_only.groups[0].title, "Files");
    let mut empty = Layout::new();
    arrange(&mut empty, &[], false, &monitor(96, 900, 700));
    assert!(empty.groups.is_empty());
    arrange(&mut empty, &[], true, &monitor(96, 900, 700));
    assert_eq!(empty.groups.len(), 1);
    assert!(matches!(
        empty.groups[0].content,
        GroupContent::Portal { .. }
    ));
}

#[test]
fn placements_fit_without_overlap_after_dip_round_trip_at_every_scale() {
    for dpi in [96, 120, 144, 168, 192, 240] {
        for (width, height) in [(800, 600), (1200, 900), (1920, 1080), (900, 320)] {
            let m = monitor(dpi, width, height);
            let strip = to_px(36, dpi);
            for counts in [[0, 0, 0], [1, 0, 0], [1, 1, 1], [345, 600, 800]] {
                for gate in [false, true] {
                    let rects = placements(counts, gate, m.work, cells(dpi), strip, dpi);
                    assert_eq!(
                        rects.iter().flatten().count(),
                        counts.iter().filter(|&&n| n > 0).count() + usize::from(gate)
                    );
                    let outer: Vec<_> = rects
                        .into_iter()
                        .flatten()
                        .map(|body| {
                            let dip = crate::creation::body_to_dip(body, m.work, dpi);
                            let body = Rect::new(
                                m.work.left + to_px(dip.left, dpi),
                                m.work.top + to_px(dip.top, dpi),
                                m.work.left + to_px(dip.right, dpi),
                                m.work.top + to_px(dip.bottom, dpi),
                            );
                            assert!(
                                body.height() >= to_px(crate::rollup::MIN_OPEN_BODY_DIP, dpi) - 2
                            );
                            Rect::new(body.left, body.top - strip, body.right, body.bottom)
                        })
                        .collect();
                    for (i, rect) in outer.iter().enumerate() {
                        assert!(rect.width() > 0 && rect.height() > strip);
                        assert_eq!(
                            rect.clamp_into(&m.work),
                            *rect,
                            "dpi={dpi} size={width}x{height}"
                        );
                        for other in outer.iter().skip(i + 1) {
                            assert_eq!(rect.overlap_area(other), 0);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn an_open_question_never_costs_the_user_an_arrangement() {
    let position = crate::model::LoosePosition { monitor: "M".into(), x: 1, y: 2 };
    for state in [Offer::Pending, Offer::Showing, Offer::Sorting, Offer::Deferred] {
        let mut layout = Layout::new();
        assert!(state.keeps_question_open(&layout));
        layout.mark_seen("c:\\desktop\\a.txt");
        assert!(state.keeps_question_open(&layout), "seen is not an arrangement");
        layout.place_loose("c:\\desktop\\a.txt", position.clone());
        assert!(!state.keeps_question_open(&layout), "a moved icon is saved");

        let mut layout = Layout::new();
        layout.icon_pages.insert("c:\\desktop\\a.txt".into(), 2);
        assert!(!state.keeps_question_open(&layout), "a Range assignment is saved");

        let mut layout = Layout::new();
        layout.desktop_locked = true;
        assert!(!state.keeps_question_open(&layout), "the lock is saved");
    }
    assert!(!Offer::No.keeps_question_open(&Layout::new()));
}

#[test]
fn only_a_coming_answer_pauses_round_up() {
    for (state, paused) in [
        (Offer::No, false),
        (Offer::Pending, true),
        (Offer::Showing, true),
        (Offer::Sorting, true),
        (Offer::Deferred, false),
    ] {
        assert_eq!(state.waiting(), paused, "{state:?}");
    }
}

#[test]
fn corrals_widen_to_show_their_items_and_the_gate_sits_beside_them() {
    let dpi = 96;
    let m = monitor(dpi, 1920, 1040);
    let cell = cells(dpi);
    let strip = to_px(36, dpi);
    let four = cell.body_size_for(4, 1).0;
    let rects = placements([40, 12, 5], true, m.work, cell, strip, dpi);
    let [programs, files, folders, gate] = rects.map(Option::unwrap);
    assert!(programs.width() > four, "a full Corral is wider than the minimum");
    assert_eq!(programs.width(), files.width());
    assert_eq!(programs.width(), folders.width());
    assert!(gate.left > programs.right);
    assert!(gate.left - programs.right < strip, "no wide gap before the Gate");
    assert!(files.top > programs.bottom && folders.top > files.bottom);
    // A short Corral above leaves its height to the fuller one below.
    let rects = placements([1, 300, 0], false, m.work, cell, strip, dpi);
    let (short, full) = (rects[0].unwrap(), rects[1].unwrap());
    assert!(full.height() > short.height());
    assert!(full.bottom <= m.work.bottom);
    // A few items keep the usual four columns.
    let rects = placements([3, 2, 1], true, m.work, cell, strip, dpi);
    assert_eq!(rects[0].unwrap().width(), four);
}

#[test]
fn several_hundred_items_keep_unique_membership_and_round_up_protection() {
    let mut f: Vec<_> = (0..750)
        .map(|i| facts(&format!("file-{i}.txt"), ItemType::File, None))
        .collect();
    f.push(f[0].clone()); // A duplicate input identity still has exactly one reference.
    let mut layout = Layout::new();
    arrange(&mut layout, &f, false, &monitor(144, 1200, 900));
    let GroupContent::Items { items } = &layout.groups[0].content else {
        panic!()
    };
    assert_eq!(items.len(), 750);
    assert_eq!(
        items
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        750
    );
    for fact in &f {
        assert!(layout.is_seen(&fact.name) && layout.is_manual(&fact.name));
    }
    let present: Vec<_> = f.iter().map(|f| f.name.clone()).collect();
    layout.round_up_observe(&present, 124);
    assert!(layout.round_up_initialized);
    for fact in &f {
        assert!(layout.is_manual(&fact.name));
    }
    let other = layout.add_group(
        "Other".into(),
        "TEST".into(),
        Rect::new(500, 100, 800, 400),
        GroupContent::Items { items: vec![] },
    );
    let rules = crate::round_up::RoundUp {
        default_destination: crate::round_up::Destination::Corral { id: other },
        ..Default::default()
    };
    let plan = crate::round_up::plan(
        &rules,
        &layout,
        &f,
        crate::round_up::Clock { now_ms: 125 },
        &crate::round_up_time::fake::Fixed(0),
        false,
    );
    assert_eq!(plan.move_count(), 0);
    assert_eq!(plan.manual_count(), f.len());
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "corral-first-run-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("temporary fixture: {e}"),
            }
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        // Only this exclusively created fixture's immediate files/empty directories are removed.
        for entry in std::fs::read_dir(&self.0).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                std::fs::remove_dir(entry.path()).unwrap();
            } else {
                std::fs::remove_file(entry.path()).unwrap();
            }
        }
        std::fs::remove_dir(&self.0).unwrap();
    }
}

fn fingerprint(path: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    std::fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let bytes = (!entry.file_type().unwrap().is_dir())
                .then(|| std::fs::read(entry.path()).unwrap());
            (entry.file_name().to_string_lossy().into_owned(), bytes)
        })
        .collect()
}

#[test]
fn sorting_and_saving_changes_no_desktop_fixture_names_or_bytes() {
    let desktop = Scratch::new();
    let config = Scratch::new();
    for (name, bytes) in [
        ("app.exe", b"executable".as_slice()),
        ("document.txt", b"document\0bytes"),
        (
            "web.url",
            b"[InternetShortcut]\r\nURL=https://example.org\r\n",
        ),
        ("unknown.lnk", b"not a valid shortcut"),
    ] {
        std::fs::write(desktop.0.join(name), bytes).unwrap();
    }
    std::fs::create_dir(desktop.0.join("Projects")).unwrap();
    let before = fingerprint(&desktop.0);
    let facts: Vec<_> = before
        .keys()
        .map(|name| crate::item_facts::facts_for(&item(&desktop.0.join(name).to_string_lossy())))
        .collect();
    let mut layout = Layout::new();
    arrange(&mut layout, &facts, false, &monitor(96, 1200, 900));
    super::super::layout_startup::Recovery::default()
        .save(&config.0.join("layout.json"), &layout)
        .unwrap();
    assert_eq!(fingerprint(&desktop.0), before);
    assert_eq!(
        layout
            .groups
            .iter()
            .map(|g| match &g.content {
                GroupContent::Items { items } => items.len(),
                _ => 0,
            })
            .sum::<usize>(),
        5
    );
}

#[test]
fn not_now_and_closed_save_an_empty_layout_so_next_start_does_not_offer() {
    let spec = spec();
    assert_eq!(spec.title, "Sort your desktop?");
    assert_eq!(spec.primary.label, "Sort my desktop");
    assert_eq!(spec.secondary.as_deref(), Some("Not now"));
    for answer in [Action::Secondary, Action::Closed] {
        assert_ne!(answer, Action::Primary);
        let dir = Scratch::new();
        let path = dir.0.join("arbitrary-test-layout.json");
        let (mut layout, mut recovery) =
            super::super::layout_startup::load(&path, "first-run-test");
        assert_eq!(recovery.first_run, Offer::Pending);
        let f = facts("keep.txt", ItemType::File, None);
        layout.place_loose(
            &f.name,
            crate::model::LoosePosition {
                monitor: "TEST".into(),
                x: 15,
                y: 35,
            },
        );
        acknowledge(&mut layout, [&*f.name]);
        recovery.first_run = Offer::No;
        recovery.save(&path, &layout).unwrap();
        let (loaded, recovery) = super::super::layout_startup::load(&path, "second-start");
        assert_eq!(loaded, layout);
        assert!(loaded.groups.is_empty());
        assert_eq!(recovery.first_run, Offer::No);
        assert!(loaded.is_seen("keep.txt"));
        let mut tracking = crate::round_up::RunTracker::default();
        for now in [0, 5000] {
            assert!(
                !tracking
                    .observe(
                        "keep.txt",
                        (Some(1), Some(1)),
                        now,
                        loaded.is_seen("keep.txt"),
                        true
                    )
                    .ready
            );
        }
    }
}

#[test]
fn only_both_missing_files_offer_never_loaded_backup_or_set_aside() {
    for state in ["fresh", "loaded", "backup", "set-aside"] {
        let dir = Scratch::new();
        let path = dir.0.join("layout.json");
        match state {
            "loaded" => crate::model::save(&path, &Layout::new()).unwrap(),
            "backup" => {
                crate::model::save(&crate::model::backup_path(&path), &Layout::new()).unwrap()
            }
            "set-aside" => std::fs::write(&path, b"{bad json").unwrap(),
            _ => {}
        }
        let (_, recovery) = super::super::layout_startup::load(&path, "first-run-test");
        assert_eq!(
            recovery.first_run == Offer::Pending,
            state == "fresh",
            "{state}"
        );
        assert_eq!(recovery.notice.is_some(), state == "set-aside");
    }
}
