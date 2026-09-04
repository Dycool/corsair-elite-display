use std::ffi::OsStr;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::ptr::{null, null_mut};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HWND};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

use crate::virtual_display::VirtualDisplayManager;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
const SW_HIDE: i32 = 0;
const WAIT_TIMEOUT: u32 = 258;
const ADMIN_INSTALL_ARG: &str = "--ced-admin-install-driver";
const ADMIN_UNINSTALL_ARG: &str = "--ced-admin-uninstall-driver";
const ADMIN_RESULT_FILE: &str = "corsair-elite-display-admin-result.txt";

#[repr(C)]
#[allow(non_snake_case, dead_code)]
struct ShellExecuteInfoW {
    cbSize: u32,
    fMask: u32,
    hwnd: HWND,
    lpVerb: *const u16,
    lpFile: *const u16,
    lpParameters: *const u16,
    lpDirectory: *const u16,
    nShow: i32,
    hInstApp: *mut std::ffi::c_void,
    lpIDList: *mut std::ffi::c_void,
    lpClass: *const u16,
    hkeyClass: *mut std::ffi::c_void,
    dwHotKey: u32,
    hIconOrMonitor: *mut std::ffi::c_void,
    hProcess: *mut std::ffi::c_void,
}

#[link(name = "shell32")]
unsafe extern "system" {
    fn ShellExecuteExW(exec_info: *mut ShellExecuteInfoW) -> i32;
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[repr(C)]
struct SpDevInfoData {
    cb_size: u32,
    class_guid: Guid,
    dev_inst: u32,
    reserved: usize,
}

const DISPLAY_CLASS_GUID: Guid = Guid {
    data1: 0x4d36e968,
    data2: 0xe325,
    data3: 0x11ce,
    data4: [0xbf, 0xc1, 0x08, 0x00, 0x2b, 0xe1, 0x03, 0x18],
};
const INVALID_HANDLE_VALUE: *mut std::ffi::c_void = -1isize as *mut std::ffi::c_void;
const DICD_GENERATE_ID: u32 = 0x0000_0001;
const SPDRP_HARDWAREID: u32 = 0x0000_0001;
const DIF_REGISTERDEVICE: u32 = 0x0000_0019;
const INSTALLFLAG_FORCE: u32 = 0x0000_0001;

#[link(name = "setupapi")]
unsafe extern "system" {
    fn SetupDiCreateDeviceInfoList(
        class_guid: *const Guid,
        hwnd_parent: HWND,
    ) -> *mut std::ffi::c_void;
    fn SetupDiCreateDeviceInfoW(
        device_info_set: *mut std::ffi::c_void,
        device_name: *const u16,
        class_guid: *const Guid,
        device_description: *const u16,
        hwnd_parent: HWND,
        creation_flags: u32,
        device_info_data: *mut SpDevInfoData,
    ) -> i32;
    fn SetupDiSetDeviceRegistryPropertyW(
        device_info_set: *mut std::ffi::c_void,
        device_info_data: *mut SpDevInfoData,
        property: u32,
        property_buffer: *const u8,
        property_buffer_size: u32,
    ) -> i32;
    fn SetupDiCallClassInstaller(
        install_function: u32,
        device_info_set: *mut std::ffi::c_void,
        device_info_data: *mut SpDevInfoData,
    ) -> i32;
    fn SetupDiDestroyDeviceInfoList(device_info_set: *mut std::ffi::c_void) -> i32;
}

#[link(name = "newdev")]
unsafe extern "system" {
    fn UpdateDriverForPlugAndPlayDevicesW(
        hwnd_parent: HWND,
        hardware_id: *const u16,
        full_inf_path: *const u16,
        install_flags: u32,
        reboot_required: *mut i32,
    ) -> i32;
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

fn wide_str(value: &str) -> Vec<u16> {
    wide(OsStr::new(value))
}

fn multi_sz(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(Some(0))
        .chain(Some(0))
        .collect()
}

fn admin_result_path() -> PathBuf {
    std::env::temp_dir().join(ADMIN_RESULT_FILE)
}

fn hidden_output(mut command: Command) -> Result<Output, String> {
    command.creation_flags(CREATE_NO_WINDOW);
    command
        .output()
        .map_err(|error| format!("Could not start {}: {error}", command.get_program().to_string_lossy()))
}

fn output_text(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{stdout}{stderr}")
}

fn copy_driver_configuration(temp_dir: &Path) -> Result<(), String> {
    let destination = PathBuf::from(r"C:\VirtualDisplayDriver");
    std::fs::create_dir_all(&destination)
        .map_err(|error| format!("Could not create {}: {error}", destination.display()))?;

    for name in ["user_edid.bin", "vdd_settings.xml"] {
        let source = temp_dir.join(name);
        let target = destination.join(name);
        std::fs::copy(&source, &target).map_err(|error| {
            format!(
                "Could not copy {} to {}: {error}",
                source.display(),
                target.display()
            )
        })?;
    }
    Ok(())
}

fn add_driver_package(inf_path: &Path) -> Result<(), String> {
    let mut command = Command::new("pnputil.exe");
    command.arg("/add-driver").arg(inf_path).arg("/install");
    let output = hidden_output(command)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("Driver package installation failed:\n{}", output_text(&output)))
    }
}

fn update_driver(inf_path: &Path) -> Result<bool, String> {
    let hardware_id = wide_str(r"Root\MttVDD");
    let inf = wide(inf_path.as_os_str());
    let mut reboot_required = 0i32;
    let result = unsafe {
        UpdateDriverForPlugAndPlayDevicesW(
            null_mut(),
            hardware_id.as_ptr(),
            inf.as_ptr(),
            INSTALLFLAG_FORCE,
            &mut reboot_required,
        )
    };
    if result == 0 {
        Err(format!(
            "Windows could not bind the Virtual Display Driver (error {}).",
            unsafe { GetLastError() }
        ))
    } else {
        Ok(reboot_required != 0)
    }
}

fn create_root_device() -> Result<(), String> {
    let device_set = unsafe { SetupDiCreateDeviceInfoList(&DISPLAY_CLASS_GUID, null_mut()) };
    if device_set == INVALID_HANDLE_VALUE {
        return Err(format!(
            "Could not create a Windows device-information set (error {}).",
            unsafe { GetLastError() }
        ));
    }

    let result = (|| {
        let mut device_info = SpDevInfoData {
            cb_size: size_of::<SpDevInfoData>() as u32,
            class_guid: DISPLAY_CLASS_GUID,
            dev_inst: 0,
            reserved: 0,
        };
        let device_name = wide_str("MttVDD");
        let description = wide_str("Corsair Virtual Screen");

        if unsafe {
            SetupDiCreateDeviceInfoW(
                device_set,
                device_name.as_ptr(),
                &DISPLAY_CLASS_GUID,
                description.as_ptr(),
                null_mut(),
                DICD_GENERATE_ID,
                &mut device_info,
            )
        } == 0
        {
            return Err(format!(
                "Could not create the Root\\MttVDD device node (error {}).",
                unsafe { GetLastError() }
            ));
        }

        let hardware_id = multi_sz(r"Root\MttVDD");
        if unsafe {
            SetupDiSetDeviceRegistryPropertyW(
                device_set,
                &mut device_info,
                SPDRP_HARDWAREID,
                hardware_id.as_ptr().cast::<u8>(),
                (hardware_id.len() * size_of::<u16>()) as u32,
            )
        } == 0
        {
            return Err(format!(
                "Could not assign the Root\\MttVDD hardware ID (error {}).",
                unsafe { GetLastError() }
            ));
        }

        if unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, device_set, &mut device_info) } == 0 {
            return Err(format!(
                "Could not register the Root\\MttVDD virtual-display device (error {}).",
                unsafe { GetLastError() }
            ));
        }

        Ok(())
    })();

    unsafe {
        SetupDiDestroyDeviceInfoList(device_set);
    }
    result
}

fn restart_virtual_display_best_effort() {
    let mut command = Command::new("pnputil.exe");
    command.args(["/restart-device", r"Root\MttVDD"]);
    let _ = hidden_output(command);
}

fn install_driver_as_admin() -> Result<(), String> {
    if !VirtualDisplayManager::is_admin() {
        return Err("The driver helper was not started with administrator privileges.".into());
    }

    let temp_dir = VirtualDisplayManager::prepare_driver_files()?;
    copy_driver_configuration(&temp_dir)?;
    let inf_path = temp_dir.join("mttvdd.inf");

    // Always stage/install the package. This also repairs a partially removed
    // package instead of trusting stale display-enumeration state.
    add_driver_package(&inf_path)?;

    // First try to bind an existing Root\MttVDD node. This is the normal path
    // after an uninstall performed by current versions of the app.
    let first_update = update_driver(&inf_path);
    thread::sleep(Duration::from_millis(500));

    if !VirtualDisplayManager::is_driver_installed() {
        // Older versions removed the root-enumerated device itself. PnPUtil can
        // stage the INF but cannot recreate such a node, so reconstruct it with
        // SetupAPI and bind the embedded driver to it.
        if let Err(update_error) = first_update {
            create_root_device().map_err(|create_error| {
                format!(
                    "{update_error}\n\nThe previous uninstall also removed the virtual-display device node, and recreating it failed: {create_error}"
                )
            })?;
            update_driver(&inf_path)?;
        } else {
            // The update API reported success but GDI has not exposed the device
            // yet. Wait briefly before creating a second node.
            for _ in 0..10 {
                thread::sleep(Duration::from_millis(200));
                if VirtualDisplayManager::is_driver_installed() {
                    break;
                }
            }
            if !VirtualDisplayManager::is_driver_installed() {
                create_root_device()?;
                update_driver(&inf_path)?;
            }
        }
    }

    restart_virtual_display_best_effort();

    for _ in 0..40 {
        if VirtualDisplayManager::is_ready() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }

    Err("The driver package was installed, but Windows did not expose the Corsair Virtual Screen device after installation.".into())
}

fn published_driver_names(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut result = Vec::new();

    for (index, line) in lines.iter().enumerate() {
        if !line.to_ascii_lowercase().contains("mttvdd.inf") {
            continue;
        }
        let start = index.saturating_sub(10);
        let end = (index + 10).min(lines.len().saturating_sub(1));
        for candidate_line in &lines[start..=end] {
            for token in candidate_line.split_whitespace() {
                let token = token.trim_matches(|character: char| {
                    !character.is_ascii_alphanumeric() && character != '.'
                });
                let lower = token.to_ascii_lowercase();
                if lower.starts_with("oem")
                    && lower.ends_with(".inf")
                    && lower[3..lower.len() - 4].chars().all(|character| character.is_ascii_digit())
                    && !result.iter().any(|existing: &String| existing.eq_ignore_ascii_case(token))
                {
                    result.push(token.to_string());
                }
            }
        }
    }

    result
}

fn uninstall_driver_as_admin() -> Result<(), String> {
    if !VirtualDisplayManager::is_admin() {
        return Err("The driver helper was not started with administrator privileges.".into());
    }

    VirtualDisplayManager::deactivate();

    let mut enum_command = Command::new("pnputil.exe");
    enum_command.arg("/enum-drivers");
    let enum_output = hidden_output(enum_command)?;
    if !enum_output.status.success() {
        return Err(format!(
            "Could not enumerate installed Windows drivers:\n{}",
            output_text(&enum_output)
        ));
    }

    let packages = published_driver_names(&String::from_utf8_lossy(&enum_output.stdout));
    for package in packages {
        let mut command = Command::new("pnputil.exe");
        command
            .arg("/delete-driver")
            .arg(&package)
            .args(["/uninstall", "/force"]);
        let output = hidden_output(command)?;
        if !output.status.success() {
            return Err(format!(
                "Could not remove driver package {package}:\n{}",
                output_text(&output)
            ));
        }
    }

    // Deliberately preserve the unbound Root\MttVDD device node. Removing the
    // node is what made install -> uninstall -> install fail: PnPUtil stages an
    // INF but does not create root-enumerated devices. Leaving the harmless,
    // unbound node lets a future install rebind immediately; the install helper
    // can also recreate it if an older version already removed it.
    let config_dir = PathBuf::from(r"C:\VirtualDisplayDriver");
    if config_dir.exists() {
        std::fs::remove_dir_all(&config_dir).map_err(|error| {
            format!("Could not remove {}: {error}", config_dir.display())
        })?;
    }

    Ok(())
}

fn write_admin_result(result: &Result<(), String>) {
    let body = match result {
        Ok(()) => "OK\n".to_string(),
        Err(error) => format!("ERROR\n{error}\n"),
    };
    let _ = std::fs::write(admin_result_path(), body);
}

/// Handles the hidden elevated helper mode before the normal single-instance
/// tray application is created. Returns Some(exit_code) when an admin helper
/// argument was present, otherwise None.
pub fn handle_admin_helper(args: &[String]) -> Option<i32> {
    let action = if args.iter().any(|arg| arg == ADMIN_INSTALL_ARG) {
        Some(install_driver_as_admin as fn() -> Result<(), String>)
    } else if args.iter().any(|arg| arg == ADMIN_UNINSTALL_ARG) {
        Some(uninstall_driver_as_admin as fn() -> Result<(), String>)
    } else {
        None
    }?;

    let result = action();
    write_admin_result(&result);
    Some(if result.is_ok() { 0 } else { 1 })
}

fn request_admin(action: &str, hwnd: HWND) -> Result<(), String> {
    let result_path = admin_result_path();
    let _ = std::fs::remove_file(&result_path);

    let executable = std::env::current_exe()
        .map_err(|error| format!("Could not locate the running application: {error}"))?;
    let verb = wide_str("runas");
    let file = wide(executable.as_os_str());
    let parameters = wide_str(action);

    let mut info: ShellExecuteInfoW = unsafe { zeroed() };
    info.cbSize = size_of::<ShellExecuteInfoW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.hwnd = hwnd;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = parameters.as_ptr();
    info.lpDirectory = null();
    info.nShow = SW_HIDE;

    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let error = unsafe { GetLastError() };
        if error == 1223 {
            return Err("Administrator permission was not granted (the UAC prompt was cancelled).".into());
        }
        return Err(format!(
            "Windows could not elevate Corsair Elite Display (error {error})."
        ));
    }

    if info.hProcess.is_null() {
        return Err("Windows did not return a process handle for the elevated app.".into());
    }

    let wait = unsafe { WaitForSingleObject(info.hProcess, 120_000) };
    if wait == WAIT_TIMEOUT {
        unsafe {
            CloseHandle(info.hProcess);
        }
        return Err("The elevated driver operation timed out after two minutes.".into());
    }

    let mut exit_code = 1u32;
    let got_exit_code = unsafe { GetExitCodeProcess(info.hProcess, &mut exit_code) } != 0;
    unsafe {
        CloseHandle(info.hProcess);
    }

    let result_text = std::fs::read_to_string(&result_path).unwrap_or_default();
    let _ = std::fs::remove_file(&result_path);

    if !got_exit_code {
        return Err("Could not read the result of the elevated driver operation.".into());
    }
    if exit_code != 0 {
        let detail = result_text
            .strip_prefix("ERROR\n")
            .unwrap_or(result_text.as_str())
            .trim();
        return Err(if detail.is_empty() {
            format!("The elevated driver operation failed with exit code {exit_code}.")
        } else {
            detail.to_string()
        });
    }

    Ok(())
}

pub fn install_driver_elevated(hwnd: HWND) -> Result<(), String> {
    request_admin(ADMIN_INSTALL_ARG, hwnd)?;
    for _ in 0..20 {
        if VirtualDisplayManager::is_ready() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
    Err("The elevated install completed, but the Corsair Virtual Screen is still not ready.".into())
}

pub fn uninstall_driver_elevated(hwnd: HWND) -> Result<(), String> {
    request_admin(ADMIN_UNINSTALL_ARG, hwnd)?;
    if VirtualDisplayManager::is_ready() {
        return Err("The elevated uninstall completed, but the Corsair Virtual Screen still appears to be configured.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::published_driver_names;

    #[test]
    fn finds_published_mttvdd_package_without_relying_on_localized_labels() {
        let sample = "Published Name: oem42.inf\nOriginal Name: mttvdd.inf\nProvider Name: MikeTheTech\n";
        assert_eq!(published_driver_names(sample), vec!["oem42.inf"]);
    }
}
