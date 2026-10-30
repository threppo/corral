//! Cached Details text. Workers read Shell facts and format them once; paint only reads strings.
use crate::shell::ShellItem;
use crate::view_state::ColumnKey;
use super::App;

pub(super) const TIMER_DETAILS: usize = 0x4445_5441;
#[derive(Clone, Debug, Default)]
pub(super) struct Text { pub size: String, pub kind: String, pub modified: String }
impl Text {
    pub fn get(&self, key: ColumnKey) -> &str { match key { ColumnKey::Size => &self.size, ColumnKey::Type => &self.kind, ColumnKey::DateModified => &self.modified, ColumnKey::Name => "" } }
    pub fn load(item: &ShellItem) -> Self {
        let kind = super::group_menu::sort_key(item, crate::sorting::METHOD_TYPE).type_text;
        format(item.is_folder, item.size, item.modified_unix, kind, size_text, date_text)
    }
}
fn format(folder: bool, size: Option<u64>, date: Option<i64>, kind: String,
    size_format: impl FnOnce(u64) -> String, date_format: impl FnOnce(i64) -> String) -> Text {
    Text { size: if folder { String::new() } else { size.map(size_format).unwrap_or_default() },
        kind, modified: date.map(date_format).unwrap_or_default() }
}
#[repr(C)]
#[derive(Default)]
struct FileTime { low: u32, high: u32 }
#[repr(C)]
#[derive(Default)]
struct SystemTime { year: u16, month: u16, weekday: u16, day: u16, hour: u16, minute: u16, second: u16, millis: u16 }
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetDateFormatEx(locale: *const u16, flags: u32, time: *const SystemTime, format: *const u16, out: *mut u16, count: i32, calendar: *const u16) -> i32;
    fn GetTimeFormatEx(locale: *const u16, flags: u32, time: *const SystemTime, format: *const u16, out: *mut u16, count: i32) -> i32;
}
// Resolve these already loaded kernel exports to avoid duplicate FFI declarations with the
// backup UI's private SYSTEMTIME representation. The ABI is Windows' FILETIME/SYSTEMTIME.
fn local_system_time(file: &FileTime) -> Option<SystemTime> {
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    use windows::core::{w, s};
    type Convert = unsafe extern "system" fn(*const FileTime, *mut SystemTime) -> i32;
    type Local = unsafe extern "system" fn(*const u8, *const SystemTime, *mut SystemTime) -> i32;
    static CONVERT: std::sync::OnceLock<Option<(Convert, Local)>> = std::sync::OnceLock::new();
    let (convert, local) = CONVERT.get_or_init(|| unsafe {
        let module = GetModuleHandleW(w!("kernel32.dll")).ok()?;
        let convert = GetProcAddress(module, s!("FileTimeToSystemTime"))?;
        let local = GetProcAddress(module, s!("SystemTimeToTzSpecificLocalTime"))?;
        Some((std::mem::transmute::<unsafe extern "system" fn() -> isize, Convert>(convert),
            std::mem::transmute::<unsafe extern "system" fn() -> isize, Local>(local)))
    }).as_ref()?;
    let mut utc = SystemTime::default();
    let mut time = SystemTime::default();
    // The local conversion applies the timezone rules at the item's date, including DST.
    (unsafe { convert(file, &mut utc) } != 0
        && unsafe { local(std::ptr::null(), &utc, &mut time) } != 0).then_some(time)
}
fn size_text(size: u64) -> String {
    let Ok(size) = i64::try_from(size) else { return String::new() };
    let mut buffer = [0u16; 128];
    if unsafe { windows::Win32::UI::Shell::StrFormatKBSizeW(size, &mut buffer) }.is_null() { return String::new(); }
    String::from_utf16_lossy(&buffer[..buffer.iter().position(|v| *v == 0).unwrap_or(buffer.len())])
}
fn date_text(unix: i64) -> String {
    let Some(ticks) = unix.checked_add(11_644_473_600).and_then(|s| u64::try_from(s).ok()).and_then(|s| s.checked_mul(10_000_000)) else { return String::new() };
    let file = FileTime { low: ticks as u32, high: (ticks >> 32) as u32 };
    let Some(time) = local_system_time(&file) else { return String::new() };
    let (mut date, mut clock) = ([0u16; 128], [0u16; 128]);
    unsafe {
        // Null locale = user's default. DATE_SHORTDATE; TIME_NOSECONDS (short time).
        let d = GetDateFormatEx(std::ptr::null(), 1, &time, std::ptr::null(), date.as_mut_ptr(), date.len() as i32, std::ptr::null());
        let t = GetTimeFormatEx(std::ptr::null(), 2, &time, std::ptr::null(), clock.as_mut_ptr(), clock.len() as i32);
        if d <= 0 || t <= 0 { return String::new(); }
        format!("{} {}", String::from_utf16_lossy(&date[..d as usize - 1]), String::from_utf16_lossy(&clock[..t as usize - 1]))
    }
}

impl App {
    /// Desktop loading already sampled size/date into ShellItem. Finish type and locale text off-thread.
    pub(super) fn load_details(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::SetTimer;
        let all: Vec<_> = self.items.iter().filter(|i| i.gate.is_none()).map(|i| i.shell.clone()).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        // Replacing the receiver rejects an older reload's answer without retaining a window pointer.
        self.details_rx = Some(rx);
        std::thread::spawn(move || {
            use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
            let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            if initialized {
                let result = all.iter().map(|s| (s.parsing_name.clone(), Text::load(s))).collect();
                let _ = tx.send(result);
                unsafe { CoUninitialize(); }
            }
        });
        unsafe { let _ = SetTimer(self.message_window, TIMER_DETAILS, 50, None); }
    }
    pub(super) fn details_ready(&mut self) {
        use windows::Win32::UI::WindowsAndMessaging::KillTimer;
        let Some(rx) = &self.details_rx else { return };
        let batch = match rx.try_recv() {
            Ok(batch) => Some(batch),
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(_) => None,
        };
        self.details_rx = None;
        unsafe { let _ = KillTimer(self.message_window, TIMER_DETAILS); }
        if let Some(batch) = batch {
            let mut by_name: std::collections::HashMap<_, _> = batch.into_iter().map(|(n, text)| (n.to_lowercase(), text)).collect();
            for item in self.items.iter_mut().filter(|i| i.gate.is_none()) {
                if let Some(text) = by_name.remove(&item.shell.parsing_name.to_lowercase()) { item.details = text; }
            }
            // A stored Type sort also consumes these same cached facts, in every view.
            self.refresh_all();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn injected_formatters_receive_only_available_file_facts() {
        let text = format(false, Some(123), Some(456), "type".into(), |n| format!("size:{n}"), |n| format!("date:{n}"));
        assert_eq!((&*text.size, &*text.modified, &*text.kind), ("size:123", "date:456", "type"));
        let text = format(true, Some(123), None, "folder".into(), |_| panic!("folder size"), |_| panic!("missing date"));
        assert!(text.size.is_empty() && text.modified.is_empty());
        let text = format(false, None, None, String::new(), |_| panic!("missing size"), |_| panic!("missing date"));
        assert!(text.size.is_empty() && text.modified.is_empty());
    }
}
