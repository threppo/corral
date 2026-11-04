//! Settings-only UIA adapter. No provider owns/borrows State. STA calls are rejected while
//! a window message (including a modal input loop) or another query is in progress.
//! Layout preparation uses the painter's helpers; `build_elements` is the pure tree builder.
//! Patterns post identities, never coordinates/pointers, and the UI thread revalidates them.
#![allow(non_upper_case_globals, non_snake_case)] // Windows SDK/COM names.
use super::*;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    panic::{AssertUnwindSafe, catch_unwind},
    rc::Rc,
};
use windows::{
    Win32::{
        Foundation::{
            BOOL, E_INVALIDARG, E_OUTOFMEMORY, E_POINTER, HWND, LPARAM, LRESULT, POINT, WPARAM,
        },
        Graphics::Gdi::ClientToScreen,
        System::{
            Com::SAFEARRAY,
            Ole::{SafeArrayCreateVector, SafeArrayDestroy, SafeArrayPutElement},
            Threading::GetCurrentThreadId,
            Variant::VT_I4,
        },
        UI::{Accessibility::*, Input::KeyboardAndMouse::GetFocus, WindowsAndMessaging::*},
    },
    core::{Error, HRESULT, IUnknown, IUnknown_Vtbl, Interface, VARIANT, implement},
};

pub(super) const WM_ACTION: u32 = WM_APP + 170;
const ROOT: &str = "CorralSettings";

fn unavailable() -> Error {
    Error::from_hresult(HRESULT(UIA_E_ELEMENTNOTAVAILABLE as i32))
}
fn unsupported() -> Error {
    Error::from_hresult(HRESULT(UIA_E_NOTSUPPORTED as i32))
}
// The explicit ABI adapters below initialise nullable out-pointers. Error::empty converts to
// S_OK, so our ergonomic trait implementations can also express a successful null result.
fn absent<T>() -> Result<T> {
    Err(Error::empty())
}
fn boundary<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|_| Err(unavailable()))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Pattern {
    None,
    Invoke,
    Toggle(bool),
    Range { value: f64, max: f64 },
    Select(bool),
    Expand(bool),
}

/// Owned measurements from the painter/hit layout, before building the UIA tree.
struct Measured {
    id: String,
    name: String,
    help: String,
    rect: RectF,
    kind: RowKind,
    navigation: bool,
    enabled: bool,
    hit: Hit,
    pattern: Pattern,
    value: Option<String>,
}
impl Measured {
    fn new(id: String, name: impl Into<String>, rect: RectF, hit: Hit, kind: RowKind) -> Self {
        Self {
            id,
            name: name.into(),
            help: String::new(),
            rect,
            kind,
            navigation: false,
            enabled: true,
            hit,
            pattern: Pattern::None,
            value: None,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct Element {
    id: String,
    name: String,
    help: String,
    rect: RectF,
    control_type: UIA_CONTROLTYPE_ID,
    enabled: bool,
    offscreen: bool,
    focused: bool,
    hit: Hit,
    pattern: Pattern,
    value: Option<String>,
}
impl Element {
    fn focusable(&self) -> bool {
        self.enabled && self.hit != Hit::Nothing
    }
    pub(super) fn hit(&self) -> Hit {
        self.hit
    }
    pub(super) fn point(&self) -> (i32, i32) {
        (
            ((self.rect.left + self.rect.right) / 2.) as i32,
            ((self.rect.top + self.rect.bottom) / 2.) as i32,
        )
    }
}

fn control_type(kind: RowKind) -> UIA_CONTROLTYPE_ID {
    match kind {
        RowKind::Check | RowKind::CheckRow | RowKind::Toggle | RowKind::Status => {
            UIA_CheckBoxControlTypeId
        }
        RowKind::Radio | RowKind::RadioRow | RowKind::Segmented | RowKind::TabStyleTile => {
            UIA_RadioButtonControlTypeId
        }
        RowKind::Combo | RowKind::Dropdown => UIA_ComboBoxControlTypeId,
        RowKind::Number | RowKind::Stepper => UIA_SpinnerControlTypeId,
        RowKind::ValueSlider => UIA_SliderControlTypeId,
        RowKind::Link | RowKind::Navigate => UIA_HyperlinkControlTypeId,
        RowKind::Button
        | RowKind::ButtonRow
        | RowKind::ExploreTile
        | RowKind::Color
        | RowKind::Expander => UIA_ButtonControlTypeId,
        _ => UIA_TextControlTypeId,
    }
}

/// Pure: accepts already measured painter geometry and values. No window, COM, TLS or I/O.
/// Client rectangles deliberately retain their unclipped size for scrolled-out controls.
fn build_elements(
    measured: Vec<Measured>,
    width: f32,
    height: f32,
    visible: bool,
    focused: Option<Hit>,
) -> Vec<Element> {
    measured
        .into_iter()
        .map(|m| {
            let control_type = if m.navigation {
                UIA_ListItemControlTypeId
            } else if matches!(m.pattern, Pattern::Range { .. }) {
                if m.hit == Hit::Spacing {
                    UIA_SpinnerControlTypeId
                } else {
                    UIA_SliderControlTypeId
                }
            } else {
                control_type(m.kind)
            };
            Element {
                offscreen: !visible
                    || m.rect.right <= 0.
                    || m.rect.left >= width
                    || m.rect.bottom <= 0.
                    || m.rect.top >= height,
                focused: m.enabled && m.hit != Hit::Nothing && focused == Some(m.hit),
                id: m.id,
                name: m.name,
                help: m.help,
                rect: m.rect,
                control_type,
                enabled: m.enabled,
                hit: m.hit,
                pattern: m.pattern,
                value: m.value,
            }
        })
        .collect()
}

fn row_id(page: Page, row: &Row) -> String {
    // Labels, field names and values are identity, never visible row indices (gates/reflow).
    format!("{page:?}.{:?}.{}.{}", row.bind, row.text, row.value)
}

fn pattern_for(hit: Hit, row: &Row, settings: &Settings) -> Pattern {
    use look_pages::Action as A;
    match hit {
        Hit::Nothing => Pattern::None,
        Hit::Toggle(f) => Pattern::Toggle(settings.is_on(f)),
        Hit::Radio(f, n) => Pattern::Select(settings.get(f) == n),
        Hit::FoldMode(n) | Hit::AnimationMode(n) => {
            Pattern::Select(row.current_value(settings) == n)
        }
        Hit::Spacing => Pattern::Range {
            value: settings.snap_padding as f64,
            max: 100.,
        },
        Hit::Combo { .. } | Hit::ColorTarget { .. } => Pattern::Expand(false),
        Hit::Expander(p, _) => Pattern::Expand(expander_open(p, row)),
        Hit::LookAction(A::Slider { kind, .. }) => Pattern::Range {
            value: match kind {
                look_pages::SliderKind::SeeThrough => {
                    look_pages::see_through_percent(settings.transparency_factor) as f64
                }
                look_pages::SliderKind::Blur => {
                    look_pages::nearest_blur(super::camouflage_options::effective_radius(settings))
                        as f64
                }
                look_pages::SliderKind::BlendStrength => {
                    look_pages::blend_percent(settings.desktop_blend_icon_opacity) as f64
                }
            },
            max: if kind == look_pages::SliderKind::Blur {
                4.
            } else {
                100.
            },
        },
        Hit::LookAction(A::TintMode(n)) => {
            Pattern::Select(look_pages::tint_mode(&look_pages::effective(settings)) == n)
        }
        Hit::LookAction(A::CamouflageMode(n)) => {
            Pattern::Select(look_pages::camouflage_mode() == n)
        }
        Hit::LookAction(A::Style(super::style_menus::Cmd::Darken(n))) => {
            Pattern::Select(settings.darken_titlebars == n)
        }
        Hit::LookAction(A::Style(_)) => Pattern::Toggle(look_pages::checked(row, settings)),
        _ => Pattern::Invoke,
    }
}

// Segment identity comes from the row even when input hit testing is gated off.
fn segment_hit(row: &Row, value: i64) -> Hit {
    if row.bind == Some(Field::SwipeAnimate) {
        Hit::AnimationMode(value)
    } else if row.secondary_bind.is_some() {
        Hit::FoldMode(value)
    } else if let Some(field) = row.bind {
        Hit::Radio(field, value)
    } else {
        Hit::Nothing
    }
}

/// Preparation may measure text, just as painting does. It owns every input, and never has
/// a State reference across layout/Win32/DirectWrite calls. Conversion below is pure.
fn measure(
    settings: &Settings,
    groups: &[GroupInfo],
    view: View,
    dpi: u32,
    width: i32,
    height: i32,
) -> Result<Vec<Measured>> {
    Ok(measure_page(settings, groups, view, dpi, width, height)?.0)
}

/// Layout only: never enter render_settings_editor, draw_page, or any preview renderer.
/// Both hit identities and keyboard focus use this same layout, rather than laying out
/// the entire page again for every row (and again for every keyboard target).
fn measure_page(
    settings: &Settings,
    groups: &[GroupInfo],
    view: View,
    dpi: u32,
    width: i32,
    height: i32,
) -> Result<(Vec<Measured>, Vec<(Hit, RectF)>)> {
    let mut out = Vec::new();
    let (x, y, w, h) = nav_item_rect(view.page.index(), dpi, height);
    let mut targets = vec![(
        Hit::Nav(view.page),
        RectF::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32),
    )];
    for (i, p) in Page::ALL.iter().enumerate() {
        let (x, y, w, h) = nav_item_rect(i, dpi, height);
        let mut e = Measured::new(
            format!("Navigation.{p:?}"),
            p.label(),
            RectF::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32),
            Hit::Nav(*p),
            RowKind::Button,
        );
        e.navigation = true;
        e.pattern = Pattern::Invoke;
        out.push(e);
    }
    if view.page == Page::Rules {
        let lay = rules_page::layout(
            settings,
            groups,
            dpi,
            width as f32,
            height as f32,
            view.scroll,
        )?;
        let header = page_header(
            "Round-up",
            "Send new desktop icons to the right Corral automatically.",
            dpi,
            width as f32,
            view.scroll,
            page_scrolls(lay.bottom + view.scroll as f32, height as f32),
        )?;
        out.push(Measured::new(
            format!("{:?}.title", view.page),
            "Round-up",
            header.title_rect,
            Hit::Nothing,
            RowKind::Label,
        ));
        for (rect, a) in rules_page::focus_targets(&lay) {
            targets.push((Hit::RuleAction(a), rect));
            use rules_page::Action as A;
            let (id, name, kind, pattern) = match a {
                A::DefaultDest { .. } => (
                    "default-destination".into(),
                    "When no rule matches".into(),
                    RowKind::Dropdown,
                    Pattern::Expand(false),
                ),
                A::ToggleRule(i) | A::RuleDest { index: i, .. } | A::EditRule(i) => {
                    let Some(rule) = settings.round_up.rules.get(i) else {
                        continue;
                    };
                    let name = rule.title(&rules_page::sentence_destination(
                        rule.destination.as_ref(),
                        groups,
                    ));
                    let dest = matches!(a, A::RuleDest { .. });
                    (
                        format!(
                            "rule.{}.{}",
                            rule.id,
                            if dest { "destination" } else { "enabled" }
                        ),
                        if dest {
                            rules_page::destination_text(rule.destination.as_ref(), groups)
                        } else {
                            name
                        },
                        if dest {
                            RowKind::Dropdown
                        } else {
                            RowKind::Toggle
                        },
                        if dest {
                            Pattern::Expand(false)
                        } else {
                            Pattern::Toggle(rule.enabled)
                        },
                    )
                }
                A::KeepApplied => (
                    "keep-applied".into(),
                    "Keep Round-up active".into(),
                    RowKind::Toggle,
                    Pattern::Toggle(settings.always_apply_rules),
                ),
                A::ApplyNow => (
                    "apply-now".into(),
                    "Run now".into(),
                    RowKind::Button,
                    Pattern::Invoke,
                ),
                A::AddRule { .. } => (
                    "add-rule".into(),
                    "+  Add a rule…".into(),
                    RowKind::Button,
                    Pattern::Invoke,
                ),
                A::RewriteReview(i) | A::DismissReview(i) => {
                    let Some(review) = settings.round_up.needs_review.get(i) else {
                        continue;
                    };
                    let rewrite = matches!(a, A::RewriteReview(_));
                    (
                        format!(
                            "review.{}.{}",
                            review.position,
                            if rewrite { "rewrite" } else { "dismiss" }
                        ),
                        if rewrite { "Rewrite…" } else { "Dismiss" }.into(),
                        RowKind::Button,
                        Pattern::Invoke,
                    )
                }
                _ => continue,
            };
            let mut e = Measured::new(
                format!("{:?}.{id}", view.page),
                name,
                rect,
                if pattern == Pattern::None {
                    Hit::Nothing
                } else {
                    Hit::RuleAction(a)
                },
                kind,
            );
            e.pattern = pattern;
            if matches!(a, rules_page::Action::DefaultDest { .. }) {
                e.value = Some(rules_page::default_dest_text(settings, groups));
                e.help = "Where a new icon goes when no rule below applies.".into();
            }
            if matches!(a, rules_page::Action::RuleDest { .. }) {
                e.value = Some(e.name.clone());
            }
            let mut label = Measured::new(
                format!("{}.label", e.id),
                e.name.clone(),
                rect,
                Hit::Nothing,
                RowKind::Label,
            );
            label.help = e.help.clone();
            out.push(label);
            out.push(e);
        }
        return Ok((out, targets));
    }
    let lay = layout_page(
        view.page,
        settings,
        dpi,
        width as f32,
        height as f32,
        view.scroll,
    )?;
    targets.extend(focus_targets_layout(view.page, dpi, &lay)?);
    out.push(Measured::new(
        format!("{:?}.title", view.page),
        lay.title,
        lay.title_rect,
        Hit::Nothing,
        RowKind::Label,
    ));
    if let Some((field, rect)) = lay.master {
        let mut e = Measured::new(
            format!("{:?}.{field:?}.master", view.page),
            lay.title,
            rect,
            Hit::Toggle(field),
            RowKind::Toggle,
        );
        e.pattern = Pattern::Toggle(settings.is_on(field));
        out.push(e);
    }
    if let Some(scope) = &lay.scope {
        for (hit, rect) in look_pages::scope_targets(scope) {
            let dropdown = matches!(hit, Hit::ColorTarget { .. });
            let mut e = Measured::new(
                format!(
                    "{:?}.scope.{}",
                    view.page,
                    if dropdown { "target" } else { "reset" }
                ),
                if dropdown {
                    look_pages::scope()
                        .title
                        .unwrap_or_else(|| "All Corrals".into())
                } else {
                    look_pages::reset_label()
                },
                rect,
                hit,
                if dropdown {
                    RowKind::Dropdown
                } else {
                    RowKind::Button
                },
            );
            e.help = if dropdown {
                "Apply changes to".into()
            } else {
                String::new()
            };
            e.pattern = if dropdown {
                Pattern::Expand(false)
            } else {
                Pattern::Invoke
            };
            if dropdown {
                e.value = Some(e.name.clone());
            }
            out.push(e);
        }
    }
    for sec in &lay.sections {
        if !sec.header.is_empty() {
            out.push(Measured::new(
                format!("{:?}.section.{}", view.page, sec.header),
                sec.header,
                sec.header_rect,
                Hit::Nothing,
                RowKind::Label,
            ));
        }
        for row in &sec.rows {
            let id = row_id(view.page, &row.row);
            let full = RectF::new(row.x, row.y, row.x + row.w, row.y + row.h);
            let row_name =
                if row.row.kind == RowKind::Status && !settings.is_on(Field::EnableCorral) {
                    "Corral is off".into()
                } else {
                    row.row.text.to_string()
                };
            let mut label = Measured::new(
                format!("{id}.label"),
                row_name.clone(),
                full,
                Hit::Nothing,
                RowKind::Label,
            );
            label.help = row.row.description().unwrap_or_default();
            if !label.name.is_empty() {
                out.push(label);
            }
            if row.row.kind == RowKind::Tiles {
                let tiles = backups::with_view(|v| v.tiles.clone());
                for (index, tile) in tiles.iter().enumerate() {
                    for (part, name, hit) in [
                        (0, "Restore", Hit::BackupRestore(index)),
                        (1, "More", Hit::BackupMore(index)),
                    ] {
                        let rect = backups::action_rect(
                            index,
                            part,
                            row.x,
                            row.y + backups::notice_height(dpi),
                            row.w,
                            dpi,
                        );
                        let mut e = Measured::new(
                            format!("Backups.{}.{}", tile.id, name),
                            name,
                            rect,
                            hit,
                            RowKind::Button,
                        );
                        e.help = format!("{}: {}", tile.title, backups::subtitle(tile));
                        e.pattern = Pattern::Invoke;
                        out.push(e);
                    }
                }
                continue;
            }
            // Ask the existing compound-control layout for all parts, even when gated off.
            let mut enabled_row = row.clone();
            enabled_row.enabled = true;
            let mut parts = look_pages::row_targets(&enabled_row, dpi)?;
            if parts.is_empty() && row.row.control_kind() == RowKind::Segmented {
                let r = control_rect(row, dpi)?;
                for (i, (_, value)) in row.row.choices.iter().enumerate() {
                    let w = (r.right - r.left) / row.row.choices.len() as f32;
                    let r = RectF::new(
                        r.left + w * i as f32,
                        r.top,
                        r.left + w * (i + 1) as f32,
                        r.bottom,
                    );
                    let hit = segment_hit(&row.row, *value);
                    parts.push((hit, r));
                }
            }
            if parts.is_empty() {
                let kind = row.row.control_kind();
                let r = if matches!(
                    kind,
                    RowKind::Dropdown | RowKind::Button | RowKind::ButtonRow | RowKind::Stepper
                ) {
                    control_rect(row, dpi)?
                } else if kind == RowKind::Expander {
                    RectF::new(full.left, full.top, full.right, full.top + scf(48., dpi))
                } else {
                    full
                };
                let hit = hit_test_layout(
                    view.page,
                    (((r.left + r.right) / 2.) as i32, (r.top + 2.) as i32),
                    dpi,
                    &lay,
                )?;
                // Gated controls still exist, but the input handler correctly refuses them.
                let hit = if hit == Hit::Nothing && !row.enabled {
                    match (kind, row.row.bind) {
                        (RowKind::Toggle | RowKind::CheckRow, Some(f)) => Hit::Toggle(f),
                        (RowKind::RadioRow, Some(f)) => Hit::Radio(f, row.row.value),
                        (RowKind::Stepper, _) => Hit::Spacing,
                        (RowKind::Dropdown, Some(f)) => Hit::Combo {
                            field: f,
                            rect: (r.left as i32, r.top as i32, r.right as i32, r.bottom as i32),
                        },
                        _ => Hit::Nothing,
                    }
                } else {
                    hit
                };
                if hit != Hit::Nothing
                    || matches!(
                        kind,
                        RowKind::Dropdown
                            | RowKind::Button
                            | RowKind::ButtonRow
                            | RowKind::Toggle
                            | RowKind::RadioRow
                            | RowKind::CheckRow
                            | RowKind::Stepper
                            | RowKind::ValueSlider
                    )
                {
                    parts.push((hit, r));
                }
            }
            for (i, (hit, rect)) in parts.into_iter().enumerate() {
                let name = match hit {
                    Hit::LookAction(look_pages::Action::Color { choice, .. }) => match choice {
                        look_pages::ColorChoice::Auto => "Auto".into(),
                        look_pages::ColorChoice::Preset(n) => {
                            format!("{} preset {}", row.row.text, n + 1)
                        }
                        look_pages::ColorChoice::Custom => "Custom…".into(),
                        look_pages::ColorChoice::Eyedropper => "Pick from screen".into(),
                    },
                    _ if row.row.control_kind() == RowKind::Segmented => row
                        .row
                        .choices
                        .get(i)
                        .map(|x| x.0)
                        .unwrap_or(row.row.text)
                        .into(),
                    _ => row
                        .row
                        .button_text
                        .map(str::to_owned)
                        .unwrap_or_else(|| row_name.clone()),
                };
                let mut e = Measured::new(
                    format!("{id}.control.{i}"),
                    name,
                    rect,
                    hit,
                    row.row.control_kind(),
                );
                e.enabled = row.enabled;
                e.help = row.row.description().unwrap_or_default();
                e.pattern = pattern_for(hit, &row.row, settings);
                if let Hit::Combo { field, .. } = hit {
                    let value = content::choice_value(settings, field);
                    e.value = combo_choices(field, value)
                        .into_iter()
                        .find(|x| x.1 == value)
                        .map(|x| x.0);
                }
                out.push(e);
            }
        }
    }
    Ok((out, targets))
}

pub(super) struct Input {
    pub settings: Settings,
    pub groups: Vec<GroupInfo>,
    pub view: View,
    pub dpi: u32,
    pub focus_index: Option<usize>,
    pub caption: Option<String>,
}

fn caption_element(page: Page, text: &str, dpi: u32, width: i32, height: i32) -> Measured {
    Measured::new(
        format!("{page:?}.caption"),
        text,
        RectF::new(
            (nav_width(dpi) + sc(16., dpi)) as f32,
            (height - sc(26., dpi)) as f32,
            (width - sc(12., dpi)) as f32,
            height as f32,
        ),
        Hit::Nothing,
        RowKind::Label,
    )
}
struct Snapshot {
    elements: Vec<Element>,
    origin: POINT,
    width: i32,
    height: i32,
    visible: bool,
}

/// Lazy and owned. Invalidation never measures/clones window state. A cached read is
/// just a generation check and an Rc clone, with no borrow held over Win32/COM calls.
#[derive(Default)]
struct SnapshotCache {
    revision: Cell<u64>,
    value: RefCell<Option<(u64, Rc<Snapshot>)>>,
}
impl SnapshotCache {
    fn invalidate(&self) {
        self.revision.set(self.revision.get().wrapping_add(1));
    }
    fn get_or_build(&self, build: impl FnOnce() -> Result<Snapshot>) -> Result<Rc<Snapshot>> {
        let revision = self.revision.get();
        if let Some((cached_revision, snapshot)) =
            self.value.try_borrow().map_err(|_| unavailable())?.as_ref()
        {
            if *cached_revision == revision {
                return Ok(snapshot.clone());
            }
        }
        let snapshot = Rc::new(build()?);
        // Layout can call out. Do not publish an obsolete snapshot after reentrant input.
        if self.revision.get() != revision {
            return Err(unavailable());
        }
        *self.value.try_borrow_mut().map_err(|_| unavailable())? =
            Some((revision, snapshot.clone()));
        Ok(snapshot)
    }
}

pub(super) fn invalidate(hwnd: HWND) {
    CONNECTION.with(|c| {
        if let Ok(c) = c.try_borrow() {
            if let Some(c) = c.as_ref().filter(|c| c.hwnd == hwnd) {
                c.cache.invalidate();
            }
        }
    });
}

thread_local! {
    static BUSY: Cell<u32> = const { Cell::new(0) };
    static NEXT: Cell<u64> = const { Cell::new(1) };
    static CONNECTION: RefCell<Option<Connection>> = const { RefCell::new(None) };
    static RETIRED: RefCell<Vec<Connection>> = const { RefCell::new(Vec::new()) };
    static RAISING: Cell<bool> = const { Cell::new(false) };
    static CLOSING: Cell<bool> = const { Cell::new(false) };
}
pub(super) struct Busy;
impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.with(|v| v.set(v.get().saturating_sub(1)));
    }
}
pub(super) fn enter() -> Busy {
    BUSY.with(|v| v.set(v.get().saturating_add(1)));
    Busy
}
pub(super) fn busy() -> bool {
    BUSY.with(|v| v.get() != 0)
}
pub(super) fn opened() {
    CLOSING.with(|v| v.set(false));
}

struct Connection {
    hwnd: HWND,
    generation: u64,
    providers: HashMap<String, Rc<IRawElementProviderSimple>>,
    pending: VecDeque<(usize, String, Command)>,
    ticket: usize,
    cache: Rc<SnapshotCache>,
    last: Option<Rc<Snapshot>>,
}
#[derive(Clone, Copy, Debug)]
pub(super) enum Command {
    Focus,
    Activate,
    Range(f64),
    Select,
    Expand,
    Collapse,
}

fn snapshot(hwnd: HWND) -> Result<Rc<Snapshot>> {
    // Providers use the normal STA COM marshaler. Never dereference State from a foreign
    // thread, including a client which obtained a raw pointer without COM marshaling.
    if busy() || unsafe { GetWindowThreadProcessId(hwnd, None) != GetCurrentThreadId() } {
        return Err(unavailable());
    }
    let _busy = enter();
    snapshot_inner(hwnd)
}
fn snapshot_inner(hwnd: HWND) -> Result<Rc<Snapshot>> {
    let cache = CONNECTION
        .with(|c| {
            c.try_borrow().ok().and_then(|c| {
                c.as_ref()
                    .filter(|c| c.hwnd == hwnd)
                    .map(|c| c.cache.clone())
            })
        })
        .ok_or_else(unavailable)?;
    cache.get_or_build(|| build_snapshot(hwnd))
}

fn build_snapshot(hwnd: HWND) -> Result<Snapshot> {
    let input = window::uia_input(hwnd).ok_or_else(unavailable)?;
    let mut client = windows::Win32::Foundation::RECT::default();
    unsafe { GetClientRect(hwnd, &mut client) }.map_err(|_| unavailable())?;
    let (width, height) = (client.right, client.bottom);
    let (mut measured, targets) = measure_page(
        &input.settings,
        &input.groups,
        input.view,
        input.dpi,
        width,
        height,
    )?;
    let focused = if unsafe { GetFocus() } == hwnd {
        input.focus_index.and_then(|i| targets.get(i).map(|x| x.0))
    } else {
        None
    };
    let visible = unsafe { IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool() };
    if let Some(caption) = &input.caption {
        measured.push(caption_element(
            input.view.page,
            caption,
            input.dpi,
            width,
            height,
        ));
    }
    let elements = build_elements(measured, width as f32, height as f32, visible, focused);
    let mut origin = POINT::default();
    if !unsafe { ClientToScreen(hwnd, &mut origin) }.as_bool() {
        return Err(unavailable());
    }
    Ok(Snapshot {
        elements,
        origin,
        width,
        height,
        visible,
    })
}

// windows 0.58's generated Result<Interface> vtables cannot write S_OK + null. These four
// ABI-equivalent declarations keep #[implement]'s identity/refcount/marshaling, while explicitly
// initialising every out-pointer, even for missing siblings/patterns/host/selection container.
#[windows::core::interface("d6dd68d1-86fd-4332-8666-9abedea2d24c")]
unsafe trait SimpleAbi: IUnknown {
    fn Options(&self, out: *mut ProviderOptions) -> HRESULT;
    fn Pattern(&self, id: UIA_PATTERN_ID, out: *mut *mut std::ffi::c_void) -> HRESULT;
    fn Property(&self, id: UIA_PROPERTY_ID, out: *mut VARIANT) -> HRESULT;
    fn Host(&self, out: *mut *mut std::ffi::c_void) -> HRESULT;
}
#[windows::core::interface("f7063da8-8359-439c-9297-bbc5299a7d87")]
unsafe trait FragmentAbi: IUnknown {
    fn Navigate(&self, direction: NavigateDirection, out: *mut *mut std::ffi::c_void) -> HRESULT;
    fn RuntimeId(&self, out: *mut *mut SAFEARRAY) -> HRESULT;
    fn Bounds(&self, out: *mut UiaRect) -> HRESULT;
    fn Embedded(&self, out: *mut *mut SAFEARRAY) -> HRESULT;
    fn Focus(&self) -> HRESULT;
    fn Root(&self, out: *mut *mut std::ffi::c_void) -> HRESULT;
}
#[windows::core::interface("620ce2a5-ab8f-40a9-86cb-de3c75599b58")]
unsafe trait RootAbi: IUnknown {
    fn FromPoint(&self, x: f64, y: f64, out: *mut *mut std::ffi::c_void) -> HRESULT;
    fn Focused(&self, out: *mut *mut std::ffi::c_void) -> HRESULT;
}
#[windows::core::interface("2acad808-b2d4-452d-a407-91ff1ad167b2")]
unsafe trait SelectionAbi: IUnknown {
    fn Select(&self) -> HRESULT;
    fn Add(&self) -> HRESULT;
    fn Remove(&self) -> HRESULT;
    fn Selected(&self, out: *mut BOOL) -> HRESULT;
    fn Container(&self, out: *mut *mut std::ffi::c_void) -> HRESULT;
}

unsafe fn output<T: Default>(out: *mut T, f: impl FnOnce() -> Result<T>) -> HRESULT {
    if out.is_null() {
        return E_POINTER;
    }
    unsafe {
        out.write(T::default());
    }
    match boundary(f) {
        Ok(value) => {
            unsafe {
                out.write(value);
            }
            HRESULT(0)
        }
        Err(error) => error.into(),
    }
}
unsafe fn interface_output<T: Interface>(
    out: *mut *mut std::ffi::c_void,
    f: impl FnOnce() -> Result<T>,
) -> HRESULT {
    unsafe { output(out, || f().map(Interface::into_raw)) }
}

#[implement(
    SimpleAbi,
    FragmentAbi,
    RootAbi,
    IInvokeProvider,
    IToggleProvider,
    IRangeValueProvider,
    SelectionAbi,
    IExpandCollapseProvider,
    IValueProvider
)]
struct Provider {
    hwnd: HWND,
    generation: u64,
    id: String,
}

// windows 0.58 #[implement] advertises IAgileObject unconditionally. This object uses the
// Settings STA: filter that marker from every interface, retaining the generated layout,
// reference counting and methods. The factory is the only way to publish a Provider.
const AGILE: windows::core::GUID =
    windows::core::GUID::from_u128(0x94ea2b94_e9cc_49e0_c0ff_ee64ca8f5b90);
unsafe extern "system" fn sta_query<const OFFSET: isize>(
    this: *mut std::ffi::c_void,
    iid: *const windows::core::GUID,
    out: *mut *mut std::ffi::c_void,
) -> HRESULT {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    unsafe {
        out.write(std::ptr::null_mut());
        if *iid == AGILE {
            return windows::Win32::Foundation::E_NOINTERFACE;
        }
        // These offsets are exactly those generated by #[implement] 0.58: identity is
        // at offset zero and each listed interface follows it, in declaration order.
        let object = &*this
            .cast::<*mut std::ffi::c_void>()
            .offset(OFFSET)
            .cast::<Provider_Impl>();
        windows::core::IUnknownImpl::QueryInterface(object, iid, out)
    }
}
macro_rules! sta_vtable {
    ($name:ident,$ty:ty,$index:tt,$offset:expr) => {
        const $name: $ty = {
            let mut v = Provider_Impl::VTABLES.$index;
            v.base__.QueryInterface = sta_query::<$offset>;
            v
        };
    };
}
sta_vtable!(STA_SIMPLE, SimpleAbi_Vtbl, 0, -1);
sta_vtable!(STA_FRAGMENT, FragmentAbi_Vtbl, 1, -2);
sta_vtable!(STA_ROOT, RootAbi_Vtbl, 2, -3);
sta_vtable!(STA_INVOKE, IInvokeProvider_Vtbl, 3, -4);
sta_vtable!(STA_TOGGLE, IToggleProvider_Vtbl, 4, -5);
sta_vtable!(STA_RANGE, IRangeValueProvider_Vtbl, 5, -6);
sta_vtable!(STA_SELECTION, SelectionAbi_Vtbl, 6, -7);
sta_vtable!(STA_EXPAND, IExpandCollapseProvider_Vtbl, 7, -8);
sta_vtable!(STA_VALUE, IValueProvider_Vtbl, 8, -9);
const STA_IDENTITY: windows::core::IInspectable_Vtbl = {
    let mut v = Provider_Impl::IDENTITY;
    v.base.QueryInterface = sta_query::<0>;
    v
};
fn make_provider(value: Provider) -> IRawElementProviderSimple {
    // Identical allocation and initial count to #[implement]'s factory, with the STA
    // vtables above installed before any interface pointer or reference can escape.
    let object = Box::new(Provider_Impl {
        identity: &STA_IDENTITY,
        vtables: (
            &STA_SIMPLE,
            &STA_FRAGMENT,
            &STA_ROOT,
            &STA_INVOKE,
            &STA_TOGGLE,
            &STA_RANGE,
            &STA_SELECTION,
            &STA_EXPAND,
            &STA_VALUE,
        ),
        this: value,
        count: windows::core::imp::WeakRefCount::new(),
    });
    let object = Box::into_raw(object);
    unsafe {
        IRawElementProviderSimple::from_raw(std::ptr::addr_of_mut!((*object).vtables.0).cast())
    }
}

#[allow(non_snake_case)]
impl SimpleAbi_Impl for Provider_Impl {
    unsafe fn Options(&self, out: *mut ProviderOptions) -> HRESULT {
        unsafe {
            output(out, || {
                IRawElementProviderSimple_Impl::ProviderOptions(self)
            })
        }
    }
    unsafe fn Pattern(&self, id: UIA_PATTERN_ID, out: *mut *mut std::ffi::c_void) -> HRESULT {
        unsafe { interface_output(out, || self.GetPatternProvider(id)) }
    }
    unsafe fn Property(&self, id: UIA_PROPERTY_ID, out: *mut VARIANT) -> HRESULT {
        unsafe { output(out, || self.GetPropertyValue(id)) }
    }
    unsafe fn Host(&self, out: *mut *mut std::ffi::c_void) -> HRESULT {
        unsafe { interface_output(out, || self.HostRawElementProvider()) }
    }
}
#[allow(non_snake_case)]
impl FragmentAbi_Impl for Provider_Impl {
    unsafe fn Navigate(&self, d: NavigateDirection, out: *mut *mut std::ffi::c_void) -> HRESULT {
        unsafe { interface_output(out, || IRawElementProviderFragment_Impl::Navigate(self, d)) }
    }
    unsafe fn RuntimeId(&self, out: *mut *mut SAFEARRAY) -> HRESULT {
        unsafe { output(out, || self.GetRuntimeId()) }
    }
    unsafe fn Bounds(&self, out: *mut UiaRect) -> HRESULT {
        unsafe { output(out, || self.BoundingRectangle()) }
    }
    unsafe fn Embedded(&self, out: *mut *mut SAFEARRAY) -> HRESULT {
        unsafe { output(out, || self.GetEmbeddedFragmentRoots()) }
    }
    unsafe fn Focus(&self) -> HRESULT {
        boundary(|| self.SetFocus()).into()
    }
    unsafe fn Root(&self, out: *mut *mut std::ffi::c_void) -> HRESULT {
        unsafe { interface_output(out, || self.FragmentRoot()) }
    }
}
#[allow(non_snake_case)]
impl RootAbi_Impl for Provider_Impl {
    unsafe fn FromPoint(&self, x: f64, y: f64, out: *mut *mut std::ffi::c_void) -> HRESULT {
        unsafe { interface_output(out, || self.ElementProviderFromPoint(x, y)) }
    }
    unsafe fn Focused(&self, out: *mut *mut std::ffi::c_void) -> HRESULT {
        unsafe { interface_output(out, || self.GetFocus()) }
    }
}
#[allow(non_snake_case)]
impl SelectionAbi_Impl for Provider_Impl {
    unsafe fn Select(&self) -> HRESULT {
        boundary(|| ISelectionItemProvider_Impl::Select(self)).into()
    }
    unsafe fn Add(&self) -> HRESULT {
        boundary(|| self.AddToSelection()).into()
    }
    unsafe fn Remove(&self) -> HRESULT {
        boundary(|| self.RemoveFromSelection()).into()
    }
    unsafe fn Selected(&self, out: *mut BOOL) -> HRESULT {
        unsafe { output(out, || self.IsSelected()) }
    }
    unsafe fn Container(&self, out: *mut *mut std::ffi::c_void) -> HRESULT {
        unsafe { interface_output(out, || self.SelectionContainer()) }
    }
}

fn provider(hwnd: HWND, generation: u64, id: &str) -> Result<IRawElementProviderSimple> {
    let existing = CONNECTION.with(|c| {
        let c = c.try_borrow().map_err(|_| unavailable())?;
        let c = c
            .as_ref()
            .filter(|c| c.hwnd == hwnd && c.generation == generation)
            .ok_or_else(unavailable)?;
        Ok::<_, Error>(c.providers.get(id).cloned())
    })?;
    if let Some(p) = existing {
        return Ok(p.as_ref().clone());
    }
    let p = make_provider(Provider {
        hwnd,
        generation,
        id: id.into(),
    });
    let p = Rc::new(p);
    let p = CONNECTION.with(|c| {
        let mut c = c.try_borrow_mut().map_err(|_| unavailable())?;
        let c = c
            .as_mut()
            .filter(|c| c.hwnd == hwnd && c.generation == generation)
            .ok_or_else(unavailable)?;
        Ok::<_, Error>(c.providers.entry(id.into()).or_insert(p).clone())
    })?;
    // AddRef/QueryInterface/Release never run while CONNECTION is borrowed.
    Ok(p.as_ref().clone())
}
fn find<'a>(snapshot: &'a Snapshot, id: &str) -> Result<&'a Element> {
    snapshot
        .elements
        .iter()
        .find(|e| e.id == id)
        .ok_or_else(unavailable)
}
impl Provider {
    fn read<T>(&self, f: impl FnOnce(Rc<Snapshot>) -> Result<T>) -> Result<T> {
        boundary(|| {
            let valid = CONNECTION.with(|c| {
                c.try_borrow().ok().is_some_and(|c| {
                    c.as_ref()
                        .is_some_and(|c| c.hwnd == self.hwnd && c.generation == self.generation)
                })
            });
            if !valid {
                return Err(unavailable());
            }
            let snap = snapshot(self.hwnd)?;
            if self.id != ROOT {
                find(&snap, &self.id)?;
            }
            CONNECTION.with(|c| {
                if let Ok(mut c) = c.try_borrow_mut() {
                    if let Some(c) = c.as_mut() {
                        if c.last.is_none() {
                            c.last = Some(snap.clone());
                        }
                    }
                }
            });
            f(snap)
        })
    }
    fn element<T>(&self, f: impl FnOnce(Element) -> Result<T>) -> Result<T> {
        self.read(|s| f(find(&s, &self.id)?.clone()))
    }
    fn post(&self, command: Command) -> Result<()> {
        self.element(|e| {
            if !e.enabled {
                return Err(Error::from_hresult(HRESULT(UIA_E_ELEMENTNOTENABLED as i32)));
            }
            match command {
                Command::Focus if !e.focusable() => return Err(unsupported()),
                Command::Activate if !matches!(e.pattern, Pattern::Invoke | Pattern::Toggle(_)) => {
                    return Err(unsupported());
                }
                Command::Select if !matches!(e.pattern, Pattern::Select(_)) => {
                    return Err(unsupported());
                }
                Command::Range(value) => match e.pattern {
                    Pattern::Range { max, .. }
                        if value.is_finite()
                            && value.fract() == 0.
                            && value >= 0.
                            && value <= max => {}
                    _ => return Err(Error::from_hresult(E_INVALIDARG)),
                },
                Command::Expand | Command::Collapse if !matches!(e.pattern, Pattern::Expand(_)) => {
                    return Err(unsupported());
                }
                _ => {}
            }
            let ticket = CONNECTION.with(|c| {
                let mut c = c.try_borrow_mut().map_err(|_| unavailable())?;
                let c = c.as_mut().ok_or_else(unavailable)?;
                c.ticket = c.ticket.wrapping_add(1);
                let ticket = c.ticket;
                c.pending.push_back((ticket, self.id.clone(), command));
                Ok::<_, Error>(ticket)
            })?;
            if let Err(error) =
                unsafe { PostMessageW(self.hwnd, WM_ACTION, WPARAM(ticket), LPARAM(0)) }
            {
                CONNECTION.with(|c| {
                    if let Ok(mut c) = c.try_borrow_mut() {
                        if let Some(c) = c.as_mut() {
                            c.pending.retain(|(id, _, _)| *id != ticket);
                        }
                    }
                });
                return Err(error);
            }
            Ok(())
        })
    }
}

pub(super) fn get_object(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    boundary(|| {
        // Factory creation never dereferences State. Even a reentrant WM_GETOBJECT can
        // return the root: its state-dependent methods then report the busy error.
        if CLOSING.with(Cell::get) || !window::uia_attached(hwnd) {
            return Err(unavailable());
        }
        let generation = CONNECTION.with(|c| {
            let mut c = c.try_borrow_mut().map_err(|_| unavailable())?;
            if c.is_none() {
                let generation = NEXT.with(|n| {
                    let v = n.get();
                    n.set(v.wrapping_add(1));
                    v
                });
                *c = Some(Connection {
                    hwnd,
                    generation,
                    providers: HashMap::new(),
                    pending: VecDeque::new(),
                    ticket: 0,
                    cache: Rc::new(SnapshotCache::default()),
                    last: None,
                });
            }
            c.as_ref()
                .filter(|c| c.hwnd == hwnd)
                .map(|c| c.generation)
                .ok_or_else(unavailable)
        })?;
        let root = provider(hwnd, generation, ROOT)?;
        Ok(unsafe { UiaReturnRawElementProvider(hwnd, wparam, lparam, &root) })
    })
    .unwrap_or(LRESULT(0))
}

pub(super) fn destroy(hwnd: HWND) {
    CLOSING.with(|v| v.set(true));
    let connection = CONNECTION.with(|c| c.try_borrow_mut().ok().and_then(|mut c| c.take()));
    if let Some(c) = connection {
        // Remove identity lookup first: reentrant WM_GETOBJECT must not resurrect a provider.
        let _busy = enter();
        unsafe {
            UiaReturnRawElementProvider(hwnd, WPARAM(0), LPARAM(0), None);
        }
        // UiaDisconnectProvider cannot call out from a cross-thread SendMessage. The main
        // loop drains this queue immediately after DispatchMessage and also after WM_QUIT.
        RETIRED.with(|r| r.borrow_mut().push(c));
    }
}

pub(super) fn disconnect_retired() {
    let connections = RETIRED.with(|r| std::mem::take(&mut *r.borrow_mut()));
    for c in connections {
        for p in c.providers.values() {
            let _ = unsafe { UiaDisconnectProvider(p.as_ref()) };
        }
    }
}

fn take_pending(
    pending: &mut VecDeque<(usize, String, Command)>,
    busy: bool,
) -> Option<(usize, String, Command)> {
    if busy { None } else { pending.pop_front() }
}

/// WM_ACTION is only a wakeup. Modal loops may consume it while busy; the outer
/// window handler drains the retained FIFO once all State borrows have ended.
pub(super) fn actions(hwnd: HWND) {
    loop {
        let pending = CONNECTION.with(|c| {
            c.try_borrow_mut().ok().and_then(|mut c| {
                c.as_mut()
                    .filter(|c| c.hwnd == hwnd)
                    .and_then(|c| take_pending(&mut c.pending, busy()))
            })
        });
        let Some((_, id, command)) = pending else {
            break;
        };
        let _ = boundary(|| {
            // Earlier actions may have changed pages, gates or row identities.
            let snap = snapshot(hwnd)?;
            let e = find(&snap, &id)?;
            if !e.enabled {
                return Err(unavailable());
            }
            let _busy = enter();
            window::uia_action(hwnd, e, command)
        });
    }
}

/// Called by the input bridge after focus reveal has scrolled/reflowed the page.
pub(super) fn refreshed(hwnd: HWND, e: &Element) -> Result<Element> {
    let s = snapshot_inner(hwnd)?;
    Ok(find(&s, &e.id)?.clone())
}

fn relative(ids: &[String], id: &str, direction: NavigateDirection) -> Option<String> {
    if id == ROOT {
        return match direction {
            NavigateDirection_FirstChild => ids.first().cloned(),
            NavigateDirection_LastChild => ids.last().cloned(),
            _ => None,
        };
    }
    let at = ids.iter().position(|x| x == id)?;
    match direction {
        NavigateDirection_Parent => Some(ROOT.into()),
        NavigateDirection_NextSibling => ids.get(at + 1).cloned(),
        NavigateDirection_PreviousSibling => at.checked_sub(1).and_then(|at| ids.get(at)).cloned(),
        _ => None,
    }
}
fn int_array(values: &[i32]) -> Result<*mut SAFEARRAY> {
    let array = unsafe { SafeArrayCreateVector(VT_I4, 0, values.len() as u32) };
    if array.is_null() {
        return Err(Error::from_hresult(E_OUTOFMEMORY));
    }
    for (index, value) in values.iter().enumerate() {
        if let Err(e) =
            unsafe { SafeArrayPutElement(array, &(index as i32), (value as *const i32).cast()) }
        {
            let _ = unsafe { SafeArrayDestroy(array) };
            return Err(e);
        }
    }
    Ok(array)
}

#[allow(non_snake_case)]
impl IRawElementProviderSimple_Impl for Provider_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        // UIA queries this immutable metadata to choose a calling thread, before it can
        // marshal state queries to our STA. It deliberately does not inspect State/TLS.
        boundary(|| Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading))
    }
    fn GetPatternProvider(&self, id: UIA_PATTERN_ID) -> Result<IUnknown> {
        self.read(|s| {
            if self.id == ROOT {
                return absent();
            }
            let e = find(&s, &self.id)?;
            let p = provider(self.hwnd, self.generation, &self.id)?;
            if id == UIA_ValuePatternId && e.value.is_some() {
                return p.cast::<IValueProvider>()?.cast();
            }
            match (id, e.pattern) {
                (UIA_InvokePatternId, Pattern::Invoke) => p.cast::<IInvokeProvider>()?.cast(),
                (UIA_TogglePatternId, Pattern::Toggle(_)) => p.cast::<IToggleProvider>()?.cast(),
                (UIA_RangeValuePatternId, Pattern::Range { .. }) => {
                    p.cast::<IRangeValueProvider>()?.cast()
                }
                (UIA_SelectionItemPatternId, Pattern::Select(_)) => {
                    p.cast::<ISelectionItemProvider>()?.cast()
                }
                (UIA_ExpandCollapsePatternId, Pattern::Expand(_)) => {
                    p.cast::<IExpandCollapseProvider>()?.cast()
                }
                _ => absent(),
            }
        })
    }
    fn GetPropertyValue(&self, id: UIA_PROPERTY_ID) -> Result<VARIANT> {
        self.read(|s| {
            if self.id == ROOT {
                return Ok(match id {
                    UIA_NamePropertyId => "Corral Settings".into(),
                    UIA_AutomationIdPropertyId => ROOT.into(),
                    UIA_ControlTypePropertyId => UIA_WindowControlTypeId.0.into(),
                    UIA_IsEnabledPropertyId => true.into(),
                    UIA_IsKeyboardFocusablePropertyId => false.into(),
                    UIA_HasKeyboardFocusPropertyId => false.into(),
                    UIA_IsOffscreenPropertyId => (!s.visible).into(),
                    UIA_IsControlElementPropertyId | UIA_IsContentElementPropertyId => true.into(),
                    _ => VARIANT::default(),
                });
            }
            let e = find(&s, &self.id)?;
            Ok(match id {
                UIA_NamePropertyId => e.name.as_str().into(),
                UIA_HelpTextPropertyId => e.help.as_str().into(),
                UIA_AutomationIdPropertyId => e.id.as_str().into(),
                UIA_ControlTypePropertyId => e.control_type.0.into(),
                UIA_IsEnabledPropertyId => e.enabled.into(),
                UIA_IsOffscreenPropertyId => e.offscreen.into(),
                UIA_HasKeyboardFocusPropertyId => e.focused.into(),
                UIA_IsKeyboardFocusablePropertyId => e.focusable().into(),
                UIA_IsControlElementPropertyId | UIA_IsContentElementPropertyId => true.into(),
                UIA_ToggleToggleStatePropertyId => match e.pattern {
                    Pattern::Toggle(v) => i32::from(v).into(),
                    _ => VARIANT::default(),
                },
                UIA_RangeValueValuePropertyId => match e.pattern {
                    Pattern::Range { value, .. } => value.into(),
                    _ => VARIANT::default(),
                },
                UIA_SelectionItemIsSelectedPropertyId => match e.pattern {
                    Pattern::Select(v) => v.into(),
                    _ => VARIANT::default(),
                },
                UIA_ExpandCollapseExpandCollapseStatePropertyId => match e.pattern {
                    Pattern::Expand(v) => i32::from(v).into(),
                    _ => VARIANT::default(),
                },
                UIA_ValueValuePropertyId => {
                    e.value.as_deref().map(VARIANT::from).unwrap_or_default()
                }
                UIA_ValueIsReadOnlyPropertyId => e
                    .value
                    .as_ref()
                    .map(|_| VARIANT::from(true))
                    .unwrap_or_default(),
                UIA_RangeValueIsReadOnlyPropertyId => match e.pattern {
                    Pattern::Range { .. } => (!e.enabled).into(),
                    _ => VARIANT::default(),
                },
                UIA_RangeValueMinimumPropertyId => match e.pattern {
                    Pattern::Range { .. } => 0f64.into(),
                    _ => VARIANT::default(),
                },
                UIA_RangeValueMaximumPropertyId => match e.pattern {
                    Pattern::Range { max, .. } => max.into(),
                    _ => VARIANT::default(),
                },
                _ => VARIANT::default(),
            })
        })
    }
    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        self.read(|_| {
            if self.id == ROOT {
                unsafe { UiaHostProviderFromHwnd(self.hwnd) }
            } else {
                absent()
            }
        })
    }
}
#[allow(non_snake_case)]
impl IRawElementProviderFragment_Impl for Provider_Impl {
    fn Navigate(&self, d: NavigateDirection) -> Result<IRawElementProviderFragment> {
        self.read(|s| {
            match relative(
                &s.elements.iter().map(|e| e.id.clone()).collect::<Vec<_>>(),
                &self.id,
                d,
            ) {
                Some(id) => provider(self.hwnd, self.generation, &id)?.cast(),
                None => absent(),
            }
        })
    }
    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        self.read(|_| {
            if self.id == ROOT {
                return Ok(std::ptr::null_mut());
            } // HWND host supplies the root ID.
            let mut id = vec![
                UiaAppendRuntimeId as i32,
                self.generation as i32,
                (self.generation >> 32) as i32,
            ];
            id.extend(self.id.encode_utf16().map(i32::from));
            int_array(&id)
        })
    }
    fn BoundingRectangle(&self) -> Result<UiaRect> {
        self.read(|s| {
            let r = if self.id == ROOT {
                RectF::new(0., 0., s.width as f32, s.height as f32)
            } else {
                find(&s, &self.id)?.rect
            };
            Ok(UiaRect {
                left: s.origin.x as f64 + r.left as f64,
                top: s.origin.y as f64 + r.top as f64,
                width: (r.right - r.left) as f64,
                height: (r.bottom - r.top) as f64,
            })
        })
    }
    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        self.read(|_| Ok(std::ptr::null_mut()))
    }
    fn SetFocus(&self) -> Result<()> {
        self.post(Command::Focus)
    }
    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        self.read(|_| provider(self.hwnd, self.generation, ROOT)?.cast())
    }
}
#[allow(non_snake_case)]
impl IRawElementProviderFragmentRoot_Impl for Provider_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        self.read(|s| {
            let (x, y) = (x - s.origin.x as f64, y - s.origin.y as f64);
            if x < 0. || y < 0. || x >= s.width as f64 || y >= s.height as f64 {
                return absent();
            }
            let id = s
                .elements
                .iter()
                .rev()
                .find(|e| {
                    !e.offscreen
                        && x >= e.rect.left as f64
                        && x < e.rect.right as f64
                        && y >= e.rect.top as f64
                        && y < e.rect.bottom as f64
                })
                .map_or(ROOT, |e| e.id.as_str());
            provider(self.hwnd, self.generation, id)?.cast()
        })
    }
    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        self.read(|s| match s.elements.iter().find(|e| e.focused) {
            Some(e) => provider(self.hwnd, self.generation, &e.id)?.cast(),
            None => absent(),
        })
    }
}
#[allow(non_snake_case)]
impl IInvokeProvider_Impl for Provider_Impl {
    fn Invoke(&self) -> Result<()> {
        self.post(Command::Activate)
    }
}
#[allow(non_snake_case)]
impl IToggleProvider_Impl for Provider_Impl {
    fn Toggle(&self) -> Result<()> {
        self.post(Command::Activate)
    }
    fn ToggleState(&self) -> Result<ToggleState> {
        self.element(|e| match e.pattern {
            Pattern::Toggle(v) => Ok(if v { ToggleState_On } else { ToggleState_Off }),
            _ => Err(unsupported()),
        })
    }
}
#[allow(non_snake_case)]
impl IRangeValueProvider_Impl for Provider_Impl {
    fn SetValue(&self, v: f64) -> Result<()> {
        self.post(Command::Range(v))
    }
    fn Value(&self) -> Result<f64> {
        self.element(|e| match e.pattern {
            Pattern::Range { value, .. } => Ok(value),
            _ => Err(unsupported()),
        })
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        self.element(|e| Ok(BOOL::from(!e.enabled)))
    }
    fn Maximum(&self) -> Result<f64> {
        self.element(|e| match e.pattern {
            Pattern::Range { max, .. } => Ok(max),
            _ => Err(unsupported()),
        })
    }
    fn Minimum(&self) -> Result<f64> {
        self.element(|_| Ok(0.))
    }
    fn LargeChange(&self) -> Result<f64> {
        self.element(|e| match e.pattern {
            Pattern::Range { max, .. } => Ok(if max == 4. { 1. } else { 10. }),
            _ => Err(unsupported()),
        })
    }
    fn SmallChange(&self) -> Result<f64> {
        self.element(|_| Ok(1.))
    }
}
#[allow(non_snake_case)]
impl ISelectionItemProvider_Impl for Provider_Impl {
    fn Select(&self) -> Result<()> {
        self.post(Command::Select)
    }
    fn AddToSelection(&self) -> Result<()> {
        self.post(Command::Select)
    }
    fn RemoveFromSelection(&self) -> Result<()> {
        self.element(|_| Err(Error::from_hresult(HRESULT(UIA_E_INVALIDOPERATION as i32))))
    }
    fn IsSelected(&self) -> Result<BOOL> {
        self.element(|e| match e.pattern {
            Pattern::Select(v) => Ok(v.into()),
            _ => Err(unsupported()),
        })
    }
    fn SelectionContainer(&self) -> Result<IRawElementProviderSimple> {
        self.element(|_| absent())
    }
}
#[allow(non_snake_case)]
impl IExpandCollapseProvider_Impl for Provider_Impl {
    fn Expand(&self) -> Result<()> {
        self.post(Command::Expand)
    }
    fn Collapse(&self) -> Result<()> {
        self.post(Command::Collapse)
    }
    fn ExpandCollapseState(&self) -> Result<ExpandCollapseState> {
        self.element(|e| match e.pattern {
            Pattern::Expand(v) => Ok(if v {
                ExpandCollapseState_Expanded
            } else {
                ExpandCollapseState_Collapsed
            }),
            _ => Err(unsupported()),
        })
    }
}

impl IValueProvider_Impl for Provider_Impl {
    fn SetValue(&self, _value: &windows::core::PCWSTR) -> Result<()> {
        self.element(|_| Err(unsupported()))
    }
    fn Value(&self) -> Result<windows::core::BSTR> {
        self.element(|e| e.value.as_deref().map(Into::into).ok_or_else(unsupported))
    }
    fn IsReadOnly(&self) -> Result<BOOL> {
        self.element(|_| Ok(true.into()))
    }
}

pub(super) fn run_command(
    hwnd: HWND,
    e: &Element,
    command: Command,
    mut key: impl FnMut(usize),
    mut click: impl FnMut((i32, i32)),
) {
    match command {
        Command::Focus => {}
        Command::Range(v) => {
            if let Pattern::Range { value, .. } = e.pattern {
                let k = if e.hit == Hit::Spacing {
                    if v > value { 0x26 } else { 0x28 }
                } else if v > value {
                    0x27
                } else {
                    0x25
                };
                for _ in 0..((v - value).abs() as usize).min(100) {
                    key(k);
                }
            }
        }
        Command::Select if matches!(e.pattern, Pattern::Select(true)) => {}
        Command::Expand if matches!(e.pattern, Pattern::Expand(true)) => {}
        Command::Collapse if matches!(e.pattern, Pattern::Expand(false)) => {}
        _ => click(e.point()),
    }
    let _ = hwnd;
}

fn metadata_changes(old: &Element, new: &Element) -> Vec<(UIA_PROPERTY_ID, VARIANT, VARIANT)> {
    let mut changes = Vec::new();
    if old.name != new.name {
        changes.push((
            UIA_NamePropertyId,
            old.name.as_str().into(),
            new.name.as_str().into(),
        ));
    }
    if old.enabled != new.enabled {
        changes.push((
            UIA_IsEnabledPropertyId,
            old.enabled.into(),
            new.enabled.into(),
        ));
    }
    changes
}

/// No layout work, allocations or event calls until WM_GETOBJECT has created a connection.
/// The caller invokes this only after the outermost message returned and State borrows ended.
pub(super) fn events(hwnd: HWND) {
    let _ = boundary(|| {
        if busy() {
            return Ok(());
        }
        let generation = CONNECTION.with(|c| {
            c.try_borrow()
                .ok()
                .and_then(|c| c.as_ref().map(|c| c.generation))
        });
        let Some(generation) = generation else {
            return Ok(());
        };
        if RAISING.with(|v| v.replace(true)) {
            return Ok(());
        }
        struct Raising;
        impl Drop for Raising {
            fn drop(&mut self) {
                RAISING.with(|v| v.set(false));
            }
        }
        let _raising = Raising;
        if !unsafe { UiaClientsAreListening() }.as_bool() {
            return Ok(());
        }
        let new = snapshot(hwnd)?;
        let old = CONNECTION.with(|c| {
            c.try_borrow_mut()
                .ok()
                .and_then(|mut c| c.as_mut().and_then(|c| c.last.replace(new.clone())))
        });
        let Some(old) = old else { return Ok(()) };
        if Rc::ptr_eq(&old, &new) {
            return Ok(());
        }
        let root = provider(hwnd, generation, ROOT)?;
        if old
            .elements
            .iter()
            .map(|e| (&e.id, e.offscreen))
            .ne(new.elements.iter().map(|e| (&e.id, e.offscreen)))
        {
            let _ = unsafe {
                UiaRaiseStructureChangedEvent(
                    &root,
                    StructureChangeType_ChildrenInvalidated,
                    std::ptr::null_mut(),
                    0,
                )
            };
        }
        let before = old.elements.iter().find(|e| e.focused).map(|e| &e.id);
        let after = new.elements.iter().find(|e| e.focused).map(|e| &e.id);
        if before != after {
            if let Some(id) = after {
                let p = provider(hwnd, generation, id)?;
                let _ = unsafe { UiaRaiseAutomationEvent(&p, UIA_AutomationFocusChangedEventId) };
            }
        }
        for e in &new.elements {
            let Some(old) = old.elements.iter().find(|o| o.id == e.id) else {
                continue;
            };
            for (id, a, b) in metadata_changes(old, e) {
                let p = provider(hwnd, generation, &e.id)?;
                let _ = unsafe { UiaRaiseAutomationPropertyChangedEvent(&p, id, &a, &b) };
            }
            let change: Option<(UIA_PROPERTY_ID, VARIANT, VARIANT)> = match (old.pattern, e.pattern)
            {
                (Pattern::Toggle(a), Pattern::Toggle(b)) if a != b => Some((
                    UIA_ToggleToggleStatePropertyId,
                    i32::from(a).into(),
                    i32::from(b).into(),
                )),
                (Pattern::Range { value: a, .. }, Pattern::Range { value: b, .. }) if a != b => {
                    Some((UIA_RangeValueValuePropertyId, a.into(), b.into()))
                }
                (Pattern::Select(a), Pattern::Select(b)) if a != b => {
                    Some((UIA_SelectionItemIsSelectedPropertyId, a.into(), b.into()))
                }
                (Pattern::Expand(a), Pattern::Expand(b)) if a != b => Some((
                    UIA_ExpandCollapseExpandCollapseStatePropertyId,
                    i32::from(a).into(),
                    i32::from(b).into(),
                )),
                _ => None,
            };
            if let Some((id, a, b)) = change {
                let p = provider(hwnd, generation, &e.id)?;
                let _ = unsafe { UiaRaiseAutomationPropertyChangedEvent(&p, id, &a, &b) };
            }
            if old.value != e.value {
                let p = provider(hwnd, generation, &e.id)?;
                let a = VARIANT::from(old.value.as_deref().unwrap_or(""));
                let b = VARIANT::from(e.value.as_deref().unwrap_or(""));
                let _ = unsafe {
                    UiaRaiseAutomationPropertyChangedEvent(&p, UIA_ValueValuePropertyId, &a, &b)
                };
            }
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture_snapshot(elements: Vec<Element>) -> Snapshot {
        Snapshot {
            elements,
            origin: POINT::default(),
            width: 1000,
            height: 760,
            visible: true,
        }
    }

    #[test]
    fn many_reads_build_once_and_page_change_builds_once_more() -> Result<()> {
        let cache = SnapshotCache::default();
        let builds = Cell::new(0);
        let read = |page| {
            cache.get_or_build(|| {
                builds.set(builds.get() + 1);
                Ok(fixture_snapshot(page_elements(page, 96, 0)?))
            })
        };
        // No client/query: neither construction nor repeated invalidation builds anything.
        for _ in 0..10 {
            cache.invalidate();
        }
        assert_eq!(builds.get(), 0);
        let first = read(Page::Personalization)?;
        for element in &first.elements {
            // Six properties plus a supported-pattern query for every element, repeatedly.
            for _ in 0..20 {
                for property in 0..7 {
                    let snapshot = read(Page::Personalization)?;
                    assert!(Rc::ptr_eq(&snapshot, &first));
                    let e = find(&snapshot, &element.id)?;
                    match property {
                        0 => assert_eq!(e.name, element.name),
                        1 => assert_eq!(e.control_type, element.control_type),
                        2 => assert_eq!(e.id, element.id),
                        3 => assert_eq!(e.offscreen, element.offscreen),
                        4 => assert_eq!(e.enabled, element.enabled),
                        5 => assert_eq!(e.focused, element.focused),
                        _ => assert_eq!(e.pattern, element.pattern),
                    }
                }
            }
        }
        assert_eq!(builds.get(), 1);
        cache.invalidate(); // The window's existing page-navigation repaint.
        assert_eq!(builds.get(), 1);
        let next = read(Page::Welcome)?;
        assert!(!Rc::ptr_eq(&next, &first));
        let stale = first
            .elements
            .iter()
            .find(|e| e.id == "Personalization.title")
            .unwrap();
        assert_eq!(
            find(&next, &stale.id).unwrap_err().code(),
            unavailable().code()
        );
        for _ in 0..1000 {
            assert!(Rc::ptr_eq(&read(Page::Welcome)?, &next));
        }
        assert_eq!(builds.get(), 2);
        Ok(())
    }

    #[test]
    fn window_invalidation_is_lazy_and_preserves_connection_identity_and_event_baseline()
    -> Result<()> {
        // Synthetic handles only: this test creates no window and calls no UI APIs.
        let hwnd = HWND(1usize as *mut _);
        invalidate(hwnd);
        assert!(CONNECTION.with(|c| c.borrow().is_none()));
        let cache = Rc::new(SnapshotCache::default());
        let first = cache.get_or_build(|| Ok(fixture_snapshot(vec![])))?;
        CONNECTION.with(|c| {
            *c.borrow_mut() = Some(Connection {
                hwnd,
                generation: 7,
                providers: HashMap::new(),
                pending: VecDeque::new(),
                ticket: 0,
                cache: cache.clone(),
                last: Some(first.clone()),
            });
        });
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                CONNECTION.with(|c| *c.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        invalidate(HWND(2usize as *mut _)); // A popup repaint cannot dirty Settings.
        assert!(Rc::ptr_eq(
            &first,
            &cache.get_or_build(|| panic!("wrong window"))?
        ));
        invalidate(hwnd);
        invalidate(hwnd); // Coalesced: no intermediate snapshot or connection generation.
        CONNECTION.with(|c| {
            let c = c.borrow();
            let c = c.as_ref().unwrap();
            assert_eq!(c.generation, 7);
            assert!(Rc::ptr_eq(c.last.as_ref().unwrap(), &first));
        });
        let next = cache.get_or_build(|| Ok(fixture_snapshot(vec![])))?;
        assert!(!Rc::ptr_eq(&first, &next));
        assert!(Rc::ptr_eq(
            &next,
            &cache.get_or_build(|| panic!("must reuse"))?
        ));
        Ok(())
    }

    #[test]
    fn snapshot_invalidated_during_build_is_not_published_and_errors_can_retry() -> Result<()> {
        let cache = SnapshotCache::default();
        assert_eq!(
            cache
                .get_or_build(|| {
                    cache.invalidate();
                    Ok(fixture_snapshot(vec![]))
                })
                .err()
                .unwrap()
                .code(),
            unavailable().code()
        );
        assert!(cache.value.borrow().is_none());
        assert!(cache.get_or_build(|| Err(unavailable())).is_err());
        assert!(cache.value.borrow().is_none());
        let snapshot = cache.get_or_build(|| Ok(fixture_snapshot(vec![])))?;
        assert!(Rc::ptr_eq(
            &snapshot,
            &cache.get_or_build(|| panic!("must reuse snapshot"))?
        ));
        Ok(())
    }

    use std::collections::HashSet;

    #[test]
    fn modal_wakeups_leave_pending_actions_for_outer_drain_in_order() {
        let mut pending = VecDeque::from([
            (1, "dropdown".into(), Command::Expand),
            (2, "next-control".into(), Command::Activate),
            (3, "last-control".into(), Command::Focus),
        ]);
        let first = take_pending(&mut pending, false).unwrap();
        assert_eq!(first.1, "dropdown");
        // Both wakeups are consumed by the dropdown's nested message loop.
        assert!(take_pending(&mut pending, true).is_none());
        assert!(take_pending(&mut pending, true).is_none());
        assert_eq!(pending.len(), 2);
        // No new wakeup is needed after the outer handler unwinds.
        assert_eq!(take_pending(&mut pending, false).unwrap().1, "next-control");
        assert_eq!(take_pending(&mut pending, false).unwrap().1, "last-control");
        assert!(take_pending(&mut pending, false).is_none());
    }

    #[test]
    fn displayed_save_captions_are_text_with_stable_identity_and_name_events() -> Result<()> {
        use crate::color_state::page::Caption;
        for page in [Page::Personalization, Page::Camouflage] {
            for dpi in [96, 240] {
                let mut elements = Vec::new();
                for caption in [Caption::SaveFailed, Caption::PreviewOnly] {
                    let shown =
                        window::displayed_caption(page, Some(&caption), "Engine notice").unwrap();
                    assert_eq!(shown, (caption.text(), caption.is_error()));
                    let e = build_elements(
                        vec![caption_element(
                            page,
                            &shown.0,
                            dpi,
                            sc(1000., dpi),
                            sc(760., dpi),
                        )],
                        scf(1000., dpi),
                        scf(760., dpi),
                        true,
                        None,
                    )
                    .remove(0);
                    assert_eq!(e.control_type, UIA_TextControlTypeId);
                    assert_eq!(e.name, caption.text());
                    assert_eq!(e.pattern, Pattern::None);
                    assert!(!e.focusable());
                    assert!(!e.offscreen);
                    assert_eq!(e.rect.top, (sc(760., dpi) - sc(26., dpi)) as f32);
                    elements.push(e);
                }
                assert_eq!(elements[0].id, elements[1].id);
                let changes = metadata_changes(&elements[0], &elements[1]);
                assert_eq!(changes.len(), 1);
                assert_eq!(changes[0].0, UIA_NamePropertyId);
                assert_eq!(
                    windows::core::BSTR::try_from(&changes[0].1)?.to_string(),
                    elements[0].name
                );
                assert_eq!(
                    windows::core::BSTR::try_from(&changes[0].2)?.to_string(),
                    elements[1].name
                );
                assert!(metadata_changes(&elements[1], &elements[1]).is_empty());
            }
        }
        assert_eq!(
            window::displayed_caption(Page::About, Some(&Caption::SaveFailed), "Engine notice"),
            Some(("Engine notice".into(), false))
        );
        assert_eq!(
            window::displayed_caption(Page::Personalization, None, "Engine notice"),
            Some(("Engine notice".into(), false))
        );
        assert_eq!(
            window::displayed_caption(Page::Personalization, None, ""),
            None
        );
        Ok(())
    }

    fn range_elements(enabled: bool, style: u32, dpi: u32) -> Result<Vec<Element>> {
        let settings = Settings {
            swipe_enabled: enabled,
            swipe_animation_style: style,
            ..Settings::default()
        };
        let (width, height) = (sc(1000., dpi), sc(760., dpi));
        Ok(build_elements(
            measure(
                &settings,
                &[],
                View {
                    page: Page::Pages,
                    hover: None,
                    scroll: 0,
                },
                dpi,
                width,
                height,
            )?,
            width as f32,
            height as f32,
            true,
            None,
        ))
    }

    #[test]
    fn ranges_gate_emits_enabled_events_without_identity_or_visibility_changes() -> Result<()> {
        for dpi in [96, 240] {
            let before = range_elements(true, 1, dpi)?;
            let after = range_elements(false, 1, dpi)?;
            assert_eq!(before.len(), after.len());
            let mut changed = 0;
            for (old, new) in before.iter().zip(&after) {
                assert_eq!(old.id, new.id);
                assert_eq!(old.offscreen, new.offscreen);
                let changes = metadata_changes(old, new);
                if old.enabled != new.enabled {
                    assert_eq!(changes.len(), 1);
                    assert_eq!(changes[0].0, UIA_IsEnabledPropertyId);
                    assert!(bool::try_from(&changes[0].1)?);
                    assert!(!bool::try_from(&changes[0].2)?);
                    let reverse = metadata_changes(new, old);
                    assert_eq!(reverse[0].0, UIA_IsEnabledPropertyId);
                    assert!(!bool::try_from(&reverse[0].1)?);
                    assert!(bool::try_from(&reverse[0].2)?);
                    changed += 1;
                } else {
                    assert!(changes.is_empty());
                }
            }
            assert!(changed >= 3);
        }
        Ok(())
    }

    #[test]
    fn disabled_animation_segments_keep_the_painted_selection_and_identity() -> Result<()> {
        for dpi in [96, 240] {
            for style in 0..=2 {
                let before = range_elements(true, style, dpi)?;
                let after = range_elements(false, style, dpi)?;
                let segments: Vec<_> = after
                    .iter()
                    .filter(|e| matches!(e.hit, Hit::AnimationMode(_)))
                    .collect();
                assert_eq!(segments.len(), 3);
                for (value, e) in segments.iter().enumerate() {
                    let old = before.iter().find(|old| old.id == e.id).unwrap();
                    assert_eq!(old.hit, e.hit);
                    assert_eq!(old.pattern, e.pattern);
                    assert_eq!(e.pattern, Pattern::Select(value == style as usize));
                    assert_eq!(e.control_type, UIA_RadioButtonControlTypeId);
                    assert!(!e.enabled);
                    assert!(!e.focusable());
                }
            }
        }
        Ok(())
    }

    fn page_elements(page: Page, dpi: u32, scroll: i32) -> Result<Vec<Element>> {
        let settings = Settings::default();
        look_pages::set_scope(None, vec![], false);
        look_pages::set_effective(&crate::color_state::effective_of(
            &settings,
            &Default::default(),
        ));
        let (w, h) = (sc(1000., dpi), sc(760., dpi));
        Ok(build_elements(
            measure(
                &settings,
                &[],
                View {
                    page,
                    hover: None,
                    scroll,
                },
                dpi,
                w,
                h,
            )?,
            w as f32,
            h as f32,
            true,
            None,
        ))
    }
    fn verify_page(page: Page) -> Result<()> {
        for dpi in [96, 240] {
            let elements = page_elements(page, dpi, 0)?;
            let ids: HashSet<_> = elements.iter().map(|e| &e.id).collect();
            assert_eq!(
                ids.len(),
                elements.len(),
                "duplicate identity on {page:?} at {dpi}"
            );
            assert!(elements.iter().all(|e| !e.name.trim().is_empty()));
            for (i, p) in Page::ALL.iter().enumerate() {
                let (x, y, w, h) = nav_item_rect(i, dpi, sc(760., dpi));
                assert_eq!(elements[i].name, p.label());
                assert_eq!(
                    elements[i].rect,
                    RectF::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32)
                );
                assert_eq!(elements[i].control_type, UIA_ListItemControlTypeId);
            }
            let settings = Settings::default();
            let targets =
                focus_targets_with(&settings, &[], page, dpi, sc(1000., dpi), sc(760., dpi), 0)?;
            // Every enabled keyboard control must have the same geometry in the UIA tree.
            for (hit, r) in targets {
                let exact = elements.iter().any(|e| e.hit == hit && e.rect == r);
                // Ordinary segmented rows have one keyboard stop over the whole group;
                // UIA exposes each equal hit segment as a selectable radio item.
                let split = elements
                    .iter()
                    .find(|e| e.hit == hit && matches!(e.pattern, Pattern::Select(_)))
                    .map(|e| e.id.rsplit_once(".control.").map(|x| x.0).unwrap_or(&e.id))
                    .is_some_and(|id| {
                        let parts: Vec<_> = elements
                            .iter()
                            .filter(|e| e.id.starts_with(&format!("{id}.control.")))
                            .collect();
                        parts.len() > 1
                            && parts
                                .iter()
                                .all(|e| e.rect.top == r.top && e.rect.bottom == r.bottom)
                            && (parts.first().unwrap().rect.left - r.left).abs() < 0.01
                            && (parts.last().unwrap().rect.right - r.right).abs() < 0.01
                    });
                assert!(
                    exact || split,
                    "missing painted focus target {page:?}: {hit:?} {r:?}"
                );
            }
            for e in elements.iter().filter(|e| e.hit != Hit::Nothing) {
                match e.pattern {
                    Pattern::Toggle(_) => assert_eq!(e.control_type, UIA_CheckBoxControlTypeId),
                    Pattern::Select(_) => assert_eq!(e.control_type, UIA_RadioButtonControlTypeId),
                    Pattern::Range { .. } => assert!(matches!(
                        e.control_type,
                        UIA_SliderControlTypeId | UIA_SpinnerControlTypeId
                    )),
                    _ => {}
                }
            }
        }
        Ok(())
    }
    macro_rules! page_test {
        ($name:ident,$page:ident) => {
            #[test]
            fn $name() -> Result<()> {
                verify_page(Page::$page)
            }
        };
    }
    page_test!(welcome, Welcome);
    page_test!(personalization, Personalization);
    page_test!(camouflage, Camouflage);
    page_test!(folder_gates, FolderPortals);
    page_test!(tabs, Tabs);
    page_test!(fold, Rollup);
    page_test!(clear_view, Hide);
    page_test!(glance, Peek);
    page_test!(ranges, Pages);
    page_test!(round_up, Rules);
    page_test!(layout, Layout);
    page_test!(backups_page, Backups);
    page_test!(about, About);

    #[test]
    fn measured_enabled_controls_have_existing_input_paths() -> Result<()> {
        let settings = Settings::default();
        look_pages::set_scope(None, vec![], false);
        look_pages::set_effective(&crate::color_state::effective_of(
            &settings,
            &Default::default(),
        ));
        for page in Page::ALL {
            for dpi in [96, 240] {
                let measured = measure(
                    &settings,
                    &[],
                    View {
                        page,
                        hover: None,
                        scroll: 0,
                    },
                    dpi,
                    sc(1000., dpi),
                    sc(760., dpi),
                )?;
                for m in measured.iter().filter(|m| {
                    m.enabled
                        && matches!(
                            m.kind,
                            RowKind::Button
                                | RowKind::ButtonRow
                                | RowKind::Toggle
                                | RowKind::CheckRow
                                | RowKind::RadioRow
                                | RowKind::Dropdown
                                | RowKind::Stepper
                                | RowKind::ValueSlider
                        )
                }) {
                    assert_ne!(
                        m.pattern,
                        Pattern::None,
                        "read-only control {page:?}: {}",
                        m.name
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn fragment_navigation_is_flat_in_painted_reading_order() -> Result<()> {
        for page in Page::ALL {
            for dpi in [96, 240] {
                let elements = page_elements(page, dpi, 0)?;
                let ids: Vec<_> = elements.iter().map(|e| e.id.clone()).collect();
                assert_eq!(
                    relative(&ids, ROOT, NavigateDirection_FirstChild),
                    ids.first().cloned()
                );
                assert_eq!(
                    relative(&ids, ROOT, NavigateDirection_LastChild),
                    ids.last().cloned()
                );
                for (i, id) in ids.iter().enumerate() {
                    assert_eq!(
                        relative(&ids, id, NavigateDirection_Parent),
                        Some(ROOT.into())
                    );
                    assert_eq!(
                        relative(&ids, id, NavigateDirection_NextSibling),
                        ids.get(i + 1).cloned()
                    );
                    assert_eq!(
                        relative(&ids, id, NavigateDirection_PreviousSibling),
                        i.checked_sub(1).and_then(|i| ids.get(i)).cloned()
                    );
                    assert_eq!(relative(&ids, id, NavigateDirection_FirstChild), None);
                }
            }
        }
        Ok(())
    }
    #[test]
    fn scroll_keeps_identity_and_geometry_and_marks_offscreen() -> Result<()> {
        for dpi in [96, 240] {
            let before = page_elements(Page::Personalization, dpi, 0)?;
            let scroll = sc(600., dpi);
            let after = page_elements(Page::Personalization, dpi, scroll)?;
            assert_eq!(
                before.iter().map(|e| &e.id).collect::<Vec<_>>(),
                after.iter().map(|e| &e.id).collect::<Vec<_>>()
            );
            for (a, b) in before.iter().zip(&after).skip(Page::ALL.len()) {
                // The painter rounds negative half-pixel row tops away from zero.
                assert!(
                    (a.rect.top - scroll as f32 - b.rect.top).abs() <= 1.01,
                    "{} at {dpi}: {:?} -> {:?}",
                    a.id,
                    a.rect,
                    b.rect
                );
                assert_eq!(a.rect.right - a.rect.left, b.rect.right - b.rect.left);
            }
            assert!(after.iter().skip(Page::ALL.len()).any(|e| e.offscreen));
        }
        Ok(())
    }
    #[test]
    fn pure_builder_handles_disabled_hidden_focus_and_all_control_types() {
        let kinds = [
            (RowKind::Toggle, UIA_CheckBoxControlTypeId),
            (RowKind::CheckRow, UIA_CheckBoxControlTypeId),
            (RowKind::Check, UIA_CheckBoxControlTypeId),
            (RowKind::Radio, UIA_RadioButtonControlTypeId),
            (RowKind::RadioRow, UIA_RadioButtonControlTypeId),
            (RowKind::Segmented, UIA_RadioButtonControlTypeId),
            (RowKind::Dropdown, UIA_ComboBoxControlTypeId),
            (RowKind::Combo, UIA_ComboBoxControlTypeId),
            (RowKind::Stepper, UIA_SpinnerControlTypeId),
            (RowKind::Number, UIA_SpinnerControlTypeId),
            (RowKind::ValueSlider, UIA_SliderControlTypeId),
            (RowKind::Link, UIA_HyperlinkControlTypeId),
            (RowKind::Navigate, UIA_HyperlinkControlTypeId),
            (RowKind::Button, UIA_ButtonControlTypeId),
            (RowKind::ButtonRow, UIA_ButtonControlTypeId),
            (RowKind::Status, UIA_CheckBoxControlTypeId),
            (RowKind::NumberedStep, UIA_TextControlTypeId),
            (RowKind::ExploreTile, UIA_ButtonControlTypeId),
            (RowKind::TabStyleTile, UIA_RadioButtonControlTypeId),
            (RowKind::Color, UIA_ButtonControlTypeId),
            (RowKind::Expander, UIA_ButtonControlTypeId),
            (RowKind::InfoBox, UIA_TextControlTypeId),
            (RowKind::Label, UIA_TextControlTypeId),
            (RowKind::Note, UIA_TextControlTypeId),
            (RowKind::Tiles, UIA_TextControlTypeId),
        ];
        let measured = kinds
            .iter()
            .enumerate()
            .map(|(i, (k, _))| {
                Measured::new(
                    i.to_string(),
                    "Label",
                    RectF::new(0., -100., 50., -50.),
                    Hit::Toggle(Field::SnapEnabled),
                    *k,
                )
            })
            .collect();
        let elements = build_elements(
            measured,
            100.,
            100.,
            false,
            Some(Hit::Toggle(Field::SnapEnabled)),
        );
        for (e, (_, expected)) in elements.iter().zip(kinds) {
            assert_eq!(e.control_type, expected);
            assert!(e.offscreen);
            assert!(e.focused);
        }
        let mut m = Measured::new(
            "disabled".into(),
            "Disabled",
            RectF::new(0., 0., 50., 50.),
            Hit::Spacing,
            RowKind::Stepper,
        );
        m.enabled = false;
        let e = build_elements(vec![m], 100., 100., true, Some(Hit::Spacing)).remove(0);
        assert!(!e.focusable());
        assert!(!e.focused);
        assert!(!e.offscreen);
    }
    #[test]
    fn stale_provider_and_reentrant_query_return_element_not_available() -> Result<()> {
        let p = make_provider(Provider {
            hwnd: HWND::default(),
            generation: u64::MAX,
            id: "gone".into(),
        });
        assert_eq!(
            unsafe { p.GetPropertyValue(UIA_NamePropertyId) }
                .unwrap_err()
                .code(),
            unavailable().code()
        );
        let f: IRawElementProviderFragment = p.cast()?;
        assert_eq!(
            unsafe { f.BoundingRectangle() }.unwrap_err().code(),
            unavailable().code()
        );
        let busy = enter();
        assert_eq!(
            snapshot(HWND::default()).err().unwrap().code(),
            unavailable().code()
        );
        drop(busy);
        Ok(())
    }
    #[test]
    fn nullable_com_results_initialise_the_out_pointer() {
        let mut out = 1usize as *mut std::ffi::c_void;
        let hr = unsafe { interface_output::<IUnknown>(&mut out, absent) };
        assert_eq!(hr, HRESULT(0));
        assert!(out.is_null());
        let hr = unsafe { interface_output::<IUnknown>(std::ptr::null_mut(), absent) };
        assert_eq!(hr, E_POINTER);
    }

    #[test]
    fn every_interface_retains_one_sta_identity_and_rejects_agility() -> Result<()> {
        let p = make_provider(Provider {
            hwnd: HWND::default(),
            generation: 1,
            id: ROOT.into(),
        });
        let expected = p.cast::<IUnknown>()?;
        let interfaces = vec![
            p.cast::<IUnknown>()?,
            p.cast::<IRawElementProviderFragment>()?.cast()?,
            p.cast::<IRawElementProviderFragmentRoot>()?.cast()?,
            p.cast::<IInvokeProvider>()?.cast()?,
            p.cast::<IToggleProvider>()?.cast()?,
            p.cast::<IRangeValueProvider>()?.cast()?,
            p.cast::<ISelectionItemProvider>()?.cast()?,
            p.cast::<IExpandCollapseProvider>()?.cast()?,
            p.cast::<IValueProvider>()?.cast()?,
        ];
        for interface in interfaces {
            assert_eq!(interface.as_raw(), expected.as_raw());
            let mut out = 1usize as *mut std::ffi::c_void;
            let hr = unsafe {
                (interface.vtable().QueryInterface)(interface.as_raw(), &AGILE, &mut out)
            };
            assert_eq!(hr, windows::Win32::Foundation::E_NOINTERFACE);
            assert!(out.is_null());
        }
        let options = unsafe { p.ProviderOptions() }?;
        assert_ne!(options.0 & ProviderOptions_UseComThreading.0, 0);
        assert_eq!(
            std::mem::size_of::<SimpleAbi_Vtbl>(),
            std::mem::size_of::<IRawElementProviderSimple_Vtbl>()
        );
        assert_eq!(
            std::mem::size_of::<FragmentAbi_Vtbl>(),
            std::mem::size_of::<IRawElementProviderFragment_Vtbl>()
        );
        assert_eq!(
            std::mem::size_of::<RootAbi_Vtbl>(),
            std::mem::size_of::<IRawElementProviderFragmentRoot_Vtbl>()
        );
        assert_eq!(
            std::mem::size_of::<SelectionAbi_Vtbl>(),
            std::mem::size_of::<ISelectionItemProvider_Vtbl>()
        );
        Ok(())
    }
    #[test]
    fn stale_identity_is_not_retargeted_to_another_row() -> Result<()> {
        let elements = page_elements(Page::Welcome, 96, 0)?;
        let snapshot = Snapshot {
            elements,
            origin: POINT::default(),
            width: 1000,
            height: 760,
            visible: true,
        };
        assert_eq!(
            find(&snapshot, "missing").unwrap_err().code(),
            unavailable().code()
        );
        Ok(())
    }
    #[test]
    fn range_and_selection_commands_use_only_existing_input_operations() {
        let mut m = Measured::new(
            "range".into(),
            "Gap",
            RectF::new(0., 0., 100., 32.),
            Hit::Spacing,
            RowKind::Stepper,
        );
        m.pattern = Pattern::Range {
            value: 5.,
            max: 100.,
        };
        let e = build_elements(vec![m], 100., 100., true, None).remove(0);
        let mut keys = Vec::new();
        let mut clicks = Vec::new();
        run_command(
            HWND::default(),
            &e,
            Command::Range(8.),
            |key| keys.push(key),
            |point| clicks.push(point),
        );
        assert_eq!(keys, vec![0x26; 3]);
        assert!(clicks.is_empty());
        run_command(
            HWND::default(),
            &e,
            Command::Activate,
            |_| {},
            |point| clicks.push(point),
        );
        assert_eq!(clicks, vec![(50, 16)]);
    }
}
