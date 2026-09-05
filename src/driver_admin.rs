use std::ffi::OsStr;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HWND};
use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

use crate::virtual_display::VirtualDisplayManager;

const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
const SW_SHOWNORMAL: i32 = 1;
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
    fn DiInstallDriverW(
        hwnd_parent: HWND,
        inf_path: *const u16,
        flags: u32,
        need_reboot: *mut i32,
    ) -> i32;
    fn DiUninstallDriverW(
        hwnd_parent: HWND,
        inf_path: *const u16,
        flags: u32,
        need_reboot: *mut i32,
    ) -> i32;
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

fn install_driver_package(inf_path: &Path) -> Result<bool, String> {
    let inf = wide(inf_path.as_os_str());
    let mut reboot_required = 0i32;
    let result = unsafe {
        DiInstallDriverW(
            null_mut(),
            inf.as_ptr(),
            0,
            &mut reboot_required,
        )
    };
    if result == 0 {
        Err(format!(
            "Windows could not install the Virtual Display Driver package (error {}).",
            unsafe { GetLastError() }
        ))
    } else {
        Ok(reboot_required != 0)
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

fn install_driver_as_admin() -> Result<(), String> {
    if !VirtualDisplayManager::is_admin() {
        return Err("The driver helper was not started with administrator privileges.".into());
    }

    let temp_dir = VirtualDisplayManager::prepare_driver_files()?;
    copy_driver_configuration(&temp_dir)?;
    let inf_path = temp_dir.join("mttvdd.inf");

    // Use Microsoft's documented device-installation APIs directly. Avoiding a
    // hidden pnputil child process makes the privileged operation easier to
    // audit and keeps the app's behavior closer to a normal Windows installer.
    install_driver_package(&inf_path)?;

    let first_update = update_driver(&inf_path);
    thread::sleep(Duration::from_millis(500));

    if !VirtualDisplayManager::is_driver_installed() {
        // Older releases could remove the root-enumerated node. Recreate it via
        // SetupAPI only when Windows no longer exposes an MttVDD device.
        if let Err(update_error) = first_update {
            create_root_device().map_err(|create_error| {
                format!(
                    "{update_error}\n\nThe previous uninstall also removed the virtual-display device node, and recreating it failed: {create_error}"
                )
            })?;
            update_driver(&inf_path)?;
        } else {
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

    for _ in 0..40 {
        if VirtualDisplayManager::is_ready() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }

    Err("The driver package was installed, but Windows did not expose the Corsair Virtual Screen device after installation.".into())
}

fn uninstall_driver_as_admin() -> Result<(), String> {
    if !VirtualDisplayManager::is_admin() {
        return Err("The driver helper was not started with administrator privileges.".into());
    }

    VirtualDisplayManager::deactivate();

    if VirtualDisplayManager::is_driver_installed() {
        let temp_dir = VirtualDisplayManager::prepare_driver_files()?;
        let inf_path = temp_dir.join("mttvdd.inf");
        let inf = wide(inf_path.as_os_str());
        let mut reboot_required = 0i32;
        let result = unsafe {
            DiUninstallDriverW(
                null_mut(),
                inf.as_ptr(),
                0,
                &mut reboot_required,
            )
        };
        if result == 0 {
            return Err(format!(
                "Windows could not uninstall the Virtual Display Driver package (error {}).",
                unsafe { GetLastError() }
            ));
        }
    }

    // Deliberately preserve the unbound Root\MttVDD device node. A future
    // install can rebind it immediately, and the install path can recreate it
    // if an older release has already removed it.
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

/// Handles the elevated helper mode before the normal single-instance tray app
/// is created. Returns Some(exit_code) when an admin-helper argument was
/// present, otherwise None.
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
    info.nShow = SW_SHOWNORMAL;

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
