//! One-time conversion of the old stored rule strings (round-up-spec 6).
//!
//! The decoder knows only the outer shape of what the old Corral serializer wrote:
//!
//! * `Data`:   `Name|Criterion|Criterion|...|`, a name wrapped in `{}` was generated; a criterion
//!   is `[<or>]<method>:<operator>:<number>:<text>` with `` ` | : < > `` in the text replaced by
//!   box characters.
//! * `Target`: `Enabled|CorralId|PlaceAtEnd|PresetId`.
//!
//! A rule converts only when every criterion has exactly one new condition with the same meaning
//! under the definitions of spec 2, the joins are all AND or all OR, and the target parses. Anything
//! else goes to `needs_review` with its stored text. No old evaluator is involved.

use serde::{Deserialize, Serialize};

use super::model::*;

/// One old rule exactly as it is stored: the data string and the target string.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredRule {
    #[serde(rename = "Data")]
    pub data: String,
    #[serde(rename = "Target")]
    pub target: String,
}

impl RoundUp {
    /// Convert the old rules and default destination. Converted rules keep their order; missing
    /// built-in rules are appended in default form (disabled, no destination).
    pub fn migrate(stored: &[StoredRule], default_corral: i32) -> Self {
        let mut result = Self {
            rules: Vec::new(),
            migrated_from_legacy: true,
            migration_pending: true,
            default_destination: if default_corral > 0 {
                Destination::Corral {
                    id: default_corral as u64,
                }
            } else {
                Destination::Desktop
            },
            ..Self::default()
        };
        for (position, old) in stored.iter().enumerate() {
            let used: Vec<String> = result
                .rules
                .iter()
                .filter_map(|r| r.preset.clone())
                .collect();
            match convert(old, position, &used) {
                Ok(rule) => result.rules.push(rule),
                Err(reason) => result.needs_review.push(ReviewItem {
                    position,
                    name: review_name(&old.data),
                    data: old.data.clone(),
                    target: old.target.clone(),
                    reason,
                }),
            }
        }
        for preset in presets() {
            if !result.rules.iter().any(|r| r.preset == preset.preset) {
                result.rules.push(preset);
            }
        }
        result
    }
}

fn review_name(data: &str) -> String {
    let name = data.split('|').next().unwrap_or("");
    let name = name
        .strip_prefix('{')
        .and_then(|n| n.strip_suffix('}'))
        .unwrap_or(name);
    if name.trim().is_empty() {
        "Unnamed rule".into()
    } else {
        name.to_string()
    }
}

/// The preset a stored `PresetId` stands for (the sign marks a customised preset).
fn preset_of(auto_id: i32) -> Option<&'static str> {
    Some(match auto_id.unsigned_abs() {
        10 => "folders",
        20 => "documents",
        30 => "images",
        40 => "music_video",
        50 => "downloads",
        60 => "web_links",
        70 => "program_shortcuts",
        _ => return None,
    })
}

struct Fields {
    enabled: bool,
    corral: i64,
    place_at_end: bool,
    auto_text: String,
    auto_id: i32,
}

fn parse_target(target: &str) -> Result<Fields, String> {
    let parts: Vec<&str> = target.split('|').collect();
    let flag = |s: &str| match s {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    };
    let [enabled, corral, place, auto] = parts[..] else {
        return Err("The rule's destination is not in the expected form.".into());
    };
    Ok(Fields {
        enabled: flag(enabled).ok_or("The rule's enabled flag is not in the expected form.")?,
        corral: corral
            .trim()
            .parse()
            .map_err(|_| "The rule's destination is not in the expected form.".to_string())?,
        place_at_end: flag(place)
            .ok_or("The rule's placement flag is not in the expected form.")?,
        auto_text: auto.to_string(),
        auto_id: auto
            .trim()
            .parse()
            .map_err(|_| "The rule's built-in id is not in the expected form.".to_string())?,
    })
}

struct Criterion {
    or: bool,
    method: String,
    op: char,
    number: u64,
    text: String,
}

fn parse_criterion(raw: &str) -> Result<Criterion, String> {
    let malformed = || "A condition is not in the expected form.".to_string();
    let (or, rest) = match raw.strip_prefix("<or>") {
        Some(rest) => (true, rest),
        None => (false, raw),
    };
    let mut parts = rest.splitn(4, ':');
    let method = parts
        .next()
        .filter(|m| !m.is_empty())
        .ok_or_else(malformed)?;
    let op = parts.next().ok_or_else(malformed)?;
    let number = parts.next().ok_or_else(malformed)?;
    let text = parts.next().ok_or_else(malformed)?;
    let mut op_chars = op.chars();
    let (Some(op), None) = (op_chars.next(), op_chars.next()) else {
        return Err(malformed());
    };
    Ok(Criterion {
        or,
        method: method.to_string(),
        op,
        number: number.trim().parse().map_err(|_| malformed())?,
        text: unescape(text),
    })
}

/// The old text escapes: box characters stand for the separators and angle brackets.
fn unescape(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{2593}' => '`',
            '\u{2592}' => '|',
            '\u{2591}' => ':',
            '\u{255e}' => '<',
            '\u{255f}' => '>',
            other => other,
        })
        .collect()
}

fn extension_list(text: &str) -> Result<Vec<String>, String> {
    let values: Vec<String> = text
        .split(',')
        .map(|e| e.trim().trim_start_matches('.').to_string())
        .filter(|e| !e.is_empty())
        .collect();
    if values.is_empty() {
        return Err("The extension list is empty.".into());
    }
    Ok(values)
}

fn size_value(bytes: u64) -> SizeValue {
    for (shift, unit) in [(30, SizeUnit::GB), (20, SizeUnit::MB), (10, SizeUnit::KB)] {
        if bytes != 0 && bytes % (1u64 << shift) == 0 {
            return SizeValue {
                n: bytes >> shift,
                unit,
            };
        }
    }
    SizeValue {
        n: bytes >> 10,
        unit: SizeUnit::KB,
    }
}

/// The single new condition meaning the same as an old criterion, or why there is none.
fn convert_criterion(c: &Criterion) -> Result<Condition, String> {
    let no_params = c.number == 0 && c.text.is_empty();
    match (c.method.as_str(), c.op) {
        ("<type>", '=' | '!') if c.number == 0 => Ok(Condition::Extension {
            op: if c.op == '=' { ExtensionOp::IsOneOf } else { ExtensionOp::IsNoneOf },
            value: extension_list(&c.text)?,
        }),
        ("<type>", 'D' | 'd') if no_params => Ok(Condition::ItemType {
            op: if c.op == 'D' { IsOp::Is } else { IsOp::IsNot },
            value: ItemType::Folder,
        }),
        ("<type>", 'V' | 'v') if no_params => Ok(Condition::ItemType {
            op: if c.op == 'V' { IsOp::Is } else { IsOp::IsNot },
            value: ItemType::SystemItem,
        }),
        ("<type>", 'F' | 'f') => Err(
            "\"Is a file\" no longer includes shortcuts; choose the item type you mean.".into(),
        ),
        ("<type>", 'P' | 'p') => Err(
            "\"Program shortcut\" is now two conditions: type Shortcut and shortcut points to a program."
                .into(),
        ),
        ("<size>", '<' | '>') if c.text.is_empty() => {
            let value = size_value(c.number);
            if value.bytes() != Some(c.number) {
                return Err("The size is not a whole number of KB, MB or GB.".into());
            }
            Ok(Condition::Size {
                op: if c.op == '>' { SizeOp::AtLeast } else { SizeOp::LessThan },
                value,
            })
        }
        ("<target>", '=' | '[' | ']' | '#') if c.number == 0 && !c.text.is_empty() => {
            Ok(Condition::TargetPath {
                op: match c.op {
                    '=' => TextOp::Contains,
                    '[' => TextOp::StartsWith,
                    ']' => TextOp::EndsWith,
                    _ => TextOp::Is,
                },
                value: c.text.clone(),
            })
        }
        ("<target>", _) => Err("This link-target test has no matching condition.".into()),
        ("<name>", _) => Err(
            "Names are now matched without the extension; rewrite this condition to say what you mean."
                .into(),
        ),
        ("<size>", _) => Err("This size test has no matching condition.".into()),
        ("<type>", _) => Err("This type test has no matching condition.".into()),
        ("<modifieddate>", _) => Err("The date test has no matching condition; rewrite it.".into()),
        ("<timesopened>" | "<timeofday>" | "<dayofweek>" | "<program>", _) => {
            Err("This kind of test is not available in Round-up any more.".into())
        }
        _ => Err("This kind of condition is not known.".into()),
    }
}

fn convert(old: &StoredRule, position: usize, used_presets: &[String]) -> Result<Rule, String> {
    let fields = parse_target(&old.target)?;
    let mut parts: Vec<&str> = old.data.split('|').collect();
    let name_raw = parts.remove(0);
    if parts.last() == Some(&"") {
        parts.pop();
    }
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        return Err("The rule has no conditions, or one of them is empty.".into());
    }
    let criteria = parts
        .iter()
        .map(|p| parse_criterion(p))
        .collect::<Result<Vec<_>, _>>()?;
    if criteria[0].or {
        return Err("The first condition starts with \"or\".".into());
    }
    let has_or = criteria[1..].iter().any(|c| c.or);
    let has_and = criteria[1..].iter().any(|c| !c.or);
    if has_or && has_and {
        return Err(
            "The conditions mix \"and\" with \"or\", which Round-up does not guess.".into(),
        );
    }
    let preset = preset_of(fields.auto_id)
        .filter(|id| !used_presets.iter().any(|used| used == id))
        .map(str::to_string);
    // The one place where an old criterion becomes two conditions: the built-in Program
    // shortcuts rule, whose meaning is exactly "a shortcut that points to a program".
    let program_preset = preset.as_deref() == Some("program_shortcuts")
        && criteria.len() == 1
        && criteria[0].method == "<type>"
        && criteria[0].op == 'P';
    let conditions = if program_preset {
        preset_rule("program_shortcuts")
            .map(|r| r.conditions)
            .unwrap_or_default()
    } else {
        criteria
            .iter()
            .map(convert_criterion)
            .collect::<Result<Vec<_>, _>>()?
    };
    for condition in &conditions {
        condition.validate()?;
    }
    let destination = match fields.corral {
        -1 if !fields.enabled => None,
        -1 => Some(Destination::Desktop),
        id if id >= 1 => Some(Destination::Corral { id: id as u64 }),
        _ => return Err("The rule's destination is not in the expected form.".into()),
    };
    let generated = name_raw.starts_with('{') && name_raw.ends_with('}') && name_raw.len() >= 2;
    let name = if generated {
        preset
            .as_deref()
            .and_then(preset_name)
            .unwrap_or_default()
            .to_string()
    } else {
        name_raw.to_string()
    };
    Ok(Rule {
        id: preset.as_ref().map_or_else(
            || format!("migrated-{position}"),
            |id| format!("preset-{id}"),
        ),
        name,
        enabled: fields.enabled,
        preset,
        match_mode: if has_or {
            MatchMode::Any
        } else {
            MatchMode::All
        },
        conditions,
        destination,
        legacy: serde_json::json!({
            "place_at_end": fields.place_at_end,
            "auto_id": fields.auto_text,
        }),
    })
}

impl ReviewItem {
    /// "Rewrite...": a rule holding whatever of the old rule maps one to one (destination, name and
    /// the conditions that convert), for the user to finish in the editor (round-up-spec 6).
    pub fn prefill(&self, id: String) -> Rule {
        let fields = parse_target(&self.target).ok();
        let mut parts: Vec<&str> = self.data.split('|').collect();
        let name_raw = parts.remove(0);
        if parts.last() == Some(&"") {
            parts.pop();
        }
        let criteria: Vec<Criterion> = parts
            .iter()
            .filter_map(|p| parse_criterion(p).ok())
            .collect();
        let conditions: Vec<Condition> = criteria
            .iter()
            .filter_map(|c| convert_criterion(c).ok())
            .collect();
        let all_or = criteria.len() > 1 && criteria[1..].iter().all(|c| c.or);
        let generated = name_raw.starts_with('{') && name_raw.ends_with('}') && name_raw.len() >= 2;
        Rule {
            id,
            name: if generated {
                String::new()
            } else {
                name_raw.to_string()
            },
            enabled: true,
            preset: None,
            match_mode: if all_or {
                MatchMode::Any
            } else {
                MatchMode::All
            },
            conditions: if conditions.is_empty() {
                vec![Condition::Name {
                    op: TextOp::Contains,
                    value: String::new(),
                }]
            } else {
                conditions
            },
            destination: fields.and_then(|f| match f.corral {
                -1 => Some(Destination::Desktop),
                id if id >= 1 => Some(Destination::Corral { id: id as u64 }),
                _ => None,
            }),
            legacy: serde_json::Value::Null,
        }
    }
}
