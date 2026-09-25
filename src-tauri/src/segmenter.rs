//! Repackages a raw MPEG-TS stream (HTTP or file) into HLS segments that WebKit can play.
//! Segments start on a key frame and begin with the latest PAT/PMT tables.

use crate::net::{self, StreamHeaders};
use iptv_core::ts;
use std::{
    collections::VecDeque,
    fs,
    io::Read,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const TARGET_SECONDS: f64 = 4.0;
const MAX_SECONDS: f64 = 12.0;
const LIVE_KEEP: usize = 18;
const LIVE_WINDOW: usize = 6;
const MAX_SEGMENT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub enum Source {
    Http { url: String, headers: StreamHeaders },
    File(PathBuf),
}

struct Segment {
    sequence: u64,
    duration: f64,
    discontinuity: bool,
}

#[derive(Default)]
struct State {
    segments: VecDeque<Segment>,
    next_sequence: u64,
    ended: bool,
    error: Option<String>,
}

pub struct Segmenter {
    dir: PathBuf,
    live: bool,
    state: Mutex<State>,
    ready: Condvar,
    stop: AtomicBool,
}

struct Cutter {
    pat: Option<Vec<u8>>,
    pmt_pids: Vec<u16>,
    pmt: Option<Vec<u8>>,
    cut_pid: Option<u16>,
    codec: &'static str,
    has_video: bool,
    current: Vec<u8>,
    start_pts: Option<u64>,
    last_pts: Option<u64>,
    started_at: Instant,
    waiting_keyframe: bool,
    /// The segment being built follows a timestamp jump.
    discontinuity: bool,
}

impl Cutter {
    fn new() -> Self {
        Self {
            pat: None,
            pmt_pids: Vec::new(),
            pmt: None,
            cut_pid: None,
            codec: "",
            has_video: false,
            current: Vec::new(),
            start_pts: None,
            last_pts: None,
            started_at: Instant::now(),
            waiting_keyframe: true,
            discontinuity: false,
        }
    }

    /// Seconds since the segment start; `None` when timestamps jump (reset, loop, splice).
    fn elapsed(&self, pts: Option<u64>) -> Option<f64> {
        match (self.start_pts, pts) {
            (Some(start), Some(now)) => {
                let seconds = ts::pts_delta(start, now) as f64 / 90_000.0;
                (seconds <= 60.0).then_some(seconds)
            }
            _ => Some(self.started_at.elapsed().as_secs_f64()),
        }
    }

    fn wall_clock(&self) -> f64 {
        self.started_at.elapsed().as_secs_f64()
    }

    fn tables(&self) -> Vec<u8> {
        let mut tables = Vec::with_capacity(ts::PACKET * 2);
        if let Some(pat) = &self.pat {
            tables.extend_from_slice(pat);
        }
        if let Some(pmt) = &self.pmt {
            tables.extend_from_slice(pmt);
        }
        tables
    }

    /// Feeds one packet; returns a finished segment (bytes, duration) when a cut happens.
    fn push(&mut self, packet: &[u8]) -> Option<(Vec<u8>, f64, bool)> {
        let pid = ts::pid(packet);
        if pid == 0 && ts::payload_unit_start(packet) {
            let pids = ts::parse_pat(packet);
            if !pids.is_empty() {
                self.pmt_pids = pids;
                self.pat = Some(packet.to_vec());
            }
            return None;
        }
        if self.pmt_pids.contains(&pid) && ts::payload_unit_start(packet) {
            if let Some(streams) = ts::parse_pmt(packet) {
                let video = streams.iter().find(|s| s.kind == ts::StreamKind::Video);
                let chosen =
                    video.or_else(|| streams.iter().find(|s| s.kind == ts::StreamKind::Audio));
                if let Some(stream) = chosen {
                    if self.cut_pid != Some(stream.pid) {
                        self.waiting_keyframe = true;
                    }
                    self.cut_pid = Some(stream.pid);
                    self.codec = stream.codec;
                    self.has_video = video.is_some();
                }
                self.pmt = Some(packet.to_vec());
            }
            return None;
        }
        if self.pat.is_none() || self.pmt.is_none() {
            return None;
        }
        let mut finished = None;
        if Some(pid) == self.cut_pid && ts::payload_unit_start(packet) {
            let pes = ts::payload(packet).unwrap_or(&[]);
            let pts = ts::pes_dts(pes);
            let keyframe = !self.has_video
                || ts::random_access(packet)
                || ts::starts_keyframe(self.codec, ts::pes_data(pes));
            if self.waiting_keyframe {
                if !keyframe {
                    return None;
                }
                self.waiting_keyframe = false;
                self.current = self.tables();
                self.start_pts = pts;
                self.started_at = Instant::now();
            } else {
                let (cut, duration, jump) = match self.elapsed(pts) {
                    Some(elapsed) => (
                        (keyframe && elapsed >= TARGET_SECONDS) || elapsed >= MAX_SECONDS,
                        elapsed,
                        false,
                    ),
                    // Timestamp jump: close the segment with its real duration and restart.
                    None => (true, self.wall_clock().clamp(0.1, MAX_SECONDS), true),
                };
                if cut {
                    let tables = self.tables();
                    let bytes = std::mem::replace(&mut self.current, tables);
                    finished = Some((
                        bytes,
                        duration.max(0.1),
                        std::mem::replace(&mut self.discontinuity, jump),
                    ));
                    self.start_pts = pts;
                    self.started_at = Instant::now();
                }
            }
            if pts.is_some() {
                self.last_pts = pts;
            }
        }
        if self.waiting_keyframe {
            return finished;
        }
        self.current.extend_from_slice(packet);
        if finished.is_none() && self.current.len() > MAX_SEGMENT_BYTES {
            let elapsed = self
                .elapsed(self.last_pts)
                .unwrap_or_else(|| self.wall_clock())
                .max(0.1);
            let tables = self.tables();
            let bytes = std::mem::replace(&mut self.current, tables);
            self.start_pts = self.last_pts;
            self.started_at = Instant::now();
            return Some((bytes, elapsed, std::mem::take(&mut self.discontinuity)));
        }
        finished
    }

    fn flush(&mut self) -> Option<(Vec<u8>, f64, bool)> {
        if self.waiting_keyframe || self.current.len() <= ts::PACKET * 2 {
            return None;
        }
        let elapsed = self
            .elapsed(self.last_pts)
            .unwrap_or_else(|| self.wall_clock())
            .max(0.1);
        Some((
            std::mem::take(&mut self.current),
            elapsed,
            self.discontinuity,
        ))
    }
}

impl Segmenter {
    pub fn start(source: Source, live: bool, dir: PathBuf) -> Result<Arc<Self>, String> {
        fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let segmenter = Arc::new(Self {
            dir,
            live,
            state: Mutex::new(State::default()),
            ready: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let worker = segmenter.clone();
        std::thread::Builder::new()
            .name("fluxo-segmenter".into())
            .spawn(move || worker.run(source))
            .map_err(|error| error.to_string())?;
        Ok(segmenter)
    }

    fn open(source: &Source) -> Result<Box<dyn Read + Send>, String> {
        match source {
            Source::File(path) => fs::File::open(path)
                .map(|file| {
                    Box::new(std::io::BufReader::with_capacity(1 << 20, file))
                        as Box<dyn Read + Send>
                })
                .map_err(|_| "Fichier vidéo illisible.".into()),
            Source::Http { url, headers } => {
                let response = headers
                    .apply(net::client(None)?.get(url))
                    .send()
                    .map_err(|error| net::transport_message(&error))?;
                if !response.status().is_success() {
                    return Err(net::status_message(
                        response.status().as_u16(),
                        net::deny_reason(&response).as_deref(),
                    ));
                }
                Ok(Box::new(response))
            }
        }
    }

    fn run(self: Arc<Self>, source: Source) {
        let mut failures = 0;
        let mut discontinuity = false;
        loop {
            if self.stop.load(Ordering::SeqCst) {
                break;
            }
            let produced = match Self::open(&source) {
                Ok(reader) => self.consume(reader, discontinuity),
                Err(error) => {
                    self.set_error(error);
                    0
                }
            };
            if !self.live || matches!(source, Source::File(_)) {
                break;
            }
            failures = if produced > 0 { 0 } else { failures + 1 };
            if failures >= 5 {
                self.set_error("Le flux s’est interrompu et ne reprend pas.".into());
                break;
            }
            discontinuity = true;
            std::thread::sleep(Duration::from_millis(800 * failures.max(1) as u64));
        }
        let mut state = self.state.lock().expect("segmenter state");
        state.ended = true;
        self.ready.notify_all();
    }

    /// Reads the stream until it ends; returns the number of segments produced.
    fn consume(&self, mut reader: Box<dyn Read + Send>, mut discontinuity: bool) -> usize {
        let mut cutter = Cutter::new();
        let mut buffer = vec![0u8; 188 * 700];
        let mut pending: Vec<u8> = Vec::new();
        let mut produced = 0;
        loop {
            if self.stop.load(Ordering::SeqCst) {
                return produced;
            }
            let read = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => break,
            };
            pending.extend_from_slice(&buffer[..read]);
            let mut offset = 0;
            while offset + ts::PACKET <= pending.len() {
                if pending[offset] != ts::SYNC {
                    match ts::sync_offset(&pending[offset..]) {
                        Some(skip) if skip > 0 => {
                            offset += skip;
                            continue;
                        }
                        _ => {
                            offset += 1;
                            continue;
                        }
                    }
                }
                if let Some((bytes, duration, jump)) =
                    cutter.push(&pending[offset..offset + ts::PACKET])
                {
                    self.publish(bytes, duration, std::mem::take(&mut discontinuity) || jump);
                    produced += 1;
                }
                offset += ts::PACKET;
            }
            pending.drain(..offset);
        }
        if let Some((bytes, duration, jump)) = cutter.flush() {
            self.publish(bytes, duration, discontinuity || jump);
            produced += 1;
        }
        produced
    }

    fn publish(&self, bytes: Vec<u8>, duration: f64, discontinuity: bool) {
        let mut state = self.state.lock().expect("segmenter state");
        let sequence = state.next_sequence;
        if fs::write(self.dir.join(format!("{sequence}.ts")), bytes).is_err() {
            state.error = Some("Écriture des segments impossible.".into());
            return;
        }
        state.next_sequence += 1;
        state.error = None;
        state.segments.push_back(Segment {
            sequence,
            duration,
            discontinuity,
        });
        if self.live {
            while state.segments.len() > LIVE_KEEP {
                if let Some(old) = state.segments.pop_front() {
                    let _ = fs::remove_file(self.dir.join(format!("{}.ts", old.sequence)));
                }
            }
        }
        self.ready.notify_all();
    }

    fn set_error(&self, error: String) {
        let mut state = self.state.lock().expect("segmenter state");
        state.error = Some(error);
        self.ready.notify_all();
    }

    /// Waits for enough segments, then returns the HLS media playlist.
    pub fn playlist(&self, wait: Duration) -> Result<String, String> {
        let needed = if self.live { 2 } else { 1 };
        let deadline = Instant::now() + wait;
        let mut state = self.state.lock().map_err(|e| e.to_string())?;
        while state.segments.len() < needed && !state.ended {
            let now = Instant::now();
            if now >= deadline {
                return Err(state
                    .error
                    .clone()
                    .unwrap_or_else(|| "Le flux n’a envoyé aucune image exploitable.".into()));
            }
            state = self
                .ready
                .wait_timeout(state, deadline - now)
                .map_err(|e| e.to_string())?
                .0;
        }
        if state.segments.is_empty() {
            return Err(state
                .error
                .clone()
                .unwrap_or_else(|| "Le flux ne contient aucune vidéo exploitable.".into()));
        }
        let visible: Vec<&Segment> = if self.live {
            let skip = state.segments.len().saturating_sub(LIVE_WINDOW);
            state.segments.iter().skip(skip).collect()
        } else {
            state.segments.iter().collect()
        };
        let target = visible
            .iter()
            .map(|segment| segment.duration)
            .fold(TARGET_SECONDS, f64::max)
            .ceil() as u64;
        let mut text = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
            visible[0].sequence
        );
        if !self.live {
            text.push_str("#EXT-X-PLAYLIST-TYPE:EVENT\n");
        }
        for segment in visible {
            if segment.discontinuity {
                text.push_str("#EXT-X-DISCONTINUITY\n");
            }
            text.push_str(&format!(
                "#EXTINF:{:.3},\nseg/{}.ts\n",
                segment.duration, segment.sequence
            ));
        }
        if state.ended && (!self.live || state.error.is_none()) {
            text.push_str("#EXT-X-ENDLIST\n");
        }
        Ok(text)
    }

    pub fn segment(&self, sequence: u64) -> Option<PathBuf> {
        let state = self.state.lock().ok()?;
        state
            .segments
            .iter()
            .any(|segment| segment.sequence == sequence)
            .then(|| self.dir.join(format!("{sequence}.ts")))
    }

    pub fn error(&self) -> Option<String> {
        self.state.lock().ok()?.error.clone()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(pid: u16, start: bool, random: bool, payload: &[u8]) -> Vec<u8> {
        let mut packet = vec![
            0x47,
            (pid >> 8) as u8 | if start { 0x40 } else { 0 },
            pid as u8,
            0x30,
        ];
        let field = 183 - payload.len();
        packet.push(field as u8);
        if field > 0 {
            packet.push(if random { 0x40 } else { 0 });
            packet.extend(std::iter::repeat_n(0xff, field - 1));
        }
        packet.extend_from_slice(payload);
        packet
    }

    fn pes(pts: u64, keyframe: bool) -> Vec<u8> {
        let mut pes = vec![0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5];
        pes.extend([
            0x21 | ((pts >> 29) as u8 & 0x0e),
            (pts >> 22) as u8,
            0x01 | ((pts >> 14) as u8 & 0xfe),
            (pts >> 7) as u8,
            0x01 | ((pts << 1) as u8 & 0xfe),
        ]);
        pes.extend([0, 0, 0, 1, if keyframe { 0x65 } else { 0x41 }]);
        pes
    }

    fn stream(seconds: u64) -> Vec<u8> {
        let mut pat = vec![
            0, 0x00, 0xb0, 13, 0, 1, 0xc1, 0, 0, 0, 1, 0xe1, 0x00, 0, 0, 0, 0,
        ];
        pat.truncate(17);
        let mut pmt = vec![
            0, 0x02, 0xb0, 18, 0, 1, 0xc1, 0, 0, 0xe1, 0x01, 0xf0, 0, 0x1b, 0xe1, 0x01, 0xf0, 0, 0,
            0, 0, 0,
        ];
        pmt.truncate(22);
        let mut data = Vec::new();
        for frame in 0..seconds * 25 {
            if frame % 25 == 0 {
                data.extend(packet(0, true, false, &pat));
                data.extend(packet(0x100, true, false, &pmt));
            }
            let keyframe = frame % 50 == 0;
            data.extend(packet(
                0x101,
                true,
                keyframe,
                &pes(90_000 + frame * 3600, keyframe),
            ));
            data.extend(packet(0x101, false, false, &[0xaa; 100]));
        }
        data
    }

    #[test]
    fn cuts_file_on_keyframes_and_serves_an_event_playlist() {
        let root = std::env::temp_dir().join(format!("fluxo-seg-{}", crate::net::random_token()));
        fs::create_dir_all(&root).unwrap();
        let file = root.join("input.ts");
        fs::write(&file, stream(21)).unwrap();
        let segmenter = Segmenter::start(Source::File(file), false, root.join("out")).unwrap();
        let mut playlist = String::new();
        for _ in 0..50 {
            playlist = segmenter.playlist(Duration::from_secs(5)).unwrap();
            if playlist.contains("ENDLIST") {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(playlist.contains("#EXT-X-ENDLIST"), "{playlist}");
        let durations: Vec<f64> = playlist
            .lines()
            .filter_map(|line| line.strip_prefix("#EXTINF:"))
            .map(|value| value.trim_end_matches(',').parse().unwrap())
            .collect();
        // Key frames every 2 s, target 4 s: 4 s segments plus the remainder.
        assert_eq!(durations[..4], [4.0, 4.0, 4.0, 4.0]);
        let first = fs::read(segmenter.segment(0).unwrap()).unwrap();
        assert_eq!(ts::pid(&first[..188]), 0);
        assert_eq!(ts::pid(&first[188..376]), 0x100);
        assert!(ts::random_access(&first[376..564]));
        segmenter.stop();
        let _ = fs::remove_dir_all(root);
    }
}
