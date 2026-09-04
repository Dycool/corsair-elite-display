use std::ffi::{OsStr, OsString, c_void};
use std::mem::transmute;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ptr::{null, null_mut};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

const GENERIC_READ: u32 = 0x80000000;
const GENERIC_WRITE: u32 = 0x40000000;
const FILE_SHARE_READ: u32 = 0x00000001;
const FILE_SHARE_WRITE: u32 = 0x00000002;
const OPEN_EXISTING: u32 = 3;
const INVALID_HANDLE_VALUE: *mut c_void = -1isize as *mut c_void;

const GUID_DEVINTERFACE_HID: [u8; 16] = [
    0xb2, 0x55, 0x1e, 0x4d, 0x6f, 0xf1, 0xcf, 0x11, 0x88, 0xcb, 0x00, 0x11, 0x11, 0x00, 0x00, 0x30,
];

const SUPPORTED_PIDS: &[&str] = &[
    "PID_0C39",
    "PID_0C33",
    "PID_0C42",
    "PID_0C4E",
    "PID_0C37",
    "PID_0C40",
    "PID_0C53",
    "PID_0C5B",
];

const COMMANDER_CORE_LCD_PID: &str = "PID_0C39";
const ICUE_LCD_DLL: &str = r"C:\Program Files\Corsair\Corsair iCUE5 Software\iD_BD_x64_cc021.dll";

#[link(name = "cfgmgr32")]
unsafe extern "system" {
    fn CM_Get_Device_Interface_List_SizeW(
        pul_len: *mut u32,
        interface_class_guid: *const u8,
        device_id: *const u16,
        flags: u32,
    ) -> u32;

    fn CM_Get_Device_Interface_ListW(
        interface_class_guid: *const u8,
        device_id: *const u16,
        buffer: *mut u16,
        buffer_len: u32,
        flags: u32,
    ) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        file_name: *const u16,
        desired_access: u32,
        share_mode: u32,
        security_attributes: *mut c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: *mut c_void,
    ) -> *mut c_void;

    fn CloseHandle(handle: *mut c_void) -> i32;
    fn GetLastError() -> u32;
}

#[link(name = "hid")]
unsafe extern "system" {
    fn HidD_SetFeature(
        hid_device_object: *mut c_void,
        report_buffer: *const u8,
        report_buffer_length: u32,
    ) -> u8;

    fn HidD_GetSerialNumberString(
        hid_device_object: *mut c_void,
        buffer: *mut u16,
        buffer_length: u32,
    ) -> u8;
}

type FnOpenDevice = unsafe extern "C" fn(
    dev: *mut *mut c_void,
    vid: u16,
    pid: u16,
    path: *const u16,
) -> i32;
type FnSetSerialNumber = unsafe extern "C" fn(dev: *mut c_void, serial: *const u8, len: u32) -> i32;
type FnSetIdisplaySerialNumber =
    unsafe extern "C" fn(dev: *mut c_void, serial: *const u8, len: u32) -> i32;
type FnSetHardwareMode = unsafe extern "C" fn(dev: *mut c_void, mode: i32) -> i32;
type FnEnterHardwareMode = unsafe extern "C" fn(dev: *mut c_void) -> i32;
type FnSetCustomizedAnimation = unsafe extern "C" fn(dev: *mut c_void, slot: u16) -> i32;
type FnSetBootAnimation = unsafe extern "C" fn(dev: *mut c_void, slot: u16) -> i32;
type FnPlayCustomizedAnimation = unsafe extern "C" fn(dev: *mut c_void) -> i32;
type FnPlayBootAnimation = unsafe extern "C" fn(dev: *mut c_void) -> i32;

fn to_wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn enumerate_hid_paths() -> Vec<String> {
    let mut paths = Vec::new();
    unsafe {
        let mut len = 0u32;
        if CM_Get_Device_Interface_List_SizeW(
            &mut len,
            GUID_DEVINTERFACE_HID.as_ptr(),
            null_mut(),
            0,
        ) != 0
            || len == 0
        {
            return paths;
        }

        let mut buffer = vec![0u16; len as usize];
        if CM_Get_Device_Interface_ListW(
            GUID_DEVINTERFACE_HID.as_ptr(),
            null_mut(),
            buffer.as_mut_ptr(),
            len,
            0,
        ) != 0
        {
            return paths;
        }

        let mut start = 0;
        for index in 0..buffer.len() {
            if buffer[index] == 0 {
                if start < index {
                    paths.push(
                        OsString::from_wide(&buffer[start..index])
                            .to_string_lossy()
                            .to_string(),
                    );
                }
                start = index + 1;
            }
        }
    }
    paths
}

fn hardware_mode_packet_1() -> [u8; 4] {
    [0x03, 0x1e, 0x01, 0x01]
}

fn hardware_mode_packet_2() -> [u8; 4] {
    [0x03, 0x1d, 0x00, 0x01]
}

fn commander_core_refresh_packet() -> [u8; 4] {
    // OpenLinkHub sends this third report when shutting down the Commander Core
    // LCD path. Besides restoring full hardware brightness it also forces the
    // cap to refresh after leaving the software-frame session.
    [0x03, 0x0b, 0x40, 0x01]
}

unsafe fn send_feature_exact(handle: *mut c_void, packet: &[u8; 4]) -> Result<(), String> {
    // OpenLinkHub sends these reports as exactly four bytes. The old code sent
    // a padded 32-byte report first; some Corsair LCD interfaces accept that
    // transfer without actually performing the hardware-mode transition.
    let result = unsafe { HidD_SetFeature(handle, packet.as_ptr(), packet.len() as u32) };
    if result == 0 {
        return Err(format!(
            "HidD_SetFeature failed with Windows error {}",
            unsafe { GetLastError() }
        ));
    }
    Ok(())
}

unsafe fn serial_number(handle: *mut c_void) -> Option<String> {
    let mut buffer = [0u16; 128];
    if unsafe {
        HidD_GetSerialNumberString(
            handle,
            buffer.as_mut_ptr(),
            (buffer.len() * std::mem::size_of::<u16>()) as u32,
        )
    } == 0
    {
        return None;
    }
    let len = buffer.iter().position(|value| *value == 0).unwrap_or(buffer.len());
    String::from_utf16(&buffer[..len]).ok().filter(|value| !value.is_empty())
}

fn restore_persisted_with_icue(serial: Option<&str>) -> Result<(), String> {
    let dll_path = to_wide(ICUE_LCD_DLL);
    let module = unsafe { LoadLibraryW(dll_path.as_ptr()) };
    if module.is_null() {
        return Err("Corsair iCUE LCD DLL is not installed; using native HID restore only".into());
    }

    unsafe {
        let get_fn = |name: &[u8]| -> *mut c_void {
            GetProcAddress(module, name.as_ptr())
                .map(|function| function as *mut c_void)
                .unwrap_or(null_mut())
        };

        let open: Option<FnOpenDevice> = transmute(get_fn(b"iD_USB_open_device_cc021\0"));
        let set_serial: Option<FnSetSerialNumber> =
            transmute(get_fn(b"iD_USB_set_serial_number_cc021\0"));
        let set_idisplay_serial: Option<FnSetIdisplaySerialNumber> =
            transmute(get_fn(b"iD_USB_set_idisplay_serial_number_cc021\0"));
        let set_hardware: Option<FnSetHardwareMode> =
            transmute(get_fn(b"iD_USB_set_hardware_mode_cc021\0"));
        let enter_hardware: Option<FnEnterHardwareMode> =
            transmute(get_fn(b"iD_USB_enter_hardware_mode_cc021\0"));
        let set_customized: Option<FnSetCustomizedAnimation> =
            transmute(get_fn(b"iD_USB_set_customized_animation_cc021\0"));
        let set_boot: Option<FnSetBootAnimation> =
            transmute(get_fn(b"iD_USB_set_boot_animation_cc021\0"));
        let play_customized: Option<FnPlayCustomizedAnimation> =
            transmute(get_fn(b"iD_USB_play_customized_animation_cc021\0"));
        let play_boot: Option<FnPlayBootAnimation> =
            transmute(get_fn(b"iD_USB_play_boot_animation_cc021\0"));

        let Some(open) = open else {
            return Err("Corsair LCD DLL is missing iD_USB_open_device_cc021".into());
        };
        let Some(set_hardware) = set_hardware else {
            return Err("Corsair LCD DLL is missing iD_USB_set_hardware_mode_cc021".into());
        };
        let Some(enter_hardware) = enter_hardware else {
            return Err("Corsair LCD DLL is missing iD_USB_enter_hardware_mode_cc021".into());
        };

        let mut device: *mut c_void = null_mut();
        let open_result = open(&mut device, 0x1b1c, 0x0c39, null());
        if open_result != 0 || device.is_null() {
            return Err(format!(
                "Corsair LCD DLL could not open the Commander Core LCD (result {open_result})"
            ));
        }

        if let Some(serial) = serial {
            let mut serial_bytes = serial.as_bytes().to_vec();
            serial_bytes.push(0);
            if let Some(function) = set_serial {
                function(device, serial_bytes.as_ptr(), (serial_bytes.len() - 1) as u32);
            }
            if let Some(function) = set_idisplay_serial {
                function(device, serial_bytes.as_ptr(), (serial_bytes.len() - 1) as u32);
            }
        }

        // This is the same persisted-image activation path used after the app
        // successfully flashes a hardware image, but without rewriting flash.
        // Re-select slot 0 and explicitly ask the controller to play its stored
        // content before entering hardware mode.
        let set_result = set_hardware(device, 0);
        if let Some(function) = set_customized {
            function(device, 0);
        }
        if let Some(function) = set_boot {
            function(device, 0);
        }
        if let Some(function) = play_customized {
            function(device);
        }
        if let Some(function) = play_boot {
            function(device);
        }
        let enter_result = enter_hardware(device);

        // Keep the DLL loaded for the application lifetime. This mirrors the
        // existing hardware-image flashing path and avoids invalidating any
        // controller state that the vendor DLL may finish asynchronously.
        thread::sleep(Duration::from_millis(30));

        if set_result != 0 {
            Err(format!(
                "Corsair LCD DLL rejected hardware image mode (result {set_result})"
            ))
        } else if enter_result != 0 {
            Err(format!(
                "Corsair LCD DLL rejected hardware-mode entry (result {enter_result})"
            ))
        } else {
            Ok(())
        }
    }
}

fn write_restore_diagnostics(lines: &[String]) {
    let _ = std::fs::write(
        std::env::temp_dir().join("corsair-elite-display-hardware-restore.txt"),
        format!("{}\n", lines.join("\n")),
    );
}

/// Forces any supported Corsair LCD back to its persisted hardware screen.
///
/// The restore is deliberately stronger than simply closing the streaming HID
/// handle. First it sends the exact four-byte feature reports used by
/// OpenLinkHub. Commander Core LCD caps also receive their third refresh report.
/// When Corsair's LCD DLL is available, the stored image/animation slot is then
/// explicitly selected and played before entering hardware mode. This avoids a
/// valid-but-stale software frame remaining latched on the panel.
pub fn restore_hardware_mode() -> Result<(), String> {
    let candidates: Vec<String> = enumerate_hid_paths()
        .into_iter()
        .filter(|path| {
            let upper = path.to_ascii_uppercase();
            upper.contains("VID_1B1C") && SUPPORTED_PIDS.iter().any(|pid| upper.contains(pid))
        })
        .collect();

    if candidates.is_empty() {
        return Err("No supported Corsair LCD found".into());
    }

    let first = hardware_mode_packet_1();
    let second = hardware_mode_packet_2();
    let refresh = commander_core_refresh_packet();
    let mut hid_succeeded = false;
    let mut commander_core_seen = false;
    let mut commander_core_serial: Option<String> = None;
    let mut last_error = String::new();
    let mut diagnostics = Vec::new();

    for path in candidates {
        let upper = path.to_ascii_uppercase();
        let is_commander_core = upper.contains(COMMANDER_CORE_LCD_PID);
        commander_core_seen |= is_commander_core;

        let path_wide = to_wide(&path);
        let handle = unsafe {
            CreateFileW(
                path_wide.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null_mut(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            last_error = format!("Could not open Corsair LCD (Windows error {})", unsafe {
                GetLastError()
            });
            diagnostics.push(last_error.clone());
            continue;
        }

        if is_commander_core && commander_core_serial.is_none() {
            commander_core_serial = unsafe { serial_number(handle) };
        }

        let result = unsafe {
            send_feature_exact(handle, &first)
                .and_then(|_| {
                    thread::sleep(Duration::from_millis(10));
                    send_feature_exact(handle, &second)
                })
                .and_then(|_| {
                    if is_commander_core {
                        thread::sleep(Duration::from_millis(10));
                        send_feature_exact(handle, &refresh)
                    } else {
                        Ok(())
                    }
                })
        };

        unsafe {
            CloseHandle(handle);
        }

        match result {
            Ok(()) => {
                hid_succeeded = true;
                diagnostics.push(if is_commander_core {
                    "Native HID restore: exact 4-byte Commander Core sequence sent".into()
                } else {
                    "Native HID restore: exact 4-byte hardware-mode sequence sent".into()
                });
            }
            Err(error) => {
                last_error = error.clone();
                diagnostics.push(format!("Native HID restore failed: {error}"));
            }
        }
    }

    if commander_core_seen {
        // The HID sequence exits software presentation. The DLL call explicitly
        // tells the controller to render its persisted slot, which is the piece
        // that a plain mode-exit command does not guarantee on every firmware.
        thread::sleep(Duration::from_millis(30));
        match restore_persisted_with_icue(commander_core_serial.as_deref()) {
            Ok(()) => {
                diagnostics.push("Corsair persisted-image restore: success".into());
                write_restore_diagnostics(&diagnostics);
                return Ok(());
            }
            Err(error) => diagnostics.push(format!(
                "Corsair persisted-image restore unavailable/failed: {error}"
            )),
        }
    }

    write_restore_diagnostics(&diagnostics);

    if hid_succeeded {
        // Systems without iCUE still get the native, exact hardware-mode path.
        Ok(())
    } else if last_error.is_empty() {
        Err("Unable to restore Corsair LCD hardware mode".into())
    } else {
        Err(last_error)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        commander_core_refresh_packet, hardware_mode_packet_1, hardware_mode_packet_2,
    };

    #[test]
    fn hardware_mode_packets_match_openlinkhub_exact_report_length() {
        assert_eq!(hardware_mode_packet_1(), [0x03, 0x1e, 0x01, 0x01]);
        assert_eq!(hardware_mode_packet_2(), [0x03, 0x1d, 0x00, 0x01]);
        assert_eq!(commander_core_refresh_packet(), [0x03, 0x0b, 0x40, 0x01]);
    }
}
