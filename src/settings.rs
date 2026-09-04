use std::ffi::OsStr;
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr::{null, null_mut};

use windows_sys::Win32::System::Registry::{
    HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "CorsairEliteDisplay";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    #[default]
    Native,
    Zoom4x3,
    Zoom16x10,
    Zoom16x9,
}

impl ViewMode {
    pub const ALL: [Self; 4] = [Self::Native, Self::Zoom4x3, Self::Zoom16x10, Self::Zoom16x9];

    pub fn zoom(self) -> f64 {
        match self {
            Self::Native => 1.0,
            Self::Zoom4x3 => 4.0 / 3.0,
            Self::Zoom16x10 => 16.0 / 10.0,
            Self::Zoom16x9 => 16.0 / 9.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Native => "Native",
            Self::Zoom4x3 => "4:3 zoom",
            Self::Zoom16x10 => "16:10 zoom",
            Self::Zoom16x9 => "16:9 zoom",
        }
    }

    fn from_setting(value: &str) -> Self {
        match value.trim() {
            "4:3" => Self::Zoom4x3,
            "16:10" => Self::Zoom16x10,
            "16:9" => Self::Zoom16x9,
            _ => Self::Native,
        }
    }

    fn setting_value(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Zoom4x3 => "4:3",
            Self::Zoom16x10 => "16:10",
            Self::Zoom16x9 => "16:9",
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub monitor: String,
    pub fps: u32,
    pub quality: u8,
    pub brightness: u8,
    pub rotation: u16,
    pub view_mode: ViewMode,
    pub show_mouse: bool,
    pub streaming: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            monitor: String::new(),
            fps: 30,
            quality: 65,
            brightness: 100,
            rotation: 0,
            view_mode: ViewMode::Native,
            show_mouse: true,
            streaming: false,
        }
    }
}

impl Settings {
    fn path() -> Option<PathBuf> {
        std::env::var_os("APPDATA").map(|p| {
            PathBuf::from(p)
                .join("CorsairEliteDisplay")
                .join("settings.ini")
        })
    }

    pub fn load() -> Self {
        let mut result = Self::default();
        let Some(path) = Self::path() else {
            return result;
        };
        let Ok(text) = fs::read_to_string(path) else {
            return result;
        };
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "monitor" => result.monitor = value.trim().to_owned(),
                "fps" => result.fps = value.trim().parse().unwrap_or(30).clamp(5, 30),
                "quality" => result.quality = value.trim().parse().unwrap_or(65).clamp(35, 95),
                "brightness" => {
                    result.brightness = value.trim().parse().unwrap_or(100).clamp(10, 100)
                }
                "rotation" => {
                    let value = value.trim().parse().unwrap_or(0);
                    result.rotation = if [0, 90, 180, 270].contains(&value) {
                        value
                    } else {
                        0
                    };
                }
                "view" => result.view_mode = ViewMode::from_setting(value),
                "show_mouse" => result.show_mouse = value.trim() != "false",
                "streaming" => {
                    let s = value.trim().to_lowercase();
                    result.streaming = s == "true" || s == "1" || s == "on";
                }
                _ => {}
            }
        }
        result
    }

    pub fn save(&self) -> Result<(), String> {
        let path = Self::path().ok_or_else(|| "APPDATA is unavailable".to_string())?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = format!(
            "monitor={}\nfps={}\nquality={}\nbrightness={}\nrotation={}\nview={}\nshow_mouse={}\nstreaming={}\n",
            self.monitor,
            self.fps,
            self.quality,
            self.brightness,
            self.rotation,
            self.view_mode.setting_value(),
            self.show_mouse,
            self.streaming
        );
        fs::write(path, text).map_err(|e| e.to_string())
    }
}

pub fn startup_enabled() -> bool {
    unsafe {
        let mut key = null_mut();
        let key_name = wide(RUN_KEY);
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_name.as_ptr(),
            0,
            KEY_QUERY_VALUE,
            &mut key,
        ) != 0
        {
            return false;
        }
        let value_name = wide(RUN_VALUE);
        let ok = RegQueryValueExW(
            key,
            value_name.as_ptr(),
            null(),
            null_mut(),
            null_mut(),
            null_mut(),
        ) == 0;
        RegCloseKey(key);
        ok
    }
}

pub fn set_startup(enabled: bool) -> Result<(), String> {
    unsafe {
        let mut key = null_mut();
        let key_name = wide(RUN_KEY);
        let mut disposition = 0;
        let status = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_name.as_ptr(),
            0,
            null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            null(),
            &mut key,
            &mut disposition,
        );
        if status != 0 {
            return Err(format!("Could not open the Windows startup key ({status})"));
        }
        let value_name = wide(RUN_VALUE);
        let status = if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let command = wide(&format!("\"{}\" --background", exe.display()));
            RegSetValueExW(
                key,
                value_name.as_ptr(),
                0,
                REG_SZ,
                command.as_ptr().cast(),
                (command.len() * 2) as u32,
            )
        } else {
            RegDeleteValueW(key, value_name.as_ptr())
        };
        RegCloseKey(key);
        if status == 0 || (!enabled && status == 2) {
            Ok(())
        } else {
            Err(format!("Could not update Windows startup ({status})"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_is_off() {
        let settings = Settings::default();
        assert_eq!(settings.streaming, false);
    }

    #[test]
    fn parses_streaming_flag_correctly() {
        let test_cases = [
            ("streaming=true", true),
            ("streaming=1", true),
            ("streaming=on", true),
            ("streaming=false", false),
            ("streaming=0", false),
            ("streaming=off", false),
        ];

        for (line, expected) in test_cases {
            let mut s = Settings::default();
            let (k, v) = line.split_once('=').unwrap();
            if k == "streaming" {
                let val = v.trim().to_lowercase();
                s.streaming = val == "true" || val == "1" || val == "on";
            }
            assert_eq!(s.streaming, expected, "Failed for {line}");
        }
    }
}
