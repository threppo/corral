//! Light/dark theme of Corral's popup menus (title menus, portal menus, shell context menus).
//!
//! Corral's menus are built in Corral's own process, which has no dark-mode
//! opt-in unless it sets one, so Corral applies Explorer's menu
//! theme, which follows the Windows mode `SystemUsesLightTheme` (observed 2026-10-12: dark menus with
//! SystemUsesLightTheme = AppsUseLightTheme = 0; Corral's were light). Corral is its own process, so it
//! sets the same mode explicitly through the undocumented uxtheme exports used for this purpose by
//! Win32 apps: ordinal 135 `SetPreferredAppMode` (1903+) and ordinal 136 `FlushMenuThemes`.
//! Re-applied on `WM_SETTINGCHANGE` "ImmersiveColorSet".

use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32};
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::{PCSTR, w};

const FORCE_DARK: i32 = 2;
const FORCE_LIGHT: i32 = 3;

/// Windows mode (taskbar/Start/Explorer menus) is light. Missing value = light (pre-1903 default).
pub(crate) fn system_uses_light_theme() -> bool {
    let mut value = 1u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    status.is_err() || value != 0
}

/// Mode passed to `SetPreferredAppMode` for a given Windows-mode setting.
pub fn app_mode(light: bool) -> i32 {
    if light { FORCE_LIGHT } else { FORCE_DARK }
}

/// Apply the current Windows mode to this process's menus. Returns the mode set, or None when the
/// uxtheme exports are unavailable (older Windows: menus stay light, as before).
pub fn apply() -> Option<i32> {
    unsafe {
        let module = LoadLibraryExW(w!("uxtheme.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        let set_mode = GetProcAddress(module, PCSTR(135usize as *const u8))?;
        let flush = GetProcAddress(module, PCSTR(136usize as *const u8))?;
        let set_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(set_mode);
        let flush: unsafe extern "system" fn() = std::mem::transmute(flush);
        let mode = app_mode(system_uses_light_theme());
        set_mode(mode);
        flush();
        Some(mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_windows_mode_forces_dark_menus() {
        assert_eq!(app_mode(false), FORCE_DARK);
        assert_eq!(app_mode(true), FORCE_LIGHT);
    }
}
