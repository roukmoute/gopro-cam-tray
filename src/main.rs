//! GoPro webcam — minimal system-tray edition.
//!
//! Runs hidden in the background: waits for the GoPro (webcam mode) and streams
//! it to the OBS Virtual Camera whenever connected. A tray icon lets you:
//!   - suspend streaming (auto-resumes when the camera is re-plugged),
//!   - toggle "run at login",
//!   - quit for good.
//!
//! Reuses the proven pipeline: UDP MPEG-TS -> demux -> Windows H.264 decoder
//! (MFT) -> NV12 -> OBS shared-memory sink. Idle footprint ~8 MB.

#![windows_subsystem = "windows"] // no console window

mod gopro;
mod mf_decode;
mod mpegts;
mod obs_vcam;
mod startup;
mod tray;

use mf_decode::Frame;
use obs_vcam::ObsVirtualCam;
use std::collections::VecDeque;
use std::net::Ipv4Addr;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const INTERVAL_100NS: u64 = 10_000_000 / 30;
const STREAM_PORT: u16 = 8554;

/// Shared control state between the tray (GUI) thread and the watcher thread.
pub struct Control {
    /// Exit the whole process.
    pub quit: AtomicBool,
    /// Stop streaming; auto-resumes once the camera is unplugged then replugged,
    /// or immediately via the tray "Reprendre" action.
    pub suspended: AtomicBool,
    /// True while a camera session is actively streaming (for the tray status).
    pub streaming: AtomicBool,
}

impl Control {
    fn new() -> Self {
        Self {
            quit: AtomicBool::new(false),
            suspended: AtomicBool::new(false),
            streaming: AtomicBool::new(false),
        }
    }
}

fn main() {
    // Single instance: bail out silently if another copy is already running.
    if already_running() {
        return;
    }

    let ctrl = Arc::new(Control::new());

    // Background watcher thread.
    let watch_ctrl = ctrl.clone();
    let watcher = std::thread::spawn(move || watch(watch_ctrl));

    // Tray icon + menu + message loop (blocks until Quit).
    tray::run(ctrl.clone());

    // Tray returned => user chose Quit.
    ctrl.quit.store(true, Ordering::SeqCst);
    let _ = watcher.join();
}

/// Named-mutex single-instance guard.
fn already_running() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        // Leak the handle on purpose: it lives for the process lifetime.
        let _ = CreateMutexW(None, true, w!("Global\\GoProCamTraySingleton"));
        GetLastError() == ERROR_ALREADY_EXISTS
    }
}

/// Watcher loop: wait for the GoPro, stream while connected, honour suspend/quit.
fn watch(ctrl: Arc<Control>) {
    let mut cam: Option<ObsVirtualCam> = None;
    let mut vts: u64 = 0;

    while !ctrl.quit.load(Ordering::SeqCst) {
        if ctrl.suspended.load(Ordering::SeqCst) {
            // While suspended, don't stream. Clear the flag once the camera is
            // gone, so a later re-plug resumes automatically.
            if gopro::detect().is_none() {
                ctrl.suspended.store(false, Ordering::SeqCst);
            }
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        match gopro::detect() {
            Some(ip) => {
                stream_once(ip, &ctrl, &mut cam, &mut vts);
            }
            None => std::thread::sleep(Duration::from_secs(2)),
        }
    }
}

enum SessionEnd {
    Disconnected,
    Suspended,
    Quit,
}

/// Stream one connected session; returns on disconnect, suspend, or quit.
fn stream_once(
    ip: Ipv4Addr,
    ctrl: &Arc<Control>,
    cam: &mut Option<ObsVirtualCam>,
    vts: &mut u64,
) -> SessionEnd {
    if gopro::start(ip).is_err() {
        return SessionEnd::Disconnected;
    }
    ctrl.streaming.store(true, Ordering::SeqCst);

    const MAX_QUEUE: usize = 3;
    let queue: Arc<Mutex<VecDeque<Frame>>> = Arc::new(Mutex::new(VecDeque::new()));

    let session = Arc::new(AtomicBool::new(true));
    let w_session = session.clone();
    let w_ctrl = ctrl.clone();
    let recv_queue = queue.clone();
    let worker = std::thread::spawn(move || {
        mf_init();
        let sock = match UdpSocket::bind(("0.0.0.0", STREAM_PORT)) {
            Ok(s) => s,
            Err(_) => return,
        };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
        set_recv_buffer(&sock, 16 * 1024 * 1024);

        let mut demux = mpegts::TsDemux::new();
        let mut decoder = match mf_decode::Decoder::new() {
            Ok(d) => d,
            Err(_) => return,
        };
        let mut frames = Vec::new();
        let mut au_ts: i64 = 0;
        let mut buf = [0u8; 65536];

        while w_session.load(Ordering::SeqCst)
            && !w_ctrl.quit.load(Ordering::SeqCst)
            && !w_ctrl.suspended.load(Ordering::SeqCst)
        {
            match sock.recv_from(&mut buf) {
                Ok((n, _)) => {
                    for au in demux.push(&buf[..n]) {
                        if decoder.decode(&au, au_ts, &mut frames).is_err() {
                            frames.clear();
                        }
                        au_ts += INTERVAL_100NS as i64;
                        if !frames.is_empty() {
                            let mut q = recv_queue.lock().unwrap();
                            for f in frames.drain(..) {
                                q.push_back(f);
                            }
                            while q.len() > MAX_QUEUE {
                                q.pop_front();
                            }
                        }
                    }
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut => {}
                Err(_) => break,
            }
        }
    });

    // Steady 30 Hz consumer with disconnect detection.
    let mut announced = false;
    let period = Duration::from_nanos(INTERVAL_100NS * 100);
    let mut next = Instant::now();
    let mut last_frame = Instant::now();
    const DISCONNECT_AFTER: Duration = Duration::from_secs(3);

    let end = loop {
        if ctrl.quit.load(Ordering::SeqCst) {
            break SessionEnd::Quit;
        }
        if ctrl.suspended.load(Ordering::SeqCst) {
            break SessionEnd::Suspended;
        }
        let frame = queue.lock().unwrap().pop_front();
        if let Some(f) = frame {
            let mut one = vec![f];
            publish_frames(cam, &mut one, vts, &mut announced);
            last_frame = Instant::now();
        } else if last_frame.elapsed() > DISCONNECT_AFTER {
            break SessionEnd::Disconnected;
        }
        next += period;
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    };

    session.store(false, Ordering::SeqCst);
    let _ = worker.join();
    ctrl.streaming.store(false, Ordering::SeqCst);
    // Turn the camera off unless it's already gone.
    if !matches!(end, SessionEnd::Disconnected) {
        gopro::stop(ip);
    }
    end
}

/// Publish decoded frames, creating the virtual camera lazily on the first one.
fn publish_frames(
    cam: &mut Option<ObsVirtualCam>,
    frames: &mut Vec<Frame>,
    timestamp: &mut u64,
    announced: &mut bool,
) {
    for f in frames.drain(..) {
        if cam.is_none() {
            match ObsVirtualCam::create(f.width, f.height, INTERVAL_100NS) {
                Ok(c) => *cam = Some(c),
                Err(_) => return,
            }
        }
        if let Some(c) = cam.as_mut() {
            c.write_nv12(&f.y, &f.uv, *timestamp);
            *timestamp += INTERVAL_100NS;
            *announced = true;
        }
    }
}

/// Media Foundation init (per worker thread).
fn mf_init() {
    use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_LITE};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = MFStartup(0x0002_0070, MFSTARTUP_LITE);
    }
}

/// Grow the UDP socket's OS receive buffer to absorb USB bursts during decode.
fn set_recv_buffer(sock: &UdpSocket, bytes: i32) {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Networking::WinSock::{setsockopt, SOCKET, SOL_SOCKET, SO_RCVBUF};
    unsafe {
        let s = SOCKET(sock.as_raw_socket() as usize);
        let val = bytes.to_ne_bytes();
        let _ = setsockopt(s, SOL_SOCKET, SO_RCVBUF, Some(&val));
    }
}
