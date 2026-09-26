//! Filmstrip of a video: one picture per position, taken by FFmpeg with a fast keyframe seek.
//! Pictures are sent to the page one by one (`thumbnail` events) as soon as they are ready, a
//! few at a time, and a newer request makes the older one stop.

use crate::{
    net::StreamHeaders,
    transcode::{clean_log_line, ffmpeg_path},
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};

const WORKERS: usize = 4;
const MAX_PICTURES: usize = 60;
const LOCAL_TIMEOUT: Duration = Duration::from_secs(12);
const REMOTE_TIMEOUT: Duration = Duration::from_secs(25);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// Identifier chosen by the page: events carry it so late pictures can be ignored.
    pub request: u64,
    /// Local path, `file:` URL or HTTP(S) address of a video with a known duration.
    pub source: String,
    pub times: Vec<f64>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub headers: StreamHeaders,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Picture {
    request: u64,
    index: usize,
    time: f64,
    /// `data:image/jpeg;base64,…`, absent when this position could not be read.
    image: Option<String>,
    done: bool,
}

#[derive(Default)]
pub struct Thumbnails {
    current: Arc<AtomicU64>,
}

enum Input {
    File(PathBuf),
    Remote(String),
}

fn input(source: &str) -> Result<Input, String> {
    if source.starts_with("http://") || source.starts_with("https://") {
        return Ok(Input::Remote(source.to_owned()));
    }
    let path = if source.starts_with("file:") {
        url::Url::parse(source)
            .ok()
            .and_then(|url| url.to_file_path().ok())
            .ok_or("Adresse de fichier invalide.")?
    } else {
        PathBuf::from(source)
    };
    if path.is_file() {
        Ok(Input::File(path))
    } else {
        Err("Fichier introuvable.".into())
    }
}

pub fn arguments(
    input: &str,
    remote: bool,
    headers: &StreamHeaders,
    time: f64,
    height: u32,
) -> Vec<String> {
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-loglevel", "error"]
        .map(String::from)
        .to_vec();
    if remote {
        args.extend(["-user_agent".into(), headers.user_agent()]);
        if let Some(referrer) = headers.referrer() {
            args.extend(["-headers".into(), format!("Referer: {referrer}\r\n")]);
        }
    }
    // `-ss` before `-i`: FFmpeg jumps to the nearest keyframe instead of decoding up to it.
    args.extend([
        "-ss".into(),
        format!("{:.3}", time.max(0.0)),
        "-i".into(),
        input.into(),
        "-map".into(),
        "0:V:0".into(),
        "-frames:v".into(),
        "1".into(),
        "-an".into(),
        "-sn".into(),
        "-dn".into(),
        "-vf".into(),
        format!("scale=-2:{height}:flags=bilinear"),
        "-q:v".into(),
        "6".into(),
        "-f".into(),
        "image2pipe".into(),
        "-c:v".into(),
        "mjpeg".into(),
        "pipe:1".into(),
    ]);
    args
}

/// One JPEG picture, or an error after `timeout` (remote sources can be slow to seek).
fn capture(ffmpeg: &PathBuf, args: &[String], timeout: Duration) -> Result<Vec<u8>, String> {
    let mut child = Command::new(ffmpeg)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("FFmpeg n’a pas pu démarrer : {error}"))?;
    let mut stdout = child.stdout.take().ok_or("Sortie FFmpeg indisponible.")?;
    let mut stderr = child.stderr.take().ok_or("Sortie FFmpeg indisponible.")?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        bytes
    });
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = reader.join().unwrap_or_default();
    let log = errors.join().unwrap_or_default();
    match status {
        Some(status) if status.success() && bytes.starts_with(&[0xff, 0xd8]) => Ok(bytes),
        None => Err("Délai dépassé.".into()),
        _ => Err(log
            .lines()
            .rev()
            .map(clean_log_line)
            .find(|line| !line.is_empty())
            .unwrap_or("Image illisible.")
            .to_owned()),
    }
}

impl Thumbnails {
    /// Starts taking the pictures; the previous request, if any, stops.
    pub fn start(&self, app: &AppHandle, request: Request) -> Result<(), String> {
        let ffmpeg = ffmpeg_path().ok_or("Les vignettes nécessitent FFmpeg.")?;
        let input = input(&request.source)?;
        let id = request.request;
        self.current.store(id, Ordering::Release);
        let (source, remote, timeout) = match input {
            Input::File(path) => (path.to_string_lossy().into_owned(), false, LOCAL_TIMEOUT),
            Input::Remote(url) => (url, true, REMOTE_TIMEOUT),
        };
        let height = request.height.unwrap_or(90).clamp(32, 360);
        let times: Vec<f64> = request
            .times
            .into_iter()
            .filter(|time| time.is_finite() && *time >= 0.0)
            .take(MAX_PICTURES)
            .collect();
        let total = times.len();
        let jobs = Arc::new(Mutex::new(
            times.into_iter().enumerate().collect::<Vec<_>>(),
        ));
        let finished = Arc::new(AtomicUsize::new(0));
        let headers = request.headers;
        // One extra connection at most for online videos: many providers refuse a second one.
        let workers = if remote { 1 } else { WORKERS }.min(total.max(1));
        for _ in 0..workers {
            let (app, jobs, finished, current) = (
                app.clone(),
                jobs.clone(),
                finished.clone(),
                self.current.clone(),
            );
            let (ffmpeg, source, headers) = (ffmpeg.clone(), source.clone(), headers.clone());
            std::thread::Builder::new()
                .name("fluxo-thumbnails".into())
                .spawn(move || {
                    loop {
                        if current.load(Ordering::Acquire) != id {
                            return;
                        }
                        let next = jobs
                            .lock()
                            .ok()
                            .and_then(|mut jobs| (!jobs.is_empty()).then(|| jobs.remove(0)));
                        let Some((index, time)) = next else {
                            return;
                        };
                        let args = arguments(&source, remote, &headers, time, height);
                        let image = capture(&ffmpeg, &args, timeout).ok().map(|bytes| {
                            format!(
                                "data:image/jpeg;base64,{}",
                                base64::engine::general_purpose::STANDARD.encode(bytes)
                            )
                        });
                        if current.load(Ordering::Acquire) != id {
                            return;
                        }
                        let done = finished.fetch_add(1, Ordering::AcqRel) + 1 == total;
                        let _ = app.emit(
                            "thumbnail",
                            Picture {
                                request: id,
                                index,
                                time,
                                image,
                                done,
                            },
                        );
                    }
                })
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Stops the running request (the filmstrip was closed or the video changed).
    pub fn cancel(&self) {
        self.current.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeks_before_opening_and_sends_the_provider_headers() {
        let headers = StreamHeaders {
            user_agent: Some("Agent".into()),
            referrer: Some("https://site.example/".into()),
        };
        let args = arguments("https://h/film.mp4", true, &headers, 12.3456, 72).join(" ");
        assert!(args.contains("-user_agent Agent"));
        assert!(args.contains("-ss 12.346 -i https://h/film.mp4"));
        assert!(args.contains("scale=-2:72"));
        assert!(args.ends_with("-f image2pipe -c:v mjpeg pipe:1"));
        let local = arguments("/tmp/film.mkv", false, &StreamHeaders::default(), 0.0, 72).join(" ");
        assert!(!local.contains("-user_agent"));
    }

    #[test]
    fn captures_a_picture_of_a_real_file() {
        let Some(ffmpeg) = ffmpeg_path() else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("fluxo-thumbs-{}", crate::net::random_token()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clip.mp4");
        let made = Command::new(&ffmpeg)
            .args([
                "-hide_banner",
                "-nostdin",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=25:duration=3",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&clip)
            .status();
        if !made.is_ok_and(|status| status.success()) {
            return;
        }
        let args = arguments(
            &clip.to_string_lossy(),
            false,
            &StreamHeaders::default(),
            1.5,
            72,
        );
        let jpeg = capture(&ffmpeg, &args, LOCAL_TIMEOUT).unwrap();
        assert!(jpeg.starts_with(&[0xff, 0xd8]));
        assert!(input(&clip.to_string_lossy()).is_ok());
        assert!(input("/nowhere/film.mkv").is_err());
        let _ = std::fs::remove_dir_all(dir);
    }
}
