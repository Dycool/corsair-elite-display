use std::mem::{size_of, zeroed};
use std::path::PathBuf;
use std::process::Command;
use std::ptr::null;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Graphics::Gdi::{
    DISPLAY_DEVICE_ATTACHED_TO_DESKTOP, DISPLAY_DEVICEW, EnumDisplayDevicesW,
};

// Embedded standalone driver files (zero external downloads needed)
const DRIVER_INF: &[u8] = include_bytes!("driver/mttvdd.inf");
const DRIVER_CAT: &[u8] = include_bytes!("driver/MttVDD.cat");
const DRIVER_DLL: &[u8] = include_bytes!("driver/MttVDD.dll");

const VDD_SETTINGS_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<vdd_settings>
  <monitors><count>1</count></monitors>
  <gpu><friendlyname>default</friendlyname></gpu>
  <global><g_refresh_rate>30</g_refresh_rate></global>
  <resolutions>
    <resolution><width>480</width><height>480</height><refresh_rate>30</refresh_rate></resolution>
  </resolutions>
  <options>
    <CustomEdid>true</CustomEdid><PreventSpoof>false</PreventSpoof>
    <EdidCeaOverride>false</EdidCeaOverride><HardwareCursor>true</HardwareCursor>
    <SDR10bit>false</SDR10bit><HDRPlus>false</HDRPlus>
    <logging>false</logging><debuglogging>false</debuglogging>
  </options>
</vdd_settings>
"#;

pub fn generate_corsair_edid(name: &str) -> [u8; 128] {
    let mut edid = [0u8; 128];
    // EDID Header
    edid[0..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
    // Vendor ID: "COR" (Corsair) -> (3 << 10) | (15 << 5) | 18 = 0x0DE2
    edid[8] = 0x0D;
    edid[9] = 0xE2;
    // Product ID: 0x0C39
    edid[10] = 0x39;
    edid[11] = 0x0C;
    // Serial number: 1
    edid[12] = 0x01;
    edid[13] = 0x00;
    edid[14] = 0x00;
    edid[15] = 0x00;
    // Manufacture Week 1, Year 2024 (1990 + 34)
    edid[16] = 1;
    edid[17] = 34;
    // EDID Version 1.3
    edid[18] = 1;
    edid[19] = 3;
    // Basic display parameters: Digital input, 7cm x 7cm
    edid[20] = 0x80;
    edid[21] = 7;
    edid[22] = 7;
    edid[23] = 120; // Gamma 2.2
    edid[24] = 0x02; // Features (RGB 4:4:4)
    // Color characteristics
    edid[25..35].copy_from_slice(&[0xEE, 0x91, 0xA3, 0x54, 0x4C, 0x99, 0x26, 0x0F, 0x50, 0x54]);
    // Standard timings unused
    for i in (38..54).step_by(2) {
        edid[i] = 0x01;
        edid[i + 1] = 0x01;
    }
    // Detailed Timing Descriptor: 480x480 @ 30Hz / 60Hz
    edid[54] = 0x40;
    edid[55] = 0x06;
    edid[56] = (480 & 0xFF) as u8;
    edid[57] = 40;
    edid[58] = (((480 >> 8) << 4) | (40 >> 8)) as u8;
    edid[59] = (480 & 0xFF) as u8;
    edid[60] = 33;
    edid[61] = (((480 >> 8) << 4) | (33 >> 8)) as u8;
    edid[62] = 16;
    edid[63] = 16;
    edid[64] = (3 << 4) | 3;
    edid[65] = 0x00;
    edid[66] = 70;
    edid[67] = 70;
    edid[71] = 0x1E;

    // Descriptor 1: 0xFC (Monitor Name)
    edid[72..77].copy_from_slice(&[0x00, 0x00, 0x00, 0xFC, 0x00]);
    let name_bytes = name.as_bytes();
    let name_len = name_bytes.len().min(13);
    edid[77..77 + name_len].copy_from_slice(&name_bytes[..name_len]);
    if name_len < 13 {
        edid[77 + name_len] = 0x0A;
        for j in (77 + name_len + 1)..90 {
            edid[j] = 0x20;
        }
    }

    // Descriptor 2: 0xFD (Monitor Range Limits)
    edid[90..95].copy_from_slice(&[0x00, 0x00, 0x00, 0xFD, 0x00]);
    edid[95] = 24; // Min V freq
    edid[96] = 65; // Max V freq
    edid[97] = 15; // Min H freq
    edid[98] = 80; // Max H freq
    edid[99] = 10; // Max clock
    edid[100] = 0x00;
    edid[101] = 0x0A;
    for j in 102..108 {
        edid[j] = 0x20;
    }

    // Descriptor 3: 0xFF (Serial Number)
    edid[108..113].copy_from_slice(&[0x00, 0x00, 0x00, 0xFF, 0x00]);
    let sn = b"0C390001\n";
    edid[113..113 + sn.len()].copy_from_slice(sn);
    for j in (113 + sn.len())..126 {
        edid[j] = 0x20;
    }

    edid[126] = 0; // Extension flag
    let sum: u32 = edid[0..127].iter().map(|&b| b as u32).sum();
    edid[127] = ((256 - (sum % 256)) % 256) as u8;

    edid
}

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
    pub fn is_admin() -> bool {
        unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
    }

    pub fn is_ready() -> bool {
        if !Self::is_driver_installed() {
            return false;
        }
        let dir = PathBuf::from(r"C:\VirtualDisplayDriver");
        let edid_path = dir.join("user_edid.bin");
        let settings_path = dir.join("vdd_settings.xml");
        if !edid_path.exists() || !settings_path.exists() {
            return false;
        }
        if let Ok(settings) = std::fs::read_to_string(&settings_path) {
            if !settings.contains("<CustomEdid>true</CustomEdid>") || !settings.contains("<width>480</width>") {
                return false;
            }
        } else {
            return false;
        }
        true
    }

    pub fn is_driver_installed() -> bool {
        for index in 0..64 {
            let mut device: DISPLAY_DEVICEW = unsafe { zeroed() };
            device.cb = size_of::<DISPLAY_DEVICEW>() as u32;
            if unsafe { EnumDisplayDevicesW(null(), index, &mut device, 0) } == 0 {
                break;
            }
            let id = from_wide(&device.DeviceID).to_ascii_lowercase();
            if id.contains("mttvdd") {
                return true;
            }
        }
        false
    }

    pub fn ensure_configured() -> Result<(), String> {
        let dir = PathBuf::from(r"C:\VirtualDisplayDriver");
        if !dir.exists() {
            if let Err(e) = std::fs::create_dir_all(&dir) {
                if !Self::is_admin() {
                    return Err("Administrator privileges are required to configure the Virtual Screen.\n\nPlease run Corsair Elite Display as Administrator.".into());
                }
                return Err(format!("Could not create C:\\VirtualDisplayDriver folder: {e}"));
            }
        }

        let edid_path = dir.join("user_edid.bin");
        let edid_bytes = generate_corsair_edid("Corsair LCD");
        let needs_edid = match std::fs::read(&edid_path) {
            Ok(existing) => existing != edid_bytes,
            Err(_) => true,
        };
        if needs_edid {
            if let Err(e) = std::fs::write(&edid_path, &edid_bytes) {
                if !Self::is_admin() {
                    return Err("Administrator privileges are required to configure the Virtual Screen EDID.\n\nPlease run Corsair Elite Display as Administrator.".into());
                }
                return Err(format!("Could not write user_edid.bin: {e}"));
            }
        }

        let settings_path = dir.join("vdd_settings.xml");
        let needs_settings = match std::fs::read_to_string(&settings_path) {
            Ok(existing) => !existing.contains("<CustomEdid>true</CustomEdid>") || !existing.contains("<width>480</width>"),
            Err(_) => true,
        };
        if needs_settings {
            if let Err(e) = std::fs::write(&settings_path, VDD_SETTINGS_XML) {
                if !Self::is_admin() {
                    return Err("Administrator privileges are required to configure Virtual Screen settings.\n\nPlease run Corsair Elite Display as Administrator.".into());
                }
                return Err(format!("Could not write vdd_settings.xml: {e}"));
            }
        }

        if !Self::is_driver_installed() {
            return Err("The Corsair Virtual Screen driver is not installed.\n\nUse Corsair Elite Display's normal driver installation prompt so Windows can request administrator permission explicitly.".into());
        }

        Ok(())
    }

    pub fn prepare_driver_files() -> Result<PathBuf, String> {
        let temp_dir = std::env::temp_dir().join("corsair_vdd");
        let _ = std::fs::create_dir_all(&temp_dir);

        let inf_path = temp_dir.join("mttvdd.inf");
        let cat_path = temp_dir.join("MttVDD.cat");
        let dll_path = temp_dir.join("MttVDD.dll");
        let edid_path = temp_dir.join("user_edid.bin");
        let settings_path = temp_dir.join("vdd_settings.xml");

        std::fs::write(&inf_path, DRIVER_INF)
            .map_err(|e| format!("Could not unpack driver inf: {e}"))?;
        std::fs::write(&cat_path, DRIVER_CAT)
            .map_err(|e| format!("Could not unpack driver cat: {e}"))?;
        std::fs::write(&dll_path, DRIVER_DLL)
            .map_err(|e| format!("Could not unpack driver dll: {e}"))?;
        std::fs::write(&edid_path, generate_corsair_edid("Corsair LCD"))
            .map_err(|e| format!("Could not unpack user_edid.bin: {e}"))?;
        std::fs::write(&settings_path, VDD_SETTINGS_XML)
            .map_err(|e| format!("Could not unpack vdd_settings.xml: {e}"))?;

        Ok(temp_dir)
    }

    pub fn activate() -> Result<(), String> {
        Self::ensure_configured()?;

        if monitor_attached() {
            return Ok(());
        }

        display_switch("/extend")?;
        for _ in 0..30 {
            if monitor_attached() {
                // Allow Windows DWM a moment to compose the desktop plane on the re-attached display
                thread::sleep(Duration::from_millis(200));
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }

        if !Self::is_driver_installed() {
            return Err("Virtual Screen is not installed.\n\nUse the app's driver installation prompt to install and configure it.".into());
        }

        Err("The Corsair Virtual Screen is not available. Please verify the display driver is installed.".into())
    }

    pub fn deactivate() {
        if monitor_attached() {
            let _ = display_switch("/internal");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edid_checksum() {
        let edid = generate_corsair_edid("Corsair LCD");
        let sum: u32 = edid.iter().map(|&b| b as u32).sum();
        assert_eq!(sum % 256, 0);
        // Verify name descriptor
        assert_eq!(&edid[72..77], &[0x00, 0x00, 0x00, 0xFC, 0x00]);
        assert_eq!(&edid[77..88], b"Corsair LCD");
    }
}
