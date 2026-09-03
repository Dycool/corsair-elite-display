use std::mem::{size_of, zeroed};
use std::path::PathBuf;
use std::process::Command;
use std::ptr::null;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Graphics::Gdi::{
    DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICEW, EnumDisplayDevicesW,
};

fn from_wide(buffer: &[u16]) -> String {
    let length = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..length])
}

fn monitor_attached() -> bool {
    for index in 0..64 {
        let mut device: DISPLAY_DEVICEW = unsafe { zeroed() };
        device.cb = size_of::<DISPLAY_DEVICEW>() as u32;
        if unsafe { EnumDisplayDevicesW(null(), index, &mut device, 0) } == 0 {
            break;
        }
        let id = from_wide(&device.DeviceID).to_ascii_lowercase();
        if id.contains("mttvdd") && device.StateFlags & DISPLAY_DEVICE_ATTACHED_TO_DESKTOP != 0 {
            return true;
        }
    }
    false
}

fn display_switch(mode: &str) -> Result<(), String> {
    let system_root = std::env::var_os("SystemRoot").ok_or("Windows directory is unavailable")?;
    let executable = PathBuf::from(system_root)
        .join("System32")
        .join("DisplaySwitch.exe");
    let status = Command::new(executable)
        .arg(mode)
        .status()
        .map_err(|error| format!("Could not change the display layout: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("Windows could not change the display layout".into())
    }
}

pub struct VirtualDisplayManager;

impl VirtualDisplayManager {
    pub fn activate() -> Result<(), String> {
        if monitor_attached() {
            return Ok(());
        }
        display_switch("/extend")?;
        for _ in 0..30 {
            if monitor_attached() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err("The runtime virtual screen is not available on this PC".into())
    }

    pub fn deactivate() {
        if monitor_attached() {
            let _ = display_switch("/internal");
        }
    }
}
