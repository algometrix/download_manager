//! Human-readable sizes, speeds, and times.

use std::num::NonZeroU32;

use chrono::{Local, TimeZone};
use downpour_core::limiter::{kib_to_bytes_per_second, DownloadLimit};

pub use downpour_core::units::{bytes, duration, speed};

/// "1.0 MB/s" for a limit given in KiB per second.
pub fn kib_rate(kib_per_second: NonZeroU32) -> String {
    speed(kib_to_bytes_per_second(kib_per_second))
}

/// How fast a download may go, given the limit from Settings (`shared`).
pub fn download_limit(limit: DownloadLimit, shared: Option<NonZeroU32>) -> String {
    match (limit, shared) {
        (DownloadLimit::FollowSettings, Some(shared)) => {
            format!("Global limit ({})", kib_rate(shared))
        }
        (DownloadLimit::FollowSettings, None) => "Global limit (none set)".to_owned(),
        (DownloadLimit::Unlimited, _) => "No limit".to_owned(),
        (DownloadLimit::Limited { kib_per_second }, _) => kib_rate(kib_per_second),
    }
}

/// "Just now", "5 min ago", "3 h ago", or a calendar date for older times.
pub fn added_at(unix: u64, now_unix: u64) -> String {
    let age = now_unix.saturating_sub(unix);
    match age {
        0..60 => "Just now".to_owned(),
        60..3600 => format!("{} min ago", age / 60),
        3600..86_400 => format!("{} h ago", age / 3600),
        _ => date(unix),
    }
}

/// A calendar date such as "Mar 03, 2027", in local time.
pub fn date(unix: u64) -> String {
    Local
        .timestamp_opt(unix as i64, 0)
        .single()
        .map(|time| time.format("%b %d, %Y").to_string())
        .unwrap_or_else(|| "Unknown date".to_owned())
}

/// Explains why a download waits for disk space and what happens next.
pub fn space_shortage(file_name: &str, directory: &std::path::Path, needed: Option<u64>) -> String {
    let free = downpour_core::disk::free_space(directory)
        .map(bytes)
        .unwrap_or_else(|_| "unknown".to_owned());
    let needed = match needed {
        Some(needed) => format!("it needs {}", bytes(needed)),
        None => "the disk is full".to_owned(),
    };
    format!(
        "{file_name}: {needed} in {} ({free} free). Free up space; the download continues by itself.",
        directory.display()
    )
}

/// Extension shown on the file tile, at most four characters.
pub fn extension_badge(file_name: &str) -> String {
    file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .filter(|extension| !extension.is_empty() && extension.len() <= 4)
        .map(str::to_ascii_uppercase)
        .unwrap_or_else(|| "FILE".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_a_downloads_speed_limit() {
        let mib = NonZeroU32::new(1024).unwrap();
        assert_eq!(kib_rate(NonZeroU32::new(256).unwrap()), "256 KB/s");
        assert_eq!(
            download_limit(DownloadLimit::FollowSettings, None),
            "Global limit (none set)"
        );
        assert_eq!(
            download_limit(DownloadLimit::FollowSettings, Some(mib)),
            "Global limit (1.0 MB/s)"
        );
        assert_eq!(
            download_limit(DownloadLimit::Unlimited, Some(mib)),
            "No limit"
        );
        let own = DownloadLimit::Limited {
            kib_per_second: mib,
        };
        assert_eq!(download_limit(own, None), "1.0 MB/s");
    }

    #[test]
    fn formats_relative_times() {
        assert_eq!(added_at(1000, 1030), "Just now");
        assert_eq!(added_at(1000, 1000 + 5 * 60), "5 min ago");
        assert_eq!(added_at(1000, 1000 + 2 * 3600), "2 h ago");
    }

    #[test]
    fn extension_badges() {
        assert_eq!(extension_badge("a.pdf"), "PDF");
        assert_eq!(extension_badge("archive.tar.gz"), "GZ");
        assert_eq!(extension_badge("README"), "FILE");
        assert_eq!(extension_badge("x.verylong"), "FILE");
    }
}
