//! mpv playback engine for the web interface.
//!
//! Each named surface (`main`, `mv-0`…) owns one mpv instance and one native video view
//! placed just below the web view, where the page leaves a transparent hole. The page drives
//! it with commands and receives `mpv` events (properties, end of file, errors).

use crate::mpv::{self, Event, Format, Mpv, Value};
use crate::net::StreamHeaders;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter};

/// Properties forwarded to the page, with the format they are observed in.
const OBSERVED: &[(&str, Format)] = &[
    ("time-pos", Format::Double),
    ("duration", Format::Double),
    ("pause", Format::Flag),
    ("paused-for-cache", Format::Flag),
    ("core-idle", Format::Flag),
    ("eof-reached", Format::Flag),
    ("seekable", Format::Flag),
    ("volume", Format::Double),
    ("mute", Format::Flag),
    ("speed", Format::Double),
    ("dwidth", Format::Int),
    ("dheight", Format::Int),
    ("track-list", Format::Text),
    ("hwdec-current", Format::Text),
    ("video-codec", Format::Text),
    ("audio-codec-name", Format::Text),
];

/// `time-pos` changes at every frame; the page only needs a few updates per second.
const TIME_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// `window.innerHeight` of the page, used to find the title bar inset on macOS.
    #[serde(default)]
    pub viewport_height: Option<f64>,
}

impl Bounds {
    fn valid(self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
            && self.width >= 1.0
            && self.height >= 1.0
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    surface: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

impl Payload {
    fn new(surface: &str, kind: &'static str) -> Self {
        Self {
            surface: surface.to_owned(),
            kind,
            name: None,
            value: None,
            error: None,
        }
    }
}

fn json(value: Value) -> serde_json::Value {
    match value {
        Value::None => serde_json::Value::Null,
        Value::Flag(flag) => flag.into(),
        Value::Int(number) => number.into(),
        Value::Double(number) => serde_json::Number::from_f64(number)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Text(text) => text.into(),
    }
}

struct Instance {
    mpv: Arc<Mpv>,
    stop: Arc<AtomicBool>,
    events: Mutex<Option<JoinHandle<()>>>,
}

impl Instance {
    /// Stops the event thread. The last reference to mpv is dropped off the main thread.
    fn shutdown(&self) {
        self.stop.store(true, Ordering::Release);
        self.mpv.wakeup();
        if let Some(thread) = self.events.lock().ok().and_then(|mut guard| guard.take()) {
            let _ = thread.join();
        }
    }
}

/// Diagnostic trace of the engine commands, enabled with `FLUXO_MPV_SAMPLE`.
fn trace(message: impl FnOnce() -> String) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ENABLED.get_or_init(|| std::env::var_os("FLUXO_MPV_SAMPLE").is_some()) {
        eprintln!("[mpv] {}", message());
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// Bytes per second read from the network into the cache.
    pub cache_speed: Option<f64>,
    /// Bits per second of the video and audio packets being decoded.
    pub video_bitrate: Option<f64>,
    pub audio_bitrate: Option<f64>,
    pub dropped_frames: Option<f64>,
}

#[derive(Default)]
pub struct VideoEngine {
    instances: Mutex<HashMap<String, Arc<Instance>>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineStatus {
    pub available: bool,
    pub library: Option<String>,
    pub error: Option<String>,
}

pub fn status() -> EngineStatus {
    if !cfg!(target_os = "macos") {
        return EngineStatus {
            available: false,
            library: mpv::available().ok(),
            error: Some("L’affichage mpv n’est pas encore disponible sur ce système.".into()),
        };
    }
    match mpv::available() {
        Ok(path) => EngineStatus {
            available: true,
            library: Some(path),
            error: None,
        },
        Err(error) => EngineStatus {
            available: false,
            library: None,
            error: Some(error),
        },
    }
}

fn valid_surface(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 16
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-');
    if ok {
        Ok(())
    } else {
        Err("Nom de surface vidéo invalide.".into())
    }
}

/// Addresses come from playlists, which are untrusted: mpv only gets streaming protocols and
/// existing local files, never its special protocols (`av://`, `edl://`, `memory://`…).
fn check_source(url: &str) -> Result<(), String> {
    const SCHEMES: &[&str] = &[
        "http", "https", "rtmp", "rtmps", "rtsp", "rtsps", "rtp", "udp", "srt",
    ];
    if url.starts_with('/') {
        return if std::path::Path::new(url).is_file() {
            Ok(())
        } else {
            Err("Fichier introuvable.".into())
        };
    }
    match url::Url::parse(url) {
        Ok(parsed) if SCHEMES.contains(&parsed.scheme()) => Ok(()),
        _ => Err("Adresse non prise en charge par mpv.".into()),
    }
}

fn create_mpv() -> Result<Mpv, String> {
    let mpv = Mpv::new(&[
        ("vo", "libmpv"),
        ("hwdec", "auto-safe"),
        ("idle", "yes"),
        ("keep-open", "yes"),
        ("terminal", "no"),
        ("config", "no"),
        ("osc", "no"),
        ("input-default-bindings", "no"),
        ("input-vo-keyboard", "no"),
        ("ytdl", "no"),
        ("audio-client-name", "Fluxo"),
        ("volume-max", "100"),
        ("hls-bitrate", "max"),
        ("cache", "yes"),
        // Per instance: the multiview runs up to five of them.
        ("demuxer-max-bytes", "64MiB"),
        ("demuxer-max-back-bytes", "16MiB"),
        ("network-timeout", "20"),
        (
            "stream-lavf-o",
            "reconnect=1,reconnect_streamed=1,reconnect_on_network_error=1,reconnect_delay_max=5",
        ),
    ])?;
    for (name, format) in OBSERVED {
        mpv.observe(name, *format)?;
    }
    mpv.request_log_messages("error")?;
    Ok(mpv)
}

fn spawn_events(
    app: AppHandle,
    surface: String,
    mpv: Arc<Mpv>,
    stop: Arc<AtomicBool>,
) -> Result<JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name(format!("fluxo-mpv-{surface}"))
        .spawn(move || {
            let mut last_time = Instant::now() - TIME_INTERVAL;
            while !stop.load(Ordering::Acquire) {
                let payload = match mpv.wait_event(1.0) {
                    Event::None | Event::Other => continue,
                    Event::Shutdown => break,
                    Event::StartFile => Payload::new(&surface, "start"),
                    Event::FileLoaded => Payload::new(&surface, "loaded"),
                    Event::VideoReconfig => Payload::new(&surface, "video"),
                    Event::PlaybackRestart => Payload::new(&surface, "restart"),
                    Event::EndFile { eof, error } => Payload {
                        value: Some(eof.into()),
                        error,
                        ..Payload::new(&surface, "end")
                    },
                    Event::Property { name, value } => {
                        if name == "time-pos" {
                            if last_time.elapsed() < TIME_INTERVAL {
                                continue;
                            }
                            last_time = Instant::now();
                        }
                        Payload {
                            name: Some(name),
                            value: Some(json(value)),
                            ..Payload::new(&surface, "property")
                        }
                    }
                    Event::Log {
                        level,
                        prefix,
                        text,
                    } => Payload {
                        name: Some(level),
                        error: Some(format!("{prefix}: {text}")),
                        ..Payload::new(&surface, "log")
                    },
                };
                if payload.kind != "property" {
                    trace(|| format!("{surface}: événement {} {:?}", payload.kind, payload.error));
                }
                let _ = app.emit("mpv", payload);
            }
        })
        .map_err(|error| error.to_string())
}

impl VideoEngine {
    fn get(&self, surface: &str) -> Result<Arc<Instance>, String> {
        self.instances
            .lock()
            .map_err(|e| e.to_string())?
            .get(surface)
            .cloned()
            .ok_or_else(|| "Lecteur mpv non initialisé.".into())
    }

    /// Creates the instance and its video view if needed.
    pub fn attach(&self, app: &AppHandle, surface: &str) -> Result<(), String> {
        valid_surface(surface)?;
        if !status().available {
            return Err(status()
                .error
                .unwrap_or_else(|| "mpv est indisponible.".into()));
        }
        if self.get(surface).is_ok() {
            return Ok(());
        }
        trace(|| format!("{surface}: création"));
        let mpv = Arc::new(create_mpv()?);
        platform::create_view(app, surface, mpv.clone())?;
        let stop = Arc::new(AtomicBool::new(false));
        let events = spawn_events(app.clone(), surface.to_owned(), mpv.clone(), stop.clone())?;
        let instance = Arc::new(Instance {
            mpv,
            stop,
            events: Mutex::new(Some(events)),
        });
        self.instances
            .lock()
            .map_err(|e| e.to_string())?
            .insert(surface.to_owned(), instance);
        Ok(())
    }

    pub fn load(
        &self,
        surface: &str,
        url: &str,
        headers: &StreamHeaders,
        start: Option<f64>,
    ) -> Result<(), String> {
        check_source(url)?;
        trace(|| format!("{surface}: chargement de {url}"));
        let instance = self.get(surface)?;
        let mpv = &instance.mpv;
        mpv.set_property("user-agent", &headers.user_agent())?;
        mpv.set_property("referrer", &headers.referrer().unwrap_or_default())?;
        mpv.set_flag("pause", false)?;
        let mut args = vec!["loadfile".to_owned(), url.to_owned(), "replace".into()];
        if let Some(start) = start.filter(|value| value.is_finite() && *value > 0.0) {
            args.extend(["-1".into(), format!("start={start:.3}")]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        mpv.command(&args)
    }

    pub fn command(&self, surface: &str, args: &[String]) -> Result<(), String> {
        const ALLOWED: &[&str] = &["stop", "seek"];
        let name = args.first().map(String::as_str).unwrap_or_default();
        if !ALLOWED.contains(&name) {
            return Err(format!("Commande mpv non autorisée : {name}"));
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.get(surface)?.mpv.command(&args)
    }

    pub fn set(&self, surface: &str, name: &str, value: &str) -> Result<(), String> {
        const ALLOWED: &[&str] = &[
            "pause",
            "volume",
            "mute",
            "speed",
            "aid",
            "sid",
            "vid",
            "sub-delay",
            "sub-scale",
            "sub-pos",
        ];
        if !ALLOWED.contains(&name) {
            return Err(format!("Propriété mpv non modifiable : {name}"));
        }
        self.get(surface)?.mpv.set_property(name, value)
    }

    /// Network and stream bitrates, polled by the page about once a second.
    pub fn stats(&self, surface: &str) -> Result<Stats, String> {
        let instance = self.get(surface)?;
        let mpv = &instance.mpv;
        let positive = |name: &str| {
            mpv.get_double(name)
                .filter(|value| value.is_finite() && *value >= 0.0)
        };
        Ok(Stats {
            cache_speed: positive("cache-speed"),
            video_bitrate: positive("video-bitrate"),
            audio_bitrate: positive("audio-bitrate"),
            dropped_frames: positive("frame-drop-count"),
        })
    }

    pub fn get_property(&self, surface: &str, name: &str) -> Result<Option<String>, String> {
        Ok(self.get(surface)?.mpv.get_string(name))
    }

    pub fn set_bounds(
        &self,
        app: &AppHandle,
        surface: &str,
        bounds: Option<Bounds>,
    ) -> Result<(), String> {
        self.get(surface)?;
        trace(|| format!("{surface}: zone {bounds:?}"));
        platform::set_frame(app, surface, bounds.filter(|bounds| bounds.valid()))
    }

    /// Stops playback and hides the view; the instance is kept for the next channel.
    pub fn detach(&self, app: &AppHandle, surface: &str) -> Result<(), String> {
        let Ok(instance) = self.get(surface) else {
            return Ok(());
        };
        let _ = instance.mpv.command(&["stop"]);
        platform::set_frame(app, surface, None)
    }

    pub fn destroy(&self, app: &AppHandle, surface: &str) -> Result<(), String> {
        let instance = self
            .instances
            .lock()
            .map_err(|e| e.to_string())?
            .remove(surface);
        if let Some(instance) = instance {
            let _ = instance.mpv.command(&["stop"]);
            platform::remove_view(app, surface)?;
            instance.shutdown();
        }
        Ok(())
    }

    pub fn destroy_all(&self, app: &AppHandle) -> Result<(), String> {
        let names: Vec<String> = self
            .instances
            .lock()
            .map_err(|e| e.to_string())?
            .keys()
            .cloned()
            .collect();
        for name in names {
            self.destroy(app, &name)?;
        }
        Ok(())
    }
}

/// Runs `f` on the main thread and waits for its result.
fn on_main<T: Send + 'static>(
    app: &AppHandle,
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    let (sender, receiver) = std::sync::mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = sender.send(f());
    })
    .map_err(|error| error.to_string())?;
    receiver
        .recv_timeout(Duration::from_secs(10))
        .map_err(|_| "Le fil principal ne répond pas.".to_owned())?
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Bounds, on_main};
    use crate::mpv::{
        Mpv,
        macos::{Frame, Surface},
    };
    use objc2::{MainThreadMarker, msg_send, rc::Retained};
    use objc2_app_kit::{NSColor, NSView};
    use objc2_foundation::{NSNumber, NSString};
    use std::{cell::RefCell, collections::HashMap, sync::Arc};
    use tauri::{AppHandle, Manager};

    thread_local! {
        /// Surfaces live on the main thread only.
        static SURFACES: RefCell<HashMap<String, Surface>> = RefCell::new(HashMap::new());
        /// The main web view, made transparent once so the video shows through the page.
        static WEBVIEW: RefCell<Option<Retained<NSView>>> = const { RefCell::new(None) };
    }

    /// Finds the main `WKWebView` and prepares it to show native video underneath.
    fn main_webview(app: &AppHandle) -> Result<(), String> {
        if WEBVIEW.with(|cell| cell.borrow().is_some()) {
            return Ok(());
        }
        let webview = app
            .get_webview("main")
            .ok_or("Vue principale introuvable.")?;
        let (sender, receiver) = std::sync::mpsc::channel();
        webview
            .with_webview(move |platform| {
                let result = (|| {
                    let view: Retained<NSView> =
                        unsafe { Retained::retain(platform.inner().cast::<NSView>()) }
                            .ok_or("Vue web introuvable.")?;
                    unsafe {
                        // Private but long-standing WebKit switch, also used by Tauri for
                        // transparent windows: the page paints its own background.
                        let no = NSNumber::new_bool(false);
                        let key = NSString::from_str("drawsBackground");
                        let _: () = msg_send![&*view, setValue: &*no, forKey: &*key];
                    }
                    if let Some(window) = view.window() {
                        // Same colour as the page, visible wherever the page is transparent.
                        let colour = NSColor::colorWithSRGBRed_green_blue_alpha(
                            17.0 / 255.0,
                            19.0 / 255.0,
                            21.0 / 255.0,
                            1.0,
                        );
                        window.setBackgroundColor(Some(&colour));
                    }
                    WEBVIEW.with(|cell| *cell.borrow_mut() = Some(view));
                    Ok::<(), String>(())
                })();
                let _ = sender.send(result);
            })
            .map_err(|error| error.to_string())?;
        receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .map_err(|_| "La vue web ne répond pas.".to_owned())?
    }

    pub fn create_view(app: &AppHandle, surface: &str, mpv: Arc<Mpv>) -> Result<(), String> {
        main_webview(app)?;
        let name = surface.to_owned();
        on_main(app, move || {
            let mtm = MainThreadMarker::new().ok_or("Fil principal attendu.")?;
            let reference = WEBVIEW
                .with(|cell| cell.borrow().clone())
                .ok_or("Vue web introuvable.")?;
            let surface = Surface::new(mtm, mpv, &reference)?;
            SURFACES.with(|cell| cell.borrow_mut().insert(name, surface));
            Ok(())
        })
    }

    pub fn set_frame(app: &AppHandle, surface: &str, bounds: Option<Bounds>) -> Result<(), String> {
        let name = surface.to_owned();
        on_main(app, move || {
            SURFACES.with(|cell| {
                if let Some(surface) = cell.borrow().get(&name) {
                    surface.set_frame(bounds.map(|b| Frame {
                        x: b.x,
                        y: b.y,
                        width: b.width,
                        height: b.height,
                        viewport_height: b.viewport_height,
                    }));
                }
            });
            Ok(())
        })
    }

    pub fn remove_view(app: &AppHandle, surface: &str) -> Result<(), String> {
        let name = surface.to_owned();
        on_main(app, move || {
            let removed = SURFACES.with(|cell| cell.borrow_mut().remove(&name));
            drop(removed);
            Ok(())
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::Bounds;
    use crate::mpv::Mpv;
    use std::sync::Arc;
    use tauri::AppHandle;

    pub fn create_view(_: &AppHandle, _: &str, _: Arc<Mpv>) -> Result<(), String> {
        Err("L’affichage mpv n’est pas encore disponible sur ce système.".into())
    }

    pub fn set_frame(_: &AppHandle, _: &str, _: Option<Bounds>) -> Result<(), String> {
        Ok(())
    }

    pub fn remove_view(_: &AppHandle, _: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_names_and_bounds_are_checked() {
        assert!(valid_surface("main").is_ok());
        assert!(valid_surface("mv-3").is_ok());
        assert!(valid_surface("../x").is_err());
        assert!(valid_surface("").is_err());
        assert!(check_source("https://h/live.m3u8").is_ok());
        assert!(check_source("rtsp://h/cam").is_ok());
        assert!(check_source("av://lavfi:testsrc").is_err());
        assert!(check_source("edl://a;b").is_err());
        assert!(check_source("/does/not/exist.mkv").is_err());
        assert!(
            Bounds {
                x: 0.0,
                y: 0.0,
                width: 640.0,
                height: 360.0,
                viewport_height: Some(728.0),
            }
            .valid()
        );
        assert!(
            !Bounds {
                x: f64::NAN,
                y: 0.0,
                width: 640.0,
                height: 360.0,
                viewport_height: Some(728.0),
            }
            .valid()
        );
        assert_eq!(json(Value::Double(f64::NAN)), serde_json::Value::Null);
        assert_eq!(json(Value::Flag(true)), serde_json::Value::Bool(true));
    }
}
