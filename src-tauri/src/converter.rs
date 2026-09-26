//! Video converter: turns local files into other formats with FFmpeg.
//!
//! Jobs run one after another on a background thread, at a lower priority so playback stays
//! smooth. FFmpeg writes to a hidden partial file renamed at the end, so a cancelled or failed
//! conversion never leaves a truncated video behind. When an encoder refuses the source
//! (VideoToolbox rejects some inputs, subtitles that cannot be copied…), the next plan is tried.

use crate::{
    export::{file_stem, unique_path},
    transcode::{clean_log_line, ffmpeg_path},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashSet, VecDeque},
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    High,
    #[default]
    Standard,
    Small,
}

impl Quality {
    /// Picks the value matching the quality: (high, standard, small).
    fn pick<T>(self, values: (T, T, T)) -> T {
        match self {
            Self::High => values.0,
            Self::Standard => values.1,
            Self::Small => values.2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Video {
    H264,
    Hevc,
    Vp9,
    Av1,
    ProRes,
    Mpeg4,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Audio {
    Aac,
    Opus,
    Mp3,
    Flac,
    Pcm,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Encode(Video, Audio),
    Gif,
    /// Container change only: streams are copied.
    Remux,
    AudioOnly(Audio),
}

pub struct Preset {
    id: &'static str,
    label: &'static str,
    /// `video`, `copy` or `audio`: sections of the format list.
    group: &'static str,
    ext: &'static str,
    muxer: &'static str,
    kind: Kind,
    /// Subtitle tracks are kept (Matroska accepts nearly all of them).
    subtitles: bool,
}

const PRESETS: &[Preset] = &[
    Preset {
        id: "mp4-h264",
        label: "MP4 · H.264 + AAC — lisible partout",
        group: "video",
        ext: "mp4",
        muxer: "mp4",
        kind: Kind::Encode(Video::H264, Audio::Aac),
        subtitles: false,
    },
    Preset {
        id: "mp4-hevc",
        label: "MP4 · HEVC (H.265) + AAC — fichiers plus légers",
        group: "video",
        ext: "mp4",
        muxer: "mp4",
        kind: Kind::Encode(Video::Hevc, Audio::Aac),
        subtitles: false,
    },
    Preset {
        id: "mkv-h264",
        label: "MKV · H.264 + AAC — garde pistes audio et sous-titres",
        group: "video",
        ext: "mkv",
        muxer: "matroska",
        kind: Kind::Encode(Video::H264, Audio::Aac),
        subtitles: true,
    },
    Preset {
        id: "webm-vp9",
        label: "WebM · VP9 + Opus — pour le web",
        group: "video",
        ext: "webm",
        muxer: "webm",
        kind: Kind::Encode(Video::Vp9, Audio::Opus),
        subtitles: false,
    },
    Preset {
        id: "mp4-av1",
        label: "MP4 · AV1 + AAC — le plus compact (encodage lent)",
        group: "video",
        ext: "mp4",
        muxer: "mp4",
        kind: Kind::Encode(Video::Av1, Audio::Aac),
        subtitles: false,
    },
    Preset {
        id: "mov-prores",
        label: "MOV · ProRes 422 — montage vidéo",
        group: "video",
        ext: "mov",
        muxer: "mov",
        kind: Kind::Encode(Video::ProRes, Audio::Pcm),
        subtitles: false,
    },
    Preset {
        id: "avi-mpeg4",
        label: "AVI · MPEG-4 + MP3 — anciens lecteurs et téléviseurs",
        group: "video",
        ext: "avi",
        muxer: "avi",
        kind: Kind::Encode(Video::Mpeg4, Audio::Mp3),
        subtitles: false,
    },
    Preset {
        id: "gif",
        label: "GIF animé — courts extraits, sans son",
        group: "video",
        ext: "gif",
        muxer: "gif",
        kind: Kind::Gif,
        subtitles: false,
    },
    Preset {
        id: "remux-mp4",
        label: "MP4 — changer de conteneur, qualité identique",
        group: "copy",
        ext: "mp4",
        muxer: "mp4",
        kind: Kind::Remux,
        subtitles: false,
    },
    Preset {
        id: "remux-mkv",
        label: "MKV — changer de conteneur, garde toutes les pistes",
        group: "copy",
        ext: "mkv",
        muxer: "matroska",
        kind: Kind::Remux,
        subtitles: true,
    },
    Preset {
        id: "mp3",
        label: "MP3",
        group: "audio",
        ext: "mp3",
        muxer: "mp3",
        kind: Kind::AudioOnly(Audio::Mp3),
        subtitles: false,
    },
    Preset {
        id: "m4a",
        label: "M4A · AAC",
        group: "audio",
        ext: "m4a",
        muxer: "ipod",
        kind: Kind::AudioOnly(Audio::Aac),
        subtitles: false,
    },
    Preset {
        id: "flac",
        label: "FLAC — sans perte",
        group: "audio",
        ext: "flac",
        muxer: "flac",
        kind: Kind::AudioOnly(Audio::Flac),
        subtitles: false,
    },
    Preset {
        id: "wav",
        label: "WAV — non compressé",
        group: "audio",
        ext: "wav",
        muxer: "wav",
        kind: Kind::AudioOnly(Audio::Pcm),
        subtitles: false,
    },
];

fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

/// Encoders compiled into FFmpeg, read once from `ffmpeg -encoders`.
fn encoders() -> &'static HashSet<String> {
    static ENCODERS: OnceLock<HashSet<String>> = OnceLock::new();
    ENCODERS.get_or_init(|| {
        let Some(ffmpeg) = ffmpeg_path() else {
            return HashSet::new();
        };
        Command::new(ffmpeg)
            .args(["-hide_banner", "-nostdin", "-encoders"])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|output| parse_encoders(&String::from_utf8_lossy(&output.stdout)))
            .unwrap_or_default()
    })
}

fn parse_encoders(text: &str) -> HashSet<String> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let flags = parts.next()?;
            // Encoder lines start with six capability flags (`V....D`).
            (flags.len() == 6 && flags.starts_with(['V', 'A', 'S']))
                .then(|| parts.next().map(str::to_owned))?
        })
        .collect()
}

fn video_encoders(video: Video) -> (Option<&'static str>, &'static [&'static str]) {
    match video {
        Video::H264 => (Some("h264_videotoolbox"), &["libx264"]),
        Video::Hevc => (Some("hevc_videotoolbox"), &["libx265"]),
        Video::Vp9 => (None, &["libvpx-vp9"]),
        Video::Av1 => (None, &["libsvtav1", "libaom-av1"]),
        Video::ProRes => (Some("prores_videotoolbox"), &["prores_ks"]),
        Video::Mpeg4 => (None, &["mpeg4"]),
    }
}

fn audio_encoder(audio: Audio) -> &'static str {
    match audio {
        Audio::Aac => "aac",
        Audio::Opus => "libopus",
        Audio::Mp3 => "libmp3lame",
        Audio::Flac => "flac",
        Audio::Pcm => "pcm_s16le",
    }
}

/// Whether FFmpeg can produce this preset, and with a hardware encoder.
fn availability(preset: &Preset, has: &dyn Fn(&str) -> bool) -> (bool, bool) {
    match preset.kind {
        Kind::Encode(video, audio) => {
            let (hardware, software) = video_encoders(video);
            let hardware = hardware.is_some_and(has);
            (
                (hardware || software.iter().any(|name| has(name))) && has(audio_encoder(audio)),
                hardware,
            )
        }
        Kind::Gif => (has("gif"), false),
        Kind::Remux => (true, false),
        Kind::AudioOnly(audio) => (has(audio_encoder(audio)), false),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresetInfo {
    id: &'static str,
    label: &'static str,
    group: &'static str,
    ext: &'static str,
    available: bool,
    hardware: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    ffmpeg: bool,
    presets: Vec<PresetInfo>,
}

pub fn catalog() -> Catalog {
    let found = encoders();
    let has = |name: &str| found.contains(name);
    Catalog {
        ffmpeg: ffmpeg_path().is_some(),
        presets: PRESETS
            .iter()
            .map(|preset| {
                let (available, hardware) = availability(preset, &has);
                PresetInfo {
                    id: preset.id,
                    label: preset.label,
                    group: preset.group,
                    ext: preset.ext,
                    available,
                    hardware,
                }
            })
            .collect(),
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Options {
    pub preset: String,
    /// Largest picture height (720, 1080…); the original size when absent.
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub quality: Quality,
    /// VideoToolbox first when the Mac has it; the software encoder remains the fallback.
    #[serde(default = "default_true")]
    pub hardware: bool,
    /// Folder of the results; next to each source when absent.
    #[serde(default)]
    pub output_dir: Option<String>,
}

fn default_true() -> bool {
    true
}

/// What FFmpeg reports about the source.
#[derive(Debug, Default, PartialEq)]
struct Source {
    duration: Option<f64>,
    width: u32,
    height: u32,
    fps: f64,
    video: bool,
    audio: bool,
}

fn parse_clock(value: &str) -> Option<f64> {
    let mut total = 0.0;
    for part in value.trim().split(':') {
        total = total * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some(total)
}

/// Reads the banner FFmpeg prints for its input (`ffmpeg -i file`).
fn parse_source(banner: &str) -> Source {
    let mut source = Source {
        fps: 30.0,
        ..Source::default()
    };
    for line in banner.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("Duration:") {
            source.duration = rest
                .split(',')
                .next()
                .and_then(parse_clock)
                .filter(|value| *value > 0.0);
        } else if line.starts_with("Stream #") && line.contains("Video:") {
            // Cover art is stored as a video stream of a single picture.
            if line.contains("(attached pic)") || source.video {
                continue;
            }
            source.video = true;
            for token in line.split([',', ' ']) {
                if let Some((w, h)) = token.split_once('x')
                    && let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>())
                    && w >= 16
                    && h >= 16
                {
                    source.width = w;
                    source.height = h;
                    break;
                }
            }
            let words: Vec<&str> = line.split([',', ' ']).filter(|w| !w.is_empty()).collect();
            if let Some(index) = words.iter().position(|word| *word == "fps")
                && index > 0
                && let Ok(fps) = words[index - 1].parse::<f64>()
                && fps > 1.0
            {
                source.fps = fps.min(120.0);
            }
        } else if line.starts_with("Stream #") && line.contains("Audio:") {
            source.audio = true;
        }
    }
    source
}

fn probe(ffmpeg: &Path, input: &Path) -> Result<Source, String> {
    let output = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-i"])
        .arg(input)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .output()
        .map_err(|error| format!("FFmpeg n’a pas pu démarrer : {error}"))?;
    let banner = String::from_utf8_lossy(&output.stderr);
    let source = parse_source(&banner);
    if !source.video && !source.audio {
        let reason = banner
            .lines()
            .rev()
            .map(clean_log_line)
            .find(|line| !line.is_empty() && !line.contains("output file"))
            .unwrap_or("format inconnu");
        return Err(format!(
            "Ce fichier n’est pas lisible par FFmpeg ({reason})."
        ));
    }
    Ok(source)
}

/// Target bitrate in kbit/s for the bitrate-driven hardware encoders.
fn video_kbps(video: Video, quality: Quality, source: &Source, height: Option<u32>) -> u32 {
    let (width, source_height) = if source.width > 0 && source.height > 0 {
        (source.width as f64, source.height as f64)
    } else {
        (1920.0, 1080.0)
    };
    let out_height = height.map_or(source_height, |max| source_height.min(max as f64));
    let out_width = width * out_height / source_height;
    let bits_per_pixel = quality.pick((0.11, 0.07, 0.04));
    let codec = if video == Video::Hevc { 0.6 } else { 1.0 };
    let kbps = out_width * out_height * source.fps * bits_per_pixel * codec / 1000.0;
    kbps.clamp(400.0, 80_000.0).round() as u32
}

fn scale(height: Option<u32>) -> String {
    match height {
        // Even dimensions: 4:2:0 encoders refuse odd ones.
        Some(max) => format!("scale=-2:'trunc(min({max},ih)/2)*2'"),
        None => "scale='trunc(iw/2)*2':'trunc(ih/2)*2'".into(),
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn video_args(
    video: Video,
    encoder: &str,
    options: &Options,
    source: &Source,
) -> Result<Vec<String>, String> {
    let quality = options.quality;
    let kbps = video_kbps(video, quality, source, options.height);
    let mut args = vec!["-vf".to_owned(), scale(options.height)];
    args.extend(["-c:v".to_owned(), encoder.to_owned()]);
    let rest: Vec<String> = match encoder {
        "h264_videotoolbox" | "hevc_videotoolbox" => {
            let mut rest = vec![
                "-b:v".into(),
                format!("{kbps}k"),
                "-maxrate".into(),
                format!("{}k", kbps * 3 / 2),
                "-bufsize".into(),
                format!("{}k", kbps * 2),
                "-allow_sw".into(),
                "1".into(),
            ];
            if encoder == "h264_videotoolbox" {
                // Closed captions re-embedded as SEI make VideoToolbox abort (see transcode.rs).
                rest.extend(strings(&["-a53cc", "0"]));
            } else {
                rest.extend(strings(&["-tag:v", "hvc1"]));
            }
            rest
        }
        "libx264" => vec![
            "-preset".into(),
            "medium".into(),
            "-crf".into(),
            quality.pick(("18", "21", "26")).into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
        ],
        "libx265" => vec![
            "-preset".into(),
            "medium".into(),
            "-crf".into(),
            quality.pick(("20", "24", "28")).into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
            "-tag:v".into(),
            "hvc1".into(),
            "-x265-params".into(),
            "log-level=error".into(),
        ],
        "libvpx-vp9" => vec![
            "-crf".into(),
            quality.pick(("28", "33", "38")).into(),
            "-b:v".into(),
            "0".into(),
            "-deadline".into(),
            "good".into(),
            "-cpu-used".into(),
            "3".into(),
            "-row-mt".into(),
            "1".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
        ],
        "libsvtav1" => vec![
            "-crf".into(),
            quality.pick(("26", "32", "38")).into(),
            "-preset".into(),
            "8".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
        ],
        "libaom-av1" => vec![
            "-crf".into(),
            quality.pick(("26", "32", "38")).into(),
            "-b:v".into(),
            "0".into(),
            "-cpu-used".into(),
            "6".into(),
            "-row-mt".into(),
            "1".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
        ],
        "prores_videotoolbox" => vec![
            "-profile:v".into(),
            quality.pick(("hq", "standard", "lt")).into(),
        ],
        "prores_ks" => vec![
            "-profile:v".into(),
            quality.pick(("hq", "standard", "lt")).into(),
            "-pix_fmt".into(),
            "yuv422p10le".into(),
        ],
        "mpeg4" => vec![
            "-q:v".into(),
            quality.pick(("2", "4", "7")).into(),
            "-tag:v".into(),
            "XVID".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
        ],
        other => return Err(format!("Encodeur vidéo inconnu : {other}")),
    };
    args.extend(rest);
    Ok(args)
}

fn audio_args(audio: Audio, quality: Quality) -> Vec<String> {
    let mut args = vec!["-c:a".to_owned(), audio_encoder(audio).to_owned()];
    match audio {
        Audio::Aac => args.extend(["-b:a".into(), quality.pick(("256k", "192k", "128k")).into()]),
        Audio::Opus => args.extend(["-b:a".into(), quality.pick(("192k", "128k", "96k")).into()]),
        Audio::Mp3 => args.extend(["-q:a".into(), quality.pick(("0", "2", "5")).into()]),
        Audio::Flac | Audio::Pcm => {}
    }
    args
}

/// The FFmpeg output arguments to try, in order. Each plan is complete: maps, codecs, filters.
fn plans(
    preset: &Preset,
    options: &Options,
    source: &Source,
    has: &dyn Fn(&str) -> bool,
) -> Result<Vec<Vec<String>>, String> {
    let mut plans = Vec::new();
    match preset.kind {
        Kind::Encode(video, audio) => {
            if !source.video {
                return Err(
                    "Ce fichier ne contient pas de vidéo : choisissez un format audio.".into(),
                );
            }
            if !has(audio_encoder(audio)) {
                return Err(format!(
                    "Cette version de FFmpeg ne contient pas l’encodeur {}.",
                    audio_encoder(audio)
                ));
            }
            let (hardware, software) = video_encoders(video);
            let mut chosen: Vec<&str> = Vec::new();
            if options.hardware
                && let Some(name) = hardware.filter(|name| has(name))
            {
                chosen.push(name);
            }
            chosen.extend(software.iter().copied().filter(|name| has(name)));
            if chosen.is_empty() {
                return Err("Cette version de FFmpeg ne sait pas produire ce format.".into());
            }
            let subtitle_variants: &[bool] = if preset.subtitles {
                &[true, false]
            } else {
                &[false]
            };
            for encoder in chosen {
                for &keep_subtitles in subtitle_variants {
                    let mut args = strings(&["-map", "0:V:0?", "-map", "0:a?"]);
                    if keep_subtitles {
                        args.extend(strings(&["-map", "0:s?", "-c:s", "copy"]));
                    } else {
                        args.push("-sn".into());
                    }
                    args.push("-dn".into());
                    args.extend(video_args(video, encoder, options, source)?);
                    args.extend(audio_args(audio, options.quality));
                    plans.push(args);
                }
            }
            // Some channel layouts are refused by Opus and AAC: stereo as a last resort.
            if let Some(last) = plans.last() {
                let mut stereo = last.clone();
                stereo.extend(strings(&["-ac", "2"]));
                plans.push(stereo);
            }
        }
        Kind::Gif => {
            if !source.video {
                return Err("Ce fichier ne contient pas de vidéo.".into());
            }
            let max = options.height.unwrap_or(480);
            plans.push(vec![
                "-map".into(),
                "0:V:0".into(),
                "-an".into(),
                "-sn".into(),
                "-dn".into(),
                "-vf".into(),
                format!(
                    "fps=12,scale=-1:'min({max},ih)':flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer"
                ),
                "-loop".into(),
                "0".into(),
            ]);
        }
        Kind::Remux => {
            let mut maps = strings(&["-map", "0:V?", "-map", "0:a?"]);
            if preset.subtitles {
                let mut with_subtitles = maps.clone();
                with_subtitles.extend(strings(&["-map", "0:s?", "-dn", "-c", "copy"]));
                plans.push(with_subtitles);
            }
            if preset.muxer == "mp4" {
                maps = strings(&["-map", "0:V:0?", "-map", "0:a?"]);
            }
            let mut copy = maps.clone();
            copy.extend(strings(&["-sn", "-dn", "-c", "copy"]));
            plans.push(copy);
            // Audio codecs the container refuses (PCM, some DTS in MP4…) are converted to AAC.
            let mut aac = maps;
            aac.extend(strings(&["-sn", "-dn", "-c:v", "copy"]));
            aac.extend(audio_args(Audio::Aac, Quality::High));
            plans.push(aac);
        }
        Kind::AudioOnly(audio) => {
            if !source.audio {
                return Err("Ce fichier ne contient pas de piste audio.".into());
            }
            let mut args = strings(&["-map", "0:a:0", "-vn", "-sn", "-dn"]);
            args.extend(audio_args(audio, options.quality));
            plans.push(args);
        }
    }
    if matches!(preset.muxer, "mp4" | "mov" | "ipod") {
        for plan in &mut plans {
            plan.extend(strings(&["-movflags", "+faststart"]));
        }
    }
    Ok(plans)
}

/// One block of `-progress` output.
#[derive(Debug, Default, PartialEq)]
struct Progress {
    time: Option<f64>,
    size: Option<u64>,
    speed: Option<f64>,
}

/// Applies one `key=value` line; returns true at the end of a block.
fn progress_line(progress: &mut Progress, line: &str) -> bool {
    let Some((key, value)) = line.trim().split_once('=') else {
        return false;
    };
    let value = value.trim();
    match key {
        // Despite its name, `out_time_ms` is in microseconds, like `out_time_us`.
        "out_time_us" | "out_time_ms" => {
            if let Ok(micros) = value.parse::<i64>()
                && micros >= 0
            {
                progress.time = Some(micros as f64 / 1_000_000.0);
            }
        }
        "total_size" => progress.size = value.parse().ok(),
        "speed" => {
            progress.speed = value
                .trim_end_matches('x')
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|speed| speed.is_finite() && *speed > 0.0)
        }
        "progress" => return true,
        _ => {}
    }
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInfo {
    pub id: u64,
    pub name: String,
    pub input: String,
    pub output: String,
    pub preset: String,
    pub label: String,
    pub status: Status,
    /// 0 to 1; stays at 0 when the duration is unknown.
    pub progress: f64,
    pub speed: Option<f64>,
    /// Seconds left, estimated from the speed.
    pub eta: Option<f64>,
    pub bytes: u64,
    pub error: Option<String>,
}

struct Job {
    info: JobInfo,
    options: Options,
}

/// The FFmpeg process of the running job, so it can be stopped from another thread.
struct Current {
    id: u64,
    child: Child,
    partial: PathBuf,
}

#[derive(Default)]
struct Inner {
    jobs: Mutex<VecDeque<Job>>,
    wake: Condvar,
    current: Mutex<Option<Current>>,
    cancelled: Mutex<HashSet<u64>>,
    counter: AtomicU64,
    started: AtomicBool,
    stopping: AtomicBool,
}

#[derive(Default)]
pub struct Converter {
    inner: Arc<Inner>,
}

const FINISHED_KEPT: usize = 50;

fn is_finished(status: Status) -> bool {
    matches!(status, Status::Done | Status::Failed | Status::Cancelled)
}

impl Converter {
    pub fn jobs(&self) -> Vec<JobInfo> {
        self.inner
            .jobs
            .lock()
            .map(|jobs| jobs.iter().map(|job| job.info.clone()).collect())
            .unwrap_or_default()
    }

    /// Queues one job per file and starts the worker if needed.
    pub fn add(
        &self,
        app: &AppHandle,
        paths: Vec<String>,
        options: Options,
    ) -> Result<Vec<JobInfo>, String> {
        let preset = preset(&options.preset).ok_or("Format de sortie inconnu.")?;
        let found = encoders();
        if ffmpeg_path().is_none() {
            return Err("Le convertisseur nécessite FFmpeg (brew install ffmpeg).".into());
        }
        if !availability(preset, &|name| found.contains(name)).0 {
            return Err("Cette version de FFmpeg ne sait pas produire ce format.".into());
        }
        let output_dir = options
            .output_dir
            .as_deref()
            .map(str::trim)
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from);
        if let Some(dir) = &output_dir
            && !dir.is_dir()
        {
            return Err("Le dossier de destination n’existe plus.".into());
        }
        let mut added = Vec::new();
        {
            let mut jobs = self.inner.jobs.lock().map_err(|e| e.to_string())?;
            let mut reserved: HashSet<PathBuf> = jobs
                .iter()
                .filter(|job| !is_finished(job.info.status))
                .map(|job| PathBuf::from(&job.info.output))
                .collect();
            for path in paths {
                let input = PathBuf::from(&path);
                if input.is_dir() {
                    return Err(format!(
                        "« {} » est un dossier : choisissez les fichiers à convertir.",
                        input
                            .file_name()
                            .map(|name| name.to_string_lossy())
                            .unwrap_or_default()
                    ));
                }
                if !input.is_file() {
                    return Err(format!("Fichier introuvable : {path}"));
                }
                let dir = match &output_dir {
                    Some(dir) => dir.clone(),
                    None => input
                        .parent()
                        .map(Path::to_path_buf)
                        .ok_or("Dossier du fichier introuvable.")?,
                };
                let output = output_path(&input, &dir, preset.ext, &reserved);
                reserved.insert(output.clone());
                let info = JobInfo {
                    id: self.inner.counter.fetch_add(1, Ordering::Relaxed) + 1,
                    name: input
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.clone()),
                    input: path,
                    output: output.to_string_lossy().into_owned(),
                    preset: preset.id.into(),
                    label: preset.label.into(),
                    status: Status::Queued,
                    progress: 0.0,
                    speed: None,
                    eta: None,
                    bytes: 0,
                    error: None,
                };
                jobs.push_back(Job {
                    info: info.clone(),
                    options: options.clone(),
                });
                added.push(info);
            }
            // Old finished jobs are forgotten so the list stays short.
            while jobs
                .iter()
                .filter(|job| is_finished(job.info.status))
                .count()
                > FINISHED_KEPT
            {
                if let Some(index) = jobs.iter().position(|job| is_finished(job.info.status)) {
                    jobs.remove(index);
                }
            }
        }
        for info in &added {
            let _ = app.emit("convert", info);
        }
        if !self.inner.started.swap(true, Ordering::AcqRel) {
            let inner = self.inner.clone();
            let app = app.clone();
            std::thread::Builder::new()
                .name("fluxo-converter".into())
                .spawn(move || inner.work(&app))
                .map_err(|error| error.to_string())?;
        }
        self.inner.wake.notify_all();
        Ok(added)
    }

    pub fn cancel(&self, app: &AppHandle, id: u64) {
        let queued = self.inner.update(id, |info| {
            if info.status == Status::Queued {
                info.status = Status::Cancelled;
            }
        });
        if let Some(info) = queued.filter(|info| info.status == Status::Cancelled) {
            let _ = app.emit("convert", info);
            return;
        }
        if let Ok(mut cancelled) = self.inner.cancelled.lock() {
            cancelled.insert(id);
        }
        if let Ok(mut current) = self.inner.current.lock()
            && let Some(current) = current.as_mut().filter(|current| current.id == id)
        {
            let _ = current.child.kill();
        }
    }

    /// Forgets finished jobs.
    pub fn clear(&self) -> Vec<JobInfo> {
        if let Ok(mut jobs) = self.inner.jobs.lock() {
            jobs.retain(|job| !is_finished(job.info.status));
        }
        self.jobs()
    }

    /// Stops FFmpeg when the app quits: its partial file is removed.
    pub fn shutdown(&self) {
        self.inner.stopping.store(true, Ordering::Release);
        self.inner.wake.notify_all();
        if let Ok(mut current) = self.inner.current.lock()
            && let Some(mut current) = current.take()
        {
            let _ = current.child.kill();
            let _ = current.child.wait();
            let _ = fs::remove_file(&current.partial);
        }
    }
}

/// `dir/name.ext`, never the source itself nor a file that exists or another job will write.
fn output_path(input: &Path, dir: &Path, ext: &str, reserved: &HashSet<PathBuf>) -> PathBuf {
    let stem = file_stem(
        &input
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    let plain = dir.join(format!("{stem}.{ext}"));
    if plain != input && !plain.exists() && !reserved.contains(&plain) {
        return plain;
    }
    let stem = format!("{stem} (converti)");
    let mut path = unique_path(dir, &stem, ext);
    let mut index = 2;
    while reserved.contains(&path) || path == input {
        path = dir.join(format!("{stem} {index}.{ext}"));
        index += 1;
    }
    path
}

fn partial_path(output: &Path) -> PathBuf {
    let name = output
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    output.with_file_name(format!(".{name}.fluxo-part"))
}

enum Attempt {
    Done,
    Failed(String),
    Cancelled,
}

impl Inner {
    fn update(&self, id: u64, change: impl FnOnce(&mut JobInfo)) -> Option<JobInfo> {
        let mut jobs = self.jobs.lock().ok()?;
        let job = jobs.iter_mut().find(|job| job.info.id == id)?;
        change(&mut job.info);
        Some(job.info.clone())
    }

    fn is_cancelled(&self, id: u64) -> bool {
        self.stopping.load(Ordering::Acquire)
            || self
                .cancelled
                .lock()
                .map(|set| set.contains(&id))
                .unwrap_or(false)
    }

    fn next(&self) -> Option<(u64, Options)> {
        let mut jobs = self.jobs.lock().ok()?;
        loop {
            if self.stopping.load(Ordering::Acquire) {
                return None;
            }
            if let Some(job) = jobs
                .iter_mut()
                .find(|job| job.info.status == Status::Queued)
            {
                job.info.status = Status::Running;
                return Some((job.info.id, job.options.clone()));
            }
            jobs = self.wake.wait(jobs).ok()?;
        }
    }

    fn work(&self, app: &AppHandle) {
        let emit = |info: &JobInfo| {
            let _ = app.emit("convert", info);
        };
        while let Some((id, options)) = self.next() {
            if let Some(info) = self.update(id, |_| {}) {
                emit(&info);
            }
            let result = self.convert(&emit, id, &options);
            let info = self.update(id, |info| {
                info.speed = None;
                info.eta = None;
                match result {
                    Ok(output) => {
                        info.status = Status::Done;
                        info.progress = 1.0;
                        info.bytes = fs::metadata(&output).map(|meta| meta.len()).unwrap_or(0);
                        info.output = output.to_string_lossy().into_owned();
                    }
                    Err(None) => info.status = Status::Cancelled,
                    Err(Some(error)) => {
                        info.status = Status::Failed;
                        info.error = Some(error);
                    }
                }
            });
            if let Ok(mut cancelled) = self.cancelled.lock() {
                cancelled.remove(&id);
            }
            if let Some(info) = info {
                emit(&info);
            }
        }
    }

    /// Runs the plans until one succeeds. `Err(None)` means cancelled.
    fn convert(
        &self,
        emit: &dyn Fn(&JobInfo),
        id: u64,
        options: &Options,
    ) -> Result<PathBuf, Option<String>> {
        let ffmpeg = ffmpeg_path().ok_or_else(|| Some("FFmpeg est introuvable.".to_owned()))?;
        let (input, output) = {
            let jobs = self.jobs.lock().map_err(|e| Some(e.to_string()))?;
            let job = jobs
                .iter()
                .find(|job| job.info.id == id)
                .ok_or_else(|| Some("Conversion introuvable.".to_owned()))?;
            (
                PathBuf::from(&job.info.input),
                PathBuf::from(&job.info.output),
            )
        };
        if !input.is_file() {
            return Err(Some("Le fichier source a été déplacé ou supprimé.".into()));
        }
        let preset = preset(&options.preset).ok_or_else(|| Some("Format inconnu.".to_owned()))?;
        let source = probe(&ffmpeg, &input).map_err(Some)?;
        let found = encoders();
        let plans = plans(preset, options, &source, &|name| found.contains(name)).map_err(Some)?;
        let partial = partial_path(&output);
        let mut last_error = String::from("La conversion a échoué.");
        for plan in plans {
            if self.is_cancelled(id) {
                return Err(None);
            }
            let mut args = strings(&[
                "-hide_banner",
                "-nostdin",
                "-y",
                "-loglevel",
                "error",
                "-progress",
                "pipe:1",
                "-nostats",
                "-i",
            ]);
            args.push(input.to_string_lossy().into_owned());
            args.extend(plan);
            args.extend(["-f".into(), preset.muxer.into()]);
            args.push(partial.to_string_lossy().into_owned());
            match self.attempt(emit, id, &ffmpeg, &args, &partial, source.duration) {
                Attempt::Done => {
                    // A file with that name may have appeared during the conversion.
                    let target = if output.exists() {
                        let dir = output.parent().unwrap_or(Path::new("."));
                        output_path(&input, dir, preset.ext, &HashSet::new())
                    } else {
                        output.clone()
                    };
                    return fs::rename(&partial, &target)
                        .map(|()| target)
                        .map_err(|error| {
                            let _ = fs::remove_file(&partial);
                            Some(format!("Enregistrement du fichier impossible : {error}"))
                        });
                }
                Attempt::Cancelled => {
                    let _ = fs::remove_file(&partial);
                    return Err(None);
                }
                Attempt::Failed(error) => {
                    let _ = fs::remove_file(&partial);
                    last_error = error;
                }
            }
        }
        Err(Some(last_error))
    }

    fn attempt(
        &self,
        emit: &dyn Fn(&JobInfo),
        id: u64,
        ffmpeg: &Path,
        args: &[String],
        partial: &Path,
        duration: Option<f64>,
    ) -> Attempt {
        // A lower priority keeps playback and the interface smooth during long conversions.
        #[cfg(unix)]
        let mut command = if Path::new("/usr/bin/nice").is_file() {
            let mut command = Command::new("/usr/bin/nice");
            command.args(["-n", "10"]).arg(ffmpeg);
            command
        } else {
            Command::new(ffmpeg)
        };
        #[cfg(not(unix))]
        let mut command = Command::new(ffmpeg);
        let spawned = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => return Attempt::Failed(format!("FFmpeg n’a pas pu démarrer : {error}")),
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let log = std::thread::spawn(move || {
            let mut lines: VecDeque<String> = VecDeque::new();
            if let Some(stderr) = stderr {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let line = clean_log_line(&line).to_owned();
                    if !line.is_empty() {
                        lines.push_back(line);
                        if lines.len() > 12 {
                            lines.pop_front();
                        }
                    }
                }
            }
            lines
        });
        match self.current.lock() {
            Ok(mut current) => {
                *current = Some(Current {
                    id,
                    child,
                    partial: partial.to_path_buf(),
                })
            }
            Err(_) => {
                let _ = child.kill();
                return Attempt::Failed("Conversion interrompue.".into());
            }
        }
        // Cancelled between the check and the launch.
        if self.is_cancelled(id)
            && let Ok(mut current) = self.current.lock()
            && let Some(current) = current.as_mut()
        {
            let _ = current.child.kill();
        }
        if let Some(stdout) = stdout {
            let mut progress = Progress::default();
            let mut last_emit = Instant::now() - Duration::from_secs(1);
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if !progress_line(&mut progress, &line)
                    || last_emit.elapsed() < Duration::from_millis(400)
                {
                    continue;
                }
                last_emit = Instant::now();
                let info = self.update(id, |info| {
                    if let (Some(time), Some(total)) = (progress.time, duration) {
                        info.progress = (time / total).clamp(0.0, 0.999);
                        info.eta = progress
                            .speed
                            .map(|speed| ((total - time) / speed).max(0.0));
                    }
                    info.speed = progress.speed;
                    info.bytes = progress.size.unwrap_or(info.bytes);
                });
                if let Some(info) = info {
                    emit(&info);
                }
            }
        }
        let status = match self.current.lock() {
            Ok(mut current) => current.take().map(|mut current| current.child.wait()),
            Err(_) => None,
        };
        let lines = log.join().unwrap_or_default();
        if self.is_cancelled(id) {
            return Attempt::Cancelled;
        }
        match status {
            Some(Ok(status)) if status.success() => Attempt::Done,
            // Taken by `shutdown`: the app is quitting.
            None => Attempt::Cancelled,
            _ => Attempt::Failed(
                lines
                    .iter()
                    .rev()
                    .find(|line| !line.starts_with("Conversion failed"))
                    .map(|line| format!("FFmpeg : {line}"))
                    .unwrap_or_else(|| "La conversion a échoué.".into()),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::plans as plan_list;
    use super::*;

    const BANNER: &str = "Input #0, matroska,webm, from 'film.mkv':\n  Duration: 01:02:03.50, start: 0.000000, bitrate: 5000 kb/s\n  Stream #0:0(fre): Video: h264 (High), yuv420p(progressive), 1920x800 [SAR 1:1 DAR 12:5], 23.98 fps, 23.98 tbr, 1k tbn (default)\n  Stream #0:1(fre): Audio: ac3, 48000 Hz, 5.1(side), fltp, 448 kb/s (default)\n  Stream #0:2: Video: mjpeg (Baseline), yuvj420p(pc), 600x800, 90k tbr (attached pic)\nAt least one output file must be specified\n";

    fn options(preset: &str) -> Options {
        Options {
            preset: preset.into(),
            height: None,
            quality: Quality::Standard,
            hardware: true,
            output_dir: None,
        }
    }

    fn everything(_: &str) -> bool {
        true
    }

    #[test]
    fn reads_duration_size_and_streams_of_the_source() {
        let source = parse_source(BANNER);
        assert_eq!(source.duration, Some(3723.5));
        assert_eq!((source.width, source.height), (1920, 800));
        assert!((source.fps - 23.98).abs() < 0.01);
        assert!(source.video && source.audio);
        let audio = parse_source(
            "  Duration: N/A, bitrate: N/A\n  Stream #0:0: Audio: mp3, 44100 Hz, stereo\n",
        );
        assert_eq!(audio.duration, None);
        assert!(!audio.video && audio.audio);
    }

    #[test]
    fn hardware_encoder_first_then_software_then_stereo() {
        let source = parse_source(BANNER);
        let plans = plan_list(
            preset("mp4-h264").unwrap(),
            &options("mp4-h264"),
            &source,
            &everything,
        )
        .unwrap();
        assert_eq!(plans.len(), 3);
        let first = plans[0].join(" ");
        assert!(first.contains("-c:v h264_videotoolbox"));
        assert!(first.contains("-a53cc 0"));
        assert!(first.contains("-map 0:V:0? -map 0:a? -sn -dn"));
        assert!(first.contains("-c:a aac -b:a 192k"));
        assert!(first.ends_with("-movflags +faststart"));
        assert!(
            plans[1]
                .join(" ")
                .contains("-c:v libx264 -preset medium -crf 21")
        );
        assert!(plans[2].join(" ").contains("-ac 2"));

        let mut software_only = options("mp4-hevc");
        software_only.hardware = false;
        software_only.height = Some(720);
        let hevc = plan_list(
            preset("mp4-hevc").unwrap(),
            &software_only,
            &source,
            &everything,
        )
        .unwrap();
        let joined = hevc[0].join(" ");
        assert!(!joined.contains("videotoolbox"));
        assert!(joined.contains("scale=-2:'trunc(min(720,ih)/2)*2'"));
        assert!(joined.contains("-tag:v hvc1"));
    }

    #[test]
    fn matroska_keeps_subtitles_and_falls_back_without_them() {
        let source = parse_source(BANNER);
        let plans = plan_list(
            preset("mkv-h264").unwrap(),
            &options("mkv-h264"),
            &source,
            &|name| name != "h264_videotoolbox",
        )
        .unwrap();
        assert!(plans[0].join(" ").contains("-map 0:s? -c:s copy"));
        assert!(plans[1].join(" ").contains("-sn"));
        assert!(
            plans
                .iter()
                .all(|plan| !plan.join(" ").contains("videotoolbox"))
        );
        assert!(
            plans
                .iter()
                .all(|plan| !plan.join(" ").contains("faststart"))
        );
    }

    #[test]
    fn remux_copies_streams_then_converts_the_audio() {
        let source = parse_source(BANNER);
        let plans = plan_list(
            preset("remux-mp4").unwrap(),
            &options("remux-mp4"),
            &source,
            &everything,
        )
        .unwrap();
        assert_eq!(plans.len(), 2);
        assert!(plans[0].join(" ").contains("-c copy"));
        assert!(plans[1].join(" ").contains("-c:v copy -c:a aac"));
    }

    #[test]
    fn audio_and_video_presets_check_the_source() {
        let audio_only = parse_source("  Stream #0:0: Audio: mp3, 44100 Hz, stereo\n");
        assert!(
            plan_list(
                preset("mp4-h264").unwrap(),
                &options("mp4-h264"),
                &audio_only,
                &everything
            )
            .is_err()
        );
        let mp3 = plan_list(
            preset("mp3").unwrap(),
            &options("mp3"),
            &audio_only,
            &everything,
        )
        .unwrap();
        assert_eq!(
            mp3[0].join(" "),
            "-map 0:a:0 -vn -sn -dn -c:a libmp3lame -q:a 2"
        );
        let video = parse_source(BANNER);
        assert!(
            plan_list(
                preset("webm-vp9").unwrap(),
                &options("webm-vp9"),
                &video,
                &|name| name != "libvpx-vp9"
            )
            .is_err()
        );
    }

    #[test]
    fn bitrate_follows_picture_size_and_quality() {
        let source = parse_source(BANNER);
        let full = video_kbps(Video::H264, Quality::Standard, &source, None);
        let small = video_kbps(Video::H264, Quality::Standard, &source, Some(720));
        let hevc = video_kbps(Video::Hevc, Quality::Standard, &source, None);
        assert!((2000..4000).contains(&full), "{full}");
        assert!(small < full && hevc < full);
    }

    #[test]
    fn parses_progress_blocks() {
        let mut progress = Progress::default();
        for line in [
            "frame=10",
            "out_time_us=12500000",
            "total_size=1048576",
            "speed=2.5x",
        ] {
            assert!(!progress_line(&mut progress, line));
        }
        assert!(progress_line(&mut progress, "progress=continue"));
        assert_eq!(
            progress,
            Progress {
                time: Some(12.5),
                size: Some(1_048_576),
                speed: Some(2.5)
            }
        );
        progress_line(&mut progress, "speed=N/A");
        assert_eq!(progress.speed, None);
    }

    #[test]
    fn lists_encoders() {
        let text = "Encoders:\n V..... = Video\n ------\n V....D libx264              libx264 H.264\n A....D aac                  AAC (Advanced Audio Coding)\n";
        let found = parse_encoders(text);
        assert!(found.contains("libx264") && found.contains("aac"));
    }

    /// Converts a generated clip to every format FFmpeg supports here (skipped without FFmpeg).
    #[test]
    fn converts_a_real_file_to_every_available_format() {
        let Some(ffmpeg) = ffmpeg_path() else {
            return;
        };
        let dir =
            std::env::temp_dir().join(format!("fluxo-convert-e2e-{}", crate::net::random_token()));
        fs::create_dir_all(&dir).unwrap();
        let input = dir.join("clip.mkv");
        let generated = Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=322x182:rate=25:duration=2",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ])
            .arg(&input)
            .status();
        if !generated.is_ok_and(|status| status.success()) {
            return;
        }
        let inner = Inner::default();
        let found = encoders();
        for preset in PRESETS {
            if !availability(preset, &|name| found.contains(name)).0 {
                continue;
            }
            let id = inner.counter.fetch_add(1, Ordering::Relaxed) + 1;
            let output = output_path(&input, &dir, preset.ext, &HashSet::new());
            inner.jobs.lock().unwrap().push_back(Job {
                info: JobInfo {
                    id,
                    name: "clip.mkv".into(),
                    input: input.to_string_lossy().into_owned(),
                    output: output.to_string_lossy().into_owned(),
                    preset: preset.id.into(),
                    label: preset.label.into(),
                    status: Status::Running,
                    progress: 0.0,
                    speed: None,
                    eta: None,
                    bytes: 0,
                    error: None,
                },
                options: options(preset.id),
            });
            let result = inner.convert(&|_| {}, id, &options(preset.id));
            let path = result.unwrap_or_else(|error| panic!("{}: {error:?}", preset.id));
            assert!(fs::metadata(&path).unwrap().len() > 0, "{}", preset.id);
            assert!(!partial_path(&output).exists(), "{}", preset.id);
            let reread = probe(&ffmpeg, &path).unwrap();
            assert_eq!(reread.video, preset.group != "audio", "{}", preset.id);
            if preset.group != "audio" {
                assert_eq!(reread.width % 2, 0, "{}: even width", preset.id);
            }
        }
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn output_never_replaces_the_source_or_a_reserved_name() {
        let dir =
            std::env::temp_dir().join(format!("fluxo-convert-{}", crate::net::random_token()));
        fs::create_dir_all(&dir).unwrap();
        let input = dir.join("film.mp4");
        fs::write(&input, b"x").unwrap();
        assert_eq!(
            output_path(&input, &dir, "mkv", &HashSet::new()),
            dir.join("film.mkv")
        );
        assert_eq!(
            output_path(&input, &dir, "mp4", &HashSet::new()),
            dir.join("film (converti).mp4")
        );
        let reserved: HashSet<PathBuf> = [dir.join("film.mkv")].into();
        assert_eq!(
            output_path(&input, &dir, "mkv", &reserved),
            dir.join("film (converti).mkv")
        );
        assert_eq!(
            partial_path(&dir.join("film.mkv")),
            dir.join(".film.mkv.fluxo-part")
        );
        let _ = fs::remove_dir_all(dir);
    }
}
