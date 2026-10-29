use std::num::NonZeroU32;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::category::Categories;
use crate::limiter::SpeedLimit;
use crate::stream::VideoQuality;

pub const CONNECTION_RANGE: std::ops::RangeInclusive<u8> = 1..=32;
pub const ACTIVE_DOWNLOAD_RANGE: std::ops::RangeInclusive<u8> = 1..=16;
const DEFAULT_USER_AGENT: &str = concat!("Downpour/", env!("CARGO_PKG_VERSION"));

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineSettings {
    pub download_dir: PathBuf,
    /// Save into each category's folder instead of the folder itself.
    pub organize_by_category: bool,
    pub categories: Categories,
    pub connections_per_download: u8,
    pub max_active_downloads: u8,
    /// `None` means unlimited.
    pub speed_limit_kib_per_second: Option<NonZeroU32>,
    /// Sent when the browser did not provide its own user agent.
    pub user_agent: String,
    /// Quality for video streams unless chosen per download.
    pub video_quality: VideoQuality,
    /// ffmpeg used to merge stream tracks; `None` searches next to the app
    /// and on `PATH`.
    pub ffmpeg_path: Option<PathBuf>,
}

impl Default for EngineSettings {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
            organize_by_category: true,
            categories: Categories::default(),
            connections_per_download: 8,
            max_active_downloads: 3,
            speed_limit_kib_per_second: None,
            user_agent: DEFAULT_USER_AGENT.to_owned(),
            video_quality: VideoQuality::Best,
            ffmpeg_path: None,
        }
    }
}

impl EngineSettings {
    /// Clamps values from an edited or older settings file into valid ranges.
    pub fn normalized(mut self) -> Self {
        self.connections_per_download = self
            .connections_per_download
            .clamp(*CONNECTION_RANGE.start(), *CONNECTION_RANGE.end());
        self.max_active_downloads = self
            .max_active_downloads
            .clamp(*ACTIVE_DOWNLOAD_RANGE.start(), *ACTIVE_DOWNLOAD_RANGE.end());
        self.download_dir = crate::paths::native(&self.download_dir);
        self.ffmpeg_path = self.ffmpeg_path.as_deref().map(crate::paths::native);
        if self.user_agent.trim().is_empty() {
            self.user_agent = DEFAULT_USER_AGENT.to_owned();
        }
        self
    }

    pub fn speed_limit(&self) -> SpeedLimit {
        match self.speed_limit_kib_per_second {
            None => SpeedLimit::Unlimited,
            Some(kib) => SpeedLimit::kib_per_second(kib),
        }
    }
}

fn default_download_dir() -> PathBuf {
    directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(|dir| dir.join("Downpour")))
        .unwrap_or_else(|| PathBuf::from("Downloads"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_out_of_range_values() {
        let settings = EngineSettings {
            connections_per_download: 0,
            max_active_downloads: 99,
            ..Default::default()
        };
        let normalized = settings.normalized();
        assert_eq!(normalized.connections_per_download, 1);
        assert_eq!(normalized.max_active_downloads, 16);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let settings: EngineSettings =
            serde_json::from_str(r#"{"max_active_downloads": 5}"#).unwrap();
        assert_eq!(settings.max_active_downloads, 5);
        assert_eq!(settings.connections_per_download, 8);
    }
}
