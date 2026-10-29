//! Persisted application settings (the model behind the settings window).
//!
//! Stored as JSON (`%APPDATA%\Corral\settings.json`, atomic write with `.bak`, same scheme as
//! `model.rs`). Stored keys are PascalCase names (`SnapEnabled`, `Swipe_Enabled`, ...).
//! Tabs also carry engine-only values without inventing controls; unknown JSON keys are
//! ignored and missing keys take the field's default.
//!
//! [`Field`] is the binding handle used by the UI. Most fields map 1:1 onto a stored value; the
//! "virtual" ones (marked below) are derived or inverted
//! (`Disabled` inverted, `IconAnimationDisabled` inverted, idle-hide milliseconds buckets, ...).
//! Values are exchanged as `i64` (bool = 0/1).

/// Clear view's offered idle choices. Zero disables idle hiding; positive values are durations.
pub const AUTO_HIDE_IDLE_CHOICES: &[(&str, i64)] = &[
    ("Never", 0),
    ("Immediately", 1),
    ("After 1 second", 1000),
    ("After 30 seconds", 30000),
    ("After 2 minutes", 120000),
    ("After 10 minutes", 600000),
];

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub mod round_up_store;
pub mod style_request;
pub use round_up_store::{backup_copy as backup_round_up_copy, prepare_migration as prepare_round_up_migration};

/// Hotkey choices in displayed order: label and stored value.
pub const HOTKEY_CHOICES: [(&str, u32); 12] = [
    ("Disabled", 0),
    ("Win + Space", 65568),
    ("Win + Shift + Space", 196640),
    ("Win + Control + Space", 327712),
    ("Win + D", 65604),
    ("Win + Shift + D", 196676),
    ("Win + Control + D", 327748),
    ("Win + Control + Shift + D", 458820),
    ("Win + Tilde", 65632),
    ("Win + Control + Alt", 851968),
    ("Win + Control + Shift", 458752),
    ("Control + Shift + Space", 393248),
];

/// Quick-hide idle delays in milliseconds, displayed order.
pub const IDLE_CHOICES_MS: [u32; 5] = [1, 1000, 30000, 120000, 600000];

/// `SnapPaddingDefault`: the value the UI check box "keep groups spaced" writes when
/// switched on. The default when `SnapPadding` is absent is 0.
pub const SNAP_PADDING_DEFAULT: u32 = 6;

/// Raw JSON text of settings blocks this build must not rewrite (round-up-spec 6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoundUpKeep {
    /// A `RoundUp` block this build cannot use (newer version or unreadable), kept verbatim.
    pub block: Option<String>,
    /// The old `Rules` value, kept verbatim while review items exist or the block is kept.
    pub legacy_rules: Option<String>,
}

// camouflage-spec 5.2: deserialize the shared field schema before applying migration.
// The wire helper avoids maintaining a second copy of unrelated settings fields.
macro_rules! settings_storage_schema {
    ($( $(#[$attr:meta])* pub $field:ident: $ty:ty, )*) => {
        #[derive(Clone, Debug, PartialEq, Serialize)]
        pub struct Settings { $( $(#[$attr])* pub $field: $ty, )* }
        #[derive(Deserialize)]
        #[serde(default)]
        struct SettingsStorage { $( $(#[$attr])* pub $field: $ty, )* }
        impl Default for SettingsStorage {
            fn default() -> Self {
                let value = Settings::default();
                Self { $( $field: value.$field, )* }
            }
        }
        impl From<SettingsStorage> for Settings {
            fn from(value: SettingsStorage) -> Self {
                Self { $( $field: value.$field, )* }
            }
        }
    };
}

/// A pending request or answer that this build cannot read (an action another build wrote, such
/// as the retired `reset_text`) is dropped; it must never make the settings file unreadable.
fn lenient_style_request<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<style_request::Request>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(d)?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()))
}
fn lenient_style_result<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<style_request::RequestResult>, D::Error> {
    let value = Option::<serde_json::Value>::deserialize(d)?;
    Ok(value.and_then(|v| serde_json::from_value(v).ok()))
}

settings_storage_schema! {
    // Welcome. `Disabled` is a root value; the checkbox "Enable Corral" shows it inverted.
    #[serde(rename = "Disabled")]
    pub disabled: bool,
    #[serde(rename = "CreateCorralWithMouseMode")]
    pub create_corral_with_mouse_mode: u32,
    /// Hidden setting; deliberately has no UI binding.
    #[serde(rename = "DisableCreateFolderPortalHere")]
    pub disable_create_folder_portal_here: bool,
    /// Rules in the old stored form (`NN` / `NN_Target` strings). Read only for the one-time
    /// Round-up migration; written back untouched while `RoundUp.needs_review` is not empty and
    /// dropped afterwards (round-up-spec 6).
    #[serde(rename = "Rules", skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<crate::round_up::StoredRule>,
    /// "By default... all new icons go to" Corral id of the old model; -1 = the desktop. Read only
    /// for the migration (the typed default lives in `round_up`), kept as it was (round-up-spec 6).
    #[serde(rename = "DefaultCorral")]
    pub default_corral: i32,
    /// Old "place at end" choice. No longer used; kept so the file stays as it was (round-up-spec 6).
    #[serde(rename = "NewItemGoToEnd")]
    pub new_item_go_to_end: bool,
    /// Corral's typed Round-up block: rules, default destination, items to review (round-up-spec 6).
    #[serde(rename = "RoundUp")]
    pub round_up: crate::round_up::RoundUp,
    /// Text that must survive a save byte for byte (round-up-spec 6); never serialized itself.
    #[serde(skip)]
    pub round_up_keep: RoundUpKeep,
    /// "Keep Round-up active" (`AlwaysApplyRules`, round-up-spec 3).
    #[serde(rename = "AlwaysApplyRules")]
    pub always_apply_rules: bool,
    /// "Sort the desktop now" request: set by the settings window, consumed and cleared by the
    /// running app, which then shows the preview (round-up-spec 4).
    #[serde(rename = "ApplyRulesNowRequested")]
    pub apply_rules_now_requested: bool,
    /// Backup action requested by the settings window (Corral protocol): only the running app
    /// writes the snapshot store; it executes, clears this and
    /// reports in `snapshot_result`. Never part of a captured snapshot (`snapshot::for_capture`).
    #[serde(rename = "SnapshotRequest", skip_serializing_if = "Option::is_none")]
    pub snapshot_request: Option<crate::snapshot::Request>,
    /// The app's answer to the last `snapshot_request` (same nonce).
    #[serde(rename = "SnapshotResult", skip_serializing_if = "Option::is_none")]
    pub snapshot_result: Option<crate::snapshot::RequestResult>,
    /// desk-setups-spec 6: only the running engine changes saved setup metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desk_setup_request: Option<crate::desk_setup::Request>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desk_setup_result: Option<crate::desk_setup::RequestResult>,
    /// Corral settings-to-engine layout styling protocol. Never captured in a backup.
    #[serde(rename = "StyleRequest", default, deserialize_with = "lenient_style_request", skip_serializing_if = "Option::is_none")]
    pub style_request: Option<style_request::Request>,
    #[serde(rename = "StyleResult", default, deserialize_with = "lenient_style_result", skip_serializing_if = "Option::is_none")]
    pub style_result: Option<style_request::RequestResult>,
    // camouflage-spec 5.1 and 3.7.
    #[serde(rename = "DesktopBlendIcons")]
    pub desktop_blend_icons: bool,
    #[serde(rename = "DesktopBlendCorralBackgrounds")]
    pub desktop_blend_corral_backgrounds: bool,
    #[serde(rename = "DesktopBlendColorOnMouseover")]
    pub desktop_blend_color_on_mouseover: bool,
    #[serde(rename = "DesktopBlendColorOnActive")]
    pub desktop_blend_color_on_active: bool,
    #[serde(rename = "DesktopBlendUsesTintIconsColor")]
    pub desktop_blend_uses_tint_icons_color: bool,
    #[serde(rename = "DesktopBlendIconOpacity")]
    pub desktop_blend_icon_opacity: u8,
    // Personalization (4.3)
    #[serde(rename = "Color")]
    pub color: u32,
    #[serde(rename = "TextColor")]
    pub text_color: u32,
    /// camouflage-spec 3.5: Auto chooses contrast against the final backdrop.
    #[serde(rename = "TextColorAuto")]
    pub text_color_auto: bool,
    #[serde(rename = "TransparencyFactor")]
    pub transparency_factor: u32,
    #[serde(rename = "DesktopTintIcons")]
    pub desktop_tint_icons: bool,
    #[serde(rename = "DesktopTintIconsColor")]
    pub desktop_tint_icons_color: u32,
    #[serde(rename = "DesktopTintIconsColorSource")]
    pub desktop_tint_icons_color_source: String,
    // camouflage-spec 5.2: read only, to migrate an old packed tint choice to the explicit source.
    #[serde(rename = "DesktopTintIconsColorUse", skip_serializing)]
    pub desktop_tint_icons_color_use: u32,
    #[serde(rename = "FadeInOpacityOnMouseover")]
    pub fade_in_opacity_on_mouseover: bool,
    #[serde(rename = "ShowLayoutAutoAdjustmentOptions")]
    pub show_layout_auto_adjustment_options: bool,
    #[serde(rename = "TabsStyle")]
    pub tabs_style: u32,
    #[serde(rename = "LabelMode")]
    pub label_mode: u32,
    #[serde(rename = "DarkenTitlebars")]
    pub darken_titlebars: u32,
    #[serde(rename = "SquareEdges")]
    pub square_edges: bool,
    #[serde(rename = "BackgroundBlurEnabled")]
    pub background_blur_enabled: bool,
    /// `Outline` (default off): a 1 px border around every Corral area.
    #[serde(rename = "Outline")]
    pub outline: bool,
    /// `BackgroundBlurRadius` (default 60, no UI): the radius for the blurred
    /// wallpaper copy.
    #[serde(rename = "BackgroundBlurRadius")]
    pub background_blur_radius: f32,
    /// Stored inverted in the UI: "Animate icon position changes" = (value == 0). Default 1.
    #[serde(rename = "IconAnimationDisabled")]
    pub icon_animation_disabled: u32,
    /// "Hide scrollbars until mouseover" = (value == 1). Default 1.
    #[serde(rename = "ScrollbarMode")]
    pub scrollbar_mode: u32,
    /// Shortcut-arrow menu bitmask; only Corral-painted icons consume it.
    #[serde(rename = "HideShortcutOverlays")]
    pub hide_shortcut_overlays: u32,
    /// Thin scrollbar variant (default 0, no UI).
    #[serde(rename = "ExtraThinScrollbars")]
    pub extra_thin_scrollbars: bool,
    // Folder Portals (4.4)
    #[serde(rename = "FolderPortalsInheritViewStateFromParent")]
    pub folder_portals_inherit_view: bool,
    // Peek (4.5)
    #[serde(rename = "ShowInTaskbar")]
    pub show_in_taskbar: bool,
    #[serde(rename = "TaskbarHotCornerEnabled")]
    pub taskbar_hot_corner_enabled: bool,
    #[serde(rename = "TaskbarHotCornerActionL")]
    pub taskbar_hot_corner_action_l: i32,
    #[serde(rename = "TaskbarHotCornerActionR")]
    pub taskbar_hot_corner_action_r: i32,
    #[serde(rename = "HotkeysGlanceEnabled")]
    pub hotkeys_glance_enabled: bool,
    /// Corral extra (user request 2026-10-14): a toggle Peek stays open when another
    /// application window is activated. Off = toggle Peek ends on activation.
    #[serde(rename = "CorralPeekKeepOpen")]
    pub peek_keep_open: bool,
    #[serde(rename = "HotkeysComboGlanceToggle")]
    pub hotkeys_combo_glance_toggle: u32,
    #[serde(rename = "HotkeysComboGlanceHold")]
    pub hotkeys_combo_glance_hold: u32,
    #[serde(rename = "HotkeysDesktopEnabled")]
    pub hotkeys_desktop_enabled: bool,
    #[serde(rename = "HotkeysComboDesktopToggle")]
    pub hotkeys_combo_desktop_toggle: u32,
    #[serde(rename = "HotkeysComboDesktopHold")]
    pub hotkeys_combo_desktop_hold: u32,
    // Hide / quick-hide (4.6)
    #[serde(rename = "QuickHideEnabled")]
    pub quick_hide_enabled: bool,
    #[serde(rename = "QuickHideExcludeCorralGroups")]
    pub quick_hide_exclude_corral_groups: bool,
    #[serde(rename = "QuickHideExcludeLooseIcons")]
    pub quick_hide_exclude_loose_icons: bool,
    /// Milliseconds; 0 = off.
    #[serde(rename = "QuickHideAutoHideOnIdle")]
    pub quick_hide_auto_hide_on_idle: u32,
    /// 0 off, 1 show on activity, 2 also on drag.
    #[serde(rename = "QuickHideAutoShowWhenActive")]
    pub quick_hide_auto_show_when_active: u32,
    #[serde(rename = "QuickHideAutoShowOnBoot")]
    pub quick_hide_auto_show_on_boot: bool,
    /// Default 0.
    #[serde(rename = "QuickHideEnabledTemp")]
    pub quick_hide_enabled_temp: bool,
    /// Engine-owned hidden state, written at every toggle and restored at start.
    #[serde(rename = "QuickHideActive")]
    pub quick_hide_active: bool,
    #[serde(rename = "NoQuickhideAnimation")]
    pub no_quick_hide_animation: bool,
    /// `Settings\Exclusions` records `key|display?` (`GROUP::<id>` for groups); `None` = value
    /// absent, so the default list (Recycle Bin, This PC, profile folder) applies.
    #[serde(rename = "Exclusions")]
    pub quick_hide_exclusions: Option<String>,
    // Rollup (4.7)
    #[serde(rename = "RollupEnabled")]
    pub rollup_enabled: bool,
    #[serde(rename = "RollupDoubleClickToToggle")]
    pub rollup_double_click_to_toggle: bool,
    /// Property `RollupOnDockToEdge`, registry value `RollupOnDock`.
    #[serde(rename = "RollupOnDock")]
    pub rollup_on_dock_to_edge: bool,
    #[serde(rename = "RollupRequireClick")]
    pub rollup_require_click: bool,
    #[serde(rename = "RollupRequireDelay")]
    pub rollup_require_delay: bool,
    /// Milliseconds (500).
    #[serde(rename = "RollupRequireDelayTime")]
    pub rollup_require_delay_time: u32,
    // Tabs (4.8)
    #[serde(rename = "TabsEnabled")]
    pub tabs_enabled: bool,
    #[serde(rename = "TabsRequireConfirmationUsesMenu")]
    pub tabs_require_confirmation_uses_menu: bool,
    #[serde(rename = "TabsSelectMouseoveredTabWhenDocked")]
    pub tabs_select_mouseovered_tab_when_docked: u32,
    #[serde(rename = "TabsAlwaysShowColors")]
    pub tabs_always_show_colors: bool,
    #[serde(rename = "TabsColoredPerTabInSegmentView")]
    pub tabs_colored_per_tab_in_segment_view: bool,
    /// Retired (corral-painting-spec 7): read so old files load, never written or acted on.
    #[serde(rename = "TabsDimNonSelectedTabOnMouseover", skip_serializing)]
    #[allow(dead_code)]
    pub tabs_dim_nonselected_tab_on_mouseover: i32,
    #[serde(rename = "UseTitlebarControls")]
    pub use_titlebar_controls: bool,
    /// Retired (corral-painting-spec 6, 7): read so old files load, never written or acted on.
    #[serde(rename = "UseTitlebarControlsOnAllTabs", skip_serializing)]
    #[allow(dead_code)]
    pub use_titlebar_controls_on_all_tabs: bool,

    #[serde(rename = "TabsAllowCreateWithDrag")]
    pub tabs_allow_create_with_drag: bool,
    #[serde(rename = "TabsRequireConfirmation")]
    pub tabs_require_confirmation: bool,
    #[serde(rename = "TabsSelectOnMouseover")]
    pub tabs_select_on_mouseover: u32,
    #[serde(rename = "TabsSelectOnMouseoverDelay")]
    pub tabs_select_on_mouseover_delay: u32,
    // Pages (4.9)
    #[serde(rename = "Swipe_Enabled")]
    pub swipe_enabled: bool,
    #[serde(rename = "Swipe_SwitchViaDragEdge")]
    pub swipe_switch_via_drag_edge: bool,
    #[serde(rename = "Swipe_SwitchViaTouch")]
    pub swipe_switch_via_touch: bool,
    #[serde(rename = "Swipe_SwitchViaKeyboard")]
    pub swipe_switch_via_keyboard: bool,
    /// Compatibility only: the engine gates wheel and keyboard on `Swipe_SwitchViaKeyboard`.
    #[serde(rename = "Swipe_SwitchViaMouseWheel")]
    pub swipe_switch_via_mouse_wheel: bool,
    #[serde(rename = "Swipe_SwitchViaMiddleDrag")]
    pub swipe_switch_via_middle_drag: bool,
    #[serde(rename = "Swipe_SwitchViaDualDrag")]
    pub swipe_switch_via_dual_drag: bool,
    /// 0 off, 1 animate, 2 animate + bounce.
    #[serde(rename = "Swipe_AnimationStyle")]
    pub swipe_animation_style: u32,
    #[serde(rename = "Swipe_ShowIconsOnInitate")]
    pub swipe_show_icons_on_initiate: bool,
    // Layout (4.11)
    #[serde(rename = "AutoTakeSnapshots")]
    pub auto_take_snapshots: bool,
    #[serde(rename = "StoreLayoutsPerMultimon")]
    pub store_layouts_per_multimon: bool,
    #[serde(rename = "AdjustGroupsForResolution")]
    pub adjust_groups_for_resolution: bool,
    #[serde(rename = "StoreGroupDataPerResolution")]
    pub store_group_data_per_resolution: bool,
    #[serde(rename = "SnapEnabled")]
    pub snap_enabled: bool,
    #[serde(rename = "SnapPadding")]
    pub snap_padding: u32,
    #[serde(rename = "SnapToDiscreteSizes")]
    pub snap_to_discrete_sizes: bool,
    #[serde(rename = "SnapLineUpCorrals")]
    pub snap_line_up_corrals: bool,
    /// `SnapShouldScale`: scale `SnapPadding` by the group DPI.
    #[serde(rename = "SnapShouldScale")]
    pub snap_should_scale: bool,
    #[serde(rename = "GroupSpacingMatchesDesktop")]
    pub group_spacing_matches_desktop: bool,
    /// Moving an icon inside a sorted Corral turns its sorting off without asking (set by "Always use
    /// this answer" + Yes in the "Turn off sorting?" prompt; settings-schema `AllowSortingBreakup`).
    #[serde(rename = "AllowSortingBreakup")]
    pub allow_sorting_breakup: bool,
    /// Automatically place titles below Corrals near the top of the screen.
    /// Default on.
    #[serde(rename = "TitleEdgeAutoAdjust")]
    pub title_edge_auto_adjust: bool,
    /// The global opacity override in View > Opacity: every Corral is
    /// drawn fully visible while set; picking an opacity value clears it.
    #[serde(rename = "ShowHiddenCorrals")]
    pub show_hidden_corrals: bool,
    /// The `Tips` sub-key: help-tip flags/counters by name (e.g. `FolderViewReorderMsg`);
    /// non-zero = seen.
    #[serde(rename = "Tips")]
    pub tips: std::collections::BTreeMap<String, u32>,
    /// Shell extensions refused inside Corral's process (`shell_guard`, no UI): an entry with a
    /// backslash is a folder fragment of the handler's DLL path, any other entry a prefix of the
    /// DLL file name or of a packaged handler's package family name. Empty by default.
    #[serde(rename = "BlockedShellExtensions")]
    pub blocked_shell_extensions: Vec<String>,
    /// Shell menu labels removed from Corral's Shell menus (`portal_menu`, no UI); compared after
    /// accelerators, a trailing ellipsis, case and extra spaces are ignored. Empty by default.
    #[serde(rename = "HiddenShellMenuEntries")]
    pub hidden_shell_menu_entries: Vec<String>,
}

// camouflage-spec 5.2: compare packed RGB channels, ignoring alpha.
pub(crate) fn migrated_tint_source(tint: u32, corral: u32) -> &'static str {
    if [0, 8, 16].into_iter().all(|shift| {
        ((tint >> shift) & 255).abs_diff((corral >> shift) & 255) <= 1
    }) { "corral" } else { "custom" }
}

impl<'de> Deserialize<'de> for Settings {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = serde_json::Value::deserialize(deserializer)?;
        let round_up_absent = raw.get("RoundUp").is_none();
        let new_source = raw.get("DesktopTintIconsColorSource").is_some();
        let legacy_source = raw.get("DesktopTintIconsColorUse").is_some();
        let raw_tint_absent = raw.get("DesktopTintIconsColor").is_none();
        // camouflage-spec 3.5: preserve an existing explicit non-white label colour.
        let old_text_custom = raw.get("TextColorAuto").is_none()
            && raw.get("TextColor").and_then(serde_json::Value::as_u64)
                .is_some_and(|value| value & 0x00ff_ffff != 0x00ff_ffff);
        let mut settings: Self = SettingsStorage::deserialize(raw)
            .map_err(serde::de::Error::custom)?.into();
        if !new_source && legacy_source {
            settings.desktop_tint_icons_color_source = migrated_tint_source(
                settings.desktop_tint_icons_color_use, settings.color).into();
            if raw_tint_absent {
                settings.desktop_tint_icons_color = settings.desktop_tint_icons_color_use & 0x00ff_ffff;
            }
        }
        if old_text_custom {
            settings.text_color_auto = false;
        }
        if round_up_absent {
            // round-up-spec 6: a payload from before Round-up (an old snapshot, an old file) keeps
            // its rules. Saving a file needs the configuration copies first; `round_up_store::decode`
            // sets `migration_pending` for files, a restored snapshot is replaced as a whole.
            // shortcut: a newer `RoundUp` block inside a snapshot restored after a downgrade is lost.
            settings.round_up = crate::round_up::RoundUp::migrate(&settings.rules, settings.default_corral);
            settings.round_up.migration_pending = false;
        }
        Ok(settings)
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            disabled: false,
            create_corral_with_mouse_mode: 0,
            disable_create_folder_portal_here: false,
            rules: Vec::new(),
            default_corral: -1,
            new_item_go_to_end: true,
            round_up: crate::round_up::RoundUp::default(),
            round_up_keep: RoundUpKeep::default(),
            always_apply_rules: false,
            apply_rules_now_requested: false,
            snapshot_request: None,
            snapshot_result: None,
            desk_setup_request: None,
            desk_setup_result: None,
            style_request: None,
            style_result: None,
            desktop_blend_icons: false,
            desktop_blend_corral_backgrounds: false,
            desktop_blend_color_on_mouseover: true,
            desktop_blend_color_on_active: false,
            desktop_blend_uses_tint_icons_color: false,
            desktop_blend_icon_opacity: 153, // camouflage-spec 5.1: 60% on the byte scale.
            color: 0,                  // RGB(0,0,0)
            text_color: 0x00FF_FFFF,   // RGB(255,255,255)
            text_color_auto: true,
            transparency_factor: 1380, // default visibility
            desktop_tint_icons: false,
            desktop_tint_icons_color: crate::color_state::TINT_COLOR_DEFAULT,
            desktop_tint_icons_color_source: "corral".into(),
            desktop_tint_icons_color_use: 0,
            fade_in_opacity_on_mouseover: true,
            show_layout_auto_adjustment_options: false,
            tabs_style: 3,
            label_mode: 2,
            darken_titlebars: 1,
            square_edges: false,
            background_blur_enabled: false,
            outline: false,
            background_blur_radius: 60.0,
            icon_animation_disabled: 1,
            scrollbar_mode: 1,
            hide_shortcut_overlays: crate::icon_overlay::DEFAULT_MASK,
            extra_thin_scrollbars: false,
            folder_portals_inherit_view: true,
            show_in_taskbar: false,
            taskbar_hot_corner_enabled: false,
            taskbar_hot_corner_action_l: 1,
            taskbar_hot_corner_action_r: 3,
            hotkeys_glance_enabled: true,
            peek_keep_open: false,
            hotkeys_combo_glance_toggle: 65568, // Win + Space
            hotkeys_combo_glance_hold: 327712,  // Win + Control + Space
            hotkeys_desktop_enabled: false,
            hotkeys_combo_desktop_toggle: 65604, // Win + D
            hotkeys_combo_desktop_hold: 327748,  // Win + Control + D
            quick_hide_enabled: true,
            quick_hide_exclude_corral_groups: false,
            quick_hide_exclude_loose_icons: false,
            quick_hide_auto_hide_on_idle: 0,
            quick_hide_auto_show_when_active: 0,
            quick_hide_auto_show_on_boot: false,
            quick_hide_enabled_temp: false,
            quick_hide_active: false,
            no_quick_hide_animation: false,
            quick_hide_exclusions: None,
            rollup_enabled: true,
            rollup_double_click_to_toggle: true,
            rollup_on_dock_to_edge: true,
            rollup_require_click: false,
            rollup_require_delay: false,
            rollup_require_delay_time: 500,
            tabs_enabled: true,
            tabs_require_confirmation_uses_menu: true,
            tabs_select_mouseovered_tab_when_docked: 0,
            tabs_always_show_colors: true,
            tabs_colored_per_tab_in_segment_view: false,
            tabs_dim_nonselected_tab_on_mouseover: 0,
            use_titlebar_controls: true,
            use_titlebar_controls_on_all_tabs: true,
            tabs_allow_create_with_drag: true,
            // Corral choice (user decision 2026-10-15): off by default
            tabs_require_confirmation: false,
            tabs_select_on_mouseover: 0,
            tabs_select_on_mouseover_delay: 1,
            swipe_enabled: false,
            swipe_switch_via_drag_edge: true,
            swipe_switch_via_touch: true,
            swipe_switch_via_keyboard: true,
            swipe_switch_via_mouse_wheel: true,
            swipe_switch_via_middle_drag: true,
            swipe_switch_via_dual_drag: false,
            swipe_animation_style: 1,
            swipe_show_icons_on_initiate: true,
            auto_take_snapshots: true,
            store_layouts_per_multimon: false,
            adjust_groups_for_resolution: true,
            store_group_data_per_resolution: false,
            snap_enabled: true,
            snap_padding: 0, // default when absent
            // User decision 2026-10-13: off by default (resizing felt too snappy).
            snap_to_discrete_sizes: false,
            snap_line_up_corrals: true,
            snap_should_scale: false,
            group_spacing_matches_desktop: true, // user decision 2026-10-21: Corrals use the desktop's icon spacing
            allow_sorting_breakup: false,
            title_edge_auto_adjust: true,
            show_hidden_corrals: false,
            tips: std::collections::BTreeMap::new(),
            blocked_shell_extensions: Vec::new(),
            hidden_shell_menu_entries: Vec::new(),
        }
    }
}

/// UI binding handle. `(virtual)` = derived from / written to several stored values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    /// (virtual) `!Disabled`.
    EnableCorral,
    CreateCorralMode,
    BlendIcons,
    /// camouflage-spec 5.4: include backgrounds, titlebars and scrollbars.
    BlendBackgrounds,
    /// Existing persisted blur gate, also exposed beside the Camouflage source slider.
    BlurSource,
    BlendColorOnMouseover,
    BlendColorOnActive,
    BlendUsesTintColor,
    BlendIconOpacity,
    TintIcons,
    TintColorMode,
    FadeInOpacity,
    FadedGroupsVisible,
    TabsStyle,
    LabelMode,
    /// (virtual) `IconAnimationDisabled == 0`.
    AnimateIcons,
    /// (virtual) `ScrollbarMode == 1`.
    HideScrollbars,
    InheritViewFromParent,
    ShowInTaskbar,
    HotCornerEnabled,
    HotCornerLeft,
    HotCornerRight,
    HotkeysGlance,
    PeekKeepOpen,
    HotkeyGlanceToggle,
    HotkeyGlanceHold,
    /// (virtual, read-only) Glance shortcuts on and the toggle shortcut is Win + Space, which Windows uses
    /// to switch the input language (peek-hotkeys-spec 8, note under the row).
    PeekWinSpaceNote,
    /// (virtual, read-only) Glance shortcuts on and the stored toggle value is one Corral cannot use
    /// (a modifier-only choice): the default runs instead.
    PeekToggleFallback,
    /// (virtual, read-only) Same for the hold shortcut.
    PeekHoldFallback,
    /// (virtual, read-only) The app published that the keyboard hook is not available (peek-hotkeys-spec 8).
    PeekHookFailed,
    HotkeysDesktop,
    HotkeyDesktopToggle,
    HotkeyDesktopHold,
    QuickHideEnabled,
    /// (virtual) 0 all, 1 icons not in groups (ExcludeCorralGroups), 2 groups only (ExcludeLooseIcons).
    QuickHideWhat,
    /// (virtual) `QuickHideAutoHideOnIdle > 0`; also mirrors AutoShow.
    AutoHideIdle,
    /// (virtual) idle delay bucket in ms, one of [`IDLE_CHOICES_MS`].
    AutoHideIdleChoice,
    /// (virtual) `QuickHideAutoShowWhenActive != 0`.
    AutoShow,
    /// (virtual) checked when value is 0 or 2.
    AutoShowOnDrag,
    AutoShowOnBoot,
    RollupEnabled,
    RollupDoubleClick,
    RollupOnDock,
    RollupRequireClick,
    RollupRequireDelay,
    TabsCreate,
    TabsConfirm,
    /// Colors checkbox: Flat is unchecked; checking switches to Segmented.
    TabsColors,
    /// (virtual, read-only) Visibility of the colours checkbox: `TabsStyle` in {1,2,3} and some Corral
    /// has its own style (`FeaturesTabs.UpdatePerTabSettingVisibility`; see [`LayoutFlags`]).
    TabsColorsVisible,
    /// (virtual) `TabsSelectOnMouseover > 0`.
    TabsMouseover,
    /// (virtual) `TabsSelectOnMouseoverDelay != 0`.
    TabsMouseoverDelay,
    /// (virtual, read-only) Camouflage page "Options" link visibility: `DesktopBlendIcons` or any
    /// Corral with its own `BlendIcons` (`FeaturesBlend.UpdateBlendOptionsAvailable`).
    BlendOptionsVisible,
    SwipeEnabled,
    SwipeDragEdge,
    SwipeTouch,
    /// Keyboard and modified wheel gate; writes both legacy UI values, reads Keyboard only.
    SwipeKeyboardWheel,
    SwipeMiddleDrag,
    SwipeDualDrag,
    /// (virtual) `Swipe_AnimationStyle > 0`.
    SwipeAnimate,
    /// (virtual) `Swipe_AnimationStyle == 2`.
    SwipeBounce,
    SwipeShowIcons,
    DailyBackups,
    /// (virtual) 0 none, 1 scale (legacy), 2 per-monitor-configuration.
    LayoutPreserve,
    SnapEnabled,
    /// (virtual) `SnapPadding != 0`; checking writes 6.
    SnapSpaced,
    SnapSizes,
    SnapLines,
    GroupSpacingMatchesDesktop,
}

fn b(v: bool) -> i64 {
    v as i64
}

impl Settings {
    /// Corral extra: keep the layout choices discoverable, independent of legacy flags.
    pub fn layout_auto_options_visible(&self) -> bool {
        true
    }

    pub fn multi_monitor_options_visible(&self, monitors: i32) -> bool {
        monitors > 1
    }

    pub fn get(&self, f: Field) -> i64 {
        use Field::*;
        match f {
            EnableCorral => b(!self.disabled),
            CreateCorralMode => self.create_corral_with_mouse_mode as i64,
            BlendIcons => b(self.desktop_blend_icons),
            BlendBackgrounds => b(self.desktop_blend_corral_backgrounds),
            BlurSource => b(self.background_blur_enabled),
            BlendColorOnMouseover => b(self.desktop_blend_color_on_mouseover),
            BlendColorOnActive => b(self.desktop_blend_color_on_active),
            BlendUsesTintColor => b(self.desktop_blend_uses_tint_icons_color),
            BlendIconOpacity => self.desktop_blend_icon_opacity as i64,
            TintIcons => b(self.desktop_tint_icons),
            TintColorMode => b(self.desktop_tint_icons_color_source == "custom"),
            FadeInOpacity => b(self.fade_in_opacity_on_mouseover),
            FadedGroupsVisible => b(layout_flags().any_group_faded),
            TabsStyle => self.tabs_style as i64,
            LabelMode => self.label_mode as i64,
            AnimateIcons => b(self.icon_animation_disabled == 0),
            HideScrollbars => b(self.scrollbar_mode == 1),
            InheritViewFromParent => b(self.folder_portals_inherit_view),
            ShowInTaskbar => b(self.show_in_taskbar),
            HotCornerEnabled => b(self.taskbar_hot_corner_enabled),
            HotCornerLeft => self.taskbar_hot_corner_action_l as i64,
            HotCornerRight => self.taskbar_hot_corner_action_r as i64,
            HotkeysGlance => b(self.hotkeys_glance_enabled),
            PeekKeepOpen => b(self.peek_keep_open),
            HotkeyGlanceToggle => self.hotkeys_combo_glance_toggle as i64,
            HotkeyGlanceHold => self.hotkeys_combo_glance_hold as i64,
            PeekWinSpaceNote => b(self.hotkeys_glance_enabled
                && crate::peek_keys::Config::from_stored(true, self.hotkeys_combo_glance_toggle, self.hotkeys_combo_glance_hold).toggle
                    == Some(crate::peek_keys::DEFAULT_TOGGLE)),
            PeekToggleFallback => b(self.hotkeys_glance_enabled && crate::peek_keys::stored_falls_back(self.hotkeys_combo_glance_toggle)),
            PeekHoldFallback => b(self.hotkeys_glance_enabled && crate::peek_keys::stored_falls_back(self.hotkeys_combo_glance_hold)),
            PeekHookFailed => b(self.hotkeys_glance_enabled && crate::peek_keys::hook_failure_published()),
            HotkeysDesktop => b(self.hotkeys_desktop_enabled),
            HotkeyDesktopToggle => self.hotkeys_combo_desktop_toggle as i64,
            HotkeyDesktopHold => self.hotkeys_combo_desktop_hold as i64,
            QuickHideEnabled => b(self.quick_hide_enabled),
            QuickHideWhat => {
                if self.quick_hide_exclude_corral_groups {
                    1
                } else if self.quick_hide_exclude_loose_icons {
                    2
                } else {
                    0
                }
            }
            AutoHideIdle => b(self.quick_hide_auto_hide_on_idle > 0),
            // Zero is the separate disabled mode; a duration read always offers at least Immediately.
            AutoHideIdleChoice => AUTO_HIDE_IDLE_CHOICES.iter()
                .map(|(_, millis)| *millis)
                .filter(|millis| *millis > 0 && *millis <= self.quick_hide_auto_hide_on_idle as i64)
                .max()
                .unwrap_or(AUTO_HIDE_IDLE_CHOICES[1].1),
            AutoShow => b(self.quick_hide_auto_show_when_active != 0),
            AutoShowOnDrag => b(self.quick_hide_auto_show_when_active == 0 || self.quick_hide_auto_show_when_active == 2),
            AutoShowOnBoot => b(self.quick_hide_auto_show_on_boot),
            RollupEnabled => b(self.rollup_enabled),
            RollupDoubleClick => b(self.rollup_double_click_to_toggle),
            RollupOnDock => b(self.rollup_on_dock_to_edge),
            RollupRequireClick => b(self.rollup_require_click),
            RollupRequireDelay => b(self.rollup_require_delay),
            TabsCreate => b(self.tabs_allow_create_with_drag),
            TabsConfirm => b(self.tabs_require_confirmation),
            TabsColors => b(self.tabs_style != 1 && (self.tabs_always_show_colors || self.tabs_colored_per_tab_in_segment_view)),
            TabsColorsVisible => b((1..=3).contains(&self.tabs_style) && layout_flags().any_group_styled),
            TabsMouseover => b(self.tabs_select_on_mouseover > 0),
            TabsMouseoverDelay => b(self.tabs_select_on_mouseover_delay != 0),
            BlendOptionsVisible => b(self.desktop_blend_icons || layout_flags().any_group_blend),
            SwipeEnabled => b(self.swipe_enabled),
            SwipeDragEdge => b(self.swipe_switch_via_drag_edge),
            SwipeTouch => b(self.swipe_switch_via_touch),
            SwipeKeyboardWheel => b(self.swipe_switch_via_keyboard),
            SwipeMiddleDrag => b(self.swipe_switch_via_middle_drag),
            SwipeDualDrag => b(self.swipe_switch_via_dual_drag),
            SwipeAnimate => b(self.swipe_animation_style > 0),
            SwipeBounce => b(self.swipe_animation_style == 2),
            SwipeShowIcons => b(self.swipe_show_icons_on_initiate),
            DailyBackups => b(self.auto_take_snapshots),
            LayoutPreserve => {
                // multimon -> per-monitor; else !AdjustGroupsForResolution -> none; else scale
                // (the legacy per-resolution mode is not offered and reads as "scale").
                if self.store_layouts_per_multimon {
                    2
                } else if !self.adjust_groups_for_resolution {
                    0
                } else {
                    1
                }
            }
            SnapEnabled => b(self.snap_enabled),
            SnapSpaced => b(self.snap_padding != 0),
            SnapSizes => b(self.snap_to_discrete_sizes),
            SnapLines => b(self.snap_line_up_corrals),
            GroupSpacingMatchesDesktop => b(self.group_spacing_matches_desktop),
        }
    }

    pub fn set(&mut self, f: Field, v: i64) {
        use Field::*;
        let on = v != 0;
        match f {
            EnableCorral => self.disabled = !on,
            CreateCorralMode => self.create_corral_with_mouse_mode = v.clamp(0, 2) as u32,
            BlendIcons => self.desktop_blend_icons = on,
            BlendBackgrounds => self.desktop_blend_corral_backgrounds = on,
            BlurSource => {
                self.background_blur_enabled = on;
                if on && (!self.background_blur_radius.is_finite() || self.background_blur_radius <= 0.0) {
                    self.background_blur_radius = 60.0;
                }
            },
            BlendColorOnMouseover => self.desktop_blend_color_on_mouseover = on,
            BlendColorOnActive => self.desktop_blend_color_on_active = on,
            BlendUsesTintColor => self.desktop_blend_uses_tint_icons_color = on,
            BlendIconOpacity => self.desktop_blend_icon_opacity = v.clamp(0, 255) as u8,
            TintIcons => self.desktop_tint_icons = on,
            TintColorMode => {
                // camouflage-spec 5.4: choosing a mode preserves the custom swatch.
                self.desktop_tint_icons_color_source = if v == 0 { "corral" } else { "custom" }.into();
            }
            FadeInOpacity => self.fade_in_opacity_on_mouseover = on,
            FadedGroupsVisible | PeekWinSpaceNote | PeekToggleFallback | PeekHoldFallback | PeekHookFailed => {},
            TabsStyle => self.tabs_style = v.clamp(0, 3) as u32,
            LabelMode => self.label_mode = v.clamp(0, 2) as u32,
            AnimateIcons => self.icon_animation_disabled = if on { 0 } else { 1 },
            HideScrollbars => self.scrollbar_mode = if on { 1 } else { 0 },
            InheritViewFromParent => self.folder_portals_inherit_view = on,
            ShowInTaskbar => self.show_in_taskbar = on,
            HotCornerEnabled => self.taskbar_hot_corner_enabled = on,
            HotCornerLeft => {
                // Leaving "Show Corrals" while the right
                // click does a full-desktop action, or choosing "Show the full desktop" while the
                // right click does nothing, makes the right click "Show Corrals".
                let old = self.taskbar_hot_corner_action_l;
                let new = v as i32;
                self.taskbar_hot_corner_action_l = new;
                let r = self.taskbar_hot_corner_action_r;
                if old == 1 && (2..=4).contains(&r) || new == 2 && r == 0 {
                    self.taskbar_hot_corner_action_r = 1;
                }
            }
            HotCornerRight => self.taskbar_hot_corner_action_r = v as i32,
            HotkeysGlance => self.hotkeys_glance_enabled = on,
            PeekKeepOpen => self.peek_keep_open = on,
            HotkeyGlanceToggle | HotkeyGlanceHold | HotkeyDesktopToggle | HotkeyDesktopHold => self.set_hotkey_combo(f, v as u32),
            HotkeysDesktop => self.hotkeys_desktop_enabled = on,
            QuickHideEnabled => self.quick_hide_enabled = on,
            QuickHideWhat => {
                self.quick_hide_exclude_corral_groups = v == 1;
                self.quick_hide_exclude_loose_icons = v == 2;
            }
            AutoHideIdle => {
                if on {
                    if self.quick_hide_auto_hide_on_idle == 0 {
                        // Unspecified: start at "Immediately" (value 1), which is
                        // also what an existing value of 0 maps to when loading.
                        self.quick_hide_auto_hide_on_idle = IDLE_CHOICES_MS[0];
                    }
                    // Idle-hide switches "Automatically show icons" on with it, which writes 2 from 0.
                    self.set(AutoShow, 1);
                } else {
                    self.quick_hide_auto_hide_on_idle = 0;
                    self.quick_hide_auto_show_when_active = 0;
                }
            }
            AutoHideIdleChoice => self.quick_hide_auto_hide_on_idle = v.max(1) as u32,
            AutoShow => {
                // `OptionAutoShow_CheckedChanged`: on writes 2 when the "drag/dropping" box is ticked
                // (it reads ticked while the value is 0 or 2, so switching on from 0 gives 2), else 1.
                self.quick_hide_auto_show_when_active = match (on, self.quick_hide_auto_show_when_active) {
                    (false, _) => 0,
                    (true, 0) => 2,
                    (true, current) => current,
                };
            }
            AutoShowOnDrag => self.quick_hide_auto_show_when_active = if on { 2 } else { 1 },
            AutoShowOnBoot => self.quick_hide_auto_show_on_boot = on,
            RollupEnabled => self.rollup_enabled = on,
            RollupDoubleClick => self.rollup_double_click_to_toggle = on,
            RollupOnDock => self.rollup_on_dock_to_edge = on,
            RollupRequireClick => self.rollup_require_click = on,
            RollupRequireDelay => self.rollup_require_delay = on,
            TabsCreate => self.tabs_allow_create_with_drag = on,
            TabsConfirm => self.tabs_require_confirmation = on,
            TabsColors => {
                if self.tabs_style == 1 { self.tabs_style = 2; }
                if !on { self.tabs_colored_per_tab_in_segment_view = false; }
                self.tabs_always_show_colors = on;
            }
            TabsColorsVisible | BlendOptionsVisible => {}
            TabsMouseover => self.tabs_select_on_mouseover = if on { 1 } else { 0 },
            TabsMouseoverDelay => self.tabs_select_on_mouseover_delay = if on { 1 } else { 0 },
            SwipeEnabled => self.swipe_enabled = on,
            SwipeDragEdge => self.swipe_switch_via_drag_edge = on,
            SwipeTouch => self.swipe_switch_via_touch = on,
            SwipeKeyboardWheel => {
                self.swipe_switch_via_keyboard = on;
                self.swipe_switch_via_mouse_wheel = on;
            }
            SwipeMiddleDrag => self.swipe_switch_via_middle_drag = on,
            SwipeDualDrag => self.swipe_switch_via_dual_drag = on,
            SwipeAnimate => {
                // Unchecking animate also drops bounce (bounce is only enabled while animating).
                self.swipe_animation_style = if on { 1 } else { 0 };
            }
            SwipeBounce => self.swipe_animation_style = if on { 2 } else { 1 },
            SwipeShowIcons => self.swipe_show_icons_on_initiate = on,
            DailyBackups => self.auto_take_snapshots = on,
            LayoutPreserve => match v {
                0 => {
                    self.store_layouts_per_multimon = false;
                    self.adjust_groups_for_resolution = false;
                }
                1 => {
                    self.store_layouts_per_multimon = false;
                    self.adjust_groups_for_resolution = true;
                    self.store_group_data_per_resolution = false;
                }
                _ => {
                    self.store_layouts_per_multimon = true;
                    self.adjust_groups_for_resolution = true;
                    self.store_group_data_per_resolution = false;
                },
            },
            SnapEnabled => self.snap_enabled = on,
            SnapSpaced => {
                self.snap_padding = if on { SNAP_PADDING_DEFAULT } else { 0 };
                if on { self.snap_should_scale = true; }
            },
            SnapSizes => self.snap_to_discrete_sizes = on,
            SnapLines => self.snap_line_up_corrals = on,
            GroupSpacingMatchesDesktop => self.group_spacing_matches_desktop = on,
        }
    }

    pub fn is_on(&self, f: Field) -> bool {
        self.get(f) != 0
    }

    /// One of the four hotkey combo boxes (`FeaturesQuickShow.FixOtherCombos`): choosing a combination
    /// other than "Disabled" that another combo already uses gives that other combo the old value of
    /// this one, so a combination is never bound twice.
    fn set_hotkey_combo(&mut self, f: Field, value: u32) {
        use Field::*;
        let old = match f {
            HotkeyGlanceToggle => self.hotkeys_combo_glance_toggle,
            HotkeyGlanceHold => self.hotkeys_combo_glance_hold,
            HotkeyDesktopToggle => self.hotkeys_combo_desktop_toggle,
            _ => self.hotkeys_combo_desktop_hold,
        };
        if value != 0 {
            let slots: [(Field, &mut u32); 4] = [
                (HotkeyGlanceToggle, &mut self.hotkeys_combo_glance_toggle),
                (HotkeyGlanceHold, &mut self.hotkeys_combo_glance_hold),
                (HotkeyDesktopToggle, &mut self.hotkeys_combo_desktop_toggle),
                (HotkeyDesktopHold, &mut self.hotkeys_combo_desktop_hold),
            ];
            for (other, slot) in slots {
                if other != f && *slot == value {
                    *slot = old;
                }
            }
        }
        match f {
            HotkeyGlanceToggle => self.hotkeys_combo_glance_toggle = value,
            HotkeyGlanceHold => self.hotkeys_combo_glance_hold = value,
            HotkeyDesktopToggle => self.hotkeys_combo_desktop_toggle = value,
            _ => self.hotkeys_combo_desktop_hold = value,
        }
    }
}

#[cfg(test)]
mod q33_tests {
    use super::*;

    #[test]
    fn layout_options_are_always_visible_without_mutating_legacy_flags() {
        for store in [false, true] {
            for adjust in [false, true] {
                for persisted in [false, true] {
                    let mut s = Settings { store_layouts_per_multimon: store, adjust_groups_for_resolution: adjust,
                        show_layout_auto_adjustment_options: persisted, ..Settings::default() };
                    let before = serde_json::to_string(&s).unwrap();
                    let expected = true;
                    assert_eq!(s.layout_auto_options_visible(), expected);
                    for monitors in [0, 1, 2, 3] { assert_eq!(s.multi_monitor_options_visible(monitors), monitors > 1); }
                    assert_eq!(serde_json::to_string(&s).unwrap(), before);
                    s.set(Field::LayoutPreserve, 2);
                    assert!(s.adjust_groups_for_resolution);
                    assert_eq!(s.layout_auto_options_visible(), expected, "choices remain discoverable after selection");
                }
            }
        }
    }

    #[test]
    fn color_fields_round_trip_with_defaults_and_never_write_the_legacy_tint_choice() {
        let mut s: Settings = serde_json::from_str("{}").unwrap();
        assert!(s.fade_in_opacity_on_mouseover);
        assert_eq!(s.desktop_tint_icons_color, crate::color_state::TINT_COLOR_DEFAULT);
        assert_eq!(s.transparency_factor, crate::color_state::SEE_THROUGH_DEFAULT);
        s.set(Field::FadeInOpacity, 0);
        s.color = 0x00B5_6F3F;
        s.transparency_factor = 1720;
        s.desktop_tint_icons_color = 0x0033_7AB9;
        s.desktop_tint_icons_color_source = "custom".into();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("DesktopTintIconsColorUse"));
        let restored = serde_json::from_str::<Settings>(&json).unwrap();
        assert_eq!(restored.desktop_tint_icons_color_source, "custom");
        assert_eq!((restored.desktop_tint_icons_color, restored.color, restored.transparency_factor), (0x0033_7AB9, 0x00B5_6F3F, 1720));
    }

    #[test]
    fn fade_checkbox_visibility_reads_group_opacity() {
        let mut layout = crate::model::Layout::new();
        // No groups means the optional checkbox is hidden.
        assert!(!LayoutFlags::from_groups(&layout.groups).any_group_faded);
        layout.add_group("QA".into(), "test".into(), crate::geometry::Rect::new(0, 0, 300, 200), crate::model::GroupContent::Items { items: Vec::new() });
        assert!(!LayoutFlags::from_groups(&layout.groups).any_group_faded);
        layout.groups[0].opacity = 100;
        let flags = LayoutFlags::from_groups(&layout.groups);
        assert!(flags.any_group_faded);
        set_layout_flags(flags);
        assert!(Settings::default().is_on(Field::FadedGroupsVisible));
        set_layout_flags(LayoutFlags::default());
    }
}

/// Facts about the Corrals of the current layout that gate a few settings rows. The settings window
/// refreshes this from the layout file when such a page opens.
/// Thread-local so that a reloaded [`Settings`] value does not lose it and tests stay independent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayoutFlags {
    /// Some Corral has an own style (any `Styling\<id>...` value; `GroupStyle` not empty).
    pub any_group_styled: bool,
    /// Some Corral has its own `BlendIcons` switched on.
    pub any_group_blend: bool,
    pub any_group_faded: bool,
}

impl LayoutFlags {
    pub fn from_groups(groups: &[crate::model::Group]) -> LayoutFlags {
        LayoutFlags {
            any_group_faded: groups.iter().any(|g| g.opacity != 255),
            any_group_styled: groups.iter().any(|g| crate::color_state::has_own_look(&g.style)),
            any_group_blend: groups.iter().any(|g| g.style.blend_icons.is_some_and(|v| v != 0)),
        }
    }
}

thread_local! {
    static LAYOUT_FLAGS: std::cell::Cell<LayoutFlags> = const { std::cell::Cell::new(LayoutFlags { any_group_styled: false, any_group_blend: false, any_group_faded: false }) };
}

/// Current layout facts of this thread (all false until [`set_layout_flags`]).
pub fn layout_flags() -> LayoutFlags {
    LAYOUT_FLAGS.with(|f| f.get())
}

pub fn set_layout_flags(flags: LayoutFlags) {
    LAYOUT_FLAGS.with(|f| f.set(flags));
}

// ---------------------------------------------------------------------------------------------
// Persistence

/// `%APPDATA%\Corral\settings.json` (temp dir when APPDATA is unset).
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Corral").join("settings.json")
}

fn backup_path(path: &Path) -> PathBuf {
    crate::model::backup_path(path)
}

fn load_file(path: &Path) -> Option<Settings> {
    let text = fs::read_to_string(path).ok()?;
    round_up_store::decode(text.trim_start_matches('\u{feff}')).ok()
}

/// Load `path`; if missing or corrupt fall back to `path.bak`, then to defaults.
///
/// Side effect: the Camouflage wallpaper source follows the blur setting (`tint::set_blur`), so
/// every load (app start, settings-file change, preview) keeps it current.
pub fn load_with_backup(path: &Path) -> Settings {
    let s = load_file(path).or_else(|| load_file(&backup_path(path))).unwrap_or_default();
    crate::tint::set_blur(s.background_blur_enabled, s.background_blur_radius);
    s
}

/// Atomic save: write `<path>.tmp`, flush, keep the previous file as `<path>.bak`, rename.
pub fn save(path: &Path, settings: &Settings) -> io::Result<()> {
    let json = round_up_store::encode(settings)?;
    if settings.round_up.migration_pending {
        // round-up-spec 6.6: the first migrated save keeps a copy of the file it replaces; when
        // the copy fails nothing is written.
        round_up_store::backup_settings_before_migration(path)?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".tmp");
    let temp = PathBuf::from(temp);
    {
        let mut file = fs::File::create(&temp)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
    }
    if path.exists() {
        fs::copy(path, backup_path(path))?;
    }
    fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn idle_choice_floors_at_every_offered_duration_boundary() {
        let mut s = Settings::default();
        for (stored, expected) in [
            (0, 1), (1, 1), (2, 1), (999, 1), (1000, 1000), (1001, 1000),
            (29999, 1000), (30000, 30000), (30001, 30000),
            (119999, 30000), (120000, 120000), (120001, 120000),
            (599999, 120000), (600000, 600000), (600001, 600000), (u32::MAX, 600000),
        ] {
            s.quick_hide_auto_hide_on_idle = stored;
            assert_eq!(s.get(Field::AutoHideIdleChoice), expected, "stored {stored}");
        }
    }

    use super::*;

    #[test]
    fn q33e_options_defaults_bindings_and_global_background_roundtrip() {
        let mut s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.get(Field::BlendIconOpacity), 153);
        assert_eq!(s.get(Field::BlendColorOnMouseover), 1);
        for f in [Field::BlendColorOnMouseover, Field::BlendColorOnActive, Field::BlendUsesTintColor] {
            s.set(f, 1);
            assert_eq!(s.get(f), 1);
        }
        s.set(Field::BlendIconOpacity, 178);
        s.desktop_blend_corral_backgrounds = true;
        let roundtrip: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(roundtrip, s);
        assert!(roundtrip.desktop_blend_corral_backgrounds);
        s.set(Field::BlendIconOpacity, 999);
        assert_eq!(s.desktop_blend_icon_opacity, 255);
    }

    #[test]
    fn an_unreadable_pending_style_request_or_result_is_dropped_not_fatal() {
        // The retired `reset_text` action, an unknown action, and a malformed answer.
        for raw in [
            r#"{"Color":5,"StyleRequest":{"nonce":1,"layout_path":"a.json","action":{"action":"reset_text","id":null}}}"#,
            r#"{"Color":5,"StyleRequest":{"nonce":1,"layout_path":"a.json","action":{"action":"nonsense"}}}"#,
            r#"{"Color":5,"StyleRequest":7,"StyleResult":{"nonce":"x"}}"#,
        ] {
            let s: Settings = serde_json::from_str(raw).unwrap();
            assert_eq!(s.color, 5, "the rest of the file still loads");
            assert!(s.style_request.is_none() && s.style_result.is_none());
        }
        let good = r#"{"StyleRequest":{"nonce":1,"layout_path":"a.json","action":{"action":"reset_all"}},"StyleResult":{"nonce":1,"ok":true,"message":"m"}}"#;
        let s: Settings = serde_json::from_str(good).unwrap();
        assert!(s.style_request.is_some() && s.style_result.is_some());
    }

    #[test]
    fn retired_tab_keys_load_from_old_files_and_are_never_written() {
        let old: Settings = serde_json::from_str(r#"{"UseTitlebarControlsOnAllTabs": false, "TabsDimNonSelectedTabOnMouseover": 5}"#).unwrap();
        assert!(!old.use_titlebar_controls_on_all_tabs);
        assert_eq!(old.tabs_dim_nonselected_tab_on_mouseover, 5);
        let written = serde_json::to_value(&old).unwrap();
        let written = written.as_object().unwrap();
        assert!(!written.contains_key("UseTitlebarControlsOnAllTabs"));
        assert!(!written.contains_key("TabsDimNonSelectedTabOnMouseover"));
    }

    #[test]
    fn tabs_absent_settings_and_hidden_values_roundtrip() {
        let mut s: Settings = serde_json::from_str("{}").unwrap();
        let tabs = crate::tabs::TabSettings::from(&s);
        assert_eq!(tabs, crate::tabs::TabSettings::default());
        s.tabs_enabled = false;
        s.tabs_require_confirmation_uses_menu = false;
        s.tabs_select_mouseovered_tab_when_docked = 1;
        s.use_titlebar_controls = false;
        s.tabs_select_on_mouseover_delay = 750;
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s, "stored values come back exactly as written");
        assert!(!back.use_titlebar_controls);
        // TabsMouseoverDelay is a flag (tabs-spec 4): any stored number only means "wait".
        let effective = crate::tabs::TabSettings::from(&back);
        assert!(effective.wait);
        s.tabs_select_on_mouseover = 1;
        s.tabs_select_on_mouseover_delay = 0;
        let effective = crate::tabs::TabSettings::from(&s);
        assert_eq!(effective.hover_wait(false), Some(0), "off switches at once");
        assert_eq!(s.tabs_select_mouseovered_tab_when_docked, 1, "stored values nobody reads are kept");
    }

    #[test]
    fn tabs_colors_checkbox_follows_style_and_per_tab_color_settings() {
        let mut s = Settings::default();
        s.tabs_style = 1;
        s.tabs_colored_per_tab_in_segment_view = true;
        assert!(!s.is_on(Field::TabsColors));
        s.set(Field::TabsColors, 1);
        assert_eq!(s.tabs_style, 2);
        assert!(s.tabs_always_show_colors && s.is_on(Field::TabsColors));
        s.set(Field::TabsColors, 0);
        assert!(!s.tabs_always_show_colors && !s.tabs_colored_per_tab_in_segment_view);
        s.tabs_style = 0;
        assert!(!s.is_on(Field::TabsColorsVisible));
        s.set(Field::TabsMouseover, 1);
        s.set(Field::TabsMouseoverDelay, 0);
        assert_eq!((s.tabs_select_on_mouseover, s.tabs_select_on_mouseover_delay), (1, 0));
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("corral-settings-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn defaults_match_documented_values() {
        let s = Settings::default();
        // Rollup defaults
        assert!(s.rollup_enabled && s.rollup_double_click_to_toggle && s.rollup_on_dock_to_edge);
        assert!(!s.rollup_require_click && !s.rollup_require_delay);
        assert_eq!(s.rollup_require_delay_time, 500);
        // 4.3 Personalization (schema: LabelMode 2, TabsStyle 3, DarkenTitlebars 1, ScrollbarMode 1, IconAnimationDisabled 1)
        assert_eq!((s.label_mode, s.tabs_style, s.darken_titlebars, s.scrollbar_mode, s.icon_animation_disabled), (2, 3, 1, 1, 1));
        assert_eq!(s.transparency_factor, 1380);
        assert_eq!((s.color, s.text_color), (0, 0xFFFFFF));
        assert!(!s.square_edges && !s.background_blur_enabled);
        assert!(!s.get(Field::AnimateIcons).eq(&1) && s.is_on(Field::HideScrollbars));
        // 4.1 Welcome
        assert_eq!(s.create_corral_with_mouse_mode, 0);
        assert!(s.is_on(Field::EnableCorral));
        // 4.5 Peek
        assert_eq!((s.taskbar_hot_corner_action_l, s.taskbar_hot_corner_action_r), (1, 3));
        assert_eq!((s.hotkeys_combo_glance_toggle, s.hotkeys_combo_glance_hold), (65568, 327712));
        assert_eq!((s.hotkeys_combo_desktop_toggle, s.hotkeys_combo_desktop_hold), (65604, 327748));
        assert!(s.hotkeys_glance_enabled && !s.hotkeys_desktop_enabled && !s.taskbar_hot_corner_enabled);
        // 4.6 Hide
        assert!(s.quick_hide_enabled && !s.quick_hide_auto_show_on_boot);
        assert_eq!((s.quick_hide_auto_hide_on_idle, s.quick_hide_auto_show_when_active), (0, 0));
        // 4.8 Tabs
        assert!(s.tabs_allow_create_with_drag && !s.tabs_require_confirmation, "Corral default: no confirmation before creating tabs");
        assert_eq!((s.tabs_select_on_mouseover, s.tabs_select_on_mouseover_delay), (0, 1));
        // 4.9 Pages
        assert!(!s.swipe_enabled && s.swipe_switch_via_drag_edge && s.swipe_show_icons_on_initiate);
        assert!(!s.swipe_switch_via_dual_drag, "ranges-dock-clearview-spec 6: the both-buttons drag is opt-in");
        let kept: Settings = serde_json::from_str(r#"{"Swipe_SwitchViaDualDrag":true}"#).unwrap();
        assert!(kept.swipe_switch_via_dual_drag, "a stored value always wins");
        assert_eq!(s.swipe_animation_style, 1);
        // 4.11 Layout
        assert!(s.snap_enabled && s.auto_take_snapshots && !s.snap_to_discrete_sizes && s.snap_line_up_corrals);
        assert_eq!(s.snap_padding, 0, "engine default when SnapPadding is absent");
        assert!(!s.snap_should_scale);
        assert!(s.group_spacing_matches_desktop && !s.store_layouts_per_multimon && s.adjust_groups_for_resolution);
        assert!(s.folder_portals_inherit_view);
    }

    #[test]
    fn hidden_disable_quick_portal_setting_defaults_and_round_trips() {
        let absent: Settings = serde_json::from_str("{}").unwrap();
        assert!(!absent.disable_create_folder_portal_here);
        let enabled: Settings = serde_json::from_str(r#"{"DisableCreateFolderPortalHere":true}"#).unwrap();
        assert!(enabled.disable_create_folder_portal_here);
        let json = serde_json::to_value(enabled).unwrap();
        assert_eq!(json["DisableCreateFolderPortalHere"], true);
    }

    #[test]
    fn shortcut_overlay_setting_uses_stored_name_and_default() {
        let mut s: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(s.hide_shortcut_overlays, 0x10000006);
        assert_eq!(serde_json::to_value(&s).unwrap()["HideShortcutOverlays"], 0x10000006u32);
        s.hide_shortcut_overlays = crate::icon_overlay::Policy(s.hide_shortcut_overlays).toggle(crate::icon_overlay::FOLDER).0;
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        let disabled: Settings = serde_json::from_str(r#"{"HideShortcutOverlays":6}"#).unwrap();
        assert!(!crate::icon_overlay::Policy(disabled.hide_shortcut_overlays).hides(crate::icon_overlay::LinkClass::Program));
    }

    #[test]
    fn serde_names_are_registry_value_names() {
        let json = serde_json::to_value(Settings::default()).unwrap();
        for key in ["RollupOnDock", "RollupRequireDelayTime", "Swipe_Enabled", "TransparencyFactor", "QuickHideAutoHideOnIdle", "HotkeysComboGlanceToggle"] {
            assert!(json.get(key).is_some(), "{key}");
        }
        assert_eq!(json["RollupRequireDelayTime"], 500);
    }

    #[test]
    fn json_roundtrip_and_partial_files() {
        let dir = temp_dir("rt");
        let path = dir.join("settings.json");
        let mut s = Settings::default();
        s.rollup_enabled = false;
        s.hotkeys_combo_glance_toggle = 458820;
        s.swipe_enabled = true;
        save(&path, &s).unwrap();
        assert_eq!(load_with_backup(&path), s);
        fs::write(&path, r#"{"RollupEnabled":false,"Unknown":1}"#).unwrap();
        let p = load_with_backup(&path);
        assert!(!p.rollup_enabled && p.tabs_style == 3);
        // A file holding only the two interop lists (no UI) loads them and defaults everything else;
        // a save keeps them.
        fs::write(&path, r#"{"BlockedShellExtensions":["\\Acme\\Widgets\\","WidgetsMenu"],"HiddenShellMenuEntries":["Configure Widgets"]}"#).unwrap();
        let p = load_with_backup(&path);
        assert_eq!(p.blocked_shell_extensions, [r"\Acme\Widgets\", "WidgetsMenu"]);
        assert_eq!(p.hidden_shell_menu_entries, ["Configure Widgets"]);
        assert!(p.rollup_enabled && p.tabs_style == 3 && p.hotkeys_glance_enabled && !p.disabled);
        save(&path, &p).unwrap();
        let back = load_with_backup(&path);
        assert_eq!((back.blocked_shell_extensions, back.hidden_shell_menu_entries), (p.blocked_shell_extensions, p.hidden_shell_menu_entries));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupt_file_falls_back_to_bak_then_defaults() {
        let dir = temp_dir("bak");
        let path = dir.join("settings.json");
        let mut s = Settings::default();
        s.snap_enabled = false;
        save(&path, &s).unwrap();
        let mut s2 = s.clone();
        s2.tabs_style = 1;
        save(&path, &s2).unwrap(); // .bak now holds `s`
        fs::write(&path, "{ corrupt").unwrap();
        assert_eq!(load_with_backup(&path), s);
        fs::write(backup_path(&path), "also corrupt").unwrap();
        assert_eq!(load_with_backup(&path), Settings::default());
        assert_eq!(load_with_backup(&dir.join("missing.json")), Settings::default());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn virtual_fields_follow_their_documented_semantics() {
        let mut s = Settings::default();
        s.set(Field::EnableCorral, 0);
        assert!(s.disabled);
        s.set(Field::AnimateIcons, 1);
        assert_eq!(s.icon_animation_disabled, 0);
        s.set(Field::HideScrollbars, 0);
        assert_eq!(s.scrollbar_mode, 0);
        s.set(Field::AutoHideIdle, 1);
        assert_eq!(s.quick_hide_auto_hide_on_idle, 1);
        s.set(Field::AutoHideIdleChoice, 120000);
        assert_eq!(s.get(Field::AutoHideIdleChoice), 120000);
        s.quick_hide_auto_hide_on_idle = 45_000;
        assert_eq!(s.get(Field::AutoHideIdleChoice), 30000);
        s.set(Field::SwipeBounce, 1);
        assert_eq!(s.swipe_animation_style, 2);
        s.set(Field::SwipeKeyboardWheel, 0);
        assert!(!s.swipe_switch_via_keyboard && !s.swipe_switch_via_mouse_wheel);
        s.set(Field::SnapSpaced, 0);
        assert_eq!(s.snap_padding, 0);
        s.set(Field::SnapSpaced, 1);
        assert_eq!(s.snap_padding, 6);
        assert!(s.snap_should_scale);
        s.set(Field::LayoutPreserve, 2);
        assert_eq!(s.get(Field::LayoutPreserve), 2);
        s.set(Field::QuickHideWhat, 2);
        assert!(s.quick_hide_exclude_loose_icons && !s.quick_hide_exclude_corral_groups);
    }

    // ---- Peek / Hide / Pages / Tabs / Camouflage page defaults ----

    #[test]
    fn hotkey_combo_never_bound_twice() {
        // FeaturesQuickShow.FixOtherCombos: the other combo takes the old value of the one changed.
        let mut s = Settings::default();
        s.set(Field::HotkeyGlanceToggle, 65604); // Win + D, used by the desktop toggle
        assert_eq!(s.hotkeys_combo_glance_toggle, 65604);
        assert_eq!(s.hotkeys_combo_desktop_toggle, 65568, "desktop toggle got the old Corrals toggle value");
        // "Disabled" (0) never swaps anything.
        let before = s.clone();
        s.set(Field::HotkeyGlanceHold, 0);
        assert_eq!(s.hotkeys_combo_glance_hold, 0);
        assert_eq!((s.hotkeys_combo_glance_toggle, s.hotkeys_combo_desktop_toggle), (before.hotkeys_combo_glance_toggle, before.hotkeys_combo_desktop_toggle));
        // Re-choosing the own value changes nothing else.
        let before = s.clone();
        s.set(Field::HotkeyDesktopHold, s.hotkeys_combo_desktop_hold as i64);
        assert_eq!(s, before);
        // Every combo set is a plain write when the value is unused.
        s.set(Field::HotkeyDesktopHold, 851968);
        assert_eq!(s.hotkeys_combo_desktop_hold, 851968);
    }

    #[test]
    fn hot_corner_left_choice_fixes_up_right_click() {
        // TaskbarHotCornerOptionL_SelectedIndexChanged.
        let mut s = Settings::default();
        assert_eq!((s.taskbar_hot_corner_action_l, s.taskbar_hot_corner_action_r), (1, 3));
        s.set(Field::HotCornerLeft, -1); // was "Show Corrals" while right click = full desktop (fast)
        assert_eq!((s.taskbar_hot_corner_action_l, s.taskbar_hot_corner_action_r), (-1, 1));
        let mut s = Settings::default();
        s.taskbar_hot_corner_action_l = -1;
        s.taskbar_hot_corner_action_r = 0;
        s.set(Field::HotCornerLeft, 2); // full desktop on the left while the right does nothing
        assert_eq!(s.taskbar_hot_corner_action_r, 1);
        s.taskbar_hot_corner_action_r = 3;
        s.set(Field::HotCornerLeft, 4); // old value was 2, not 1: right click stays
        assert_eq!(s.taskbar_hot_corner_action_r, 3);
        s.set(Field::HotCornerRight, 0);
        assert_eq!(s.taskbar_hot_corner_action_l, 4, "the right click choice never touches the left");
    }

    #[test]
    fn auto_show_from_off_writes_drag_drop_variant() {
        // OptionAutoShow_CheckedChanged: the drag/drop box reads ticked while the value is 0, so 2.
        let mut s = Settings::default();
        assert_eq!(s.quick_hide_auto_show_when_active, 0);
        s.set(Field::AutoShow, 1);
        assert_eq!(s.quick_hide_auto_show_when_active, 2);
        assert!(s.is_on(Field::AutoShowOnDrag));
        s.set(Field::AutoShowOnDrag, 0);
        assert_eq!(s.quick_hide_auto_show_when_active, 1);
        s.set(Field::AutoShow, 1);
        assert_eq!(s.quick_hide_auto_show_when_active, 1, "already on: unchanged");
        s.set(Field::AutoShow, 0);
        assert_eq!(s.quick_hide_auto_show_when_active, 0);
        // Idle-hide switches auto-show on and off with it (OptionAutoHideIdle_CheckedChanged).
        s.set(Field::AutoHideIdle, 1);
        assert_eq!((s.quick_hide_auto_hide_on_idle, s.quick_hide_auto_show_when_active), (1, 2));
        s.set(Field::AutoHideIdle, 0);
        assert_eq!((s.quick_hide_auto_hide_on_idle, s.quick_hide_auto_show_when_active), (0, 0));
    }

    #[test]
    fn layout_flags_gate_tabs_colors_and_camouflage_options() {
        use crate::model::{GroupContent, Layout};
        set_layout_flags(LayoutFlags::default());
        let mut s = Settings::default();
        assert!(!s.is_on(Field::TabsColorsVisible), "no Corral with an own style");
        assert!(!s.is_on(Field::BlendOptionsVisible));
        let mut layout = Layout::new();
        let plain = layout.add_group("A".into(), "m".into(), crate::geometry::Rect::default(), GroupContent::Items { items: vec![] });
        let styled = layout.add_group("B".into(), "m".into(), crate::geometry::Rect::default(), GroupContent::Items { items: vec![] });
        layout.group_mut(styled).unwrap().style.color = Some(0x123456);
        set_layout_flags(LayoutFlags::from_groups(&layout.groups));
        assert_eq!(layout_flags(), LayoutFlags { any_group_styled: true, any_group_blend: false, any_group_faded: false });
        for (style, visible) in [(0, false), (1, true), (2, true), (3, true)] {
            s.tabs_style = style;
            assert_eq!(s.is_on(Field::TabsColorsVisible), visible, "TabsStyle {style}");
        }
        // Camouflage "Options": global switch or a Corral with its own BlendIcons.
        assert!(!s.is_on(Field::BlendOptionsVisible));
        s.desktop_blend_icons = true;
        assert!(s.is_on(Field::BlendOptionsVisible));
        s.desktop_blend_icons = false;
        layout.group_mut(plain).unwrap().style.blend_icons = Some(0);
        assert!(!LayoutFlags::from_groups(&layout.groups).any_group_blend, "an own BlendIcons of 0 is off");
        layout.group_mut(plain).unwrap().style.blend_icons = Some(1);
        set_layout_flags(LayoutFlags::from_groups(&layout.groups));
        assert!(s.is_on(Field::BlendOptionsVisible));
        // The flags are not part of the persisted settings.
        assert_eq!(serde_json::to_value(&s).unwrap().get("BlendOptionsVisible"), None);
        set_layout_flags(LayoutFlags::default());
    }

    #[test]
    fn pages_hide_tabs_defaults() {
        // FeaturesQuickHide / FeaturesDesktopSwipe / FeaturesTabs defaults.
        let s = Settings::default();
        assert_eq!((s.get(Field::QuickHideWhat), s.get(Field::AutoHideIdle), s.get(Field::AutoShow)), (0, 0, 0));
        assert_eq!(s.get(Field::AutoHideIdleChoice), 1, "stored 0 reads as Immediately");
        assert_eq!(s.get(Field::AutoShowOnDrag), 1, "value 0 reads as ticked");
        assert!(s.is_on(Field::SwipeDragEdge) && s.is_on(Field::SwipeTouch) && s.is_on(Field::SwipeKeyboardWheel));
        assert!(s.is_on(Field::SwipeMiddleDrag) && !s.is_on(Field::SwipeDualDrag) && s.is_on(Field::SwipeShowIcons));
        assert!(!s.is_on(Field::SwipeEnabled) && s.is_on(Field::SwipeAnimate) && !s.is_on(Field::SwipeBounce));
        assert!(s.is_on(Field::TabsCreate) && !s.is_on(Field::TabsConfirm) && !s.is_on(Field::TabsMouseover),
            "TabsRequireConfirmation: Corral defaults to off");
        assert!(s.is_on(Field::TabsMouseoverDelay), "TabsSelectOnMouseoverDelay defaults to 1");
        assert!(s.is_on(Field::TabsColors), "TabsAlwaysShowColors defaults to true");
        assert!(!s.is_on(Field::BlendIcons));
        // Mouseover tabs: set writes 1/0.
        let mut t = Settings::default();
        t.set(Field::TabsMouseover, 1);
        assert_eq!(t.tabs_select_on_mouseover, 1);
        t.set(Field::TabsMouseover, 0);
        assert_eq!(t.tabs_select_on_mouseover, 0);
    }

    #[test]
    fn page_keyboard_and_wheel_gate_ignores_legacy_wheel_flag() {
        let mut s = Settings::default();
        s.swipe_switch_via_keyboard = true;
        s.swipe_switch_via_mouse_wheel = false;
        assert_eq!(s.get(Field::SwipeKeyboardWheel), 1, "Keyboard is read for both triggers");
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert!(!back.swipe_switch_via_mouse_wheel, "preserve the unused value for settings compatibility");
        assert_eq!(back.get(Field::SwipeKeyboardWheel), 1);
        s.swipe_switch_via_keyboard = false;
        s.swipe_switch_via_mouse_wheel = true;
        assert_eq!(s.get(Field::SwipeKeyboardWheel), 0);
        s.set(Field::SwipeKeyboardWheel, 1);
        assert!(s.swipe_switch_via_keyboard && s.swipe_switch_via_mouse_wheel, "the UI still writes both");
    }

    #[test]
    fn tab_confirmation_defaults_off_but_a_stored_value_wins() {
        let absent: Settings = serde_json::from_str("{}").unwrap();
        assert!(!absent.tabs_require_confirmation, "Corral default: off");
        assert!(!Settings::default().tabs_require_confirmation);
        let stored_on: Settings = serde_json::from_str(r#"{"TabsRequireConfirmation":true}"#).unwrap();
        assert!(stored_on.tabs_require_confirmation, "an explicit stored value is kept");
        let stored_off: Settings = serde_json::from_str(r#"{"TabsRequireConfirmation":false}"#).unwrap();
        assert!(!stored_off.tabs_require_confirmation);
        let back: Settings = serde_json::from_str(&serde_json::to_string(&stored_on).unwrap()).unwrap();
        assert!(back.tabs_require_confirmation);
    }
}

#[cfg(test)]
mod camouflage_storage_tests {
    use super::*;

    #[test]
    fn fresh_camouflage_defaults_match_spec_5_1() {
        let value: Settings = serde_json::from_str("{}").unwrap();
        assert!(!value.desktop_blend_icons);
        assert_eq!(value.desktop_blend_icon_opacity, 153);
        assert!(!value.desktop_blend_corral_backgrounds);
        assert!(value.desktop_blend_color_on_mouseover);
        assert!(!value.desktop_blend_color_on_active);
        assert!(!value.desktop_blend_uses_tint_icons_color);
        assert_eq!(value.desktop_tint_icons_color_source, "corral");
    }

    #[test]
    fn packed_legacy_migration_compares_stored_tint_with_corral_with_one_step_tolerance() {
        for (packed, expected) in [(0xff112233u32, "corral"), (0x9a122134, "corral"),
            (0x00132233, "custom"), (0x00112033, "custom"), (0x00112235, "custom")] {
            let input = serde_json::json!({"Color": 0x112233,
                "DesktopTintIconsColorUse": packed,
                "DesktopBlendIconOpacity": 178, "SnapPadding": 17});
            let value: Settings = serde_json::from_value(input).unwrap();
            assert_eq!(value.desktop_tint_icons_color_source, expected);
            assert_eq!(value.desktop_tint_icons_color, packed & 0x00ffffff);
            assert_eq!(value.desktop_blend_icon_opacity, 178);
            assert_eq!(value.snap_padding, 17);
            let saved = serde_json::to_value(&value).unwrap();
            assert_eq!(saved["DesktopTintIconsColorSource"], expected);
            assert!(saved.get("DesktopTintIconsColorUse").is_none());
            let back: Settings = serde_json::from_value(saved).unwrap();
            assert_eq!(back.snap_padding, 17);
            assert_eq!(back.desktop_tint_icons_color_source, expected);
        }
        let explicit: Settings = serde_json::from_value(serde_json::json!({
            "DesktopTintIconsColor":0x112233, "DesktopTintIconsColorUse":0xffabcdefu32
        })).unwrap();
        assert_eq!(explicit.desktop_tint_icons_color, 0x112233,
            "an explicit custom swatch survives migration");
    }

    #[test]
    fn auto_text_migration_preserves_explicit_custom_label_colour() {
        for (input, expected) in [(serde_json::json!({}), true),
            (serde_json::json!({"TextColor":0xffffff}), true),
            (serde_json::json!({"TextColor":0x112233}), false),
            (serde_json::json!({"TextColor":0x112233,"TextColorAuto":true}), true),
            (serde_json::json!({"TextColor":0xffffff,"TextColorAuto":false}), false)] {
            let value: Settings = serde_json::from_value(input).unwrap();
            assert_eq!(value.text_color_auto, expected);
        }
    }

    #[test]
    fn custom_source_equal_to_background_remains_custom() {
        let value = Settings { color:0x112233,
            desktop_tint_icons_color:0x112233,
            desktop_tint_icons_color_source:"custom".into(),
            desktop_tint_icons:true, ..Settings::default() };
        let shared = crate::color_state::Globals::from_settings(&value);
        assert_eq!(shared.tint_source, crate::color_state::TintSource::Custom);
        let mut back = Settings::default();
        shared.write_to(&mut back);
        assert_eq!(back.desktop_tint_icons_color_source, "custom");
        assert_eq!(back.desktop_tint_icons_color, 0x112233);
    }

    #[test]
    fn explicit_source_wins_and_mode_selection_preserves_custom_colour() {
        for source in ["corral", "custom"] {
            let mut value: Settings = serde_json::from_value(serde_json::json!({
                "DesktopTintIconsColorSource":source, "DesktopTintIconsColorUse": 7,
                "DesktopTintIconsColor": 0x010203, "Color": 0x112233,
                "CamouflageAlgorithm": 1})).unwrap(); // the retired switch is ignored
            assert_eq!(value.desktop_tint_icons_color_source, source);
            value.set(Field::TintColorMode, 0);
            assert_eq!(value.desktop_tint_icons_color_source, "corral");
            value.set(Field::TintColorMode, 1);
            assert_eq!(value.desktop_tint_icons_color_source, "custom");
            assert_eq!(value.desktop_tint_icons_color, 0x010203);
        }
    }
}
