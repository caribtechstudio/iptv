//! HLS playlist helpers: variants, segments and URI rewriting.

use url::Url;

#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub uri: String,
    pub bandwidth: u64,
    pub height: u32,
    pub codecs: Option<String>,
    pub audio_group: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub uri: String,
    pub duration: f64,
    pub sequence: u64,
}

pub fn is_playlist(text: &str) -> bool {
    text.trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with("#EXTM3U")
}

pub fn is_master(text: &str) -> bool {
    text.contains("#EXT-X-STREAM-INF")
}

/// Attributes of a tag such as `#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS="a,b"`.
pub fn attributes(line: &str) -> Vec<(String, String)> {
    let body = line.split_once(':').map_or("", |(_, body)| body);
    let mut result = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let Some((key, after)) = rest.split_once('=') else {
            break;
        };
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            match quoted.split_once('"') {
                Some((value, next)) => (value, next.trim_start_matches(',')),
                None => (quoted, ""),
            }
        } else {
            match after.split_once(',') {
                Some((value, next)) => (value, next),
                None => (after, ""),
            }
        };
        result.push((key.trim().to_ascii_uppercase(), value.to_owned()));
        rest = next;
    }
    result
}

fn attribute(line: &str, name: &str) -> Option<String> {
    attributes(line)
        .into_iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

fn join(base: &Url, uri: &str) -> Option<String> {
    base.join(uri.trim()).ok().map(|url| url.to_string())
}

pub fn variants(text: &str, base: &Url) -> Vec<Variant> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut result = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if !line.starts_with("#EXT-X-STREAM-INF") {
            continue;
        }
        let Some(uri) = lines[index + 1..]
            .iter()
            .find(|next| !next.is_empty())
            .filter(|next| !next.starts_with('#'))
            .and_then(|next| join(base, next))
        else {
            continue;
        };
        let height = attribute(line, "RESOLUTION")
            .and_then(|value| value.split_once('x').and_then(|(_, h)| h.parse().ok()))
            .unwrap_or(0);
        result.push(Variant {
            uri,
            bandwidth: attribute(line, "BANDWIDTH")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
            height,
            codecs: attribute(line, "CODECS"),
            audio_group: attribute(line, "AUDIO"),
        });
    }
    result
}

/// Audio renditions carried by a separate playlist.
pub fn audio_renditions(text: &str, base: &Url) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with("#EXT-X-MEDIA") && line.contains("TYPE=AUDIO"))
        .filter_map(|line| attribute(line, "URI"))
        .filter_map(|uri| join(base, &uri))
        .collect()
}

pub fn media_sequence(text: &str) -> u64 {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

pub fn target_duration(text: &str) -> Option<f64> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("#EXT-X-TARGETDURATION:"))
        .and_then(|value| value.trim().parse().ok())
}

pub fn is_endlist(text: &str) -> bool {
    text.contains("#EXT-X-ENDLIST")
}

pub fn segments(text: &str, base: &Url) -> Vec<Segment> {
    let mut sequence = media_sequence(text);
    let mut duration = 0.0;
    let mut result = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(info) = line.strip_prefix("#EXTINF:") {
            duration = info
                .split(',')
                .next()
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(0.0);
        } else if !line.is_empty() && !line.starts_with('#') {
            if let Some(uri) = join(base, line) {
                result.push(Segment {
                    uri,
                    duration,
                    sequence,
                });
            }
            sequence += 1;
            duration = 0.0;
        }
    }
    result
}

/// Encryption method of the first key tag that is not `NONE`.
pub fn key_method(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with("#EXT-X-KEY") || line.starts_with("#EXT-X-SESSION-KEY"))
        .filter_map(|line| attribute(line, "METHOD"))
        .find(|method| method != "NONE")
}

pub fn map_uri(text: &str, base: &Url) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| line.starts_with("#EXT-X-MAP"))
        .and_then(|line| attribute(line, "URI"))
        .and_then(|uri| join(base, &uri))
}

/// Rewrites every URI of a playlist (media lines and `URI="…"` attributes).
/// Key servers of DRM systems (`skd:`) and inline `data:` URIs are left untouched.
pub fn rewrite(text: &str, base: &Url, mut map: impl FnMut(&Url) -> String) -> String {
    let mut output = String::with_capacity(text.len() + 256);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            output.push('\n');
            continue;
        }
        if !trimmed.starts_with('#') {
            match base.join(trimmed) {
                Ok(url) => output.push_str(&map(&url)),
                Err(_) => output.push_str(trimmed),
            }
            output.push('\n');
            continue;
        }
        let mut rest = trimmed;
        while let Some(index) = rest.find("URI=\"") {
            let (before, after) = rest.split_at(index + 5);
            output.push_str(before);
            let Some(end) = after.find('"') else {
                rest = after;
                break;
            };
            let uri = &after[..end];
            match base.join(uri) {
                Ok(url) if matches!(url.scheme(), "http" | "https") => output.push_str(&map(&url)),
                _ => output.push_str(uri),
            }
            rest = &after[end..];
        }
        output.push_str(rest);
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_variants_renditions_and_rewrite() {
        let base = Url::parse("https://cdn.example/live/master.m3u8?t=1").unwrap();
        let master = "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"fr\",URI=\"audio/fr.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=5000,RESOLUTION=1920x1080,CODECS=\"avc1.64,mp4a.40.2\",AUDIO=\"a\"\nhd/index.m3u8\n";
        let variants = variants(master, &base);
        assert_eq!(variants[0].uri, "https://cdn.example/live/hd/index.m3u8");
        assert_eq!(variants[0].height, 1080);
        assert_eq!(variants[0].codecs.as_deref(), Some("avc1.64,mp4a.40.2"));
        assert_eq!(
            audio_renditions(master, &base),
            vec!["https://cdn.example/live/audio/fr.m3u8"]
        );
        let rewritten = rewrite(master, &base, |url| format!("P[{url}]"));
        assert!(rewritten.contains("URI=\"P[https://cdn.example/live/audio/fr.m3u8]\""));
        assert!(rewritten.contains("\nP[https://cdn.example/live/hd/index.m3u8]\n"));
    }

    #[test]
    fn media_segments_keys_and_maps() {
        let base = Url::parse("http://h/p/index.m3u8").unwrap();
        let media = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:40\n#EXT-X-KEY:METHOD=AES-128,URI=\"k.key\"\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:5.5,\na.ts\n#EXTINF:6,\nb.ts\n#EXT-X-ENDLIST";
        let segments = segments(media, &base);
        assert_eq!(segments.len(), 2);
        assert_eq!(segments[1].sequence, 41);
        assert_eq!(segments[0].duration, 5.5);
        assert_eq!(key_method(media).as_deref(), Some("AES-128"));
        assert_eq!(
            map_uri(media, &base).as_deref(),
            Some("http://h/p/init.mp4")
        );
        assert_eq!(target_duration(media), Some(6.0));
        assert!(is_endlist(media));
        let rewritten = rewrite(
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\"",
            &base,
            |_| "X".into(),
        );
        assert!(rewritten.contains("skd://x"));
    }
}
