//! Command-line arguments, and the parsers for values people type.

use std::num::NonZeroU32;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use downpour_core::stream::VideoQuality;

#[derive(Debug, Parser)]
#[command(
    name = "downpour-cli",
    version,
    about = "Downpour from the command line",
    long_about = "Download files and video streams (HLS, DASH) in the terminal with \
                  `downpour-cli get`, or hand links to the running Downpour app with `downpour-cli send`."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
    /// Log what the engine does (to stderr).
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Download in this terminal, with several connections per file.
    Get(GetArgs),
    /// Hand links to the running Downpour app, which asks or starts them as
    /// its settings say.
    Send(SendArgs),
}

#[derive(Debug, Args)]
pub struct Links {
    /// Addresses of files or stream manifests (.m3u8, .mpd).
    pub urls: Vec<String>,
    /// Also read addresses from this file, one per line ("-" reads stdin).
    /// Blank lines and lines starting with "#" are skipped.
    #[arg(short = 'i', long = "input", value_name = "FILE")]
    pub input: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct GetArgs {
    #[command(flatten)]
    pub links: Links,
    /// Folder to save into.
    #[arg(short, long, value_name = "DIR", default_value = ".")]
    pub output: PathBuf,
    /// File name to save as (only with a single address).
    #[arg(short = 'n', long = "name", value_name = "FILE")]
    pub file_name: Option<String>,
    /// Connections per download.
    #[arg(short, long, value_name = "N", default_value_t = 8,
          value_parser = clap::value_parser!(u8).range(1..=32))]
    pub connections: u8,
    /// Downloads that run at the same time.
    #[arg(short = 'j', long, value_name = "N", default_value_t = 3,
          value_parser = clap::value_parser!(u8).range(1..=16))]
    pub parallel: u8,
    /// Total speed limit, e.g. 500K or 2.5M (bytes per second).
    #[arg(short, long, value_name = "RATE", value_parser = parse_rate)]
    pub limit: Option<SpeedLimit>,
    /// Video stream quality: "best", or a height such as 720 or 1080p.
    #[arg(short, long, value_name = "QUALITY", default_value = "best", value_parser = parse_quality)]
    pub quality: VideoQuality,
    /// ffmpeg to merge separate video and audio tracks (default: found automatically).
    #[arg(long, value_name = "PATH")]
    pub ffmpeg: Option<PathBuf>,
    #[command(flatten)]
    pub request: RequestArgs,
    /// Print only the results.
    #[arg(short = 's', long)]
    pub silent: bool,
}

/// Headers some servers need before they hand out the file.
#[derive(Debug, Args)]
pub struct RequestArgs {
    /// Page the link was found on (sent as Referer).
    #[arg(long, value_name = "URL")]
    pub referer: Option<String>,
    #[arg(long, value_name = "TEXT")]
    pub user_agent: Option<String>,
    /// Cookie header, e.g. "session=abc; lang=en".
    #[arg(long, value_name = "COOKIES")]
    pub cookie: Option<String>,
    /// Origin header; video CDNs often check it.
    #[arg(long, value_name = "URL")]
    pub origin: Option<String>,
}

#[derive(Debug, Args)]
pub struct SendArgs {
    #[command(flatten)]
    pub links: Links,
    /// File name to suggest (only with a single address).
    #[arg(short = 'n', long = "name", value_name = "FILE")]
    pub file_name: Option<String>,
    /// Page the link was found on (sent as Referer).
    #[arg(long, value_name = "URL")]
    pub referer: Option<String>,
}

/// A speed limit as the engine takes it: whole KiB per second, never zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpeedLimit(pub NonZeroU32);

/// "500K", "2.5M", "1G", or plain bytes per second; K, M, and G are powers of
/// 1024, as in the app.
pub fn parse_rate(text: &str) -> Result<SpeedLimit, String> {
    let text = text.trim();
    let trimmed = text.trim_end_matches("/s").trim_end_matches(['B', 'b']);
    let (number, multiplier) = match trimmed.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&trimmed[..trimmed.len() - 1], 1024.0),
        Some('M') => (&trimmed[..trimmed.len() - 1], 1024.0 * 1024.0),
        Some('G') => (&trimmed[..trimmed.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (trimmed, 1.0),
    };
    let value: f64 = number
        .trim()
        .parse()
        .map_err(|_| format!("\"{text}\" is not a speed; try 500K or 2M"))?;
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("\"{text}\" must be more than zero"));
    }
    let kib = (value * multiplier / 1024.0).round();
    if kib > f64::from(u32::MAX) {
        return Err(format!("\"{text}\" is more than Downpour can limit to"));
    }
    NonZeroU32::new(kib as u32)
        .map(SpeedLimit)
        .ok_or_else(|| format!("\"{text}\" is below 1 KB/s, the smallest limit"))
}

/// "best", "720", or "1080p".
pub fn parse_quality(text: &str) -> Result<VideoQuality, String> {
    let text = text.trim().to_ascii_lowercase();
    if text == "best" {
        return Ok(VideoQuality::Best);
    }
    let pixels: u32 = text
        .trim_end_matches('p')
        .parse()
        .map_err(|_| format!("\"{text}\" is not a quality; use best, 720, or 1080p"))?;
    if pixels == 0 {
        return Err("a video height must be more than zero".to_owned());
    }
    Ok(VideoQuality::MaxHeight { pixels })
}

/// The addresses in a list file: one per line, without blanks and comments.
pub fn addresses_in(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kib(text: &str) -> u32 {
        parse_rate(text).unwrap().0.get()
    }

    #[test]
    fn rates_use_binary_units() {
        assert_eq!(kib("500K"), 500);
        assert_eq!(kib("2M"), 2048);
        assert_eq!(kib("2.5m"), 2560);
        assert_eq!(kib("1G"), 1024 * 1024);
        assert_eq!(kib("4096"), 4);
        assert_eq!(kib("3MB/s"), 3072);
    }

    #[test]
    fn rates_reject_nonsense() {
        assert!(parse_rate("fast").is_err());
        assert!(parse_rate("0").is_err());
        assert!(parse_rate("-1M").is_err());
        assert!(parse_rate("100").is_err(), "below 1 KB/s");
    }

    #[test]
    fn qualities_accept_best_and_heights() {
        assert_eq!(parse_quality("Best").unwrap(), VideoQuality::Best);
        assert_eq!(
            parse_quality("720").unwrap(),
            VideoQuality::MaxHeight { pixels: 720 }
        );
        assert_eq!(
            parse_quality("1080p").unwrap(),
            VideoQuality::MaxHeight { pixels: 1080 }
        );
        assert!(parse_quality("hd").is_err());
        assert!(parse_quality("0").is_err());
    }

    #[test]
    fn list_files_skip_blanks_and_comments() {
        let list = "https://a.example/1.zip\n\n  # mirrors\n https://b.example/2.zip \n";
        assert_eq!(
            addresses_in(list).collect::<Vec<_>>(),
            ["https://a.example/1.zip", "https://b.example/2.zip"]
        );
    }

    #[test]
    fn the_command_line_is_well_formed() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
