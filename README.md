# Corsair Elite Display

<p align="center">
  <img src="assets/icon-on.png" alt="Corsair Elite Display" width="128">
</p>

**Turn a Corsair Elite LCD cooler into a real 480×480 second Windows screen.** One small native background app, no iCUE, no bundled runtime, no bloat.

Move any window onto the virtual display and it appears on the cooler at up to the panel's native 30 FPS.

## Features

- **Real second display** — mirror a dedicated Windows monitor instead of uploading individual images or GIFs
- **Low-latency streaming** — latest-frame-only capture with no buffering or stale-frame queue
- **Native 30 FPS** — matches the LCD's maximum refresh rate
- **Flicker-free capture** — the mouse cursor is intentionally excluded from the captured image
- **Tray-only background app** — no main window; every setting lives in the notification-area menu
- **Tiny native EXE** — no Electron, WebView, .NET, Python, installer, or bundled runtime
- **Runtime-only monitor** — the 480×480 Windows display exists only while the app is open and switched on
- **Hardware hand-off** — switching off or exiting releases the LCD so its saved hardware screen resumes
- **Starts with Windows** — optional per-user startup toggle
- **Automatic reconnect** — waits quietly and reconnects if the cooler is unplugged or temporarily unavailable
- **Useful controls only** — on/off, frame rate, JPEG quality, brightness, rotation, and startup

## Quick Start

1. Download `corsair-elite-display-v1.0.0-windows-x64.exe` from [Releases](../../releases/latest).
2. Run it.
3. Move any window onto the 480×480 display.

Turning the app off removes that display immediately. Exiting—or even terminating the
app unexpectedly—also removes it through a tiny watchdog process. The detached Windows
display layout is saved per user, so a power interruption cannot make the screen return
by itself at the next boot.

The app has no main window. Right-click the cooler icon in the Windows notification area to turn the second screen on/off or change any setting.

> [!IMPORTANT]
> The app never installs or changes drivers and never requests administrator access. It uses
> the signed virtual-display target already available on this PC and changes its Windows
> desktop mode only while the app is running and switched on.

> [!NOTE]
> Corsair iCUE may claim exclusive access to the LCD. Close iCUE if the app reports that the cooler is unavailable. Fan, pump, and RGB control remain with the cooler hardware or your existing controller software.

## Lowest-Latency Setup

- Use the included 480×480 virtual display so no scaling is needed.
- Select **30 FPS** and **75% · Balanced** quality.
- Keep the target window at 480×480 when possible.
- The capture worker runs above normal priority, reuses its image/JPEG buffers, and immediately discards timing debt instead of allowing latency to accumulate.

The LCD hardware refreshes at 30 Hz, so selecting more than 30 FPS would only increase USB and CPU use without producing more visible frames.

## Supported LCDs

| USB ID | Device |
|---|---|
| `1B1C:0C39` | Elite LCD |
| `1B1C:0C33` | Elite LCD Upgrade Kit |
| `1B1C:0C4E` | iCUE LINK / TITAN LCD |
| `1B1C:0C42` | XC7 RGB Elite LCD |

## Building from Source

Install the stable Rust toolchain and a current Windows SDK, then run:

```powershell
cargo build --release
```

The standalone executable is created at `target\release\corsair-elite-display.exe`.

## How It Works

```text
Windows display
    ↓  GDI capture (480×480, cursor excluded)
Reusable RGB buffer
    ↓  low-latency JPEG encoding
Corsair HID image packets
    ↓
Cooler LCD (480×480 at up to 30 Hz)
```

Settings are stored in `%APPDATA%\CorsairEliteDisplay\settings.ini`. Enabling startup adds only the current EXE to the standard per-user Windows startup entry. The monitor is enabled and disabled with Windows' built-in per-user display switching, so runtime use needs no administrator access.

## Known Limitations

- Windows 10/11 x64 only.
- The app controls the LCD image, brightness, and rotation. It does not control the pump, fans, or RGB lighting.
- Hardware-screen restoration relies on the cooler firmware's device-memory behavior after the app releases its HID connection.

## Acknowledgments

- The Corsair LCD JPEG packet format was independently documented by the open-source hardware community.

## License

MIT
