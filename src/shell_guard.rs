//! Keeps chosen shell extensions out of Corral's process.
//!
//! Shell context menus built by Corral (portal body menus, desktop/item menus) make the shell load
//! every registered handler in-process. Some handlers must not run inside Corral (another desktop
//! organizer's menu handler, for example, adds its own entries to Gate menus). The setting
//! `BlockedShellExtensions` lists them; at start-up Corral looks (read-only) for shell-extension
//! handlers that match and registers a refusing class factory for each CLSID with
//! `CoRegisterClassObject`. COM serves in-process activations in this process from that table
//! first, so the DLL is never loaded and the shell skips the handler. Packaged verb handlers
//! (Windows 11 menu) whose package family matches are refused the same way. Nothing outside the
//! process changes: no registry writes, no system-wide class registration, Explorer keeps its
//! handlers. The list is empty by default.

use std::ffi::c_void;

use windows::Win32::Foundation::{BOOL, CLASS_E_CLASSNOTAVAILABLE, ERROR_SUCCESS};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoRegisterClassObject, IClassFactory, IClassFactory_Impl, REGCLS_MULTIPLEUSE};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CLASSES_ROOT, KEY_READ, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};
use windows::core::{GUID, HSTRING, IUnknown, PWSTR, Result, implement};

/// ProgID roots whose `shellex` handlers the shell loads for Corral's menus and drops.
const ROOTS: [&str; 8] = ["*", "AllFilesystemObjects", "Folder", "Directory", "Directory\\Background", "DesktopBackground", "Drive", "lnkfile"];
const HANDLER_KINDS: [&str; 4] = ["ContextMenuHandlers", "DragDropHandlers", "PropertySheetHandlers", "CopyHookHandlers"];

#[implement(IClassFactory)]
struct Refuse;

impl IClassFactory_Impl for Refuse_Impl {
    fn CreateInstance(&self, _outer: Option<&IUnknown>, _iid: *const GUID, object: *mut *mut c_void) -> Result<()> {
        if !object.is_null() {
            unsafe { *object = std::ptr::null_mut() };
        }
        Err(CLASS_E_CLASSNOTAVAILABLE.into())
    }
    fn LockServer(&self, _lock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// Whether an `InprocServer32` path matches a `BlockedShellExtensions` entry: an entry with a
/// backslash is a folder fragment found anywhere in the path, any other entry a prefix of the
/// module file name. Case is ignored.
fn is_blocked_server(path: &str, patterns: &[String]) -> bool {
    let lower = path.to_ascii_lowercase().replace('/', "\\");
    let file = lower.rsplit('\\').next().unwrap_or(&lower).trim_matches('"');
    patterns.iter().map(|p| p.to_ascii_lowercase().replace('/', "\\")).filter(|p| !p.is_empty()).any(|p| {
        if p.contains('\\') { lower.contains(&p) } else { file.starts_with(&p) }
    })
}

/// Whether a package family name starts with a `BlockedShellExtensions` entry (one without a
/// backslash).
fn is_blocked_package(name: &str, patterns: &[String]) -> bool {
    let lower = name.to_ascii_lowercase();
    patterns.iter().map(|p| p.to_ascii_lowercase()).any(|p| !p.is_empty() && !p.contains('\\') && lower.starts_with(&p))
}

/// The handler's CLSID: its default value, or the key name itself.
fn handler_clsid(key_path: &str, name: &str) -> Option<String> {
    let value = reg_string(&format!("{key_path}\\{name}"), None);
    [value.as_deref(), Some(name)].into_iter().flatten().find(|v| v.starts_with('{') && v.ends_with('}')).map(str::to_string)
}

fn reg_string(subkey: &str, value: Option<&str>) -> Option<String> {
    let mut buffer = [0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let value = value.map(HSTRING::from);
    let status = unsafe {
        RegGetValueW(
            HKEY_CLASSES_ROOT,
            &HSTRING::from(subkey),
            value.as_ref().map_or(windows::core::PCWSTR::null(), |v| windows::core::PCWSTR(v.as_ptr())),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            None,
            Some(buffer.as_mut_ptr() as *mut c_void),
            Some(&mut size),
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..len]))
}

fn subkeys(path: &str) -> Vec<String> {
    let mut key = HKEY::default();
    if unsafe { RegOpenKeyExW(HKEY_CLASSES_ROOT, &HSTRING::from(path), 0, KEY_READ, &mut key) } != ERROR_SUCCESS {
        return Vec::new();
    }
    let mut names = Vec::new();
    for index in 0.. {
        let mut buffer = [0u16; 260];
        let mut len = buffer.len() as u32;
        if unsafe { RegEnumKeyExW(key, index, PWSTR(buffer.as_mut_ptr()), &mut len, None, PWSTR::null(), None, None) } != ERROR_SUCCESS {
            break;
        }
        names.push(String::from_utf16_lossy(&buffer[..len as usize]));
    }
    let _ = unsafe { RegCloseKey(key) };
    names
}

fn parse_clsid(text: &str) -> Option<GUID> {
    let hex: String = text.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (hex.len() == 32).then(|| u128::from_str_radix(&hex, 16).ok().map(GUID::from_u128)).flatten()
}

/// Find the shell-extension CLSIDs that match `patterns` (`BlockedShellExtensions`) and refuse
/// them in this process. Call once on the UI thread after `CoInitializeEx`. Returns the blocked
/// CLSIDs (for the log).
pub fn block_extensions(patterns: &[String]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    if patterns.iter().all(|p| p.is_empty()) {
        return found;
    }
    for root in ROOTS {
        for kind in HANDLER_KINDS {
            let path = format!("{root}\\shellex\\{kind}");
            for name in subkeys(&path) {
                let Some(clsid) = handler_clsid(&path, &name) else { continue };
                if found.iter().any(|c| c.eq_ignore_ascii_case(&clsid)) {
                    continue;
                }
                let server = reg_string(&format!("CLSID\\{clsid}\\InprocServer32"), None).unwrap_or_default();
                if is_blocked_server(&server, patterns) {
                    found.push(clsid);
                }
            }
        }
    }
    // Packaged (Windows 11 menu) verb handlers: `HKCR\PackagedCom\Package\<package>\Class\{clsid}`,
    // often served out of process by a COM surrogate. Only the in-process table is used, so
    // Explorer keeps reaching the real one.
    for package in subkeys("PackagedCom\\Package").into_iter().filter(|p| is_blocked_package(p, patterns)) {
        for clsid in subkeys(&format!("PackagedCom\\Package\\{package}\\Class")) {
            if !found.iter().any(|c| c.eq_ignore_ascii_case(&clsid)) {
                found.push(clsid);
            }
        }
    }
    let factory: IClassFactory = Refuse.into();
    found.retain(|clsid| {
        let Some(guid) = parse_clsid(clsid) else { return false };
        // The registration lives as long as the process; it is never revoked.
        unsafe { CoRegisterClassObject(&guid, &factory, CLSCTX_INPROC_SERVER, REGCLS_MULTIPLEUSE) }.is_ok()
    });
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_listed_servers_only() {
        let patterns: Vec<String> = [r"\Acme\Widgets\", "WidgetsMenu", "Acme.Widgets"].map(String::from).to_vec();
        assert!(is_blocked_server(r"C:\Program Files (x86)\Acme\Widgets\Anything64.dll", &patterns));
        assert!(is_blocked_server(r"D:\Apps\W\WidgetsMenu-x64.dll", &patterns));
        assert!(is_blocked_server(r#""C:\X\widgetsmenu64.dll""#, &patterns));
        assert!(!is_blocked_server(r"C:\Windows\System32\shell32.dll", &patterns));
        assert!(!is_blocked_server(r"C:\Users\user\Desktop\widgets\target\debug\corral.exe", &patterns));
        assert!(!is_blocked_server("", &patterns));
        assert!(!is_blocked_server(r"D:\Apps\W\WidgetsMenu-x64.dll", &[]));
        assert!(!is_blocked_server(r"D:\Apps\W\WidgetsMenu-x64.dll", &[String::new()]));
        assert!(is_blocked_package("Acme.Widgets.DesktopMenu_1.0.0.0_neutral__a8b8bqct24ect", &patterns));
        assert!(!is_blocked_package("Microsoft.WindowsTerminal_1.0_x64__8wekyb3d8bbwe", &patterns));
        assert!(!is_blocked_package("Acme.Widgets.DesktopMenu_1.0", &[r"\Acme\Widgets\".to_string()]));
    }

    #[test]
    fn clsid_text_parses() {
        assert_eq!(parse_clsid("{12345678-9ABC-4def-8123-456789ABCDEF}"), Some(GUID::from_u128(0x12345678_9abc_4def_8123_456789abcdef)));
        assert_eq!(parse_clsid("WidgetsShellExt"), None);
    }
}
