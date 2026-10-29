//! A record of every finished download, kept after the download leaves the
//! list, so statistics stay complete.

use serde::{Deserialize, Serialize};

use crate::record::DownloadRecord;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedDownload {
    pub finished_at_unix: u64,
    pub bytes: u64,
    pub file_name: String,
    /// Lowercase extension without the dot; empty when the name has none.
    pub extension: String,
    pub category: String,
    /// Host the file came from, e.g. `cdn.example.com`.
    pub host: String,
    pub is_stream: bool,
}

impl CompletedDownload {
    pub fn from_record(record: &DownloadRecord, bytes: u64, category: &str) -> Option<Self> {
        let finished_at_unix = record.finished_at_unix?;
        let file_name = record
            .target
            .as_ref()
            .and_then(|target| target.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| record.file_name.value.clone());
        Some(Self {
            finished_at_unix,
            bytes,
            extension: extension_of(&file_name),
            file_name,
            category: category.to_owned(),
            host: record.url.host_str().unwrap_or_default().to_owned(),
            is_stream: !matches!(record.source, crate::record::DownloadSource::File),
        })
    }
}

pub fn extension_of(file_name: &str) -> String {
    match file_name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
            extension.to_ascii_lowercase()
        }
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_is_lowercase_and_optional() {
        assert_eq!(extension_of("Movie.MP4"), "mp4");
        assert_eq!(extension_of("archive.tar.gz"), "gz");
        assert_eq!(extension_of("README"), "");
        assert_eq!(extension_of(".hidden"), "");
    }
}
