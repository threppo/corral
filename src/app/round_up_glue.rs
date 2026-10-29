//! Round-up in the running app (docs/design/round-up-spec.md sections 3, 4 and 6).
//!
//! The planner is pure (`crate::round_up`); this module feeds it the desktop's facts, shows the
//! preview of "Sort the desktop now", takes the automatic backup, and applies a plan through the
//! layout's membership functions. Nothing here moves, copies, renames, deletes or writes a file of
//! the user's: the only files written are Corral's own settings, layout and backup copies.

use super::{App, TIMER_RELOAD_ITEMS, log};
use crate::dialog::{self, Action};
use crate::model::GroupContent;
use crate::round_up::{self, Clock, Destination, PlanStatus};
use crate::settings as settings_store;
use windows::Win32::UI::WindowsAndMessaging::SetTimer;

/// How soon an item that is still changing is looked at again (a little over `SETTLE_MS`).
const SETTLE_RECHECK_MS: u32 = 2100;

fn now_ms() -> u64 {
    Clock::now().now_ms.max(0) as u64
}

impl App {
    /// Round-up-spec 6.6: copy the configuration before the first migrated save, and an older
    /// layout before the first start marks anything in it. On failure Round-up stays paused.
    pub(super) fn prepare_round_up(&mut self) -> bool {
        let (settings_path, layout_path) = (
            self.opts.settings_path.clone(),
            self.opts.layout_path.clone(),
        );
        // A preview that shares the real settings file must not migrate it (QA only).
        if self.opts.mode == super::Mode::Preview && settings_path == settings_store::default_path()
        {
            self.settings.round_up.paused_reason =
                Some("Round-up is paused in previews that share the real settings file.".into());
            return false;
        }
        let pending = self.settings.round_up.migration_pending;
        if pending {
            if let Err(error) = settings_store::prepare_round_up_migration(
                &settings_path,
                &layout_path,
                &mut self.settings,
            ) {
                log(&format!("round-up: paused, backup failed: {error}"));
                return false;
            }
            self.round_up_layout_copied = true;
        } else if !self.layout.round_up_initialized && !self.round_up_layout_copied {
            match settings_store::backup_round_up_copy(&layout_path) {
                Ok(_) => self.round_up_layout_copied = true,
                Err(error) => {
                    log(&format!("round-up: paused, layout backup failed: {error}"));
                    self.settings.round_up.paused_reason = Some(format!(
                        "Round-up is paused: your layout could not be backed up. {error}"
                    ));
                    return false;
                }
            }
        }
        if pending {
            if let Err(error) = settings_store::save(&settings_path, &self.settings_to_save()) {
                log(&format!(
                    "round-up: migrated settings could not be saved: {error}"
                ));
                self.settings.round_up.paused_reason = Some(format!(
                    "Round-up is paused: the settings could not be saved. {error}"
                ));
                return false;
            }
            self.settings_stamp = self.settings_file_stamp();
            log("round-up: old rules converted");
        }
        true
    }

    /// The settings window's "Sort the desktop now" request (`ApplyRulesNowRequested`): consumed and
    /// cleared here, then the preview opens. Called after start-up and after each settings reload.
    /// Editing rules, or "Keep Round-up active", never sorts anything by itself.
    pub(super) fn consume_rules_requests(&mut self) {
        if !self.settings.apply_rules_now_requested {
            return;
        }
        self.settings.apply_rules_now_requested = false;
        if let Err(e) = settings_store::save(&self.opts.settings_path, &self.settings_to_save()) {
            log(&format!("round-up: clearing the request failed: {e}"));
        }
        self.settings_stamp = self.settings_file_stamp();
        if self.apply_rules_now() > 0 {
            self.refresh_all();
        }
    }

    /// Items inside a Corral that Round-up has never seen came from the user or an import: they are
    /// the user's choice and are never moved by Round-up (round-up-spec 3, "Manual placement").
    fn protect_foreign_members(&mut self) -> bool {
        let now = now_ms();
        let mut changed = false;
        let names: Vec<String> = self
            .items
            .iter()
            .map(|i| i.shell.parsing_name.clone())
            .collect();
        for name in names {
            if !self.layout.is_seen(&name)
                && !self.layout.is_manual(&name)
                && self.layout.group_of(&name).is_some() // a Corral's own members; Gate items are never marked
            {
                self.layout.mark_manual(&name, now);
                self.layout.mark_seen(&name);
                changed = true;
            }
        }
        changed
    }

    /// "Sort the desktop now": preview the plan, ask, take the automatic backup, apply the very plan
    /// that was shown (round-up-spec 4). Returns the number of icons moved.
    pub(super) fn apply_rules_now(&mut self) -> usize {
        if self.settings.disabled || !self.prepare_round_up() || !self.settings.round_up.active() {
            return 0;
        }
        let light = dialog::system_light();
        if self.protect_foreign_members() {
            self.save();
        }
        let Some(zone) = crate::round_up_time::LocalTimeZone::capture() else {
            dialog::notice(
                self.surface_hwnd,
                light,
                "Round-up",
                "The time zone could not be read, so Round-up cannot check the desktop now.",
            );
            return 0;
        };
        let clock = Clock::now();
        let facts: Vec<_> = self
            .items
            .iter()
            .filter(|i| i.gate.is_none()) // files a Gate shows are never Round-up candidates
            .map(|i| crate::item_facts::facts_for(&i.shell))
            .collect();
        let name_of = |layout: &crate::model::Layout, d: &Destination| match d {
            Destination::Desktop => "the desktop".to_string(),
            Destination::Corral { id } => layout
                .groups
                .iter()
                .find(|g| g.id == *id && matches!(g.content, GroupContent::Items { .. }))
                .map_or("a Corral that no longer exists".to_string(), |g| {
                    if g.title.trim().is_empty() {
                        "Unnamed Corral".to_string()
                    } else {
                        g.title.clone()
                    }
                }),
        };
        let mut reviewed = round_up::plan(
            &self.settings.round_up,
            &self.layout,
            &facts,
            clock,
            &zone,
            false,
        );
        let first = round_up::preview_spec(&reviewed, false, &|d| name_of(&self.layout, d));
        let result = {
            let (round_up_block, layout) = (&self.settings.round_up, &self.layout);
            dialog::show_refreshing(self.surface_hwnd, light, first, &mut |checks| {
                let include = checks.first().copied().unwrap_or(false);
                reviewed = round_up::plan(round_up_block, layout, &facts, clock, &zone, include);
                round_up::preview_spec(&reviewed, include, &|d| name_of(layout, d))
            })
        };
        if result.action != Action::Primary || reviewed.move_count() == 0 {
            return 0;
        }
        // The automatic backup is what makes this undoable: without it nothing moves.
        if let Err(error) = self.snapshot_capture_automatic("Before Round-up") {
            log(&format!(
                "round-up: automatic backup failed, nothing moved: {error}"
            ));
            dialog::notice(
                self.surface_hwnd,
                light,
                "Round-up did not move anything",
                &format!(
                    "The automatic backup could not be saved, so no icons were moved.\n{error}"
                ),
            );
            return 0;
        }
        let moved = round_up::apply_plan(&mut self.layout, &reviewed);
        for entry in &reviewed.entries {
            if !matches!(entry.status, PlanStatus::Unchecked(_) | PlanStatus::Manual) {
                self.round_up_tracking.judged(&entry.item);
            }
        }
        self.save();
        log(&format!("round-up: sort now moved {moved} icon(s)"));
        let unchecked = reviewed.unchecked_count();
        if unchecked > 0 {
            dialog::notice(
                self.surface_hwnd,
                light,
                "Round-up",
                &format!(
                    "{unchecked} {} could not be checked and stayed where {} {}.",
                    if unchecked == 1 { "icon" } else { "icons" },
                    if unchecked == 1 { "it" } else { "they" },
                    if unchecked == 1 { "was" } else { "were" },
                ),
            );
        }
        if moved > 0 {
            self.load_items();
        }
        moved
    }

    /// The automatic runs (round-up-spec 3, "When rules run"): a new item once it has been unchanged
    /// for about two seconds, a changed item while "Keep Round-up active" is on. Called whenever the
    /// desktop's items were read. Items the user placed are never touched.
    pub(super) fn run_rules(&mut self) -> usize {
        if self.settings.disabled || self.items.is_empty() || !self.settings.round_up.active() {
            return 0;
        }
        let now = Clock::now().now_ms;
        // Round-up works on the desktop's own items; files a Gate shows are never its candidates.
        let present: Vec<String> = self
            .items
            .iter()
            .filter(|i| i.gate.is_none())
            .map(|i| i.shell.parsing_name.clone())
            .collect();
        let mut dirty = self.layout.round_up_observe(&present, now.max(0) as u64);
        dirty |= self.protect_foreign_members();
        if !self.settings.round_up.has_work() {
            // Nothing to do yet: what is on the desktop now still counts as seen, so a rule added
            // later does not suddenly sort the existing desktop (round-up-spec 3, "Seen").
            for name in &present {
                dirty |= self.layout.mark_seen(name);
            }
            if dirty {
                self.save();
            }
            return 0;
        }
        let keep_active = self.settings.always_apply_rules;
        let mut waiting = false;
        let mut candidates = Vec::new();
        for item in self.items.iter().filter(|i| i.gate.is_none()) {
            let name = &item.shell.parsing_name;
            if self.pending_drops.contains_key(&name.to_lowercase()) || self.layout.is_manual(name)
            {
                continue;
            }
            let signature = (item.shell.size, item.shell.modified_unix);
            let seen = self.layout.is_seen(name);
            let look = self
                .round_up_tracking
                .observe(name, signature, now, seen, keep_active);
            waiting |= look.settling;
            if look.ready {
                candidates.push(item.shell.clone());
            }
        }
        self.round_up_tracking.retain(&present);
        if waiting && !self.shell_change_armed {
            // SAFETY: plain timer call on our own message window.
            unsafe {
                SetTimer(
                    self.message_window,
                    TIMER_RELOAD_ITEMS,
                    SETTLE_RECHECK_MS,
                    None,
                );
            }
        }
        let mut moved = 0;
        if !candidates.is_empty() {
            if let Some(zone) = crate::round_up_time::LocalTimeZone::capture() {
                let facts: Vec<_> = candidates
                    .iter()
                    .map(crate::item_facts::facts_for)
                    .collect();
                let plan = round_up::plan(
                    &self.settings.round_up,
                    &self.layout,
                    &facts,
                    Clock::now(),
                    &zone,
                    false,
                );
                moved = round_up::apply_plan(&mut self.layout, &plan);
                for entry in &plan.entries {
                    if !matches!(entry.status, PlanStatus::Unchecked(_) | PlanStatus::Manual) {
                        self.round_up_tracking.judged(&entry.item);
                    }
                }
                dirty |= plan
                    .entries
                    .iter()
                    .any(|e| !matches!(e.status, PlanStatus::Unchecked(_) | PlanStatus::Manual));
                for entry in plan.entries.iter().filter(|e| e.status == PlanStatus::Move) {
                    log(&format!(
                        "round-up: {} -> {:?} ({})",
                        entry.item,
                        entry.to,
                        entry.rule.as_deref().unwrap_or("default")
                    ));
                }
            }
        }
        if dirty || moved > 0 {
            self.save();
        }
        moved
    }

    /// "Let Round-up decide" (icon menu): the icons are no longer the user's own placement. They stay
    /// seen (an item inside a Corral is never "new") and are judged right now, so the command works
    /// for Corral members too.
    pub(super) fn let_round_up_decide(&mut self, names: &[String]) {
        let facts: Vec<_> = self
            .items
            .iter()
            .filter(|i| {
                names
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(&i.shell.parsing_name))
            })
            .map(|i| crate::item_facts::facts_for(&i.shell))
            .collect();
        for name in names {
            self.layout.clear_manual(name);
            self.layout.mark_seen(name);
            self.round_up_tracking.forget(name);
        }
        let mut moved = 0;
        if !self.settings.disabled && self.settings.round_up.active() {
            if let Some(zone) = crate::round_up_time::LocalTimeZone::capture() {
                moved = round_up::hand_back(
                    &self.settings.round_up,
                    &mut self.layout,
                    &facts,
                    Clock::now(),
                    &zone,
                );
            }
        }
        log(&format!(
            "round-up: handed back {} icon(s), {moved} moved",
            names.len()
        ));
        self.save();
        if moved > 0 {
            self.load_items();
            self.refresh_all();
        } else {
            self.schedule_item_reload();
        }
    }

    /// Carry the Round-up state of an item that Corral itself renamed to its new name.
    pub(super) fn carry_round_up_state(&mut self, old: &str, new: &str) {
        if self.layout.is_manual(old) {
            self.layout.mark_manual(new, now_ms());
        }
        if self.layout.is_seen(old) {
            self.layout.mark_seen(new);
        }
    }
}
