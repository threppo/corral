//! Static page content for the settings window: header, subtitle, section headers and card rows.
//!
//! The approved Corral redesign is in `docs/design/settings-redesign-spec.md`. Legacy declarations
//! keep their structure until their U2-U4 conversion; `Row::control_kind` supplies the kit
//! default mapping without changing their fields or gates. Declarations use `Row::new`, `.b(field)`,
//! `.g(gates)` and optional `.choices`, `.lines`, `.button`, `.destination` or `.range` metadata.
//! Controllers pair `Row::current_value` with `Row::apply_value` for merged field mappings, and
//! own persistence/reload after applying a value. Hidden, licensing and business-only items stay omitted.

use super::Page;
use crate::settings::{Field, HOTKEY_CHOICES};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Status,
    NumberedStep,
    ExploreTile,
    TabStyleTile,
    Toggle,
    Segmented,
    Dropdown,
    Color,
    Navigate,
    ButtonRow,
    RadioRow,
    CheckRow,
    Stepper,
    Expander,
    InfoBox,
    /// A settings-field slider; `choices` selects named steps, empty means percent.
    ValueSlider,
    Number,
    Check,
    Radio,
    Combo,
    Button,
    /// Hyperlink-coloured text.
    Link,
    /// Plain label (card title when `desc` is set).
    Label,
    /// Grey small note.
    Note,
    /// The existing backup tile list (Backups page, `backups.rs`; U4 replaces its presentation).
    Tiles,
}

/// Condition on another field's truthiness ("gates the next N controls").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    LayoutAuto,
    /// desk-setups-spec 6: available only for remembered monitor setups.
    DeskSetups,
    MultiMonitor,
    /// Row is enabled only while the field is on.
    Enable(Field),
    /// Row is enabled only while the field is off.
    EnableNot(Field),
    /// Row is shown only while the field is on.
    Show(Field),
    /// Shown only on Windows build >= n (`Utils.IsWin11BuildOrGreater(n)`; Windows 11 = 22000).
    BuildAtLeast(u32),
    /// Shown only on Windows build < n.
    BuildBelow(u32),
}

/// `Utils.IsWin11()`: the Peek "Hot Corner" section exists on Windows 11 only.
const WIN11: u32 = 22000;
/// Windows 11 24H2: its own "Show desktop" path replaces the old WinKey+D speed-up (`Initalize`).
const WIN11_24H2: u32 = 26100;

#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub kind: RowKind,
    pub text: &'static str,
    /// Description under a card title (Welcome page style).
    pub desc: Option<&'static str>,
    /// Settings field behind a checkbox / radio / combo.
    pub bind: Option<Field>,
    /// Radio: the field value this radio selects.
    pub value: i64,
    pub gates: &'static [Gate],
    /// Link: page to switch to when clicked (Welcome page cards).
    pub goto: Option<Page>,
    /// Explicit display choices; empty dropdowns use `combo_choices` for the bound field.
    pub choices: &'static [(&'static str, i64)],
    /// Additional field for merged controls (Fold opening mode).
    pub secondary_bind: Option<Field>,
    /// Inline expander bullets or information body lines.
    pub lines: &'static [&'static str],
    /// Initial state only; the window remembers the user's choice per page.
    pub expanded: bool,
    pub button_text: Option<&'static str>,
    pub primary: bool,
    /// Check-row destination chip; selection is supplied by the page's controller.
    pub destination_chip: Option<&'static str>,
    /// Inclusive minimum/maximum and keyboard step for steppers and continuous sliders.
    pub range: Option<(i64, i64, i64)>,
}

impl Row {
    pub const fn new(kind: RowKind, text: &'static str, desc: Option<&'static str>) -> Row {
        Row {
            kind,
            text,
            desc,
            bind: None,
            value: 0,
            gates: &[],
            goto: None,
            choices: &[],
            secondary_bind: None,
            lines: &[],
            expanded: false,
            button_text: None,
            primary: false,
            destination_chip: None,
            range: None,
        }
    }
    pub const fn b(mut self, field: Field) -> Row {
        self.bind = Some(field);
        self
    }
    pub const fn v(mut self, field: Field, value: i64) -> Row {
        self.bind = Some(field);
        self.value = value;
        self
    }
    pub const fn g(mut self, gates: &'static [Gate]) -> Row {
        self.gates = gates;
        self
    }
    pub const fn to(mut self, page: Page) -> Row {
        self.goto = Some(page);
        self
    }
    pub const fn choices(mut self, choices: &'static [(&'static str, i64)]) -> Row {
        self.choices = choices;
        self
    }
    pub const fn secondary(mut self, field: Field) -> Row {
        self.secondary_bind = Some(field);
        self
    }
    pub const fn lines(mut self, lines: &'static [&'static str], expanded: bool) -> Row {
        self.lines = lines;
        self.expanded = expanded;
        self
    }
    pub const fn button(mut self, text: &'static str, primary: bool) -> Row {
        self.button_text = Some(text);
        self.primary = primary;
        self
    }
    pub const fn destination(mut self, text: &'static str) -> Row {
        self.destination_chip = Some(text);
        self
    }
    pub const fn range(mut self, min: i64, max: i64, step: i64) -> Row {
        self.range = Some((min, max, step));
        self
    }

    /// Explicit compatibility mapping keeps old page declarations behaviorally unchanged.
    pub const fn control_kind(self) -> RowKind {
        match self.kind {
            RowKind::Check => RowKind::Toggle,
            RowKind::Radio => RowKind::RadioRow,
            RowKind::Combo => RowKind::Dropdown,
            RowKind::Link if self.goto.is_some() => RowKind::Navigate,
            RowKind::Number => RowKind::Stepper,
            other => other,
        }
    }

    /// Display value for ordinary and merged settings. Keep this paired with `apply_value`.
    pub fn current_value(self, settings: &crate::settings::Settings) -> i64 {
        if self.bind == Some(Field::RollupRequireClick)
            && self.secondary_bind == Some(Field::RollupRequireDelay)
        {
            fold_open_mode(settings)
        } else if self.bind == Some(Field::AutoHideIdle) {
            idle_mode(settings)
        } else if self.bind == Some(Field::SwipeAnimate) && self.secondary_bind.is_some() {
            settings.swipe_animation_style as i64
        } else {
            self.bind.map_or(self.value, |field| settings.get(field))
        }
    }

    /// Updates the existing settings model; the caller still owns save/reload/live-apply.
    /// Returns false for inert rows and values outside an explicit choice list.
    pub fn apply_value(self, settings: &mut crate::settings::Settings, value: i64) -> bool {
        if !self.choices.is_empty() && !self.choices.iter().any(|choice| choice.1 == value) {
            return false;
        }
        if self.bind == Some(Field::RollupRequireClick)
            && self.secondary_bind == Some(Field::RollupRequireDelay)
        {
            return apply_fold_open_mode(settings, value);
        }
        if self.bind == Some(Field::AutoHideIdle) {
            return apply_idle_mode(settings, value);
        }
        if self.bind == Some(Field::SwipeAnimate) && self.secondary_bind.is_some() {
            return apply_animation(settings, value);
        }
        if let Some(field) = self.bind {
            settings.set(field, value);
            true
        } else {
            false
        }
    }

    /// Runtime-only copy stays out of the static declarations (expanded settings folder path).
    pub fn description(self) -> Option<String> {
        if self.text == "This desk setup" {
            Some(super::desk_setups::status())
        } else if self.button_text == Some("Open folder") {
            crate::settings::default_path()
                .parent()
                .map(|path| path.display().to_string())
        } else if self.kind == RowKind::ValueSlider && matches!(self.text, "Blur" | "Wallpaper blur") && crate::tint::live_wallpaper_host() {
            // wallpaper-blur-spec 3.3: a live-wallpaper host is never captured; say what blur uses instead.
            Some(format!("{} {LIVE_WALLPAPER_NOTE}", self.desc.unwrap_or_default()).trim().to_owned())
        } else {
            self.desc.map(str::to_owned)
        }
    }
}

/// Caption under the Blur rows while a live-wallpaper host runs (wallpaper-blur-spec 3.3).
pub const LIVE_WALLPAPER_NOTE: &str = "With a live wallpaper, blur uses the still picture Windows reports.";

pub const HIDE_CHOICES: &[(&str, i64)] = &[("Everything", 0), ("Loose icons", 1), ("Corrals", 2)];
pub const IDLE_CHOICES: &[(&str, i64)] = crate::settings::AUTO_HIDE_IDLE_CHOICES;
pub const ANIMATION_CHOICES: &[(&str, i64)] = &[("Off", 0), ("Slide", 1), ("Bounce", 2)];

pub fn idle_mode(settings: &crate::settings::Settings) -> i64 {
    if settings.is_on(Field::AutoHideIdle) {
        settings.get(Field::AutoHideIdleChoice)
    } else {
        0
    }
}
// Existing storage has one duration: zero is off. There is no independent persisted choice.
// Never follows that established disable path; retaining a prior duration across Never needs
// a schema decision by the lead (recorded in the U2 handoff), not an unused UI shadow value.
pub fn apply_idle_mode(settings: &mut crate::settings::Settings, value: i64) -> bool {
    if !IDLE_CHOICES.iter().any(|c| c.1 == value) {
        return false;
    }
    if value == 0 {
        settings.set(Field::AutoHideIdle, 0);
    } else {
        if !settings.is_on(Field::AutoHideIdle) {
            settings.set(Field::AutoHideIdle, 1);
        }
        settings.set(Field::AutoHideIdleChoice, value);
    }
    true
}
pub fn apply_animation(settings: &mut crate::settings::Settings, value: i64) -> bool {
    if !(0..=2).contains(&value) {
        return false;
    }
    settings.swipe_animation_style = value as u32;
    true
}
pub fn choice_value(settings: &crate::settings::Settings, field: Field) -> i64 {
    if field == Field::AutoHideIdle {
        idle_mode(settings)
    } else {
        settings.get(field)
    }
}

pub const FOLD_OPEN_CHOICES: &[(&str, i64)] =
    &[("On hover", 0), ("After a pause", 1), ("On click", 2)];

/// A click requirement takes precedence over the remembered hover-delay choice.
pub fn fold_open_mode(settings: &crate::settings::Settings) -> i64 {
    if settings.is_on(Field::RollupRequireClick) {
        2
    } else if settings.is_on(Field::RollupRequireDelay) {
        1
    } else {
        0
    }
}

/// Choosing click leaves the remembered delay untouched.
pub fn apply_fold_open_mode(settings: &mut crate::settings::Settings, mode: i64) -> bool {
    match mode {
        0 | 1 => {
            settings.set(Field::RollupRequireClick, 0);
            settings.set(Field::RollupRequireDelay, mode);
        }
        2 => settings.set(Field::RollupRequireClick, 1),
        _ => return false,
    }
    true
}

#[derive(Clone, Debug)]
pub struct Section {
    /// Empty = no section header here (Camouflage controls under the screenshot).
    pub header: &'static str,
    /// Joined list card (Welcome page: JoinTop/JoinBottom stack, one row per group).
    pub joined: bool,
    pub rows: Vec<Row>,
    /// Visibility gates of the whole section (header and card).
    pub gates: &'static [Gate],
}

impl Section {
    fn when(mut self, gates: &'static [Gate]) -> Section {
        self.gates = gates;
        self
    }
}

#[derive(Clone, Debug)]
pub struct PageContent {
    pub title: &'static str,
    pub subtitle: &'static str,
    pub sections: Vec<Section>,
    pub master: Option<Field>,
}

/// Legacy capture button retained for old controller callers; the Backups page uses a button row.
pub const SAVE_BACKUP_ROW: Row = Row::new(RowKind::Button, super::backups::SAVE_TEXT, None);
pub const TILES_ROW: Row = Row::new(RowKind::Tiles, "", None);

const fn chk(text: &'static str) -> Row {
    Row::new(RowKind::Check, text, None)
}
const fn rad(text: &'static str) -> Row {
    Row::new(RowKind::Radio, text, None)
}
const fn cmb(text: &'static str) -> Row {
    Row::new(RowKind::Combo, text, None)
}
const fn btn(text: &'static str) -> Row {
    Row::new(RowKind::Button, text, None)
}
const fn lnk(text: &'static str) -> Row {
    Row::new(RowKind::Link, text, None)
}
const fn lbl(text: &'static str) -> Row {
    Row::new(RowKind::Label, text, None)
}
const fn note(text: &'static str) -> Row {
    Row::new(RowKind::Note, text, None)
}
const fn card_l(text: &'static str, desc: &'static str) -> Row {
    Row::new(RowKind::Label, text, Some(desc))
}
const fn card_k(text: &'static str, desc: &'static str) -> Row {
    Row::new(RowKind::Link, text, Some(desc))
}
fn sec(header: &'static str, rows: Vec<Row>) -> Section {
    Section {
        header,
        joined: false,
        rows,
        gates: &[],
    }
}
fn joined(header: &'static str, rows: Vec<Row>) -> Section {
    Section {
        header,
        joined: true,
        rows,
        gates: &[],
    }
}

pub fn content(page: Page) -> PageContent {
    match page {
        // 4.1
        Page::Welcome => PageContent {
            title: "Welcome to Corral",
            subtitle: "Keep desktop icons in Corrals and browse folders through Gates.",
            master: None,
            sections: vec![
                sec("", vec![Row::new(RowKind::Status, "Corral is on", Some("Your Corrals, Gates and shortcuts are active. Turn Corral off to show the plain desktop.")).b(Field::EnableCorral)]),
                sec("Get started", vec![
                    Row::new(RowKind::NumberedStep, "Create a Corral", Some("Drag across an empty part of the desktop, then choose Create Corral here.")).v(Field::EnableCorral, 1),
                    Row::new(RowKind::NumberedStep, "Name it", Some("Click the Corral title and type a name.")).v(Field::EnableCorral, 2),
                    Row::new(RowKind::NumberedStep, "Move and resize", Some("Drag the title to move a Corral. Drag an edge to resize it.")).v(Field::EnableCorral, 3),
                ]),
                sec("Explore", vec![
                    Row::new(RowKind::ExploreTile, "Personalization", Some("Colors, see-through and titlebars.")).to(Page::Personalization),
                    Row::new(RowKind::ExploreTile, "Camouflage", Some("Icons that blend into the wallpaper.")).to(Page::Camouflage),
                    Row::new(RowKind::ExploreTile, "Folder Gates", Some("Browse a folder right on the desktop.")).to(Page::FolderPortals),
                    Row::new(RowKind::ExploreTile, "Tabs", Some("Stack Corrals in one place as tabs.")).to(Page::Tabs),
                    Row::new(RowKind::ExploreTile, "Fold", Some("Fold Corrals down to their titles.")).to(Page::Rollup),
                    Row::new(RowKind::ExploreTile, "Clear view", Some("Temporarily hide desktop icons and Corrals.")).to(Page::Hide),
                    Row::new(RowKind::ExploreTile, "Glance", Some("Bring Corrals above your windows.")).to(Page::Peek),
                    Row::new(RowKind::ExploreTile, "Ranges", Some("Organize icons across separate desktop pages.")).to(Page::Pages),
                    Row::new(RowKind::ExploreTile, "Round-up", Some("Send new icons to the right Corral.")).to(Page::Rules),
                    Row::new(RowKind::ExploreTile, "Backups", Some("Save and restore your layout.")).to(Page::Backups),
                ]),
                sec("General", vec![Row::new(RowKind::Dropdown, "Create Corrals with", Some("Which mouse button draws a new Corral on the desktop.")).b(Field::CreateCorralMode)]),
            ],
        },
        Page::Camouflage => PageContent {
            title: "Camouflage",
            subtitle: "Icons take on the colors of the wallpaper behind them, for a quieter desktop.",
            master: Some(Field::BlendIcons),
            sections: vec![
                sec("Blending", vec![
                    Row::new(RowKind::ValueSlider, "Blend strength", Some("How strongly icon colors follow the wallpaper.")).b(Field::BlendIconOpacity),
                    Row::new(RowKind::ValueSlider, "Wallpaper blur", Some("Smooths the colors Camouflage picks up. Same setting as Blur in Personalization.")).g(&[Gate::EnableNot(Field::BlendUsesTintColor)]),
                    Row::new(RowKind::Toggle, "Blend Corral backgrounds", Some("Also tint the Corral background, titlebar and scrollbar the same way.")).b(Field::BlendBackgrounds),
                    Row::new(RowKind::Toggle, "Use the icon tint color", Some("Blend icons toward your tint color from Personalization instead of the wallpaper.")).b(Field::BlendUsesTintColor),
                ]).when(&[Gate::Show(Field::BlendOptionsVisible)]),
                sec("Show real colors", vec![
                    Row::new(RowKind::Toggle, "When you point at a Corral", Some("Its icons show their real colors while the pointer is over it.")).b(Field::BlendColorOnMouseover),
                    Row::new(RowKind::Toggle, "While the desktop is active", Some("All icons show their real colors after you click the desktop, until you switch to a window.")).b(Field::BlendColorOnActive),
                ]).when(&[Gate::Show(Field::BlendOptionsVisible)]),
                sec("", vec![Row::new(RowKind::Expander, "How it works", None).lines(&[
                    "Camouflage picks a dark and a light color from the wallpaper behind each Corral and redraws its icons with them.",
                    "Choose a different Corral in Personalization to give it its own Camouflage setting.",
                    "Point at a Corral to see its real colors when that option is on.",
                ], false)]),
            ],
        },
        Page::Personalization => PageContent {
            title: "Personalization",
            subtitle: "Choose how your Corrals look. Changes show on the desktop right away.",
            master: None,
            sections: vec![
                sec("Background", vec![
                    Row::new(RowKind::Color, "Color", Some("Behind the icons and the titlebar.")),
                    Row::new(RowKind::ValueSlider, "See-through", Some("How much of the wallpaper shows through.")),
                    Row::new(RowKind::ValueSlider, "Blur", Some("Softens the wallpaper behind Corrals. Also used by Camouflage.")),
                    Row::new(RowKind::Toggle, "Rounded corners", None),
                    Row::new(RowKind::Toggle, "Outline", Some("A thin line, lighter or darker than the Corral, around each Corral.")),
                ]),
                sec("Titlebar", vec![
                    Row::new(RowKind::Segmented, "Show the titlebar", Some("On hover keeps the desktop calmer.")).b(Field::LabelMode).choices(&[("Always",2),("On hover",1),("Never",0)]),
                    Row::new(RowKind::Color, "Text color", Some("Auto picks light or dark text for contrast.")),
                    Row::new(RowKind::Segmented, "Titlebar shade", Some("Darken the titlebar so the name stands out.")).choices(&[("None",0),("Dark",1),("Darker",2)]),
                ]),
                sec("Icons", vec![
                    Row::new(RowKind::Segmented, "Camouflage", Some("Override Camouflage for this Corral.")).choices(&[("Like all Corrals",0),("On",1),("Off",2)]),
                    Row::new(RowKind::Segmented, "Tint icons", Some("Color the icons inside Corrals.")).choices(&[("Off",0),("Match Corral",1),("Custom",2)]),
                    Row::new(RowKind::Color, "Tint color", Some("Used when Tint icons is Custom.")),
                    Row::new(RowKind::Navigate, "Shortcut arrows", Some("Choose which shortcut arrows to hide on icons in Corrals.")),
                ]),
                sec("", vec![
                    Row::new(RowKind::Expander, "Advanced", None),
                    Row::new(RowKind::Toggle, "Titlebar buttons on hover", Some("Show the menu and close buttons when you point at a titlebar.")),
                    Row::new(RowKind::Toggle, "Titlebar below near the screen top", Some("When a Corral touches the top edge, its titlebar moves underneath.")),
                    Row::new(RowKind::Toggle, "Animate icon movement", Some("Icons glide to their new place when Corrals are sorted or resized.")).b(Field::AnimateIcons),
                    Row::new(RowKind::Toggle, "Scrollbars only on hover", Some("Hide Corral scrollbars until you point at the Corral.")).b(Field::HideScrollbars),
                    Row::new(RowKind::Toggle, "Restore see-through on hover", Some("A faded Corral becomes solid while you point at it.")).b(Field::FadeInOpacity).g(&[Gate::Show(Field::FadedGroupsVisible)]),
                ]),
            ],
        },
        // Appearance section is omitted
        Page::FolderPortals => PageContent {
            title: "Folder Gates",
            subtitle: "Show a folder on the desktop and browse it without opening a window.",
            master: None,
            sections: vec![
                sec("", vec![Row::new(RowKind::Expander, "How it works", None).lines(&[
                    "Create a Gate: drag across the desktop and choose Create Folder Gate here, or hold Alt while you drop a folder on the desktop.",
                    "Change its view, icon size or sort order from the menu in the top-left corner; Ctrl + mouse wheel resizes icons.",
                    "Removing a Gate never deletes the folder. Double-click empty space inside a Gate to open the folder in Explorer.",
                ], true)]),
                sec("Settings", vec![Row::new(RowKind::Toggle, "Subfolders use the parent folder view", Some("When you open a subfolder in a Gate, it keeps the view of the folder above it.")).b(Field::InheritViewFromParent)]),
            ],
        },
        // 4.5. Hot Corner is Windows 11 only; the WinKey+D speed-up and its combos are hidden from
        // Windows 11 build 26100 on. The labels beside the combos stay enabled (only the
        // combos follow their check box). "Show icon as a running task" needs `ShowInTaskbarAsRunningEver`,
        // which nothing in the UI sets: not offered.
        Page::Peek => PageContent {
            title: "Glance",
            subtitle: "Bring your Corrals above open windows with a shortcut, the tray icon or a screen corner.",
            master: None,
            sections: vec![
                // peek-hotkeys-spec 8: the warning appears while the keyboard hook is not available
                sec("", vec![Row::new(RowKind::InfoBox, "Shortcuts are not available", Some("Corral could not listen for keys. Try turning the shortcuts off and on."))])
                    .when(&[Gate::Show(Field::PeekHookFailed)]),
                sec("Shortcuts", vec![
                    Row::new(RowKind::Toggle, "Glance shortcuts", Some("Use the keys below anywhere in Windows.")).b(Field::HotkeysGlance),
                    Row::new(RowKind::Dropdown, "Show Corrals until you press again", Some("Press once to show, again to hide.")).b(Field::HotkeyGlanceToggle).g(&[Gate::Enable(Field::HotkeysGlance)]),
                    Row::new(RowKind::Label, "Windows also uses Win + Space to switch the input language.", Some("While shortcuts are on, Corral takes it.")).g(&[Gate::Show(Field::PeekWinSpaceNote)]),
                    Row::new(RowKind::Label, "The saved toggle shortcut is no longer available.", Some("Corral uses Win + Space instead. Pick another one to replace it.")).g(&[Gate::Show(Field::PeekToggleFallback)]),
                    Row::new(RowKind::Dropdown, "Show Corrals while held", Some("Corrals stay up only while you hold the keys.")).b(Field::HotkeyGlanceHold).g(&[Gate::Enable(Field::HotkeysGlance)]),
                    Row::new(RowKind::Label, "The saved hold shortcut is no longer available.", Some("Corral uses Win + Ctrl + Space instead. Pick another one to replace it.")).g(&[Gate::Show(Field::PeekHoldFallback)]),
                    Row::new(RowKind::Toggle, "Keep Glance open when you click other windows", Some("Otherwise a click elsewhere ends Glance.")).b(Field::PeekKeepOpen).g(&[Gate::Enable(Field::HotkeysGlance)]),
                    Row::new(RowKind::Toggle, "Speed up Show desktop (Win + D)", None).b(Field::HotkeysDesktop).g(&[Gate::BuildBelow(WIN11_24H2)]),
                    Row::new(RowKind::Dropdown, "Show the desktop until you press again", None).b(Field::HotkeyDesktopToggle).g(&[Gate::BuildBelow(WIN11_24H2), Gate::Enable(Field::HotkeysDesktop)]),
                    Row::new(RowKind::Dropdown, "Show the desktop while held", None).b(Field::HotkeyDesktopHold).g(&[Gate::BuildBelow(WIN11_24H2), Gate::Enable(Field::HotkeysDesktop)]),
                ]),
                sec("Tray icon", vec![Row::new(RowKind::Toggle, "Show the Corral icon in the notification area", Some("Click it to Glance at your Corrals.")).b(Field::ShowInTaskbar)]),
                sec("Screen corner", vec![
                    Row::new(RowKind::Toggle, "Use the bottom-right corner", Some("Click the far corner of the taskbar.")).b(Field::HotCornerEnabled),
                    Row::new(RowKind::Dropdown, "Left click", None).b(Field::HotCornerLeft).g(&[Gate::Show(Field::HotCornerEnabled)]),
                    Row::new(RowKind::Dropdown, "Right click", None).b(Field::HotCornerRight).g(&[Gate::Show(Field::HotCornerEnabled)]),
                ]).when(&[Gate::BuildAtLeast(WIN11)]),
            ],
        },
        Page::Hide => PageContent {
            title: "Clear view",
            subtitle: "Double-click an empty part of the desktop to hide your icons. Double-click again to bring them back.",
            master: Some(Field::QuickHideEnabled),
            sections: vec![
                sec("", vec![Row::new(RowKind::Expander, "How it works", None).lines(&[
                    "Double-click an empty spot on the desktop: icons fade out. Double-click again to show them.",
                    "To keep a Corral visible, right-click inside it and choose View > Keep visible in Clear view.",
                    "Clear view only hides. It never moves or deletes files.",
                ], true)]),
                sec("What to hide", vec![Row::new(RowKind::Segmented, "Hide", Some("Everything means icons and Corrals. Loose icons are the ones outside any Corral.")).b(Field::QuickHideWhat).choices(HIDE_CHOICES)]),
                sec("Automatically", vec![
                    Row::new(RowKind::Dropdown, "Hide when the desktop is idle", Some("Icons fade out after this long without activity.")).b(Field::AutoHideIdle).secondary(Field::AutoHideIdleChoice).choices(IDLE_CHOICES).g(&[Gate::Enable(Field::QuickHideEnabled)]),
                    Row::new(RowKind::Toggle, "Show icons when you click the desktop", Some("Hidden icons come back as soon as the desktop is active.")).b(Field::AutoShow).g(&[Gate::Enable(Field::QuickHideEnabled)]),
                    Row::new(RowKind::Toggle, "Show icons while you drop files on the desktop", Some("So you can see where a dragged item will land.")).b(Field::AutoShowOnDrag).g(&[Gate::Enable(Field::QuickHideEnabled), Gate::Show(Field::AutoShow), Gate::Enable(Field::AutoShow)]),
                    Row::new(RowKind::Toggle, "Start with icons visible", None).b(Field::AutoShowOnBoot).g(&[Gate::Enable(Field::QuickHideEnabled), Gate::EnableNot(Field::AutoHideIdle)]),
                ]),
            ],
        },
        Page::Rollup => PageContent {
            title: "Fold",
            subtitle: "Fold a Corral down to its title and open it only when you need it.",
            master: Some(Field::RollupEnabled),
            sections: vec![
                sec("Folding", vec![
                    Row::new(RowKind::Toggle, "Double-click a title to fold or unfold", Some("The quickest way to fold a single Corral.")).b(Field::RollupDoubleClick).g(&[Gate::Enable(Field::RollupEnabled)]),
                    Row::new(RowKind::Toggle, "Fold Corrals docked at a screen edge", Some("Corrals pushed against the top or bottom edge fold automatically.")).b(Field::RollupOnDock).g(&[Gate::Enable(Field::RollupEnabled)]),
                ]),
                sec("Opening", vec![
                    Row::new(RowKind::Segmented, "Open a folded Corral", Some("On hover is fastest; On click never opens by accident."))
                        .b(Field::RollupRequireClick).secondary(Field::RollupRequireDelay)
                        .choices(FOLD_OPEN_CHOICES).g(&[Gate::Enable(Field::RollupEnabled)]),
                ]),
                sec("", vec![Row::new(RowKind::Expander, "How it works", None).lines(&[
                    "Double-click a Corral title to fold it, and again to unfold it.",
                    "Point at a folded title to see its icons; move away and it folds again.",
                    "Drag a Corral to the top or bottom edge of the screen to dock it there.",
                ], false).g(&[Gate::Enable(Field::RollupEnabled)])]),
            ],
        },
        // 4.8
        Page::Tabs => PageContent {
            title: "Tabs",
            subtitle: "Stack several Corrals in one place and switch between them with tabs.",
            master: None,
            sections: vec![
                sec("Tab style", vec![
                    Row::new(RowKind::TabStyleTile, "Menu only", None).v(Field::TabsStyle, 0),
                    Row::new(RowKind::TabStyleTile, "Flat", None).v(Field::TabsStyle, 1),
                    Row::new(RowKind::TabStyleTile, "Segmented", None).v(Field::TabsStyle, 2),
                    Row::new(RowKind::TabStyleTile, "Rounded", None).v(Field::TabsStyle, 3),
                ]),
                sec("Behavior", vec![
                    Row::new(RowKind::Toggle, "Make tabs by dropping one Corral onto another", Some("Turn off to stop new tab groups. Existing tabs stay.")).b(Field::TabsCreate),
                    Row::new(RowKind::Toggle, "Ask before combining Corrals", Some("Confirm each time you drop a Corral onto another one.")).b(Field::TabsConfirm),
                    Row::new(RowKind::Toggle, "Switch tabs on hover", Some("Point at a tab to show it, without clicking.")).b(Field::TabsMouseover),
                    Row::new(RowKind::Toggle, "Wait briefly before switching", Some("Avoids switching when the pointer just passes over a tab.")).b(Field::TabsMouseoverDelay).g(&[Gate::Show(Field::TabsMouseover)]),
                    Row::new(RowKind::Toggle, "Color each tab like its Corral", Some("Shown when at least one Corral has its own color.")).b(Field::TabsColors).g(&[Gate::Show(Field::TabsColorsVisible)]),
                ]),
                sec("", vec![Row::new(RowKind::Expander, "How it works", None).lines(&[
                    "Drop one Corral onto another to stack them as tabs.",
                    "Hold Shift and drag a tab to reorder it, or drag it away to make it a separate Corral again.",
                ], false)]),
            ],
        },
        Page::Pages => PageContent {
            title: "Ranges",
            subtitle: "Organize icons and Corrals across separate desktop pages and slide between them. Not related to Windows virtual desktops.",
            master: Some(Field::SwipeEnabled),
            sections: vec![
                sec("", vec![Row::new(RowKind::Expander, "How it works", None).lines(&[
                    "Pull the left or right edge of the desktop sideways to move to the next Range.",
                    "To add a Range or carry an item over, drag it to the screen edge and hold it there for a moment.",
                    "With the keyboard: click the desktop, then press Alt + Left or Alt + Right.",
                ], false)]),
                sec("Switch Ranges by", vec![
                    Row::new(RowKind::CheckRow, "Pulling a screen edge", None).b(Field::SwipeDragEdge).g(&[Gate::Enable(Field::SwipeEnabled)]),
                    Row::new(RowKind::CheckRow, "Swiping with two fingers", None).b(Field::SwipeTouch).g(&[Gate::Enable(Field::SwipeEnabled)]),
                    Row::new(RowKind::CheckRow, "Alt + arrow keys or Alt + mouse wheel", None).b(Field::SwipeKeyboardWheel).g(&[Gate::Enable(Field::SwipeEnabled)]),
                    Row::new(RowKind::CheckRow, "Dragging with the middle button", None).b(Field::SwipeMiddleDrag).g(&[Gate::Enable(Field::SwipeEnabled)]),
                    Row::new(RowKind::CheckRow, "Dragging with both buttons", None).b(Field::SwipeDualDrag).g(&[Gate::Enable(Field::SwipeEnabled)]),
                ]),
                sec("Animation", vec![
                    Row::new(RowKind::Segmented, "Animation", Some("How the desktop moves when you change Range.")).b(Field::SwipeAnimate).secondary(Field::SwipeBounce).choices(ANIMATION_CHOICES).g(&[Gate::Enable(Field::SwipeEnabled)]),
                    Row::new(RowKind::Toggle, "Show hidden icons when you change Range", Some("Brings back icons hidden by Clear view.")).b(Field::SwipeShowIcons).g(&[Gate::Enable(Field::SwipeEnabled)]),
                ]),
            ],
        },
        Page::Rules => PageContent {
            title: "Round-up",
            subtitle: "Round-up sends icons to Corrals using the criteria you choose.",
            master: None,
            // The Round-up page is drawn by `rules_page`; only the title metadata is read from here.
            sections: vec![],
        },
        // 4.11
        Page::Layout => PageContent {
            title: "Layout",
            subtitle: "How Corrals line up, and what happens to them when your monitors change.",
            master: None,
            sections: vec![
                sec("When monitors change", vec![
                    Row::new(RowKind::RadioRow, "Remember a layout for each monitor setup", Some("Works with one or more monitors. Recommended.")).v(Field::LayoutPreserve, 2),
                    Row::new(RowKind::RadioRow, "Scale Corrals to fit the display", Some("For a single monitor whose resolution changes.")).v(Field::LayoutPreserve, 1),
                    Row::new(RowKind::RadioRow, "Don't adjust", Some("Corrals keep their pixel positions and can end up off screen.")).v(Field::LayoutPreserve, 0),
                    Row::new(RowKind::ButtonRow, "Swap monitors", Some("Move the Corrals of your first two monitors to the other screen.")).button("Swap", false).g(&[Gate::MultiMonitor]),
                    Row::new(RowKind::ButtonRow, "This desk setup", None).button("Saved setups…", false).g(&[Gate::DeskSetups]),
                ]).when(&[Gate::LayoutAuto]),
                sec("Snapping", vec![
                    Row::new(RowKind::Toggle, "Snap Corrals while moving", Some("Corrals jump to neighbouring edges and to each other.")).b(Field::SnapEnabled),
                    Row::new(RowKind::Toggle, "Keep a gap between Corrals", Some("Snapped Corrals stay a little apart.")).b(Field::SnapSpaced).g(&[Gate::Enable(Field::SnapEnabled)]),
                    Row::new(RowKind::Stepper, "Gap", Some("In pixels at 100 % scaling.")).range(0,100,1).g(&[Gate::Enable(Field::SnapEnabled), Gate::Enable(Field::SnapSpaced)]),
                    Row::new(RowKind::Toggle, "Resize in whole icon cells", Some("Edges stop where a full row or column of icons fits.")).b(Field::SnapSizes).g(&[Gate::Enable(Field::SnapEnabled)]),
                    Row::new(RowKind::Toggle, "Align edges with nearby Corrals", Some("Guides appear when edges line up.")).b(Field::SnapLines).g(&[Gate::Enable(Field::SnapEnabled)]),
                ]),
                sec("", vec![Row::new(RowKind::InfoBox, "Hold Shift to skip snapping", Some("While you drag or resize a Corral, Shift turns snapping off for that move."))]),
                sec("Icon spacing", vec![Row::new(RowKind::Toggle, "Match the desktop icon spacing", Some("Corrals use the same spacing as the desktop. Windows' own spacing is never changed.")).b(Field::GroupSpacingMatchesDesktop)]),
            ],
        },
        Page::Backups => PageContent {
            title: "Backups",
            subtitle: "Save your Corral layout and bring it back later.",
            master: None,
            sections: vec![
                sec("", vec![
                    Row::new(RowKind::ButtonRow, "Save a backup now", Some("Includes every Corral, Gate and tab on all monitors.")).button(super::backups::SAVE_TEXT, true),
                    Row::new(RowKind::Toggle, "Save a backup every day", Some("Corral keeps the daily backups of the last seven days.")).b(Field::DailyBackups),
                ]),
                sec("Saved backups", vec![TILES_ROW]),
                sec("", vec![Row::new(RowKind::InfoBox, "Restoring is safe to try", Some("Before a restore, Corral offers to back up the current layout first. Files on the desktop are never moved or deleted."))]),
            ],
        },
        // Corral identity and settings-folder action (approved redesign section 7).
        Page::About => PageContent {
            // Replacement identity.
            title: "Corral",
            subtitle: concat!("Version ", env!("CARGO_PKG_VERSION")),
            master: None,
            sections: vec![
                sec("", vec![card_l("An independent desktop organizer for Windows: Corrals, Folder Gates, Tabs, Ranges and more.", "Free software under the GNU General Public License, version 3 or later.")]),
                sec("Files", vec![Row::new(RowKind::ButtonRow, "Settings and layouts", None).button("Open folder", false)]),
            ],
        },
    }
}

#[cfg(test)]
thread_local! {
    /// Test override of [`windows_build`] (this thread only).
    pub(super) static BUILD_OVERRIDE: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Windows build number from the registry (26100 when unreadable); cached.
pub(super) fn windows_build() -> u32 {
    #[cfg(test)]
    if let Some(build) = BUILD_OVERRIDE.with(|b| b.get()) {
        return build;
    }
    use std::sync::OnceLock;
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::w;
    static BUILD: OnceLock<u32> = OnceLock::new();
    *BUILD.get_or_init(|| {
        let mut buf = [0u16; 16];
        let mut size = std::mem::size_of_val(&buf) as u32;
        // SAFETY: buffer and size describe `buf`.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                w!(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"),
                w!("CurrentBuildNumber"),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if status.is_err() {
            return 26100;
        }
        let len = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..len])
            .trim()
            .parse()
            .unwrap_or(26100)
    })
}

/// Choices of a combo row in displayed order: `(text, stored value)`. `current` is the stored value:
/// the hot corner lists keep a stored "Show the full desktop" (2) on older Windows builds, as
/// `ReloadComboBoxes_HotCorner` does. A stored value that is not in the list shows the first item
/// (`UpdateComboBoxesSelection_*`: no match selects index 0), e.g. the default right click action 3
/// ("fast") on Windows 11 24H2 shows "Do nothing".
pub fn combo_choices(field: Field, current: i64) -> Vec<(String, i64)> {
    combo_choices_for_build(field, current, windows_build())
}

pub fn combo_choices_for_build(field: Field, current: i64, build: u32) -> Vec<(String, i64)> {
    let list = |items: &[(&str, i64)]| {
        items
            .iter()
            .map(|(t, v)| (t.to_string(), *v))
            .collect::<Vec<_>>()
    };
    match field {
        Field::AutoHideIdle => IDLE_CHOICES
            .iter()
            .map(|(t, v)| (t.to_string(), *v))
            .collect(),
        Field::BlendIconOpacity => list(&[
            ("Icon blend strength: 50%", 127),
            ("Icon blend strength: 60%", 153),
            ("Icon blend strength: 70%", 178),
            ("Icon blend strength: 80%", 204),
            ("Icon blend strength: 90%", 229),
            ("Icon blend strength: 100%", 255),
        ]),
        Field::TintColorMode => list(&[
            ("Match Corral", 0),
            ("Use custom color", 1),
        ]),
        Field::CreateCorralMode => list(&[
            ("Either button", 0),
            ("Left button", 1),
            ("Right button", 2),
        ]),
        Field::QuickHideWhat => list(&[
            ("Hide desktop icons and Corrals", 0),
            ("Hide only ungrouped icons", 1),
            ("Hide only Corrals", 2),
        ]),
        Field::HotCornerLeft | Field::HotCornerRight => {
            let none = if field == Field::HotCornerLeft { -1 } else { 0 };
            let late = build >= WIN11_24H2;
            let mut v = vec![
                ("Do nothing".to_string(), none),
                ("Show Corrals".to_string(), 1),
            ];
            if late || current == 2 {
                v.push(("Show the full desktop".to_string(), 2));
            }
            if !late {
                v.push(("Show the full desktop (fast)".to_string(), 3));
                v.push(("Show the full desktop (classic)".to_string(), 4));
            }
            v
        }
        // Glance shortcuts: the modifier-only choices are not offered (peek-hotkeys-spec); a
        // stored one stays listed so the dropdown shows what the file holds, with a note under the row.
        Field::HotkeyGlanceToggle | Field::HotkeyGlanceHold => HOTKEY_CHOICES
            .iter()
            .filter(|(_, v)| *v as i64 == current || crate::peek_keys::decode_stored(*v) != crate::peek_keys::Stored::Invalid)
            .map(|(t, v)| (t.replace("Control", "Ctrl"), *v as i64))
            .collect(),
        Field::HotkeyDesktopToggle | Field::HotkeyDesktopHold => HOTKEY_CHOICES
            .iter()
            .map(|(t, v)| (t.replace("Control", "Ctrl"), *v as i64))
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{LayoutFlags, Settings, set_layout_flags};

    #[test]
    fn fold_redesign_structure_and_master_gating() {
        let page = content(Page::Rollup);
        assert_eq!(page.master, Some(Field::RollupEnabled));
        assert_eq!(
            page.sections.iter().map(|s| s.header).collect::<Vec<_>>(),
            ["Folding", "Opening", ""]
        );
        let rows: Vec<_> = page.sections.iter().flat_map(|s| &s.rows).collect();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].bind, Some(Field::RollupDoubleClick));
        assert_eq!(rows[1].bind, Some(Field::RollupOnDock));
        assert_eq!(rows[2].kind, RowKind::Segmented);
        assert_eq!(rows[2].choices, FOLD_OPEN_CHOICES);
        assert_eq!(rows[2].bind, Some(Field::RollupRequireClick));
        assert_eq!(rows[2].secondary_bind, Some(Field::RollupRequireDelay));
        assert_eq!(rows[3].kind, RowKind::Expander);
        assert!(!rows[3].expanded);
        assert_eq!(rows[3].lines.len(), 3);
        let mut settings = Settings::default();
        for on in [false, true] {
            settings.set(Field::RollupEnabled, i64::from(on));
            for row in &rows {
                assert_eq!(super::super::gates_state(row.gates, &settings), (true, on));
            }
        }
        for (row, field) in [
            (rows[0], Field::RollupDoubleClick),
            (rows[1], Field::RollupOnDock),
        ] {
            for value in [0, 1] {
                assert!(row.apply_value(&mut settings, value));
                assert_eq!(settings.get(field), value);
                settings.set(field, 1 - value);
                assert_eq!(row.current_value(&settings), 1 - value);
            }
        }
    }

    #[test]
    fn fold_open_mode_maps_every_stored_pair_and_preserves_delay_on_click() {
        let row = content(Page::Rollup).sections[1].rows[0];
        for click in [0, 1] {
            for delay in [0, 1] {
                let mut settings = Settings::default();
                settings.set(Field::RollupRequireClick, click);
                settings.set(Field::RollupRequireDelay, delay);
                assert_eq!(
                    row.current_value(&settings),
                    if click == 1 { 2 } else { delay }
                );
                for mode in [0, 1, 2] {
                    settings.set(Field::RollupRequireClick, click);
                    settings.set(Field::RollupRequireDelay, delay);
                    assert!(row.apply_value(&mut settings, mode));
                    assert_eq!(
                        settings.get(Field::RollupRequireClick),
                        i64::from(mode == 2)
                    );
                    assert_eq!(
                        settings.get(Field::RollupRequireDelay),
                        if mode == 2 { delay } else { mode }
                    );
                    assert_eq!(row.current_value(&settings), mode);
                }
                let before = settings.clone();
                assert!(!row.apply_value(&mut settings, 3));
                assert_eq!(settings, before);
            }
        }
    }

    #[test]
    fn folder_gates_inline_help_and_inheritance_binding() {
        let page = content(Page::FolderPortals);
        assert_eq!(page.master, None);
        assert_eq!(page.sections.len(), 2);
        let help = page.sections[0].rows[0];
        assert_eq!(help.kind, RowKind::Expander);
        assert!(help.expanded);
        assert_eq!(help.lines.len(), 3);
        let toggle = page.sections[1].rows[0];
        assert_eq!(toggle.kind, RowKind::Toggle);
        assert_eq!(toggle.bind, Some(Field::InheritViewFromParent));
        let mut settings = Settings::default();
        for value in [0, 1] {
            assert!(toggle.apply_value(&mut settings, value));
            assert_eq!(toggle.current_value(&settings), value);
        }
        assert!(
            page.sections
                .iter()
                .flat_map(|s| &s.rows)
                .all(|r| r.kind != RowKind::Link)
        );
    }

    #[test]
    fn about_contains_identity_notice_and_only_folder_action() {
        let page = content(Page::About);
        assert_eq!(page.title, "Corral");
        assert_eq!(
            page.subtitle,
            concat!("Version ", env!("CARGO_PKG_VERSION"))
        );
        assert_eq!(page.master, None);
        let rows: Vec<_> = page.sections.iter().flat_map(|s| &s.rows).collect();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].desc,
            Some("Free software under the GNU General Public License, version 3 or later.")
        );
        assert_eq!(rows[1].kind, RowKind::ButtonRow);
        assert_eq!(rows[1].button_text, Some("Open folder"));
        assert!(
            rows[1]
                .description()
                .is_some_and(|path| !path.is_empty() && !path.contains('%'))
        );
        assert!(
            rows.iter()
                .all(|row| !matches!(row.kind, RowKind::Combo | RowKind::Dropdown | RowKind::Link))
        );
    }

    #[test]
    fn backups_are_a_separate_page_with_existing_bindings() {
        let layout = content(Page::Layout);
        assert!(layout.sections.iter().all(|s| s.header != "Layout backups"));
        assert!(
            layout
                .sections
                .iter()
                .flat_map(|s| &s.rows)
                .all(|r| r.bind != Some(Field::DailyBackups))
        );
        let page = content(Page::Backups);
        assert_eq!(page.master, None);
        assert_eq!(
            page.sections[0].rows[0].button_text,
            Some(super::super::backups::SAVE_TEXT)
        );
        assert!(page.sections[0].rows[0].primary);
        let daily = page.sections[0].rows[1];
        assert_eq!(daily.bind, Some(Field::DailyBackups));
        let mut settings = Settings::default();
        for value in [0, 1] {
            assert!(daily.apply_value(&mut settings, value));
            assert_eq!(daily.current_value(&settings), value);
        }
        assert_eq!(page.sections[1].header, "Saved backups");
        assert_eq!(page.sections[1].rows[0].kind, RowKind::Tiles);
        assert_eq!(page.sections[2].rows[0].kind, RowKind::InfoBox);
    }

    #[test]
    fn legacy_row_mapping_keeps_settings_gates_and_destinations() {
        for (old, new) in [
            (RowKind::Check, RowKind::Toggle),
            (RowKind::Radio, RowKind::RadioRow),
            (RowKind::Combo, RowKind::Dropdown),
            (RowKind::Number, RowKind::Stepper),
        ] {
            let row = Row::new(old, "Legacy", None)
                .b(Field::DailyBackups)
                .g(&[Gate::Enable(Field::SnapEnabled)]);
            assert_eq!(row.control_kind(), new);
            assert_eq!(row.bind, Some(Field::DailyBackups));
            assert_eq!(row.gates, &[Gate::Enable(Field::SnapEnabled)]);
        }
        assert_eq!(
            lnk("Navigate").to(Page::About).control_kind(),
            RowKind::Navigate
        );
        assert_eq!(lnk("Help").control_kind(), RowKind::Link);
        for kind in [
            RowKind::Label,
            RowKind::Note,
            RowKind::Tiles,
        ] {
            assert_eq!(Row::new(kind, "Legacy", None).control_kind(), kind);
        }
    }

    fn with_build<T>(build: u32, f: impl FnOnce() -> T) -> T {
        BUILD_OVERRIDE.with(|b| b.set(Some(build)));
        let out = f();
        BUILD_OVERRIDE.with(|b| b.set(None));
        out
    }

    /// Texts of the rows (and section headers) a page shows for `settings`, with their enabled flag.
    fn shown(page: Page, settings: &Settings) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        for section in content(page).sections {
            if !super::super::gates_state(section.gates, settings).0 {
                continue;
            }
            if !section.header.is_empty() {
                out.push((format!("## {}", section.header), true));
            }
            for row in section.rows {
                let (visible, enabled) = super::super::gates_state(row.gates, settings);
                if visible {
                    out.push((row.text.to_string(), enabled));
                }
            }
        }
        out
    }

    fn texts(page: Page, settings: &Settings) -> Vec<String> {
        shown(page, settings).into_iter().map(|(t, _)| t).collect()
    }

    fn enabled(page: Page, settings: &Settings, text: &str) -> bool {
        shown(page, settings)
            .into_iter()
            .find(|(t, _)| t == text)
            .unwrap_or_else(|| panic!("{text:?} not shown"))
            .1
    }

    #[test]
    fn peek_page_order_labels_and_build_gates() {
        let s = Settings::default();
        let c = content(Page::Peek);
        assert_eq!(
            c.sections.iter().map(|s| s.header).collect::<Vec<_>>(),
            ["", "Shortcuts", "Tray icon", "Screen corner"] // the first section is the hook-failure warning
        );
        for build in [19045, 21999, 22000, 22631, 26099, 26100, 26200] {
            with_build(build, || {
                let t = texts(Page::Peek, &s);
                assert_eq!(t.iter().any(|t| t == "## Screen corner"), build >= 22000);
                for label in [
                    "Speed up Show desktop (Win + D)",
                    "Show the desktop until you press again",
                    "Show the desktop while held",
                ] {
                    assert_eq!(
                        t.contains(&label.to_string()),
                        build < 26100,
                        "{build} {label}"
                    );
                }
                assert!(!t.contains(&"Customize icon".to_string()));
            });
        }
    }

    #[test]
    fn peek_gating_follows_the_check_boxes() {
        let mut s = Settings::default();
        with_build(22631, || {
            for on in [false, true] {
                s.set(Field::HotkeysGlance, on as i64);
                for label in [
                    "Show Corrals until you press again",
                    "Show Corrals while held",
                    "Keep Glance open when you click other windows",
                ] {
                    assert_eq!(enabled(Page::Peek, &s, label), on);
                }
                s.set(Field::HotkeysDesktop, on as i64);
                for label in [
                    "Show the desktop until you press again",
                    "Show the desktop while held",
                ] {
                    assert_eq!(enabled(Page::Peek, &s, label), on);
                }
                s.set(Field::HotCornerEnabled, on as i64);
                for label in ["Left click", "Right click"] {
                    assert_eq!(texts(Page::Peek, &s).contains(&label.to_string()), on);
                }
            }
            s.set(Field::ShowInTaskbar, 1);
            assert!(!texts(Page::Peek, &s).contains(&"Customize icon".to_string()));
        });
    }

    #[test]
    fn glance_shortcut_notes_and_the_choice_lists() {
        const WIN_SPACE_NOTE: &str = "Windows also uses Win + Space to switch the input language.";
        const TOGGLE_FALLBACK: &str = "The saved toggle shortcut is no longer available.";
        const HOLD_FALLBACK: &str = "The saved hold shortcut is no longer available.";
        let mut s = Settings::default();
        with_build(22631, || {
            // the default toggle is Win + Space: the language-switch note shows, the warning does not
            let t = texts(Page::Peek, &s);
            assert!(t.contains(&WIN_SPACE_NOTE.to_string()));
            assert!(!t.contains(&"Shortcuts are not available".to_string()));
            assert!(!t.contains(&TOGGLE_FALLBACK.to_string()) && !t.contains(&HOLD_FALLBACK.to_string()));
            // another toggle: no note
            s.set(Field::HotkeyGlanceToggle, 196640); // Win + Shift + Space
            assert!(!texts(Page::Peek, &s).contains(&WIN_SPACE_NOTE.to_string()));
            // shortcuts off: no note
            s.set(Field::HotkeyGlanceToggle, 65568);
            s.set(Field::HotkeysGlance, 0);
            assert!(!texts(Page::Peek, &s).contains(&WIN_SPACE_NOTE.to_string()));
            s.set(Field::HotkeysGlance, 1);
            // a stored modifier-only choice runs the default and says so
            s.set(Field::HotkeyGlanceToggle, 851968);
            s.set(Field::HotkeyGlanceHold, 458752);
            let t = texts(Page::Peek, &s);
            assert!(t.contains(&TOGGLE_FALLBACK.to_string()) && t.contains(&HOLD_FALLBACK.to_string()));
            assert!(t.contains(&WIN_SPACE_NOTE.to_string()), "the default toggle is Win + Space");
        });
        // the dropdown no longer offers the modifier-only choices, but keeps a stored one visible
        let labels = |cur: i64| combo_choices(Field::HotkeyGlanceToggle, cur).into_iter().map(|c| c.0).collect::<Vec<_>>();
        let normal = labels(65568);
        assert_eq!(normal.len(), 10);
        assert!(!normal.contains(&"Win + Ctrl + Alt".to_string()) && !normal.contains(&"Win + Ctrl + Shift".to_string()));
        assert_eq!(normal[0], "Disabled");
        let stored = labels(851968);
        assert_eq!(stored.len(), 11);
        assert!(stored.contains(&"Win + Ctrl + Alt".to_string()));
        // the Desktop-mode lists are untouched
        assert_eq!(combo_choices(Field::HotkeyDesktopToggle, 65604).len(), 12);
    }

    #[test]
    fn hot_corner_lists_follow_the_windows_build() {
        let names = |f: Field, cur: i64, build: u32| combo_choices_for_build(f, cur, build);
        let old_l = names(Field::HotCornerLeft, 1, 22631);
        assert_eq!(
            old_l,
            vec![
                ("Do nothing".to_string(), -1),
                ("Show Corrals".to_string(), 1),
                ("Show the full desktop (fast)".to_string(), 3),
                ("Show the full desktop (classic)".to_string(), 4),
            ]
        );
        let old_r = names(Field::HotCornerRight, 3, 22631);
        assert_eq!(
            old_r[0],
            ("Do nothing".to_string(), 0),
            "right click: Do nothing is 0, left click: -1"
        );
        assert_eq!(old_r.iter().map(|c| c.1).collect::<Vec<_>>(), [0, 1, 3, 4]);
        // A stored 2 stays selectable on older builds.
        assert_eq!(
            names(Field::HotCornerRight, 2, 22631)
                .iter()
                .map(|c| c.1)
                .collect::<Vec<_>>(),
            [0, 1, 2, 3, 4]
        );
        // 24H2: only "Show the full desktop"; a stored 3 or 4 is not in the list (the control shows index 0).
        for cur in [1, 2, 3, 4] {
            assert_eq!(
                names(Field::HotCornerRight, cur, 26100)
                    .iter()
                    .map(|c| c.1)
                    .collect::<Vec<_>>(),
                [0, 1, 2]
            );
        }
        assert_eq!(
            names(Field::HotCornerLeft, 3, 26200)
                .iter()
                .map(|c| c.1)
                .collect::<Vec<_>>(),
            [-1, 1, 2]
        );
        assert_eq!(
            names(Field::HotCornerLeft, 1, 26200)[2].0,
            "Show the full desktop"
        );
    }

    #[test]
    fn hotkey_choice_lists() {
        let c = combo_choices_for_build(Field::HotkeyGlanceToggle, 0, 22631);
        let expect = [
            ("Disabled", 0),
            ("Win + Space", 65568),
            ("Win + Shift + Space", 196640),
            ("Win + Ctrl + Space", 327712),
            ("Win + D", 65604),
            ("Win + Shift + D", 196676),
            ("Win + Ctrl + D", 327748),
            ("Win + Ctrl + Shift + D", 458820),
            ("Win + Tilde", 65632),
            ("Win + Ctrl + Alt", 851968),
            ("Win + Ctrl + Shift", 458752),
            ("Ctrl + Shift + Space", 393248),
        ];
        // Glance shortcuts do not offer the two modifier-only choices any more (peek-hotkeys-spec, lead
        // amendment); the Desktop-mode lists keep all twelve.
        let glance: Vec<_> = expect.iter().filter(|(t, _)| !matches!(*t, "Win + Ctrl + Alt" | "Win + Ctrl + Shift")).collect();
        assert_eq!(c.len(), 10);
        for ((t, v), (et, ev)) in c.iter().zip(glance) {
            assert_eq!((t.as_str(), *v), (*et, *ev));
        }
        assert_eq!(combo_choices_for_build(Field::HotkeyGlanceHold, 0, 22631), c);
        let desktop = combo_choices_for_build(Field::HotkeyDesktopToggle, 0, 22631);
        assert_eq!(desktop.len(), 12);
        for ((t, v), (et, ev)) in desktop.iter().zip(expect) {
            assert_eq!((t.as_str(), *v), (et, ev));
        }
        assert_eq!(combo_choices_for_build(Field::HotkeyDesktopHold, 0, 22631), desktop);
    }

    #[test]
    fn clear_view_redesign_keeps_hide_enabled_and_existing_gates() {
        let mut s = Settings::default();
        let c = content(Page::Hide);
        assert_eq!(c.master, Some(Field::QuickHideEnabled));
        assert!(c.sections[0].rows[0].expanded);
        let hide = c.sections[1].rows[0];
        assert_eq!(hide.kind, RowKind::Segmented);
        assert_eq!(hide.choices, HIDE_CHOICES);
        for on in [0, 1] {
            s.set(Field::QuickHideEnabled, on);
            assert!(enabled(Page::Hide, &s, "Hide"));
            for label in [
                "Hide when the desktop is idle",
                "Show icons when you click the desktop",
                "Start with icons visible",
            ] {
                assert_eq!(enabled(Page::Hide, &s, label), on == 1);
            }
        }
        apply_idle_mode(&mut s, 30000);
        assert!(!enabled(Page::Hide, &s, "Start with icons visible"));
        assert!(
            texts(Page::Hide, &s)
                .contains(&"Show icons while you drop files on the desktop".to_string())
        );
        s.set(Field::AutoShow, 0);
        assert!(
            !texts(Page::Hide, &s)
                .contains(&"Show icons while you drop files on the desktop".to_string())
        );
    }

    #[test]
    fn ranges_redesign_structure_and_enable_gates() {
        let mut s = Settings::default();
        let c = content(Page::Pages);
        assert_eq!(c.master, Some(Field::SwipeEnabled));
        assert!(
            c.subtitle
                .contains("Not related to Windows virtual desktops.")
        );
        assert!(!c.sections[0].rows[0].expanded);
        assert_eq!(c.sections[1].rows.len(), 5);
        assert!(
            c.sections[1]
                .rows
                .iter()
                .all(|r| r.kind == RowKind::CheckRow)
        );
        for on in [0, 1] {
            s.set(Field::SwipeEnabled, on);
            for row in c.sections[1].rows.iter().chain(&c.sections[2].rows) {
                assert_eq!(enabled(Page::Pages, &s, row.text), on == 1);
            }
        }
        s.swipe_animation_style = 0;
        assert!(
            enabled(Page::Pages, &s, "Animation"),
            "Bounce remains selectable directly when animation is off"
        );
    }

    #[test]
    fn tabs_redesign_picker_and_behavior_show_gates() {
        set_layout_flags(LayoutFlags::default());
        let mut s = Settings::default();
        let c = content(Page::Tabs);
        assert_eq!(
            c.sections[0]
                .rows
                .iter()
                .map(|r| (r.kind, r.text, r.value))
                .collect::<Vec<_>>(),
            vec![
                (RowKind::TabStyleTile, "Menu only", 0),
                (RowKind::TabStyleTile, "Flat", 1),
                (RowKind::TabStyleTile, "Segmented", 2),
                (RowKind::TabStyleTile, "Rounded", 3)
            ]
        );
        assert!(!c.sections[2].rows[0].expanded);
        assert!(!texts(Page::Tabs, &s).contains(&"Wait briefly before switching".to_string()));
        s.set(Field::TabsMouseover, 1);
        assert!(texts(Page::Tabs, &s).contains(&"Wait briefly before switching".to_string()));
        assert!(!texts(Page::Tabs, &s).contains(&"Color each tab like its Corral".to_string()));
        set_layout_flags(LayoutFlags {
            any_group_styled: true,
            any_group_blend: false,
            any_group_faded: false,
        });
        for style in 0..=3 {
            s.set(Field::TabsStyle, style);
            assert_eq!(
                texts(Page::Tabs, &s).contains(&"Color each tab like its Corral".to_string()),
                style > 0
            );
        }
        s.set(Field::TabsCreate, 0);
        assert!(enabled(Page::Tabs, &s, "Ask before combining Corrals"));
        assert!(enabled(Page::Tabs, &s, "Switch tabs on hover"));
        set_layout_flags(LayoutFlags::default());
    }

    #[test]
    fn camouflage_page_keeps_applicability_gate_with_a_master_and_named_blur() {
        set_layout_flags(LayoutFlags::default());
        let mut s = Settings::default();
        assert_eq!(texts(Page::Camouflage, &s), ["How it works"]);
        s.set(Field::BlendIcons, 1);
        let visible = [
            "## Blending",
            "Blend strength",
            "Wallpaper blur",
            "Blend Corral backgrounds",
            "Use the icon tint color",
            "## Show real colors",
            "When you point at a Corral",
            "While the desktop is active",
            "How it works",
        ];
        assert_eq!(texts(Page::Camouflage, &s), visible);
        s.set(Field::BlendIcons, 0);
        set_layout_flags(LayoutFlags {
            any_group_styled: false,
            any_group_blend: true,
            any_group_faded: false,
        });
        assert_eq!(texts(Page::Camouflage, &s), visible);
        s.desktop_blend_uses_tint_icons_color = true;
        assert!(!enabled(Page::Camouflage,&s,"Wallpaper blur"));
        assert!(texts(Page::Camouflage,&s).contains(&"Wallpaper blur".to_owned()));
        s.desktop_blend_uses_tint_icons_color = false;
        assert!(enabled(Page::Camouflage,&s,"Wallpaper blur"));
        let page = content(Page::Camouflage);
        assert_eq!(page.master, Some(Field::BlendIcons));
        assert_eq!(
            page.subtitle,
            "Icons take on the colors of the wallpaper behind them, for a quieter desktop."
        );
        set_layout_flags(LayoutFlags::default());
    }
}
