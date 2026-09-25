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

pub fn arguments(options: &Options, dir: &Path) -> Vec<String> {
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
        if options.deinterlace {
            args.extend(["-vf", "yadif=deint=interlaced"].map(String::from));
        }
        args.extend(
            [
                "-c:v",
                "h264_videotoolbox",
                "-allow_sw",
                "1",
                "-realtime",
                "1",
                "-b:v",
                "6M",
                "-maxrate",
                "8M",
                "-g",
                "100",
                "-pix_fmt",
                "yuv420p",
            ]
            .map(String::from),
        );
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

impl Job {
    pub fn start(options: Options, dir: PathBuf) -> Result<Self, String> {
        let ffmpeg = ffmpeg_path().ok_or(
            "Le moteur de compatibilité nécessite FFmpeg. Installez-le avec « brew install ffmpeg », puis réessayez.",
        )?;
        fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let log = fs::File::create(dir.join("ffmpeg.log")).map_err(|e| e.to_string())?;
        let child = Command::new(ffmpeg)
            .args(arguments(&options, &dir))
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

    /// Error printed by FFmpeg if the process has already stopped.
    pub fn failure(&self) -> Option<String> {
        let mut guard = self.child.lock().ok()?;
        let status = guard.as_mut()?.try_wait().ok()??;
        if status.success() {
            return None;
        }
        let log = fs::read_to_string(self.dir.join("ffmpeg.log")).unwrap_or_default();
        let last = log
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("");
        Some(
            format!("La conversion FFmpeg a échoué. {last}")
                .trim()
                .to_owned(),
        )
    }

    pub fn wait_playlist(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let playlist = self.dir.join("index.m3u8");
        loop {
            if fs::read_to_string(&playlist).is_ok_and(|text| text.contains("#EXTINF")) {
                return Ok(());
            }
            if let Some(error) = self.failure() {
                return Err(error);
            }
            if Instant::now() >= deadline {
                return Err("La conversion FFmpeg n’a produit aucune image à temps.".into());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_live_arguments_with_headers() {
        let headers = StreamHeaders {
            user_agent: Some("Agent".into()),
            referrer: Some("https://site.example/".into()),
        };
        let args = arguments(
            &Options {
                input: "http://h/live.ts",
                headers: &headers,
                live: true,
                copy_video: false,
                deinterlace: true,
            },
            Path::new("/tmp/job"),
        );
        let joined = args.join(" ");
        assert!(joined.contains("-user_agent Agent"));
        assert!(joined.contains("Referer: https://site.example/\r\n"));
        assert!(joined.contains("-c:v h264_videotoolbox"));
        assert!(joined.contains("yadif"));
        assert!(joined.contains("delete_segments"));
        assert!(joined.ends_with("/tmp/job/index.m3u8"));
        let local = arguments(
            &Options {
                input: "/tmp/film.mkv",
                headers: &StreamHeaders::default(),
                live: false,
                copy_video: true,
                deinterlace: false,
            },
            Path::new("/tmp/job"),
        );
        assert!(!local.contains(&"-user_agent".to_string()));
        assert!(local.join(" ").contains("-c:v copy"));
        assert!(local.join(" ").contains("-hls_playlist_type event"));
    }
}
