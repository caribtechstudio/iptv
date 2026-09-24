mod local_media;
mod xtream;

use flate2::read::GzDecoder;
use iptv_core::{
    Channel, Library, Playlist, Program, RecentItem, XtreamAccount, is_hls_manifest, now_iso,
    parse_m3u, parse_xmltv,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tauri::{AppHandle, Manager};

const MAX_PLAYLIST_BYTES: u64 = 20 * 1024 * 1024;
const MAX_EPG_BYTES: u64 = 60 * 1024 * 1024;
const MAX_YOUTUBE_PAGE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_HLS_MANIFEST_BYTES: u64 = 1024 * 1024;

struct Store {
    path: PathBuf,
    library: Mutex<Library>,
    programs: Mutex<Vec<Program>>,
}

struct YoutubePlayerState {
    generation: AtomicU64,
}

#[derive(Clone, Copy, Deserialize)]
struct YoutubeBounds {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

impl YoutubeBounds {
    fn rect(self) -> Result<tauri::Rect, String> {
        if ![self.x, self.y, self.width, self.height]
            .iter()
            .all(|value| value.is_finite())
            || self.width <= 0.0
            || self.height <= 0.0
        {
            return Err("Zone de lecture YouTube invalide.".into());
        }
        Ok(tauri::Rect {
            position: tauri::Position::Logical(tauri::LogicalPosition::new(self.x, self.y)),
            size: tauri::Size::Logical(tauri::LogicalSize::new(self.width, self.height)),
        })
    }
}

impl Store {
    fn load(path: PathBuf) -> Self {
        let library = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(library) => library,
                Err(_) => {
                    let backup = path
                        .with_extension(format!("json.corrupt.{}", chrono::Utc::now().timestamp()));
                    let _ = fs::rename(&path, backup);
                    Library::default()
                }
            },
            Err(_) => Library::default(),
        };
        Self {
            path,
            library: Mutex::new(library),
            programs: Mutex::new(Vec::new()),
        }
    }

    fn save(&self, library: &Library) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(library).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    fn change<R>(&self, f: impl FnOnce(&mut Library) -> Result<R, String>) -> Result<R, String> {
        let mut guard = self.library.lock().map_err(|e| e.to_string())?;
        let mut next = guard.clone();
        let result = f(&mut next)?;
        self.save(&next)?;
        *guard = next;
        Ok(result)
    }
}

fn read_source(source: &str, limit: u64) -> Result<String, String> {
    let bytes = if source.starts_with("https://") || source.starts_with("http://") {
        let response = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(25))
            .user_agent("Fluxo/0.1")
            .build()
            .map_err(|e| e.to_string())?
            .get(source)
            .send()
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("Téléchargement impossible : {e}"))?;
        if response.content_length().is_some_and(|len| len > limit) {
            return Err("Fichier trop volumineux.".into());
        }
        let mut bytes = Vec::new();
        response
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > limit {
            return Err("Fichier trop volumineux.".into());
        }
        bytes
    } else {
        let path = Path::new(source);
        if !path.is_file() {
            return Err("Fichier introuvable.".into());
        }
        if fs::metadata(path).map_err(|e| e.to_string())?.len() > limit {
            return Err("Fichier trop volumineux.".into());
        }
        fs::read(path).map_err(|e| e.to_string())?
    };
    let bytes = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoded = Vec::new();
        GzDecoder::new(&bytes[..])
            .take(limit + 1)
            .read_to_end(&mut decoded)
            .map_err(|e| format!("Archive GZip invalide : {e}"))?;
        if decoded.len() as u64 > limit {
            return Err("Fichier décompressé trop volumineux.".into());
        }
        decoded
    } else {
        bytes
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn import_channels(source: &str) -> Result<Vec<iptv_core::Channel>, String> {
    let text = read_source(source, MAX_PLAYLIST_BYTES)?;
    catalog_channels(source, &text)?.ok_or_else(|| {
        "Cette adresse est un flux HLS. Utilise « Lire une URL » pour le lancer.".into()
    })
}

fn parse_playlist_channels(source: &str, text: &str) -> Result<Vec<iptv_core::Channel>, String> {
    let base = if source.starts_with("http") {
        Some(source.to_owned())
    } else {
        url::Url::from_file_path(source)
            .ok()
            .map(|url| url.to_string())
    };
    parse_m3u(text, base.as_deref())
}

fn catalog_channels(source: &str, text: &str) -> Result<Option<Vec<Channel>>, String> {
    if is_hls_manifest(text) {
        Ok(None)
    } else {
        parse_playlist_channels(source, text).map(Some)
    }
}

fn is_youtube_url(source: &str) -> bool {
    url::Url::parse(source).is_ok_and(|url| {
        matches!(url.scheme(), "http" | "https")
            && matches!(
                url.host_str(),
                Some("youtube.com" | "www.youtube.com" | "m.youtube.com" | "youtu.be")
            )
    })
}

fn youtube_video_id_from_url(source: &str) -> Option<String> {
    if !is_youtube_url(source) {
        return None;
    }
    let url = url::Url::parse(source).ok()?;
    let segments: Vec<_> = url
        .path_segments()?
        .filter(|part| !part.is_empty())
        .collect();
    let candidate = if url.host_str() == Some("youtu.be") {
        segments.first().copied()
    } else if segments.first() == Some(&"watch") {
        return url
            .query_pairs()
            .find(|(key, _)| key == "v")
            .map(|(_, value)| value.into_owned())
            .filter(|id| valid_youtube_video_id(id));
    } else if matches!(segments.first(), Some(&"live" | &"shorts" | &"embed")) {
        segments.get(1).copied()
    } else {
        None
    }?;
    valid_youtube_video_id(candidate).then(|| candidate.to_owned())
}

fn valid_youtube_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn youtube_video_id_from_html(html: &str) -> Option<String> {
    for marker in [
        "<link rel=\"canonical\" href=\"",
        "<meta property=\"og:url\" content=\"",
    ] {
        if let Some(candidate) = html
            .split_once(marker)
            .and_then(|(_, rest)| rest.split('"').next())
            && let Some(id) = youtube_video_id_from_url(candidate)
        {
            return Some(id);
        }
    }
    None
}

fn resolve_youtube_video_id(source: &str) -> Result<String, String> {
    if !is_youtube_url(source) {
        return Err("Cette adresse n’est pas une page YouTube valide.".into());
    }
    if let Some(id) = youtube_video_id_from_url(source) {
        return Ok(id);
    }
    let html = read_source(source, MAX_YOUTUBE_PAGE_BYTES)?;
    youtube_video_id_from_html(&html)
        .ok_or_else(|| "Aucune vidéo YouTube intégrable trouvée pour cette chaîne.".into())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HlsManifest {
    url: String,
    text: String,
}

#[tauri::command]
async fn fetch_hls_manifest(source: String) -> Result<HlsManifest, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let parsed = url::Url::parse(&source).map_err(|_| "Adresse HLS invalide.")?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err("Le flux HLS doit utiliser HTTP ou HTTPS.".into());
        }
        let response = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(12))
            .user_agent("Fluxo/0.3")
            .build()
            .map_err(|error| error.to_string())?
            .get(parsed)
            .send()
            .and_then(|response| response.error_for_status())
            .map_err(|error| format!("Playlist HLS indisponible : {error}"))?;
        if response
            .content_length()
            .is_some_and(|length| length > MAX_HLS_MANIFEST_BYTES)
        {
            return Err("Playlist HLS trop volumineuse.".into());
        }
        let url = response.url().to_string();
        let mut bytes = Vec::new();
        response
            .take(MAX_HLS_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_HLS_MANIFEST_BYTES {
            return Err("Playlist HLS trop volumineuse.".into());
        }
        let text =
            String::from_utf8(bytes).map_err(|_| "La playlist HLS n’est pas encodée en UTF-8.")?;
        if !text.trim_start_matches('\u{feff}').starts_with("#EXTM3U") {
            return Err("Cette adresse ne renvoie pas une playlist HLS.".into());
        }
        Ok(HlsManifest { url, text })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn open_youtube_player(
    app: AppHandle,
    url: String,
    bounds: YoutubeBounds,
) -> Result<u64, String> {
    let generation = app
        .state::<YoutubePlayerState>()
        .generation
        .fetch_add(1, Ordering::SeqCst)
        + 1;
    let video_id = tauri::async_runtime::spawn_blocking(move || resolve_youtube_video_id(&url))
        .await
        .map_err(|error| error.to_string())??;

    if app
        .state::<YoutubePlayerState>()
        .generation
        .load(Ordering::SeqCst)
        != generation
    {
        return Ok(0);
    }

    #[cfg(target_os = "macos")]
    {
        let player = if let Some(webview) = app.get_webview("youtube-player") {
            webview
        } else {
            let main = app
                .get_window("main")
                .ok_or("Fenêtre principale introuvable.")?;
            main.add_child(
                tauri::webview::WebviewBuilder::new(
                    "youtube-player",
                    tauri::WebviewUrl::App("youtube-loading.html".into()),
                ),
                tauri::LogicalPosition::new(bounds.x, bounds.y),
                tauri::LogicalSize::new(bounds.width, bounds.height),
            )
            .map_err(|error| error.to_string())?
        };
        player
            .set_bounds(bounds.rect()?)
            .map_err(|error| error.to_string())?;
        let embed_url =
            format!("https://www.youtube.com/embed/{video_id}?autoplay=1&playsinline=1");
        player
            .with_webview(move |webview| unsafe {
                use objc2_foundation::{NSMutableURLRequest, NSString, NSURL};
                let view: &objc2_web_kit::WKWebView = &*webview.inner().cast();
                let url = NSURL::URLWithString(&NSString::from_str(&embed_url))
                    .expect("L’adresse YouTube construite est valide");
                let request = NSMutableURLRequest::requestWithURL(&url);
                request.setValue_forHTTPHeaderField(
                    Some(&NSString::from_str("https://app.fluxo.iptv")),
                    &NSString::from_str("Referer"),
                );
                view.loadRequest(&request);
            })
            .map_err(|error| error.to_string())?;
        player.show().map_err(|error| error.to_string())?;
        Ok(generation)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, bounds, video_id);
        Err("Le lecteur YouTube intégré est disponible sur macOS.".into())
    }
}

#[tauri::command]
fn set_youtube_player_bounds(
    app: AppHandle,
    session: u64,
    bounds: Option<YoutubeBounds>,
) -> Result<(), String> {
    if app
        .state::<YoutubePlayerState>()
        .generation
        .load(Ordering::SeqCst)
        != session
    {
        return Ok(());
    }
    if let Some(player) = app.get_webview("youtube-player") {
        if let Some(bounds) = bounds {
            player
                .set_bounds(bounds.rect()?)
                .map_err(|error| error.to_string())?;
            player.show().map_err(|error| error.to_string())?;
        } else {
            player.hide().map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
fn hide_youtube_player(app: AppHandle) -> Result<(), String> {
    app.state::<YoutubePlayerState>()
        .generation
        .fetch_add(1, Ordering::SeqCst);
    if let Some(player) = app.get_webview("youtube-player") {
        player.hide().map_err(|error| error.to_string())?;
        #[cfg(target_os = "macos")]
        player
            .with_webview(|webview| unsafe {
                let view: &objc2_web_kit::WKWebView = &*webview.inner().cast();
                view.stopLoading();
                view.loadHTMLString_baseURL(&objc2_foundation::NSString::from_str(""), None);
            })
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn save_playlist(
    store: &Store,
    name: String,
    source: String,
    channels: Vec<Channel>,
) -> Result<Playlist, String> {
    let playlist = Playlist {
        id: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
            .to_string(),
        name,
        source,
        channels,
        updated_at: now_iso(),
    };
    store.change(|library| {
        library.playlists.push(playlist.clone());
        Ok(playlist)
    })
}

#[tauri::command]
fn get_library(app: AppHandle) -> Result<Library, String> {
    Ok(app
        .state::<Store>()
        .library
        .lock()
        .map_err(|e| e.to_string())?
        .clone())
}

#[tauri::command]
async fn add_playlist(app: AppHandle, name: String, source: String) -> Result<Playlist, String> {
    let name = name.trim().to_owned();
    let source = source.trim().to_owned();
    if name.is_empty() || source.is_empty() {
        return Err("Indique un nom et une source.".into());
    }
    let app_clone = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let channels = import_channels(&source)?;
        let store = app_clone.state::<Store>();
        save_playlist(&store, name, source, channels)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn add_playlist_if_catalog(
    app: AppHandle,
    name: String,
    source: String,
) -> Result<Option<Playlist>, String> {
    let name = name.trim().to_owned();
    let source = source.trim().to_owned();
    if name.is_empty() || source.is_empty() {
        return Err("Indique un nom et une source.".into());
    }
    let app_clone = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let text = read_source(&source, MAX_PLAYLIST_BYTES)?;
        let Some(channels) = catalog_channels(&source, &text)? else {
            return Ok(None);
        };
        let store = app_clone.state::<Store>();
        save_playlist(&store, name, source, channels).map(Some)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn refresh_playlist(app: AppHandle, id: String) -> Result<Playlist, String> {
    let app_clone = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let store = app_clone.state::<Store>();
        let source = store
            .library
            .lock()
            .map_err(|e| e.to_string())?
            .playlists
            .iter()
            .find(|item| item.id == id)
            .ok_or("Playlist introuvable.")?
            .source
            .clone();
        let channels = if source == local_media::PLAYLIST_SOURCE {
            store
                .library
                .lock()
                .map_err(|e| e.to_string())?
                .playlists
                .iter()
                .find(|item| item.id == id)
                .ok_or("Médias locaux introuvables.")?
                .channels
                .iter()
                .filter(|channel| {
                    url::Url::parse(&channel.stream_url)
                        .ok()
                        .and_then(|url| url.to_file_path().ok())
                        .is_some_and(|path| path.is_file())
                })
                .cloned()
                .collect()
        } else if source.starts_with("xtream://") {
            let account = store
                .library
                .lock()
                .map_err(|e| e.to_string())?
                .xtream_accounts
                .iter()
                .find(|item| format!("xtream://{}", item.id) == source)
                .cloned()
                .ok_or("Compte Xtream introuvable.")?;
            xtream::import(&account, &xtream::password(&account.id)?)?
        } else {
            import_channels(&source)?
        };
        store.change(|library| {
            let playlist = library
                .playlists
                .iter_mut()
                .find(|item| item.id == id)
                .ok_or("Playlist introuvable.")?;
            playlist.channels = channels;
            playlist.updated_at = now_iso();
            Ok(playlist.clone())
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LocalImport {
    playlist: Playlist,
    channels: Vec<Channel>,
    skipped: usize,
    truncated: bool,
}

#[tauri::command]
async fn import_local_media(app: AppHandle, paths: Vec<String>) -> Result<LocalImport, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let scan = local_media::scan(&paths)?;
        let playlist = app.state::<Store>().change(|library| {
            let playlist = if let Some(playlist) = library
                .playlists
                .iter_mut()
                .find(|item| item.id == local_media::PLAYLIST_ID)
            {
                let mut seen: std::collections::HashSet<String> = playlist
                    .channels
                    .iter()
                    .map(|item| item.stream_url.clone())
                    .collect();
                for channel in &scan.channels {
                    if seen.insert(channel.stream_url.clone()) {
                        playlist.channels.push(channel.clone());
                    }
                }
                playlist.updated_at = now_iso();
                playlist.clone()
            } else {
                let playlist = Playlist {
                    id: local_media::PLAYLIST_ID.into(),
                    name: "Médias locaux".into(),
                    source: local_media::PLAYLIST_SOURCE.into(),
                    channels: scan.channels.clone(),
                    updated_at: now_iso(),
                };
                library.playlists.push(playlist.clone());
                playlist
            };
            Ok(playlist)
        })?;
        Ok(LocalImport {
            playlist,
            channels: scan.channels,
            skipped: scan.skipped,
            truncated: scan.truncated,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn remove_playlist(app: AppHandle, id: String) -> Result<(), String> {
    let account = app
        .state::<Store>()
        .library
        .lock()
        .map_err(|e| e.to_string())?
        .xtream_accounts
        .iter()
        .find(|item| item.id == id)
        .cloned();
    app.state::<Store>().change(|library| {
        if id == local_media::PLAYLIST_ID {
            let local_ids: std::collections::HashSet<_> = library
                .playlists
                .iter()
                .find(|playlist| playlist.id == id)
                .map(|playlist| {
                    playlist
                        .channels
                        .iter()
                        .map(|channel| channel.id.clone())
                        .collect()
                })
                .unwrap_or_default();
            library
                .favorites
                .retain(|favorite| !local_ids.contains(favorite));
            library
                .recent
                .retain(|recent| !local_ids.contains(&recent.url));
        }
        library.playlists.retain(|item| item.id != id);
        library.xtream_accounts.retain(|item| item.id != id);
        library
            .favorites
            .retain(|favorite| !favorite.starts_with(&format!("xtream://{id}/")));
        library
            .recent
            .retain(|recent| !recent.url.starts_with(&format!("xtream://{id}/")));
        Ok(())
    })?;
    if account.is_some() {
        let _ = xtream::keychain_entry(&id)?.delete_credential();
    }
    Ok(())
}

#[tauri::command]
async fn add_xtream_account(
    app: AppHandle,
    name: String,
    server: String,
    username: String,
    password: String,
) -> Result<Playlist, String> {
    let name = name.trim().to_owned();
    let username = username.trim().to_owned();
    if name.is_empty() || username.is_empty() || password.is_empty() {
        return Err("Indiquez un nom, un utilisateur et un mot de passe.".into());
    }
    let server = xtream::normalize_server(&server)?;
    tauri::async_runtime::spawn_blocking(move || {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()
            .to_string();
        let account = XtreamAccount {
            id: id.clone(),
            server,
            username,
        };
        let channels = xtream::import(&account, &password)?;
        let entry = xtream::keychain_entry(&id)?;
        entry
            .set_password(&password)
            .map_err(|_| "Impossible d’enregistrer le mot de passe dans le trousseau macOS.")?;
        let playlist = Playlist {
            id,
            name,
            source: format!("xtream://{}", account.id),
            channels,
            updated_at: now_iso(),
        };
        let result = app.state::<Store>().change(|library| {
            library.xtream_accounts.push(account);
            library.playlists.push(playlist.clone());
            Ok(playlist)
        });
        if result.is_err() {
            let _ = entry.delete_credential();
        }
        result
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn resolve_stream(
    app: AppHandle,
    reference: String,
    extension: Option<String>,
) -> Result<String, String> {
    let account_id = xtream::account_id(&reference)?;
    let account = app
        .state::<Store>()
        .library
        .lock()
        .map_err(|e| e.to_string())?
        .xtream_accounts
        .iter()
        .find(|item| item.id == account_id)
        .cloned()
        .ok_or("Compte Xtream introuvable.")?;
    tauri::async_runtime::spawn_blocking(move || {
        xtream::resolve(
            &account,
            &xtream::password(&account_id)?,
            &reference,
            extension.as_deref(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn get_series_episodes(
    app: AppHandle,
    reference: String,
) -> Result<Vec<xtream::Episode>, String> {
    let account_id = xtream::account_id(&reference)?;
    let account = app
        .state::<Store>()
        .library
        .lock()
        .map_err(|e| e.to_string())?
        .xtream_accounts
        .iter()
        .find(|item| item.id == account_id)
        .cloned()
        .ok_or("Compte Xtream introuvable.")?;
    tauri::async_runtime::spawn_blocking(move || {
        xtream::episodes(&account, &xtream::password(&account_id)?, &reference)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn toggle_favorite(app: AppHandle, id: String) -> Result<bool, String> {
    app.state::<Store>().change(|library| {
        if let Some(index) = library.favorites.iter().position(|item| item == &id) {
            library.favorites.remove(index);
            Ok(false)
        } else {
            library.favorites.push(id);
            Ok(true)
        }
    })
}

#[tauri::command]
fn record_recent(
    app: AppHandle,
    name: String,
    url: String,
    extension: Option<String>,
) -> Result<(), String> {
    app.state::<Store>().change(|library| {
        library.recent.retain(|item| item.url != url);
        library.recent.insert(
            0,
            RecentItem {
                name,
                url,
                played_at: now_iso(),
                container_extension: extension,
            },
        );
        library.recent.truncate(30);
        Ok(())
    })
}

#[tauri::command]
async fn set_epg_source(app: AppHandle, source: String) -> Result<usize, String> {
    let source = source.trim().to_owned();
    let app_clone = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let programs = if source.is_empty() {
            Vec::new()
        } else {
            parse_xmltv(&read_source(&source, MAX_EPG_BYTES)?)?
        };
        let store = app_clone.state::<Store>();
        store.change(|library| {
            library.epg_source = if source.is_empty() {
                None
            } else {
                Some(source)
            };
            Ok(())
        })?;
        let count = programs.len();
        *store.programs.lock().map_err(|e| e.to_string())? = programs;
        Ok(count)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_programs(app: AppHandle, channel_id: String) -> Result<Vec<Program>, String> {
    let now = chrono::Utc::now().timestamp();
    Ok(app
        .state::<Store>()
        .programs
        .lock()
        .map_err(|e| e.to_string())?
        .iter()
        .filter(|item| item.channel_id == channel_id && item.stop > now && item.start < now + 86400)
        .take(20)
        .cloned()
        .collect())
}

#[tauri::command]
fn allow_media_file(app: AppHandle, path: String) -> Result<String, String> {
    let canonical = Path::new(&path).canonicalize().map_err(|e| e.to_string())?;
    if !canonical.is_file() {
        return Err("Choisis un fichier multimédia.".into());
    }
    app.asset_protocol_scope()
        .allow_file(&canonical)
        .map_err(|e| e.to_string())?;
    Ok(canonical.to_string_lossy().into_owned())
}

#[tauri::command]
fn read_subtitle(path: String) -> Result<String, String> {
    let path = Path::new(&path);
    let valid = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "srt" | "vtt"));
    if !valid || !path.is_file() {
        return Err("Choisissez un fichier de sous-titres SRT ou VTT.".into());
    }
    if fs::metadata(path).map_err(|e| e.to_string())?.len() > 2 * 1024 * 1024 {
        return Err("Fichier de sous-titres trop volumineux.".into());
    }
    fs::read_to_string(path)
        .map_err(|_| "Sous-titres illisibles : choisissez un fichier UTF-8.".into())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_persisted_scope::init())
        .setup(|app| {
            let path = app.path().app_data_dir()?.join("library.json");
            app.manage(Store::load(path));
            app.manage(YoutubePlayerState {
                generation: AtomicU64::new(0),
            });
            let source = app
                .state::<Store>()
                .library
                .lock()
                .ok()
                .and_then(|library| library.epg_source.clone());
            if let Some(source) = source {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    if let Ok(text) = read_source(&source, MAX_EPG_BYTES)
                        && let Ok(programs) = parse_xmltv(&text)
                        && let Ok(mut guard) = handle.state::<Store>().programs.lock()
                    {
                        *guard = programs;
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_library,
            add_xtream_account,
            resolve_stream,
            get_series_episodes,
            add_playlist,
            add_playlist_if_catalog,
            fetch_hls_manifest,
            open_youtube_player,
            set_youtube_player_bounds,
            hide_youtube_player,
            import_local_media,
            refresh_playlist,
            remove_playlist,
            toggle_favorite,
            record_recent,
            set_epg_source,
            get_programs,
            allow_media_file,
            read_subtitle
        ])
        .run(tauri::generate_context!())
        .expect("Impossible de lancer Fluxo");
}

#[cfg(test)]
mod tests {
    use super::{catalog_channels, youtube_video_id_from_html, youtube_video_id_from_url};

    #[test]
    fn m3u8_url_distinguishes_channel_catalog_from_hls_video() {
        let source = "https://raw.githubusercontent.com/Free-TV/IPTV/master/playlist.m3u8";
        let catalog = "#EXTM3U x-tvg-url=\"https://example.org/guide.xml\"\n#EXTINF:-1 tvg-id=\"Kanali7.al\" group-title=\"Albania\",Kanali 7\nhttps://example.org/live/kanali7.m3u8";
        let channels = catalog_channels(source, catalog).unwrap().unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].name, "Kanali 7");
        assert_eq!(channels[0].group, "Albania");
        assert_eq!(
            channels[0].stream_url,
            "https://example.org/live/kanali7.m3u8"
        );

        let hls = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegment.ts";
        assert!(catalog_channels(source, hls).unwrap().is_none());
    }

    #[test]
    fn youtube_live_page_resolves_to_a_safe_video_id() {
        let html = "<link rel=\"canonical\" href=\"https://www.youtube.com/watch?v=NiRIbKwAejk\">";
        assert_eq!(
            youtube_video_id_from_html(html).as_deref(),
            Some("NiRIbKwAejk")
        );
        assert_eq!(
            youtube_video_id_from_url("https://youtu.be/NiRIbKwAejk").as_deref(),
            Some("NiRIbKwAejk")
        );
        assert!(
            youtube_video_id_from_url("https://youtube.com.evil.example/watch?v=NiRIbKwAejk")
                .is_none()
        );
    }
}
