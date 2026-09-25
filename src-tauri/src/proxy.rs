//! Local HTTP proxy on 127.0.0.1 used by the player:
//! - relays HLS with the headers a playlist requires (User-Agent, Referer) and rewrites URIs;
//! - serves the HLS produced by the segmenter and by FFmpeg;
//! - records upstream errors per session for diagnostics.
//!
//! Every URL carries a random token, so other local processes cannot use it as an open proxy.

use crate::{
    net::{self, StreamHeaders},
    segmenter::Segmenter,
    transcode::Job,
};
use iptv_core::hls;
use serde::Serialize;
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use url::Url;

const MAX_PLAYLIST: u64 = 4 * 1024 * 1024;
const RELAY_IDLE: Duration = Duration::from_secs(15 * 60);
const WORKER_IDLE: Duration = Duration::from_secs(75);

pub enum Kind {
    Relay,
    Segmenter(Arc<Segmenter>),
    Transcode(Job),
}

pub struct Session {
    headers: StreamHeaders,
    kind: Kind,
    last_access: Mutex<Instant>,
    errors: Mutex<VecDeque<String>>,
}

impl Session {
    fn touch(&self) {
        if let Ok(mut last) = self.last_access.lock() {
            *last = Instant::now();
        }
    }

    fn record(&self, message: String) {
        if let Ok(mut errors) = self.errors.lock() {
            if errors.back() != Some(&message) {
                errors.push_back(message);
            }
            while errors.len() > 8 {
                errors.pop_front();
            }
        }
    }

    fn shutdown(&self) {
        match &self.kind {
            Kind::Relay => {}
            Kind::Segmenter(segmenter) => segmenter.stop(),
            Kind::Transcode(job) => job.stop(),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatus {
    pub alive: bool,
    pub errors: Vec<String>,
    pub failure: Option<String>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct OpenedStream {
    pub session: String,
    pub url: String,
}

pub struct Proxy {
    port: u16,
    token: String,
    work_dir: PathBuf,
    counter: AtomicU64,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    client: reqwest::blocking::Client,
}

fn hex_encode(value: &str) -> String {
    value.bytes().map(|byte| format!("{byte:02x}")).collect()
}

fn hex_decode(value: &str) -> Option<String> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(value.get(index..index + 2)?, 16).ok())
        .collect();
    String::from_utf8(bytes?).ok()
}

fn file_name(url: &Url) -> String {
    let name = url
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .unwrap_or("")
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
        .take(60)
        .collect::<String>();
    if name.is_empty() {
        "media".into()
    } else {
        name
    }
}

struct Request {
    method: String,
    path: String,
    range: Option<String>,
}

fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let mut range = None;
    let mut total = 0;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).ok()?;
        total += read;
        if read == 0 || line == "\r\n" || line == "\n" || total > 32 * 1024 {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("range")
        {
            range = Some(value.trim().to_owned());
        }
    }
    Some(Request {
        method,
        path,
        range,
    })
}

struct Reply<'a> {
    status: u16,
    content_type: &'a str,
    length: Option<u64>,
    extra: Vec<(String, String)>,
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        416 => "Range Not Satisfiable",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

fn write_head(stream: &mut TcpStream, reply: &Reply) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: Range\r\nAccess-Control-Expose-Headers: Content-Length, Content-Range\r\nConnection: close\r\n",
        reply.status,
        reason(reply.status),
        reply.content_type
    );
    if let Some(length) = reply.length {
        head.push_str(&format!("Content-Length: {length}\r\n"));
    }
    for (name, value) in &reply.extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &[u8], head_only: bool) {
    let reply = Reply {
        status,
        content_type,
        length: Some(body.len() as u64),
        extra: vec![("Cache-Control".into(), "no-cache".into())],
    };
    if write_head(stream, &reply).is_ok() && !head_only {
        let _ = stream.write_all(body);
    }
}

fn serve_file(stream: &mut TcpStream, path: &std::path::Path, content_type: &str, head_only: bool) {
    match fs::read(path) {
        Ok(bytes) => respond(stream, 200, content_type, &bytes, head_only),
        Err(_) => respond(stream, 404, "text/plain", b"", head_only),
    }
}

impl Proxy {
    pub fn start(work_dir: PathBuf) -> Result<Arc<Self>, String> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let _ = fs::remove_dir_all(&work_dir);
        fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
        let proxy = Arc::new(Self {
            port,
            token: net::random_token(),
            work_dir,
            counter: AtomicU64::new(1),
            sessions: Mutex::new(HashMap::new()),
            client: net::client(Some(Duration::from_secs(45)))?,
        });
        let server = proxy.clone();
        std::thread::Builder::new()
            .name("fluxo-proxy".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let proxy = server.clone();
                    let _ = std::thread::Builder::new()
                        .name("fluxo-proxy-conn".into())
                        .spawn(move || proxy.handle(stream));
                }
            })
            .map_err(|e| e.to_string())?;
        let janitor = proxy.clone();
        std::thread::Builder::new()
            .name("fluxo-proxy-janitor".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(10));
                    janitor.evict_idle();
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(proxy)
    }

    fn base(&self, session: &str) -> String {
        format!("http://127.0.0.1:{}/{}/{}", self.port, self.token, session)
    }

    fn relay_url(&self, session: &str, upstream: &Url) -> String {
        format!(
            "{}/r/{}/{}",
            self.base(session),
            hex_encode(upstream.as_str()),
            file_name(upstream)
        )
    }

    fn register(&self, headers: StreamHeaders, kind: Kind) -> String {
        let id = format!("s{}", self.counter.fetch_add(1, Ordering::SeqCst));
        let session = Arc::new(Session {
            headers,
            kind,
            last_access: Mutex::new(Instant::now()),
            errors: Mutex::new(VecDeque::new()),
        });
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(id.clone(), session);
        }
        id
    }

    fn next_dir(&self) -> PathBuf {
        self.work_dir.join(net::random_token())
    }

    pub fn open_relay(&self, url: &str, headers: StreamHeaders) -> Result<OpenedStream, String> {
        let upstream = Url::parse(url).map_err(|_| "Adresse du flux invalide.")?;
        if !matches!(upstream.scheme(), "http" | "https") {
            return Err("Le relais accepte seulement HTTP ou HTTPS.".into());
        }
        let session = self.register(headers, Kind::Relay);
        Ok(OpenedStream {
            url: self.relay_url(&session, &upstream),
            session,
        })
    }

    pub fn open_segmenter(
        &self,
        source: crate::segmenter::Source,
        live: bool,
    ) -> Result<OpenedStream, String> {
        let headers = match &source {
            crate::segmenter::Source::Http { headers, .. } => headers.clone(),
            crate::segmenter::Source::File(_) => StreamHeaders::default(),
        };
        let segmenter = Segmenter::start(source, live, self.next_dir())?;
        let session = self.register(headers, Kind::Segmenter(segmenter));
        Ok(OpenedStream {
            url: format!("{}/live.m3u8", self.base(&session)),
            session,
        })
    }

    pub fn open_transcode(
        &self,
        options: crate::transcode::Options,
    ) -> Result<OpenedStream, String> {
        let job = crate::transcode::launch(&options, || self.next_dir(), Duration::from_secs(30))?;
        let session = self.register(StreamHeaders::default(), Kind::Transcode(job));
        Ok(OpenedStream {
            url: format!("{}/f/index.m3u8", self.base(&session)),
            session,
        })
    }

    pub fn close(&self, id: &str) {
        let removed = self
            .sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(id));
        if let Some(session) = removed {
            session.shutdown();
        }
    }

    pub fn status(&self, id: &str) -> SessionStatus {
        let session = self
            .sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(id).cloned());
        match session {
            Some(session) => SessionStatus {
                alive: true,
                errors: session
                    .errors
                    .lock()
                    .map(|errors| errors.iter().cloned().collect())
                    .unwrap_or_default(),
                failure: match &session.kind {
                    Kind::Relay => None,
                    Kind::Segmenter(segmenter) => segmenter.error(),
                    Kind::Transcode(job) => job.failure(),
                },
            },
            None => SessionStatus {
                alive: false,
                errors: Vec::new(),
                failure: None,
            },
        }
    }

    fn evict_idle(&self) {
        let expired: Vec<Arc<Session>> = match self.sessions.lock() {
            Ok(mut sessions) => {
                let ids: Vec<String> = sessions
                    .iter()
                    .filter(|(_, session)| {
                        let idle = session
                            .last_access
                            .lock()
                            .map(|t| t.elapsed())
                            .unwrap_or_default();
                        idle > if matches!(session.kind, Kind::Relay) {
                            RELAY_IDLE
                        } else {
                            WORKER_IDLE
                        }
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
                ids.iter().filter_map(|id| sessions.remove(id)).collect()
            }
            Err(_) => Vec::new(),
        };
        for session in expired {
            session.shutdown();
        }
    }

    fn handle(&self, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(20)));
        let Some(request) = read_request(&stream) else {
            return;
        };
        let head_only = request.method == "HEAD";
        if request.method == "OPTIONS" {
            respond(&mut stream, 204, "text/plain", b"", true);
            return;
        }
        if request.method != "GET" && !head_only {
            respond(&mut stream, 400, "text/plain", b"", false);
            return;
        }
        let path = request.path.split('?').next().unwrap_or("");
        let parts: Vec<&str> = path.trim_start_matches('/').splitn(4, '/').collect();
        if parts.len() < 3 || parts[0] != self.token {
            respond(&mut stream, 403, "text/plain", b"", head_only);
            return;
        }
        let session = self
            .sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(parts[1]).cloned());
        let Some(session) = session else {
            respond(&mut stream, 404, "text/plain", b"session", head_only);
            return;
        };
        session.touch();
        let rest = parts[2..].join("/");
        match (&session.kind, rest.as_str()) {
            (Kind::Relay, rest) if rest.starts_with("r/") => {
                let encoded = rest[2..].split('/').next().unwrap_or("");
                match hex_decode(encoded).and_then(|url| Url::parse(&url).ok()) {
                    Some(upstream) if matches!(upstream.scheme(), "http" | "https") => self.relay(
                        &mut stream,
                        parts[1],
                        &session,
                        &upstream,
                        request.range,
                        head_only,
                    ),
                    _ => respond(&mut stream, 400, "text/plain", b"", head_only),
                }
            }
            (Kind::Segmenter(segmenter), "live.m3u8") => {
                match segmenter.playlist(Duration::from_secs(25)) {
                    Ok(text) => respond(
                        &mut stream,
                        200,
                        "application/vnd.apple.mpegurl",
                        text.as_bytes(),
                        head_only,
                    ),
                    Err(error) => {
                        session.record(error.clone());
                        respond(&mut stream, 502, "text/plain", error.as_bytes(), head_only)
                    }
                }
            }
            (Kind::Segmenter(segmenter), rest) if rest.starts_with("seg/") => {
                match rest[4..]
                    .trim_end_matches(".ts")
                    .parse()
                    .ok()
                    .and_then(|n| segmenter.segment(n))
                {
                    Some(path) => serve_file(&mut stream, &path, "video/mp2t", head_only),
                    None => respond(&mut stream, 404, "text/plain", b"", head_only),
                }
            }
            (Kind::Transcode(job), rest) if rest.starts_with("f/") => {
                let name = &rest[2..];
                if name.is_empty()
                    || !name
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_'))
                    || name.starts_with('.')
                {
                    respond(&mut stream, 400, "text/plain", b"", head_only);
                    return;
                }
                let content_type = if name.ends_with(".m3u8") {
                    "application/vnd.apple.mpegurl"
                } else {
                    "video/mp2t"
                };
                if let Some(error) = job.failure() {
                    session.record(error);
                }
                serve_file(&mut stream, &job.dir().join(name), content_type, head_only)
            }
            _ => respond(&mut stream, 404, "text/plain", b"", head_only),
        }
    }

    fn relay(
        &self,
        stream: &mut TcpStream,
        id: &str,
        session: &Session,
        upstream: &Url,
        range: Option<String>,
        head_only: bool,
    ) {
        let mut request = session.headers.apply(self.client.get(upstream.clone()));
        if let Some(range) = &range {
            request = request.header(reqwest::header::RANGE, range);
        }
        let response = match request.send() {
            Ok(response) => response,
            Err(error) => {
                let message = format!(
                    "{} ({})",
                    net::transport_message(&error),
                    net::redact(upstream.as_str())
                );
                session.record(message);
                respond(stream, 502, "text/plain", b"", head_only);
                return;
            }
        };
        let status = response.status().as_u16();
        if !response.status().is_success() {
            session.record(format!(
                "{} ({})",
                net::status_message(status, net::deny_reason(&response).as_deref()),
                net::redact(upstream.as_str())
            ));
            respond(stream, status, "text/plain", b"", head_only);
            return;
        }
        let final_url = response.url().clone();
        let header = |name: reqwest::header::HeaderName| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let content_type = header(reqwest::header::CONTENT_TYPE).unwrap_or_default();
        let length = response.content_length();
        let content_range = header(reqwest::header::CONTENT_RANGE);
        let path = final_url.path().to_ascii_lowercase();
        let playlist_hint = content_type.to_ascii_lowercase().contains("mpegurl")
            || path.ends_with(".m3u8")
            || path.ends_with(".m3u");
        let mut reader = BufReader::with_capacity(64 * 1024, response);
        let is_playlist = playlist_hint
            || (status == 200
                && !content_type.starts_with("video/")
                && reader
                    .fill_buf()
                    .map(|buf| {
                        buf.starts_with(b"#EXTM3U") || buf.starts_with("\u{feff}#EXTM3U".as_bytes())
                    })
                    .unwrap_or(false));
        if is_playlist {
            let mut body = Vec::new();
            if reader
                .by_ref()
                .take(MAX_PLAYLIST)
                .read_to_end(&mut body)
                .is_err()
            {
                session.record("Playlist interrompue par le serveur.".into());
                respond(stream, 502, "text/plain", b"", head_only);
                return;
            }
            let text = String::from_utf8_lossy(&body);
            let rewritten = hls::rewrite(&text, &final_url, |url| self.relay_url(id, url));
            respond(
                stream,
                200,
                "application/vnd.apple.mpegurl",
                rewritten.as_bytes(),
                head_only,
            );
            return;
        }
        let mut extra = vec![("Accept-Ranges".to_owned(), "bytes".to_owned())];
        if let Some(content_range) = content_range {
            extra.push(("Content-Range".into(), content_range));
        }
        let reply = Reply {
            status,
            content_type: if content_type.is_empty() {
                "application/octet-stream"
            } else {
                &content_type
            },
            length,
            extra,
        };
        if write_head(stream, &reply).is_err() || head_only {
            return;
        }
        let _ = std::io::copy(&mut reader, stream);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Write};

    fn upstream() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut agent = String::new();
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                    if line.to_ascii_lowercase().starts_with("user-agent:") {
                        agent = line[11..].trim().to_owned();
                    }
                    line.clear();
                }
                let path = first.split_whitespace().nth(1).unwrap().to_owned();
                let (status, body): (&str, &[u8]) = if agent != "Needed/1" {
                    ("403 Forbidden", b"")
                } else if path == "/live/index.m3u8" {
                    ("200 OK", b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-KEY:METHOD=AES-128,URI=\"../k.key\"\n#EXTINF:4,\nseg1.ts\n")
                } else if path == "/live/seg1.ts" {
                    ("200 OK", b"SEGMENT")
                } else {
                    ("404 Not Found", b"")
                };
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = stream.write_all(body);
            }
        });
        format!("http://{address}")
    }

    fn get(url: &str) -> (u16, String) {
        let response = reqwest::blocking::get(url).unwrap();
        (response.status().as_u16(), response.text().unwrap())
    }

    #[test]
    fn relays_with_required_headers_and_rewrites_playlists() {
        let base = upstream();
        let proxy =
            Proxy::start(std::env::temp_dir().join(format!("fluxo-proxy-{}", net::random_token())))
                .unwrap();
        let headers = StreamHeaders {
            user_agent: Some("Needed/1".into()),
            referrer: None,
        };
        let opened = proxy
            .open_relay(&format!("{base}/live/index.m3u8"), headers)
            .unwrap();
        let (status, playlist) = get(&opened.url);
        assert_eq!(status, 200);
        let segment = playlist
            .lines()
            .find(|line| line.starts_with("http://127.0.0.1"))
            .unwrap()
            .to_owned();
        assert!(playlist.contains(&format!("URI=\"{}", proxy.base(&opened.session))));
        assert_eq!(get(&segment), (200, "SEGMENT".into()));

        let blocked = proxy
            .open_relay(&format!("{base}/live/index.m3u8"), StreamHeaders::default())
            .unwrap();
        assert_eq!(get(&blocked.url).0, 403);
        let status = proxy.status(&blocked.session);
        assert!(status.errors[0].starts_with("Le serveur refuse l’accès"));

        let forged = opened.url.replace(&proxy.token, "wrong");
        assert_eq!(get(&forged).0, 403);
        proxy.close(&opened.session);
        assert!(!proxy.status(&opened.session).alive);
    }
}
