use std::ffi::{OsStr, c_void};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET,
    DEFAULT_PITCH, DeleteObject, Ellipse, FF_DONTCARE, FW_NORMAL, FW_SEMIBOLD, FillRect,
    GetStockObject, HBRUSH, HFONT, InvalidateRect, NULL_PEN, OUT_DEFAULT_PRECIS, SelectObject,
    UpdateWindow, WHITE_BRUSH,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::{BST_CHECKED, BST_UNCHECKED, DRAWITEMSTRUCT, ODS_SELECTED};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::capture::{MonitorInfo, StreamController, get_monitors};
use crate::settings::{Settings, set_startup, startup_enabled};
use crate::virtual_display::VirtualDisplayManager;

pub const WINDOW_CLASS: &str = "CorsairEliteDisplayWindow";
const APP_TITLE: &str = "Corsair Elite Display";
const WM_TRAY: u32 = WM_APP + 1;
const WM_SHOW_APP: u32 = WM_APP + 2;
const TIMER_STATUS: usize = 1;

const ID_MONITOR: i32 = 100;
const ID_FPS: i32 = 101;
const ID_QUALITY: i32 = 102;
const ID_BRIGHTNESS: i32 = 103;
const ID_ROTATION: i32 = 104;
const ID_TOGGLE: i32 = 105;
const ID_STARTUP: i32 = 106;
const ID_INSTALL: i32 = 107;
const ID_STATUS: i32 = 108;

const MENU_OPEN: usize = 201;
const MENU_TOGGLE: usize = 202;
const MENU_STARTUP: usize = 203;
const MENU_EXIT: usize = 204;

const ACCENT: u32 = 0x00d9a91f;
const ACCENT_DARK: u32 = 0x00a97d11;
const SWITCH_OFF: u32 = 0x00b8b2ac;
const WHITE: u32 = 0x00ffffff;

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn resource_id(id: usize) -> *const u16 {
    std::ptr::with_exposed_provenance(id)
}

fn copy_wide<const N: usize>(target: &mut [u16; N], value: &str) {
    let value = wide(value);
    let len = value.len().min(N);
    target[..len].copy_from_slice(&value[..len]);
}

struct AppState {
    hwnd: HWND,
    controls: [HWND; 9],
    monitors: Vec<MonitorInfo>,
    settings: Settings,
    controller: StreamController,
    taskbar_created: u32,
    icon_on: HICON,
    icon_off: HICON,
    font_body: HFONT,
    font_title: HFONT,
}

impl AppState {
    fn new() -> Self {
        let settings = Settings::load();
        Self {
            hwnd: null_mut(),
            controls: [null_mut(); 9],
            monitors: Vec::new(),
            controller: StreamController::new(settings.clone()),
            settings,
            taskbar_created: 0,
            icon_on: null_mut(),
            icon_off: null_mut(),
            font_body: null_mut(),
            font_title: null_mut(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn create_control(
        &mut self,
        class: &str,
        text: &str,
        style: i32,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        id: i32,
    ) -> HWND {
        let class = wide(class);
        let text = wide(text);
        let control = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                text.as_ptr(),
                WS_CHILD | WS_VISIBLE | style as u32,
                x,
                y,
                width,
                height,
                self.hwnd,
                id as usize as HMENU,
                null_mut(),
                null(),
            )
        };
        if !control.is_null() {
            unsafe { SendMessageW(control, WM_SETFONT, self.font_body as WPARAM, 1) };
        }
        control
    }

    unsafe fn init_window(&mut self, hwnd: HWND) {
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
        unsafe {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as WPARAM, self.icon_on as LPARAM);
            SendMessageW(
                hwnd,
                WM_SETICON,
                ICON_SMALL as WPARAM,
                self.icon_on as LPARAM,
            );
        }

        self.font_body = unsafe { create_font(16, FW_NORMAL as i32) };
        self.font_title = unsafe { create_font(26, FW_SEMIBOLD as i32) };
        let taskbar = wide("TaskbarCreated");
        self.taskbar_created = unsafe { RegisterWindowMessageW(taskbar.as_ptr()) };

        unsafe {
            // SS_ICON (3) keeps the icon control dependency-free on Win32.
            let icon_control = self.create_control("STATIC", "", 3, 24, 20, 48, 48, 0);
            SendMessageW(icon_control, STM_SETICON, self.icon_on as WPARAM, 0);
            let title = self.create_control("STATIC", APP_TITLE, 0, 82, 20, 450, 32, 0);
            SendMessageW(title, WM_SETFONT, self.font_title as WPARAM, 1);
            self.create_control(
                "STATIC",
                "A real second screen, fitted into your cooler.",
                0,
                84,
                55,
                450,
                22,
                0,
            );
            self.create_control("STATIC", "Stream to cooler", 0, 24, 100, 180, 24, 0);
            self.controls[5] = self.create_control(
                "BUTTON",
                "",
                BS_OWNERDRAW | WS_TABSTOP as i32,
                500,
                94,
                62,
                32,
                ID_TOGGLE,
            );
            self.controls[8] =
                self.create_control("STATIC", "Starting…", 0, 24, 130, 538, 24, ID_STATUS);

            self.create_control("STATIC", "Display", 0, 24, 174, 110, 22, 0);
            self.controls[0] = self.create_control(
                "COMBOBOX",
                "",
                CBS_DROPDOWNLIST | WS_TABSTOP as i32,
                150,
                169,
                412,
                220,
                ID_MONITOR,
            );
            self.create_control("STATIC", "Frame rate", 0, 24, 222, 110, 22, 0);
            self.controls[1] = self.create_control(
                "COMBOBOX",
                "",
                CBS_DROPDOWNLIST | WS_TABSTOP as i32,
                150,
                217,
                120,
                180,
                ID_FPS,
            );
            self.create_control("STATIC", "Quality", 0, 306, 222, 86, 22, 0);
            self.controls[2] = self.create_control(
                "COMBOBOX",
                "",
                CBS_DROPDOWNLIST | WS_TABSTOP as i32,
                402,
                217,
                160,
                180,
                ID_QUALITY,
            );
            self.create_control("STATIC", "Brightness", 0, 24, 270, 110, 22, 0);
            self.controls[3] = self.create_control(
                "COMBOBOX",
                "",
                CBS_DROPDOWNLIST | WS_TABSTOP as i32,
                150,
                265,
                120,
                180,
                ID_BRIGHTNESS,
            );
            self.create_control("STATIC", "Rotation", 0, 306, 270, 86, 22, 0);
            self.controls[4] = self.create_control(
                "COMBOBOX",
                "",
                CBS_DROPDOWNLIST | WS_TABSTOP as i32,
                402,
                265,
                160,
                180,
                ID_ROTATION,
            );
            self.controls[7] = self.create_control(
                "BUTTON",
                "Start automatically with Windows",
                BS_AUTOCHECKBOX | WS_TABSTOP as i32,
                24,
                322,
                280,
                28,
                ID_STARTUP,
            );
            self.controls[6] = self.create_control(
                "BUTTON",
                "Install virtual display…",
                BS_PUSHBUTTON | WS_TABSTOP as i32,
                354,
                316,
                208,
                36,
                ID_INSTALL,
            );
        }

        for text in ["10 FPS", "15 FPS", "20 FPS", "30 FPS"] {
            unsafe { combo_add(self.controls[1], text) };
        }
        for text in ["55% · Fastest", "65%", "75% · Balanced", "85% · Sharpest"] {
            unsafe { combo_add(self.controls[2], text) };
        }
        for text in ["25%", "50%", "75%", "100%"] {
            unsafe { combo_add(self.controls[3], text) };
        }
        for text in ["0°", "90°", "180°", "270°"] {
            unsafe { combo_add(self.controls[4], text) };
        }
        unsafe {
            combo_select_value(
                self.controls[1],
                &[10, 15, 20, 30],
                self.settings.fps as i32,
            );
            combo_select_value(
                self.controls[2],
                &[55, 65, 75, 85],
                self.settings.quality as i32,
            );
            combo_select_value(
                self.controls[3],
                &[25, 50, 75, 100],
                self.settings.brightness as i32,
            );
            combo_select_value(
                self.controls[4],
                &[0, 90, 180, 270],
                self.settings.rotation as i32,
            );
            SendMessageW(
                self.controls[7],
                BM_SETCHECK,
                if startup_enabled() {
                    BST_CHECKED as usize
                } else {
                    BST_UNCHECKED as usize
                },
                0,
            );
        }
        self.refresh_monitors();
        self.refresh_status();
        self.add_tray_icon();
        unsafe { SetTimer(hwnd, TIMER_STATUS, 500, None) };
    }

    fn refresh_monitors(&mut self) {
        self.monitors = get_monitors();
        unsafe { SendMessageW(self.controls[0], CB_RESETCONTENT, 0, 0) };
        for monitor in &self.monitors {
            unsafe { combo_add(self.controls[0], &monitor.label) };
        }
        let selected = self
            .monitors
            .iter()
            .position(|monitor| monitor.device_name == self.settings.monitor)
            .or_else(|| {
                self.monitors
                    .iter()
                    .position(|monitor| monitor.width() == 480 && monitor.height() == 480)
            })
            .or_else(|| self.monitors.iter().position(|monitor| !monitor.primary))
            .unwrap_or(0);
        if let Some(monitor) = self.monitors.get(selected) {
            self.settings.monitor = monitor.device_name.clone();
            self.apply_settings();
        }
        unsafe { SendMessageW(self.controls[0], CB_SETCURSEL, selected, 0) };
    }

    fn apply_settings(&mut self) {
        self.controller.update_settings(self.settings.clone());
        self.controller.set_running(self.settings.streaming);
        let _ = self.settings.save();
    }

    fn read_controls(&mut self) {
        let monitor = unsafe { SendMessageW(self.controls[0], CB_GETCURSEL, 0, 0) } as usize;
        if let Some(selected) = self.monitors.get(monitor) {
            self.settings.monitor = selected.device_name.clone();
        }
        self.settings.fps = [10, 15, 20, 30][unsafe { combo_index(self.controls[1]) }.min(3)];
        self.settings.quality = [55, 65, 75, 85][unsafe { combo_index(self.controls[2]) }.min(3)];
        self.settings.brightness =
            [25, 50, 75, 100][unsafe { combo_index(self.controls[3]) }.min(3)];
        self.settings.rotation = [0, 90, 180, 270][unsafe { combo_index(self.controls[4]) }.min(3)];
        self.apply_settings();
    }

    fn refresh_status(&mut self) {
        let stats = self.controller.stats();
        let status = if !self.controller.is_running() {
            "Off · cooler hardware screen active".to_string()
        } else if stats.fps > 0.0 {
            format!("On · {:.1} FPS · {:.1} ms", stats.fps, stats.latency_ms)
        } else {
            stats.status
        };
        unsafe {
            set_text(self.controls[8], &status);
            InvalidateRect(self.controls[5], null(), 0);
        }
        self.update_tray_icon(&status);
    }

    fn toggle_streaming(&mut self) {
        self.settings.streaming = !self.controller.is_running();
        self.apply_settings();
        self.refresh_status();
    }

    fn show_window(&self) {
        unsafe {
            ShowWindow(self.hwnd, SW_RESTORE);
            SetForegroundWindow(self.hwnd);
        }
    }

    fn tray_data(&self, tooltip: &str) -> NOTIFYICONDATAW {
        let mut icon: NOTIFYICONDATAW = unsafe { zeroed() };
        icon.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        icon.hWnd = self.hwnd;
        icon.uID = 1;
        icon.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        icon.uCallbackMessage = WM_TRAY;
        icon.hIcon = if self.controller.is_running() {
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

    fn update_tray_icon(&self, status: &str) {
        let icon = self.tray_data(&format!("{APP_TITLE}\n{status}"));
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &icon) };
    }

    fn remove_tray_icon(&self) {
        let icon = self.tray_data("");
        unsafe { Shell_NotifyIconW(NIM_DELETE, &icon) };
    }

    fn tray_menu(&mut self) {
        unsafe {
            let menu = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            append_menu(menu, MF_STRING, MENU_OPEN, "Open settings");
            append_menu(
                menu,
                MF_STRING,
                MENU_TOGGLE,
                if self.controller.is_running() {
                    "Turn off"
                } else {
                    "Turn on"
                },
            );
            append_menu(
                menu,
                MF_STRING | if startup_enabled() { MF_CHECKED } else { 0 },
                MENU_STARTUP,
                "Start with Windows",
            );
            AppendMenuW(menu, MF_SEPARATOR, 0, null());
            append_menu(menu, MF_STRING, MENU_EXIT, "Exit");
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
            );
            DestroyMenu(menu);
            match command as usize {
                MENU_OPEN => self.show_window(),
                MENU_TOGGLE => self.toggle_streaming(),
                MENU_STARTUP => {
                    let enabled = !startup_enabled();
                    if set_startup(enabled).is_ok() {
                        SendMessageW(
                            self.controls[7],
                            BM_SETCHECK,
                            if enabled {
                                BST_CHECKED as usize
                            } else {
                                BST_UNCHECKED as usize
                            },
                            0,
                        );
                    }
                }
                MENU_EXIT => {
                    DestroyWindow(self.hwnd);
                }
                _ => {}
            }
        }
    }

    unsafe fn draw_toggle(&self, item: &DRAWITEMSTRUCT) {
        let enabled = self.controller.is_running();
        let pressed = item.itemState & ODS_SELECTED != 0;
        let track_color = if enabled {
            if pressed { ACCENT_DARK } else { ACCENT }
        } else {
            SWITCH_OFF
        };
        let brush = unsafe { CreateSolidBrush(track_color) };
        let old_brush = unsafe { SelectObject(item.hDC, brush) };
        let old_pen = unsafe { SelectObject(item.hDC, GetStockObject(NULL_PEN)) };
        let rect = item.rcItem;
        let diameter = rect.bottom - rect.top;
        unsafe {
            Ellipse(
                item.hDC,
                rect.left,
                rect.top,
                rect.left + diameter,
                rect.bottom,
            );
            Ellipse(
                item.hDC,
                rect.right - diameter,
                rect.top,
                rect.right,
                rect.bottom,
            );
            let middle = RECT {
                left: rect.left + diameter / 2,
                top: rect.top,
                right: rect.right - diameter / 2,
                bottom: rect.bottom,
            };
            FillRect(item.hDC, &middle, brush);
        }

        let knob = unsafe { CreateSolidBrush(WHITE) };
        unsafe { SelectObject(item.hDC, knob) };
        let pad = 4;
        let knob_size = diameter - pad * 2;
        let x = if enabled {
            rect.right - pad - knob_size
        } else {
            rect.left + pad
        };
        unsafe {
            Ellipse(
                item.hDC,
                x,
                rect.top + pad,
                x + knob_size,
                rect.bottom - pad,
            );
            SelectObject(item.hDC, old_pen);
            SelectObject(item.hDC, old_brush);
            DeleteObject(knob);
            DeleteObject(brush);
        }
    }
}

impl Drop for AppState {
    fn drop(&mut self) {
        unsafe {
            if !self.font_title.is_null() {
                DeleteObject(self.font_title);
            }
            if !self.font_body.is_null() {
                DeleteObject(self.font_body);
            }
        }
    }
}

unsafe fn create_font(height: i32, weight: i32) -> HFONT {
    let face = wide("Segoe UI");
    unsafe {
        CreateFontW(
            -height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            (DEFAULT_PITCH | FF_DONTCARE) as u32,
            face.as_ptr(),
        )
    }
}

unsafe fn combo_add(combo: HWND, value: &str) {
    let value = wide(value);
    unsafe { SendMessageW(combo, CB_ADDSTRING, 0, value.as_ptr() as LPARAM) };
}

unsafe fn combo_index(combo: HWND) -> usize {
    let index = unsafe { SendMessageW(combo, CB_GETCURSEL, 0, 0) };
    if index < 0 { 0 } else { index as usize }
}

unsafe fn combo_select_value(combo: HWND, values: &[i32], value: i32) {
    let index = values
        .iter()
        .position(|candidate| *candidate == value)
        .unwrap_or(values.len() - 1);
    unsafe { SendMessageW(combo, CB_SETCURSEL, index, 0) };
}

unsafe fn set_text(hwnd: HWND, value: &str) {
    let value = wide(value);
    unsafe { SetWindowTextW(hwnd, value.as_ptr()) };
}

unsafe fn append_menu(menu: HMENU, flags: MENU_ITEM_FLAGS, id: usize, label: &str) {
    let label = wide(label);
    unsafe { AppendMenuW(menu, flags, id, label.as_ptr()) };
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
    let state = unsafe { state_from_window(hwnd) };
    if let Some(state) = state {
        if message == state.taskbar_created && message != 0 {
            state.add_tray_icon();
            return 0;
        }
        match message {
            WM_CREATE => {
                unsafe { state.init_window(hwnd) };
                return 0;
            }
            WM_DRAWITEM => {
                let item = unsafe { &*(lparam as *const DRAWITEMSTRUCT) };
                if item.CtlID == ID_TOGGLE as u32 {
                    unsafe { state.draw_toggle(item) };
                    return 1;
                }
            }
            WM_COMMAND => {
                let id = (wparam & 0xffff) as i32;
                let notification = ((wparam >> 16) & 0xffff) as u32;
                match id {
                    ID_TOGGLE => state.toggle_streaming(),
                    ID_INSTALL => {
                        if let Err(error) = VirtualDisplayManager::request_install() {
                            show_error(hwnd, "Could not install display", &error);
                        }
                    }
                    ID_STARTUP => {
                        let checked = unsafe { SendMessageW(state.controls[7], BM_GETCHECK, 0, 0) }
                            == BST_CHECKED as isize;
                        if let Err(error) = set_startup(checked) {
                            show_error(hwnd, "Startup setting", &error);
                        }
                    }
                    ID_MONITOR | ID_FPS | ID_QUALITY | ID_BRIGHTNESS | ID_ROTATION
                        if notification == CBN_SELCHANGE =>
                    {
                        state.read_controls();
                    }
                    _ => {}
                }
                return 0;
            }
            WM_TIMER => {
                if wparam == TIMER_STATUS {
                    state.refresh_status();
                }
                return 0;
            }
            WM_DISPLAYCHANGE => {
                state.refresh_monitors();
                return 0;
            }
            WM_TRAY => {
                match lparam as u32 {
                    WM_LBUTTONUP | WM_LBUTTONDBLCLK => state.show_window(),
                    WM_RBUTTONUP | WM_CONTEXTMENU => state.tray_menu(),
                    _ => {}
                }
                return 0;
            }
            WM_SHOW_APP => {
                state.show_window();
                return 0;
            }
            WM_CLOSE => {
                unsafe { ShowWindow(hwnd, SW_HIDE) };
                return 0;
            }
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

fn show_error(hwnd: HWND, title: &str, error: &str) {
    let error = wide(error);
    let title = wide(title);
    unsafe { MessageBoxW(hwnd, error.as_ptr(), title.as_ptr(), MB_OK | MB_ICONERROR) };
}

pub fn run(background: bool) {
    unsafe {
        let instance = GetModuleHandleW(null());
        let class_name = wide(WINDOW_CLASS);
        let title = wide(APP_TITLE);
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hIcon: LoadIconW(instance, resource_id(1)),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: GetStockObject(WHITE_BRUSH) as HBRUSH,
            lpszClassName: class_name.as_ptr(),
            ..zeroed()
        };
        if RegisterClassW(&class) == 0 {
            return;
        }

        let state_ptr = Box::into_raw(Box::new(AppState::new()));
        let hwnd = CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            606,
            420,
            null_mut(),
            null_mut(),
            instance,
            state_ptr.cast::<c_void>(),
        );
        if hwnd.is_null() {
            drop(Box::from_raw(state_ptr));
            return;
        }
        if !background {
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
        }

        let mut message: MSG = zeroed();
        while GetMessageW(&mut message, null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        drop(Box::from_raw(state_ptr));
    }
}
