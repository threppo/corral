//! Pure policy for Corral-painted icons.
//! Unresolved links retain their overlay; stored targets need no Resolve or existence probe.

pub const ENABLED: u32 = 0x1000_0000;
pub const WEB: u32 = 0x2;
pub const PROGRAM: u32 = 0x4;
pub const FOLDER: u32 = 0x8;
pub const DOCUMENT: u32 = 0x1000;
pub const DEFAULT_MASK: u32 = ENABLED | WEB | PROGRAM;
pub const LINK_LABEL: &str = "Choose which shortcut arrows to hide";
pub const CHOICES: [(&str, u32); 4] = [
    ("Web links", WEB), ("Program shortcuts", PROGRAM),
    ("Folder shortcuts", FOLDER), ("Document shortcuts", DOCUMENT),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkClass { NonLink, Web, Program, Folder, Document, Unknown }

fn extension(path: &str) -> Option<&str> {
    path.rsplit(['\\', '/']).next()?.rsplit_once('.').map(|(_, ext)| ext)
}

/// The adapter supplies only the saved .lnk target and directory attribute, never Resolve/Save.
/// .exe/.com/.bat/.cmd/.scr/.pif are program targets; other saved filesystem paths
/// are documents. Missing targets can still be classified from saved data, including UNC paths.
pub fn classify(name: &str, is_link: bool, target: Option<&str>, target_is_folder: bool) -> LinkClass {
    let ext = extension(name).unwrap_or("");
    if ext.eq_ignore_ascii_case("url") || ext.eq_ignore_ascii_case("website") { return LinkClass::Web; }
    if !ext.eq_ignore_ascii_case("lnk") { return if is_link { LinkClass::Unknown } else { LinkClass::NonLink }; }
    let Some(target) = target.filter(|t| !t.is_empty()) else { return LinkClass::Unknown };
    if target_is_folder { return LinkClass::Folder; }
    if target.get(..7).is_some_and(|s| s.eq_ignore_ascii_case("http://"))
        || target.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://")) { return LinkClass::Web; }
    // Non-filesystem / advertised links must not become document links by default.
    if target.starts_with("::") || !target.contains(['\\', '/']) { return LinkClass::Unknown; }
    match extension(target) {
        Some(ext) if ["exe", "com", "bat", "cmd", "scr", "pif"].iter().any(|e| ext.eq_ignore_ascii_case(e)) => LinkClass::Program,
        Some(_) => LinkClass::Document,
        None => LinkClass::Unknown,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Policy(pub u32);

impl Policy {
    pub fn hides(self, class: LinkClass) -> bool {
        let bit = match class {
            LinkClass::Web => WEB, LinkClass::Program => PROGRAM,
            LinkClass::Folder => FOLDER, LinkClass::Document => DOCUMENT,
            LinkClass::NonLink | LinkClass::Unknown => 0,
        };
        self.0 & ENABLED != 0 && self.0 & bit != 0
    }

    /// Legacy 1 is reset to 0 before enabling/XOR.
    pub fn toggle(self, bit: u32) -> Self {
        if !CHOICES.iter().any(|c| c.1 == bit) { return self; }
        Self(((if self.0 == 1 { 0 } else { self.0 }) | ENABLED) ^ bit)
    }

    /// Keep the Shell-selected status overlay unless it is positively identified as its link arrow.
    pub fn overlay(self, class: LinkClass, selected: u32, shortcut: Option<u32>) -> u32 {
        if self.hides(class) && shortcut == Some(selected) { 0 } else { selected }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey { pub name: String, pub size: u32, pub policy: Policy }

impl CacheKey {
    pub fn new(name: &str, size: u32, policy: Policy) -> Self {
        Self { name: name.to_lowercase(), size, policy }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn default_and_each_selection_bit() {
        assert_eq!(DEFAULT_MASK, 0x10000006);
        let classes = [LinkClass::Web, LinkClass::Program, LinkClass::Folder, LinkClass::Document];
        for (index, (_, bit)) in CHOICES.iter().enumerate() {
            for (other, class) in classes.iter().enumerate() {
                assert_eq!(Policy(ENABLED | bit).hides(*class), index == other);
                assert!(!Policy(*bit).hides(*class));
            }
        }
        assert!(Policy(DEFAULT_MASK).hides(LinkClass::Web));
        assert!(Policy(DEFAULT_MASK).hides(LinkClass::Program));
        assert!(!Policy(DEFAULT_MASK).hides(LinkClass::Folder));
        assert!(!Policy(DEFAULT_MASK).hides(LinkClass::Document));
        assert!(!Policy(u32::MAX).hides(LinkClass::NonLink));
        assert!(!Policy(u32::MAX).hides(LinkClass::Unknown));
        assert!(!Policy(1).hides(LinkClass::Web));
        assert!(!Policy(0x20000006).hides(LinkClass::Web)); // the web bit is bit 28, not any high bit
    }
    #[test]
    fn four_link_classes_and_nonlinks() {
        assert_eq!(classify("web.URL", true, None, false), LinkClass::Web);
        assert_eq!(classify("web.website", false, None, false), LinkClass::Web);
        assert_eq!(classify("app.lnk", true, Some(r"C:\app.EXE"), false), LinkClass::Program);
        assert_eq!(classify("folder.lnk", true, Some(r"C:\folder.ext"), true), LinkClass::Folder);
        assert_eq!(classify("doc.lnk", true, Some(r"C:\doc.pdf"), false), LinkClass::Document);
        assert_eq!(classify("app.exe", false, None, false), LinkClass::NonLink);
        assert_eq!(classify(r"C:\name.url\file.txt", false, None, false), LinkClass::NonLink);
    }
    #[test]
    fn broken_network_and_unknown_targets_are_read_only_inputs() {
        // A missing local target and unavailable server need no metadata/Resolve to classify.
        for path in [r"C:\missing\app.exe", r"\\offline\share\app.exe", r"%ProgramFiles%\app.exe"] {
            assert_eq!(classify("x.lnk", true, Some(path), false), LinkClass::Program);
        }
        assert_eq!(classify("x.lnk", true, Some(r"\\offline\share\folder"), true), LinkClass::Folder);
        assert_eq!(classify("x.lnk", true, Some(r"\\offline\share\doc.txt"), false), LinkClass::Document);
        for target in [None, Some(""), Some("::{virtual}"), Some("advertised"), Some(r"C:\unknown")] {
            assert_eq!(classify("x.lnk", true, target, false), LinkClass::Unknown);
        }
        assert_eq!(classify("other.link", true, None, false), LinkClass::Unknown);
    }
    #[test]
    fn status_overlays_survive_and_unknown_arrow_identity_fails_closed() {
        let p = Policy(DEFAULT_MASK);
        assert_eq!(p.overlay(LinkClass::Program, 1, Some(1)), 0);
        assert_eq!(p.overlay(LinkClass::Program, 3, Some(1)), 3);
        assert_eq!(p.overlay(LinkClass::Program, 1, None), 1);
        assert_eq!(p.overlay(LinkClass::Folder, 1, Some(1)), 1);
        assert_eq!(Policy(0).overlay(LinkClass::Web, 1, Some(1)), 1);
        assert_eq!(p.overlay(LinkClass::NonLink, 1, Some(1)), 1);
    }
    #[test]
    fn toggles_preserve_other_bits_and_legacy_value() {
        assert_eq!(Policy(1).toggle(FOLDER).0, ENABLED | FOLDER);
        let p = Policy(DEFAULT_MASK | 0x2000);
        assert_eq!(p.toggle(PROGRAM).toggle(PROGRAM), p);
        assert_eq!(Policy(WEB).toggle(FOLDER).0, ENABLED | WEB | FOLDER);
        assert_eq!(p.toggle(0x8000), p); // portal/cloud controls are deliberately not exposed
    }
    #[test]
    fn bitmap_key_changes_on_policy_size_or_item_and_matches_case() {
        let key = CacheKey::new(r"C:\A.lnk", 48, Policy(DEFAULT_MASK));
        assert_eq!(key, CacheKey::new(r"c:\a.LNK", 48, Policy(DEFAULT_MASK)));
        assert_ne!(key, CacheKey::new(r"C:\A.lnk", 48, Policy(DEFAULT_MASK).toggle(WEB)));
        assert_ne!(key, CacheKey::new(r"C:\A.lnk", 48, Policy(WEB | PROGRAM)));
        assert_ne!(key, CacheKey::new(r"C:\A.lnk", 32, Policy(DEFAULT_MASK)));
        assert_ne!(key, CacheKey::new(r"C:\B.lnk", 48, Policy(DEFAULT_MASK)));
    }
}
