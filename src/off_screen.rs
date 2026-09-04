use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use image::codecs::gif::GifDecoder;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::{overlay, resize, FilterType};
use image::{AnimationDecoder, DynamicImage, ExtendedColorType, RgbImage};

use crate::corsair::CorsairLcdDevice;
use crate::settings::Settings;

const LCD_SIZE: u32 = 480;
const MAX_MEDIA_BYTES: u64 = 30 * 1024 * 1024;
const MAX_GIF_FRAMES: usize = 900;
const MAX_ENCODED_BYTES: usize = 96 * 1024 * 1024;

struct MediaFrame {
    jpeg: Vec<u8>,
    delay: Duration,
}

struct PreparedMedia {
    frames: Vec<MediaFrame>,
}

pub struct OffScreenController {
    running: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    settings: Arc<Mutex<Settings>>,
    worker: Option<JoinHandle<()>>,
}

impl OffScreenController {
    pub fn new(settings: Settings) -> Self {
        let running = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let shared_settings = Arc::new(Mutex::new(settings));

        let worker_running = Arc::clone(&running);
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_settings = Arc::clone(&shared_settings);
        let worker = thread::spawn(move || {
            off_screen_loop(worker_running, worker_shutdown, worker_settings);
        });

        Self {
            running,
            shutdown,
            settings: shared_settings,
            worker: Some(worker),
        }
    }

    pub fn update_settings(&self, settings: Settings) {
        if let Ok(mut current) = self.settings.lock() {
            *current = settings;
        }
    }

    pub fn set_running(&self, running: bool) {
        self.running.store(running, Ordering::Release);
    }
}

impl Drop for OffScreenController {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        self.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn log_error(error: impl AsRef<str>) {
    let _ = std::fs::write(
        std::env::temp_dir().join("corsair-elite-display-off-screen.txt"),
        error.as_ref(),
    );
}

fn off_screen_loop(
    running: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    settings: Arc<Mutex<Settings>>,
) {
    let mut device: Option<CorsairLcdDevice> = None;
    let mut prepared: Option<PreparedMedia> = None;
    let mut loaded_path = String::new();
    let mut loaded_quality = 0u8;
    let mut frame_index = 0usize;
    let mut next_frame = Instant::now();
    let mut next_connect = Instant::now();
    let mut applied_brightness = u8::MAX;
    let mut applied_rotation = u16::MAX;

    while !shutdown.load(Ordering::Acquire) {
        if !running.load(Ordering::Acquire) {
            if let Some(connected) = device.take() {
                connected.release_to_hardware();
            }
            applied_brightness = u8::MAX;
            applied_rotation = u16::MAX;
            thread::sleep(Duration::from_millis(30));
            continue;
        }

        let config = settings.lock().map(|value| value.clone()).unwrap_or_default();
        let media_path = config.off_screen_media.trim();

        if media_path.is_empty() {
            if let Some(connected) = device.take() {
                connected.release_to_hardware();
            }
            prepared = None;
            loaded_path.clear();
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        if loaded_path != media_path || loaded_quality != config.quality {
            loaded_path = media_path.to_owned();
            loaded_quality = config.quality;
            frame_index = 0;
            next_frame = Instant::now();

            match prepare_media(Path::new(media_path), config.quality) {
                Ok(media) => {
                    prepared = Some(media);
                }
                Err(error) => {
                    prepared = None;
                    log_error(error);
                }
            }
        }

        let Some(media) = prepared.as_ref() else {
            thread::sleep(Duration::from_millis(250));
            continue;
        };

        if device.is_none() && Instant::now() >= next_connect {
            match CorsairLcdDevice::open() {
                Ok(found) => {
                    let _ = found.set_brightness(config.brightness);
                    let _ = found.set_rotation(config.rotation);
                    applied_brightness = config.brightness;
                    applied_rotation = config.rotation;
                    device = Some(found);
                    frame_index = 0;
                    next_frame = Instant::now();
                }
                Err(error) => {
                    log_error(error);
                    next_connect = Instant::now() + Duration::from_secs(1);
                }
            }
        }

        let Some(connected) = device.as_ref() else {
            thread::sleep(Duration::from_millis(100));
            continue;
        };

        if applied_brightness != config.brightness {
            if connected.set_brightness(config.brightness).is_ok() {
                applied_brightness = config.brightness;
            }
        }
        if applied_rotation != config.rotation {
            if connected.set_rotation(config.rotation).is_ok() {
                applied_rotation = config.rotation;
            }
        }

        let now = Instant::now();
        if now < next_frame {
            let remaining = next_frame - now;
            thread::sleep(remaining.min(Duration::from_millis(15)));
            continue;
        }

        let frame = &media.frames[frame_index];
        if let Err(error) = connected.send_frame(&frame.jpeg) {
            log_error(error);
            device = None;
            next_connect = Instant::now() + Duration::from_secs(1);
            thread::sleep(Duration::from_millis(50));
            continue;
        }

        next_frame = Instant::now() + frame.delay;
        frame_index = (frame_index + 1) % media.frames.len();
    }

    if let Some(connected) = device.take() {
        connected.release_to_hardware();
    }
}

fn prepare_media(path: &Path, quality: u8) -> Result<PreparedMedia, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("Could not read custom Off Screen media: {error}"))?;
    if metadata.len() > MAX_MEDIA_BYTES {
        return Err("Custom Off Screen media is larger than 30 MB".into());
    }

    let is_gif = path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("gif"))
        .unwrap_or(false);

    if is_gif {
        prepare_gif(path, quality)
    } else {
        let image = image::ImageReader::open(path)
            .map_err(|error| format!("Could not open custom Off Screen image: {error}"))?
            .with_guessed_format()
            .map_err(|error| format!("Could not detect custom Off Screen image format: {error}"))?
            .decode()
            .map_err(|error| format!("Could not decode custom Off Screen image: {error}"))?;
        let jpeg = encode_frame(&image, quality)?;
        Ok(PreparedMedia {
            frames: vec![MediaFrame {
                jpeg,
                delay: Duration::from_secs(1),
            }],
        })
    }
}

fn prepare_gif(path: &Path, quality: u8) -> Result<PreparedMedia, String> {
    let file = File::open(path)
        .map_err(|error| format!("Could not open custom Off Screen GIF: {error}"))?;
    let decoder = GifDecoder::new(BufReader::new(file))
        .map_err(|error| format!("Could not decode custom Off Screen GIF: {error}"))?;

    let mut frames = Vec::new();
    let mut encoded_bytes = 0usize;

    for result in decoder.into_frames() {
        if frames.len() >= MAX_GIF_FRAMES {
            return Err(format!(
                "Custom Off Screen GIF has more than {MAX_GIF_FRAMES} frames"
            ));
        }

        let frame = result.map_err(|error| format!("Could not decode GIF frame: {error}"))?;
        let (numerator, denominator) = frame.delay().numer_denom_ms();
        let delay_ms = if denominator == 0 {
            100
        } else {
            ((numerator as u64 + denominator as u64 - 1) / denominator as u64).clamp(34, 1000)
        };

        let image = DynamicImage::ImageRgba8(frame.into_buffer());
        let jpeg = encode_frame(&image, quality)?;
        encoded_bytes = encoded_bytes.saturating_add(jpeg.len());
        if encoded_bytes > MAX_ENCODED_BYTES {
            return Err("Custom Off Screen GIF expands beyond the safe in-memory limit".into());
        }

        frames.push(MediaFrame {
            jpeg,
            delay: Duration::from_millis(delay_ms),
        });
    }

    if frames.is_empty() {
        return Err("Custom Off Screen GIF contains no frames".into());
    }

    Ok(PreparedMedia { frames })
}

fn encode_frame(image: &DynamicImage, quality: u8) -> Result<Vec<u8>, String> {
    let source = image.to_rgb8();
    let (source_width, source_height) = source.dimensions();
    if source_width == 0 || source_height == 0 {
        return Err("Custom Off Screen image has invalid dimensions".into());
    }

    let scale =
        (LCD_SIZE as f64 / source_width as f64).min(LCD_SIZE as f64 / source_height as f64);
    let width = ((source_width as f64 * scale).round() as u32).clamp(1, LCD_SIZE);
    let height = ((source_height as f64 * scale).round() as u32).clamp(1, LCD_SIZE);
    let resized = resize(&source, width, height, FilterType::Triangle);

    let mut canvas = RgbImage::new(LCD_SIZE, LCD_SIZE);
    let x = ((LCD_SIZE - width) / 2) as i64;
    let y = ((LCD_SIZE - height) / 2) as i64;
    overlay(&mut canvas, &resized, x, y);

    let mut jpeg = Vec::with_capacity(32 * 1024);
    JpegEncoder::new_with_quality(&mut jpeg, quality.clamp(35, 85))
        .encode(
            canvas.as_raw(),
            LCD_SIZE,
            LCD_SIZE,
            ExtendedColorType::Rgb8,
        )
        .map_err(|error| format!("Could not encode custom Off Screen frame: {error}"))?;

    Ok(jpeg)
}
