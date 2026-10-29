//! Stop-sorting confirmation. The shared Corral frame owns rendering and keyboard interaction.
//! Stored-answer behavior remains at the caller: caption close discards the checkbox.
use crate::dialog::{self, Action, Checkbox, DialogResult, DialogSpec};
use windows::Win32::Foundation::HWND;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    Closed,
}
pub fn question(desktop: bool) -> String {
    if desktop {
        "You moved an icon on the desktop. Stop sorting on the desktop to arrange its icons by hand?".into()
    } else {
        "You moved an icon inside a sorted Corral. Stop sorting it so you can arrange its icons by hand?".into()
    }
}
pub fn spec(desktop: bool) -> DialogSpec {
    let mut spec = DialogSpec::new(
        if desktop {
            "Stop sorting the desktop?"
        } else {
            "Stop sorting this Corral?"
        },
        question(desktop),
        "Stop sorting",
    );
    spec.secondary = Some("Keep sorting".into());
    spec.checks
        .push(Checkbox::new("Always use this answer", false));
    spec
}
pub fn outcome(result: DialogResult) -> (Answer, bool) {
    let checked = result.checks.first().copied().unwrap_or(false);
    match result.action {
        Action::Primary => (Answer::Yes, checked),
        Action::Secondary => (Answer::No, checked),
        Action::Closed | Action::Left => (Answer::Closed, false),
    }
}
pub fn ask(desktop: bool) -> (Answer, bool) {
    ask_owned(HWND::default(), desktop)
}
/// Use the caller's popup owner for the app path; the standalone QA entry keeps `ask`.
pub fn ask_owned(owner: HWND, desktop: bool) -> (Answer, bool) {
    outcome(dialog::show(owner, dialog::system_light(), spec(desktop)))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_variant_preserves_existing_desktop_wording() {
        let d = question(true);
        assert!(d.contains("You moved an icon on the desktop."));
        assert!(d.contains("Stop sorting on the desktop to arrange its icons by hand?"));
        assert_eq!(
            question(false),
            "You moved an icon inside a sorted Corral. Stop sorting it so you can arrange its icons by hand?"
        );
    }
    #[test]
    fn accepted_no_and_escape_keep_checkbox_but_window_close_discards_it() {
        for checked in [false, true] {
            for (action, answer, stored) in [
                (Action::Primary, Answer::Yes, checked),
                (Action::Secondary, Answer::No, checked),
                (Action::Closed, Answer::Closed, false),
            ] {
                assert_eq!(
                    outcome(DialogResult {
                        action,
                        checks: vec![checked],
                        text: String::new()
                    }),
                    (answer, stored)
                );
            }
        }
    }
    #[test]
    fn stop_sorting_golden_renders() {
        let dir = std::path::Path::new("target/qa/settings-redesign/u4");
        std::fs::create_dir_all(dir).unwrap();
        for desktop in [false, true] {
            let spec = spec(desktop);
            for light in [false, true] {
                for dpi in [96, 120, 144, 240] {
                    let l = dialog::layout(&spec, 440.);
                    let s = dpi as f32 / 96.;
                    let mut surface = crate::render::Surface::new(
                        (l.width * s).round() as u32,
                        (l.height * s).round() as u32,
                    )
                    .unwrap();
                    surface
                        .draw(|ctx| dialog::draw_spec(ctx, &spec, dpi, light))
                        .unwrap();
                    surface
                        .save_png(&dir.join(format!(
                            "dialog-stop-sorting-{}-{}-{dpi}.png",
                            if desktop { "desktop" } else { "corral" },
                            if light { "light" } else { "dark" }
                        )))
                        .unwrap();
                }
            }
        }
    }
}
