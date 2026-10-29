//! Three-valued evaluation of conditions and rules (round-up-spec 2 and 3). Pure: facts, clock and
//! time zone in, verdicts out.

use super::model::*;
use crate::item_facts::{ItemFacts, TargetClass};
use crate::round_up_time::{Clock, TimeZone, threshold};

/// Result of a condition or rule. `CouldNotEvaluate` is only returned when a fact should exist but
/// could not be obtained; "not applicable" is a definite `No`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Yes,
    No,
    CouldNotEvaluate(String),
}

/// All: any No gives No, else any unknown gives unknown, else Yes. Any: any Yes gives Yes, else any
/// unknown gives unknown, else No (round-up-spec 3, "Combining").
pub fn combine(mode: MatchMode, values: &[Verdict]) -> Verdict {
    let unknown = values
        .iter()
        .find(|v| matches!(v, Verdict::CouldNotEvaluate(_)));
    match mode {
        MatchMode::All => {
            if values.contains(&Verdict::No) || values.is_empty() {
                Verdict::No
            } else {
                unknown.cloned().unwrap_or(Verdict::Yes)
            }
        }
        MatchMode::Any => {
            if values.contains(&Verdict::Yes) {
                Verdict::Yes
            } else {
                unknown.cloned().unwrap_or(Verdict::No)
            }
        }
    }
}

fn yes(value: bool) -> Verdict {
    if value { Verdict::Yes } else { Verdict::No }
}

fn unknown(reason: &str) -> Verdict {
    Verdict::CouldNotEvaluate(reason.into())
}

/// Simple case folding without normalisation (round-up-spec 2): lower case per character, the two
/// characters whose lower-case forms differ by position folded to their common form.
pub fn fold(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{03c2}' => '\u{03c3}',
            '\u{017f}' => 's',
            _ => {
                let mut lower = c.to_lowercase();
                match (lower.next(), lower.next()) {
                    (Some(first), None) => first,
                    _ => c,
                }
            }
        })
        .collect()
}

/// The text after the last dot of a file name; none without a dot or with a trailing dot.
pub fn extension(name: &str) -> &str {
    name.rsplit_once('.').map_or("", |(_, ext)| ext)
}

/// The name without its extension (".gitignore" has an empty one).
pub fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(base, _)| base)
}

fn extension_matches(op: ExtensionOp, values: &[String], actual: &str) -> bool {
    let listed = values.iter().any(|v| fold(v) == fold(actual));
    match op {
        ExtensionOp::HasNone => actual.is_empty(),
        ExtensionOp::IsOneOf => listed,
        ExtensionOp::IsNoneOf => !listed,
    }
}

fn text_matches(op: TextOp, value: &str, actual: &str) -> bool {
    let (value, actual) = (fold(value), fold(actual));
    match op {
        TextOp::Is => actual == value,
        TextOp::Contains => actual.contains(&value),
        TextOp::DoesNotContain => !actual.contains(&value),
        TextOp::StartsWith => actual.starts_with(&value),
        TextOp::EndsWith => actual.ends_with(&value),
        TextOp::MatchesPattern => wildcard(&actual, &value),
    }
}

/// `*` is any run of characters (also none), `?` is one character, everything else is literal.
pub fn wildcard(actual: &str, pattern: &str) -> bool {
    let a: Vec<char> = actual.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    let (mut i, mut j) = (0, 0);
    let mut star: Option<usize> = None;
    let mut retry = 0;
    while i < a.len() {
        if j < p.len() && (p[j] == '?' || p[j] == a[i]) && p[j] != '*' {
            i += 1;
            j += 1;
        } else if j < p.len() && p[j] == '*' {
            star = Some(j);
            retry = i;
            j += 1;
        } else if let Some(s) = star {
            retry += 1;
            i = retry;
            j = s + 1;
        } else {
            return false;
        }
    }
    while j < p.len() && p[j] == '*' {
        j += 1;
    }
    j == p.len()
}

pub fn evaluate(
    condition: &Condition,
    facts: &ItemFacts,
    clock: Clock,
    zone: &dyn TimeZone,
) -> Verdict {
    use crate::item_facts::ItemType;
    let named = matches!(facts.kind, ItemType::File | ItemType::Shortcut);
    let target = || -> Result<&crate::item_facts::TargetFacts, Verdict> {
        if facts.kind != ItemType::Shortcut {
            return Err(Verdict::No);
        }
        facts
            .target
            .as_ref()
            .ok_or_else(|| unknown("The shortcut's target could not be read."))
    };
    match condition {
        Condition::ItemType { op, value } => yes((facts.kind == *value) == (*op == IsOp::Is)),
        Condition::Extension { op, value } => {
            let actual = if named {
                extension(&facts.file_name)
            } else {
                ""
            };
            yes(extension_matches(*op, value, actual))
        }
        Condition::Name { op, value } => {
            let actual = if named {
                stem(&facts.file_name)
            } else {
                &facts.file_name
            };
            yes(text_matches(*op, value, actual))
        }
        Condition::Size { op, value } => {
            if !named {
                return Verdict::No;
            }
            let Some(size) = facts.size else {
                return unknown("The size could not be read.");
            };
            let Some(bound) = value.bytes() else {
                return unknown("The size limit is too large.");
            };
            yes(match op {
                SizeOp::AtLeast => size >= bound,
                SizeOp::LessThan => size < bound,
            })
        }
        Condition::Modified { op, value } => {
            if facts.kind == ItemType::SystemItem {
                return Verdict::No;
            }
            let Some(modified) = facts.modified_ms else {
                return unknown("The change date could not be read.");
            };
            let Some(bound) = threshold(clock, *value, zone) else {
                return unknown("The date limit could not be worked out.");
            };
            yes(match op {
                DateOp::NewerThan => modified >= bound,
                DateOp::OlderThan => modified < bound,
            })
        }
        Condition::ShortcutTarget { op, value } => match target() {
            Err(verdict) => verdict,
            Ok(t) => {
                let class = match value {
                    TargetKind::Program => TargetClass::Program,
                    TargetKind::Folder => TargetClass::Folder,
                    TargetKind::File => TargetClass::File,
                };
                yes((t.class == class) == (*op == IsOp::Is))
            }
        },
        Condition::TargetExtension { op, value } => match target() {
            Err(verdict) => verdict,
            Ok(t) => {
                let name = t.path.rsplit(['\\', '/']).next().unwrap_or("");
                let actual = if t.class == TargetClass::Web {
                    ""
                } else {
                    extension(name)
                };
                yes(extension_matches(*op, value, actual))
            }
        },
        Condition::TargetPath { op, value } => match target() {
            Err(verdict) => verdict,
            Ok(t) => yes(text_matches(*op, value, &t.path)),
        },
        Condition::Unknown { .. } => unknown("This rule needs an update of Corral."),
    }
}

/// A rule's verdict and the conditions that held (for the preview's "why").
pub fn evaluate_rule(
    rule: &Rule,
    facts: &ItemFacts,
    clock: Clock,
    zone: &dyn TimeZone,
) -> (Verdict, Vec<String>) {
    let values: Vec<Verdict> = rule
        .conditions
        .iter()
        .map(|c| evaluate(c, facts, clock, zone))
        .collect();
    let reasons = rule
        .conditions
        .iter()
        .zip(&values)
        .filter(|(_, v)| **v == Verdict::Yes)
        .map(|(c, _)| c.phrase())
        .collect();
    (combine(rule.match_mode, &values), reasons)
}
