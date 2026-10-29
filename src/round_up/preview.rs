//! The "Sort the desktop now" preview (round-up-spec 4): the plan as dialog text.
//!
//! The text is generated from the plan that will be applied, so what the user reviews is exactly
//! what runs. There are no per-row choices: to exclude an icon, cancel and adjust a rule.

use super::model::Destination;
use super::plan::{Plan, PlanStatus};
use crate::dialog::{Checkbox, DialogRow, DialogSpec, RowKind};

pub const TITLE: &str = "Sort the desktop now";
pub const INCLUDE_MANUAL: &str = "Include icons I placed myself";

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Build the dialog for `plan`. `name_of` gives the display name of a destination.
pub fn preview_spec(
    plan: &Plan,
    include_manual: bool,
    name_of: &dyn Fn(&Destination) -> String,
) -> DialogSpec {
    let moves = plan.move_count();
    let body = if moves == 0 {
        "Nothing to move. Icons change Corrals only; your files are never moved or deleted."
    } else {
        "Icons change Corrals only. Your files stay exactly where they are."
    };
    let mut spec = DialogSpec::new(
        TITLE,
        body,
        if moves == 0 {
            "Close".to_string()
        } else {
            format!("Move {}", plural(moves, "icon", "icons"))
        },
    );
    spec.width = 680.;
    spec.secondary = (moves > 0).then(|| "Cancel".to_string());

    let mut rows = Vec::new();
    if moves > 0 {
        rows.push(DialogRow::new(
            RowKind::Heading,
            format!("Will move ({moves})"),
        ));
        let mut moving: Vec<_> = plan
            .entries
            .iter()
            .filter(|e| e.status == PlanStatus::Move)
            .map(|e| (name_of(&e.to), e))
            .collect();
        moving.sort_by(|a, b| {
            (a.0.to_lowercase(), a.1.label.to_lowercase())
                .cmp(&(b.0.to_lowercase(), b.1.label.to_lowercase()))
        });
        for (destination, entry) in moving {
            rows.push(DialogRow::new(
                RowKind::Item,
                format!("{} \u{2192} {destination}", entry.label),
            ));
            let rule = entry.rule.as_deref().unwrap_or("Default");
            let why = entry.reasons.join("; ");
            rows.push(DialogRow::new(RowKind::Caption, format!("{rule}: {why}")));
        }
    }
    let unchecked = plan.unchecked_count();
    if unchecked > 0 {
        rows.push(DialogRow::new(
            RowKind::Heading,
            format!("Could not be checked ({unchecked})"),
        ));
        for entry in &plan.entries {
            if let PlanStatus::Unchecked(reason) = &entry.status {
                rows.push(DialogRow::new(RowKind::Item, entry.label.clone()));
                let rule = entry.rule.as_deref().unwrap_or("Round-up");
                rows.push(DialogRow::new(
                    RowKind::Caption,
                    format!("{rule}: {reason} It stays where it is."),
                ));
            }
        }
    }
    let manual = plan.manual_count();
    let included = plan.entries.iter().filter(|e| e.clear_manual).count();
    if manual > 0 {
        rows.push(DialogRow::new(
            RowKind::Heading,
            format!("Skipped, placed by you ({manual})"),
        ));
    }
    rows.push(DialogRow::new(
        RowKind::Heading,
        format!("No change ({})", plan.unchanged_count()),
    ));
    for warning in &plan.warnings {
        rows.push(DialogRow::new(RowKind::Caption, warning.clone()));
    }
    spec.rows = rows;
    if manual > 0 || included > 0 || include_manual {
        spec.checks
            .push(Checkbox::new(INCLUDE_MANUAL, include_manual));
    }
    spec
}
