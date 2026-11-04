//! "Start with Windows" (Settings > Welcome): a task in Task Scheduler that runs Corral when the user logs in.
//!
//! A task and not a `Run` registry value: Explorer works through `Run` one entry at a time after the desktop is up,
//! so it starts with the icons in plain view (50 to 128 s on the user's PC, 2026-11-04), and it skipped Corral's entry
//! with no error. A logon trigger for one's own account needs no administrator rights (MS-TSCH 3.2.5.1.1). Windows is
//! the record of the choice: nothing is stored in the settings file.

use std::os::windows::io::IntoRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Console::{STD_ERROR_HANDLE, SetStdHandle};

const TASK_NAME: &str = "Corral";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What Settings shows; read once when its window opens, then changed by `set`.
static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn cached() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn refresh() {
    ENABLED.store(task_exists(), Ordering::Relaxed);
}

/// Turn it on for the running exe, or off.
pub fn set(on: bool) -> Result<(), String> {
    if on { register()? } else { unregister()? }
    ENABLED.store(on, Ordering::Relaxed);
    Ok(())
}

fn schtasks(args: &[&str]) -> Result<(), String> {
    // by full path: never one found in the current folder
    let system = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    let output = std::process::Command::new(system.join("System32").join("schtasks.exe"))
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&output.stderr).trim().to_string()) }
}

fn task_exists() -> bool {
    schtasks(&["/Query", "/TN", TASK_NAME]).is_ok()
}

fn unregister() -> Result<(), String> {
    if !task_exists() {
        return Ok(());
    }
    schtasks(&["/Delete", "/TN", TASK_NAME, "/F"])
}

fn register() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let user = match (std::env::var("USERDOMAIN"), std::env::var("USERNAME")) {
        (Ok(domain), Ok(name)) => format!("{domain}\\{name}"),
        (_, Ok(name)) => name,
        _ => return Err("no user name".into()),
    };
    create_task(TASK_NAME, &task_xml(&user, &exe))
}

fn create_task(name: &str, xml: &str) -> Result<(), String> {
    let file = std::env::temp_dir().join(format!("corral-start-{}.xml", std::process::id()));
    // the task XML declares UTF-16: a byte order mark and little-endian units
    let bytes: Vec<u8> = std::iter::once(0xFEFFu16).chain(xml.encode_utf16()).flat_map(u16::to_le_bytes).collect();
    std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
    let result = schtasks(&["/Create", "/TN", name, "/XML", &file.to_string_lossy(), "/F"]);
    let _ = std::fs::remove_file(&file);
    result
}

fn xml_text(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// Run as the user, not elevated: an elevated Corral could not take drops from Explorer. A task defaults to below-normal
/// priority (7), which would make Corral slow behind everything else starting at login; 4 is normal. No time limit: it
/// runs until the session ends. On battery too.
fn task_xml(user: &str, exe: &Path) -> String {
    let user = xml_text(user);
    let command = xml_text(&exe.to_string_lossy());
    let folder = xml_text(&exe.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default());
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts Corral when you log in. Turn it off in Corral's Settings.</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{command}</Command><Arguments>--desktop --autostart</Arguments><WorkingDirectory>{folder}</WorkingDirectory></Exec></Actions>
</Task>
"#
    )
}

/// A Corral started at login has no console and nobody watching: keep the last two runs' logs and any panic.
/// Call while this is the only thread and only after taking the desktop lock, so a refused start cannot rotate the
/// logs of the Corral that is running.
pub fn start_logs() {
    let Some(base) = std::env::var_os("LOCALAPPDATA") else { return };
    let dir = PathBuf::from(base).join("Corral").join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    rotate_logs(&dir);
    unsafe {
        std::env::set_var("CORRAL_LOG", dir.join("corral.log"));
        std::env::set_var("RUST_BACKTRACE", "1");
    }
    if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("stderr.log")) {
        unsafe {
            let _ = SetStdHandle(STD_ERROR_HANDLE, HANDLE(file.into_raw_handle()));
        }
    }
}

fn rotate_logs(dir: &Path) {
    for name in ["corral.log", "stderr.log"] {
        let _ = std::fs::rename(dir.join(name), dir.join(format!("previous-{name}")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_task_runs_at_logon_as_the_user_at_normal_priority() {
        let xml = task_xml(r"PC\Ala", Path::new(r"C:\Apps\Corral\corral.exe"));
        assert!(xml.contains(r"<LogonTrigger><Enabled>true</Enabled><UserId>PC\Ala</UserId></LogonTrigger>"));
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"), "not elevated");
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"), "in the user's session");
        assert!(xml.contains("<Priority>4</Priority>"), "the default would be below normal");
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"), "the default would stop it after 3 days");
        assert!(xml.contains(r"<Command>C:\Apps\Corral\corral.exe</Command><Arguments>--desktop --autostart</Arguments><WorkingDirectory>C:\Apps\Corral</WorkingDirectory>"));
    }

    /// Changes the machine for a moment, so not part of the normal run:
    /// `cargo test --offline autostart -- --ignored`. Registers the real file under another name and removes it.
    #[test]
    #[ignore]
    fn windows_accepts_the_task_file_without_administrator_rights() {
        let name = format!("CorralTaskTest-{}", std::process::id());
        let user = format!("{}\\{}", std::env::var("USERDOMAIN").unwrap(), std::env::var("USERNAME").unwrap());
        let created = create_task(&name, &task_xml(&user, Path::new(r"C:\Windows\System32\cmd.exe")));
        let found = schtasks(&["/Query", "/TN", &name]);
        let removed = schtasks(&["/Delete", "/TN", &name, "/F"]);
        assert_eq!((created, found, removed), (Ok(()), Ok(()), Ok(())));
    }

    #[test]
    fn names_and_paths_cannot_break_the_task_file() {
        let xml = task_xml("PC\\A&B", Path::new(r"C:\R&D <new>\it's\corral.exe"));
        assert!(xml.contains(r"<UserId>PC\A&amp;B</UserId>"));
        assert!(xml.contains(r"<Command>C:\R&amp;D &lt;new&gt;\it&apos;s\corral.exe</Command>"));
        assert!(!xml.contains("A&B") && !xml.contains("<new>"));
    }

    #[test]
    fn the_last_two_runs_keep_their_logs() {
        let dir = std::env::temp_dir().join(format!("corral-log-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("corral.log"), "last").unwrap();
        std::fs::write(dir.join("previous-corral.log"), "older").unwrap();
        rotate_logs(&dir);
        assert_eq!(std::fs::read_to_string(dir.join("previous-corral.log")).unwrap(), "last");
        assert!(!dir.join("corral.log").exists());
        rotate_logs(&dir); // nothing to rotate: the older one stays
        assert_eq!(std::fs::read_to_string(dir.join("previous-corral.log")).unwrap(), "last");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
