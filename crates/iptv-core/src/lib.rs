use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use url::Url;

pub mod export;
pub mod hls;
pub mod names;
pub mod ts;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub group: String,
    pub stream_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tvg_id: Option<String>,
    #[serde(default)]
    pub kind: ChannelKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_extension: Option<String>,
    /// HTTP headers some providers require (from `http-user-agent`, `#EXTVLCOPT`, `#EXTHTTP`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referrer: Option<String>,
    /// Lowercase ISO 3166 code, from `tvg-country` or the `tvg-id` suffix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Days of catch-up archive exposed by the provider (Xtream `tv_archive_duration`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catchup_days: Option<u32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    #[default]
    Live,
    Movie,
    Series,
    Episode,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XtreamAccount {
    pub id: String,
    pub server: String,
    pub username: String,
    /// `allowed_output_formats` announced by the provider (`m3u8`, `ts`…).
    #[serde(default)]
    pub output_formats: Vec<String>,
    /// Offset in seconds between the provider clock and UTC, used for catch-up URLs.
    #[serde(default)]
    pub utc_offset: Option<i32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    pub name: String,
    pub source: String,
    #[serde(default)]
    pub channels: Vec<Channel>,
    pub updated_at: String,
    /// Guide announced by the playlist header (`x-tvg-url`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epg_url: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentItem {
    pub name: String,
    pub url: String,
    pub played_at: String,
    #[serde(default)]
    pub container_extension: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub url: String,
    pub name: String,
    pub position: f64,
    pub duration: f64,
    pub updated_at: String,
    #[serde(default)]
    pub kind: ChannelKind,
    #[serde(default)]
    pub container_extension: Option<String>,
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
    #[serde(default)]
    pub xtream_accounts: Vec<XtreamAccount>,
    #[serde(default)]
    pub hidden_groups: Vec<String>,
    #[serde(default)]
    pub progress: Vec<Progress>,
    /// When true, guides announced by playlists and Xtream accounts are not loaded.
    #[serde(default)]
    pub ignore_playlist_epg: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Program {
    pub channel_id: String,
    pub title: String,
    pub description: String,
    pub start: i64,
    pub stop: i64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct XmltvChannel {
    pub id: String,
    pub names: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct XmltvGuide {
    pub channels: Vec<XmltvChannel>,
    pub programs: Vec<Program>,
}

#[derive(Default)]
struct PendingChannel {
    name: String,
    group: String,
    logo: Option<String>,
    tvg_id: Option<String>,
    user_agent: Option<String>,
    referrer: Option<String>,
    country: Option<String>,
    language: Option<String>,
}

fn non_empty(value: Option<&String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Country code from `tvg-country` (`FR`, `FR;BE`) or an iptv-org style id (`M6.fr@HD`).
pub fn channel_country(tvg_country: Option<&str>, tvg_id: Option<&str>) -> Option<String> {
    let valid = |code: &str| code.len() == 2 && code.bytes().all(|b| b.is_ascii_alphabetic());
    let normalize = |code: &str| {
        let code = code.to_ascii_lowercase();
        if code == "uk" { "gb".to_owned() } else { code }
    };
    if let Some(first) = tvg_country
        .and_then(|value| {
            value
                .split([';', ',', ' ', '|'])
                .find(|part| !part.is_empty())
        })
        .filter(|code| valid(code))
    {
        return Some(normalize(first));
    }
    let id = tvg_id?.split('@').next()?;
    let (_, suffix) = id.rsplit_once('.')?;
    valid(suffix).then(|| normalize(suffix))
}

/// Guide URLs announced in the `#EXTM3U` header (`x-tvg-url`, `url-tvg`).
pub fn m3u_epg_urls(input: &str) -> Vec<String> {
    let Some(header) = input
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .and_then(|line| line.strip_prefix("#EXTM3U"))
    else {
        return Vec::new();
    };
    let attrs = parse_attributes(header);
    let mut urls = Vec::new();
    for key in ["x-tvg-url", "url-tvg", "tvg-url"] {
        for url in attrs.get(key).map(String::as_str).unwrap_or("").split(',') {
            let url = url.trim();
            if (url.starts_with("https://") || url.starts_with("http://"))
                && !urls.iter().any(|known| known == url)
            {
                urls.push(url.to_owned());
            }
        }
    }
    urls
}

fn apply_http_option(pending: &mut PendingChannel, key: &str, value: &str) {
    let value = value.trim().trim_matches('"').trim();
    if value.is_empty() {
        return;
    }
    match key.trim().to_ascii_lowercase().as_str() {
        "http-user-agent" | "user-agent" => pending.user_agent = Some(value.to_owned()),
        "http-referrer" | "http-referer" | "referrer" | "referer" => {
            pending.referrer = Some(value.to_owned())
        }
        _ => {}
    }
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
    let mut pending: Option<PendingChannel> = None;
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
            let tvg_id = non_empty(attrs.get("tvg-id"));
            let mut next = PendingChannel {
                name,
                group,
                logo: non_empty(attrs.get("tvg-logo")),
                country: channel_country(
                    attrs.get("tvg-country").map(String::as_str),
                    tvg_id.as_deref(),
                ),
                language: non_empty(attrs.get("tvg-language"))
                    .and_then(|value| value.split([';', ',']).next().map(|v| v.trim().to_owned())),
                tvg_id,
                ..PendingChannel::default()
            };
            for key in ["http-user-agent", "http-referrer", "http-referer"] {
                if let Some(value) = attrs.get(key) {
                    apply_http_option(&mut next, key, value);
                }
            }
            pending = Some(next);
        } else if let Some(group) = line.strip_prefix("#EXTGRP:") {
            if let Some(pending) = &mut pending {
                pending.group = group.trim().to_owned();
            } else {
                group_override = Some(group.trim().to_owned());
            }
        } else if let Some(option) = line.strip_prefix("#EXTVLCOPT:") {
            if let (Some(pending), Some((key, value))) = (&mut pending, option.split_once('=')) {
                apply_http_option(pending, key, value);
            }
        } else if let Some(json) = line.strip_prefix("#EXTHTTP:") {
            if let (Some(pending), Ok(serde_json::Value::Object(map))) = (
                &mut pending,
                serde_json::from_str::<serde_json::Value>(json),
            ) {
                for (key, value) in map {
                    if let Some(value) = value.as_str() {
                        apply_http_option(pending, &key, value);
                    }
                }
            }
        } else if !line.starts_with('#')
            && let Some(item) = pending.take()
        {
            let stream_url = resolve_url(line, base).unwrap_or_else(|| line.to_owned());
            if !is_playable_source(&stream_url) || !seen.insert(stream_url.clone()) {
                continue;
            }
            let id = format!("{}|{}", item.tvg_id.as_deref().unwrap_or(""), stream_url);
            channels.push(Channel {
                id,
                name: item.name,
                group: item.group,
                stream_url,
                logo: item.logo,
                tvg_id: item.tvg_id,
                kind: ChannelKind::Live,
                user_agent: item.user_agent,
                referrer: item.referrer,
                country: item.country,
                language: item.language,
                ..Channel::default()
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

fn parse_attributes(info: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();
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
    parse_xmltv_guide(input).map(|guide| guide.programs)
}

#[derive(Clone, Copy, PartialEq)]
enum XmlField {
    Title,
    Desc,
    DisplayName,
}

/// Parses XMLTV channels (id + display names) and programmes.
pub fn parse_xmltv_guide(input: &str) -> Result<XmltvGuide, String> {
    let mut reader = Reader::from_str(input);
    reader.config_mut().trim_text(false);
    let mut guide = XmltvGuide::default();
    let mut current: Option<Program> = None;
    let mut channel: Option<XmltvChannel> = None;
    let mut name = String::new();
    let mut field = None::<XmlField>;
    let push_text =
        |current: &mut Option<Program>, name: &mut String, field, text: &str| match field {
            Some(XmlField::Title) => {
                if let Some(program) = current {
                    program.title.push_str(text)
                }
            }
            Some(XmlField::Desc) => {
                if let Some(program) = current {
                    program.description.push_str(text)
                }
            }
            Some(XmlField::DisplayName) => name.push_str(text),
            None => {}
        };
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref() == b"programme" => {
                let mut channel_id = None;
                let mut start = None;
                let mut stop = None;
                for attr in e.attributes().flatten() {
                    let value = attr
                        .decode_and_unescape_value(reader.decoder())
                        .map_err(|err| err.to_string())?
                        .into_owned();
                    match attr.key.as_ref() {
                        b"channel" => channel_id = Some(value),
                        b"start" => start = parse_xmltv_time(&value),
                        b"stop" => stop = parse_xmltv_time(&value),
                        _ => {}
                    }
                }
                current = match (channel_id, start, stop) {
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
            Ok(Event::Start(e)) if e.name().as_ref() == b"channel" => {
                channel = e
                    .attributes()
                    .flatten()
                    .find(|attr| attr.key.as_ref() == b"id")
                    .and_then(|attr| attr.decode_and_unescape_value(reader.decoder()).ok())
                    .map(|id| XmltvChannel {
                        id: id.into_owned(),
                        names: Vec::new(),
                    });
            }
            Ok(Event::Start(e)) if e.name().as_ref() == b"title" => field = Some(XmlField::Title),
            Ok(Event::Start(e)) if e.name().as_ref() == b"desc" => field = Some(XmlField::Desc),
            Ok(Event::Start(e)) if e.name().as_ref() == b"display-name" && channel.is_some() => {
                name.clear();
                field = Some(XmlField::DisplayName);
            }
            Ok(Event::Text(e)) if field.is_some() => {
                let text = e.xml_content().map_err(|err| err.to_string())?;
                push_text(&mut current, &mut name, field, &text);
            }
            Ok(Event::CData(e)) if field.is_some() => {
                let text = e.decode().map_err(|err| err.to_string())?;
                push_text(&mut current, &mut name, field, &text);
            }
            Ok(Event::GeneralRef(e)) if field.is_some() => {
                let decoded = e.decode().map_err(|err| err.to_string())?;
                let value = if let Some(ch) = e.resolve_char_ref().map_err(|err| err.to_string())? {
                    ch.to_string()
                } else {
                    quick_xml::escape::resolve_predefined_entity(&decoded)
                        .unwrap_or("")
                        .to_owned()
                };
                push_text(&mut current, &mut name, field, &value);
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"programme" => {
                if let Some(mut program) = current.take() {
                    program.title = program.title.trim().to_owned();
                    program.description = program.description.trim().to_owned();
                    if !program.title.is_empty() {
                        guide.programs.push(program);
                    }
                }
                field = None;
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"display-name" => {
                if let Some(channel) = &mut channel {
                    let value = name.trim();
                    if !value.is_empty() {
                        channel.names.push(value.to_owned());
                    }
                }
                field = None;
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"channel" => {
                if let Some(channel) = channel.take() {
                    guide.channels.push(channel);
                }
            }
            Ok(Event::End(e)) if e.name().as_ref() == b"title" || e.name().as_ref() == b"desc" => {
                field = None
            }
            Ok(Event::Eof) => break,
            Err(err) => return Err(format!("XMLTV invalide : {err}")),
            _ => {}
        }
    }
    guide.programs.sort_by_key(|program| program.start);
    Ok(guide)
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
    fn m3u_http_headers_country_and_guide_url() {
        let input = "#EXTM3U x-tvg-url=\"https://epg.example/a.xml.gz,https://epg.example/b.xml\"\n#EXTINF:-1 tvg-id=\"M6.fr@HD\" http-user-agent=\"Agent/1\",M6 HD\n#EXTVLCOPT:http-referrer=https://site.example/\nhttp://example.org/m6.m3u8\n#EXTINF:-1 tvg-country=\"BE;FR\" tvg-language=\"French\",La Une\n#EXTHTTP:{\"User-Agent\":\"Kodi\",\"Referer\":\"https://rtbf.example/\"}\nhttp://example.org/une.m3u8";
        let channels = parse_m3u(input, None).unwrap();
        assert_eq!(channels[0].user_agent.as_deref(), Some("Agent/1"));
        assert_eq!(
            channels[0].referrer.as_deref(),
            Some("https://site.example/")
        );
        assert_eq!(channels[0].country.as_deref(), Some("fr"));
        assert_eq!(channels[1].user_agent.as_deref(), Some("Kodi"));
        assert_eq!(
            channels[1].referrer.as_deref(),
            Some("https://rtbf.example/")
        );
        assert_eq!(channels[1].country.as_deref(), Some("be"));
        assert_eq!(channels[1].language.as_deref(), Some("French"));
        assert_eq!(
            m3u_epg_urls(input),
            vec!["https://epg.example/a.xml.gz", "https://epg.example/b.xml"]
        );
        assert_eq!(
            channel_country(None, Some("BBCOne.uk")).as_deref(),
            Some("gb")
        );
        assert_eq!(channel_country(None, Some("NoCountry")), None);
    }

    #[test]
    fn xmltv_channels_and_display_names() {
        let xml = r#"<tv><channel id="M6.fr"><display-name>M6</display-name><display-name>M6 HD</display-name></channel><programme channel="M6.fr" start="20260924180000 +0000" stop="20260924190000 +0000"><title>Show</title></programme></tv>"#;
        let guide = parse_xmltv_guide(xml).unwrap();
        assert_eq!(guide.channels[0].id, "M6.fr");
        assert_eq!(guide.channels[0].names, vec!["M6", "M6 HD"]);
        assert_eq!(guide.programs.len(), 1);
    }

    #[test]
    fn xmltv_cdata_and_escaped_channel() {
        let xml = r#"<tv><programme channel="news&amp;more" start="20260924180000 +0000" stop="20260924190000 +0000"><title><![CDATA[News & more]]></title></programme></tv>"#;
        let programs = parse_xmltv(xml).unwrap();
        assert_eq!(programs[0].channel_id, "news&more");
        assert_eq!(programs[0].title, "News & more");
    }
}
