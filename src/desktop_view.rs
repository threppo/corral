//! Read-only snapshot of Explorer's desktop list view (icon positions, size, spacing, flags).
//!
//! Path: `ShellWindows::FindWindowSW(CSIDL_DESKTOP, SWC_DESKTOP)` -> `IServiceProvider` ->
//! `SID_STopLevelBrowser` -> `IShellBrowser::QueryActiveShellView` -> `IFolderView2`.
//! Only getters are called; no `SelectAndPositionItems`, `SetCurrentFolderFlags`,
//! `SetViewModeAndIconSize` or anything else that could change Explorer's saved layout.
//!
//! COM must be initialized by the caller on the calling thread (apartment-threaded,
//! `COINIT_APARTMENTTHREADED`); nothing here initializes it. Item names use the same
//! `SIGDN_DESKTOPABSOLUTEPARSING` form as `crate::shell`.
#![allow(dead_code)]

use std::ffi::c_void;

use windows::Win32::Foundation::POINT;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, IServiceProvider};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    CSIDL_DESKTOP, FOLDERVIEWMODE, FWF_AUTOARRANGE, FWF_SNAPTOGRID, IEnumIDList, IFolderView2, IShellBrowser,
    IShellFolder, IShellItem, IShellWindows, SHCreateItemWithParent, SID_STopLevelBrowser, SIGDN_DESKTOPABSOLUTEPARSING,
    SVGIO_ALLVIEW, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows,
};
use windows::core::{Interface, VARIANT};

/// One icon on the desktop list view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopIcon {
    /// Absolute parsing name, same form as `crate::shell::ShellItem::parsing_name`.
    pub parsing_name: String,
    /// Top-left of the item in client coordinates of the desktop list view, physical pixels.
    pub position: (i32, i32),
}

/// Explorer's current desktop view state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesktopViewInfo {
    pub icons: Vec<DesktopIcon>,
    /// Icon image size in pixels (`IFolderView2::GetViewModeAndIconSize`).
    pub icon_size: u32,
    /// Raw `FOLDERVIEWMODE` value.
    pub view_mode: i32,
    /// Item spacing (`IFolderView::GetSpacing`), pixels.
    pub spacing: (i32, i32),
    /// `FWF_AUTOARRANGE` set.
    pub auto_arrange: bool,
    /// `FWF_SNAPTOGRID` set.
    pub align_to_grid: bool,
}

/// Reads only the icon size (logical DIPs) and the item spacing (px) of the live desktop view.
/// Strictly read-only; cheap enough to repeat whenever the cell inputs are refreshed.
pub fn read_spacing_and_icon_size() -> windows::core::Result<((i32, i32), i32)> {
    let view = desktop_folder_view()?;
    let mut mode = FOLDERVIEWMODE(0);
    let mut size = 0i32;
    unsafe { view.GetViewModeAndIconSize(&mut mode, &mut size)? };
    let mut spacing = POINT::default();
    unsafe { view.GetSpacing(&mut spacing)? };
    Ok(((spacing.x, spacing.y), size))
}

/// Reads the live desktop view out of process. Strictly read-only.
pub fn read_desktop_view() -> windows::core::Result<DesktopViewInfo> {
    let view = desktop_folder_view()?;

    let mut mode = FOLDERVIEWMODE(0);
    let mut size = 0i32;
    unsafe { view.GetViewModeAndIconSize(&mut mode, &mut size)? };

    let mut spacing = POINT::default();
    unsafe { view.GetSpacing(&mut spacing)? };

    let flags = unsafe { view.GetCurrentFolderFlags()? } as i32;

    let folder: IShellFolder = unsafe { view.GetFolder()? };
    let enumerator: IEnumIDList = unsafe { view.Items(SVGIO_ALLVIEW)? };

    let mut icons = Vec::new();
    loop {
        let mut pidl: [*mut ITEMIDLIST; 1] = [std::ptr::null_mut()];
        let mut fetched = 0u32;
        let hr = unsafe { enumerator.Next(&mut pidl, Some(&mut fetched)) };
        if hr.0 != 0 || fetched == 0 || pidl[0].is_null() {
            break;
        }
        let icon = icon_from_pidl(&view, &folder, pidl[0]);
        unsafe { CoTaskMemFree(Some(pidl[0] as *const c_void)) };
        if let Ok(icon) = icon {
            icons.push(icon);
        }
    }

    Ok(DesktopViewInfo {
        icons,
        icon_size: size.max(0) as u32,
        view_mode: mode.0,
        spacing: (spacing.x, spacing.y),
        auto_arrange: flags & FWF_AUTOARRANGE.0 != 0,
        align_to_grid: flags & FWF_SNAPTOGRID.0 != 0,
    })
}

fn desktop_folder_view() -> windows::core::Result<IFolderView2> {
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL)?;
        let location = VARIANT::from(CSIDL_DESKTOP as i32);
        let root = VARIANT::default();
        let mut hwnd = 0i32;
        let dispatch = windows.FindWindowSW(&location, &root, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH)?;
        let provider: IServiceProvider = dispatch.cast()?;
        let browser: IShellBrowser = provider.QueryService(&SID_STopLevelBrowser)?;
        let shell_view = browser.QueryActiveShellView()?;
        shell_view.cast::<IFolderView2>()
    }
}

fn icon_from_pidl(
    view: &IFolderView2,
    folder: &IShellFolder,
    pidl: *const ITEMIDLIST,
) -> windows::core::Result<DesktopIcon> {
    let item: IShellItem = unsafe { SHCreateItemWithParent(None, folder, pidl)? };
    let name = unsafe { item.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING)? };
    let parsing_name = unsafe { name.to_string() }.unwrap_or_default();
    unsafe { CoTaskMemFree(Some(name.0 as *const c_void)) };
    let point = unsafe { view.GetItemPosition(pidl)? };
    Ok(DesktopIcon { parsing_name, position: (point.x, point.y) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

    #[test]
    fn reads_live_desktop_view() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        let info = read_desktop_view().expect("read desktop view");
        println!(
            "icons={} icon_size={} view_mode={} spacing={:?} auto_arrange={} align_to_grid={}",
            info.icons.len(),
            info.icon_size,
            info.view_mode,
            info.spacing,
            info.auto_arrange,
            info.align_to_grid
        );
        for i in info.icons.iter().take(5) {
            println!("  {:?} at {:?}", i.parsing_name, i.position);
        }
        assert!(info.icon_size > 0);
        assert!(info.icons.iter().all(|i| !i.parsing_name.is_empty()));

        let view: BTreeSet<String> = info.icons.iter().map(|i| i.parsing_name.to_lowercase()).collect();
        let shell: BTreeSet<String> = crate::shell::enumerate_desktop()
            .expect("enumerate desktop")
            .iter()
            .map(|i| i.parsing_name.to_lowercase())
            .collect();
        println!("only in view: {:?}", view.difference(&shell).collect::<Vec<_>>());
        println!("only in shell: {:?}", shell.difference(&view).collect::<Vec<_>>());
        // The view is the visible desktop; `enumerate_desktop` is the namespace-root superset that also
        // lists items Explorer hides (This PC, Libraries, user folder, ...). Every view icon must be in it.
        assert!(view.is_subset(&shell), "view icons missing from shell enumeration");
    }
}
