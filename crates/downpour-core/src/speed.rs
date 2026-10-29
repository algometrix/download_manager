use std::collections::VecDeque;
use std::time::{Duration, Instant};

const WINDOW: Duration = Duration::from_secs(3);

/// Average throughput over a short sliding window of byte-count samples.
#[derive(Debug, Default)]
pub struct SpeedMeter {
    samples: VecDeque<(Instant, u64)>,
}

impl SpeedMeter {
    pub fn record(&mut self, now: Instant, total_bytes: u64) {
        self.samples.push_back((now, total_bytes));
        while let Some(&(at, _)) = self.samples.front() {
            if now.duration_since(at) > WINDOW && self.samples.len() > 2 {
                self.samples.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn bytes_per_second(&self) -> u64 {
        let (Some(&(first_at, first_bytes)), Some(&(last_at, last_bytes))) =
            (self.samples.front(), self.samples.back())
        else {
            return 0;
        };
        let elapsed = last_at.duration_since(first_at).as_secs_f64();
        if elapsed < 0.2 {
            return 0;
        }
        (last_bytes.saturating_sub(first_bytes) as f64 / elapsed) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn averages_over_window() {
        let start = Instant::now();
        let mut meter = SpeedMeter::default();
        meter.record(start, 0);
        meter.record(start + Duration::from_secs(1), 1000);
        meter.record(start + Duration::from_secs(2), 3000);
        assert_eq!(meter.bytes_per_second(), 1500);
    }

    #[test]
    fn drops_old_samples() {
        let start = Instant::now();
        let mut meter = SpeedMeter::default();
        meter.record(start, 0);
        meter.record(start + Duration::from_secs(10), 0);
        meter.record(start + Duration::from_secs(11), 500);
        meter.record(start + Duration::from_secs(12), 1000);
        assert_eq!(meter.bytes_per_second(), 500);
    }
}
