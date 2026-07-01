//! Writer for the OBS Virtual Camera shared-memory queue.
//!
//! This reproduces, byte-for-byte, the layout OBS uses in
//! `plugins/win-dshow/shared-memory-queue.c` so that the OBS virtual-camera
//! DShow/MediaFoundation filter (already installed on the machine) reads our
//! frames — WITHOUT OBS itself running. We play the role OBS normally plays:
//! the *writer* that creates the `OBSVirtualCamVideo` file mapping.
//!
//! Frame format is NV12: a Y plane of `cx*cy` bytes (stride == cx) immediately
//! followed by an interleaved UV plane of `cx*cy/2` bytes.

use std::ffi::c_void;
use std::ptr::{copy_nonoverlapping, read_volatile, write_volatile};

// --- Minimal kernel32 FFI (no external crates → tiny binary) --------------

type Handle = isize;
const INVALID_HANDLE_VALUE: Handle = -1;
const PAGE_READWRITE: u32 = 0x04;
const FILE_MAP_ALL_ACCESS: u32 = 0xF001F;
const FILE_MAP_READ: u32 = 0x0004;

#[link(name = "kernel32")]
extern "system" {
    fn CreateFileMappingW(
        h_file: Handle,
        lp_attributes: *const c_void,
        fl_protect: u32,
        dw_maximum_size_high: u32,
        dw_maximum_size_low: u32,
        lp_name: *const u16,
    ) -> Handle;
    fn OpenFileMappingW(dw_desired_access: u32, b_inherit: i32, lp_name: *const u16) -> Handle;
    fn MapViewOfFile(
        h_map: Handle,
        dw_desired_access: u32,
        dw_offset_high: u32,
        dw_offset_low: u32,
        dw_bytes_to_map: usize,
    ) -> *mut c_void;
    fn UnmapViewOfFile(lp_base: *const c_void) -> i32;
    fn CloseHandle(h: Handle) -> i32;
}

// --- OBS queue layout (matches shared-memory-queue.c exactly) --------------

const FRAME_HEADER_SIZE: usize = 32;
const FRAME_COUNT: usize = 3;

const STATE_STARTING: u32 = 1;
const STATE_READY: u32 = 2;
const STATE_STOPPING: u32 = 3;

/// Exact mirror of OBS `struct queue_header`. `#[repr(C)]` reproduces the same
/// padding (the u64 forces 8-byte alignment before `interval`), giving 80 bytes.
#[repr(C)]
struct QueueHeader {
    write_idx: u32,
    read_idx: u32,
    state: u32,
    offsets: [u32; FRAME_COUNT],
    kind: u32, // SHARED_QUEUE_TYPE_VIDEO = 0
    cx: u32,
    cy: u32,
    interval: u64,
    reserved: [u32; 8],
}

fn align32(n: usize) -> usize {
    (n + 31) & !31
}

/// Wide, null-terminated name of the mapping OBS looks for.
fn video_name() -> Vec<u16> {
    "OBSVirtualCamVideo\0".encode_utf16().collect()
}

pub struct ObsVirtualCam {
    handle: Handle,
    base: *mut u8,
    offsets: [usize; FRAME_COUNT],
    cx: u32,
    cy: u32,
    y_size: usize,
}

impl ObsVirtualCam {
    /// Create the shared queue as the writer. `interval` is the frame interval
    /// in 100-ns units (e.g. 333_333 for 30 fps).
    pub fn create(cx: u32, cy: u32, interval: u64) -> Result<Self, String> {
        let frame_size = (cx as usize) * (cy as usize) * 3 / 2;

        // Compute frame offsets exactly like OBS does.
        let mut offsets = [0usize; FRAME_COUNT];
        let mut size = align32(std::mem::size_of::<QueueHeader>());
        for slot in offsets.iter_mut() {
            *slot = size;
            size = align32(size + frame_size + FRAME_HEADER_SIZE);
        }
        let total = size;

        let name = video_name();

        unsafe {
            // Fail if the mapping already exists (OBS running, or a leftover
            // instance) — matches OBS behaviour and avoids fighting over it.
            let existing = OpenFileMappingW(FILE_MAP_READ, 0, name.as_ptr());
            if existing != 0 {
                CloseHandle(existing);
                return Err(
                    "OBSVirtualCamVideo already in use — is OBS (or another \
                     instance) already running the virtual camera?"
                        .into(),
                );
            }

            let handle = CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                std::ptr::null(),
                PAGE_READWRITE,
                0,
                total as u32,
                name.as_ptr(),
            );
            if handle == 0 {
                return Err("CreateFileMappingW failed (is the OBS virtual camera installed?)".into());
            }

            let base = MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, 0) as *mut u8;
            if base.is_null() {
                CloseHandle(handle);
                return Err("MapViewOfFile failed".into());
            }

            // Initialise the header.
            let hdr = base as *mut QueueHeader;
            write_volatile(
                hdr,
                QueueHeader {
                    write_idx: 0,
                    read_idx: 0,
                    state: STATE_STARTING,
                    offsets: [offsets[0] as u32, offsets[1] as u32, offsets[2] as u32],
                    kind: 0,
                    cx,
                    cy,
                    interval,
                    reserved: [0; 8],
                },
            );

            Ok(Self {
                handle,
                base,
                offsets,
                cx,
                cy,
                y_size: (cx as usize) * (cy as usize),
            })
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.cx, self.cy)
    }

    /// Publish one NV12 frame. `y` must be `cx*cy` bytes (stride == cx) and `uv`
    /// must be `cx*cy/2` bytes. `timestamp` is in 100-ns units.
    pub fn write_nv12(&mut self, y: &[u8], uv: &[u8], timestamp: u64) {
        debug_assert_eq!(y.len(), self.y_size);
        debug_assert_eq!(uv.len(), self.y_size / 2);

        unsafe {
            let hdr = self.base as *mut QueueHeader;
            let inc = read_volatile(&(*hdr).write_idx).wrapping_add(1);
            write_volatile(&mut (*hdr).write_idx, inc);

            let idx = (inc % FRAME_COUNT as u32) as usize;
            let off = self.offsets[idx];

            let ts_ptr = self.base.add(off) as *mut u64;
            *ts_ptr = timestamp;

            let frame_ptr = self.base.add(off + FRAME_HEADER_SIZE);
            copy_nonoverlapping(y.as_ptr(), frame_ptr, self.y_size);
            copy_nonoverlapping(uv.as_ptr(), frame_ptr.add(self.y_size), self.y_size / 2);

            write_volatile(&mut (*hdr).read_idx, inc);
            write_volatile(&mut (*hdr).state, STATE_READY);
        }
    }
}

impl Drop for ObsVirtualCam {
    fn drop(&mut self) {
        unsafe {
            let hdr = self.base as *mut QueueHeader;
            write_volatile(&mut (*hdr).state, STATE_STOPPING);
            UnmapViewOfFile(self.base as *const c_void);
            CloseHandle(self.handle);
        }
    }
}
