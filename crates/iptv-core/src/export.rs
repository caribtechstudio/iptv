//! M3U export: the attributes `parse_m3u` reads are written back, so an exported list
//! imports into Fluxo (and VLC, Kodi, TiviMate…) with its groups, logos, guide ids and the
//! HTTP headers its channels require.

use crate::Channel;

/// One channel ready to be written: `url` replaces `channel.stream_url` when the stored
/// address is not playable elsewhere (Xtream references are resolved by the caller).
pub struct ExportEntry<'a> {
    pub channel: &'a Channel,
    pub url: &'a str,
}

/// Attribute values cannot contain quotes or line breaks in M3U.
fn attribute(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            '"' => '\'',
            '\r' | '\n' | '\t' => ' ',
            other => other,
        })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The display name ends the `#EXTINF` line: line breaks would split the entry.
fn display_name(value: &str) -> String {
    let name = value.replace(['\r', '\n'], " ");
    let name = name.trim();
    if name.is_empty() {
        "Chaîne sans nom".into()
    } else {
        name.to_owned()
    }
}

/// Writes an extended M3U playlist. `epg_urls` go in the `x-tvg-url` header.
pub fn write_m3u(entries: &[ExportEntry], epg_urls: &[String]) -> String {
    let mut out = String::with_capacity(entries.len() * 180 + 64);
    out.push_str("#EXTM3U");
    let guides: Vec<String> = epg_urls
        .iter()
        .map(|url| attribute(url))
        .filter(|url| !url.is_empty())
        .collect();
    if !guides.is_empty() {
        out.push_str(&format!(" x-tvg-url=\"{}\"", guides.join(",")));
    }
    out.push('\n');
    for ExportEntry { channel, url } in entries {
        let url = url.trim();
        if url.is_empty() || url.contains(['\r', '\n']) {
            continue;
        }
        out.push_str("#EXTINF:-1");
        let mut push = |key: &str, value: Option<&str>| {
            if let Some(value) = value.map(attribute).filter(|value| !value.is_empty()) {
                out.push_str(&format!(" {key}=\"{value}\""));
            }
        };
        push("tvg-id", channel.tvg_id.as_deref());
        push("tvg-name", Some(&channel.name));
        push("tvg-logo", channel.logo.as_deref());
        push("tvg-country", channel.country.as_deref());
        push("tvg-language", channel.language.as_deref());
        push("group-title", Some(&channel.group));
        push("http-user-agent", channel.user_agent.as_deref());
        push("http-referrer", channel.referrer.as_deref());
        out.push(',');
        out.push_str(&display_name(&channel.name));
        out.push('\n');
        // VLC and most players read the headers from `#EXTVLCOPT` rather than attributes.
        if let Some(agent) = channel.user_agent.as_deref().map(attribute) {
            out.push_str(&format!("#EXTVLCOPT:http-user-agent={agent}\n"));
        }
        if let Some(referrer) = channel.referrer.as_deref().map(attribute) {
            out.push_str(&format!("#EXTVLCOPT:http-referrer={referrer}\n"));
        }
        out.push_str(url);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{m3u_epg_urls, parse_m3u};

    #[test]
    fn exported_list_imports_back_with_its_attributes() {
        let channels = [
            Channel {
                id: "x".into(),
                name: "M6 \"HD\", la suite".into(),
                group: "France | Généralistes".into(),
                stream_url: "xtream://acc/live/12".into(),
                logo: Some("https://logo.example/m6.png".into()),
                tvg_id: Some("M6.fr@HD".into()),
                user_agent: Some("Agent/1".into()),
                referrer: Some("https://site.example/".into()),
                country: Some("fr".into()),
                language: Some("French".into()),
                ..Channel::default()
            },
            Channel {
                id: "y".into(),
                name: "Radio\nlocale".into(),
                group: "Radios".into(),
                stream_url: "https://example.org/radio.mp3".into(),
                ..Channel::default()
            },
        ];
        let resolved = "http://server.example/live/user/pass/12.m3u8";
        let entries = [
            ExportEntry {
                channel: &channels[0],
                url: resolved,
            },
            ExportEntry {
                channel: &channels[1],
                url: &channels[1].stream_url,
            },
        ];
        let text = write_m3u(&entries, &["https://epg.example/a.xml".into()]);
        assert!(text.starts_with("#EXTM3U x-tvg-url=\"https://epg.example/a.xml\"\n"));
        assert_eq!(m3u_epg_urls(&text), vec!["https://epg.example/a.xml"]);

        let parsed = parse_m3u(&text, None).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "M6 \"HD\", la suite");
        assert_eq!(parsed[0].group, "France | Généralistes");
        assert_eq!(parsed[0].stream_url, resolved);
        assert_eq!(parsed[0].tvg_id.as_deref(), Some("M6.fr@HD"));
        assert_eq!(
            parsed[0].logo.as_deref(),
            Some("https://logo.example/m6.png")
        );
        assert_eq!(parsed[0].user_agent.as_deref(), Some("Agent/1"));
        assert_eq!(parsed[0].referrer.as_deref(), Some("https://site.example/"));
        assert_eq!(parsed[0].country.as_deref(), Some("fr"));
        assert_eq!(parsed[0].language.as_deref(), Some("French"));
        assert_eq!(parsed[1].name, "Radio locale");
        assert_eq!(parsed[1].group, "Radios");
    }

    #[test]
    fn empty_list_is_a_bare_header() {
        assert_eq!(write_m3u(&[], &[]), "#EXTM3U\n");
    }
}
