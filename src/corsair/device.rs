use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::ptr::null_mut;

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

    fn HidD_GetProductString(
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

fn query_product_string(path: &str) -> Option<String> {
    let path_u16 = to_u16_vec(path);
    let handle = unsafe {
        CreateFileW(
            path_u16.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null_mut(),
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut buf = [0u16; 256];
    let ok = unsafe { HidD_GetProductString(handle, buf.as_mut_ptr(), (buf.len() * 2) as u32) };
    unsafe { CloseHandle(handle); }
    if ok != 0 {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    } else {
        None
    }
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
        unsafe {
            let mut len = 0u32;
            let res = CM_Get_Device_Interface_List_SizeW(
                &mut len,
                GUID_DEVINTERFACE_HID.as_ptr(),
                null_mut(),
                0,
            );
            if res != 0 || len == 0 {
                return matches;
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
                return matches;
            }

            let mut start = 0;
            for i in 0..buffer.len() {
                if buffer[i] == 0 {
                    if start < i {
                        let s = OsString::from_wide(&buffer[start..i])
                            .to_string_lossy()
                            .to_string();
                        let upper = s.to_uppercase();
                        if upper.contains("VID_1B1C") {
                            if SUPPORTED_PIDS.iter().any(|pid| upper.contains(pid)) {
                                matches.push(s);
                            } else if let Some(product_name) = query_product_string(&s) {
                                if product_name.to_uppercase().contains("LCD") {
                                    matches.push(s);
                                }
                            }
                        }
                    }
                    start = i + 1;
                }
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
                let mut buf = [0u16; 256];
                let ok = unsafe { HidD_GetProductString(handle, buf.as_mut_ptr(), (buf.len() * 2) as u32) };
                let product_name = if ok != 0 {
                    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                    String::from_utf16_lossy(&buf[..len])
                } else {
                    "Corsair Elite LCD".to_string()
                };

                let device = Self {
                    handle,
                    product_name,
                };
                return Ok(device);
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
                return Err(format!("HidD_SetFeature (brightness) failed: {}", GetLastError()));
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
                return Err(format!("HidD_SetFeature (rotation) failed: {}", GetLastError()));
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

    /// Releases software ownership of the LCD.
    /// The cooler microcontroller resumes its saved hardware screen as soon as the handle is closed.
    /// We do NOT send any reset or clear opcodes, ensuring the onboard hardware image remains intact.
    pub fn release_to_hardware(self) {
        drop(self);
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
    use super::frame_packet;

    #[test]
    fn image_packet_has_the_observed_corsair_header_and_padding() {
        let packet = frame_packet(&[0xff, 0xd8, 0xff], 7, true);
        assert_eq!(&packet[..8], &[0x02, 0x05, 0x40, 0x01, 7, 0, 3, 0]);
        assert_eq!(&packet[8..11], &[0xff, 0xd8, 0xff]);
        assert!(packet[11..].iter().all(|byte| *byte == 0));
    }
}
