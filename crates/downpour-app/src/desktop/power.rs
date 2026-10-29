//! Keeps the PC from going to sleep while downloads run, when Settings say
//! so. The display may still turn off; only system sleep is held off.

use downpour_core::DownloadStatus;

use crate::app_settings::SleepWhileDownloading;

/// Whether Downpour is holding off sleep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SleepHold {
    Released,
    Held,
}

/// What `setting` asks for while the downloads are in these states.
pub fn wanted_hold<'a>(
    setting: SleepWhileDownloading,
    statuses: impl IntoIterator<Item = &'a DownloadStatus>,
) -> SleepHold {
    let running = statuses.into_iter().any(DownloadStatus::is_active);
    match (setting, running) {
        (SleepWhileDownloading::KeepAwake, true) => SleepHold::Held,
        (SleepWhileDownloading::KeepAwake, false) | (SleepWhileDownloading::Allow, _) => {
            SleepHold::Released
        }
    }
}

/// Holds off sleep through `SetThreadExecutionState`. Windows ties that
/// request to the calling thread, so this lives on the UI thread, and it is
/// released when dropped (and by Windows when the process exits).
pub struct SleepBlocker {
    hold: SleepHold,
}

impl SleepBlocker {
    pub fn new() -> Self {
        Self {
            hold: SleepHold::Released,
        }
    }

    pub fn set(&mut self, hold: SleepHold) {
        if hold == self.hold {
            return;
        }
        // Recorded even when Windows refuses, so a failure is logged once
        // instead of every frame.
        self.hold = hold;
        match native::set_execution_state(hold) {
            Ok(()) => tracing::info!(?hold, "changed whether the PC may sleep"),
            Err(error) => {
                tracing::warn!(%error, ?hold, "could not change whether the PC may sleep")
            }
        }
    }
}

impl Drop for SleepBlocker {
    fn drop(&mut self) {
        self.set(SleepHold::Released);
    }
}

#[derive(Debug, thiserror::Error)]
#[error("SetThreadExecutionState was refused")]
pub struct ExecutionStateRefused;

#[cfg(windows)]
mod native {
    use windows_sys::Win32::System::Power::{
        SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
    };

    use super::{ExecutionStateRefused, SleepHold};

    pub fn set_execution_state(hold: SleepHold) -> Result<(), ExecutionStateRefused> {
        let flags = match hold {
            SleepHold::Held => ES_CONTINUOUS | ES_SYSTEM_REQUIRED,
            SleepHold::Released => ES_CONTINUOUS,
        };
        // SAFETY: takes flags only; returns the previous state, or 0 on failure.
        let previous = unsafe { SetThreadExecutionState(flags) };
        if previous == 0 {
            return Err(ExecutionStateRefused);
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod native {
    use super::{ExecutionStateRefused, SleepHold};

    pub fn set_execution_state(_hold: SleepHold) -> Result<(), ExecutionStateRefused> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_is_held_off_only_while_a_download_runs() {
        let running = [DownloadStatus::Paused, DownloadStatus::Downloading];
        let idle = [
            DownloadStatus::Paused,
            DownloadStatus::Queued,
            DownloadStatus::Completed,
        ];
        assert_eq!(
            wanted_hold(SleepWhileDownloading::KeepAwake, &running),
            SleepHold::Held
        );
        assert_eq!(
            wanted_hold(SleepWhileDownloading::KeepAwake, &idle),
            SleepHold::Released
        );
        assert_eq!(
            wanted_hold(SleepWhileDownloading::Allow, &running),
            SleepHold::Released
        );
    }
}
