//! Choosing a local file name from what the browser, URL, and server say.

use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use serde::{Deserialize, Serialize};
use url::Url;

const FALLBACK_NAME: &str = "download";
const MAX_NAME_CHARS: usize = 200;

/// Where a file name came from, strongest first. A server-provided name only
/// replaces a guess derived from the URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileNameSource {
    User,
    Browser,
    Server,
    Url,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileName {
    pub value: String,
    pub source: FileNameSource,
}

impl FileName {
    pub fn new(raw: &str, source: FileNameSource) -> Self {
        Self {
            value: sanitize_file_name(raw),
            source,
        }
    }

    pub fn guess_from_url(url: &Url) -> Self {
        let last_segment = url
            .path_segments()
            .and_then(|mut segments| segments.next_back())
            .map(|segment| percent_decode_str(segment).decode_utf8_lossy().into_owned())
            .unwrap_or_default();
        Self::new(&last_segment, FileNameSource::Url)
    }

    /// Manifests are usually named `master.m3u8` or `manifest.mpd`; the
    /// video's name, when the URL has one, is a folder further up the path.
    pub fn guess_for_stream(url: &Url) -> Self {
        let meaningful = url
            .path_segments()
            .into_iter()
            .flatten()
            .rev()
            .map(|segment| percent_decode_str(segment).decode_utf8_lossy().into_owned())
            .map(|segment| match segment.rsplit_once('.') {
                Some((stem, _)) if !stem.is_empty() => stem.to_owned(),
                _ => segment,
            })
            .find(|stem| !is_generic_stream_name(stem));
        Self::new(
            meaningful.as_deref().unwrap_or(STREAM_FALLBACK_NAME),
            FileNameSource::Url,
        )
    }

    pub fn is_replaceable_by_server(&self) -> bool {
        self.source == FileNameSource::Url
    }
}

const STREAM_FALLBACK_NAME: &str = "video";
/// Path segments that name a playlist's role rather than the video.
const GENERIC_STREAM_NAMES: [&str; 12] = [
    "master",
    "index",
    "playlist",
    "manifest",
    "chunklist",
    "main",
    "stream",
    "video",
    "prog_index",
    "hls",
    "dash",
    "media",
];

fn is_generic_stream_name(stem: &str) -> bool {
    let lower = stem.to_ascii_lowercase();
    let is_quality = lower
        .strip_suffix('p')
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()));
    lower.is_empty()
        || GENERIC_STREAM_NAMES.contains(&lower.as_str())
        || lower.chars().all(|c| c.is_ascii_digit())
        || is_quality
}

/// Makes a name safe on Windows, macOS, and Linux file systems.
pub fn sanitize_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(MAX_NAME_CHARS)
        .collect();
    let trimmed = cleaned.trim().trim_end_matches(['.', ' ']);
    if trimmed.is_empty() || trimmed == "." || trimmed == ".." {
        return FALLBACK_NAME.to_owned();
    }
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    if reserved {
        format!("_{trimmed}")
    } else {
        trimmed.to_owned()
    }
}

/// Extracts the file name from a `Content-Disposition` header, preferring the
/// RFC 6266 `filename*` form over the legacy `filename` parameter.
pub fn file_name_from_content_disposition(header: &str) -> Option<String> {
    let mut legacy = None;
    for parameter in split_parameters(header) {
        let Some((key, value)) = parameter.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if key == "filename*" {
            if let Some(decoded) = decode_extended_value(value) {
                return Some(decoded);
            }
        } else if key == "filename" {
            legacy = Some(unquote(value));
        }
    }
    legacy.filter(|name| !name.is_empty())
}

fn split_parameters(header: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut in_quotes = false;
    let mut start = 0;
    for (index, c) in header.char_indices() {
        match c {
            '"' => in_quotes = !in_quotes,
            ';' if !in_quotes => {
                parts.push(&header[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&header[start..]);
    parts
}

fn unquote(value: &str) -> String {
    let inner = value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value);
    inner.replace("\\\"", "\"")
}

/// Decodes `charset'language'percent-encoded` (RFC 8187). Only UTF-8 and
/// ISO-8859-1 are defined by the RFC; other charsets are rejected.
fn decode_extended_value(value: &str) -> Option<String> {
    let mut parts = value.splitn(3, '\'');
    let charset = parts.next()?.to_ascii_lowercase();
    let _language = parts.next()?;
    let encoded = parts.next()?;
    let bytes: Vec<u8> = percent_decode_str(encoded).collect();
    let decoded = match charset.as_str() {
        "utf-8" => String::from_utf8(bytes).ok()?,
        "iso-8859-1" => bytes.into_iter().map(char::from).collect(),
        _ => return None,
    };
    (!decoded.is_empty()).then_some(decoded)
}

/// Returns `dir/name`, or `dir/name (n).ext` for the first `n` for which
/// `is_taken` is false.
pub fn first_free_path(dir: &Path, name: &str, is_taken: impl Fn(&Path) -> bool) -> PathBuf {
    let candidate = dir.join(name);
    if !is_taken(&candidate) {
        return candidate;
    }
    let (stem, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, Some(extension)),
        _ => (name, None),
    };
    (1u32..)
        .map(|n| match extension {
            Some(extension) => dir.join(format!("{stem} ({n}).{extension}")),
            None => dir.join(format!("{stem} ({n})")),
        })
        .find(|path| !is_taken(path))
        .expect("an unbounded counter always finds a free name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_guess_decodes_last_segment() {
        let url = Url::parse("https://example.com/files/My%20Report.pdf?x=1").unwrap();
        assert_eq!(FileName::guess_from_url(&url).value, "My Report.pdf");

        let root = Url::parse("https://example.com/").unwrap();
        assert_eq!(FileName::guess_from_url(&root).value, FALLBACK_NAME);
    }

    #[test]
    fn stream_names_skip_generic_segments() {
        let named =
            Url::parse("https://cdn.example/videos/my-scene/1080p/master.m3u8?t=1").unwrap();
        assert_eq!(FileName::guess_for_stream(&named).value, "my-scene");
        let bare = Url::parse("https://cdn.example/123/index.m3u8").unwrap();
        assert_eq!(FileName::guess_for_stream(&bare).value, "video");
    }

    #[test]
    fn sanitizes_hostile_names() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("a<b>c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitize_file_name("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_file_name("COM1"), "_COM1");
        assert_eq!(sanitize_file_name("trailing. "), "trailing");
        assert_eq!(sanitize_file_name(""), FALLBACK_NAME);
    }

    #[test]
    fn content_disposition_prefers_extended_name() {
        let header =
            r#"attachment; filename="fallback.txt"; filename*=UTF-8''na%C3%AFve%20file.txt"#;
        assert_eq!(
            file_name_from_content_disposition(header).as_deref(),
            Some("naïve file.txt")
        );
    }

    #[test]
    fn content_disposition_legacy_name_with_semicolon() {
        let header = r#"attachment; filename="a;b.zip""#;
        assert_eq!(
            file_name_from_content_disposition(header).as_deref(),
            Some("a;b.zip")
        );
        assert_eq!(file_name_from_content_disposition("inline"), None);
    }

    #[test]
    fn finds_numbered_free_path() {
        let dir = Path::new("downloads");
        let taken = [dir.join("a.zip"), dir.join("a (1).zip")];
        let path = first_free_path(dir, "a.zip", |p| taken.iter().any(|t| t == p));
        assert_eq!(path, dir.join("a (2).zip"));
        assert_eq!(first_free_path(dir, "b", |_| false), dir.join("b"));
    }
}
