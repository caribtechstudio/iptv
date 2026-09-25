mod epg;
mod health;
mod local_media;
pub mod net;
pub mod probe;
pub mod proxy;
mod recorder;
pub mod segmenter;
mod store;
pub mod transcode;
mod xtream;

use epg::{EpgRef, Guide, NowNext};
use iptv_core::{
    Channel, Library, Playlist, Program, Progress, RecentItem, XtreamAccount, is_hls_manifest,
    m3u_epg_urls, now_iso, parse_m3u,
};
use net::StreamHeaders;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use store::Store;
use tauri::{AppHandle, Emitter, Manager};

const MAX_PLAYLIST_BYTES: u64 = 20 * 1024 * 1024;
const MAX_YOUTUBE_PAGE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_HLS_MANIFEST_BYTES: u64 = 1024 * 1024;
const EPG_REFRESH: Duration = Duration::from_secs(6 * 3600);
const MAX_PROGRESS: usize = 200;

struct YoutubePlayerState {
    generation: AtomicU64,
}

struct EpgState {
    guide: Mutex<Arc<Guide>>,
    loading: AtomicBool,
}

struct Services {
    proxy: Arc<proxy::Proxy>,
    health: Arc<health::Health>,
    recorder: recorder::Recorder,
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

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|error| error.to_string())?
}

struct Imported {
    channels: Vec<Channel>,
    epg_url: Option<String>,
}

fn import_channels(source: &str) -> Result<Imported, String> {
    let text = net::read_source(source, MAX_PLAYLIST_BYTES)?;
    catalog_channels(source, &text)?.ok_or_else(|| {
        "Cette adresse est un flux HLS. Utilise « Lire une URL » pour le lancer.".into()
    })
}

fn parse_playlist_channels(source: &str, text: &str) -> Result<Vec<Channel>, String> {
    let base = if source.starts_with("http") {
        Some(source.to_owned())
    } else {
        url::Url::from_file_path(source)
            .ok()
            .map(|url| url.to_string())
    };
    parse_m3u(text, base.as_deref())
}

fn catalog_channels(source: &str, text: &str) -> Result<Option<Imported>, String> {
    if is_hls_manifest(text) {
        Ok(None)
    } else {
        let epg_url = m3u_epg_urls(text);
        parse_playlist_channels(source, text).map(|channels| {
            Some(Imported {
                channels,
                epg_url: (!epg_url.is_empty()).then(|| epg_url.join(",")),
            })
        })
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
    let html = net::read_source(source, MAX_YOUTUBE_PAGE_BYTES)?;
    youtube_video_id_from_html(&html)
        .ok_or_else(|| "Aucune vidéo YouTube intégrable trouvée pour cette chaîne.".into())
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
            .user_agent(net::PLAYER_USER_AGENT)
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

fn new_id() -> Result<String, String> {
    Ok(std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos()
        .to_string())
}

fn ensure_new_source(store: &Store, source: &str) -> Result<(), String> {
    let exists = store.read(|library| library.playlists.iter().any(|p| p.source == source))?;
    if exists {
        Err("Cette playlist est déjà importée. Actualisez-la depuis Sources & guide TV.".into())
    } else {
        Ok(())
    }
}

fn save_playlist(
    store: &Store,
    name: String,
    source: String,
    imported: Imported,
) -> Result<Playlist, String> {
    let playlist = Playlist {
        id: new_id()?,
        name,
        source,
        channels: imported.channels,
        updated_at: now_iso(),
        epg_url: imported.epg_url,
    };
    store.change(|library| {
        library.playlists.push(playlist.clone());
        Ok(playlist)
    })
}

#[tauri::command]
fn get_library(app: AppHandle) -> Result<Library, String> {
    app.state::<Store>().read(Library::clone)
}

#[tauri::command]
async fn add_playlist(app: AppHandle, name: String, source: String) -> Result<Playlist, String> {
    let name = name.trim().to_owned();
    let source = source.trim().to_owned();
    if name.is_empty() || source.is_empty() {
        return Err("Indique un nom et une source.".into());
    }
    let playlist = blocking({
        let app = app.clone();
        move || {
            let store = app.state::<Store>();
            ensure_new_source(&store, &source)?;
            let imported = import_channels(&source)?;
            save_playlist(&store, name, source, imported)
        }
    })
    .await?;
    reload_epg_in_background(&app);
    Ok(playlist)
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
    let playlist = blocking({
        let app = app.clone();
        move || {
            let text = net::read_source(&source, MAX_PLAYLIST_BYTES)?;
            let Some(imported) = catalog_channels(&source, &text)? else {
                return Ok(None);
            };
            let store = app.state::<Store>();
            ensure_new_source(&store, &source)?;
            save_playlist(&store, name, source, imported).map(Some)
        }
    })
    .await?;
    if playlist.is_some() {
        reload_epg_in_background(&app);
    }
    Ok(playlist)
}

fn find_account(store: &Store, id: &str) -> Result<XtreamAccount, String> {
    store
        .read(|library| {
            library
                .xtream_accounts
                .iter()
                .find(|item| item.id == id)
                .cloned()
        })?
        .ok_or_else(|| "Compte Xtream introuvable.".into())
}

#[tauri::command]
async fn refresh_playlist(app: AppHandle, id: String) -> Result<Playlist, String> {
    let playlist = blocking({
        let app = app.clone();
        move || {
            let store = app.state::<Store>();
            let (source, channels) = store
                .read(|library| {
                    library
                        .playlists
                        .iter()
                        .find(|item| item.id == id)
                        .map(|item| (item.source.clone(), item.channels.clone()))
                })?
                .ok_or("Playlist introuvable.")?;
            let (imported, account_info) = if source == local_media::PLAYLIST_SOURCE {
                let channels = channels
                    .into_iter()
                    .filter(|channel| {
                        url::Url::parse(&channel.stream_url)
                            .ok()
                            .and_then(|url| url.to_file_path().ok())
                            .is_some_and(|path| path.is_file())
                    })
                    .collect();
                (
                    Imported {
                        channels,
                        epg_url: None,
                    },
                    None,
                )
            } else if let Some(account_id) = source.strip_prefix("xtream://") {
                let account = find_account(&store, account_id)?;
                let (channels, info) = xtream::import(&account, &xtream::password(&account.id)?)?;
                (
                    Imported {
                        channels,
                        epg_url: None,
                    },
                    Some((account.id, info)),
                )
            } else {
                (import_channels(&source)?, None)
            };
            store.change(|library| {
                if let Some((account_id, info)) = account_info
                    && let Some(account) = library
                        .xtream_accounts
                        .iter_mut()
                        .find(|a| a.id == account_id)
                {
                    account.output_formats = info.output_formats;
                    account.utc_offset = info.utc_offset;
                }
                let playlist = library
                    .playlists
                    .iter_mut()
                    .find(|item| item.id == id)
                    .ok_or("Playlist introuvable.")?;
                playlist.channels = imported.channels;
                playlist.epg_url = imported.epg_url;
                playlist.updated_at = now_iso();
                Ok(playlist.clone())
            })
        }
    })
    .await?;
    reload_epg_in_background(&app);
    Ok(playlist)
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
    blocking(move || {
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
                    epg_url: None,
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
}

/// Only the small state file is rewritten: the catalogue keeps its `updated_at`.
#[tauri::command]
fn rename_playlist(app: AppHandle, id: String, name: String) -> Result<(), String> {
    let name = name.trim().to_owned();
    if name.is_empty() {
        return Err("Indique un nom.".into());
    }
    app.state::<Store>().change(|library| {
        library
            .playlists
            .iter_mut()
            .find(|item| item.id == id)
            .ok_or("Playlist introuvable.")?
            .name = name;
        Ok(())
    })
}

#[tauri::command]
fn remove_playlist(app: AppHandle, id: String) -> Result<(), String> {
    let store = app.state::<Store>();
    let account = store.read(|library| library.xtream_accounts.iter().any(|item| item.id == id))?;
    store.change(|library| {
        let ids: std::collections::HashSet<String> = library
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
        let urls: std::collections::HashSet<String> = library
            .playlists
            .iter()
            .find(|playlist| playlist.id == id)
            .map(|playlist| {
                playlist
                    .channels
                    .iter()
                    .map(|channel| channel.stream_url.clone())
                    .collect()
            })
            .unwrap_or_default();
        library.favorites.retain(|favorite| !ids.contains(favorite));
        if id == local_media::PLAYLIST_ID {
            library.recent.retain(|recent| !urls.contains(&recent.url));
            library.progress.retain(|item| !urls.contains(&item.url));
        }
        library.playlists.retain(|item| item.id != id);
        library.xtream_accounts.retain(|item| item.id != id);
        let prefix = format!("xtream://{id}/");
        library
            .favorites
            .retain(|favorite| !favorite.starts_with(&prefix));
        library
            .recent
            .retain(|recent| !recent.url.starts_with(&prefix));
        library
            .progress
            .retain(|item| !item.url.starts_with(&prefix));
        Ok(())
    })?;
    if account {
        let _ = xtream::keychain_entry(&id)?.delete_credential();
    }
    reload_epg_in_background(&app);
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
    let playlist = blocking({
        let app = app.clone();
        move || {
            let id = new_id()?;
            let mut account = XtreamAccount {
                id: id.clone(),
                server,
                username,
                ..XtreamAccount::default()
            };
            let (channels, info) = xtream::import(&account, &password)?;
            account.output_formats = info.output_formats;
            account.utc_offset = info.utc_offset;
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
                epg_url: None,
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
        }
    })
    .await?;
    reload_epg_in_background(&app);
    Ok(playlist)
}

#[tauri::command]
async fn resolve_stream(
    app: AppHandle,
    reference: String,
    extension: Option<String>,
) -> Result<String, String> {
    let account = find_account(&app.state::<Store>(), &xtream::account_id(&reference)?)?;
    blocking(move || {
        xtream::resolve(
            &account,
            &xtream::password(&account.id)?,
            &reference,
            extension.as_deref(),
        )
    })
    .await
}

#[tauri::command]
async fn resolve_catchup(
    app: AppHandle,
    reference: String,
    start: i64,
    stop: i64,
) -> Result<String, String> {
    let account = find_account(&app.state::<Store>(), &xtream::account_id(&reference)?)?;
    blocking(move || {
        xtream::catchup(
            &account,
            &xtream::password(&account.id)?,
            &reference,
            start,
            stop,
        )
    })
    .await
}

#[tauri::command]
async fn get_series_episodes(
    app: AppHandle,
    reference: String,
) -> Result<Vec<xtream::Episode>, String> {
    let account = find_account(&app.state::<Store>(), &xtream::account_id(&reference)?)?;
    blocking(move || xtream::episodes(&account, &xtream::password(&account.id)?, &reference)).await
}

#[tauri::command]
fn toggle_favorite(app: AppHandle, id: String) -> Result<Vec<String>, String> {
    app.state::<Store>().change(|library| {
        if let Some(index) = library.favorites.iter().position(|item| item == &id) {
            library.favorites.remove(index);
        } else {
            library.favorites.push(id);
        }
        Ok(library.favorites.clone())
    })
}

#[tauri::command]
fn reorder_favorites(app: AppHandle, ids: Vec<String>) -> Result<Vec<String>, String> {
    app.state::<Store>().change(|library| {
        let mut ordered: Vec<String> = ids
            .into_iter()
            .filter(|id| library.favorites.contains(id))
            .collect();
        ordered.dedup();
        for id in &library.favorites {
            if !ordered.contains(id) {
                ordered.push(id.clone());
            }
        }
        library.favorites = ordered;
        Ok(library.favorites.clone())
    })
}

#[tauri::command]
fn record_recent(
    app: AppHandle,
    name: String,
    url: String,
    extension: Option<String>,
) -> Result<Vec<RecentItem>, String> {
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
        Ok(library.recent.clone())
    })
}

#[tauri::command]
fn set_hidden_groups(app: AppHandle, groups: Vec<String>) -> Result<Vec<String>, String> {
    app.state::<Store>().change(|library| {
        let mut groups = groups;
        groups.sort();
        groups.dedup();
        library.hidden_groups = groups;
        Ok(library.hidden_groups.clone())
    })
}

#[tauri::command]
fn save_progress(app: AppHandle, item: Progress) -> Result<Vec<Progress>, String> {
    app.state::<Store>().change(|library| {
        library.progress.retain(|existing| existing.url != item.url);
        let finished = item.duration > 0.0 && item.position >= item.duration * 0.95;
        if item.position >= 15.0 && !finished {
            library.progress.insert(
                0,
                Progress {
                    updated_at: now_iso(),
                    ..item
                },
            );
        }
        library.progress.truncate(MAX_PROGRESS);
        Ok(library.progress.clone())
    })
}

#[tauri::command]
fn clear_progress(app: AppHandle, url: String) -> Result<Vec<Progress>, String> {
    app.state::<Store>().change(|library| {
        library.progress.retain(|existing| existing.url != url);
        Ok(library.progress.clone())
    })
}

/// Empties « Récents » and « Reprendre ».
#[tauri::command]
fn clear_history(app: AppHandle) -> Result<(), String> {
    app.state::<Store>().change(|library| {
        library.recent.clear();
        library.progress.clear();
        Ok(())
    })
}

/// Restores a blank library: playlists, Xtream accounts (and their keychain passwords),
/// favourites, history, guide settings and availability results. Recordings on disk are kept.
#[tauri::command]
fn reset_app(app: AppHandle) -> Result<(), String> {
    let accounts = app.state::<Store>().change(|library| {
        let accounts: Vec<String> = library
            .xtream_accounts
            .iter()
            .map(|account| account.id.clone())
            .collect();
        *library = Library::default();
        Ok(accounts)
    })?;
    for id in accounts {
        if let Ok(entry) = xtream::keychain_entry(&id) {
            let _ = entry.delete_credential();
        }
    }
    app.state::<Services>().health.clear();
    if let Ok(mut current) = app.state::<EpgState>().guide.lock() {
        *current = Arc::new(Guide::default());
    }
    let _ = app.emit("epg-updated", 0);
    Ok(())
}

// ---------- Guide TV ----------

const MAX_PLAYLIST_GUIDES: usize = 6;

/// Interest per country: channels of the playlist, boosted by favourites and history.
fn country_weights(library: &Library, playlist: &Playlist) -> HashMap<String, u64> {
    let mut weights: HashMap<String, u64> = HashMap::new();
    for channel in &playlist.channels {
        if let Some(country) = &channel.country {
            let mut weight = 1;
            if library.favorites.contains(&channel.id) {
                weight += 1000;
            }
            if library
                .recent
                .iter()
                .any(|item| item.url == channel.stream_url)
            {
                weight += 300;
            }
            *weights.entry(country.to_ascii_uppercase()).or_default() += weight;
        }
    }
    weights
}

/// Playlists such as Free-TV announce dozens of guides (one per country, plus a 190 MB
/// "all sources" file). Keep every guide of short lists, otherwise the guides whose file
/// name names a country present in the playlist, most relevant first.
fn pick_guides(urls: &str, weights: &HashMap<String, u64>) -> Vec<String> {
    let urls: Vec<String> = urls
        .split(',')
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
        .collect();
    if urls.len() <= 3 {
        return urls;
    }
    let mut scored: Vec<(u64, String)> = urls
        .into_iter()
        .filter_map(|url| {
            let file = url.rsplit('/').next().unwrap_or("").to_ascii_uppercase();
            if file.contains("ALL_SOURCES") || file.contains("ALL-SOURCES") {
                return None;
            }
            let score = file
                .split(|ch: char| !ch.is_ascii_alphanumeric())
                .map(|token| token.trim_end_matches(|ch: char| ch.is_ascii_digit()))
                .filter(|token| token.len() == 2)
                .filter_map(|token| weights.get(token))
                .max()
                .copied()?;
            Some((score, url))
        })
        .collect();
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored
        .into_iter()
        .take(MAX_PLAYLIST_GUIDES)
        .map(|(_, url)| url)
        .collect()
}

fn epg_sources(app: &AppHandle) -> Vec<epg::Source> {
    let Ok((manual, playlists, accounts, ignore)) = app.state::<Store>().read(|library| {
        (
            library.epg_source.clone(),
            library
                .playlists
                .iter()
                .filter_map(|p| {
                    p.epg_url.as_deref().map(|urls| {
                        (
                            p.name.clone(),
                            pick_guides(urls, &country_weights(library, p)),
                        )
                    })
                })
                .collect::<Vec<_>>(),
            library.xtream_accounts.clone(),
            library.ignore_playlist_epg,
        )
    }) else {
        return Vec::new();
    };
    let mut sources = Vec::new();
    if let Some(location) = manual.filter(|value| !value.trim().is_empty()) {
        sources.push(epg::Source {
            label: "Guide personnel".into(),
            location,
        });
    }
    if !ignore {
        for (name, urls) in playlists {
            for url in urls {
                if !sources.iter().any(|source| source.location == url) {
                    let file = url
                        .rsplit('/')
                        .next()
                        .unwrap_or("")
                        .split('?')
                        .next()
                        .unwrap_or("");
                    sources.push(epg::Source {
                        label: format!("Playlist « {name} » · {file}"),
                        location: url,
                    });
                }
            }
        }
        for account in accounts {
            if let Ok(url) = xtream::password(&account.id)
                .and_then(|password| xtream::xmltv_url(&account, &password))
            {
                let name = app
                    .state::<Store>()
                    .read(|library| {
                        library
                            .playlists
                            .iter()
                            .find(|p| p.id == account.id)
                            .map(|p| p.name.clone())
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "Xtream".into());
                sources.push(epg::Source {
                    label: format!("Xtream « {name} »"),
                    location: url,
                });
            }
        }
    }
    sources
}

fn reload_epg_in_background(app: &AppHandle) {
    let state = app.state::<EpgState>();
    if state.loading.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let _ = app.emit("epg-loading", ());
        let guide = Guide::load(epg_sources(&app), chrono::Utc::now().timestamp());
        let count = guide.program_count();
        let state = app.state::<EpgState>();
        if let Ok(mut current) = state.guide.lock() {
            *current = Arc::new(guide);
        }
        state.loading.store(false, Ordering::SeqCst);
        let _ = app.emit("epg-updated", count);
    });
}

fn guide(app: &AppHandle) -> Arc<Guide> {
    app.state::<EpgState>()
        .guide
        .lock()
        .map(|guide| guide.clone())
        .unwrap_or_default()
}

#[tauri::command]
async fn set_epg_source(app: AppHandle, source: String) -> Result<usize, String> {
    let source = source.trim().to_owned();
    if !source.is_empty() {
        let check = source.clone();
        blocking(move || {
            net::read_source(&check, epg::MAX_EPG_BYTES)
                .and_then(|text| iptv_core::parse_xmltv_guide(&text))
                .map(|_| ())
        })
        .await?;
    }
    app.state::<Store>().change(|library| {
        library.epg_source = (!source.is_empty()).then_some(source);
        Ok(())
    })?;
    let app_clone = app.clone();
    let guide = blocking(move || {
        Ok(Guide::load(
            epg_sources(&app_clone),
            chrono::Utc::now().timestamp(),
        ))
    })
    .await?;
    let count = guide.program_count();
    if let Ok(mut current) = app.state::<EpgState>().guide.lock() {
        *current = Arc::new(guide);
    }
    let _ = app.emit("epg-updated", count);
    Ok(count)
}

#[tauri::command]
fn set_playlist_epg(app: AppHandle, enabled: bool) -> Result<(), String> {
    app.state::<Store>().change(|library| {
        library.ignore_playlist_epg = !enabled;
        Ok(())
    })?;
    reload_epg_in_background(&app);
    Ok(())
}

#[tauri::command]
fn reload_epg(app: AppHandle) {
    reload_epg_in_background(&app);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EpgStatus {
    loading: bool,
    programs: usize,
    loaded_at: i64,
    sources: Vec<epg::SourceStatus>,
}

#[tauri::command]
fn epg_status(app: AppHandle) -> EpgStatus {
    let guide = guide(&app);
    EpgStatus {
        loading: app.state::<EpgState>().loading.load(Ordering::SeqCst),
        programs: guide.program_count(),
        loaded_at: guide.loaded_at,
        sources: guide.sources.clone(),
    }
}

#[tauri::command]
fn get_programs(
    app: AppHandle,
    reference: EpgRef,
    from: Option<i64>,
    to: Option<i64>,
) -> Vec<Program> {
    let now = chrono::Utc::now().timestamp();
    guide(&app).programs(
        &reference,
        from.unwrap_or(now),
        to.unwrap_or(now + 86_400),
        60,
    )
}

#[tauri::command]
fn get_now_next(app: AppHandle, references: Vec<EpgRef>) -> HashMap<String, NowNext> {
    let now = chrono::Utc::now().timestamp();
    let guide = guide(&app);
    references
        .iter()
        .take(600)
        .filter_map(|reference| {
            guide
                .now_next(reference, now)
                .map(|value| (reference.key.clone(), value))
        })
        .collect()
}

#[tauri::command]
fn get_guide(
    app: AppHandle,
    references: Vec<EpgRef>,
    from: i64,
    to: i64,
) -> HashMap<String, Vec<Program>> {
    let guide = guide(&app);
    references
        .iter()
        .take(300)
        .map(|reference| {
            (
                reference.key.clone(),
                guide.programs(reference, from, to, 80),
            )
        })
        .filter(|(_, programs)| !programs.is_empty())
        .collect()
}

// ---------- Lecture : sonde, proxy, segmenteur, FFmpeg ----------

#[tauri::command]
async fn probe_stream(url: String, headers: Option<StreamHeaders>) -> Result<probe::Probe, String> {
    blocking(move || Ok(probe::probe(&url, &headers.unwrap_or_default()))).await
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenStreamRequest {
    url: String,
    #[serde(default)]
    headers: StreamHeaders,
    /// `relay`, `segmenter` or `transcode`.
    mode: String,
    #[serde(default)]
    live: bool,
    #[serde(default)]
    copy_video: bool,
    #[serde(default)]
    deinterlace: bool,
}

fn local_path(url: &str) -> Option<PathBuf> {
    if url.starts_with("file:") {
        url::Url::parse(url).ok()?.to_file_path().ok()
    } else if url.starts_with('/') {
        Some(PathBuf::from(url))
    } else {
        None
    }
}

#[tauri::command]
async fn open_stream(
    app: AppHandle,
    request: OpenStreamRequest,
) -> Result<proxy::OpenedStream, String> {
    let proxy = app.state::<Services>().proxy.clone();
    blocking(move || {
        let local = local_path(&request.url);
        if let Some(path) = &local
            && !path.is_file()
        {
            return Err("Fichier introuvable.".into());
        }
        match request.mode.as_str() {
            "relay" => proxy.open_relay(&request.url, request.headers),
            "segmenter" => {
                let source = match local {
                    Some(path) => segmenter::Source::File(path),
                    None => segmenter::Source::Http {
                        url: request.url.clone(),
                        headers: request.headers,
                    },
                };
                proxy.open_segmenter(source, request.live)
            }
            "transcode" => {
                let input = local
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or(request.url.clone());
                proxy.open_transcode(transcode::Options {
                    input: &input,
                    headers: &request.headers,
                    live: request.live,
                    copy_video: request.copy_video,
                    deinterlace: request.deinterlace,
                })
            }
            _ => Err("Mode de lecture inconnu.".into()),
        }
    })
    .await
}

#[tauri::command]
fn close_stream(app: AppHandle, session: String) {
    app.state::<Services>().proxy.close(&session);
}

#[tauri::command]
fn stream_status(app: AppHandle, session: String) -> proxy::SessionStatus {
    app.state::<Services>().proxy.status(&session)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EngineInfo {
    ffmpeg: Option<String>,
    recordings_dir: String,
}

#[tauri::command]
fn engine_info(app: AppHandle) -> EngineInfo {
    EngineInfo {
        ffmpeg: transcode::ffmpeg_path().map(|path| path.to_string_lossy().into_owned()),
        recordings_dir: app.state::<Services>().recorder.list().dir,
    }
}

// ---------- Disponibilité des chaînes ----------

#[tauri::command]
fn check_channels(app: AppHandle, items: Vec<health::CheckItem>) -> usize {
    let health = app.state::<Services>().health.clone();
    health.enqueue(app.clone(), items.into_iter().take(2000).collect())
}

#[tauri::command]
fn get_health(app: AppHandle) -> HashMap<String, health::HealthEntry> {
    app.state::<Services>().health.snapshot()
}

#[tauri::command]
fn report_health(app: AppHandle, url: String, ok: bool, message: Option<String>) {
    if url.starts_with("http://") || url.starts_with("https://") {
        app.state::<Services>().health.record(
            url,
            health::HealthEntry {
                ok,
                message,
                checked_at: chrono::Utc::now().timestamp(),
            },
        );
    }
}

// ---------- Enregistrements ----------

#[tauri::command]
async fn start_recording(
    app: AppHandle,
    url: String,
    headers: Option<StreamHeaders>,
    name: String,
) -> Result<recorder::RecordingInfo, String> {
    let app_clone = app.clone();
    blocking(move || {
        app_clone.state::<Services>().recorder.start(
            app_clone.clone(),
            url,
            headers.unwrap_or_default(),
            name,
        )
    })
    .await
}

#[tauri::command]
fn stop_recording(app: AppHandle, id: String) {
    app.state::<Services>().recorder.stop(&id);
}

#[tauri::command]
fn list_recordings(app: AppHandle) -> recorder::Recordings {
    app.state::<Services>().recorder.list()
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
            let data = app.path().app_data_dir()?;
            app.manage(Store::load(data.clone()));
            app.manage(YoutubePlayerState {
                generation: AtomicU64::new(0),
            });
            app.manage(EpgState {
                guide: Mutex::new(Arc::new(Guide::default())),
                loading: AtomicBool::new(false),
            });
            let work = app.path().app_cache_dir()?.join("streams");
            let recordings = app
                .path()
                .video_dir()
                .unwrap_or_else(|_| data.join("recordings"))
                .join("Fluxo");
            app.manage(Services {
                proxy: proxy::Proxy::start(work)?,
                health: health::Health::load(data.join("health.json")),
                recorder: recorder::Recorder::new(recordings),
            });
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                loop {
                    reload_epg_in_background(&handle);
                    std::thread::sleep(EPG_REFRESH);
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_library,
            add_xtream_account,
            resolve_stream,
            resolve_catchup,
            get_series_episodes,
            add_playlist,
            add_playlist_if_catalog,
            fetch_hls_manifest,
            probe_stream,
            open_stream,
            close_stream,
            stream_status,
            engine_info,
            open_youtube_player,
            set_youtube_player_bounds,
            hide_youtube_player,
            import_local_media,
            refresh_playlist,
            rename_playlist,
            remove_playlist,
            toggle_favorite,
            reorder_favorites,
            record_recent,
            set_hidden_groups,
            save_progress,
            clear_progress,
            clear_history,
            reset_app,
            set_epg_source,
            set_playlist_epg,
            reload_epg,
            epg_status,
            get_programs,
            get_now_next,
            get_guide,
            check_channels,
            get_health,
            report_health,
            start_recording,
            stop_recording,
            list_recordings,
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
        let imported = catalog_channels(source, catalog).unwrap().unwrap();
        assert_eq!(imported.channels.len(), 1);
        assert_eq!(imported.channels[0].name, "Kanali 7");
        assert_eq!(imported.channels[0].group, "Albania");
        assert_eq!(
            imported.channels[0].stream_url,
            "https://example.org/live/kanali7.m3u8"
        );
        assert_eq!(
            imported.epg_url.as_deref(),
            Some("https://example.org/guide.xml")
        );

        let hls = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegment.ts";
        assert!(catalog_channels(source, hls).unwrap().is_none());
    }

    #[test]
    fn picks_guides_of_the_countries_that_matter() {
        let urls = "https://e/epg_ripper_AL1.xml.gz, https://e/epg_ripper_ALL_SOURCES1.xml.gz, https://e/epg_ripper_FR1.xml.gz, https://e/epg_ripper_RAKUTEN_FR1.xml.gz, https://e/epg_ripper_US1.xml.gz";
        let weights =
            std::collections::HashMap::from([("FR".to_owned(), 1200), ("US".to_owned(), 40)]);
        let picked = super::pick_guides(urls, &weights);
        assert_eq!(
            picked[..2],
            [
                "https://e/epg_ripper_FR1.xml.gz",
                "https://e/epg_ripper_RAKUTEN_FR1.xml.gz"
            ]
        );
        assert_eq!(picked.len(), 3);
        assert!(!picked.iter().any(|url| url.contains("ALL_SOURCES")));
        assert_eq!(
            super::pick_guides("https://a/x.xml,https://b/y.xml", &weights).len(),
            2
        );
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
