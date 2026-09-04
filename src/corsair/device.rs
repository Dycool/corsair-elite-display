use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ptr::{null, null_mut};

use image::ExtendedColorType;

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
    "PID_0C39", // Corsair Commander Core LCD Cap (Elite Capellix LCD)
    "PID_0C33", // Corsair Elite LCD XT Cap
    "PID_0C42", // Corsair Nautilus LCD Cap / Capellix XT LCD
    "PID_0C4E", // Corsair iCUE LINK AIO LCD Screen Module (Titan, H100i/H150i/H170i LINK)
    "PID_0C37", // Corsair H100i/H150i/H170i ELITE LCD / RGB
    "PID_0C40", // Corsair iCUE LINK System Hub LCD
    "PID_0C53", // Corsair iCUE LINK XD5 LCD / Reservoir
    "PID_0C5B", // Corsair LCD Module
];

#[link(name = "cfgmgr32")]
unsafe extern "system" {
    fn CM_Get_Device_Interface_List_SizeW(
        pulLen: *mut u32,
        InterfaceClassGuid: *const u8,
        pDeviceID: *const u16,
        ulFlags: u32,
    ) -> u32;

    fn CM_Get_Device_Interface_ListW(
        InterfaceClassGuid: *const u8,
        pDeviceID: *const u16,
        Buffer: *mut u16,
        BufferLen: u32,
        ulFlags: u32,
    ) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateFileW(
        lpFileName: *const u16,
        dwDesiredAccess: u32,
        dwShareMode: u32,
        lpSecurityAttributes: *mut std::ffi::c_void,
        dwCreationDisposition: u32,
        dwFlagsAndAttributes: u32,
        hTemplateFile: *mut std::ffi::c_void,
    ) -> *mut std::ffi::c_void;

    fn WriteFile(
        hFile: *mut std::ffi::c_void,
        lpBuffer: *const u8,
        nNumberOfBytesToWrite: u32,
        lpNumberOfBytesWritten: *mut u32,
        lpOverlapped: *mut std::ffi::c_void,
    ) -> i32;

    fn CloseHandle(hObject: *mut std::ffi::c_void) -> i32;
    fn GetLastError() -> u32;
}

#[link(name = "hid")]
unsafe extern "system" {
    fn HidD_SetFeature(
        HidDeviceObject: *mut std::ffi::c_void,
        ReportBuffer: *const u8,
        ReportBufferLength: u32,
    ) -> u8;

    fn HidD_GetSerialNumberString(
        HidDeviceObject: *mut std::ffi::c_void,
        Buffer: *mut u16,
        BufferLength: u32,
    ) -> u8;
}

fn to_u16_vec(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn enumerate_hid_paths() -> Vec<String> {
    let mut paths = Vec::new();
    unsafe {
        let mut len = 0u32;
        let res = CM_Get_Device_Interface_List_SizeW(
            &mut len,
            GUID_DEVINTERFACE_HID.as_ptr(),
            null_mut(),
            0,
        );
        if res != 0 || len == 0 {
            return paths;
        }

        let mut buffer = vec![0u16; len as usize];
        let res = CM_Get_Device_Interface_ListW(
            GUID_DEVINTERFACE_HID.as_ptr(),
            null_mut(),
            buffer.as_mut_ptr(),
            len,
            0,
        );
        if res != 0 {
            return paths;
        }

        let mut start = 0;
        for i in 0..buffer.len() {
            if buffer[i] == 0 {
                if start < i {
                    paths.push(
                        OsString::from_wide(&buffer[start..i])
                            .to_string_lossy()
                            .to_string(),
                    );
                }
                start = i + 1;
            }
        }
    }
    paths
}

fn hardware_mode_packet_1() -> [u8; 32] {
    let mut packet = [0u8; 32];
    packet[..4].copy_from_slice(&[0x03, 0x1e, 0x00, 0x01]);
    packet
}

fn hardware_mode_packet_2() -> [u8; 32] {
    let mut packet = [0u8; 32];
    packet[..4].copy_from_slice(&[0x03, 0x1d, 0x00, 0x01]);
    packet
}

#[cfg(test)]
fn frame_packet(chunk: &[u8], part_num: u16, is_end: bool) -> [u8; 1024] {
    debug_assert!(chunk.len() <= 1016);
    let mut packet = [0u8; 1024];
    packet[0] = 0x02;
    packet[1] = 0x05;
    packet[2] = 0x40;
    packet[3] = u8::from(is_end);
    packet[4..6].copy_from_slice(&part_num.to_le_bytes());
    packet[6..8].copy_from_slice(&(chunk.len() as u16).to_le_bytes());
    packet[8..8 + chunk.len()].copy_from_slice(chunk);
    packet
}

pub struct CorsairLcdDevice {
    handle: *mut std::ffi::c_void,
    #[allow(dead_code)]
    product_name: String,
}

unsafe impl Send for CorsairLcdDevice {}
unsafe impl Sync for CorsairLcdDevice {}

impl CorsairLcdDevice {
    fn find_device_paths() -> Vec<String> {
        let mut matches = Vec::new();
        for path in enumerate_hid_paths() {
            let upper = path.to_uppercase();
            if upper.contains("VID_1B1C") && SUPPORTED_PIDS.iter().any(|pid| upper.contains(pid)) {
                matches.push(path);
            }
        }
        matches
    }

    pub fn open() -> Result<Self, String> {
        let paths = Self::find_device_paths();
        if paths.is_empty() {
            return Err("Waiting for a supported Corsair LCD...".to_string());
        }
        let mut last_error = 0;
        for path in &paths {
            let path_u16 = to_u16_vec(path);
            let handle = unsafe {
                CreateFileW(
                    path_u16.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    null_mut(),
                    OPEN_EXISTING,
                    0,
                    null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                return Ok(Self {
                    handle,
                    product_name: "Corsair Elite LCD".to_string(),
                });
            }
            last_error = unsafe { GetLastError() };
        }
        Err(format!(
            "Corsair LCD found but unavailable (Windows error {last_error}). Close iCUE if it is using the screen."
        ))
    }

    #[allow(dead_code)]
    pub fn product_name(&self) -> &str {
        &self.product_name
    }

    pub fn get_serial_number(&self) -> Option<String> {
        let mut buf = [0u16; 128];
        let ok = unsafe {
            HidD_GetSerialNumberString(self.handle, buf.as_mut_ptr(), (buf.len() * 2) as u32)
        };
        if ok != 0 {
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            String::from_utf16(&buf[..len]).ok()
        } else {
            None
        }
    }

    pub fn set_brightness(&self, percent: u8) -> Result<(), String> {
        let raw_val = match percent {
            0 => 0x00,
            1..=16 => 0x01,
            17..=49 => 0x04,
            50..=83 => 0x10,
            _ => 0x40,
        };
        let mut packet = [0u8; 32];
        packet[0] = 0x03; // Report ID
        packet[1] = 0x0B; // Opcode: LCD Brightness
        packet[2] = raw_val;
        packet[3] = 0x01;
        unsafe {
            let res = HidD_SetFeature(self.handle, packet.as_ptr(), packet.len() as u32);
            if res == 0 {
                return Err(format!(
                    "HidD_SetFeature (brightness) failed: {}",
                    GetLastError()
                ));
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub fn set_rotation(&self, angle: u16) -> Result<(), String> {
        let raw_val = match angle {
            0 => 0x00,
            90 => 0x01,
            180 => 0x02,
            270 => 0x03,
            _ => return Err(format!("Unsupported rotation angle: {}", angle)),
        };
        let mut packet = [0u8; 32];
        packet[0] = 0x03; // Report ID
        packet[1] = 0x0C; // Opcode: LCD Rotation
        packet[2] = raw_val;
        unsafe {
            let res = HidD_SetFeature(self.handle, packet.as_ptr(), packet.len() as u32);
            if res == 0 {
                return Err(format!(
                    "HidD_SetFeature (rotation) failed: {}",
                    GetLastError()
                ));
            }
        }
        Ok(())
    }

    pub fn send_frame(&self, jpeg_data: &[u8]) -> Result<(), String> {
        let max_len = 1024;
        let header_size = 8;
        let real_max_len = max_len - header_size;
        let total_len = jpeg_data.len();
        let mut packet = [0u8; 1024];
        packet[0] = 0x02;
        packet[1] = 0x05;
        packet[2] = 0x40;

        for (part_num, chunk) in (0_u16..).zip(jpeg_data.chunks(real_max_len)) {
            let chunk_len = chunk.len();
            let is_end = (part_num as usize * real_max_len) + chunk_len >= total_len;

            packet[3] = u8::from(is_end);
            packet[4..6].copy_from_slice(&part_num.to_le_bytes());
            packet[6..8].copy_from_slice(&(chunk_len as u16).to_le_bytes());
            packet[8..8 + chunk_len].copy_from_slice(chunk);
            packet[8 + chunk_len..].fill(0);

            self.write_packet(&packet)?;
        }

        Ok(())
    }

    fn write_packet(&self, packet: &[u8]) -> Result<(), String> {
        unsafe {
            let mut written = 0u32;
            let success = WriteFile(
                self.handle,
                packet.as_ptr(),
                packet.len() as u32,
                &mut written,
                null_mut(),
            );
            if success == 0 || written != packet.len() as u32 {
                return Err(format!("WriteFile failed with error {}", GetLastError()));
            }
        }
        Ok(())
    }

    fn send_black_fallback_frame(&self) -> Result<(), String> {
        // Never leave a captured desktop frame frozen on the pump display while waiting for
        // the LCD firmware to fall back to its saved hardware screen on its own.
        let black_pixels = vec![0u8; 480 * 480 * 3];
        let mut jpeg = Vec::with_capacity(8 * 1024);
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 60)
            .encode(&black_pixels, 480, 480, ExtendedColorType::Rgb8)
            .map_err(|error| format!("JPEG black fallback encoding failed: {error}"))?;
        self.send_frame(&jpeg)
    }

    fn send_feature_packet(&self, packet: &[u8; 32]) -> Result<(), String> {
        unsafe {
            let mut res = HidD_SetFeature(self.handle, packet.as_ptr(), packet.len() as u32);
            if res == 0 {
                res = HidD_SetFeature(self.handle, packet.as_ptr(), 4);
            }
            if res == 0 {
                return Err(format!(
                    "HidD_SetFeature failed with Windows error {}",
                    GetLastError()
                ));
            }
        }
        Ok(())
    }

    /// Switches the LCD back to hardware mode using OpenLinkHub's known-good sequence:
    /// Report 1: 03 1E 01 01
    /// Report 2: 03 1D 00 01 (Device Session Control: 0x00 = return to hardware mode)
    pub fn switch_to_hardware_mode(&self) -> Result<(), String> {
        let p1 = hardware_mode_packet_1();
        let p2 = hardware_mode_packet_2();
        self.send_feature_packet(&p1)?;
        std::thread::sleep(std::time::Duration::from_millis(10));
        self.send_feature_packet(&p2)?;
        Ok(())
    }

    /// Releases the LCD streaming interface and switches the display back to hardware mode.
    /// If switching to hardware mode fails, falls back to sending a black frame to avoid
    /// leaving a desktop capture frozen on screen.
    pub fn release_to_hardware(self) {
        let hw_result = self.switch_to_hardware_mode();
        let fallback_result = if hw_result.is_err() {
            self.send_black_fallback_frame()
        } else {
            Ok(())
        };
        drop(self);

        if let Err(error) = hw_result {
            let log_msg = match fallback_result {
                Err(fb) => format!("Hardware mode switch failed: {error}; fallback black frame failed: {fb}"),
                Ok(()) => format!("Hardware mode switch failed: {error}; black fallback frame sent"),
            };
            let _ = std::fs::write(
                std::env::temp_dir().join("corsair-elite-display-hardware-restore.txt"),
                log_msg,
            );
        }
    }

    /// Attempts to locate any connected Corsair LCD and return it to hardware mode.
    pub fn restore_hardware_mode() -> Result<(), String> {
        let paths = Self::find_device_paths();
        if paths.is_empty() {
            return Err("No supported Corsair LCD found".into());
        }
        let mut succeeded = false;
        let mut last_error = String::new();
        for path in &paths {
            let path_u16 = to_u16_vec(path);
            let handle = unsafe {
                CreateFileW(
                    path_u16.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    null_mut(),
                    OPEN_EXISTING,
                    0,
                    null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                continue;
            }
            let dev = Self {
                handle,
                product_name: String::new(),
            };
            match dev.switch_to_hardware_mode() {
                Ok(()) => {
                    succeeded = true;
                }
                Err(e) => {
                    last_error = e;
                }
            }
            drop(dev);
        }
        if succeeded {
            Ok(())
        } else if !last_error.is_empty() {
            Err(last_error)
        } else {
            Err("Unable to open any Corsair LCD interface (check if iCUE has exclusive access)".into())
        }
    }
}

type FnOpenDevice = unsafe extern "C" fn(
    dev: *mut *mut std::ffi::c_void,
    vid: u16,
    pid: u16,
    path: *const u16,
) -> i32;

type FnSetSerialNumber = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
    serial: *const u8,
    len: u32,
) -> i32;

type FnSetIdisplaySerialNumber = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
    serial: *const u8,
    len: u32,
) -> i32;

#[repr(C)]
struct HwBgParam {
    unknown: u32,
    size: u32,
    data: *const u8,
}

type FnUpdateHardwareModeBackground = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
    param: *const HwBgParam,
) -> i32;

type FnUpdateCustomizedAnimationSegment = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
    data: *const u8,
    len: u32,
    segment_idx: u32,
    crc: u32,
) -> i32;

type FnDeleteScreenConfig = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
) -> i32;

type FnPerformScreenConfig = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
) -> i32;

type FnSetVisibleOfItem = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
    item_id: u8,
    visible: u8,
) -> i32;

type FnSetHardwareMode = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
    mode: i32,
) -> i32;

type FnEnterHardwareMode = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
) -> i32;

type FnSetCustomizedAnimation = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
    slot: u16,
) -> i32;

type FnSetBootAnimation = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
    slot: u16,
) -> i32;

type FnPlayCustomizedAnimation = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
) -> i32;

type FnPlayBootAnimation = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
) -> i32;

type FnShowFullScreenImage = unsafe extern "C" fn(
    dev: *mut std::ffi::c_void,
    data: *const u8,
    len: u32,
) -> i32;

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

pub fn flash_hardware_image(path: &std::path::Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("File does not exist: {}", path.display()));
    }

    // 1. Load image and process to 480x480 RGB JPEG
    let dynamic_img = image::open(path).map_err(|e| format!("Failed to read image: {e}"))?;
    let resized = dynamic_img.resize_exact(480, 480, image::imageops::FilterType::Lanczos3);
    let rgb = resized.to_rgb8();
    let mut jpeg_data = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, 95)
        .encode(&rgb, 480, 480, ExtendedColorType::Rgb8)
        .map_err(|e| format!("JPEG encoding failed: {e}"))?;

    // 2. Try DLL-based flashing via iD_BD_x64_cc021.dll
    let dll_path = to_u16_vec(r"C:\Program Files\Corsair\Corsair iCUE5 Software\iD_BD_x64_cc021.dll");
    let h_module = unsafe { windows_sys::Win32::System::LibraryLoader::LoadLibraryW(dll_path.as_ptr()) };

    let mut dll_success = false;
    if !h_module.is_null() {
        unsafe {
            let get_fn = |name: &[u8]| -> *mut std::ffi::c_void {
                windows_sys::Win32::System::LibraryLoader::GetProcAddress(
                    h_module,
                    name.as_ptr(),
                ).map(|f| f as *mut std::ffi::c_void).unwrap_or(null_mut())
            };

            let fn_open: Option<FnOpenDevice> = std::mem::transmute(get_fn(b"iD_USB_open_device_cc021\0"));
            let fn_serial: Option<FnSetSerialNumber> = std::mem::transmute(get_fn(b"iD_USB_set_serial_number_cc021\0"));
            let fn_idisplay: Option<FnSetIdisplaySerialNumber> = std::mem::transmute(get_fn(b"iD_USB_set_idisplay_serial_number_cc021\0"));
            let fn_update_bg: Option<FnUpdateHardwareModeBackground> = std::mem::transmute(get_fn(b"iD_USB_update_hardware_mode_background_cc021\0"));
            let fn_update_anim: Option<FnUpdateCustomizedAnimationSegment> = std::mem::transmute(get_fn(b"iD_USB_update_customized_animation_segment_cc021\0"));
            let fn_delete: Option<FnDeleteScreenConfig> = std::mem::transmute(get_fn(b"iD_USB_delete_screen_config_cc021\0"));
            let fn_perform: Option<FnPerformScreenConfig> = std::mem::transmute(get_fn(b"iD_USB_perform_screen_config_cc021\0"));
            let fn_visible: Option<FnSetVisibleOfItem> = std::mem::transmute(get_fn(b"iD_USB_set_visible_of_item_cc021\0"));
            let fn_set_hw: Option<FnSetHardwareMode> = std::mem::transmute(get_fn(b"iD_USB_set_hardware_mode_cc021\0"));
            let fn_enter_hw: Option<FnEnterHardwareMode> = std::mem::transmute(get_fn(b"iD_USB_enter_hardware_mode_cc021\0"));
            let fn_set_cust: Option<FnSetCustomizedAnimation> = std::mem::transmute(get_fn(b"iD_USB_set_customized_animation_cc021\0"));
            let fn_set_boot: Option<FnSetBootAnimation> = std::mem::transmute(get_fn(b"iD_USB_set_boot_animation_cc021\0"));
            let fn_play_cust: Option<FnPlayCustomizedAnimation> = std::mem::transmute(get_fn(b"iD_USB_play_customized_animation_cc021\0"));
            let fn_play_boot: Option<FnPlayBootAnimation> = std::mem::transmute(get_fn(b"iD_USB_play_boot_animation_cc021\0"));
            let fn_show: Option<FnShowFullScreenImage> = std::mem::transmute(get_fn(b"iD_USB_show_full_screen_image_cc021\0"));

            if let (Some(open), Some(serial), Some(update_bg), Some(visible), Some(perform), Some(set_hw), Some(show)) =
                (fn_open, fn_serial, fn_update_bg, fn_visible, fn_perform, fn_set_hw, fn_show)
            {
                let mut dev: *mut std::ffi::c_void = null_mut();
                let res = open(&mut dev, 0x1B1C, 0x0C39, null());
                if res == 0 && !dev.is_null() {
                    // Dynamically retrieve and pass the device USB descriptor serial number if available
                    if let Ok(lcd) = CorsairLcdDevice::open() {
                        if let Some(serial_str) = lcd.get_serial_number() {
                            let mut serial_bytes = serial_str.into_bytes();
                            serial_bytes.push(0);
                            serial(dev, serial_bytes.as_ptr(), (serial_bytes.len() - 1) as u32);
                            if let Some(idisplay) = fn_idisplay {
                                idisplay(dev, serial_bytes.as_ptr(), (serial_bytes.len() - 1) as u32);
                            }
                        }
                    }

                    // Flash background image to pump cap flash memory
                    let param = HwBgParam {
                        unknown: 0,
                        size: jpeg_data.len() as u32,
                        data: jpeg_data.as_ptr(),
                    };
                    let bg_res = update_bg(dev, &param);

                    // Flash customized animation segment with proper container
                    if let Some(update_anim) = fn_update_anim {
                        let mut anim_buf = Vec::with_capacity(13 + jpeg_data.len());
                        anim_buf.push(1u8); // type = 1
                        anim_buf.extend_from_slice(&480u16.to_le_bytes()); // width
                        anim_buf.extend_from_slice(&480u16.to_le_bytes()); // height
                        anim_buf.extend_from_slice(&33u16.to_le_bytes());  // delay
                        anim_buf.extend_from_slice(&1u16.to_le_bytes());   // frame count
                        anim_buf.extend_from_slice(&(jpeg_data.len() as u32).to_le_bytes());
                        anim_buf.extend_from_slice(&jpeg_data);
                        
                        let crc = crc32(&jpeg_data);

                        update_anim(dev, anim_buf.as_ptr(), anim_buf.len() as u32, 0, crc);
                    }

                    // Delete sensor widgets so no number or gauge is drawn over the hardware image
                    if let Some(delete) = fn_delete {
                        delete(dev);
                        perform(dev);
                    }

                    // Hide overlay widgets 1..=8 and keep item 0 (background) visible
                    for item_id in 1..=8 {
                        visible(dev, item_id, 0);
                    }
                    visible(dev, 0, 1);
                    perform(dev);

                    // Set hardware mode to 0 (Image/Animation mode, NOT sensor number mode 1)
                    set_hw(dev, 0);

                    // Activate boot and customized animation slots
                    if let Some(set_cust) = fn_set_cust { set_cust(dev, 0); }
                    if let Some(set_boot) = fn_set_boot { set_boot(dev, 0); }
                    if let Some(play_cust) = fn_play_cust { play_cust(dev); }
                    if let Some(play_boot) = fn_play_boot { play_boot(dev); }

                    // Immediately show full screen image
                    show(dev, jpeg_data.as_ptr(), jpeg_data.len() as u32);

                    if let Some(enter_hw) = fn_enter_hw {
                        enter_hw(dev);
                    }

                    if bg_res == 0 {
                        dll_success = true;
                    }
                }
            }
        }
    }

    // 3. Always also send frame directly via our native HID transport to guarantee immediate display
    let hid_res = CorsairLcdDevice::open().and_then(|dev| {
        let _ = dev.switch_to_hardware_mode();
        dev.send_frame(&jpeg_data)
    });

    if dll_success || hid_res.is_ok() {
        Ok(())
    } else {
        Err("Failed to flash image to Corsair Elite LCD. Ensure the device is connected and not locked by another application.".into())
    }
}

impl Drop for CorsairLcdDevice {
    fn drop(&mut self) {
        if !self.handle.is_null() && self.handle != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.handle);
            }
            self.handle = INVALID_HANDLE_VALUE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{frame_packet, hardware_mode_packet_1, hardware_mode_packet_2};

    #[test]
    fn image_packet_has_the_observed_corsair_header_and_padding() {
        let packet = frame_packet(&[0xff, 0xd8, 0xff], 7, true);
        assert_eq!(&packet[..8], &[0x02, 0x05, 0x40, 0x01, 7, 0, 3, 0]);
        assert_eq!(&packet[8..11], &[0xff, 0xd8, 0xff]);
        assert!(packet[11..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn hardware_mode_packets_match_openlinkhub_protocol() {
        let p1 = hardware_mode_packet_1();
        assert_eq!(&p1[..4], &[0x03, 0x1e, 0x00, 0x01]);
        assert!(p1[4..].iter().all(|byte| *byte == 0));

        let p2 = hardware_mode_packet_2();
        assert_eq!(&p2[..4], &[0x03, 0x1d, 0x00, 0x01]);
        assert!(p2[4..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn image_resizing_and_jpeg_encoding() {
        let img = image::RgbImage::new(100, 100);
        let dynamic = image::DynamicImage::ImageRgb8(img);
        let resized = dynamic.resize_exact(480, 480, image::imageops::FilterType::Lanczos3);
        assert_eq!(resized.width(), 480);
        assert_eq!(resized.height(), 480);
        let rgb = resized.to_rgb8();
        let mut jpeg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
            .encode(&rgb, 480, 480, image::ExtendedColorType::Rgb8)
            .unwrap();
        assert!(jpeg.len() > 100);
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
    }
}
