//! Compatibility engine: FFmpeg converts sources macOS cannot decode (MPEG-2, HEVC in TS,
//! DASH, MKV/AVI…) into H.264/AAC HLS served by the local proxy.

use crate::net::StreamHeaders;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

static FFMPEG: OnceLock<Option<PathBuf>> = OnceLock::new();

/// FFmpeg bundled next to the app, installed by Homebrew/MacPorts, or on the `PATH`.
pub fn ffmpeg_path() -> Option<PathBuf> {
    FFMPEG
        .get_or_init(|| {
            let mut candidates = Vec::new();
            if let Ok(exe) = std::env::current_exe()
                && let Some(dir) = exe.parent()
            {
                candidates.push(dir.join("ffmpeg"));
                candidates.push(dir.join("../Resources/ffmpeg"));
            }
            for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin"] {
                candidates.push(Path::new(dir).join("ffmpeg"));
            }
            if let Some(path) = std::env::var_os("PATH") {
                candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("ffmpeg")));
            }
            candidates.into_iter().find(|path| path.is_file())
        })
        .clone()
}

pub struct Job {
    dir: PathBuf,
    child: Mutex<Option<Child>>,
}

pub struct Options<'a> {
    pub input: &'a str,
    pub headers: &'a StreamHeaders,
    pub live: bool,
    /// Keeps the original H.264 video and only converts audio/container.
    pub copy_video: bool,
    pub deinterlace: bool,
}

/// H.264 encoder used when the video has to be converted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoder {
    /// VideoToolbox: fast and light, but it rejects some inputs.
    Hardware,
    /// libx264: slower, accepts everything. Used when the hardware encoder fails.
    Software,
}

/// Largest picture height sent to the player. Converting 4K in real time is out of reach of
/// most Macs, and the video area of Fluxo never shows more than 1080 lines.
const MAX_HEIGHT: u32 = 1080;

pub fn arguments(options: &Options, encoder: Encoder, dir: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error"]
        .map(String::from)
        .to_vec();
    let remote = options.input.starts_with("http://") || options.input.starts_with("https://");
    if remote {
        args.extend(["-user_agent".into(), options.headers.user_agent()]);
        if let Some(referrer) = options.headers.referrer() {
            args.extend(["-headers".into(), format!("Referer: {referrer}\r\n")]);
        }
        args.extend(
            [
                "-reconnect",
                "1",
                "-reconnect_streamed",
                "1",
                "-reconnect_on_network_error",
                "1",
                "-reconnect_delay_max",
                "5",
            ]
            .map(String::from),
        );
    }
    args.extend([
        "-fflags".into(),
        "+genpts+discardcorrupt".into(),
        "-i".into(),
        options.input.into(),
    ]);
    args.extend(["-map", "0:v:0?", "-map", "0:a:0?", "-sn", "-dn"].map(String::from));
    if options.copy_video {
        args.extend(["-c:v", "copy"].map(String::from));
    } else {
        let mut filters = Vec::new();
        if options.deinterlace {
            filters.push("yadif=deint=interlaced".to_owned());
        }
        filters.push(format!("scale=-2:'min({MAX_HEIGHT},ih)'"));
        args.extend(["-vf".to_owned(), filters.join(",")]);
        let video: &[&str] = match encoder {
            // `-a53cc 0`: closed captions carried by the source are re-embedded by
            // VideoToolbox as SEI data it then fails to parse, which aborts the conversion.
            Encoder::Hardware => &[
                "-c:v",
                "h264_videotoolbox",
                "-a53cc",
                "0",
                "-allow_sw",
                "1",
                "-realtime",
                "1",
                "-b:v",
                "6M",
                "-maxrate",
                "8M",
            ],
            Encoder::Software => &[
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "23", "-maxrate", "8M",
                "-bufsize", "16M",
            ],
        };
        args.extend(video.iter().map(|arg| (*arg).to_owned()));
        args.extend(["-g", "100", "-pix_fmt", "yuv420p"].map(String::from));
    }
    args.extend(["-c:a", "aac", "-b:a", "192k", "-ac", "2", "-ar", "48000"].map(String::from));
    args.extend(["-f", "hls", "-hls_time", "4"].map(String::from));
    if options.live {
        args.extend(
            [
                "-hls_list_size",
                "8",
                "-hls_flags",
                "delete_segments+independent_segments+temp_file",
            ]
            .map(String::from),
        );
    } else {
        args.extend(
            [
                "-hls_list_size",
                "0",
                "-hls_playlist_type",
                "event",
                "-hls_flags",
                "independent_segments+temp_file",
            ]
            .map(String::from),
        );
    }
    args.extend([
        "-hls_segment_filename".into(),
        dir.join("seg%05d.ts").to_string_lossy().into_owned(),
        dir.join("index.m3u8").to_string_lossy().into_owned(),
    ]);
    args
}

/// Why a conversion did not produce a first segment.
#[derive(Debug, PartialEq, Eq)]
pub enum StartError {
    /// FFmpeg stopped: bad input, unsupported codec, encoder failure…
    Exited(String),
    /// FFmpeg is still running but nothing came out in time.
    TimedOut(String),
}

impl StartError {
    fn into_message(self) -> String {
        match self {
            Self::Exited(message) | Self::TimedOut(message) => message,
        }
    }
}

/// True once the playlist lists a segment with a real duration. FFmpeg that aborts before the
/// first frame leaves `#EXTINF:0.000000` and `#EXT-X-ENDLIST`, which must not count as playable.
fn playlist_ready(text: &str) -> bool {
    text.lines()
        .filter_map(|line| line.strip_prefix("#EXTINF:"))
        .filter_map(|rest| rest.split(',').next()?.trim().parse::<f64>().ok())
        .any(|duration| duration > 0.0)
}

/// Drops the `[encoder @ 0x…]` prefixes FFmpeg puts in front of its messages.
fn clean_log_line(line: &str) -> &str {
    let mut line = line.trim();
    while line.starts_with('[')
        && let Some(end) = line.find("] ")
    {
        line = line[end + 2..].trim_start();
    }
    line
}

impl Job {
    pub fn start(options: &Options, encoder: Encoder, dir: PathBuf) -> Result<Self, String> {
        let ffmpeg = ffmpeg_path().ok_or(
            "Le moteur de compatibilité nécessite FFmpeg. Installez-le avec « brew install ffmpeg », puis réessayez.",
        )?;
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let log = fs::File::create(dir.join("ffmpeg.log")).map_err(|e| e.to_string())?;
        let child = Command::new(ffmpeg)
            .args(arguments(options, encoder, &dir))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map_err(|error| format!("FFmpeg n’a pas pu démarrer : {error}"))?;
        Ok(Self {
            dir,
            child: Mutex::new(Some(child)),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether the process has already stopped, and with which status.
    fn exit_status(&self) -> Option<std::process::ExitStatus> {
        self.child.lock().ok()?.as_mut()?.try_wait().ok()?
    }

    fn last_error(&self) -> String {
        let log = fs::read_to_string(self.dir.join("ffmpeg.log")).unwrap_or_default();
        log.lines()
            .rev()
            .map(clean_log_line)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .to_owned()
    }

    /// Error printed by FFmpeg if the process has already stopped.
    pub fn failure(&self) -> Option<String> {
        if self.exit_status()?.success() {
            return None;
        }
        Some(
            format!("La conversion FFmpeg a échoué. {}", self.last_error())
                .trim()
                .to_owned(),
        )
    }

    pub fn wait_playlist(&self, timeout: Duration) -> Result<(), StartError> {
        let deadline = Instant::now() + timeout;
        let playlist = self.dir.join("index.m3u8");
        loop {
            // Read the exit status first: whatever the playlist says afterwards is final.
            let exited = self.exit_status();
            let ready = fs::read_to_string(&playlist).is_ok_and(|text| playlist_ready(&text));
            if let Some(status) = exited
                && !status.success()
            {
                return Err(StartError::Exited(
                    self.failure()
                        .unwrap_or_else(|| "La conversion FFmpeg a échoué.".into()),
                ));
            }
            if ready {
                return Ok(());
            }
            if exited.is_some() {
                return Err(StartError::Exited(
                    "La conversion FFmpeg s’est arrêtée sans produire d’image.".into(),
                ));
            }
            if Instant::now() >= deadline {
                return Err(StartError::TimedOut(
                    "La conversion FFmpeg n’a produit aucune image à temps.".into(),
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    pub fn stop(&self) {
        if let Ok(mut guard) = self.child.lock()
            && let Some(mut child) = guard.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Starts a conversion and waits for its first segment. The hardware encoder goes first; when
/// FFmpeg stops with an error (VideoToolbox rejects some sources), the software encoder gets
/// one attempt so the channel still plays. A timeout is not retried: the source is slow, not
/// the encoder.
pub fn launch(
    options: &Options,
    dir: impl Fn() -> PathBuf,
    timeout: Duration,
) -> Result<Job, String> {
    let attempt = |encoder| -> Result<Job, StartError> {
        let job = Job::start(options, encoder, dir()).map_err(StartError::Exited)?;
        match job.wait_playlist(timeout) {
            Ok(()) => Ok(job),
            Err(error) => {
                job.stop();
                Err(error)
            }
        }
    };
    match attempt(Encoder::Hardware) {
        Ok(job) => Ok(job),
        Err(StartError::Exited(hardware)) if !options.copy_video && ffmpeg_path().is_some() => {
            attempt(Encoder::Software)
                .map_err(|software| format!("{} ({hardware})", software.into_message()))
        }
        Err(error) => Err(error.into_message()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options<'a>(
        input: &'a str,
        headers: &'a StreamHeaders,
        live: bool,
        copy_video: bool,
        deinterlace: bool,
    ) -> Options<'a> {
        Options {
            input,
            headers,
            live,
            copy_video,
            deinterlace,
        }
    }

    #[test]
    fn builds_live_arguments_with_headers() {
        let headers = StreamHeaders {
            user_agent: Some("Agent".into()),
            referrer: Some("https://site.example/".into()),
        };
        let args = arguments(
            &options("http://h/live.ts", &headers, true, false, true),
            Encoder::Hardware,
            Path::new("/tmp/job"),
        );
        let joined = args.join(" ");
        assert!(joined.contains("-user_agent Agent"));
        assert!(joined.contains("Referer: https://site.example/\r\n"));
        assert!(joined.contains("-c:v h264_videotoolbox"));
        assert!(joined.contains("-vf yadif=deint=interlaced,scale=-2:'min(1080,ih)'"));
        assert!(joined.contains("delete_segments"));
        assert!(joined.ends_with("/tmp/job/index.m3u8"));
        let no_headers = StreamHeaders::default();
        let local = arguments(
            &options("/tmp/film.mkv", &no_headers, false, true, false),
            Encoder::Hardware,
            Path::new("/tmp/job"),
        );
        assert!(!local.contains(&"-user_agent".to_string()));
        assert!(local.join(" ").contains("-c:v copy"));
        assert!(!local.contains(&"-vf".to_string()));
        assert!(local.join(" ").contains("-hls_playlist_type event"));
    }

    #[test]
    fn hardware_encoder_ignores_source_closed_captions_and_caps_the_height() {
        let headers = StreamHeaders::default();
        let hardware = arguments(
            &options("https://h/4k.m3u8", &headers, true, false, false),
            Encoder::Hardware,
            Path::new("/tmp/job"),
        )
        .join(" ");
        assert!(hardware.contains("-c:v h264_videotoolbox -a53cc 0"));
        assert!(hardware.contains("-vf scale=-2:'min(1080,ih)'"));
        assert!(!hardware.contains("yadif"));
        let software = arguments(
            &options("https://h/4k.m3u8", &headers, true, false, false),
            Encoder::Software,
            Path::new("/tmp/job"),
        )
        .join(" ");
        assert!(software.contains("-c:v libx264 -preset veryfast"));
        assert!(!software.contains("videotoolbox"));
        assert!(software.contains("-pix_fmt yuv420p"));
    }

    #[test]
    fn empty_ended_playlist_is_not_ready() {
        let aborted =
            "#EXTM3U\n#EXT-X-TARGETDURATION:0\n#EXTINF:0.000000,\nseg00000.ts\n#EXT-X-ENDLIST\n";
        assert!(!playlist_ready(aborted));
        assert!(!playlist_ready(""));
        let playing = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4.000000,\nseg00000.ts\n";
        assert!(playlist_ready(playing));
    }

    #[test]
    fn strips_ffmpeg_prefixes_from_log_lines() {
        assert_eq!(
            clean_log_line(
                "[vost#0:0/h264_videotoolbox @ 0xc9cffc900] [enc:h264_videotoolbox @ 0xc9cffb5d0] Error encoding a frame: Invalid data"
            ),
            "Error encoding a frame: Invalid data"
        );
        assert_eq!(clean_log_line("  Plain message "), "Plain message");
    }
}
