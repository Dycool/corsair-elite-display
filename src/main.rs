#![windows_subsystem = "windows"]

mod app;
mod capture;
mod corsair;
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

    // Strict single-instance guard: acquire mutex with initial ownership requested
    let mutex_name = wide("Local\\CorsairEliteDisplay.SingleInstance");
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

    // Ensure virtual monitor is cleaned up on panic
    std::panic::set_hook(Box::new(|_| {
        virtual_display::VirtualDisplayManager::deactivate();
    }));

    app::run(args.iter().any(|arg| arg == "--background"));
    unsafe { CloseHandle(mutex) };
}
