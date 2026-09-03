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
}

fn to_u16_vec(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

#[allow(dead_code)]
pub struct CorsairLcdDevice {
    handle: *mut std::ffi::c_void,
    pub path: String,
}

unsafe impl Send for CorsairLcdDevice {}
unsafe impl Sync for CorsairLcdDevice {}

impl CorsairLcdDevice {
    pub fn find_device_path() -> Option<String> {
        let supported_pids = ["0C39", "0C33", "0C4E", "0C42"];
        unsafe {
            let mut len = 0u32;
            let res = CM_Get_Device_Interface_List_SizeW(
                &mut len,
                GUID_DEVINTERFACE_HID.as_ptr(),
                null_mut(),
                0,
            );
            if res != 0 || len == 0 {
                return None;
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
                return None;
            }

            let mut start = 0;
            for i in 0..buffer.len() {
                if buffer[i] == 0 {
                    if start < i {
                        let s = OsString::from_wide(&buffer[start..i]).to_string_lossy().to_string();
                        let upper = s.to_uppercase();
                        if upper.contains("VID_1B1C") && supported_pids.iter().any(|pid| upper.contains(pid)) {
                            return Some(s);
                        }
                    }
                    start = i + 1;
                }
            }
        }
        None
    }

    pub fn open() -> Result<Self, String> {
        let path = Self::find_device_path()
            .ok_or_else(|| "No supported Corsair LCD device found via PnP".to_string())?;

        let path_u16 = to_u16_vec(&path);
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
            let err = unsafe { GetLastError() };
            return Err(format!("Failed to open device handle (Win32 error {})", err));
        }

        Ok(Self { handle, path })
    }

    pub fn set_brightness(&self, percent: u8) -> Result<(), String> {
        let raw_val = match percent {
            0..=16 => 0x01,
            17..=49 => 0x04,
            50..=83 => 0x10,
            _ => 0x40,
        };

        let mut packet = [0u8; 32];
        packet[0] = 0x03; // Report ID
        packet[1] = 0x0B; // Opcode
        packet[2] = raw_val;

        unsafe {
            let res = HidD_SetFeature(self.handle, packet.as_ptr(), packet.len() as u32);
            if res == 0 {
                return Err(format!("HidD_SetFeature (brightness) failed: {}", GetLastError()));
            }
        }
        Ok(())
    }

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
        packet[1] = 0x0C; // Opcode
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
        let mut part_num: u16 = 0;

        for chunk in jpeg_data.chunks(real_max_len) {
            let chunk_len = chunk.len();
            let is_end = if (part_num as usize * real_max_len) + chunk_len >= jpeg_data.len() {
                1u8
            } else {
                0u8
            };

            let mut packet = Vec::with_capacity(max_len);
            packet.push(0x02); // Opcode IMG_TX
            packet.push(0x05);
            packet.push(0x40);
            packet.push(is_end);
            packet.extend_from_slice(&part_num.to_le_bytes());
            packet.extend_from_slice(&(chunk_len as u16).to_le_bytes());
            packet.extend_from_slice(chunk);

            if packet.len() < max_len {
                packet.resize(max_len, 0x00);
            }

            unsafe {
                let mut written = 0u32;
                let success = WriteFile(
                    self.handle,
                    packet.as_ptr(),
                    packet.len() as u32,
                    &mut written,
                    null_mut(),
                );
                if success == 0 {
                    let err = GetLastError();
                    return Err(format!("WriteFile failed with error {}", err));
                }
            }

            part_num += 1;
        }

        Ok(())
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
