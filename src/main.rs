#![windows_subsystem = "windows"]

mod app;
mod capture;
mod corsair;
mod driver_admin;
mod hardware_media;
mod hardware_restore;
mod settings;
mod virtual_display;

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_APP};

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Elevated driver administration is performed by this same executable.
    // Handle it before the single-instance guard so the normal tray instance
    // can remain open while Windows elevates a short-lived helper copy of us.
    if let Some(exit_code) = driver_admin::handle_admin_helper(&args) {
        std::process::exit(exit_code);
    }

    if args.iter().any(|arg| arg == "--self-test") {
        std::process::exit(match capture::self_test() {
            Ok(()) => 0,
            Err(error) => {
                let _ = std::fs::write(
                    std::env::temp_dir().join("corsair-elite-display-self-test.txt"),
                    error,
                );
                1
            }
        });
    }

    if args.iter().any(|arg| {
        arg == "--hardware-mode"
            || arg == "--restore-hardware"
            || arg == "--off"
            || arg == "-hw"
    }) {
        let result = hardware_restore::restore_hardware_mode();
        let _ = std::fs::write(
            std::env::temp_dir().join("corsair-elite-display-hardware-restore.txt"),
            match &result {
                Ok(()) => "Successfully restored hardware mode\n".to_string(),
                Err(error) => format!("Failed to restore hardware mode: {error}\n"),
            },
        );
        std::process::exit(match result {
            Ok(()) => 0,
            Err(_) => 1,
        });
    }

    // Strict single-instance guard: acquire mutex with initial ownership requested
    let mutex_name = wide("CorsairEliteDisplay_SingleInstance");
    let mutex = unsafe { CreateMutexW(null_mut(), 1, mutex_name.as_ptr()) };
    let last_error = unsafe { GetLastError() };

    if mutex.is_null() || last_error == ERROR_ALREADY_EXISTS || last_error == ERROR_ACCESS_DENIED {
        // Another instance is already running: signal it to show its tray status and exit immediately
        let class_name = wide(app::WINDOW_CLASS);
        let existing = unsafe { FindWindowW(class_name.as_ptr(), null()) };
        if !existing.is_null() {
            unsafe { PostMessageW(existing, WM_APP + 2, 0, 0) };
        }
        if !mutex.is_null() {
            unsafe { CloseHandle(mutex) };
        }
        return;
    }

    // Ensure virtual monitor is cleaned up and hardware mode restored on panic
    std::panic::set_hook(Box::new(|_| {
        let _ = hardware_restore::restore_hardware_mode();
        virtual_display::VirtualDisplayManager::deactivate();
    }));

    app::run(args.iter().any(|arg| arg == "--background"));
    unsafe { CloseHandle(mutex) };
}
