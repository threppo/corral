//! The Round-up page: pure dynamic layout, painting, focus order and hit-testing
//! (round-up-spec 5; geometry follows the U4 settings kit).
//!
//! Cards in order: New icons, Needs review (only when non-empty), Your rules (one ordered list,
//! built-in rules included), a warning box when needed, Running. Rule actions always carry the
//! rule's index in the stored list, so the list's order is the evaluation order.

use windows::core::Result;

use super::kit::{
    controls::{self, ButtonStyle, Control, VisualState},
    theme::Theme as Tokens,
};
use super::{CONTROL_POINTER, FOCUS_RING, Header, Theme, UI, page_header, scf, style};
use crate::render::{Align, Ctx, RectF, Rgba, measure_text};
use crate::round_up::{Destination, Rule};
use crate::settings::Settings;

/// A Corral or Gate of the current layout as the pickers need it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupInfo {
    pub id: i32,
    pub title: String,
    /// Folder Gate: never offered as a destination.
    pub portal: bool,
    pub items: usize,
}

impl GroupInfo {
    /// Picker text: the title, or "Unnamed Corral with N icons" for a blank one.
    pub fn picker_text(&self) -> String {
        if self.title.trim().is_empty() {
            format!(
                "Unnamed Corral with {} icon{}",
                self.items,
                if self.items == 1 { "" } else { "s" }
            )
        } else {
            self.title.clone()
        }
    }

    pub fn link_text(&self) -> String {
        if self.title.trim().is_empty() {
            "Unnamed Corral".to_string()
        } else {
            self.title.clone()
        }
    }
}

/// Corrals a rule may send icons to: no Gates, sorted by title (case-insensitive).
pub fn pickable(groups: &[GroupInfo]) -> Vec<&GroupInfo> {
    let mut list: Vec<&GroupInfo> = groups.iter().filter(|g| !g.portal).collect();
    list.sort_by_key(|g| g.picker_text().to_lowercase());
    list
}

fn find(groups: &[GroupInfo], id: u64) -> Option<&GroupInfo> {
    groups.iter().find(|g| g.id as u64 == id && !g.portal)
}

pub fn corral_exists(groups: &[GroupInfo], id: u64) -> bool {
    find(groups, id).is_some()
}

/// Destination as chips and sentences name it.
pub fn destination_text(destination: Option<&Destination>, groups: &[GroupInfo]) -> String {
    match destination {
        Some(Destination::Desktop) => "Leave on the desktop".into(),
        Some(Destination::Corral { id }) => find(groups, *id)
            .map(GroupInfo::link_text)
            .unwrap_or_else(|| "Corral missing".into()),
        None => "Choose a Corral".into(),
    }
}

/// The name of a destination inside a sentence ("put it in ...").
pub fn sentence_destination(destination: Option<&Destination>, groups: &[GroupInfo]) -> String {
    match destination {
        Some(Destination::Corral { id }) => find(groups, *id)
            .map(GroupInfo::link_text)
            .unwrap_or_else(|| "a Corral that no longer exists".into()),
        _ => "a Corral you choose".into(),
    }
}

/// Text of the "When no rule matches" dropdown.
pub fn default_dest_text(settings: &Settings, groups: &[GroupInfo]) -> String {
    match &settings.round_up.default_destination {
        Destination::Desktop => "Leave it on the desktop".into(),
        other => destination_text(Some(other), groups),
    }
}

/// A rule is suspended while its Corral does not exist (round-up-spec 3).
pub fn suspended(rule: &Rule, groups: &[GroupInfo]) -> bool {
    matches!(&rule.destination, Some(Destination::Corral { id }) if !corral_exists(groups, *id))
}

thread_local! {
    /// Icons the last check could not evaluate; shown in the warning box (round-up-spec 5).
    static UNCHECKED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub fn set_unchecked(count: usize) {
    UNCHECKED.with(|c| c.set(count));
}

/// Actions always use indices into the stored list, so visual grouping can never change priority.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    DefaultDest { anchor: (i32, i32) },
    ToggleRule(usize),
    EditRule(usize),
    RuleDest { index: usize, anchor: (i32, i32) },
    AddRule { anchor: (i32, i32) },
    RuleContext { index: usize, anchor: (i32, i32) },
    MoveRule { index: usize, up: bool },
    RewriteReview(usize),
    DismissReview(usize),
    ApplyNow,
    KeepApplied,
}

struct Row {
    rect: RectF,
    kind: Kind,
}

enum Kind {
    Default {
        control: RectF,
        dest: String,
        stacked: bool,
    },
    Review {
        name: String,
        lines: Vec<String>,
        buttons: [RectF; 2],
    },
    Rule {
        checked: bool,
        dim: bool,
        chip: RectF,
        dest: String,
        danger: bool,
        up: RectF,
        down: RectF,
        label_lines: Vec<String>,
        caption_lines: Vec<String>,
    },
    Add,
    Keep(bool),
    Run {
        control: RectF,
    },
}

struct Section {
    label: &'static str,
    label_rect: RectF,
    card: RectF,
    rows: Vec<Row>,
}

struct Notice {
    rect: RectF,
    title: String,
    lines: Vec<String>,
}

enum Block {
    Section(Section),
    Notice(Notice),
}

pub struct Lay {
    header: Header,
    blocks: Vec<Block>,
    hits: Vec<(RectF, Action)>,
    contexts: Vec<(RectF, usize)>,
    pub bottom: f32,
}

const TITLE: &str = "Round-up";
const SUBTITLE: &str = "Send new desktop icons to the right Corral automatically.";
const WARNING_GLYPH: &str = "\u{E7BA}";

fn anchor(r: RectF) -> (i32, i32) {
    (r.left.round() as i32, r.bottom.round() as i32)
}

fn inside(r: RectF, p: (i32, i32)) -> bool {
    p.0 as f32 >= r.left && (p.0 as f32) < r.right && p.1 as f32 >= r.top && (p.1 as f32) < r.bottom
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Word wrap that also breaks a single over-long token (a file name, a stored rule string).
fn wrap_all(text: &str, size: f32, weight: u16, width: f32, dpi: u32) -> Result<Vec<String>> {
    let size = scf(size, dpi);
    let mut result = Vec::new();
    for line in super::wrap(text, size, weight, width)? {
        let mut rest = line.as_str();
        while !rest.is_empty() {
            if measure_text(rest, UI, size, weight)?.0 <= width {
                result.push(rest.to_string());
                break;
            }
            let mut end = 0;
            for (i, c) in rest.char_indices() {
                let next = i + c.len_utf8();
                if end > 0 && measure_text(&rest[..next], UI, size, weight)?.0 > width {
                    break;
                }
                end = next;
            }
            result.push(rest[..end].trim_end().to_string());
            rest = rest[end..].trim_start();
        }
    }
    if result.is_empty() {
        result.push(String::new());
    }
    Ok(result)
}

pub fn layout(
    settings: &Settings,
    groups: &[GroupInfo],
    dpi: u32,
    width: f32,
    height: f32,
    scroll: i32,
) -> Result<Lay> {
    let lay = layout_with(settings, groups, dpi, width, scroll, false)?;
    if super::page_scrolls(lay.bottom + scroll as f32, height) {
        layout_with(settings, groups, dpi, width, scroll, true)
    } else {
        Ok(lay)
    }
}

struct Cursor<'a> {
    x: f32,
    right: f32,
    y: f32,
    dpi: u32,
    hits: &'a mut Vec<(RectF, Action)>,
}

fn layout_with(
    settings: &Settings,
    groups: &[GroupInfo],
    dpi: u32,
    width: f32,
    scroll: i32,
    scrolls: bool,
) -> Result<Lay> {
    let d = |v: f32| scf(v, dpi);
    let header = page_header(TITLE, SUBTITLE, dpi, width, scroll, scrolls)?;
    let (x, right) = (header.x0, header.right);
    let mut hits = Vec::new();
    let mut contexts = Vec::new();
    let mut blocks = Vec::new();
    let mut c = Cursor {
        x,
        right,
        y: header.y,
        dpi,
        hits: &mut hits,
    };
    let round_up = &settings.round_up;
    let narrow = (right - x) / d(1.) < 520.;

    if let Some(reason) = &round_up.paused_reason {
        blocks.push(Block::Notice(notice(&mut c, "Round-up is paused", reason)?));
    } else {
        blocks.push(Block::Section(default_section(
            &mut c, settings, groups, narrow,
        )));
        if !round_up.needs_review.is_empty() {
            blocks.push(Block::Section(review_section(&mut c, settings, narrow)?));
        }
        blocks.push(Block::Section(rules_section(
            &mut c,
            settings,
            groups,
            narrow,
            &mut contexts,
        )?));
        let missing = round_up.missing_destinations(|id| corral_exists(groups, id));
        let unchecked = UNCHECKED.with(|u| u.get());
        let mut messages = Vec::new();
        if missing > 0 {
            messages.push(format!(
                "{} to a Corral that no longer exists. They wait until the Corral is back or you choose another one.",
                plural(missing, "rule points", "rules point")
            ));
        }
        if unchecked > 0 {
            messages.push(format!(
                "{} could not be checked. {} where {} {}.",
                plural(unchecked, "icon", "icons"),
                if unchecked == 1 {
                    "It stays"
                } else {
                    "They stay"
                },
                if unchecked == 1 { "it" } else { "they" },
                if unchecked == 1 { "is" } else { "are" },
            ));
        }
        if !messages.is_empty() {
            let title = if missing > 0 {
                "Some Corrals are missing"
            } else {
                "Some icons could not be checked"
            };
            blocks.push(Block::Notice(notice(&mut c, title, &messages.join(" "))?));
        }
        blocks.push(Block::Section(running_section(&mut c, settings, narrow)?));
    }
    let bottom = c.y + d(40.);
    Ok(Lay {
        header,
        blocks,
        hits,
        contexts,
        bottom,
    })
}

fn begin(c: &mut Cursor<'_>) -> (RectF, f32) {
    let dpi = c.dpi;
    let d = |v: f32| scf(v, dpi);
    let label_rect = RectF::new(c.x, c.y + d(20.), c.right, c.y + d(40.));
    c.y += d(48.);
    let top = c.y;
    c.y += d(1.);
    (label_rect, top)
}

fn finish(
    c: &mut Cursor<'_>,
    label: &'static str,
    label_rect: RectF,
    top: f32,
    rows: Vec<Row>,
) -> Section {
    c.y += scf(1., c.dpi);
    Section {
        label,
        label_rect,
        card: RectF::new(c.x, top, c.right, c.y),
        rows,
    }
}

fn notice(c: &mut Cursor<'_>, title: &str, body: &str) -> Result<Notice> {
    let dpi = c.dpi;
    let d = |v: f32| scf(v, dpi);
    c.y += d(20.);
    let text_w = (c.right - c.x - d(16. + 28. + 16.)).max(d(40.));
    let lines = wrap_all(body, 12., 400, text_w, c.dpi)?;
    let h = d(12. + 20. + 12.) + d(16.) * lines.len() as f32;
    let rect = RectF::new(c.x, c.y, c.right, c.y + h);
    c.y += h;
    Ok(Notice {
        rect,
        title: title.into(),
        lines,
    })
}

fn default_section(
    c: &mut Cursor<'_>,
    settings: &Settings,
    groups: &[GroupInfo],
    narrow: bool,
) -> Section {
    let dpi = c.dpi;
    let d = |v: f32| scf(v, dpi);
    let (label_rect, top) = begin(c);
    let h = d(if narrow { 96. } else { 58. });
    let rect = RectF::new(c.x + d(1.), c.y, c.right - d(1.), c.y + h);
    let control = RectF::new(
        c.right - d(237.),
        if narrow { c.y + d(54.) } else { c.y + d(13.) },
        c.right - d(17.),
        if narrow { c.y + d(86.) } else { c.y + d(45.) },
    );
    c.hits.push((
        control,
        Action::DefaultDest {
            anchor: anchor(control),
        },
    ));
    let row = Row {
        rect,
        kind: Kind::Default {
            control,
            dest: default_dest_text(settings, groups),
            stacked: narrow,
        },
    };
    c.y += h;
    finish(c, "New icons", label_rect, top, vec![row])
}

fn review_section(c: &mut Cursor<'_>, settings: &Settings, narrow: bool) -> Result<Section> {
    let dpi = c.dpi;
    let d = |v: f32| scf(v, dpi);
    let (label_rect, top) = begin(c);
    let mut rows = Vec::new();
    for (index, item) in settings.round_up.needs_review.iter().enumerate() {
        let buttons_w = d(90. + 8. + 78.);
        let text_w = if narrow {
            c.right - c.x - d(34.)
        } else {
            c.right - c.x - d(34.) - buttons_w - d(16.)
        };
        let mut lines = wrap_all(&item.data, 12., 400, text_w.max(d(40.)), c.dpi)?;
        lines.extend(wrap_all(&item.reason, 12., 400, text_w.max(d(40.)), c.dpi)?);
        let h =
            d(12. + 20. + 12.) + d(16.) * lines.len() as f32 + if narrow { d(44.) } else { d(0.) };
        let h = h.max(d(64.));
        let rect = RectF::new(c.x + d(1.), c.y, c.right - d(1.), c.y + h);
        let by = if narrow {
            rect.bottom - d(44.)
        } else {
            rect.top + (h - d(32.)) / 2.
        };
        let rewrite = RectF::new(
            if narrow {
                c.x + d(17.)
            } else {
                c.right - d(17.) - buttons_w
            },
            by,
            (if narrow {
                c.x + d(17.)
            } else {
                c.right - d(17.) - buttons_w
            }) + d(90.),
            by + d(32.),
        );
        let dismiss = RectF::new(
            rewrite.right + d(8.),
            by,
            rewrite.right + d(8. + 78.),
            by + d(32.),
        );
        c.hits.push((rewrite, Action::RewriteReview(index)));
        c.hits.push((dismiss, Action::DismissReview(index)));
        rows.push(Row {
            rect,
            kind: Kind::Review {
                name: item.name.clone(),
                lines,
                buttons: [rewrite, dismiss],
            },
        });
        c.y += h;
    }
    Ok(finish(c, "Needs review", label_rect, top, rows))
}

fn rules_section(
    c: &mut Cursor<'_>,
    settings: &Settings,
    groups: &[GroupInfo],
    narrow: bool,
    contexts: &mut Vec<(RectF, usize)>,
) -> Result<Section> {
    let dpi = c.dpi;
    let d = |v: f32| scf(v, dpi);
    let (label_rect, top) = begin(c);
    let mut rows = Vec::new();
    for (index, rule) in settings.round_up.rules.iter().enumerate() {
        let gone = suspended(rule, groups);
        let dest = if gone {
            "Corral missing".to_string()
        } else {
            destination_text(rule.destination.as_ref(), groups)
        };
        let chip_w = (measure_text(&dest, UI, d(14.), 400)?.0 + d(36.))
            .min((c.right - c.x) * 0.45)
            .max(d(40.));
        let chip_left = c.right - d(17.) - chip_w;
        // The move buttons are always reserved so a row never changes size on hover.
        let down_left = chip_left - d(8.) - d(24.);
        let up_left = down_left - d(24.);
        let text_left = c.x + d(49.);
        let text_w = (up_left - text_left - d(12.)).max(d(20.));
        let destination_name = sentence_destination(rule.destination.as_ref(), groups);
        let sentence = rule.sentence(&destination_name);
        let title = rule.title(&destination_name);
        let label_lines = wrap_all(&title, 14., 400, text_w, c.dpi)?;
        let mut caption = String::new();
        if rule.preset.is_some() {
            caption.push_str("Built in");
        }
        if rule.needs_update() {
            caption.push_str(if caption.is_empty() { "" } else { ". " });
            caption.push_str("Needs an update of Corral.");
        } else if !rule.name.trim().is_empty() {
            caption.push_str(if caption.is_empty() { "" } else { ". " });
            caption.push_str(&sentence);
        }
        let caption_lines = if caption.is_empty() {
            Vec::new()
        } else {
            wrap_all(&caption, 12., 400, text_w, c.dpi)?
        };
        let row_h = (d(16. + 20. * label_lines.len() as f32) + d(16.) * caption_lines.len() as f32)
            .max(d(44.));
        let rect = RectF::new(c.x + d(1.), c.y, c.right - d(1.), c.y + row_h);
        let chip = RectF::new(
            chip_left,
            c.y + (row_h - d(24.)) / 2.,
            c.right - d(17.),
            c.y + (row_h + d(24.)) / 2.,
        );
        let up = RectF::new(up_left, chip.top, up_left + d(24.), chip.bottom);
        let down = RectF::new(down_left, chip.top, down_left + d(24.), chip.bottom);
        c.hits.push((
            RectF::new(rect.left, rect.top, up.left - d(4.), rect.bottom),
            Action::ToggleRule(index),
        ));
        c.hits.push((up, Action::MoveRule { index, up: true }));
        c.hits.push((down, Action::MoveRule { index, up: false }));
        c.hits.push((
            chip,
            Action::RuleDest {
                index,
                anchor: anchor(chip),
            },
        ));
        contexts.push((rect, index));
        rows.push(Row {
            rect,
            kind: Kind::Rule {
                checked: rule.enabled,
                dim: gone || rule.needs_update(),
                chip,
                dest,
                danger: gone,
                up,
                down,
                label_lines,
                caption_lines,
            },
        });
        c.y += row_h;
    }
    let rect = RectF::new(c.x + d(1.), c.y, c.right - d(1.), c.y + d(44.));
    c.hits.push((
        rect,
        Action::AddRule {
            anchor: anchor(rect),
        },
    ));
    rows.push(Row {
        rect,
        kind: Kind::Add,
    });
    c.y += d(44.);
    let _ = narrow;
    Ok(finish(c, "Your rules", label_rect, top, rows))
}

fn running_section(c: &mut Cursor<'_>, settings: &Settings, narrow: bool) -> Result<Section> {
    let dpi = c.dpi;
    let d = |v: f32| scf(v, dpi);
    let _ = narrow;
    let (label_rect, top) = begin(c);
    let mut rows = Vec::new();
    for n in 0..2 {
        let desc = if n == 0 { KEEP_DESC } else { RUN_DESC };
        let text_w = (c.right - c.x - d(if n == 1 { 128. } else { 104. })).max(d(80.));
        let lines = super::wrap(desc, d(12.), 400, text_w)?.len().max(1);
        let h = (d(38.) + d(16.) * lines as f32 + d(4.)).max(d(58.));
        let rect = RectF::new(c.x + d(1.), c.y, c.right - d(1.), c.y + h);
        let kind = if n == 0 {
            c.hits.push((rect, Action::KeepApplied));
            Kind::Keep(settings.always_apply_rules)
        } else {
            let control = RectF::new(
                c.right - d(97.),
                c.y + (h - d(32.)) / 2.,
                c.right - d(17.),
                c.y + (h + d(32.)) / 2.,
            );
            c.hits.push((control, Action::ApplyNow));
            Kind::Run { control }
        };
        rows.push(Row { rect, kind });
        c.y += h;
    }
    Ok(finish(c, "Running", label_rect, top, rows))
}

const KEEP_DESC: &str = "Check icons you did not place yourself when they change. Changing a rule never sorts the desktop.";
const RUN_DESC: &str =
    "See what would move first, then decide. Icons you placed yourself stay put.";

pub fn content_height(
    settings: &Settings,
    groups: &[GroupInfo],
    dpi: u32,
    width: i32,
    height: i32,
) -> Result<i32> {
    Ok(
        layout(settings, groups, dpi, width as f32, height as f32, 0)?
            .bottom
            .ceil() as i32,
    )
}

pub fn hit(lay: &Lay, point: (i32, i32)) -> Option<Action> {
    lay.hits
        .iter()
        .rev()
        .find(|(r, _)| inside(*r, point))
        .map(|(_, a)| *a)
}

pub fn context_at(lay: &Lay, point: (i32, i32)) -> Option<Action> {
    lay.contexts
        .iter()
        .find(|(r, _)| inside(*r, point))
        .map(|(r, index)| Action::RuleContext {
            index: *index,
            anchor: anchor(*r),
        })
}

/// Keyboard stops: everything but the mouse-only move buttons (Alt+Up / Alt+Down move the row).
pub fn focus_targets(lay: &Lay) -> Vec<(RectF, Action)> {
    lay.hits
        .iter()
        .filter(|(_, a)| !matches!(a, Action::MoveRule { .. }))
        .cloned()
        .collect()
}

fn pointer_state(r: RectF, selected: bool) -> VisualState {
    let pointer = CONTROL_POINTER.with(|p| p.get());
    let hovered =
        pointer.is_some_and(|(x, y, _)| x >= r.left && x < r.right && y >= r.top && y < r.bottom);
    let pressed = hovered && pointer.is_some_and(|(_, _, pressed)| pressed);
    VisualState {
        selected,
        hovered,
        pressed,
        ..Default::default()
    }
}

fn fitted(text: &str, width: f32, dpi: u32) -> String {
    if measure_text(text, UI, scf(14., dpi), 400).is_ok_and(|m| m.0 <= width) {
        return text.into();
    }
    let mut s = text.to_owned();
    while !s.is_empty() {
        s.pop();
        let candidate = format!("{s}\u{2026}");
        if measure_text(&candidate, UI, scf(14., dpi), 400).is_ok_and(|m| m.0 <= width) {
            return candidate;
        }
    }
    String::new()
}

pub fn draw_check(
    ctx: &Ctx,
    r: RectF,
    checked: bool,
    theme: &Theme,
    dpi: u32,
    alpha: u8,
) -> Result<()> {
    let z = 96. / dpi as f32;
    controls::draw_control(
        ctx,
        Control::Checkbox,
        RectF::new(r.left * z, r.top * z, r.right * z, r.bottom * z),
        "",
        VisualState {
            enabled: alpha == 255,
            ..pointer_state(r, checked)
        },
        &Tokens::new(theme.light),
        dpi,
        0.,
    )
}

fn control(
    ctx: &Ctx,
    kind: Control,
    r: RectF,
    label: &str,
    selected: bool,
    t: &Tokens,
    dpi: u32,
) -> Result<()> {
    let z = 96. / dpi as f32;
    controls::draw_control(
        ctx,
        kind,
        RectF::new(r.left * z, r.top * z, r.right * z, r.bottom * z),
        label,
        pointer_state(r, selected),
        t,
        dpi,
        0.,
    )
}

pub fn draw(ctx: &Ctx, lay: &Lay, theme: &Theme, dpi: u32) -> Result<()> {
    draw_focused(ctx, lay, theme, dpi, None)
}

/// `focused` is an index into [`focus_targets`].
pub fn draw_focused(
    ctx: &Ctx,
    lay: &Lay,
    theme: &Theme,
    dpi: u32,
    focused: Option<usize>,
) -> Result<()> {
    let t = Tokens::new(theme.light);
    let d = |v: f32| scf(v, dpi);
    let text = |s: &str, r: RectF, size: f32, strong: bool, color: Rgba| {
        ctx.draw_text(
            s,
            r,
            &style(
                UI,
                d(size),
                if strong { 600 } else { 400 },
                color,
                Align::Left,
            ),
        )
    };
    text(TITLE, lay.header.title_rect, 28., true, t.text)?;
    for (line, r) in &lay.header.subtitle {
        text(line, *r, 14., false, t.secondary)?;
    }
    let ring = FOCUS_RING.with(|f| f.get());
    for block in &lay.blocks {
        match block {
            Block::Notice(n) => draw_notice(ctx, n, &t, dpi)?,
            Block::Section(sec) => {
                text(sec.label, sec.label_rect, 14., true, t.text)?;
                ctx.fill_rounded_rect(sec.card, d(8.), t.surface)?;
                ctx.stroke_rounded_rect(sec.card, d(8.), d(1.), t.border)?;
                for (n, row) in sec.rows.iter().enumerate() {
                    draw_row(ctx, sec, n, row, theme, &t, dpi, ring)?;
                }
            }
        }
    }
    if let Some((r, _)) = focused.and_then(|i| focus_targets(lay).into_iter().nth(i)) {
        ctx.stroke_rounded_rect(r, d(4.), d(2.), t.focus)?;
    }
    Ok(())
}

fn draw_notice(ctx: &Ctx, n: &Notice, t: &Tokens, dpi: u32) -> Result<()> {
    let d = |v: f32| scf(v, dpi);
    control(ctx, Control::WarningBox, n.rect, "", false, t, dpi)?;
    ctx.draw_text(
        WARNING_GLYPH,
        RectF::new(
            n.rect.left + d(16.),
            n.rect.top + d(12.),
            n.rect.left + d(32.),
            n.rect.top + d(32.),
        ),
        &style(super::icon_family(), d(16.), 400, t.accent, Align::Center),
    )?;
    let left = n.rect.left + d(44.);
    ctx.draw_text(
        &n.title,
        RectF::new(
            left,
            n.rect.top + d(12.),
            n.rect.right - d(16.),
            n.rect.top + d(32.),
        ),
        &style(UI, d(14.), 600, t.text, Align::Left),
    )?;
    for (i, line) in n.lines.iter().enumerate() {
        let top = n.rect.top + d(32. + 16. * i as f32);
        ctx.draw_text(
            line,
            RectF::new(left, top, n.rect.right - d(16.), top + d(16.)),
            &style(UI, d(12.), 400, t.secondary, Align::Left),
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn draw_row(
    ctx: &Ctx,
    sec: &Section,
    n: usize,
    row: &Row,
    theme: &Theme,
    t: &Tokens,
    dpi: u32,
    ring: Option<RectF>,
) -> Result<()> {
    let d = |v: f32| scf(v, dpi);
    let text = |s: &str, r: RectF, size: f32, strong: bool, color: Rgba| {
        ctx.draw_text(
            s,
            r,
            &style(
                UI,
                d(size),
                if strong { 600 } else { 400 },
                color,
                Align::Left,
            ),
        )
    };
    let r = row.rect;
    let left = r.left + d(16.);
    let right = r.right - d(16.);
    if n > 0 {
        ctx.fill_rect(
            RectF::new(left, r.top, right, r.top + d(1.)),
            t.border_subtle,
        )?;
    }
    let hovered = pointer_state(r, false).hovered;
    let row_hover = hovered
        && match &row.kind {
            Kind::Rule { chip, up, down, .. } => {
                !pointer_state(*chip, false).hovered
                    && !pointer_state(*up, false).hovered
                    && !pointer_state(*down, false).hovered
            }
            Kind::Add | Kind::Keep(_) => true,
            _ => false,
        };
    if row_hover {
        ctx.with_clip(r, |ctx| {
            ctx.fill_rounded_rect(
                RectF::new(
                    sec.card.left + d(1.),
                    sec.card.top + d(1.),
                    sec.card.right - d(1.),
                    sec.card.bottom - d(1.),
                ),
                d(7.),
                t.subtle_hover,
            )
        })?;
    }
    match &row.kind {
        Kind::Rule {
            checked,
            dim,
            chip,
            dest,
            danger,
            up,
            down,
            label_lines,
            caption_lines,
        } => {
            let cy = (r.top + r.bottom) / 2.;
            draw_check(
                ctx,
                RectF::new(left, cy - d(10.), left + d(20.), cy + d(10.)),
                *checked,
                theme,
                dpi,
                255,
            )?;
            let block_h = d(20.) * label_lines.len() as f32 + d(16.) * caption_lines.len() as f32;
            let mut y = (r.top + r.bottom - block_h) / 2.;
            for line in label_lines {
                text(
                    line,
                    RectF::new(left + d(32.), y, up.left - d(12.), y + d(20.)),
                    14.,
                    false,
                    if *dim { t.secondary } else { t.text },
                )?;
                y += d(20.);
            }
            for line in caption_lines {
                text(
                    line,
                    RectF::new(left + d(32.), y, up.left - d(12.), y + d(16.)),
                    12.,
                    false,
                    t.secondary,
                )?;
                y += d(16.);
            }
            let focused_row = ring.is_some_and(|f| {
                f.top >= r.top - d(4.)
                    && f.bottom <= r.bottom + d(4.)
                    && f.left >= r.left
                    && f.right <= r.right + d(4.)
            });
            if hovered || focused_row {
                for (rect, glyph) in [(up, "\u{E70E}"), (down, "\u{E70D}")] {
                    control(ctx, Control::IconButton, *rect, "", false, t, dpi)?;
                    ctx.draw_text(
                        glyph,
                        *rect,
                        &style(
                            super::icon_family(),
                            d(12.),
                            400,
                            t.secondary,
                            Align::Center,
                        ),
                    )?;
                }
            }
            ctx.fill_rounded_rect(*chip, d(4.), t.subtle_selected)?;
            control(
                ctx,
                Control::Button(ButtonStyle::Subtle),
                *chip,
                "",
                false,
                t,
                dpi,
            )?;
            ctx.draw_text(
                "\u{E76C}",
                RectF::new(chip.left + d(8.), chip.top, chip.left + d(24.), chip.bottom),
                &style(
                    super::icon_family(),
                    d(16.),
                    400,
                    t.secondary,
                    Align::Center,
                ),
            )?;
            text(
                &fitted(dest, chip.right - chip.left - d(36.), dpi),
                RectF::new(
                    chip.left + d(28.),
                    chip.top,
                    chip.right - d(8.),
                    chip.bottom,
                ),
                14.,
                false,
                if *danger {
                    t.danger
                } else if *checked {
                    t.text
                } else {
                    t.secondary
                },
            )?;
        }
        Kind::Add => text(
            "+  Add a rule\u{2026}",
            RectF::new(left, r.top, right, r.bottom),
            14.,
            false,
            t.text,
        )?,
        Kind::Review {
            name,
            lines,
            buttons,
        } => {
            text(
                name,
                RectF::new(left, r.top + d(12.), right, r.top + d(32.)),
                14.,
                false,
                t.text,
            )?;
            for (i, line) in lines.iter().enumerate() {
                let top = r.top + d(32. + 16. * i as f32);
                text(
                    line,
                    RectF::new(left, top, right, top + d(16.)),
                    12.,
                    false,
                    t.secondary,
                )?;
            }
            control(
                ctx,
                Control::Button(ButtonStyle::Secondary),
                buttons[0],
                "Rewrite\u{2026}",
                false,
                t,
                dpi,
            )?;
            control(
                ctx,
                Control::Button(ButtonStyle::Subtle),
                buttons[1],
                "Dismiss",
                false,
                t,
                dpi,
            )?;
        }
        Kind::Default {
            control: c,
            dest,
            stacked,
        } => {
            let tr = if *stacked { right } else { c.left - d(16.) };
            text(
                "When no rule matches",
                RectF::new(left, r.top + d(10.), tr, r.top + d(30.)),
                14.,
                false,
                t.text,
            )?;
            let desc = "Where a new icon goes if none of the rules below applies.";
            for (i, line) in super::wrap(desc, d(12.), 400, (tr - left).max(d(40.)))?
                .iter()
                .enumerate()
            {
                let top = r.top + d(32. + 16. * i as f32);
                text(
                    line,
                    RectF::new(left, top, tr, top + d(16.)),
                    12.,
                    false,
                    t.secondary,
                )?;
            }
            control(
                ctx,
                Control::Dropdown,
                *c,
                &fitted(dest, c.right - c.left - d(40.), dpi),
                false,
                t,
                dpi,
            )?;
        }
        Kind::Keep(on) => {
            let toggle = RectF::new(
                right - d(40.),
                r.top + (r.bottom - r.top - d(20.)) / 2.,
                right,
                r.top + (r.bottom - r.top + d(20.)) / 2.,
            );
            running_text(
                ctx,
                r,
                left,
                toggle.left - d(16.),
                "Keep Round-up active",
                KEEP_DESC,
                t,
                dpi,
            )?;
            control(ctx, Control::Toggle, toggle, "", *on, t, dpi)?;
        }
        Kind::Run { control: c } => {
            running_text(
                ctx,
                r,
                left,
                c.left - d(16.),
                "Sort the desktop now",
                RUN_DESC,
                t,
                dpi,
            )?;
            control(
                ctx,
                Control::Button(ButtonStyle::Secondary),
                *c,
                "Run now",
                false,
                t,
                dpi,
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn running_text(
    ctx: &Ctx,
    r: RectF,
    left: f32,
    tr: f32,
    title: &str,
    desc: &str,
    t: &Tokens,
    dpi: u32,
) -> Result<()> {
    let d = |v: f32| scf(v, dpi);
    ctx.draw_text(
        title,
        RectF::new(left, r.top + d(10.), tr, r.top + d(30.)),
        &style(UI, d(14.), 400, t.text, Align::Left),
    )?;
    for (i, line) in super::wrap(desc, d(12.), 400, (tr - left).max(d(40.)))?
        .iter()
        .enumerate()
    {
        let top = r.top + d(32. + 16. * i as f32);
        ctx.draw_text(
            line,
            RectF::new(left, top, tr, top + d(16.)),
            &style(UI, d(12.), 400, t.secondary, Align::Left),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::round_up::{Condition, ReviewItem, TextOp};

    fn groups() -> Vec<GroupInfo> {
        vec![
            GroupInfo {
                id: 3,
                title: "Finance".into(),
                portal: false,
                items: 4,
            },
            GroupInfo {
                id: 4,
                title: "Gate".into(),
                portal: true,
                items: 0,
            },
        ]
    }

    fn page_settings() -> Settings {
        let mut s = Settings::default();
        let mut rule = s.round_up.new_user_rule();
        rule.name = "Invoices".into();
        rule.conditions = vec![Condition::Name {
            op: TextOp::Contains,
            value: "invoice".into(),
        }];
        rule.destination = Some(Destination::Corral { id: 3 });
        s.round_up.insert_rule(rule);
        s
    }

    #[test]
    fn rules_are_one_ordered_list_with_the_new_rule_above_the_built_in_ones() {
        let s = page_settings();
        assert_eq!(s.round_up.rules[0].name, "Invoices");
        assert!(s.round_up.rules[1].preset.is_some());
        let lay = layout(&s, &groups(), 96, 1000., 720., 0).unwrap();
        let labels: Vec<_> = lay
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Section(s) => Some(s.label),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["New icons", "Your rules", "Running"]);
        let indices: Vec<_> = lay.contexts.iter().map(|(_, i)| *i).collect();
        assert_eq!(indices, (0..s.round_up.rules.len()).collect::<Vec<_>>());
        assert!(lay.hits.iter().any(|(_, a)| matches!(a, Action::ApplyNow)));
    }

    #[test]
    fn there_is_no_rule_priority_row_and_gates_are_never_destinations() {
        let s = page_settings();
        let lay = layout(&s, &groups(), 96, 1000., 720., 0).unwrap();
        for block in &lay.blocks {
            if let Block::Section(sec) = block {
                assert_ne!(sec.label, "By file type");
            }
        }
        assert_eq!(pickable(&groups()).len(), 1);
    }

    #[test]
    fn missing_corral_shows_in_the_chip_and_in_a_warning_box() {
        let mut s = page_settings();
        s.round_up.rules[0].destination = Some(Destination::Corral { id: 99 });
        let lay = layout(&s, &groups(), 96, 1000., 720., 0).unwrap();
        let mut found = false;
        for block in &lay.blocks {
            match block {
                Block::Notice(n) => {
                    assert!(n.lines.join(" ").contains("no longer exists"));
                    found = true;
                }
                Block::Section(sec) => {
                    for row in &sec.rows {
                        if let Kind::Rule { dest, danger, .. } = &row.kind {
                            if *danger {
                                assert_eq!(dest, "Corral missing");
                            }
                        }
                    }
                }
            }
        }
        assert!(found);
    }

    #[test]
    fn needs_review_card_appears_only_with_items_and_has_two_actions_per_row() {
        let mut s = page_settings();
        assert!(
            !layout(&s, &groups(), 96, 1000., 720., 0)
                .unwrap()
                .blocks
                .iter()
                .any(|b| matches!(b, Block::Section(sec) if sec.label == "Needs review"))
        );
        s.round_up.needs_review.push(ReviewItem {
            position: 3,
            name: "Old".into(),
            data: "{Old}|<name>:=:0:x|".into(),
            target: "1|3|1|0".into(),
            reason: "Names are now matched without the extension.".into(),
        });
        let lay = layout(&s, &groups(), 96, 1000., 720., 0).unwrap();
        assert!(lay.hits.iter().any(|(_, a)| *a == Action::RewriteReview(0)));
        assert!(lay.hits.iter().any(|(_, a)| *a == Action::DismissReview(0)));
    }

    #[test]
    fn paused_page_shows_only_the_reason() {
        let mut s = page_settings();
        s.round_up.paused_reason = Some("Round-up is paused: newer version.".into());
        let lay = layout(&s, &groups(), 96, 1000., 720., 0).unwrap();
        assert_eq!(lay.blocks.len(), 1);
        assert!(focus_targets(&lay).is_empty());
    }

    #[test]
    fn move_buttons_are_mouse_only_and_every_target_fits_at_every_dpi_and_width() {
        let s = page_settings();
        for dpi in [96, 120, 144, 240] {
            for w in [760., 1000.] {
                let z = dpi as f32 / 96.;
                let lay = layout(&s, &groups(), dpi, w * z, 520. * z, 0).unwrap();
                assert!(
                    lay.hits
                        .iter()
                        .any(|(_, a)| matches!(a, Action::MoveRule { .. }))
                );
                assert!(
                    !focus_targets(&lay)
                        .iter()
                        .any(|(_, a)| matches!(a, Action::MoveRule { .. }))
                );
                for (r, _) in &lay.hits {
                    assert!(
                        r.left >= lay.header.x0 - 1. && r.right <= lay.header.right + 1.,
                        "{dpi} {w}"
                    );
                    assert!(r.right > r.left && r.bottom > r.top);
                }
                // The move buttons never overlap the destination chip.
                for block in &lay.blocks {
                    if let Block::Section(sec) = block {
                        for row in &sec.rows {
                            if let Kind::Rule { chip, up, down, .. } = &row.kind {
                                assert!(down.right <= chip.left && up.right <= down.left);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn hit_test_prefers_chip_and_move_buttons_over_the_row_toggle() {
        let s = page_settings();
        let lay = layout(&s, &groups(), 96, 1000., 720., 0).unwrap();
        let (chip, up, down) = lay
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Section(sec) => sec.rows.iter().find_map(|r| match &r.kind {
                    Kind::Rule { chip, up, down, .. } => Some((*chip, *up, *down)),
                    _ => None,
                }),
                _ => None,
            })
            .unwrap();
        let mid = |r: RectF| {
            (
                ((r.left + r.right) / 2.) as i32,
                ((r.top + r.bottom) / 2.) as i32,
            )
        };
        assert!(matches!(
            hit(&lay, mid(chip)),
            Some(Action::RuleDest { index: 0, .. })
        ));
        assert_eq!(
            hit(&lay, mid(up)),
            Some(Action::MoveRule { index: 0, up: true })
        );
        assert_eq!(
            hit(&lay, mid(down)),
            Some(Action::MoveRule {
                index: 0,
                up: false
            })
        );
        assert_eq!(
            hit(&lay, (chip.left as i32 - 300, mid(chip).1)),
            Some(Action::ToggleRule(0))
        );
    }

    #[test]
    fn long_rule_titles_wrap_completely() {
        let mut s = page_settings();
        s.round_up.rules[0].name = format!("A rule named {}", "verylongfilename".repeat(12));
        for dpi in [96, 240] {
            let z = dpi as f32 / 96.;
            let lay = layout(&s, &groups(), dpi, 760. * z, 520. * z, 0).unwrap();
            let Block::Section(sec) = lay
                .blocks
                .iter()
                .find(|b| matches!(b, Block::Section(s) if s.label == "Your rules"))
                .unwrap()
            else {
                unreachable!()
            };
            let Kind::Rule { label_lines, .. } = &sec.rows[0].kind else {
                unreachable!()
            };
            assert!(label_lines.len() > 1);
            assert_eq!(
                label_lines.join("").replace(' ', ""),
                s.round_up.rules[0].name.replace(' ', "")
            );
        }
    }

    #[test]
    fn page_draws_at_every_dpi_in_both_themes() {
        let s = page_settings();
        for dpi in [96, 120, 144, 240] {
            for theme in [Theme::dark(), Theme::light()] {
                let z = dpi as f32 / 96.;
                let lay = layout(&s, &groups(), dpi, 1000. * z, 720. * z, 0).unwrap();
                let mut surface =
                    crate::render::Surface::new((1000. * z) as u32, lay.bottom.ceil() as u32)
                        .unwrap();
                surface
                    .draw(|ctx| {
                        ctx.clear(theme.panel_bg);
                        draw(ctx, &lay, &theme, dpi)
                    })
                    .unwrap();
            }
        }
    }
}
