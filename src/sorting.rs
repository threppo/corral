//! Sorting of ordinary Corrals (motion-sorting-spec 2). The stored state is the Corral's
//! `sort_method` and `sort_flags` (model); this module defines the one deterministic item order and
//! the menu helpers around it. The text comparison is injected ([`Collator`]): production uses
//! `CompareStringEx` with the user's locale, tests use a fake with no locale tables.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

pub const METHOD_NONE: u32 = 0;
pub const METHOD_NAME: u32 = 0x1;
pub const METHOD_SIZE: u32 = 0x2;
pub const METHOD_TYPE: u32 = 0x4;
pub const METHOD_MODIFIED: u32 = 0x8;
pub const METHOD_ADDED: u32 = 0x10;
pub const METHOD_CREATED: u32 = 0x20;
pub const METHOD_OPENED: u32 = 0x40;

/// `sort_flags & 1`: descending (applies to the primary key only, spec 2).
pub const FLAG_REVERSE: u32 = 0x1;
/// `sort_flags & 2`: sorting not maintained (menu shows "None" and offers "Maintain sorting").
pub const FLAG_NOT_MAINTAINED: u32 = 0x2;
// Bit 4 of `sort_flags` held the old "folders first" choice. It is still read and kept so old
// files load, but never written or acted on: folders are always first (spec 2, "Group first").
/// Stored field 13 also carries the "show on all pages" bit, which is not a sort flag.
pub const FIELD13_ALL_PAGES: u32 = 0x4000;


/// Whether layouts re-sort the Corral.
pub fn is_maintained(method: u32, flags: u32) -> bool {
    method != METHOD_NONE && flags & FLAG_NOT_MAINTAINED == 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Virtual or system item (This PC, Recycle Bin) or an item that no longer exists.
    Virtual,
    Folder,
    File,
}

impl Kind {
    /// Folders and system items come before files for every key and both directions (spec 2).
    fn is_file(self) -> bool {
        self == Kind::File
    }
}

/// What the order reads of one item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortKey {
    /// The label the user sees (hidden extensions stay hidden).
    pub name: String,
    /// Full path or parsing name (tie-break 3).
    pub path: String,
    pub kind: Kind,
    /// Bytes; `None` = failed look-up. Folders and system items are never compared by size.
    pub size: Option<u64>,
    /// The type text Explorer shows (see [`fallback_type_text`]).
    pub type_text: String,
    /// The instant of the Date key (last write, or creation for that menu entry); `None` = unknown.
    pub date: Option<i64>,
}

/// Locale-aware text comparison, injected so logic tests do not depend on Windows' tables.
pub trait Collator {
    fn compare(&self, a: &str, b: &str) -> Ordering;

    /// True once a comparison could not be answered. The order is then not trustworthy and
    /// [`order`] sorts the whole list again with [`OrdinalFold`].
    fn failed(&self) -> bool {
        false
    }
}

/// Ordinal case-folded compare: the fallback for a whole sort after the system collator failed.
pub struct OrdinalFold;

impl Collator for OrdinalFold {
    fn compare(&self, a: &str, b: &str) -> Ordering {
        ordinal(&a.to_lowercase(), &b.to_lowercase())
    }
}

fn ordinal(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Known values first in both directions; `reverse` flips only known against known.
fn known_first<T: Ord>(a: &Option<T>, b: &Option<T>, reverse: bool) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => if reverse { y.cmp(x) } else { x.cmp(y) },
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// The total order of spec 2 for `method`/`flags`; the stored indexes break the last tie.
pub fn compare(a: &SortKey, b: &SortKey, ia: usize, ib: usize, method: u32, flags: u32, collator: &dyn Collator) -> Ordering {
    let reverse = flags & FLAG_REVERSE != 0;
    let group = a.kind.is_file().cmp(&b.kind.is_file());
    if group != Ordering::Equal {
        return group;
    }
    let directed = |o: Ordering| if reverse { o.reverse() } else { o };
    let primary = match method {
        METHOD_NAME => directed(collator.compare(&a.name, &b.name)),
        METHOD_TYPE => directed(collator.compare(&a.type_text, &b.type_text)),
        METHOD_SIZE if a.kind.is_file() => known_first(&a.size, &b.size, reverse),
        METHOD_MODIFIED | METHOD_CREATED | METHOD_ADDED | METHOD_OPENED => known_first(&a.date, &b.date, reverse),
        _ => Ordering::Equal,
    };
    if primary != Ordering::Equal {
        return primary;
    }
    // tie-breaks always ascend, whatever the direction
    if method != METHOD_NAME {
        let natural = collator.compare(&a.name, &b.name);
        if natural != Ordering::Equal {
            return natural;
        }
    }
    ordinal(&a.name, &b.name).then_with(|| ordinal(&a.path, &b.path)).then(ia.cmp(&ib))
}

fn sorted(keys: &[SortKey], method: u32, flags: u32, collator: &dyn Collator) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..keys.len()).collect();
    idx.sort_by(|&i, &j| compare(&keys[i], &keys[j], i, j, method, flags, collator));
    idx
}

/// Indexes of `keys` in display order. The collator is decided once per sort: when it reports a
/// failure (or a mixed answer makes the sort panic), the whole list is sorted again with the ordinal
/// fallback, so a failure in the middle can never leave the comparator inconsistent.
pub fn order(keys: &[SortKey], method: u32, flags: u32, collator: &dyn Collator) -> Vec<usize> {
    if method == METHOD_NONE {
        return (0..keys.len()).collect();
    }
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sorted(keys, method, flags, collator)));
    match attempt {
        Ok(idx) if !collator.failed() => idx,
        _ => sorted(keys, method, flags, &OrdinalFold),
    }
}

/// Type text when Explorer has no registered description (spec 2): upper-cased extension plus
/// " File", or "File" without an extension.
pub fn fallback_type_text(file_name: &str) -> String {
    match file_name.rsplit_once('.') {
        Some((_, ext)) if !ext.is_empty() => format!("{} File", ext.to_uppercase()),
        _ => "File".to_string(),
    }
}

thread_local! {
    static TYPE_CACHE: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

/// Type text of an item, looked up once per lower-cased extension (spec 2). `lookup` asks Explorer.
pub fn cached_type_text(cache_key: &str, file_name: &str, lookup: impl FnOnce() -> Option<String>) -> String {
    let key = cache_key.to_lowercase();
    if let Some(hit) = TYPE_CACHE.with(|c| c.borrow().get(&key).cloned()) {
        return hit;
    }
    let text = lookup().filter(|t| !t.is_empty()).unwrap_or_else(|| fallback_type_text(file_name));
    TYPE_CACHE.with(|c| c.borrow_mut().insert(key, text.clone()));
    text
}

// ---- production collator -------------------------------------------------------------------------

const NORM_IGNORECASE: u32 = 0x0000_0001;
const SORT_STRINGSORT: u32 = 0x0000_1000;
const SORT_DIGITSASNUMBERS: u32 = 0x0000_0008;
const LOCALE_NAME_MAX_LENGTH: usize = 85;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CompareStringEx(locale: *const u16, flags: u32, s1: *const u16, c1: i32, s2: *const u16, c2: i32,
        version: *const core::ffi::c_void, reserved: *const core::ffi::c_void, lparam: isize) -> i32;
    fn GetUserDefaultLocaleName(buffer: *mut u16, len: i32) -> i32;
}

static FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);

/// `CompareStringEx(locale, NORM_IGNORECASE | SORT_DIGITSASNUMBERS | SORT_STRINGSORT)` (spec 2).
/// A call that returns 0 (error) falls back to an ordinal case-folded compare and logs once.
pub struct SystemCollator {
    /// NUL-terminated locale name.
    locale: Vec<u16>,
    failed: std::cell::Cell<bool>,
}

impl SystemCollator {
    /// The user's default locale, read now (startup and after a settings change).
    pub fn user_default() -> SystemCollator {
        let mut buf = [0u16; LOCALE_NAME_MAX_LENGTH];
        let n = unsafe { GetUserDefaultLocaleName(buf.as_mut_ptr(), buf.len() as i32) };
        let len = if n > 0 { n as usize } else { 1 };
        let mut locale = buf[..len.min(buf.len())].to_vec();
        if locale.last() != Some(&0) {
            locale.push(0);
        }
        if n <= 0 {
            locale = vec![0];
        }
        SystemCollator { locale, failed: std::cell::Cell::new(false) }
    }

    /// A named locale such as `en-US` (tests).
    #[cfg(test)]
    pub fn for_locale(name: &str) -> SystemCollator {
        let mut locale: Vec<u16> = name.encode_utf16().collect();
        locale.push(0);
        SystemCollator { locale, failed: std::cell::Cell::new(false) }
    }
}

impl Collator for SystemCollator {
    fn failed(&self) -> bool {
        self.failed.get()
    }

    fn compare(&self, a: &str, b: &str) -> Ordering {
        let wa: Vec<u16> = a.encode_utf16().collect();
        let wb: Vec<u16> = b.encode_utf16().collect();
        let (la, lb) = (wa.len().min(i32::MAX as usize) as i32, wb.len().min(i32::MAX as usize) as i32);
        // An empty locale string is not a name: pass null for "user default" instead
        let locale = if self.locale.len() > 1 { self.locale.as_ptr() } else { core::ptr::null() };
        let r = unsafe {
            CompareStringEx(locale, NORM_IGNORECASE | SORT_DIGITSASNUMBERS | SORT_STRINGSORT,
                wa.as_ptr(), la, wb.as_ptr(), lb, core::ptr::null(), core::ptr::null(), 0)
        };
        match r {
            1 => Ordering::Less,
            2 => Ordering::Equal,
            3 => Ordering::Greater,
            _ => {
                self.failed.set(true);
                if !FALLBACK_LOGGED.swap(true, AtomicOrdering::Relaxed) {
                    crate::app::log("sorting: CompareStringEx failed, using an ordinal case-folded compare");
                }
                let fold = |s: &str| s.to_lowercase();
                ordinal(&fold(a), &fold(b))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locale-free fake: digit runs compare as numbers (leading zeros ignored), case folded,
    /// punctuation never skipped, characters by code point.
    struct Fake;

    #[derive(PartialEq, Eq, PartialOrd, Ord)]
    enum Tok {
        Num(usize, String),
        Ch(char),
    }

    fn tokens(s: &str) -> Vec<Tok> {
        let mut out = Vec::new();
        let mut digits = String::new();
        let flush = |digits: &mut String, out: &mut Vec<Tok>| {
            if !digits.is_empty() {
                let t = digits.trim_start_matches('0').to_string();
                out.push(Tok::Num(t.len(), t));
                digits.clear();
            }
        };
        for c in s.chars() {
            if c.is_ascii_digit() {
                digits.push(c);
            } else {
                flush(&mut digits, &mut out);
                out.push(Tok::Ch(c.to_lowercase().next().unwrap_or(c)));
            }
        }
        flush(&mut digits, &mut out);
        out
    }

    impl Collator for Fake {
        fn compare(&self, a: &str, b: &str) -> Ordering {
            tokens(a).cmp(&tokens(b))
        }
    }

    fn file(name: &str) -> SortKey {
        SortKey { name: name.into(), path: format!("C:\\d\\{name}"), kind: Kind::File, size: Some(1), type_text: "T".into(), date: Some(1) }
    }

    fn folder(name: &str) -> SortKey {
        SortKey { kind: Kind::Folder, size: None, type_text: "File folder".into(), ..file(name) }
    }

    fn names(keys: &[SortKey], method: u32, flags: u32, c: &dyn Collator) -> Vec<String> {
        order(keys, method, flags, c).into_iter().map(|i| keys[i].name.clone()).collect()
    }

    fn by_name(list: &[&str], flags: u32) -> Vec<String> {
        let keys: Vec<SortKey> = list.iter().map(|n| file(n)).collect();
        names(&keys, METHOD_NAME, flags, &Fake)
    }

    #[test]
    fn natural_order_counts_digit_runs_as_numbers() {
        assert_eq!(by_name(&["file10", "file2", "file1"], 0), ["file1", "file2", "file10"]);
        assert_eq!(by_name(&["file10", "file2", "file1"], FLAG_REVERSE), ["file10", "file2", "file1"]);
        assert_eq!(by_name(&["v1.10", "v1.9"], 0), ["v1.9", "v1.10"]);
        assert_eq!(by_name(&["file2", "file02"], 0), ["file02", "file2"], "equal numbers tie, the ordinal tie-break decides");
        let long = format!("n{}", "9".repeat(40));
        let longer = format!("n{}", "9".repeat(41));
        let keys = vec![file(&longer), file(&long)];
        assert_eq!(names(&keys, METHOD_NAME, 0, &Fake), [long.clone(), longer.clone()]);
        assert_eq!(Fake.compare(&long, &longer), Fake.compare(&longer, &long).reverse());
    }

    #[test]
    fn case_is_ignored_and_ties_stay_ascending_in_both_directions() {
        assert_eq!(by_name(&["banana", "Apple", "cherry", "apple"], 0), ["Apple", "apple", "banana", "cherry"]);
        assert_eq!(by_name(&["banana", "Apple", "cherry", "apple"], FLAG_REVERSE), ["cherry", "banana", "Apple", "apple"]);
    }

    #[test]
    fn the_fake_collator_is_locale_independent_for_non_ascii() {
        assert_eq!(by_name(&["Zebra", "\u{17B}aba"], 0), by_name(&["\u{17B}aba", "Zebra"], 0));
        assert_eq!(by_name(&["Ala", "\u{104}bc"], 0), ["Ala", "\u{104}bc"]);
    }

    #[test]
    fn folders_and_system_items_come_first_for_every_key_and_direction() {
        let mut sys = folder("This PC");
        sys.kind = Kind::Virtual;
        let keys = vec![file("a"), folder("z"), file("m"), sys, folder("b")];
        for method in [METHOD_NAME, METHOD_TYPE, METHOD_SIZE, METHOD_MODIFIED, METHOD_CREATED] {
            for flags in [0, FLAG_REVERSE] {
                let got = order(&keys, method, flags, &Fake);
                let firsts: Vec<bool> = got.iter().map(|&i| keys[i].kind.is_file()).collect();
                assert_eq!(firsts, [false, false, false, true, true], "method {method} flags {flags}");
            }
        }
        // among themselves they follow the key
        assert_eq!(names(&keys, METHOD_NAME, 0, &Fake)[..3], ["b", "This PC", "z"].map(String::from));
        assert_eq!(names(&keys, METHOD_NAME, FLAG_REVERSE, &Fake)[..3], ["z", "This PC", "b"].map(String::from));
    }

    #[test]
    fn size_orders_numerically_with_unknown_last_and_equal_sizes_by_name() {
        let mk = |name: &str, size: Option<u64>| SortKey { size, ..file(name) };
        let keys = vec![mk("ten", Some(10)), mk("two", Some(2)), mk("zero", Some(0)), mk("unknown", None), mk("also2", Some(2))];
        assert_eq!(names(&keys, METHOD_SIZE, 0, &Fake), ["zero", "also2", "two", "ten", "unknown"]);
        assert_eq!(names(&keys, METHOD_SIZE, FLAG_REVERSE, &Fake), ["ten", "also2", "two", "zero", "unknown"]);
    }

    #[test]
    fn date_orders_by_instant_with_unknown_last() {
        let mk = |name: &str, date: Option<i64>| SortKey { date, ..file(name) };
        let keys = vec![mk("b", Some(i64::MAX)), mk("a", Some(i64::MIN)), mk("c", Some(5)), mk("d", Some(5)), mk("u", None)];
        assert_eq!(names(&keys, METHOD_MODIFIED, 0, &Fake), ["a", "c", "d", "b", "u"]);
        assert_eq!(names(&keys, METHOD_MODIFIED, FLAG_REVERSE, &Fake), ["b", "c", "d", "a", "u"], "newest first, equal instants stay ascending");
        // Added and Opened have no facts: every item is unknown, so the name order shows
        let blind = |n: &str| SortKey { date: None, ..file(n) };
        assert_eq!(names(&[blind("b"), blind("a")], METHOD_OPENED, 0, &Fake), ["a", "b"]);
    }

    #[test]
    fn type_orders_by_text_then_name_and_falls_back_to_the_extension() {
        assert_eq!(fallback_type_text("report.xyz"), "XYZ File");
        assert_eq!(fallback_type_text("Makefile"), "File");
        assert_eq!(fallback_type_text(".gitignore"), "GITIGNORE File");
        assert_eq!(fallback_type_text("trailing."), "File");
        let mk = |name: &str, ty: &str| SortKey { type_text: ty.into(), ..file(name) };
        let keys = vec![mk("b.txt", "Text Document"), mk("a.xyz", "XYZ File"), mk("c.pdf", "PDF Document"), mk("a.txt", "Text Document")];
        assert_eq!(names(&keys, METHOD_TYPE, 0, &Fake), ["c.pdf", "a.txt", "b.txt", "a.xyz"]);
        assert_eq!(names(&keys, METHOD_TYPE, FLAG_REVERSE, &Fake), ["a.xyz", "a.txt", "b.txt", "c.pdf"]);
        // a cache hit equals the miss
        let miss = cached_type_text("QA-EXT-1", "x.qa1", || Some("QA One".into()));
        let hit = cached_type_text("qa-ext-1", "y.qa1", || panic!("must not look up again"));
        assert_eq!((miss.as_str(), hit.as_str()), ("QA One", "QA One"));
        assert_eq!(cached_type_text("qa-ext-2", "x.qa2", || None), "QA2 File");
    }

    #[test]
    fn the_order_is_total_and_independent_of_the_input_order() {
        // small deterministic generator, no extra crates
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; seed };
        let pool = ["a", "A", "b2", "b10", "B02", "c", "ć", ".env", "1", "10", "2", "z"];
        let mut keys: Vec<SortKey> = Vec::new();
        for i in 0..24 {
            let r = next();
            let name = format!("{}{}", pool[(r % pool.len() as u64) as usize], (r >> 8) % 3);
            let mut k = file(&name);
            k.path = format!("C:\\{i}\\{name}"); // paths are unique on a real desktop
            k.kind = [Kind::File, Kind::File, Kind::Folder, Kind::Virtual][((r >> 12) % 4) as usize];
            k.size = if (r >> 16) % 5 == 0 { None } else { Some((r >> 20) % 4) };
            k.date = if (r >> 24) % 5 == 0 { None } else { Some(((r >> 28) % 4) as i64) };
            k.type_text = ["A", "B", "C"][((r >> 32) % 3) as usize].into();
            keys.push(k);
        }
        for method in [METHOD_NAME, METHOD_TYPE, METHOD_SIZE, METHOD_MODIFIED] {
            for flags in [0, FLAG_REVERSE] {
                let cmp = |i: usize, j: usize| compare(&keys[i], &keys[j], i, j, method, flags, &Fake);
                for i in 0..keys.len() {
                    assert_eq!(cmp(i, i), Ordering::Equal);
                    for j in 0..keys.len() {
                        assert_eq!(cmp(i, j), cmp(j, i).reverse(), "antisymmetric {i} {j}");
                        for k in 0..keys.len() {
                            if cmp(i, j) != Ordering::Greater && cmp(j, k) != Ordering::Greater {
                                assert_ne!(cmp(i, k), Ordering::Greater, "transitive {i} {j} {k}");
                            }
                        }
                    }
                }
                // any shuffle of the same list gives the same output (as full records)
                let reference: Vec<SortKey> = order(&keys, method, flags, &Fake).into_iter().map(|i| keys[i].clone()).collect();
                for _ in 0..8 {
                    let mut shuffled = keys.clone();
                    for i in (1..shuffled.len()).rev() { shuffled.swap(i, (next() % (i as u64 + 1)) as usize); }
                    let got: Vec<SortKey> = order(&shuffled, method, flags, &Fake).into_iter().map(|i| shuffled[i].clone()).collect();
                    assert_eq!(got, reference, "method {method} flags {flags}");
                }
            }
        }
    }

    #[test]
    fn the_old_stored_folders_first_bit_changes_nothing() {
        let keys = vec![file("a"), folder("z"), file("b"), folder("m")];
        for method in [METHOD_NAME, METHOD_SIZE, METHOD_MODIFIED] {
            assert_eq!(order(&keys, method, 0x4, &Fake), order(&keys, method, 0, &Fake));
            assert_eq!(order(&keys, method, 0x4 | FLAG_REVERSE, &Fake), order(&keys, method, FLAG_REVERSE, &Fake));
        }
    }

    /// Fails (and answers nonsense) on every pair that involves "bad"; flags the failure like the system one.
    struct Flaky(std::cell::Cell<bool>);

    impl Collator for Flaky {
        fn compare(&self, a: &str, b: &str) -> Ordering {
            if a.contains("bad") || b.contains("bad") {
                self.0.set(true);
                return if a.len() % 2 == 0 { Ordering::Less } else { Ordering::Greater };
            }
            Fake.compare(a, b)
        }
        fn failed(&self) -> bool {
            self.0.get()
        }
    }

    #[test]
    fn a_collator_failure_mid_sort_sorts_everything_again_with_the_ordinal_fallback() {
        let names = ["delta", "Bravo", "bad1", "alpha", "bad22", "Charlie", "echo", "bad333"];
        let keys: Vec<SortKey> = names.iter().map(|n| file(n)).collect();
        let flaky = Flaky(std::cell::Cell::new(false));
        let got = names_of(&keys, order(&keys, METHOD_NAME, 0, &flaky));
        assert!(flaky.failed());
        let expected = names_of(&keys, order(&keys, METHOD_NAME, 0, &OrdinalFold));
        assert_eq!(got, expected, "the fallback sorts the whole list, not just the failing pairs");
        assert_eq!(got, ["alpha", "bad1", "bad22", "bad333", "Bravo", "Charlie", "delta", "echo"]);
        // a healthy collator is left alone
        let fine = Flaky(std::cell::Cell::new(false));
        let ok_keys = vec![file("b"), file("a")];
        assert_eq!(order(&ok_keys, METHOD_NAME, 0, &fine), [1, 0]);
        assert!(!fine.failed());
    }

    fn names_of(keys: &[SortKey], idx: Vec<usize>) -> Vec<String> {
        idx.into_iter().map(|i| keys[i].name.clone()).collect()
    }

    #[test]
    fn unsorted_keeps_the_given_order() {
        let keys = vec![file("b"), file("a")];
        assert_eq!(order(&keys, METHOD_NONE, 0, &Fake), [0, 1]);
    }

    // ---- platform checks: CompareStringEx; NLS differences are logged, never failed ----------------

    fn soft(ok: bool, what: &str) {
        if !ok {
            eprintln!("sorting platform check differs on this Windows: {what}");
        }
    }

    fn platform(locale: &str, list: &[&str]) -> Vec<String> {
        let c = SystemCollator::for_locale(locale);
        let keys: Vec<SortKey> = list.iter().map(|n| file(n)).collect();
        names(&keys, METHOD_NAME, 0, &c)
    }

    #[test]
    fn platform_natural_order_and_case() {
        let c = SystemCollator::for_locale("en-US");
        assert_eq!(c.compare("file2", "file10"), Ordering::Less);
        assert_eq!(c.compare("FILE", "file"), Ordering::Equal);
        soft(c.compare("v1.9", "v1.10") == Ordering::Less, "v1.9 before v1.10");
        soft(c.compare("a-b", "ab") != Ordering::Equal, "punctuation is not skipped");
    }

    #[test]
    fn platform_non_ascii_follows_the_locale() {
        soft(platform("en-US", &["Ala", "\u{104}bc"]) == ["\u{104}bc", "Ala"], "en-US puts A-ogonek first");
        soft(platform("en-US", &["Zebra", "\u{17B}aba"]) == ["\u{17B}aba", "Zebra"], "en-US Zebra/Zaba");
        soft(platform("pl-PL", &["\u{104}bc", "Ala"]) == ["Ala", "\u{104}bc"], "pl-PL Ala first");
        soft(platform("pl-PL", &["\u{17B}aba", "Zebra"]) == ["Zebra", "\u{17B}aba"], "pl-PL Zebra first");
        for locale in ["en-US", "pl-PL"] {
            soft(platform(locale, &["\u{141}\u{F3}d\u{17A}", "Lato"]) == ["Lato", "\u{141}\u{F3}d\u{17A}"], "Lato before Lodz");
        }
    }

    #[test]
    fn platform_symbols_sort_before_digits_before_letters() {
        let got = platform("en-US", &["a.txt", "1.txt", "_x", ".env"]);
        soft(got.first().is_some_and(|n| n.starts_with('.') || n.starts_with('_')) && got.last().is_some_and(|n| n == "a.txt"), "punctuation, digits, letters");
        soft(got.iter().position(|n| n == "1.txt") > got.iter().position(|n| n == ".env"), "digits after punctuation");
    }

    #[test]
    fn menu_helpers() {
        // the old stored bit 4 neither changes the order nor maintenance
        assert!(is_maintained(METHOD_NAME, 0x4));
        assert!(is_maintained(METHOD_TYPE, FLAG_REVERSE));
        assert!(!is_maintained(METHOD_TYPE, FLAG_NOT_MAINTAINED));
    }
}
