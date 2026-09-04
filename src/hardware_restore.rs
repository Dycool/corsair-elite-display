use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ptr::null_mut;
use std::thread;
use std::time::Duration;

const GENERIC_READ: u32 = 0x80000000;
const GENERIC_WRITE: u32 = 0x40000000;
const FILE_SHARE_READ: u32 = 0x00000001;
const FILE_SHARE_WRITE: u32 = 0x00000002;
const OPEN_EXISTING: u32 = 3;
const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;

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
        security_attributes: *mut std::ffi::c_void,
        creation_disposition: u32,
        flags_and_attributes: u32,
        template_file: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;

    fn CloseHandle(handle: *mut std::ffi::c_void) -> i32;
    fn GetLastError() -> u32;
}

#[link(name = "hid")]
unsafe extern "system" {
    fn HidD_SetFeature(
        hid_device_object: *mut std::ffi::c_void,
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

fn hardware_mode_packet_1() -> [u8; 32] {
    let mut packet = [0u8; 32];
    // OpenLinkHub's known-good hardware-mode entry report is 03 1E 01 01.
    // The previous app path accidentally sent 03 1E 00 01, which leaves the
    // final streamed frame latched on the LCD on affected Elite LCD caps.
    packet[..4].copy_from_slice(&[0x03, 0x1e, 0x01, 0x01]);
    packet
}

fn hardware_mode_packet_2() -> [u8; 32] {
    let mut packet = [0u8; 32];
    packet[..4].copy_from_slice(&[0x03, 0x1d, 0x00, 0x01]);
    packet
}

unsafe fn send_feature(handle: *mut std::ffi::c_void, packet: &[u8; 32]) -> Result<(), String> {
    let mut result = unsafe { HidD_SetFeature(handle, packet.as_ptr(), packet.len() as u32) };
    if result == 0 {
        result = unsafe { HidD_SetFeature(handle, packet.as_ptr(), 4) };
    }
    if result == 0 {
        return Err(format!(
            "HidD_SetFeature failed with Windows error {}",
            unsafe { GetLastError() }
        ));
    }
    Ok(())
}

/// Forces any supported Corsair LCD back to the persisted hardware screen.
///
/// This deliberately uses the exact OpenLinkHub hardware-mode sequence rather
/// than relying on the streaming transport to time out. It is called after the
/// streaming worker has been stopped, and again during application shutdown as
/// a final safety net against a captured desktop frame remaining latched.
pub fn restore_hardware_mode() -> Result<(), String> {
    let paths = enumerate_hid_paths();
    let candidates: Vec<String> = paths
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
    let mut last_error = String::new();

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
            continue;
        }

        let result = unsafe {
            send_feature(handle, &first).and_then(|_| {
                thread::sleep(Duration::from_millis(10));
                send_feature(handle, &second)
            })
        };
        unsafe {
            CloseHandle(handle);
        }

        match result {
            Ok(()) => {
                // Give the controller a brief moment to swap from software frame
                // presentation to its persisted hardware-mode content.
                thread::sleep(Duration::from_millis(20));
                return Ok(());
            }
            Err(error) => last_error = error,
        }
    }

    if last_error.is_empty() {
        Err("Unable to restore Corsair LCD hardware mode".into())
    } else {
        Err(last_error)
    }
}

#[cfg(test)]
mod tests {
    use super::{hardware_mode_packet_1, hardware_mode_packet_2};

    #[test]
    fn hardware_mode_packets_match_openlinkhub() {
        assert_eq!(&hardware_mode_packet_1()[..4], &[0x03, 0x1e, 0x01, 0x01]);
        assert_eq!(&hardware_mode_packet_2()[..4], &[0x03, 0x1d, 0x00, 0x01]);
    }
}
