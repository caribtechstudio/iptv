use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub group: String,
    pub stream_url: String,
    pub logo: Option<String>,
    pub tvg_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub source: String,
    pub channels: Vec<Channel>,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentItem {
    pub name: String,
    pub url: String,
    pub played_at: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    #[serde(default)]
    pub playlists: Vec<Playlist>,
    #[serde(default)]
    pub favorites: Vec<String>,
    #[serde(default)]
    pub recent: Vec<RecentItem>,
    #[serde(default)]
    pub epg_source: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Program {
    pub channel_id: String,
    pub title: String,
    pub description: String,
    pub start: i64,
    pub stop: i64,
}

pub fn parse_m3u(input: &str, base: Option<&str>) -> Result<Vec<Channel>, String> {
    let input = input.trim_start_matches('\u{feff}');
    if !input
        .lines()
        .any(|line| line.trim_start().starts_with("#EXTINF:"))
    {
        return Err("Aucune chaîne trouvée dans la playlist M3U.".into());
    }
    let mut channels = Vec::new();
    let mut pending: Option<(String, String, Option<String>, Option<String>)> = None;
    let mut group_override: Option<String> = None;
    let mut seen = HashSet::new();
    for line in input.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            let attrs = parse_attributes(info);
            let name = split_extinf_name(info)
                .filter(|s| !s.is_empty())
                .or_else(|| attrs.get("tvg-name").cloned())
                .unwrap_or_else(|| "Chaîne sans nom".into());
            let group = attrs
                .get("group-title")
                .cloned()
                .or_else(|| group_override.take())
                .unwrap_or_else(|| "Autres".into());
            pending = Some((
                name,
                group,
                attrs.get("tvg-logo").cloned(),
                attrs.get("tvg-id").cloned(),
            ));
        } else if let Some(group) = line.strip_prefix("#EXTGRP:") {
            if let Some(pending) = &mut pending {
                pending.1 = group.trim().to_owned();
            } else {
                group_override = Some(group.trim().to_owned());
            }
        } else if !line.starts_with('#')
            && let Some((name, group, logo, tvg_id)) = pending.take()
        {
            let stream_url = resolve_url(line, base).unwrap_or_else(|| line.to_owned());
            if !is_playable_source(&stream_url) || !seen.insert(stream_url.clone()) {
                continue;
            }
            let id = format!("{}|{}", tvg_id.as_deref().unwrap_or(""), stream_url);
            channels.push(Channel {
                id,
                name,
                group,
                stream_url,
                logo,
                tvg_id,
            });
        }
    }
    if channels.is_empty() {
        Err("Aucun flux valide dans la playlist.".into())
    } else {
        Ok(channels)
    }
}

fn split_extinf_name(info: &str) -> Option<String> {
    let mut quoted = false;
    for (index, ch) in info.char_indices() {
        if ch == '"' {
            quoted = !quoted;
        }
        if ch == ',' && !quoted {
            return Some(info[index + 1..].trim().to_owned());
        }
    }
    None
}

fn parse_attributes(info: &str) -> std::collections::HashMap<String, String> {
    let mut result = std::collections::HashMap::new();
    let bytes = info.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && !bytes[i].is_ascii_alphabetic() {
            i += 1;
        }
        let start = i;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_')
        {
            i += 1;
        }
        if start == i {
            break;
        }
        let key = &info[start..i];
        if i >= bytes.len() || bytes[i] != b'=' {
            continue;
        }
        i += 1;
        if i >= bytes.len() {
            break;
        }
        let value = if bytes[i] == b'"' {
            i += 1;
            let begin = i;
            while i < bytes.len() && bytes[i] != b'"' {
                i += 1;
            }
            let value = &info[begin..i];
            if i < bytes.len() {
                i += 1;
            }
            value
        } else {
            let begin = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b',' {
                i += 1;
            }
            &info[begin..i]
        };
        result.insert(key.to_ascii_lowercase(), value.to_owned());
    }
    result
}

fn resolve_url(value: &str, base: Option<&str>) -> Option<String> {
    if let Ok(url) = Url::parse(value) {
        return Some(url.to_string());
    }
    base.and_then(|base| Url::parse(base).ok())?
        .join(value)
        .ok()
        .map(|url| url.to_string())
}

pub fn is_playable_source(value: &str) -> bool {
    Url::parse(value)
        .map(|url| matches!(url.scheme(), "http" | "https" | "file"))
        .unwrap_or(false)
}

pub fn is_hls_manifest(input: &str) -> bool {
    input
        .lines()
        .any(|line| line.trim_start().starts_with("#EXT-X-"))
}

pub fn parse_xmltv(input: &str) -> Result<Vec<Program>, String> {
    let mut reader = Reader::from_str(input);
    reader.config_mut().trim_text(false);
    let mut programs = Vec::new();
    let mut current: Option<Program> = None;
    let mut field = None::<&str>;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == b"programme" => {
                let mut channel = None;
                let mut start = None;
                let mut stop = None;
                for attr in e.attributes().flatten() {
                    let value = attr
                        .decode_and_unescape_value(reader.decoder())
                        .map_err(|err| err.to_string())?
                        .into_owned();
                    match attr.key.as_ref() {
                        b"channel" => channel = Some(value),
                        b"start" => start = parse_xmltv_time(&value),
                        b"stop" => stop = parse_xmltv_time(&value),
                        _ => {}
                    }
                }
                current = match (channel, start, stop) {
                    (Some(channel_id), Some(start), Some(stop)) if stop > start => Some(Program {
                        channel_id,
                        title: String::new(),
                        description: String::new(),
                        start,
                        stop,
                    }),
                    _ => None,
                };
            }
            Ok(Event::Start(e)) if e.name().as_ref() == b"title" => field = Some("title"),
            Ok(Event::Start(e)) if e.name().as_ref() == b"desc" => field = Some("desc"),
            Ok(Event::Text(e)) => {
                if let (Some(program), Some(field)) = (&mut current, field) {
                    let text = e.xml_content().map_err(|err| err.to_string())?;
                    if field == "title" {
                        program.title.push_str(&text);
                    } else {
                        program.description.push_str(&text);
                    }
                }
            }
            Ok(Event::CData(e)) => {
                if let (Some(program), Some(field)) = (&mut current, field) {
                    let text = e.decode().map_err(|err| err.to_string())?;
                    if field == "title" {
                        program.title.push_str(&text);
                    } else {
                        program.description.push_str(&text);
                    }
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if let (Some(program), Some(field)) = (&mut current, field) {
                    let decoded = e.decode().map_err(|err| err.to_string())?;
                    let value =
                        if let Some(ch) = e.resolve_char_ref().map_err(|err| err.to_string())? {
                            ch.to_string()
                        } else {
                            quick_xml::escape::resolve_predefined_entity(&decoded)
                                .unwrap_or("")
                                .to_owned()
                        };
                    if field == "title" {
                        program.title.push_str(&value);
                    } else {
                        program.description.push_str(&value);
                    }
                }
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"programme" => {
                if let Some(mut program) = current.take() {
                    program.title = program.title.trim().to_owned();
                    program.description = program.description.trim().to_owned();
                    if !program.title.is_empty() {
                        programs.push(program);
                    }
                }
                field = None;
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"title" || e.name().as_ref() == b"desc" => {
                field = None
            }
            Ok(Event::Eof) => break,
            Err(err) => return Err(format!("XMLTV invalide : {err}")),
            _ => {}
        }
    }
    programs.sort_by_key(|program| program.start);
    Ok(programs)
}

pub fn parse_xmltv_time(value: &str) -> Option<i64> {
    let compact = value.trim();
    let raw = compact.get(..14)?;
    let naive = NaiveDateTime::parse_from_str(raw, "%Y%m%d%H%M%S").ok()?;
    let suffix = compact.get(14..).unwrap_or("").trim();
    let offset = if suffix.len() == 5 && (suffix.starts_with('+') || suffix.starts_with('-')) {
        let hours: i32 = suffix[1..3].parse().ok()?;
        let minutes: i32 = suffix[3..5].parse().ok()?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        let seconds = (hours * 3600 + minutes * 60) * if suffix.starts_with('-') { -1 } else { 1 };
        FixedOffset::east_opt(seconds)?
    } else {
        FixedOffset::east_opt(0)?
    };
    offset
        .from_local_datetime(&naive)
        .single()
        .map(|date: DateTime<FixedOffset>| date.timestamp())
}

pub fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn m3u_groups_quotes_relative_and_dedup() {
        let input = "\u{feff}#EXTM3U\n#EXTINF:-1 tvg-id=\"abc\" group-title=\"News, World\",News One\n../live/one.m3u8\n#EXTINF:-1,Duplicate\n../live/one.m3u8\n#EXTINF:-1,Radio\nhttps://example.org/radio.mp3";
        let channels = parse_m3u(input, Some("https://example.org/lists/main.m3u")).unwrap();
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].group, "News, World");
        assert_eq!(channels[0].stream_url, "https://example.org/live/one.m3u8");
        assert_eq!(channels[0].tvg_id.as_deref(), Some("abc"));
    }

    #[test]
    fn hls_detection() {
        assert!(is_hls_manifest("#EXTM3U\n#EXT-X-TARGETDURATION:6"));
        assert!(!is_hls_manifest("#EXTM3U\n#EXTINF:-1,TV"));
    }

    #[test]
    fn extgrp_and_local_media() {
        let input = "#EXTM3U\n#EXTINF:-1,Local movie\n#EXTGRP:Films\nmovie.mp4";
        let channels = parse_m3u(input, Some("file:///tmp/list.m3u")).unwrap();
        assert_eq!(channels[0].group, "Films");
        assert_eq!(channels[0].stream_url, "file:///tmp/movie.mp4");
    }

    #[test]
    fn xmltv_time_and_programs() {
        let xml = r#"<tv><programme channel="abc" start="20260924180000 +0200" stop="20260924190000 +0200"><title>News &amp; more</title><desc>Info</desc></programme></tv>"#;
        let programs = parse_xmltv(xml).unwrap();
        assert_eq!(programs.len(), 1);
        assert_eq!(programs[0].title, "News & more");
        assert_eq!(programs[0].start, 1790265600);
    }

    #[test]
    fn xmltv_cdata_and_escaped_channel() {
        let xml = r#"<tv><programme channel="news&amp;more" start="20260924180000 +0000" stop="20260924190000 +0000"><title><![CDATA[News & more]]></title></programme></tv>"#;
        let programs = parse_xmltv(xml).unwrap();
        assert_eq!(programs[0].channel_id, "news&more");
        assert_eq!(programs[0].title, "News & more");
    }
}
