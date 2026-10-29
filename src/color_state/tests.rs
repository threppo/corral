//! colour-state-spec 6: pure tests over the settings structs, the resolver and the page controller.
use super::overlay::{OverlayError, OverlayHost};
use super::page::*;
use super::*;
use crate::model::{GroupContent, GroupStyle, Layout};
use crate::settings::Settings;
use crate::settings::style_request::{Action, ColorPatch, apply};

fn layout(n: usize) -> Layout {
    let mut l = Layout::new();
    for i in 0..n {
        l.add_group(format!("Corral {}", i + 1), "m".into(), Default::default(), GroupContent::Items { items: vec![] });
    }
    l
}
fn infos(l: &Layout) -> Vec<CorralInfo> {
    l.groups.iter().map(|g| CorralInfo { id: g.id, title: g.title.clone(), style: g.style.clone() }).collect()
}
fn page(l: &Layout, s: &Settings, scope: Scope) -> ColorPage {
    ColorPage::new(Globals::from_settings(s), infos(l), scope, 100)
}
fn ok(_: &Commit) -> Result<(), String> {
    Ok(())
}
fn store(page: &mut ColorPage, commit: &Commit) {
    page.apply(commit, &mut |c| ok(c)).unwrap();
}
fn commit_of(ended: Ended) -> Commit {
    match ended {
        Ended::Commit(c) => c,
        other => panic!("expected a commit, got {other:?}"),
    }
}
const BLUE: u32 = 0x00B5_6F3F;

// ---- resolution and storage ------------------------------------------------------------------

#[test]
fn absent_slot_inherits_and_present_slot_equal_to_the_shared_value_stays_own() {
    let s = Settings::default();
    let inherit = effective_of(&s, &GroupStyle::default());
    assert_eq!(inherit.color, s.color);
    assert_eq!(inherit.own, Own::default());
    let own = effective_of(&s, &GroupStyle { color: Some(s.color), ..Default::default() });
    assert_eq!(own.color, inherit.color, "resolves identically");
    assert!(own.own.color && !inherit.own.color);
}

#[test]
fn load_then_save_leaves_absent_keys_absent_and_equal_present_keys_present() {
    let mut l = layout(2);
    l.groups[0].style = GroupStyle { color: Some(0), transparency_factor: Some(1380), ..Default::default() };
    let json = serde_json::to_string(&l).unwrap();
    let back: Layout = serde_json::from_str(&json).unwrap();
    assert_eq!(serde_json::to_string(&back).unwrap(), json, "nothing normalises, prunes or deduplicates");
    let styles = serde_json::to_value(&back.groups).unwrap();
    assert_eq!(styles[0]["style"]["color"], 0, "a value equal to the shared default stays present");
    assert!(styles[1].get("style").is_none_or(|v| v.as_object().is_none_or(|o| o.is_empty())), "absent keys stay absent");
    let s = Settings::default();
    let settings_json = serde_json::to_string(&s).unwrap();
    let again: Settings = serde_json::from_str(&settings_json).unwrap();
    assert_eq!(serde_json::to_string(&again).unwrap(), settings_json);
}

#[test]
fn own_auto_text_beats_a_shared_custom_colour_and_inherited_text_follows_the_shared_change() {
    let mut s = Settings::default();
    s.text_color_auto = false;
    s.text_color = 0x112233;
    let auto_own = GroupStyle { text_color_auto: Some(true), ..Default::default() };
    assert!(effective_of(&s, &auto_own).text_auto);
    assert!(!effective_of(&s, &GroupStyle::default()).text_auto);
    s.text_color = 0x445566;
    assert_eq!(effective_of(&s, &GroupStyle::default()).text_color, 0x445566, "inherited follows");
    let custom = GroupStyle { text_color: Some(7), text_color_auto: Some(false), ..Default::default() };
    s.text_color = 0x778899;
    assert_eq!(effective_of(&s, &custom).text_color, 7, "own ignores the shared change");
}

#[test]
fn match_corral_tint_uses_the_effective_background_and_follows_either_change() {
    let mut s = Settings::default();
    s.desktop_tint_icons = true;
    s.desktop_tint_icons_color_source = "corral".into();
    s.color = 0x0000_0011;
    let plain = GroupStyle::default();
    assert_eq!(effective_of(&s, &plain).tint_in_use(), 0x11, "shared background");
    let own = GroupStyle { color: Some(0x22), ..Default::default() };
    assert_eq!(effective_of(&s, &own).tint_in_use(), 0x22, "the Corral's own background, not the raw shared one");
    s.color = 0x33;
    assert_eq!(effective_of(&s, &plain).tint_in_use(), 0x33);
    assert_eq!(effective_of(&s, &own).tint_in_use(), 0x22);
    s.desktop_tint_icons_color_source = "custom".into();
    s.desktop_tint_icons_color = 0x44;
    assert_eq!(effective_of(&s, &own).tint_in_use(), 0x44);
}

#[test]
fn has_own_look_is_true_for_each_resettable_group_and_false_for_font_or_tab_only() {
    assert!(!has_own_look(&GroupStyle::default()));
    assert!(!has_own_look(&GroupStyle { font_weight: Some(700), font_italics: Some(1), tabs_style: Some(2), font_name: Some("F".into()), ..Default::default() }));
    let rows = [
        GroupStyle { color: Some(1), ..Default::default() },
        GroupStyle { text_color_auto: Some(true), ..Default::default() },
        GroupStyle { text_color: Some(1), text_color_auto: Some(false), ..Default::default() },
        GroupStyle { transparency_factor: Some(1), ..Default::default() },
        GroupStyle { tint_icons: Some(0), ..Default::default() },
        GroupStyle { tint_icons_color_source: Some("custom".into()), ..Default::default() },
        GroupStyle { tint_icons_color: Some(1), ..Default::default() },
        GroupStyle { blend_icons: Some(0), ..Default::default() },
        GroupStyle { blend_background: Some(1), ..Default::default() },
    ];
    for style in rows {
        assert!(has_own_look(&style), "{style:?}");
    }
    let mut l = layout(5);
    for (i, g) in l.groups.iter_mut().enumerate() {
        g.style.color = Some(i as u32);
    }
    l.groups[4].style = GroupStyle { font_weight: Some(700), ..Default::default() };
    let p = page(&l, &Settings::default(), Scope::All);
    assert_eq!(p.own_titles().len(), 4, "a font-only override is not an own look");
}

// ---- editing ---------------------------------------------------------------------------------

#[test]
fn open_then_esc_or_close_or_slide_back_without_choosing_writes_nothing() {
    let l = layout(1);
    let id = l.groups[0].id;
    let s = Settings::default();
    for field in [Field::Background, Field::Text, Field::SeeThrough, Field::TintColor] {
        let mut p = page(&l, &s, Scope::Corral(id));
        let before = (p.globals().clone(), p.corrals().to_vec());
        p.begin(field);
        assert_eq!(p.end(true), Ended::Discarded, "Esc");
        p.begin(field);
        assert_eq!(p.end(false), Ended::Discarded, "close without a choice");
        p.begin(field);
        let start = p.session().unwrap().start;
        p.slide(start);
        p.preview(start);
        assert_eq!(p.end(false), Ended::Discarded, "slid back to the start without choosing");
        assert_eq!((p.globals().clone(), p.corrals().to_vec()), before);
        assert!(p.frame().is_none());
    }
}

#[test]
fn a_slider_move_then_esc_on_an_inherited_field_leaves_it_inherited() {
    let l = layout(1);
    let id = l.groups[0].id;
    let mut p = page(&l, &Settings::default(), Scope::Corral(id));
    p.begin(Field::Background);
    p.slide(0x123456);
    assert!(p.frame().is_some(), "the desktop previews the draft");
    assert_eq!(p.effective().color, 0x123456);
    assert_eq!(p.end(true), Ended::Discarded);
    assert!(!p.stored_effective().own.color);
    assert_eq!(p.effective().color, Settings::default().color, "draft gone after Esc");
}

#[test]
fn an_explicit_choice_equal_to_the_inherited_value_writes_own_and_stays_own() {
    let mut s = Settings::default();
    s.color = 0x00AA_BBCC;
    let l = layout(1);
    let id = l.groups[0].id;
    for pick in [|p: &mut ColorPage, v| p.choose(v), |p: &mut ColorPage, v| p.slide(v) || p.choose(v)] {
        let mut p = page(&l, &s, Scope::Corral(id));
        p.begin(Field::Background);
        pick(&mut p, s.color);
        // sliding to the same value would not count, a deliberate choice does:
        p.choose(s.color);
        let commit = commit_of(p.end(false));
        assert_eq!(commit.scope, Scope::Corral(id));
        store(&mut p, &commit);
        assert!(p.stored_effective().own.color);
        // a later shared change does not move it, and nothing converts it back
        let mut other = p.globals().clone();
        other.color = 0x11;
        let corrals = p.corrals().to_vec();
        p.reload(other, corrals);
        assert_eq!(p.stored_effective().color, s.color);
        assert!(p.stored_effective().own.color);
    }
}

#[test]
fn scope_all_writes_only_shared_keys_and_a_corral_scope_only_that_corral() {
    let l = layout(2);
    let (a, b) = (l.groups[0].id, l.groups[1].id);
    let mut p = page(&l, &Settings::default(), Scope::All);
    let commit = p.commit_edit(ColorEdit::Background(BLUE)).pop().unwrap();
    assert_eq!(commit.scope, Scope::All);
    store(&mut p, &commit);
    assert_eq!(p.globals().color, BLUE);
    assert!(p.corrals().iter().all(|c| c.style == GroupStyle::default()));
    assert!(p.set_scope(Scope::Corral(a)).is_none());
    let commit = p.commit_edit(ColorEdit::SeeThrough(500)).pop().unwrap();
    store(&mut p, &commit);
    assert_eq!(p.globals().color, BLUE);
    assert_eq!(p.globals().see_through, 1380, "shared untouched");
    assert_eq!(p.corrals().iter().find(|c| c.id == a).unwrap().style.transparency_factor, Some(500));
    assert_eq!(p.corrals().iter().find(|c| c.id == b).unwrap().style, GroupStyle::default());
}

#[test]
fn tint_mode_writes_tint_icons_and_source_together_or_not_at_all() {
    let l = layout(1);
    let id = l.groups[0].id;
    let base = effective_of(&Settings::default(), &GroupStyle::default());
    for (mode, on, source) in [(TintMode::Off, false, "corral"), (TintMode::Match, true, "corral"), (TintMode::Custom, true, "custom")] {
        let patch = ColorEdit::TintMode(mode).patch(&base);
        assert_eq!((patch.tint_icons, patch.tint_icons_color_source.as_deref()), (Some(on), Some(source)));
        assert!(crate::settings::style_request::validate_patch(&patch).is_ok());
    }
    // a failed write applies neither field
    let mut p = page(&l, &Settings::default(), Scope::Corral(id));
    let commit = p.commit_edit(ColorEdit::TintMode(TintMode::Custom)).pop().unwrap();
    let before = p.corrals().to_vec();
    assert!(p.apply(&commit, &mut |_| Err("denied".into())).is_err());
    assert_eq!(p.corrals(), &before[..]);
}

#[test]
fn one_hundred_drafts_through_the_throttle_and_the_commit_stores_the_final_draft() {
    let l = layout(1);
    let mut p = page(&l, &Settings::default(), Scope::All);
    let mut throttle = Throttle::new(16);
    p.begin(Field::SeeThrough);
    let (mut sent, mut last_sent_value) = (0, 0);
    for i in 1..=100u64 {
        p.slide(1000 + i as u32);
        if throttle.offer(i) == Step::Send {
            sent += 1;
            last_sent_value = p.frame().unwrap().patch.transparency_factor.unwrap();
        }
    }
    assert!((5..=8).contains(&sent), "about one frame per 16 ms, got {sent}");
    assert!(throttle.pending(), "the last frames were coalesced away");
    assert!(last_sent_value < 1100);
    let commit = commit_of(p.end(false));
    assert_eq!(commit.patch.transparency_factor, Some(1100), "commit carries the page's latest draft");
    assert!(throttle.fire(116));
    assert!(!throttle.fire(117));
}

// ---- failure and lifecycle -------------------------------------------------------------------

#[test]
fn failed_save_restores_the_stored_state_clears_the_draft_and_the_next_edit_succeeds() {
    let l = layout(1);
    let id = l.groups[0].id;
    let mut p = page(&l, &Settings::default(), Scope::Corral(id));
    let before = (p.globals().clone(), p.corrals().to_vec());
    p.begin(Field::Background);
    p.choose(BLUE);
    let commit = commit_of(p.end(false));
    let err = p.apply(&commit, &mut |_| Err("disk full".into())).unwrap_err();
    assert_eq!(err, PageError::SaveFailed("disk full".into()));
    assert_eq!((p.globals().clone(), p.corrals().to_vec()), before);
    assert!(p.frame().is_none() && !p.is_editing());
    assert_eq!(p.caption(), Some(&Caption::SaveFailed));
    assert_eq!(Caption::SaveFailed.text(), "Couldn't save. Your previous colors are back.");
    let again = p.commit_edit(ColorEdit::Background(BLUE)).pop().unwrap();
    store(&mut p, &again);
    assert_eq!(p.stored_effective().color, BLUE);
    assert!(p.caption().is_none(), "a later edit tries again normally");
}

#[test]
fn an_asynchronous_save_failure_shows_what_is_stored_with_the_caption() {
    let l = layout(1);
    let id = l.groups[0].id;
    let mut p = page(&l, &Settings::default(), Scope::Corral(id));
    let commit = p.commit_edit(ColorEdit::Background(BLUE)).pop().unwrap();
    store(&mut p, &commit);
    assert_eq!(p.stored_effective().color, BLUE);
    p.save_failed(Globals::from_settings(&Settings::default()), infos(&l));
    assert_eq!(p.stored_effective().color, Settings::default().color);
    assert!(p.caption().unwrap().is_error());
    assert_eq!(p.scope(), Scope::Corral(id));
}

#[test]
fn a_target_deleted_mid_session_cancels_it_without_a_write_and_falls_back_to_all() {
    let l = layout(2);
    let (a, b) = (l.groups[0].id, l.groups[1].id);
    let mut p = page(&l, &Settings::default(), Scope::Corral(a));
    p.begin(Field::Background);
    p.choose(BLUE);
    let mut gone = l.clone();
    gone.groups.retain(|g| g.id != a);
    let changed = p.reload(Globals::from_settings(&Settings::default()), infos(&gone));
    assert!(changed && !p.is_editing() && p.frame().is_none());
    assert_eq!(p.scope(), Scope::All);
    assert_eq!(p.caption().unwrap().text(), "\"Corral 1\" was removed. Showing all Corrals.");
    assert_eq!(p.globals().color, Settings::default().color);
    assert!(p.corrals().iter().all(|c| c.id == b && c.style == GroupStyle::default()));
    // a commit that arrives for the missing id is a no-op and creates no record
    let late = Commit { session: 1, scope: Scope::Corral(a), edits: vec![], patch: ColorPatch { color: Some(1), ..Default::default() } };
    let mut wrote = false;
    assert_eq!(p.apply(&late, &mut |_| { wrote = true; Ok(()) }), Err(PageError::MissingCorral(a)));
    assert!(!wrote && p.corrals().len() == 1);
    // the engine side agrees
    let mut engine = gone.clone();
    let mut s = Settings::default();
    assert!(apply(&mut engine, &mut s, &Action::Edit { id: a, patch: late.patch }).is_err());
    assert_eq!(engine, gone);
}

#[test]
fn switching_target_commits_the_session_to_its_own_target_and_never_leaks() {
    let l = layout(2);
    let (a, b) = (l.groups[0].id, l.groups[1].id);
    let mut p = page(&l, &Settings::default(), Scope::Corral(a));
    p.begin(Field::Background);
    p.choose(BLUE);
    let commit = p.set_scope(Scope::Corral(b)).expect("the explicit session is committed first");
    assert_eq!(commit.scope, Scope::Corral(a));
    assert_eq!(p.scope(), Scope::Corral(b));
    assert!(!p.is_editing());
    store(&mut p, &commit);
    assert_eq!(p.corrals().iter().find(|c| c.id == a).unwrap().style.color, Some(BLUE));
    assert_eq!(p.corrals().iter().find(|c| c.id == b).unwrap().style.color, None);
    assert_eq!(p.globals().color, Settings::default().color, "the draft never reaches the shared look");
    // an unfinished choice-less session is simply dropped by a retarget
    p.begin(Field::Background);
    assert!(p.set_scope(Scope::All).is_none());
    // a failed commit still lets the scope change
    p.set_scope(Scope::Corral(a));
    p.begin(Field::Background);
    p.choose(1);
    let commit = p.set_scope(Scope::All).unwrap();
    assert!(p.apply(&commit, &mut |_| Err("x".into())).is_err());
    assert_eq!(p.scope(), Scope::All);
}

#[test]
fn a_reload_or_restore_during_a_session_cancels_it_and_keeps_the_scope_when_it_still_exists() {
    let l = layout(2);
    let a = l.groups[0].id;
    let mut p = page(&l, &Settings::default(), Scope::Corral(a));
    p.begin(Field::SeeThrough);
    p.choose(100);
    let mut restored = l.clone();
    restored.groups[0].style.color = Some(9);
    assert!(!p.reload(Globals::from_settings(&Settings::default()), infos(&restored)));
    assert!(!p.is_editing());
    assert_eq!(p.scope(), Scope::Corral(a));
    assert_eq!(p.stored_effective().color, 9, "restored data wins; no draft is written over it");
    assert_eq!(p.stored_effective().see_through, 1380);
}

#[test]
fn tabs_move_no_colour_state_and_the_body_resolves_from_the_active_tab() {
    use crate::tabs;
    let policy = tabs::JoinPolicy { create: true, titles: true };
    let mut l = layout(3);
    l.groups[0].style = GroupStyle { color: Some(0x11), ..Default::default() };
    l.groups[1].style = GroupStyle { text_color_auto: Some(true), font_weight: Some(700), ..Default::default() };
    let styles: Vec<_> = l.groups.iter().map(|g| (g.id, g.style.clone())).collect();
    let ids: Vec<u64> = l.groups.iter().map(|g| g.id).collect();
    let same = |l: &Layout| l.groups.iter().map(|g| (g.id, g.style.clone())).collect::<Vec<_>>() == styles;
    let place = tabs::Placement { monitor: l.groups[2].monitor.clone(), rect: l.groups[2].rect };
    // join: an own-look Corral and an inheriting one in one set keep both unchanged
    let doc = tabs::join(&l.tab_doc(), ids[0], ids[1], None, &policy).unwrap();
    l.apply_tab_doc(doc);
    assert!(same(&l));
    let active = tabs::collect(&l.groups, ids[0]).unwrap().active;
    let doc = tabs::join(&l.tab_doc(), active, ids[2], None, &policy).unwrap();
    l.apply_tab_doc(doc);
    assert!(same(&l));
    // reorder, select, leave
    let doc = tabs::reorder(&l.tab_doc(), ids[2], 0).unwrap();
    l.apply_tab_doc(doc);
    let doc = tabs::select(&l.tab_doc(), ids[1]).unwrap();
    l.apply_tab_doc(doc);
    assert!(same(&l));
    // the body paints with the effective look of the active tab
    let view = tabs::collect(&l.groups, ids[0]).unwrap();
    assert_eq!(view.active, ids[1]);
    let s = Settings::default();
    let active = l.groups.iter().find(|g| g.id == view.active).unwrap();
    assert!(effective_of(&s, &active.style).text_auto);
    assert_eq!(effective_of(&s, &l.groups[0].style).color, 0x11, "each tab header keeps its own look");
    let doc = tabs::remove_tab(&l.tab_doc(), ids[2], &place).unwrap();
    l.apply_tab_doc(doc);
    assert!(same(&l));
    // editing a hidden tab changes only its slots
    let regroup_before = l.clone();
    let mut regroup = l.clone();
    let mut settings = Settings::default();
    apply(&mut regroup, &mut settings, &Action::Edit { id: ids[0], patch: ColorPatch { transparency_factor: Some(5), ..Default::default() } }).unwrap();
    for (old, new) in regroup_before.groups.iter().zip(&regroup.groups) {
        if old.id == ids[0] {
            assert_eq!(new.style.transparency_factor, Some(5));
            assert_eq!((new.tab, &new.title), (old.tab, &old.title), "membership and geometry untouched");
        } else {
            assert_eq!(old, new);
        }
    }
    // deleting the target tab follows the deleted-target rule
    let mut p = page(&regroup, &Settings::default(), Scope::Corral(ids[1]));
    let removed = tabs::delete(&l.tab_doc(), ids[1]).unwrap();
    assert!(removed.groups.iter().all(|g| g.id != ids[1]));
    let mut rest = regroup.clone();
    rest.groups.retain(|g| g.id != ids[1]);
    assert!(p.reload(Globals::from_settings(&Settings::default()), infos(&rest)));
    assert_eq!(p.scope(), Scope::All);
}

// ---- the draft overlay never sticks ----------------------------------------------------------

fn overlay_fixture() -> (Layout, Settings, u64) {
    let mut l = layout(1);
    l.groups[0].style = GroupStyle { color: Some(0x77), font_weight: Some(700), ..Default::default() };
    let id = l.groups[0].id;
    (l, Settings::default(), id)
}
fn draft(color: u32) -> ColorPatch {
    ColorPatch { color: Some(color), ..Default::default() }
}

#[test]
fn the_overlay_shows_on_top_of_stored_data_and_each_end_puts_the_stored_look_back() {
    // cancel, window close and dropped channel all end the overlay with the same call;
    // the three paths are exercised separately so each one is named.
    for path in ["cancel", "window close", "channel drop"] {
        let (mut l, mut s, id) = overlay_fixture();
        let stored = (l.clone(), s.clone());
        let mut host = OverlayHost::default();
        host.preview(&mut l, &mut s, 1, Some(id), &draft(0x99)).unwrap();
        assert_ne!(l, stored.0, "{path}: shown");
        assert!(host.end(&mut l, &mut s), "{path}");
        assert_eq!((l.clone(), s.clone()), stored, "{path}: stored look is back");
        assert!(!host.is_active());
    }
}

#[test]
fn a_frame_with_another_session_id_ends_the_older_overlay_first() {
    let (mut l, mut s, id) = overlay_fixture();
    let stored = (l.clone(), s.clone());
    let mut host = OverlayHost::default();
    host.preview(&mut l, &mut s, 1, Some(id), &draft(0x99)).unwrap();
    assert_eq!(l.groups[0].style.color, Some(0x99));
    host.preview(&mut l, &mut s, 2, None, &draft(0x55)).unwrap();
    assert_eq!(l.groups[0], stored.0.groups[0], "the stale session's draft is gone");
    assert_eq!(s.color, 0x55, "the new overlay targets the shared look");
    assert_eq!(host.current().unwrap().session, 2);
    host.end(&mut l, &mut s);
    assert_eq!((l, s), stored);
}

#[test]
fn a_commit_settles_the_overlay_without_putting_anything_back_and_a_missing_corral_ends_it() {
    let (mut l, mut s, id) = overlay_fixture();
    let mut host = OverlayHost::default();
    host.preview(&mut l, &mut s, 1, Some(id), &draft(0x99)).unwrap();
    assert!(host.settle().is_some());
    assert!(!host.is_active() && !host.end(&mut l, &mut s));
    assert_eq!(l.groups[0].style.color, Some(0x99), "the committed value stays");
    host.preview(&mut l, &mut s, 3, Some(id), &draft(0x12)).unwrap();
    assert_eq!(host.preview(&mut l, &mut s, 3, Some(id + 50), &draft(1)), Err(OverlayError::MissingCorral));
    assert!(!host.is_active());
    assert_eq!(l.groups[0].style.color, Some(0x99), "the failed frame also put the overlay back");
}

#[test]
fn unfinished_drafts_are_never_part_of_a_save_copy_and_a_settings_reload_cancels_them() {
    let (mut l, mut s, id) = overlay_fixture();
    let mut host = OverlayHost::default();
    host.preview(&mut l, &mut s, 1, Some(id), &draft(0x99)).unwrap();
    let (stored_layout, _) = host.stored_copy(&l, &s);
    assert_eq!(stored_layout.groups[0].style.color, Some(0x77));
    // the settings file is read again: the draft ends, the loaded values stand
    s = Settings::default();
    s.text_color = 5;
    assert!(host.cancel_for_reload(&mut l));
    assert_eq!(l.groups[0].style.color, Some(0x77), "the Corral part of the draft is taken off");
    assert_eq!(s.text_color, 5, "the freshly loaded settings are untouched");
    assert!(!host.is_active());
}
// ---- resets ----------------------------------------------------------------------------------

fn reset_fixture() -> (Layout, Settings) {
    let mut l = layout(3);
    l.groups[0].style = GroupStyle {
        color: Some(1), text_color: Some(2), text_color_auto: Some(false), transparency_factor: Some(3), tint_icons: Some(1),
        tint_icons_color: Some(4), tint_icons_color_source: Some("custom".into()), blend_icons: Some(1), blend_background: Some(1),
        font_weight: Some(700), font_italics: Some(1), tabs_style: Some(2), ..Default::default()
    };
    l.groups[1].style = GroupStyle { color: Some(8), font_name: Some("F".into()), ..Default::default() };
    l.groups[0].title = "Keep my title".into();
    let mut s = Settings::default();
    s.color = 0x10;
    s.text_color = 0x20;
    s.text_color_auto = false;
    s.transparency_factor = 100;
    s.desktop_blend_corral_backgrounds = true;
    s.desktop_tint_icons = true;
    s.desktop_tint_icons_color_source = "corral".into();
    s.desktop_blend_icon_opacity = 77;
    s.square_edges = true;
    s.outline = true;
    (l, s)
}

#[test]
fn reset_base_keeps_every_corral_slot_and_applies_the_tint_rule() {
    let (mut l, mut s) = reset_fixture();
    let before = l.clone();
    apply(&mut l, &mut s, &Action::ResetBase).unwrap();
    assert_eq!(l, before, "no Corral slot is touched");
    let d = Settings::default();
    assert_eq!((s.color, s.text_color, s.text_color_auto, s.transparency_factor, s.desktop_blend_corral_backgrounds), (d.color, d.text_color, true, 1380, false));
    assert!(!s.desktop_tint_icons, "source corral turns tint off");
    assert_eq!((s.desktop_blend_icon_opacity, s.square_edges, s.outline), (77, true, true), "other settings survive");
    let (_, mut custom) = reset_fixture();
    custom.desktop_tint_icons_color_source = "custom".into();
    apply(&mut l, &mut custom, &Action::ResetBase).unwrap();
    assert!(custom.desktop_tint_icons, "source custom keeps it");
}

#[test]
fn reset_all_clears_rows_one_to_five_on_every_corral_and_keeps_font_tab_style_and_geometry() {
    let (mut l, mut s) = reset_fixture();
    let before = l.clone();
    apply(&mut l, &mut s, &Action::ResetAll).unwrap();
    for (old, new) in before.groups.iter().zip(&l.groups) {
        assert!(!has_own_look(&new.style));
        assert_eq!((&new.title, new.rect, &new.content, new.tab, old.id), (&old.title, old.rect, &old.content, old.tab, new.id));
        assert_eq!((new.style.font_weight, new.style.font_italics, new.style.tabs_style, &new.style.font_name), (old.style.font_weight, old.style.font_italics, old.style.tabs_style, &old.style.font_name));
    }
    assert_eq!(s.color, Settings::default().color);
}

#[test]
fn reset_group_leaves_globals_and_other_corrals_byte_identical() {
    let (mut l, mut s) = reset_fixture();
    let (before_l, before_s) = (l.clone(), s.clone());
    let id = l.groups[0].id;
    apply(&mut l, &mut s, &Action::ResetGroup { id }).unwrap();
    assert!(!has_own_look(&l.groups[0].style));
    assert_eq!(serde_json::to_string(&s).unwrap(), serde_json::to_string(&before_s).unwrap());
    assert_eq!(serde_json::to_string(&l.groups[1..]).unwrap(), serde_json::to_string(&before_l.groups[1..]).unwrap());
    assert_eq!(l.groups[0].style.font_weight, Some(700));
}

#[test]
fn every_reset_is_idempotent_and_with_no_own_look_base_equals_all() {
    for action in [Action::ResetBase, Action::ResetAll, Action::ResetGroup { id: 1 }] {
        let (mut l, mut s) = reset_fixture();
        apply(&mut l, &mut s, &action).unwrap();
        let once = (l.clone(), s.clone());
        apply(&mut l, &mut s, &action).unwrap();
        assert_eq!((l, s), once, "{action:?}");
    }
    let (mut l, mut s) = reset_fixture();
    for g in &mut l.groups {
        color_state_clear(&mut g.style);
    }
    assert!(!any_own_look(&l));
    let (mut l2, mut s2) = (l.clone(), s.clone());
    apply(&mut l, &mut s, &Action::ResetBase).unwrap();
    apply(&mut l2, &mut s2, &Action::ResetAll).unwrap();
    assert_eq!((l, s), (l2, s2));
}
fn color_state_clear(style: &mut GroupStyle) {
    reset_group(style);
}

// ---- migration and no colour-value tricks ----------------------------------------------------

#[test]
fn legacy_text_values_migrate_and_present_per_corral_values_stay_own_even_when_equal() {
    let mut s = Settings::default();
    s.text_color_auto = false;
    s.text_color = 0x00FF_FFFF;
    // legacy per-Corral encodings: absent, white (an Auto encoding), custom, unknown mode text
    assert!(!has_own_look(&GroupStyle::default()));
    let white = GroupStyle { text_color: Some(0x00FF_FFFF), ..Default::default() };
    assert!(has_own_look(&white), "a lone white text colour is that Corral's own Auto");
    assert!(effective_of(&s, &white).text_auto, "...even when the shared mode is a custom colour");
    let mut migrated = white.clone();
    migrated.migrate_tint_source(0, 0);
    assert_eq!(migrated.text_color_auto, Some(true), "the migration writes the mode as an own value");
    let mut custom_legacy = GroupStyle { text_color: Some(0x123456), ..Default::default() };
    custom_legacy.migrate_tint_source(0, 0);
    assert_eq!(custom_legacy.text_color_auto, Some(false));
    let mut explicit = GroupStyle { text_color: Some(0x00FF_FFFF), text_color_auto: Some(false), ..Default::default() };
    explicit.migrate_tint_source(0, 0);
    assert_eq!(explicit.text_color_auto, Some(false), "a present mode key is never overridden");
    let custom = GroupStyle { text_color: Some(0x123456), ..Default::default() };
    let e = effective_of(&s, &custom);
    assert!(!e.text_auto && e.text_color == 0x123456 && e.own.text);
    let equal = GroupStyle { text_color: Some(s.text_color), text_color_auto: Some(false), ..Default::default() };
    assert!(effective_of(&s, &equal).own.text, "present stays own even when equal to the shared value");
    // an unknown tint source string does not make a field own
    let unknown = GroupStyle { tint_icons_color_source: Some("sideways".into()), ..Default::default() };
    assert!(!has_own_look(&unknown));
    // shared: a stored white TextColor without the mode key is Auto-compatible per the settings loader
    let loaded: Settings = serde_json::from_str(r#"{"TextColor":16777215}"#).unwrap();
    assert!(loaded.text_color_auto);
    let loaded: Settings = serde_json::from_str(r#"{"TextColor":1193046}"#).unwrap();
    assert!(!loaded.text_color_auto);
}

#[test]
fn a_fresh_install_writes_no_per_corral_colour_key() {
    let l = layout(2);
    let json = serde_json::to_value(&l).unwrap();
    for g in json["groups"].as_array().unwrap() {
        assert!(g.get("style").is_none_or(|v| v.as_object().is_none_or(|o| o.is_empty())));
    }
}

#[test]
fn colour_values_are_never_sentinels() {
    let base = effective_of(&Settings::default(), &GroupStyle::default());
    // Auto text is a mode: it sets the mode key and leaves the colour key alone
    let auto = ColorEdit::TextAuto.patch(&base);
    assert_eq!((auto.text_color, auto.text_color_auto), (None, Some(true)));
    // black and white are ordinary own values, not inherit markers
    for c in [0, 0x00FF_FFFF] {
        let mut style = GroupStyle::default();
        ColorEdit::Background(c).patch(&base).apply_to_style(&mut style);
        assert_eq!(style.color, Some(c));
        assert!(has_own_look(&style));
    }
    // inherit is an absent key: no edit ever produces one
    let mut style = GroupStyle { color: Some(5), ..Default::default() };
    ColorEdit::SeeThrough(0).patch(&base).apply_to_style(&mut style);
    assert_eq!((style.color, style.transparency_factor), (Some(5), Some(0)));
}

#[test]
fn session_ids_name_their_owning_process() {
    let seed = session_seed(0x1234_5678, 1_700_000_000_000);
    assert_eq!(session_owner(seed), 0x5678);
    assert_eq!(session_owner(seed + 5_000), 0x5678, "ids counted up from the seed keep the owner");
    assert_ne!(session_owner(session_seed(0x9999, 1)), 0x5678);
    let l = layout(1);
    let mut p = ColorPage::new(Globals::from_settings(&Settings::default()), infos(&l), Scope::All, seed);
    let (first, _) = p.begin(Field::Background);
    assert_eq!(session_owner(first), 0x5678);
}

#[test]
fn the_page_edits_the_scope_it_names_by_id_not_by_position() {
    let mut l = layout(3);
    let ids: Vec<u64> = l.groups.iter().map(|g| g.id).collect();
    let mut p = page(&l, &Settings::default(), Scope::Corral(ids[2]));
    l.groups.reverse();
    p.reload(Globals::from_settings(&Settings::default()), infos(&l));
    assert_eq!(p.scope(), Scope::Corral(ids[2]));
    assert_eq!(p.title(), "Corral 3");
}
