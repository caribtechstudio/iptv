//! Background availability checks of channel sources, persisted in `health.json`.

use crate::net::{self, StreamHeaders};
use iptv_core::{hls, ts};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Emitter};

const WORKERS: usize = 10;
const MAX_ENTRIES: usize = 60_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HealthEntry {
    pub ok: bool,
    #[serde(default)]
    pub message: Option<String>,
    pub checked_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckItem {
    pub url: String,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub referrer: Option<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct HealthEvent {
    url: String,
    #[serde(flatten)]
    entry: HealthEntry,
}

pub struct Health {
    path: PathBuf,
    entries: Mutex<HashMap<String, HealthEntry>>,
    queue: Mutex<VecDeque<CheckItem>>,
    queued: Mutex<HashSet<String>>,
    active: Mutex<usize>,
}

pub fn check(item: &CheckItem) -> HealthEntry {
    let now = chrono::Utc::now().timestamp();
    let headers = StreamHeaders {
        user_agent: item.user_agent.clone(),
        referrer: item.referrer.clone(),
    };
    let failed = |message: String| HealthEntry {
        ok: false,
        message: Some(message),
        checked_at: now,
    };
    let client = match net::client(Some(Duration::from_secs(8))) {
        Ok(client) => client,
        Err(error) => return failed(error),
    };
    let response = match headers.apply(client.get(&item.url)).send() {
        Ok(response) => response,
        Err(error) => return failed(net::transport_message(&error)),
    };
    if !response.status().is_success() {
        return failed(net::status_message(
            response.status().as_u16(),
            net::deny_reason(&response).as_deref(),
        ));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let body = match net::read_prefix(response, 16 * 1024) {
        Ok(body) => body,
        Err(_) => return failed("Le serveur a interrompu la réponse.".into()),
    };
    let text = String::from_utf8_lossy(&body);
    let ok = if hls::is_playlist(&text) {
        text.contains("#EXTINF") || hls::is_master(&text)
    } else {
        ts::looks_like_ts(&body)
            || body.get(4..8).is_some_and(|b| b == b"ftyp" || b == b"styp")
            || content_type.starts_with("video/")
            || content_type.starts_with("audio/")
            || content_type.contains("octet-stream")
    };
    if ok {
        HealthEntry {
            ok: true,
            message: None,
            checked_at: now,
        }
    } else if hls::is_playlist(&text) {
        failed("La playlist HLS est vide : la chaîne est peut-être hors antenne.".into())
    } else if content_type.contains("html") {
        failed("Cette adresse renvoie une page web, pas un flux vidéo.".into())
    } else {
        failed("Le serveur ne renvoie pas un flux vidéo reconnu.".into())
    }
}

impl Health {
    pub fn load(path: PathBuf) -> Arc<Self> {
        let entries = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Arc::new(Self {
            path,
            entries: Mutex::new(entries),
            queue: Mutex::new(VecDeque::new()),
            queued: Mutex::new(HashSet::new()),
            active: Mutex::new(0),
        })
    }

    pub fn snapshot(&self) -> HashMap<String, HealthEntry> {
        self.entries
            .lock()
            .map(|entries| entries.clone())
            .unwrap_or_default()
    }

    fn save(&self) {
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        if entries.len() > MAX_ENTRIES {
            let mut ages: Vec<i64> = entries.values().map(|entry| entry.checked_at).collect();
            ages.sort_unstable();
            let cutoff = ages[entries.len() - MAX_ENTRIES];
            entries.retain(|_, entry| entry.checked_at >= cutoff);
        }
        if let Ok(bytes) = serde_json::to_vec(&*entries) {
            let tmp = self.path.with_extension("tmp");
            if fs::write(&tmp, bytes).is_ok() {
                let _ = fs::rename(tmp, &self.path);
            }
        }
    }

    /// Forgets every availability result, in memory and on disk.
    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.clear();
        }
        self.save();
    }

    pub fn record(&self, url: String, entry: HealthEntry) {
        if let Ok(mut entries) = self.entries.lock() {
            entries.insert(url, entry);
        }
        self.save();
    }

    /// Queues checks; results are sent as `health` events.
    pub fn enqueue(self: &Arc<Self>, app: AppHandle, items: Vec<CheckItem>) -> usize {
        let mut added = 0;
        if let (Ok(mut queue), Ok(mut queued)) = (self.queue.lock(), self.queued.lock()) {
            for item in items {
                if (item.url.starts_with("http://") || item.url.starts_with("https://"))
                    && queued.insert(item.url.clone())
                {
                    queue.push_back(item);
                    added += 1;
                }
            }
        }
        let Ok(mut active) = self.active.lock() else {
            return added;
        };
        while *active < WORKERS {
            *active += 1;
            let health = self.clone();
            let app = app.clone();
            std::thread::spawn(move || health.work(app));
        }
        added
    }

    fn work(self: Arc<Self>, app: AppHandle) {
        let mut batch = Vec::new();
        loop {
            let item = self
                .queue
                .lock()
                .ok()
                .and_then(|mut queue| queue.pop_front());
            let Some(item) = item else { break };
            let entry = check(&item);
            if let Ok(mut entries) = self.entries.lock() {
                entries.insert(item.url.clone(), entry.clone());
            }
            if let Ok(mut queued) = self.queued.lock() {
                queued.remove(&item.url);
            }
            batch.push(HealthEvent {
                url: item.url,
                entry,
            });
            if batch.len() >= 8 {
                let _ = app.emit("health", std::mem::take(&mut batch));
            }
        }
        if !batch.is_empty() {
            let _ = app.emit("health", batch);
        }
        let last = self.active.lock().map(|mut active| {
            *active -= 1;
            *active == 0
        });
        if last.unwrap_or(false) {
            self.save();
            let _ = app.emit("health-done", ());
        }
    }
}
