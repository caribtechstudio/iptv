//! Library persistence: small user state (`state.json`) and one catalogue file per playlist.
//! Favourites, history or progress updates only rewrite the small state file.

use iptv_core::{Channel, Library};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub struct Store {
    dir: PathBuf,
    pub library: Mutex<Library>,
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn safe_file_name(id: &str) -> String {
    id.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

impl Store {
    pub fn load(dir: PathBuf) -> Self {
        let state = dir.join("state.json");
        let legacy = dir.join("library.json");
        let (library, migrated) = if state.is_file() {
            let mut library: Library = match fs::read(&state)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            {
                Some(library) => library,
                None => {
                    let _ = fs::rename(
                        &state,
                        state.with_extension(format!(
                            "json.corrupt.{}",
                            chrono::Utc::now().timestamp()
                        )),
                    );
                    Library::default()
                }
            };
            for playlist in &mut library.playlists {
                playlist.channels = fs::read(
                    dir.join("catalog")
                        .join(format!("{}.json", safe_file_name(&playlist.id))),
                )
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Vec<Channel>>(&bytes).ok())
                .unwrap_or_default();
            }
            (library, false)
        } else if let Ok(bytes) = fs::read(&legacy) {
            match serde_json::from_slice::<Library>(&bytes) {
                Ok(library) => (library, true),
                Err(_) => {
                    let _ = fs::rename(
                        &legacy,
                        legacy.with_extension(format!(
                            "json.corrupt.{}",
                            chrono::Utc::now().timestamp()
                        )),
                    );
                    (Library::default(), false)
                }
            }
        } else {
            (Library::default(), false)
        };
        let store = Self {
            dir,
            library: Mutex::new(library),
        };
        if migrated {
            let mut library = store.library.lock().expect("library lock");
            let ids: Vec<String> = library.playlists.iter().map(|p| p.id.clone()).collect();
            // The legacy file is kept untouched so an older Fluxo build still finds its data;
            // `state.json` takes precedence from now on.
            let _ = store.save(&mut library, &ids);
        }
        store
    }

    fn catalog_path(&self, id: &str) -> PathBuf {
        self.dir
            .join("catalog")
            .join(format!("{}.json", safe_file_name(id)))
    }

    fn save(&self, library: &mut Library, catalogs: &[String]) -> Result<(), String> {
        for id in catalogs {
            match library.playlists.iter().find(|playlist| &playlist.id == id) {
                Some(playlist) => write_atomic(
                    &self.catalog_path(id),
                    &serde_json::to_vec(&playlist.channels).map_err(|e| e.to_string())?,
                )?,
                None => {
                    let _ = fs::remove_file(self.catalog_path(id));
                }
            }
        }
        let channels: Vec<Vec<Channel>> = library
            .playlists
            .iter_mut()
            .map(|playlist| std::mem::take(&mut playlist.channels))
            .collect();
        let state = serde_json::to_vec_pretty(&*library).map_err(|e| e.to_string());
        for (playlist, channels) in library.playlists.iter_mut().zip(channels) {
            playlist.channels = channels;
        }
        write_atomic(&self.dir.join("state.json"), &state?)
    }

    /// Applies a change and persists it. Catalogue files are rewritten only for playlists
    /// that were added, removed or whose `updated_at` changed.
    pub fn change<R>(
        &self,
        f: impl FnOnce(&mut Library) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut library = self.library.lock().map_err(|e| e.to_string())?;
        let before: HashMap<String, String> = library
            .playlists
            .iter()
            .map(|playlist| (playlist.id.clone(), playlist.updated_at.clone()))
            .collect();
        let result = f(&mut library)?;
        let mut changed: Vec<String> = library
            .playlists
            .iter()
            .filter(|playlist| before.get(&playlist.id) != Some(&playlist.updated_at))
            .map(|playlist| playlist.id.clone())
            .collect();
        changed.extend(
            before
                .keys()
                .filter(|id| !library.playlists.iter().any(|playlist| &playlist.id == *id))
                .cloned(),
        );
        self.save(&mut library, &changed)?;
        Ok(result)
    }

    pub fn read<R>(&self, f: impl FnOnce(&Library) -> R) -> Result<R, String> {
        Ok(f(&*self.library.lock().map_err(|e| e.to_string())?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iptv_core::Playlist;

    #[test]
    fn migrates_legacy_file_and_keeps_catalogs_out_of_state() {
        let dir = std::env::temp_dir().join(format!("fluxo-store-{}", crate::net::random_token()));
        fs::create_dir_all(&dir).unwrap();
        let mut library = Library::default();
        library.playlists.push(Playlist {
            id: "p1".into(),
            name: "Liste".into(),
            source: "https://example.org/a.m3u".into(),
            channels: vec![Channel {
                id: "c".into(),
                name: "Chaîne".into(),
                stream_url: "https://e/c.m3u8".into(),
                ..Channel::default()
            }],
            updated_at: "t0".into(),
            epg_url: None,
        });
        fs::write(
            dir.join("library.json"),
            serde_json::to_vec(&library).unwrap(),
        )
        .unwrap();

        let store = Store::load(dir.clone());
        assert!(dir.join("library.json").is_file());
        assert!(dir.join("catalog/p1.json").is_file());
        let state = fs::read_to_string(dir.join("state.json")).unwrap();
        assert!(!state.contains("Chaîne"));
        store
            .change(|library| {
                library.favorites.push("c".into());
                Ok(())
            })
            .unwrap();

        let reloaded = Store::load(dir.clone());
        let library = reloaded.library.lock().unwrap();
        assert_eq!(library.playlists[0].channels.len(), 1);
        assert_eq!(library.favorites, vec!["c"]);
        drop(library);
        reloaded
            .change(|library| {
                library.playlists.clear();
                Ok(())
            })
            .unwrap();
        assert!(!dir.join("catalog/p1.json").exists());
        let _ = fs::remove_dir_all(dir);
    }
}
