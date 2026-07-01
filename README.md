# GoPro Cam

A tiny, native tray app that turns a **GoPro** into a **webcam**. It is a
lightweight replacement for GoPro's official (and rarely updated) webcam utility.

- Small and light: ~800 KB executable, ~10 MB RAM at idle.
- No background bloat: no console window, single instance, lives in the system tray.
- Just works: turn the GoPro on, connect it over USB, and it starts streaming
  automatically; unplug and it goes back to waiting.
- Low latency, smooth 30 fps, no on-screen artefacts.

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

- **[OBS Studio](https://github.com/obsproject/obs-studio/releases) installed**
  (for its virtual-camera component). OBS does not need to be running; its
  virtual-camera module just needs to be registered, which happens once you
  install OBS and start its virtual camera a single time.
- A **GoPro** that streams over USB (recent models start automatically when
  powered on and connected over USB-C, nothing to enable on the camera).
- A **Rust toolchain** to build.

## Build

```
cargo build --release
```

## Usage

Run the app. A camera icon appears in the system tray. Turn the GoPro on and
connect it over USB, then pick **"OBS Virtual Camera"** as your camera in any app.

Right-click the tray icon:

| Menu entry | Effect |
|---|---|
| *Status* | GoPro: streaming / suspended / waiting |
| **Suspendre (reprend au rebranchement)** | Stop streaming but keep watching; auto-resumes when the camera is re-plugged |
| **Reprendre** | Resume immediately (when suspended) |
| **Lancer au démarrage** | Toggle auto-start at login (a hidden launcher in the Startup folder) |
| **Quitter** | Quit for good |

## Platform support

The portable core (HTTP control plus UDP capture) is platform-agnostic; only the
H.264 decoder and the virtual-camera sink are platform-specific. The current
implementation targets the desktop; Linux and macOS backends are on the roadmap.

## Notes and limitations

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
