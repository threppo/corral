//! Settings window: left navigation, page header, card layout and bound controls.
//!
//! Drawing, layout, hit testing and click application share the approved Corral design in
//! `docs/design/settings-redesign-spec.md`. The Win32 shell in `window.rs` supplies input and
//! retained state. Unconverted pages retain their setting fields/gates through the default kit
//! mapping; the dynamic Round-up page still has its own renderer in `rules_page.rs`.
//!
//! Nav icon glyphs come from "Segoe Fluent Icons" (file `SegoeIcons.ttf`), falling back to
//! "Segoe MDL2 Assets" (`segmdl2.ttf`) when that file is absent. Codepoint table (private use area,
//! all present in both fonts):
//!
//! | Page | Codepoint | Fluent name |
//! |---|---|---|
//! | Welcome | U+E80F | Home |
//! | Camouflage | U+E790 | Color |
//! | Personalization | U+E771 | Personalize |
//! | FolderPortals | U+E838 | FolderOpen |
//! | Peek | U+E890 | View |
//! | Hide | U+ED1A | Hide |
//! | Rollup | U+E70E | ChevronUp |
//! | Tabs | U+E8A1 | (tabbed window glyph, see QA PNG) |
//! | Pages | U+E7C4 | TaskView |
//! | Rules | U+E8CB | Sort |
//! | Layout | U+E80A | GridView |
//! | Backups | U+E81C | History |
//! | About | U+E946 | Info |
//!
//! All 13 codepoints were checked in both installed font cmap tables on 2026-10-16.

#![allow(dead_code)]

mod appearance_reset;
mod backup_dialog;
mod backups;
mod camouflage_options;
mod camouflage_preview;
pub(super) mod color_popover;
mod content;
mod desk_setups;
mod desktop_pages;
mod help;
pub mod kit;
pub(super) mod look_pages;
mod rules_dialog;
mod rules_page;
pub(super) mod sample_corral;
mod style_menus;
mod window;

pub use rules_page::GroupInfo;
pub use window::run_settings_window;

use std::path::Path;

use windows::core::Result;

use crate::render::{Align, RectF, Rgba, Surface, TextStyle, measure_text};
use crate::settings::{Field, Settings};
use content::{Gate, Row, RowKind, combo_choices, content};

/// Nav pages in display order. Business/Licensing/Deployment are omitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Page {
    Welcome,
    Camouflage,
    Personalization,
    FolderPortals,
    Peek,
    Hide,
    Rollup,
    Tabs,
    Pages,
    Rules,
    Layout,
    Backups,
    About,
}

impl Page {
    pub const ALL: [Page; 13] = [
        Page::Welcome,
        Page::Personalization,
        Page::Camouflage,
        Page::FolderPortals,
        Page::Tabs,
        Page::Rollup,
        Page::Hide,
        Page::Peek,
        Page::Pages,
        Page::Rules,
        Page::Layout,
        Page::Backups,
        Page::About,
    ];

    /// English nav label.
    pub fn label(self) -> &'static str {
        match self {
            Page::Welcome => "Welcome",
            Page::Camouflage => "Camouflage",
            Page::Personalization => "Personalization",
            Page::FolderPortals => "Folder Gates",
            Page::Peek => "Glance",
            Page::Hide => "Clear view",
            Page::Rollup => "Fold",
            Page::Tabs => "Tabs",
            Page::Pages => "Ranges",
            Page::Rules => "Round-up",
            Page::Layout => "Layout",
            Page::Backups => "Backups",
            Page::About => "About",
        }
    }

    /// Page by its English name (nav label without the trademark sign), for the `--page` option.
    pub fn from_name(name: &str) -> Option<Page> {
        let name = name.trim();
        Page::ALL.iter().copied().find(|p| {
            p.label()
                .trim_end_matches('\u{2122}')
                .eq_ignore_ascii_case(name)
                || format!("{p:?}").eq_ignore_ascii_case(name)
        })
    }

    /// Icon glyph (see the table in the module docs).
    pub fn glyph(self) -> char {
        match self {
            Page::Welcome => '\u{E80F}',
            Page::Camouflage => '\u{E790}',
            Page::Personalization => '\u{E771}',
            Page::FolderPortals => '\u{E838}',
            Page::Peek => '\u{E890}',
            Page::Hide => '\u{ED1A}',
            Page::Rollup => '\u{E70E}',
            Page::Tabs => '\u{E8A1}',
            Page::Pages => '\u{E7C4}',
            Page::Rules => '\u{E8CB}',
            Page::Layout => '\u{E80A}',
            Page::Backups => '\u{E81C}',
            Page::About => '\u{E946}',
        }
    }

    fn index(self) -> usize {
        Page::ALL.iter().position(|p| *p == self).unwrap_or(0)
    }
}

/// Compatibility field names derived from the canonical approved kit Theme.
#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub light: bool,
    /// NavPanelBackground (non-acrylic): window / nav / page background.
    pub panel_bg: Rgba,
    pub nav_text: Rgba,
    pub nav_text_disabled: Rgba,
    pub nav_selected: Rgba,
    pub nav_hover: Rgba,
    pub header_text: Rgba,
    pub sub_header_text: Rgba,
    pub sub_header_text2: Rgba,
    pub group_fill: Rgba,
    pub group_outline: Rgba,
    pub control_border: Rgba,
    /// Hyperlink colour. Dark: (132,115,200); light: derived, unverified.
    pub link: Rgba,
    /// Hint / footnote text (NavLabelTextDisabledSolid: (108,108,108) light, (127,127,127) dark; frame 043 shows 128).
    pub note_text: Rgba,
    /// Unchecked check box fill (frame 040: (243,243,243) in the dark theme).
    pub check_fill: Rgba,
    /// SystemAccent; Windows accent fallback (0,120,215).
    pub accent: Rgba,
}

impl Theme {
    pub const FALLBACK_ACCENT: Rgba = Rgba::new(143, 91, 34, 255);

    pub fn light() -> Theme {
        Self::from_tokens(kit::theme::Theme::new(true))
    }
    pub fn dark() -> Theme {
        Self::from_tokens(kit::theme::Theme::new(false))
    }
    fn from_tokens(t: kit::theme::Theme) -> Self {
        Self {
            light: t.light,
            panel_bg: t.window,
            nav_text: t.text,
            nav_text_disabled: t.disabled,
            nav_selected: t.subtle_selected,
            nav_hover: t.subtle_hover,
            header_text: t.text,
            sub_header_text: t.secondary,
            sub_header_text2: t.secondary,
            group_fill: t.surface,
            group_outline: t.border,
            control_border: t.border,
            link: t.link,
            note_text: t.secondary,
            check_fill: t.control,
            accent: t.accent,
        }
    }

    pub fn with_accent(self, _accent: Rgba) -> Theme {
        self
    }
}

// ---------------------------------------------------------------------------------------------
// Geometry (shared by drawing and hit testing)

fn sc(v: f32, dpi: u32) -> i32 {
    (v * dpi as f32 / 96.0).round() as i32
}

fn scf(v: f32, dpi: u32) -> f32 {
    v * dpi as f32 / 96.0
}

/// Approved fixed 256-DIP navigation column.
fn nav_width(dpi: u32) -> i32 {
    sc(256.0, dpi)
}

/// Figma navigation: 36-DIP rows and group headings with 2-DIP gaps; compact on short clients.
fn nav_item_rect(index: usize, dpi: u32, height: i32) -> (i32, i32, i32, i32) {
    let height_dip = height as f32 * 96.0 / dpi.max(1) as f32;
    let short = height_dip < 488.0;
    let (bottom, about) = if short { (8.0, 8.0) } else { (16.0, 12.0) };
    let group_h = if short {
        ((height_dip - 368.0) / 3.0).clamp(20.0, 36.0)
    } else {
        36.0
    };
    let fixed = 8.0 + bottom + about + 16.0 * 2.0 + 3.0 * group_h;
    let row_h = ((height_dip - fixed) / Page::ALL.len() as f32).clamp(24.0, 36.0);
    let groups = usize::from(index >= 1) + usize::from(index >= 3) + usize::from(index >= 9);
    let about_gap = usize::from(index >= 12);
    let top = 8.0
        + index as f32 * row_h
        + groups as f32 * group_h
        + (index + groups + about_gap) as f32 * 2.0
        + about_gap as f32 * about;
    let y = sc(top, dpi);
    (sc(16.0, dpi), y, sc(224.0, dpi), sc(top + row_h, dpi) - y)
}

/// Which nav item (if any) contains `point` (client coordinates).
pub fn nav_hit_test(point: (i32, i32), dpi: u32, height: i32) -> Option<Page> {
    Page::ALL.iter().enumerate().find_map(|(i, page)| {
        let (x, y, w, h) = nav_item_rect(i, dpi, height);
        (point.0 >= x && point.0 < x + w && point.1 >= y && point.1 < y + h).then_some(*page)
    })
}

fn icon_family() -> &'static str {
    if Path::new(r"C:\Windows\Fonts\SegoeIcons.ttf").exists() {
        "Segoe Fluent Icons"
    } else {
        "Segoe MDL2 Assets"
    }
}

/// Point size to pixel size at `dpi`.
fn pt_px(pt: f32, dpi: u32) -> f32 {
    pt * dpi as f32 / 72.0
}

fn style<'a>(
    family: &'a str,
    size_px: f32,
    weight: u16,
    color: Rgba,
    align: Align,
) -> TextStyle<'a> {
    TextStyle {
        family,
        size_px,
        weight,
        color,
        align,
        shadow: None,
    }
}

const UI: &str = "Segoe UI Variable";
/// Page sub-header size.
const SUB_PT: f32 = 10.5;

/// Greedy word wrap using DirectWrite measurement.
fn wrap(text: &str, size_px: f32, weight: u16, max_w: f32) -> Result<Vec<String>> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let cand = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{cur} {word}")
        };
        if cur.is_empty()
            || measure_text(&cand, kit::theme::font_family(), size_px, weight)?.0 <= max_w
        {
            cur = cand;
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    Ok(lines)
}

// ---------------------------------------------------------------------------------------------
// Drawing

/// What the window shows besides the settings values.
#[derive(Clone, Copy, Debug)]
pub struct View {
    pub page: Page,
    pub hover: Option<Page>,
    /// Vertical scroll offset of the page content in pixels (>= 0).
    pub scroll: i32,
}

/// Draw the whole settings client area into `surface` with default settings values and no scroll.
pub fn render_settings(
    surface: &mut Surface,
    page: Page,
    hover: Option<Page>,
    theme: &Theme,
    dpi: u32,
) -> Result<()> {
    render_settings_state(
        surface,
        &View {
            page,
            hover,
            scroll: 0,
        },
        &Settings::default(),
        theme,
        dpi,
    )
}

/// Draw the whole settings client area (size = client size at `dpi`) with real settings state.
pub fn render_settings_state(
    surface: &mut Surface,
    view: &View,
    settings: &Settings,
    theme: &Theme,
    dpi: u32,
) -> Result<()> {
    render_settings_groups(surface, view, settings, &[], theme, dpi)
}

/// As [`render_settings_state`]; `groups` are the Corrals of the current layout (Rules page).
pub fn render_settings_groups(
    surface: &mut Surface,
    view: &View,
    settings: &Settings,
    groups: &[GroupInfo],
    theme: &Theme,
    dpi: u32,
) -> Result<()> {
    render_settings_editor(surface, view, settings, groups, theme, dpi, None)
}

/// The settings window supplies its retained HSV state; offscreen callers may use stored RGB.
pub fn render_settings_editor(
    surface: &mut Surface,
    view: &View,
    settings: &Settings,
    groups: &[GroupInfo],
    theme: &Theme,
    dpi: u32,
    editor: Option<&crate::color_state::Effective>,
) -> Result<()> {
    if let Some(effective) = editor {
        look_pages::set_effective(effective);
    } else {
        look_pages::set_effective(&crate::color_state::effective_of(settings, &Default::default()));
    }
    let width = surface.width() as f32;
    let height = surface.height() as f32;
    if view.page == Page::Rules {
        let lay = rules_page::layout(settings, groups, dpi, width, height, view.scroll)?;
        return surface.draw(|ctx| {
            ctx.clear(theme.panel_bg);
            rules_page::draw(ctx, &lay, theme, dpi)?;
            ctx.fill_rect(
                RectF::new(0.0, 0.0, nav_width(dpi) as f32, height),
                theme.panel_bg,
            )?;
            draw_nav(ctx, view.page, view.hover, theme, dpi, height as i32)?;
            draw_focus_ring(ctx, theme, dpi)
        });
    }
    let lay = layout_page(view.page, settings, dpi, width, height, view.scroll)?;
    if matches!(view.page, Page::Personalization | Page::Camouflage) {
        LOOK_CACHE.with(|cache| {
            *cache.borrow_mut() = Some(LookCache {
                page: view.page,
                dpi,
                width: surface.width(),
                height: surface.height(),
                scroll: view.scroll,
                rows: lay
                    .sections
                    .iter()
                    .flat_map(|s| s.rows.iter().cloned())
                    .collect(),
                preview: lay.preview,
            })
        });
    }

    surface.draw(|ctx| {
        ctx.clear(theme.panel_bg);
        draw_page(ctx, &lay, theme, dpi, settings, editor)?;
        if view.page == Page::Camouflage {
            camouflage_preview::draw(ctx, &lay, settings, theme, dpi)?;
        }
        // The nav column is opaque and drawn last so scrolled content never shows through it.
        ctx.fill_rect(
            RectF::new(0.0, 0.0, nav_width(dpi) as f32, height),
            theme.panel_bg,
        )?;
        draw_nav(ctx, view.page, view.hover, theme, dpi, height as i32)?;
        draw_focus_ring(ctx, theme, dpi)
    })
}

fn draw_focus_ring(ctx: &crate::render::Ctx, theme: &Theme, dpi: u32) -> Result<()> {
    FOCUS_RING.with(|f| {
        if let Some(r) = f.get() {
            let m = scf(3.0, dpi);
            ctx.stroke_rounded_rect(
                RectF::new(r.left - m, r.top - m, r.right + m, r.bottom + m),
                scf(7.0, dpi),
                scf(2.0, dpi),
                kit::theme::Theme::new(theme.light).focus,
            )
        } else {
            Ok(())
        }
    })
}

fn draw_nav(
    ctx: &crate::render::Ctx,
    selected: Page,
    hover: Option<Page>,
    theme: &Theme,
    dpi: u32,
    height: i32,
) -> Result<()> {
    let icons = icon_family();
    for (index, label) in [(1, "Look"), (3, "Desktop"), (9, "Organize")] {
        let (_, y, _, _) = nav_item_rect(index, dpi, height);
        ctx.draw_text_cached(
            label,
            RectF::new(
                scf(28.0, dpi),
                y as f32 - scf(22.0, dpi),
                scf(240.0, dpi),
                y as f32 - scf(6.0, dpi),
            ),
            &style(
                kit::theme::font_family(),
                scf(12.0, dpi),
                600,
                theme.sub_header_text,
                Align::Left,
            ),
        )?;
    }
    for (i, page) in Page::ALL.iter().enumerate() {
        let (x, y, w, h) = nav_item_rect(i, dpi, height);
        let rect = RectF::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32);
        let is_sel = *page == selected;
        if is_sel {
            ctx.fill_rounded_rect(rect, scf(4.0, dpi), theme.nav_selected)?;
        } else if hover == Some(*page) {
            ctx.fill_rounded_rect(rect, scf(4.0, dpi), theme.nav_hover)?;
        }
        // 16 px icon at x = 12, vertically centred (2.1).
        let icon_px = scf(16.0, dpi);
        let ix = x as f32 + scf(12.0, dpi);
        let icon_rect = RectF::new(ix, rect.top, ix + icon_px, rect.bottom);
        let glyph = page.glyph().to_string();
        ctx.draw_text_cached(
            &glyph,
            icon_rect,
            &style(icons, icon_px, 400, theme.nav_text, Align::Center),
        )?;
        // Selection accent bar stays 3 x 16 DIP even when short-window rows compact.
        if is_sel {
            let bar_h = scf(16.0, dpi);
            let top = rect.top + (h as f32 - bar_h) / 2.0;
            ctx.fill_rounded_rect(
                RectF::new(rect.left, top, rect.left + scf(3.0, dpi), top + bar_h),
                scf(1.25, dpi),
                theme.accent,
            )?;
        }
        // Approved navigation: 12 DIP padding + 16 DIP glyph + 12 DIP gap.
        let tx = x as f32 + scf(40.0, dpi);
        let text_rect = RectF::new(
            tx,
            rect.top - scf(1.0, dpi),
            rect.right,
            rect.bottom - scf(1.0, dpi),
        );
        ctx.draw_text_cached(
            page.label(),
            text_rect,
            &style(
                kit::theme::font_family(),
                pt_px(10.5, dpi),
                if is_sel { 600 } else { 400 },
                theme.nav_text,
                Align::Left,
            ),
        )?;
    }
    Ok(())
}

// Measured from 96-dpi reference frames: check rows are
// 23-24 px apart, card padding 8-9 px, 12 px from the card edge to the box.
const ROW_H: f32 = 23.5;
const COMBO_H: f32 = 30.0;
const NOTE_H: f32 = 20.0;
const CARD_PAD_X: f32 = 12.0;
const CARD_PAD_Y: f32 = 8.5;
/// Welcome descriptions wrap ~52 px before the card edge (frame 002: wraps before "or detach.").
const DESC_RIGHT_PAD: f32 = 38.0;
const JOIN_GAP: f32 = 2.0;
const PAGE_BOTTOM_PAD: f32 = 40.0; // Approved content bottom padding.

// ---------------------------------------------------------------------------------------------
// Layout (shared by drawing, hit testing and scroll range)

/// Visible/enabled state of a row from its gates against the settings.
fn row_state(row: &Row, settings: &Settings) -> (bool, bool) {
    gates_state(row.gates, settings)
}

/// Visible/enabled state from a gate list (rows and whole sections).
fn gates_state(gates: &[Gate], settings: &Settings) -> (bool, bool) {
    let mut visible = true;
    let mut enabled = true;
    for gate in gates {
        match *gate {
            Gate::LayoutAuto => visible &= settings.layout_auto_options_visible(),
            Gate::DeskSetups => visible &= settings.get(Field::LayoutPreserve) == 2,
            Gate::MultiMonitor => {
                visible &= settings.multi_monitor_options_visible(monitor_count())
            }
            Gate::Show(f) => visible &= settings.is_on(f),
            Gate::Enable(f) => enabled &= settings.is_on(f),
            Gate::EnableNot(f) => enabled &= !settings.is_on(f),
            Gate::BuildAtLeast(n) => visible &= content::windows_build() >= n,
            Gate::BuildBelow(n) => visible &= content::windows_build() < n,
        }
    }
    (visible, enabled)
}

#[derive(Clone)]
struct RowLay {
    row: Row,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    enabled: bool,
}

#[derive(Clone)]
struct SecLay {
    header: &'static str,
    header_rect: RectF,
    card: RectF,
    joined: bool,
    rows: Vec<RowLay>,
}

struct PageLay {
    title: &'static str,
    title_rect: RectF,
    title_px: f32,
    subtitle: Vec<(String, RectF)>,
    sections: Vec<SecLay>,
    /// Camouflage live-preview box (aspect-matched to the primary monitor).
    preview: Option<RectF>,
    scope: Option<look_pages::ScopeLay>,
    preview_caption: Vec<(String, RectF)>,
    /// Bottom edge of the content including the bottom padding (in the scrolled frame).
    bottom: f32,
    master: Option<(Field, RectF)>,
    page: Page,
}

#[derive(Clone)]
struct LookCache {
    page: Page,
    dpi: u32,
    width: u32,
    height: u32,
    scroll: i32,
    rows: Vec<RowLay>,
    preview: Option<RectF>,
}
thread_local! {
    static EXPANDERS: std::cell::RefCell<std::collections::HashMap<(Page, &'static str),bool>> = std::cell::RefCell::new(std::collections::HashMap::new());
    static CONTROL_POINTER: std::cell::Cell<Option<(f32,f32,bool)>> = const { std::cell::Cell::new(None) };
    static FOCUS_RING: std::cell::Cell<Option<RectF>> = const { std::cell::Cell::new(None) };
    static LOOK_CACHE:std::cell::RefCell<Option<LookCache>> = const {std::cell::RefCell::new(None)};
}
pub fn set_control_pointer(point: Option<(f32, f32)>, pressed: bool) {
    CONTROL_POINTER.with(|p| p.set(point.map(|(x, y)| (x, y, pressed))));
}
pub fn set_focus_ring(rect: Option<RectF>) {
    FOCUS_RING.with(|r| r.set(rect));
}
pub fn toggle_expander(page: Page, text: &'static str) {
    let default = content(page)
        .sections
        .iter()
        .flat_map(|s| &s.rows)
        .find(|r| r.text == text)
        .is_some_and(|r| r.expanded);
    EXPANDERS.with(|e| {
        let mut e = e.borrow_mut();
        let v = e.entry((page, text)).or_insert(default);
        *v = !*v;
    });
}
fn expander_open(page: Page, row: &Row) -> bool {
    EXPANDERS.with(|e| {
        e.borrow()
            .get(&(page, row.text))
            .copied()
            .unwrap_or(row.expanded)
    })
}
fn control_width(row: &Row, dpi: u32) -> Result<f32> {
    Ok(match row.control_kind() {
        RowKind::Toggle => scf(40.0, dpi),
        RowKind::Dropdown => scf(220.0, dpi),
        RowKind::Button | RowKind::ButtonRow => (measure_text(
            row.button_text.unwrap_or(row.text),
            kit::theme::font_family(),
            scf(14.0, dpi),
            400,
        )?
        .0 + scf(24.0, dpi))
        .max(scf(80.0, dpi)),
        RowKind::Segmented => {
            let labels: Vec<f32> = row
                .choices
                .iter()
                .map(|(label, _)| {
                    measure_text(label, kit::theme::font_family(), 14.0, 600).map(|size| size.0)
                })
                .collect::<Result<_>>()?;
            scf(kit::controls::segmented_width(&labels), dpi)
        }
        RowKind::Color => scf(349.0, dpi),
        RowKind::ValueSlider => scf(328.0, dpi),
        RowKind::Stepper => scf(104.0, dpi),
        _ => 0.0,
    })
}
/// Reserve the bullet column before wrapping; adding a prefix after measurement loses text to ellipsis.
fn inline_help_lines(text: &str, row_width: f32, dpi: u32) -> Result<Vec<String>> {
    wrap(
        text,
        scf(12.0, dpi),
        400,
        (row_width - scf(40.0, dpi)).max(1.0),
    )
}

fn row_height(row: &Row, inner_w: f32, dpi: u32) -> Result<f32> {
    if desktop_pages::custom(row.kind) {
        return desktop_pages::row_height(row, inner_w, dpi);
    }
    if row.kind == RowKind::InfoBox {
        let text_w = (inner_w - scf(28.0, dpi)).max(1.0);
        let title_h =
            wrap(row.text, scf(14.0, dpi), 600, text_w)?.len().max(1) as f32 * scf(20.0, dpi);
        let body_h = row
            .description()
            .map(|d| wrap(&d, scf(12.0, dpi), 400, text_w))
            .transpose()?
            .map_or(0.0, |lines| lines.len() as f32 * scf(16.0, dpi));
        return Ok(title_h + body_h + scf(26.0, dpi));
    }
    if row.kind == RowKind::Tiles {
        return Ok(backups::list_height(backups::tile_count(), inner_w, dpi));
    }
    if row.kind == RowKind::Expander {
        return Ok(scf(48.0, dpi));
    }
    let cw = control_width(row, dpi)?;
    let stack = inner_w < scf(488.0, dpi) && cw > 0.0;
    let leading = if matches!(row.kind, RowKind::RadioRow | RowKind::CheckRow) {
        scf(32.0, dpi)
    } else {
        0.0
    };
    let tw = if stack {
        inner_w - leading
    } else {
        (inner_w - leading - cw - scf(if cw > 0.0 { 16.0 } else { 0.0 }, dpi)).max(scf(40.0, dpi))
    };
    let title = wrap(row.text, scf(14.0, dpi), 400, tw)?.len().max(1) as f32 * scf(20.0, dpi);
    let desc = if let Some(d) = row.description() {
        wrap(&d, scf(12.0, dpi), 400, tw)?.len() as f32 * scf(16.0, dpi)
    } else {
        0.0
    };
    if row.kind == RowKind::CheckRow && row.desc.is_none() {
        return Ok((title + scf(16.0, dpi)).max(scf(44.0, dpi)));
    }
    Ok((title
        + desc
        + if desc > 0.0 { scf(2.0, dpi) } else { 0.0 }
        + scf(20.0, dpi)
        + if stack { scf(40.0, dpi) } else { 0.0 })
    .max(scf(48.0, dpi)))
}

/// Page header geometry shared by every page.
struct Header {
    title_rect: RectF,
    title_px: f32,
    subtitle: Vec<(String, RectF)>,
    /// Content left edge, right edge and the y where the first section starts.
    x0: f32,
    right: f32,
    y: f32,
}

fn page_header(
    title: &str,
    subtitle: &str,
    dpi: u32,
    width: f32,
    scroll: i32,
    scrolls: bool,
) -> Result<Header> {
    page_header_with_master(title, subtitle, dpi, width, scroll, scrolls, 0.0)
}

fn page_header_with_master(
    title: &str,
    subtitle: &str,
    dpi: u32,
    width: f32,
    scroll: i32,
    scrolls: bool,
    master_width: f32,
) -> Result<Header> {
    let left = nav_width(dpi) as f32;
    // Wide (maximized) windows: the column grows to 960 DIP and is centred in the space beside the nav.
    let avail = width - scf(right_margin(scrolls), dpi) - left - scf(24.0, dpi);
    let column = avail.min(scf(960.0, dpi));
    let x0 = left + scf(24.0, dpi) + ((avail - column) / 2.0).max(0.0).floor();
    let right = x0 + column;
    // Welcome (frame 002) sits 1 px lower, has 20 px sub-header lines and starts its first card 2 px later.
    let welcome = title.starts_with("Welcome");
    let mut y = scf(8.0, dpi) - scroll as f32;

    // Approved page title: 28 DIP / 36 DIP semibold.
    let head_px = pt_px(21.0, dpi);
    let head_h = scf(36.0, dpi).round();
    let text_right = right
        - if master_width > 0.0 {
            master_width + scf(24.0, dpi)
        } else {
            0.0
        };
    let title_rect = RectF::new(x0, y, text_right, y + head_h);
    y += head_h + scf(4.0, dpi);

    // Subtitle: 14 DIP / 20 DIP, followed by 8 DIP bottom padding.
    let sub_px = pt_px(SUB_PT, dpi);
    let sub_h = scf(20.0, dpi).round();
    let sx = x0;
    let mut lines = Vec::new();
    for line in wrap(subtitle, sub_px, 400, text_right - sx)? {
        lines.push((line, RectF::new(sx, y, text_right, y + sub_h)));
        y += sub_h + if welcome { scf(1.0, dpi) } else { 0.0 };
    }
    y += scf(8.0, dpi);
    Ok(Header {
        title_rect,
        title_px: head_px,
        subtitle: lines,
        x0,
        right,
        y,
    })
}

/// Right content margin at 96 DPI: 16 px (card right margin 32 designer) while the page fits,
/// [`SCROLL_RIGHT_MARGIN`] when it scrolls and the scrollbar takes the space.
pub(crate) fn right_margin(scrolls: bool) -> f32 {
    if scrolls { SCROLL_RIGHT_MARGIN } else { 40.0 }
}

/// Right content margin of a scrolling page (96 DPI). Measured as 33-34 px
/// (handoff 2026-10-13: "33-34 px when a page scrolls"); 34 is the upper end of the
/// measurement.
pub(crate) const SCROLL_RIGHT_MARGIN: f32 = 40.0;

/// Whether a page whose content is `content_h` tall (laid out at the non-scrolling margin) scrolls in
/// a viewport `viewport_h` tall. Decided once from the non-scrolling layout: the wider margin can
/// only make the content taller, so a page that overflows stays overflowing (no feedback loop).
pub(crate) fn page_scrolls(content_h: f32, viewport_h: f32) -> bool {
    content_h > viewport_h
}

/// Lay a page out for a client `width` x `height`: first at the normal margin, again with the
/// scrollbar margin when the content overflows `height`.
fn layout_page(
    page: Page,
    settings: &Settings,
    dpi: u32,
    width: f32,
    height: f32,
    scroll: i32,
) -> Result<PageLay> {
    let plain = layout_page_with(page, settings, dpi, width, height, scroll, false)?;
    // `bottom` is in the scrolled frame: add the offset back to get the content height.
    if page_scrolls(plain.bottom + scroll as f32, height) {
        return layout_page_with(page, settings, dpi, width, height, scroll, true);
    }
    Ok(plain)
}

fn layout_page_with(
    page: Page,
    settings: &Settings,
    dpi: u32,
    width: f32,
    height: f32,
    scroll: i32,
    scrolls: bool,
) -> Result<PageLay> {
    let data = content(page);
    let master_width = if data.master.is_some() {
        ["On", "Off"]
            .iter()
            .map(|label| {
                measure_text(label, kit::theme::font_family(), scf(14.0, dpi), 400)
                    .map(|size| size.0)
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .fold(0.0_f32, f32::max)
            + scf(52.0, dpi)
    } else {
        0.0
    };
    let Header {
        mut title_rect,
        title_px: head_px,
        mut subtitle,
        x0,
        right,
        mut y,
    } = page_header_with_master(
        data.title,
        data.subtitle,
        dpi,
        width,
        scroll,
        scrolls,
        master_width,
    )?;

    let mut preview = None;
    let mut scope = None;
    let mut preview_caption = Vec::new();
    if page == Page::Personalization {
        let bar = look_pages::scope_layout(x0, right, y, dpi)?;
        y = bar.bottom;
        scope = Some(bar);
        preview = Some(RectF::new(x0, y, right, y + scf(200.0, dpi)));
        y += scf(208.0, dpi);
        for line in wrap(
            "Your wallpaper with a sample Corral. It follows every change on this page.",
            scf(12.0, dpi),
            400,
            right - x0,
        )? {
            preview_caption.push((line, RectF::new(x0, y, right, y + scf(16.0, dpi))));
            y += scf(16.0, dpi);
        }
    } else if page == Page::Camouflage {
        y += scf(16.0, dpi);
        // Monitor-shaped box (no letterbox bars), at most 45 % of the window height, centred.
        let (bw, bh) =
            camouflage_preview::box_size(right - x0, height, camouflage_preview::monitor_aspect(), dpi);
        let bx = x0 + ((right - x0 - bw) / 2.0).floor();
        preview = Some(RectF::new(bx, y, bx + bw, y + bh));
        y += bh + scf(8.0, dpi);
        for line in wrap(
            "Live view of your main desktop, with your real Corrals and Folder Gates.",
            scf(12.0, dpi),
            400,
            right - x0,
        )? {
            preview_caption.push((line, RectF::new(x0, y, right, y + scf(16.0, dpi))));
            y += scf(16.0, dpi);
        }
    }
    if page == Page::About {
        // The 64-DIP icon starts 16 DIP below the content padding; centre the 58-DIP text group.
        title_rect.top = scf(27.0, dpi) - scroll as f32;
        title_rect.bottom = title_rect.top + scf(36.0, dpi);
        for (_, rect) in &mut subtitle {
            rect.top = title_rect.bottom + scf(2.0, dpi);
            rect.bottom = rect.top + scf(20.0, dpi);
        }
        y = scf(112.0, dpi) - scroll as f32;
    }
    let mut sections = Vec::new();
    for section in &data.sections {
        if !gates_state(section.gates, settings).0 {
            continue;
        }
        // Section headings carry their own 20/20/8-DIP spacing. Unlabelled cards use a 16-DIP gap.
        if section.header.is_empty()
            && (!sections.is_empty()
                || section
                    .rows
                    .first()
                    .is_some_and(|r| r.kind == RowKind::Expander))
        {
            y += scf(
                if section
                    .rows
                    .first()
                    .is_some_and(|r| r.kind == RowKind::InfoBox)
                {
                    12.0
                } else if matches!(page, Page::Personalization | Page::Camouflage) {
                    20.0
                } else {
                    16.0
                },
                dpi,
            );
        } else if page == Page::Backups && sections.is_empty() && section.header.is_empty() {
            y += scf(16.0, dpi);
        }
        let header_rect = RectF::new(
            x0 + scf(2.0, dpi),
            y + scf(20.0, dpi),
            right,
            y + scf(40.0, dpi),
        );
        if !section.header.is_empty() {
            y += scf(48.0, dpi);
        }
        let cx = x0;
        let info_box = section
            .rows
            .first()
            .is_some_and(|r| r.kind == RowKind::InfoBox);
        let (pad_x, pad_y) = if info_box {
            (scf(16.0, dpi), 0.0)
        } else {
            (scf(17.0, dpi), scf(1.0, dpi))
        };
        let inner_w = right - cx - 2.0 * pad_x;
        let card_top = y.ceil();
        let mut ry = card_top + pad_y;
        let mut rows = Vec::new();
        let source = backup_rows(page, section.header, &section.rows);
        let advanced_closed = page == Page::Personalization
            && section
                .rows
                .first()
                .is_some_and(|r| r.text == "Advanced" && !expander_open(page, r));
        if source.first().is_some_and(|r| desktop_pages::grid(r.kind)) {
            let columns =
                desktop_pages::columns(source[0].kind, (right - cx) / (dpi as f32 / 96.0));
            let gap = scf(12.0, dpi);
            let tile_w = ((right - cx - gap * (columns - 1) as f32) / columns as f32).floor();
            for chunk in source.chunks(columns) {
                let h = chunk
                    .iter()
                    .map(|r| row_height(r, tile_w, dpi))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .fold(0.0_f32, f32::max)
                    .ceil();
                for (i, row) in chunk.iter().enumerate() {
                    let x = (cx + i as f32 * (tile_w + gap)).round();
                    rows.push(RowLay {
                        row: *row,
                        x,
                        y: ry.round(),
                        w: tile_w,
                        h,
                        enabled: true,
                    });
                }
                ry += h + gap;
            }
            ry -= gap;
        }
        for (row_index, original) in source.iter().enumerate() {
            if desktop_pages::grid(original.kind) || (advanced_closed && row_index > 0)
                || (page == Page::Personalization && original.text == "Camouflage" && look_pages::scope().title.is_none()) {
                continue;
            }
            let (adapted, scope_enabled) = if page == Page::Personalization {
                look_pages::adapt_row(*original, settings)
            } else {
                (*original, true)
            };
            let row = &adapted;
            let (visible, enabled) = row_state(row, settings);
            let enabled = enabled && scope_enabled;
            if !visible {
                continue;
            }
            if section.joined && !rows.is_empty() {
                ry += scf(JOIN_GAP, dpi); // joined cards leave a 2 px gap
            }
            ry = ry.round();
            let mut h = row_height(row, inner_w, dpi)?.ceil();
            if page == Page::About && row.kind == RowKind::Label {
                // About notice: 14-DIP vertical padding and a 6-DIP text gap.
                h += scf(12.0, dpi);
            }
            if row.kind == RowKind::Expander && expander_open(page, row) {
                for line in row.lines {
                    h += inline_help_lines(line, inner_w, dpi)?.len() as f32 * scf(16.0, dpi)
                        + scf(6.0, dpi);
                }
                h += scf(14.0, dpi);
            }
            let enabled = enabled
                && (row.kind == RowKind::Expander
                    || page == Page::Camouflage && settings.is_on(Field::BlendOptionsVisible)
                    || (page == Page::Hide && row.bind == Some(Field::QuickHideWhat))
                    || data.master.is_none_or(|f| settings.is_on(f)));
            let mut row = *row;
            if row.bind == Some(Field::AutoShowOnBoot) && settings.is_on(Field::AutoHideIdle) {
                row.desc = Some("Not available while idle hiding is on.");
                // The reason must be measured as well as drawn.
                h = h.max(row_height(&row, inner_w, dpi)?.ceil());
            }
            h = h.ceil();
            rows.push(RowLay {
                row,
                x: (cx + pad_x).round(),
                y: ry,
                w: inner_w.round(),
                h,
                enabled,
            });
            ry += h;
        }
        let card_bottom = ry + pad_y;
        y = card_bottom;
        sections.push(SecLay {
            header: section.header,
            header_rect,
            card: RectF::new(cx, card_top, right, card_bottom),
            joined: section.joined,
            rows,
        });
    }
    Ok(PageLay {
        page,
        master: data.master.map(|f| {
            (
                f,
                RectF::new(
                    right - master_width,
                    title_rect.top + scf(10.0, dpi),
                    right,
                    title_rect.top + scf(30.0, dpi),
                ),
            )
        }),
        title: data.title,
        title_rect,
        title_px: head_px,
        subtitle,
        sections,
        preview,
        scope,
        preview_caption,
        bottom: y + scf(PAGE_BOTTOM_PAD, dpi),
    })
}

/// Rows of a section; the "Layout backups" card swaps its link for the tile list and the "Save backup"
/// button while the list is revealed.
fn backup_rows(_page: Page, _header: &str, rows: &[Row]) -> Vec<Row> {
    rows.to_vec()
}

/// Total height of a page's content in pixels at `dpi` for a client of `width` (for scroll limits).
pub fn content_height(
    settings: &Settings,
    page: Page,
    dpi: u32,
    width: i32,
    height: i32,
) -> Result<i32> {
    content_height_with(settings, &[], page, dpi, width, height)
}

/// As [`content_height`] with the layout's Corrals (the Rules page height depends on them).
pub fn content_height_with(
    settings: &Settings,
    groups: &[GroupInfo],
    page: Page,
    dpi: u32,
    width: i32,
    height: i32,
) -> Result<i32> {
    if page == Page::Rules {
        return rules_page::content_height(settings, groups, dpi, width, height);
    }
    Ok(
        layout_page(page, settings, dpi, width as f32, height as f32, 0)?
            .bottom
            .ceil() as i32,
    )
}

/// Largest scroll offset for a client of `width` x `height`.
pub fn max_scroll(
    settings: &Settings,
    page: Page,
    dpi: u32,
    width: i32,
    height: i32,
) -> Result<i32> {
    max_scroll_with(settings, &[], page, dpi, width, height)
}

pub fn max_scroll_with(
    settings: &Settings,
    groups: &[GroupInfo],
    page: Page,
    dpi: u32,
    width: i32,
    height: i32,
) -> Result<i32> {
    Ok((content_height_with(settings, groups, page, dpi, width, height)? - height).max(0))
}

fn monitor_count() -> i32 {
    // SAFETY: read-only display topology query, no Explorer/desktop interaction.
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(
            windows::Win32::UI::WindowsAndMessaging::SM_CMONITORS,
        )
    }
}

/// Shared mouse/keyboard stepper mutation; existing settings serialization and reload own persistence.
pub fn adjust_spacing(settings: &mut Settings, delta: i32) -> bool {
    let next = (i64::from(settings.snap_padding) + i64::from(delta)).clamp(0, 100) as u32;
    let changed = next != settings.snap_padding || !settings.snap_should_scale;
    settings.snap_padding = next;
    settings.snap_should_scale = true;
    changed
}

pub fn parse_spacing(text: &str) -> Option<u32> {
    text.trim().parse::<u32>().ok().filter(|n| *n <= 100)
}

fn control_rect(row: &RowLay, dpi: u32) -> Result<RectF> {
    let w = control_width(&row.row, dpi)?.min(row.w);
    let stack = row.w < scf(488.0, dpi);
    let x = if stack { row.x } else { row.x + row.w - w };
    let top = if stack {
        row.y + row.h - scf(42.0, dpi)
    } else {
        row.y + (row.h - scf(32.0, dpi)) / 2.0
    };
    Ok(RectF::new(x, top, x + w, top + scf(32.0, dpi)))
}
fn combo_box_rect(row: &RowLay, dpi: u32) -> RectF {
    control_rect(row, dpi).unwrap()
}

// ---------------------------------------------------------------------------------------------
// Hit testing and clicks

/// Result of hit testing a client point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Spacing,
    SpacingStep(i32),
    OpenSettingsFolder,
    FoldMode(i64),
    AnimationMode(i64),
    Expander(Page, &'static str),
    ResetAppearance,
    ColorTarget {
        anchor: (i32, i32),
    },
    /// Personalization -> "More background style options" (`titlebar` false) or "More titlebar style options".
    StyleMenu {
        titlebar: bool,
        anchor: (i32, i32),
    },
    /// "How to ..." link; the label keys `help::lookup`.
    Help(&'static str),
    SwapMonitors,
    SavedDeskSetups,
    Nothing,
    Nav(Page),
    /// Link card that switches page (Welcome).
    Goto(Page),
    Toggle(Field),
    Radio(Field, i64),
    /// Combo box; `rect` = `(left, top, right, bottom)` in client pixels.
    Combo {
        field: Field,
        rect: (i32, i32, i32, i32),
    },
    /// "Save / Restore a backup" link.
    BackupLink,
    /// "Save backup" button.
    SaveBackup,
    /// Backup tile (index into the ascending list).
    BackupTile(usize),
    BackupRestore(usize),
    BackupMore(usize),
    RuleAction(rules_page::Action),
    LookAction(look_pages::Action),
    /// Personalization -> Additional Tweaks link; menu anchored below that link.
    ShortcutArrows {
        anchor: (i32, i32),
    },
}

/// A settings value changed by a click or a menu selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub field: Field,
    pub value: i64,
}

pub fn hit_test(
    settings: &Settings,
    page: Page,
    point: (i32, i32),
    dpi: u32,
    width: i32,
    height: i32,
    scroll: i32,
) -> Result<Hit> {
    if point.0 < nav_width(dpi) {
        return Ok(nav_hit_test(point, dpi, height)
            .map(Hit::Nav)
            .unwrap_or(Hit::Nothing));
    }
    let lay = layout_page(page, settings, dpi, width as f32, height as f32, scroll)?;
    let (px, py) = (point.0 as f32, point.1 as f32);
    if let Some(scope) = &lay.scope {
        if let Some(hit) = look_pages::scope_hit(scope, px, py) {
            return Ok(hit);
        }
    }
    if let Some((f, r)) = lay.master {
        if px >= r.left && px < r.right && py >= r.top && py < r.bottom {
            return Ok(Hit::Toggle(f));
        }
    }
    for section in &lay.sections {
        for row in &section.rows {
            if !row.enabled {
                continue;
            }
            let inside = |r: RectF| px >= r.left && px < r.right && py >= r.top && py < r.bottom;
            let full = if desktop_pages::grid(row.row.kind) {
                RectF::new(row.x, row.y, row.x + row.w, row.y + row.h)
            } else {
                RectF::new(
                    section.card.left + scf(1.0, dpi),
                    row.y,
                    section.card.right - scf(1.0, dpi),
                    row.y + row.h,
                )
            };
            if inside(full) {
                match row.row.kind {
                    RowKind::ExploreTile => {
                        return Ok(row.row.goto.map(Hit::Goto).unwrap_or(Hit::Nothing));
                    }
                    RowKind::TabStyleTile => {
                        return Ok(Hit::Radio(Field::TabsStyle, row.row.value));
                    }
                    RowKind::Status => return Ok(Hit::Toggle(Field::EnableCorral)),
                    _ => {}
                }
            }
            if row.row.kind == RowKind::Expander
                && inside(RectF::new(
                    full.left,
                    full.top,
                    full.right,
                    full.top + scf(48.0, dpi),
                ))
            {
                return Ok(Hit::Expander(page, row.row.text));
            }
            if let Some(hit) = look_pages::hit_row(row, px, py, dpi)? {
                return Ok(hit);
            }
            if row.row.kind == RowKind::Segmented && inside(control_rect(row, dpi)?) {
                let r = control_rect(row, dpi)?;
                let i = (((px - r.left) / (r.right - r.left) * row.row.choices.len() as f32)
                    as usize)
                    .min(row.row.choices.len().saturating_sub(1));
                if let Some((_, v)) = row.row.choices.get(i) {
                    return Ok(if row.row.bind == Some(Field::SwipeAnimate) {
                        Hit::AnimationMode(*v)
                    } else if row.row.secondary_bind.is_some() {
                        Hit::FoldMode(*v)
                    } else if let Some(f) = row.row.bind {
                        Hit::Radio(f, *v)
                    } else {
                        Hit::Nothing
                    });
                }
            }
            if row.row.button_text == Some("Swap") && inside(control_rect(row, dpi)?) {
                return Ok(Hit::SwapMonitors);
            }
            if row.row.button_text == Some("Saved setups…") && inside(control_rect(row, dpi)?) {
                return Ok(Hit::SavedDeskSetups);
            }
            if row.row.button_text == Some("Open folder") && inside(control_rect(row, dpi)?) {
                return Ok(Hit::OpenSettingsFolder);
            }
            if row.row.button_text == Some("Save backup") && inside(control_rect(row, dpi)?) {
                return Ok(Hit::SaveBackup);
            }
            match row.row.kind {
                RowKind::Number | RowKind::Stepper => {
                    let r = control_rect(row, dpi)?;
                    let scale = dpi as f32 / 96.0;
                    let bounds = RectF::new(
                        r.left / scale,
                        r.top / scale,
                        r.right / scale,
                        r.bottom / scale,
                    );
                    if let Some(hit) = kit::controls::hit_control(
                        kit::controls::Control::Stepper,
                        bounds,
                        px,
                        py,
                        dpi,
                        true,
                        3,
                    ) {
                        return Ok(match hit.part {
                            0 => Hit::SpacingStep(-1),
                            2 => Hit::SpacingStep(1),
                            _ => Hit::Spacing,
                        });
                    }
                }
                RowKind::Check | RowKind::Toggle | RowKind::CheckRow => {
                    if let (true, Some(f)) = (inside(full), row.row.bind) {
                        return Ok(Hit::Toggle(f));
                    }
                }
                RowKind::Radio | RowKind::RadioRow => {
                    if let (true, Some(f)) = (inside(full), row.row.bind) {
                        return Ok(Hit::Radio(f, row.row.value));
                    }
                }
                RowKind::Combo | RowKind::Dropdown => {
                    let r = combo_box_rect(row, dpi);
                    if inside(r) && row.row.text == "All groups" {
                        return Ok(Hit::ColorTarget {
                            anchor: (r.left as i32, r.bottom as i32),
                        });
                    }
                    if let (true, Some(f)) = (inside(r), row.row.bind) {
                        return Ok(Hit::Combo {
                            field: f,
                            rect: (r.left as i32, r.top as i32, r.right as i32, r.bottom as i32),
                        });
                    }
                }
                RowKind::Link | RowKind::Navigate => {
                    if inside(full)
                        && page == Page::Personalization
                        && (row.row.text == "More background style options"
                            || row.row.text == "More titlebar style options")
                    {
                        return Ok(Hit::StyleMenu {
                            titlebar: row.row.text == "More titlebar style options",
                            anchor: (row.x as i32, (row.y + row.h) as i32),
                        });
                    }
                    if inside(full) {
                        if let Some(help) = help::lookup(row.row.text) {
                            return Ok(Hit::Help(help.label));
                        }
                    }
                    if inside(full) && row.row.text == "Swap monitors" {
                        return Ok(Hit::SwapMonitors);
                    }
                    if inside(full) && row.row.text == "Reset all to default" {
                        return Ok(Hit::ResetAppearance);
                    }
                    if inside(full)
                        && (row.row.text == crate::icon_overlay::LINK_LABEL
                            || row.row.text == "Shortcut arrows")
                    {
                        return Ok(Hit::ShortcutArrows {
                            anchor: (row.x as i32, (row.y + row.h) as i32),
                        });
                    }
                    if let (true, Some(target)) = (inside(full), row.row.goto) {
                        return Ok(Hit::Goto(target));
                    }
                    if inside(full) && row.row.text == backups::LINK_TEXT {
                        return Ok(Hit::BackupLink);
                    }
                }
                RowKind::Button | RowKind::ButtonRow => {
                    if row.row.text == backups::SAVE_TEXT && inside(combo_box_rect(row, dpi)) {
                        return Ok(Hit::SaveBackup);
                    }
                }
                RowKind::Tiles => {
                    if let Some(action) = backups::list_action_at(
                        (px, py),
                        backups::tile_count(),
                        row.x,
                        row.y + backups::notice_height(dpi),
                        row.w,
                        dpi,
                    ) {
                        return Ok(match action {
                            backups::ListAction::Restore(i) => Hit::BackupRestore(i),
                            backups::ListAction::More(i) => Hit::BackupMore(i),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    Ok(Hit::Nothing)
}

/// Apply a click on a checkbox / radio row to `settings`. Navigation, links and combos are not
/// changes (see [`hit_test`] / [`apply_choice`]).
pub fn apply_click(
    settings: &mut Settings,
    page: Page,
    point: (i32, i32),
    dpi: u32,
    width: i32,
    height: i32,
    scroll: i32,
) -> Option<Change> {
    match hit_test(settings, page, point, dpi, width, height, scroll).ok()? {
        Hit::Toggle(field) => {
            let value = 1 - settings.get(field).min(1);
            settings.set(field, value);
            Some(Change { field, value })
        }
        Hit::Radio(field, value) => {
            if settings.get(field) == value {
                return None;
            }
            settings.set(field, value);
            Some(Change { field, value })
        }
        _ => None,
    }
}

/// Store a combo menu selection.
pub fn apply_choice(settings: &mut Settings, field: Field, value: i64) -> Change {
    if field == Field::AutoHideIdle {
        content::apply_idle_mode(settings, value);
    } else {
        settings.set(field, value);
    }
    Change { field, value }
}

// ---------------------------------------------------------------------------------------------
// Drawing

fn draw_page(
    ctx: &crate::render::Ctx,
    lay: &PageLay,
    theme: &Theme,
    dpi: u32,
    settings: &Settings,
    editor: Option<&crate::color_state::Effective>,
) -> Result<()> {
    if lay.page == Page::About {
        kit::hero::draw(
            ctx,
            RectF::new(
                lay.title_rect.left,
                lay.title_rect.top - scf(3.0, dpi),
                lay.title_rect.left + scf(64.0, dpi),
                lay.title_rect.top + scf(61.0, dpi),
            ),
        )?;
    }
    let heading = if lay.page == Page::About {
        RectF::new(
            lay.title_rect.left + scf(84.0, dpi),
            lay.title_rect.top,
            lay.title_rect.right,
            lay.title_rect.bottom,
        )
    } else {
        lay.title_rect
    };
    ctx.draw_text_cached(
        lay.title,
        heading,
        &style(
            kit::theme::font_family(),
            lay.title_px,
            600,
            theme.header_text,
            Align::Left,
        ),
    )?;
    if let Some((f, r)) = lay.master {
        ctx.draw_text_cached(
            if settings.is_on(f) { "On" } else { "Off" },
            RectF::new(r.left, r.top, r.right - scf(52.0, dpi), r.bottom),
            &style(
                kit::theme::font_family(),
                scf(14.0, dpi),
                400,
                theme.nav_text,
                Align::Left,
            ),
        )?;
        kit_draw(
            ctx,
            kit::controls::Control::Toggle,
            RectF::new(r.right - scf(40.0, dpi), r.top, r.right, r.bottom),
            "",
            true,
            settings.is_on(f),
            theme,
            dpi,
            0.0,
        )?;
    }
    let sub_px = pt_px(SUB_PT, dpi);
    for (line, rect) in &lay.subtitle {
        let rect = if lay.page == Page::About {
            RectF::new(
                rect.left + scf(84.0, dpi),
                rect.top,
                rect.right,
                rect.bottom,
            )
        } else {
            *rect
        };
        ctx.draw_text_cached(
            line,
            rect,
            &style(
                kit::theme::font_family(),
                sub_px,
                400,
                theme.sub_header_text,
                Align::Left,
            ),
        )?;
    }
    if let Some(scope) = &lay.scope {
        look_pages::draw_scope(ctx, scope, theme, dpi)?;
    }
    if lay.page == Page::Personalization {
        if let Some(rect) = lay.preview {
            let fallback;
            let model = if let Some(m) = editor {
                m
            } else {
                fallback = look_pages::effective(settings);
                &fallback
            };
            sample_corral::draw(ctx, rect, dpi, settings, model, theme)?;
        }
    }
    for (line, rect) in &lay.preview_caption {
        ctx.draw_text_cached(
            line,
            *rect,
            &style(
                kit::theme::font_family(),
                scf(12.0, dpi),
                400,
                theme.sub_header_text,
                Align::Left,
            ),
        )?;
    }
    for section in &lay.sections {
        ctx.draw_text_cached(
            section.header,
            section.header_rect,
            &style(
                kit::theme::font_family(),
                scf(14.0, dpi),
                600,
                theme.header_text,
                Align::Left,
            ),
        )?;
        let card = section.card;
        if !section
            .rows
            .first()
            .is_some_and(|r| desktop_pages::grid(r.row.kind))
        {
            ctx.fill_rounded_rect(
                card,
                scf(8.0, dpi),
                if section
                    .rows
                    .first()
                    .is_some_and(|r| r.row.kind == RowKind::InfoBox)
                {
                    kit::theme::Theme::new(theme.light).surface_raised
                } else {
                    theme.group_fill
                },
            )?;
            if !section
                .rows
                .first()
                .is_some_and(|r| r.row.kind == RowKind::InfoBox)
            {
                ctx.stroke_rounded_rect(
                    card,
                    scf(8.0, dpi),
                    scf(1.0, dpi).max(1.0),
                    theme.group_outline,
                )?;
            }
        }
        for (i, row) in section.rows.iter().enumerate() {
            if i > 0 && !desktop_pages::grid(row.row.kind) {
                // Joined groups (JoinTop/JoinBottom) look like one list with 1 px separators.
                ctx.fill_rect(
                    RectF::new(
                        card.left + scf(16.0, dpi),
                        row.y - scf(1.0, dpi),
                        card.right - scf(16.0, dpi),
                        row.y,
                    ),
                    kit::theme::Theme::new(theme.light).border_subtle,
                )?;
            }
            // Clip a card-sized rounded fill to the row so end rows keep the card corners.
            if row.enabled
                && matches!(row.row.control_kind(), RowKind::Toggle | RowKind::Navigate)
                && CONTROL_POINTER.with(|p| {
                    p.get().is_some_and(|(x, y, _)| {
                        x >= card.left + scf(1.0, dpi)
                            && x < card.right - scf(1.0, dpi)
                            && y >= row.y
                            && y < row.y + row.h
                    })
                })
            {
                ctx.with_clip(
                    RectF::new(card.left, row.y, card.right, row.y + row.h),
                    |ctx| {
                        ctx.fill_rounded_rect(
                            RectF::new(
                                card.left + scf(1.0, dpi),
                                card.top + scf(1.0, dpi),
                                card.right - scf(1.0, dpi),
                                card.bottom - scf(1.0, dpi),
                            ),
                            scf(7.0, dpi),
                            theme.nav_hover,
                        )
                    },
                )?;
            }
            draw_row(ctx, row, theme, dpi, settings, editor, lay.page)?;
        }
    }
    Ok(())
}

fn with_alpha(c: Rgba, a: u8) -> Rgba {
    Rgba::new(c.r, c.g, c.b, ((c.a as u32 * a as u32) / 255) as u8)
}

/// Check box (96 dpi): 13 px square; unchecked = filled
/// near-white square (243 in the dark theme) with soft edge, checked = accent fill with a white check.
fn draw_checkbox(
    ctx: &crate::render::Ctx,
    r: RectF,
    checked: bool,
    theme: &Theme,
    dpi: u32,
    alpha: u8,
) -> Result<()> {
    if checked {
        ctx.fill_rounded_rect(r, scf(2.0, dpi), with_alpha(theme.accent, alpha))?;
        let glyph_px = (r.right - r.left) * 0.8;
        ctx.draw_text_cached(
            "\u{E73E}",
            r,
            &style(
                icon_family(),
                glyph_px,
                400,
                with_alpha(Rgba::new(255, 255, 255, 255), alpha),
                Align::Center,
            ),
        )
    } else {
        let inset = scf(0.75, dpi);
        let inner = RectF::new(
            r.left + inset,
            r.top + inset,
            r.right - inset,
            r.bottom - inset,
        );
        ctx.fill_rounded_rect(inner, scf(1.5, dpi), with_alpha(theme.check_fill, alpha))?;
        if theme.light {
            ctx.stroke_rounded_rect(
                r,
                scf(2.0, dpi),
                scf(1.0, dpi).max(1.0),
                with_alpha(Rgba::new(120, 120, 120, 255), alpha),
            )?;
        }
        Ok(())
    }
}

/// Backup tiles: placeholder thumbnail (Corral takes no screenshots) above the title; the hovered tile gets an accent outline.
fn draw_tiles(ctx: &crate::render::Ctx, lay: &RowLay, theme: &Theme, dpi: u32) -> Result<()> {
    let action_at = |x, y| {
        backups::list_action_at(
            (x, y),
            backups::tile_count(),
            lay.x,
            lay.y + backups::notice_height(dpi),
            lay.w,
            dpi,
        )
    };
    let hover = CONTROL_POINTER
        .with(|p| p.get())
        .and_then(|(x, y, _)| action_at(x, y));
    let focus = FOCUS_RING
        .with(|p| p.get())
        .and_then(|r| action_at((r.left + r.right) / 2.0, (r.top + r.bottom) / 2.0));
    backups::draw_list(
        ctx,
        lay.x,
        lay.y,
        lay.w,
        dpi,
        theme.light,
        lay.enabled,
        hover,
        focus,
    )
}

/// Checkbox / radio / combo / button drawing. Approximations (the glyph is OS
/// drawn): 13 px box, 2 px radius, 1 px border in the disabled-text colour; checked = accent fill with
/// a white check glyph from the icon font; radio = ring, selected = accent ring with centre dot; combo
/// = 1 px border, radius 4, text plus a chevron glyph. Disabled controls use half alpha.

fn kit_draw(
    ctx: &crate::render::Ctx,
    kind: kit::controls::Control,
    r: RectF,
    label: &str,
    enabled: bool,
    selected: bool,
    theme: &Theme,
    dpi: u32,
    value: f32,
) -> Result<()> {
    let scale = dpi as f32 / 96.0;
    kit::controls::draw_control(
        ctx,
        kind,
        RectF::new(
            r.left / scale,
            r.top / scale,
            r.right / scale,
            r.bottom / scale,
        ),
        label,
        kit::controls::VisualState {
            enabled,
            selected,
            hovered: CONTROL_POINTER.with(|p| {
                p.get().is_some_and(|(x, y, _)| {
                    x >= r.left && x < r.right && y >= r.top && y < r.bottom
                })
            }),
            pressed: CONTROL_POINTER.with(|p| {
                p.get().is_some_and(|(x, y, down)| {
                    down && x >= r.left && x < r.right && y >= r.top && y < r.bottom
                })
            }),
            ..Default::default()
        },
        &kit::theme::Theme::new(theme.light),
        dpi,
        value,
    )
}
fn draw_row(
    ctx: &crate::render::Ctx,
    lay: &RowLay,
    theme: &Theme,
    dpi: u32,
    settings: &Settings,
    _editor: Option<&crate::color_state::Effective>,
    page: Page,
) -> Result<()> {
    use kit::controls::{ButtonStyle, Control as C};
    if look_pages::draw_row(ctx, lay, theme, dpi, settings)? {
        return Ok(());
    }
    let row = &lay.row;
    let kind = row.control_kind();
    if desktop_pages::custom(kind) {
        return desktop_pages::draw(ctx, lay, theme, dpi, settings);
    }
    let alpha = if lay.enabled { 255 } else { 102 };

    let fg = with_alpha(theme.nav_text, alpha);
    let secondary = with_alpha(theme.sub_header_text, alpha);
    let (x, y, w, h) = (lay.x, lay.y, lay.w, lay.h);
    let r = control_rect(lay, dpi)?;
    let text = |t: &str, r: RectF, size: f32, weight: u16, color: Rgba| {
        ctx.draw_text_cached(
            t,
            r,
            &style(
                kit::theme::font_family(),
                scf(size, dpi),
                weight,
                color,
                Align::Left,
            ),
        )
    };
    if kind == RowKind::InfoBox {
        ctx.draw_text_cached(
            "\u{E82F}",
            RectF::new(
                x,
                y + scf(12.0, dpi),
                x + scf(16.0, dpi),
                y + scf(28.0, dpi),
            ),
            &style(icon_family(), scf(16.0, dpi), 400, secondary, Align::Center),
        )?;
        let tx = x + scf(28.0, dpi);
        let mut yy = y + scf(12.0, dpi);
        for line in wrap(row.text, scf(14.0, dpi), 600, w - scf(28.0, dpi))? {
            text(
                &line,
                RectF::new(tx, yy, x + w, yy + scf(20.0, dpi)),
                14.0,
                600,
                fg,
            )?;
            yy += scf(20.0, dpi);
        }
        yy += scf(2.0, dpi);
        if let Some(body) = row.description() {
            for line in wrap(&body, scf(12.0, dpi), 400, w - scf(28.0, dpi))? {
                text(
                    &line,
                    RectF::new(tx, yy, x + w, yy + scf(16.0, dpi)),
                    12.0,
                    400,
                    secondary,
                )?;
                yy += scf(16.0, dpi);
            }
        }
        return Ok(());
    }
    if kind == RowKind::Tiles {
        return draw_tiles(ctx, lay, theme, dpi);
    }
    if kind == RowKind::Expander {
        if row.text != "Advanced" {
            ctx.draw_text_cached(
                "\u{E82F}",
                RectF::new(x, y, x + scf(16.0, dpi), y + scf(48.0, dpi)),
                &style(icon_family(), scf(16.0, dpi), 400, secondary, Align::Center),
            )?;
        }
        text(
            row.text,
            RectF::new(
                x + scf(if row.text == "Advanced" { 0.0 } else { 28.0 }, dpi),
                y,
                x + w - scf(28.0, dpi),
                y + scf(48.0, dpi),
            ),
            14.0,
            400,
            fg,
        )?;
        let open = expander_open(page, row);
        ctx.draw_text_cached(
            if open { "\u{E70E}" } else { "\u{E70D}" },
            RectF::new(x + w - scf(20.0, dpi), y, x + w, y + scf(48.0, dpi)),
            &style(icon_family(), scf(12.0, dpi), 400, secondary, Align::Center),
        )?;
        if open {
            let mut yy = y + scf(48.0, dpi);
            for line in row.lines {
                for (i, l) in inline_help_lines(line, w, dpi)?.iter().enumerate() {
                    if i == 0 {
                        text(
                            "•",
                            RectF::new(
                                x + scf(28.0, dpi),
                                yy,
                                x + scf(40.0, dpi),
                                yy + scf(16.0, dpi),
                            ),
                            12.0,
                            400,
                            secondary,
                        )?;
                    }
                    text(
                        l,
                        RectF::new(x + scf(40.0, dpi), yy, x + w, yy + scf(16.0, dpi)),
                        12.0,
                        400,
                        secondary,
                    )?;
                    yy += scf(16.0, dpi);
                }
                yy += scf(6.0, dpi);
            }
        }
        return Ok(());
    }
    let cw = control_width(row, dpi)?;
    let stack = w < scf(488.0, dpi) && cw > 0.0;
    let leading = if matches!(kind, RowKind::RadioRow | RowKind::CheckRow) {
        scf(32.0, dpi)
    } else {
        0.0
    };
    let tx = x + leading;
    let right = if stack || cw == 0.0 {
        x + w
    } else {
        r.left - scf(16.0, dpi)
    };
    let about_notice = page == Page::About && kind == RowKind::Label;
    let mut yy = y + scf(if about_notice { 14.0 } else { 10.0 }, dpi);
    let title = if row.text == "Blur strength" {
        format!(
            "Blur strength: {}",
            camouflage_options::effective_radius(settings)
        )
    } else {
        row.text.to_string()
    };
    for line in wrap(
        &title,
        scf(14.0, dpi),
        400,
        (right - tx).max(scf(40.0, dpi)),
    )? {
        text(
            &line,
            RectF::new(tx, yy, right, yy + scf(20.0, dpi)),
            14.0,
            400,
            if kind == RowKind::Link {
                theme.link
            } else {
                fg
            },
        )?;
        yy += scf(20.0, dpi);
    }
    if let Some(d) = row.description() {
        yy += scf(if about_notice { 6.0 } else { 2.0 }, dpi);
        for line in wrap(&d, scf(12.0, dpi), 400, (right - tx).max(scf(40.0, dpi)))? {
            text(
                &line,
                RectF::new(tx, yy, right, yy + scf(16.0, dpi)),
                12.0,
                400,
                secondary,
            )?;
            yy += scf(16.0, dpi);
        }
    }
    match kind {
        RowKind::Toggle => kit_draw(
            ctx,
            C::Toggle,
            RectF::new(
                r.right - scf(40.0, dpi),
                r.top + scf(6.0, dpi),
                r.right,
                r.top + scf(26.0, dpi),
            ),
            "",
            lay.enabled,
            look_pages::checked(row, settings),
            theme,
            dpi,
            0.0,
        )?,
        RowKind::RadioRow | RowKind::CheckRow => {
            let checked = row.bind.is_some_and(|f| {
                if kind == RowKind::RadioRow {
                    settings.get(f) == row.value
                } else {
                    settings.is_on(f)
                }
            });
            kit_draw(
                ctx,
                if kind == RowKind::RadioRow {
                    C::Radio
                } else {
                    C::Checkbox
                },
                RectF::new(
                    x,
                    y + scf(14.0, dpi),
                    x + scf(20.0, dpi),
                    y + scf(34.0, dpi),
                ),
                "",
                lay.enabled,
                checked,
                theme,
                dpi,
                0.0,
            )?;
        }
        RowKind::Dropdown | RowKind::Button | RowKind::ButtonRow => {
            let label = if kind == RowKind::Dropdown {
                if let Some(f) = row.bind {
                    let choices = combo_choices(f, content::choice_value(settings, f));
                    choices
                        .iter()
                        .find(|(_, v)| *v == content::choice_value(settings, f))
                        .or_else(|| choices.first())
                        .map(|(label, _)| label.clone())
                        .unwrap_or_else(|| row.text.into())
                } else {
                    row.text.into()
                }
            } else {
                row.button_text.unwrap_or(row.text).to_string()
            };
            kit_draw(
                ctx,
                if kind == RowKind::Dropdown {
                    C::Dropdown
                } else {
                    C::Button(if row.primary {
                        ButtonStyle::Primary
                    } else {
                        ButtonStyle::Secondary
                    })
                },
                r,
                &label,
                lay.enabled,
                false,
                theme,
                dpi,
                0.0,
            )?;
        }
        RowKind::Segmented => {
            let scale = dpi as f32 / 96.0;
            let bounds = RectF::new(
                r.left / scale,
                r.top / scale,
                r.right / scale,
                r.bottom / scale,
            );
            let labels: Vec<&str> = row.choices.iter().map(|(label, _)| *label).collect();
            let selected = row
                .choices
                .iter()
                .position(|(_, value)| *value == look_pages::current(row, settings))
                .unwrap_or(0);
            let hovered = CONTROL_POINTER
                .with(|pointer| pointer.get())
                .and_then(|(px, py, _)| {
                    if px >= r.left && px < r.right && py >= r.top && py < r.bottom {
                        Some((((px - r.left) / (r.right - r.left)) * labels.len() as f32) as usize)
                    } else {
                        None
                    }
                });
            kit::controls::draw_segmented(
                ctx,
                bounds,
                &labels,
                selected,
                hovered,
                kit::controls::VisualState {
                    enabled: lay.enabled,
                    ..Default::default()
                },
                &kit::theme::Theme::new(theme.light),
                dpi,
            )?;
        }
        RowKind::Stepper => kit_draw(
            ctx,
            C::Stepper,
            r,
            &settings.snap_padding.min(100).to_string(),
            lay.enabled,
            false,
            theme,
            dpi,
            0.0,
        )?,
        RowKind::Navigate => {
            ctx.draw_text_cached(
                "\u{E76C}",
                RectF::new(x + w - scf(20.0, dpi), y, x + w, y + h),
                &style(icon_family(), scf(16.0, dpi), 400, secondary, Align::Center),
            )?;
        }
        RowKind::InfoBox => {
            for line in row.lines {
                for l in wrap(line, scf(12.0, dpi), 400, w)? {
                    text(
                        &l,
                        RectF::new(x, yy, x + w, yy + scf(16.0, dpi)),
                        12.0,
                        400,
                        secondary,
                    )?;
                    yy += scf(16.0, dpi);
                }
            }
        }
        _ => {}
    }
    Ok(())
}
/// Repaint the active appearance slider and sample without repainting unrelated controls or navigation.
pub fn render_look_slider_row(
    surface: &mut Surface,
    view: &View,
    settings: &Settings,
    theme: &Theme,
    dpi: u32,
    editor: Option<&crate::color_state::Effective>,
    kind: look_pages::SliderKind,
) -> Result<Option<RectF>> {
    if let Some(m) = editor {
        look_pages::set_effective(m);
    }
    let cached = LOOK_CACHE.with(|c| c.borrow().clone()).filter(|c| {
        c.page == view.page
            && c.dpi == dpi
            && c.width == surface.width()
            && c.height == surface.height()
            && c.scroll == view.scroll
    });
    let cache = if let Some(cache) = cached {
        cache
    } else {
        let lay = layout_page(
            view.page,
            settings,
            dpi,
            surface.width() as f32,
            surface.height() as f32,
            view.scroll,
        )?;
        LookCache {
            page: view.page,
            dpi,
            width: surface.width(),
            height: surface.height(),
            scroll: view.scroll,
            rows: lay.sections.into_iter().flat_map(|s| s.rows).collect(),
            preview: lay.preview,
        }
    };
    let Some(row) = cache
        .rows
        .iter()
        .find(|r| look_pages::slider(&r.row) == Some(kind))
    else {
        return Ok(None);
    };
    let r = RectF::new(row.x, row.y, row.x + row.w, row.y + row.h);
    let clip = RectF::new(
        r.left,
        r.top + scf(2.0, dpi),
        r.right,
        r.bottom - scf(2.0, dpi),
    );
    surface.draw(|ctx| {
        ctx.with_clip(clip, |ctx| {
            ctx.fill_rect(clip, theme.group_fill)?;
            draw_row(ctx, row, theme, dpi, settings, editor, view.page)
        })?;
        if view.page == Page::Personalization {
            if let Some(preview) = cache.preview {
                ctx.fill_rect(RectF::new(preview.left-scf(1.0,dpi),preview.top-scf(1.0,dpi),preview.right+scf(1.0,dpi),preview.bottom+scf(1.0,dpi)), theme.panel_bg)?;
                let m = look_pages::effective(settings);
                sample_corral::draw(ctx, preview, dpi, settings, &m, theme)?;
            }
        }
        if view.page == Page::Camouflage {
            if let Some(preview) = cache.preview {
                ctx.fill_rect(RectF::new(preview.left-scf(1.0,dpi),preview.top-scf(1.0,dpi),preview.right+scf(1.0,dpi),preview.bottom+scf(1.0,dpi)), theme.panel_bg)?;
                camouflage_preview::draw_rect(ctx, preview, dpi, theme, settings)?;
            }
        }
        Ok(())
    })?;
    let dirty = cache.preview.map_or(r, |p| {
        RectF::new(
            r.left.min(p.left),
            r.top.min(p.top),
            r.right.max(p.right),
            r.bottom.max(p.bottom),
        )
    });
    Ok(Some(dirty))
}
pub fn focus_targets(
    settings: &Settings,
    page: Page,
    dpi: u32,
    width: i32,
    height: i32,
    scroll: i32,
) -> Result<Vec<(Hit, RectF)>> {
    focus_targets_with(settings, &[], page, dpi, width, height, scroll)
}

pub fn focus_targets_with(
    settings: &Settings,
    groups: &[GroupInfo],
    page: Page,
    dpi: u32,
    width: i32,
    height: i32,
    scroll: i32,
) -> Result<Vec<(Hit, RectF)>> {
    let (x, y, w, h) = nav_item_rect(page.index(), dpi, height);
    let mut targets = vec![(
        Hit::Nav(page),
        RectF::new(x as f32, y as f32, (x + w) as f32, (y + h) as f32),
    )];
    if page == Page::Rules {
        let lay = rules_page::layout(
            settings,
            groups,
            dpi,
            width as f32,
            height as f32,
            scroll,
        )?;
        targets.extend(
            rules_page::focus_targets(&lay)
                .into_iter()
                .map(|(r, a)| (Hit::RuleAction(a), r)),
        );
        return Ok(targets);
    }
    let lay = layout_page(page, settings, dpi, width as f32, height as f32, scroll)?;
    if let Some((f, r)) = lay.master {
        targets.push((Hit::Toggle(f), r));
    }
    if let Some(scope) = &lay.scope {
        targets.extend(look_pages::scope_targets(scope));
    }
    for sec in &lay.sections {
        for row in &sec.rows {
            if !row.enabled {
                continue;
            }
            let look_targets = look_pages::row_targets(row, dpi)?;
            if !look_targets.is_empty() {
                targets.extend(look_targets);
                continue;
            }
            if row.row.kind == RowKind::Tiles {
                for action in backups::focus_actions(backups::tile_count()) {
                    let r = backups::action_rect(
                        action.index(),
                        action.part(),
                        row.x,
                        row.y + backups::notice_height(dpi),
                        row.w,
                        dpi,
                    );
                    targets.push((
                        match action {
                            backups::ListAction::Restore(i) => Hit::BackupRestore(i),
                            backups::ListAction::More(i) => Hit::BackupMore(i),
                        },
                        r,
                    ));
                }
                continue;
            }
            let r = if matches!(
                row.row.control_kind(),
                RowKind::Dropdown
                    | RowKind::Button
                    | RowKind::ButtonRow
                    | RowKind::Segmented
                    | RowKind::Stepper
            ) {
                control_rect(row, dpi)?
            } else {
                RectF::new(
                    row.x,
                    row.y,
                    row.x + row.w,
                    row.y
                        + if row.row.kind == RowKind::Expander {
                            scf(48.0, dpi)
                        } else {
                            row.h
                        },
                )
            };
            let point = if row.row.control_kind() == RowKind::Stepper {
                (
                    ((r.left + r.right) / 2.0) as i32,
                    ((r.top + r.bottom) / 2.0) as i32,
                )
            } else {
                ((r.left + 2.0) as i32, (r.top + 2.0) as i32)
            };
            let hit = hit_test(settings, page, point, dpi, width, height, scroll)?;
            if hit != Hit::Nothing {
                targets.push((hit, r));
            }
        }
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    #[test]
    fn c2_desk_setup_row_is_only_in_remembered_layout_mode_and_button_is_focusable()
    -> windows::core::Result<()> {
        use super::*;
        let mut settings = Settings::default();
        for mode in [0, 1, 2] {
            settings.set(Field::LayoutPreserve, mode);
            for dpi in [96, 120, 144, 240] {
                let (w, h) = (sc(760., dpi), sc(1600., dpi));
                let lay = layout_page(Page::Layout, &settings, dpi, w as f32, h as f32, 0)?;
                let row = lay
                    .sections
                    .iter()
                    .flat_map(|s| &s.rows)
                    .find(|r| r.row.text == "This desk setup");
                assert_eq!(row.is_some(), mode == 2);
                if let Some(row) = row {
                    let rect = control_rect(row, dpi)?;
                    assert!(rect.left >= nav_width(dpi) as f32 && rect.right <= w as f32);
                    assert_eq!(
                        hit_test(
                            &settings,
                            Page::Layout,
                            (
                                ((rect.left + rect.right) / 2.) as i32,
                                ((rect.top + rect.bottom) / 2.) as i32
                            ),
                            dpi,
                            w,
                            h,
                            0
                        )?,
                        Hit::SavedDeskSetups
                    );
                    assert!(
                        focus_targets(&settings, Page::Layout, dpi, w, h, 0)?
                            .iter()
                            .any(|(hit, _)| *hit == Hit::SavedDeskSetups)
                    );
                }
            }
        }
        Ok(())
    }
    #[test]
    fn u3_color_swatches_share_target_and_scaled_hit_focus_geometry() {
        let s = Settings::default();
        look_pages::set_scope(None, vec![], false);
        look_pages::set_effective(&crate::color_state::effective_of(&s, &Default::default()));
        for dpi in [96, 120, 144, 240] {
            let (w, h) = (sc(1000.0, dpi), sc(2000.0, dpi));
            let lay = layout_page(Page::Personalization, &s, dpi, w as f32, h as f32, 0).unwrap();
            let row = lay
                .sections
                .iter()
                .flat_map(|sec| &sec.rows)
                .find(|r| r.row.text == "Text color")
                .unwrap();
            let targets = look_pages::row_targets(row, dpi).unwrap();
            assert_eq!(targets.len(), 10);
            for (hit, r) in targets {
                assert_eq!(
                    hit_test(
                        &s,
                        Page::Personalization,
                        (
                            ((r.left + r.right) / 2.0) as i32,
                            ((r.top + r.bottom) / 2.0) as i32
                        ),
                        dpi,
                        w,
                        h,
                        0
                    )
                    .unwrap(),
                    hit
                );
            }
        }
    }

    use super::*;
    use std::path::PathBuf;

    #[test]
    fn spacing_parsing_and_disabled_tint_hit_testing() {
        for (text, expected) in [
            ("0", Some(0)),
            (" 6 ", Some(6)),
            ("100", Some(100)),
            ("101", None),
            ("-1", None),
            ("6.5", None),
            ("", None),
        ] {
            assert_eq!(parse_spacing(text), expected);
        }
        for dpi in [96, 120, 144, 240] {
            let s = Settings::default();
            let (w, h) = (sc(1000.0, dpi), sc(2000.0, dpi));
            for mode in [0, 1, 2] {
                let base = crate::color_state::effective_of(&s, &Default::default());
                let patch = crate::color_state::ColorEdit::TintMode(
                    crate::color_state::TintMode::from_index(mode).unwrap(),
                )
                .patch(&base);
                let mut shared = crate::color_state::Globals::from_settings(&s);
                patch.apply_to_globals(&mut shared);
                look_pages::set_effective(&crate::color_state::resolve(&shared, &Default::default()));
                let lay =
                    layout_page(Page::Personalization, &s, dpi, w as f32, h as f32, 0).unwrap();
                let row = lay
                    .sections
                    .iter()
                    .flat_map(|sec| &sec.rows)
                    .find(|r| r.row.text == "Tint color")
                    .unwrap();
                assert_eq!(row.enabled, mode == 2);
                assert_eq!(
                    !look_pages::row_targets(row, dpi).unwrap().is_empty(),
                    mode == 2
                );
            }
        }
    }

    #[test]
    fn u3_removed_legacy_controls_and_keeps_title_segments() {
        let data = content(Page::Personalization);
        let rows: Vec<_> = data.sections.iter().flat_map(|s| &s.rows).collect();
        assert!(!data.sections.iter().any(|s| s.header == "Tab Style"));
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r.kind, RowKind::Combo | RowKind::Link))
        );
        assert_eq!(rows.iter().filter(|r| r.kind == RowKind::Color).count(), 3);
        let title = rows.iter().find(|r| r.text == "Show the titlebar").unwrap();
        let mut settings = Settings::default();
        for value in [0, 1, 2] {
            assert!(title.apply_value(&mut settings, value));
            assert_eq!(title.current_value(&settings), value);
        }
    }

    #[test]
    fn shortcut_arrow_link_opens_menu_below_link_at_each_dpi() {
        let mut settings = Settings::default();
        for dpi in [96, 144, 240] {
            let width = sc(1000.0, dpi);
            let height = sc(1600.0, dpi);
            let lay = layout_page(
                Page::Personalization,
                &settings,
                dpi,
                width as f32,
                height as f32,
                0,
            )
            .unwrap();
            let section = lay.sections.iter().find(|s| s.header == "Icons").unwrap();
            let row = section
                .rows
                .iter()
                .find(|r| r.row.text == "Shortcut arrows")
                .unwrap();
            assert_eq!(row.row.kind, RowKind::Navigate);
            let at = (row.x as i32 + 2, (row.y + row.h / 2.0) as i32);
            assert_eq!(
                hit_test(&settings, Page::Personalization, at, dpi, width, height, 0).unwrap(),
                Hit::ShortcutArrows {
                    anchor: (row.x as i32, (row.y + row.h) as i32)
                }
            );
            let before = settings.clone();
            assert!(
                apply_click(
                    &mut settings,
                    Page::Personalization,
                    at,
                    dpi,
                    width,
                    height,
                    0
                )
                .is_none()
            );
            assert_eq!(
                settings, before,
                "opening/canceling the menu must not toggle a value"
            );
        }
    }

    fn px(s: &Surface, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * s.width() + x) * 4) as usize;
        s.pixels()[i..i + 4].try_into().unwrap()
    }

    fn out_dir() -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("qa")
            .join("settings");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn corral_rows_wrap_and_controls_fit_at_minimum_and_default_sizes() {
        for dpi in [96, 120, 144, 240] {
            let s = Settings::default();
            for page in Page::ALL {
                let nav_w = measure_text(page.label(), UI, pt_px(10.5, dpi), 400)
                    .unwrap()
                    .0;
                assert!(nav_w <= scf(172.0, dpi), "nav {page:?} at {dpi}: {nav_w}");
                for width in [760.0, 1000.0, 1800.0] {
                    let lay =
                        layout_page(page, &s, dpi, scf(width, dpi), scf(520.0, dpi), 0).unwrap();
                    assert!(lay.title_rect.left >= nav_width(dpi) as f32);
                    assert!(lay.title_rect.right <= scf(width - 40.0, dpi) + 1.0);
                    for section in &lay.sections {
                        assert!(section.card.left >= nav_width(dpi) as f32);
                        assert!(section.card.right <= scf(width - 40.0, dpi) + 1.0);
                        assert!(section.card.right - section.card.left <= scf(960.0, dpi) + 1.0);
                        if width == 1800.0 {
                            // Maximized: the column is centred in the space right of the nav.
                            let gap_l = section.card.left - nav_width(dpi) as f32 - scf(24.0, dpi);
                            let gap_r = scf(width - 40.0, dpi) - section.card.right;
                            assert!((gap_l - gap_r).abs() <= scf(2.0, dpi) + 1.0, "{page:?} {gap_l} {gap_r}");
                        }
                        for row in &section.rows {
                            assert!(
                                row.x >= section.card.left
                                    && row.x + row.w <= section.card.right + 1.0
                            );
                            assert!(
                                row.y >= section.card.top
                                    && row.y + row.h <= section.card.bottom + 1.0
                            );
                            if matches!(
                                row.row.control_kind(),
                                RowKind::Toggle
                                    | RowKind::Dropdown
                                    | RowKind::Segmented
                                    | RowKind::Button
                                    | RowKind::ButtonRow
                                    | RowKind::Stepper
                            ) {
                                let r = control_rect(row, dpi).unwrap();
                                assert!(
                                    r.left >= row.x - 1.0 && r.right <= row.x + row.w + 1.0,
                                    "{page:?} {:?} control outside row at {width} DIP/{dpi} dpi: {r:?}",
                                    row.row.text
                                );
                                assert!(r.top >= row.y - 1.0 && r.bottom <= row.y + row.h + 1.0);
                                if row.row.kind == RowKind::Segmented {
                                    let per_segment = (r.right - r.left - scf(4.0, dpi))
                                        / row.row.choices.len() as f32;
                                    for (label, _) in row.row.choices {
                                        let measured = measure_text(
                                            label,
                                            kit::theme::font_family(),
                                            scf(14.0, dpi),
                                            600,
                                        )
                                        .unwrap()
                                        .0;
                                        assert!(measured + scf(16.0, dpi) <= per_segment + 1.0);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn u1_fix_navigation_and_focus_fit_minimum_client_at_every_dpi() {
        let settings = Settings::default();
        for dpi in [96, 120, 144, 240] {
            for height in [436.0, 520.0, 584.0, 720.0] {
                let h = sc(height, dpi);
                for (index, page) in Page::ALL.iter().enumerate() {
                    let (x, y, w, row_h) = nav_item_rect(index, dpi, h);
                    assert!(
                        y >= sc(4.0, dpi) && y + row_h + sc(4.0, dpi) <= h,
                        "nav/focus clipped: {page:?} at {height} DIP/{dpi}"
                    );
                    if height == 720.0 {
                        assert_eq!(row_h, sc(36.0, dpi));
                    }
                    for scroll in [0, 73, 600] {
                        let targets =
                            focus_targets(&settings, *page, dpi, sc(760.0, dpi), h, scroll)
                                .unwrap();
                        assert_eq!(targets[0].0, Hit::Nav(*page));
                        assert_eq!(targets[0].1.top, y as f32);
                        assert_eq!(targets[0].1.bottom, (y + row_h) as f32);
                        assert_eq!(
                            hit_test(
                                &settings,
                                Page::Personalization,
                                (x + w / 2, y + row_h / 2),
                                dpi,
                                sc(760.0, dpi),
                                h,
                                scroll
                            )
                            .unwrap(),
                            Hit::Nav(*page)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn u1_fix_inline_help_reserves_bullets_before_wrapping_at_minimum_size() {
        let page = content(Page::FolderPortals);
        for dpi in [96, 120, 144, 240] {
            let row_width = scf(760.0 - 256.0 - 24.0 - 40.0 - 32.0, dpi);
            for row in page
                .sections
                .iter()
                .flat_map(|section| &section.rows)
                .filter(|row| row.kind == RowKind::Expander)
            {
                for source in row.lines {
                    let lines = inline_help_lines(source, row_width, dpi).unwrap();
                    assert_eq!(
                        lines.join(" "),
                        source.split_whitespace().collect::<Vec<_>>().join(" ")
                    );
                    for line in lines {
                        let measured =
                            measure_text(&line, kit::theme::font_family(), scf(12.0, dpi), 400)
                                .unwrap()
                                .0;
                        assert!(
                            measured <= row_width - scf(40.0, dpi) + 1.0,
                            "help text clipped at {dpi}: {line}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn u1_fix_stepper_mouse_parts_apply_and_clamp_existing_spacing() {
        for dpi in [96, 120, 144, 240] {
            let mut settings = Settings::default();
            settings.set(Field::SnapEnabled, 1);
            settings.set(Field::SnapSpaced, 1);
            settings.snap_padding = 50;
            for width in [760.0, 1000.0] {
                let w = sc(width, dpi);
                let h = sc(520.0, dpi);
                let layout =
                    layout_page(Page::Layout, &settings, dpi, w as f32, h as f32, 0).unwrap();
                let row = layout
                    .sections
                    .iter()
                    .flat_map(|section| &section.rows)
                    .find(|row| row.row.control_kind() == RowKind::Stepper)
                    .unwrap();
                let r = control_rect(row, dpi).unwrap();
                let scale = dpi as f32 / 96.0;
                let parts = kit::controls::stepper_parts(
                    RectF::new(
                        r.left / scale,
                        r.top / scale,
                        r.right / scale,
                        r.bottom / scale,
                    ),
                    dpi,
                );
                for (part, expected) in
                    parts
                        .iter()
                        .zip([Hit::SpacingStep(-1), Hit::Spacing, Hit::SpacingStep(1)])
                {
                    let point = (
                        ((part.left + part.right) / 2.0) as i32,
                        ((part.top + part.bottom) / 2.0) as i32,
                    );
                    assert_eq!(
                        hit_test(&settings, Page::Layout, point, dpi, w, h, 0).unwrap(),
                        expected
                    );
                }
                assert!(
                    focus_targets(&settings, Page::Layout, dpi, w, h, 0)
                        .unwrap()
                        .iter()
                        .any(|(hit, _)| *hit == Hit::Spacing),
                    "keyboard targets the value, not either button"
                );
                settings.set(Field::SnapEnabled, 0);
                assert_eq!(
                    hit_test(
                        &settings,
                        Page::Layout,
                        (
                            ((r.left + r.right) / 2.0) as i32,
                            ((r.top + r.bottom) / 2.0) as i32
                        ),
                        dpi,
                        w,
                        h,
                        0
                    )
                    .unwrap(),
                    Hit::Nothing
                );
                settings.set(Field::SnapEnabled, 1);
            }
            assert!(adjust_spacing(&mut settings, 1));
            assert_eq!(settings.snap_padding, 51);
            assert!(adjust_spacing(&mut settings, -1));
            assert_eq!(settings.snap_padding, 50);
            assert!(settings.snap_should_scale);
            adjust_spacing(&mut settings, 1000);
            assert_eq!(settings.snap_padding, 100);
            assert!(!adjust_spacing(&mut settings, 1));
            adjust_spacing(&mut settings, -1000);
            assert_eq!(settings.snap_padding, 0);
            assert!(!adjust_spacing(&mut settings, -1));
        }
    }

    #[test]
    fn u1_figma_pilot_geometry_and_matched_viewport_renders() {
        // Figma Fold 49:5007: outer 1000x678, 1-DIP border and 32-DIP caption => 998x644 body.
        // These coordinates are from the approved frame metadata, independent of our layout arithmetic.
        EXPANDERS.with(|states| states.borrow_mut().clear());
        let mut settings = Settings::default();
        content::apply_fold_open_mode(&mut settings, 1);
        let fold = layout_page(Page::Rollup, &settings, 96, 998.0, 644.0, 0).unwrap();
        let bounds = |r: RectF| [r.left, r.top, r.right, r.bottom];
        assert_eq!(bounds(fold.title_rect)[..2], [280.0, 8.0]);
        assert_eq!(bounds(fold.sections[0].card), [280.0, 124.0, 958.0, 242.0]);
        assert_eq!(bounds(fold.sections[1].card), [280.0, 290.0, 958.0, 350.0]);
        assert_eq!(bounds(fold.sections[2].card), [280.0, 366.0, 958.0, 416.0]);
        for (index, top) in [
            8.0, 84.0, 122.0, 198.0, 236.0, 274.0, 312.0, 350.0, 388.0, 464.0, 502.0, 540.0, 592.0,
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(nav_item_rect(index, 96, 644), (16, top as i32, 224, 36));
        }
        // About 52:6991: icon y24, centred text y27, notice card y112..184.
        let about = layout_page(Page::About, &settings, 96, 998.0, 644.0, 0).unwrap();
        assert_eq!(about.title_rect.top, 27.0);
        assert_eq!(about.subtitle[0].1.top, 65.0);
        assert_eq!(bounds(about.sections[0].card), [280.0, 112.0, 958.0, 184.0]);
        // Full Toggle row includes the horizontal padding inside the card border.
        assert_eq!(
            hit_test(&settings, Page::Rollup, (282, 154), 96, 998, 644, 0).unwrap(),
            Hit::Toggle(Field::RollupDoubleClick)
        );
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/qa/settings-redesign/u1");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
            for page in [Page::Rollup, Page::About, Page::FolderPortals] {
                let mut surface = Surface::new(998, 644).unwrap();
                render_settings_state(
                    &mut surface,
                    &View {
                        page,
                        hover: None,
                        scroll: 0,
                    },
                    &settings,
                    &theme,
                    96,
                )
                .unwrap();
                surface
                    .save_png(&dir.join(format!("figma-body-{name}-{page:?}.png").to_lowercase()))
                    .unwrap();
            }
        }
    }

    #[test]
    fn u1_pilot_pages_golden_renders() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/qa/settings-redesign/u1");
        std::fs::create_dir_all(&dir).unwrap();
        EXPANDERS.with(|states| states.borrow_mut().clear());
        backups::collapse();
        for (name, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
            for dpi in [96, 120, 144, 240] {
                for page in [
                    Page::Rollup,
                    Page::FolderPortals,
                    Page::About,
                    Page::Backups,
                    Page::Peek,
                ] {
                    for (width, height, suffix) in [(1000.0, 720.0, ""), (760.0, 520.0, "-minimum")]
                    {
                        let mut surface =
                            Surface::new(sc(width, dpi) as u32, sc(height, dpi) as u32).unwrap();
                        render_settings_state(
                            &mut surface,
                            &View {
                                page,
                                hover: None,
                                scroll: 0,
                            },
                            &Settings::default(),
                            &theme,
                            dpi,
                        )
                        .unwrap();
                        surface
                            .save_png(
                                &dir.join(
                                    format!("{name}-{dpi}-{page:?}{suffix}.png").to_lowercase(),
                                ),
                            )
                            .unwrap();
                    }
                }
            }
        }
    }

    #[test]
    fn u1_pilot_controls_hit_their_own_fields_and_actions_at_each_dpi() {
        for dpi in [96, 120, 144, 240] {
            let settings = Settings::default();
            let (w, h) = (sc(1000.0, dpi), sc(1600.0, dpi));
            let fold = layout_page(Page::Rollup, &settings, dpi, w as f32, h as f32, 0).unwrap();
            let (_, r) = fold.master.unwrap();
            let centre = |r: RectF| {
                (
                    ((r.left + r.right) / 2.0) as i32,
                    ((r.top + r.bottom) / 2.0) as i32,
                )
            };
            assert_eq!(
                hit_test(&settings, Page::Rollup, centre(r), dpi, w, h, 0).unwrap(),
                Hit::Toggle(Field::RollupEnabled)
            );
            let opening = fold
                .sections
                .iter()
                .flat_map(|s| &s.rows)
                .find(|r| r.row.kind == RowKind::Segmented)
                .unwrap();
            let r = control_rect(opening, dpi).unwrap();
            for mode in 0..3 {
                let x = r.left + (r.right - r.left) * (mode as f32 + 0.5) / 3.0;
                assert_eq!(
                    hit_test(
                        &settings,
                        Page::Rollup,
                        (x as i32, ((r.top + r.bottom) / 2.0) as i32),
                        dpi,
                        w,
                        h,
                        0
                    )
                    .unwrap(),
                    Hit::FoldMode(mode)
                );
            }
            let mut disabled = settings.clone();
            disabled.rollup_enabled = false;
            assert_eq!(
                hit_test(&disabled, Page::Rollup, centre(r), dpi, w, h, 0).unwrap(),
                Hit::Nothing
            );
            for (page, text, expected) in [
                (
                    Page::FolderPortals,
                    "Subfolders use the parent folder view",
                    Hit::Toggle(Field::InheritViewFromParent),
                ),
                (Page::About, "Settings and layouts", Hit::OpenSettingsFolder),
                (Page::Backups, "Save a backup now", Hit::SaveBackup),
                (
                    Page::Backups,
                    "Save a backup every day",
                    Hit::Toggle(Field::DailyBackups),
                ),
            ] {
                let lay = layout_page(page, &settings, dpi, w as f32, h as f32, 0).unwrap();
                let row = lay
                    .sections
                    .iter()
                    .flat_map(|s| &s.rows)
                    .find(|r| r.row.text == text)
                    .unwrap();
                assert_eq!(
                    hit_test(
                        &settings,
                        page,
                        centre(control_rect(row, dpi).unwrap()),
                        dpi,
                        w,
                        h,
                        0
                    )
                    .unwrap(),
                    expected,
                    "{page:?} dpi={dpi}"
                );
            }
        }
    }

    #[test]
    fn u1_nav_is_independent_of_content_scroll_and_expander_header_stays_put() {
        let settings = Settings::default();
        for dpi in [96, 120, 144, 240] {
            let (w, h) = (sc(1000.0, dpi), sc(720.0, dpi));
            for scroll in [0, 73, 600] {
                for (i, page) in Page::ALL.iter().enumerate() {
                    let (x, y, nw, nh) = nav_item_rect(i, dpi, sc(720.0, dpi));
                    assert_eq!(
                        hit_test(
                            &settings,
                            Page::Personalization,
                            (x + nw / 2, y + nh / 2),
                            dpi,
                            w,
                            h,
                            scroll
                        )
                        .unwrap(),
                        Hit::Nav(*page)
                    );
                }
            }
            EXPANDERS.with(|states| states.borrow_mut().clear());
            let open =
                layout_page(Page::FolderPortals, &settings, dpi, w as f32, h as f32, 73).unwrap();
            toggle_expander(Page::FolderPortals, "How it works");
            let closed =
                layout_page(Page::FolderPortals, &settings, dpi, w as f32, h as f32, 73).unwrap();
            assert_eq!(open.sections[0].rows[0].y, closed.sections[0].rows[0].y);
            assert!(open.sections[1].card.top > closed.sections[1].card.top);
            assert_eq!(open.title_rect.top, closed.title_rect.top);
        }
        EXPANDERS.with(|states| states.borrow_mut().clear());
    }

    #[test]
    fn renders_every_page_to_png() {
        let dir = out_dir();
        for (tname, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for dpi in [96u32, 144, 240] {
                for page in Page::ALL {
                    let mut s = Surface::new(sc(828.0, dpi) as u32, sc(655.0, dpi) as u32).unwrap();
                    let hover = if page == Page::Welcome {
                        Some(Page::Peek)
                    } else {
                        None
                    };
                    render_settings(&mut s, page, hover, &theme, dpi).unwrap();
                    let name = format!("{tname}-{dpi}-{page:?}.png").to_lowercase();
                    s.save_png(&dir.join(name)).unwrap();
                }
            }
        }
    }

    /// QA: client-size renders (1048x767 = a 1064x798 frame minus the 8 px frame and 31 px
    /// caption) of the pages, written to `target/qa/settings-look/`.
    #[test]
    fn renders_look_comparison_pages() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("qa")
            .join("settings-look");
        std::fs::create_dir_all(&dir).unwrap();
        let tag = std::env::var("LOOK_TAG").unwrap_or_else(|_| "after".to_string());
        for page in [
            Page::Welcome,
            Page::Personalization,
            Page::Rules,
            Page::Layout,
            Page::Rollup,
        ] {
            let mut s = Surface::new(1048, 767).unwrap();
            let v = View {
                page,
                hover: None,
                scroll: 0,
            };
            let settings = Settings::default();
            render_settings_state(&mut s, &v, &settings, &Theme::dark(), 96).unwrap();
            s.save_png(&dir.join(format!("{tag}-{page:?}.png").to_lowercase()))
                .unwrap();
        }
    }

    #[test]
    fn hit_test_matches_items_and_rejects_outside() {
        for dpi in [96u32, 144, 192] {
            for (i, page) in Page::ALL.iter().enumerate() {
                let (x, y, w, h) = nav_item_rect(i, dpi, sc(720.0, dpi));
                assert_eq!(
                    nav_hit_test((x + w / 2, y + h / 2), dpi, sc(720.0, dpi)),
                    Some(*page),
                    "dpi {dpi} {page:?}"
                );
                assert_eq!(nav_hit_test((x, y), dpi, sc(720.0, dpi)), Some(*page));
                assert_eq!(nav_hit_test((x - 1, y + h / 2), dpi, sc(720.0, dpi)), None);
            }
            assert_eq!(nav_hit_test((-5, 40), dpi, sc(720.0, dpi)), None);
            assert_eq!(
                nav_hit_test((nav_width(dpi), 40), dpi, sc(720.0, dpi)),
                None
            );
            assert_eq!(
                nav_hit_test((nav_width(dpi) + 100, 40), dpi, sc(720.0, dpi)),
                None
            );
            assert_eq!(nav_hit_test((sc(100.0, dpi), 2), dpi, sc(720.0, dpi)), None);
            let (_, y, _, h) = nav_item_rect(Page::ALL.len() - 1, dpi, sc(720.0, dpi));
            assert_eq!(
                nav_hit_test((sc(100.0, dpi), y + h + 2), dpi, sc(720.0, dpi)),
                None
            );
        }
    }

    #[test]
    fn selected_accent_bar_is_accent_coloured() {
        let accent = Rgba::new(200, 50, 60, 255);
        for theme in [
            Theme::dark().with_accent(accent),
            Theme::light().with_accent(accent),
        ] {
            let expected = [theme.accent.b, theme.accent.g, theme.accent.r, 255];
            assert_ne!(
                theme.accent, accent,
                "settings use Corral amber even when Windows supplies an accent"
            );
            for dpi in [96u32, 144, 240] {
                for (i, page) in Page::ALL.iter().enumerate() {
                    let mut s = Surface::new(sc(828.0, dpi) as u32, sc(655.0, dpi) as u32).unwrap();
                    render_settings(&mut s, *page, None, &theme, dpi).unwrap();
                    let (x, y, _, h) = nav_item_rect(i, dpi, sc(655.0, dpi));
                    let p = px(&s, (x + sc(1.5, dpi) / 2 + 1) as u32, (y + h / 2) as u32);
                    assert_eq!(p, expected, "{page:?} dpi {dpi}");
                    // An unselected item has no accent pixel at the same place.
                    let other = (i + 1) % Page::ALL.len();
                    let (ox, oy, _, oh) = nav_item_rect(other, dpi, sc(655.0, dpi));
                    let q = px(&s, (ox + 2) as u32, (oy + oh / 2) as u32);
                    assert_ne!(q, expected);
                }
            }
        }
    }

    #[test]
    fn backups_page_tiles_save_button_and_layout_separation() {
        let s = Settings::default();
        let (w, h, dpi) = (1000, 900, 96u32);
        // The page always exposes the save action and tile list, with no reveal link.
        backups::collapse();
        let layout = layout_page(Page::Layout, &s, dpi, w as f32, h as f32, 0).unwrap();
        assert!(
            layout
                .sections
                .iter()
                .flat_map(|s| &s.rows)
                .all(|r| r.row.kind != RowKind::Tiles && r.row.bind != Some(Field::DailyBackups))
        );
        let empty = layout_page(Page::Backups, &s, dpi, w as f32, h as f32, 0).unwrap();
        assert!(
            empty
                .sections
                .iter()
                .flat_map(|s| &s.rows)
                .any(|r| r.row.kind == RowKind::Tiles)
        );
        let mk = |n: u64| backups::Tile {
            id: format!("{n:032x}"),
            title: format!("Backup {n}"),
            created_ms: n,
            automatic: false,
        };
        backups::expand((1..=7).map(mk).collect());
        let lay = layout_page(Page::Backups, &s, dpi, w as f32, h as f32, 0).unwrap();
        let rows: Vec<_> = lay.sections.iter().flat_map(|s| &s.rows).collect();
        assert!(rows.iter().all(|r| r.row.text != backups::LINK_TEXT));
        let tiles = rows
            .iter()
            .find(|r| r.row.kind == RowKind::Tiles)
            .expect("tiles row");
        let button = rows
            .iter()
            .find(|r| r.row.button_text == Some(backups::SAVE_TEXT))
            .expect("save button");
        assert!(
            button.y + button.h <= tiles.y,
            "the save action precedes the list"
        );
        let r = backups::action_rect(
            3,
            0,
            tiles.x,
            tiles.y + backups::notice_height(dpi),
            tiles.w,
            dpi,
        );
        let centre = (
            ((r.left + r.right) / 2.0) as i32,
            ((r.top + r.bottom) / 2.0) as i32,
        );
        assert_eq!(
            hit_test(&s, Page::Backups, centre, dpi, w, h, 0).unwrap(),
            Hit::BackupRestore(3)
        );
        let control = control_rect(button, dpi).unwrap();
        let at = (
            (control.left + control.right) as i32 / 2,
            (control.top + control.bottom) as i32 / 2,
        );
        assert_eq!(
            hit_test(&s, Page::Backups, at, dpi, w, h, 0).unwrap(),
            Hit::SaveBackup
        );
        // A longer list makes the page taller (it scrolls) and still draws in both themes.
        let tall = content_height(&s, Page::Backups, dpi, w, h).unwrap();
        backups::expand((1..=40).map(mk).collect());
        assert!(content_height(&s, Page::Backups, dpi, w, h).unwrap() > tall);
        for theme in [Theme::dark(), Theme::light()] {
            let mut surface = Surface::new(w as u32, h as u32).unwrap();
            render_settings_state(
                &mut surface,
                &View {
                    page: Page::Backups,
                    hover: None,
                    scroll: 0,
                },
                &s,
                &theme,
                dpi,
            )
            .unwrap();
        }
        backups::collapse();
    }

    // ---- click application (no window) ----

    const W: i32 = 828;
    /// Test viewport height (the settings window's initial client height).
    const H: i32 = 655;

    /// Centre of the first row of `page` whose text is `text` (client pixels).
    fn centre_of(settings: &Settings, page: Page, text: &str, dpi: u32, scroll: i32) -> (i32, i32) {
        let width = sc(W as f32, dpi);
        let lay = layout_page(
            page,
            settings,
            dpi,
            width as f32,
            sc(H as f32, dpi) as f32,
            scroll,
        )
        .unwrap();
        if text == "@master" {
            let (_, r) = lay.master.expect("page master switch");
            return (
                ((r.left + r.right) / 2.0) as i32,
                ((r.top + r.bottom) / 2.0) as i32,
            );
        }
        for sec in &lay.sections {
            for r in &sec.rows {
                if r.row.text == text {
                    if matches!(
                        r.row.control_kind(),
                        RowKind::Toggle
                            | RowKind::Dropdown
                            | RowKind::Segmented
                            | RowKind::Button
                            | RowKind::ButtonRow
                            | RowKind::Stepper
                    ) {
                        let box_rect = control_rect(r, dpi).unwrap();
                        return (
                            ((box_rect.left + box_rect.right) / 2.0) as i32,
                            ((box_rect.top + box_rect.bottom) / 2.0) as i32,
                        );
                    }
                    return ((r.x + scf(20.0, dpi)) as i32, (r.y + r.h / 2.0) as i32);
                }
            }
        }
        panic!("row {text:?} not visible on {page:?}");
    }

    fn click(s: &mut Settings, page: Page, text: &str, dpi: u32, scroll: i32) -> Option<Change> {
        let p = centre_of(s, page, text, dpi, scroll);
        apply_click(
            s,
            page,
            p,
            dpi,
            sc(W as f32, dpi),
            sc(H as f32, dpi),
            scroll,
        )
    }

    #[test]
    fn clicks_toggle_the_bound_fields() {
        for dpi in [96u32, 144, 240] {
            let mut s = Settings::default();
            // Fold master switch, then dependent rows become inert.
            let c = click(&mut s, Page::Rollup, "@master", dpi, 0).unwrap();
            assert_eq!(
                c,
                Change {
                    field: Field::RollupEnabled,
                    value: 0
                }
            );
            assert!(!s.rollup_enabled);
            assert!(
                click(
                    &mut s,
                    Page::Rollup,
                    "Double-click a title to fold or unfold",
                    dpi,
                    0
                )
                .is_none()
            );
            assert!(s.rollup_double_click_to_toggle);
            click(&mut s, Page::Rollup, "@master", dpi, 0).unwrap();
            assert!(s.rollup_enabled);
            click(
                &mut s,
                Page::Rollup,
                "Double-click a title to fold or unfold",
                dpi,
                0,
            )
            .unwrap();
            assert!(!s.rollup_double_click_to_toggle);
            // Advanced retains the bound animation/scrollbar fields.
            EXPANDERS.with(|e| {
                e.borrow_mut()
                    .insert((Page::Personalization, "Advanced"), true);
            });
            click(
                &mut s,
                Page::Personalization,
                "Animate icon movement",
                dpi,
                0,
            )
            .unwrap();
            assert_eq!(s.icon_animation_disabled, 0);
            click(
                &mut s,
                Page::Personalization,
                "Scrollbars only on hover",
                dpi,
                0,
            )
            .unwrap();
            assert_eq!(s.scrollbar_mode, 0);
            EXPANDERS.with(|e| {
                e.borrow_mut().remove(&(Page::Personalization, "Advanced"));
            });
            // Pages: everything below "Enable pages" is inert until it is on
            assert!(click(&mut s, Page::Pages, "Animation", dpi, 0).is_none());
            click(&mut s, Page::Pages, "@master", dpi, 0).unwrap();
            assert!(s.swipe_enabled);
            click(
                &mut s,
                Page::Pages,
                "Dragging with the middle button",
                dpi,
                0,
            )
            .unwrap();
            assert!(!s.swipe_switch_via_middle_drag);
            // Layout radio group
            click(
                &mut s,
                Page::Layout,
                "Remember a layout for each monitor setup",
                dpi,
                0,
            )
            .unwrap();
            assert!(s.store_layouts_per_multimon);
            click(&mut s, Page::Layout, "Snap Corrals while moving", dpi, 0).unwrap();
            assert!(!s.snap_enabled);
            // The merged dropdown uses the production choice adapter.
            let mut h = Settings::default();
            apply_choice(&mut h, Field::AutoHideIdle, 30000);
            assert_eq!(h.quick_hide_auto_hide_on_idle, 30000);
            apply_choice(&mut h, Field::AutoHideIdle, 0);
            assert_eq!(h.quick_hide_auto_hide_on_idle, 0);
        }
    }

    #[test]
    fn welcome_enable_and_links_and_combo() {
        let dpi = 96;
        let mut s = Settings::default();
        let c = click(&mut s, Page::Welcome, "Corral is on", dpi, 0).unwrap();
        assert_eq!(
            c,
            Change {
                field: Field::EnableCorral,
                value: 0
            }
        );
        assert!(s.disabled);
        let p = centre_of(&s, Page::Welcome, "Fold", dpi, 0);
        assert_eq!(
            hit_test(&s, Page::Welcome, p, dpi, W, H, 0).unwrap(),
            Hit::Goto(Page::Rollup)
        );
        let p = centre_of(&s, Page::Welcome, "Create Corrals with", dpi, 0);
        let p = (p.0 - 30, p.1);
        match hit_test(&s, Page::Welcome, p, dpi, W, H, 0).unwrap() {
            Hit::Combo { field, .. } => {
                assert_eq!(field, Field::CreateCorralMode);
                apply_choice(&mut s, field, 2);
                assert_eq!(s.create_corral_with_mouse_mode, 2);
            }
            other => panic!("{other:?}"),
        }
        // nav hit
        let (nx, ny, nw, nh) = nav_item_rect(Page::Rollup.index(), dpi, H);
        assert_eq!(
            hit_test(&s, Page::Welcome, (nx + nw / 2, ny + nh / 2), dpi, W, H, 0).unwrap(),
            Hit::Nav(Page::Rollup)
        );
    }

    #[test]
    fn scrollbar_margin_decision_is_pure() {
        assert_eq!(right_margin(false), 40.0);
        assert_eq!(right_margin(true), 40.0);
        assert!(page_scrolls(656.0, 655.0));
        assert!(!page_scrolls(655.0, 655.0));
        assert!(!page_scrolls(100.0, 655.0));
    }

    #[test]
    fn overflowing_page_uses_the_scrollbar_margin_and_scales_with_dpi() {
        let s = Settings::default();
        for dpi in [96u32, 144, 240] {
            let width = sc(W as f32, dpi) as f32;
            let card_right = |page: Page, height: f32| {
                layout_page(page, &s, dpi, width, height, 0)
                    .unwrap()
                    .sections[0]
                    .card
                    .right
            };
            // Personalization overflows 655 (see scroll_moves_hit_targets_and_is_bounded); a huge viewport fits it.
            let fits = card_right(Page::Personalization, 100_000.0);
            let scrolls = card_right(Page::Personalization, 655.0);
            assert_eq!(fits, width - scf(40.0, dpi));
            assert_eq!(scrolls, width - scf(40.0, dpi));
            // The margin does not change with the scroll offset.
            let moved = layout_page(Page::Personalization, &s, dpi, width, 655.0, 100)
                .unwrap()
                .sections[0]
                .card
                .right;
            assert_eq!(moved, scrolls);
        }
    }

    #[test]
    fn scroll_moves_hit_targets_and_is_bounded() {
        let dpi = 96;
        let s = Settings::default();
        let h = content_height(&s, Page::Personalization, dpi, W, 655).unwrap();
        assert!(
            h > 655,
            "Personalization overflows the initial window ({h})"
        );
        let max = max_scroll(&s, Page::Personalization, dpi, W, 655).unwrap();
        assert_eq!(max, h - 655);
        let a = centre_of(&s, Page::Personalization, "Tint icons", dpi, 0);
        let b = centre_of(&s, Page::Personalization, "Tint icons", dpi, 100);
        assert_eq!(a.1 - b.1, 100);
        let m = Settings::default();
        assert!(matches!(
            hit_test(&m, Page::Personalization, b, dpi, W, H, 100).unwrap(),
            Hit::LookAction(look_pages::Action::TintMode(_))
        ));
        assert!(
            !m.desktop_tint_icons,
            "hit testing alone does not apply a typed tint action"
        );
        assert_eq!(max_scroll(&s, Page::About, dpi, W, 655).unwrap(), 0);
    }

    #[test]
    fn fold_master_on_and_off_render_distinctly_with_amber() {
        let dpi = 96;
        let theme = Theme::dark();
        let mut off = Settings::default();
        off.rollup_enabled = false;
        let on = Settings::default();
        let mut a = Surface::new(sc(828.0, dpi) as u32, sc(655.0, dpi) as u32).unwrap();
        let mut b = Surface::new(sc(828.0, dpi) as u32, sc(655.0, dpi) as u32).unwrap();
        let v = View {
            page: Page::Rollup,
            hover: None,
            scroll: 0,
        };
        render_settings_state(&mut a, &v, &on, &theme, dpi).unwrap();
        render_settings_state(&mut b, &v, &off, &theme, dpi).unwrap();
        let lay = layout_page(Page::Rollup, &on, dpi, 828.0, 655.0, 0).unwrap();
        let (_, master) = lay.master.unwrap();
        let mut changes = 0;
        let expected = [theme.accent.b, theme.accent.g, theme.accent.r, 255];
        let mut amber_pixels = 0;
        for y in master.top as u32..master.bottom as u32 {
            for x in master.left as u32..master.right as u32 {
                changes += usize::from(px(&a, x, y) != px(&b, x, y));
                amber_pixels += usize::from(px(&a, x, y) == expected);
            }
        }
        assert!(changes > 100, "the master visibly changes state");
        assert!(
            amber_pixels > 100,
            "the on master uses the theme's amber fill"
        );
        a.save_png(&out_dir().join("rollup-checked.png")).unwrap();
        b.save_png(&out_dir().join("rollup-unchecked.png")).unwrap();
    }
}

#[cfg(test)]
mod q33h_link_tests {
    use super::*;

    /// Click the centre of a link row of `page` (tall window so nothing is clipped).
    fn click(page: Page, text: &str, dpi: u32) -> Hit {
        let s = Settings::default();
        let (w, h) = (sc(828.0, dpi), sc(3000.0, dpi));
        let lay = layout_page(page, &s, dpi, w as f32, h as f32, 0).unwrap();
        let row = lay
            .sections
            .iter()
            .flat_map(|s| &s.rows)
            .find(|r| r.row.kind == RowKind::Link && r.row.text == text)
            .unwrap_or_else(|| panic!("no link {text:?} on {page:?}"));
        hit_test(
            &s,
            page,
            ((row.x + 4.0) as i32, (row.y + row.h / 2.0) as i32),
            dpi,
            w,
            h,
            0,
        )
        .unwrap()
    }

    #[test]
    fn style_links_open_their_popups_and_help_links_open_their_text() {
        // every help link on every page is hit as `Hit::Help` with its own label
        let mut count = 0;
        for page in Page::ALL {
            for section in content::content(page).sections {
                for row in section.rows.iter().filter(|r| r.kind == RowKind::Link) {
                    if help::lookup(row.text).is_some() {
                        assert_eq!(
                            click(page, row.text, 96),
                            Hit::Help(help::lookup(row.text).unwrap().label),
                            "{page:?} {}",
                            row.text
                        );
                        count += 1;
                    }
                }
            }
        }
        let inline_gate_help = help::HELP
            .iter()
            .filter(|h| h.label.contains("Gate"))
            .count();
        assert_eq!(
            count
                + inline_gate_help
                + help::HELP
                    .iter()
                    .filter(|h| !h.label.contains("Gate"))
                    .count(),
            help::HELP.len(),
            "Folder Gate help is now inline; legacy entries stay available to other callers"
        );
    }
}

#[cfg(test)]
mod u4_full_page_tests {
    use super::*;
    #[test]
    fn integrated_organize_pages_render_with_fixed_navigation_and_action_focus() -> Result<()> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/qa/settings-redesign/u4");
        std::fs::create_dir_all(&dir).unwrap();
        let mut settings = Settings::default();
        for (i, rule) in settings.round_up.rules.iter_mut().take(3).enumerate() {
            rule.enabled = true;
            rule.destination = Some(crate::round_up::Destination::Corral { id: (i + 1) as u64 });
        }
        for text in ["invoice", "receipt"] {
            let mut rule = settings.round_up.new_user_rule();
            rule.conditions = vec![crate::round_up::Condition::Name {
                op: crate::round_up::TextOp::Contains,
                value: text.into(),
            }];
            rule.destination = Some(crate::round_up::Destination::Corral { id: 1 });
            settings.round_up.insert_rule(rule);
        }
        let groups: Vec<_> = ["Projects", "Documents", "Photos"]
            .into_iter()
            .enumerate()
            .map(|(i, title)| GroupInfo {
                id: (i + 1) as i32,
                title: title.into(),
                portal: false,
                items: 4,
            })
            .collect();
        for light in [true, false] {
            let theme = if light { Theme::light() } else { Theme::dark() };
            for dpi in [96, 120, 144, 240] {
                for (name, width, height) in [("default", 1000., 720.), ("minimum", 760., 520.)] {
                    let (w, h) = (sc(width, dpi), sc(height, dpi));
                    for page in [Page::Backups, Page::Rules] {
                        backups::expand(vec![
                            backups::Tile {
                                id: "manual-qa".into(),
                                title: "Before reorganizing".into(),
                                created_ms: 1_792_300_000_000,
                                automatic: false,
                            },
                            backups::Tile {
                                id: "daily-qa".into(),
                                title: "Daily backup".into(),
                                created_ms: 1_792_200_000_000,
                                automatic: true,
                            },
                            backups::Tile {
                                id: "daily-qa-2".into(),
                                title: "Daily backup".into(),
                                created_ms: 1_792_100_000_000,
                                automatic: true,
                            },
                            backups::Tile {
                                id: "manual-qa-2".into(),
                                title: "Work setup, two monitors".into(),
                                created_ms: 1_792_000_000_000,
                                automatic: false,
                            },
                        ]);
                        backups::set_notice(None);
                        let maximum = max_scroll_with(&settings, &groups, page, dpi, w, h)?;
                        for (position, scroll) in [("top", 0), ("bottom", maximum)] {
                            let mut surface = Surface::new(w as u32, h as u32)?;
                            set_focus_ring(None);
                            set_control_pointer(None, false);
                            render_settings_editor(
                                &mut surface,
                                &View {
                                    page,
                                    hover: None,
                                    scroll,
                                },
                                &settings,
                                &groups,
                                &theme,
                                dpi,
                                None,
                            )?;
                            surface.save_png(
                                &dir.join(
                                    format!(
                                        "integrated-{page:?}-{}-{dpi}-{name}-{position}.png",
                                        if light { "light" } else { "dark" }
                                    )
                                    .to_lowercase(),
                                ),
                            )?;
                        }
                        let targets = focus_targets_with(&settings, &groups, page, dpi, w, h, 0)?;
                        assert!(targets.len() > 1);
                        assert!(matches!(targets[0].0,Hit::Nav(p) if p==page));
                        let (nx, ny, nw, nh) = nav_item_rect(page.index(), dpi, h);
                        for scroll in [0, maximum] {
                            assert_eq!(
                                hit_test(
                                    &settings,
                                    page,
                                    (nx + nw / 2, ny + nh / 2),
                                    dpi,
                                    w,
                                    h,
                                    scroll
                                )?,
                                Hit::Nav(page)
                            );
                        }
                        if page == Page::Backups {
                            let actions: Vec<_> = targets
                                .iter()
                                .filter(|(hit, _)| {
                                    matches!(hit, Hit::BackupRestore(_) | Hit::BackupMore(_))
                                })
                                .collect();
                            assert_eq!(actions.len(), 8);
                            for (hit, r) in actions {
                                assert_eq!(
                                    hit_test(
                                        &settings,
                                        page,
                                        (
                                            ((r.left + r.right) / 2.) as i32,
                                            ((r.top + r.bottom) / 2.) as i32
                                        ),
                                        dpi,
                                        w,
                                        h,
                                        0
                                    )?,
                                    *hit
                                );
                            }
                        } else {
                            assert!(targets.iter().any(|(hit, _)| matches!(
                                hit,
                                Hit::RuleAction(rules_page::Action::RuleDest { .. })
                            )));
                        }
                    }
                }
            }
        }
        backups::collapse();
        set_focus_ring(None);
        set_control_pointer(None, false);
        Ok(())
    }
}
#[cfg(test)]
mod u3_page_tests {
    use super::*;
    fn reset_scope(settings: &Settings) {
        look_pages::set_scope(None, vec![], false);
        look_pages::set_effective(&crate::color_state::effective_of(settings, &Default::default()));
        set_control_pointer(None, false);
        set_focus_ring(None);
    }
    #[test]
    fn scope_disabled_rows_and_expansion_share_stable_layout_at_every_dpi() -> Result<()> {
        let settings = Settings::default();
        reset_scope(&settings);
        for dpi in [96, 120, 144, 240] {
            for size in [(1000.0, 720.0), (760.0, 520.0)] {
                let (w, h) = (sc(size.0, dpi), sc(size.1, dpi));
                EXPANDERS.with(|e| {
                    e.borrow_mut()
                        .insert((Page::Personalization, "Advanced"), false)
                });
                let closed = layout_page(
                    Page::Personalization,
                    &settings,
                    dpi,
                    w as f32,
                    h as f32,
                    sc(200.0, dpi),
                )?;
                let header = closed.sections.last().unwrap().card.top;
                toggle_expander(Page::Personalization, "Advanced");
                let open = layout_page(
                    Page::Personalization,
                    &settings,
                    dpi,
                    w as f32,
                    h as f32,
                    sc(200.0, dpi),
                )?;
                assert_eq!(
                    header,
                    open.sections.last().unwrap().card.top,
                    "expander header is the scroll anchor"
                );
                assert!(open.bottom > closed.bottom);
                look_pages::set_scope(Some("Downloads".into()), vec!["Downloads".into()], true);
                let scoped =
                    layout_page(Page::Personalization, &settings, dpi, w as f32, h as f32, 0)?;
                for row in scoped.sections.iter().flat_map(|s| &s.rows) {
                    if !look_pages::supports_group(&row.row) {
                        assert!(!row.enabled);
                        assert_eq!(row.row.desc, Some("Applies to all Corrals."));
                    }
                }
                for (hit, r) in focus_targets(&settings, Page::Personalization, dpi, w, h, 0)? {
                    assert!(r.left >= 0.0 && r.right <= w as f32, "{hit:?}: {r:?}");
                }
                let caption = scoped.scope.as_ref().unwrap();
                assert!(caption.dropdown.right <= w as f32 - scf(40.0, dpi));
                reset_scope(&settings);
            }
        }
        EXPANDERS.with(|e| e.borrow_mut().remove(&(Page::Personalization, "Advanced")));
        Ok(())
    }
    #[test]
    fn golden_look_pages_both_themes_four_dpis_default_minimum_and_scopes() -> Result<()> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/qa/settings-redesign/u3");
        std::fs::create_dir_all(&dir).unwrap();
        let mut settings = Settings::default();
        settings.desktop_blend_icons = true;
        settings.desktop_blend_icon_opacity = 178;
        settings.color = look_pages::preset(look_pages::ColorChannel::Background, 2).unwrap();
        settings.transparency_factor = 1100;
        settings.outline = true;
        settings.darken_titlebars = 0;
        settings.desktop_tint_icons = true;
        settings.desktop_tint_icons_color =
            look_pages::preset(look_pages::ColorChannel::Tint, 5).unwrap();
        crate::settings::set_layout_flags(crate::settings::LayoutFlags {
            any_group_styled: true,
            any_group_blend: true,
            any_group_faded: true,
        });
        for dpi in [96, 120, 144, 240] {
            for (theme_name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
                for (size_name, wd, ht) in [("default", 1000.0, 720.0), ("minimum", 760.0, 520.0)] {
                    let (w, h) = (sc(wd, dpi), sc(ht, dpi));
                    for page in [Page::Personalization, Page::Camouflage] {
                        let page_name = if page == Page::Personalization {
                            "personalization"
                        } else {
                            "camouflage"
                        };
                        for (variant, expanded, scoped, bottom) in if page == Page::Personalization
                        {
                            vec![
                                ("top", false, false, false),
                                ("bottom", false, false, true),
                                ("advanced", true, false, true),
                                ("scoped-top", false, true, false),
                                ("scoped-bottom", true, true, true),
                            ]
                        } else {
                            vec![
                                ("top", false, false, false),
                                ("bottom", false, false, true),
                                ("expanded", true, false, true),
                            ]
                        } {
                            reset_scope(&settings);
                            if page == Page::Personalization {
                                look_pages::set_scope(
                                    if scoped {
                                        Some("Downloads".into())
                                    } else {
                                        None
                                    },
                                    vec!["Downloads".into(), "Games".into()],
                                    scoped,
                                );
                                EXPANDERS
                                    .with(|e| e.borrow_mut().insert((page, "Advanced"), expanded));
                            } else {
                                EXPANDERS.with(|e| {
                                    e.borrow_mut().insert((page, "How it works"), expanded)
                                });
                            }
                            let scroll = if bottom {
                                max_scroll(&settings, page, dpi, w, h)?
                            } else {
                                0
                            };
                            let mut surface = Surface::new(w as u32, h as u32)?;
                            let view = View {
                                page,
                                hover: None,
                                scroll,
                            };
                            render_settings_state(&mut surface, &view, &settings, &theme, dpi)?;
                            surface.save_png(&dir.join(format!(
                                "{page_name}-{theme_name}-{dpi}-{size_name}-{variant}.png"
                            )))?;
                            for (_, r) in focus_targets(&settings, page, dpi, w, h, scroll)? {
                                assert!(r.left >= 0.0 && r.right <= w as f32 + 1.0);
                            }
                        }
                    }
                }
            }
        }
        reset_scope(&settings);
        EXPANDERS.with(|e| {
            e.borrow_mut().remove(&(Page::Personalization, "Advanced"));
            e.borrow_mut().remove(&(Page::Camouflage, "How it works"));
        });
        crate::settings::set_layout_flags(crate::settings::LayoutFlags::default());
        Ok(())
    }
    #[test]
    fn long_unbroken_scope_title_is_readable_at_minimum_size() -> Result<()> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/qa/settings-redesign/u3");
        std::fs::create_dir_all(&dir).unwrap();
        let settings = Settings::default();
        for dpi in [96, 240] {
            for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
                reset_scope(&settings);
                look_pages::set_scope(Some("W".repeat(255)), vec![], true);
                let (w, h) = (sc(760.0, dpi), sc(520.0, dpi));
                let mut surface = Surface::new(w as u32, h as u32)?;
                render_settings_state(
                    &mut surface,
                    &View {
                        page: Page::Personalization,
                        hover: None,
                        scroll: 0,
                    },
                    &settings,
                    &theme,
                    dpi,
                )?;
                surface
                    .save_png(&dir.join(format!("personalization-long-scope-{name}-{dpi}.png")))?;
            }
        }
        reset_scope(&settings);
        Ok(())
    }
    #[test]
    fn shared_sliders_cover_endpoints_without_rewriting_inbetween_values() -> Result<()> {
        let mut settings = Settings::default();
        settings.background_blur_radius = 48.5;
        settings.background_blur_enabled = true;
        reset_scope(&settings);
        for dpi in [96, 120, 144, 240] {
            let (w, h) = (sc(1000.0, dpi), sc(2000.0, dpi));
            let lay = layout_page(Page::Personalization, &settings, dpi, w as f32, h as f32, 0)?;
            for row in lay
                .sections
                .iter()
                .flat_map(|s| &s.rows)
                .filter(|r| r.row.kind == RowKind::ValueSlider)
            {
                for (hit, r) in look_pages::row_targets(row, dpi)? {
                    assert_eq!(
                        hit_test(
                            &settings,
                            Page::Personalization,
                            (
                                ((r.left + r.right) / 2.0) as i32,
                                ((r.top + r.bottom) / 2.0) as i32
                            ),
                            dpi,
                            w,
                            h,
                            0
                        )?,
                        hit
                    );
                    if let Hit::LookAction(look_pages::Action::Slider { kind, left, right }) = hit {
                        assert_eq!(look_pages::slider_value_at(kind, left, left, right), 0);
                        assert_eq!(
                            look_pages::slider_value_at(kind, right, left, right),
                            if kind == look_pages::SliderKind::Blur {
                                4
                            } else {
                                100
                            }
                        );
                    }
                }
            }
        }
        assert_eq!(settings.background_blur_radius, 48.5);
        Ok(())
    }
}
