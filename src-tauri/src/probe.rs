//! Stream probe: follows master → variant → segment with the player's headers, reads the real
//! codecs and decides which playback engine can handle the source.

use crate::net::{self, StreamHeaders};
use iptv_core::{hls, ts};
use serde::Serialize;
use std::time::Duration;
use url::Url;

const MAX_PLAYLIST: u64 = 2 * 1024 * 1024;
const MAX_SEGMENT_PREFIX: u64 = 400 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    /// WebKit plays the address directly (through the relay if headers are needed).
    #[default]
    Native,
    /// Raw MPEG-TS: repackaged into HLS by the local segmenter.
    Segmenter,
    /// Codecs or containers macOS cannot decode: FFmpeg is required.
    Transcode,
    /// Nothing can play it (server error, DRM, web page…).
    None,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    pub kind: &'static str,
    pub ok: bool,
    pub status: Option<u16>,
    pub message: Option<String>,
    pub video: Vec<String>,
    pub audio: Vec<String>,
    /// The stream was inspected and carries no audio track at all.
    pub audio_missing: bool,
    pub encryption: Option<String>,
    pub fmp4: bool,
    pub engine: Engine,
    /// Codecs detected that WebKit cannot decode.
    pub unsupported: Vec<String>,
    pub steps: Vec<String>,
}

impl Probe {
    fn fail(mut self, status: Option<u16>, message: String) -> Self {
        self.ok = false;
        self.status = status;
        self.message = Some(message);
        self.engine = Engine::None;
        if self.kind.is_empty() {
            self.kind = "error";
        }
        self
    }
}

struct Fetched {
    url: Url,
    content_type: String,
    body: Vec<u8>,
}

fn fetch(
    client: &reqwest::blocking::Client,
    url: &str,
    headers: &StreamHeaders,
    limit: u64,
) -> Result<Fetched, (Option<u16>, String)> {
    let response = headers
        .apply(client.get(url))
        .send()
        .map_err(|error| (None, net::transport_message(&error)))?;
    let status = response.status();
    if !status.is_success() {
        return Err((
            Some(status.as_u16()),
            net::status_message(status.as_u16(), net::deny_reason(&response).as_deref()),
        ));
    }
    let url = response.url().clone();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let body = net::read_prefix(response, limit).map_err(|error| {
        if error.to_string().to_ascii_lowercase().contains("timed out") {
            (
                None,
                "Le serveur a cessé d’envoyer des données (délai dépassé).".to_owned(),
            )
        } else {
            (None, "La réponse du serveur a été interrompue.".to_owned())
        }
    })?;
    Ok(Fetched {
        url,
        content_type,
        body,
    })
}

/// Codec names from an HLS `CODECS` attribute.
pub fn codecs_from_attribute(value: &str) -> (Vec<String>, Vec<String>) {
    let mut video = Vec::new();
    let mut audio = Vec::new();
    for codec in value.split(',').map(|c| c.trim().to_ascii_lowercase()) {
        let (name, is_video) = if codec.starts_with("avc1") || codec.starts_with("avc3") {
            ("h264", true)
        } else if codec.starts_with("hvc1") || codec.starts_with("hev1") {
            ("hevc", true)
        } else if codec.starts_with("dvh1") || codec.starts_with("dvhe") {
            ("dolby-vision", true)
        } else if codec.starts_with("av01") {
            ("av1", true)
        } else if codec.starts_with("vp09") {
            ("vp9", true)
        } else if codec == "mp4a.40.34" || codec == "mp4a.6b" || codec == "mp4a.69" {
            ("mpeg-audio", false)
        } else if codec.starts_with("mp4a") {
            ("aac", false)
        } else if codec == "ac-3" {
            ("ac3", false)
        } else if codec == "ec-3" {
            ("eac3", false)
        } else if codec.starts_with("opus") {
            ("opus", false)
        } else if codec.starts_with("fLaC") || codec.starts_with("flac") {
            ("flac", false)
        } else {
            continue;
        };
        let list = if is_video { &mut video } else { &mut audio };
        if !list.iter().any(|known| known == name) {
            list.push(name.to_owned());
        }
    }
    (video, audio)
}

/// Codecs WebKit on macOS cannot decode inside the given container.
pub fn unsupported_codecs(video: &[String], audio: &[String], in_ts: bool) -> Vec<String> {
    let mut result = Vec::new();
    for codec in video {
        let bad = match codec.as_str() {
            "mpeg2video" | "mpeg1video" | "vp9" | "av1" => true,
            // AVFoundation only accepts HEVC in fragmented MP4 HLS.
            "hevc" => in_ts,
            _ => false,
        };
        if bad {
            result.push(codec.clone());
        }
    }
    for codec in audio {
        if matches!(codec.as_str(), "aac-latm" | "opus" | "flac") {
            result.push(codec.clone());
        }
    }
    result
}

fn apply_ts(probe: &mut Probe, data: &[u8]) -> bool {
    let Some(streams) = ts::stream_info(data) else {
        return false;
    };
    for stream in streams {
        let list = match stream.kind {
            ts::StreamKind::Video => &mut probe.video,
            ts::StreamKind::Audio => &mut probe.audio,
            ts::StreamKind::Other => continue,
        };
        if !list.iter().any(|codec| codec == stream.codec) {
            list.push(stream.codec.to_owned());
        }
    }
    true
}

fn finish_codecs(mut probe: Probe, in_ts: bool, raw_ts: bool) -> Probe {
    probe.unsupported = unsupported_codecs(&probe.video, &probe.audio, in_ts);
    probe.ok = true;
    probe.engine = if !probe.unsupported.is_empty() {
        probe.message = Some(format!(
            "Ce flux utilise un format que macOS ne décode pas ({}).",
            probe.unsupported.join(", ")
        ));
        Engine::Transcode
    } else if raw_ts {
        Engine::Segmenter
    } else {
        Engine::Native
    };
    if probe.audio_missing && probe.message.is_none() {
        probe.message = Some(
            "Cette chaîne ne diffuse aucune piste audio : l’absence de son vient de la source."
                .into(),
        );
    }
    probe
}

pub fn probe(url: &str, headers: &StreamHeaders) -> Probe {
    let mut probe = Probe::default();
    let Ok(parsed) = Url::parse(url) else {
        return probe.fail(None, "Adresse du flux invalide.".into());
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return probe.fail(None, "La sonde requiert une adresse HTTP ou HTTPS.".into());
    }
    let client = match net::client(Some(Duration::from_secs(10))) {
        Ok(client) => client,
        Err(error) => return probe.fail(None, error),
    };
    let first = match fetch(&client, url, headers, MAX_PLAYLIST) {
        Ok(fetched) => fetched,
        Err((status, message)) => return probe.fail(status, message),
    };
    probe.steps.push(format!(
        "Adresse : réponse reçue ({} octets lus)",
        first.body.len()
    ));

    if ts::looks_like_ts(&first.body) {
        probe.kind = "ts";
        probe.steps.push("Flux MPEG-TS brut (non HLS)".into());
        if !apply_ts(&mut probe, &first.body) {
            probe
                .steps
                .push("Table des pistes absente du début du flux".into());
        }
        probe.audio_missing = !probe.video.is_empty() && probe.audio.is_empty();
        return finish_codecs(probe, true, true);
    }
    let text = String::from_utf8_lossy(&first.body);
    if hls::is_playlist(&text) {
        probe.kind = "hls";
        return probe_hls(probe, &client, headers, first.url, &text);
    }
    let head =
        String::from_utf8_lossy(&first.body[..first.body.len().min(2048)]).to_ascii_lowercase();
    if head.contains("<mpd") || first.content_type.contains("dash+xml") {
        probe.kind = "dash";
        probe.ok = true;
        probe.engine = Engine::Transcode;
        probe.message = Some(
            "Flux MPEG-DASH : macOS ne le lit qu’avec le moteur de compatibilité FFmpeg.".into(),
        );
        return probe;
    }
    if head.contains("<html")
        || head.contains("<!doctype html")
        || first.content_type.contains("text/html")
    {
        probe.kind = "html";
        return probe.fail(
            None,
            "Cette adresse renvoie une page web, pas un flux vidéo.".into(),
        );
    }
    if first
        .body
        .get(4..8)
        .is_some_and(|b| b == b"ftyp" || b == b"styp" || b == b"moof")
    {
        probe.kind = "mp4";
    } else if first.content_type.starts_with("audio/") {
        probe.kind = "audio";
    } else if first.body.starts_with(b"\x1aE\xdf\xa3") {
        probe.kind = "mkv";
        probe.ok = true;
        probe.engine = Engine::Transcode;
        probe.message = Some(
            "Conteneur Matroska (MKV) : lecture via le moteur de compatibilité FFmpeg.".into(),
        );
        return probe;
    } else {
        probe.kind = "other";
    }
    probe.ok = true;
    probe.engine = Engine::Native;
    probe
}

fn probe_hls(
    mut probe: Probe,
    client: &reqwest::blocking::Client,
    headers: &StreamHeaders,
    base: Url,
    text: &str,
) -> Probe {
    let mut media_text = text.to_owned();
    let mut media_base = base.clone();
    let mut declared_audio = false;
    if hls::is_master(text) {
        let variants = hls::variants(text, &base);
        let Some(variant) = variants.first() else {
            return probe.fail(
                None,
                "La playlist HLS n’annonce aucune variante lisible.".into(),
            );
        };
        probe.steps.push(format!(
            "Playlist principale : {} variante(s)",
            variants.len()
        ));
        if let Some(codecs) = &variant.codecs {
            let (video, audio) = codecs_from_attribute(codecs);
            declared_audio = !audio.is_empty();
            probe.video = video;
            probe.audio = audio;
        }
        for rendition in hls::audio_renditions(text, &base).into_iter().take(1) {
            declared_audio = true;
            if let Err((status, message)) = fetch(client, &rendition, headers, MAX_PLAYLIST) {
                probe
                    .steps
                    .push(format!("Piste audio séparée inaccessible : {message}"));
                let mut probe = probe.fail(
                    status,
                    format!("La piste audio de ce flux est inaccessible. {message}"),
                );
                probe.kind = "hls";
                return probe;
            }
            probe.steps.push("Piste audio séparée accessible".into());
        }
        match fetch(client, &variant.uri, headers, MAX_PLAYLIST) {
            Ok(fetched) => {
                media_text = String::from_utf8_lossy(&fetched.body).into_owned();
                media_base = fetched.url;
                probe.steps.push("Variante : playlist reçue".into());
            }
            Err((status, message)) => {
                let mut probe = probe.fail(
                    status,
                    format!("La playlist principale répond, mais pas la variante vidéo. {message}"),
                );
                probe.kind = "hls";
                return probe;
            }
        }
    }
    if !hls::is_playlist(&media_text) {
        return probe.fail(
            None,
            "Le serveur répond, mais ne renvoie pas une playlist vidéo HLS valide.".into(),
        );
    }
    if let Some(method) = hls::key_method(&media_text) {
        probe.steps.push(format!("Chiffrement : {method}"));
        if method.starts_with("SAMPLE-AES") {
            probe.encryption = Some(method);
            let mut probe = probe.fail(
                None,
                "Ce flux est protégé par DRM : sa lecture exige une licence du fournisseur.".into(),
            );
            probe.kind = "hls";
            return probe;
        }
        probe.encryption = Some(method);
    }
    probe.fmp4 = hls::map_uri(&media_text, &media_base).is_some();
    let segments = hls::segments(&media_text, &media_base);
    let Some(segment) = segments.iter().rev().nth(1).or(segments.last()) else {
        let mut probe = probe.fail(
            None,
            "La playlist HLS ne contient aucun segment : la chaîne est peut-être hors antenne."
                .into(),
        );
        probe.kind = "hls";
        return probe;
    };
    if probe.encryption.is_some() || probe.fmp4 {
        // Segments are encrypted or fragmented MP4: trust the declared codecs.
        match fetch(client, &segment.uri, headers, 1024) {
            Ok(_) => probe.steps.push("Segment accessible".into()),
            Err((status, message)) => {
                let mut probe = probe.fail(
                    status,
                    format!("La playlist répond, mais les segments vidéo sont refusés. {message}"),
                );
                probe.kind = "hls";
                return probe;
            }
        }
        let in_ts = !probe.fmp4;
        return finish_codecs(probe, in_ts, false);
    }
    match fetch(client, &segment.uri, headers, MAX_SEGMENT_PREFIX) {
        Ok(fetched) => {
            probe
                .steps
                .push(format!("Segment : {} octets analysés", fetched.body.len()));
            let declared_video = std::mem::take(&mut probe.video);
            let declared = std::mem::take(&mut probe.audio);
            if apply_ts(&mut probe, &fetched.body) {
                if probe.audio.is_empty() && !declared_audio {
                    probe.audio_missing = !probe.video.is_empty();
                }
                if probe.audio.is_empty() {
                    probe.audio = declared;
                }
            } else {
                probe.video = declared_video;
                probe.audio = declared;
            }
            finish_codecs(probe, ts::looks_like_ts(&fetched.body), false)
        }
        Err((status, message)) => {
            let mut probe = probe.fail(
                status,
                format!("La playlist répond, mais les segments vidéo sont refusés. {message}"),
            );
            probe.kind = "hls";
            probe
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Write};
    use std::net::TcpListener;

    pub fn serve(routes: Vec<(&'static str, String, Vec<u8>)>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                    line.clear();
                }
                let path = first.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let (head, body) = routes
                    .iter()
                    .find(|(route, _, _)| *route == path)
                    .map(|(_, head, body)| (head.clone(), body.clone()))
                    .unwrap_or(("HTTP/1.1 404 Not Found".into(), Vec::new()));
                let _ = stream.write_all(
                    format!(
                        "{head}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.write_all(&body);
            }
        });
        format!("http://{address}")
    }

    fn ts_sample(streams: &[(u8, u16, &[u8])]) -> Vec<u8> {
        let mut data = iptv_core_test_pat();
        data.extend(iptv_core_test_pmt(streams));
        data
    }

    fn iptv_core_test_pat() -> Vec<u8> {
        let mut p = vec![
            0x47, 0x40, 0x00, 0x10, 0, 0x00, 0xb0, 13, 0, 1, 0xc1, 0, 0, 0, 1, 0xe1, 0x00, 0, 0, 0,
            0,
        ];
        p.resize(188, 0xff);
        p
    }

    fn iptv_core_test_pmt(streams: &[(u8, u16, &[u8])]) -> Vec<u8> {
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
        let mut p = vec![0x47, 0x41, 0x00, 0x10, 0, 0x02, 0xb0, length as u8];
        p.extend(body);
        p.extend([0, 0, 0, 0]);
        p.resize(188, 0xff);
        p
    }

    #[test]
    fn follows_variant_and_reads_segment_codecs() {
        let base = serve(vec![
            ("/index.m3u8", "HTTP/1.1 200 OK".into(), b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.64,mp4a.40.2\"\nv/mono.m3u8\n".to_vec()),
            ("/v/mono.m3u8", "HTTP/1.1 200 OK".into(), b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\na.ts\n#EXTINF:6,\nb.ts\n".to_vec()),
            ("/v/a.ts", "HTTP/1.1 200 OK".into(), ts_sample(&[(0x1b, 0x101, &[]), (0x0f, 0x102, &[])])),
            ("/mpeg2.m3u8", "HTTP/1.1 200 OK".into(), b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nm.ts\n#EXTINF:6,\nm.ts\n".to_vec()),
            ("/m.ts", "HTTP/1.1 200 OK".into(), ts_sample(&[(0x02, 0x101, &[])])),
            ("/raw", "HTTP/1.1 200 OK".into(), ts_sample(&[(0x1b, 0x101, &[]), (0x03, 0x102, &[])])),
            ("/blocked.m3u8", "HTTP/1.1 403 Forbidden\r\nX-Deny-Reason: deny_backend".into(), Vec::new()),
            ("/segments403.m3u8", "HTTP/1.1 200 OK".into(), b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nnope.ts\n".to_vec()),
            ("/page", "HTTP/1.1 200 OK\r\nContent-Type: text/html".into(), b"<!DOCTYPE html><html></html>".to_vec()),
        ]);
        let headers = StreamHeaders::default();

        let ok = probe(&format!("{base}/index.m3u8"), &headers);
        assert!(ok.ok, "{ok:?}");
        assert_eq!(ok.video, vec!["h264"]);
        assert_eq!(ok.audio, vec!["aac"]);
        assert_eq!(ok.engine, Engine::Native);

        let mpeg2 = probe(&format!("{base}/mpeg2.m3u8"), &headers);
        assert_eq!(mpeg2.engine, Engine::Transcode);
        assert!(mpeg2.audio_missing);

        let raw = probe(&format!("{base}/raw"), &headers);
        assert_eq!((raw.kind, raw.engine), ("ts", Engine::Segmenter));

        let blocked = probe(&format!("{base}/blocked.m3u8"), &headers);
        assert_eq!(blocked.status, Some(403));
        assert_eq!(
            blocked.message.as_deref(),
            Some("Le serveur bloque l’accès à cette chaîne (HTTP 403 : protection du flux).")
        );

        let segments = probe(&format!("{base}/segments403.m3u8"), &headers);
        assert_eq!(segments.status, Some(404));
        assert!(
            segments
                .message
                .unwrap()
                .starts_with("La playlist répond, mais les segments")
        );

        let page = probe(&format!("{base}/page"), &headers);
        assert_eq!((page.kind, page.engine), ("html", Engine::None));
    }

    #[test]
    fn declared_codecs() {
        let (video, audio) = codecs_from_attribute("hvc1.1.6.L120.B0,mp4a.40.2,ec-3");
        assert_eq!(video, vec!["hevc"]);
        assert_eq!(audio, vec!["aac", "eac3"]);
        assert_eq!(unsupported_codecs(&video, &audio, true), vec!["hevc"]);
        assert!(unsupported_codecs(&video, &audio, false).is_empty());
    }
}
