use std::ffi::{OsStr, OsString, c_void};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ptr::{null, null_mut};
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

const COMMANDER_CORE_LCD_PID: &str = "PID_0C39";
const LCD_STREAM_REPORT_LEN: u16 = 1024;
const ICUE_CC021_DLL: &str =
    r"C:\Program Files\Corsair\Corsair iCUE5 Software\iD_BD_x64_cc021.dll";

#[repr(C)]
struct HidpCaps {
    usage: u16,
    usage_page: u16,
    input_report_byte_length: u16,
    output_report_byte_length: u16,
    feature_report_byte_length: u16,
    reserved: [u16; 17],
    number_link_collection_nodes: u16,
    number_input_button_caps: u16,
    number_input_value_caps: u16,
    number_input_data_indices: u16,
    number_output_button_caps: u16,
    number_output_value_caps: u16,
    number_output_data_indices: u16,
    number_feature_button_caps: u16,
    number_feature_value_caps: u16,
    number_feature_data_indices: u16,
}

#[derive(Clone, Copy)]
struct ReportCaps {
    output_len: u16,
    feature_len: u16,
}

type FnOpenDevice = unsafe extern "C" fn(
    dev: *mut *mut c_void,
    vid: u16,
    pid: u16,
    path: *const u16,
) -> i32;

type FnEnterHardwareMode = unsafe extern "C" fn(dev: *mut c_void) -> i32;

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

    fn HidD_GetPreparsedData(
        hid_device_object: *mut c_void,
        preparsed_data: *mut *mut c_void,
    ) -> u8;

    fn HidD_FreePreparsedData(preparsed_data: *mut c_void) -> u8;

    fn HidP_GetCaps(preparsed_data: *const c_void, capabilities: *mut HidpCaps) -> i32;
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

fn commander_core_stop_packet() -> [u8; 4] {
    // OpenLinkHub's normal Commander Core Stop() sends this third live LCD
    // report. It does not select an image slot or write persisted LCD storage.
    [0x03, 0x0b, 0x40, 0x01]
}

unsafe fn report_caps(handle: *mut c_void) -> Result<ReportCaps, String> {
    let mut preparsed: *mut c_void = null_mut();
    if unsafe { HidD_GetPreparsedData(handle, &mut preparsed) } == 0 || preparsed.is_null() {
        return Err(format!(
            "HidD_GetPreparsedData failed with Windows error {}",
            unsafe { GetLastError() }
        ));
    }

    let mut caps = HidpCaps {
        usage: 0,
        usage_page: 0,
        input_report_byte_length: 0,
        output_report_byte_length: 0,
        feature_report_byte_length: 0,
        reserved: [0; 17],
        number_link_collection_nodes: 0,
        number_input_button_caps: 0,
        number_input_value_caps: 0,
        number_input_data_indices: 0,
        number_output_button_caps: 0,
        number_output_value_caps: 0,
        number_output_data_indices: 0,
        number_feature_button_caps: 0,
        number_feature_value_caps: 0,
        number_feature_data_indices: 0,
    };

    let status = unsafe { HidP_GetCaps(preparsed, &mut caps) };
    unsafe {
        HidD_FreePreparsedData(preparsed);
    }

    if status < 0 {
        return Err(format!("HidP_GetCaps failed with NTSTATUS 0x{:08x}", status as u32));
    }
    if caps.feature_report_byte_length < 4 {
        return Err(format!(
            "HID feature report length {} is too short for the LCD mode command",
            caps.feature_report_byte_length
        ));
    }

    Ok(ReportCaps {
        output_len: caps.output_report_byte_length,
        feature_len: caps.feature_report_byte_length,
    })
}

unsafe fn send_feature_hidapi_style(
    handle: *mut c_void,
    packet: &[u8; 4],
    feature_len: u16,
) -> Result<(), String> {
    let mut report = vec![0u8; feature_len as usize];
    report[..packet.len()].copy_from_slice(packet);

    let result = unsafe { HidD_SetFeature(handle, report.as_ptr(), report.len() as u32) };
    if result == 0 {
        return Err(format!(
            "HidD_SetFeature({} bytes) failed with Windows error {}",
            report.len(),
            unsafe { GetLastError() }
        ));
    }
    Ok(())
}

unsafe fn send_hardware_handoff(
    handle: *mut c_void,
    caps: ReportCaps,
    is_commander_core: bool,
) -> Result<(), String> {
    let first = hardware_mode_packet_1();
    let second = hardware_mode_packet_2();

    unsafe { send_feature_hidapi_style(handle, &first, caps.feature_len) }?;
    thread::sleep(Duration::from_millis(10));
    unsafe { send_feature_hidapi_style(handle, &second, caps.feature_len) }?;

    if is_commander_core {
        thread::sleep(Duration::from_millis(10));
        let third = commander_core_stop_packet();
        unsafe { send_feature_hidapi_style(handle, &third, caps.feature_len) }?;
    }

    Ok(())
}

/// Ask Corsair's own installed iCUE device library to perform only its runtime
/// hardware-mode entry operation. This deliberately does not call any of the
/// hardware-image update, animation-slot, screen-config, visibility, or flash
/// functions used by the separate hardware-image editor.
fn vendor_enter_hardware_mode() -> Result<(), String> {
    let dll_path = to_wide(ICUE_CC021_DLL);
    let module = unsafe {
        windows_sys::Win32::System::LibraryLoader::LoadLibraryW(dll_path.as_ptr())
    };
    if module.is_null() {
        return Err("Corsair iCUE device library is not installed".into());
    }

    unsafe {
        let get_fn = |name: &[u8]| -> *mut c_void {
            windows_sys::Win32::System::LibraryLoader::GetProcAddress(module, name.as_ptr())
                .map(|function| function as *mut c_void)
                .unwrap_or(null_mut())
        };

        let open: Option<FnOpenDevice> =
            std::mem::transmute(get_fn(b"iD_USB_open_device_cc021\0"));
        let enter: Option<FnEnterHardwareMode> =
            std::mem::transmute(get_fn(b"iD_USB_enter_hardware_mode_cc021\0"));

        let Some(open) = open else {
            return Err("Corsair iCUE library does not expose its device-open function".into());
        };
        let Some(enter) = enter else {
            return Err("Corsair iCUE library does not expose its hardware-mode entry function".into());
        };

        let mut device: *mut c_void = null_mut();
        let open_result = open(&mut device, 0x1b1c, 0x0c39, null());
        if open_result != 0 || device.is_null() {
            return Err(format!(
                "Corsair iCUE library could not open the Commander Core LCD (result {open_result})"
            ));
        }

        let enter_result = enter(device);
        if enter_result != 0 {
            return Err(format!(
                "Corsair iCUE library hardware-mode entry failed (result {enter_result})"
            ));
        }
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
/// OFF is intentionally non-destructive. It never selects hardware-image slots,
/// plays/resets animations, writes flash, deletes screen configuration, or calls
/// any iCUE configuration routine. For Commander Core LCD PID 0C39, the normal
/// HID handoff is followed only by Corsair's vendor-provided runtime
/// enter-hardware-mode operation.
pub fn restore_hardware_mode() -> Result<(), String> {
    let mut candidates: Vec<String> = enumerate_hid_paths()
        .into_iter()
        .filter(|path| {
            let upper = path.to_ascii_uppercase();
            upper.contains("VID_1B1C") && SUPPORTED_PIDS.iter().any(|pid| upper.contains(pid))
        })
        .collect();

    if candidates.is_empty() {
        return Err("No supported Corsair LCD found".into());
    }

    candidates.sort_by_key(|path| {
        let upper = path.to_ascii_uppercase();
        if upper.contains("MI_00") { 0 } else { 1 }
    });

    let mut succeeded = false;
    let mut last_error = String::new();
    let mut diagnostics = Vec::new();
    let mut saw_stream_interface = false;

    for path in candidates {
        let upper_path = path.to_ascii_uppercase();
        let is_commander_core = upper_path.contains(COMMANDER_CORE_LCD_PID);
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

        let caps = match unsafe { report_caps(handle) } {
            Ok(caps) => caps,
            Err(error) => {
                last_error = error.clone();
                diagnostics.push(format!("Skipping HID interface: {error}"));
                unsafe { CloseHandle(handle) };
                continue;
            }
        };

        diagnostics.push(format!(
            "HID caps: output={} feature={} commander_core={} path={}",
            caps.output_len, caps.feature_len, is_commander_core, path
        ));

        if caps.output_len < LCD_STREAM_REPORT_LEN {
            unsafe { CloseHandle(handle) };
            continue;
        }
        saw_stream_interface = true;

        let mut interface_ok = true;
        for attempt in 1..=3 {
            match unsafe { send_hardware_handoff(handle, caps, is_commander_core) } {
                Ok(()) => diagnostics.push(format!(
                    "Hardware handoff attempt {attempt}: sent {} using {}-byte feature reports",
                    if is_commander_core {
                        "Commander Core normal 3-report LCD sequence"
                    } else {
                        "2-report LCD sequence"
                    },
                    caps.feature_len
                )),
                Err(error) => {
                    last_error = error.clone();
                    diagnostics.push(format!("Hardware handoff attempt {attempt} failed: {error}"));
                    interface_ok = false;
                    break;
                }
            }
            if attempt < 3 {
                thread::sleep(Duration::from_millis(125));
            }
        }

        unsafe { CloseHandle(handle) };

        if interface_ok {
            if is_commander_core {
                match vendor_enter_hardware_mode() {
                    Ok(()) => diagnostics.push(
                        "Commander Core vendor handoff: Corsair iCUE runtime hardware-mode entry succeeded; persisted image/GIF configuration was not changed"
                            .into(),
                    ),
                    Err(error) => diagnostics.push(format!(
                        "Commander Core vendor handoff unavailable: {error}; public HID handoff still completed"
                    )),
                }
            }

            succeeded = true;
            diagnostics.push(
                "Hardware restore complete; persisted image/GIF state was not modified".into(),
            );
            break;
        }
    }

    if !saw_stream_interface {
        diagnostics.push(
            "No HID interface exposing the 1024-byte LCD streaming report was found".into(),
        );
    }

    write_restore_diagnostics(&diagnostics);

    if succeeded {
        Ok(())
    } else if last_error.is_empty() {
        Err("Unable to identify the Corsair LCD streaming HID interface".into())
    } else {
        Err(last_error)
    }
}

#[cfg(test)]
mod tests {
    use super::{commander_core_stop_packet, hardware_mode_packet_1, hardware_mode_packet_2};

    #[test]
    fn hardware_mode_packets_match_openlinkhub() {
        assert_eq!(hardware_mode_packet_1(), [0x03, 0x1e, 0x01, 0x01]);
        assert_eq!(hardware_mode_packet_2(), [0x03, 0x1d, 0x00, 0x01]);
        assert_eq!(commander_core_stop_packet(), [0x03, 0x0b, 0x40, 0x01]);
    }
}
