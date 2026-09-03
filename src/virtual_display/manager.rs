use std::fs;
use std::path::Path;
use std::process::Command;

const VDD_DIR: &str = r"C:\VirtualDisplayDriver";
const VDD_SETTINGS_PATH: &str = r"C:\VirtualDisplayDriver\vdd_settings.xml";

pub struct VirtualDisplayManager;

impl VirtualDisplayManager {
    pub fn is_installed() -> bool {
        Path::new(VDD_DIR).exists() || Path::new(VDD_SETTINGS_PATH).exists()
    }

    pub fn is_480_configured() -> bool {
        if let Ok(content) = fs::read_to_string(VDD_SETTINGS_PATH) {
            content.contains("<width>480</width>") && content.contains("<height>480</height>")
        } else {
            false
        }
    }

    pub fn write_480_config() -> Result<(), String> {
        let dir = Path::new(VDD_DIR);
        if !dir.exists() {
            fs::create_dir_all(dir).map_err(|e| format!("Failed to create {}: {}", VDD_DIR, e))?;
        }

        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<!-- Corsair Elite LCD 480x480 Virtual Display Configuration -->
<vdd_settings>
    <monitors>
        <count>1</count>
    </monitors>

    <gpu>
        <friendlyname>default</friendlyname>
    </gpu>

    <global>
        <g_refresh_rate>30</g_refresh_rate>
        <g_refresh_rate>60</g_refresh_rate>
    </global>

    <resolutions>
        <resolution>
            <width>480</width>
            <height>480</height>
            <refresh_rate>30</refresh_rate>
        </resolution>
        <resolution>
            <width>480</width>
            <height>480</height>
            <refresh_rate>60</refresh_rate>
        </resolution>
    </resolutions>

    <logging>
        <SendLogsThroughPipe>false</SendLogsThroughPipe>
        <logging>false</logging>
        <debuglogging>false</debuglogging>
    </logging>

    <colour>
        <SDR10bit>false</SDR10bit>
        <HDRPlus>false</HDRPlus>
        <ColourFormat>RGB</ColourFormat>
    </colour>

    <cursor>
        <HardwareCursor>true</HardwareCursor>
        <CursorMaxX>128</CursorMaxX>
        <CursorMaxY>128</CursorMaxY>
        <AlphaCursorSupport>true</AlphaCursorSupport>
    </cursor>
</vdd_settings>
"#;

        fs::write(VDD_SETTINGS_PATH, xml)
            .map_err(|e| format!("Failed to write {}: {}", VDD_SETTINGS_PATH, e))?;
        Ok(())
    }

    pub fn install_vdd_via_winget() -> Result<(), String> {
        // Pre-create the 480x480 config before installing
        let _ = Self::write_480_config();

        let status = Command::new("winget")
            .args([
                "install",
                "--id=VirtualDrivers.Virtual-Display-Driver",
                "-e",
                "--accept-source-agreements",
                "--accept-package-agreements",
            ])
            .status()
            .map_err(|e| format!("Failed to launch winget: {}", e))?;

        if status.success() {
            let _ = Self::write_480_config();
            Ok(())
        } else {
            Err(format!("Winget exited with status: {}", status))
        }
    }

    pub fn launch_installer_script() -> Result<(), String> {
        let _ = Self::write_480_config();
        let bat_path = r"C:\Users\diogo\repos\corsair-elite-display\Install-Virtual-Display.bat";
        Command::new("cmd")
            .args(["/c", "start", "", bat_path])
            .spawn()
            .map_err(|e| format!("Failed to launch installer: {}", e))?;
        Ok(())
    }

}
