#![windows_subsystem = "windows"]

mod app;
mod capture;
mod corsair;
mod settings;
mod virtual_display;

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, PostMessageW, WM_APP};

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn main() {
    if std::env::args().any(|arg| arg == "--self-test") {
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

    if std::env::args().any(|arg| arg == "--install-driver") {
        virtual_display::install_embedded_driver();
        return;
    }

    let mutex_name = wide("Local\\CorsairEliteDisplay.SingleInstance");
    let mutex = unsafe { CreateMutexW(null_mut(), 0, mutex_name.as_ptr()) };
    if mutex.is_null() {
        return;
    }

    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let class_name = wide(app::WINDOW_CLASS);
        let existing = unsafe { FindWindowW(class_name.as_ptr(), null()) };
        if !existing.is_null() {
            unsafe { PostMessageW(existing, WM_APP + 2, 0, 0) };
        }
        unsafe { CloseHandle(mutex) };
        return;
    }

    app::run(std::env::args().any(|arg| arg == "--background"));
    unsafe { CloseHandle(mutex) };
}
