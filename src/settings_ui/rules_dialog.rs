//! The Round-up rule editor (round-up-spec 5): a live sentence over a flat list of conditions with
//! an All / Any switch, a Corral picker, a rule name and a live match line.
//!
//! The editor only changes an in-memory [`Rule`]; saving, ordering and applying belong to the
//! caller. The renderer is a small scene (drawn with the settings kit) over a modal window with
//! native EDIT children for the text values.

use std::cell::Cell;
use std::sync::Once;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateFontW, CreateSolidBrush, DeleteObject, EndPaint, HBRUSH, HDC, HFONT,
    HGDIOBJ, InvalidateRect, PAINTSTRUCT, SRCCOPY, SetBkColor, SetTextColor,
};
use windows::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetFocus, GetKeyState, IsWindowEnabled, SetFocus, VK_DOWN, VK_ESCAPE, VK_MENU,
    VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, w};

use super::kit::controls::{self, ButtonStyle, Control};
use super::kit::theme::{Theme as Tokens, Type};
use super::rules_page::{self, GroupInfo};
use super::{Theme, UI, scf};
use crate::item_facts::ItemFacts;
use crate::render::{Align, Ctx, RectF, Rgba, Surface, TextStyle};
use crate::round_up::{
    Age, Condition, DateOp, Destination, ExtensionOp, IsOp, ItemType, MatchMode, Rule, SizeOp,
    SizeUnit, SizeValue, TargetKind, TextOp, TimeUnit,
};

const WIDTH_DIP: f32 = 680.;
const TIMER_CAPTION: usize = 41;

// ---------------------------------------------------------------------------------------------
// The editable form of a condition

/// The eight kinds in menu order (round-up-spec 5).
const KIND_LABELS: [&str; 8] = [
    "Item type",
    "Extension",
    "Name",
    "Size",
    "Date modified",
    "Shortcut points to",
    "Target extension",
    "Target path",
];

const TEXT_OPS: [(&str, TextOp); 6] = [
    ("is", TextOp::Is),
    ("contains", TextOp::Contains),
    ("does not contain", TextOp::DoesNotContain),
    ("starts with", TextOp::StartsWith),
    ("ends with", TextOp::EndsWith),
    ("matches pattern", TextOp::MatchesPattern),
];
const EXTENSION_OPS: [(&str, ExtensionOp); 3] = [
    ("is one of", ExtensionOp::IsOneOf),
    ("is none of", ExtensionOp::IsNoneOf),
    ("has none", ExtensionOp::HasNone),
];
const ITEM_TYPES: [(&str, ItemType); 4] = [
    ("Folder", ItemType::Folder),
    ("File", ItemType::File),
    ("Shortcut", ItemType::Shortcut),
    ("System item", ItemType::SystemItem),
];
const TARGET_KINDS: [(&str, TargetKind); 3] = [
    ("Program", TargetKind::Program),
    ("Folder", TargetKind::Folder),
    ("File", TargetKind::File),
];
const SIZE_UNITS: [(&str, SizeUnit); 3] = [
    ("KB", SizeUnit::KB),
    ("MB", SizeUnit::MB),
    ("GB", SizeUnit::GB),
];
const TIME_UNITS: [(&str, TimeUnit); 4] = [
    ("hours", TimeUnit::Hours),
    ("days", TimeUnit::Days),
    ("weeks", TimeUnit::Weeks),
    ("months", TimeUnit::Months),
];

fn kind_index(c: &Condition) -> Option<usize> {
    Some(match c {
        Condition::ItemType { .. } => 0,
        Condition::Extension { .. } => 1,
        Condition::Name { .. } => 2,
        Condition::Size { .. } => 3,
        Condition::Modified { .. } => 4,
        Condition::ShortcutTarget { .. } => 5,
        Condition::TargetExtension { .. } => 6,
        Condition::TargetPath { .. } => 7,
        Condition::Unknown { .. } => return None,
    })
}

/// A fresh condition of kind `index` with the first operator and a sensible value.
fn default_condition(index: usize) -> Condition {
    match index {
        0 => Condition::ItemType {
            op: IsOp::Is,
            value: ItemType::Folder,
        },
        1 => Condition::Extension {
            op: ExtensionOp::IsOneOf,
            value: Vec::new(),
        },
        2 => Condition::Name {
            op: TextOp::Contains,
            value: String::new(),
        },
        3 => Condition::Size {
            op: SizeOp::AtLeast,
            value: SizeValue {
                n: 10,
                unit: SizeUnit::MB,
            },
        },
        4 => Condition::Modified {
            op: DateOp::NewerThan,
            value: Age {
                n: 7,
                unit: TimeUnit::Days,
            },
        },
        5 => Condition::ShortcutTarget {
            op: IsOp::Is,
            value: TargetKind::Program,
        },
        6 => Condition::TargetExtension {
            op: ExtensionOp::IsOneOf,
            value: Vec::new(),
        },
        _ => Condition::TargetPath {
            op: TextOp::Contains,
            value: String::new(),
        },
    }
}

/// Operator labels of a condition's kind.
fn op_labels(c: &Condition) -> Vec<&'static str> {
    match c {
        Condition::ItemType { .. } | Condition::ShortcutTarget { .. } => vec!["is", "is not"],
        Condition::Extension { .. } | Condition::TargetExtension { .. } => {
            EXTENSION_OPS.iter().map(|o| o.0).collect()
        }
        Condition::Name { .. } | Condition::TargetPath { .. } => {
            TEXT_OPS.iter().map(|o| o.0).collect()
        }
        Condition::Size { .. } => vec!["is at least", "is less than"],
        Condition::Modified { .. } => vec!["is newer than", "is older than"],
        Condition::Unknown { .. } => Vec::new(),
    }
}

fn op_index(c: &Condition) -> usize {
    match c {
        Condition::ItemType { op, .. } | Condition::ShortcutTarget { op, .. } => {
            usize::from(*op == IsOp::IsNot)
        }
        Condition::Extension { op, .. } | Condition::TargetExtension { op, .. } => {
            EXTENSION_OPS.iter().position(|o| o.1 == *op).unwrap_or(0)
        }
        Condition::Name { op, .. } | Condition::TargetPath { op, .. } => {
            TEXT_OPS.iter().position(|o| o.1 == *op).unwrap_or(0)
        }
        Condition::Size { op, .. } => usize::from(*op == SizeOp::LessThan),
        Condition::Modified { op, .. } => usize::from(*op == DateOp::OlderThan),
        Condition::Unknown { .. } => 0,
    }
}

fn set_op(c: &mut Condition, i: usize) {
    let is = if i == 0 { IsOp::Is } else { IsOp::IsNot };
    match c {
        Condition::ItemType { op, .. } | Condition::ShortcutTarget { op, .. } => *op = is,
        Condition::Extension { op, .. } | Condition::TargetExtension { op, .. } => {
            *op = EXTENSION_OPS[i.min(2)].1
        }
        Condition::Name { op, .. } | Condition::TargetPath { op, .. } => *op = TEXT_OPS[i.min(5)].1,
        Condition::Size { op, .. } => {
            *op = if i == 0 {
                SizeOp::AtLeast
            } else {
                SizeOp::LessThan
            }
        }
        Condition::Modified { op, .. } => {
            *op = if i == 0 {
                DateOp::NewerThan
            } else {
                DateOp::OlderThan
            }
        }
        Condition::Unknown { .. } => {}
    }
}

/// How the value of a condition is entered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Value {
    Choice,
    Text,
    Number,
    None,
}

fn value_kind(c: &Condition) -> Value {
    match c {
        Condition::ItemType { .. } | Condition::ShortcutTarget { .. } => Value::Choice,
        Condition::Extension { op, .. } | Condition::TargetExtension { op, .. } => {
            if *op == ExtensionOp::HasNone {
                Value::None
            } else {
                Value::Text
            }
        }
        Condition::Name { .. } | Condition::TargetPath { .. } => Value::Text,
        Condition::Size { .. } | Condition::Modified { .. } => Value::Number,
        Condition::Unknown { .. } => Value::None,
    }
}

fn choice_labels(c: &Condition) -> Vec<&'static str> {
    match c {
        Condition::ItemType { .. } => ITEM_TYPES.iter().map(|o| o.0).collect(),
        Condition::ShortcutTarget { .. } => TARGET_KINDS.iter().map(|o| o.0).collect(),
        _ => Vec::new(),
    }
}

fn choice_index(c: &Condition) -> usize {
    match c {
        Condition::ItemType { value, .. } => {
            ITEM_TYPES.iter().position(|o| o.1 == *value).unwrap_or(0)
        }
        Condition::ShortcutTarget { value, .. } => {
            TARGET_KINDS.iter().position(|o| o.1 == *value).unwrap_or(0)
        }
        _ => 0,
    }
}

fn set_choice(c: &mut Condition, i: usize) {
    match c {
        Condition::ItemType { value, .. } => *value = ITEM_TYPES[i.min(3)].1,
        Condition::ShortcutTarget { value, .. } => *value = TARGET_KINDS[i.min(2)].1,
        _ => {}
    }
}

fn unit_labels(c: &Condition) -> Vec<&'static str> {
    match c {
        Condition::Size { .. } => SIZE_UNITS.iter().map(|o| o.0).collect(),
        Condition::Modified { .. } => TIME_UNITS.iter().map(|o| o.0).collect(),
        _ => Vec::new(),
    }
}

fn unit_index(c: &Condition) -> usize {
    match c {
        Condition::Size { value, .. } => SIZE_UNITS
            .iter()
            .position(|o| o.1 == value.unit)
            .unwrap_or(0),
        Condition::Modified { value, .. } => TIME_UNITS
            .iter()
            .position(|o| o.1 == value.unit)
            .unwrap_or(0),
        _ => 0,
    }
}

fn set_unit(c: &mut Condition, i: usize) {
    match c {
        Condition::Size { value, .. } => value.unit = SIZE_UNITS[i.min(2)].1,
        Condition::Modified { value, .. } => value.unit = TIME_UNITS[i.min(3)].1,
        _ => {}
    }
}

/// One condition row: the typed condition plus the text the user is typing for its value.
#[derive(Clone, Debug, PartialEq)]
struct Form {
    cond: Condition,
    text: String,
    /// The typed text does not make a value (a letter in a number).
    parse_error: Option<&'static str>,
}

impl Form {
    fn new(cond: Condition) -> Self {
        let text = match &cond {
            Condition::Extension { value, .. } | Condition::TargetExtension { value, .. } => {
                value.join(", ")
            }
            Condition::Name { value, .. } | Condition::TargetPath { value, .. } => value.clone(),
            Condition::Size { value, .. } => value.n.to_string(),
            Condition::Modified { value, .. } => value.n.to_string(),
            _ => String::new(),
        };
        Self {
            cond,
            text,
            parse_error: None,
        }
    }

    /// The user edited the value text: take it over into the typed condition.
    fn set_text(&mut self, text: String) {
        self.parse_error = None;
        match &mut self.cond {
            Condition::Extension { value, .. } | Condition::TargetExtension { value, .. } => {
                *value = text
                    .split([',', ';', ' '])
                    .map(|e| e.trim().trim_start_matches('.').to_string())
                    .filter(|e| !e.is_empty())
                    .collect();
            }
            Condition::Name { value, .. } | Condition::TargetPath { value, .. } => {
                *value = text.clone()
            }
            Condition::Size { value, .. } => match text.trim().parse::<u64>() {
                Ok(n) => value.n = n,
                Err(_) => self.parse_error = Some("Enter a whole number."),
            },
            Condition::Modified { value, .. } => match text.trim().parse::<u32>() {
                Ok(n) => value.n = n,
                Err(_) => self.parse_error = Some("Enter a whole number."),
            },
            _ => {}
        }
        self.text = text;
    }

    fn error(&self) -> Option<String> {
        self.parse_error
            .map(str::to_string)
            .or_else(|| self.cond.validate().err())
    }
}

// ---------------------------------------------------------------------------------------------
// Editor state

struct EditorState {
    rule: Rule,
    forms: Vec<Form>,
    groups: Vec<GroupInfo>,
    above: Vec<Rule>,
    is_new: bool,
    deleted: bool,
    /// First visible condition row and how many rows fit (long rule lists scroll).
    scroll: usize,
    visible: usize,
    caption: String,
    facts: Option<Result<Vec<ItemFacts>, String>>,
}

impl EditorState {
    fn new(rule: &Rule, is_new: bool, groups: &[GroupInfo], above: &[Rule]) -> Self {
        Self {
            rule: rule.clone(),
            forms: rule.conditions.iter().cloned().map(Form::new).collect(),
            groups: groups.to_vec(),
            above: above.to_vec(),
            is_new,
            deleted: false,
            scroll: 0,
            visible: 4,
            caption: String::new(),
            facts: None,
        }
    }

    /// Copy the forms into the rule so that its sentence and validation are current.
    fn sync(&mut self) {
        self.rule.conditions = self.forms.iter().map(|f| f.cond.clone()).collect();
    }

    /// The first thing keeping Save disabled, in words.
    fn error(&self) -> Option<String> {
        self.forms.iter().find_map(Form::error).or_else(|| {
            self.rule
                .destination
                .is_none()
                .then(|| "Choose where matching icons go.".to_string())
        })
    }

    fn mode(&self) -> MatchMode {
        self.rule.match_mode
    }
}

// ---------------------------------------------------------------------------------------------
// Scene

#[derive(Clone, Debug)]
enum Draw {
    Control {
        rect: RectF,
        kind: Control,
        label: String,
        enabled: bool,
    },
    Segmented {
        rect: RectF,
        labels: [&'static str; 2],
        selected: usize,
    },
    Text {
        rect: RectF,
        text: String,
        ty: Type,
        color: Rgba,
        align: Align,
        icon: bool,
    },
    Box {
        rect: RectF,
        fill: Option<Rgba>,
        stroke: Option<Rgba>,
        radius: f32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Act {
    MatchAll,
    MatchAny,
    Kind(usize),
    Op(usize),
    Choice(usize),
    Unit(usize),
    Remove(usize),
    Add,
    Destination,
    DeleteRule,
    Save,
    Cancel,
}

#[derive(Clone, Copy, Debug)]
struct Region {
    rect: RectF,
    act: Act,
    enabled: bool,
    /// Draws a hover wash (buttons and links).
    wash: bool,
}

/// Which native edit: a condition row's value, or the rule name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditKey {
    Value(usize),
    Name,
}

#[derive(Clone, Debug)]
struct EditSpec {
    key: EditKey,
    rect: RectF,
    text: String,
}

#[derive(Clone, Debug, Default)]
struct Scene {
    draws: Vec<Draw>,
    regions: Vec<Region>,
    edits: Vec<EditSpec>,
    /// Client size in device pixels.
    size: (i32, i32),
    footer_top: f32,
    light: bool,
}

struct Builder {
    scene: Scene,
    dpi: u32,
    t: Tokens,
}

impl Builder {
    fn d(&self, v: f32) -> f32 {
        scf(v, self.dpi)
    }

    /// DIP rectangle to device pixels.
    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> RectF {
        RectF::new(self.d(x), self.d(y), self.d(x + w), self.d(y + h))
    }

    fn text(&mut self, ty: Type, x: f32, y: f32, w: f32, text: &str, color: Rgba) {
        let h = ty.metrics().1;
        let rect = self.rect(x, y, w, h);
        self.scene.draws.push(Draw::Text {
            rect,
            text: text.into(),
            ty,
            color,
            align: Align::Left,
            icon: false,
        });
    }

    /// Text vertically centred in a `h`-high box.
    fn text_in(&mut self, ty: Type, x: f32, y: f32, w: f32, h: f32, text: &str, color: Rgba) {
        let rect = self.rect(x, y, w, h);
        self.scene.draws.push(Draw::Text {
            rect,
            text: text.into(),
            ty,
            color,
            align: Align::Left,
            icon: false,
        });
    }

    fn boxed(&mut self, rect: RectF, fill: Option<Rgba>, stroke: Option<Rgba>, radius: f32) {
        self.scene.draws.push(Draw::Box {
            rect,
            fill,
            stroke,
            radius,
        });
    }

    fn region(&mut self, rect: RectF, act: Act, enabled: bool, wash: bool) {
        self.scene.regions.push(Region {
            rect,
            act,
            enabled,
            wash,
        });
    }

    /// A dropdown-looking button: field box, text, chevron; the whole box is the click region.
    fn combo(&mut self, x: f32, y: f32, w: f32, text: &str, act: Act) {
        let rect = self.rect(x, y, w, 32.);
        let (fill, stroke, radius) = (self.t.control, self.t.border, self.d(4.));
        self.boxed(rect, Some(fill), Some(stroke), radius);
        let label = self.rect(x + 12., y, w - 36., 32.);
        let color = self.t.text;
        self.scene.draws.push(Draw::Text {
            rect: label,
            text: text.into(),
            ty: Type::Body,
            color,
            align: Align::Left,
            icon: false,
        });
        let chevron = self.rect(x + w - 24., y, 16., 32.);
        let color = self.t.secondary;
        self.scene.draws.push(Draw::Text {
            rect: chevron,
            text: "\u{E70D}".into(),
            ty: Type::Caption,
            color,
            align: Align::Center,
            icon: true,
        });
        self.region(rect, act, true, false);
    }

    fn field(&mut self, x: f32, y: f32, w: f32, key: EditKey, text: &str) {
        let rect = self.rect(x, y, w, 32.);
        let (fill, stroke, radius) = (self.t.control, self.t.border, self.d(4.));
        self.boxed(rect, Some(fill), Some(stroke), radius);
        let inner = self.rect(x + 10., y + 6., w - 20., 20.);
        self.scene.edits.push(EditSpec {
            key,
            rect: inner,
            text: text.into(),
        });
    }

    fn button(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        label: &str,
        act: Act,
        enabled: bool,
        primary: bool,
    ) {
        let rect = self.rect(x, y, w, 32.);
        let kind = Control::Button(if primary {
            ButtonStyle::Primary
        } else {
            ButtonStyle::Secondary
        });
        self.scene.draws.push(Draw::Control {
            rect,
            kind,
            label: label.into(),
            enabled,
        });
        self.region(rect, act, enabled, true);
    }

    fn link(&mut self, x: f32, y: f32, text: &str, act: Act, color: Rgba) -> f32 {
        let width = crate::render::measure_text(text, UI, self.d(14.), 400)
            .map(|m| m.0)
            .unwrap_or(120.)
            / self.d(1.)
            + 8.;
        let rect = self.rect(x, y, width, 32.);
        self.scene.draws.push(Draw::Text {
            rect: self.rect(x + 4., y, width - 4., 32.),
            text: text.into(),
            ty: Type::Body,
            color,
            align: Align::Left,
            icon: false,
        });
        self.region(rect, act, true, true);
        width
    }
}

/// Lay the whole editor out for the current state.
fn editor_scene(st: &EditorState, theme: &Theme, dpi: u32) -> Scene {
    let t = Tokens::new(theme.light);
    let mut b = Builder {
        scene: Scene {
            light: theme.light,
            ..Default::default()
        },
        dpi,
        t,
    };
    let inner = WIDTH_DIP - 48.;
    let mut y = 24.;
    b.text(Type::DialogTitle, 24., y, inner, "Round-up rule", t.text);
    y += 36.;
    b.text(
        Type::Body,
        24.,
        y,
        inner,
        "Icons that match these conditions go to the Corral you pick.",
        t.secondary,
    );
    y += 36.;

    // "An icon matches when [All | Any] of these are true:"
    let single = st.forms.len() <= 1;
    if single {
        b.text_in(
            Type::Body,
            24.,
            y,
            inner,
            32.,
            "An icon matches when this is true:",
            t.text,
        );
    } else {
        b.text_in(
            Type::Body,
            24.,
            y,
            150.,
            32.,
            "An icon matches when",
            t.text,
        );
        let seg = b.rect(180., y, 112., 32.);
        let selected = usize::from(st.mode() == MatchMode::Any);
        b.scene.draws.push(Draw::Segmented {
            rect: seg,
            labels: ["All", "Any"],
            selected,
        });
        let half = b.d(56.);
        b.region(
            RectF::new(seg.left, seg.top, seg.left + half, seg.bottom),
            Act::MatchAll,
            true,
            false,
        );
        b.region(
            RectF::new(seg.left + half, seg.top, seg.right, seg.bottom),
            Act::MatchAny,
            true,
            false,
        );
        b.text_in(Type::Body, 304., y, 300., 32., "of these are true:", t.text);
    }
    y += 44.;

    // Condition rows.
    let last = (st.scroll + st.visible).min(st.forms.len());
    for i in st.scroll..last {
        let form = &st.forms[i];
        let conj = if i == 0 {
            "Where"
        } else if st.mode() == MatchMode::Any {
            "or"
        } else {
            "and"
        };
        b.text_in(Type::Body, 24., y, 72., 32., conj, t.secondary);
        let kind_label = kind_index(&form.cond).map_or("Needs an update", |k| KIND_LABELS[k]);
        b.combo(104., y, 170., kind_label, Act::Kind(i));
        if kind_index(&form.cond).is_some() {
            let ops = op_labels(&form.cond);
            b.combo(
                282.,
                y,
                140.,
                ops.get(op_index(&form.cond)).copied().unwrap_or(""),
                Act::Op(i),
            );
            match value_kind(&form.cond) {
                Value::Choice => {
                    let labels = choice_labels(&form.cond);
                    b.combo(
                        430.,
                        y,
                        190.,
                        labels.get(choice_index(&form.cond)).copied().unwrap_or(""),
                        Act::Choice(i),
                    );
                }
                Value::Text => b.field(430., y, 190., EditKey::Value(i), &form.text),
                Value::Number => {
                    b.field(430., y, 84., EditKey::Value(i), &form.text);
                    let units = unit_labels(&form.cond);
                    b.combo(
                        522.,
                        y,
                        98.,
                        units.get(unit_index(&form.cond)).copied().unwrap_or(""),
                        Act::Unit(i),
                    );
                }
                Value::None => {}
            }
        } else {
            b.text_in(
                Type::Caption,
                282.,
                y,
                338.,
                32.,
                "Saved by a newer version of Corral. It is kept as it is.",
                t.secondary,
            );
        }
        if st.forms.len() > 1 {
            let rect = b.rect(628., y, 28., 32.);
            b.scene.draws.push(Draw::Control {
                rect,
                kind: Control::IconButton,
                label: String::new(),
                enabled: true,
            });
            b.scene.draws.push(Draw::Text {
                rect,
                text: "\u{E711}".into(),
                ty: Type::Caption,
                color: t.secondary,
                align: Align::Center,
                icon: true,
            });
            b.region(rect, Act::Remove(i), true, false);
        }
        y += 40.;
        if let Some(error) = form.error().filter(|_| kind_index(&form.cond).is_some()) {
            b.text(Type::Caption, 104., y - 6., 552., &error, t.danger);
            y += 20.;
        }
    }
    if st.forms.len() > st.visible {
        let text = format!(
            "Conditions {} to {} of {}. Scroll to see the others.",
            st.scroll + 1,
            last,
            st.forms.len()
        );
        b.text(Type::Caption, 104., y - 4., 552., &text, t.secondary);
        y += 24.;
    }
    b.link(100., y, "+  Add a condition", Act::Add, t.link);
    y += 44.;
    let divider = b.rect(24., y, inner, 1.);
    b.boxed(divider, Some(t.border), None, 0.);
    y += 13.;

    // The live sentence.
    let destination = rules_page::sentence_destination(st.rule.destination.as_ref(), &st.groups);
    let sentence = st.rule.sentence(&destination);
    let lines = super::wrap(&sentence, b.d(14.), 400, b.d(inner))
        .unwrap_or_else(|_| vec![sentence.clone()]);
    for line in lines {
        b.text(Type::Body, 24., y, inner, &line, t.text);
        y += 20.;
    }
    y += 12.;

    // Destination and name.
    b.text_in(
        Type::Body,
        24.,
        y,
        180.,
        32.,
        "Send matching icons to",
        t.text,
    );
    let dest = rules_page::destination_text(st.rule.destination.as_ref(), &st.groups);
    b.combo(212., y, 240., &dest, Act::Destination);
    y += 44.;
    b.text_in(Type::Body, 24., y, 180., 32., "Rule name", t.text);
    b.field(212., y, 444., EditKey::Name, &st.rule.name);
    y += 36.;
    b.text(
        Type::Caption,
        212.,
        y,
        444.,
        "Leave it empty to name the rule automatically.",
        t.secondary,
    );
    y += 24.;

    // The live match line, or what keeps Save disabled.
    let error = st.error();
    let caption = match &error {
        Some(error) => error.clone(),
        None if st.caption.is_empty() => "Checking today's desktop\u{2026}".to_string(),
        None => st.caption.clone(),
    };
    let wrapped =
        super::wrap(&caption, b.d(12.), 400, b.d(inner)).unwrap_or_else(|_| vec![caption.clone()]);
    for line in wrapped {
        b.text(
            Type::Caption,
            24.,
            y,
            inner,
            &line,
            if error.is_some() {
                t.danger
            } else {
                t.secondary
            },
        );
        y += 16.;
    }
    if st
        .forms
        .iter()
        .any(|f| matches!(f.cond, Condition::Modified { .. }))
    {
        b.text(
            Type::Caption,
            24.,
            y,
            inner,
            "Date rules are checked only when Round-up runs, not on a timer.",
            t.secondary,
        );
        y += 16.;
    }
    y += 16.;

    // Footer.
    b.scene.footer_top = b.d(y);
    y += 21.;
    if !st.is_new {
        let (label, color) = if st.rule.preset.is_some() {
            ("Reset to default", t.link)
        } else {
            ("Delete rule", t.danger)
        };
        b.link(20., y, label, Act::DeleteRule, color);
    }
    b.button(472., y, 88., "Cancel", Act::Cancel, true, false);
    b.button(568., y, 88., "Save", Act::Save, error.is_none(), true);
    y += 53.;
    b.scene.size = (b.d(WIDTH_DIP) as i32, b.d(y) as i32);
    b.scene
}

// ---------------------------------------------------------------------------------------------
// Painting

fn draw_scene(
    ctx: &Ctx,
    scene: &Scene,
    hover: Option<usize>,
    dpi: u32,
) -> windows::core::Result<()> {
    let tokens = Tokens::new(scene.light);
    let z = 96. / dpi as f32;
    crate::dialog::draw_frame(
        ctx,
        scene.size.0 as f32 * z,
        scene.size.1 as f32 * z,
        scene.footer_top * z,
        &tokens,
        dpi,
    )?;
    let icons = super::icon_family();
    let dip = |r: RectF| RectF::new(r.left * z, r.top * z, r.right * z, r.bottom * z);
    for draw in &scene.draws {
        match draw {
            Draw::Control {
                rect,
                kind,
                label,
                enabled,
            } => {
                let state = controls::VisualState {
                    enabled: *enabled,
                    ..Default::default()
                };
                controls::draw_control(ctx, *kind, dip(*rect), label, state, &tokens, dpi, 0.)?;
            }
            Draw::Segmented {
                rect,
                labels,
                selected,
            } => {
                controls::draw_segmented(
                    ctx,
                    dip(*rect),
                    labels,
                    *selected,
                    None,
                    Default::default(),
                    &tokens,
                    dpi,
                )?;
            }
            Draw::Box {
                rect,
                fill,
                stroke,
                radius,
            } => {
                if let Some(fill) = fill {
                    ctx.fill_rounded_rect(*rect, *radius, *fill)?;
                }
                if let Some(stroke) = stroke {
                    ctx.stroke_rounded_rect(*rect, *radius, scf(1., dpi).max(1.), *stroke)?;
                }
            }
            Draw::Text {
                rect,
                text,
                ty,
                color,
                align,
                icon,
            } => {
                let mut style: TextStyle<'_> = ty.style(dpi, *color);
                style.align = *align;
                if *icon {
                    style.family = icons;
                }
                ctx.draw_text(text, *rect, &style)?;
            }
        }
    }
    for edit in &scene.edits {
        let mut style = Type::Body.style(dpi, tokens.text);
        style.align = Align::Left;
        ctx.draw_text(&edit.text, edit.rect, &style)?;
    }
    if let Some(region) = hover
        .and_then(|i| scene.regions.get(i))
        .filter(|r| r.wash && r.enabled)
    {
        ctx.fill_rounded_rect(region.rect, scf(4., dpi), tokens.subtle_hover)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FocusTarget {
    Region(usize),
    Edit(EditKey),
}

fn focus_order(scene: &Scene) -> Vec<(RectF, FocusTarget)> {
    let mut v: Vec<_> = scene
        .regions
        .iter()
        .enumerate()
        .filter(|(_, r)| r.enabled)
        .map(|(i, r)| (r.rect, FocusTarget::Region(i)))
        .chain(
            scene
                .edits
                .iter()
                .map(|e| (e.rect, FocusTarget::Edit(e.key))),
        )
        .collect();
    // Reading order: by row, then left to right. The two halves of All | Any are one stop.
    v.sort_by(|a, b| {
        let row = |r: RectF| ((r.top + r.bottom) / 16.).round() as i32;
        row(a.0)
            .cmp(&row(b.0))
            .then_with(|| a.0.left.total_cmp(&b.0.left))
    });
    v
}

fn draw_focus(
    ctx: &Ctx,
    scene: &Scene,
    target: Option<FocusTarget>,
    dpi: u32,
) -> windows::core::Result<()> {
    let Some((r, t)) = focus_order(scene)
        .into_iter()
        .find(|(_, t)| Some(*t) == target)
    else {
        return Ok(());
    };
    let tokens = Tokens::new(scene.light);
    if matches!(t, FocusTarget::Edit(_)) {
        let bar = RectF::new(
            r.left - scf(8., dpi),
            r.bottom + scf(4., dpi),
            r.right + scf(8., dpi),
            r.bottom + scf(6., dpi),
        );
        ctx.fill_rect(bar, tokens.accent)
    } else {
        ctx.stroke_rounded_rect(r, scf(4., dpi), scf(2., dpi), tokens.focus)
    }
}

fn region_at(scene: &Scene, p: (i32, i32)) -> Option<usize> {
    let (x, y) = (p.0 as f32, p.1 as f32);
    scene
        .regions
        .iter()
        .rposition(|r| x >= r.rect.left && x < r.rect.right && y >= r.rect.top && y < r.rect.bottom)
}

// ---------------------------------------------------------------------------------------------
// Window

struct Dlg {
    theme: Theme,
    dpi: u32,
    surface: Option<Surface>,
    state: EditorState,
    scene: Scene,
    hover: Option<usize>,
    edits: Vec<(HWND, EditKey)>,
    font: HFONT,
    brush: HBRUSH,
    syncing: bool,
    done: bool,
    accepted: bool,
    focus: Option<FocusTarget>,
    keyboard_visible: bool,
}

fn colorref(c: Rgba) -> COLORREF {
    COLORREF(c.r as u32 | (c.g as u32) << 8 | (c.b as u32) << 16)
}

fn dlg_ptr(hwnd: HWND) -> *mut Dlg {
    // SAFETY: reads the pointer stored by WM_NCCREATE (null before and after the dialog's life).
    unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Dlg }
}

const STYLE: WINDOW_STYLE = WINDOW_STYLE(WS_POPUP.0 | WS_SYSMENU.0 | WS_CLIPCHILDREN.0);

fn outer_size(client: (i32, i32), dpi: u32) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: client.0,
        bottom: client.1,
    };
    // SAFETY: plain geometry call on a local RECT.
    let _ = unsafe { AdjustWindowRectExForDpi(&mut r, STYLE, false, WS_EX_TOOLWINDOW, dpi) };
    (r.right - r.left, r.bottom - r.top)
}

fn paint(hwnd: HWND, dlg: &mut Dlg) {
    let (w, h) = dlg.scene.size;
    if w <= 0 || h <= 0 {
        return;
    }
    let fits = dlg
        .surface
        .as_ref()
        .is_some_and(|s| s.width() == w as u32 && s.height() == h as u32);
    if !fits {
        dlg.surface = Surface::new(w as u32, h as u32).ok();
    }
    let Some(surface) = dlg.surface.as_mut() else {
        return;
    };
    let (scene, hover, dpi) = (&dlg.scene, dlg.hover, dlg.dpi);
    let (focus, show_focus) = (dlg.focus, dlg.keyboard_visible);
    let _ = surface.draw(|ctx| {
        draw_scene(ctx, scene, hover, dpi)?;
        if show_focus {
            draw_focus(ctx, scene, focus, dpi)?;
        }
        Ok(())
    });
    let mut ps = PAINTSTRUCT::default();
    // SAFETY: a BeginPaint/EndPaint pair with a live memory DC of the same size.
    unsafe {
        let hdc = BeginPaint(hwnd, &mut ps);
        let _ = BitBlt(hdc, 0, 0, w, h, surface.dc(), 0, 0, SRCCOPY);
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Pop up a choice menu at a client point; returns the chosen index.
fn pick(hwnd: HWND, items: &[(String, bool)], anchor: (i32, i32)) -> Option<usize> {
    let entries: Vec<_> = items
        .iter()
        .enumerate()
        .map(|(i, (text, checked))| super::window::Entry::Item {
            id: i + 1,
            text: text.clone(),
            checked: *checked,
            enabled: true,
        })
        .collect();
    super::window::show_corral_menu(hwnd, &entries, anchor)
        .and_then(|id| id.checked_sub(1))
        .filter(|i| *i < items.len())
}

fn read_edit(edit: HWND) -> String {
    // SAFETY: the buffer is local and sized from the control's own length.
    unsafe {
        let mut buf = vec![0u16; GetWindowTextLengthW(edit).max(0) as usize + 1];
        let n = GetWindowTextW(edit, &mut buf);
        String::from_utf16_lossy(&buf[..n.max(0) as usize])
    }
}

thread_local! {
    static REGISTERED: Cell<bool> = const { Cell::new(false) };
}
static REGISTER: Once = Once::new();

fn register_class(instance: HINSTANCE) {
    REGISTER.call_once(|| {
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DROPSHADOW,
            lpfnWndProc: Some(dlg_proc),
            hInstance: instance,
            // SAFETY: stock cursor.
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW).unwrap_or_default() },
            lpszClassName: w!("CorralRulesDialog"),
            ..Default::default()
        };
        // SAFETY: class registration with a static window procedure.
        unsafe {
            RegisterClassExW(&class);
        }
    });
    REGISTERED.with(|r| r.set(true));
}

fn make_font(dpi: u32) -> HFONT {
    // SAFETY: plain GDI font creation.
    unsafe {
        CreateFontW(
            -(scf(14., dpi).round() as i32),
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            &HSTRING::from(super::kit::theme::font_family()),
        )
    }
}

/// How many condition rows fit in a window no taller than `max_height` DIP.
fn configure_viewport(d: &mut Dlg, max_height: f32) {
    d.state.visible = 4;
    while d.state.visible > 1
        && editor_scene(&d.state, &d.theme, d.dpi).size.1 as f32 > scf(max_height, d.dpi)
    {
        d.state.visible -= 1;
    }
    d.state.scroll = d
        .state
        .scroll
        .min(d.state.forms.len().saturating_sub(d.state.visible));
}

/// Recompute the scene and bring the child edits and the window size in line with it.
fn rebuild(hwnd: HWND, structural: bool) {
    // SAFETY: the pointer is the dialog's own box; all calls happen on its thread.
    unsafe {
        let Some(d) = dlg_ptr(hwnd).as_mut() else {
            return;
        };
        let old_act = match d.focus {
            Some(FocusTarget::Region(i)) => d.scene.regions.get(i).map(|r| r.act),
            _ => None,
        };
        d.scene = editor_scene(&d.state, &d.theme, d.dpi);
        d.hover = None;
        d.syncing = true;
        if structural {
            for (h, _) in d.edits.drain(..) {
                let _ = DestroyWindow(h);
            }
        }
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or_default();
        for e in &d.scene.edits {
            let r = e.rect;
            let (x, y, w, h) = (
                r.left as i32,
                r.top as i32,
                (r.right - r.left) as i32,
                (r.bottom - r.top) as i32,
            );
            if let Some((edit, _)) = d.edits.iter().find(|(_, k)| *k == e.key) {
                let _ = SetWindowPos(*edit, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE);
            } else if let Ok(edit) = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("EDIT"),
                &HSTRING::from(e.text.as_str()),
                WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | 0x0080),
                x,
                y,
                w,
                h,
                hwnd,
                HMENU::default(),
                instance,
                None,
            ) {
                SendMessageW(edit, WM_SETFONT, WPARAM(d.font.0 as usize), LPARAM(1));
                d.edits.push((edit, e.key));
            }
        }
        d.syncing = false;
        if let Some(act) = old_act {
            d.focus = d
                .scene
                .regions
                .iter()
                .position(|r| r.act == act && r.enabled)
                .map(FocusTarget::Region);
        }
        if let Some(FocusTarget::Edit(k)) = d.focus {
            if let Some((edit, _)) = d.edits.iter().find(|(_, key)| *key == k) {
                let _ = SetFocus(*edit);
            }
        }
        let (ow, oh) = outer_size(d.scene.size, d.dpi);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            ow,
            oh,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE,
        );
        let _ = InvalidateRect(hwnd, None, false);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Semantic {
    Action(Act),
    Edit(EditKey),
}

fn semantic(scene: &Scene, target: FocusTarget) -> Option<Semantic> {
    match target {
        FocusTarget::Region(i) => scene.regions.get(i).map(|r| Semantic::Action(r.act)),
        FocusTarget::Edit(k) => Some(Semantic::Edit(k)),
    }
}

/// Tab / Shift+Tab over every stop of the dialog, scrolling hidden condition rows into view.
fn advance_focus(hwnd: HWND, reverse: bool) {
    // SAFETY: as in `rebuild`.
    unsafe {
        let Some(d) = dlg_ptr(hwnd).as_mut() else {
            return;
        };
        let actual = GetFocus();
        let current = d
            .edits
            .iter()
            .find(|(h, _)| *h == actual)
            .map(|(_, k)| Semantic::Edit(*k))
            .or_else(|| d.focus.and_then(|f| semantic(&d.scene, f)));
        // The order is taken from a scene that shows every row.
        let mut all = EditorState::new(
            &d.state.rule,
            d.state.is_new,
            &d.state.groups,
            &d.state.above,
        );
        all.forms = d.state.forms.clone();
        all.visible = all.forms.len().max(1);
        all.caption = d.state.caption.clone();
        let scene = editor_scene(&all, &d.theme, d.dpi);
        let order: Vec<Semantic> = focus_order(&scene)
            .into_iter()
            .filter_map(|(_, f)| semantic(&scene, f))
            .collect();
        if order.is_empty() {
            return;
        }
        let index = current.and_then(|f| order.iter().position(|x| *x == f));
        let next = match (index, reverse) {
            (Some(i), true) => (i + order.len() - 1) % order.len(),
            (Some(i), false) => (i + 1) % order.len(),
            (None, true) => order.len() - 1,
            (None, false) => 0,
        };
        let target = order[next];
        let row = match target {
            Semantic::Action(
                Act::Kind(i) | Act::Op(i) | Act::Choice(i) | Act::Unit(i) | Act::Remove(i),
            ) => Some(i),
            Semantic::Edit(EditKey::Value(i)) => Some(i),
            _ => None,
        };
        if let Some(row) = row {
            let before = d.state.scroll;
            if row < before {
                d.state.scroll = row;
            } else if row >= before + d.state.visible {
                d.state.scroll = row + 1 - d.state.visible;
            }
            if d.state.scroll != before {
                rebuild(hwnd, true);
            }
        }
        let d = &mut *dlg_ptr(hwnd);
        d.focus = match target {
            Semantic::Action(a) => d
                .scene
                .regions
                .iter()
                .position(|r| r.act == a)
                .map(FocusTarget::Region),
            Semantic::Edit(k) => Some(FocusTarget::Edit(k)),
        };
        match d.focus {
            Some(FocusTarget::Edit(k)) => {
                if let Some((h, _)) = d.edits.iter().find(|(_, key)| *key == k) {
                    let _ = SetFocus(*h);
                }
            }
            _ => {
                let _ = SetFocus(hwnd);
            }
        }
        d.keyboard_visible = true;
        let _ = InvalidateRect(hwnd, None, false);
    }
}

/// The live match line: how this rule alone would act on today's desktop (never applies anything).
fn refresh_caption(hwnd: HWND) {
    // SAFETY: as in `rebuild`.
    let Some(d) = (unsafe { dlg_ptr(hwnd).as_mut() }) else {
        return;
    };
    d.state.sync();
    d.state.caption = if d.state.rule.validate().is_err() && d.state.error().is_some() {
        String::new()
    } else {
        let facts = d
            .state
            .facts
            .get_or_insert_with(crate::item_facts::desktop_facts);
        match (facts, crate::round_up_time::LocalTimeZone::capture()) {
            (Ok(facts), Some(zone)) => crate::round_up::caption(
                &d.state.rule,
                &d.state.above,
                facts,
                crate::round_up::Clock::now(),
                &zone,
            ),
            (Err(message), _) => message.clone(),
            (_, None) => "The time zone could not be read.".into(),
        }
    };
    rebuild(hwnd, false);
}

fn schedule_caption(hwnd: HWND) {
    // SAFETY: plain timer call; the dialog kills it when it fires.
    unsafe {
        let _ = SetTimer(hwnd, TIMER_CAPTION, 200, None);
    }
}

fn on_act(hwnd: HWND, act: Act) {
    let ptr = dlg_ptr(hwnd);
    if ptr.is_null() {
        return;
    }
    // SAFETY: the box lives until the message loop ends; menus pump messages but never free it.
    let d = unsafe { &mut *ptr };
    let anchor = d
        .scene
        .regions
        .iter()
        .find(|r| r.act == act)
        .map_or((24, 100), |r| (r.rect.left as i32, r.rect.bottom as i32));
    let mut structural = false;
    match act {
        Act::Save => {
            d.state.sync();
            if d.state.error().is_none() {
                d.state.rule.name = d.state.rule.name.trim().to_string();
                d.accepted = true;
                d.done = true;
            }
            return;
        }
        Act::Cancel => {
            d.done = true;
            return;
        }
        Act::MatchAll => d.state.rule.match_mode = MatchMode::All,
        Act::MatchAny => d.state.rule.match_mode = MatchMode::Any,
        Act::Kind(i) => {
            let current = kind_index(&d.state.forms[i].cond);
            let items: Vec<_> = KIND_LABELS
                .iter()
                .enumerate()
                .map(|(k, l)| ((*l).to_string(), Some(k) == current))
                .collect();
            if let Some(k) = pick(hwnd, &items, anchor) {
                let d = unsafe { &mut *ptr };
                if Some(k) != current {
                    d.state.forms[i] = Form::new(default_condition(k));
                    structural = true;
                }
            }
        }
        Act::Op(i) => {
            let labels = op_labels(&d.state.forms[i].cond);
            let current = op_index(&d.state.forms[i].cond);
            let items: Vec<_> = labels
                .iter()
                .enumerate()
                .map(|(k, l)| ((*l).to_string(), k == current))
                .collect();
            if let Some(k) = pick(hwnd, &items, anchor) {
                let d = unsafe { &mut *ptr };
                set_op(&mut d.state.forms[i].cond, k);
                structural = true;
            }
        }
        Act::Choice(i) => {
            let labels = choice_labels(&d.state.forms[i].cond);
            let current = choice_index(&d.state.forms[i].cond);
            let items: Vec<_> = labels
                .iter()
                .enumerate()
                .map(|(k, l)| ((*l).to_string(), k == current))
                .collect();
            if let Some(k) = pick(hwnd, &items, anchor) {
                let d = unsafe { &mut *ptr };
                set_choice(&mut d.state.forms[i].cond, k);
            }
        }
        Act::Unit(i) => {
            let labels = unit_labels(&d.state.forms[i].cond);
            let current = unit_index(&d.state.forms[i].cond);
            let items: Vec<_> = labels
                .iter()
                .enumerate()
                .map(|(k, l)| ((*l).to_string(), k == current))
                .collect();
            if let Some(k) = pick(hwnd, &items, anchor) {
                let d = unsafe { &mut *ptr };
                set_unit(&mut d.state.forms[i].cond, k);
            }
        }
        Act::Add => {
            d.state.forms.push(Form::new(default_condition(2)));
            d.state.scroll = d.state.forms.len().saturating_sub(d.state.visible);
            structural = true;
        }
        Act::Remove(i) => {
            if d.state.forms.len() > 1 {
                d.state.forms.remove(i);
                d.state.scroll = d
                    .state
                    .scroll
                    .min(d.state.forms.len().saturating_sub(d.state.visible));
                structural = true;
            }
        }
        Act::Destination => {
            let mut options = vec![Destination::Desktop];
            options.extend(
                rules_page::pickable(&d.state.groups)
                    .iter()
                    .map(|g| Destination::Corral { id: g.id as u64 }),
            );
            let items: Vec<_> = options
                .iter()
                .map(|o| {
                    (
                        rules_page::destination_text(Some(o), &d.state.groups),
                        d.state.rule.destination.as_ref() == Some(o),
                    )
                })
                .collect();
            if let Some(k) = pick(hwnd, &items, anchor) {
                let d = unsafe { &mut *ptr };
                d.state.rule.destination = Some(options[k].clone());
            }
        }
        Act::DeleteRule => {
            let reset = d.state.rule.preset.clone();
            let light = d.theme.light;
            let ask = crate::dialog::confirm(
                hwnd,
                light,
                if reset.is_some() {
                    "Reset rule?"
                } else {
                    "Remove rule?"
                },
                if reset.is_some() {
                    "Reset this rule to its default conditions?"
                } else {
                    "Remove this Round-up rule?"
                },
                if reset.is_some() { "Reset" } else { "Remove" },
                reset.is_none(),
            );
            let d = unsafe { &mut *ptr };
            if ask {
                match reset.as_deref().and_then(crate::round_up::preset_rule) {
                    Some(default) => {
                        let rule = Rule {
                            enabled: d.state.rule.enabled,
                            destination: d.state.rule.destination.clone(),
                            ..default
                        };
                        d.state.forms = rule.conditions.iter().cloned().map(Form::new).collect();
                        d.state.rule = rule;
                        d.state.scroll = 0;
                        structural = true;
                    }
                    None => {
                        d.state.deleted = true;
                        d.accepted = true;
                        d.done = true;
                        return;
                    }
                }
            }
        }
    }
    let d = unsafe { &mut *ptr };
    d.state.sync();
    rebuild(hwnd, structural);
    schedule_caption(hwnd);
}

fn on_edit_changed(hwnd: HWND, edit: HWND) {
    // SAFETY: as in `rebuild`.
    let Some(d) = (unsafe { dlg_ptr(hwnd).as_mut() }) else {
        return;
    };
    if d.syncing {
        return;
    }
    let Some(key) = d.edits.iter().find(|(h, _)| *h == edit).map(|(_, k)| *k) else {
        return;
    };
    let text = read_edit(edit);
    match key {
        EditKey::Name => d.state.rule.name = text,
        EditKey::Value(row) => {
            if let Some(form) = d.state.forms.get_mut(row) {
                form.set_text(text);
            }
        }
    }
    d.state.sync();
    d.scene = editor_scene(&d.state, &d.theme, d.dpi);
    // SAFETY: invalidation of our own window.
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
    schedule_caption(hwnd);
}

extern "system" fn dlg_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: lParam is this message's CREATESTRUCTW; the param is our boxed state.
        unsafe {
            let cs = &*(l.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, w, l);
        }
    }
    let ptr = dlg_ptr(hwnd);
    if ptr.is_null() {
        // SAFETY: default processing before the state exists.
        return unsafe { DefWindowProcW(hwnd, msg, w, l) };
    }
    // SAFETY: the box is alive for the whole modal loop.
    let d = unsafe { &mut *ptr };
    match msg {
        WM_PAINT => {
            paint(hwnd, d);
            return LRESULT(0);
        }
        WM_ERASEBKGND => return LRESULT(1),
        WM_NCHITTEST => {
            let mut p = POINT {
                x: (l.0 as i16) as i32,
                y: ((l.0 >> 16) as i16) as i32,
            };
            // SAFETY: coordinate conversion of a local point.
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut p);
            }
            let caption = p.y < scf(60., d.dpi) as i32;
            return LRESULT(if caption {
                HTCAPTION as isize
            } else {
                HTCLIENT as isize
            });
        }
        WM_COMMAND => {
            if (w.0 >> 16) & 0xffff == 0x300 {
                on_edit_changed(hwnd, HWND(l.0 as *mut _));
            }
            return LRESULT(0);
        }
        WM_CTLCOLOREDIT => {
            let tokens = Tokens::new(d.theme.light);
            // SAFETY: the DC belongs to this message; the brush is owned by the dialog.
            unsafe {
                SetTextColor(HDC(w.0 as *mut _), colorref(tokens.text));
                SetBkColor(HDC(w.0 as *mut _), colorref(tokens.control));
            }
            return LRESULT(d.brush.0 as isize);
        }
        WM_TIMER if w.0 == TIMER_CAPTION => {
            // SAFETY: plain timer call.
            unsafe {
                let _ = KillTimer(hwnd, TIMER_CAPTION);
            }
            refresh_caption(hwnd);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let p = ((l.0 as i16) as i32, ((l.0 >> 16) as i16) as i32);
            let hover = region_at(&d.scene, p);
            if hover != d.hover {
                d.hover = hover;
                // SAFETY: invalidation of our own window.
                unsafe {
                    let _ = InvalidateRect(hwnd, None, false);
                }
            }
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let p = ((l.0 as i16) as i32, ((l.0 >> 16) as i16) as i32);
            if let Some(i) = region_at(&d.scene, p) {
                d.focus = Some(FocusTarget::Region(i));
                if d.scene.regions[i].enabled {
                    on_act(hwnd, d.scene.regions[i].act);
                }
            }
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            let last = d.state.forms.len().saturating_sub(d.state.visible);
            if ((w.0 >> 16) as i16) > 0 {
                d.state.scroll = d.state.scroll.saturating_sub(1);
            } else {
                d.state.scroll = (d.state.scroll + 1).min(last);
            }
            rebuild(hwnd, true);
            return LRESULT(0);
        }
        WM_DPICHANGED => {
            d.dpi = (w.0 & 0xffff) as u32;
            d.surface = None;
            let font = make_font(d.dpi);
            // SAFETY: the old font is owned by the dialog and no longer selected anywhere.
            unsafe {
                let _ = DeleteObject(HGDIOBJ(d.font.0));
            }
            d.font = font;
            // SAFETY: lParam points to the suggested rectangle for this message.
            let r = unsafe { &*(l.0 as *const RECT) };
            let center = POINT {
                x: (r.left + r.right) / 2,
                y: (r.top + r.bottom) / 2,
            };
            let (work, _) = crate::dialog::monitor_at(center);
            configure_viewport(
                d,
                ((work.bottom - work.top) as f32 * 96. / d.dpi as f32 - 48.).max(280.),
            );
            rebuild(hwnd, true);
            let (ow, oh) = outer_size(d.scene.size, d.dpi);
            let pos = crate::dialog::opening_bounds(
                work,
                d.dpi,
                ow as f32 * 96. / d.dpi as f32,
                oh as f32 * 96. / d.dpi as f32,
                center,
            );
            // SAFETY: repositions our own window.
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    pos.left,
                    pos.top,
                    ow,
                    oh,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            return LRESULT(0);
        }
        WM_CLOSE => {
            d.done = true;
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            // SAFETY: detaches the state pointer before the window goes away.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
        }
        _ => {}
    }
    // SAFETY: default processing.
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}

/// Run the editor modally over `owner`; returns the finished state and whether it was accepted.
fn run(owner: HWND, state: EditorState, theme: &Theme) -> (EditorState, bool) {
    // SAFETY: the usual window creation and message loop; the state box outlives the window.
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)
            .map(|m| HINSTANCE(m.0))
            .unwrap_or_default();
        register_class(instance);
        let mut r = RECT::default();
        let _ = GetWindowRect(owner, &mut r);
        let center = POINT {
            x: (r.left + r.right) / 2,
            y: (r.top + r.bottom) / 2,
        };
        let (work, dpi) = crate::dialog::monitor_at(center);
        let tokens = Tokens::new(theme.light);
        let mut d = Box::new(Dlg {
            theme: *theme,
            dpi,
            surface: None,
            state,
            scene: Scene::default(),
            hover: None,
            edits: Vec::new(),
            font: make_font(dpi),
            brush: CreateSolidBrush(colorref(tokens.control)),
            syncing: false,
            done: false,
            accepted: false,
            focus: None,
            keyboard_visible: false,
        });
        configure_viewport(
            &mut d,
            ((work.bottom - work.top) as f32 * 96. / dpi as f32 - 48.).max(280.),
        );
        d.scene = editor_scene(&d.state, theme, dpi);
        let (ow, oh) = outer_size(d.scene.size, dpi);
        let pos = crate::dialog::opening_bounds(
            work,
            dpi,
            ow as f32 * 96. / dpi as f32,
            oh as f32 * 96. / dpi as f32,
            center,
        );
        let raw = Box::into_raw(d);
        let created = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            w!("CorralRulesDialog"),
            w!("Round-up rule"),
            STYLE,
            pos.left,
            pos.top,
            ow,
            oh,
            owner,
            HMENU::default(),
            instance,
            Some(raw as *const _),
        );
        let Ok(hwnd) = created else {
            let d = Box::from_raw(raw);
            let _ = DeleteObject(HGDIOBJ(d.font.0));
            let _ = DeleteObject(HGDIOBJ(d.brush.0));
            return (d.state, false);
        };
        let dark = i32::from(!theme.light);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWINDOWATTRIBUTE(20),
            &dark as *const _ as *const _,
            4,
        );
        rebuild(hwnd, true);
        let owner_enabled = IsWindowEnabled(owner).as_bool();
        if owner_enabled {
            let _ = EnableWindow(owner, false);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        advance_focus(hwnd, false);
        schedule_caption(hwnd);
        let mut msg = MSG::default();
        while !(*raw).done && GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let ours = msg.hwnd == hwnd || IsChild(hwnd, msg.hwnd).as_bool();
            if (msg.message == WM_KEYDOWN || msg.message == WM_SYSKEYDOWN) && ours {
                let key = msg.wParam.0 as u16;
                if key == VK_ESCAPE.0 {
                    (*raw).done = true;
                    continue;
                }
                if key == VK_TAB.0 {
                    advance_focus(hwnd, GetKeyState(VK_SHIFT.0 as i32) < 0);
                    continue;
                }
                let activates = key == VK_SPACE.0
                    || key == VK_RETURN.0
                    || (key == VK_DOWN.0 && GetKeyState(VK_MENU.0 as i32) < 0);
                if msg.hwnd == hwnd && activates {
                    if let Some(FocusTarget::Region(i)) = (*raw).focus {
                        let region = (&*raw).scene.regions.get(i).copied().filter(|r| r.enabled);
                        if let Some(region) = region {
                            on_act(hwnd, region.act);
                            continue;
                        }
                    }
                }
                if key == VK_RETURN.0 {
                    on_act(hwnd, Act::Save);
                    continue;
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if owner_enabled {
            let _ = EnableWindow(owner, true);
        }
        let _ = DestroyWindow(hwnd);
        let _ = SetForegroundWindow(owner);
        let d = Box::from_raw(raw);
        let _ = DeleteObject(HGDIOBJ(d.font.0));
        let _ = DeleteObject(HGDIOBJ(d.brush.0));
        (d.state, d.accepted)
    }
}

pub enum EditOutcome {
    Ok(Rule),
    Delete,
    Cancel,
}

/// Edit `rule`. `above` are the rules ahead of it in the list (for the live match line); a new rule
/// has no Delete link.
pub fn edit_rule(
    owner: HWND,
    rule: &Rule,
    is_new: bool,
    groups: &[GroupInfo],
    above: &[Rule],
    theme: &Theme,
) -> EditOutcome {
    let (state, accepted) = run(owner, EditorState::new(rule, is_new, groups, above), theme);
    if !accepted {
        EditOutcome::Cancel
    } else if state.deleted {
        EditOutcome::Delete
    } else {
        let mut rule = state.rule;
        rule.conditions = state.forms.into_iter().map(|f| f.cond).collect();
        EditOutcome::Ok(rule)
    }
}

/// "Rename…": the rule name alone, with "Reset to default" returning to the generated name.
pub fn rename_rule(owner: HWND, rule: &mut Rule, theme: &Theme) -> bool {
    let mut spec = crate::dialog::DialogSpec::new("Rename rule", "", "Save");
    spec.field = Some(crate::dialog::Field {
        label: "Rule name".into(),
        value: rule.name.clone(),
        hint: "Leave it empty to name the rule automatically.".into(),
        required: false,
        max_chars: 255,
    });
    spec.left_action = Some("Reset to default".into());
    let result = crate::dialog::show(owner, theme.light, spec);
    match result.action {
        crate::dialog::Action::Primary => {
            rule.name = result.text.trim().into();
            true
        }
        crate::dialog::Action::Left => {
            rule.name = rule
                .preset
                .as_deref()
                .and_then(crate::round_up::preset_name)
                .unwrap_or_default()
                .to_string();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule_with(conditions: Vec<Condition>) -> Rule {
        let mut rule = Rule {
            id: "r-1".into(),
            name: String::new(),
            enabled: true,
            preset: None,
            match_mode: MatchMode::All,
            conditions,
            destination: Some(Destination::Desktop),
            legacy: serde_json::Value::Null,
        };
        rule.match_mode = MatchMode::All;
        rule
    }

    fn all_kinds() -> Vec<Condition> {
        (0..8).map(default_condition).collect()
    }

    #[test]
    fn every_kind_roundtrips_through_its_form_and_operators() {
        for (index, condition) in all_kinds().into_iter().enumerate() {
            assert_eq!(kind_index(&condition), Some(index));
            let form = Form::new(condition.clone());
            assert_eq!(form.cond, condition);
            for op in 0..op_labels(&condition).len() {
                let mut c = condition.clone();
                set_op(&mut c, op);
                assert_eq!(op_index(&c), op, "{index} {op}");
            }
        }
    }

    #[test]
    fn typed_values_become_conditions_and_bad_input_blocks_saving() {
        let mut ext = Form::new(default_condition(1));
        assert!(ext.error().is_some());
        ext.set_text(".PDF, docx;txt".into());
        assert_eq!(
            ext.cond,
            Condition::Extension {
                op: ExtensionOp::IsOneOf,
                value: vec!["PDF".into(), "docx".into(), "txt".into()]
            }
        );
        assert!(ext.error().is_none());
        let mut name = Form::new(default_condition(2));
        assert!(name.error().is_some());
        name.set_text("invoice".into());
        assert!(name.error().is_none());
        let mut size = Form::new(default_condition(3));
        for bad in ["", "ten", "-1", "1.5"] {
            size.set_text(bad.into());
            assert!(size.error().is_some(), "{bad:?}");
        }
        size.set_text("25".into());
        assert_eq!(
            size.cond,
            Condition::Size {
                op: SizeOp::AtLeast,
                value: SizeValue {
                    n: 25,
                    unit: SizeUnit::MB
                }
            }
        );
        let mut date = Form::new(default_condition(4));
        date.set_text("0".into());
        assert!(date.error().is_some());
    }

    #[test]
    fn unknown_conditions_have_no_kind_and_stay_as_they_are() {
        let raw = serde_json::json!({"type": "future", "op": "x", "value": 1});
        let unknown = Condition::Unknown { raw: raw.clone() };
        assert_eq!(kind_index(&unknown), None);
        let form = Form::new(unknown);
        assert!(form.error().is_some());
        assert_eq!(serde_json::to_value(&form.cond).unwrap(), raw);
    }

    #[test]
    fn editor_has_all_any_and_no_priority_surface_and_fits_at_every_dpi() {
        let rule = rule_with(vec![default_condition(1), default_condition(2)]);
        let state = EditorState::new(&rule, true, &[], &[]);
        for dpi in [96, 120, 144, 240] {
            let scene = editor_scene(&state, &Theme::dark(), dpi);
            assert!(scene.regions.iter().any(|r| r.act == Act::MatchAll));
            assert!(scene.regions.iter().any(|r| r.act == Act::MatchAny));
            for r in &scene.regions {
                assert!(
                    r.rect.left >= 0.
                        && r.rect.right <= scene.size.0 as f32
                        && r.rect.bottom <= scene.size.1 as f32
                );
            }
            assert!(!scene.draws.iter().any(
                |d| matches!(d, Draw::Text { text, .. } if text.to_lowercase().contains("priority"))
            ));
        }
    }

    #[test]
    fn one_condition_hides_the_switch_and_the_remove_button() {
        let rule = rule_with(vec![default_condition(2)]);
        let state = EditorState::new(&rule, true, &[], &[]);
        let scene = editor_scene(&state, &Theme::light(), 96);
        assert!(
            !scene
                .regions
                .iter()
                .any(|r| matches!(r.act, Act::MatchAll | Act::MatchAny | Act::Remove(_)))
        );
        assert!(scene.draws.iter().any(
            |d| matches!(d, Draw::Text { text, .. } if text == "An icon matches when this is true:")
        ));
    }

    #[test]
    fn row_labels_follow_the_switch() {
        let mut rule = rule_with(vec![
            default_condition(2),
            default_condition(2),
            default_condition(2),
        ]);
        let joined = |rule: &Rule| -> Vec<String> {
            let state = EditorState::new(rule, true, &[], &[]);
            editor_scene(&state, &Theme::dark(), 96)
                .draws
                .into_iter()
                .filter_map(|d| match d {
                    Draw::Text { text, .. } if ["Where", "and", "or"].contains(&text.as_str()) => {
                        Some(text)
                    }
                    _ => None,
                })
                .collect()
        };
        assert_eq!(joined(&rule), ["Where", "and", "and"]);
        rule.match_mode = MatchMode::Any;
        assert_eq!(joined(&rule), ["Where", "or", "or"]);
    }

    #[test]
    fn save_is_disabled_until_the_rule_is_complete() {
        let mut rule = rule_with(vec![default_condition(2)]);
        rule.destination = None;
        let mut state = EditorState::new(&rule, true, &[], &[]);
        let save = |state: &EditorState| {
            editor_scene(state, &Theme::dark(), 96)
                .regions
                .iter()
                .find(|r| r.act == Act::Save)
                .map(|r| r.enabled)
        };
        assert_eq!(save(&state), Some(false));
        state.forms[0].set_text("invoice".into());
        assert_eq!(save(&state), Some(false), "still no destination");
        state.rule.destination = Some(Destination::Desktop);
        state.sync();
        assert_eq!(save(&state), Some(true));
    }

    #[test]
    fn many_conditions_scroll_and_every_stop_stays_reachable() {
        let rule = rule_with((0..9).map(|_| default_condition(2)).collect());
        let mut state = EditorState::new(&rule, false, &[], &[]);
        state.visible = 3;
        let scene = editor_scene(&state, &Theme::dark(), 96);
        let kinds = scene
            .regions
            .iter()
            .filter(|r| matches!(r.act, Act::Kind(_)))
            .count();
        assert_eq!(kinds, 3);
        let mut all = EditorState::new(&rule, false, &[], &[]);
        all.visible = 9;
        let scene = editor_scene(&all, &Theme::dark(), 96);
        assert_eq!(
            scene
                .regions
                .iter()
                .filter(|r| matches!(r.act, Act::Kind(_)))
                .count(),
            9
        );
        assert!(focus_order(&scene).len() > 9);
    }
}
