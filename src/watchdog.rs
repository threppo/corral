//! Crash watchdog: a second `corral.exe --watchdog` process that waits for the main process to
//! exit (normally or not) and restores the Explorer desktop list view's initial visibility.

use crate::desktop;
use std::os::windows::process::CommandExt;
use std::process::{Child, Command};
use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::System::Threading::{INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub struct Args {
    pub parent_pid: u32,
    pub listview: usize,
    pub explorer_pid: u32,
    pub was_visible: bool,
}

impl Args {
    pub fn to_command_line(&self) -> Vec<String> {
        vec![
            "--watchdog".into(),
            self.parent_pid.to_string(),
            format!("{:#x}", self.listview),
            self.explorer_pid.to_string(),
            (self.was_visible as u8).to_string(),
        ]
    }

    pub fn parse(rest: &[String]) -> Option<Self> {
        let [parent, listview, explorer, visible] = rest else { return None };
        Some(Self {
            parent_pid: parent.parse().ok()?,
            listview: usize::from_str_radix(listview.trim_start_matches("0x"), 16).ok()?,
            explorer_pid: explorer.parse().ok()?,
            was_visible: visible == "1",
        })
    }
}

pub fn spawn(args: &Args) -> std::io::Result<Child> {
    Command::new(std::env::current_exe()?).args(args.to_command_line()).creation_flags(CREATE_NO_WINDOW).spawn()
}

/// Body of the watchdog process. Returns the process exit code.
pub fn run(args: &Args) -> i32 {
    unsafe {
        let Ok(parent) = OpenProcess(PROCESS_SYNCHRONIZE, false, args.parent_pid) else {
            // Parent already gone: restore immediately.
            desktop::restore_listview(HWND(args.listview as *mut _), args.explorer_pid, args.was_visible);
            return 0;
        };
        WaitForSingleObject(parent, INFINITE);
        let _ = CloseHandle(parent);
    }
    let restored = desktop::restore_listview(HWND(args.listview as *mut _), args.explorer_pid, args.was_visible);
    if restored { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_roundtrip() {
        let args = Args { parent_pid: 4242, listview: 0x1011a, explorer_pid: 9932, was_visible: true };
        let line = args.to_command_line();
        assert_eq!(line[0], "--watchdog");
        let parsed = Args::parse(&line[1..]).unwrap();
        assert_eq!((parsed.parent_pid, parsed.listview, parsed.explorer_pid, parsed.was_visible), (4242, 0x1011a, 9932, true));
        assert!(Args::parse(&line[1..3]).is_none());
    }
}
