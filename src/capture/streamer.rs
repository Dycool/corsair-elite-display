use std::ffi::{OsStr, c_void};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use image::ExtendedColorType;
use windows_sys::Win32::Foundation::{GetLastError, LPARAM, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLACKNESS, COLORONCOLOR, CreateCompatibleDC, CreateDCW,
    CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors, GetDC,
    GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFOEXW, PatBlt, ReleaseDC, SRCCOPY,
    SelectObject, SetStretchBltMode, StretchBlt,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSORINFO, DI_NORMAL, DrawIconEx, GetCursorInfo, GetIconInfo, ICONINFO,
    MONITORINFOF_PRIMARY,
};

use crate::corsair::CorsairLcdDevice;
use crate::settings::{Settings, ViewMode};

const LCD_SIZE: i32 = 480;

#[derive(Clone)]
pub struct MonitorInfo {
    pub device_name: String,
    pub label: String,
    pub rect: RECT,
    pub primary: bool,
}

impl MonitorInfo {
    pub fn width(&self) -> i32 {
        self.rect.right - self.rect.left
    }
    pub fn height(&self) -> i32 {
        self.rect.bottom - self.rect.top
    }
}

unsafe extern "system" fn enum_monitor(
    monitor: HMONITOR,
    _dc: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> i32 {
    let list = unsafe { &mut *(data as *mut Vec<MonitorInfo>) };
    let mut info: MONITORINFOEXW = unsafe { zeroed() };
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(monitor, (&mut info as *mut MONITORINFOEXW).cast()) } != 0 {
        let end = info
            .szDevice
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(info.szDevice.len());
        let name = String::from_utf16_lossy(&info.szDevice[..end]);
        let rect = info.monitorInfo.rcMonitor;
        let primary = info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0;
        let suffix = if primary { " (Primary)" } else { "" };
        list.push(MonitorInfo {
            device_name: name.clone(),
            label: format!(
                "{} - {}x{}{}",
                name,
                rect.right - rect.left,
                rect.bottom - rect.top,
                suffix
            ),
            rect,
            primary,
        });
    }
    1
}

pub fn get_monitors() -> Vec<MonitorInfo> {
    let mut result: Vec<MonitorInfo> = Vec::new();
    unsafe {
        EnumDisplayMonitors(
            null_mut(),
            null(),
            Some(enum_monitor),
            &mut result as *mut _ as LPARAM,
        );
    }
    result.sort_by_key(|m| {
        (
            !((m.width() == LCD_SIZE) && (m.height() == LCD_SIZE)),
            !m.primary,
        )
    });
    result
}

pub fn self_test() -> Result<(), String> {
    let monitors = get_monitors();
    let monitor = monitors
        .first()
        .ok_or_else(|| "No active display was found".to_string())?;
    let mut capture = GdiCapture::new()?;
    let image = capture.capture(monitor, 100, 0, ViewMode::Native, true)?;
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 75)
        .encode(
            image,
            LCD_SIZE as u32,
            LCD_SIZE as u32,
            ExtendedColorType::Rgb8,
        )
        .map_err(|error| error.to_string())?;
    if jpeg.starts_with(&[0xff, 0xd8]) && jpeg.ends_with(&[0xff, 0xd9]) {
        Ok(())
    } else {
        Err("The capture pipeline did not produce a valid JPEG".to_string())
    }
}

struct GdiCapture {
    desktop_dc: HDC,
    source_dc: HDC,
    source_name: String,
    memory_dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    pixels: *mut u8,
    rgb: Vec<u8>,
    cursor_handle: *mut c_void,
    cursor_hotspot_x: i32,
    cursor_hotspot_y: i32,
}

impl GdiCapture {
    fn new() -> Result<Self, String> {
        unsafe {
            let desktop_dc = GetDC(null_mut());
            if desktop_dc.is_null() {
                return Err("Could not access the Windows desktop".into());
            }
            let memory_dc = CreateCompatibleDC(desktop_dc);
            if memory_dc.is_null() {
                ReleaseDC(null_mut(), desktop_dc);
                return Err("Could not create a capture context".into());
            }
            let mut bmi: BITMAPINFO = zeroed();
            bmi.bmiHeader = BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: LCD_SIZE,
                biHeight: -LCD_SIZE,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..zeroed()
            };
            let mut bits: *mut c_void = null_mut();
            let bitmap =
                CreateDIBSection(desktop_dc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if bitmap.is_null() || bits.is_null() {
                DeleteDC(memory_dc);
                ReleaseDC(null_mut(), desktop_dc);
                return Err("Could not allocate the capture buffer".into());
            }
            let old_bitmap = SelectObject(memory_dc, bitmap);
            SetStretchBltMode(memory_dc, COLORONCOLOR);
            Ok(Self {
                desktop_dc,
                source_dc: null_mut(),
                source_name: String::new(),
                memory_dc,
                bitmap,
                old_bitmap,
                pixels: bits.cast(),
                rgb: vec![0; (LCD_SIZE * LCD_SIZE * 3) as usize],
                cursor_handle: null_mut(),
                cursor_hotspot_x: 0,
                cursor_hotspot_y: 0,
            })
        }
    }

    fn capture(
        &mut self,
        monitor: &MonitorInfo,
        brightness: u8,
        rotation: u16,
        view_mode: ViewMode,
        show_mouse: bool,
    ) -> Result<&[u8], String> {
        let source_w = monitor.width();
        let source_h = monitor.height();
        if source_w <= 0 || source_h <= 0 {
            return Err("The selected display has no active surface".into());
        }

        if self.source_name != monitor.device_name {
            unsafe {
                if !self.source_dc.is_null() {
                    DeleteDC(self.source_dc);
                }
                let driver: Vec<u16> = OsStr::new("DISPLAY").encode_wide().chain(Some(0)).collect();
                let device: Vec<u16> = OsStr::new(&monitor.device_name)
                    .encode_wide()
                    .chain(Some(0))
                    .collect();
                self.source_dc = CreateDCW(driver.as_ptr(), device.as_ptr(), null(), null());
            }
            if self.source_dc.is_null() {
                self.source_name.clear();
                return Err(format!(
                    "Could not access {} (error {})",
                    monitor.device_name,
                    unsafe { GetLastError() }
                ));
            }
            self.source_name.clone_from(&monitor.device_name);
        }

        let scale = (LCD_SIZE as f64 / source_w as f64).min(LCD_SIZE as f64 / source_h as f64)
            * view_mode.zoom();
        let dest_w = (source_w as f64 * scale).round() as i32;
        let dest_h = (source_h as f64 * scale).round() as i32;
        let dest_x = (LCD_SIZE - dest_w) / 2;
        let dest_y = (LCD_SIZE - dest_h) / 2;
        unsafe {
            PatBlt(self.memory_dc, 0, 0, LCD_SIZE, LCD_SIZE, BLACKNESS);
            if StretchBlt(
                self.memory_dc,
                dest_x,
                dest_y,
                dest_w,
                dest_h,
                self.source_dc,
                0,
                0,
                source_w,
                source_h,
                SRCCOPY,
            ) == 0
            {
                return Err(format!(
                    "Windows screen capture failed (error {})",
                    GetLastError()
                ));
            }
            if show_mouse {
                self.draw_cursor(
                    monitor,
                    (source_w, source_h),
                    (dest_x, dest_y, dest_w, dest_h),
                );
            }
        }

        let source =
            unsafe { std::slice::from_raw_parts(self.pixels, (LCD_SIZE * LCD_SIZE * 4) as usize) };
        let brightness = brightness as u16;
        let size = LCD_SIZE as usize;
        for y in 0..size {
            for x in 0..size {
                let (sx, sy) = match rotation {
                    90 => (y, size - 1 - x),
                    180 => (size - 1 - x, size - 1 - y),
                    270 => (size - 1 - y, x),
                    _ => (x, y),
                };
                let source_index = (sy * size + sx) * 4;
                let target_index = (y * size + x) * 3;
                self.rgb[target_index] =
                    ((source[source_index + 2] as u16 * brightness) / 100) as u8;
                self.rgb[target_index + 1] =
                    ((source[source_index + 1] as u16 * brightness) / 100) as u8;
                self.rgb[target_index + 2] =
                    ((source[source_index] as u16 * brightness) / 100) as u8;
            }
        }
        Ok(&self.rgb)
    }

    fn draw_cursor(
        &mut self,
        monitor: &MonitorInfo,
        (source_w, source_h): (i32, i32),
        (dest_x, dest_y, dest_w, dest_h): (i32, i32, i32, i32),
    ) {
        let mut cursor: CURSORINFO = unsafe { zeroed() };
        cursor.cbSize = size_of::<CURSORINFO>() as u32;
        if unsafe { GetCursorInfo(&mut cursor) } == 0
            || cursor.flags & CURSOR_SHOWING == 0
            || cursor.hCursor.is_null()
        {
            return;
        }

        let local_x = cursor.ptScreenPos.x - monitor.rect.left;
        let local_y = cursor.ptScreenPos.y - monitor.rect.top;
        if local_x < 0 || local_y < 0 || local_x >= source_w || local_y >= source_h {
            return;
        }

        if self.cursor_handle != cursor.hCursor {
            let mut icon: ICONINFO = unsafe { zeroed() };
            if unsafe { GetIconInfo(cursor.hCursor, &mut icon) } != 0 {
                self.cursor_hotspot_x = icon.xHotspot as i32;
                self.cursor_hotspot_y = icon.yHotspot as i32;
                if !icon.hbmMask.is_null() {
                    unsafe { DeleteObject(icon.hbmMask) };
                }
                if !icon.hbmColor.is_null() {
                    unsafe { DeleteObject(icon.hbmColor) };
                }
            }
            self.cursor_handle = cursor.hCursor;
        }

        let x = dest_x + local_x * dest_w / source_w - self.cursor_hotspot_x;
        let y = dest_y + local_y * dest_h / source_h - self.cursor_hotspot_y;
        unsafe {
            DrawIconEx(
                self.memory_dc,
                x,
                y,
                cursor.hCursor,
                0,
                0,
                0,
                null_mut(),
                DI_NORMAL,
            );
        }
    }
}

impl Drop for GdiCapture {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.memory_dc, self.old_bitmap);
            DeleteObject(self.bitmap);
            DeleteDC(self.memory_dc);
            if !self.source_dc.is_null() {
                DeleteDC(self.source_dc);
            }
            ReleaseDC(null_mut(), self.desktop_dc);
        }
    }
}

#[derive(Clone, Debug)]
pub struct StreamStats {
    pub fps: f32,
    pub frame_count: u64,
    pub frame_bytes: usize,
    pub latency_ms: f32,
    pub status: String,
}

impl Default for StreamStats {
    fn default() -> Self {
        Self {
            fps: 0.0,
            frame_count: 0,
            frame_bytes: 0,
            latency_ms: 0.0,
            status: "Starting…".into(),
        }
    }
}

pub struct StreamController {
    running: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    settings: Arc<Mutex<Settings>>,
    stats: Arc<Mutex<StreamStats>>,
    worker: Option<JoinHandle<()>>,
}

impl StreamController {
    pub fn new(settings: Settings) -> Self {
        let running = Arc::new(AtomicBool::new(settings.streaming));
        let shutdown = Arc::new(AtomicBool::new(false));
        let shared_settings = Arc::new(Mutex::new(settings));
        let stats = Arc::new(Mutex::new(StreamStats::default()));

        let worker_running = Arc::clone(&running);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_settings = Arc::clone(&shared_settings);
        let worker_stats = Arc::clone(&stats);
        let worker = thread::spawn(move || {
            stream_loop(
                worker_running,
                worker_shutdown,
                worker_settings,
                worker_stats,
            );
        });

        Self {
            running,
            shutdown,
            settings: shared_settings,
            stats,
            worker: Some(worker),
        }
    }

    pub fn set_running(&self, running: bool) {
        let changed = self.running.swap(running, Ordering::AcqRel) != running;
        if changed && let Ok(mut current) = self.stats.lock() {
            current.fps = 0.0;
            current.latency_ms = 0.0;
            current.frame_bytes = 0;
            current.status = if running {
                "Preparing the 480×480 second screen…".into()
            } else {
                "Off · cooler hardware screen active".into()
            };
        }
    }
    pub fn update_settings(&self, settings: Settings) {
        if let Ok(mut current) = self.settings.lock() {
            *current = settings;
        }
    }
    pub fn stats(&self) -> StreamStats {
        self.stats.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

impl Drop for StreamController {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn set_status(stats: &Mutex<StreamStats>, status: impl Into<String>) {
    if let Ok(mut current) = stats.lock() {
        current.status = status.into();
    }
}

fn stream_loop(
    running: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    settings: Arc<Mutex<Settings>>,
    stats: Arc<Mutex<StreamStats>>,
) {
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
    let mut capture = match GdiCapture::new() {
        Ok(capture) => capture,
        Err(error) => {
            set_status(&stats, error);
            return;
        }
    };
    let mut device: Option<CorsairLcdDevice> = None;
    let mut next_connect = Instant::now();
    let mut second_started = Instant::now();
    let mut frames_this_second = 0u32;
    let mut total_frames = 0u64;
    let mut monitors = get_monitors();
    let mut monitors_refreshed = Instant::now();
    let mut jpeg = Vec::with_capacity(64 * 1024);
    let mut next_frame = Instant::now();

    while !shutdown.load(Ordering::Acquire) {
        if !running.load(Ordering::Acquire) {
            if let Some(connected) = device.take() {
                connected.release_to_hardware();
            }
            set_status(&stats, "Off · hardware screen restored");
            next_frame = Instant::now();
            thread::sleep(Duration::from_millis(50));
            continue;
        }

        if device.is_none() && Instant::now() >= next_connect {
            match CorsairLcdDevice::open() {
                Ok(found) => device = Some(found),
                Err(error) => {
                    set_status(&stats, error);
                    next_connect = Instant::now() + Duration::from_secs(2);
                }
            }
        }
        if device.is_none() {
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        let frame_started = Instant::now();
        let config = settings.lock().map(|s| s.clone()).unwrap_or_default();
        if monitors_refreshed.elapsed() >= Duration::from_secs(1) {
            monitors = get_monitors();
            monitors_refreshed = Instant::now();
        }
        let selected = monitors
            .iter()
            .find(|monitor| {
                monitor.device_name == config.monitor
                    && monitor.width() == LCD_SIZE
                    && monitor.height() == LCD_SIZE
                    && !monitor.primary
            })
            .or_else(|| {
                monitors.iter().find(|monitor| {
                    monitor.width() == LCD_SIZE && monitor.height() == LCD_SIZE && !monitor.primary
                })
            })
            .or_else(|| {
                monitors
                    .iter()
                    .find(|monitor| monitor.width() == LCD_SIZE && monitor.height() == LCD_SIZE)
            });
        let Some(monitor) = selected else {
            set_status(&stats, "Preparing the 480×480 second screen…");
            thread::sleep(Duration::from_millis(250));
            continue;
        };

        jpeg.clear();
        let result = capture
            .capture(
                monitor,
                config.brightness,
                config.rotation,
                config.view_mode,
                config.show_mouse,
            )
            .and_then(|pixels| {
                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, config.quality)
                    .encode(
                        pixels,
                        LCD_SIZE as u32,
                        LCD_SIZE as u32,
                        ExtendedColorType::Rgb8,
                    )
                    .map_err(|e| format!("JPEG encoding failed: {e}"))?;
                let bytes = jpeg.len();
                device.as_ref().unwrap().send_frame(&jpeg)?;
                Ok(bytes)
            });

        match result {
            Ok(frame_bytes) => {
                total_frames += 1;
                frames_this_second += 1;
                if second_started.elapsed() >= Duration::from_secs(1) {
                    let measured_fps =
                        frames_this_second as f32 / second_started.elapsed().as_secs_f32();
                    if let Ok(mut current) = stats.lock() {
                        current.fps = measured_fps;
                        current.frame_count = total_frames;
                        current.frame_bytes = frame_bytes;
                        current.latency_ms = frame_started.elapsed().as_secs_f32() * 1000.0;
                        current.status =
                            format!("Streaming {} at {:.1} FPS", monitor.label, measured_fps);
                    }
                    frames_this_second = 0;
                    second_started = Instant::now();
                }
            }
            Err(error) => {
                set_status(&stats, error);
                device = None;
                next_connect = Instant::now() + Duration::from_secs(1);
            }
        }

        let target = Duration::from_secs_f64(1.0 / config.fps.max(1) as f64);
        next_frame += target;
        let now = Instant::now();
        if next_frame > now {
            thread::sleep(next_frame - now);
        } else {
            next_frame = now;
        }
    }

    if let Some(connected) = device.take() {
        connected.release_to_hardware();
    }
}
