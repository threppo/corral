//! Folder Gate preference policy. Design: folder-gate-views-spec sections 2-8.
//! This module has no filesystem, registry, window or COM dependencies.
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Icons,
    List,
    Details,
}
impl View {
    pub fn from_key(key: Option<&str>) -> Self {
        match key { Some("list") => Self::List, Some("details") => Self::Details, _ => Self::Icons }
    }
    pub fn from_mode(mode: i32) -> Self {
        match mode { 2 | 3 => Self::List, 4 | 8 => Self::Details, _ => Self::Icons }
    }
    pub fn mode(self) -> i32 {
        match self { Self::Icons => 1, Self::List => 3, Self::Details => 4 }
    }
}

/// A user view command patches only this folder's mode. Legacy sizes, flags, columns and unknown
/// fields survive, including unset entries; opening/inheriting a folder never calls this.
pub fn set_view(store: &mut ViewStates, folder: &str, view: View) {
    let existing = store.iter().filter(|(p, _)| folder_key(p) == folder_key(folder))
        .max_by_key(|(_, v)| (v.set, group_count(v), v.order)).map(|(p, _)| p.clone());
    let key = existing.unwrap_or_else(|| folder_key(folder));
    let entry = store.entry(key).or_default();
    let mut original = entry.original.take().filter(|v| matches!(v, StoredJson::Object(_)))
        .unwrap_or_else(|| StoredJson::from_value(entry.raw.clone().filter(Value::is_object).unwrap_or_else(|| json!({}))));
    original.put("view_mode", json!(view.mode()));
    original.put("set", json!(true));
    entry.raw = serde_json::to_value(&original).ok();
    entry.original = Some(original);
    entry.view_mode = view.mode();
    entry.set = true;
}

/// Drawn columns retain raw entries so newer and legacy Shell columns survive edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnKey { Name, Size, Type, DateModified }
impl ColumnKey {
    pub fn key(self) -> &'static str { match self { Self::Name => "name", Self::Size => "size", Self::Type => "type", Self::DateModified => "date_modified" } }
    pub fn label(self) -> &'static str { match self { Self::Name => "Name", Self::Size => "Size", Self::Type => "Type", Self::DateModified => "Date modified" } }
    pub fn parse(s: &str) -> Option<Self> { match s { "size" => Some(Self::Size), "type" => Some(Self::Type), "date_modified" => Some(Self::DateModified), _ => None } }
    pub fn sort(self) -> i32 { match self { Self::Name => 1, Self::Size => 2, Self::Type => 3, Self::DateModified => 4 } }
    pub fn method(self) -> u32 { match self { Self::Name => crate::sorting::METHOD_NAME, Self::Size => crate::sorting::METHOD_SIZE, Self::Type => crate::sorting::METHOD_TYPE, Self::DateModified => crate::sorting::METHOD_MODIFIED } }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailsColumns(pub Vec<Value>);
impl Default for DetailsColumns { fn default() -> Self { Self(vec![json!({"key":"size", "width":80})]) } }
impl DetailsColumns {
    pub fn parse(value: Option<&Value>) -> Self { value.and_then(Value::as_array).map(|a| Self(a.clone())).unwrap_or_default() }
    pub fn visible(&self) -> Vec<(ColumnKey, i32)> {
        let mut out = Vec::new();
        for v in &self.0 {
            let Some(key) = v.get("key").and_then(Value::as_str).and_then(ColumnKey::parse) else { continue };
            if out.iter().any(|(k, _)| *k == key) { continue; }
            let width = v.get("width").and_then(Value::as_i64).unwrap_or(80).clamp(40, i32::MAX as i64) as i32;
            out.push((key, width));
        }
        out
    }
    pub fn contains(&self, key: ColumnKey) -> bool { self.visible().iter().any(|(k, _)| *k == key) }
    pub fn resize(&mut self, key: ColumnKey, width: i32) { for v in &mut self.0 { if v.get("key").and_then(Value::as_str) == Some(key.key()) { v["width"] = json!(width.max(40)); } } }
    pub fn toggle(&mut self, key: ColumnKey) {
        if self.contains(key) { self.0.retain(|v| v.get("key").and_then(Value::as_str) != Some(key.key())); }
        else { self.0.push(json!({"key":key.key(),"width":if key == ColumnKey::DateModified {160} else {80}})); }
    }
    pub fn reset(&mut self) {
        self.0.retain(|v| v.get("key").and_then(Value::as_str).and_then(ColumnKey::parse).is_none());
    }
}
pub fn details_columns(folder: &str, store: &ViewStates, inherit: bool) -> DetailsColumns {
    let mut path = Some(folder_key(folder));
    while let Some(p) = path {
        if let Some(v) = stored(store, &p).filter(|v| v.set) {
            if let Some(raw) = v.raw.as_ref().and_then(|v| v.get("col_info")).filter(|v| v.is_array()) {
                return DetailsColumns::parse(Some(raw));
            }
        }
        path = if inherit { parent_key(&p) } else { None };
    }
    DetailsColumns::default()
}
/// Patch exactly one folder, retaining mode, sort, unknown fields and case aliases.
pub fn set_details_columns(store: &mut ViewStates, folder: &str, columns: &DetailsColumns) {
    patch_details_columns(store, folder, Some(columns));
}
/// Reset only this folder's drawn columns; opaque Shell/newer entries survive.
pub fn reset_details_columns(store: &mut ViewStates, folder: &str) {
    patch_details_columns(store, folder, None);
}
fn patch_details_columns(store: &mut ViewStates, folder: &str, columns: Option<&DetailsColumns>) {
    let existing = store.iter().filter(|(p, _)| folder_key(p) == folder_key(folder))
        .max_by_key(|(_, v)| (v.set, group_count(v), v.order)).map(|(p, _)| p.clone());
    let entry = store.entry(existing.unwrap_or_else(|| folder_key(folder))).or_default();
    let mut original = entry.original.take().filter(|v| matches!(v, StoredJson::Object(_)))
        .unwrap_or_else(|| StoredJson::from_value(entry.raw.clone().filter(Value::is_object).unwrap_or_else(|| json!({}))));
    let mut opaque = DetailsColumns(entry.raw.as_ref().and_then(|v| v.get("col_info"))
        .and_then(Value::as_array).cloned().unwrap_or_default());
    opaque.reset();
    let mut updated = columns.cloned().unwrap_or_else(|| opaque.clone());
    for (i, value) in opaque.0.iter().enumerate() {
        if updated.0.iter().filter(|v| *v == value).count() < opaque.0[..=i].iter().filter(|v| *v == value).count() {
            updated.0.push(value.clone());
        }
    }
    // Like a sort patch, the first edit must not activate ignored style/sort groups.
    // The caller archives the original entry before this explicit write.
    if !entry.set {
        entry.view_mode = 0;
        entry.icon_size = 0;
        entry.sort_mode = 0;
        for field in ["view_mode", "icon_size", "sort_mode"] { original.remove(field); }
    }
    if columns.is_none() && updated.0.is_empty() { original.remove("col_info"); }
    else { original.put("col_info", json!(updated.0)); }
    original.put("set", json!(true));
    entry.raw = serde_json::to_value(&original).ok();
    entry.original = Some(original);
    entry.set = true;
}

pub const FLAG_NON_ICON: u32 = 0x01;
pub const FLAG_HIDE_HEADER: u32 = 0x02;
pub const FLAG_CUSTOM_SIZE: u32 = 0x10;
pub const FLAG_CUSTOM_MODE: u32 = 0x20;
pub const FLAG_LIST: u32 = 0x40;
const TRANSIENT: u32 = 0x0800_0000;
const KNOWN_FLAGS: u32 =
    FLAG_NON_ICON | FLAG_HIDE_HEADER | FLAG_CUSTOM_SIZE | FLAG_CUSTOM_MODE | FLAG_LIST;
pub const NAME_FMTID: &str = "{B725F130-47EF-101A-A5F1-02608C9EEBAC}";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    pub fmtid: String,
    pub pid: u32,
    pub width: i32,
}
impl Column {
    pub fn name() -> Self {
        Self {
            fmtid: NAME_FMTID.into(),
            pid: 10,
            width: 0,
        }
    }
    pub fn same_property(&self, other: &Self) -> bool {
        self.pid == other.pid && self.fmtid.eq_ignore_ascii_case(&other.fmtid)
    }
}

/// Ordered JSON keeps untouched legacy fields and property order through snapshot serialization
/// (folder-gate-views-spec 7). Formatting remains the enclosing layout writer's responsibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoredJson {
    Scalar(Value),
    Array(Vec<StoredJson>),
    Object(Vec<(String, StoredJson)>),
}
impl StoredJson {
    fn put(&mut self, key: &str, value: Value) {
        let Self::Object(fields) = self else { return };
        let value = Self::from_value(value);
        if let Some((_, v)) = fields.iter_mut().find(|(k, _)| k == key) {
            *v = value;
        } else {
            fields.push((key.into(), value));
        }
    }
    fn remove(&mut self, key: &str) {
        if let Self::Object(fields) = self {
            fields.retain(|(k, _)| k != key);
        }
    }
    fn from_value(value: Value) -> Self {
        match value {
            Value::Object(fields) => Self::Object(
                fields
                    .into_iter()
                    .map(|(k, v)| (k, Self::from_value(v)))
                    .collect(),
            ),
            Value::Array(values) => Self::Array(values.into_iter().map(Self::from_value).collect()),
            v => Self::Scalar(v),
        }
    }
}
impl Serialize for StoredJson {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self {
            Self::Scalar(v) => v.serialize(s),
            Self::Array(values) => {
                let mut seq = s.serialize_seq(Some(values.len()))?;
                for v in values {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
            Self::Object(fields) => {
                let mut map = s.serialize_map(Some(fields.len()))?;
                for (k, v) in fields {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}
impl<'de> Deserialize<'de> for StoredJson {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Json;
        impl<'de> serde::de::Visitor<'de> for Json {
            type Value = StoredJson;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("JSON value")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(StoredJson::Scalar(Value::Null))
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(StoredJson::Scalar(json!(v)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(StoredJson::Scalar(json!(v)))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(StoredJson::Scalar(json!(v)))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(StoredJson::Scalar(json!(v)))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(StoredJson::Scalar(json!(v)))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Self::Value, E> {
                Ok(StoredJson::Scalar(json!(v)))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(v) = a.next_element()? {
                    values.push(v);
                }
                Ok(StoredJson::Array(values))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Self::Value, A::Error> {
                let mut fields = Vec::new();
                while let Some(v) = a.next_entry()? {
                    fields.push(v);
                }
                Ok(StoredJson::Object(fields))
            }
        }
        d.deserialize_any(Json)
    }
}

/// Stored shapes stay readable; raw fields survive until that exact group is patched (spec 7).
#[derive(Clone, Debug, Default)]
pub struct ViewState {
    pub view_mode: i32,
    pub icon_size: i32,
    pub sort_mode: i32,
    pub view_flags: u32,
    pub set: bool,
    pub col_info: Option<Vec<Column>>,
    pub raw: Option<Value>,
    pub original: Option<StoredJson>,
    pub order: usize,
}
impl PartialEq for ViewState {
    fn eq(&self, other: &Self) -> bool {
        serde_json::to_value(self).ok() == serde_json::to_value(other).ok()
    }
}
impl Eq for ViewState {}
fn integer(v: &Value, name: &str) -> i32 {
    v.get(name)
        .and_then(Value::as_i64)
        .and_then(|x| i32::try_from(x).ok())
        .unwrap_or(0)
}
fn valid_mode(mode: i32) -> bool {
    matches!(mode, 1..=6 | 8)
}
fn valid_sort(sort: i32) -> bool {
    matches!(sort, -5..=-1 | 1..=5)
}
fn valid_guid(s: &str) -> bool {
    let s = s
        .strip_prefix('{')
        .and_then(|s| s.strip_suffix('}'))
        .unwrap_or(s);
    s.len() == 36
        && s.chars().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == '-'
            } else {
                c.is_ascii_hexdigit()
            }
        })
}
fn normal_columns(columns: Vec<Column>) -> Option<Vec<Column>> {
    let mut out = Vec::<Column>::new();
    for mut c in columns {
        if !valid_guid(&c.fmtid) {
            continue;
        }
        c.fmtid = format!(
            "{{{}}}",
            c.fmtid.trim_matches(['{', '}']).to_ascii_uppercase()
        );
        if out.iter().any(|x| x.same_property(&c)) {
            continue;
        }
        c.width = if c.width == 0 {
            0
        } else {
            c.width.clamp(24, 1200)
        };
        out.push(c);
    }
    if out.is_empty() {
        return None;
    }
    let name = out
        .iter()
        .position(|c| c.same_property(&Column::name()))
        .map(|i| out.remove(i))
        .unwrap_or_else(Column::name);
    out.insert(0, name);
    Some(out)
}
impl<'de> Deserialize<'de> for ViewState {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let original = StoredJson::deserialize(d)?;
        let raw = serde_json::to_value(&original).map_err(serde::de::Error::custom)?;
        let mode = integer(&raw, "view_mode");
        let sort = integer(&raw, "sort_mode");
        let size = integer(&raw, "icon_size");
        let columns = raw.get("col_info").and_then(Value::as_array).and_then(|a| {
            normal_columns(
                a.iter()
                    .filter_map(|c| {
                        Some(Column {
                            fmtid: c.get("fmtid")?.as_str()?.into(),
                            pid: c.get("pid")?.as_u64()?.try_into().ok()?,
                            width: integer(c, "width"),
                        })
                    })
                    .collect(),
            )
        });
        Ok(Self {
            view_mode: if valid_mode(mode) { mode } else { 0 },
            icon_size: if size > 0 {
                size.clamp(16, 256)
            } else if size < 0 {
                -1
            } else {
                0
            },
            sort_mode: if valid_sort(sort) { sort } else { 0 },
            view_flags: raw
                .get("view_flags")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0)
                & !TRANSIENT,
            set: raw.get("set").and_then(Value::as_bool).unwrap_or(false),
            col_info: columns,
            raw: Some(raw),
            original: Some(original),
            order: 0,
        })
    }
}
impl Serialize for ViewState {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if let Some(original) = &self.original {
            return original.serialize(s);
        }
        if let Some(raw) = &self.raw {
            return raw.serialize(s);
        }
        json!({"view_mode":self.view_mode,"icon_size":self.icon_size,"sort_mode":self.sort_mode,
            "view_flags":self.view_flags,"set":self.set,"col_info":self.col_info})
        .serialize(s)
    }
}
/// Input order is retained for case-collision precedence, independent of map ordering (spec 7).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewStates(BTreeMap<String, ViewState>);
impl Deref for ViewStates {
    type Target = BTreeMap<String, ViewState>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl DerefMut for ViewStates {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
impl ViewStates {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
impl Serialize for ViewStates {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut entries: Vec<_> = self.0.iter().collect();
        entries.sort_by_key(|(_, v)| v.order);
        let mut map = s.serialize_map(Some(entries.len()))?;
        for (k, v) in entries {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}
impl<'de> Deserialize<'de> for ViewStates {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Entries;
        impl<'de> serde::de::Visitor<'de> for Entries {
            type Value = ViewStates;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("folder preferences")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Self::Value, A::Error> {
                let mut out = ViewStates::default();
                let mut order = 0;
                while let Some((k, mut v)) = a.next_entry::<String, ViewState>()? {
                    v.order = order;
                    order += 1;
                    out.insert(k, v);
                }
                Ok(out)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(ViewStates::default())
            }
        }
        d.deserialize_any(Entries)
    }
}

/// Lexical Windows keys only; simple one-character folding never expands a character (spec 3.1).
pub fn folder_key(path: &str) -> String {
    let p = path.replace('/', "\\");
    let p = if p
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("\\\\?\\UNC\\"))
    {
        format!("\\\\{}", &p[8..])
    } else {
        p.strip_prefix("\\\\?\\").unwrap_or(&p).to_string()
    };
    let mut p: String = p
        .chars()
        .map(|c| {
            // Single-character fold equivalents that lowercase alone does not unify (spec 3.1).
            let c = match c {
                '\u{00b5}' => '\u{03bc}',
                '\u{017f}' => 's',
                '\u{0345}' | '\u{1fbe}' => '\u{03b9}',
                '\u{03c2}' => '\u{03c3}',
                '\u{03d0}' => '\u{03b2}',
                '\u{03d1}' => '\u{03b8}',
                '\u{03d5}' => '\u{03c6}',
                '\u{03d6}' => '\u{03c0}',
                '\u{03f0}' => '\u{03ba}',
                '\u{03f1}' => '\u{03c1}',
                '\u{03f5}' => '\u{03b5}',
                '\u{1e9b}' => '\u{1e61}',
                '\u{1c80}' => '\u{0432}',
                '\u{1c81}' => '\u{0434}',
                '\u{1c82}' => '\u{043e}',
                '\u{1c83}' => '\u{0441}',
                '\u{1c84}' | '\u{1c85}' => '\u{0442}',
                '\u{1c86}' => '\u{044a}',
                '\u{1c87}' => '\u{0463}',
                '\u{1c88}' => '\u{a64b}',
                _ => c,
            };
            let mut lower = c.to_lowercase();
            let first = lower.next().unwrap_or(c);
            if lower.next().is_none() { first } else { c }
        })
        .collect();
    while p.ends_with('\\') && !(p.len() == 3 && p.as_bytes()[1] == b':') {
        p.pop();
    }
    p
}
pub fn parent_key(path: &str) -> Option<String> {
    let p = folder_key(path);
    if p.starts_with("\\\\") {
        let parts: Vec<_> = p[2..].split('\\').collect();
        if parts.len() <= 2 || parts.iter().any(|s| s.is_empty()) {
            return None;
        }
        return p.rfind('\\').map(|i| p[..i].into());
    }
    if p.len() < 3 || p.as_bytes()[1] != b':' || p.as_bytes()[2] != b'\\' {
        return None;
    }
    if p.len() == 3 {
        return None;
    }
    p.rfind('\\')
        .map(|i| p[..if i == 2 { 3 } else { i }].into())
}
fn group_count(v: &ViewState) -> usize {
    usize::from(valid_mode(v.view_mode))
        + usize::from(valid_sort(v.sort_mode))
        + usize::from(v.col_info.is_some())
}
pub fn stored<'a>(states: &'a ViewStates, folder: &str) -> Option<&'a ViewState> {
    let k = folder_key(folder);
    if k.starts_with("\\\\") && k[2..].split('\\').count() < 2 {
        return None;
    }
    states
        .iter()
        .filter(|(p, _)| folder_key(p) == k)
        .map(|(_, v)| v)
        .max_by_key(|v| (v.set, group_count(v), v.order))
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconSize {
    Follow,
    Fixed(i32),
}
impl IconSize {
    fn from_stored(n: i32) -> Self {
        if n <= 0 {
            Self::Follow
        } else {
            Self::Fixed(n.clamp(16, 256))
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Style {
    pub mode: i32,
    pub size: IconSize,
    pub folder_flags: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Own,
    Inherited(String),
    Default,
}
#[derive(Clone, Copy, Debug)]
pub struct Context {
    /// Current desktop icon size in Shell units (the same units as a Fixed size).
    pub desktop_icon_size: i32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedView {
    pub style: Style,
    pub sort: i32,
    pub columns: Option<Vec<Column>>,
    pub style_source: Source,
    pub sort_source: Source,
    pub columns_source: Source,
}
pub fn resolve_view(
    folder: &str,
    store: &ViewStates,
    inherit: bool,
    _ctx: Context,
) -> ResolvedView {
    let mut out = ResolvedView {
        style: Style {
            mode: 1,
            size: IconSize::Follow,
            folder_flags: 0,
        },
        sort: 1,
        columns: None,
        style_source: Source::Default,
        sort_source: Source::Default,
        columns_source: Source::Default,
    };
    let mut path = Some(folder_key(folder));
    let mut own = true;
    while let Some(p) = path {
        if let Some(v) = stored(store, &p).filter(|v| v.set) {
            let source = if own {
                Source::Own
            } else {
                Source::Inherited(p.clone())
            };
            if out.style_source == Source::Default && valid_mode(v.view_mode) {
                out.style = Style {
                    mode: v.view_mode,
                    size: IconSize::from_stored(v.icon_size),
                    folder_flags: v.view_flags & !TRANSIENT,
                };
                out.style_source = source.clone();
            }
            if out.sort_source == Source::Default && valid_sort(v.sort_mode) {
                out.sort = v.sort_mode;
                out.sort_source = source.clone();
            }
            if out.columns_source == Source::Default {
                if let Some(c) = v.col_info.clone().and_then(normal_columns) {
                    out.columns = Some(c);
                    out.columns_source = source;
                }
            }
        }
        path = if inherit { parent_key(&p) } else { None };
        own = false;
    }
    out
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Patch {
    pub style: Option<Style>,
    pub sort: Option<i32>,
    pub columns: Option<Vec<Column>>,
}
/// folder-gate-views-spec 7: everything `apply_patch` is about to drop or replace is copied here
/// first (the caller stores it in `Layout.view_unparsed`, keyed by the entry's own key), so no
/// stored data is lost: non-object aliases of the folder, object aliases that lose the case
/// merge, and an unset winner whose untrusted known fields the first patch clears.
pub fn unparsed_for_folder(store: &ViewStates, folder: &str) -> Vec<(String, StoredJson)> {
    let key = folder_key(folder);
    let aliases: Vec<_> = store.iter().filter(|(path, _)| folder_key(path) == key).collect();
    let winner = aliases
        .iter()
        .max_by_key(|(_, v)| (v.set, group_count(v), v.order))
        .map(|(path, _)| path.as_str());
    aliases
        .iter()
        .filter_map(|(path, entry)| {
            let raw = entry.raw.as_ref()?;
            let archive = match raw.as_object() {
                None => true,
                Some(_) if Some(path.as_str()) != winner => true,
                Some(fields) => {
                    !entry.set && ["view_mode", "icon_size", "sort_mode", "col_info"].iter().any(|f| fields.contains_key(*f))
                }
            };
            archive.then(|| {
                let original = entry.original.clone().unwrap_or_else(|| StoredJson::from_value(raw.clone()));
                ((*path).clone(), original)
            })
        })
        .collect()
}

/// Group patches preserve raw unknown fields and flags and collapse case aliases only on write (spec 7).
pub fn apply_patch(store: &mut ViewStates, folder: &str, patch: Patch, ctx: Context) {
    let mut entry = stored(store, folder).cloned().unwrap_or_default();
    let mut raw = entry
        .raw
        .clone()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    // folder-gate-views-spec 2, 4, 7: an unset entry has no trusted groups. Its first
    // user patch must not turn old Windows defaults in other groups into user preferences.
    // Migration happens only on this explicit write; unknown fields/flag bits stay intact.
    if !entry.set {
        entry.view_mode = 0;
        entry.icon_size = 0;
        entry.sort_mode = 0;
        entry.col_info = None;
        for field in ["view_mode", "icon_size", "sort_mode", "col_info"] {
            raw.as_object_mut().expect("patch object").remove(field);
        }
    }
    if let Some(style) = patch.style {
        entry.view_mode = style.mode;
        entry.icon_size = match style.size {
            IconSize::Follow => -ctx.desktop_icon_size.max(1),
            IconSize::Fixed(n) => n,
        };
        entry.view_flags =
            (entry.view_flags & !KNOWN_FLAGS & !TRANSIENT) | (style.folder_flags & KNOWN_FLAGS);
        raw["view_mode"] = json!(entry.view_mode);
        raw["icon_size"] = json!(entry.icon_size);
        raw["view_flags"] = json!(entry.view_flags);
    }
    if let Some(sort) = patch.sort {
        entry.sort_mode = sort;
        raw["sort_mode"] = json!(sort);
    }
    if let Some(columns) = patch.columns {
        entry.col_info = Some(columns);
        raw["col_info"] = json!(entry.col_info);
    }
    entry.set = true;
    raw["set"] = json!(true);
    let mut original = entry
        .original
        .take()
        .filter(|v| matches!(v, StoredJson::Object(_)))
        .unwrap_or_else(|| {
            StoredJson::from_value(
                entry
                    .raw
                    .clone()
                    .filter(Value::is_object)
                    .unwrap_or_else(|| json!({})),
            )
        });
    if let Some(old) = entry.raw.as_ref().filter(|v| v.is_object()) {
        for field in old.as_object().expect("old object").keys() {
            if raw.get(field).is_none() {
                original.remove(field);
            }
        }
        for (k, v) in raw.as_object().expect("patch object") {
            if old.get(k) != Some(v) {
                original.put(k, v.clone());
            }
        }
    } else {
        for (k, v) in raw.as_object().expect("patch object") {
            original.put(k, v.clone());
        }
    }
    entry.original = Some(original);
    entry.raw = Some(raw);
    let key = folder_key(folder);
    store.retain(|p, _| folder_key(p) != key);
    store.insert(key, entry);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn corral_keys_and_gate_modes_map_tolerantly_both_ways() {
        for key in [None, Some("icons"), Some("future"), Some("List")] { assert_eq!(View::from_key(key), View::Icons); }
        assert_eq!(View::from_key(Some("list")), View::List);
        assert_eq!(View::from_key(Some("details")), View::Details);
        for mode in [0, 1, 5, 6, 7, 99, -1] { assert_eq!(View::from_mode(mode), View::Icons); }
        for mode in [2, 3] { assert_eq!(View::from_mode(mode), View::List); }
        for mode in [4, 8] { assert_eq!(View::from_mode(mode), View::Details); }
        for (view, mode) in [(View::Icons, 1), (View::List, 3), (View::Details, 4)] {
            assert_eq!(view.mode(), mode);
            assert_eq!(View::from_mode(mode), view);
        }
    }

    #[test]
    fn user_view_change_preserves_every_other_field_and_folder() {
        for set in [true, false] {
            let before = json!({r"C:\QA": {"view_mode": 8, "set": set, "icon_size": 100,
                "view_flags": 1234567, "sort_mode": -4, "col_info": [{"future": "opaque"}], "unknown": [1, {"x": true}]},
                r"C:\other": {"view_mode": 2, "set": true, "unknown": 42}});
            let mut states: ViewStates = serde_json::from_value(before.clone()).unwrap();
            set_view(&mut states, r"c:\qa\", View::List);
            let mut expected = before;
            expected[r"C:\QA"]["view_mode"] = json!(3);
            expected[r"C:\QA"]["set"] = json!(true);
            assert_eq!(serde_json::to_value(&states).unwrap(), expected);
            assert_eq!(View::from_mode(resolve(r"C:\QA\child", &states, true).style.mode), View::List);
            assert_eq!(View::from_mode(resolve(r"C:\QA\child", &states, false).style.mode), View::Icons);
        }
    }

    #[test]
    fn changing_inherited_view_creates_only_the_child_entry() {
        let mut states: ViewStates = serde_json::from_value(json!({r"C:\QA": {"view_mode": 8, "set": true, "unknown": 12}})).unwrap();
        let parent = serde_json::to_value(stored(&states, r"C:\QA").unwrap()).unwrap();
        let baseline = serde_json::to_value(&states).unwrap();
        assert_eq!(View::from_mode(resolve(r"C:\QA\child", &states, true).style.mode), View::Details);
        assert_eq!(serde_json::to_value(&states).unwrap(), baseline, "resolving never saves");
        set_view(&mut states, r"C:\QA\child", View::Icons);
        assert_eq!(states.len(), 2);
        assert_eq!(serde_json::to_value(stored(&states, r"C:\QA").unwrap()).unwrap(), parent);
        assert_eq!(View::from_mode(resolve(r"C:\QA\child", &states, true).style.mode), View::Icons);
    }
    fn ctx(_dpi: u32) -> Context {
        Context {
            desktop_icon_size: 32,
        }
    }
    fn entry(value: Value) -> ViewState {
        serde_json::from_value(value).unwrap()
    }
    fn resolve(folder: &str, store: &ViewStates, inherit: bool) -> ResolvedView {
        resolve_view(folder, store, inherit, ctx(96))
    }
    fn columns() -> Vec<Column> {
        vec![
            Column {
                width: 100,
                ..Column::name()
            },
            Column {
                fmtid: NAME_FMTID.into(),
                pid: 12,
                width: 80,
            },
        ]
    }

    #[test]
    fn own_parent_default_resolve_per_group() {
        let mut store = ViewStates::default();
        store.insert(
            r"D:\a".into(),
            entry(json!({"set":true,"view_mode":4,"icon_size":48,"col_info":columns()})),
        );
        store.insert(r"D:\a\b".into(), entry(json!({"set":true,"sort_mode":-2})));
        let child = resolve(r"D:\a\b", &store, true);
        assert_eq!(child.style.mode, 4);
        assert_eq!(child.style_source, Source::Inherited(r"d:\a".into()));
        assert_eq!(child.sort, -2);
        assert_eq!(child.sort_source, Source::Own);
        assert!(child.columns.is_some());
        let grandchild = resolve(r"D:\a\b\c", &store, true);
        assert_eq!(grandchild.sort_source, Source::Inherited(r"d:\a\b".into()));
        let off = resolve(r"D:\a\b\c", &store, false);
        assert_eq!(off.style.mode, 1);
        assert_eq!(off.style.size, IconSize::Follow);
        assert_eq!(off.sort, 1);
        assert_eq!(off.columns, None);
        store.insert(r"D:\a\b\c".into(), entry(json!({"set":true,"view_mode":3})));
        assert_eq!(resolve(r"D:\a\b\c", &store, true).style_source, Source::Own);
    }
    #[test]
    fn lexical_roots_unc_extended_paths_and_case() {
        assert_eq!(parent_key(r"C:\"), None);
        assert_eq!(parent_key(r"C:\a"), Some(r"c:\".into()));
        assert_eq!(parent_key(r"\\srv\share\x"), Some(r"\\srv\share".into()));
        assert_eq!(parent_key(r"\\srv\share"), None);
        assert_eq!(parent_key(r"\\srv"), None);
        assert_eq!(folder_key(r"\\?\C:\Users\me\"), folder_key("c:/USERS/me"));
        assert_eq!(
            folder_key(r"\\?\UNC\SRV\Share\x"),
            folder_key(r"\\srv\share\x")
        );
        assert_eq!(parent_key("shell:Downloads"), None);
        let p = format!(r"C:\{}\trailing. ", "x".repeat(500));
        assert!(folder_key(&p).ends_with("trailing. "));
        assert_eq!(folder_key(r"C:\ŻÓŁĆ\Ä"), folder_key(r"c:\żółć\ä"));
    }
    #[test]
    fn case_collisions_prefer_set_then_groups_then_input_order() {
        let store:ViewStates=serde_json::from_str(r#"{"c:\\X":{"set":true,"sort_mode":2},"C:\\x":{"set":false,"view_mode":4,"sort_mode":3}}"#).unwrap();
        assert_eq!(resolve(r"C:\X", &store, false).sort, 2);
        let store:ViewStates=serde_json::from_str(r#"{"c:\\X":{"set":true,"sort_mode":2},"C:\\x":{"set":true,"view_mode":4,"sort_mode":3}}"#).unwrap();
        assert_eq!(resolve(r"C:\X", &store, false).sort, 3);
        let store: ViewStates = serde_json::from_str(
            r#"{"c:\\X":{"set":true,"sort_mode":2},"C:\\x":{"set":true,"sort_mode":3}}"#,
        )
        .unwrap();
        assert_eq!(resolve(r"C:\X", &store, false).sort, 3);
        let reload: ViewStates =
            serde_json::from_str(&serde_json::to_string(&store).unwrap()).unwrap();
        assert_eq!(resolve(r"C:\X", &reload, false).sort, 3);
    }
    #[test]
    fn ignore_unset_and_invalid_style_modes() {
        for mode in [0, -1, 7, 99] {
            let mut store = ViewStates::default();
            store.insert(
                r"c:\x".into(),
                entry(json!({"set":true,"view_mode":mode,"sort_mode":-4})),
            );
            let v = resolve(r"c:\x", &store, true);
            assert_eq!(v.style_source, Source::Default);
            assert_eq!(v.sort, -4);
        }
        let mut store = ViewStates::default();
        store.insert(
            r"c:\x".into(),
            entry(json!({"set":false,"view_mode":4,"sort_mode":-4})),
        );
        assert_eq!(resolve(r"c:\x", &store, true).sort, 1);
        store.insert(r"c:\x".into(), entry(json!({"set":true})));
        assert_eq!(resolve(r"c:\x", &store, true).style_source, Source::Default);
    }
    #[test]
    fn stored_icon_values_are_preserved() {
        for (n, want) in [
            (-4000, IconSize::Follow),
            (-1, IconSize::Follow),
            (0, IconSize::Follow),
            (5, IconSize::Fixed(16)),
            (4000, IconSize::Fixed(256)),
        ] {
            let mut store = ViewStates::default();
            store.insert(
                "f".into(),
                entry(json!({"set":true,"view_mode":1,"icon_size":n})),
            );
            assert_eq!(resolve("f", &store, false).style.size, want);
        }
        for mode in [2, 3, 4, 6, 8] {
            let mut store = ViewStates::default();
            store.insert(
                "f".into(),
                entry(json!({"set":true,"view_mode":mode,"icon_size":48})),
            );
            let r = resolve("f", &store, false);
            assert_eq!(r.style.size, IconSize::Fixed(48));
        }
    }
    #[test]
    fn two_gates_patch_independent_groups_and_last_same_group_wins() {
        let mut store = ViewStates::default();
        apply_patch(&mut store, "f", Patch { sort: Some(-2), ..Default::default() }, ctx(96));
        apply_patch(&mut store, "f", Patch {
            style: Some(Style { mode: 4, size: IconSize::Follow, folder_flags: 0 }), ..Default::default()
        }, ctx(96));
        let resolved = resolve("f", &store, false);
        assert_eq!(resolved.style.mode, 4);
        assert_eq!(resolved.sort, -2);
        apply_patch(&mut store, "f", Patch { sort: Some(3), ..Default::default() }, ctx(96));
        assert_eq!(resolve("f", &store, false).sort, 3);
    }

    #[test]
    fn parent_patch_is_visible_to_the_next_child_navigation() {
        let mut store = ViewStates::default();
        apply_patch(&mut store, r"c:\parent", Patch { sort: Some(-2), ..Default::default() }, ctx(96));
        assert_eq!(resolve(r"c:\parent\child", &store, true).sort, -2);
    }

    #[test]
    fn all_sort_values_are_independent_of_style() {
        for n in -6..=6 {
            let mut store = ViewStates::default();
            store.insert("f".into(), entry(json!({"set":true,"sort_mode":n})));
            let r = resolve("f", &store, false);
            assert_eq!(r.sort, if valid_sort(n) { n } else { 1 });
            assert_eq!(r.style_source, Source::Default);
        }
    }
    #[test]
    fn columns_name_first_dedup_width_clamp_and_bad_properties() {
        let cols = normal_columns(vec![
            Column {
                width: 5,
                pid: 12,
                ..Column::name()
            },
            Column {
                width: 5000,
                ..Column::name()
            },
            Column::name(),
            Column {
                fmtid: "bad".into(),
                pid: 9,
                width: 100,
            },
        ])
        .unwrap();
        let bare = Column {
            fmtid: NAME_FMTID.trim_matches(['{', '}']).to_ascii_lowercase(),
            pid: 10,
            width: 99,
        };
        assert_eq!(
            normal_columns(vec![Column::name(), bare]).unwrap(),
            vec![Column::name()]
        );
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].pid, 10);
        assert_eq!(cols[0].width, 1200);
        assert_eq!(cols[1].width, 24);
        let cols = normal_columns(vec![Column {
            width: 0,
            pid: 12,
            ..Column::name()
        }])
        .unwrap();
        assert_eq!(cols[0], Column::name());
        assert_eq!(cols[1].width, 0);
        assert!(
            normal_columns(vec![Column {
                fmtid: "unknown".into(),
                pid: 9,
                width: 1
            }])
            .is_none()
        );
    }
    #[test]
    fn migration_bad_fields_and_nonobjects_do_not_fail_and_no_load_write_occurs() {
        let input = r#"{"f":{"set":true,"view_mode":"bad","icon_size":[],"sort_mode":-2,"view_flags":134217858,"col_info":[false,{}, {"fmtid":"bad","pid":10}],"extra":{"keep":42}},"gone":null,"array":[1,2],"bool":true}"#;
        let store: ViewStates = serde_json::from_str(input).unwrap();
        assert_eq!(store["f"].view_mode, 0);
        assert_eq!(store["f"].sort_mode, -2);
        assert_eq!(store["f"].view_flags, 130);
        assert_eq!(
            serde_json::from_str::<Value>(input).unwrap(),
            serde_json::to_value(&store).unwrap()
        );
        assert_eq!(resolve("f", &store, false).sort, -2);
        assert_eq!(resolve("gone", &store, false).style_source, Source::Default);
    }
    #[test]
    fn patch_preserves_unknown_fields_flags_unparsed_and_unrelated_entries() {
        let mut store:ViewStates=serde_json::from_value(json!({"f":{"set":true,"view_mode":1,"view_flags":0x08000080u32,"extra":[1,2],"sort_mode":"bad"},"unparsed":[42],"untouched":{"legacy":false}})).unwrap();
        let before = serde_json::to_value(&store).unwrap();
        apply_patch(
            &mut store,
            "f",
            Patch {
                style: Some(Style {
                    mode: 4,
                    size: IconSize::Follow,
                    folder_flags: FLAG_HIDE_HEADER,
                }),
                ..Default::default()
            },
            ctx(144),
        );
        let after = serde_json::to_value(&store).unwrap();
        assert_eq!(after["f"]["view_flags"], 130);
        assert_eq!(after["f"]["icon_size"], -32);
        assert_eq!(after["f"]["extra"], before["f"]["extra"]);
        assert_eq!(after["f"]["sort_mode"], "bad");
        assert_eq!(after["unparsed"], before["unparsed"]);
        assert_eq!(after["untouched"], before["untouched"]);
        let reloaded: ViewStates = serde_json::from_value(after).unwrap();
        assert_eq!(resolve("f", &store, false), resolve("f", &reloaded, false));
    }
    #[test]
    fn first_user_patch_does_not_activate_untrusted_legacy_groups() {
        let mut store = ViewStates::default();
        store.insert(
            r"c:\parent".into(),
            entry(json!({"set":true,"view_mode":3,"icon_size":48})),
        );
        store.insert(r"c:\parent\child".into(),entry(json!({"set":false,"view_mode":4,"icon_size":96,"sort_mode":-4,"col_info":columns(),"view_flags":130,"extra":{"keep":42}})));
        apply_patch(
            &mut store,
            r"c:\parent\child",
            Patch {
                sort: Some(2),
                ..Default::default()
            },
            ctx(96),
        );
        let r = resolve(r"c:\parent\child", &store, true);
        assert_eq!(r.style_source, Source::Inherited(r"c:\parent".into()));
        assert_eq!(r.style.mode, 3);
        assert_eq!(r.sort_source, Source::Own);
        assert_eq!(r.sort, 2);
        assert_eq!(r.columns, None);
        let raw = serde_json::to_value(&store).unwrap();
        let child = &raw[r"c:\parent\child"];
        assert_eq!(child["extra"]["keep"], 42);
        assert_eq!(child["view_flags"], 130);
        assert!(child.get("view_mode").is_none());
        assert!(child.get("col_info").is_none());
        let reload: ViewStates = serde_json::from_value(raw).unwrap();
        assert_eq!(resolve(r"c:\parent\child", &reload, true), r);
    }
    #[test]
    fn snapshot_preserves_nonobject_aliases_even_when_that_folder_is_configured() {
        let mut layout = crate::model::Layout::new();
        layout.view_states =
            serde_json::from_str(r#"{"C:\\folder":[42,{"z":1,"a":2}],"c:\\FOLDER":null}"#).unwrap();
        let before = serde_json::to_string(&layout).unwrap();
        assert!(!before.contains("view_unparsed"));
        layout
            .view_unparsed
            .extend(unparsed_for_folder(&layout.view_states, r"c:\folder"));
        apply_patch(
            &mut layout.view_states,
            r"c:\folder",
            Patch {
                sort: Some(-2),
                ..Default::default()
            },
            ctx(96),
        );
        assert_eq!(layout.view_unparsed.len(), 2);
        let bytes = serde_json::to_string(&layout).unwrap();
        assert!(bytes.contains(r#""C:\\folder":[42,{"z":1,"a":2}]"#));
        let reload: crate::model::Layout = serde_json::from_str(&bytes).unwrap();
        assert_eq!(reload.view_unparsed, layout.view_unparsed);
        assert_eq!(resolve(r"c:\folder", &reload.view_states, false).sort, -2);
    }
    #[test]
    fn compact_migration_matrix_preserves_field_order_and_unparsed_bytes() {
        let input = r#"{"c:\\X":{"set":true,"sort_mode":-3,"unknown":{"z":9,"a":1}},"C:\\x":{"view_mode":7,"icon_size":4000,"view_flags":134217858,"sort_mode":"bad","set":false,"col_info":[null,{"fmtid":"bad","pid":2}]},"nonobject":[false,null,17],"empty":{}}"#;
        let store: ViewStates = serde_json::from_str(input).unwrap();
        assert_eq!(serde_json::to_string(&store).unwrap(), input);
        let pretty = serde_json::to_string_pretty(&store).unwrap();
        let reloaded: ViewStates = serde_json::from_str(&pretty).unwrap();
        assert_eq!(serde_json::to_string_pretty(&reloaded).unwrap(), pretty);
    }
    #[test]
    fn simple_case_fold_does_not_expand_or_apply_turkish_special_rules() {
        assert_eq!(folder_key(r"c:\Σ\ς\µ\ſ"), folder_key(r"C:\σ\σ\μ\s"));
        assert_eq!(folder_key(r"c:\ẞ"), folder_key(r"c:\ß"));
        assert_ne!(folder_key(r"c:\ß"), folder_key(r"c:\ss"));
        assert_ne!(folder_key(r"c:\ı"), folder_key(r"c:\i"));
    }
    #[test]
    fn canonical_snapshot_serialization_is_byte_stable_without_user_patch() {
        let mut layout = crate::model::Layout::new();
        layout.view_states = serde_json::from_value(
            json!({"f":{"set":true,"sort_mode":-3,"custom":17},"missing":"keep"}),
        )
        .unwrap();
        let before = serde_json::to_vec_pretty(&layout).unwrap();
        let loaded: crate::model::Layout = serde_json::from_slice(&before).unwrap();
        let after = serde_json::to_vec_pretty(&loaded).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn follow_size_is_written_in_shell_units() {
        let mut store = ViewStates::default();
        let c = Context { desktop_icon_size: 48 };
        let p = Patch { style: Some(Style { mode: 4, size: IconSize::Follow, folder_flags: 0 }), ..Default::default() };
        apply_patch(&mut store, "f", p, c);
        assert_eq!(stored(&store, "f").unwrap().icon_size, -48);
    }
    #[test]
    fn first_patch_archives_an_unset_entry_before_clearing_it() {
        let original = json!({"set":false,"view_mode":4,"icon_size":96,"sort_mode":-4,"col_info":columns(),"view_flags":130,"extra":{"keep":42}});
        let mut layout = crate::model::Layout::new();
        layout.view_states = serde_json::from_value(json!({r"C:\Folder": original.clone(), "other": {"set":false,"view_mode":3}})).unwrap();
        for (path, raw) in unparsed_for_folder(&layout.view_states, r"c:\folder") {
            layout.view_unparsed.entry(path).or_insert(raw);
        }
        apply_patch(&mut layout.view_states, r"c:\folder", Patch { sort: Some(2), ..Default::default() }, ctx(96));
        assert_eq!(layout.view_unparsed.len(), 1);
        let archived = serde_json::to_value(&layout.view_unparsed[r"C:\Folder"]).unwrap();
        assert_eq!(archived, original);
        let out = serde_json::to_value(&layout.view_states).unwrap();
        assert!(out[r"c:\folder"].get("view_mode").is_none());
        assert_eq!(out["other"], json!({"set":false,"view_mode":3}));
        let reload: crate::model::Layout = serde_json::from_slice(&serde_json::to_vec(&layout).unwrap()).unwrap();
        assert_eq!(reload.view_unparsed, layout.view_unparsed);
    }
    #[test]
    fn unset_entry_without_known_fields_and_set_entries_are_not_archived() {
        let store: ViewStates = serde_json::from_value(json!({"a":{"set":false,"view_flags":2,"x":1},"b":{"set":true,"view_mode":4}})).unwrap();
        assert!(unparsed_for_folder(&store, "a").is_empty());
        assert!(unparsed_for_folder(&store, "b").is_empty());
    }
    #[test]
    fn losing_object_case_aliases_are_archived_not_dropped() {
        let loser = json!({"set":true,"sort_mode":2,"keep":[1]});
        let mut layout = crate::model::Layout::new();
        layout.view_states = serde_json::from_value(json!({
            r"c:\X": loser.clone(),
            r"C:\x": {"set":true,"view_mode":4,"sort_mode":3},
        })).unwrap();
        let archive = unparsed_for_folder(&layout.view_states, r"C:\X");
        assert_eq!(archive.len(), 1);
        assert_eq!(archive[0].0, r"c:\X");
        assert_eq!(serde_json::to_value(&archive[0].1).unwrap(), loser);
        layout.view_unparsed.extend(archive);
        apply_patch(&mut layout.view_states, r"C:\X", Patch { sort: Some(-1), ..Default::default() }, ctx(96));
        assert_eq!(layout.view_states.len(), 1);
        assert_eq!(resolve(r"C:\X", &layout.view_states, false).sort, -1);
        assert_eq!(serde_json::to_value(&layout.view_unparsed[r"c:\X"]).unwrap(), loser);
    }
}

#[cfg(test)]
mod details_tests {
    use super::*;
    #[test]
    fn legacy_only_gate_columns_survive_toggle_resize_and_reload() {
        for raw in [json!([{"fmtid":"legacy", "pid":2, "width":99, "extra":{"keep":true}}]),
            json!([false, 42, null, {"unknown":[1,2]}, {"key":"future", "extra":true}])] {
            let mut store: ViewStates = serde_json::from_value(json!({"folder":{"set":true,"view_mode":4,"col_info":raw,"extra":12}})).unwrap();
            let mut columns = details_columns("folder", &store, true);
            assert_eq!(columns.0, raw.as_array().unwrap().clone());
            assert!(columns.visible().is_empty(), "opaque entries are never displayed");
            columns.toggle(ColumnKey::Size);
            set_details_columns(&mut store, "folder", &columns);
            store = serde_json::from_value(serde_json::to_value(&store).unwrap()).unwrap();
            let mut columns = details_columns("folder", &store, true);
            columns.resize(ColumnKey::Size, 123);
            set_details_columns(&mut store, "folder", &columns);
            let saved = serde_json::to_value(&store).unwrap();
            assert_eq!(&saved["folder"]["col_info"].as_array().unwrap()[..raw.as_array().unwrap().len()], raw.as_array().unwrap());
            assert_eq!(saved["folder"]["extra"], 12);
            let reload: ViewStates = serde_json::from_value(saved).unwrap();
            assert_eq!(details_columns("folder", &reload, true).visible(), vec![(ColumnKey::Size,123)]);
        }
    }
    #[test]
    fn column_patch_does_not_activate_unset_view_or_sort() {
        let ignored = json!({"set":false,"view_mode":1,"icon_size":96,"sort_mode":-2,
            "col_info":[{"key":"type","width":90},{"legacy":true},{"legacy":true}],"view_flags":128,"extra":{"keep":1}});
        let mut layout = crate::model::Layout::new();
        layout.view_states = serde_json::from_value(json!({r"C:\parent":{"set":true,"view_mode":4,"sort_mode":-3},
            r"C:\parent\child":ignored})).unwrap();
        let mut columns = details_columns(r"C:\parent\child", &layout.view_states, true);
        columns.resize(ColumnKey::Size, 111);
        layout.view_unparsed.extend(unparsed_for_folder(&layout.view_states, r"C:\parent\child"));
        set_details_columns(&mut layout.view_states, r"C:\parent\child", &columns);
        let reload: crate::model::Layout = serde_json::from_value(serde_json::to_value(&layout).unwrap()).unwrap();
        let child = serde_json::to_value(stored(&reload.view_states, r"C:\parent\child").unwrap()).unwrap();
        for field in ["view_mode", "icon_size", "sort_mode"] { assert!(child.get(field).is_none()); }
        assert_eq!(child["set"], true);
        assert_eq!(child["view_flags"], 128);
        assert_eq!(child["extra"], ignored["extra"]);
        assert_eq!(child["col_info"], json!([{"key":"size","width":111},{"legacy":true},{"legacy":true}]));
        assert_eq!(serde_json::to_value(&reload.view_unparsed[r"C:\parent\child"]).unwrap(), ignored);
        let resolved = resolve_view(r"C:\parent\child", &reload.view_states, true, Context {desktop_icon_size:32});
        assert_eq!(resolved.style.mode, 4);
        assert_eq!(resolved.sort, -3);
    }
    #[test]
    fn gate_reset_removes_drawn_columns_and_preserves_opaque_entries() {
        for raw in [json!([{"key":"size","width":123},{"key":"type","extra":true}]),
            json!([{"key":"size","width":123},{"fmtid":"legacy","pid":2,"extra":true},false,{"key":"future","x":1}])] {
            let mut store: ViewStates = serde_json::from_value(json!({"folder":{"set":true,"view_mode":4,"sort_mode":-2,"col_info":raw,"extra":42},
                "other":{"set":true,"col_info":[{"key":"size","width":99}]}})).unwrap();
            let before = serde_json::to_value(&store).unwrap();
            reset_details_columns(&mut store, "folder");
            let reload: ViewStates = serde_json::from_value(serde_json::to_value(&store).unwrap()).unwrap();
            let saved = serde_json::to_value(&reload).unwrap();
            let mut expected = DetailsColumns::parse(Some(&raw));
            expected.reset();
            if expected.0.is_empty() {
                assert!(saved["folder"].get("col_info").is_none());
                assert_eq!(details_columns("folder", &reload, false), DetailsColumns::default());
            } else { assert_eq!(saved["folder"]["col_info"], json!(expected.0)); }
            assert_eq!(saved["folder"]["view_mode"], 4);
            assert_eq!(saved["folder"]["sort_mode"], -2);
            assert_eq!(saved["folder"]["extra"], 42);
            assert_eq!(saved["other"], before["other"]);
        }
    }
    #[test]
    fn tolerant_columns_preserve_unknown_entries_through_resize_toggle_reset_and_reload() {
        let raw = json!([{"key":"size","width":2,"extra":true}, {"key":"future","width":75,"other":[1]}, false, {"key":"type","width":"bad"}, {"key":"size","width":900}]);
        let mut c = DetailsColumns::parse(Some(&raw));
        assert_eq!(c.visible(), vec![(ColumnKey::Size,40),(ColumnKey::Type,80)]);
        c.resize(ColumnKey::Size,99);
        assert_eq!(c.0[0]["extra"], true);
        let unknown = c.0[1].clone();
        c.toggle(ColumnKey::Type);
        c.toggle(ColumnKey::DateModified);
        assert_eq!(DetailsColumns::parse(Some(&json!(c.0))),c);
        c.reset();
        assert!(c.visible().is_empty());
        assert_eq!(c.0[0], unknown);
        assert_eq!(c.0[1], false);
        assert_eq!(DetailsColumns::parse(None), DetailsColumns::default());
        assert_eq!(DetailsColumns::parse(Some(&json!({}))), DetailsColumns::default());
        assert!(DetailsColumns::parse(Some(&json!([]))).visible().is_empty());
    }
    #[test]
    fn gate_col_info_inherits_and_writes_only_current_folder() {
        let mut store: ViewStates = serde_json::from_value(json!({"c:\\parent":{"set":true,"view_mode":4,"sort_mode":-3,"col_info":[{"key":"future","width":50},{"key":"type","width":90}]},"c:\\parent\\child":{"set":true,"view_mode":3,"sort_mode":2,"extra":123},"other":[false]})).unwrap();
        let before = serde_json::to_value(&store).unwrap();
        let mut c = details_columns(r"C:\parent\child", &store, true);
        assert_eq!(c.visible(),vec![(ColumnKey::Type,90)]);
        c.resize(ColumnKey::Type,105);
        set_details_columns(&mut store,r"C:\parent\child",&c);
        let after = serde_json::to_value(&store).unwrap();
        assert_eq!(after[r"c:\parent"],before[r"c:\parent"]);
        assert_eq!(after["other"],before["other"]);
        assert_eq!(after[r"c:\parent\child"]["view_mode"],3);
        assert_eq!(after[r"c:\parent\child"]["sort_mode"],2);
        assert_eq!(after[r"c:\parent\child"]["extra"],123);
        let reload: ViewStates = serde_json::from_value(after).unwrap();
        assert_eq!(details_columns(r"c:\parent\child",&reload,true), c);
        assert_eq!(details_columns(r"c:\parent\child\deep",&reload,true), c);
        assert_eq!(details_columns(r"c:\parent\child\deep",&reload,false), DetailsColumns::default());
    }
    #[test]
    fn corral_columns_round_trip_and_no_view_change_changes_membership() {
        let mut layout = crate::model::Layout::new();
        let id = layout.add_group("test".into(),"monitor".into(),crate::geometry::Rect::new(0,0,300,200),crate::model::GroupContent::Items{items:vec!["file".into()]});
        assert_eq!(layout.groups[0].columns(),DetailsColumns::default());
        let c = DetailsColumns::parse(Some(&json!([{"key":"size","width":70},{"key":"future","width":110,"keep":true}])));
        layout.group_mut(id).unwrap().set_columns(&c);
        layout.group_mut(id).unwrap().set_view(View::Details);
        let reload: crate::model::Layout = serde_json::from_value(serde_json::to_value(&layout).unwrap()).unwrap();
        assert_eq!(reload.groups[0].columns(),c);
        assert_eq!(reload.groups[0].content,layout.groups[0].content);
    }
}
