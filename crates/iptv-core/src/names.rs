//! Channel name normalisation shared by EPG matching and source failover.
//! `src/catalog.js` implements the same rules for the interface.

const QUALITY_TOKENS: &[&str] = &[
    "hd", "fhd", "uhd", "sd", "4k", "8k", "hevc", "h264", "h265", "backup", "raw", "hq", "lq",
];

fn fold(ch: char) -> char {
    match ch {
        'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'ö' | 'õ' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        other => other,
    }
}

/// `"TF1 HD (1080p) [Geo-blocked]"` → `"tf1"`; `"France 2"` → `"france2"`.
pub fn normalize_channel_name(name: &str) -> String {
    let mut cleaned = String::with_capacity(name.len());
    let mut depth = 0usize;
    for ch in name.chars() {
        match ch {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            _ if depth == 0 => cleaned.extend(ch.to_lowercase().map(fold)),
            _ => {}
        }
    }
    cleaned
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| {
            !token.is_empty()
                && !QUALITY_TOKENS.contains(token)
                && !(token.ends_with('p')
                    && token.len() > 3
                    && token[..token.len() - 1].bytes().all(|b| b.is_ascii_digit()))
        })
        .collect()
}

/// `"M6.fr@HD"` → `"m6.fr"`.
pub fn base_tvg_id(id: &str) -> String {
    id.split('@').next().unwrap_or(id).trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_ignore_quality_and_annotations() {
        assert_eq!(
            normalize_channel_name("TF1 HD (1080p) [Geo-blocked]"),
            "tf1"
        );
        assert_eq!(normalize_channel_name("TF1"), "tf1");
        assert_eq!(normalize_channel_name("France 2 FHD"), "france2");
        assert_eq!(normalize_channel_name("Télé Matin 720p"), "telematin");
        assert_eq!(base_tvg_id("M6.fr@HD"), "m6.fr");
    }
}
