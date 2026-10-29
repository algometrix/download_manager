//! What the terminal shows while downloads run, and how each one ended.
//! Pure functions over engine snapshots, so they are tested without a
//! network or a terminal.

use std::path::PathBuf;

use downpour_core::units::{bytes, speed};
use downpour_core::{DownloadStatus, DownloadView};

/// How a download ended, once it has.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Saved { path: PathBuf, size: u64 },
    Failed { reason: String },
}

/// `None` while the download is still queued, running, or waiting for space.
pub fn outcome(download: &DownloadView) -> Option<Outcome> {
    match &download.status {
        DownloadStatus::Completed => Some(Outcome::Saved {
            path: download
                .target
                .clone()
                .unwrap_or_else(|| download.directory.join(&download.file_name)),
            size: download.received,
        }),
        DownloadStatus::Failed { reason } => Some(Outcome::Failed {
            reason: reason.clone(),
        }),
        // Only the user pauses downloads, and this run never does; a pause
        // would otherwise wait forever.
        DownloadStatus::Paused => Some(Outcome::Failed {
            reason: "paused".to_owned(),
        }),
        DownloadStatus::Queued
        | DownloadStatus::Connecting
        | DownloadStatus::Downloading
        | DownloadStatus::WaitingForSpace { .. } => None,
    }
}

/// The permanent line printed when a download ends.
pub fn outcome_line(name: &str, outcome: &Outcome) -> String {
    match outcome {
        Outcome::Saved { path, size } => format!("Saved   {} ({})", path.display(), bytes(*size)),
        Outcome::Failed { reason } => format!("Failed  {name}: {reason}"),
    }
}

/// One line about the downloads still running, e.g.
/// "[1/3] 45% · 3.2 MB/s · 2 running · big.iso".
pub fn status_line(unfinished: &[&DownloadView], finished: usize, total: usize) -> String {
    let mut parts = Vec::new();
    let received: u64 = unfinished.iter().map(|d| d.received).sum();
    let sizes: Option<u64> = unfinished.iter().map(|d| d.size).sum();
    match sizes.filter(|size| *size > 0) {
        Some(size) => parts.push(format!(
            "{}% of {}",
            (received as f64 / size as f64 * 100.0).floor().min(100.0),
            bytes(size)
        )),
        None => parts.push(bytes(received)),
    }
    parts.push(speed(unfinished.iter().map(|d| d.bytes_per_second).sum()));
    let running = unfinished.iter().filter(|d| d.status.is_active()).count();
    if running > 0 {
        parts.push(format!("{running} running"));
    }
    if let Some(waiting) = unfinished.iter().find_map(|d| match &d.status {
        DownloadStatus::WaitingForSpace { directory, .. } => Some(directory),
        _ => None,
    }) {
        parts.push(format!("waiting for disk space in {}", waiting.display()));
    } else if let [only] = unfinished {
        parts.push(only.file_name.clone());
    }
    format!("[{finished}/{total}] {}", parts.join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use downpour_core::{CategoryId, DownloadId, QueueId};

    fn view(status: DownloadStatus, received: u64, size: Option<u64>) -> DownloadView {
        DownloadView {
            id: DownloadId(1),
            file_name: "big.iso".to_owned(),
            url: url::Url::parse("https://example.com/big.iso").unwrap(),
            page_url: None,
            referrer: None,
            queue: QueueId::MAIN,
            directory: PathBuf::from("out"),
            target: None,
            category: CategoryId::OTHER,
            status,
            size,
            received,
            bytes_per_second: 2 * 1024 * 1024,
            connections: 8,
            segments: Vec::new(),
            resumable: Some(true),
            size_is_estimate: false,
            progress_fraction: None,
            stream: None,
            added_at_unix: 0,
            finished_at_unix: None,
            stopping: false,
            speed_limit: downpour_core::limiter::DownloadLimit::FollowSettings,
            can_start_now: false,
        }
    }

    #[test]
    fn status_line_shows_share_speed_and_name() {
        let running = view(
            DownloadStatus::Downloading,
            50 * 1024 * 1024,
            Some(100 * 1024 * 1024),
        );
        assert_eq!(
            status_line(&[&running], 1, 2),
            "[1/2] 50% of 100 MB · 2.0 MB/s · 1 running · big.iso"
        );
    }

    #[test]
    fn unknown_sizes_show_bytes_received() {
        let running = view(DownloadStatus::Downloading, 3 * 1024 * 1024, None);
        assert!(status_line(&[&running], 0, 1).starts_with("[0/1] 3.0 MB · "));
    }

    #[test]
    fn only_finished_states_have_an_outcome() {
        assert_eq!(outcome(&view(DownloadStatus::Downloading, 0, None)), None);
        assert_eq!(outcome(&view(DownloadStatus::Queued, 0, None)), None);
        assert_eq!(
            outcome(&view(DownloadStatus::Completed, 7, Some(7))),
            Some(Outcome::Saved {
                path: PathBuf::from("out").join("big.iso"),
                size: 7
            })
        );
        let failed = DownloadStatus::Failed {
            reason: "HTTP 404".into(),
        };
        assert_eq!(
            outcome(&view(failed, 0, None)),
            Some(Outcome::Failed {
                reason: "HTTP 404".into()
            })
        );
    }
}
