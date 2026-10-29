//! Scan production Rust literals, including newly added UI source files.
//! Comments, test bodies and the explicit technical exceptions below are not UI copy.
use std::{fs, path::Path};

// A small Rust lexer keeps quotes in comments, raw strings and character literals
// from confusing the scan. `code` masks those regions for cfg(test) brace matching.
fn literals(source: &str) -> (Vec<(usize, String)>, Vec<u8>) {
    let b = source.as_bytes();
    let mut code = b.to_vec();
    let mut strings = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        if b[i..].starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' { i += 1; }
        } else if b[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < b.len() && depth > 0 {
                if b[i..].starts_with(b"/*") { depth += 1; i += 2; }
                else if b[i..].starts_with(b"*/") { depth -= 1; i += 2; }
                else { i += 1; }
            }
        } else {
            let mut r = i;
            if b[r] == b'b' && r + 1 < b.len() { r += 1; }
            let raw = b[r] == b'r';
            let mut quote = r + usize::from(raw);
            if raw { while quote < b.len() && b[quote] == b'#' { quote += 1; } }
            if quote < b.len() && b[quote] == b'"' {
                let hashes = if raw { quote - r - 1 } else { 0 };
                let body = quote + 1;
                i = body;
                while i < b.len() {
                    if !raw && b[i] == b'\\' { i = (i + 2).min(b.len()); continue; }
                    if b[i] == b'"' && b.get(i + 1..i + 1 + hashes).is_some_and(|s| s.iter().all(|c| *c == b'#')) { break; }
                    i += 1;
                }
                strings.push((start, source[body..i].to_owned()));
                i = (i + 1 + hashes).min(b.len());
            } else if b[i] == b'\'' {
                // Character literals only; a lifetime is left in the code stream.
                let mut end = i + 1;
                if b.get(end) == Some(&b'\\') {
                    end += 2;
                    if b.get(end - 1) == Some(&b'u') && b.get(end) == Some(&b'{') {
                        while end < b.len() && b[end] != b'}' { end += 1; }
                        end += 1;
                    }
                } else if end < b.len() {
                    end += source[end..].chars().next().unwrap().len_utf8();
                }
                if b.get(end) == Some(&b'\'') { i = end + 1; } else { i += 1; continue; }
            } else { i += 1; continue; }
        }
        code[start..i].fill(b' ');
    }
    (strings, code)
}

fn test_regions(code: &[u8]) -> Vec<(usize, usize)> {
    let marker = b"#[cfg(test)]";
    let mut regions = Vec::new();
    for start in 0..code.len().saturating_sub(marker.len()) {
        if !code[start..].starts_with(marker) { continue; }
        let Some(open) = (start + marker.len()..code.len()).find(|i| code[*i] == b'{') else { continue; };
        let mut depth = 1;
        let mut end = open + 1;
        while end < code.len() && depth > 0 {
            if code[end] == b'{' { depth += 1; }
            if code[end] == b'}' { depth -= 1; }
            end += 1;
        }
        regions.push((start, end));
    }
    regions
}

fn old_name(s: &str) -> bool {
    let text = s.replace("\\n", " ").replace("\\r", " ").replace("\\t", " ");
    text.split(|c: char| !c.is_alphanumeric() && c != '_').any(|word| {
        ["portal", "portals", "chameleon", "peek", "pages", "rollup", "rules"]
            .iter().any(|old| word.eq_ignore_ascii_case(old))
    }) || ["quick-hide", "quick hide", "roll-up", "roll up", "(tm)", "™", "®", "\\u{2122}", "\\u{ae}"]
        .iter().any(|old| text.to_lowercase().contains(old))
}

fn technical_exception(path: &str, text: &str, line: &str) -> bool {
    // Diagnostic log lines consumed by QA scripts; never drawn in Corral's UI.
    if line.contains("log(") || line.contains("eprintln!(") || line.contains("writeln!(f,") { return true; }
    match (path, text) {
        // Continuation lines inside existing multiline diagnostic calls.
        ("explorer_browser.rs", s) if s.starts_with("portal view:") || s.starts_with("portal list colours:") => true,
        ("app/peek.rs", s) if s.starts_with("peek:") => true,
        ("app/quick_hide_glue.rs", s) if s.starts_with("quick-hide: settled ") => true,
        // Persistent/import keys and Shell property names must remain compatible.
        ("settings.rs", "Rules") if line.contains("serde(rename") => true,
        ("settings/round_up_store.rs", "Rules") => true, // the old settings key the migration reads
        ("explorer_browser.rs", "Corral.Portal") => true,
        // CLI options, worker thread names and stable QA artifact names.
        ("main.rs", "--page" | "--probe-portal") => true,
        ("peek_keys.rs", "peek-keys-hook") => true,
        ("app/pages_glue.rs", "pages-frame.png" | "pages-current.png" | "pages-from.png" | "pages-to.png") => true,
        ("app/peek.rs", "portal-popup#{id}") => true,
        ("app/showdesk.rs", "PORTAL") => true, // QA role label
        _ => false,
    }
}

// Approved redesign copy uses "rules" descriptively within Round-up, never as the old page name.
// Keep this scoped to exact strings and files from settings-redesign-spec.md sections 7-8.
fn approved_redesign_copy(path: &str, text: &str) -> bool {
    matches!((path, text),
        ("settings_ui/content.rs", "Organize icons across separate desktop pages." | "Organize icons and Corrals across separate desktop pages and slide between them. Not related to Windows virtual desktops.") |
        ("settings_ui/rules_page.rs", "Your rules" | "Where a new icon goes if none of the rules below applies." | "rules point") |
        // round-up-spec 4 and 5: the live match line and the editor's note on date conditions.
        ("round_up/plan.rs", "{missing} rules point to a Corral that no longer exists." | "are taken by rules above.") |
        ("settings_ui/rules_dialog.rs", "Date rules are checked only when Round-up runs, not on a timer.") |
        ("settings_ui/rules_dialog.rs", "Rules higher in the list win when several match the same icon." | "No enabled rules"))
}

fn scan(dir: &Path, root: &Path, failures: &mut Vec<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() { scan(&path, root, failures); continue; }
        if path.extension().is_none_or(|e| e != "rs") { continue; }
        // A file that only holds #[cfg(test)] code (declared as `mod tests;`) is test code.
        if path.file_name().is_some_and(|n| n == "tests.rs") { continue; }
        let relative = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let source = fs::read_to_string(&path).unwrap();
        let (strings, code) = literals(&source);
        let tests = test_regions(&code);
        for (at, text) in strings {
            if tests.iter().any(|(start, end)| (*start..*end).contains(&at)) || !old_name(&text) { continue; }
            let start = source[..at].rfind('\n').map_or(0, |i| i + 1);
            let end = source[at..].find('\n').map_or(source.len(), |i| at + i);
            if !technical_exception(&relative, &text, &source[start..end]) && !approved_redesign_copy(&relative, &text) {
                failures.push(format!("{relative}:{}: {text}", source[..at].bytes().filter(|b| *b == b'\n').count() + 1));
            }
        }
    }
}

#[test]
fn production_ui_literals_use_corral_names() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut failures = Vec::new();
    scan(&root, &root, &mut failures);
    assert!(failures.is_empty(), "Old UI names outside the technical allowlist:\n{}", failures.join("\n"));
}

#[test]
fn scanner_catches_names_marks_raw_strings_and_escaped_line_breaks() {
    for text in ["portal", "Folder Portal", "Chameleon", "Peek", "Pages", "Quick-hide", "Rollup", "roll up", "Rules", "(TM)", "™", "\\u{2122}", "first\\nPeek"] {
        assert!(old_name(text), "missed {text}");
    }
    for text in ["Corral", "Folder Gate", "Camouflage", "Glance", "Ranges", "Clear view", "Fold", "Round-up", "CorralPortalOwner", "PeekKeepOpen"] {
        assert!(!old_name(text), "false positive {text}");
    }
    let source = "// \"Rollup\"\n/* \"Peek\" /* nested */ */ let a = r##\"Portal\"##; let c = '\"'; let b = \"Pages\\nPeek\"; #[cfg(test)] mod tests { let t = \"Rollup\"; }";
    let (strings, code) = literals(source);
    assert_eq!(strings.iter().map(|(_, s)| s.as_str()).collect::<Vec<_>>(), ["Portal", "Pages\\nPeek", "Rollup"]);
    let tests = test_regions(&code);
    assert_eq!(strings.iter().filter(|(at, _)| !tests.iter().any(|(a, b)| (*a..*b).contains(at))).count(), 2);
    assert!(!technical_exception("settings_ui/content.rs", "portal text", "lbl("));
    assert!(!technical_exception("settings_ui/content.rs", "Rules", "title: "));
    assert!(approved_redesign_copy("settings_ui/rules_page.rs", "Your rules"));
    assert!(approved_redesign_copy("settings_ui/rules_dialog.rs", "Rules higher in the list win when several match the same icon."));
    assert!(!approved_redesign_copy("settings_ui/content.rs", "Your rules"));
    assert!(approved_redesign_copy("settings_ui/content.rs", "Organize icons across separate desktop pages."));
    assert!(!approved_redesign_copy("settings_ui/content.rs", "Pages"));
    assert!(!approved_redesign_copy("settings_ui/rules_page.rs", "Rules"));
}
