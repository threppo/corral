//! Planning and applying Round-up (round-up-spec 3 and 4).
//!
//! The planner is pure: facts in, an explained plan out. Applying a plan changes Corral membership
//! through `Layout`'s existing membership functions and does nothing else; no code here has any
//! way to move, copy, rename, delete or write a file.

use std::collections::BTreeMap;

use super::eval::{Verdict, evaluate_rule};
use super::model::*;
use crate::item_facts::ItemFacts;
use crate::model::{GroupContent, Layout};
use crate::round_up_time::{Clock, TimeZone};

/// What a run decided for one item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanStatus {
    /// The item goes to `to`.
    Move,
    /// A fact was missing, so the item stays where it is and stays unseen. Carries the reason.
    Unchecked(String),
    /// The user placed the item by hand; it is left alone.
    Manual,
    /// The item is already where the rules want it.
    NoChange,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanEntry {
    /// The item's identity (parsing name).
    pub item: String,
    /// What the user reads (the real file name).
    pub label: String,
    pub from: Destination,
    pub to: Destination,
    /// Name of the deciding rule (or the one that could not be checked).
    pub rule: Option<String>,
    /// The conditions that held, as phrases ("has the extension pdf").
    pub reasons: Vec<String>,
    pub status: PlanStatus,
    /// The item was manual and the run includes such items: applying clears the mark.
    pub clear_manual: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub entries: Vec<PlanEntry>,
    /// Rules or the default pointing to a Corral that no longer exists.
    pub warnings: Vec<String>,
}

impl Plan {
    pub fn count(&self, wanted: fn(&PlanStatus) -> bool) -> usize {
        self.entries.iter().filter(|e| wanted(&e.status)).count()
    }
    pub fn move_count(&self) -> usize {
        self.count(|s| *s == PlanStatus::Move)
    }
    pub fn unchecked_count(&self) -> usize {
        self.count(|s| matches!(s, PlanStatus::Unchecked(_)))
    }
    pub fn manual_count(&self) -> usize {
        self.count(|s| *s == PlanStatus::Manual)
    }
    pub fn unchanged_count(&self) -> usize {
        self.count(|s| *s == PlanStatus::NoChange)
    }
}

/// A Corral (a group of icons, not a Gate) with this id exists.
pub fn corral_exists(layout: &Layout, id: u64) -> bool {
    layout
        .groups
        .iter()
        .any(|g| g.id == id && matches!(g.content, GroupContent::Items { .. }))
}

pub fn destination_exists(destination: &Destination, layout: &Layout) -> bool {
    match destination {
        Destination::Desktop => true,
        Destination::Corral { id } => corral_exists(layout, *id),
    }
}

impl RoundUp {
    /// Rules (and the default) whose Corral is gone: they are suspended, not deleted
    /// (round-up-spec 3, "Destination missing").
    pub fn missing_destinations(&self, exists: impl Fn(u64) -> bool) -> usize {
        let gone = |d: &Destination| matches!(d, Destination::Corral { id } if !exists(*id));
        self.rules
            .iter()
            .filter(|r| r.destination.as_ref().is_some_and(gone))
            .count()
            + usize::from(gone(&self.default_destination))
    }
}

/// How a rule is named in a plan: its name, else its place in the list.
pub fn rule_label(rules: &[Rule], rule: &Rule) -> String {
    if !rule.name.trim().is_empty() {
        return rule.name.clone();
    }
    let place = rules
        .iter()
        .position(|r| r.id == rule.id)
        .map_or(0, |i| i + 1);
    format!("Rule {place}")
}

/// Decide every item (round-up-spec 3).
///
/// Enabled, complete rules whose Corral exists run top to bottom; the first Yes decides. A rule that
/// cannot be evaluated before any Yes halts the item: it stays where it is, unseen, with the
/// reason. When nothing matches, a Corral default applies; "Leave it on the desktop" has no
/// opinion. Manual items are left alone unless `include_manual`.
pub fn plan(
    round_up: &RoundUp,
    layout: &Layout,
    facts: &[ItemFacts],
    clock: Clock,
    zone: &dyn TimeZone,
    include_manual: bool,
) -> Plan {
    let mut plan = Plan::default();
    let missing = round_up.missing_destinations(|id| corral_exists(layout, id));
    if missing > 0 {
        plan.warnings.push(if missing == 1 {
            "1 rule points to a Corral that no longer exists.".to_string()
        } else {
            format!("{missing} rules point to a Corral that no longer exists.")
        });
    }
    let default_destination = Some(&round_up.default_destination)
        .filter(|d| matches!(d, Destination::Corral { .. }) && destination_exists(d, layout));
    for item in facts {
        let from = layout
            .group_of(&item.name)
            .map_or(Destination::Desktop, |id| Destination::Corral { id });
        let manual = layout.is_manual(&item.name);
        let mut entry = PlanEntry {
            item: item.name.clone(),
            label: item.file_name.clone(),
            to: from.clone(),
            from,
            rule: None,
            reasons: Vec::new(),
            status: PlanStatus::NoChange,
            clear_manual: manual && include_manual,
        };
        if manual && !include_manual {
            entry.status = PlanStatus::Manual;
            plan.entries.push(entry);
            continue;
        }
        if !round_up.active() {
            let reason = round_up
                .paused_reason
                .clone()
                .unwrap_or_else(|| "Round-up is paused.".into());
            entry.status = PlanStatus::Unchecked(reason);
            plan.entries.push(entry);
            continue;
        }
        let mut decided = false;
        for rule in round_up.rules.iter().filter(|r| r.runs()) {
            let Some(to) = rule.destination.as_ref() else {
                continue;
            };
            if !destination_exists(to, layout) {
                continue; // suspended until the Corral returns
            }
            match evaluate_rule(rule, item, clock, zone) {
                (Verdict::No, _) => continue,
                (Verdict::CouldNotEvaluate(reason), _) => {
                    entry.rule = Some(rule_label(&round_up.rules, rule));
                    entry.status = PlanStatus::Unchecked(reason);
                }
                (Verdict::Yes, reasons) => {
                    entry.rule = Some(rule_label(&round_up.rules, rule));
                    entry.to = to.clone();
                    entry.reasons = reasons;
                }
            }
            decided = true;
            break;
        }
        if !decided {
            if let Some(default) = default_destination {
                entry.to = default.clone();
                entry.reasons = vec!["no rule matched".into()];
            }
        }
        if !matches!(entry.status, PlanStatus::Unchecked(_)) && entry.from != entry.to {
            entry.status = PlanStatus::Move;
        }
        plan.entries.push(entry);
    }
    plan
}

/// Apply a reviewed plan to the layout's membership and nothing else (round-up-spec 1, 4).
/// Returns the number of items moved. Items whose situation changed since the plan was made are
/// skipped, so applying twice, or applying a stale plan, never does more than the plan said.
pub fn apply_plan(layout: &mut Layout, plan: &Plan) -> usize {
    let mut moved = 0;
    for entry in &plan.entries {
        if matches!(entry.status, PlanStatus::Unchecked(_) | PlanStatus::Manual) {
            continue;
        }
        let current = layout
            .group_of(&entry.item)
            .map_or(Destination::Desktop, |id| Destination::Corral { id });
        if current != entry.from || !destination_exists(&entry.to, layout) {
            continue;
        }
        if layout.is_manual(&entry.item) && !entry.clear_manual {
            continue;
        }
        if entry.status == PlanStatus::Move {
            match entry.to {
                Destination::Corral { id } => layout.insert_item(&entry.item, id, usize::MAX),
                Destination::Desktop => layout.assign_item(&entry.item, None),
            }
            moved += 1;
        }
        if entry.clear_manual {
            layout.clear_manual(&entry.item);
        }
        layout.mark_seen(&entry.item);
    }
    moved
}

/// "Let Round-up decide" (round-up-spec 3): hand these items back. The marks are cleared, the
/// items stay seen, and they are judged at once, so nothing waits for a "new item" that never
/// comes (an item inside a Corral is never new). Returns the number of items moved.
pub fn hand_back(
    round_up: &RoundUp,
    layout: &mut Layout,
    facts: &[ItemFacts],
    clock: Clock,
    zone: &dyn TimeZone,
) -> usize {
    for item in facts {
        layout.clear_manual(&item.name);
        layout.mark_seen(&item.name);
    }
    let planned = plan(round_up, layout, facts, clock, zone, false);
    apply_plan(layout, &planned)
}

/// Cheap change signature of an item: size and last write as the Shell listed them.
pub type Signature = (Option<u64>, Option<i64>);

/// Decides when an automatic run looks at an item (round-up-spec 3, "When rules run"):
/// a new item after it has been unchanged for about 2 seconds, a seen item again only after a
/// change and only while "Keep Round-up active" is on. Runtime only; nothing is persisted.
#[derive(Default)]
pub struct RunTracker {
    entries: BTreeMap<String, Tracked>,
}

struct Tracked {
    signature: Signature,
    stable_since_ms: i64,
    evaluated: Option<Signature>,
}

/// How long an item must stay unchanged before it is judged (a download in progress).
pub const SETTLE_MS: i64 = 2000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observation {
    /// Evaluate the item now.
    pub ready: bool,
    /// The item needs a look but is still changing; come back soon.
    pub settling: bool,
}

impl RunTracker {
    pub fn observe(
        &mut self,
        name: &str,
        signature: Signature,
        now_ms: i64,
        seen: bool,
        keep_active: bool,
    ) -> Observation {
        let entry = self
            .entries
            .entry(name.to_lowercase())
            .or_insert_with(|| Tracked {
                signature,
                stable_since_ms: now_ms,
                evaluated: seen.then_some(signature),
            });
        if entry.signature != signature {
            entry.signature = signature;
            entry.stable_since_ms = now_ms;
        }
        let needed = !seen || (keep_active && entry.evaluated != Some(signature));
        let settling = needed && now_ms.saturating_sub(entry.stable_since_ms) < SETTLE_MS;
        Observation {
            ready: needed && !settling,
            settling,
        }
    }

    /// The item was judged (moved, or left because nothing matched).
    pub fn judged(&mut self, name: &str) {
        if let Some(entry) = self.entries.get_mut(&name.to_lowercase()) {
            entry.evaluated = Some(entry.signature);
        }
    }

    /// Forget an item so it is looked at again ("Let Round-up decide").
    pub fn forget(&mut self, name: &str) {
        self.entries.remove(&name.to_lowercase());
    }

    /// Drop entries of items that are gone.
    pub fn retain(&mut self, present: &[String]) {
        let keys: std::collections::BTreeSet<String> =
            present.iter().map(|s| s.to_lowercase()).collect();
        self.entries.retain(|k, _| keys.contains(k));
    }
}

/// The editor's live line under "Send matching icons to" (round-up-spec 4): how this rule alone
/// would act on today's desktop. Never applies anything.
pub fn caption(
    rule: &Rule,
    above: &[Rule],
    facts: &[ItemFacts],
    clock: Clock,
    zone: &dyn TimeZone,
) -> String {
    let mut matched: Vec<&str> = Vec::new();
    let (mut taken, mut unchecked) = (0usize, 0usize);
    for item in facts {
        match evaluate_rule(rule, item, clock, zone).0 {
            Verdict::Yes => {
                matched.push(&item.file_name);
                if above
                    .iter()
                    .filter(|r| r.runs())
                    .any(|r| evaluate_rule(r, item, clock, zone).0 == Verdict::Yes)
                {
                    taken += 1;
                }
            }
            Verdict::CouldNotEvaluate(_) => unchecked += 1,
            Verdict::No => {}
        }
    }
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            one.to_string()
        } else {
            many.to_string()
        }
    };
    let mut text = if matched.is_empty() {
        "Matches no icons now.".to_string()
    } else {
        let shown = matched
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        let more = matched.len().saturating_sub(3);
        format!(
            "Matches {} {} now: {shown}{}.",
            matched.len(),
            plural(matched.len(), "icon", "icons"),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        )
    };
    if taken > 0 {
        text.push_str(&format!(
            " {taken} {}",
            plural(
                taken,
                "is taken by a rule above.",
                "are taken by rules above."
            )
        ));
    }
    if unchecked > 0 {
        text.push_str(&format!(" {unchecked} could not be checked."));
    }
    text
}
