# GoPro Cam

A tiny, native app that turns a **GoPro** into a **webcam** — a lightweight
replacement for GoPro's official (and rarely updated) webcam utility. Runs on
**Windows** (system-tray app) and **Linux** (CLI, via v4l2loopback).

- Small and light: ~800 KB executable, ~10 MB RAM at idle.
- No background bloat: no console window, single instance, lives in the system tray.
- Just works: turn the GoPro on, connect it over USB, and it starts streaming
  automatically; unplug and it goes back to waiting.
- Low latency, smooth 30 fps, no on-screen artefacts.
- Replaces GoPro's official Webcam app: you don't need it installed or running
  (in fact, don't run both at once — see the note below).

## Download

Latest builds (single executable, no installer):

- **Windows:** [⬇ gopro-cam-tray.exe](https://github.com/roukmoute/gopro-cam-tray/releases/latest/download/gopro-cam-tray.exe)
  (on first run see [Windows SmartScreen](#windows-smartscreen) below)
- **Linux (x86-64, static):** [⬇ gopro-cam-tray](https://github.com/roukmoute/gopro-cam-tray/releases/latest/download/gopro-cam-tray)
  (`chmod +x gopro-cam-tray` before running)

Or browse every version on the
[Releases](https://github.com/roukmoute/gopro-cam-tray/releases) page.

## How it works

When it is powered on and connected over USB, the GoPro exposes a USB network
interface and streams MPEG-TS/H.264 over UDP. This app drives it over the
documented HTTP API, then processes the stream in a few steps:

1. HTTP: start / stop the camera's USB stream.
2. Receive the UDP MPEG-TS stream.
3. Demux it into an H.264 elementary stream.
4. Decode H.264 into raw NV12 frames using the system's built-in decoder.
5. Publish the frames to a virtual camera.
6. Any app (Zoom, Teams, Meet, the Camera app, ...) sees it as a regular webcam.

## Requirements

Common: a **GoPro** that streams over USB (recent models start automatically
when powered on and connected over USB-C, nothing to enable on the camera).

**Windows:**
- **[OBS Studio](https://github.com/obsproject/obs-studio/releases) installed**
  (for its virtual-camera component). OBS does not need to be running; its
  virtual-camera module just needs to be registered, which happens once you
  install OBS and start its virtual camera a single time.

**Linux:**
- **`ffmpeg`** installed (`sudo apt install ffmpeg`).
- The **`v4l2loopback`** kernel module, loaded to create the virtual camera:
  ```
  sudo apt install v4l2loopback-dkms
  sudo modprobe v4l2loopback exclusive_caps=1 card_label="GoPro"
  ```
  Two common gotchas:
  - **Recent kernels (6.x):** the distro's `v4l2loopback` package may be too old
    to build. Install the latest from source instead:
    ```
    git clone https://github.com/umlaeute/v4l2loopback
    cd v4l2loopback && make && sudo make install && sudo depmod -a
    ```
  - **Secure Boot:** `modprobe` fails with *"Key was rejected by service"* unless
    the module is signed. Enroll the DKMS key
    (`sudo mokutil --import /var/lib/dkms/mok.pub`, then reboot and complete the
    MOK enrolment), or disable Secure Boot.

To build from source: a **Rust toolchain**.

## Build

```
cargo build --release
```

## Usage

### Windows

1. Run the app. A camera icon appears in the system tray.
2. Turn the GoPro on and connect it over USB.
3. In your video app (Zoom, Teams, Google Meet, Discord, the Windows Camera app,
   ...), open its camera / video settings and select **"OBS Virtual Camera"** as
   the video device. That's the camera this app feeds; nothing works until you
   pick it.

Right-click the tray icon:

| Menu entry | Effect |
|---|---|
| *Status* | GoPro: streaming / suspended / waiting |
| **Suspendre (reprend au rebranchement)** | Stop streaming but keep watching; auto-resumes when the camera is re-plugged |
| **Reprendre** | Resume immediately (when suspended) |
| **Lancer au démarrage** | Toggle auto-start at login (a hidden launcher in the Startup folder) |
| **Quitter** | Quit for good |

### Windows SmartScreen

The released `.exe` is not code-signed, so on first run Windows may warn
"Windows protected your PC" with an unknown publisher. Click **More info**, then
**Run anyway**. Removing this warning would require a paid code-signing
certificate.

### Linux

1. Make sure `ffmpeg` is installed and `v4l2loopback` is loaded (see
   [Requirements](#requirements)).
2. Turn the GoPro on and connect it over USB.
3. Run the daemon (it auto-detects the v4l2loopback device):
   ```
   ./gopro-cam-tray
   # or force a device:  ./gopro-cam-tray /dev/videoN
   ```
   It streams while the camera is connected and waits when it's unplugged;
   press Ctrl+C to quit.
4. Select the **"GoPro"** camera (the v4l2loopback device) in your video app.

## Platform support

Windows and Linux (x86-64) are supported; macOS is on the roadmap. The core
(GoPro discovery, HTTP control, UDP capture) is shared. Only the H.264 decode
and the virtual-camera sink differ per platform: Media Foundation + the OBS
Virtual Camera on Windows, `ffmpeg` + `v4l2loopback` on Linux.

## Notes and limitations

- **Use only one webcam app at a time.** Don't run this alongside GoPro's
  official Webcam software: both drive the camera's HTTP API and bind the same
  UDP port, which conflicts and can leave the camera stuck. If that happens, the
  fix is on the camera: *Preferences → Connections → Reset Connections* (a USB
  unplug alone won't reset it, since USB keeps the camera powered).
- Streams at 1080p30 (whatever the GoPro's USB stream outputs).
- The low-latency `STARTLTP` protocol used by GoPro's macOS app is not publicly
  documented; this uses the standard UDP/8554 endpoint.
- Why OBS isn't bundled: OBS's virtual camera is GPLv2. This project only talks
  to it at runtime through shared memory (the same approach as `pyvirtualcam`),
  it does not redistribute any OBS code, which keeps it MIT. A fully
  self-contained version would require shipping our own virtual-camera module (a
  possible future addition).

## Acknowledgements

- The [OBS Studio](https://github.com/obsproject/obs-studio) project (the
  virtual-camera shared-memory format is reproduced from its virtual-camera plugin).
- The GoPro reverse-engineering community:
  [Open GoPro](https://gopro.github.io/OpenGoPro/),
  [gopro_as_webcam_on_linux](https://github.com/jschmid1/gopro_as_webcam_on_linux),
  and [GoProStream](https://github.com/KonradIT/GoProStream).

## License

MIT, see [LICENSE](LICENSE).
