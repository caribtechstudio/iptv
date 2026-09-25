//! Records live streams to `~/Movies/Fluxo`: HLS segments are appended as they are published
//! (TS or fragmented MP4), other streams are copied as received. Encrypted HLS uses FFmpeg.

use crate::{
    net::{self, StreamHeaders},
    transcode,
};
use iptv_core::hls;
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};
use url::Url;

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingInfo {
    pub id: String,
    pub name: String,
    pub path: String,
    pub started_at: i64,
    pub bytes: u64,
    pub active: bool,
    pub error: Option<String>,
    /// Stream address being recorded (credentials hidden), to match the current channel.
    pub source: String,
}

struct Recording {
    info: Mutex<RecordingInfo>,
    bytes: AtomicU64,
    stop: AtomicBool,
}

pub struct Recorder {
    dir: PathBuf,
    recordings: Mutex<HashMap<String, Arc<Recording>>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingFile {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub modified: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recordings {
    pub dir: String,
    pub active: Vec<RecordingInfo>,
    pub files: Vec<RecordingFile>,
}

fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() || matches!(ch, ' ' | '-' | '_' | '(' | ')') {
                ch
            } else {
                ' '
            }
        })
        .collect();
    let cleaned = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned: String = cleaned.chars().take(80).collect();
    if cleaned.is_empty() {
        "Enregistrement".into()
    } else {
        cleaned
    }
}

impl Recorder {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            recordings: Mutex::new(HashMap::new()),
        }
    }

    pub fn start(
        &self,
        app: AppHandle,
        url: String,
        headers: StreamHeaders,
        name: String,
    ) -> Result<RecordingInfo, String> {
        let parsed = Url::parse(&url).map_err(|_| "Adresse du flux invalide.")?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("Seuls les flux en ligne peuvent être enregistrés.".into());
        }
        if let Ok(recordings) = self.recordings.lock()
            && recordings.values().any(|r| {
                r.info
                    .lock()
                    .is_ok_and(|info| info.active && info.source == net::redact(&url))
            })
        {
            return Err("Cette chaîne est déjà en cours d’enregistrement.".into());
        }
        fs::create_dir_all(&self.dir)
            .map_err(|_| "Impossible de créer le dossier des enregistrements.")?;
        let stamp = chrono::Local::now().format("%Y-%m-%d %Hh%M").to_string();
        let base = format!("{} {stamp}", safe_name(&name));
        let id = net::random_token();
        let info = RecordingInfo {
            id: id.clone(),
            name,
            path: self
                .dir
                .join(format!("{base}.ts"))
                .to_string_lossy()
                .into_owned(),
            started_at: chrono::Utc::now().timestamp(),
            bytes: 0,
            active: true,
            error: None,
            source: net::redact(&url),
        };
        let recording = Arc::new(Recording {
            info: Mutex::new(info.clone()),
            bytes: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });
        self.recordings
            .lock()
            .map_err(|e| e.to_string())?
            .insert(id, recording.clone());
        let dir = self.dir.clone();
        let _ = app.emit("recording", info.clone());
        std::thread::spawn(move || {
            let result = record(&recording, &dir, &base, &url, &headers);
            if let Ok(mut info) = recording.info.lock() {
                info.active = false;
                info.bytes = recording.bytes.load(Ordering::SeqCst);
                if let Err(error) = result {
                    info.error = Some(error);
                }
                let _ = app.emit("recording", info.clone());
            }
        });
        Ok(info)
    }

    pub fn stop(&self, id: &str) {
        if let Ok(recordings) = self.recordings.lock()
            && let Some(recording) = recordings.get(id)
        {
            recording.stop.store(true, Ordering::SeqCst);
        }
    }

    pub fn list(&self) -> Recordings {
        let active = self
            .recordings
            .lock()
            .map(|recordings| {
                recordings
                    .values()
                    .filter_map(|recording| {
                        let mut info = recording.info.lock().ok()?.clone();
                        info.bytes = recording.bytes.load(Ordering::SeqCst);
                        Some(info)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut files: Vec<RecordingFile> = fs::read_dir(&self.dir)
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| {
                        let path = entry.path();
                        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
                        if !matches!(ext.as_str(), "ts" | "mp4" | "mkv") {
                            return None;
                        }
                        let meta = entry.metadata().ok()?;
                        Some(RecordingFile {
                            name: path.file_stem()?.to_string_lossy().into_owned(),
                            path: path.to_string_lossy().into_owned(),
                            bytes: meta.len(),
                            modified: meta
                                .modified()
                                .ok()
                                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|d| d.as_secs() as i64)
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort_by_key(|file| std::cmp::Reverse(file.modified));
        Recordings {
            dir: self.dir.to_string_lossy().into_owned(),
            active,
            files,
        }
    }
}

fn set_path(recording: &Recording, path: &Path) {
    if let Ok(mut info) = recording.info.lock() {
        info.path = path.to_string_lossy().into_owned();
    }
}

fn copy_body(
    recording: &Recording,
    mut reader: impl Read,
    file: &mut fs::File,
) -> Result<(), String> {
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        if recording.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let read = reader
            .read(&mut buffer)
            .map_err(|_| "Le flux s’est interrompu.".to_string())?;
        if read == 0 {
            return Ok(());
        }
        file.write_all(&buffer[..read])
            .map_err(|_| "Écriture de l’enregistrement impossible (disque plein ?).".to_string())?;
        recording.bytes.fetch_add(read as u64, Ordering::SeqCst);
    }
}

fn record(
    recording: &Recording,
    dir: &Path,
    base: &str,
    url: &str,
    headers: &StreamHeaders,
) -> Result<(), String> {
    let client = net::client(Some(Duration::from_secs(30)))?;
    let stream_client = net::client(None)?;
    let response = headers
        .apply(stream_client.get(url))
        .send()
        .map_err(|error| net::transport_message(&error))?;
    if !response.status().is_success() {
        return Err(net::status_message(
            response.status().as_u16(),
            net::deny_reason(&response).as_deref(),
        ));
    }
    let mut reader = std::io::BufReader::new(response);
    let is_playlist = {
        use std::io::BufRead;
        let buffer = reader.fill_buf().map_err(|e| e.to_string())?;
        buffer.starts_with(b"#EXTM3U") || buffer.starts_with("\u{feff}#EXTM3U".as_bytes())
    };
    if !is_playlist {
        let path = dir.join(format!("{base}.ts"));
        set_path(recording, &path);
        let mut file = fs::File::create(&path).map_err(|e| e.to_string())?;
        return copy_body(recording, reader, &mut file);
    }
    let mut text = String::new();
    reader
        .take(4 * 1024 * 1024)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    let mut media_url = Url::parse(url).map_err(|e| e.to_string())?;
    let mut separate_audio = false;
    if hls::is_master(&text) {
        separate_audio = !hls::audio_renditions(&text, &media_url).is_empty();
        let best = hls::variants(&text, &media_url)
            .into_iter()
            .max_by_key(|variant| (variant.height, variant.bandwidth));
        media_url = Url::parse(&best.ok_or("Aucune variante à enregistrer.")?.uri)
            .map_err(|e| e.to_string())?;
        text = fetch_text(&client, headers, &media_url)?.1;
    }
    if hls::key_method(&text).is_some() || separate_audio {
        return record_with_ffmpeg(recording, dir, base, url, headers);
    }
    let fmp4 = hls::map_uri(&text, &media_url);
    let path = dir.join(format!(
        "{base}.{}",
        if fmp4.is_some() { "mp4" } else { "ts" }
    ));
    set_path(recording, &path);
    let mut file = fs::File::create(&path).map_err(|e| e.to_string())?;
    if let Some(init) = fmp4 {
        let body = headers
            .apply(client.get(&init))
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(|e| net::transport_message(&e))?;
        copy_body(recording, body, &mut file)?;
    }
    let mut done: HashSet<u64> = HashSet::new();
    let mut last_new = Instant::now();
    loop {
        let (final_url, playlist) = fetch_text(&client, headers, &media_url)?;
        let target = hls::target_duration(&playlist)
            .unwrap_or(6.0)
            .clamp(1.0, 20.0);
        for segment in hls::segments(&playlist, &final_url) {
            if recording.stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            if !done.insert(segment.sequence) {
                continue;
            }
            last_new = Instant::now();
            match headers
                .apply(client.get(&segment.uri))
                .send()
                .and_then(|r| r.error_for_status())
            {
                Ok(body) => copy_body(recording, body, &mut file)?,
                Err(_) => continue,
            }
        }
        if hls::is_endlist(&playlist) || recording.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        if last_new.elapsed() > Duration::from_secs(60) {
            return Err("Le flux ne publie plus de nouveaux segments.".into());
        }
        let wait = Instant::now() + Duration::from_secs_f64(target / 2.0);
        while Instant::now() < wait {
            if recording.stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

fn fetch_text(
    client: &reqwest::blocking::Client,
    headers: &StreamHeaders,
    url: &Url,
) -> Result<(Url, String), String> {
    let response = headers
        .apply(client.get(url.clone()))
        .send()
        .map_err(|e| net::transport_message(&e))?;
    if !response.status().is_success() {
        return Err(net::status_message(
            response.status().as_u16(),
            net::deny_reason(&response).as_deref(),
        ));
    }
    let final_url = response.url().clone();
    let bytes = net::read_prefix(response, 4 * 1024 * 1024).map_err(|e| e.to_string())?;
    Ok((final_url, String::from_utf8_lossy(&bytes).into_owned()))
}

fn record_with_ffmpeg(
    recording: &Recording,
    dir: &Path,
    base: &str,
    url: &str,
    headers: &StreamHeaders,
) -> Result<(), String> {
    let ffmpeg = transcode::ffmpeg_path().ok_or("Ce flux est chiffré ou a un audio séparé : son enregistrement nécessite FFmpeg (brew install ffmpeg).")?;
    let path = dir.join(format!("{base}.ts"));
    set_path(recording, &path);
    let mut args = vec![
        "-hide_banner".to_owned(),
        "-loglevel".into(),
        "error".into(),
        "-user_agent".into(),
        headers.user_agent(),
    ];
    if let Some(referrer) = headers.referrer() {
        args.extend(["-headers".into(), format!("Referer: {referrer}\r\n")]);
    }
    args.extend([
        "-i".into(),
        url.into(),
        "-map".into(),
        "0".into(),
        "-c".into(),
        "copy".into(),
        "-f".into(),
        "mpegts".into(),
        "-y".into(),
    ]);
    args.push(path.to_string_lossy().into_owned());
    let mut child = Command::new(ffmpeg)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("FFmpeg n’a pas pu démarrer : {e}"))?;
    loop {
        if let Ok(meta) = fs::metadata(&path) {
            recording.bytes.store(meta.len(), Ordering::SeqCst);
        }
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err("FFmpeg a interrompu l’enregistrement.".into())
            };
        }
        if recording.stop.load(Ordering::SeqCst) {
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(b"q");
            }
            std::thread::sleep(Duration::from_secs(2));
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::safe_name;

    #[test]
    fn file_names_are_safe() {
        assert_eq!(safe_name("TF1 / HD: \"live\""), "TF1 HD live");
        assert_eq!(safe_name("///"), "Enregistrement");
    }
}
