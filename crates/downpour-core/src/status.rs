use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Lifecycle of one download. Transitions are validated by [`DownloadStatus::apply`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DownloadStatus {
    Queued,
    Connecting,
    Downloading,
    Paused,
    Completed,
    Failed {
        reason: String,
    },
    /// The disk is too full; the download continues by itself once
    /// `directory` has room for `needed_bytes` (when known).
    WaitingForSpace {
        directory: PathBuf,
        needed_bytes: Option<u64>,
    },
}

/// Something that happened to a download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusEvent {
    /// The queue picked the download and a transfer task started.
    Started,
    /// The server answered and bytes are flowing.
    Connected,
    /// The user asked to stop; the transfer task has exited.
    Paused,
    /// The user asked to (re)start; the download waits for a queue slot.
    Enqueued,
    Completed,
    Failed {
        reason: String,
    },
    /// The transfer stopped because the disk is full.
    OutOfSpace {
        directory: PathBuf,
        needed_bytes: Option<u64>,
    },
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error("cannot apply {event:?} to a download that is {from:?}")]
pub struct InvalidTransition {
    pub from: DownloadStatus,
    pub event: StatusEvent,
}

impl DownloadStatus {
    pub fn apply(&self, event: StatusEvent) -> Result<DownloadStatus, InvalidTransition> {
        use DownloadStatus as S;
        use StatusEvent as E;
        let next = match (self, &event) {
            (S::Queued, E::Started) => S::Connecting,
            (S::Connecting, E::Connected) => S::Downloading,
            (S::Connecting | S::Downloading, E::Completed) => S::Completed,
            (S::Connecting | S::Downloading, E::Failed { reason }) => S::Failed {
                reason: reason.clone(),
            },
            (
                S::Connecting | S::Downloading,
                E::OutOfSpace {
                    directory,
                    needed_bytes,
                },
            ) => S::WaitingForSpace {
                directory: directory.clone(),
                needed_bytes: *needed_bytes,
            },
            (S::Queued | S::Connecting | S::Downloading | S::WaitingForSpace { .. }, E::Paused) => {
                S::Paused
            }
            (S::Paused | S::Failed { .. } | S::WaitingForSpace { .. }, E::Enqueued) => S::Queued,
            _ => {
                return Err(InvalidTransition {
                    from: self.clone(),
                    event,
                })
            }
        };
        Ok(next)
    }

    /// A transfer task may be running for downloads in these states.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            DownloadStatus::Connecting | DownloadStatus::Downloading
        )
    }

    /// A transfer has started and not stopped: it runs, or it waits for disk
    /// space and continues by itself.
    pub fn is_underway(&self) -> bool {
        match self {
            DownloadStatus::Connecting
            | DownloadStatus::Downloading
            | DownloadStatus::WaitingForSpace { .. } => true,
            DownloadStatus::Queued
            | DownloadStatus::Paused
            | DownloadStatus::Completed
            | DownloadStatus::Failed { .. } => false,
        }
    }

    /// Bytes remain to be downloaded without the user starting it over: it
    /// is queued, running, paused, or waiting for space.
    pub fn has_work_left(&self) -> bool {
        match self {
            DownloadStatus::Queued
            | DownloadStatus::Connecting
            | DownloadStatus::Downloading
            | DownloadStatus::Paused
            | DownloadStatus::WaitingForSpace { .. } => true,
            DownloadStatus::Completed | DownloadStatus::Failed { .. } => false,
        }
    }

    /// A finished download has no more bytes to limit.
    pub fn can_change_speed_limit(&self) -> bool {
        *self != DownloadStatus::Completed
    }

    pub fn can_resume(&self) -> bool {
        matches!(
            self,
            DownloadStatus::Paused
                | DownloadStatus::Failed { .. }
                | DownloadStatus::WaitingForSpace { .. }
        )
    }

    pub fn can_pause(&self) -> bool {
        matches!(
            self,
            DownloadStatus::Queued
                | DownloadStatus::Connecting
                | DownloadStatus::Downloading
                | DownloadStatus::WaitingForSpace { .. }
        )
    }

    /// A download persisted while active belonged to a process that exited;
    /// its transfer task no longer exists.
    pub fn after_restart(self) -> DownloadStatus {
        if self.is_active() {
            DownloadStatus::Paused
        } else {
            self
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            DownloadStatus::Queued => "Queued",
            DownloadStatus::Connecting => "Connecting",
            DownloadStatus::Downloading => "Downloading",
            DownloadStatus::Paused => "Paused",
            DownloadStatus::Completed => "Completed",
            DownloadStatus::Failed { .. } => "Failed",
            DownloadStatus::WaitingForSpace { .. } => "Waiting for disk space",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_lifecycle() {
        let status = DownloadStatus::Queued
            .apply(StatusEvent::Started)
            .and_then(|s| s.apply(StatusEvent::Connected))
            .and_then(|s| s.apply(StatusEvent::Completed))
            .unwrap();
        assert_eq!(status, DownloadStatus::Completed);
    }

    #[test]
    fn completed_download_cannot_restart() {
        let error = DownloadStatus::Completed
            .apply(StatusEvent::Enqueued)
            .unwrap_err();
        assert_eq!(error.from, DownloadStatus::Completed);
    }

    #[test]
    fn failed_download_can_be_retried() {
        let failed = DownloadStatus::Failed {
            reason: "timeout".into(),
        };
        assert_eq!(
            failed.apply(StatusEvent::Enqueued).unwrap(),
            DownloadStatus::Queued
        );
    }

    #[test]
    fn full_disk_waits_and_resumes_into_the_queue() {
        let waiting = DownloadStatus::Downloading
            .apply(StatusEvent::OutOfSpace {
                directory: PathBuf::from("D:/dl"),
                needed_bytes: Some(10),
            })
            .unwrap();
        assert!(matches!(
            waiting,
            DownloadStatus::WaitingForSpace {
                needed_bytes: Some(10),
                ..
            }
        ));
        assert!(!waiting.is_active());
        assert_eq!(
            waiting.apply(StatusEvent::Enqueued).unwrap(),
            DownloadStatus::Queued
        );
        assert_eq!(
            waiting.apply(StatusEvent::Paused).unwrap(),
            DownloadStatus::Paused
        );
    }

    #[test]
    fn underway_and_unfinished_downloads() {
        let waiting = DownloadStatus::WaitingForSpace {
            directory: PathBuf::from("D:/dl"),
            needed_bytes: None,
        };
        assert!(waiting.is_underway() && waiting.has_work_left());
        assert!(DownloadStatus::Downloading.is_underway());
        assert!(!DownloadStatus::Paused.is_underway());
        assert!(DownloadStatus::Paused.has_work_left());
        assert!(DownloadStatus::Queued.has_work_left());
        assert!(!DownloadStatus::Completed.has_work_left());
        let failed = DownloadStatus::Failed {
            reason: "gone".into(),
        };
        assert!(!failed.has_work_left() && !failed.is_underway());
    }

    #[test]
    fn active_downloads_become_paused_after_restart() {
        assert_eq!(
            DownloadStatus::Downloading.after_restart(),
            DownloadStatus::Paused
        );
        assert_eq!(
            DownloadStatus::Queued.after_restart(),
            DownloadStatus::Queued
        );
    }
}
