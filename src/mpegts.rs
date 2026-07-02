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
            // Stack copy: ends the borrow of self.buf without a heap alloc per
            // packet (~10,000/s at stream bitrate).
            let mut pkt = [0u8; TS_PACKET];
            pkt.copy_from_slice(&self.buf[i..i + TS_PACKET]);
            self.handle_packet(&pkt, &mut out);
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

#[cfg(test)]
mod tests {
    use super::*;

    // Build a 188-byte TS packet (payload only, no adaptation field). Short
    // payloads are padded with 0xFF, exactly like real stuffing.
    fn ts(pid: u16, pusi: bool, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0xFFu8; TS_PACKET];
        p[0] = SYNC;
        p[1] = (if pusi { 0x40 } else { 0 }) | ((pid >> 8) as u8 & 0x1F);
        p[2] = (pid & 0xFF) as u8;
        p[3] = 0x10; // adaptation_field_control = 01 (payload only)
        let n = payload.len().min(TS_PACKET - 4);
        p[4..4 + n].copy_from_slice(&payload[..n]);
        p
    }

    // PAT with a single program pointing at `pmt_pid`.
    fn pat(pmt_pid: u16) -> Vec<u8> {
        let mut v = vec![0x00u8]; // pointer_field
        v.extend_from_slice(&[
            0x00, // table_id (PAT)
            0xB0, 0x0D, // syntax=1, section_length=13
            0x00, 0x01, // transport_stream_id
            0xC1, // version/current_next
            0x00, 0x00, // section#, last#
            0x00, 0x01, // program_number = 1
            0xE0 | ((pmt_pid >> 8) as u8 & 0x1F),
            (pmt_pid & 0xFF) as u8, // reserved + PMT PID
            0xDE, 0xAD, 0xBE, 0xEF, // CRC (unchecked by the demuxer)
        ]);
        v
    }

    // PMT declaring one H.264 (stream_type 0x1B) stream on `video_pid`.
    fn pmt(video_pid: u16) -> Vec<u8> {
        let mut v = vec![0x00u8]; // pointer_field
        v.extend_from_slice(&[
            0x02, // table_id (PMT)
            0xB0, 0x12, // syntax=1, section_length=18
            0x00, 0x01, // program_number
            0xC1, // version/current_next
            0x00, 0x00, // section#, last#
            0xE0, 0x00, // reserved + PCR_PID
            0xF0, 0x00, // reserved + program_info_length = 0
            0x1B, // stream_type = H.264
            0xE0 | ((video_pid >> 8) as u8 & 0x1F),
            (video_pid & 0xFF) as u8, // elementary PID
            0xF0, 0x00, // reserved + ES_info_length = 0
            0xDE, 0xAD, 0xBE, 0xEF, // CRC (unchecked)
        ]);
        v
    }

    // Video PES payload (9-byte header + elementary-stream bytes).
    fn pes(es: &[u8]) -> Vec<u8> {
        let mut v = vec![0x00, 0x00, 0x01, 0xE0, 0x00, 0x00, 0x80, 0x00, 0x00];
        v.extend_from_slice(es);
        v
    }

    const PMT_PID: u16 = 0x0100;
    const VID_PID: u16 = 0x0101;

    #[test]
    fn extracts_access_unit_through_pat_pmt_pes() {
        let mut d = TsDemux::new();
        let es1 = [0u8, 0, 0, 1, 0x09, 0x10];
        let es2 = [0u8, 0, 0, 1, 0x67, 0x42];

        let mut stream = Vec::new();
        stream.extend(ts(0, true, &pat(PMT_PID)));
        stream.extend(ts(PMT_PID, true, &pmt(VID_PID)));
        stream.extend(ts(VID_PID, true, &pes(&es1)));
        // A second PES (new PUSI) flushes the first access unit.
        stream.extend(ts(VID_PID, true, &pes(&es2)));

        let aus = d.push(&stream);
        assert_eq!(aus.len(), 1, "exactly one AU should be flushed");
        assert!(aus[0].starts_with(&es1), "AU should begin with the first PES payload");
        assert_eq!(&aus[0][..4], &[0, 0, 0, 1], "AU should start with an Annex-B start code");
    }

    #[test]
    fn no_output_until_the_next_pusi() {
        let mut d = TsDemux::new();
        let mut stream = Vec::new();
        stream.extend(ts(0, true, &pat(PMT_PID)));
        stream.extend(ts(PMT_PID, true, &pmt(VID_PID)));
        stream.extend(ts(VID_PID, true, &pes(&[0, 0, 0, 1, 0x67])));
        // Only one PES so far: it is still buffered, nothing flushed yet.
        assert!(d.push(&stream).is_empty());
    }

    #[test]
    fn reassembles_a_pes_split_across_packets() {
        let mut d = TsDemux::new();
        // Exact-size payloads (no stuffing) so we can assert byte-for-byte.
        let head = vec![0xAAu8; TS_PACKET - 4 - 9]; // fills packet 1 exactly
        let tail = vec![0xBBu8; TS_PACKET - 4]; // fills packet 2 exactly

        let mut stream = Vec::new();
        stream.extend(ts(0, true, &pat(PMT_PID)));
        stream.extend(ts(PMT_PID, true, &pmt(VID_PID)));
        stream.extend(ts(VID_PID, true, &pes(&head))); // starts the AU
        stream.extend(ts(VID_PID, false, &tail)); // continuation
        stream.extend(ts(VID_PID, true, &pes(&[0xCC]))); // flush

        let aus = d.push(&stream);
        assert_eq!(aus.len(), 1);
        let mut expected = head.clone();
        expected.extend_from_slice(&tail);
        assert_eq!(aus[0], expected);
    }

    #[test]
    fn resyncs_after_leading_garbage() {
        let mut d = TsDemux::new();
        let es = [0u8, 0, 0, 1, 0x65, 0x88];

        let mut stream = vec![0x00, 0x11, 0x22]; // junk before the first sync byte
        stream.extend(ts(0, true, &pat(PMT_PID)));
        stream.extend(ts(PMT_PID, true, &pmt(VID_PID)));
        stream.extend(ts(VID_PID, true, &pes(&es)));
        stream.extend(ts(VID_PID, true, &pes(&[0, 0, 0, 1, 0x41])));

        let aus = d.push(&stream);
        assert_eq!(aus.len(), 1);
        assert!(aus[0].starts_with(&es));
    }
}
