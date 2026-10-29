//! "How to ..." help links of the settings pages.
//!
//! Each link opens a plain message box (never a web page). The wording is Corral's own: same facts and
//! steps, Corral names (Corral, Gate, Ranges, Clear view, Fold). Corral shows the box with the title
//! "Corral". No URL is ever opened.

/// One help link: the label shown on the page and the box content.
#[derive(Clone, Copy, Debug)]
pub struct Help {
    pub label: &'static str,
    pub title: &'static str,
    pub body: &'static str,
}

const fn h(label: &'static str, body: &'static str) -> Help {
    Help { label, title: "Corral", body }
}

pub const HELP: &[Help] = &[
    // Pages page -> "Ranges"
    h("How to use Ranges",
      "To change Range with the mouse, grab the left or right edge of the desktop and pull sideways. You can also hold both mouse buttons and drag from anywhere on the desktop, or drag with the middle button."),
    h("How to move icons and Corrals between Ranges",
      "To carry an icon or a Corral to another Range, drag it to the left or right edge of the screen and keep it there for a moment. The desktop flips to the neighbouring Range so you can drop the item there. You can add as many Ranges to the left and right as you like."),
    h("How to switch Ranges by touch",
      "On a touch screen, drag the desktop left or right with two fingers to change Range."),
    h("How to add a Range",
      "To start a new Range, drag an icon or a Corral to the edge of the screen and hold it there briefly. The desktop switches to an empty Range; drop the item to place it on it."),
    h("How to switch Ranges with shortcuts",
      "To change Range from the keyboard, click the desktop to give it focus, then press Alt together with the left or right arrow key. Holding Alt while turning the mouse wheel over the focused desktop works as well."),
    // Quick-hide page -> "Clear view"
    h("How to use",
      "To use Clear view, double-click an empty spot anywhere on the desktop and the icons vanish. Double-click again to bring them back."),
    h("How to keep a Corral visible",
      "To keep a Corral visible during Clear view, right-click an empty area inside it and choose the Clear view exclusion entry (under its View menu). The same can be done for any single icon on the desktop."),
    // Folder Portals page -> "Gates"
    h("How to create a Folder Gate",
      "A Folder Gate can be made in several ways:\n\n1. Draw a rectangle on the desktop and pick the Folder Gate entry from the menu that appears.\n\n2. Right-click a folder and use the entry that shows the folder on the desktop (under More options).\n\n3. Hold Alt while dropping a folder on the desktop."),
    h("How to remove a Folder Gate",
      "Remove a Folder Gate like any other Corral: point at it and click the \"x\" that appears. The folder itself is not deleted."),
    h("How to open a Gate in Explorer",
      "To open the folder of a Folder Gate in a window, double-click empty space inside the Corral, or hold Control while double-clicking its titlebar."),
    h("How to return to the Gate root",
      "To jump straight back to the root folder of a Folder Gate, hold Shift while clicking the Up icon, or right-click the Up icon and choose \"Navigate to root\"."),
    h("How to choose a Gate view",
      "To change how a Folder Gate looks, click the menu icon in the top left corner of the Corral (it shows while the mouse is over it). The View menu offers many styles, such as Details and List."),
    h("How to resize Gate icons",
      "Change the icon size of a Folder Gate by choosing a size in its View menu, or by clicking inside it and turning the mouse wheel while holding Control."),
    h("How to sort Gate icons",
      "Icons in a Folder Gate cannot be placed by hand. To change their order, right-click an empty part of the Corral, open Sort by and pick an option."),
    h("How to toggle the Gate details header",
      "To hide the header of a details view, click the menu icon in the top left corner of the Corral (it shows while the mouse is over it) and choose \"Hide details header\" from the View menu."),
];

/// The help entry behind a page link label, if the label is a help link.
pub fn lookup(label: &str) -> Option<&'static Help> {
    HELP.iter().find(|help| help.label == label)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use crate::settings_ui::{content::content, content::RowKind, Page};

    /// Every "How to ..." link on any page (and the two Quick-hide links) must open a text.
    #[test]
    fn every_help_link_label_maps_to_a_text() {
        let settings = Settings::default();
        let mut seen = 0;
        for page in Page::ALL {
            let _ = &settings;
            for section in content(page).sections {
                for row in section.rows {
                    if row.kind != RowKind::Link { continue; }
                    let is_help = row.text.starts_with("How to ") || (page == Page::Hide && section.header == "Using Clear view");
                    if !is_help { continue; }
                    let help = lookup(row.text).unwrap_or_else(|| panic!("help link {:?} on {page:?} has no text", row.text));
                    assert!(help.body.len() > 40, "{}", help.label);
                    seen += 1;
                }
            }
        }
        let retained_gate_help = HELP.len();
        assert_eq!(seen + retained_gate_help, HELP.len(), "Folder Gate help now appears inline; the old lookup entries remain valid for other callers");
        assert_eq!(seen, 0, "Folder Gates, Clear view and Ranges help is now inline");
        for help in HELP {
            assert!(help.body.len() > 40, "retained text remains complete: {}", help.label);
            assert_eq!(lookup(help.label).map(|found| found.body), Some(help.body));
        }
    }

    #[test]
    fn labels_are_unique_and_text_uses_corral_names_without_marks() {
        let mut labels: Vec<_> = HELP.iter().map(|help| help.label).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), HELP.len());
        for help in HELP {
            for banned in ["Folder Portal", "Chameleon", "Peek", "Rollup", "\u{2122}", "\u{AE}", "http"] {
                assert!(!help.body.contains(banned), "{:?} appears in the text of {:?}", banned, help.label);
            }
            assert_eq!(help.title, "Corral");
        }
    }
}
