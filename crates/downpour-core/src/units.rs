//! Sizes, speeds, and times as people read them, shared by the app and the
//! command line.

use std::time::Duration;

const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

pub fn bytes(value: u64) -> String {
    let mut amount = value as f64;
    let mut unit = 0;
    while amount >= 1024.0 && unit < UNITS.len() - 1 {
        amount /= 1024.0;
        unit += 1;
    }
    match unit {
        0 => format!("{value} B"),
        _ if amount >= 100.0 => format!("{amount:.0} {}", UNITS[unit]),
        _ => format!("{amount:.1} {}", UNITS[unit]),
    }
}

pub fn speed(bytes_per_second: u64) -> String {
    format!("{}/s", bytes(bytes_per_second))
}

pub fn duration(value: Duration) -> String {
    let total = value.as_secs();
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    match (hours, minutes) {
        (0, 0) => format!("{seconds}s"),
        (0, _) => format!("{minutes}m {seconds:02}s"),
        _ if hours >= 100 => "> 4 days".to_owned(),
        _ => format!("{hours}h {minutes:02}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(250 * 1024 * 1024), "250 MB");
        assert_eq!(bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn formats_durations() {
        assert_eq!(duration(Duration::from_secs(9)), "9s");
        assert_eq!(duration(Duration::from_secs(125)), "2m 05s");
        assert_eq!(duration(Duration::from_secs(3 * 3600 + 7 * 60)), "3h 07m");
        assert_eq!(duration(Duration::from_secs(500 * 3600)), "> 4 days");
    }
}
