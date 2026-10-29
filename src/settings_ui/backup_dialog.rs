//! Backups prompts composed from the shared Corral dialog kit. These adapters never open or
//! mutate the snapshot store; the settings window sends the accepted action as its existing request.

use super::Theme;
use crate::dialog::{self, Action, Checkbox, DialogResult, DialogSpec, Field};
use windows::Win32::Foundation::HWND;

pub const DELETE_TEXT: &str =
    "This backup is removed for good. Your current layout isn't affected.";
pub const RECAPTURE_TEXT: &str = "Replace this backup with the current layout?";
pub const RESTORE_NEVER: &str = "Don't ask again";
pub const RESTORE_FIRST: &str = "Back up the current layout first";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chosen {
    Recapture,
    Delete,
    Restore { backup_first: bool, never_ask: bool },
}
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RestoreOutcome {
    /// Retained for callers of the former combined inspect dialog. Rename is now a list menu action.
    pub renamed: Option<String>,
    pub chosen: Option<Chosen>,
}

pub fn name_spec(rename: bool, initial: &str) -> DialogSpec {
    let mut spec = DialogSpec::new(
        if rename {
            "Rename backup"
        } else {
            "Name this backup"
        },
        "",
        "Save",
    );
    spec.field = Some(Field {
        label: String::new(),
        value: initial.into(),
        hint: "Up to 255 characters.".into(),
        required: true,
        max_chars: 255,
    });
    spec
}
pub fn restore_spec(title: &str, taken: &str) -> DialogSpec {
    let mut spec = DialogSpec::new(
        format!("Restore \"{title}\"?"),
        format!(
            "Your Corrals go back to how they were {taken}. Files on the desktop are not moved or deleted."
        ),
        "Restore",
    );
    spec.checks = vec![
        Checkbox {
            label: RESTORE_FIRST.into(),
            checked: true,
        },
        Checkbox {
            label: RESTORE_NEVER.into(),
            checked: false,
        },
    ];
    spec
}
pub fn delete_spec(title: &str) -> DialogSpec {
    let mut spec = DialogSpec::new(format!("Delete \"{title}\"?"), DELETE_TEXT, "Delete");
    spec.primary.danger = true;
    spec
}
pub fn update_spec(title: &str) -> DialogSpec {
    DialogSpec::new(format!("Update \"{title}\"?"), RECAPTURE_TEXT, "Update")
}

/// Cancelling never persists either checkbox. On acceptance the booleans are independent.
pub fn restore_outcome(result: &DialogResult) -> Option<Chosen> {
    if result.action != Action::Primary {
        return None;
    }
    Some(Chosen::Restore {
        backup_first: result.checks.first().copied().unwrap_or(true),
        never_ask: result.checks.get(1).copied().unwrap_or(false),
    })
}
pub fn restore_dialog(
    owner: HWND,
    theme: &Theme,
    _dpi: u32,
    title: &str,
    taken: &str,
    skip_prompt: bool,
) -> RestoreOutcome {
    // The parent normally handles a remembered tip before opening a dialog. Preserve the old
    // wrapper's skip semantics for any remaining callers: legacy tip alone meant restore only.
    let chosen = if skip_prompt {
        Some(Chosen::Restore {
            backup_first: false,
            never_ask: false,
        })
    } else {
        restore_outcome(&dialog::show(
            owner,
            theme.light,
            restore_spec(title, taken),
        ))
    };
    RestoreOutcome {
        renamed: None,
        chosen,
    }
}
pub fn name_dialog(
    owner: HWND,
    theme: &Theme,
    _dpi: u32,
    caption: &str,
    initial: &str,
) -> Option<String> {
    let result = dialog::show(
        owner,
        theme.light,
        name_spec(caption.to_lowercase().contains("rename"), initial),
    );
    (result.action == Action::Primary).then_some(result.text)
}
pub fn confirm_delete(owner: HWND, theme: &Theme, title: &str) -> bool {
    dialog::show(owner, theme.light, delete_spec(title)).action == Action::Primary
}
pub fn confirm_update(owner: HWND, theme: &Theme, title: &str) -> bool {
    dialog::show(owner, theme.light, update_spec(title)).action == Action::Primary
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restored_checkboxes_are_independent_and_cancel_is_inert() {
        for backup_first in [false, true] {
            for never_ask in [false, true] {
                let mut result = DialogResult {
                    action: Action::Primary,
                    checks: vec![backup_first, never_ask],
                    text: String::new(),
                };
                assert_eq!(
                    restore_outcome(&result),
                    Some(Chosen::Restore {
                        backup_first,
                        never_ask
                    })
                );
                result.action = Action::Secondary;
                assert_eq!(restore_outcome(&result), None);
                result.action = Action::Closed;
                assert_eq!(restore_outcome(&result), None);
            }
        }
    }
    #[test]
    fn backup_prompt_specs_use_approved_copy_and_validation() {
        let restore = restore_spec("Work setup", "2026-10-18 09:00");
        assert_eq!(restore.title, "Restore \"Work setup\"?");
        assert_eq!(restore.width, 440.0);
        assert!(restore.checks[0].checked);
        assert!(!restore.checks[1].checked);
        assert!(delete_spec("A").primary.danger);
        assert!(!update_spec("A").primary.danger);
        for rename in [true, false] {
            let name = name_spec(rename, "Current");
            let field = name.field.unwrap();
            assert!(field.required);
            assert_eq!(field.max_chars, 255);
            assert_eq!(field.hint, "Up to 255 characters.");
            assert_eq!(name.primary.label, "Save");
        }
        assert_eq!(
            DELETE_TEXT,
            "This backup is removed for good. Your current layout isn't affected."
        );
        assert_eq!(
            RECAPTURE_TEXT,
            "Replace this backup with the current layout?"
        );
    }
    #[test]
    fn backup_dialog_goldens() -> windows::core::Result<()> {
        use crate::render::Surface;
        let output = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/qa/settings-redesign/u4");
        std::fs::create_dir_all(&output).unwrap();
        let invalid = name_spec(false, "");
        let specs = [
            (
                "restore-backup",
                restore_spec("Daily backup", "today at 09:00"),
            ),
            ("rename-backup", name_spec(true, "Work setup, two monitors")),
            ("name-backup", name_spec(false, "2026-10-18 12:40")),
            ("name-backup-error", invalid),
            ("delete-backup", delete_spec("Before reorganizing")),
            ("update-backup", update_spec("Work setup, two monitors")),
        ];
        for light in [true, false] {
            for dpi in [96u32, 120, 144, 240] {
                for (name, spec) in &specs {
                    let geometry = dialog::layout(spec, spec.width);
                    let scale = dpi as f32 / 96.0;
                    let mut surface = Surface::new(
                        (geometry.width * scale).ceil() as u32,
                        (geometry.height * scale).ceil() as u32,
                    )?;
                    surface.draw(|ctx| {
                        if *name == "name-backup-error" {
                            dialog::draw_spec_state(
                                ctx,
                                spec,
                                dpi,
                                light,
                                Some(super::super::backups::NAME_EMPTY),
                                Some(dialog::Focus::Field),
                                true,
                            )
                        } else if spec.field.is_some() {
                            // Match the initial native EDIT focus shown in the approved Figma frame.
                            dialog::draw_spec_state(
                                ctx,
                                spec,
                                dpi,
                                light,
                                None,
                                Some(dialog::Focus::Field),
                                false,
                            )
                        } else {
                            dialog::draw_spec(ctx, spec, dpi, light)
                        }
                    })?;
                    surface.save_png(&output.join(format!(
                        "{name}-{}-{dpi}.png",
                        if light { "light" } else { "dark" }
                    )))?;
                }
            }
        }
        Ok(())
    }
}
