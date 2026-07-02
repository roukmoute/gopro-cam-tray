//! GoPro webcam — cross-platform.
//!
//! - Windows: a system-tray app that feeds the OBS Virtual Camera
//!   (UDP MPEG-TS -> demux -> Media Foundation H.264 decoder -> NV12 -> OBS
//!   shared memory), with a live preview window.
//! - Linux: a CLI daemon that drives the GoPro and pipes its stream through
//!   ffmpeg into a v4l2loopback device.
//!
//! The GoPro discovery + control core (`gopro`) is shared across platforms.

#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod gopro;

#[cfg(windows)]
mod mf_decode;
#[cfg(windows)]
mod mpegts;
#[cfg(windows)]
mod obs_vcam;
#[cfg(windows)]
mod startup;
#[cfg(windows)]
mod tray;

#[cfg(target_os = "linux")]
mod linux;

/// UDP port the GoPro streams MPEG-TS to.
const STREAM_PORT: u16 = 8554;

fn main() {
    #[cfg(windows)]
    windows_main();
    #[cfg(target_os = "linux")]
    linux::run();
}

// ===========================================================================
// Windows backend
// ===========================================================================

#[cfg(windows)]
use mf_decode::{Frame, FramePool};
#[cfg(windows)]
use obs_vcam::ObsVirtualCam;
#[cfg(windows)]
use std::collections::VecDeque;
#[cfg(windows)]
use std::net::{Ipv4Addr, UdpSocket};
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(windows)]
use std::sync::{Arc, Mutex};
#[cfg(windows)]
use std::time::{Duration, Instant};

#[cfg(windows)]
const INTERVAL_100NS: u64 = 10_000_000 / 30;

/// One decoded frame kept for the preview window (tightly-packed NV12).
#[cfg(windows)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,
    pub uv: Vec<u8>,
}

/// Shared control state between the tray (GUI) thread and the watcher thread.
#[cfg(windows)]
pub struct Control {
    /// Exit the whole process.
    pub quit: AtomicBool,
    /// Stop streaming; auto-resumes once the camera is unplugged then replugged,
    /// or immediately via the tray "Reprendre" action.
    pub suspended: AtomicBool,
    /// True while a camera session is actively streaming (for the tray status).
    pub streaming: AtomicBool,
    /// True while the preview window is open. Only then does the stream loop
    /// stash a frame, so a closed preview costs nothing.
    pub preview_on: AtomicBool,
    /// Latest frame for the preview window to draw.
    pub preview: Mutex<Option<PreviewFrame>>,
    /// Bumped (under the `preview` lock) whenever a new frame is stashed, so
    /// the preview window can skip re-converting an unchanged frame.
    pub preview_seq: AtomicU64,
}

#[cfg(windows)]
impl Control {
    fn new() -> Self {
        Self {
            quit: AtomicBool::new(false),
            suspended: AtomicBool::new(false),
            streaming: AtomicBool::new(false),
            preview_on: AtomicBool::new(false),
            preview: Mutex::new(None),
            preview_seq: AtomicU64::new(0),
        }
    }
}

#[cfg(windows)]
fn windows_main() {
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
#[cfg(windows)]
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
#[cfg(windows)]
fn watch(ctrl: Arc<Control>) {
    let mut cam: Option<ObsVirtualCam> = None;
    let mut vts: u64 = 0;
    let mut tick = 0u32;

    while !ctrl.quit.load(Ordering::SeqCst) {
        if ctrl.suspended.load(Ordering::SeqCst) {
            // While suspended, don't stream. Clear the flag once the camera is
            // gone, so a later re-plug resumes automatically. Keep the 500 ms
            // sleep for quit responsiveness, but only probe the camera every
            // 2 s — each probe is a full HTTP round-trip to it.
            tick += 1;
            if tick % 4 == 0 && gopro::detect().is_none() {
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

#[cfg(windows)]
enum SessionEnd {
    Disconnected,
    Suspended,
    Quit,
}

/// Stream one connected session; returns on disconnect, suspend, or quit.
#[cfg(windows)]
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
    // Consumed frame buffers go back to the decoder through this pool, making
    // the steady-state pipeline allocation-free.
    let pool = Arc::new(FramePool::default());

    let session = Arc::new(AtomicBool::new(true));
    let w_session = session.clone();
    let w_ctrl = ctrl.clone();
    let recv_queue = queue.clone();
    let w_pool = pool.clone();
    let worker = std::thread::spawn(move || {
        mf_init();
        let sock = match UdpSocket::bind(("0.0.0.0", STREAM_PORT)) {
            Ok(s) => s,
            Err(_) => return,
        };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
        set_recv_buffer(&sock, 16 * 1024 * 1024);

        let mut demux = mpegts::TsDemux::new();
        let mut decoder = match mf_decode::Decoder::new(w_pool.clone()) {
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
                            // Drop the stalest frames, recycling their buffers.
                            while q.len() > MAX_QUEUE {
                                if let Some(f) = q.pop_front() {
                                    w_pool.put(f.y, f.uv);
                                }
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
            publish_one(cam, &f, vts, &mut announced);
            // Hand the frame's buffers to the preview window (no copy) while
            // it's open, otherwise recycle them straight into the pool.
            let Frame { width, height, y, uv } = f;
            if ctrl.preview_on.load(Ordering::Relaxed) {
                let old = {
                    let mut slot = ctrl.preview.lock().unwrap();
                    ctrl.preview_seq.fetch_add(1, Ordering::Release);
                    slot.replace(PreviewFrame { width, height, y, uv })
                };
                // The displaced frame's buffers drop back into the pool,
                // outside the preview lock.
                if let Some(o) = old {
                    pool.put(o.y, o.uv);
                }
            } else {
                pool.put(y, uv);
            }
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

/// Publish one decoded frame, creating the virtual camera lazily on the first.
#[cfg(windows)]
fn publish_one(
    cam: &mut Option<ObsVirtualCam>,
    f: &Frame,
    timestamp: &mut u64,
    announced: &mut bool,
) {
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

/// Media Foundation init (per worker thread).
#[cfg(windows)]
fn mf_init() {
    use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_LITE};
    use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = MFStartup(0x0002_0070, MFSTARTUP_LITE);
    }
}

/// Grow the UDP socket's OS receive buffer to absorb USB bursts during decode.
#[cfg(windows)]
fn set_recv_buffer(sock: &UdpSocket, bytes: i32) {
    use std::os::windows::io::AsRawSocket;
    use windows::Win32::Networking::WinSock::{setsockopt, SOCKET, SOL_SOCKET, SO_RCVBUF};
    unsafe {
        let s = SOCKET(sock.as_raw_socket() as usize);
        let val = bytes.to_ne_bytes();
        let _ = setsockopt(s, SOL_SOCKET, SO_RCVBUF, Some(&val));
    }
}
