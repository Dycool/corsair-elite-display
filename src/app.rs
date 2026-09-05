use std::ffi::{OsStr, c_void};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::capture::{StreamController, get_monitors};
use crate::settings::{Settings, ViewMode, set_startup, startup_enabled};
use crate::virtual_display::VirtualDisplayManager;

pub const WINDOW_CLASS: &str = "CorsairEliteDisplayWindow";
const APP_TITLE: &str = "Corsair Elite Display";
const WM_TRAY: u32 = WM_APP + 1;
const WM_SHOW_APP: u32 = WM_APP + 2;
const TIMER_STATUS: usize = 1;

const MENU_TOGGLE: usize = 200;
const MENU_STARTUP: usize = 201;
const MENU_EXIT: usize = 202;
const MENU_SHOW_MOUSE: usize = 203;
const MENU_SET_HARDWARE_IMAGE: usize = 204;
const MENU_UNINSTALL_DRIVER: usize = 205;
const MENU_VIEW_BASE: usize = 250;
const MENU_FPS_BASE: usize = 300;
const MENU_QUALITY_BASE: usize = 400;
const MENU_BRIGHTNESS_BASE: usize = 500;
const MENU_ROTATION_BASE: usize = 600;

const FPS_VALUES: [u32; 4] = [10, 15, 20, 30];
const QUALITY_VALUES: [u8; 4] = [55, 65, 75, 85];
// The capture pipeline scales encoded RGB values, so the old 25/50/75 settings
// look approximately like 0/25/50% luminance. Preserve those proven levels,
// keep 100% untouched, and add an interpolated level for a useful 75% step.
const BRIGHTNESS_VALUES: [u8; 5] = [25, 50, 75, 88, 100];
const BRIGHTNESS_LABELS: [u8; 5] = [0, 25, 50, 75, 100];
const ROTATION_VALUES: [u16; 4] = [0, 90, 180, 270];

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn resource_id(id: usize) -> *const u16 {
    std::ptr::with_exposed_provenance(id)
}

fn copy_wide<const N: usize>(target: &mut [u16; N], value: &str) {
    let value = wide(value);
    let length = value.len().min(N);
    target[..length].copy_from_slice(&value[..length]);
}

struct AppState {
    hwnd: HWND,
    settings: Settings,
    controller: StreamController,
    taskbar_created: u32,
    icon_on: HICON,
    icon_off: HICON,
}

impl AppState {
    fn new() -> Self {
        let settings = Settings::load();
        Self {
            hwnd: null_mut(),
            controller: StreamController::new(settings.clone()),
            settings,
            taskbar_created: 0,
            icon_on: null_mut(),
            icon_off: null_mut(),
        }
    }

    unsafe fn initialize(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        let instance = unsafe { GetModuleHandleW(null()) };
        self.icon_on = unsafe { LoadIconW(instance, resource_id(1)) };
        self.icon_off = unsafe { LoadIconW(instance, resource_id(2)) };
        if self.icon_on.is_null() {
            self.icon_on = unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
        }
        if self.icon_off.is_null() {
            self.icon_off = self.icon_on;
        }
        unsafe { windows_sys::Win32::Media::timeBeginPeriod(1) };
        self.taskbar_created = unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) };
        self.add_tray_icon();

        if !VirtualDisplayManager::is_ready() {
            let msg = wide(
                "The Corsair Virtual Screen driver is not installed or configured.\n\nAdministrator privileges are required to install it.\n\nClick OK and Corsair Elite Display itself will request administrator permission to install the driver."
            );
            let title = wide("Virtual Screen Driver Required");
            let response = unsafe {
                MessageBoxW(
                    hwnd,
                    msg.as_ptr(),
                    title.as_ptr(),
                    MB_OKCANCEL | MB_ICONWARNING,
                )
            };
            if response == IDOK {
                match crate::driver_admin::install_driver_elevated(hwnd) {
                    Ok(()) => {
                        let success_msg = wide("The Virtual Screen driver was successfully installed and configured!");
                        unsafe {
                            MessageBoxW(hwnd, success_msg.as_ptr(), title.as_ptr(), MB_OK | MB_ICONINFORMATION);
                        }
                    }
                    Err(e) => {
                        let err_msg = wide(&format!("Virtual Screen driver installation failed:\n\n{e}"));
                        unsafe {
                            MessageBoxW(hwnd, err_msg.as_ptr(), wide("Installation Error").as_ptr(), MB_OK | MB_ICONERROR);
                        }
                    }
                }
            }
        }

        if self.settings.streaming {
            if let Err(error) = VirtualDisplayManager::activate() {
                self.settings.streaming = false;
                self.controller.set_running(false);
                self.save();
                let _ = crate::hardware_restore::restore_hardware_mode();
                show_error(hwnd, &error);
            }
        } else {
            self.controller.set_running(false);
            VirtualDisplayManager::deactivate();
            let _ = crate::hardware_restore::restore_hardware_mode();
        }
        self.refresh_monitor();
        self.refresh_tray();
        unsafe { SetTimer(hwnd, TIMER_STATUS, 1_000, None) };
    }

    fn refresh_monitor(&mut self) {
        if let Some(monitor) = get_monitors()
            .into_iter()
            .find(|monitor| monitor.width() == 480 && monitor.height() == 480 && !monitor.primary)
        {
            self.settings.monitor = monitor.device_name;
            self.apply();
        }
    }

    fn apply(&mut self) {
        self.controller.update_settings(self.settings.clone());
        self.controller.set_running(self.settings.streaming);
        self.save();
    }

    fn save(&self) {
        let _ = self.settings.save();
    }

    fn restore_hardware_after_stream_stop(&mut self) {
        // set_running(false) joins the worker, so no captured frame can be
        // submitted after the hardware-mode handoff begins.
        self.controller.set_running(false);
        let _ = crate::hardware_restore::restore_hardware_mode();
    }

    fn toggle(&mut self) {
        if self.settings.streaming {
            self.settings.streaming = false;
            self.save();
            self.restore_hardware_after_stream_stop();
            VirtualDisplayManager::deactivate();
        } else {
            if !VirtualDisplayManager::is_ready() {
                let msg = wide(
                    "The Corsair Virtual Screen driver is not installed or configured.\n\nAdministrator privileges are required to install it.\n\nClick OK and Corsair Elite Display itself will request administrator permission to install the driver."
                );
                let title = wide("Virtual Screen Driver Required");
                let response = unsafe {
                    MessageBoxW(self.hwnd, msg.as_ptr(), title.as_ptr(), MB_OKCANCEL | MB_ICONWARNING)
                };
                if response == IDOK {
                    if let Err(e) = crate::driver_admin::install_driver_elevated(self.hwnd) {
                        let err_msg = wide(&format!("Virtual Screen driver installation failed:\n\n{e}"));
                        unsafe {
                            MessageBoxW(self.hwnd, err_msg.as_ptr(), wide("Installation Error").as_ptr(), MB_OK | MB_ICONERROR);
                        }
                        self.restore_hardware_after_stream_stop();
                        return;
                    }
                    let ok_msg = wide("The Virtual Screen driver was successfully installed and configured!");
                    unsafe {
                        MessageBoxW(self.hwnd, ok_msg.as_ptr(), title.as_ptr(), MB_OK | MB_ICONINFORMATION);
                    }
                } else {
                    self.restore_hardware_after_stream_stop();
                    return;
                }
            }

            if let Err(error) = VirtualDisplayManager::activate() {
                show_error(self.hwnd, &error);
                self.restore_hardware_after_stream_stop();
                return;
            }

            self.settings.streaming = true;
            self.apply();
        }
        self.refresh_tray();
    }

    fn tick(&mut self) {
        self.refresh_tray();
    }

    fn tray_data(&self, tooltip: &str) -> NOTIFYICONDATAW {
        let mut icon: NOTIFYICONDATAW = unsafe { zeroed() };
        icon.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        icon.hWnd = self.hwnd;
        icon.uID = 1;
        icon.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        icon.uCallbackMessage = WM_TRAY;
        icon.hIcon = if self.settings.streaming {
            self.icon_on
        } else {
            self.icon_off
        };
        copy_wide(&mut icon.szTip, tooltip);
        icon
    }

    fn add_tray_icon(&self) {
        let icon = self.tray_data(APP_TITLE);
        unsafe { Shell_NotifyIconW(NIM_ADD, &icon) };
    }

    fn refresh_tray(&self) {
        let tooltip = if self.settings.streaming {
            let stats = self.controller.stats();
            if stats.fps > 0.0 {
                format!("{APP_TITLE}\nOn · {:.1} FPS", stats.fps)
            } else {
                format!("{APP_TITLE}\nOn")
            }
        } else {
            format!("{APP_TITLE}\nOff")
        };
        let icon = self.tray_data(&tooltip);
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &icon) };
    }

    fn remove_tray_icon(&self) {
        let icon = self.tray_data("");
        unsafe { Shell_NotifyIconW(NIM_DELETE, &icon) };
    }

    fn show_already_running_notification(&self) {
        let mut icon: NOTIFYICONDATAW = unsafe { zeroed() };
        icon.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        icon.hWnd = self.hwnd;
        icon.uID = 1;
        icon.uFlags = NIF_INFO;
        copy_wide(&mut icon.szInfo, "Corsair Elite Display is already active in your system tray.");
        copy_wide(&mut icon.szInfoTitle, APP_TITLE);
        icon.dwInfoFlags = NIIF_INFO;
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &icon) };
    }

    fn set_fps(&mut self, index: usize) {
        if let Some(value) = FPS_VALUES.get(index) {
            self.settings.fps = *value;
            self.apply();
        }
    }

    fn set_quality(&mut self, index: usize) {
        if let Some(value) = QUALITY_VALUES.get(index) {
            self.settings.quality = *value;
            self.apply();
        }
    }

    fn set_brightness(&mut self, index: usize) {
        if let Some(value) = BRIGHTNESS_VALUES.get(index) {
            self.settings.brightness = *value;
            self.apply();
        }
    }

    fn set_rotation(&mut self, index: usize) {
        if let Some(value) = ROTATION_VALUES.get(index) {
            self.settings.rotation = *value;
            self.apply();
        }
    }

    fn set_view_mode(&mut self, index: usize) {
        if let Some(value) = ViewMode::ALL.get(index) {
            self.settings.view_mode = *value;
            self.apply();
        }
    }

    fn tray_menu(&mut self) {
        unsafe {
            let menu = CreatePopupMenu();
            let fps = CreatePopupMenu();
            let quality = CreatePopupMenu();
            let brightness = CreatePopupMenu();
            let rotation = CreatePopupMenu();
            let view = CreatePopupMenu();
            if menu.is_null()
                || fps.is_null()
                || quality.is_null()
                || brightness.is_null()
                || rotation.is_null()
                || view.is_null()
            {
                return;
            }

            append_checked(menu, MENU_TOGGLE, "On/Off", self.settings.streaming);
            append_enabled_menu(
                menu,
                MENU_SET_HARDWARE_IMAGE,
                "Set hardware image/GIF...",
                !self.settings.streaming,
            );
            append_menu(menu, MENU_UNINSTALL_DRIVER, "Uninstall virtual display driver...");
            append_checked(
                menu,
                MENU_SHOW_MOUSE,
                "Show mouse",
                self.settings.show_mouse,
            );
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            for (index, value) in ViewMode::ALL.iter().enumerate() {
                append_checked(
                    view,
                    MENU_VIEW_BASE + index,
                    value.label(),
                    self.settings.view_mode == *value,
                );
            }
            for (index, value) in FPS_VALUES.iter().enumerate() {
                let label = format!("{value} FPS");
                append_checked(
                    fps,
                    MENU_FPS_BASE + index,
                    &label,
                    self.settings.fps == *value,
                );
            }
            for (index, value) in QUALITY_VALUES.iter().enumerate() {
                append_checked(
                    quality,
                    MENU_QUALITY_BASE + index,
                    &format!("{value}%"),
                    self.settings.quality == *value,
                );
            }
            for (index, (&value, &label)) in BRIGHTNESS_VALUES
                .iter()
                .zip(BRIGHTNESS_LABELS.iter())
                .enumerate()
            {
                append_checked(
                    brightness,
                    MENU_BRIGHTNESS_BASE + index,
                    &format!("{label}%"),
                    self.settings.brightness == value,
                );
            }
            for (index, value) in ROTATION_VALUES.iter().enumerate() {
                append_checked(
                    rotation,
                    MENU_ROTATION_BASE + index,
                    &format!("{value}°"),
                    self.settings.rotation == *value,
                );
            }
            append_submenu(menu, fps, "Frame rate");
            append_submenu(menu, quality, "Quality");
            append_submenu(menu, brightness, "Brightness");
            append_submenu(menu, rotation, "Rotation");
            append_submenu(menu, view, "View");
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            append_checked(menu, MENU_STARTUP, "Start with Windows", startup_enabled());
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            append_menu(menu, MENU_EXIT, "Exit");

            let mut point = POINT::default();
            GetCursorPos(&mut point);
            SetForegroundWindow(self.hwnd);
            let command = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD,
                point.x,
                point.y,
                0,
                self.hwnd,
                null(),
            ) as usize;
            DestroyMenu(menu);
            PostMessageW(self.hwnd, WM_NULL, 0, 0);

            match command {
                MENU_TOGGLE => self.toggle(),
                MENU_SET_HARDWARE_IMAGE => self.open_hardware_image_dialog(),
                MENU_UNINSTALL_DRIVER => self.uninstall_driver(),
                MENU_SHOW_MOUSE => {
                    self.settings.show_mouse = !self.settings.show_mouse;
                    self.apply();
                }
                MENU_STARTUP => {
                    if let Err(error) = set_startup(!startup_enabled()) {
                        show_error(self.hwnd, &error);
                    }
                }
                MENU_EXIT => {
                    DestroyWindow(self.hwnd);
                }
                id if (MENU_FPS_BASE..MENU_FPS_BASE + FPS_VALUES.len()).contains(&id) => {
                    self.set_fps(id - MENU_FPS_BASE)
                }
                id if (MENU_QUALITY_BASE..MENU_QUALITY_BASE + QUALITY_VALUES.len())
                    .contains(&id) =>
                {
                    self.set_quality(id - MENU_QUALITY_BASE)
                }
                id if (MENU_BRIGHTNESS_BASE..MENU_BRIGHTNESS_BASE + BRIGHTNESS_VALUES.len())
                    .contains(&id) =>
                {
                    self.set_brightness(id - MENU_BRIGHTNESS_BASE)
                }
                id if (MENU_ROTATION_BASE..MENU_ROTATION_BASE + ROTATION_VALUES.len())
                    .contains(&id) =>
                {
                    self.set_rotation(id - MENU_ROTATION_BASE)
                }
                id if (MENU_VIEW_BASE..MENU_VIEW_BASE + ViewMode::ALL.len()).contains(&id) => {
                    self.set_view_mode(id - MENU_VIEW_BASE)
                }
                _ => {}
            }
        }
    }

    fn open_hardware_image_dialog(&mut self) {
        if self.settings.streaming {
            let msg = wide("Turn the virtual screen Off before changing the hardware image/GIF.");
            let caption = wide("Corsair Elite Display");
            unsafe {
                MessageBoxW(
                    self.hwnd,
                    msg.as_ptr(),
                    caption.as_ptr(),
                    MB_OK | MB_ICONINFORMATION,
                );
            }
            return;
        }

        let mut file_buf = [0u16; 1024];
        let filter = wide("Image & GIF Files (*.png;*.jpg;*.jpeg;*.gif;*.bmp)\0*.png;*.jpg;*.jpeg;*.gif;*.bmp\0All Files (*.*)\0*.*\0\0");
        let title = wide("Select Hardware Image or GIF");
        let mut ofn: windows_sys::Win32::UI::Controls::Dialogs::OPENFILENAMEW = unsafe { zeroed() };
        ofn.lStructSize = size_of::<windows_sys::Win32::UI::Controls::Dialogs::OPENFILENAMEW>() as u32;
        ofn.hwndOwner = self.hwnd;
        ofn.lpstrFilter = filter.as_ptr();
        ofn.lpstrFile = file_buf.as_mut_ptr();
        ofn.nMaxFile = file_buf.len() as u32;
        ofn.lpstrTitle = title.as_ptr();
        ofn.Flags = windows_sys::Win32::UI::Controls::Dialogs::OFN_FILEMUSTEXIST
            | windows_sys::Win32::UI::Controls::Dialogs::OFN_PATHMUSTEXIST;

        let ok = unsafe { windows_sys::Win32::UI::Controls::Dialogs::GetOpenFileNameW(&mut ofn) };
        if ok == 0 {
            return;
        }

        let len = file_buf.iter().position(|&c| c == 0).unwrap_or(file_buf.len());
        let path_str = String::from_utf16_lossy(&file_buf[..len]);
        let path = std::path::Path::new(&path_str);

        let flash_res = crate::corsair::flash_hardware_image(path);

        match flash_res {
            Ok(()) => {
                let msg = wide("Hardware image/GIF has been successfully applied to your cooler screen!");
                let caption = wide("Corsair Elite Display");
                unsafe {
                    MessageBoxW(self.hwnd, msg.as_ptr(), caption.as_ptr(), MB_OK | MB_ICONINFORMATION);
                }
            }
            Err(err) => {
                let msg = wide(&format!("Failed to apply hardware image:\n\n{err}"));
                let caption = wide("Corsair Elite Display - Error");
                unsafe {
                    MessageBoxW(self.hwnd, msg.as_ptr(), caption.as_ptr(), MB_OK | MB_ICONERROR);
                }
            }
        }
    }

    fn uninstall_driver(&mut self) {
        let msg = wide("Are you sure you want to uninstall the Corsair Virtual Screen driver and remove its configuration?\n\nCorsair Elite Display will close after the driver is removed.");
        let title = wide("Uninstall Virtual Display Driver");
        let confirm = unsafe {
            MessageBoxW(
                self.hwnd,
                msg.as_ptr(),
                title.as_ptr(),
                MB_YESNO | MB_ICONQUESTION,
            )
        };
        if confirm != IDYES {
            return;
        }

        if self.settings.streaming {
            self.settings.streaming = false;
            self.save();
        }
        self.controller.set_running(false);
        let _ = crate::hardware_restore::restore_hardware_mode();
        VirtualDisplayManager::deactivate();

        match crate::driver_admin::uninstall_driver_elevated(self.hwnd) {
            Ok(()) => {
                let success_msg = wide("The Corsair Virtual Screen driver has been successfully uninstalled.\n\nCorsair Elite Display will now close.");
                unsafe {
                    MessageBoxW(
                        self.hwnd,
                        success_msg.as_ptr(),
                        title.as_ptr(),
                        MB_OK | MB_ICONINFORMATION,
                    );
                    DestroyWindow(self.hwnd);
                }
                return;
            }
            Err(e) => {
                let err_msg = wide(&format!("Failed to uninstall driver:\n\n{e}"));
                unsafe {
                    MessageBoxW(
                        self.hwnd,
                        err_msg.as_ptr(),
                        wide("Uninstall Error").as_ptr(),
                        MB_OK | MB_ICONERROR,
                    );
                }
            }
        }
        self.refresh_tray();
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Media::timeEndPeriod(1) };
        self.controller.set_running(false);
        let _ = crate::hardware_restore::restore_hardware_mode();
        VirtualDisplayManager::deactivate();
    }
}

unsafe fn append_menu(menu: HMENU, id: usize, label: &str) {
    let label = wide(label);
    unsafe { AppendMenuW(menu, MF_STRING, id, label.as_ptr()) };
}

unsafe fn append_enabled_menu(menu: HMENU, id: usize, label: &str, enabled: bool) {
    let label = wide(label);
    let flags = MF_STRING | if enabled { 0 } else { MF_GRAYED };
    unsafe { AppendMenuW(menu, flags, id, label.as_ptr()) };
}

unsafe fn append_checked(menu: HMENU, id: usize, label: &str, checked: bool) {
    let label = wide(label);
    let flags = MF_STRING | if checked { MF_CHECKED } else { 0 };
    unsafe { AppendMenuW(menu, flags, id, label.as_ptr()) };
}

unsafe fn append_submenu(menu: HMENU, submenu: HMENU, label: &str) {
    let label = wide(label);
    unsafe { AppendMenuW(menu, MF_POPUP, submenu as usize, label.as_ptr()) };
}

unsafe fn state_from_window(hwnd: HWND) -> Option<&'static mut AppState> {
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut AppState;
    unsafe { pointer.as_mut() }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = unsafe { &*(lparam as *const CREATESTRUCTW) };
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize) };
    }
    if let Some(state) = unsafe { state_from_window(hwnd) } {
        if message == state.taskbar_created && message != 0 {
            state.add_tray_icon();
            return 0;
        }
        match message {
            WM_CREATE => {
                unsafe { state.initialize(hwnd) };
                return 0;
            }
            WM_TIMER if wparam == TIMER_STATUS => {
                state.tick();
                return 0;
            }
            WM_DISPLAYCHANGE => {
                state.refresh_monitor();
                return 0;
            }
            WM_TRAY => {
                match lparam as u32 {
                    WM_LBUTTONUP => {
                        state.toggle();
                    }
                    WM_RBUTTONUP | WM_CONTEXTMENU => {
                        state.tray_menu();
                    }
                    _ => {}
                }
                return 0;
            }
            WM_SHOW_APP => {
                state.show_already_running_notification();
                state.tray_menu();
                return 0;
            }
            WM_QUERYENDSESSION => return 1,
            WM_ENDSESSION if wparam != 0 => {
                unsafe { DestroyWindow(hwnd) };
                return 0;
            }
            WM_DESTROY => {
                state.remove_tray_icon();
                unsafe {
                    KillTimer(hwnd, TIMER_STATUS);
                    PostQuitMessage(0);
                }
                return 0;
            }
            _ => {}
        }
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn show_error(hwnd: HWND, error: &str) {
    let message = wide(error);
    let title = wide(APP_TITLE);
    unsafe { MessageBoxW(hwnd, message.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

pub fn run(_background: bool) {
    unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide(WINDOW_CLASS);
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hIcon: LoadIconW(instance, resource_id(1)),
            lpszClassName: class_name.as_ptr(),
            ..zeroed()
        };
        if RegisterClassW(&class) == 0 {
            return;
        }

        let state = Box::into_raw(Box::new(AppState::new()));
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class_name.as_ptr(),
            wide(APP_TITLE).as_ptr(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            state.cast::<c_void>(),
        );
        if hwnd.is_null() {
            drop(Box::from_raw(state));
            return;
        }

        let mut message: MSG = zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        drop(Box::from_raw(state));
    }
}
