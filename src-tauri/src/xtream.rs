use iptv_core::{Channel, ChannelKind, XtreamAccount};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{io::Read, time::Duration};
use url::Url;

const MAX_API_BYTES: u64 = 32 * 1024 * 1024;
const KEYCHAIN_SERVICE: &str = "app.fluxo.iptv.xtream";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    pub id: String,
    pub name: String,
    pub season: String,
    pub extension: String,
    pub stream_url: String,
}

#[derive(Deserialize)]
struct AuthResponse {
    user_info: Option<Value>,
    server_info: Option<Value>,
}

/// Provider capabilities returned at login.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountInfo {
    pub output_formats: Vec<String>,
    pub utc_offset: Option<i32>,
}

fn account_info(parsed: &AuthResponse) -> AccountInfo {
    let output_formats = parsed
        .user_info
        .as_ref()
        .and_then(|info| info.get("allowed_output_formats"))
        .and_then(Value::as_array)
        .map(|formats| {
            formats
                .iter()
                .filter_map(value_string)
                .map(|format| format.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    // `time_now` is the provider's local clock, `timestamp_now` the UTC epoch.
    let utc_offset = parsed.server_info.as_ref().and_then(|info| {
        let local = chrono::NaiveDateTime::parse_from_str(
            &property(info, "time_now")?,
            "%Y-%m-%d %H:%M:%S",
        )
        .ok()?;
        let epoch: i64 = property(info, "timestamp_now")?.parse().ok()?;
        let offset = local.and_utc().timestamp() - epoch;
        // Round to the quarter hour to absorb the delay between both fields.
        Some(((offset as f64 / 900.0).round() * 900.0) as i32)
    });
    AccountInfo {
        output_formats,
        utc_offset,
    }
}

pub fn normalize_server(raw: &str) -> Result<String, String> {
    let mut url = Url::parse(raw.trim()).map_err(|_| "Adresse du serveur Xtream invalide.")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("Le serveur Xtream doit utiliser HTTP ou HTTPS.".into());
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "Entrez seulement l’adresse de base du serveur, sans identifiants ni paramètres."
                .into(),
        );
    }
    if !url.path().ends_with('/') {
        url.set_path(&format!("{}/", url.path()));
    }
    Ok(url.to_string())
}

pub fn keychain_entry(id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, id).map_err(|_| "Trousseau macOS indisponible.".into())
}

pub fn password(id: &str) -> Result<String, String> {
    keychain_entry(id)?
        .get_password()
        .map_err(|_| "Mot de passe Xtream introuvable dans le trousseau macOS.".into())
}

fn api_url(
    account: &XtreamAccount,
    password: &str,
    action: Option<&str>,
    extra: Option<(&str, &str)>,
) -> Result<Url, String> {
    let mut url = Url::parse(&account.server)
        .map_err(|_| "Adresse Xtream invalide.")?
        .join("player_api.php")
        .map_err(|_| "Adresse Xtream invalide.")?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("username", &account.username)
            .append_pair("password", password);
        if let Some(action) = action {
            query.append_pair("action", action);
        }
        if let Some((key, value)) = extra {
            query.append_pair(key, value);
        }
    }
    Ok(url)
}

fn fetch(url: Url) -> Result<Value, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(25))
        .user_agent("Fluxo/0.2")
        .build()
        .map_err(|_| "Connexion Xtream impossible.")?;
    // Never include the URL in errors: it contains the provider password.
    let response = client
        .get(url)
        .send()
        .map_err(|_| "Serveur Xtream inaccessible.")?;
    if !response.status().is_success() {
        return Err(format!(
            "Le serveur Xtream a répondu avec le code {}.",
            response.status().as_u16()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_API_BYTES)
    {
        return Err("Réponse Xtream trop volumineuse.".into());
    }
    let mut bytes = Vec::new();
    response
        .take(MAX_API_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Lecture de la réponse Xtream impossible.")?;
    if bytes.len() as u64 > MAX_API_BYTES {
        return Err("Réponse Xtream trop volumineuse.".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Réponse Xtream invalide.".into())
}

fn api(
    account: &XtreamAccount,
    password: &str,
    action: Option<&str>,
    extra: Option<(&str, &str)>,
) -> Result<Value, String> {
    fetch(api_url(account, password, action, extra)?)
}

fn value_string(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
fn property(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(value_string)
        .filter(|v| !v.is_empty())
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
}
fn extension(value: Option<String>, default: &str) -> String {
    let ext = value.unwrap_or_else(|| default.into()).to_ascii_lowercase();
    if !ext.is_empty() && ext.len() <= 8 && ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        ext
    } else {
        default.into()
    }
}

pub fn validate(account: &XtreamAccount, password: &str) -> Result<AccountInfo, String> {
    let value = api(account, password, None, None)?;
    let parsed: AuthResponse =
        serde_json::from_value(value).map_err(|_| "Réponse de connexion Xtream invalide.")?;
    let auth = parsed.user_info.as_ref().and_then(|info| info.get("auth"));
    if auth == Some(&Value::from(1))
        || auth == Some(&Value::from("1"))
        || auth == Some(&Value::from(true))
    {
        Ok(account_info(&parsed))
    } else {
        Err("Identifiants Xtream refusés par le serveur.".into())
    }
}

fn categories(
    account: &XtreamAccount,
    password: &str,
    action: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    let mut names = std::collections::HashMap::new();
    if let Some(items) = api(account, password, Some(action), None)?.as_array() {
        for item in items {
            if let (Some(id), Some(name)) = (
                property(item, "category_id"),
                property(item, "category_name"),
            ) {
                names.insert(id, name);
            }
        }
    }
    Ok(names)
}

pub fn import(
    account: &XtreamAccount,
    password: &str,
) -> Result<(Vec<Channel>, AccountInfo), String> {
    let info = validate(account, password)?;
    let mut channels = Vec::new();
    for (category_action, stream_action, kind, prefix, fallback) in [
        (
            "get_live_categories",
            "get_live_streams",
            ChannelKind::Live,
            "live",
            "TV",
        ),
        (
            "get_vod_categories",
            "get_vod_streams",
            ChannelKind::Movie,
            "movie",
            "Films",
        ),
        (
            "get_series_categories",
            "get_series",
            ChannelKind::Series,
            "series",
            "Séries",
        ),
    ] {
        let groups = categories(account, password, category_action).unwrap_or_default();
        let Ok(response) = api(account, password, Some(stream_action), None) else {
            continue;
        };
        let Some(items) = response.as_array() else {
            continue;
        };
        for item in items {
            let id = if kind == ChannelKind::Series {
                property(item, "series_id")
            } else {
                property(item, "stream_id")
            };
            let Some(id) = id.filter(|id| valid_id(id)) else {
                continue;
            };
            let Some(name) = property(item, "name") else {
                continue;
            };
            let category = property(item, "category_id")
                .and_then(|id| groups.get(&id).cloned())
                .unwrap_or_else(|| fallback.into());
            let logo = property(
                item,
                if kind == ChannelKind::Series {
                    "cover"
                } else {
                    "stream_icon"
                },
            );
            let ext = if kind == ChannelKind::Movie {
                Some(extension(property(item, "container_extension"), "mp4"))
            } else {
                None
            };
            let stream_url = format!("xtream://{}/{}/{}", account.id, prefix, id);
            let archive = kind == ChannelKind::Live
                && matches!(property(item, "tv_archive").as_deref(), Some("1" | "true"));
            let catchup_days = archive
                .then(|| property(item, "tv_archive_duration").and_then(|d| d.parse().ok()))
                .flatten()
                .filter(|days: &u32| *days > 0);
            channels.push(Channel {
                id: stream_url.clone(),
                name,
                group: format!("{} · {}", fallback, category),
                stream_url,
                logo,
                tvg_id: property(item, "epg_channel_id"),
                kind: kind.clone(),
                container_extension: ext,
                catchup_days,
                ..Channel::default()
            });
        }
    }
    if channels.is_empty() {
        Err("Aucun média trouvé sur ce compte Xtream.".into())
    } else {
        Ok((channels, info))
    }
}

fn parse_ref(reference: &str) -> Result<(String, String, String), String> {
    let url = Url::parse(reference).map_err(|_| "Référence Xtream invalide.")?;
    if url.scheme() != "xtream" || url.query().is_some() || url.fragment().is_some() {
        return Err("Référence Xtream invalide.".into());
    }
    let account = url.host_str().ok_or("Compte Xtream invalide.")?.to_owned();
    let segments: Vec<_> = url
        .path_segments()
        .ok_or("Média Xtream invalide.")?
        .collect();
    if segments.len() != 2
        || !valid_id(segments[1])
        || !matches!(segments[0], "live" | "movie" | "series" | "episode")
    {
        return Err("Média Xtream invalide.".into());
    }
    Ok((account, segments[0].into(), segments[1].into()))
}

pub fn account_id(reference: &str) -> Result<String, String> {
    parse_ref(reference).map(|(id, _, _)| id)
}

pub fn resolve(
    account: &XtreamAccount,
    password: &str,
    reference: &str,
    ext: Option<&str>,
) -> Result<String, String> {
    let (id, kind, stream_id) = parse_ref(reference)?;
    if id != account.id || kind == "series" {
        return Err("Média Xtream invalide.".into());
    }
    let live_format = live_format(account);
    let (folder, default) = match kind.as_str() {
        "live" => ("live", live_format),
        "movie" => ("movie", "mp4"),
        "episode" => ("series", "mp4"),
        _ => return Err("Média Xtream invalide.".into()),
    };
    let ext = if kind == "live" {
        default.to_owned()
    } else {
        extension(ext.map(str::to_owned), default)
    };
    let mut url = Url::parse(&account.server).map_err(|_| "Serveur Xtream invalide.")?;
    url.path_segments_mut()
        .map_err(|_| "Serveur Xtream invalide.")?
        .pop_if_empty()
        .push(folder)
        .push(&account.username)
        .push(password)
        .push(&format!("{stream_id}.{ext}"));
    Ok(url.to_string())
}

/// HLS when the provider allows it, otherwise MPEG-TS (played through the segmenter).
pub fn live_format(account: &XtreamAccount) -> &'static str {
    if account.output_formats.is_empty() || account.output_formats.iter().any(|f| f == "m3u8") {
        "m3u8"
    } else {
        "ts"
    }
}

/// Catch-up address: `/timeshift/{user}/{pass}/{minutes}/{YYYY-MM-DD:HH-MM}/{id}.{ext}`,
/// with the start expressed in the provider's clock.
pub fn catchup(
    account: &XtreamAccount,
    password: &str,
    reference: &str,
    start: i64,
    stop: i64,
) -> Result<String, String> {
    let (id, kind, stream_id) = parse_ref(reference)?;
    if id != account.id || kind != "live" {
        return Err("Le replay n’est disponible que pour les chaînes en direct.".into());
    }
    if stop <= start {
        return Err("Programme invalide pour le replay.".into());
    }
    let minutes = ((stop - start) as f64 / 60.0).ceil() as i64;
    let local =
        chrono::DateTime::from_timestamp(start + i64::from(account.utc_offset.unwrap_or(0)), 0)
            .ok_or("Horaire de replay invalide.")?;
    let mut url = Url::parse(&account.server).map_err(|_| "Serveur Xtream invalide.")?;
    url.path_segments_mut()
        .map_err(|_| "Serveur Xtream invalide.")?
        .pop_if_empty()
        .push("timeshift")
        .push(&account.username)
        .push(password)
        .push(&minutes.to_string())
        .push(&local.format("%Y-%m-%d:%H-%M").to_string())
        .push(&format!("{stream_id}.ts"));
    Ok(url.to_string())
}

/// Guide published by the provider.
pub fn xmltv_url(account: &XtreamAccount, password: &str) -> Result<String, String> {
    let mut url = Url::parse(&account.server)
        .and_then(|url| url.join("xmltv.php"))
        .map_err(|_| "Adresse Xtream invalide.")?;
    url.query_pairs_mut()
        .append_pair("username", &account.username)
        .append_pair("password", password);
    Ok(url.to_string())
}

pub fn episodes(
    account: &XtreamAccount,
    password: &str,
    reference: &str,
) -> Result<Vec<Episode>, String> {
    let (id, kind, series_id) = parse_ref(reference)?;
    if id != account.id || kind != "series" {
        return Err("Série Xtream invalide.".into());
    }
    let value = api(
        account,
        password,
        Some("get_series_info"),
        Some(("series_id", &series_id)),
    )?;
    let mut episodes = Vec::new();
    let Some(seasons) = value.get("episodes").and_then(Value::as_object) else {
        return Err("Aucun épisode disponible pour cette série.".into());
    };
    for (season, items) in seasons {
        if let Some(items) = items.as_array() {
            for item in items {
                let Some(episode_id) = property(item, "id").filter(|id| valid_id(id)) else {
                    continue;
                };
                let reference = format!("xtream://{}/episode/{}", account.id, episode_id);
                episodes.push(Episode {
                    id: episode_id,
                    name: property(item, "title").unwrap_or_else(|| "Épisode".into()),
                    season: season.clone(),
                    extension: extension(property(item, "container_extension"), "mp4"),
                    stream_url: reference,
                });
            }
        }
    }
    episodes.sort_by(|a, b| a.season.cmp(&b.season).then_with(|| a.id.cmp(&b.id)));
    Ok(episodes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    #[test]
    fn server_validation_and_secret_url() {
        assert_eq!(
            normalize_server("https://example.com/iptv").unwrap(),
            "https://example.com/iptv/"
        );
        assert!(normalize_server("https://user:pass@example.com/").is_err());
        let mut account = XtreamAccount {
            id: "abc".into(),
            server: "https://example.com/iptv/".into(),
            username: "a b".into(),
            ..XtreamAccount::default()
        };
        let url = resolve(&account, "p/ss", "xtream://abc/movie/12", Some("mkv")).unwrap();
        assert_eq!(url, "https://example.com/iptv/movie/a%20b/p%2Fss/12.mkv");
        assert!(resolve(&account, "p", "xtream://other/movie/12", None).is_err());
        assert!(
            resolve(&account, "p", "xtream://abc/live/3", None)
                .unwrap()
                .ends_with("/3.m3u8")
        );
        account.output_formats = vec!["ts".into()];
        assert!(
            resolve(&account, "p", "xtream://abc/live/3", None)
                .unwrap()
                .ends_with("/3.ts")
        );
        account.utc_offset = Some(7200);
        // 2026-09-24 18:00 UTC is 20:00 on a UTC+2 provider clock.
        let replay = catchup(
            &account,
            "p",
            "xtream://abc/live/3",
            1_790_272_800,
            1_790_276_400,
        )
        .unwrap();
        assert_eq!(
            replay,
            "https://example.com/iptv/timeshift/a%20b/p/60/2026-09-24:20-00/3.ts"
        );
        assert!(
            xmltv_url(&account, "p")
                .unwrap()
                .starts_with("https://example.com/iptv/xmltv.php?username=a+b")
        );
    }

    #[test]
    fn imports_catalogue_and_episodes_from_provider() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            for _ in 0..8 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap();
                let request = String::from_utf8_lossy(&buffer[..read]);
                let target = request.split_whitespace().nth(1).unwrap();
                let url = Url::parse(&format!("http://localhost{target}")).unwrap();
                let query: std::collections::HashMap<_, _> =
                    url.query_pairs().into_owned().collect();
                assert_eq!(query.get("password").map(String::as_str), Some("secret"));
                let body = match query.get("action").map(String::as_str) {
                    None => {
                        r#"{"user_info":{"auth":1,"allowed_output_formats":["ts"]},"server_info":{"time_now":"2026-09-24 20:00:05","timestamp_now":1790272800}}"#
                    }
                    Some("get_live_categories") => {
                        r#"[{"category_id":"10","category_name":"Info"}]"#
                    }
                    Some("get_live_streams") => {
                        r#"[{"stream_id":12,"name":"Journal","category_id":"10","epg_channel_id":"news","tv_archive":1,"tv_archive_duration":"7"}]"#
                    }
                    Some("get_vod_categories") => "[]",
                    Some("get_vod_streams") => {
                        r#"[{"stream_id":34,"name":"Film","container_extension":"mkv"}]"#
                    }
                    Some("get_series_categories") => "[]",
                    Some("get_series") => r#"[{"series_id":56,"name":"Série"}]"#,
                    Some("get_series_info") => {
                        r#"{"episodes":{"1":[{"id":"78","title":"Pilote","container_extension":"mp4"}]}}"#
                    }
                    other => panic!("Unexpected action: {other:?}"),
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        let account = XtreamAccount {
            id: "account".into(),
            server: format!("http://127.0.0.1:{port}/"),
            username: "user".into(),
            ..XtreamAccount::default()
        };
        let (channels, info) = import(&account, "secret").unwrap();
        assert_eq!(channels.len(), 3);
        assert_eq!(info.output_formats, vec!["ts"]);
        assert_eq!(info.utc_offset, Some(7200));
        assert_eq!(channels[0].catchup_days, Some(7));
        assert_eq!(channels[0].group, "TV · Info");
        assert_eq!(channels[1].container_extension.as_deref(), Some("mkv"));
        let episodes = episodes(&account, "secret", "xtream://account/series/56").unwrap();
        assert_eq!(episodes[0].stream_url, "xtream://account/episode/78");
        server.join().unwrap();
    }
}
