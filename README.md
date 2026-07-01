# GoPro Cam (Windows)

A tiny, native Windows tray app that turns a **GoPro** into a **webcam** — a
lightweight replacement for GoPro's official (and rarely updated) webcam utility.

- **Small & light**: ~800 KB executable, ~10 MB RAM at idle.
- **Zero background bloat**: no console window, single instance, lives in the
  system tray.
- **Just works**: plug the GoPro in (webcam mode) and it starts streaming
  automatically; unplug and it goes back to waiting.
- **Low latency, smooth 30 fps**, no on-screen artefacts.

## How it works

```
GoPro (USB) ──HTTP start──▶ camera
          ──UDP MPEG-TS──▶ demux ─▶ Windows H.264 decoder (Media Foundation)
          ─▶ NV12 ─▶ OBS Virtual Camera shared memory ─▶ any app
```

The GoPro, in webcam mode, exposes a USB network interface and streams
MPEG-TS/H.264 over UDP. This app drives it over the documented HTTP API, demuxes
the transport stream, decodes H.264 with the **built-in Windows decoder** (no
bundled codec), and publishes NV12 frames into the **OBS Virtual Camera** shared
memory — the same channel OBS uses. That means the camera shows up as
*"OBS Virtual Camera"* in Zoom / Teams / Meet / the Camera app, **without OBS
running**.

## Requirements

- **Windows 10 / 11**.
- **OBS Studio installed** (for its virtual-camera component). OBS does **not**
  need to be running — only its virtual-camera DLL needs to be registered, which
  happens once you install OBS and start its virtual camera a single time.
- A **GoPro with webcam mode** (recent firmware exposes it automatically over
  USB-C).
- To build: a **Rust toolchain** (MSVC) and the Windows SDK (`rc.exe`, for the
  embedded icon).

## Build

```powershell
cargo build --release
# -> target\release\gopro-cam-tray.exe
```

## Usage

Run `gopro-cam-tray.exe`. A camera icon appears in the system tray. Connect the
GoPro (webcam mode) and pick **"OBS Virtual Camera"** as your camera in any app.

Right-click the tray icon:

| Menu entry | Effect |
|---|---|
| *Status* | GoPro: streaming / suspended / waiting |
| **Suspendre (reprend au rebranchement)** | Stop streaming but keep watching; auto-resumes when the camera is re-plugged |
| **Reprendre** | Resume immediately (when suspended) |
| **Lancer au démarrage** | Toggle auto-start at login (a hidden launcher in the Startup folder) |
| **Quitter** | Quit for good |

## Notes & limitations

- Streams at 1080p30 (whatever the GoPro's webcam mode outputs).
- Windows only. The portable core (HTTP control + UDP capture) is
  cross-platform; only the virtual-camera sink is Windows-specific here.
- The low-latency `STARTLTP` protocol used by GoPro's macOS app is not publicly
  documented; this uses the standard UDP/8554 endpoint.

## Acknowledgements

- The OBS Studio project — the virtual-camera shared-memory format is reproduced
  from its `win-dshow` plugin.
- The GoPro reverse-engineering community (Open GoPro, `gopro_as_webcam_on_linux`,
  `GoProStream`).

## License

MIT — see [LICENSE](LICENSE).
