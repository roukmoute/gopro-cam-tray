//! Linux backend: drive the GoPro over its HTTP API, let ffmpeg decode the UDP
//! MPEG-TS stream into a v4l2loopback device, and expose a system-tray icon
//! (StatusNotifierItem) with the same menu as the Windows version.
//!
//! Requirements on the machine:
//!   - `v4l2loopback` kernel module loaded (creates /dev/videoN),
//!     e.g.  sudo modprobe v4l2loopback exclusive_caps=1 card_label="GoPro"
//!   - `ffmpeg` installed.
//! The tray uses the StatusNotifier protocol (works on GNOME with AppIndicator,
//! KDE, etc.); under i3 you need an SNI->XEmbed bridge such as `snixembed`.

use crate::{gopro, STREAM_PORT};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

struct Control {
    quit: AtomicBool,
    suspended: AtomicBool,
    streaming: AtomicBool,
}

pub fn run() {
    // Single instance: hold an flock for the whole process lifetime.
    let _lock = match acquire_lock() {
        Some(f) => f,
        None => {
            eprintln!("gopro-cam-tray is already running.");
            return;
        }
    };

    if which("ffmpeg").is_none() {
        eprintln!("ffmpeg not found in PATH. Install it (e.g. sudo apt install ffmpeg).");
        std::process::exit(1);
    }
    let device = match resolve_device() {
        Some(d) => d,
        None => {
            eprintln!(
                "No v4l2loopback device found.\n\
                 Load it first, e.g.:\n\
                 \x20 sudo modprobe v4l2loopback exclusive_caps=1 card_label=\"GoPro\"\n\
                 or pass the device explicitly:  gopro-cam-tray /dev/videoN"
            );
            std::process::exit(1);
        }
    };

    let ctrl = Arc::new(Control {
        quit: AtomicBool::new(false),
        suspended: AtomicBool::new(false),
        streaming: AtomicBool::new(false),
    });

    {
        let c = ctrl.clone();
        let _ = ctrlc::set_handler(move || c.quit.store(true, Ordering::SeqCst));
    }

    // Streaming loop on its own thread.
    let sc = ctrl.clone();
    let dev = device.clone();
    let worker = std::thread::spawn(move || stream_loop(sc, dev));

    // Tray + event loop on the main thread (blocks until quit).
    run_tray(ctrl.clone(), device);

    ctrl.quit.store(true, Ordering::SeqCst);
    let _ = worker.join();
}

// --- Streaming ------------------------------------------------------------

fn stream_loop(ctrl: Arc<Control>, device: String) {
    while !ctrl.quit.load(Ordering::SeqCst) {
        if ctrl.suspended.load(Ordering::SeqCst) {
            // Auto-clear once the camera is unplugged, so a re-plug resumes.
            if gopro::detect().is_none() {
                ctrl.suspended.store(false, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }

        let ip = match gopro::detect() {
            Some(ip) => ip,
            None => {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        if gopro::start(ip).is_err() {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }
        ctrl.streaming.store(true, Ordering::SeqCst);

        if let Ok(mut child) = spawn_ffmpeg(&device) {
            while !ctrl.quit.load(Ordering::SeqCst) && !ctrl.suspended.load(Ordering::SeqCst) {
                match child.try_wait() {
                    Ok(Some(_)) => break, // ffmpeg exited (camera gone / error)
                    Ok(None) => std::thread::sleep(Duration::from_millis(300)),
                    Err(_) => break,
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        }

        ctrl.streaming.store(false, Ordering::SeqCst);
        gopro::stop(ip);
        if !ctrl.quit.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(500));
        }
    }
}

/// Launch ffmpeg: receive the GoPro's UDP MPEG-TS, decode, write to v4l2loopback.
/// The child is set to die with us (PR_SET_PDEATHSIG) so a killed daemon never
/// leaves an ffmpeg holding the UDP port.
fn spawn_ffmpeg(device: &str) -> std::io::Result<Child> {
    let input = format!("udp://0.0.0.0:{STREAM_PORT}?overrun_nonfatal=1&fifo_size=5000000");
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-fflags",
        "nobuffer",
        "-flags",
        "low_delay",
        "-i",
        &input,
        "-pix_fmt",
        "yuv420p",
        "-f",
        "v4l2",
        device,
    ])
    .stdin(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    cmd.spawn()
}

/// Open a preview window (ffplay on the loopback). Fire-and-forget: the user
/// closes the window; it also dies with us via PR_SET_PDEATHSIG.
fn spawn_ffplay(device: &str) -> std::io::Result<Child> {
    let mut cmd = Command::new("ffplay");
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-window_title",
        "GoPro — Aperçu",
        device,
    ])
    .stdin(Stdio::null());
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    cmd.spawn()
}

// --- System tray (StatusNotifierItem via ksni) ----------------------------

struct GoProTray {
    ctrl: Arc<Control>,
    device: String,
}

impl ksni::Tray for GoProTray {
    fn id(&self) -> String {
        "gopro-cam-tray".into()
    }
    fn icon_name(&self) -> String {
        "camera-web".into()
    }
    fn title(&self) -> String {
        "GoPro Cam".into()
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        let streaming = self.ctrl.streaming.load(Ordering::SeqCst);
        let suspended = self.ctrl.suspended.load(Ordering::SeqCst);
        let status = if streaming {
            "GoPro : diffusion en cours"
        } else if suspended {
            "GoPro : suspendu"
        } else {
            "GoPro : en attente"
        };

        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            StandardItem {
                label: status.into(),
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
        ];

        // Preview: open the loopback in ffplay (only shows while streaming).
        items.push(
            StandardItem {
                label: "Aperçu".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = spawn_ffplay(&t.device);
                }),
                ..Default::default()
            }
            .into(),
        );

        if suspended {
            items.push(
                StandardItem {
                    label: "Reprendre".into(),
                    activate: Box::new(|t: &mut Self| t.ctrl.suspended.store(false, Ordering::SeqCst)),
                    ..Default::default()
                }
                .into(),
            );
        } else {
            items.push(
                StandardItem {
                    label: "Suspendre (reprend au rebranchement)".into(),
                    activate: Box::new(|t: &mut Self| t.ctrl.suspended.store(true, Ordering::SeqCst)),
                    ..Default::default()
                }
                .into(),
            );
        }

        items.push(
            CheckmarkItem {
                label: "Lancer au démarrage".into(),
                checked: autostart_enabled(),
                activate: Box::new(|_t: &mut Self| {
                    if autostart_enabled() {
                        let _ = autostart_disable();
                    } else {
                        let _ = autostart_enable();
                    }
                }),
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quitter".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| t.ctrl.quit.store(true, Ordering::SeqCst)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

fn run_tray(ctrl: Arc<Control>, device: String) {
    use ksni::TrayMethods;

    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => {
            // No async runtime => run headless until quit.
            while !ctrl.quit.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(300));
            }
            return;
        }
    };

    rt.block_on(async {
        let handle = GoProTray {
            ctrl: ctrl.clone(),
            device,
        }
        .spawn()
        .await
        .ok();
        if handle.is_none() {
            eprintln!("Could not register the tray icon (no StatusNotifier host?). Running headless.");
        }
        loop {
            if ctrl.quit.load(Ordering::SeqCst) {
                break;
            }
            // Refresh the menu/status to reflect the streaming thread's state.
            if let Some(h) = &handle {
                let _ = h.update(|_t: &mut GoProTray| {}).await;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}

// --- Auto-start (XDG autostart .desktop) ----------------------------------

fn autostart_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.config", std::env::var("HOME").unwrap_or_default()));
    std::path::PathBuf::from(base)
        .join("autostart")
        .join("gopro-cam-tray.desktop")
}

fn autostart_enabled() -> bool {
    autostart_path().exists()
}

fn autostart_enable() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let path = autostart_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=GoPro Cam\n\
         Exec={}\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    std::fs::write(path, content)
}

fn autostart_disable() -> std::io::Result<()> {
    let path = autostart_path();
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

// --- Helpers --------------------------------------------------------------

/// Single-instance lock: hold an exclusive flock for the process lifetime.
fn acquire_lock() -> Option<std::fs::File> {
    use std::os::unix::io::AsRawFd;
    let path = format!(
        "{}/gopro-cam-tray.lock",
        std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())
    );
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(path)
        .ok()?;
    let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret == 0 {
        Some(file)
    } else {
        None
    }
}

/// Output device: 1st CLI arg, then $GOPRO_CAM_DEVICE, then auto-detect.
fn resolve_device() -> Option<String> {
    std::env::args()
        .nth(1)
        .filter(|a| a.starts_with("/dev/"))
        .or_else(|| std::env::var("GOPRO_CAM_DEVICE").ok())
        .or_else(find_loopback)
}

/// Find a v4l2loopback device: a video node backed by a virtual (not USB) device.
fn find_loopback() -> Option<String> {
    let mut found: Vec<String> = std::fs::read_dir("/sys/class/video4linux")
        .ok()?
        .flatten()
        .filter_map(|e| {
            let target = std::fs::read_link(e.path()).ok()?;
            if target.to_string_lossy().contains("devices/virtual") {
                e.file_name().into_string().ok()
            } else {
                None
            }
        })
        .collect();
    found.sort();
    found.first().map(|n| format!("/dev/{n}"))
}

/// Minimal `which`: is `bin` on the PATH?
fn which(bin: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let p = dir.join(bin);
        if p.is_file() {
            Some(p)
        } else {
            None
        }
    })
}
