//! Export of playlists to M3U files other players can open. Xtream references are resolved
//! to the provider's addresses (they carry the account credentials); series are containers
//! of episodes and have no address of their own, so they are left out.

use crate::xtream;
use iptv_core::{
    Channel, ChannelKind, Library, Playlist, XtreamAccount,
    export::{ExportEntry, write_m3u},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub playlists: Vec<String>,
    /// A file (everything merged) or, with `separate`, a folder receiving one file per playlist.
    pub destination: String,
    #[serde(default)]
    pub separate: bool,
}

#[derive(Serialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub files: Vec<String>,
    pub channels: usize,
    /// Series and entries whose address could not be resolved.
    pub skipped: usize,
}

struct Resolved {
    channels: Vec<(Channel, String)>,
    guides: Vec<String>,
    skipped: usize,
}

/// Credentials of each Xtream account, read from the keychain once per export.
struct Accounts<'a> {
    known: &'a [XtreamAccount],
    passwords: HashMap<String, Option<String>>,
}

impl Accounts<'_> {
    fn password(&mut self, id: &str) -> Option<(&XtreamAccount, String)> {
        let account = self.known.iter().find(|account| account.id == id)?;
        let password = self
            .passwords
            .entry(id.to_owned())
            .or_insert_with(|| xtream::password(id).ok())
            .clone()?;
        Some((account, password))
    }
}

fn resolve_playlist(playlist: &Playlist, accounts: &mut Accounts) -> Resolved {
    let mut resolved = Resolved {
        channels: Vec::with_capacity(playlist.channels.len()),
        guides: playlist
            .epg_url
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
            .map(str::to_owned)
            .collect(),
        skipped: 0,
    };
    if let Some((account, password)) = accounts.password(&playlist.id)
        && let Ok(url) = xtream::xmltv_url(account, &password)
    {
        resolved.guides.push(url);
    }
    for channel in &playlist.channels {
        if channel.kind == ChannelKind::Series {
            resolved.skipped += 1;
            continue;
        }
        let url = if channel.stream_url.starts_with("xtream://") {
            let resolved_url = xtream::account_id(&channel.stream_url)
                .ok()
                .and_then(|id| accounts.password(&id))
                .and_then(|(account, password)| {
                    xtream::resolve(
                        account,
                        &password,
                        &channel.stream_url,
                        channel.container_extension.as_deref(),
                    )
                    .ok()
                });
            match resolved_url {
                Some(url) => url,
                None => {
                    resolved.skipped += 1;
                    continue;
                }
            }
        } else {
            channel.stream_url.clone()
        };
        resolved.channels.push((channel.clone(), url));
    }
    resolved
}

fn render(parts: &[&Resolved]) -> (String, usize) {
    let mut seen = HashSet::new();
    let mut guides: Vec<String> = Vec::new();
    let mut entries = Vec::new();
    for part in parts {
        for guide in &part.guides {
            if !guides.contains(guide) {
                guides.push(guide.clone());
            }
        }
        for (channel, url) in &part.channels {
            if seen.insert(url.as_str()) {
                entries.push(ExportEntry { channel, url });
            }
        }
    }
    (write_m3u(&entries, &guides), entries.len())
}

/// Characters macOS, Windows and Linux all accept in a file name.
pub fn file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
            ch if ch.is_control() => ' ',
            ch => ch,
        })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    if cleaned.is_empty() {
        "Playlist".into()
    } else {
        cleaned.chars().take(120).collect()
    }
}

/// `dir/stem.ext`, or `dir/stem (2).ext`… when a file already has that name.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let mut path = dir.join(format!("{stem}.{ext}"));
    let mut index = 2;
    while path.exists() {
        path = dir.join(format!("{stem} ({index}).{ext}"));
        index += 1;
    }
    path
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    let tmp = path.with_extension("m3u.part");
    fs::write(&tmp, text).map_err(|error| format!("Écriture impossible : {error}"))?;
    fs::rename(&tmp, path).map_err(|error| {
        let _ = fs::remove_file(&tmp);
        format!("Écriture impossible : {error}")
    })
}

/// Runs the export. `library` is a snapshot: keychain reads and writes happen outside the store lock.
pub fn export(library: &Library, request: &Request) -> Result<Outcome, String> {
    let playlists: Vec<&Playlist> = request
        .playlists
        .iter()
        .filter_map(|id| library.playlists.iter().find(|playlist| &playlist.id == id))
        .collect();
    if playlists.is_empty() {
        return Err("Choisissez au moins une playlist à exporter.".into());
    }
    let mut accounts = Accounts {
        known: &library.xtream_accounts,
        passwords: HashMap::new(),
    };
    let resolved: Vec<Resolved> = playlists
        .iter()
        .map(|playlist| resolve_playlist(playlist, &mut accounts))
        .collect();
    let skipped = resolved.iter().map(|part| part.skipped).sum();
    let destination = PathBuf::from(request.destination.trim());
    if request.separate {
        if !destination.is_dir() {
            return Err("Choisissez un dossier de destination.".into());
        }
        let mut outcome = Outcome {
            files: Vec::new(),
            channels: 0,
            skipped,
        };
        for (playlist, part) in playlists.iter().zip(&resolved) {
            let (text, count) = render(&[part]);
            let path = unique_path(&destination, &file_stem(&playlist.name), "m3u");
            write(&path, &text)?;
            outcome.files.push(path.to_string_lossy().into_owned());
            outcome.channels += count;
        }
        return Ok(outcome);
    }
    let mut path = destination;
    if path.as_os_str().is_empty() || path.is_dir() {
        return Err("Choisissez le fichier à créer.".into());
    }
    let has_extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "m3u" | "m3u8"));
    if !has_extension {
        path.set_extension("m3u");
    }
    let parts: Vec<&Resolved> = resolved.iter().collect();
    let (text, channels) = render(&parts);
    write(&path, &text)?;
    Ok(Outcome {
        files: vec![path.to_string_lossy().into_owned()],
        channels,
        skipped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playlist(id: &str, name: &str, urls: &[&str]) -> Playlist {
        Playlist {
            id: id.into(),
            name: name.into(),
            source: String::new(),
            channels: urls
                .iter()
                .map(|url| Channel {
                    id: (*url).into(),
                    name: format!("Chaîne {url}"),
                    group: "Groupe".into(),
                    stream_url: (*url).into(),
                    ..Channel::default()
                })
                .collect(),
            updated_at: String::new(),
            epg_url: Some("https://epg.example/guide.xml".into()),
        }
    }

    #[test]
    fn merges_or_splits_playlists_and_skips_series() {
        let dir = std::env::temp_dir().join(format!("fluxo-export-{}", crate::net::random_token()));
        fs::create_dir_all(&dir).unwrap();
        let mut first = playlist("a", "Sport/Info", &["https://e/1.m3u8", "https://e/2.m3u8"]);
        first.channels.push(Channel {
            id: "s".into(),
            name: "Série".into(),
            stream_url: "xtream://a/series/4".into(),
            kind: ChannelKind::Series,
            ..Channel::default()
        });
        let library = Library {
            playlists: vec![
                first,
                playlist("b", "Films", &["https://e/2.m3u8", "file:///tmp/film.mkv"]),
            ],
            ..Library::default()
        };

        let merged = export(
            &library,
            &Request {
                playlists: vec!["a".into(), "b".into()],
                destination: dir.join("tout").to_string_lossy().into_owned(),
                separate: false,
            },
        )
        .unwrap();
        assert_eq!(merged.channels, 3, "the shared address is written once");
        assert_eq!(merged.skipped, 1);
        assert!(merged.files[0].ends_with("tout.m3u"));
        let text = fs::read_to_string(&merged.files[0]).unwrap();
        assert_eq!(text.matches("#EXTINF").count(), 3);
        assert_eq!(text.matches("guide.xml").count(), 1);

        let split = export(
            &library,
            &Request {
                playlists: vec!["a".into(), "b".into()],
                destination: dir.to_string_lossy().into_owned(),
                separate: true,
            },
        )
        .unwrap();
        assert_eq!(split.files.len(), 2);
        assert!(split.files[0].ends_with("Sport-Info.m3u"));
        assert!(split.files[1].ends_with("Films.m3u"));
        assert_eq!(split.channels, 4);

        let again = export(
            &library,
            &Request {
                playlists: vec!["b".into()],
                destination: dir.to_string_lossy().into_owned(),
                separate: true,
            },
        )
        .unwrap();
        assert!(
            again.files[0].ends_with("Films (2).m3u"),
            "never overwrites"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn file_names_are_portable() {
        assert_eq!(file_stem("  ../a:b*c?  "), "-a-b-c-");
        assert_eq!(file_stem(""), "Playlist");
    }
}
