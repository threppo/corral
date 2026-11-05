//! Native Shell context menus for items Corral renders itself.
//!
//! Items are identified by Shell parsing names (`SIGDN_DESKTOPABSOLUTEPARSING`), e.g.
//! `C:\Users\X\Desktop\a.txt` or `::{645FF040-5081-101B-9F08-00AA002F954E}`.
//!
//! Requirements for the caller:
//! * COM must be initialized on the calling thread as an STA (`COINIT_APARTMENTTHREADED`);
//!   nothing here initializes it.
//! * `ShellMenu::show` runs a modal menu loop owned by `owner`. The owner's window procedure must
//!   forward `WM_INITMENUPOPUP`, `WM_DRAWITEM`, `WM_MEASUREITEM` and `WM_MENUCHAR` to
//!   `ShellMenu::handle_menu_msg` (return its `LRESULT` when it yields `Some`, otherwise fall
//!   through to the default handling). Without that, "Open with", "Send to" and "New" submenus
//!   stay empty or are drawn badly. Call `show` only while the `ShellMenu` is alive and reachable
//!   from the window procedure (e.g. stored in app state before calling `show`).
//! * "Rename": Corral draws items itself, so the Shell cannot rename in place. `invoke` refuses the
//!   `rename` verb (returns `Err`); the caller should compare `verb(id)` / `is_rename(id)` after
//!   `show` and start its own in-place rename instead of invoking.
//! * Items must share one parent folder. User-desktop and public-desktop items resolve to
//!   different filesystem parents, so mixed selections yield `Err` (the caller may fall back to a
//!   per-parent menu or the first item's parent).

use std::mem::size_of;

use crate::shell_items_pidl::bind_items;

use windows::Win32::Foundation::{E_INVALIDARG, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::IBindCtx;
use windows::Win32::UI::Shell::{
    BHID_SFObject, CMINVOKECOMMANDINFOEX, IContextMenu,
    IContextMenu2, IContextMenu3, IShellFolder, IShellItem, SHCreateItemFromParsingName,
    SHGetDesktopFolder,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, GetForegroundWindow, HMENU,
    SW_SHOWNORMAL, SetForegroundWindow, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenuEx, WM_DRAWITEM,
    WM_INITMENUPOPUP, WM_MEASUREITEM, WM_MENUCHAR,
};
use windows::core::{Error, HSTRING, Interface, PCSTR, PCWSTR, PSTR, Result};

const FIRST_ID: u32 = 1;
const LAST_ID: u32 = 0x7FFF;
/// The composed Gate body menu also holds Corral's commands (below the Gate ids that end at 0x6124, and from
/// 0x9001 up): the folder's background handlers get the 8K ids between them.
pub const GATE_BACKGROUND_FIRST_ID: u32 = 0x7000;
pub const GATE_BACKGROUND_LAST_ID: u32 = 0x8FFF;

const CMF_NORMAL: u32 = 0x0;
const CMF_CANRENAME: u32 = 0x10;
const CMF_EXTENDEDVERBS: u32 = 0x100;
const GCS_VERBW: u32 = 4;
const CMIC_MASK_UNICODE: u32 = 0x4000;
const CMIC_MASK_SHIFT_DOWN: u32 = 0x1000_0000;
const CMIC_MASK_PTINVOKE: u32 = 0x2000_0000;
const CMIC_MASK_CONTROL_DOWN: u32 = 0x4000_0000;

/// A populated Shell context menu. Destroys its `HMENU` on drop.
pub struct ShellMenu {
    menu: HMENU,
    ctx: IContextMenu,
    ctx2: Option<IContextMenu2>,
    ctx3: Option<IContextMenu3>,
    first: u32,
    last: u32,
    gate_new_submenu: Option<HMENU>,
}

fn invalid(message: &str) -> Error {
    Error::new(E_INVALIDARG, message)
}

/// Remove the `HiddenShellMenuEntries` labels from Corral's Shell menus (host spec 6).
fn filter_gate_entry(menu: HMENU) {
    use crate::portal_menu::{self, ShellItem};
    use windows::Win32::UI::WindowsAndMessaging::*;
    let count = unsafe { GetMenuItemCount(menu) }.max(0) as u32;
    let mut items = Vec::new();
    for position in 0..count {
        let mut text = [0u16; 512];
        let len = unsafe { GetMenuStringW(menu, position, Some(&mut text), MF_BYPOSITION) }.max(0) as usize;
        let submenu = unsafe { GetSubMenu(menu, position as i32) };
        if !submenu.is_invalid() { filter_gate_entry(submenu); }
        let mut item = if unsafe { GetMenuState(menu, position, MF_BYPOSITION) } & MF_SEPARATOR.0 != 0 {
            ShellItem::separator()
        } else { ShellItem::text(unsafe { GetMenuItemID(menu, position as i32) }, &String::from_utf16_lossy(&text[..len])) };
        if !submenu.is_invalid() { item.submenu = Some(Vec::new()); }
        items.push(item);
    }
    for position in portal_menu::removal_plan(&items, portal_menu::hidden_entries()) {
        unsafe { let _ = DeleteMenu(menu, position as u32, MF_BYPOSITION); }
    }
}

impl ShellMenu {
    fn build(
        menu: HMENU,
        ctx: IContextMenu,
        flags: u32,
    ) -> Result<ShellMenu> {
        let menu = Self::build_at(menu, ctx, flags, 0, FIRST_ID, LAST_ID)?;
        filter_gate_entry(menu.menu);
        Ok(menu)
    }

    fn build_at(menu: HMENU, ctx: IContextMenu, flags: u32, at: u32, first: u32, last: u32) -> Result<ShellMenu> {
        let menu = MenuGuard(menu);
        unsafe { ctx.QueryContextMenu(menu.0, at, first, last, flags)? };
        let ctx2 = ctx.cast::<IContextMenu2>().ok();
        let ctx3 = ctx.cast::<IContextMenu3>().ok();
        let hmenu = menu.release();
        Ok(ShellMenu { menu: hmenu, ctx, ctx2, ctx3, first, last, gate_new_submenu: None })
    }

    /// Menu for one or more items that share a parent folder (selection menu).
    /// `CMF_EXTENDEDVERBS` is added when Shift is currently held by the caller's intent: pass it
    /// through [`ShellMenu::for_items_ex`] for that; this variant is the normal menu.
    pub fn for_items(owner: HWND, parsing_names: &[String]) -> Result<ShellMenu> {
        Self::for_items_ex(owner, parsing_names, false)
    }

    /// As [`ShellMenu::for_items`], with `shift` adding `CMF_EXTENDEDVERBS`.
    pub fn for_items_ex(owner: HWND, parsing_names: &[String], shift: bool) -> Result<ShellMenu> {
        let items = bind_items(
            parsing_names, "no items given for the context menu", "a combined menu is not possible",
        )?;
        let children = items.children()?;
        let ctx: IContextMenu = unsafe { items.folder.GetUIObjectOf(owner, &children, None)? };

        let menu = unsafe { CreatePopupMenu()? };
        let mut flags = CMF_NORMAL | CMF_CANRENAME;
        if shift {
            flags |= CMF_EXTENDEDVERBS;
        }
        Self::build(menu, ctx, flags)
    }

    /// Background menu of the desktop (`None`) or of the folder with the given parsing name.
    pub fn for_background(owner: HWND, folder_parsing_name: Option<&str>) -> Result<ShellMenu> {
        let ctx = Self::background_context(owner, folder_parsing_name)?;
        let menu = unsafe { CreatePopupMenu()? };
        Self::build(menu, ctx, CMF_NORMAL)
    }

    fn background_context(owner: HWND, folder_parsing_name: Option<&str>) -> Result<IContextMenu> {
        let folder: IShellFolder = match folder_parsing_name {
            None => unsafe { SHGetDesktopFolder()? },
            Some(name) => {
                let item: IShellItem = unsafe { SHCreateItemFromParsingName(&HSTRING::from(name), None::<&IBindCtx>)? };
                unsafe { item.BindToHandler(None::<&IBindCtx>, &BHID_SFObject)? }
            }
        };
        unsafe { folder.CreateViewObject(owner) }
    }

    /// Takes ownership of a Gate menu prefix and appends only New / Properties for its folder.
    /// Paste / Paste shortcut are supplied by the caller and invoked by canonical verb.
    pub fn for_gate_background(owner: HWND, folder: &str, prefix: HMENU) -> Result<ShellMenu> {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let prefix = MenuGuard(prefix);
        let ctx = Self::background_context(owner, Some(folder))?;
        let start = unsafe { GetMenuItemCount(prefix.0) }.max(0) as u32;
        let mut menu = Self::build_at(prefix.release(), ctx, CMF_NORMAL, start, GATE_BACKGROUND_FIRST_ID, GATE_BACKGROUND_LAST_ID)?;
        let count = unsafe { GetMenuItemCount(menu.menu) }.max(0) as u32;
        let mut kept = Vec::new();
        let mut remove = Vec::new();
        for position in start..count {
            let mut info = MENUITEMINFOW { cbSize: size_of::<MENUITEMINFOW>() as u32, fMask: MIIM_ID | MIIM_SUBMENU | MIIM_FTYPE, ..Default::default() };
            if unsafe { GetMenuItemInfoW(menu.menu, position, true, &mut info) }.is_err() || info.fType.contains(MFT_SEPARATOR) {
                remove.push(position);
                continue;
            }
            let verb = menu.verb(info.wID);
            let submenu = !info.hSubMenu.is_invalid();
            // live 2026-10-29: the New submenu item itself answers the canonical verb "New", so no extension
            // submenu is initialized just to be inspected
            match crate::portal_menu::gate_background_entry(verb.as_deref(), submenu) {
                Some(entry) if !kept.contains(&entry) => {
                    if entry == crate::portal_menu::BackgroundEntry::New { menu.gate_new_submenu = Some(info.hSubMenu); }
                    kept.push(entry);
                }
                _ => remove.push(position),
            }
        }
        for position in remove.into_iter().rev() {
            unsafe { let _ = DeleteMenu(menu.menu, position, MF_BYPOSITION); }
        }
        Ok(menu)
    }

    /// Check the populated New subtree after tracking: its children need not have canonical verbs.
    /// The handle belongs to this menu; no labels or command-id ranges identify New's children.
    pub fn is_gate_new(&self, id: u32) -> bool {
        fn contains(menu: HMENU, id: u32) -> bool {
            use windows::Win32::UI::WindowsAndMessaging::{GetMenuItemCount, GetMenuItemID, GetSubMenu};
            for pos in 0..unsafe { GetMenuItemCount(menu) }.max(0) {
                let sub = unsafe { GetSubMenu(menu, pos) };
                if !sub.is_invalid() {
                    if contains(sub, id) { return true; }
                } else if unsafe { GetMenuItemID(menu, pos) } == id { return true; }
            }
            false
        }
        (self.first..=self.last).contains(&id) && self.gate_new_submenu.is_some_and(|menu| contains(menu, id))
    }

    pub fn hmenu(&self) -> HMENU {
        self.menu
    }

    /// Canonical verb (e.g. "delete", "copy", "rename", "properties") for a command id returned by
    /// `show`, when the handler provides one.
    pub fn verb(&self, id: u32) -> Option<String> {
        if id < self.first || id > self.last {
            return None;
        }
        let mut buffer = [0u16; 256];
        let result = unsafe {
            self.ctx.GetCommandString((id - self.first) as usize, GCS_VERBW, None, PSTR(buffer.as_mut_ptr() as *mut u8), buffer.len() as u32)
        };
        if result.is_err() {
            return None;
        }
        let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
        if len == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&buffer[..len]))
    }

    /// True when `id` is the "rename" verb; the caller must run its own in-place rename.
    pub fn is_rename(&self, id: u32) -> bool {
        self.verb(id).is_some_and(|v| v.eq_ignore_ascii_case("rename"))
    }

    /// Invoke a canonical verb (e.g. "paste") that the handler supports even when its menu does not
    /// list it. Never "rename" (see module docs).
    pub fn invoke_verb(&self, owner: HWND, verb: &str, point: (i32, i32)) -> Result<()> {
        if verb.eq_ignore_ascii_case("rename") {
            return Err(invalid("rename is handled by the caller (in-place rename)"));
        }
        let ansi: Vec<u8> = verb.bytes().chain(std::iter::once(0)).collect();
        let wide: Vec<u16> = verb.encode_utf16().chain(std::iter::once(0)).collect();
        let info = CMINVOKECOMMANDINFOEX {
            cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
            fMask: CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE,
            hwnd: owner,
            lpVerb: PCSTR(ansi.as_ptr()),
            lpParameters: PCSTR::null(),
            lpDirectory: PCSTR::null(),
            nShow: SW_SHOWNORMAL.0,
            dwHotKey: 0,
            hIcon: Default::default(),
            lpTitle: PCSTR::null(),
            lpVerbW: PCWSTR(wide.as_ptr()),
            lpParametersW: PCWSTR::null(),
            lpDirectoryW: PCWSTR::null(),
            lpTitleW: PCWSTR::null(),
            ptInvoke: POINT { x: point.0, y: point.1 },
        };
        unsafe { self.ctx.InvokeCommand(&info as *const CMINVOKECOMMANDINFOEX as *const _) }
    }

    /// Invoke a command chosen from the menu. The "rename" verb is refused (see module docs).
    pub fn invoke(&self, owner: HWND, id: u32, point: (i32, i32), shift: bool, ctrl: bool) -> Result<()> {
        if id < self.first || id > self.last {
            return Err(invalid("command id outside the menu range"));
        }
        if self.is_rename(id) {
            return Err(invalid("rename is handled by the caller (in-place rename)"));
        }
        let offset = (id - self.first) as usize;
        let mut mask = CMIC_MASK_UNICODE | CMIC_MASK_PTINVOKE;
        if shift {
            mask |= CMIC_MASK_SHIFT_DOWN;
        }
        if ctrl {
            mask |= CMIC_MASK_CONTROL_DOWN;
        }
        let info = CMINVOKECOMMANDINFOEX {
            cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
            fMask: mask,
            hwnd: owner,
            lpVerb: PCSTR(offset as *const u8),
            lpParameters: PCSTR::null(),
            lpDirectory: PCSTR::null(),
            nShow: SW_SHOWNORMAL.0,
            dwHotKey: 0,
            hIcon: Default::default(),
            lpTitle: PCSTR::null(),
            lpVerbW: PCWSTR(offset as *const u16),
            lpParametersW: PCWSTR::null(),
            lpDirectoryW: PCWSTR::null(),
            lpTitleW: PCWSTR::null(),
            ptInvoke: POINT { x: point.0, y: point.1 },
        };
        unsafe { self.ctx.InvokeCommand(&info as *const CMINVOKECOMMANDINFOEX as *const _) }
    }

    /// Forward owner-window menu messages (see module docs). Returns `Some(result)` when handled.
    pub fn handle_menu_msg(&self, msg: u32, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR => {}
            _ => return None,
        }
        if let Some(ctx3) = &self.ctx3 {
            let mut result = LRESULT(0);
            return unsafe { ctx3.HandleMenuMsg2(msg, wparam, lparam, Some(&mut result as *mut LRESULT)) }.ok().map(|_| result);
        }
        if msg == WM_MENUCHAR {
            return None;
        }
        if let Some(ctx2) = &self.ctx2 {
            return unsafe { ctx2.HandleMenuMsg(msg, wparam, lparam) }.ok().map(|_| LRESULT(0));
        }
        None
    }

    /// Show the menu modally at a screen point; returns the chosen command id (`None` = dismissed).
    /// `owner`'s window procedure must forward menu messages to [`ShellMenu::handle_menu_msg`].
    /// If the chosen verb is "rename" it is only returned, never invoked.
    pub fn show(&self, owner: HWND, screen_point: (i32, i32)) -> Option<u32> {
        unsafe {
            if GetForegroundWindow() != owner {
                let _ = SetForegroundWindow(owner);
            }
            let id = TrackPopupMenuEx(self.menu, (TPM_RETURNCMD | TPM_RIGHTBUTTON).0, screen_point.0, screen_point.1, owner, None);
            if id.0 > 0 { Some(id.0 as u32) } else { None }
        }
    }
}

impl Drop for ShellMenu {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.menu);
        }
    }
}

/// Destroys the menu on early-return paths until ownership moves into `ShellMenu`.
struct MenuGuard(HMENU);

impl MenuGuard {
    fn release(self) -> HMENU {
        let menu = self.0;
        std::mem::forget(self);
        menu
    }
}

impl Drop for MenuGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyMenu(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::UI::WindowsAndMessaging::{GetMenuItemCount, GetMenuItemID, GetMenuStringW, MF_BYPOSITION};
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

    fn init() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
    }

    /// (position, text, verb) for each top-level menu entry. Never invokes anything.
    fn describe(menu: &ShellMenu) -> Vec<(String, Option<String>)> {
        let count = unsafe { GetMenuItemCount(menu.hmenu()) };
        let mut rows = Vec::new();
        for pos in 0..count.max(0) as u32 {
            let mut buffer = [0u16; 256];
            let n = unsafe { GetMenuStringW(menu.hmenu(), pos, Some(&mut buffer), MF_BYPOSITION) };
            let text = String::from_utf16_lossy(&buffer[..n.max(0) as usize]);
            let id = unsafe { GetMenuItemID(menu.hmenu(), pos as i32) };
            let verb = if id != u32::MAX && id != 0 { menu.verb(id) } else { None };
            println!("  [{pos}] id={id} text={text:?} verb={verb:?}");
            rows.push((text, verb));
        }
        rows
    }

    #[test]
    fn background_menu_has_items() {
        init();
        let menu = ShellMenu::for_background(HWND::default(), None).expect("background menu");
        let rows = describe(&menu);
        assert!(rows.len() >= 3, "expected >= 3 items, got {}", rows.len());
        // The folder's own background menu lacks the view-provided Refresh/Paste/View entries
        // (those come from IShellView); it carries New / Display / Personalize instead.
        let known = ["refresh", "paste", "view", "display", "personalize"];
        let hit = rows.iter().any(|(text, verb)| {
            let t = text.to_lowercase();
            let v = verb.clone().unwrap_or_default().to_lowercase();
            known.iter().any(|k| t.contains(k) || v.contains(k))
        });
        let has_submenu = rows.iter().any(|(_, v)| v.is_none());
        assert!(hit || has_submenu, "no refresh/paste/view/display/personalize entry found");
    }

    #[test]
    fn items_menu_for_temp_file() {
        init();
        let dir = std::env::temp_dir().join(format!("corral_shell_menu_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.txt");
        std::fs::write(&file, b"x").unwrap();
        let result = ShellMenu::for_items(HWND::default(), &[file.to_string_lossy().to_string()]);
        let outcome = result.map(|menu| {
            let rows = describe(&menu);
            rows.iter().any(|(_, v)| {
                v.as_deref().is_some_and(|v| v.eq_ignore_ascii_case("delete") || v.eq_ignore_ascii_case("copy"))
            })
        });
        let _ = std::fs::remove_file(&file);
        let _ = std::fs::remove_dir(&dir);
        assert!(outcome.expect("items menu"), "no delete/copy verb");
    }

    #[test]
    fn empty_items_is_error() {
        init();
        assert!(ShellMenu::for_items(HWND::default(), &[]).is_err());
    }

    #[test]
    fn missing_path_is_error() {
        init();
        let missing = std::env::temp_dir().join("corral_shell_menu_does_not_exist_9f3a").join("nope.txt");
        assert!(ShellMenu::for_items(HWND::default(), &[missing.to_string_lossy().to_string()]).is_err());
    }
}
