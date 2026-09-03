use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use image::imageops::FilterType;
use image::{ImageBuffer, RgbaImage};
use xcap::Monitor;

use crate::corsair::CorsairLcdDevice;

#[link(name = "user32")]
unsafe extern "system" {
    fn OpenInputDesktop(dwFlags: u32, fInherit: i32, dwDesiredAccess: u32) -> *mut std::ffi::c_void;
    fn SetThreadDesktop(hDesktop: *mut std::ffi::c_void) -> i32;
}

fn attach_input_desktop() {
    unsafe {
        let hdesk = OpenInputDesktop(0, 0, 0x01FF);
        if !hdesk.is_null() {
            SetThreadDesktop(hdesk);
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct MonitorInfo {
    pub id: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
}

pub struct StreamStats {
    pub fps: f32,
    pub frame_count: u64,
    pub last_frame_size_bytes: usize,
    pub last_latency_ms: f32,
    pub status: String,
}

pub struct StreamController {
    is_running: Arc<AtomicBool>,
    worker_handle: Option<JoinHandle<()>>,
    pub stats: Arc<Mutex<StreamStats>>,
    pub preview_image: Arc<Mutex<Option<RgbaImage>>>,
    pub selected_monitor_idx: Arc<AtomicU32>,
    pub brightness_req: Arc<AtomicU32>, // 0..100, 999 = no change
    pub rotation_req: Arc<AtomicU32>,   // 0, 90, 180, 270, 999 = no change
}

impl StreamController {
    pub fn new() -> Self {
        Self {
            is_running: Arc::new(AtomicBool::new(false)),
            worker_handle: None,
            stats: Arc::new(Mutex::new(StreamStats {
                fps: 0.0,
                frame_count: 0,
                last_frame_size_bytes: 0,
                last_latency_ms: 0.0,
                status: "Idle".to_string(),
            })),
            preview_image: Arc::new(Mutex::new(None)),
            selected_monitor_idx: Arc::new(AtomicU32::new(0)),
            brightness_req: Arc::new(AtomicU32::new(999)),
            rotation_req: Arc::new(AtomicU32::new(999)),
        }
    }

    pub fn get_monitors() -> Vec<MonitorInfo> {
        let mut list = Vec::new();
        if let Ok(monitors) = Monitor::all() {
            for (i, m) in monitors.into_iter().enumerate() {
                list.push(MonitorInfo {
                    id: i,
                    name: m.name().unwrap_or_else(|_| format!("Display {}", i + 1)),
                    width: m.width().unwrap_or(0),
                    height: m.height().unwrap_or(0),
                    is_primary: m.is_primary().unwrap_or(false),
                });
            }
        }
        list
    }

    pub fn is_streaming(&self) -> bool {
        self.is_running.load(Ordering::SeqCst)
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.is_streaming() {
            return Ok(());
        }

        self.is_running.store(true, Ordering::SeqCst);

        let is_running = Arc::clone(&self.is_running);
        let stats = Arc::clone(&self.stats);
        let preview_image = Arc::clone(&self.preview_image);
        let selected_idx = Arc::clone(&self.selected_monitor_idx);
        let brightness_req = Arc::clone(&self.brightness_req);
        let rotation_req = Arc::clone(&self.rotation_req);

        let handle = thread::spawn(move || {
            attach_input_desktop();

            // Connect to Corsair LCD device
            let device = match CorsairLcdDevice::open() {
                Ok(dev) => {
                    if let Ok(mut s) = stats.lock() {
                        s.status = "Connected to LCD. Streaming...".to_string();
                    }
                    dev
                }
                Err(err) => {
                    if let Ok(mut s) = stats.lock() {
                        s.status = format!("Device error: {}", err);
                    }
                    is_running.store(false, Ordering::SeqCst);
                    return;
                }
            };

            let target_frame_time = Duration::from_micros(33_333); // 30 FPS
            let mut frames_this_sec = 0u32;
            let mut sec_timer = Instant::now();
            let mut total_frames = 0u64;

            while is_running.load(Ordering::SeqCst) {
                let frame_start = Instant::now();

                // Check brightness / rotation requests
                let b_val = brightness_req.swap(999, Ordering::SeqCst);
                if b_val <= 100 {
                    let _ = device.set_brightness(b_val as u8);
                }

                let r_val = rotation_req.swap(999, Ordering::SeqCst);
                if r_val <= 270 {
                    let _ = device.set_rotation(r_val as u16);
                }

                // Capture selected monitor
                let mon_idx = selected_idx.load(Ordering::Relaxed) as usize;
                let monitors = Monitor::all().unwrap_or_default();
                let monitor = monitors.into_iter().nth(mon_idx);

                if let Some(mon) = monitor {
                    match mon.capture_image() {
                        Ok(captured) => {
                            // Resize to 480x480 if not already 480x480
                            let final_img: RgbaImage = if captured.width() == 480 && captured.height() == 480 {
                                captured
                            } else {
                                image::imageops::resize(&captured, 480, 480, FilterType::Triangle)
                            };

                            // Update GUI preview texture (clone for UI thread)
                            if let Ok(mut prev) = preview_image.try_lock() {
                                *prev = Some(final_img.clone());
                            }

                            // Convert to RGB for JPEG encoding
                            let rgb_img: ImageBuffer<image::Rgb<u8>, Vec<u8>> =
                                ImageBuffer::from_fn(480, 480, |x, y| {
                                    let p = final_img.get_pixel(x, y);
                                    image::Rgb([p[0], p[1], p[2]])
                                });

                            let mut jpeg_bytes = Vec::with_capacity(32 * 1024);
                            let mut encoder =
                                image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_bytes, 80);
                            let _ = encoder.encode_image(&rgb_img);

                            let frame_size = jpeg_bytes.len();
                            let _ = device.send_frame(&jpeg_bytes);

                            total_frames += 1;
                            frames_this_sec += 1;

                            let latency = frame_start.elapsed().as_secs_f32() * 1000.0;

                            if sec_timer.elapsed() >= Duration::from_secs(1) {
                                let fps = frames_this_sec as f32 / sec_timer.elapsed().as_secs_f32();
                                frames_this_sec = 0;
                                sec_timer = Instant::now();

                                if let Ok(mut s) = stats.lock() {
                                    s.fps = fps;
                                    s.frame_count = total_frames;
                                    s.last_frame_size_bytes = frame_size;
                                    s.last_latency_ms = latency;
                                    s.status = "Streaming active (30 FPS)".to_string();
                                }
                            }
                        }
                        Err(e) => {
                            if let Ok(mut s) = stats.lock() {
                                s.status = format!("Capture error: {:?}", e);
                            }
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                } else {
                    if let Ok(mut s) = stats.lock() {
                        s.status = "Selected monitor not found".to_string();
                    }
                    thread::sleep(Duration::from_millis(100));
                }

                let elapsed = frame_start.elapsed();
                if elapsed < target_frame_time {
                    thread::sleep(target_frame_time - elapsed);
                }
            }

            if let Ok(mut s) = stats.lock() {
                s.fps = 0.0;
                s.status = "Stopped".to_string();
            }
        });

        self.worker_handle = Some(handle);
        Ok(())
    }

    pub fn stop(&mut self) {
        self.is_running.store(false, Ordering::SeqCst);
        if let Some(h) = self.worker_handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for StreamController {
    fn drop(&mut self) {
        self.stop();
    }
}
