//! H.264 -> NV12 decoding via the Media Foundation H.264 decoder MFT, which is
//! built into Windows (no bundled codec, so the binary stays tiny). We feed it
//! Annex-B access units and get back tightly-packed NV12 frames (stride removed).

use std::mem::ManuallyDrop;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

// HRESULTs returned by the MFT control flow (as plain i32 for comparison).
const E_NEED_MORE_INPUT: i32 = 0xC00D6D72u32 as i32;
const E_STREAM_CHANGE: i32 = 0xC00D6D61u32 as i32;
const E_NOTACCEPTING: i32 = 0xC00D36B5u32 as i32;

pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub y: Vec<u8>,  // width*height
    pub uv: Vec<u8>, // width*height/2 (interleaved)
}

pub struct Decoder {
    transform: IMFTransform,
    width: u32,  // coded width
    height: u32, // coded height
    stride: u32,
    // Exact visible rectangle (removes codec padding, e.g. 1088 -> 1080).
    crop_x: usize,
    crop_y: usize,
    disp_w: u32,
    disp_h: u32,
}

impl Decoder {
    /// Caller must have called CoInitializeEx + MFStartup beforehand.
    pub fn new() -> windows::core::Result<Self> {
        unsafe {
            let transform: IMFTransform =
                CoCreateInstance(&CLSID_MSH264DecoderMFT, None, CLSCTX_INPROC_SERVER)?;

            // Low-latency mode: tell the decoder to emit each frame as soon as
            // it's ready instead of buffering for B-frame reordering. This is the
            // main fix for the ~1s pipeline latency.
            if let Ok(attrs) = transform.GetAttributes() {
                let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            }

            // Input: H.264.
            let input = MFCreateMediaType()?;
            input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            input.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            transform.SetInputType(0, &input, 0)?;

            // Output: first available NV12 type (frame size filled in later via
            // the STREAM_CHANGE that fires once the decoder sees the SPS).
            let mut dec = Self {
                transform,
                width: 0,
                height: 0,
                stride: 0,
                crop_x: 0,
                crop_y: 0,
                disp_w: 0,
                disp_h: 0,
            };
            dec.select_nv12_output()?;

            dec.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            dec.transform
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(dec)
        }
    }

    fn select_nv12_output(&mut self) -> windows::core::Result<()> {
        unsafe {
            let mut i = 0u32;
            loop {
                let t = self.transform.GetOutputAvailableType(0, i)?;
                if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    self.transform.SetOutputType(0, &t, 0)?;
                    if let Ok(size) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
                        self.width = (size >> 32) as u32;
                        self.height = (size & 0xFFFF_FFFF) as u32;
                    }
                    self.stride = t.GetUINT32(&MF_MT_DEFAULT_STRIDE).unwrap_or(self.width);

                    // Default: full coded frame. Refine with the display aperture
                    // if present (gives the exact visible rect, stripping padding).
                    self.crop_x = 0;
                    self.crop_y = 0;
                    self.disp_w = self.width;
                    self.disp_h = self.height;
                    let mut blob = [0u8; 16];
                    if t
                        .GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, &mut blob, None)
                        .is_ok()
                    {
                        let ox = i16::from_le_bytes([blob[2], blob[3]]) as i32;
                        let oy = i16::from_le_bytes([blob[6], blob[7]]) as i32;
                        let cx = i32::from_le_bytes([blob[8], blob[9], blob[10], blob[11]]);
                        let cy = i32::from_le_bytes([blob[12], blob[13], blob[14], blob[15]]);
                        if cx > 0 && cy > 0 {
                            self.crop_x = ox.max(0) as usize & !1; // keep chroma alignment
                            self.crop_y = oy.max(0) as usize & !1;
                            self.disp_w = cx as u32;
                            self.disp_h = cy as u32;
                        }
                    }
                    return Ok(());
                }
                i += 1;
            }
        }
    }

    /// Feed one access unit; push any decoded frames into `out`.
    pub fn decode(
        &mut self,
        au: &[u8],
        time_100ns: i64,
        out: &mut Vec<Frame>,
    ) -> windows::core::Result<()> {
        unsafe {
            let sample = MFCreateSample()?;
            let buffer = MFCreateMemoryBuffer(au.len() as u32)?;
            let mut ptr: *mut u8 = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(au.as_ptr(), ptr, au.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(au.len() as u32)?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time_100ns)?;

            loop {
                match self.transform.ProcessInput(0, &sample, 0) {
                    Ok(()) => break,
                    Err(e) if e.code().0 == E_NOTACCEPTING => {
                        self.drain(out)?;
                    }
                    Err(e) => return Err(e),
                }
            }
            self.drain(out)?;
            Ok(())
        }
    }

    fn drain(&mut self, out: &mut Vec<Frame>) -> windows::core::Result<()> {
        unsafe {
            loop {
                let info = self.transform.GetOutputStreamInfo(0)?;
                let out_sample = MFCreateSample()?;
                let out_buf = MFCreateMemoryBuffer(info.cbSize)?;
                out_sample.AddBuffer(&out_buf)?;

                let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(Some(out_sample)),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                let res = self.transform.ProcessOutput(0, &mut buffers, &mut status);
                let produced = buffers[0].pSample.take();

                match res {
                    Ok(()) => {
                        if let Some(s) = produced {
                            if let Some(f) = self.extract(&s)? {
                                out.push(f);
                            }
                        }
                    }
                    Err(e) if e.code().0 == E_NEED_MORE_INPUT => return Ok(()),
                    Err(e) if e.code().0 == E_STREAM_CHANGE => {
                        self.select_nv12_output()?;
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            }
        }
    }

    fn extract(&self, sample: &IMFSample) -> windows::core::Result<Option<Frame>> {
        if self.stride == 0 || self.disp_w == 0 || self.disp_h == 0 {
            return Ok(None);
        }
        unsafe {
            let cbuf = sample.ConvertToContiguousBuffer()?;
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut cur: u32 = 0;
            cbuf.Lock(&mut ptr, None, Some(&mut cur))?;

            let stride = self.stride as usize;
            // coded height from the contiguous length: total = stride*coded_h*3/2
            let coded_h = (cur as usize) * 2 / (3 * stride);
            let uv_off = stride * coded_h;

            // Copy only the visible rectangle, tightly packed (stride == disp_w).
            let dw = self.disp_w as usize;
            let dh = self.disp_h as usize;

            let mut y = vec![0u8; dw * dh];
            for row in 0..dh {
                let src = ptr.add((self.crop_y + row) * stride + self.crop_x);
                std::ptr::copy_nonoverlapping(src, y.as_mut_ptr().add(row * dw), dw);
            }

            let mut uv = vec![0u8; dw * dh / 2];
            for row in 0..(dh / 2) {
                let src = ptr.add(uv_off + (self.crop_y / 2 + row) * stride + self.crop_x);
                std::ptr::copy_nonoverlapping(src, uv.as_mut_ptr().add(row * dw), dw);
            }

            cbuf.Unlock()?;
            Ok(Some(Frame {
                width: self.disp_w,
                height: self.disp_h,
                y,
                uv,
            }))
        }
    }
}
