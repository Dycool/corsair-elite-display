use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use image::codecs::gif::GifDecoder;
use image::{AnimationDecoder, DynamicImage, ExtendedColorType, ImageFormat};

use crate::corsair::CorsairLcdDevice;

const LCD_SIZE: u32 = 480;
const CACHE_FILE: &str = "hardware-media.cache";

#[derive(Clone)]
struct HardwareFrame {
    jpeg: Arc<[u8]>,
    delay: Duration,
}

#[derive(Clone)]
pub struct HardwareMedia {
    source: Arc<[u8]>,
    frames: Arc<[HardwareFrame]>,
}

impl HardwareMedia {
    pub fn from_path(path: &Path) -> Result<Self, String> {
        let bytes = fs::read(path)
            .map_err(|error| format!("Could not read hardware image/GIF: {error}"))?;
        Self::from_bytes(bytes)
    }

    pub fn load_cached() -> Result<Option<Self>, String> {
        let Some(path) = cache_path() else {
            return Ok(None);
        };
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)
            .map_err(|error| format!("Could not read cached hardware image/GIF: {error}"))?;
        match Self::from_bytes(bytes) {
            Ok(media) => Ok(Some(media)),
            Err(error) => {
                let _ = fs::remove_file(path);
                Err(format!("Cached hardware image/GIF was invalid and was removed: {error}"))
            }
        }
    }

    pub fn persist_cache(&self) -> Result<(), String> {
        let path = cache_path().ok_or_else(|| "APPDATA is unavailable".to_string())?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(path, self.source.as_ref())
            .map_err(|error| format!("Could not save the OFF-mode hardware media cache: {error}"))
    }

    fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        let format = image::guess_format(&bytes)
            .map_err(|error| format!("Unsupported hardware image/GIF format: {error}"))?;
        let source: Arc<[u8]> = bytes.into();

        let frames = if format == ImageFormat::Gif {
            decode_gif(source.as_ref())?
        } else {
            let image = image::load_from_memory(source.as_ref())
                .map_err(|error| format!("Could not decode hardware image: {error}"))?;
            vec![HardwareFrame {
                jpeg: encode_for_lcd(&image)?.into(),
                delay: Duration::ZERO,
            }]
        };

        if frames.is_empty() {
            return Err("Hardware image/GIF contained no displayable frames".into());
        }

        Ok(Self {
            source,
            frames: frames.into(),
        })
    }
}

pub struct HardwarePlayback {
    media: Option<HardwareMedia>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl HardwarePlayback {
    pub fn new(media: Option<HardwareMedia>) -> Self {
        Self {
            media,
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
        }
    }

    pub fn replace_media(&mut self, media: HardwareMedia) {
        self.stop();
        self.media = Some(media);
    }

    pub fn clear_media(&mut self) {
        self.stop();
        self.media = None;
    }

    /// Starts OFF-mode playback from the already-decoded RAM copy.
    /// Returns false when no cached hardware media exists yet.
    pub fn start(&mut self) -> bool {
        self.stop();
        let Some(media) = self.media.clone() else {
            return false;
        };

        self.stop.store(false, Ordering::Release);
        let stop = Arc::clone(&self.stop);
        self.worker = Some(thread::spawn(move || playback_loop(media, stop)));
        true
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.stop.store(false, Ordering::Release);
    }
}

impl Drop for HardwarePlayback {
    fn drop(&mut self) {
        self.stop();
    }
}

fn cache_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|root| {
        PathBuf::from(root)
            .join("CorsairEliteDisplay")
            .join(CACHE_FILE)
    })
}

fn encode_for_lcd(image: &DynamicImage) -> Result<Vec<u8>, String> {
    let resized = image.resize_exact(
        LCD_SIZE,
        LCD_SIZE,
        image::imageops::FilterType::Lanczos3,
    );
    let rgb = resized.to_rgb8();
    let mut jpeg = Vec::with_capacity(64 * 1024);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95)
        .encode(
            &rgb,
            LCD_SIZE,
            LCD_SIZE,
            ExtendedColorType::Rgb8,
        )
        .map_err(|error| format!("Could not encode hardware media for the LCD: {error}"))?;
    Ok(jpeg)
}

fn decode_gif(bytes: &[u8]) -> Result<Vec<HardwareFrame>, String> {
    let decoder = GifDecoder::new(Cursor::new(bytes))
        .map_err(|error| format!("Could not decode hardware GIF: {error}"))?;
    let frames = decoder
        .into_frames()
        .collect_frames()
        .map_err(|error| format!("Could not decode hardware GIF frames: {error}"))?;

    let mut output = Vec::with_capacity(frames.len());
    for frame in frames {
        let delay = frame.delay();
        let (numerator, denominator) = delay.numer_denom_ms();
        let delay_ms = if denominator == 0 {
            100
        } else {
            ((numerator as u64 + denominator as u64 - 1) / denominator as u64).clamp(20, 10_000)
        };
        let image = DynamicImage::ImageRgba8(frame.into_buffer());
        output.push(HardwareFrame {
            jpeg: encode_for_lcd(&image)?.into(),
            delay: Duration::from_millis(delay_ms),
        });
    }
    Ok(output)
}

fn playback_loop(media: HardwareMedia, stop: Arc<AtomicBool>) {
    if media.frames.len() == 1 {
        play_static(&media.frames[0], &stop);
    } else {
        play_animation(&media, &stop);
    }
}

fn play_static(frame: &HardwareFrame, stop: &AtomicBool) {
    let mut last_error = String::new();
    for _ in 0..30 {
        if stop.load(Ordering::Acquire) {
            return;
        }
        match CorsairLcdDevice::open().and_then(|device| device.send_frame(frame.jpeg.as_ref())) {
            Ok(()) => return,
            Err(error) => last_error = error,
        }
        sleep_interruptible(stop, Duration::from_millis(100));
    }
    if !last_error.is_empty() {
        write_playback_error(&last_error);
    }
}

fn play_animation(media: &HardwareMedia, stop: &AtomicBool) {
    let mut device: Option<CorsairLcdDevice> = None;
    let mut index = 0usize;
    let mut last_error = String::new();

    while !stop.load(Ordering::Acquire) {
        if device.is_none() {
            match CorsairLcdDevice::open() {
                Ok(found) => device = Some(found),
                Err(error) => {
                    last_error = error;
                    sleep_interruptible(stop, Duration::from_millis(200));
                    continue;
                }
            }
        }

        let frame = &media.frames[index];
        match device
            .as_ref()
            .expect("device checked above")
            .send_frame(frame.jpeg.as_ref())
        {
            Ok(()) => {
                index = (index + 1) % media.frames.len();
                sleep_interruptible(stop, frame.delay);
            }
            Err(error) => {
                last_error = error;
                device = None;
                sleep_interruptible(stop, Duration::from_millis(100));
            }
        }
    }

    if !last_error.is_empty() && !stop.load(Ordering::Acquire) {
        write_playback_error(&last_error);
    }
}

fn sleep_interruptible(stop: &AtomicBool, duration: Duration) {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::Acquire) {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        thread::sleep((deadline - now).min(Duration::from_millis(20)));
    }
}

fn write_playback_error(error: &str) {
    let _ = fs::write(
        std::env::temp_dir().join("corsair-elite-display-off-playback.txt"),
        format!("OFF-mode hardware media playback failed: {error}\n"),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_hardware_media_encodes_to_lcd_jpeg() {
        let image = DynamicImage::ImageRgb8(image::RgbImage::new(32, 32));
        let jpeg = encode_for_lcd(&image).unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8]);
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xff, 0xd9]);
    }
}
