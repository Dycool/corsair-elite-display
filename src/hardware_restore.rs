use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ptr::null_mut;
use std::thread;
use std::time::Duration;

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
}

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

unsafe fn send_feature_exact(handle: *mut c_void, packet: &[u8; 4]) -> Result<(), String> {
    // These are mode-switch reports only. Deliberately send exactly four bytes,
    // matching the known-good hardware-mode sequence, and do not invoke any
    // vendor API that can select/reset/rewrite persisted hardware image state.
    let result = unsafe { HidD_SetFeature(handle, packet.as_ptr(), packet.len() as u32) };
    if result == 0 {
        return Err(format!(
            "HidD_SetFeature failed with Windows error {}",
            unsafe { GetLastError() }
        ));
    }
    Ok(())
}

fn write_restore_diagnostics(lines: &[String]) {
    let _ = std::fs::write(
        std::env::temp_dir().join("corsair-elite-display-hardware-restore.txt"),
        format!("{}\n", lines.join("\n")),
    );
}

/// Returns a supported Corsair LCD from software-frame presentation to its
/// already-configured hardware mode without touching the persisted image/GIF.
///
/// OFF must be non-destructive: it never calls the iCUE DLL, never selects a
/// hardware slot, never starts a boot/custom animation explicitly, never writes
/// flash, and never changes LCD brightness. It only sends the two HID reports
/// that leave software mode and hand presentation back to the device firmware.
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
    let mut succeeded = false;
    let mut last_error = String::new();
    let mut diagnostics = Vec::new();

    for path in candidates {
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

        let result = unsafe {
            send_feature_exact(handle, &first).and_then(|_| {
                thread::sleep(Duration::from_millis(10));
                send_feature_exact(handle, &second)
            })
        };

        unsafe {
            CloseHandle(handle);
        }

        match result {
            Ok(()) => {
                succeeded = true;
                diagnostics.push(
                    "Hardware restore: exact 4-byte mode-exit sequence sent; persisted image untouched"
                        .into(),
                );
            }
            Err(error) => {
                last_error = error.clone();
                diagnostics.push(format!("Hardware restore failed: {error}"));
            }
        }
    }

    write_restore_diagnostics(&diagnostics);

    if succeeded {
        // Give the firmware a brief moment to swap presentation back to the
        // image/GIF it already has stored in hardware mode.
        thread::sleep(Duration::from_millis(20));
        Ok(())
    } else if last_error.is_empty() {
        Err("Unable to return Corsair LCD to hardware mode".into())
    } else {
        Err(last_error)
    }
}

#[cfg(test)]
mod tests {
    use super::{hardware_mode_packet_1, hardware_mode_packet_2};

    #[test]
    fn hardware_mode_packets_are_exact_and_non_mutating() {
        assert_eq!(hardware_mode_packet_1(), [0x03, 0x1e, 0x01, 0x01]);
        assert_eq!(hardware_mode_packet_2(), [0x03, 0x1d, 0x00, 0x01]);
    }
}
