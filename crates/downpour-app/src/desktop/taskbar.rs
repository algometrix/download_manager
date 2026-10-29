//! Overall download progress on the taskbar button.
//!
//! A batch starts when a download starts and lasts until none of its
//! downloads has work left. Finished downloads stay in the batch so the bar
//! never moves backward when one of several completes. The bar is
//! - normal while a download in the batch runs,
//! - red while one waits for disk space,
//! - yellow when every unfinished download in the batch is paused or
//!   waiting in its queue,
//! - cleared when the batch is over.

use std::collections::BTreeSet;

use downpour_core::{DownloadId, DownloadStatus, DownloadView};

use crate::platform::MainWindow;

/// The bar is set in thousandths, so it is updated only when it visibly moves.
const PERMILLE_FULL: u16 = 1000;

/// What the taskbar button shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskbarProgress {
    /// Nothing downloading: no bar.
    Idle,
    /// Downloading, but no download in the batch knows its size yet.
    Indeterminate,
    Normal {
        permille: u16,
    },
    Paused {
        permille: u16,
    },
    Error {
        permille: u16,
    },
}

/// The facts about one download the bar needs.
#[derive(Clone, Copy, Debug)]
pub struct TransferState<'a> {
    pub id: DownloadId,
    pub status: &'a DownloadStatus,
    pub received: u64,
    pub size: Option<u64>,
}

impl<'a> From<&'a DownloadView> for TransferState<'a> {
    fn from(download: &'a DownloadView) -> Self {
        TransferState {
            id: download.id,
            status: &download.status,
            received: download.received,
            size: download.size,
        }
    }
}

/// Remembers which downloads make up the current batch.
#[derive(Debug, Default)]
pub struct ProgressTracker {
    batch: BTreeSet<DownloadId>,
}

impl ProgressTracker {
    pub fn update<'a>(
        &mut self,
        downloads: impl IntoIterator<Item = TransferState<'a>>,
    ) -> TaskbarProgress {
        let downloads: Vec<TransferState<'a>> = downloads.into_iter().collect();
        // A failed or removed download leaves the batch: its bytes would
        // hold the bar short of full.
        self.batch.retain(|id| {
            downloads.iter().any(|download| {
                download.id == *id && !matches!(download.status, DownloadStatus::Failed { .. })
            })
        });
        self.batch.extend(
            downloads
                .iter()
                // A download that starts running, or gets stuck, starts or
                // joins a batch.
                .filter(|download| download.status.is_underway())
                .map(|download| download.id),
        );
        let members: Vec<&TransferState<'a>> = downloads
            .iter()
            .filter(|download| self.batch.contains(&download.id))
            .collect();
        if !members.iter().any(|member| member.status.has_work_left()) {
            self.batch.clear();
            return TaskbarProgress::Idle;
        }

        let permille = permille_done(&members);
        let waiting_for_space = members
            .iter()
            .any(|member| matches!(member.status, DownloadStatus::WaitingForSpace { .. }));
        let running = members.iter().any(|member| member.status.is_active());
        match (waiting_for_space, running, permille) {
            (true, _, permille) => TaskbarProgress::Error {
                permille: permille.unwrap_or(PERMILLE_FULL),
            },
            (false, true, Some(permille)) => TaskbarProgress::Normal { permille },
            (false, true, None) => TaskbarProgress::Indeterminate,
            // A full bar keeps the color visible when no size is known.
            (false, false, permille) => TaskbarProgress::Paused {
                permille: permille.unwrap_or(PERMILLE_FULL),
            },
        }
    }
}

/// The share of the batch's known bytes received, or `None` when no member
/// knows its size.
fn permille_done(members: &[&TransferState<'_>]) -> Option<u16> {
    let (received, total) = members
        .iter()
        .filter_map(|member| {
            let size = member.size.filter(|size| *size > 0)?;
            let received = match member.status {
                DownloadStatus::Completed => size,
                _ => member.received.min(size),
            };
            Some((received, size))
        })
        .fold((0u128, 0u128), |(received, total), (part, size)| {
            (received + u128::from(part), total + u128::from(size))
        });
    if total == 0 {
        return None;
    }
    let permille = received * u128::from(PERMILLE_FULL) / total;
    Some(u16::try_from(permille).unwrap_or(PERMILLE_FULL))
}

/// The taskbar button itself, through `ITaskbarList3`. Calls are made only
/// when the shown state changes.
pub struct TaskbarButton {
    #[cfg_attr(not(windows), allow(dead_code))]
    window: MainWindow,
    shown: Option<TaskbarProgress>,
    #[cfg(windows)]
    list: native::TaskbarList,
}

impl TaskbarButton {
    pub fn new(window: MainWindow) -> Self {
        Self {
            window,
            shown: None,
            #[cfg(windows)]
            list: native::TaskbarList::NotCreated,
        }
    }

    /// Windows forgets the bar when it recreates the button (Explorer
    /// restarted, or the window came back from the tray).
    pub fn forget_shown(&mut self) {
        self.shown = None;
    }

    pub fn show(&mut self, progress: TaskbarProgress) {
        if self.shown == Some(progress) {
            return;
        }
        // Remembered even when it fails, so a window without a button (in
        // the tray) is not retried every frame.
        self.shown = Some(progress);
        #[cfg(windows)]
        self.list.show(self.window, progress);
    }
}

#[cfg(windows)]
mod native {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::{
        ITaskbarList3, TaskbarList as TaskbarListClass, TBPFLAG, TBPF_ERROR, TBPF_INDETERMINATE,
        TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED,
    };

    use super::{TaskbarProgress, PERMILLE_FULL};
    use crate::desktop::com::{Apartment, ComApartment, ComError};
    use crate::platform::MainWindow;

    /// The interface and the COM apartment it lives in. Fields drop in
    /// order: the interface is released before COM is left.
    pub struct ReadyList {
        list: ITaskbarList3,
        _apartment: ComApartment,
    }

    pub enum TaskbarList {
        NotCreated,
        Ready(ReadyList),
        /// Creating it failed once; it was logged and is not retried.
        Unavailable,
    }

    #[derive(Debug, thiserror::Error)]
    enum CreateError {
        #[error(transparent)]
        Com(#[from] ComError),
        #[error("ITaskbarList3: {0}")]
        Interface(#[from] windows::core::Error),
    }

    impl TaskbarList {
        pub fn show(&mut self, window: MainWindow, progress: TaskbarProgress) {
            let Some(list) = self.ready() else {
                return;
            };
            let hwnd = HWND(window.hwnd());
            let (flag, permille): (TBPFLAG, Option<u16>) = match progress {
                TaskbarProgress::Idle => (TBPF_NOPROGRESS, None),
                TaskbarProgress::Indeterminate => (TBPF_INDETERMINATE, None),
                TaskbarProgress::Normal { permille } => (TBPF_NORMAL, Some(permille)),
                TaskbarProgress::Paused { permille } => (TBPF_PAUSED, Some(permille)),
                TaskbarProgress::Error { permille } => (TBPF_ERROR, Some(permille)),
            };
            // SAFETY: `hwnd` is this process's live main window, and `list`
            // was created on this (the UI) thread.
            let result = unsafe {
                list.SetProgressState(hwnd, flag)
                    .and_then(|()| match permille {
                        Some(permille) => list.SetProgressValue(
                            hwnd,
                            u64::from(permille),
                            u64::from(PERMILLE_FULL),
                        ),
                        None => Ok(()),
                    })
            };
            if let Err(error) = result {
                // Expected while the window is in the tray and has no button.
                tracing::debug!(%error, ?progress, "could not set the taskbar progress");
            }
        }

        fn ready(&mut self) -> Option<&ITaskbarList3> {
            if let TaskbarList::NotCreated = self {
                *self = match create() {
                    Ok(ready) => TaskbarList::Ready(ready),
                    Err(error) => {
                        tracing::warn!(%error, "no taskbar progress");
                        TaskbarList::Unavailable
                    }
                };
            }
            match self {
                TaskbarList::Ready(ready) => Some(&ready.list),
                TaskbarList::NotCreated | TaskbarList::Unavailable => None,
            }
        }
    }

    /// Runs on the UI thread, which the taskbar interface requires to be a
    /// single-threaded apartment. winit has usually made it one already (for
    /// drag and drop); entering again is then counted and balanced.
    fn create() -> Result<ReadyList, CreateError> {
        let apartment = ComApartment::enter(Apartment::SingleThreaded)?;
        // SAFETY: plain COM object creation inside the apartment entered
        // above; the interface is released before the apartment is left.
        let list = unsafe {
            let list: ITaskbarList3 =
                CoCreateInstance(&TaskbarListClass, None, CLSCTX_INPROC_SERVER)?;
            list.HrInit()?;
            list
        };
        Ok(ReadyList {
            list,
            _apartment: apartment,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Download {
        id: u64,
        status: DownloadStatus,
        received: u64,
        size: Option<u64>,
    }

    fn download(id: u64, status: DownloadStatus, received: u64, size: Option<u64>) -> Download {
        Download {
            id,
            status,
            received,
            size,
        }
    }

    fn states(downloads: &[Download]) -> Vec<TransferState<'_>> {
        downloads
            .iter()
            .map(|download| TransferState {
                id: DownloadId(download.id),
                status: &download.status,
                received: download.received,
                size: download.size,
            })
            .collect()
    }

    fn failed() -> DownloadStatus {
        DownloadStatus::Failed {
            reason: "timeout".into(),
        }
    }

    #[test]
    fn nothing_running_shows_no_bar() {
        let mut tracker = ProgressTracker::default();
        let list = [
            download(1, DownloadStatus::Paused, 10, Some(100)),
            download(2, DownloadStatus::Completed, 100, Some(100)),
            download(3, DownloadStatus::Queued, 0, Some(100)),
        ];
        // Downloads paused or queued before anything ran are not a batch.
        assert_eq!(tracker.update(states(&list)), TaskbarProgress::Idle);
    }

    #[test]
    fn running_downloads_share_one_bar() {
        let mut tracker = ProgressTracker::default();
        let list = [
            download(1, DownloadStatus::Downloading, 50, Some(100)),
            download(2, DownloadStatus::Downloading, 0, Some(300)),
        ];
        assert_eq!(
            tracker.update(states(&list)),
            TaskbarProgress::Normal { permille: 125 }
        );
    }

    #[test]
    fn unknown_sizes_make_the_bar_indeterminate() {
        let mut tracker = ProgressTracker::default();
        let list = [download(1, DownloadStatus::Connecting, 0, None)];
        assert_eq!(
            tracker.update(states(&list)),
            TaskbarProgress::Indeterminate
        );
    }

    #[test]
    fn a_finished_download_keeps_counting_so_the_bar_never_moves_back() {
        let mut tracker = ProgressTracker::default();
        let running = [
            download(1, DownloadStatus::Downloading, 90, Some(100)),
            download(2, DownloadStatus::Downloading, 10, Some(100)),
        ];
        assert_eq!(
            tracker.update(states(&running)),
            TaskbarProgress::Normal { permille: 500 }
        );
        let one_done = [
            download(1, DownloadStatus::Completed, 100, Some(100)),
            download(2, DownloadStatus::Downloading, 20, Some(100)),
        ];
        assert_eq!(
            tracker.update(states(&one_done)),
            TaskbarProgress::Normal { permille: 600 }
        );
        let all_done = [
            download(1, DownloadStatus::Completed, 100, Some(100)),
            download(2, DownloadStatus::Completed, 100, Some(100)),
        ];
        assert_eq!(tracker.update(states(&all_done)), TaskbarProgress::Idle);
    }

    #[test]
    fn pausing_the_batch_turns_the_bar_yellow_until_it_resumes() {
        let mut tracker = ProgressTracker::default();
        let running = [download(1, DownloadStatus::Downloading, 40, Some(100))];
        tracker.update(states(&running));
        let paused = [download(1, DownloadStatus::Paused, 40, Some(100))];
        assert_eq!(
            tracker.update(states(&paused)),
            TaskbarProgress::Paused { permille: 400 }
        );
        assert_eq!(
            tracker.update(states(&running)),
            TaskbarProgress::Normal { permille: 400 }
        );
    }

    #[test]
    fn a_full_disk_turns_the_bar_red() {
        let mut tracker = ProgressTracker::default();
        let list = [
            download(1, DownloadStatus::Downloading, 50, Some(100)),
            download(
                2,
                DownloadStatus::WaitingForSpace {
                    directory: "D:/dl".into(),
                    needed_bytes: Some(10),
                },
                50,
                Some(100),
            ),
        ];
        assert_eq!(
            tracker.update(states(&list)),
            TaskbarProgress::Error { permille: 500 }
        );
    }

    #[test]
    fn failed_and_removed_downloads_leave_the_batch() {
        let mut tracker = ProgressTracker::default();
        let running = [
            download(1, DownloadStatus::Downloading, 10, Some(100)),
            download(2, DownloadStatus::Downloading, 90, Some(100)),
            download(3, DownloadStatus::Downloading, 0, Some(100)),
        ];
        tracker.update(states(&running));
        let later = [
            download(1, failed(), 10, Some(100)),
            download(2, DownloadStatus::Downloading, 95, Some(100)),
        ];
        assert_eq!(
            tracker.update(states(&later)),
            TaskbarProgress::Normal { permille: 950 }
        );
        let only_failures = [download(1, failed(), 10, Some(100))];
        assert_eq!(
            tracker.update(states(&only_failures)),
            TaskbarProgress::Idle
        );
    }

    #[test]
    fn received_beyond_an_estimated_size_still_reads_as_full() {
        let mut tracker = ProgressTracker::default();
        let list = [download(1, DownloadStatus::Downloading, 150, Some(100))];
        assert_eq!(
            tracker.update(states(&list)),
            TaskbarProgress::Normal { permille: 1000 }
        );
    }
}
