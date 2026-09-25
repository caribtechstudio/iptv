//! TV guide: merges several XMLTV sources (manual, playlist `x-tvg-url`, Xtream `xmltv.php`),
//! matches channels by id, base id or normalised name, and answers now/next and grid queries.

use crate::net;
use iptv_core::{
    Program, XmltvGuide,
    names::{base_tvg_id, normalize_channel_name},
    parse_xmltv_guide,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const MAX_EPG_BYTES: u64 = 80 * 1024 * 1024;
const KEEP_PAST: i64 = 8 * 86_400;
const KEEP_FUTURE: i64 = 4 * 86_400;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceStatus {
    pub label: String,
    pub programs: usize,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Source {
    pub label: String,
    pub location: String,
}

#[derive(Default)]
pub struct Guide {
    programs: HashMap<String, Vec<Program>>,
    ids: HashMap<String, String>,
    base_ids: HashMap<String, String>,
    names: HashMap<String, String>,
    pub sources: Vec<SourceStatus>,
    pub loaded_at: i64,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpgRef {
    pub key: String,
    #[serde(default)]
    pub tvg_id: Option<String>,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NowNext {
    pub now: Option<Program>,
    pub next: Option<Program>,
}

impl Guide {
    pub fn build(parts: Vec<(Source, Result<XmltvGuide, String>)>, now: i64) -> Self {
        let mut guide = Guide {
            loaded_at: now,
            ..Guide::default()
        };
        for (source, result) in parts {
            match result {
                Ok(parsed) => {
                    let mut count = 0;
                    for channel in parsed.channels {
                        guide
                            .ids
                            .entry(channel.id.to_lowercase())
                            .or_insert_with(|| channel.id.clone());
                        guide
                            .base_ids
                            .entry(base_tvg_id(&channel.id))
                            .or_insert_with(|| channel.id.clone());
                        for name in &channel.names {
                            let key = normalize_channel_name(name);
                            if !key.is_empty() {
                                guide.names.entry(key).or_insert_with(|| channel.id.clone());
                            }
                        }
                    }
                    for program in parsed.programs {
                        if program.stop < now - KEEP_PAST || program.start > now + KEEP_FUTURE {
                            continue;
                        }
                        count += 1;
                        guide
                            .ids
                            .entry(program.channel_id.to_lowercase())
                            .or_insert_with(|| program.channel_id.clone());
                        guide
                            .base_ids
                            .entry(base_tvg_id(&program.channel_id))
                            .or_insert_with(|| program.channel_id.clone());
                        guide
                            .programs
                            .entry(program.channel_id.clone())
                            .or_default()
                            .push(program);
                    }
                    guide.sources.push(SourceStatus {
                        label: source.label,
                        programs: count,
                        error: None,
                    });
                }
                Err(error) => guide.sources.push(SourceStatus {
                    label: source.label,
                    programs: 0,
                    error: Some(error),
                }),
            }
        }
        for list in guide.programs.values_mut() {
            list.sort_by_key(|program| program.start);
            list.dedup_by(|a, b| a.start == b.start);
        }
        guide
    }

    pub fn load(sources: Vec<Source>, now: i64) -> Self {
        let handles: Vec<_> = sources
            .into_iter()
            .map(|source| {
                std::thread::spawn(move || {
                    let parsed = net::read_source(&source.location, MAX_EPG_BYTES)
                        .and_then(|text| parse_xmltv_guide(&text));
                    (source, parsed)
                })
            })
            .collect();
        let parts = handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect();
        Self::build(parts, now)
    }

    pub fn program_count(&self) -> usize {
        self.programs.values().map(Vec::len).sum()
    }

    /// Guide id for a channel: exact id, case-insensitive id, id without `@feed`, then name.
    pub fn resolve(&self, tvg_id: Option<&str>, name: &str) -> Option<&str> {
        if let Some(id) = tvg_id.map(str::trim).filter(|id| !id.is_empty()) {
            if self.programs.contains_key(id) {
                return self.programs.get_key_value(id).map(|(key, _)| key.as_str());
            }
            if let Some(found) = self
                .ids
                .get(&id.to_lowercase())
                .or_else(|| self.base_ids.get(&base_tvg_id(id)))
                && self.programs.contains_key(found)
            {
                return Some(found);
            }
        }
        let key = normalize_channel_name(name);
        self.names
            .get(&key)
            .filter(|id| self.programs.contains_key(*id))
            .map(String::as_str)
    }

    pub fn programs(&self, reference: &EpgRef, from: i64, to: i64, limit: usize) -> Vec<Program> {
        let Some(id) = self.resolve(reference.tvg_id.as_deref(), &reference.name) else {
            return Vec::new();
        };
        self.programs[id]
            .iter()
            .filter(|program| program.stop > from && program.start < to)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn now_next(&self, reference: &EpgRef, now: i64) -> Option<NowNext> {
        let id = self.resolve(reference.tvg_id.as_deref(), &reference.name)?;
        let list = &self.programs[id];
        let index = list.partition_point(|program| program.stop <= now);
        let now_program = list
            .get(index)
            .filter(|program| program.start <= now)
            .cloned();
        let next = list
            .get(if now_program.is_some() {
                index + 1
            } else {
                index
            })
            .cloned();
        (now_program.is_some() || next.is_some()).then_some(NowNext {
            now: now_program,
            next,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iptv_core::XmltvChannel;

    fn program(channel: &str, start: i64, stop: i64, title: &str) -> Program {
        Program {
            channel_id: channel.into(),
            title: title.into(),
            description: String::new(),
            start,
            stop,
        }
    }

    #[test]
    fn matches_channels_by_id_feed_and_name() {
        let parsed = XmltvGuide {
            channels: vec![XmltvChannel {
                id: "M6.fr".into(),
                names: vec!["M6".into()],
            }],
            programs: vec![
                program("M6.fr", 1000, 2000, "Avant"),
                program("M6.fr", 2000, 3000, "Maintenant"),
                program("M6.fr", 3000, 4000, "Après"),
                program("Old.fr", -9_000_000, -8_999_000, "Trop vieux"),
            ],
        };
        let guide = Guide::build(
            vec![(
                Source {
                    label: "test".into(),
                    location: String::new(),
                },
                Ok(parsed),
            )],
            2500,
        );
        assert_eq!(guide.program_count(), 3);
        assert_eq!(guide.resolve(Some("M6.fr@HD"), "x"), Some("M6.fr"));
        assert_eq!(guide.resolve(Some("m6.FR"), "x"), Some("M6.fr"));
        assert_eq!(guide.resolve(None, "M6 HD (1080p)"), Some("M6.fr"));
        let reference = EpgRef {
            key: "k".into(),
            tvg_id: None,
            name: "M6".into(),
        };
        let now = guide.now_next(&reference, 2500).unwrap();
        assert_eq!(now.now.unwrap().title, "Maintenant");
        assert_eq!(now.next.unwrap().title, "Après");
        assert_eq!(guide.programs(&reference, 2500, 3500, 10).len(), 2);
    }
}
