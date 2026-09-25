//! HTTP helpers shared by the probe, the local proxy, the recorder and the health checks.

use flate2::read::GzDecoder;
use reqwest::blocking::{Client, RequestBuilder};
use serde::{Deserialize, Serialize};
use std::{fs, io::Read, path::Path, time::Duration};

/// Requests made for the player identify as the macOS media stack, like WebKit does.
pub const PLAYER_USER_AGENT: &str =
    "AppleCoreMedia/1.0.0 (Macintosh; U; Intel Mac OS X 10_15_7; fr_fr)";
pub const APP_USER_AGENT: &str = "Fluxo/0.4";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StreamHeaders {
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub referrer: Option<String>,
}

fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .trim()
        .to_owned()
}

impl StreamHeaders {
    pub fn user_agent(&self) -> String {
        self.user_agent
            .as_deref()
            .map(clean)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| PLAYER_USER_AGENT.into())
    }

    pub fn referrer(&self) -> Option<String> {
        self.referrer
            .as_deref()
            .map(clean)
            .filter(|value| !value.is_empty())
    }

    pub fn is_custom(&self) -> bool {
        self.user_agent
            .as_deref()
            .is_some_and(|v| !clean(v).is_empty())
            || self.referrer().is_some()
    }

    pub fn apply(&self, request: RequestBuilder) -> RequestBuilder {
        let mut request = request.header(reqwest::header::USER_AGENT, self.user_agent());
        if let Some(referrer) = self.referrer() {
            if let Ok(url) = url::Url::parse(&referrer) {
                let origin = url.origin().ascii_serialization();
                if origin != "null" {
                    request = request.header(reqwest::header::ORIGIN, origin);
                }
            }
            request = request.header(reqwest::header::REFERER, referrer);
        }
        request
    }
}

pub fn client(timeout: Option<Duration>) -> Result<Client, String> {
    let mut builder = Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .redirect(reqwest::redirect::Policy::limited(8));
    builder = match timeout {
        Some(timeout) => builder.timeout(timeout),
        None => builder.timeout(None),
    };
    builder.build().map_err(|error| error.to_string())
}

pub fn status_message(status: u16, deny_reason: Option<&str>) -> String {
    match status {
        401 => "Le serveur demande une authentification (HTTP 401).".into(),
        403 if deny_reason
            .is_some_and(|reason| reason.contains("deny") || reason.contains("denied")) =>
        {
            "Le serveur bloque l’accès à cette chaîne (HTTP 403 : protection du flux).".into()
        }
        403 => "Le serveur refuse l’accès à cette chaîne (HTTP 403).".into(),
        404 => "Cette adresse de flux est introuvable sur le serveur (HTTP 404).".into(),
        410 => "Ce flux a été retiré du serveur (HTTP 410).".into(),
        429 => "Le serveur limite les demandes de lecture (HTTP 429).".into(),
        451 => "Ce flux n’est pas disponible dans votre pays (HTTP 451).".into(),
        500..=599 => format!("Le serveur de cette chaîne est indisponible (HTTP {status})."),
        _ => format!("Le serveur refuse le flux (HTTP {status})."),
    }
}

pub fn transport_message(error: &reqwest::Error) -> String {
    let detail = format!("{error:?}").to_ascii_lowercase();
    if error.is_timeout() {
        "Le serveur de cette chaîne ne répond pas (délai dépassé).".into()
    } else if detail.contains("dns") || detail.contains("resolve") || detail.contains("nodename") {
        "Le nom du serveur de cette chaîne est introuvable (DNS).".into()
    } else if detail.contains("certificate") || detail.contains("tls") || detail.contains("ssl") {
        "Le certificat HTTPS du serveur est invalide ou expiré.".into()
    } else if error.is_connect() {
        "Connexion impossible au serveur de cette chaîne.".into()
    } else if error.is_redirect() {
        "Le serveur redirige en boucle.".into()
    } else {
        "Le serveur de cette chaîne est inaccessible.".into()
    }
}

pub fn deny_reason(response: &reqwest::blocking::Response) -> Option<String> {
    response
        .headers()
        .get("x-deny-reason")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Reads at most `limit` bytes of a body.
pub fn read_prefix(reader: impl Read, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Downloads (or reads) a text document such as a playlist or a guide, decompressing GZip.
pub fn read_source(source: &str, limit: u64) -> Result<String, String> {
    let bytes = if source.starts_with("https://") || source.starts_with("http://") {
        let response = client(Some(Duration::from_secs(60)))?
            .get(source)
            .header(reqwest::header::USER_AGENT, APP_USER_AGENT)
            .send()
            .map_err(|error| {
                format!("Téléchargement impossible : {}", transport_message(&error))
            })?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            return Err(match status {
                404 | 410 => format!("Fichier introuvable sur le serveur (HTTP {status})."),
                401 | 403 => format!("Le serveur refuse le téléchargement (HTTP {status})."),
                _ => format!("Téléchargement impossible (HTTP {status})."),
            });
        }
        if response.content_length().is_some_and(|len| len > limit) {
            return Err("Fichier trop volumineux.".into());
        }
        let bytes = read_prefix(response, limit + 1).map_err(|e| e.to_string())?;
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
        let decoded = read_prefix(GzDecoder::new(&bytes[..]), limit * 4 + 1)
            .map_err(|e| format!("Archive GZip invalide : {e}"))?;
        if decoded.len() as u64 > limit * 4 {
            return Err("Fichier décompressé trop volumineux.".into());
        }
        decoded
    } else {
        bytes
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Random identifier for local URLs, without an extra dependency.
pub fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut output = String::new();
    for round in 0..2u64 {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(round);
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default(),
        );
        output.push_str(&format!("{:016x}", hasher.finish()));
    }
    output
}

/// Hides credentials that providers embed in stream paths or queries.
pub fn redact(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            let host = parsed.host_str().unwrap_or("");
            let last = parsed
                .path_segments()
                .and_then(|mut segments| segments.next_back())
                .unwrap_or("");
            format!("{}://{host}/…/{last}", parsed.scheme())
        }
        Err(_) => "adresse".into(),
    }
}
