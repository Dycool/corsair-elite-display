use std::ffi::OsStr;
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::null_mut;

use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MessageBoxW, SW_SHOWNORMAL,
};

const VDD_DIR: &str = r"C:\VirtualDisplayDriver";
const VDD_SETTINGS_PATH: &str = r"C:\VirtualDisplayDriver\vdd_settings.xml";
const DEVCON: &[u8] = include_bytes!("../../Dependencies/devcon.exe");
const DRIVER_INF: &[u8] = include_bytes!("../../SignedDrivers/x86/VDD/MttVDD.inf");
const DRIVER_CAT: &[u8] = include_bytes!("../../SignedDrivers/x86/VDD/mttvdd.cat");
const DRIVER_DLL: &[u8] = include_bytes!("../../SignedDrivers/x86/VDD/MttVDD.dll");

const SETTINGS_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<vdd_settings>
  <monitors><count>1</count></monitors>
  <gpu><friendlyname>default</friendlyname></gpu>
  <global><g_refresh_rate>30</g_refresh_rate><g_refresh_rate>60</g_refresh_rate></global>
  <resolutions>
    <resolution><width>480</width><height>480</height><refresh_rate>30</refresh_rate></resolution>
    <resolution><width>480</width><height>480</height><refresh_rate>60</refresh_rate></resolution>
  </resolutions>
  <options>
    <CustomEdid>false</CustomEdid><PreventSpoof>false</PreventSpoof>
    <EdidCeaOverride>false</EdidCeaOverride><HardwareCursor>true</HardwareCursor>
    <SDR10bit>false</SDR10bit><HDRPlus>false</HDRPlus>
    <logging>false</logging><debuglogging>false</debuglogging>
  </options>
</vdd_settings>
"#;

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

pub struct VirtualDisplayManager;

impl VirtualDisplayManager {
    pub fn request_install() -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let operation = wide("runas");
        let file = wide(&exe.to_string_lossy());
        let parameters = wide("--install-driver");
        let result = unsafe {
            ShellExecuteW(
                null_mut(),
                operation.as_ptr(),
                file.as_ptr(),
                parameters.as_ptr(),
                null_mut(),
                SW_SHOWNORMAL,
            )
        } as isize;
        if result <= 32 {
            Err(format!(
                "Windows could not start the administrator installer ({result})"
            ))
        } else {
            Ok(())
        }
    }
}

fn write_payload(directory: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(directory).map_err(|e| format!("Could not create installer folder: {e}"))?;
    fs::write(directory.join("devcon.exe"), DEVCON).map_err(|e| e.to_string())?;
    fs::write(directory.join("MttVDD.inf"), DRIVER_INF).map_err(|e| e.to_string())?;
    fs::write(directory.join("mttvdd.cat"), DRIVER_CAT).map_err(|e| e.to_string())?;
    fs::write(directory.join("MttVDD.dll"), DRIVER_DLL).map_err(|e| e.to_string())?;
    fs::create_dir_all(VDD_DIR).map_err(|e| format!("Could not create {VDD_DIR}: {e}"))?;
    fs::write(VDD_SETTINGS_PATH, SETTINGS_XML)
        .map_err(|e| format!("Could not write display settings: {e}"))?;
    Ok(directory.join("devcon.exe"))
}

fn perform_install() -> Result<(), String> {
    let directory = std::env::temp_dir().join("CorsairEliteDisplayDriver-1.0");
    let devcon = write_payload(&directory)?;
    let status = Command::new(devcon)
        .current_dir(&directory)
        .args(["install", "MttVDD.inf", r"Root\MttVDD"])
        .status()
        .map_err(|e| format!("Could not run the driver installer: {e}"))?;
    if !status.success() {
        return Err(format!(
            "The virtual display driver installer returned {status}"
        ));
    }
    let _ = Command::new("DisplaySwitch.exe").arg("/extend").status();
    Ok(())
}

pub fn install_embedded_driver() {
    let result = perform_install();
    let (message, title, flags) = match result {
        Ok(()) => (
            "The 480x480 virtual display was installed. Windows has been switched to Extend mode.",
            "Corsair Elite Display",
            MB_OK | MB_ICONINFORMATION,
        ),
        Err(ref error) => (
            error.as_str(),
            "Driver installation failed",
            MB_OK | MB_ICONERROR,
        ),
    };
    let message = wide(message);
    let title = wide(title);
    unsafe {
        MessageBoxW(null_mut(), message.as_ptr(), title.as_ptr(), flags);
    }
}
