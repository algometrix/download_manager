//! Joins downloaded tracks into one playable file with ffmpeg (stream copy,
//! no re-encoding).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio_util::sync::CancellationToken;

use super::plan::{SegmentContainer, StreamPlan};

#[cfg(windows)]
const FFMPEG_FILE: &str = "ffmpeg.exe";
#[cfg(not(windows))]
const FFMPEG_FILE: &str = "ffmpeg";
/// How much of ffmpeg's error output to keep in a failure reason.
const MAX_ERROR_CHARS: usize = 600;

/// Finds ffmpeg: the configured path, then next to this executable, then `PATH`.
pub fn locate_ffmpeg(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = configured {
        return path.is_file().then(|| path.to_path_buf());
    }
    let beside_app = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(FFMPEG_FILE)));
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(FFMPEG_FILE));
    beside_app
        .into_iter()
        .chain(on_path)
        .find(|path| path.is_file())
}

/// The file format a finished stream is saved in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    /// Remuxed by ffmpeg into this container.
    Remuxed(Muxer),
    /// The single track's pieces, joined as downloaded.
    AsDownloaded(SegmentContainer),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Muxer {
    Mp4,
    WebM,
    Matroska,
}

impl Muxer {
    fn ffmpeg_format(self) -> &'static str {
        match self {
            Muxer::Mp4 => "mp4",
            Muxer::WebM => "webm",
            Muxer::Matroska => "matroska",
        }
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "this video has separate video and audio tracks, which need ffmpeg to be merged. \
     Install ffmpeg or set its location in Settings, then resume the download"
)]
pub struct FfmpegRequired;

impl OutputFormat {
    /// Remuxes whenever ffmpeg is available, because joined MPEG-TS or
    /// fragmented MP4 pieces play poorly in many players.
    pub fn choose(
        plan: &StreamPlan,
        ffmpeg: Option<&Path>,
    ) -> Result<OutputFormat, FfmpegRequired> {
        let all_webm = plan
            .tracks
            .iter()
            .all(|track| track.container == SegmentContainer::WebM);
        let any_webm = plan
            .tracks
            .iter()
            .any(|track| track.container == SegmentContainer::WebM);
        match (ffmpeg, plan.tracks.as_slice()) {
            (Some(_), _) if all_webm => Ok(OutputFormat::Remuxed(Muxer::WebM)),
            (Some(_), _) if any_webm => Ok(OutputFormat::Remuxed(Muxer::Matroska)),
            (Some(_), _) => Ok(OutputFormat::Remuxed(Muxer::Mp4)),
            (None, [track]) => Ok(OutputFormat::AsDownloaded(track.container)),
            (None, _) => Err(FfmpegRequired),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Remuxed(Muxer::Mp4) => "mp4",
            OutputFormat::Remuxed(Muxer::WebM) => "webm",
            OutputFormat::Remuxed(Muxer::Matroska) => "mkv",
            OutputFormat::AsDownloaded(container) => container.extension(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("cannot start ffmpeg at {path}: {source}")]
    Start {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("ffmpeg could not merge the tracks ({status}): {message}")]
    Failed { status: String, message: String },
    #[error("cancelled")]
    Cancelled,
}

/// Copies the video and audio streams of `inputs` into `output`.
pub async fn remux(
    ffmpeg: &Path,
    inputs: &[PathBuf],
    output: &Path,
    muxer: Muxer,
    cancel: &CancellationToken,
) -> Result<(), MergeError> {
    let mut command = tokio::process::Command::new(ffmpeg);
    command.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]);
    for input in inputs {
        command.arg("-i").arg(input);
    }
    // Only video and audio: transport streams often carry ID3 or data
    // streams that MP4 cannot hold.
    for index in 0..inputs.len() {
        command.args([
            "-map",
            &format!("{index}:v?"),
            "-map",
            &format!("{index}:a?"),
        ]);
    }
    command.args(["-c", "copy"]);
    if muxer == Muxer::Mp4 {
        command.args(["-movflags", "+faststart"]);
    }
    command
        .args(["-f", muxer.ffmpeg_format()])
        .arg(output)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let child = command.spawn().map_err(|source| MergeError::Start {
        path: ffmpeg.to_path_buf(),
        source,
    })?;
    let finished = tokio::select! {
        biased;
        // Dropping the child's future kills ffmpeg (kill_on_drop).
        _ = cancel.cancelled() => return Err(MergeError::Cancelled),
        finished = child.wait_with_output() => finished,
    };
    let output = finished.map_err(|source| MergeError::Start {
        path: ffmpeg.to_path_buf(),
        source,
    })?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stderr = stderr.trim();
    // The last lines name the actual failure; earlier ones are context.
    let tail_start = stderr
        .char_indices()
        .rev()
        .nth(MAX_ERROR_CHARS)
        .map_or(0, |(index, _)| index);
    let message = match &stderr[tail_start..] {
        "" => "no error output".to_owned(),
        tail => tail.to_owned(),
    };
    Err(MergeError::Failed {
        status: output.status.to_string(),
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::plan::{StreamFormat, TrackPlan, TrackRole};

    fn plan(containers: &[SegmentContainer]) -> StreamPlan {
        StreamPlan {
            format: StreamFormat::Hls,
            tracks: containers
                .iter()
                .map(|container| TrackPlan {
                    role: TrackRole::Video,
                    container: *container,
                    pieces: Vec::new(),
                })
                .collect(),
            variant: None,
        }
    }

    #[test]
    fn chooses_output_format() {
        let ffmpeg = Some(Path::new("ffmpeg"));
        let ts = plan(&[SegmentContainer::MpegTs]);
        assert_eq!(
            OutputFormat::choose(&ts, ffmpeg).unwrap().extension(),
            "mp4"
        );
        assert_eq!(OutputFormat::choose(&ts, None).unwrap().extension(), "ts");
        let webm = plan(&[SegmentContainer::WebM, SegmentContainer::WebM]);
        assert_eq!(
            OutputFormat::choose(&webm, ffmpeg).unwrap().extension(),
            "webm"
        );
        let mixed = plan(&[SegmentContainer::Fmp4, SegmentContainer::WebM]);
        assert_eq!(
            OutputFormat::choose(&mixed, ffmpeg).unwrap().extension(),
            "mkv"
        );
        let separate = plan(&[SegmentContainer::Fmp4, SegmentContainer::Fmp4]);
        assert_eq!(OutputFormat::choose(&separate, None), Err(FfmpegRequired));
    }
}
