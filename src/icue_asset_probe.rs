use std::ffi::{c_void, OsStr};
use std::fmt::Write as _;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::{null, null_mut};

const ICUE_CC021_DLL: &str =
    r"C:\Program Files\Corsair\Corsair iCUE5 Software\iD_BD_x64_cc021.dll";
const REPORT_FILE: &str = "corsair-elite-display-cc021-asset-probe.txt";
const VID_CORSAIR: u16 = 0x1b1c;
const PID_COMMANDER_CORE_LCD: u16 = 0x0c39;
const HARDWARE_BACKGROUND_ASSET: u8 = 2;

// Signatures below are recovered from the uploaded cc021 DLL itself. This probe
// deliberately uses only query/receive operations. The vendor open/close calls
// do change the LCD's *runtime* software/hardware session state, but none of the
// functions below erase, program, or otherwise write persistent flash storage.
type FnOpenDevice = unsafe extern "C" fn(
    dev: *mut *mut c_void,
    vid: u16,
    pid: u16,
    serial: *const u16,
) -> i32;
type FnCloseDevice = unsafe extern "C" fn(dev: *mut *mut c_void) -> i32;
type FnGetU32 = unsafe extern "C" fn(dev: *mut c_void, value: *mut u32) -> i32;
type FnGetAssetCrc = unsafe extern "C" fn(
    dev: *mut c_void,
    crc: *mut u32,
    asset_type: u8,
) -> i32;
type FnReceiveInput = unsafe extern "C" fn(dev: *mut c_void) -> i32;
type FnReceiveInputAck = unsafe extern "C" fn(dev: *mut c_void) -> i32;
type FnWaitEvent = unsafe extern "C" fn(
    dev: *mut c_void,
    event: *mut u8,
    timeout_ms: u32,
) -> i32;

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

unsafe fn proc(module: *mut c_void, name: &[u8]) -> *mut c_void {
    unsafe {
        windows_sys::Win32::System::LibraryLoader::GetProcAddress(module, name.as_ptr())
            .map(|function| function as *mut c_void)
            .unwrap_or(null_mut())
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index != 0 {
            out.push(' ');
        }
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

fn parse_event(event: &[u8; 512]) -> String {
    let event_type = event[0];
    let declared = u32::from_le_bytes([event[1], event[2], event[3], event[4]]) as usize;
    let available = event.len() - 5;
    let payload_len = declared.min(available);
    let preview_len = payload_len.min(96);
    format!(
        "event_type=0x{event_type:02x} declared_payload={} payload_preview[{}]={}",
        declared,
        preview_len,
        hex(&event[5..5 + preview_len])
    )
}

/// Exercise only the cc021 backend's known query/device-to-host primitives.
///
/// Reverse engineering of iD_BD_x64_cc021.dll established:
/// - get_asset_CRC(asset=2) uses feature reports 03 23 00 02 then report 16;
/// - get_free_size_of_flash is a feature-report query;
/// - get_animation_hash is a feature-report query;
/// - receive_input_data sends 03 1F;
/// - wait_event reads a 512-byte HID input report;
/// - receive_input_data_ack sends 03 21.
///
/// No unknown command is sent, and no persistent-image/resource write function
/// is resolved or called by this probe.
pub fn run() -> Result<PathBuf, String> {
    let mut lines = Vec::new();
    lines.push("Corsair Elite Display - cc021 asset readback probe".to_string());
    lines.push("Persistent storage writes: NONE".to_string());
    lines.push("Unknown/guessed opcodes: NONE".to_string());
    lines.push(
        "Note: vendor open/close toggles the volatile LCD software/hardware session state."
            .to_string(),
    );

    let dll_w = wide(ICUE_CC021_DLL);
    let module = unsafe {
        windows_sys::Win32::System::LibraryLoader::LoadLibraryW(dll_w.as_ptr())
    };
    if module.is_null() {
        return Err(format!("Could not load {ICUE_CC021_DLL}"));
    }

    let result = unsafe {
        let open: Option<FnOpenDevice> = std::mem::transmute(proc(
            module,
            b"iD_USB_open_device_cc021\0",
        ));
        let close: Option<FnCloseDevice> = std::mem::transmute(proc(
            module,
            b"iD_USB_close_device_cc021\0",
        ));
        let get_asset_crc: Option<FnGetAssetCrc> = std::mem::transmute(proc(
            module,
            b"iD_USB_get_asset_CRC_cc021\0",
        ));
        let get_free_flash: Option<FnGetU32> = std::mem::transmute(proc(
            module,
            b"iD_USB_get_free_size_of_flash_cc021\0",
        ));
        let get_animation_hash: Option<FnGetU32> = std::mem::transmute(proc(
            module,
            b"iD_USB_get_animation_hash_cc021\0",
        ));
        let receive_input: Option<FnReceiveInput> = std::mem::transmute(proc(
            module,
            b"iD_USB_receive_input_data_cc021\0",
        ));
        let receive_ack: Option<FnReceiveInputAck> = std::mem::transmute(proc(
            module,
            b"iD_USB_receive_input_data_ack_cc021\0",
        ));
        let wait_event: Option<FnWaitEvent> = std::mem::transmute(proc(
            module,
            b"iD_USB_wait_event_cc021\0",
        ));

        let Some(open) = open else {
            return Err("cc021 DLL is missing iD_USB_open_device_cc021".into());
        };
        let Some(close) = close else {
            return Err("cc021 DLL is missing iD_USB_close_device_cc021".into());
        };
        let Some(get_asset_crc) = get_asset_crc else {
            return Err("cc021 DLL is missing iD_USB_get_asset_CRC_cc021".into());
        };
        let Some(get_free_flash) = get_free_flash else {
            return Err("cc021 DLL is missing iD_USB_get_free_size_of_flash_cc021".into());
        };
        let Some(get_animation_hash) = get_animation_hash else {
            return Err("cc021 DLL is missing iD_USB_get_animation_hash_cc021".into());
        };
        let Some(receive_input) = receive_input else {
            return Err("cc021 DLL is missing iD_USB_receive_input_data_cc021".into());
        };
        let Some(receive_ack) = receive_ack else {
            return Err("cc021 DLL is missing iD_USB_receive_input_data_ack_cc021".into());
        };
        let Some(wait_event) = wait_event else {
            return Err("cc021 DLL is missing iD_USB_wait_event_cc021".into());
        };

        let mut device: *mut c_void = null_mut();
        let open_result = open(
            &mut device,
            VID_CORSAIR,
            PID_COMMANDER_CORE_LCD,
            null(),
        );
        lines.push(format!(
            "open: result={open_result} device_nonnull={}",
            !device.is_null()
        ));
        if open_result != 0 || device.is_null() {
            return Err(format!(
                "Corsair cc021 DLL could not open VID 1B1C PID 0C39 (result {open_result}). Close iCUE and retry."
            ));
        }

        let mut asset_crc = 0u32;
        let crc_result = get_asset_crc(
            device,
            &mut asset_crc,
            HARDWARE_BACKGROUND_ASSET,
        );
        lines.push(format!(
            "asset_crc: type={} result={} crc=0x{:08x}",
            HARDWARE_BACKGROUND_ASSET, crc_result, asset_crc
        ));

        let mut free_flash = 0u32;
        let free_result = get_free_flash(device, &mut free_flash);
        lines.push(format!(
            "free_flash: result={free_result} bytes={free_flash} (0x{free_flash:08x})"
        ));

        let mut animation_hash = 0u32;
        let hash_result = get_animation_hash(device, &mut animation_hash);
        lines.push(format!(
            "animation_hash: result={hash_result} hash=0x{animation_hash:08x}"
        ));

        // 03 1F is a named, exported vendor operation recovered from this exact
        // DLL. We make one request only; if a packet becomes available, capture
        // it verbatim and acknowledge it with the matching exported 03 21 op.
        let receive_result = receive_input(device);
        lines.push(format!("receive_input_data: result={receive_result}"));

        if receive_result == 0 {
            let mut event = [0u8; 512];
            let wait_result = wait_event(device, event.as_mut_ptr(), 1_000);
            lines.push(format!("wait_event: result={wait_result}"));
            if wait_result == 0 {
                lines.push(parse_event(&event));
                let ack_result = receive_ack(device);
                lines.push(format!("receive_input_data_ack: result={ack_result}"));
            }
        }

        let close_result = close(&mut device);
        lines.push(format!(
            "close: result={close_result} device_nonnull_after={}",
            !device.is_null()
        ));
        Ok::<(), String>(())
    };

    let output = std::env::temp_dir().join(REPORT_FILE);
    let write_result = std::fs::write(&output, format!("{}\n", lines.join("\n")));

    if let Err(error) = result {
        let _ = write_result;
        return Err(format!("{error}. Partial report: {}", output.display()));
    }
    write_result.map_err(|error| format!("Could not write {}: {error}", output.display()))?;
    Ok(output)
}
