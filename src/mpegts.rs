//! Minimal streaming MPEG-TS demuxer: takes raw TS bytes (as they arrive over
//! UDP or from a file) and yields H.264 access units (Annex-B elementary stream).
//!
//! Scope is deliberately narrow — just enough for the GoPro webcam stream:
//!   PAT (PID 0) -> PMT -> first stream of type 0x1B (H.264) -> its PES payloads.
//! Each PES (delimited by payload_unit_start_indicator) is emitted as one access
//! unit, ready to hand to the Media Foundation H.264 decoder.

const TS_PACKET: usize = 188;
const SYNC: u8 = 0x47;
const STREAM_TYPE_H264: u8 = 0x1B;

pub struct TsDemux {
    pmt_pid: Option<u16>,
    video_pid: Option<u16>,
    cur_au: Vec<u8>,
    buf: Vec<u8>,
}

impl TsDemux {
    pub fn new() -> Self {
        Self {
            pmt_pid: None,
            video_pid: None,
            cur_au: Vec::with_capacity(256 * 1024),
            buf: Vec::with_capacity(2 * TS_PACKET),
        }
    }

    /// Feed arbitrary bytes; returns any complete access units produced.
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();

        let mut i = 0;
        while i + TS_PACKET <= self.buf.len() {
            if self.buf[i] != SYNC {
                // Resync: hunt for the next sync byte.
                i += 1;
                continue;
            }
            let pkt = &self.buf[i..i + TS_PACKET].to_vec();
            self.handle_packet(pkt, &mut out);
            i += TS_PACKET;
        }
        // Keep the unconsumed tail.
        self.buf.drain(..i);
        out
    }

    fn handle_packet(&mut self, pkt: &[u8], out: &mut Vec<Vec<u8>>) {
        let pusi = (pkt[1] & 0x40) != 0;
        let pid = (((pkt[1] & 0x1F) as u16) << 8) | pkt[2] as u16;
        let afc = (pkt[3] >> 4) & 0x3;

        // Compute payload start, skipping any adaptation field.
        let mut off = 4usize;
        if afc & 0x2 != 0 {
            if off >= pkt.len() {
                return;
            }
            let af_len = pkt[off] as usize;
            off += 1 + af_len;
        }
        if afc & 0x1 == 0 || off >= pkt.len() {
            return; // no payload
        }
        let payload = &pkt[off..];

        if pid == 0 {
            self.parse_pat(payload, pusi);
        } else if Some(pid) == self.pmt_pid {
            self.parse_pmt(payload, pusi);
        } else if Some(pid) == self.video_pid {
            self.handle_video(payload, pusi, out);
        }
    }

    fn parse_pat(&mut self, payload: &[u8], pusi: bool) {
        if self.pmt_pid.is_some() {
            return;
        }
        let mut p = payload;
        if pusi {
            // Skip pointer_field.
            let ptr = p[0] as usize;
            if 1 + ptr >= p.len() {
                return;
            }
            p = &p[1 + ptr..];
        }
        // table_id(1) section_length(2) tsid(2) ver(1) sec#(1) last#(1) => 8 bytes
        if p.len() < 12 {
            return;
        }
        let section_length = (((p[1] & 0x0F) as usize) << 8) | p[2] as usize;
        let end = (3 + section_length).min(p.len());
        // Program entries start at byte 8, run until end-4 (CRC).
        let mut i = 8;
        while i + 4 <= end.saturating_sub(4) {
            let program_number = ((p[i] as u16) << 8) | p[i + 1] as u16;
            let pid = (((p[i + 2] & 0x1F) as u16) << 8) | p[i + 3] as u16;
            if program_number != 0 {
                self.pmt_pid = Some(pid);
                return;
            }
            i += 4;
        }
    }

    fn parse_pmt(&mut self, payload: &[u8], pusi: bool) {
        if self.video_pid.is_some() {
            return;
        }
        let mut p = payload;
        if pusi {
            let ptr = p[0] as usize;
            if 1 + ptr >= p.len() {
                return;
            }
            p = &p[1 + ptr..];
        }
        if p.len() < 12 {
            return;
        }
        let section_length = (((p[1] & 0x0F) as usize) << 8) | p[2] as usize;
        let end = (3 + section_length).min(p.len());
        let program_info_length = (((p[10] & 0x0F) as usize) << 8) | p[11] as usize;
        let mut i = 12 + program_info_length;
        while i + 5 <= end.saturating_sub(4) {
            let stream_type = p[i];
            let elem_pid = (((p[i + 1] & 0x1F) as u16) << 8) | p[i + 2] as u16;
            let es_info_len = (((p[i + 3] & 0x0F) as usize) << 8) | p[i + 4] as usize;
            if stream_type == STREAM_TYPE_H264 {
                self.video_pid = Some(elem_pid);
                return;
            }
            i += 5 + es_info_len;
        }
    }

    fn handle_video(&mut self, payload: &[u8], pusi: bool, out: &mut Vec<Vec<u8>>) {
        if pusi {
            // A new PES begins → the accumulated one is a finished access unit.
            if !self.cur_au.is_empty() {
                out.push(std::mem::take(&mut self.cur_au));
            }
            // Strip the PES header, keep the elementary-stream payload.
            if let Some(es) = strip_pes_header(payload) {
                self.cur_au.extend_from_slice(es);
            }
        } else {
            self.cur_au.extend_from_slice(payload);
        }
    }
}

/// Given a PES packet payload, return the elementary-stream bytes after the PES
/// header, or None if it doesn't look like a video PES.
fn strip_pes_header(p: &[u8]) -> Option<&[u8]> {
    // packet_start_code_prefix 00 00 01, then stream_id.
    if p.len() < 9 || p[0] != 0x00 || p[1] != 0x00 || p[2] != 0x01 {
        return None;
    }
    // p[3] = stream_id (0xE0..0xEF for video). p[4..6] = PES_packet_length.
    // p[8] = PES_header_data_length; ES starts right after it.
    let header_data_len = p[8] as usize;
    let es_start = 9 + header_data_len;
    if es_start >= p.len() {
        return None;
    }
    Some(&p[es_start..])
}
