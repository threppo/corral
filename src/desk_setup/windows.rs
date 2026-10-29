//! Read-only Windows adapter for desk-setups-spec 2.1-2.3.
//!
//! No raw serial, device path, adapter LUID or target id leaves this adapter.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::c_void;
use std::mem::size_of;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HWND, POINT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint,
};
use windows::Win32::System::Registry::{
    HKEY, KEY_QUERY_VALUE, REG_BINARY, REG_VALUE_TYPE, RegCloseKey, RegQueryValueExW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::core::{PCWSTR, w};

use super::{Connector, Model, MonitorIdentity, MonitorState};
use crate::geometry::Monitor;

/// The geometry list is already enumerated by Corral. Join active paths by the
/// source GDI name only during this read (desk-setups-spec 2.1).
pub fn read_monitors(monitors: &[Monitor]) -> Vec<MonitorState> {
    let identities = read_identities();
    let left = monitors.iter().map(|m| m.bounds.left).min().unwrap_or(0);
    let top = monitors.iter().map(|m| m.bounds.top).min().unwrap_or(0);
    let mut result: Vec<MonitorState> = monitors
        .iter()
        .map(|monitor| {
            let (dpi, primary) = monitor_dpi_primary(monitor);
            MonitorState {
                id: identities
                    .get(&monitor.name.to_uppercase())
                    .cloned()
                    .unwrap_or_default(),
                size_px: (
                    monitor.bounds.width().max(0) as u32,
                    monitor.bounds.height().max(0) as u32,
                ),
                scale_pct: ((u64::from(dpi) * 100 + 48) / 96) as u32,
                offset_px: (
                    monitor.bounds.left.saturating_sub(left),
                    monitor.bounds.top.saturating_sub(top),
                ),
                primary,
                gdi_name: monitor.name.clone(),
                work: monitor.work,
                ..Default::default()
            }
        })
        .collect();
    discard_duplicate_serials(&mut result);
    result
}

fn monitor_dpi_primary(monitor: &Monitor) -> (u32, bool) {
    // The top-left pixel is guaranteed to lie on this monitor, unlike a centre
    // calculated using potentially overflowing virtual-screen coordinates.
    let handle = unsafe {
        MonitorFromPoint(
            POINT {
                x: monitor.bounds.left,
                y: monitor.bounds.top,
            },
            MONITOR_DEFAULTTONULL,
        )
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let primary = unsafe { GetMonitorInfoW(handle, &mut info) }.as_bool() && info.dwFlags & 1 != 0;
    let mut x = 0;
    let mut y = 0;
    let dpi = if unsafe { GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut x, &mut y) }.is_ok()
        && x > 0
    {
        x
    } else {
        monitor.dpi.max(1)
    };
    (dpi, primary)
}

fn active_paths() -> Option<Vec<DISPLAYCONFIG_PATH_INFO>> {
    // A display may change between the sizing and query calls. Retry only that
    // documented race, with a bound so a flapping cable cannot block the app.
    for _ in 0..3 {
        let mut path_count = 0;
        let mut mode_count = 0;
        if unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        } != ERROR_SUCCESS
        {
            return None;
        }
        if path_count > 4096 || mode_count > 16384 {
            return None;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let status = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        if status != ERROR_SUCCESS || path_count as usize > paths.len() {
            return None;
        }
        paths.truncate(path_count as usize);
        return Some(paths);
    }
    None
}

fn read_identities() -> BTreeMap<String, MonitorIdentity> {
    let mut identities = BTreeMap::new();
    for path in active_paths().unwrap_or_default() {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
        source.header.adapterId = path.sourceInfo.adapterId;
        source.header.id = path.sourceInfo.id;
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0 {
            continue;
        }
        let gdi = wide_string(&source.viewGdiDeviceName).to_uppercase();
        // Cloned displays use the first target in Windows path order, even when
        // that target cannot provide an identity (desk-setups-spec 2.3).
        if identities.contains_key(&gdi) {
            continue;
        }
        let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
        target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
        target.header.size = size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
        target.header.adapterId = path.targetInfo.adapterId;
        target.header.id = path.targetInfo.id;
        let mut identity = MonitorIdentity::default();
        if unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } == 0 {
            let name = wide_string(&target.monitorFriendlyDeviceName);
            identity.name = (!name.trim().is_empty()).then_some(name);
            let path = wide_string(&target.monitorDevicePath);
            let edid = read_edid(&path);
            if unsafe { target.flags.Anonymous.value } & 4 != 0 {
                // EDID's manufacturer word has big-endian bytes. Device-info
                // exposes the raw EDID word; prefer the explicit EDID bytes
                // when accessible rather than relying on host byte order.
                let mfr_word = edid
                    .as_deref()
                    .filter(|e| valid_edid(e))
                    .map(|e| u16::from_be_bytes([e[8], e[9]]))
                    .unwrap_or_else(|| target.edidManufactureId.swap_bytes());
                identity.model = manufacturer(mfr_word).map(|mfr| Model {
                    mfr,
                    product: target.edidProductCodeId,
                });
                identity.connector = Some(Connector {
                    tech: target.outputTechnology.0 as u32,
                    instance: target.connectorInstance,
                });
                if let (Some(model), Some(serial)) = (
                    identity.model.as_ref(),
                    edid.as_deref().and_then(edid_serial),
                ) {
                    identity.serial_hash =
                        short_hash(&format!("{}:{}:{}", model.mfr, model.product, serial));
                }
            }
        }
        identities.insert(gdi, identity);
    }
    identities
}

fn wide_string(value: &[u16]) -> String {
    String::from_utf16_lossy(&value[..value.iter().position(|&v| v == 0).unwrap_or(value.len())])
}

struct DeviceSet(HDEVINFO);
impl Drop for DeviceSet {
    fn drop(&mut self) {
        let _ = unsafe { SetupDiDestroyDeviceInfoList(self.0) };
    }
}
struct RegistryKey(HKEY);
impl Drop for RegistryKey {
    fn drop(&mut self) {
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn read_edid(path: &str) -> Option<Vec<u8>> {
    if path.is_empty() {
        return None;
    }
    // SetupAPI treats the interface path as opaque. No hard-coded HKLM path or
    // device-instance string parsing is needed (desk-setups-spec 2.1).
    let set = DeviceSet(unsafe { SetupDiCreateDeviceInfoList(None, HWND::default()) }.ok()?);
    let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    let mut interface = SP_DEVICE_INTERFACE_DATA {
        cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
        ..Default::default()
    };
    unsafe { SetupDiOpenDeviceInterfaceW(set.0, PCWSTR(path.as_ptr()), 0, Some(&mut interface)) }
        .ok()?;
    let mut device = SP_DEVINFO_DATA {
        cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    };
    // The documented no-detail-buffer form fills DeviceInfoData and returns
    // ERROR_INSUFFICIENT_BUFFER. No dynamic interface-path buffer is required.
    let detail = unsafe {
        SetupDiGetDeviceInterfaceDetailW(set.0, &interface, None, 0, None, Some(&mut device))
    };
    if !detail
        .is_err_and(|e| e.code() == windows::core::HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0))
    {
        return None;
    }
    let key = RegistryKey(
        unsafe {
            SetupDiOpenDevRegKey(
                set.0,
                &device,
                DICS_FLAG_GLOBAL.0,
                0,
                DIREG_DEV,
                KEY_QUERY_VALUE.0,
            )
        }
        .ok()?,
    );
    let mut length = 0;
    let mut kind = REG_VALUE_TYPE::default();
    if unsafe {
        RegQueryValueExW(
            key.0,
            w!("EDID"),
            None,
            Some(&mut kind),
            None,
            Some(&mut length),
        )
    } != ERROR_SUCCESS
        || kind != REG_BINARY
        || !(128..=32768).contains(&length)
    {
        return None;
    }
    let mut bytes = vec![0; length as usize];
    if unsafe {
        RegQueryValueExW(
            key.0,
            w!("EDID"),
            None,
            Some(&mut kind),
            Some(bytes.as_mut_ptr()),
            Some(&mut length),
        )
    } != ERROR_SUCCESS
        || kind != REG_BINARY
        || length as usize > bytes.len()
    {
        return None;
    }
    bytes.truncate(length as usize);
    Some(bytes)
}

fn manufacturer(word: u16) -> Option<String> {
    let values = [(word >> 10) & 31, (word >> 5) & 31, word & 31];
    if word & 0x8000 != 0 || values.iter().any(|&v| !(1..=26).contains(&v)) {
        return None;
    }
    Some(
        values
            .iter()
            .map(|&v| (b'A' + v as u8 - 1) as char)
            .collect(),
    )
}

fn valid_edid(edid: &[u8]) -> bool {
    edid.len() >= 128
        && edid[..8] == [0, 255, 255, 255, 255, 255, 255, 0]
        && edid[..128].iter().fold(0u8, |sum, &v| sum.wrapping_add(v)) == 0
}

fn edid_serial(edid: &[u8]) -> Option<String> {
    if !valid_edid(edid) {
        return None;
    }
    for descriptor in edid[54..126].chunks_exact(18) {
        if descriptor[..5] != [0, 0, 0, 0xff, 0] {
            continue;
        }
        let bytes = &descriptor[5..];
        let end = bytes
            .iter()
            .position(|&v| v == 0 || v == b'\n' || v == b'\r')
            .unwrap_or(bytes.len());
        if !bytes[..end]
            .iter()
            .all(|v| v.is_ascii() && !v.is_ascii_control())
        {
            continue;
        }
        let serial = String::from_utf8_lossy(&bytes[..end]).trim().to_owned();
        if !serial.is_empty()
            && !serial.chars().all(|c| c == '0' || c.is_whitespace())
            && serial != "01010101"
        {
            return Some(serial);
        }
    }
    let numeric = u32::from_le_bytes(edid[12..16].try_into().ok()?);
    (numeric != 0 && numeric != 0x01010101).then(|| numeric.to_string())
}

fn discard_duplicate_serials(monitors: &mut [MonitorState]) {
    let mut seen = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for monitor in monitors.iter() {
        if let (Some(model), Some(serial)) = (&monitor.id.model, &monitor.id.serial_hash) {
            let key = (model.mfr.clone(), model.product, serial.clone());
            if !seen.insert(key.clone()) {
                duplicates.insert(key);
            }
        }
    }
    for monitor in monitors {
        if let (Some(model), Some(serial)) = (&monitor.id.model, &monitor.id.serial_hash) {
            if duplicates.contains(&(model.mfr.clone(), model.product, serial.clone())) {
                monitor.id.serial_hash = None;
            }
        }
    }
}

#[link(name = "bcrypt")]
unsafe extern "system" {
    fn BCryptOpenAlgorithmProvider(
        handle: *mut *mut c_void,
        algorithm: *const u16,
        implementation: *const u16,
        flags: u32,
    ) -> i32;
    fn BCryptHash(
        handle: *mut c_void,
        secret: *const u8,
        secret_len: u32,
        input: *const u8,
        input_len: u32,
        output: *mut u8,
        output_len: u32,
    ) -> i32;
    fn BCryptCloseAlgorithmProvider(handle: *mut c_void, flags: u32) -> i32;
}

fn short_hash(value: &str) -> Option<String> {
    let input_len = value.len().try_into().ok()?;
    let mut algorithm = std::ptr::null_mut();
    if unsafe {
        BCryptOpenAlgorithmProvider(&mut algorithm, w!("SHA256").as_ptr(), std::ptr::null(), 0)
    } < 0
    {
        return None;
    }
    let mut digest = [0u8; 32];
    let result = unsafe {
        BCryptHash(
            algorithm,
            std::ptr::null(),
            0,
            value.as_ptr(),
            input_len,
            digest.as_mut_ptr(),
            digest.len() as u32,
        )
    };
    let _ = unsafe { BCryptCloseAlgorithmProvider(algorithm, 0) };
    (result >= 0).then(|| {
        digest[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edid(numeric: u32, descriptor: Option<&str>) -> Vec<u8> {
        let mut bytes = vec![0; 128];
        bytes[..8].copy_from_slice(&[0, 255, 255, 255, 255, 255, 255, 0]);
        bytes[12..16].copy_from_slice(&numeric.to_le_bytes());
        if let Some(serial) = descriptor {
            bytes[57] = 0xff;
            let serial = serial.as_bytes();
            bytes[59..59 + serial.len()].copy_from_slice(serial);
        }
        bytes[127] = 0u8.wrapping_sub(bytes[..127].iter().fold(0u8, |sum, &v| sum.wrapping_add(v)));
        bytes
    }

    #[test]
    fn descriptor_serial_wins_and_is_trimmed() {
        assert_eq!(
            edid_serial(&edid(42, Some(" ABC123 \n"))),
            Some("ABC123".into())
        );
    }

    #[test]
    fn absent_or_placeholder_descriptor_uses_numeric_serial() {
        assert_eq!(edid_serial(&edid(42, None)), Some("42".into()));
        assert_eq!(edid_serial(&edid(42, Some("000000"))), Some("42".into()));
        assert_eq!(edid_serial(&edid(0, Some("01010101"))), None);
        assert_eq!(edid_serial(&edid(0x01010101, None)), None);
        assert_eq!(edid_serial(&edid(0, None)), None);
    }

    #[test]
    fn invalid_edid_never_produces_a_serial() {
        let mut bytes = edid(42, Some("ABC"));
        bytes[17] ^= 1;
        assert_eq!(edid_serial(&bytes), None);
        assert_eq!(edid_serial(&bytes[..64]), None);
    }

    #[test]
    fn manufacturer_uses_three_letter_edid_encoding() {
        assert_eq!(manufacturer(0x10ac), Some("DEL".into()));
        assert_eq!(manufacturer(0), None);
        assert_eq!(manufacturer(0xffff), None);
    }

    #[test]
    fn short_serial_hash_is_sha256_prefix() {
        assert_eq!(short_hash("abc"), Some("ba7816bf8f01cfea".into()));
    }

    #[test]
    fn duplicate_serials_are_discarded_only_for_the_same_model() {
        let monitor = |product| MonitorState {
            id: MonitorIdentity {
                model: Some(Model {
                    mfr: "DEL".into(),
                    product,
                }),
                serial_hash: Some("0123456789abcdef".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut monitors = vec![monitor(1), monitor(1), monitor(2)];
        discard_duplicate_serials(&mut monitors);
        assert!(monitors[0].id.serial_hash.is_none());
        assert!(monitors[1].id.serial_hash.is_none());
        assert!(monitors[2].id.serial_hash.is_some());
    }
}
