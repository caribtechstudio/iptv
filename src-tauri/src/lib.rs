mod xtream;

use flate2::read::GzDecoder;
use iptv_core::{
    Library, Playlist, Program, RecentItem, XtreamAccount, is_hls_manifest, now_iso, parse_m3u,
    parse_xmltv,
};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};
use tauri::{AppHandle, Manager};

const MAX_PLAYLIST_BYTES: u64 = 20 * 1024 * 1024;
const MAX_EPG_BYTES: u64 = 60 * 1024 * 1024;

struct Store {
    path: PathBuf,
    library: Mutex<Library>,
    programs: Mutex<Vec<Program>>,
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
    if is_hls_manifest(&text) {
        return Err(
            "Cette adresse est un flux HLS. Utilise « Lire une URL » pour le lancer.".into(),
        );
    }
    let base = if source.starts_with("http") {
        Some(source.to_owned())
    } else {
        url::Url::from_file_path(source)
            .ok()
            .map(|url| url.to_string())
    };
    parse_m3u(&text, base.as_deref())
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
        let playlist = Playlist {
            id: format!(
                "{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|e| e.to_string())?
                    .as_nanos()
            ),
            name,
            source,
            channels,
            updated_at: now_iso(),
        };
        store.change(|library| {
            library.playlists.push(playlist.clone());
            Ok(playlist)
        })
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
        let channels = if source.starts_with("xtream://") {
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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_persisted_scope::init())
        .setup(|app| {
            let path = app.path().app_data_dir()?.join("library.json");
            app.manage(Store::load(path));
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
