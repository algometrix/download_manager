//! The last few minutes of total download speed, for the live graph on the
//! Statistics page.
//!
//! A background thread samples the engine once a second. Sampling does not
//! depend on frames, so the graph also covers time the window spent hidden
//! in the tray.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use downpour_core::DownloadManager;
use eframe::egui;

/// How much history the graph shows.
pub const WINDOW_SECONDS: usize = 300;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);

/// Total speed and running downloads at one moment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    pub bytes_per_second: u64,
    pub active: usize,
}

/// The newest `WINDOW_SECONDS` samples, oldest first.
#[derive(Clone, Debug, Default)]
pub struct SpeedHistory {
    samples: VecDeque<Sample>,
}

/// The figures shown next to the graph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpeedSummary {
    pub now: u64,
    pub peak: u64,
    pub average: u64,
}

impl SpeedHistory {
    pub fn push(&mut self, sample: Sample) {
        if self.samples.len() == WINDOW_SECONDS {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn samples(&self) -> Vec<Sample> {
        self.samples.iter().copied().collect()
    }

    pub fn summary(&self) -> SpeedSummary {
        let speeds = || self.samples.iter().map(|sample| sample.bytes_per_second);
        let count = self.samples.len() as u64;
        SpeedSummary {
            now: self
                .samples
                .back()
                .map_or(0, |sample| sample.bytes_per_second),
            peak: speeds().max().unwrap_or(0),
            average: if count == 0 {
                0
            } else {
                speeds().sum::<u64>() / count
            },
        }
    }
}

/// Owns the sampling thread; dropping it stops and joins the thread.
pub struct SpeedSampler {
    history: Arc<Mutex<SpeedHistory>>,
    /// Set while the Statistics page is open, so the window repaints each
    /// second only when someone is looking at the graph.
    watching: Arc<AtomicBool>,
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl SpeedSampler {
    pub fn start(manager: DownloadManager, ctx: egui::Context) -> std::io::Result<Self> {
        let history = Arc::new(Mutex::new(SpeedHistory::default()));
        let watching = Arc::new(AtomicBool::new(false));
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = {
            let history = Arc::clone(&history);
            let watching = Arc::clone(&watching);
            std::thread::Builder::new()
                .name("downpour-speed-sampler".into())
                .spawn(move || loop {
                    let downloads = manager.snapshot();
                    let sample = Sample {
                        bytes_per_second: downloads.iter().map(|d| d.bytes_per_second).sum(),
                        active: downloads.iter().filter(|d| d.status.is_active()).count(),
                    };
                    history
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push(sample);
                    if watching.load(Ordering::Relaxed) {
                        ctx.request_repaint();
                    }
                    // Waiting on the channel doubles as an interruptible sleep:
                    // a message or a dropped sender ends the loop at once.
                    match stopped.recv_timeout(SAMPLE_INTERVAL) {
                        Err(RecvTimeoutError::Timeout) => continue,
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    }
                })?
        };
        Ok(Self {
            history,
            watching,
            stop: Some(stop),
            thread: Some(thread),
        })
    }

    pub fn history(&self) -> SpeedHistory {
        self.history
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn set_watching(&self, watching: bool) {
        self.watching.store(watching, Ordering::Relaxed);
    }
}

impl Drop for SpeedSampler {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                tracing::warn!("the speed sampler thread panicked");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speed(bytes_per_second: u64) -> Sample {
        Sample {
            bytes_per_second,
            active: 1,
        }
    }

    #[test]
    fn keeps_only_the_newest_window() {
        let mut history = SpeedHistory::default();
        for second in 0..(WINDOW_SECONDS as u64 + 10) {
            history.push(speed(second));
        }
        let samples = history.samples();
        assert_eq!(samples.len(), WINDOW_SECONDS);
        assert_eq!(samples[0].bytes_per_second, 10);
        assert_eq!(
            samples.last().unwrap().bytes_per_second,
            WINDOW_SECONDS as u64 + 9
        );
    }

    #[test]
    fn summarizes_now_peak_and_average() {
        let mut history = SpeedHistory::default();
        assert_eq!(history.summary(), SpeedSummary::default());
        for bytes in [100, 400, 100] {
            history.push(speed(bytes));
        }
        assert_eq!(
            history.summary(),
            SpeedSummary {
                now: 100,
                peak: 400,
                average: 200
            }
        );
    }
}
