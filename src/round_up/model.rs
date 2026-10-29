//! Typed Round-up rules, conditions and their stored form (round-up-spec 2 and 6).
//!
//! Pure data: nothing in this module reads or writes a file.

use serde::{Deserialize, Serialize};

pub use crate::item_facts::ItemType;
pub use crate::round_up_time::{Age, TimeUnit};

/// Newest `RoundUp.version` this build understands (round-up-spec 6).
pub const BLOCK_VERSION: u32 = 1;

/// Where an item goes (round-up-spec 2): a Corral by its stable id, or nowhere (the desktop).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Destination {
    #[default]
    Desktop,
    Corral {
        id: u64,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    #[default]
    All,
    Any,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IsOp {
    Is,
    IsNot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionOp {
    IsOneOf,
    IsNoneOf,
    HasNone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextOp {
    Is,
    Contains,
    DoesNotContain,
    StartsWith,
    EndsWith,
    MatchesPattern,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeOp {
    AtLeast,
    LessThan,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateOp {
    NewerThan,
    OlderThan,
}

/// What a shortcut points to (round-up-spec 2, "Shortcut points to").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Program,
    Folder,
    File,
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SizeUnit {
    KB,
    MB,
    GB,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeValue {
    pub n: u64,
    pub unit: SizeUnit,
}

impl SizeValue {
    /// 1024-based, as Explorer shows sizes. `None` when the product does not fit.
    pub fn bytes(self) -> Option<u64> {
        let factor: u64 = match self.unit {
            SizeUnit::KB => 1 << 10,
            SizeUnit::MB => 1 << 20,
            SizeUnit::GB => 1 << 30,
        };
        self.n.checked_mul(factor)
    }
}

/// One condition of a rule. `Unknown` keeps a condition this build cannot read exactly as it was
/// stored, so saving never loses it (round-up-spec 6).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Condition {
    ItemType { op: IsOp, value: ItemType },
    Extension { op: ExtensionOp, value: Vec<String> },
    Name { op: TextOp, value: String },
    Size { op: SizeOp, value: SizeValue },
    Modified { op: DateOp, value: Age },
    ShortcutTarget { op: IsOp, value: TargetKind },
    TargetExtension { op: ExtensionOp, value: Vec<String> },
    TargetPath { op: TextOp, value: String },
    Unknown { raw: serde_json::Value },
}

#[derive(Serialize)]
struct Wire<'a, O: Serialize, V: Serialize> {
    #[serde(rename = "type")]
    kind: &'a str,
    op: O,
    value: V,
}

impl Serialize for Condition {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::ItemType { op, value } => Wire {
                kind: "item_type",
                op,
                value,
            }
            .serialize(s),
            Self::Extension { op, value } => Wire {
                kind: "extension",
                op,
                value,
            }
            .serialize(s),
            Self::Name { op, value } => Wire {
                kind: "name",
                op,
                value,
            }
            .serialize(s),
            Self::Size { op, value } => Wire {
                kind: "size",
                op,
                value,
            }
            .serialize(s),
            Self::Modified { op, value } => Wire {
                kind: "modified",
                op,
                value,
            }
            .serialize(s),
            Self::ShortcutTarget { op, value } => Wire {
                kind: "shortcut_target",
                op,
                value,
            }
            .serialize(s),
            Self::TargetExtension { op, value } => Wire {
                kind: "target_extension",
                op,
                value,
            }
            .serialize(s),
            Self::TargetPath { op, value } => Wire {
                kind: "target_path",
                op,
                value,
            }
            .serialize(s),
            Self::Unknown { raw } => raw.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Condition {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Pair<O, V> {
            op: O,
            value: V,
        }
        fn pair<O, V>(raw: &serde_json::Value) -> Option<Pair<O, V>>
        where
            O: serde::de::DeserializeOwned,
            V: serde::de::DeserializeOwned,
        {
            serde_json::from_value(raw.clone()).ok()
        }
        let raw = serde_json::Value::deserialize(d)?;
        let parsed = match raw.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "item_type" => pair(&raw).map(|Pair { op, value }| Self::ItemType { op, value }),
            "extension" => pair(&raw).map(|Pair { op, value }| Self::Extension { op, value }),
            "name" => pair(&raw).map(|Pair { op, value }| Self::Name { op, value }),
            "size" => pair(&raw).map(|Pair { op, value }| Self::Size { op, value }),
            "modified" => pair(&raw).map(|Pair { op, value }| Self::Modified { op, value }),
            "shortcut_target" => {
                pair(&raw).map(|Pair { op, value }| Self::ShortcutTarget { op, value })
            }
            "target_extension" => {
                pair(&raw).map(|Pair { op, value }| Self::TargetExtension { op, value })
            }
            "target_path" => pair(&raw).map(|Pair { op, value }| Self::TargetPath { op, value }),
            _ => None,
        };
        Ok(parsed.unwrap_or(Self::Unknown { raw }))
    }
}

fn is_null(value: &serde_json::Value) -> bool {
    value.is_null()
}

/// A rule: one sentence over a flat list of conditions (round-up-spec 2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    /// Stable id; never changes when the rule is edited, renamed or reordered.
    pub id: String,
    /// Empty = use the generated sentence.
    #[serde(default)]
    pub name: String,
    pub enabled: bool,
    /// Built-in preset this rule came from, if any.
    #[serde(default)]
    pub preset: Option<String>,
    #[serde(rename = "match", default)]
    pub match_mode: MatchMode,
    pub conditions: Vec<Condition>,
    /// `None` only on a disabled rule (an unconfigured preset).
    #[serde(default)]
    pub destination: Option<Destination>,
    /// Old values kept verbatim by the migration (`place_at_end`, `auto_id`); never read again.
    #[serde(default, skip_serializing_if = "is_null")]
    pub legacy: serde_json::Value,
}

/// A rule of the old model the migration could not convert one to one (round-up-spec 6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewItem {
    /// Position in the old list.
    pub position: usize,
    pub name: String,
    pub data: String,
    pub target: String,
    pub reason: String,
}

/// The `RoundUp` block of the settings file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoundUp {
    pub version: u32,
    pub migrated_from_legacy: bool,
    pub default_destination: Destination,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub needs_review: Vec<ReviewItem>,
    /// Why Round-up does nothing (newer block, unreadable block, failed backup). Runtime only.
    #[serde(skip)]
    pub paused_reason: Option<String>,
    /// A migration happened in memory and no configuration copy exists yet. Runtime only.
    #[serde(skip)]
    pub migration_pending: bool,
}

impl Default for RoundUp {
    fn default() -> Self {
        Self {
            version: BLOCK_VERSION,
            migrated_from_legacy: true,
            default_destination: Destination::Desktop,
            rules: presets(),
            needs_review: Vec::new(),
            paused_reason: None,
            migration_pending: false,
        }
    }
}

impl RoundUp {
    /// Round-up may run: understood version, not paused, nothing unsaved from a migration.
    pub fn active(&self) -> bool {
        self.version <= BLOCK_VERSION && self.paused_reason.is_none() && !self.migration_pending
    }

    /// There is something an automatic run could do (round-up-spec 3, "When rules run").
    pub fn has_work(&self) -> bool {
        self.active()
            && (matches!(self.default_destination, Destination::Corral { .. })
                || self.rules.iter().any(Rule::runs))
    }

    /// A new rule goes above the first built-in rule, so it can win (round-up-spec 5).
    pub fn insert_rule(&mut self, rule: Rule) -> usize {
        let index = self
            .rules
            .iter()
            .position(|r| r.preset.is_some())
            .unwrap_or(self.rules.len());
        self.rules.insert(index, rule);
        index
    }

    /// Swap a rule with its neighbour. Returns the new index.
    pub fn move_rule(&mut self, index: usize, up: bool) -> Option<usize> {
        let other = if up { index.checked_sub(1)? } else { index + 1 };
        if index >= self.rules.len() || other >= self.rules.len() {
            return None;
        }
        self.rules.swap(index, other);
        Some(other)
    }

    /// An id no rule of this block uses yet.
    pub fn new_rule_id(&self) -> String {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        loop {
            let id = format!(
                "r-{:x}{:02x}",
                crate::round_up_time::Clock::now().now_ms,
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 256
            );
            if !self.rules.iter().any(|r| r.id == id) {
                return id;
            }
        }
    }

    /// A blank user rule with a fresh id.
    pub fn new_user_rule(&self) -> Rule {
        Rule {
            id: self.new_rule_id(),
            name: String::new(),
            enabled: true,
            preset: None,
            match_mode: MatchMode::All,
            conditions: vec![Condition::Name {
                op: TextOp::Contains,
                value: String::new(),
            }],
            destination: None,
            legacy: serde_json::Value::Null,
        }
    }
}

impl Rule {
    /// The rule takes part in evaluation: enabled, complete, and with a destination.
    pub fn runs(&self) -> bool {
        self.enabled && self.destination.is_some() && self.validate().is_ok()
    }

    /// What is wrong with the rule, in words for the editor (round-up-spec 5, invalid input).
    pub fn validate(&self) -> Result<(), String> {
        if self.conditions.is_empty() {
            return Err("Add a condition.".into());
        }
        for condition in &self.conditions {
            condition.validate()?;
        }
        if self.enabled && self.destination.is_none() {
            return Err("Choose where matching icons go.".into());
        }
        Ok(())
    }

    /// The rule has a condition this build cannot read ("needs an update", round-up-spec 6).
    pub fn needs_update(&self) -> bool {
        self.conditions
            .iter()
            .any(|c| matches!(c, Condition::Unknown { .. }))
    }

    fn join(&self) -> &'static str {
        match self.match_mode {
            MatchMode::All => " and ",
            MatchMode::Any => " or ",
        }
    }

    /// `is a folder and its name contains "x"`: the conditions read as one phrase.
    pub fn condition_phrase(&self) -> String {
        self.conditions
            .iter()
            .map(Condition::phrase)
            .collect::<Vec<_>>()
            .join(self.join())
    }

    /// `When an item has the extension pdf, put it in Accounts.` (round-up-spec 5.)
    pub fn sentence(&self, destination: &str) -> String {
        let outcome = match self.destination {
            Some(Destination::Desktop) => "leave it on the desktop".to_string(),
            _ => format!("put it in {destination}"),
        };
        format!("When an item {}, {outcome}.", self.condition_phrase())
    }

    /// The name shown in lists: the user's name, else the generated sentence.
    pub fn title(&self, destination: &str) -> String {
        if self.name.trim().is_empty() {
            self.sentence(destination)
        } else {
            self.name.clone()
        }
    }
}

impl Condition {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Unknown { .. } => Err("This condition needs an update of Corral.".into()),
            Self::Name { value, .. } | Self::TargetPath { value, .. }
                if value.trim().is_empty() =>
            {
                Err("Enter some text.".into())
            }
            Self::Extension { op, value } | Self::TargetExtension { op, value }
                if *op != ExtensionOp::HasNone =>
            {
                if value.is_empty() {
                    Err("Enter at least one extension.".into())
                } else if value.iter().any(|e| !valid_extension(e)) {
                    Err("Write extensions without a dot, like pdf.".into())
                } else {
                    Ok(())
                }
            }
            Self::Size { value, .. } if value.bytes().is_none() => {
                Err("That size is too large.".into())
            }
            Self::Modified { value, .. } if value.n == 0 => Err("Enter a number above 0.".into()),
            _ => Ok(()),
        }
    }

    /// The condition as the phrase after `When an item` (round-up-spec 5, sentence).
    pub fn phrase(&self) -> String {
        match self {
            Self::ItemType { op, value } => format!(
                "{} {}",
                if *op == IsOp::Is { "is" } else { "is not" },
                match value {
                    ItemType::Folder => "a folder",
                    ItemType::File => "a file",
                    ItemType::Shortcut => "a shortcut",
                    ItemType::SystemItem => "a system item",
                }
            ),
            Self::Extension { op, value } => extension_phrase("", *op, value),
            Self::TargetExtension { op, value } => extension_phrase("its target ", *op, value),
            Self::Name { op, value } => text_phrase("its name", *op, value),
            Self::TargetPath { op, value } => text_phrase("its target path", *op, value),
            Self::Size { op, value } => format!(
                "has a size {} {} {:?}",
                if *op == SizeOp::AtLeast {
                    "of at least"
                } else {
                    "below"
                },
                value.n,
                value.unit
            ),
            Self::Modified { op, value } => format!(
                "{} {} {}",
                if *op == DateOp::NewerThan {
                    "was changed in the last"
                } else {
                    "was last changed more than"
                },
                value.n,
                unit_word(*value, *op == DateOp::OlderThan)
            ),
            Self::ShortcutTarget { op, value } => format!(
                "{} to {}",
                if *op == IsOp::Is {
                    "points"
                } else {
                    "does not point"
                },
                match value {
                    TargetKind::Program => "a program",
                    TargetKind::Folder => "a folder",
                    TargetKind::File => "a file",
                }
            ),
            Self::Unknown { .. } => "needs an update".into(),
        }
    }
}

fn valid_extension(extension: &str) -> bool {
    !extension.is_empty()
        && !extension.contains(['.', '*', '?', '/', '\\', ' ', ','])
        && extension.trim() == extension
}

fn unit_word(age: Age, ago: bool) -> String {
    let word = match age.unit {
        TimeUnit::Hours => "hour",
        TimeUnit::Days => "day",
        TimeUnit::Weeks => "week",
        TimeUnit::Months => "month",
    };
    format!(
        "{word}{}{}",
        if age.n == 1 { "" } else { "s" },
        if ago { " ago" } else { "" }
    )
}

fn extension_phrase(owner: &str, op: ExtensionOp, value: &[String]) -> String {
    let list = value.join(", ");
    match op {
        ExtensionOp::HasNone => format!("{owner}has no extension"),
        ExtensionOp::IsOneOf if value.len() == 1 => format!("{owner}has the extension {list}"),
        ExtensionOp::IsOneOf => format!("{owner}has one of the extensions {list}"),
        ExtensionOp::IsNoneOf => format!("{owner}has none of the extensions {list}"),
    }
}

fn text_phrase(subject: &str, op: TextOp, value: &str) -> String {
    let verb = match op {
        TextOp::Is => "is",
        TextOp::Contains => "contains",
        TextOp::DoesNotContain => "does not contain",
        TextOp::StartsWith => "starts with",
        TextOp::EndsWith => "ends with",
        TextOp::MatchesPattern => "matches the pattern",
    };
    format!("{subject} {verb} \"{value}\"")
}

/// The seven built-in rules, in the seeded order (round-up-spec 6): disabled, no destination.
pub fn presets() -> Vec<Rule> {
    PRESET_IDS.iter().filter_map(|id| preset_rule(id)).collect()
}

pub const PRESET_IDS: [&str; 7] = [
    "program_shortcuts",
    "web_links",
    "folders",
    "documents",
    "images",
    "music_video",
    "downloads",
];

/// Name shown for a built-in rule.
pub fn preset_name(id: &str) -> Option<&'static str> {
    Some(match id {
        "program_shortcuts" => "Program shortcuts",
        "web_links" => "Web links",
        "folders" => "Folders",
        "documents" => "Documents",
        "images" => "Images",
        "music_video" => "Music and video",
        "downloads" => "Downloads",
        _ => return None,
    })
}

/// A built-in rule in its default form (also what "Reset to default" restores).
pub fn preset_rule(id: &str) -> Option<Rule> {
    let list = |text: &str| Condition::Extension {
        op: ExtensionOp::IsOneOf,
        value: text.split_whitespace().map(str::to_string).collect(),
    };
    let conditions = match id {
        "program_shortcuts" => vec![
            Condition::ItemType {
                op: IsOp::Is,
                value: ItemType::Shortcut,
            },
            Condition::ShortcutTarget {
                op: IsOp::Is,
                value: TargetKind::Program,
            },
        ],
        "web_links" => vec![list("url")],
        "folders" => vec![Condition::ItemType {
            op: IsOp::Is,
            value: ItemType::Folder,
        }],
        "documents" => vec![list(
            "pdf doc docx xls xlsx ppt pptx txt rtf odt ods csv md",
        )],
        "images" => vec![list("jpg jpeg png gif bmp webp tif tiff heic svg")],
        "music_video" => vec![list("mp3 wav flac m4a aac ogg mp4 mkv mov avi wmv webm")],
        "downloads" => vec![list("zip 7z rar iso msi torrent")],
        _ => return None,
    };
    Some(Rule {
        id: format!("preset-{id}"),
        name: preset_name(id)?.to_string(),
        enabled: false,
        preset: Some(id.to_string()),
        match_mode: MatchMode::All,
        conditions,
        destination: None,
        legacy: serde_json::Value::Null,
    })
}
