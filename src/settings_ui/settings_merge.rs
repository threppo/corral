//! Settings-window edits, compared with the last copy the window read or saved.

use crate::settings::{self, Settings};
use serde_json::Value;

/// Top-level keys are the unit of an edit, including keys added or removed by serde's
/// `skip_serializing_if`. Start with latest so untouched values are copied exactly.
fn merge_values(base: &Value, edited: &Value, latest: &Value) -> Value {
    let mut merged = latest.as_object().expect("settings object").clone();
    let base = base.as_object().expect("settings object");
    let edited = edited.as_object().expect("settings object");
    for key in base.keys().chain(edited.keys()) {
        if edited.get(key) != base.get(key) {
            match edited.get(key) {
                Some(value) => {
                    merged.insert(key.clone(), value.clone());
                }
                None => {
                    merged.remove(key);
                }
            }
        }
    }
    Value::Object(merged)
}

/// Pure three-way merge. Serialization uses the stored (renamed) keys, never Field
/// bindings. Unknown keys still follow Settings' existing load/save policy: ignored.
pub(super) fn merge(
    base: &Settings,
    edited: &Settings,
    latest: &Settings,
) -> serde_json::Result<Settings> {
    let base = serde_json::to_value(base)?;
    let edits = serde_json::to_value(edited)?;
    let stored = serde_json::to_value(latest)?;
    let mut merged: Settings = serde_json::from_value(merge_values(&base, &edits, &stored))?;

    // These legacy values are readable but never stored. A serde round trip must not
    // reset their in-memory values just because the serializer intentionally omits them.
    merged.desktop_tint_icons_color_use = latest.desktop_tint_icons_color_use;

    // Editing any rule replaces the whole RoundUp key because the rules are an ordered list.
    // Round-up also owns skipped migration/pause flags and verbatim raw blocks. Keep
    // those with the side selected for their stored key; do not unpause a newer block
    // or lose the requirement to make migration backups.
    let round_up = if edits.get("RoundUp") != base.get("RoundUp") {
        edited
    } else {
        latest
    };
    merged.round_up = round_up.round_up.clone();
    merged.round_up_keep.block = round_up.round_up_keep.block.clone();
    let rules = if edits.get("Rules") != base.get("Rules") {
        edited
    } else {
        latest
    };
    merged.round_up_keep.legacy_rules = rules.round_up_keep.legacy_rules.clone();

    // Preserve the existing ownership/nonce policy for these protocol fields.
    settings::style_request::merge_protocol(&mut merged, latest);
    merged.desk_setup_request = latest.desk_setup_request.clone();
    merged.desk_setup_result = latest.desk_setup_result.clone();
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn engine_quick_hide_and_unrelated_user_edit_both_survive() {
        let base = Settings::default();
        let mut edited = base.clone();
        edited.snap_enabled = !base.snap_enabled;
        let mut latest = base.clone();
        latest.quick_hide_active = !base.quick_hide_active;
        let merged = merge(&base, &edited, &latest).unwrap();
        assert_eq!(merged.quick_hide_active, latest.quick_hide_active);
        assert_eq!(merged.snap_enabled, edited.snap_enabled);
    }

    #[test]
    fn engine_taskbar_close_and_unrelated_user_edit_both_survive() {
        let base = Settings {
            show_in_taskbar: true,
            ..Settings::default()
        };
        let mut edited = base.clone();
        edited.snap_padding = 6;
        let mut latest = base.clone();
        latest.show_in_taskbar = false;
        let merged = merge(&base, &edited, &latest).unwrap();
        assert!(!merged.show_in_taskbar);
        assert_eq!(merged.snap_padding, 6);
    }

    #[test]
    fn user_edit_wins_a_conflict() {
        let base = Settings {
            tabs_style: 0,
            ..Settings::default()
        };
        let edited = Settings {
            tabs_style: 2,
            ..base.clone()
        };
        let latest = Settings {
            tabs_style: 3,
            ..base.clone()
        };
        assert_eq!(merge(&base, &edited, &latest).unwrap().tabs_style, 2);
    }

    #[test]
    fn no_edits_keeps_every_latest_stored_value_and_skipped_metadata() {
        let base = Settings::default();
        let mut latest = base.clone();
        latest.quick_hide_active = !base.quick_hide_active;
        latest.quick_hide_exclusions = Some(" 1,02, 3 ".into());
        latest.background_blur_radius = 7.25;
        latest.desktop_tint_icons_color_use = 123;
        latest.round_up.paused_reason = Some("migration backup unavailable".into());
        latest.round_up.migration_pending = true;
        latest.tips.insert("kept".into(), 17);
        let merged = merge(&base, &base, &latest).unwrap();
        assert_eq!(
            serde_json::to_value(&merged).unwrap(),
            serde_json::to_value(&latest).unwrap()
        );
        assert_eq!(merged, latest);
    }

    #[test]
    fn optional_keys_follow_presence_and_the_whole_top_level_value_is_preserved() {
        let base = json!({"removed": [1], "untouched": {"x": 1}, "latest_removes": 2});
        let edited = json!({"added": null, "untouched": {"x": 1}, "latest_removes": 2});
        let latest =
            json!({"removed": [2], "untouched": {"x": 2, "new": [3, null]}, "latest_only": 9});
        assert_eq!(
            merge_values(&base, &edited, &latest),
            json!({
                "added": null, "untouched": {"x": 2, "new": [3, null]}, "latest_only": 9
            })
        );
        let base = Settings::default();
        let mut edited = base.clone();
        edited.snapshot_request = Some(crate::settings_ui::backups::new_request(
            5,
            crate::snapshot::Action::Capture {
                title: Some("QA".into()),
            },
        ));
        let added = merge(&base, &edited, &base).unwrap();
        assert_eq!(added.snapshot_request, edited.snapshot_request);
        assert!(
            merge(&added, &base, &added)
                .unwrap()
                .snapshot_request
                .is_none()
        );
    }

    #[test]
    fn unknown_default_and_retired_keys_keep_the_existing_storage_policy() {
        let latest = settings::round_up_store::decode(r#"{"UnknownFutureKey":{"a":1},"UseTitlebarControlsOnAllTabs":false,"TabsDimNonSelectedTabOnMouseover":5,"DesktopTintIconsColorUse":23}"#).unwrap();
        let base = Settings::default();
        let merged = merge(&base, &base, &latest).unwrap();
        let saved: Value =
            serde_json::from_str(&settings::round_up_store::encode(&merged).unwrap()).unwrap();
        let expected: Value =
            serde_json::from_str(&settings::round_up_store::encode(&latest).unwrap()).unwrap();
        assert_eq!(saved, expected);
        for key in [
            "UnknownFutureKey",
            "UseTitlebarControlsOnAllTabs",
            "TabsDimNonSelectedTabOnMouseover",
            "DesktopTintIconsColorUse",
            "SnapshotRequest",
            "StyleRequest",
            "desk_setup_request",
        ] {
            assert!(saved.get(key).is_none(), "unexpected stored key: {key}");
        }
        assert!(saved.get("QuickHideActive").is_some());
        assert!(saved.get("quick_hide_active").is_none());
    }

    #[test]
    fn protected_round_up_and_legacy_rules_keep_their_exact_raw_values() {
        let base = Settings::default();
        let latest = settings::round_up_store::decode(r#"{"RoundUp": { "version" : 999, "future": [1, 2] }, "Rules": [ { "Data" : "unreadable", "extra" : true } ]}"#).unwrap();
        let edited = Settings {
            snap_padding: 6,
            ..base.clone()
        };
        let merged = merge(&base, &edited, &latest).unwrap();
        assert_eq!(merged.round_up_keep, latest.round_up_keep);
        assert_eq!(merged.round_up, latest.round_up);
        let saved = settings::round_up_store::encode(&merged).unwrap();
        for (key, raw) in [
            ("RoundUp", latest.round_up_keep.block),
            ("Rules", latest.round_up_keep.legacy_rules),
        ] {
            let span = settings::round_up_store::top_level_value(&saved, key).unwrap();
            assert_eq!(&saved[span], raw.unwrap());
        }
    }

    #[test]
    fn protocol_nonce_policy_and_disk_owned_desk_setup_fields_are_unchanged() {
        use settings::style_request::{Action, Request, RequestResult};
        let base = Settings::default();
        let mut edited = base.clone();
        let mut latest = base.clone();
        latest.style_request = Some(Request {
            nonce: 20,
            layout_path: "qa.json".into(),
            action: Action::ResetAll,
        });
        latest.style_result = Some(RequestResult {
            nonce: 19,
            ok: true,
            message: "kept".into(),
        });
        latest.desk_setup_result = Some(crate::desk_setup::RequestResult {
            nonce: 18,
            ok: true,
            message: "kept".into(),
        });
        latest.desk_setup_request = Some(crate::desk_setup::Request {
            nonce: 19,
            layout_path: "qa.json".into(),
            action: crate::desk_setup::Action::SwapMonitors,
        });
        for nonce in [10, 20, 30] {
            edited.style_request = Some(Request {
                nonce,
                layout_path: "qa.json".into(),
                action: Action::ResetAll,
            });
            edited.style_result = Some(RequestResult {
                nonce,
                ok: false,
                message: "edit".into(),
            });
            edited.desk_setup_result = Some(crate::desk_setup::RequestResult {
                nonce: 99,
                ok: false,
                message: "ignored".into(),
            });
            edited.desk_setup_request = Some(crate::desk_setup::Request {
                nonce: 99,
                layout_path: "qa.json".into(),
                action: crate::desk_setup::Action::SwapMonitors,
            });
            let mut expected = edited.clone();
            settings::style_request::merge_protocol(&mut expected, &latest);
            let merged = merge(&base, &edited, &latest).unwrap();
            assert_eq!(merged.style_request, expected.style_request);
            assert_eq!(merged.style_result, expected.style_result);
            assert_eq!(merged.desk_setup_request, latest.desk_setup_request);
            assert_eq!(merged.desk_setup_result, latest.desk_setup_result);
        }
        latest.style_request = None;
        latest.style_result = None;
        latest.desk_setup_request = None;
        latest.desk_setup_result = None;
        edited = base.clone();
        assert_eq!(merge(&base, &edited, &latest).unwrap(), latest);
    }
}
