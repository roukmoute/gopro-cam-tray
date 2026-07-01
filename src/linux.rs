//! Linux backend: drive the GoPro over its HTTP API, then let ffmpeg decode the
//! UDP MPEG-TS stream and feed it into a v4l2loopback device (`/dev/videoN`),
//! which apps then see as a regular webcam.
//!
//! Requirements on the machine:
//!   - the `v4l2loopback` kernel module loaded (creates the /dev/videoN device),
//!     e.g.  sudo modprobe v4l2loopback exclusive_caps=1 card_label="GoPro"
//!   - `ffmpeg` installed.

use crate::{gopro, STREAM_PORT};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub fn run() {
    let running = Arc::new(AtomicBool::new(true));
    {
        let r = running.clone();
        let _ = ctrlc::set_handler(move || r.store(false, Ordering::SeqCst));
    }

    // Output device: 1st CLI arg, then $GOPRO_CAM_DEVICE, then auto-detect.
    let device = std::env::args()
        .nth(1)
        .filter(|a| a.starts_with("/dev/"))
        .or_else(|| std::env::var("GOPRO_CAM_DEVICE").ok())
        .or_else(find_loopback);

    let device = match device {
        Some(d) => d,
        None => {
            eprintln!(
                "No v4l2loopback device found.\n\
                 Load it first, e.g.:\n\
                 \x20 sudo modprobe v4l2loopback exclusive_caps=1 card_label=\"GoPro\"\n\
                 then rerun, or pass the device explicitly:  gopro-cam-tray /dev/videoN"
            );
            std::process::exit(1);
        }
    };

    if which("ffmpeg").is_none() {
        eprintln!("ffmpeg not found in PATH. Install it (e.g. sudo apt install ffmpeg).");
        std::process::exit(1);
    }

    println!("GoPro webcam -> {device}. Waiting for the camera... (Ctrl+C to quit)");

    while running.load(Ordering::SeqCst) {
        let ip = match gopro::detect() {
            Some(ip) => ip,
            None => {
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };

        println!("GoPro found at {ip}, starting stream.");
        if gopro::start(ip).is_err() {
            std::thread::sleep(Duration::from_secs(2));
            continue;
        }

        match spawn_ffmpeg(&device) {
            Ok(mut child) => {
                // Wait until ffmpeg exits (camera gone / error) or the user quits.
                while running.load(Ordering::SeqCst) {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) => std::thread::sleep(Duration::from_millis(300)),
                        Err(_) => break,
                    }
                }
                let _ = child.kill();
                let _ = child.wait();
            }
            Err(e) => eprintln!("failed to launch ffmpeg: {e}"),
        }

        gopro::stop(ip);
        if running.load(Ordering::SeqCst) {
            println!("Stream ended, waiting for the camera...");
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    println!("\nStopped.");
}

/// Launch ffmpeg: receive the GoPro's UDP MPEG-TS, decode, write to v4l2loopback.
fn spawn_ffmpeg(device: &str) -> std::io::Result<Child> {
    let input = format!("udp://0.0.0.0:{STREAM_PORT}?overrun_nonfatal=1&fifo_size=5000000");
    Command::new("ffmpeg")
        .args([
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
        .stdin(Stdio::null())
        .spawn()
}

/// Find a v4l2loopback device by looking for a video node backed by a virtual
/// (not physical/USB) device in sysfs.
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
