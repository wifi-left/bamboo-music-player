//! Minimal MPEG-TS audio demultiplexer for HLS segments.
//!
//! HLS segments are frequently MPEG-TS. Symphonia has no TS demuxer, but for
//! radio playback we only need the audio elementary stream: parse PAT/PMT,
//! pick the first AAC (ADTS) or MPEG-audio stream, strip PES/ID3 framing and
//! append the payload to a continuous elementary stream that symphonia's ADTS
//! or MP3 reader can consume.

const PACKET_LEN: usize = 188;

const STREAM_MPEG1_AUDIO: u8 = 0x03;
const STREAM_MPEG2_AUDIO: u8 = 0x04;
const STREAM_ADTS_AAC: u8 = 0x0F;
const STREAM_MPEG4_AUDIO: u8 = 0x11;
const STREAM_AC3: u8 = 0x81;
const STREAM_EAC3: u8 = 0x87;

#[derive(Default)]
pub struct TsDemuxer {
    pmt_pid: Option<u16>,
    audio_pid: Option<u16>,
    stream_type: Option<u8>,
    /// Audio codec found in the PMT that symphonia cannot decode here.
    unsupported: Option<&'static str>,
    /// Whether a PMT section has been parsed at least once.
    pmt_seen: bool,
    /// Partial TS packet carried between feed calls.
    pending: Vec<u8>,
    /// PSI sections reassembled across TS packets.
    pat_section: Vec<u8>,
    pmt_section: Vec<u8>,
    /// PES payload bytes still expected in the current PES packet.
    pes_remaining: Option<usize>,
    last_cc: Option<u8>,
}

impl TsDemuxer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn audio_stream_type(&self) -> Option<u8> {
        self.stream_type
    }

    /// Name of the first audio codec in the PMT that this demuxer cannot
    /// hand to symphonia (AC-3, E-AC-3, MPEG-4 LATM).
    pub fn unsupported_codec(&self) -> Option<&'static str> {
        self.unsupported
    }

    /// Whether a program map table has been parsed. Until then a missing audio
    /// PID means "not yet known", not "stream has no audio".
    pub fn has_program_map(&self) -> bool {
        self.pmt_seen
    }

    /// Feed one downloaded segment (or arbitrary bytes) and append any audio
    /// elementary-stream bytes to `out`.
    pub fn feed(&mut self, data: &[u8], out: &mut Vec<u8>) {
        self.pending.extend_from_slice(data);
        let mut offset = 0;
        while offset + PACKET_LEN <= self.pending.len() {
            if self.pending[offset] != 0x47 {
                // Resynchronize on the next sync byte.
                let Some(position) = self.pending[offset..].iter().position(|byte| *byte == 0x47)
                else {
                    self.pending.clear();
                    return;
                };
                offset += position;
                if offset + PACKET_LEN > self.pending.len() {
                    break;
                }
            }
            let mut packet = [0u8; PACKET_LEN];
            packet.copy_from_slice(&self.pending[offset..offset + PACKET_LEN]);
            self.handle_packet(&packet, out);
            offset += PACKET_LEN;
        }
        if offset > 0 {
            self.pending.drain(..offset);
        }
    }

    fn handle_packet(&mut self, packet: &[u8; PACKET_LEN], out: &mut Vec<u8>) {
        // transport_error_indicator: the packet payload is corrupt.
        if packet[1] & 0x80 != 0 {
            return;
        }
        let pusi = packet[1] & 0x40 != 0;
        let pid = (((packet[1] & 0x1F) as u16) << 8) | packet[2] as u16;
        let afc = (packet[3] >> 4) & 0x03;
        let cc = packet[3] & 0x0F;

        let mut payload_start = 4;
        if afc & 0x02 != 0 {
            let af_len = packet[4] as usize;
            payload_start = 5 + af_len;
            if payload_start > PACKET_LEN {
                return;
            }
        }
        if afc & 0x01 == 0 || payload_start >= PACKET_LEN {
            return;
        }
        let payload = &packet[payload_start..];

        if pid == 0 {
            let mut sections = Vec::new();
            push_psi(&mut self.pat_section, payload, pusi, |section| {
                sections.push(section.to_vec());
            });
            for section in &sections {
                self.parse_pat(section);
            }
            return;
        }
        if Some(pid) == self.pmt_pid {
            let mut sections = Vec::new();
            push_psi(&mut self.pmt_section, payload, pusi, |section| {
                sections.push(section.to_vec());
            });
            for section in &sections {
                self.parse_pmt(section);
            }
            return;
        }
        if Some(pid) != self.audio_pid {
            return;
        }

        // Duplicate packet (same continuity counter without a new PES start).
        if !pusi && self.last_cc == Some(cc) {
            return;
        }
        let discontinuity = self.last_cc.is_some_and(|last| cc != (last + 1) & 0x0F);
        if discontinuity && !pusi {
            // Lost a packet: drop the rest of the current PES so partial
            // frames are not spliced into the output stream.
            self.pes_remaining = None;
        }
        self.last_cc = Some(cc);

        if pusi {
            self.handle_pes_start(payload, out);
        } else if let Some(remaining) = self.pes_remaining {
            let take = remaining.min(payload.len());
            out.extend_from_slice(&payload[..take]);
            self.pes_remaining = Some(remaining - take);
        } else {
            // No PES length (unbounded) or a stream without header framing.
            out.extend_from_slice(strip_id3(payload));
        }
    }

    fn handle_pes_start(&mut self, payload: &[u8], out: &mut Vec<u8>) {
        if payload.len() >= 9 && payload[..3] == [0x00, 0x00, 0x01] {
            let pes_len = u16::from_be_bytes([payload[4], payload[5]]) as usize;
            let header_len = payload[8] as usize;
            let data_start = 9 + header_len;
            if data_start > payload.len() {
                return;
            }
            let expected = pes_len.saturating_sub(3 + header_len);
            let available = payload.len() - data_start;
            let take = if pes_len == 0 {
                available
            } else {
                expected.min(available)
            };
            let data = strip_id3(&payload[data_start..data_start + take]);
            out.extend_from_slice(data);
            self.pes_remaining = if pes_len == 0 {
                None
            } else {
                Some(expected.saturating_sub(take))
            };
        } else {
            // Some streams put raw frames directly in the TS payload.
            out.extend_from_slice(strip_id3(payload));
            self.pes_remaining = None;
        }
    }

    fn parse_pat(&mut self, section: &[u8]) {
        if section.first() != Some(&0x00) || section.len() < 8 {
            return;
        }
        let mut offset = 8;
        let end = section.len().saturating_sub(4);
        while offset + 4 <= end {
            let program = u16::from_be_bytes([section[offset], section[offset + 1]]);
            let pid = u16::from_be_bytes([section[offset + 2], section[offset + 3]]) & 0x1FFF;
            offset += 4;
            if program != 0 {
                if self.pmt_pid != Some(pid) {
                    self.pmt_pid = Some(pid);
                    self.audio_pid = None;
                    self.stream_type = None;
                    self.unsupported = None;
                    self.pmt_seen = false;
                    self.pmt_section.clear();
                }
                return;
            }
        }
    }

    fn parse_pmt(&mut self, section: &[u8]) {
        if section.first() != Some(&0x02) || section.len() < 12 {
            return;
        }
        self.pmt_seen = true;
        let program_info_len = (u16::from_be_bytes([section[10], section[11]]) & 0x0FFF) as usize;
        let mut offset = 12 + program_info_len;
        let end = section.len().saturating_sub(4);
        let mut audio = None;
        let mut unsupported = None;
        while offset + 5 <= end {
            let stream_type = section[offset];
            let pid = u16::from_be_bytes([section[offset + 1], section[offset + 2]]) & 0x1FFF;
            let es_len =
                (u16::from_be_bytes([section[offset + 3], section[offset + 4]]) & 0x0FFF) as usize;
            if matches!(
                stream_type,
                STREAM_ADTS_AAC | STREAM_MPEG1_AUDIO | STREAM_MPEG2_AUDIO
            ) {
                // Prefer the first decodable audio stream even when the PMT
                // lists an unsupported codec first.
                if audio.is_none() {
                    audio = Some((stream_type, pid));
                }
            } else if unsupported.is_none()
                && let Some(name) = unsupported_codec_name(stream_type)
            {
                unsupported = Some(name);
            }
            offset += 5 + es_len;
        }
        match audio {
            Some((stream_type, pid)) => {
                if self.audio_pid != Some(pid) || self.stream_type != Some(stream_type) {
                    self.pes_remaining = None;
                    self.last_cc = None;
                }
                self.audio_pid = Some(pid);
                self.stream_type = Some(stream_type);
                self.unsupported = None;
            }
            None => {
                if self.audio_pid.is_some() {
                    self.pes_remaining = None;
                    self.last_cc = None;
                }
                self.audio_pid = None;
                self.stream_type = None;
                self.unsupported = unsupported;
            }
        }
    }
}

fn unsupported_codec_name(stream_type: u8) -> Option<&'static str> {
    match stream_type {
        STREAM_MPEG4_AUDIO => Some("MPEG-4 LATM/AAC"),
        STREAM_AC3 => Some("AC-3"),
        STREAM_EAC3 => Some("E-AC-3"),
        _ => None,
    }
}

/// Feed a PSI payload into a section assembler.
///
/// When `pusi` is set, the first byte is the `pointer_field`: the bytes before
/// the next section belong to the previous section. Otherwise the whole
/// payload continues the section already in progress. PAT/PMT normally fit in
/// one TS packet, but large PMTs and some multiplexers split them, so keeping
/// the remainder is required rather than optional.
fn push_psi(buffer: &mut Vec<u8>, payload: &[u8], pusi: bool, mut on_section: impl FnMut(&[u8])) {
    let mut data = payload;
    if pusi {
        let pointer = data.first().copied().unwrap_or(0) as usize;
        let pointer = pointer.min(data.len().saturating_sub(1));
        if !buffer.is_empty() {
            buffer.extend_from_slice(&data[1..1 + pointer]);
            drain_sections(buffer, &mut on_section);
            if !buffer.is_empty() {
                // The previous section never completed; drop it before the
                // new section (which starts right after the pointer bytes).
                buffer.clear();
            }
        }
        data = &data[1 + pointer..];
    } else if buffer.is_empty() {
        return;
    }
    buffer.extend_from_slice(data);
    drain_sections(buffer, &mut on_section);
}

fn drain_sections(buffer: &mut Vec<u8>, on_section: &mut impl FnMut(&[u8])) {
    loop {
        if buffer.len() < 3 {
            return;
        }
        let section_len = (((buffer[1] & 0x0F) as usize) << 8) | buffer[2] as usize;
        if section_len == 0 || section_len > 4093 {
            buffer.clear();
            return;
        }
        let total = 3 + section_len;
        if buffer.len() < total {
            return;
        }
        let section: Vec<u8> = buffer.drain(..total).collect();
        on_section(&section);
    }
}

/// Skip leading ID3v2 tags (HLS AAC-in-TS commonly prefixes PES payloads with
/// an ID3 timestamp tag).
fn strip_id3(mut data: &[u8]) -> &[u8] {
    while data.len() >= 10 && &data[..3] == b"ID3" {
        let size = ((data[6] as usize & 0x7F) << 21)
            | ((data[7] as usize & 0x7F) << 14)
            | ((data[8] as usize & 0x7F) << 7)
            | (data[9] as usize & 0x7F);
        let footer = if data[5] & 0x10 != 0 { 10 } else { 0 };
        let total = 10 + size + footer;
        if total > data.len() {
            return &[];
        }
        data = &data[total..];
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    const PMT_PID: u16 = 0x1000;
    const AUDIO_PID: u16 = 0x0101;

    fn ts_packet(pid: u16, pusi: bool, cc: u8, payload: &[u8]) -> [u8; PACKET_LEN] {
        assert!(payload.len() <= 183);
        let mut packet = [0xFFu8; PACKET_LEN];
        packet[0] = 0x47;
        packet[1] = ((pid >> 8) as u8 & 0x1F) | if pusi { 0x40 } else { 0 };
        packet[2] = pid as u8;
        packet[3] = 0x30 | (cc & 0x0F);
        let af_len = 183 - payload.len();
        packet[4] = af_len as u8;
        if af_len > 0 {
            packet[5] = 0x00;
            for byte in &mut packet[6..5 + af_len] {
                *byte = 0xFF;
            }
        }
        let start = 5 + af_len;
        packet[start..start + payload.len()].copy_from_slice(payload);
        packet
    }

    fn psi_packet(pid: u16, cc: u8, section: &[u8]) -> [u8; PACKET_LEN] {
        let mut payload = Vec::with_capacity(section.len() + 1);
        payload.push(0);
        payload.extend_from_slice(section);
        ts_packet(pid, true, cc, &payload)
    }

    fn pat(pmt_pid: u16) -> Vec<u8> {
        let mut section = vec![0x00, 0xB0, 13, 0x00, 0x01, 0xC1, 0x00, 0x00];
        section.extend_from_slice(&[0x00, 0x01]);
        section.extend_from_slice(&[0xE0 | (pmt_pid >> 8) as u8, pmt_pid as u8]);
        section.extend_from_slice(&[0, 0, 0, 0]);
        section
    }

    fn pmt(streams: &[(u8, u16)]) -> Vec<u8> {
        let section_len = 9 + streams.len() * 5 + 4;
        let mut section = vec![0x02, 0xB0 | (section_len >> 8) as u8, section_len as u8];
        section.extend_from_slice(&[0x00, 0x01, 0xC1, 0x00, 0x00]);
        section.extend_from_slice(&[0xE0, 0x01, 0xF0, 0x00]);
        for (stream_type, pid) in streams {
            section.push(*stream_type);
            section.extend_from_slice(&[0xE0 | (pid >> 8) as u8, *pid as u8, 0xF0, 0x00]);
        }
        section.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(section.len(), 3 + section_len);
        section
    }

    fn pes(data: &[u8]) -> Vec<u8> {
        let len = 3 + data.len();
        let mut out = vec![0x00, 0x00, 0x01, 0xC0];
        out.extend_from_slice(&(len as u16).to_be_bytes());
        out.extend_from_slice(&[0x00, 0x00, 0x00]);
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn demuxes_aac_payload_from_pat_pmt_and_pes() {
        let mut demuxer = TsDemuxer::new();
        let mut out = Vec::new();
        demuxer.feed(&psi_packet(0, 0, &pat(PMT_PID)), &mut out);
        demuxer.feed(
            &psi_packet(PMT_PID, 0, &pmt(&[(0x0F, AUDIO_PID)])),
            &mut out,
        );
        demuxer.feed(&ts_packet(AUDIO_PID, true, 0, &pes(b"AAA")), &mut out);
        demuxer.feed(&ts_packet(AUDIO_PID, true, 1, &pes(b"BBB")), &mut out);
        assert_eq!(demuxer.audio_stream_type(), Some(STREAM_ADTS_AAC));
        assert_eq!(out, b"AAABBB");
    }

    #[test]
    fn reassembles_pes_across_ts_packets_without_stuffing() {
        let mut demuxer = TsDemuxer::new();
        let mut out = Vec::new();
        demuxer.feed(&psi_packet(0, 0, &pat(PMT_PID)), &mut out);
        demuxer.feed(
            &psi_packet(PMT_PID, 0, &pmt(&[(0x0F, AUDIO_PID)])),
            &mut out,
        );

        let payload: Vec<u8> = (0..300u32).map(|value| value as u8).collect();
        let packet = pes(&payload);
        let first = &packet[..180];
        let rest = &packet[180..];
        demuxer.feed(&ts_packet(AUDIO_PID, true, 0, first), &mut out);
        demuxer.feed(&ts_packet(AUDIO_PID, false, 1, rest), &mut out);
        assert_eq!(out, payload);
    }

    #[test]
    fn strips_id3_timestamps_before_adts() {
        let mut demuxer = TsDemuxer::new();
        let mut out = Vec::new();
        demuxer.feed(&psi_packet(0, 0, &pat(PMT_PID)), &mut out);
        demuxer.feed(
            &psi_packet(PMT_PID, 0, &pmt(&[(0x0F, AUDIO_PID)])),
            &mut out,
        );

        let mut payload = vec![b'I', b'D', b'3', 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07];
        payload.extend_from_slice(b"TAGDATA");
        payload.extend_from_slice(b"ADTS");
        demuxer.feed(&ts_packet(AUDIO_PID, true, 0, &pes(&payload)), &mut out);
        assert_eq!(out, b"ADTS");
    }

    #[test]
    fn reassembles_pat_and_pmt_split_across_packets() {
        let mut demuxer = TsDemuxer::new();
        let mut out = Vec::new();

        let pat = pat(PMT_PID);
        let (first, rest) = pat.split_at(10);
        let mut payload = vec![0];
        payload.extend_from_slice(first);
        demuxer.feed(&ts_packet(0, true, 0, &payload), &mut out);
        assert_eq!(demuxer.pmt_pid, None);
        demuxer.feed(&ts_packet(0, false, 1, rest), &mut out);
        assert_eq!(demuxer.pmt_pid, Some(PMT_PID));

        let pmt = pmt(&[(0x0F, AUDIO_PID)]);
        let (first, rest) = pmt.split_at(9);
        let mut payload = vec![0];
        payload.extend_from_slice(first);
        demuxer.feed(&ts_packet(PMT_PID, true, 0, &payload), &mut out);
        assert!(demuxer.audio_stream_type().is_none());
        demuxer.feed(&ts_packet(PMT_PID, false, 1, rest), &mut out);
        assert_eq!(demuxer.audio_stream_type(), Some(STREAM_ADTS_AAC));

        demuxer.feed(&ts_packet(AUDIO_PID, true, 0, &pes(b"SPLIT")), &mut out);
        assert_eq!(out, b"SPLIT");
    }

    #[test]
    fn pointer_field_finishes_a_section_and_starts_the_next() {
        let mut demuxer = TsDemuxer::new();
        let mut out = Vec::new();
        let pat = pat(PMT_PID);
        let (first, rest) = pat.split_at(6);
        let mut payload = vec![0];
        payload.extend_from_slice(first);
        demuxer.feed(&ts_packet(0, true, 0, &payload), &mut out);

        // The next PUSI packet carries the tail of the split section in its
        // pointer bytes, followed by a complete new section.
        let mut payload = vec![rest.len() as u8];
        payload.extend_from_slice(rest);
        payload.extend_from_slice(&pat);
        demuxer.feed(&ts_packet(0, true, 1, &payload), &mut out);
        assert_eq!(demuxer.pmt_pid, Some(PMT_PID));
    }

    #[test]
    fn reports_unsupported_audio_codecs() {
        for (stream_type, name) in [
            (STREAM_AC3, "AC-3"),
            (STREAM_EAC3, "E-AC-3"),
            (STREAM_MPEG4_AUDIO, "MPEG-4 LATM/AAC"),
        ] {
            let mut demuxer = TsDemuxer::new();
            let mut out = Vec::new();
            demuxer.feed(&psi_packet(0, 0, &pat(PMT_PID)), &mut out);
            demuxer.feed(
                &psi_packet(PMT_PID, 0, &pmt(&[(stream_type, AUDIO_PID)])),
                &mut out,
            );
            assert!(demuxer.has_program_map());
            assert!(demuxer.audio_stream_type().is_none());
            assert_eq!(demuxer.unsupported_codec(), Some(name));
        }
    }

    #[test]
    fn prefers_a_supported_stream_when_the_pmt_lists_ac3_first() {
        let mut demuxer = TsDemuxer::new();
        let mut out = Vec::new();
        demuxer.feed(&psi_packet(0, 0, &pat(PMT_PID)), &mut out);
        demuxer.feed(
            &psi_packet(
                PMT_PID,
                0,
                &pmt(&[(STREAM_AC3, 0x0200), (STREAM_ADTS_AAC, AUDIO_PID)]),
            ),
            &mut out,
        );
        assert_eq!(demuxer.audio_stream_type(), Some(STREAM_ADTS_AAC));
        assert_eq!(demuxer.audio_pid, Some(AUDIO_PID));
        assert!(demuxer.unsupported_codec().is_none());
        demuxer.feed(&ts_packet(AUDIO_PID, true, 0, &pes(b"AAC")), &mut out);
        assert_eq!(out, b"AAC");
    }

    #[test]
    fn skips_video_and_picks_the_audio_stream_from_pmt() {
        let mut demuxer = TsDemuxer::new();
        let mut out = Vec::new();
        demuxer.feed(&psi_packet(0, 0, &pat(PMT_PID)), &mut out);
        demuxer.feed(
            &psi_packet(PMT_PID, 0, &pmt(&[(0x1B, 0x0100), (0x0F, AUDIO_PID)])),
            &mut out,
        );
        demuxer.feed(&ts_packet(0x0100, true, 0, &pes(b"VIDEO")), &mut out);
        demuxer.feed(&ts_packet(AUDIO_PID, true, 0, &pes(b"AUDIO")), &mut out);
        assert_eq!(out, b"AUDIO");
    }
}
