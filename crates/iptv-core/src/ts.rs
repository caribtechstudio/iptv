//! Minimal MPEG-TS inspection: program map, codecs, timestamps and key frames.

use serde::Serialize;

pub const PACKET: usize = 188;
pub const SYNC: u8 = 0x47;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Video,
    Audio,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TsStream {
    pub pid: u16,
    pub stream_type: u8,
    pub codec: &'static str,
    pub kind: StreamKind,
}

pub fn pid(packet: &[u8]) -> u16 {
    (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2])
}

pub fn payload_unit_start(packet: &[u8]) -> bool {
    packet[1] & 0x40 != 0
}

/// Random access indicator of the adaptation field.
pub fn random_access(packet: &[u8]) -> bool {
    let control = (packet[3] >> 4) & 3;
    matches!(control, 2 | 3) && packet[4] > 0 && packet[5] & 0x40 != 0
}

pub fn payload(packet: &[u8]) -> Option<&[u8]> {
    let control = (packet[3] >> 4) & 3;
    let offset = match control {
        1 => 4,
        3 => 5 + usize::from(packet[4]),
        _ => return None,
    };
    packet.get(offset..).filter(|payload| !payload.is_empty())
}

/// PSI section of a packet starting a table (skips the pointer field).
fn section(packet: &[u8]) -> Option<&[u8]> {
    if !payload_unit_start(packet) {
        return None;
    }
    let payload = payload(packet)?;
    payload.get(1 + usize::from(*payload.first()?)..)
}

pub fn parse_pat(packet: &[u8]) -> Vec<u16> {
    let Some(s) = section(packet) else {
        return Vec::new();
    };
    if s.len() < 8 || s[0] != 0 {
        return Vec::new();
    }
    let length = ((usize::from(s[1]) & 0x0f) << 8) | usize::from(s[2]);
    let end = (3 + length).saturating_sub(4).min(s.len());
    let mut pids = Vec::new();
    let mut index = 8;
    while index + 4 <= end {
        let program = (u16::from(s[index]) << 8) | u16::from(s[index + 1]);
        if program != 0 {
            pids.push((u16::from(s[index + 2] & 0x1f) << 8) | u16::from(s[index + 3]));
        }
        index += 4;
    }
    pids
}

fn describe(stream_type: u8, descriptors: &[u8]) -> (&'static str, StreamKind) {
    match stream_type {
        0x01 => ("mpeg1video", StreamKind::Video),
        0x02 => ("mpeg2video", StreamKind::Video),
        0x1b => ("h264", StreamKind::Video),
        0x24 => ("hevc", StreamKind::Video),
        0x03 | 0x04 => ("mpeg-audio", StreamKind::Audio),
        0x0f => ("aac", StreamKind::Audio),
        0x11 => ("aac-latm", StreamKind::Audio),
        0x81 => ("ac3", StreamKind::Audio),
        0x87 => ("eac3", StreamKind::Audio),
        0x06 => {
            let mut index = 0;
            while index + 1 < descriptors.len() {
                match descriptors[index] {
                    0x6a => return ("ac3", StreamKind::Audio),
                    0x7a => return ("eac3", StreamKind::Audio),
                    0x56 => return ("teletext", StreamKind::Other),
                    0x59 => return ("dvb-subtitles", StreamKind::Other),
                    _ => {}
                }
                index += 2 + usize::from(descriptors[index + 1]);
            }
            ("private", StreamKind::Other)
        }
        0x15 => ("id3", StreamKind::Other),
        0x86 => ("scte35", StreamKind::Other),
        _ => ("unknown", StreamKind::Other),
    }
}

pub fn parse_pmt(packet: &[u8]) -> Option<Vec<TsStream>> {
    let s = section(packet)?;
    if s.len() < 12 || s[0] != 2 {
        return None;
    }
    let length = ((usize::from(s[1]) & 0x0f) << 8) | usize::from(s[2]);
    let end = (3 + length).saturating_sub(4).min(s.len());
    let info_length = ((usize::from(s[10]) & 0x0f) << 8) | usize::from(s[11]);
    let mut index = 12 + info_length;
    let mut streams = Vec::new();
    while index + 5 <= end {
        let stream_type = s[index];
        let pid = (u16::from(s[index + 1] & 0x1f) << 8) | u16::from(s[index + 2]);
        let es_length = ((usize::from(s[index + 3]) & 0x0f) << 8) | usize::from(s[index + 4]);
        let descriptors = s
            .get(index + 5..(index + 5 + es_length).min(end))
            .unwrap_or(&[]);
        let (codec, kind) = describe(stream_type, descriptors);
        streams.push(TsStream {
            pid,
            stream_type,
            codec,
            kind,
        });
        index += 5 + es_length;
    }
    Some(streams)
}

/// Offset of the first packet boundary (two consecutive sync bytes).
pub fn sync_offset(data: &[u8]) -> Option<usize> {
    (0..data.len().min(PACKET * 4)).find(|&index| {
        data[index] == SYNC && data.get(index + PACKET).is_none_or(|byte| *byte == SYNC)
    })
}

pub fn looks_like_ts(data: &[u8]) -> bool {
    sync_offset(data).is_some_and(|offset| {
        offset < PACKET
            && data.len() >= offset + PACKET * 2
            && data[offset + PACKET] == SYNC
            && data
                .get(offset + PACKET * 2)
                .is_none_or(|byte| *byte == SYNC)
    })
}

/// Elementary streams declared by the first program map of a TS buffer.
pub fn stream_info(data: &[u8]) -> Option<Vec<TsStream>> {
    let start = sync_offset(data)?;
    let mut pmt_pids = Vec::new();
    for packet in data[start..].as_chunks::<PACKET>().0 {
        if packet[0] != SYNC {
            return None;
        }
        let pid = pid(packet);
        if pid == 0 && pmt_pids.is_empty() {
            pmt_pids = parse_pat(packet);
        } else if pmt_pids.contains(&pid)
            && let Some(streams) = parse_pmt(packet)
        {
            return Some(streams);
        }
    }
    None
}

/// Presentation timestamp (90 kHz) of a PES header, if present.
pub fn pes_pts(pes: &[u8]) -> Option<u64> {
    if pes.len() < 14 || pes[..3] != [0, 0, 1] || pes[7] & 0x80 == 0 {
        return None;
    }
    let b = &pes[9..14];
    Some(
        (u64::from(b[0] >> 1) & 0x07) << 30
            | u64::from(b[1]) << 22
            | u64::from(b[2] >> 1) << 15
            | u64::from(b[3]) << 7
            | u64::from(b[4] >> 1),
    )
}

/// Decoding timestamp (monotonic, unlike PTS with B-frames), falling back to the PTS.
pub fn pes_dts(pes: &[u8]) -> Option<u64> {
    if pes.len() >= 19 && pes[..3] == [0, 0, 1] && pes[7] & 0xc0 == 0xc0 {
        let b = &pes[14..19];
        return Some(
            (u64::from(b[0] >> 1) & 0x07) << 30
                | u64::from(b[1]) << 22
                | u64::from(b[2] >> 1) << 15
                | u64::from(b[3]) << 7
                | u64::from(b[4] >> 1),
        );
    }
    pes_pts(pes)
}

/// Payload of a PES packet after its header.
pub fn pes_data(pes: &[u8]) -> &[u8] {
    if pes.len() < 9 || pes[..3] != [0, 0, 1] {
        return &[];
    }
    pes.get(9 + usize::from(pes[8])..).unwrap_or(&[])
}

/// Detects the start of a decodable picture (IDR/IRAP, parameter sets, MPEG-2 sequence header).
pub fn starts_keyframe(codec: &str, data: &[u8]) -> bool {
    let mut index = 0;
    while index + 3 < data.len() {
        if data[index] == 0 && data[index + 1] == 0 && data[index + 2] == 1 {
            let header = data[index + 3];
            let found = match codec {
                "h264" => matches!(header & 0x1f, 5 | 7),
                "hevc" => matches!((header >> 1) & 0x3f, 16..=21 | 32..=34),
                "mpeg2video" | "mpeg1video" => header == 0xb3,
                _ => false,
            };
            if found {
                return true;
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    false
}

/// Difference between two 33-bit timestamps, handling wrap-around.
pub fn pts_delta(from: u64, to: u64) -> u64 {
    const WRAP: u64 = 1 << 33;
    (to + WRAP - from) % WRAP
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn packet(pid: u16, start: bool, random: bool, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![
            SYNC,
            (pid >> 8) as u8 | if start { 0x40 } else { 0 },
            pid as u8,
        ];
        let stuffing = PACKET - 4 - payload.len();
        if stuffing > 0 || random {
            packet.push(0x30);
            let field = stuffing.max(2) - 1;
            packet.push(field as u8);
            if field > 0 {
                packet.push(if random { 0x40 } else { 0 });
                packet.extend(std::iter::repeat_n(0xff, field - 1));
            }
        } else {
            packet.push(0x10);
        }
        packet.extend_from_slice(payload);
        packet.resize(PACKET, 0xff);
        packet
    }

    pub fn pat(pmt: u16) -> Vec<u8> {
        packet(
            0,
            true,
            false,
            &[
                0,
                0x00,
                0xb0,
                13,
                0,
                1,
                0xc1,
                0,
                0,
                0,
                1,
                0xe0 | (pmt >> 8) as u8,
                pmt as u8,
                0,
                0,
                0,
                0,
            ],
        )
    }

    pub fn pmt(pmt: u16, streams: &[(u8, u16, &[u8])]) -> Vec<u8> {
        let mut body = vec![0, 1, 0xc1, 0, 0, 0xe1, 0x00, 0xf0, 0];
        for (stream_type, pid, descriptors) in streams {
            body.extend([
                *stream_type,
                0xe0 | (pid >> 8) as u8,
                *pid as u8,
                0xf0,
                descriptors.len() as u8,
            ]);
            body.extend_from_slice(descriptors);
        }
        let length = body.len() + 4;
        let mut section = vec![0, 0x02, 0xb0 | (length >> 8) as u8, length as u8];
        section.extend(body);
        section.extend([0, 0, 0, 0]);
        packet(pmt, true, false, &section)
    }

    pub fn pes(pts: u64, data: &[u8]) -> Vec<u8> {
        let mut pes = vec![0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5];
        pes.extend([
            0x21 | ((pts >> 29) as u8 & 0x0e),
            (pts >> 22) as u8,
            0x01 | ((pts >> 14) as u8 & 0xfe),
            (pts >> 7) as u8,
            0x01 | ((pts << 1) as u8 & 0xfe),
        ]);
        pes.extend_from_slice(data);
        pes
    }

    #[test]
    fn reads_program_map_codecs_timestamps_and_keyframes() {
        let mut data = pat(0x100);
        data.extend(pmt(
            0x100,
            &[
                (0x1b, 0x101, &[]),
                (0x06, 0x102, &[0x6a, 1, 0]),
                (0x0f, 0x103, &[]),
            ],
        ));
        let streams = stream_info(&data).unwrap();
        let codecs: Vec<_> = streams.iter().map(|s| (s.pid, s.codec, s.kind)).collect();
        assert_eq!(
            codecs,
            vec![
                (0x101, "h264", StreamKind::Video),
                (0x102, "ac3", StreamKind::Audio),
                (0x103, "aac", StreamKind::Audio)
            ]
        );
        let pes = pes(123_456, &[0, 0, 0, 1, 0x09, 0xf0, 0, 0, 1, 0x67]);
        assert_eq!(pes_pts(&pes), Some(123_456));
        assert!(starts_keyframe("h264", pes_data(&pes)));
        assert!(!starts_keyframe("h264", &[0, 0, 1, 0x41]));
        assert_eq!(pts_delta((1 << 33) - 10, 20), 30);
        let video = packet(0x101, true, true, &pes);
        assert!(random_access(&video));
        assert!(looks_like_ts(&data));
    }
}
