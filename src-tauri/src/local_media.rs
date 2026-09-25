use iptv_core::{Channel, ChannelKind};
use serde::Serialize;
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};
use url::Url;

pub const PLAYLIST_ID: &str = "local-media";
pub const PLAYLIST_SOURCE: &str = "local://media";
const MAX_MEDIA: usize = 500;
const MAX_DEPTH: usize = 8;
const EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "webm", "mkv", "avi", "ts", "mpg", "mpeg", "mp3", "m4a", "aac", "wav",
    "aiff", "aif", "flac", "ogg", "opus", "m2ts", "mts", "wmv", "flv",
];

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanResult {
    pub channels: Vec<Channel>,
    pub skipped: usize,
    pub truncated: bool,
}

pub fn scan(paths: &[String]) -> Result<ScanResult, String> {
    if paths.is_empty() {
        return Err("Aucun fichier déposé.".into());
    }
    if paths.len() > MAX_MEDIA {
        return Err("Déposez au maximum 500 éléments à la fois.".into());
    }
    let mut result = ScanResult {
        channels: Vec::new(),
        skipped: 0,
        truncated: false,
    };
    let mut seen_files = HashSet::new();
    let mut seen_dirs = HashSet::new();
    for path in paths {
        let path = Path::new(path);
        let group = if path.is_dir() {
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Dossier")
                .to_owned()
        } else {
            "Fichiers locaux".into()
        };
        visit(
            path,
            &group,
            0,
            &mut seen_files,
            &mut seen_dirs,
            &mut result,
        );
        if result.truncated {
            break;
        }
    }
    if result.channels.is_empty() {
        return Err("Aucun fichier audio ou vidéo reconnu dans le dépôt.".into());
    }
    result
        .channels
        .sort_by(|a, b| a.group.cmp(&b.group).then_with(|| a.name.cmp(&b.name)));
    Ok(result)
}

fn visit(
    path: &Path,
    group: &str,
    depth: usize,
    seen_files: &mut HashSet<PathBuf>,
    seen_dirs: &mut HashSet<PathBuf>,
    result: &mut ScanResult,
) {
    if result.channels.len() >= MAX_MEDIA {
        result.truncated = true;
        return;
    }
    let Ok(canonical) = path.canonicalize() else {
        result.skipped += 1;
        return;
    };
    if canonical.is_dir() {
        if depth >= MAX_DEPTH || !seen_dirs.insert(canonical.clone()) {
            return;
        }
        let Ok(entries) = fs::read_dir(&canonical) else {
            result.skipped += 1;
            return;
        };
        let mut entries: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                !path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with('.'))
            })
            .collect();
        entries.sort();
        for entry in entries {
            visit(&entry, group, depth + 1, seen_files, seen_dirs, result);
            if result.truncated {
                break;
            }
        }
        return;
    }
    if !canonical.is_file() || !seen_files.insert(canonical.clone()) {
        return;
    }
    let Some(ext) = canonical
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
    else {
        result.skipped += 1;
        return;
    };
    if !EXTENSIONS.contains(&ext.as_str()) {
        result.skipped += 1;
        return;
    }
    let Ok(url) = Url::from_file_path(&canonical) else {
        result.skipped += 1;
        return;
    };
    let name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Média")
        .to_owned();
    let stream_url = url.to_string();
    result.channels.push(Channel {
        id: stream_url.clone(),
        name,
        group: group.to_owned(),
        stream_url,
        logo: None,
        tvg_id: None,
        kind: ChannelKind::Movie,
        container_extension: Some(ext),
        ..Channel::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn scans_nested_media_deduplicates_and_skips_other_files() {
        let dir = std::env::temp_dir().join(format!(
            "fluxo-scan-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join("Saison 1")).unwrap();
        fs::write(dir.join("Saison 1/episode 1.mp4"), b"").unwrap();
        fs::write(dir.join("Saison 1/episode 2.mkv"), b"").unwrap();
        fs::write(dir.join("notes.txt"), b"").unwrap();
        let root = dir.to_string_lossy().into_owned();
        let result = scan(&[
            root.clone(),
            dir.join("Saison 1/episode 1.mp4")
                .to_string_lossy()
                .into_owned(),
        ])
        .unwrap();
        assert_eq!(result.channels.len(), 2);
        assert_eq!(result.skipped, 1);
        assert_eq!(
            result.channels[0].group,
            dir.file_name().unwrap().to_string_lossy()
        );
        assert!(result.channels[0].stream_url.starts_with("file://"));
        fs::remove_dir_all(dir).unwrap();
    }
}
