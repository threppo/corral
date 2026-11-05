//! Shared PIDL ownership and parent-folder binding for Shell selections.

use std::ffi::c_void;

use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG};
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    ILClone, ILFindLastID, ILFree, ILIsEqual, ILRemoveLastID, IShellFolder,
    SHBindToParent, SHParseDisplayName,
};
use windows::core::{Error, HSTRING, Result};

// Keep allocator provenance separate: SHParseDisplayName allocations use CoTaskMemFree,
// while ILClone allocations use ILFree. Both callers previously used these same rules.
pub(super) struct Pidl(pub(super) *mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CoTaskMemFree(Some(self.0 as *const c_void)) };
        }
    }
}

pub(super) struct ClonedPidl(pub(super) *mut ITEMIDLIST);

impl Drop for ClonedPidl {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { ILFree(Some(self.0 as *const ITEMIDLIST)) };
        }
    }
}

pub(super) fn parse(name: &str) -> Result<Pidl> {
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    unsafe { SHParseDisplayName(&HSTRING::from(name), None::<&IBindCtx>, &mut pidl, 0, None)? };
    if pidl.is_null() {
        return Err(Error::new(E_FAIL, "SHParseDisplayName returned no item"));
    }
    Ok(Pidl(pidl))
}

/// Parent-folder PIDL of an absolute PIDL (a clone with the last ID removed).
pub(super) fn parent_pidl(pidl: &Pidl) -> Result<ClonedPidl> {
    let clone = unsafe { ILClone(pidl.0 as *const ITEMIDLIST) };
    if clone.is_null() {
        return Err(Error::new(E_FAIL, "ILClone failed"));
    }
    let clone = ClonedPidl(clone);
    unsafe { let _ = ILRemoveLastID(Some(clone.0)); }
    Ok(clone)
}

/// Keep this owner alive through GetUIObjectOf: child pointers borrow its absolute PIDLs.
pub(super) struct ParentItems {
    pub(super) folder: IShellFolder,
    // Preserve the original guard lifetime and drop order: folder, parent clone, items.
    _parent_pidl: ClonedPidl,
    pidls: Vec<Pidl>,
}

impl ParentItems {
    pub(super) fn children(&self) -> Result<Vec<*const ITEMIDLIST>> {
        let children: Vec<*const ITEMIDLIST> = self.pidls.iter()
            .map(|p| unsafe { ILFindLastID(p.0 as *const ITEMIDLIST) } as *const ITEMIDLIST)
            .collect();
        if children.iter().any(|c| c.is_null()) {
            return Err(Error::new(E_FAIL, "could not determine child item identifiers"));
        }
        Ok(children)
    }
}

/// Parse in selection order, reject different parents, then bind the first item's parent.
/// The caller supplies its existing empty-selection message and mixed-parent explanation.
pub(super) fn bind_items(
    parsing_names: &[String],
    empty_message: &str,
    mixed_parent_explanation: &str,
) -> Result<ParentItems> {
    if parsing_names.is_empty() {
        return Err(Error::new(E_INVALIDARG, empty_message));
    }
    let pidls = parsing_names.iter().map(|n| parse(n)).collect::<Result<Vec<_>>>()?;
    let first_parent = parent_pidl(&pidls[0])?;
    for (i, pidl) in pidls.iter().enumerate().skip(1) {
        let parent = parent_pidl(pidl)?;
        if !unsafe { ILIsEqual(first_parent.0 as *const ITEMIDLIST, parent.0 as *const ITEMIDLIST) }.as_bool() {
            return Err(Error::new(E_INVALIDARG, &format!(
                "items do not share a parent folder (\"{}\" differs from \"{}\"); {}",
                parsing_names[i], parsing_names[0], mixed_parent_explanation
            )));
        }
    }
    let folder: IShellFolder = unsafe { SHBindToParent::<IShellFolder>(pidls[0].0 as *const ITEMIDLIST, None)? };
    Ok(ParentItems { folder, _parent_pidl: first_parent, pidls })
}
