//! A video stream resolved into the media pieces to download, and the
//! quality choices it offers.

use serde::{Deserialize, Serialize};
use url::Url;

/// Adaptive streaming protocol of a manifest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamFormat {
    /// HTTP Live Streaming (`.m3u8` playlists).
    Hls,
    /// MPEG-DASH (`.mpd` manifests).
    Dash,
}

impl StreamFormat {
    /// Recognizes a manifest by the extension of its URL path.
    pub fn from_url(url: &Url) -> Option<StreamFormat> {
        let path = url.path().to_ascii_lowercase();
        if path.ends_with(".m3u8") {
            Some(StreamFormat::Hls)
        } else if path.ends_with(".mpd") {
            Some(StreamFormat::Dash)
        } else {
            None
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StreamFormat::Hls => "HLS",
            StreamFormat::Dash => "DASH",
        }
    }
}

/// Which video quality to download when a stream offers several.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VideoQuality {
    #[default]
    Best,
    /// The best variant no taller than this many pixels, or the smallest one
    /// when every variant is taller.
    MaxHeight { pixels: u32 },
}

impl VideoQuality {
    pub const CHOICES: [VideoQuality; 5] = [
        VideoQuality::Best,
        VideoQuality::MaxHeight { pixels: 2160 },
        VideoQuality::MaxHeight { pixels: 1080 },
        VideoQuality::MaxHeight { pixels: 720 },
        VideoQuality::MaxHeight { pixels: 480 },
    ];

    pub fn label(self) -> String {
        match self {
            VideoQuality::Best => "Best available".to_owned(),
            VideoQuality::MaxHeight { pixels } => format!("Up to {pixels}p"),
        }
    }

    /// Index of the preferred variant among `variants`, or `None` if empty.
    pub fn pick(self, variants: &[VariantInfo]) -> Option<usize> {
        let rank = |variant: &VariantInfo| (variant.height.unwrap_or(0), variant.bandwidth);
        let best_of = |indices: &mut dyn Iterator<Item = usize>| {
            indices.max_by_key(|&index| rank(&variants[index]))
        };
        match self {
            VideoQuality::Best => best_of(&mut (0..variants.len())),
            VideoQuality::MaxHeight { pixels } => {
                let fitting = best_of(
                    &mut (0..variants.len())
                        .filter(|&index| variants[index].height.is_none_or(|h| h <= pixels)),
                );
                fitting.or_else(|| (0..variants.len()).min_by_key(|&index| rank(&variants[index])))
            }
        }
    }
}

/// One quality a stream offers, as shown to the user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VariantInfo {
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// Bits per second, as announced by the manifest.
    pub bandwidth: u64,
    pub codecs: Option<String>,
}

impl VariantInfo {
    pub fn label(&self) -> String {
        let size = match self.height {
            Some(height) => format!("{height}p"),
            None => "Unknown size".to_owned(),
        };
        if self.bandwidth == 0 {
            return size;
        }
        let megabits = self.bandwidth as f64 / 1_000_000.0;
        format!("{size} · {megabits:.1} Mbps")
    }
}

/// What a manifest offers, before choosing a quality.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamOffer {
    pub format: StreamFormat,
    /// Empty when the manifest describes a single quality without details.
    pub variants: Vec<VariantInfo>,
    /// Audio comes as a separate track that must be merged with the video.
    pub separate_audio: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackRole {
    Video,
    Audio,
    /// Video and audio interleaved in the same pieces.
    Combined,
}

impl TrackRole {
    pub fn label(self) -> &'static str {
        match self {
            TrackRole::Video => "Video",
            TrackRole::Audio => "Audio",
            TrackRole::Combined => "Video and audio",
        }
    }
}

/// How a track's pieces are packaged; decides how they can be joined.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentContainer {
    /// MPEG transport stream: pieces join by plain concatenation.
    MpegTs,
    /// Fragmented MP4: an initialization piece followed by media fragments.
    Fmp4,
    WebM,
    /// Raw audio such as ADTS AAC.
    Elementary,
}

impl SegmentContainer {
    /// The file extension for this container when saved without remuxing.
    pub fn extension(self) -> &'static str {
        match self {
            SegmentContainer::MpegTs => "ts",
            SegmentContainer::Fmp4 => "mp4",
            SegmentContainer::WebM => "webm",
            SegmentContainer::Elementary => "aac",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ByteRange {
    pub start: u64,
    pub length: u64,
}

impl ByteRange {
    /// The `Range` header value for this span.
    pub fn header_value(self) -> String {
        format!("bytes={}-{}", self.start, self.start + self.length - 1)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum PieceEncryption {
    Clear,
    /// HLS `METHOD=AES-128`: the whole piece is AES-128-CBC with PKCS#7
    /// padding, keyed by the 16 bytes served at `key_url`.
    Aes128 {
        key_url: Url,
        iv: [u8; 16],
    },
}

/// One request's worth of media: an initialization piece or a segment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaPiece {
    pub url: Url,
    pub range: Option<ByteRange>,
    pub encryption: PieceEncryption,
}

impl MediaPiece {
    pub fn clear(url: Url) -> Self {
        Self {
            url,
            range: None,
            encryption: PieceEncryption::Clear,
        }
    }
}

/// Every piece of one track, in playback order. The initialization piece,
/// when the container has one, comes first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrackPlan {
    pub role: TrackRole,
    pub container: SegmentContainer,
    pub pieces: Vec<MediaPiece>,
}

/// The tracks chosen for download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamPlan {
    pub format: StreamFormat,
    pub tracks: Vec<TrackPlan>,
    /// The chosen video quality, when the manifest described it.
    pub variant: Option<VariantInfo>,
}

impl StreamPlan {
    /// Whether saved progress made against `other` is still valid for this
    /// plan: the same tracks with the same number of pieces. URLs may differ
    /// because signed addresses are reissued.
    pub fn has_same_layout(&self, other: &StreamPlan) -> bool {
        self.tracks.len() == other.tracks.len()
            && self.tracks.iter().zip(&other.tracks).all(|(a, b)| {
                a.role == b.role && a.container == b.container && a.pieces.len() == b.pieces.len()
            })
    }

    pub fn needs_merge(&self) -> bool {
        self.tracks.len() > 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variant(height: u32, bandwidth: u64) -> VariantInfo {
        VariantInfo {
            width: None,
            height: Some(height),
            bandwidth,
            codecs: None,
        }
    }

    #[test]
    fn best_quality_prefers_height_then_bandwidth() {
        let variants = [
            variant(720, 3_000_000),
            variant(1080, 5_000_000),
            variant(1080, 6_000_000),
        ];
        assert_eq!(VideoQuality::Best.pick(&variants), Some(2));
    }

    #[test]
    fn max_height_picks_best_fitting_or_smallest() {
        let variants = [
            variant(480, 1_000_000),
            variant(720, 3_000_000),
            variant(1080, 5_000_000),
        ];
        assert_eq!(
            VideoQuality::MaxHeight { pixels: 720 }.pick(&variants),
            Some(1)
        );
        assert_eq!(
            VideoQuality::MaxHeight { pixels: 360 }.pick(&variants),
            Some(0)
        );
        assert_eq!(VideoQuality::Best.pick(&[]), None);
    }

    #[test]
    fn recognizes_manifest_urls() {
        let hls = Url::parse("https://cdn.example/v/master.m3u8?token=1").unwrap();
        let dash = Url::parse("https://cdn.example/v/manifest.MPD").unwrap();
        let file = Url::parse("https://cdn.example/v/movie.mp4").unwrap();
        assert_eq!(StreamFormat::from_url(&hls), Some(StreamFormat::Hls));
        assert_eq!(StreamFormat::from_url(&dash), Some(StreamFormat::Dash));
        assert_eq!(StreamFormat::from_url(&file), None);
    }

    #[test]
    fn byte_range_header_is_inclusive() {
        let range = ByteRange {
            start: 100,
            length: 50,
        };
        assert_eq!(range.header_value(), "bytes=100-149");
    }
}
