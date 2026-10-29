//! Tests of the Round-up core (docs/design/round-up-spec.md section 7). They use fake facts, a
//! fake clock and time zone, and a fake file tree that can only be read.

use super::eval::{Verdict, combine, evaluate, evaluate_rule, extension, fold, stem, wildcard};
use super::*;
use crate::geometry::Rect;
use crate::item_facts::fake::{FakeTree, item};
use crate::item_facts::{ItemFacts, TargetClass, TargetFacts, collect_with};
use crate::model::{GroupContent, Layout};
use crate::round_up_time::fake::{DAY_MS, Dst, Fixed, HOUR_MS, clock, date, utc};
use crate::round_up_time::{Clock, TimeZone};
use serde_json::json;

const KB: u64 = 1 << 10;
const MB: u64 = 1 << 20;

// ---------------------------------------------------------------------------------------------
// Builders

fn facts(name: &str, kind: ItemType) -> ItemFacts {
    ItemFacts {
        name: format!("C:\\desk\\{name}"),
        file_name: name.into(),
        kind,
        size: Some(0),
        modified_ms: Some(0),
        target: None,
    }
}
fn file(name: &str) -> ItemFacts {
    facts(name, ItemType::File)
}
fn sized(name: &str, size: u64) -> ItemFacts {
    ItemFacts {
        size: Some(size),
        ..file(name)
    }
}
fn folder(name: &str) -> ItemFacts {
    ItemFacts {
        size: None,
        ..facts(name, ItemType::Folder)
    }
}
fn system(name: &str) -> ItemFacts {
    ItemFacts {
        name: "::{645FF040-5081-101B-9F08-00AA002F954E}".into(),
        file_name: name.into(),
        kind: ItemType::SystemItem,
        size: None,
        modified_ms: None,
        target: None,
    }
}
fn link(name: &str, target: Option<(&str, TargetClass)>) -> ItemFacts {
    ItemFacts {
        target: target.map(|(path, class)| TargetFacts {
            path: path.into(),
            class,
        }),
        ..facts(name, ItemType::Shortcut)
    }
}

fn now() -> Clock {
    Clock {
        now_ms: utc(2024, 6, 15, 12, 0),
    }
}
fn ev(condition: &Condition, item: &ItemFacts) -> Verdict {
    evaluate(condition, item, now(), &Fixed(0))
}
fn is_yes(condition: &Condition, item: &ItemFacts) -> bool {
    ev(condition, item) == Verdict::Yes
}
fn unknown(v: &Verdict) -> bool {
    matches!(v, Verdict::CouldNotEvaluate(_))
}

fn ext(op: ExtensionOp, values: &[&str]) -> Condition {
    Condition::Extension {
        op,
        value: values.iter().map(|s| s.to_string()).collect(),
    }
}
fn name(op: TextOp, value: &str) -> Condition {
    Condition::Name {
        op,
        value: value.into(),
    }
}
fn size(op: SizeOp, n: u64, unit: SizeUnit) -> Condition {
    Condition::Size {
        op,
        value: SizeValue { n, unit },
    }
}
fn modified(op: DateOp, n: u32, unit: TimeUnit) -> Condition {
    Condition::Modified {
        op,
        value: Age { n, unit },
    }
}
fn kind(op: IsOp, value: ItemType) -> Condition {
    Condition::ItemType { op, value }
}

fn rule(id: &str, conditions: Vec<Condition>, destination: Destination) -> Rule {
    Rule {
        id: id.into(),
        name: id.into(),
        enabled: true,
        preset: None,
        match_mode: MatchMode::All,
        conditions,
        destination: Some(destination),
        legacy: serde_json::Value::Null,
    }
}
fn round_up(rules: Vec<Rule>, default_destination: Destination) -> RoundUp {
    RoundUp {
        rules,
        default_destination,
        ..RoundUp::default()
    }
}
fn corral(id: u64) -> Destination {
    Destination::Corral { id }
}

/// A layout with Corrals of these titles; returns their ids.
fn layout_with(titles: &[&str]) -> (Layout, Vec<u64>) {
    let mut layout = Layout::new();
    let ids = titles
        .iter()
        .map(|t| {
            layout.add_group(
                (*t).into(),
                "m".into(),
                Rect::default(),
                GroupContent::Items { items: vec![] },
            )
        })
        .collect();
    (layout, ids)
}

fn world() -> (Layout, u64, u64) {
    let (layout, ids) = layout_with(&["Accounts", "Pictures"]);
    (layout, ids[0], ids[1])
}

fn run(ru: &RoundUp, layout: &Layout, items: &[ItemFacts], include_manual: bool) -> Plan {
    plan(ru, layout, items, now(), &Fixed(0), include_manual)
}

fn entry<'a>(plan: &'a Plan, name: &str) -> &'a PlanEntry {
    plan.entries.iter().find(|e| e.label == name).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Truth tables (spec 7, "Truth tables")

fn shape(v: &Verdict) -> u8 {
    match v {
        Verdict::Yes => 0,
        Verdict::No => 1,
        Verdict::CouldNotEvaluate(_) => 2,
    }
}

#[test]
fn all_and_any_truth_tables_for_one_two_and_three_conditions() {
    let values = [
        Verdict::Yes,
        Verdict::No,
        Verdict::CouldNotEvaluate("x".into()),
    ];
    for n in 1..=3u32 {
        for code in 0..3usize.pow(n) {
            let list: Vec<Verdict> = (0..n)
                .map(|i| values[(code / 3usize.pow(i)) % 3].clone())
                .collect();
            let has = |v: u8| list.iter().any(|x| shape(x) == v);
            let all = if has(1) {
                1
            } else if has(2) {
                2
            } else {
                0
            };
            let any = if has(0) {
                0
            } else if has(2) {
                2
            } else {
                1
            };
            assert_eq!(shape(&combine(MatchMode::All, &list)), all, "all {list:?}");
            assert_eq!(shape(&combine(MatchMode::Any, &list)), any, "any {list:?}");
        }
    }
    let (no, yes, cannot) = (
        Verdict::No,
        Verdict::Yes,
        Verdict::CouldNotEvaluate("?".into()),
    );
    assert_eq!(
        combine(MatchMode::All, &[no.clone(), cannot.clone()]),
        Verdict::No
    );
    assert_eq!(
        combine(MatchMode::Any, &[yes.clone(), cannot.clone()]),
        Verdict::Yes
    );
    assert!(unknown(&combine(MatchMode::All, &[yes, cannot.clone()])));
    assert!(unknown(&combine(MatchMode::Any, &[no, cannot])));
}

#[test]
fn rules_combine_their_conditions_with_the_same_tables() {
    let item = ItemFacts {
        modified_ms: None,
        ..file("a.pdf")
    };
    let yes = kind(IsOp::Is, ItemType::File);
    let no = kind(IsOp::IsNot, ItemType::File);
    let cannot = modified(DateOp::NewerThan, 1, TimeUnit::Days);
    let pick = |i: usize| [yes.clone(), no.clone(), cannot.clone()][i].clone();
    for mode in [MatchMode::All, MatchMode::Any] {
        for a in 0..3 {
            for b in 0..3 {
                for c in 0..3 {
                    let r = Rule {
                        match_mode: mode,
                        ..rule("r", vec![pick(a), pick(b), pick(c)], corral(1))
                    };
                    let each: Vec<Verdict> = r.conditions.iter().map(|x| ev(x, &item)).collect();
                    let got = evaluate_rule(&r, &item, now(), &Fixed(0)).0;
                    assert_eq!(shape(&got), shape(&combine(mode, &each)));
                }
            }
        }
    }
}

#[test]
fn negative_operators_never_turn_unknown_into_yes() {
    let broken = link("x.lnk", None);
    let negatives = [
        Condition::ShortcutTarget {
            op: IsOp::IsNot,
            value: TargetKind::Program,
        },
        Condition::TargetExtension {
            op: ExtensionOp::IsNoneOf,
            value: vec!["exe".into()],
        },
        Condition::TargetExtension {
            op: ExtensionOp::HasNone,
            value: vec![],
        },
        Condition::TargetPath {
            op: TextOp::DoesNotContain,
            value: "x".into(),
        },
    ];
    for c in &negatives {
        assert!(unknown(&ev(c, &broken)), "{c:?}");
    }
    let unread = ItemFacts {
        size: None,
        modified_ms: None,
        ..file("a.txt")
    };
    assert!(unknown(&ev(
        &size(SizeOp::LessThan, 1, SizeUnit::KB),
        &unread
    )));
    assert!(unknown(&ev(
        &modified(DateOp::OlderThan, 1, TimeUnit::Days),
        &unread
    )));
}

// ---------------------------------------------------------------------------------------------
// First-match order (spec 7, "First-match order")

#[test]
fn the_first_rule_that_says_yes_decides_and_reordering_changes_the_outcome() {
    let (layout, accounts, pictures) = world();
    let pdf = rule(
        "pdf",
        vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
        corral(accounts),
    );
    let any_file = rule(
        "files",
        vec![kind(IsOp::Is, ItemType::File)],
        corral(pictures),
    );
    let items = [file("a.pdf")];
    let ru = round_up(vec![pdf.clone(), any_file.clone()], Destination::Desktop);
    assert_eq!(
        run(&ru, &layout, &items, false).entries[0].to,
        corral(accounts)
    );
    let swapped = round_up(vec![any_file, pdf], Destination::Desktop);
    assert_eq!(
        run(&swapped, &layout, &items, false).entries[0].to,
        corral(pictures)
    );
}

#[test]
fn an_unknown_rule_above_a_yes_halts_the_item_and_keeps_it_unseen() {
    let (mut layout, accounts, pictures) = world();
    let big = rule(
        "big",
        vec![size(SizeOp::AtLeast, 1, SizeUnit::MB)],
        corral(pictures),
    );
    let pdf = rule(
        "pdf",
        vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
        corral(accounts),
    );
    let unread = ItemFacts {
        size: None,
        ..file("a.pdf")
    };
    let ru = round_up(vec![big, pdf], Destination::Desktop);
    let plan = run(&ru, &layout, &[unread.clone()], false);
    let e = &plan.entries[0];
    assert!(matches!(e.status, PlanStatus::Unchecked(_)), "{e:?}");
    assert_eq!(e.rule.as_deref(), Some("big"));
    assert_eq!(e.to, e.from);
    assert_eq!(apply_plan(&mut layout, &plan), 0);
    assert!(!layout.is_seen(&unread.name), "unknown results stay unseen");
    assert_eq!(layout.group_of(&unread.name), None);
}

#[test]
fn an_unknown_rule_below_a_yes_is_irrelevant() {
    let (layout, accounts, pictures) = world();
    let pdf = rule(
        "pdf",
        vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
        corral(accounts),
    );
    let big = rule(
        "big",
        vec![size(SizeOp::AtLeast, 1, SizeUnit::MB)],
        corral(pictures),
    );
    let item = ItemFacts {
        size: None,
        ..file("a.pdf")
    };
    let plan = run(
        &round_up(vec![pdf, big], Destination::Desktop),
        &layout,
        &[item],
        false,
    );
    assert_eq!(plan.entries[0].status, PlanStatus::Move);
    assert_eq!(plan.entries[0].to, corral(accounts));
}

#[test]
fn disabled_and_suspended_rules_are_skipped() {
    let (layout, accounts, pictures) = world();
    let mut off = rule(
        "off",
        vec![kind(IsOp::Is, ItemType::File)],
        corral(accounts),
    );
    off.enabled = false;
    let gone = rule("gone", vec![kind(IsOp::Is, ItemType::File)], corral(999));
    let live = rule(
        "live",
        vec![kind(IsOp::Is, ItemType::File)],
        corral(pictures),
    );
    let plan = run(
        &round_up(vec![off, gone, live], Destination::Desktop),
        &layout,
        &[file("a.txt")],
        false,
    );
    assert_eq!(plan.entries[0].to, corral(pictures));
    assert_eq!(plan.entries[0].rule.as_deref(), Some("live"));
}

#[test]
fn nothing_matching_uses_a_corral_default_and_a_desktop_default_has_no_opinion() {
    let (mut layout, accounts, _) = world();
    let pdf = rule(
        "pdf",
        vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
        corral(accounts),
    );
    let other = file("notes.txt");
    let with_default = round_up(vec![pdf.clone()], corral(accounts));
    let plan_default = run(&with_default, &layout, &[other.clone()], false);
    assert_eq!(plan_default.entries[0].to, corral(accounts));
    assert_eq!(plan_default.entries[0].status, PlanStatus::Move);
    // "Leave it on the desktop" does not pull an item out of a Corral just because nothing matched.
    layout.insert_item(&other.name, accounts, usize::MAX);
    let plan_desktop = run(
        &round_up(vec![pdf], Destination::Desktop),
        &layout,
        &[other],
        false,
    );
    assert_eq!(plan_desktop.entries[0].status, PlanStatus::NoChange);
    assert_eq!(plan_desktop.entries[0].to, corral(accounts));
}

#[test]
fn a_plan_lists_the_deciding_rule_and_every_satisfied_condition() {
    let (layout, accounts, _) = world();
    let mut r = rule(
        "Accounts",
        vec![
            ext(ExtensionOp::IsOneOf, &["pdf"]),
            name(TextOp::Contains, "invoice"),
        ],
        corral(accounts),
    );
    r.match_mode = MatchMode::Any;
    let plan = run(
        &round_up(vec![r], Destination::Desktop),
        &layout,
        &[file("invoice.pdf")],
        false,
    );
    let e = &plan.entries[0];
    assert_eq!(e.rule.as_deref(), Some("Accounts"));
    assert_eq!(
        e.reasons,
        ["has the extension pdf", "its name contains \"invoice\""]
    );
}

// ---------------------------------------------------------------------------------------------
// Extension and name (spec 7)

#[test]
fn extension_matches_any_letter_case_and_takes_the_text_after_the_last_dot() {
    for name in ["a.PDF", "a.Pdf", "a.pdf"] {
        assert!(
            is_yes(&ext(ExtensionOp::IsOneOf, &["pdf"]), &file(name)),
            "{name}"
        );
    }
    assert!(is_yes(&ext(ExtensionOp::IsOneOf, &["PDF"]), &file("a.pdf")));
    assert_eq!(extension("a.tar.gz"), "gz");
    assert!(is_yes(
        &ext(ExtensionOp::IsOneOf, &["gz"]),
        &file("a.tar.gz")
    ));
    assert!(!is_yes(
        &ext(ExtensionOp::IsOneOf, &["tar"]),
        &file("a.tar.gz")
    ));
    assert_eq!(extension(".gitignore"), "gitignore");
    assert!(is_yes(
        &ext(ExtensionOp::IsOneOf, &["gitignore"]),
        &file(".gitignore")
    ));
    assert_eq!(extension("README"), "");
    assert_eq!(extension("trailing."), "");
}

#[test]
fn has_none_and_is_none_of_treat_missing_extensions_and_folders_alike() {
    for plain in [file("README"), file("trailing.")] {
        assert!(
            is_yes(&ext(ExtensionOp::HasNone, &[]), &plain),
            "{}",
            plain.file_name
        );
        assert!(is_yes(&ext(ExtensionOp::IsNoneOf, &["pdf"]), &plain));
        assert!(!is_yes(&ext(ExtensionOp::IsOneOf, &["pdf"]), &plain));
    }
    assert!(!is_yes(&ext(ExtensionOp::HasNone, &[]), &file("a.pdf")));
    assert!(!is_yes(
        &ext(ExtensionOp::IsNoneOf, &["pdf"]),
        &file("a.pdf")
    ));
    // Folders and system items never have an extension, whatever their name looks like.
    for item in [folder("Projects.pdf"), system("Recycle Bin.pdf")] {
        assert!(is_yes(&ext(ExtensionOp::HasNone, &[]), &item));
        assert!(!is_yes(&ext(ExtensionOp::IsOneOf, &["pdf"]), &item));
    }
}

#[test]
fn name_uses_the_stem_for_files_and_shortcuts_and_the_whole_name_for_the_rest() {
    assert_eq!(stem("report.final.pdf"), "report.final");
    assert!(is_yes(
        &name(TextOp::Contains, "final"),
        &file("report.final.pdf")
    ));
    assert!(
        !is_yes(&name(TextOp::Contains, "pdf"), &file("report.final.pdf")),
        "extensions are matched by Extension"
    );
    assert!(is_yes(
        &name(TextOp::Is, "report.final"),
        &file("report.final.pdf")
    ));
    assert!(is_yes(
        &name(TextOp::Contains, "v2"),
        &folder("Projects.v2")
    ));
    assert!(is_yes(
        &name(TextOp::Is, "Projects.v2"),
        &folder("Projects.v2")
    ));
    assert!(is_yes(
        &name(TextOp::EndsWith, "bin"),
        &system("Recycle Bin")
    ));
    assert!(is_yes(
        &name(TextOp::StartsWith, "docs"),
        &link("Docs.lnk", None)
    ));
    assert!(!is_yes(
        &name(TextOp::Contains, "lnk"),
        &link("Docs.lnk", None)
    ));
}

#[test]
fn name_operators_cover_contains_starts_ends_and_their_negation() {
    let item = file("Quarterly Invoice 2024.pdf");
    assert!(is_yes(&name(TextOp::Contains, "invoice"), &item));
    assert!(!is_yes(&name(TextOp::DoesNotContain, "invoice"), &item));
    assert!(is_yes(&name(TextOp::DoesNotContain, "receipt"), &item));
    assert!(is_yes(&name(TextOp::StartsWith, "QUARTERLY"), &item));
    assert!(is_yes(&name(TextOp::EndsWith, "2024"), &item));
    assert!(!is_yes(&name(TextOp::EndsWith, "pdf"), &item));
}

#[test]
fn case_folding_is_simple_and_handles_non_ascii_without_normalising() {
    assert!(is_yes(&name(TextOp::Is, "ÄRGER"), &file("ärger.txt")));
    assert!(is_yes(
        &name(TextOp::Contains, "ÉCOLE"),
        &file("L'école.txt")
    ));
    assert_eq!(fold("ΣΑΣ"), fold("σας"), "final sigma folds to sigma");
    assert!(is_yes(&name(TextOp::Is, "ΣΑΣ"), &file("σας.txt")));
    // No Unicode normalisation: a decomposed e-acute is not the composed one.
    assert!(!is_yes(&name(TextOp::Is, "e\u{301}"), &file("\u{e9}.txt")));
}

#[test]
fn patterns_use_star_and_question_mark_and_treat_everything_else_literally() {
    let matches = |pattern: &str, stem_name: &str| {
        is_yes(
            &name(TextOp::MatchesPattern, pattern),
            &file(&format!("{stem_name}.txt")),
        )
    };
    assert!(matches("inv*", "Invoice-2024"));
    assert!(matches("*2024", "invoice-2024"));
    assert!(matches("i*e*4", "invoice-2024"));
    assert!(matches("*", "anything"));
    assert!(matches("?at", "cat"));
    assert!(!matches("?at", "at"));
    assert!(!matches("?at", "chat"));
    assert!(matches("a.b", "a.b"));
    assert!(!matches("a.b", "axb"), "the dot is a dot");
    assert!(matches("a[1]", "a[1]"), "brackets are literal");
    assert!(!matches("a[1]", "a1"));
    assert!(!matches("[a-z]", "b"));
    assert!(wildcard("abc", "a*"));
    assert!(wildcard("", "*"));
    assert!(!wildcard("", "?"));
}

#[test]
fn a_dotfile_has_an_empty_name_stem() {
    let dot = file(".gitignore");
    assert_eq!(stem(&dot.file_name), "");
    assert!(
        !is_yes(&name(TextOp::Contains, "git"), &dot),
        "the extension is not part of the name"
    );
    assert!(is_yes(&name(TextOp::DoesNotContain, "git"), &dot));
}

// ---------------------------------------------------------------------------------------------
// Item type and shortcuts (spec 7, "Shortcuts")

#[test]
fn every_item_has_exactly_one_type() {
    let items = [
        file("a.txt"),
        folder("d"),
        link("l.lnk", None),
        system("Bin"),
    ];
    let all = [
        ItemType::Folder,
        ItemType::File,
        ItemType::Shortcut,
        ItemType::SystemItem,
    ];
    for item in &items {
        let holding = all
            .iter()
            .filter(|t| is_yes(&kind(IsOp::Is, **t), item))
            .count();
        assert_eq!(holding, 1, "{}", item.file_name);
        for t in all {
            assert_ne!(
                is_yes(&kind(IsOp::Is, t), item),
                is_yes(&kind(IsOp::IsNot, t), item)
            );
        }
    }
    assert!(
        !is_yes(&kind(IsOp::Is, ItemType::File), &link("l.lnk", None)),
        "File excludes shortcuts"
    );
}

#[test]
fn shortcut_points_to_program_folder_or_file_and_web_is_none_of_them() {
    let to = |class: TargetClass| link("x.lnk", Some(("C:\\somewhere\\target", class)));
    let points = |op: IsOp, value: TargetKind, item: &ItemFacts| {
        is_yes(&Condition::ShortcutTarget { op, value }, item)
    };
    for (class, expected) in [
        (TargetClass::Program, TargetKind::Program),
        (TargetClass::Folder, TargetKind::Folder),
        (TargetClass::File, TargetKind::File),
    ] {
        let item = to(class);
        for value in [TargetKind::Program, TargetKind::Folder, TargetKind::File] {
            assert_eq!(points(IsOp::Is, value, &item), value == expected);
            assert_eq!(points(IsOp::IsNot, value, &item), value != expected);
        }
    }
    let web = link("w.url", Some(("https://example.org/", TargetClass::Web)));
    for value in [TargetKind::Program, TargetKind::Folder, TargetKind::File] {
        assert!(!points(IsOp::Is, value, &web));
        assert!(points(IsOp::IsNot, value, &web));
    }
}

#[test]
fn an_unreadable_target_is_unknown_for_every_target_condition() {
    let broken = link("x.lnk", None);
    let conditions = [
        Condition::ShortcutTarget {
            op: IsOp::Is,
            value: TargetKind::Folder,
        },
        Condition::TargetExtension {
            op: ExtensionOp::IsOneOf,
            value: vec!["exe".into()],
        },
        Condition::TargetPath {
            op: TextOp::Contains,
            value: "x".into(),
        },
    ];
    for c in &conditions {
        assert!(unknown(&ev(c, &broken)), "{c:?}");
    }
}

#[test]
fn target_conditions_do_not_apply_to_ordinary_items_even_with_negative_operators() {
    let conditions = [
        Condition::ShortcutTarget {
            op: IsOp::IsNot,
            value: TargetKind::Program,
        },
        Condition::TargetExtension {
            op: ExtensionOp::IsNoneOf,
            value: vec!["exe".into()],
        },
        Condition::TargetExtension {
            op: ExtensionOp::HasNone,
            value: vec![],
        },
        Condition::TargetPath {
            op: TextOp::DoesNotContain,
            value: "x".into(),
        },
    ];
    for item in [file("a.txt"), folder("d"), system("Bin")] {
        for c in &conditions {
            assert_eq!(ev(c, &item), Verdict::No, "{} {c:?}", item.file_name);
        }
    }
}

#[test]
fn target_extension_and_path_read_the_targets_own_name() {
    let item = link("x.lnk", Some(("C:\\Docs\\Report.PDF", TargetClass::File)));
    assert!(is_yes(
        &Condition::TargetExtension {
            op: ExtensionOp::IsOneOf,
            value: vec!["pdf".into()]
        },
        &item
    ));
    assert!(is_yes(
        &Condition::TargetPath {
            op: TextOp::StartsWith,
            value: "c:\\docs".into()
        },
        &item
    ));
    assert!(is_yes(
        &Condition::TargetPath {
            op: TextOp::MatchesPattern,
            value: "c:\\*.pdf".into()
        },
        &item
    ));
    let web = link(
        "w.url",
        Some(("https://example.org/a.html", TargetClass::Web)),
    );
    assert!(is_yes(
        &Condition::TargetExtension {
            op: ExtensionOp::HasNone,
            value: vec![]
        },
        &web
    ));
}

// ---------------------------------------------------------------------------------------------
// Size (spec 7, "Size")

#[test]
fn size_boundaries_are_exact_and_kilobytes_are_1024_bytes() {
    let ten = |op| size(op, 10, SizeUnit::MB);
    let exact = sized("a.bin", 10 * MB);
    assert!(is_yes(&ten(SizeOp::AtLeast), &exact));
    assert!(!is_yes(&ten(SizeOp::LessThan), &exact));
    let below = sized("a.bin", 10 * MB - 1);
    assert!(!is_yes(&ten(SizeOp::AtLeast), &below));
    assert!(is_yes(&ten(SizeOp::LessThan), &below));
    assert!(is_yes(&ten(SizeOp::AtLeast), &sized("a.bin", 10 * MB + 1)));
    assert!(is_yes(
        &size(SizeOp::AtLeast, 1, SizeUnit::KB),
        &sized("a", 1024)
    ));
    assert!(!is_yes(
        &size(SizeOp::AtLeast, 1, SizeUnit::KB),
        &sized("a", 1023)
    ));
    assert!(is_yes(
        &size(SizeOp::AtLeast, 1, SizeUnit::GB),
        &sized("a", 1024 * MB)
    ));
    assert!(!is_yes(
        &size(SizeOp::AtLeast, 1, SizeUnit::GB),
        &sized("a", 1000 * MB)
    ));
}

#[test]
fn zero_byte_files_and_zero_limits() {
    let empty = sized("empty.txt", 0);
    assert!(is_yes(&size(SizeOp::LessThan, 1, SizeUnit::KB), &empty));
    assert!(is_yes(&size(SizeOp::AtLeast, 0, SizeUnit::KB), &empty));
    assert!(!is_yes(&size(SizeOp::LessThan, 0, SizeUnit::KB), &empty));
}

#[test]
fn folders_and_system_items_have_no_size_and_an_unread_size_is_unknown() {
    for item in [folder("d"), system("Bin")] {
        assert_eq!(
            ev(&size(SizeOp::AtLeast, 0, SizeUnit::KB), &item),
            Verdict::No
        );
        assert_eq!(
            ev(&size(SizeOp::LessThan, 1, SizeUnit::GB), &item),
            Verdict::No
        );
    }
    let unread = ItemFacts {
        size: None,
        ..file("a.txt")
    };
    assert!(unknown(&ev(
        &size(SizeOp::AtLeast, 1, SizeUnit::KB),
        &unread
    )));
    // a shortcut's size is that of the shortcut file itself
    let shortcut = ItemFacts {
        size: Some(2 * KB),
        ..link("x.lnk", None)
    };
    assert!(is_yes(&size(SizeOp::AtLeast, 2, SizeUnit::KB), &shortcut));
}

// ---------------------------------------------------------------------------------------------
// Date (spec 7, "Date")

fn modified_at(ms: i64) -> ItemFacts {
    ItemFacts {
        modified_ms: Some(ms),
        ..file("a.txt")
    }
}

#[test]
fn newer_and_older_are_exact_complements_at_the_boundary_instant() {
    let bound = now().now_ms - 7 * DAY_MS;
    let newer = modified(DateOp::NewerThan, 7, TimeUnit::Days);
    let older = modified(DateOp::OlderThan, 7, TimeUnit::Days);
    for (at, is_newer) in [
        (bound, true),
        (bound + 1, true),
        (bound - 1, false),
        (now().now_ms, true),
    ] {
        assert_eq!(is_yes(&newer, &modified_at(at)), is_newer, "{at}");
        assert_eq!(is_yes(&older, &modified_at(at)), !is_newer, "{at}");
    }
}

#[test]
fn a_modified_time_in_the_future_counts_as_newer_and_an_unset_one_is_unknown() {
    let newer = modified(DateOp::NewerThan, 1, TimeUnit::Hours);
    let older = modified(DateOp::OlderThan, 1, TimeUnit::Hours);
    let future = modified_at(now().now_ms + 10 * DAY_MS);
    assert!(is_yes(&newer, &future));
    assert!(!is_yes(&older, &future));
    let unset = ItemFacts {
        modified_ms: None,
        ..file("a.txt")
    };
    assert!(unknown(&ev(&newer, &unset)));
    assert!(unknown(&ev(&older, &unset)));
    // a system item has no change date to read: not applicable, a definite No
    assert_eq!(ev(&newer, &system("Bin")), Verdict::No);
}

#[test]
fn months_clamp_to_the_last_day_of_a_shorter_month() {
    let zone = Fixed(0);
    let newer = modified(DateOp::NewerThan, 1, TimeUnit::Months);
    let leap = clock(date(2024, 3, 31, 12, 0), &zone);
    let bound = utc(2024, 2, 29, 12, 0);
    assert_eq!(
        evaluate(&newer, &modified_at(bound), leap, &zone),
        Verdict::Yes
    );
    assert_eq!(
        evaluate(&newer, &modified_at(bound - 1), leap, &zone),
        Verdict::No
    );
    let plain = clock(date(2023, 3, 31, 12, 0), &zone);
    assert_eq!(
        evaluate(&newer, &modified_at(utc(2023, 2, 28, 12, 0)), plain, &zone),
        Verdict::Yes
    );
    assert_eq!(
        evaluate(
            &newer,
            &modified_at(utc(2023, 2, 28, 12, 0) - 1),
            plain,
            &zone
        ),
        Verdict::No
    );
}

#[test]
fn weeks_follow_the_calendar_in_a_zone_east_of_utc() {
    let zone = Fixed(5 * HOUR_MS + 30 * 60_000);
    let at = clock(date(2024, 1, 3, 0, 15), &zone);
    let bound = zone.resolve_local(date(2023, 12, 27, 0, 15)).unwrap();
    let newer = modified(DateOp::NewerThan, 1, TimeUnit::Weeks);
    assert_eq!(
        evaluate(&newer, &modified_at(bound), at, &zone),
        Verdict::Yes
    );
    assert_eq!(
        evaluate(&newer, &modified_at(bound - 1), at, &zone),
        Verdict::No
    );
}

#[test]
fn hours_are_exact_while_days_follow_a_daylight_saving_change() {
    let zone = Dst;
    let at = clock(date(2024, 3, 31, 12, 0), &zone); // the day the clocks went forward
    let item = modified_at(at.now_ms - 23 * HOUR_MS - 30 * 60_000);
    let hours = modified(DateOp::NewerThan, 24, TimeUnit::Hours);
    let days = modified(DateOp::NewerThan, 1, TimeUnit::Days);
    assert_eq!(
        evaluate(&hours, &item, at, &zone),
        Verdict::Yes,
        "24 hours reach back 24 hours"
    );
    assert_eq!(
        evaluate(&days, &item, at, &zone),
        Verdict::No,
        "1 day is only 23 hours here"
    );
}

#[test]
fn a_nonexistent_local_time_moves_forward_and_an_ambiguous_one_takes_the_earlier_instant() {
    let zone = Dst;
    let newer = modified(DateOp::NewerThan, 1, TimeUnit::Days);
    // Spring: 02:30 on 2024-03-31 does not exist locally.
    let at = clock(date(2024, 4, 1, 2, 30), &zone);
    let gap_bound = utc(2024, 3, 31, 1, 30);
    assert_eq!(
        evaluate(&newer, &modified_at(gap_bound), at, &zone),
        Verdict::Yes
    );
    assert_eq!(
        evaluate(&newer, &modified_at(gap_bound - 1), at, &zone),
        Verdict::No
    );
    // Autumn: 02:30 on 2024-10-27 happens twice; the earlier instant is the limit.
    let at = clock(date(2024, 10, 28, 2, 30), &zone);
    let overlap_bound = utc(2024, 10, 27, 0, 30);
    assert_eq!(
        evaluate(&newer, &modified_at(overlap_bound), at, &zone),
        Verdict::Yes
    );
    assert_eq!(
        evaluate(&newer, &modified_at(overlap_bound - 1), at, &zone),
        Verdict::No
    );
}

#[test]
fn the_planner_works_with_any_time_zone() {
    let (layout, accounts, _) = world();
    let recent = rule(
        "recent",
        vec![modified(DateOp::NewerThan, 1, TimeUnit::Days)],
        corral(accounts),
    );
    let zone: &dyn TimeZone = &Dst;
    let at = clock(date(2024, 4, 1, 12, 0), &Dst);
    let item = ItemFacts {
        modified_ms: Some(at.now_ms - HOUR_MS),
        ..file("a.txt")
    };
    let planned = plan(
        &round_up(vec![recent], Destination::Desktop),
        &layout,
        &[item],
        at,
        zone,
        false,
    );
    assert_eq!(planned.entries[0].to, corral(accounts));
}

// ---------------------------------------------------------------------------------------------
// Destinations (spec 7, "Destination")

#[test]
fn a_deleted_corral_suspends_its_rule_with_a_warning_and_the_same_id_revives_it() {
    let (mut layout, accounts, pictures) = world();
    let pdf = rule(
        "pdf",
        vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
        corral(accounts),
    );
    let fallback = rule(
        "any",
        vec![kind(IsOp::Is, ItemType::File)],
        corral(pictures),
    );
    let ru = round_up(vec![pdf, fallback], Destination::Desktop);
    let item = file("a.pdf");
    let removed = layout.groups.remove(0);
    let plan_gone = run(&ru, &layout, &[item.clone()], false);
    assert_eq!(
        plan_gone.entries[0].to,
        corral(pictures),
        "the rule below takes over"
    );
    assert_eq!(
        plan_gone.warnings,
        ["1 rule points to a Corral that no longer exists."]
    );
    assert_eq!(ru.missing_destinations(|id| corral_exists(&layout, id)), 1);
    layout.groups.insert(0, removed);
    let plan_back = run(&ru, &layout, &[item], false);
    assert_eq!(plan_back.entries[0].to, corral(accounts));
    assert!(plan_back.warnings.is_empty());
}

#[test]
fn a_gate_is_not_a_destination() {
    let (mut layout, _, _) = world();
    let gate = layout.add_group(
        "Gate".into(),
        "m".into(),
        Rect::default(),
        GroupContent::Portal {
            path: "C:\\x".into(),
        },
    );
    assert!(!corral_exists(&layout, gate));
    let ru = round_up(
        vec![rule(
            "r",
            vec![kind(IsOp::Is, ItemType::File)],
            corral(gate),
        )],
        Destination::Desktop,
    );
    let plan = run(&ru, &layout, &[file("a.txt")], false);
    assert_eq!(plan.entries[0].status, PlanStatus::NoChange);
    assert_eq!(plan.warnings.len(), 1);
}

#[test]
fn a_missing_default_corral_behaves_like_the_desktop_with_a_warning() {
    let (layout, _, _) = world();
    let plan = run(
        &round_up(vec![], corral(777)),
        &layout,
        &[file("a.txt")],
        false,
    );
    assert_eq!(plan.entries[0].status, PlanStatus::NoChange);
    assert_eq!(plan.warnings.len(), 1);
}

#[test]
fn the_desktop_destination_pulls_a_non_manual_item_out_and_leaves_a_loose_one_alone() {
    let (mut layout, accounts, _) = world();
    let to_desktop = rule(
        "out",
        vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
        Destination::Desktop,
    );
    let ru = round_up(vec![to_desktop], corral(accounts));
    let inside = file("in.pdf");
    let loose = file("loose.pdf");
    layout.insert_item(&inside.name, accounts, usize::MAX);
    let plan = run(&ru, &layout, &[inside.clone(), loose.clone()], false);
    assert_eq!(entry(&plan, "in.pdf").status, PlanStatus::Move);
    assert_eq!(entry(&plan, "in.pdf").to, Destination::Desktop);
    assert_eq!(entry(&plan, "loose.pdf").status, PlanStatus::NoChange);
    assert_eq!(apply_plan(&mut layout, &plan), 1);
    assert_eq!(layout.group_of(&inside.name), None);
    assert_eq!(layout.group_of(&loose.name), None);
}

#[test]
fn a_rule_without_a_destination_cannot_be_enabled() {
    let mut r = rule(
        "r",
        vec![kind(IsOp::Is, ItemType::File)],
        Destination::Desktop,
    );
    r.destination = None;
    assert!(r.validate().is_err());
    assert!(!r.runs());
    r.enabled = false;
    assert!(
        r.validate().is_ok(),
        "an unconfigured built-in rule may stay off"
    );
    assert!(
        presets()
            .iter()
            .all(|p| !p.enabled && p.destination.is_none() && p.validate().is_ok())
    );
    let (layout, _, _) = world();
    r.enabled = true;
    let plan = run(
        &round_up(vec![r], Destination::Desktop),
        &layout,
        &[file("a.txt")],
        false,
    );
    assert_eq!(plan.entries[0].status, PlanStatus::NoChange);
}

#[test]
fn invalid_conditions_are_reported_in_words() {
    assert!(name(TextOp::Contains, "  ").validate().is_err());
    assert!(ext(ExtensionOp::IsOneOf, &[]).validate().is_err());
    assert!(ext(ExtensionOp::IsOneOf, &[".pdf"]).validate().is_err());
    assert!(ext(ExtensionOp::IsOneOf, &["p*f"]).validate().is_err());
    assert!(ext(ExtensionOp::HasNone, &[]).validate().is_ok());
    assert!(
        modified(DateOp::NewerThan, 0, TimeUnit::Days)
            .validate()
            .is_err()
    );
    assert!(
        size(SizeOp::AtLeast, u64::MAX, SizeUnit::GB)
            .validate()
            .is_err()
    );
    assert!(
        Condition::Unknown {
            raw: json!({"type": "future"})
        }
        .validate()
        .is_err()
    );
    let empty = Rule {
        conditions: vec![],
        ..rule("r", vec![], Destination::Desktop)
    };
    assert!(empty.validate().is_err());
}

// ---------------------------------------------------------------------------------------------
// Manual placement, seen and first start (spec 7)

#[test]
fn manual_items_are_skipped_unless_the_checkbox_includes_them_and_applying_clears_the_mark() {
    let (mut layout, accounts, _) = world();
    let ru = round_up(
        vec![rule(
            "pdf",
            vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
            corral(accounts),
        )],
        Destination::Desktop,
    );
    let mine = file("mine.pdf");
    layout.mark_manual(&mine.name, 5);
    let skipped = run(&ru, &layout, &[mine.clone()], false);
    assert_eq!(skipped.entries[0].status, PlanStatus::Manual);
    assert_eq!(skipped.manual_count(), 1);
    assert_eq!(apply_plan(&mut layout, &skipped), 0);
    assert_eq!(layout.group_of(&mine.name), None);
    assert!(layout.is_manual(&mine.name));
    let included = run(&ru, &layout, &[mine.clone()], true);
    assert_eq!(included.entries[0].status, PlanStatus::Move);
    assert!(included.entries[0].clear_manual);
    assert_eq!(apply_plan(&mut layout, &included), 1);
    assert_eq!(layout.group_of(&mine.name), Some(accounts));
    assert!(
        !layout.is_manual(&mine.name),
        "ticking the box and applying clears the mark"
    );
}

#[test]
fn round_ups_own_moves_are_not_manual_and_a_stale_plan_does_nothing_extra() {
    let (mut layout, accounts, _) = world();
    let ru = round_up(
        vec![rule(
            "pdf",
            vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
            corral(accounts),
        )],
        Destination::Desktop,
    );
    let item = file("a.pdf");
    let planned = run(&ru, &layout, &[item.clone()], false);
    assert_eq!(apply_plan(&mut layout, &planned), 1);
    assert!(!layout.is_manual(&item.name));
    assert!(layout.is_seen(&item.name));
    // The user drags it away, then the old plan is applied again: nothing happens.
    layout.assign_item(&item.name, None);
    layout.mark_manual(&item.name, 1);
    assert_eq!(apply_plan(&mut layout, &planned), 0);
    assert_eq!(layout.group_of(&item.name), None);
}

#[test]
fn manual_marks_are_pruned_after_thirty_days_without_the_item() {
    let mut layout = Layout::new();
    layout.mark_manual("C:\\desk\\gone.txt", 0);
    layout.mark_manual("C:\\desk\\here.txt", 0);
    let present = vec!["C:\\desk\\here.txt".to_string()];
    layout.round_up_observe(&present, crate::model::MANUAL_FORGET_MS - 1);
    assert!(layout.is_manual("C:\\desk\\gone.txt"));
    layout.round_up_observe(&present, crate::model::MANUAL_FORGET_MS);
    assert!(
        !layout.is_manual("C:\\desk\\gone.txt"),
        "absent for 30 days"
    );
    assert!(
        layout.is_manual("C:\\desk\\here.txt"),
        "present items keep their mark"
    );
}

#[test]
fn first_start_with_an_older_layout_marks_everything_seen_members_manual_and_moves_nothing() {
    let (mut layout, accounts, _) = world();
    layout.round_up_initialized = false;
    let member = file("member.txt");
    let loose = file("loose.txt");
    layout.insert_item(&member.name, accounts, usize::MAX);
    let before = layout.groups.clone();
    let present = vec![member.name.clone(), loose.name.clone()];
    assert!(layout.round_up_observe(&present, 10));
    assert!(layout.round_up_initialized);
    assert!(layout.is_seen(&member.name) && layout.is_seen(&loose.name));
    assert!(
        layout.is_manual(&member.name),
        "its membership came from the user or an older version"
    );
    assert!(
        !layout.is_manual(&loose.name),
        "loose items are seen but not manual"
    );
    assert_eq!(layout.groups, before, "nothing moved");
    // Only items that appear afterwards are new.
    let fresh = file("fresh.txt");
    assert!(!layout.is_seen(&fresh.name));
    let ru = round_up(
        vec![rule(
            "txt",
            vec![kind(IsOp::Is, ItemType::File)],
            corral(accounts),
        )],
        Destination::Desktop,
    );
    let plan = run(&ru, &layout, &[member, fresh], false);
    assert_eq!(plan.manual_count(), 1);
    assert_eq!(entry(&plan, "fresh.txt").status, PlanStatus::Move);
}

#[test]
fn a_layout_from_the_old_engine_still_gets_the_one_time_pass() {
    // The old engine had already set `seen_initialized`; only `round_up_initialized` counts.
    let (mut layout, accounts, pictures) = world();
    layout.seen_initialized = true;
    layout.round_up_initialized = false;
    let member = file("member.txt");
    layout.insert_item(&member.name, accounts, usize::MAX);
    // icons the layout knows but an early-boot read missed
    layout.place_loose(
        "desk/later.txt",
        crate::model::LoosePosition {
            monitor: "m".into(),
            x: 1,
            y: 1,
        },
    );
    layout.set_icon_page("desk/paged.txt", 2);
    layout.mark_manual("desk/old-mark.txt", 0);
    let groups_before = layout.groups.clone();
    let present = vec![member.name.clone(), "desk/other.txt".to_string()];
    assert!(layout.round_up_observe(&present, crate::model::MANUAL_FORGET_MS * 2));
    assert!(layout.round_up_initialized);
    assert!(layout.is_manual(&member.name) && layout.is_seen(&member.name));
    assert!(layout.is_seen("desk/other.txt") && !layout.is_manual("desk/other.txt"));
    assert!(layout.is_seen("desk/later.txt") && layout.is_seen("desk/paged.txt"));
    assert!(
        layout.is_manual("desk/old-mark.txt"),
        "nothing is pruned in that pass"
    );
    assert_eq!(layout.groups, groups_before, "nothing moved");
    // and it runs once (the stale mark is pruned by the next ordinary read, so drop it first)
    layout.clear_manual("desk/old-mark.txt");
    assert!(!layout.round_up_observe(&present, crate::model::MANUAL_FORGET_MS * 2));
    let ru = round_up(
        vec![rule(
            "txt",
            vec![kind(IsOp::Is, ItemType::File)],
            corral(pictures),
        )],
        Destination::Desktop,
    );
    assert_eq!(run(&ru, &layout, &[member], false).manual_count(), 1);
    assert!(
        Layout::new().round_up_initialized,
        "a brand-new layout has nothing to protect"
    );
}

#[test]
fn letting_round_up_decide_judges_a_manual_corral_member_at_once() {
    let (mut layout, accounts, pictures) = world();
    let ru = round_up(
        vec![rule(
            "pdf",
            vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
            corral(pictures),
        )],
        Destination::Desktop,
    );
    let member = file("a.pdf");
    let other = file("b.pdf");
    layout.insert_item(&member.name, accounts, usize::MAX);
    layout.insert_item(&other.name, accounts, usize::MAX);
    layout.mark_manual(&member.name, 1);
    layout.mark_manual(&other.name, 1);
    let moved = hand_back(&ru, &mut layout, &[member.clone()], now(), &Fixed(0));
    assert_eq!(moved, 1);
    assert_eq!(layout.group_of(&member.name), Some(pictures));
    assert!(!layout.is_manual(&member.name) && layout.is_seen(&member.name));
    assert_eq!(
        layout.group_of(&other.name),
        Some(accounts),
        "other icons keep their mark and place"
    );
    assert!(layout.is_manual(&other.name));
    // an item no rule wants stays where it is, no longer manual
    let stay = file("c.txt");
    layout.insert_item(&stay.name, accounts, usize::MAX);
    layout.mark_manual(&stay.name, 1);
    assert_eq!(
        hand_back(&ru, &mut layout, &[stay.clone()], now(), &Fixed(0)),
        0
    );
    assert_eq!(layout.group_of(&stay.name), Some(accounts));
    assert!(!layout.is_manual(&stay.name));
}

#[test]
fn members_that_are_not_on_the_desktop_right_now_are_manual_too() {
    let (mut layout, accounts, _) = world();
    layout.round_up_initialized = false;
    layout.insert_item("C:\\desk\\away.txt", accounts, usize::MAX);
    layout.round_up_observe(&["C:\\desk\\other.txt".to_string()], 1);
    assert!(layout.is_manual("C:\\desk\\away.txt"));
}

#[test]
fn an_empty_desktop_does_not_initialise_an_older_layout_yet() {
    let mut layout = Layout::new();
    layout.round_up_initialized = false;
    assert!(!layout.round_up_observe(&[], 1));
    assert!(!layout.round_up_initialized);
}

#[test]
fn a_renamed_file_is_a_new_item_without_the_old_ones_standing() {
    let (mut layout, accounts, _) = world();
    let ru = round_up(
        vec![rule(
            "pdf",
            vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
            corral(accounts),
        )],
        Destination::Desktop,
    );
    let old = file("old.pdf");
    let planned = run(&ru, &layout, &[old.clone()], false);
    apply_plan(&mut layout, &planned);
    assert!(layout.is_seen(&old.name));
    layout.mark_manual(&old.name, 1);
    let renamed = file("renamed.pdf");
    assert!(!layout.is_seen(&renamed.name));
    assert!(!layout.is_manual(&renamed.name));
}

// ---------------------------------------------------------------------------------------------
// Idempotence and run timing

#[test]
fn applying_a_plan_and_planning_again_gives_an_empty_plan() {
    let (mut layout, accounts, pictures) = world();
    let ru = round_up(
        vec![
            rule(
                "pdf",
                vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
                corral(accounts),
            ),
            rule(
                "img",
                vec![ext(ExtensionOp::IsOneOf, &["png"])],
                corral(pictures),
            ),
        ],
        Destination::Desktop,
    );
    let items = [file("a.pdf"), file("b.png"), file("c.txt")];
    let first = run(&ru, &layout, &items, false);
    assert_eq!(first.move_count(), 2);
    assert_eq!(apply_plan(&mut layout, &first), 2);
    let second = run(&ru, &layout, &items, false);
    assert_eq!(second.move_count(), 0);
    assert_eq!(apply_plan(&mut layout, &second), 0);
    assert_eq!(
        apply_plan(&mut layout, &first),
        0,
        "the old plan no longer applies"
    );
    assert_eq!(layout.group_of(&items[0].name), Some(accounts));
    assert_eq!(layout.group_of(&items[1].name), Some(pictures));
}

#[test]
fn a_new_item_is_judged_after_two_quiet_seconds() {
    let sig = (Some(10), Some(1));
    let at = |t: &mut RunTracker, ms: i64, sig| t.observe("A.pdf", sig, ms, false, false);
    let mut tracker = RunTracker::default();
    assert_eq!(
        at(&mut tracker, 0, sig),
        Observation {
            ready: false,
            settling: true
        }
    );
    assert!(at(&mut tracker, 1999, sig).settling);
    assert!(at(&mut tracker, 2000, sig).ready);
    // a download still growing keeps resetting the wait
    let mut growing = RunTracker::default();
    assert!(at(&mut growing, 0, (Some(1), Some(1))).settling);
    assert!(at(&mut growing, 1500, (Some(2), Some(1))).settling);
    assert!(at(&mut growing, 3499, (Some(2), Some(1))).settling);
    assert!(at(&mut growing, 3500, (Some(2), Some(1))).ready);
}

#[test]
fn keep_active_looks_again_only_at_changed_seen_items() {
    let mut tracker = RunTracker::default();
    let (before, after) = ((Some(10), Some(1)), (Some(99), Some(2)));
    // seen and unchanged: never
    for t in [0, 5000, 10_000] {
        let look = tracker.observe("a.pdf", before, t, true, true);
        assert_eq!(
            look,
            Observation {
                ready: false,
                settling: false
            }
        );
    }
    // changed: after the settle time, only with Keep Round-up active
    assert!(tracker.observe("a.pdf", after, 11_000, true, true).settling);
    assert!(tracker.observe("a.pdf", after, 13_000, true, true).ready);
    let mut off = RunTracker::default();
    off.observe("a.pdf", before, 0, true, false);
    assert_eq!(
        off.observe("a.pdf", after, 20_000, true, false),
        Observation {
            ready: false,
            settling: false
        }
    );
    // once judged, it is quiet again
    tracker.judged("a.pdf");
    assert_eq!(
        tracker.observe("a.pdf", after, 14_000, true, true),
        Observation {
            ready: false,
            settling: false
        }
    );
    // an item that could not be checked is not decided and is retried
    let mut retry = RunTracker::default();
    assert!(retry.observe("b.pdf", before, 0, false, false).settling);
    assert!(retry.observe("b.pdf", before, 2000, false, false).ready);
    assert!(retry.observe("b.pdf", before, 4000, false, false).ready);
    retry.forget("b.pdf");
    assert!(
        retry.observe("b.pdf", before, 5000, false, false).settling,
        "forgotten items start over"
    );
}

// ---------------------------------------------------------------------------------------------
// Storage (spec 7, "Roundtrip")

fn every_condition() -> Vec<Condition> {
    vec![
        kind(IsOp::IsNot, ItemType::SystemItem),
        ext(ExtensionOp::IsOneOf, &["pdf", "docx"]),
        ext(ExtensionOp::HasNone, &[]),
        name(TextOp::MatchesPattern, "inv*"),
        size(SizeOp::AtLeast, 10, SizeUnit::MB),
        size(SizeOp::LessThan, 1, SizeUnit::GB),
        modified(DateOp::NewerThan, 7, TimeUnit::Days),
        modified(DateOp::OlderThan, 3, TimeUnit::Months),
        Condition::ShortcutTarget {
            op: IsOp::Is,
            value: TargetKind::Folder,
        },
        Condition::TargetExtension {
            op: ExtensionOp::IsNoneOf,
            value: vec!["exe".into()],
        },
        Condition::TargetPath {
            op: TextOp::Contains,
            value: "C:\\Tools".into(),
        },
        Condition::Unknown {
            raw: json!({"type": "future_kind", "op": "x", "value": [1, 2]}),
        },
    ]
}

#[test]
fn every_condition_type_roundtrips_to_identical_json() {
    let mut r = rule("r-0f3a", every_condition(), corral(12));
    r.legacy = json!({"place_at_end": true, "auto_id": "30"});
    let block = round_up(vec![r], Destination::Desktop);
    let first = serde_json::to_string_pretty(&block).unwrap();
    let loaded: RoundUp = serde_json::from_str(&first).unwrap();
    assert_eq!(loaded.rules, block.rules);
    let second = serde_json::to_string_pretty(&loaded).unwrap();
    assert_eq!(first, second);
}

#[test]
fn the_stored_shape_is_the_one_in_the_spec() {
    let r = rule(
        "r-0f3a",
        vec![
            ext(ExtensionOp::IsOneOf, &["pdf"]),
            size(SizeOp::AtLeast, 10, SizeUnit::MB),
            modified(DateOp::NewerThan, 7, TimeUnit::Days),
        ],
        corral(12),
    );
    let value = serde_json::to_value(&r).unwrap();
    assert_eq!(
        value["conditions"][0],
        json!({"type": "extension", "op": "is_one_of", "value": ["pdf"]})
    );
    assert_eq!(
        value["conditions"][1],
        json!({"type": "size", "op": "at_least", "value": {"n": 10, "unit": "MB"}})
    );
    assert_eq!(
        value["conditions"][2],
        json!({"type": "modified", "op": "newer_than", "value": {"n": 7, "unit": "days"}})
    );
    assert_eq!(value["destination"], json!({"kind": "corral", "id": 12}));
    assert_eq!(value["match"], "all");
    assert!(
        value.get("legacy").is_none(),
        "no legacy block unless migrated"
    );
    assert_eq!(
        serde_json::to_value(Destination::Desktop).unwrap(),
        json!({"kind": "desktop"})
    );
    let block = serde_json::to_value(round_up(vec![r], Destination::Desktop)).unwrap();
    for key in [
        "version",
        "migrated_from_legacy",
        "default_destination",
        "rules",
        "needs_review",
    ] {
        assert!(block.get(key).is_some(), "{key}");
    }
    assert!(block.get("paused_reason").is_none() && block.get("migration_pending").is_none());
}

#[test]
fn an_unknown_condition_disables_its_rule_and_is_kept() {
    let raw = json!({"type": "future_kind", "op": "x", "value": [1, 2]});
    let loaded: Condition = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(loaded, Condition::Unknown { raw: raw.clone() });
    assert_eq!(serde_json::to_value(&loaded).unwrap(), raw);
    let r = rule("r", vec![loaded, kind(IsOp::Is, ItemType::File)], corral(1));
    assert!(r.needs_update() && !r.runs());
    // a known type with an unreadable operator is also kept instead of being lost
    let odd: Condition = serde_json::from_value(
        json!({"type": "size", "op": "sideways", "value": {"n": 1, "unit": "MB"}}),
    )
    .unwrap();
    assert!(matches!(odd, Condition::Unknown { .. }));
}

#[test]
fn new_rules_go_above_the_built_in_ones_and_can_be_reordered() {
    let mut ru = RoundUp::default();
    assert_eq!(
        ru.rules
            .iter()
            .map(|r| r.preset.clone().unwrap())
            .collect::<Vec<_>>(),
        [
            "program_shortcuts",
            "web_links",
            "folders",
            "documents",
            "images",
            "music_video",
            "downloads"
        ]
    );
    let first = ru.new_user_rule();
    assert!(ru.rules.iter().all(|r| r.id != first.id));
    assert_eq!(ru.insert_rule(first.clone()), 0);
    let second = ru.new_user_rule();
    assert_ne!(second.id, first.id);
    assert_eq!(
        ru.insert_rule(second.clone()),
        1,
        "right above the first built-in rule"
    );
    assert_eq!(
        (ru.rules[0].id.as_str(), ru.rules[1].id.as_str()),
        (first.id.as_str(), second.id.as_str())
    );
    assert!(ru.rules[2].preset.is_some());
    assert_eq!(ru.move_rule(0, true), None);
    assert_eq!(ru.move_rule(0, false), Some(1));
    assert_eq!(ru.rules[0].id, second.id);
    assert_eq!(ru.move_rule(ru.rules.len() - 1, false), None);
}

#[test]
fn built_in_rules_use_the_suggested_lists() {
    let documents = preset_rule("documents").unwrap();
    assert_eq!(
        documents.conditions,
        vec![ext(
            ExtensionOp::IsOneOf,
            &[
                "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "rtf", "odt", "ods",
                "csv", "md"
            ]
        )]
    );
    let program = preset_rule("program_shortcuts").unwrap();
    assert_eq!(
        program.conditions,
        vec![
            kind(IsOp::Is, ItemType::Shortcut),
            Condition::ShortcutTarget {
                op: IsOp::Is,
                value: TargetKind::Program
            }
        ]
    );
    assert_eq!(
        preset_rule("folders").unwrap().conditions,
        vec![kind(IsOp::Is, ItemType::Folder)]
    );
    assert!(preset_rule("nonsense").is_none());
}

// ---------------------------------------------------------------------------------------------
// Sentences and the live caption

#[test]
fn the_sentence_reads_as_the_rule_and_is_the_automatic_name() {
    let mut r = rule(
        "r",
        vec![
            ext(ExtensionOp::IsOneOf, &["pdf"]),
            name(TextOp::Contains, "invoice"),
        ],
        corral(1),
    );
    r.name = String::new();
    let sentence = r.sentence("Accounts");
    assert_eq!(
        sentence,
        "When an item has the extension pdf and its name contains \"invoice\", put it in Accounts."
    );
    assert_eq!(r.title("Accounts"), sentence);
    r.match_mode = MatchMode::Any;
    assert!(r.sentence("Accounts").contains(" or its name"));
    r.name = "Invoices".into();
    assert_eq!(r.title("Accounts"), "Invoices");
    r.destination = Some(Destination::Desktop);
    assert!(r.sentence("x").ends_with("leave it on the desktop."));
    assert_eq!(
        modified(DateOp::NewerThan, 1, TimeUnit::Days).phrase(),
        "was changed in the last 1 day"
    );
    assert_eq!(
        modified(DateOp::OlderThan, 2, TimeUnit::Weeks).phrase(),
        "was last changed more than 2 weeks ago"
    );
}

#[test]
fn the_live_caption_counts_matches_rules_above_and_unreadable_icons() {
    let big_pdf = rule(
        "r",
        vec![
            ext(ExtensionOp::IsOneOf, &["pdf"]),
            size(SizeOp::AtLeast, 1, SizeUnit::KB),
        ],
        corral(1),
    );
    let items = [
        sized("a.pdf", 5 * KB),
        sized("b.pdf", 5 * KB),
        sized("c.pdf", 5 * KB),
        sized("d.pdf", 5 * KB),
        ItemFacts {
            size: None,
            ..file("e.pdf")
        },
        sized("f.txt", 5 * KB),
    ];
    let starts = |letter: &str| rule(letter, vec![name(TextOp::StartsWith, letter)], corral(2));
    assert_eq!(
        caption(
            &big_pdf,
            &[starts("a"), starts("b")],
            &items,
            now(),
            &Fixed(0)
        ),
        "Matches 4 icons now: a.pdf, b.pdf, c.pdf and 1 more. 2 are taken by rules above. 1 could not be checked."
    );
    assert_eq!(
        caption(&big_pdf, &[starts("a")], &items, now(), &Fixed(0)),
        "Matches 4 icons now: a.pdf, b.pdf, c.pdf and 1 more. 1 is taken by a rule above. 1 could not be checked."
    );
    let none = rule("none", vec![name(TextOp::Is, "zzz")], corral(1));
    assert_eq!(
        caption(&none, &[], &items, now(), &Fixed(0)),
        "Matches no icons now."
    );
    let one = rule("one", vec![ext(ExtensionOp::IsOneOf, &["txt"])], corral(1));
    assert_eq!(
        caption(&one, &[], &items, now(), &Fixed(0)),
        "Matches 1 icon now: f.txt."
    );
}

// ---------------------------------------------------------------------------------------------
// Migration (spec 7, "Migration")

fn old(name: &str, criteria: &[&str], enabled: u8, target: i64, auto: i32) -> StoredRule {
    StoredRule {
        data: format!("{name}|{}|", criteria.join("|")),
        target: format!("{enabled}|{target}|1|{auto}"),
    }
}

fn converted(stored: &[StoredRule]) -> RoundUp {
    RoundUp::migrate(stored, -1)
}

fn user_rules(ru: &RoundUp) -> Vec<&Rule> {
    ru.rules.iter().filter(|r| r.preset.is_none()).collect()
}

#[test]
fn a_simple_single_criterion_converts_with_name_enabled_flag_and_destination() {
    let ru = converted(&[old("Invoices", &["<type>:=:0:pdf, .DOCX"], 1, 3, 0)]);
    let r = &ru.rules[0];
    assert_eq!(r.name, "Invoices");
    assert!(r.enabled);
    assert_eq!(r.destination, Some(corral(3)));
    assert_eq!(r.match_mode, MatchMode::All);
    assert_eq!(
        r.conditions,
        vec![ext(ExtensionOp::IsOneOf, &["pdf", "DOCX"])]
    );
    assert_eq!(r.legacy, json!({"place_at_end": true, "auto_id": "0"}));
    assert!(ru.needs_review.is_empty());
    assert!(ru.migration_pending && ru.migrated_from_legacy);
}

#[test]
fn criteria_joined_only_by_and_become_all_and_only_by_or_become_any() {
    let and = converted(&[old("A", &["<type>:D:0:", "<size>:>:10485760:"], 1, 2, 0)]);
    assert_eq!(and.rules[0].match_mode, MatchMode::All);
    assert_eq!(
        and.rules[0].conditions,
        vec![
            kind(IsOp::Is, ItemType::Folder),
            size(SizeOp::AtLeast, 10, SizeUnit::MB)
        ]
    );
    let or = converted(&[old(
        "B",
        &["<type>:=:0:pdf", "<or><type>:=:0:doc", "<or><type>:!:0:tmp"],
        1,
        2,
        0,
    )]);
    assert_eq!(or.rules[0].match_mode, MatchMode::Any);
    assert_eq!(or.rules[0].conditions.len(), 3);
    assert_eq!(
        or.rules[0].conditions[2],
        ext(ExtensionOp::IsNoneOf, &["tmp"])
    );
}

#[test]
fn mixed_and_or_is_never_guessed_and_keeps_its_text_verbatim() {
    let mixed = old(
        "Mixed",
        &["<type>:=:0:pdf", "<or><type>:=:0:doc", "<type>:D:0:"],
        1,
        4,
        0,
    );
    let ru = converted(&[mixed.clone()]);
    assert!(user_rules(&ru).is_empty());
    let item = &ru.needs_review[0];
    assert_eq!((item.position, item.name.as_str()), (0, "Mixed"));
    assert_eq!(
        (item.data.as_str(), item.target.as_str()),
        (mixed.data.as_str(), mixed.target.as_str())
    );
    assert!(item.reason.contains("and") && item.reason.contains("or"));
}

#[test]
fn rules_that_cannot_convert_one_to_one_go_to_review_in_their_old_order() {
    let stored = [
        old("Keep", &["<type>:=:0:pdf"], 1, 3, 0),
        old("{By name}", &["<name>:=:0:invoice"], 1, 3, 0),
        old("Times", &["<timeofday>:<:60:"], 1, 3, 0),
        old("Date", &["<modifieddate>:<:7:"], 1, 3, 0),
        old("Between", &["<size>:B:100:200"], 1, 3, 0),
        old("Odd size", &["<size>:<:1536:"], 1, 3, 0),
        old("Is file", &["<type>:F:0:"], 1, 3, 0),
        old("Not contains", &["<target>:!:0:x"], 1, 3, 0),
        old("Tilde", &["<type>:~:0:pdf"], 1, 3, 0),
    ];
    let ru = converted(&stored);
    assert_eq!(user_rules(&ru).len(), 1);
    assert_eq!(
        ru.needs_review
            .iter()
            .map(|r| r.position)
            .collect::<Vec<_>>(),
        (1..9).collect::<Vec<_>>()
    );
    assert_eq!(
        ru.needs_review[0].name, "By name",
        "a generated name loses its braces"
    );
    assert!(
        ru.needs_review
            .iter()
            .all(|r| !r.reason.is_empty() && !r.data.is_empty())
    );
}

#[test]
fn zero_criteria_and_malformed_shapes_go_to_review() {
    let stored = [
        StoredRule {
            data: "Nothing|".into(),
            target: "1|3|1|0".into(),
        },
        StoredRule {
            data: "Bad|<type>=pdf|".into(),
            target: "1|3|1|0".into(),
        },
        StoredRule {
            data: "Target|<type>:=:0:pdf|".into(),
            target: "garbage".into(),
        },
        StoredRule {
            data: "NoTarget|<type>:=:0:pdf|".into(),
            target: String::new(),
        },
        StoredRule {
            data: "BadFlag|<type>:=:0:pdf|".into(),
            target: "2|3|1|0".into(),
        },
        StoredRule {
            data: "BadCorral|<type>:=:0:pdf|".into(),
            target: "1|abc|1|0".into(),
        },
        StoredRule {
            data: "Zero|<type>:=:0:pdf|".into(),
            target: "1|0|1|0".into(),
        },
        StoredRule {
            data: "EmptyPart|<type>:=:0:pdf||".into(),
            target: "1|3|1|0".into(),
        },
        StoredRule {
            data: "FirstOr|<or><type>:=:0:pdf|".into(),
            target: "1|3|1|0".into(),
        },
        StoredRule {
            data: String::new(),
            target: String::new(),
        },
        StoredRule {
            data: "null".into(),
            target: String::new(),
        },
    ];
    let ru = converted(&stored);
    assert!(user_rules(&ru).is_empty());
    assert_eq!(ru.needs_review.len(), stored.len());
    for (item, original) in ru.needs_review.iter().zip(&stored) {
        assert_eq!(
            (&item.data, &item.target),
            (&original.data, &original.target)
        );
    }
}

#[test]
fn the_desktop_target_the_missing_corral_and_the_default_destination() {
    let ru = RoundUp::migrate(
        &[
            old("OnDesktop", &["<type>:=:0:pdf"], 1, -1, 0),
            old("OffUnset", &["<type>:=:0:doc"], 0, -1, 0),
            old("Gone", &["<type>:=:0:png"], 1, 99, 0),
        ],
        5,
    );
    assert_eq!(ru.rules[0].destination, Some(Destination::Desktop));
    assert_eq!(
        ru.rules[1].destination, None,
        "disabled and never configured"
    );
    assert!(!ru.rules[1].enabled);
    assert_eq!(ru.rules[2].destination, Some(corral(99)));
    assert_eq!(ru.default_destination, corral(5));
    let (layout, _, _) = world();
    assert_eq!(
        ru.missing_destinations(|id| corral_exists(&layout, id)),
        2,
        "the rule's Corral 99 and the default"
    );
    assert_eq!(
        RoundUp::migrate(&[], -1).default_destination,
        Destination::Desktop
    );
    assert_eq!(
        RoundUp::migrate(&[], 0).default_destination,
        Destination::Desktop
    );
}

#[test]
fn a_recognised_preset_keeps_its_place_and_converted_conditions_and_missing_ones_are_added_off() {
    let ru = converted(&[
        old("Mine", &["<type>:=:0:pdf"], 1, 3, 0),
        old("{Images}", &["<type>:=:0:jpg, png"], 1, 4, 30),
        old("{Program Shortcuts}", &["<type>:P:0:"], 1, 4, 70),
    ]);
    assert_eq!(ru.rules[0].name, "Mine");
    let images = &ru.rules[1];
    assert_eq!(
        (
            images.preset.as_deref(),
            images.id.as_str(),
            images.name.as_str()
        ),
        (Some("images"), "preset-images", "Images")
    );
    assert_eq!(
        images.conditions,
        vec![ext(ExtensionOp::IsOneOf, &["jpg", "png"])]
    );
    assert!(images.enabled);
    assert_eq!(images.legacy["auto_id"], "30");
    let program = &ru.rules[2];
    assert_eq!(program.preset.as_deref(), Some("program_shortcuts"));
    assert_eq!(
        program.conditions,
        preset_rule("program_shortcuts").unwrap().conditions
    );
    assert_eq!(ru.rules.len(), 1 + 7);
    let added = &ru.rules[3..];
    assert_eq!(added.len(), 5);
    assert!(
        added
            .iter()
            .all(|r| !r.enabled && r.destination.is_none() && r.preset.is_some())
    );
    // a customised preset (negative id) is still that preset; a second copy is an ordinary rule
    let twice = converted(&[
        old("{Images}", &["<type>:=:0:jpg"], 1, 4, -30),
        old("{Images}", &["<type>:=:0:gif"], 1, 4, 30),
    ]);
    assert_eq!(twice.rules[0].preset.as_deref(), Some("images"));
    assert_eq!(twice.rules[1].preset, None);
    assert_eq!(twice.rules[1].name, "");
}

#[test]
fn link_target_criteria_that_match_one_to_one_convert_with_their_text_unescaped() {
    let ru = converted(&[
        old("A", &["<target>:=:0:chrome"], 1, 3, 0),
        old("B", &["<target>:#:0:C\u{2591}\\Tools"], 1, 3, 0),
        old("C", &["<target>:[:0:C"], 1, 3, 0),
        old("D", &["<target>:]:0:.exe"], 1, 3, 0),
    ]);
    let got: Vec<_> = ru
        .rules
        .iter()
        .take(4)
        .map(|r| r.conditions[0].clone())
        .collect();
    let target = |op, value: &str| Condition::TargetPath {
        op,
        value: value.into(),
    };
    assert_eq!(
        got,
        vec![
            target(TextOp::Contains, "chrome"),
            target(TextOp::Is, "C:\\Tools"),
            target(TextOp::StartsWith, "C"),
            target(TextOp::EndsWith, ".exe"),
        ]
    );
}

#[test]
fn sizes_convert_to_the_largest_whole_unit() {
    let criteria: Vec<String> = [10 * MB, 2 * KB, 3 * 1024 * MB, 0]
        .iter()
        .map(|bytes| format!("<size>:<:{bytes}:"))
        .collect();
    let stored: Vec<StoredRule> = criteria
        .iter()
        .map(|c| old("S", &[c.as_str()], 1, 3, 0))
        .collect();
    let ru = converted(&stored);
    let got: Vec<_> = ru
        .rules
        .iter()
        .take(4)
        .map(|r| r.conditions[0].clone())
        .collect();
    assert_eq!(
        got,
        vec![
            size(SizeOp::LessThan, 10, SizeUnit::MB),
            size(SizeOp::LessThan, 2, SizeUnit::KB),
            size(SizeOp::LessThan, 3, SizeUnit::GB),
            size(SizeOp::LessThan, 0, SizeUnit::KB),
        ]
    );
}

#[test]
fn converting_twice_gives_the_same_result() {
    let stored = [
        old("A", &["<type>:=:0:pdf"], 1, 3, 0),
        old("B", &["<name>:=:0:x"], 1, 3, 0),
    ];
    assert_eq!(converted(&stored), converted(&stored));
}

#[test]
fn rewriting_a_review_item_prefills_whatever_mapped() {
    let ru = converted(&[old(
        "Mixed",
        &["<type>:=:0:pdf", "<name>:=:0:invoice", "<size>:>:1048576:"],
        1,
        3,
        0,
    )]);
    assert_eq!(ru.needs_review.len(), 1);
    let prefill = ru.needs_review[0].prefill("r-new".into());
    assert_eq!(prefill.id, "r-new");
    assert_eq!(prefill.destination, Some(corral(3)));
    assert_eq!(prefill.name, "Mixed");
    assert_eq!(
        prefill.conditions,
        vec![
            ext(ExtensionOp::IsOneOf, &["pdf"]),
            size(SizeOp::AtLeast, 1, SizeUnit::MB)
        ]
    );
    let nothing = converted(&[StoredRule {
        data: "x|".into(),
        target: "garbage".into(),
    }]);
    let blank = nothing.needs_review[0].prefill("r".into());
    assert_eq!(blank.conditions.len(), 1);
    assert_eq!(blank.destination, None);
}

// ---------------------------------------------------------------------------------------------
// No file operation, anywhere (spec 7, "Never a file operation")

/// Production source of a module: everything before its test module, without comment lines.
fn production(source: &str) -> String {
    let code = source.split("#[cfg(test)]").next().unwrap();
    code.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_round_up_code_has_no_way_to_write_a_file() {
    let sources = [
        ("model", include_str!("model.rs")),
        ("eval", include_str!("eval.rs")),
        ("plan", include_str!("plan.rs")),
        ("preview", include_str!("preview.rs")),
        ("migrate", include_str!("migrate.rs")),
        ("round_up", include_str!("../round_up.rs")),
        ("round_up_time", include_str!("../round_up_time.rs")),
    ];
    let forbidden = [
        "std::fs",
        "fs::",
        "File::",
        "OpenOptions",
        "remove_file",
        "remove_dir",
        "rename(",
        "copy(",
        "write(",
        "create_dir",
        "set_permissions",
        "std::process",
        "SHFileOperation",
        "IFileOperation",
        "MoveFile",
        "DeleteFile",
        "CopyFile",
        "ShellExecute",
    ];
    for (module, source) in sources {
        let code = production(source);
        for word in forbidden {
            assert!(!code.contains(word), "{module} mentions {word}");
        }
    }
}

#[test]
fn applying_a_plan_touches_the_layouts_membership_functions_and_nothing_else() {
    let code: String = production(include_str!("plan.rs"))
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let allowed = [
        "group_of",
        "is_manual",
        "insert_item",
        "assign_item",
        "clear_manual",
        "mark_seen",
        "groups",
    ];
    let mut rest = code.as_str();
    let mut seen = 0;
    while let Some(at) = rest.find("layout.") {
        rest = &rest[at + "layout.".len()..];
        let ident: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        assert!(
            allowed.contains(&ident.as_str()),
            "plan.rs uses layout.{ident}"
        );
        seen += 1;
    }
    assert!(seen >= 6, "the scan found the layout calls");
    assert!(code.contains("layout.insert_item") && code.contains("layout.assign_item"));
}

#[test]
fn a_full_sort_over_a_fake_desktop_leaves_the_fake_file_tree_equal() {
    let tree = FakeTree::default()
        .file("C:\\desk\\invoice.pdf", 3 * KB, Some(1))
        .file("C:\\desk\\photo.png", 2 * MB, Some(2))
        .file("C:\\desk\\notes.txt", 10, Some(3))
        .folder("C:\\desk\\Projects")
        .link("C:\\desk\\tool.lnk", "C:\\Apps\\tool.exe")
        .file("C:\\Apps\\tool.exe", 1, Some(1))
        .link("C:\\desk\\broken.lnk", "C:\\gone\\x.exe");
    let before = tree.fingerprint();
    let (mut layout, ids) = layout_with(&["Documents", "Pictures", "Programs"]);
    let program_shortcut = vec![
        kind(IsOp::Is, ItemType::Shortcut),
        Condition::ShortcutTarget {
            op: IsOp::Is,
            value: TargetKind::Program,
        },
    ];
    let ru = round_up(
        vec![
            rule("programs", program_shortcut, corral(ids[2])),
            rule(
                "documents",
                vec![ext(ExtensionOp::IsOneOf, &["pdf", "txt"])],
                corral(ids[0]),
            ),
            rule(
                "pictures",
                vec![ext(ExtensionOp::IsOneOf, &["png"])],
                corral(ids[1]),
            ),
        ],
        Destination::Desktop,
    );
    let names = [
        "invoice.pdf",
        "photo.png",
        "notes.txt",
        "Projects",
        "tool.lnk",
        "broken.lnk",
    ];
    let items: Vec<ItemFacts> = names
        .iter()
        .map(|n| {
            let mut shell = item(&format!("C:\\desk\\{n}"));
            shell.is_folder = *n == "Projects";
            collect_with(&shell, &tree)
        })
        .collect();
    let planned = run(&ru, &layout, &items, false);
    assert_eq!(entry(&planned, "invoice.pdf").to, corral(ids[0]));
    assert_eq!(entry(&planned, "photo.png").to, corral(ids[1]));
    assert_eq!(entry(&planned, "tool.lnk").to, corral(ids[2]));
    assert!(
        matches!(
            entry(&planned, "broken.lnk").status,
            PlanStatus::Unchecked(_)
        ),
        "an unreadable target halts the item"
    );
    assert_eq!(entry(&planned, "Projects").status, PlanStatus::NoChange);
    assert_eq!(apply_plan(&mut layout, &planned), 4);
    assert_eq!(
        tree.fingerprint(),
        before,
        "the fake file tree is unchanged"
    );
    assert!(!tree.reads.borrow().is_empty());
}

// ---------------------------------------------------------------------------------------------
// Preview (spec 7, "Preview")

#[test]
fn the_preview_shows_exactly_the_plan_that_is_applied_and_its_counts_add_up() {
    let (mut layout, accounts, _) = world();
    let ru = round_up(
        vec![rule(
            "Accounts",
            vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
            corral(accounts),
        )],
        Destination::Desktop,
    );
    layout.mark_manual("C:\\desk\\mine.pdf", 1);
    let items = vec![
        file("a.pdf"),
        file("b.pdf"),
        file("c.txt"),
        file("mine.pdf"),
        file("d.bin"),
    ];
    let planned = run(&ru, &layout, &items, false);
    assert_eq!(
        planned,
        run(&ru, &layout, &items, false),
        "planning is deterministic"
    );
    let counts = (
        planned.move_count(),
        planned.unchecked_count(),
        planned.manual_count(),
        planned.unchanged_count(),
    );
    assert_eq!(counts.0 + counts.1 + counts.2 + counts.3, items.len());
    assert_eq!(counts, (2, 0, 1, 2));
    let name_of = |d: &Destination| match d {
        Destination::Desktop => "the desktop".to_string(),
        Destination::Corral { .. } => "Accounts".to_string(),
    };
    let spec = preview_spec(&planned, false, &name_of);
    assert_eq!(spec.title, "Sort the desktop now");
    assert_eq!(spec.width, 680.);
    assert_eq!(spec.primary.label, "Move 2 icons");
    assert_eq!(spec.secondary.as_deref(), Some("Cancel"));
    let texts: Vec<_> = spec.rows.iter().map(|r| r.text.as_str()).collect();
    assert!(texts.contains(&"Will move (2)"));
    assert!(texts.contains(&"a.pdf \u{2192} Accounts"));
    assert!(texts.contains(&"Accounts: has the extension pdf"));
    assert!(texts.contains(&"Skipped, placed by you (1)"));
    assert!(texts.contains(&"No change (2)"));
    assert_eq!(spec.checks.len(), 1);
    assert_eq!(spec.checks[0].label, "Include icons I placed myself");
    assert!(!spec.checks[0].checked);
    // ticking the box re-plans with the placed-by-me icons included
    let included = run(&ru, &layout, &items, true);
    let spec = preview_spec(&included, true, &name_of);
    assert_eq!(spec.primary.label, "Move 3 icons");
    assert!(spec.checks[0].checked);
    // applying the reviewed plan gives exactly what it listed
    assert_eq!(apply_plan(&mut layout, &included), 3);
    for e in included
        .entries
        .iter()
        .filter(|e| e.status == PlanStatus::Move)
    {
        assert_eq!(layout.group_of(&e.item), Some(accounts));
    }
}

#[test]
fn an_empty_plan_says_so_and_offers_only_close() {
    let (layout, _, _) = world();
    let planned = run(
        &round_up(vec![], Destination::Desktop),
        &layout,
        &[file("a.txt")],
        false,
    );
    let spec = preview_spec(&planned, false, &|_| "x".into());
    assert!(spec.body.starts_with("Nothing to move"));
    assert_eq!(spec.primary.label, "Close");
    assert!(spec.secondary.is_none());
    assert!(
        spec.checks.is_empty(),
        "no icons of the user's own, so no checkbox"
    );
}

#[test]
fn icons_that_could_not_be_checked_are_listed_with_their_reason() {
    let (layout, accounts, _) = world();
    let ru = round_up(
        vec![rule(
            "big",
            vec![size(SizeOp::AtLeast, 1, SizeUnit::MB)],
            corral(accounts),
        )],
        Destination::Desktop,
    );
    let planned = run(
        &ru,
        &layout,
        &[ItemFacts {
            size: None,
            ..file("x.bin")
        }],
        false,
    );
    let spec = preview_spec(&planned, false, &|_| "x".into());
    let texts: Vec<String> = spec.rows.iter().map(|r| r.text.clone()).collect();
    assert!(texts.contains(&"Could not be checked (1)".to_string()));
    assert!(
        texts
            .iter()
            .any(|t| t.starts_with("big: The size could not be read.")
                && t.ends_with("It stays where it is."))
    );
}

// ---------------------------------------------------------------------------------------------
// Pausing

#[test]
fn a_paused_or_pending_round_up_decides_nothing() {
    let (layout, accounts, _) = world();
    let mut ru = round_up(
        vec![rule(
            "pdf",
            vec![ext(ExtensionOp::IsOneOf, &["pdf"])],
            corral(accounts),
        )],
        Destination::Desktop,
    );
    assert!(ru.active() && ru.has_work());
    ru.paused_reason = Some("Round-up is paused: newer version.".into());
    assert!(!ru.active() && !ru.has_work());
    let plan = run(&ru, &layout, &[file("a.pdf")], false);
    assert!(matches!(plan.entries[0].status, PlanStatus::Unchecked(_)));
    ru.paused_reason = None;
    ru.migration_pending = true;
    assert!(!ru.active());
    ru.migration_pending = false;
    ru.rules[0].enabled = false;
    assert!(!ru.has_work());
    ru.default_destination = corral(accounts);
    assert!(ru.has_work(), "a Corral default is work");
}
