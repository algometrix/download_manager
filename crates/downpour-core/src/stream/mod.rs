//! Video streams: HLS playlists and DASH manifests are resolved into media
//! pieces, downloaded several at a time, decrypted when they use standard
//! HLS AES-128, and joined into one file (with ffmpeg when tracks must be
//! merged or remuxed).
//!
//! DRM-protected streams (Widevine, PlayReady, FairPlay, SAMPLE-AES) are
//! detected and refused; their keys are never available to a downloader.

pub mod crypto;
pub mod dash;
pub(crate) mod download;
pub mod hls;
pub mod mux;
pub mod plan;
pub mod resolve;

pub use plan::{StreamFormat, StreamOffer, StreamPlan, TrackRole, VariantInfo, VideoQuality};
pub use resolve::StreamError;

/// Why a manifest cannot be turned into a download.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("not an HLS playlist")]
    NotHls,
    #[error("not a DASH manifest")]
    NotDash,
    #[error("this video is protected by DRM ({0}) and cannot be downloaded")]
    Drm(String),
    #[error("live streams cannot be downloaded yet")]
    Live,
    #[error("the manifest lists no video or audio")]
    Empty,
    #[error("not supported yet: {0}")]
    Unsupported(String),
    #[error("the manifest is malformed: {0}")]
    Malformed(String),
}
