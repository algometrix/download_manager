//! Parses HLS playlists (RFC 8216) into variants and media pieces.

use url::Url;

use super::plan::{ByteRange, MediaPiece, PieceEncryption, SegmentContainer, VariantInfo};
use super::ManifestError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Playlist {
    /// Lists the qualities; each points to a media playlist.
    Multivariant(MultivariantPlaylist),
    /// Lists the pieces of one track.
    Media(MediaPlaylist),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MultivariantPlaylist {
    pub variants: Vec<Variant>,
    pub audio_renditions: Vec<AudioRendition>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Variant {
    pub url: Url,
    pub info: VariantInfo,
    /// `GROUP-ID` of the audio renditions that go with this variant.
    pub audio_group: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioRendition {
    pub group_id: String,
    pub name: Option<String>,
    pub is_default: bool,
    /// `None` when the audio is interleaved in the variant's own pieces.
    pub url: Option<Url>,
}

impl MultivariantPlaylist {
    /// The separate audio playlist to download with `variant`, if any.
    pub fn audio_for(&self, variant: &Variant) -> Option<&AudioRendition> {
        let group = variant.audio_group.as_deref()?;
        let members = || {
            self.audio_renditions
                .iter()
                .filter(move |rendition| rendition.group_id == group && rendition.url.is_some())
        };
        members()
            .find(|rendition| rendition.is_default)
            .or_else(|| members().next())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaPlaylist {
    pub container: SegmentContainer,
    /// The initialization piece first (if any), then every media segment.
    pub pieces: Vec<MediaPiece>,
}

pub fn parse(text: &str, base: &Url) -> Result<Playlist, ManifestError> {
    let mut lines = text
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    if lines.next() != Some("#EXTM3U") {
        return Err(ManifestError::NotHls);
    }
    let lines: Vec<&str> = lines.collect();
    let is_multivariant = lines
        .iter()
        .any(|line| line.starts_with("#EXT-X-STREAM-INF"));
    if is_multivariant {
        parse_multivariant(&lines, base).map(Playlist::Multivariant)
    } else {
        parse_media(&lines, base).map(Playlist::Media)
    }
}

fn parse_multivariant(lines: &[&str], base: &Url) -> Result<MultivariantPlaylist, ManifestError> {
    let mut variants = Vec::new();
    let mut audio_renditions = Vec::new();
    let mut pending: Option<Attributes> = None;

    for line in lines {
        if let Some(list) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            pending = Some(Attributes::parse(list));
        } else if let Some(list) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attributes = Attributes::parse(list);
            if attributes.get("TYPE") != Some("AUDIO") {
                continue;
            }
            let Some(group_id) = attributes.get("GROUP-ID") else {
                continue;
            };
            audio_renditions.push(AudioRendition {
                group_id: group_id.to_owned(),
                name: attributes.get("NAME").map(str::to_owned),
                is_default: attributes.get("DEFAULT") == Some("YES"),
                url: attributes
                    .get("URI")
                    .map(|uri| join(base, uri))
                    .transpose()?,
            });
        } else if line.starts_with('#') {
            continue;
        } else if let Some(attributes) = pending.take() {
            let (width, height) = attributes
                .get("RESOLUTION")
                .and_then(parse_resolution)
                .unzip();
            variants.push(Variant {
                url: join(base, line)?,
                info: VariantInfo {
                    width,
                    height,
                    bandwidth: attributes
                        .get("BANDWIDTH")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0),
                    codecs: attributes.get("CODECS").map(str::to_owned),
                },
                audio_group: attributes.get("AUDIO").map(str::to_owned),
            });
        }
    }
    if variants.is_empty() {
        return Err(ManifestError::Empty);
    }
    Ok(MultivariantPlaylist {
        variants,
        audio_renditions,
    })
}

/// The key in effect for the segments that follow an `EXT-X-KEY` tag.
#[derive(Clone)]
enum ActiveKey {
    None,
    Aes128 { key_url: Url, iv: Option<[u8; 16]> },
}

fn parse_media(lines: &[&str], base: &Url) -> Result<MediaPlaylist, ManifestError> {
    let mut pieces = Vec::new();
    let mut init: Option<MediaPiece> = None;
    let mut key = ActiveKey::None;
    let mut media_sequence: u64 = 0;
    let mut segment_count: u64 = 0;
    let mut pending_range: Option<(u64, Option<u64>)> = None;
    // Where the previous byte-range segment ended, for ranges without an offset.
    let mut previous_range_end: Option<(Url, u64)> = None;
    let mut has_end = false;
    let mut is_vod = false;
    let mut saw_segment_tag = false;

    for line in lines {
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            media_sequence = value
                .trim()
                .parse()
                .map_err(|_| ManifestError::Malformed(format!("bad media sequence {value:?}")))?;
        } else if *line == "#EXT-X-ENDLIST" {
            has_end = true;
        } else if let Some(kind) = line.strip_prefix("#EXT-X-PLAYLIST-TYPE:") {
            is_vod = kind.trim() == "VOD";
        } else if let Some(list) = line.strip_prefix("#EXT-X-KEY:") {
            key = parse_key(&Attributes::parse(list), base)?;
        } else if let Some(list) = line.strip_prefix("#EXT-X-MAP:") {
            let attributes = Attributes::parse(list);
            let uri = attributes
                .get("URI")
                .ok_or_else(|| ManifestError::Malformed("EXT-X-MAP without URI".into()))?;
            let range = attributes
                .get("BYTERANGE")
                .map(parse_byte_range)
                .transpose()?
                .map(|(length, offset)| ByteRange {
                    start: offset.unwrap_or(0),
                    length,
                });
            let piece = MediaPiece {
                url: join(base, uri)?,
                range,
                encryption: PieceEncryption::Clear,
            };
            match &init {
                Some(existing) if *existing != piece => {
                    return Err(ManifestError::Unsupported(
                        "the playlist switches initialization segments mid-stream".into(),
                    ))
                }
                _ => init = Some(piece),
            }
        } else if let Some(value) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            pending_range = Some(parse_byte_range(value)?);
        } else if line.starts_with("#EXTINF") {
            saw_segment_tag = true;
        } else if line.starts_with('#') {
            continue;
        } else {
            let url = join(base, line)?;
            let range =
                match pending_range.take() {
                    None => None,
                    Some((length, Some(start))) => Some(ByteRange { start, length }),
                    Some((length, None)) => match &previous_range_end {
                        Some((previous_url, end)) if *previous_url == url => Some(ByteRange {
                            start: *end,
                            length,
                        }),
                        _ => return Err(ManifestError::Malformed(
                            "byte range without offset does not follow a range of the same file"
                                .into(),
                        )),
                    },
                };
            previous_range_end = range.map(|range| (url.clone(), range.start + range.length));
            let encryption = match &key {
                ActiveKey::None => PieceEncryption::Clear,
                ActiveKey::Aes128 { key_url, iv } => PieceEncryption::Aes128 {
                    key_url: key_url.clone(),
                    // Without an explicit IV, the segment's sequence number is the IV.
                    iv: iv.unwrap_or_else(|| {
                        (u128::from(media_sequence) + u128::from(segment_count)).to_be_bytes()
                    }),
                },
            };
            pieces.push(MediaPiece {
                url,
                range,
                encryption,
            });
            segment_count += 1;
        }
    }

    if !has_end && !is_vod {
        return Err(ManifestError::Live);
    }
    if pieces.is_empty() || !saw_segment_tag {
        return Err(ManifestError::Empty);
    }
    let container = match &init {
        Some(_) => SegmentContainer::Fmp4,
        None => container_from_extension(&pieces[0].url),
    };
    if let Some(init) = init {
        pieces.insert(0, init);
    }
    Ok(MediaPlaylist { container, pieces })
}

fn parse_key(attributes: &Attributes, base: &Url) -> Result<ActiveKey, ManifestError> {
    let method = attributes.get("METHOD").unwrap_or("NONE");
    let key_format = attributes.get("KEYFORMAT").unwrap_or("identity");
    match method {
        "NONE" => Ok(ActiveKey::None),
        "AES-128" if key_format == "identity" => {
            let uri = attributes
                .get("URI")
                .ok_or_else(|| ManifestError::Malformed("AES-128 key without URI".into()))?;
            let iv = attributes.get("IV").map(parse_iv).transpose()?;
            Ok(ActiveKey::Aes128 {
                key_url: join(base, uri)?,
                iv,
            })
        }
        // SAMPLE-AES and non-identity key formats are DRM systems
        // (FairPlay, Widevine, PlayReady) whose keys are never handed out.
        other => Err(ManifestError::Drm(format!(
            "HLS {other} with key format {key_format}"
        ))),
    }
}

fn parse_iv(value: &str) -> Result<[u8; 16], ManifestError> {
    let invalid = || ManifestError::Malformed(format!("bad IV {value:?}"));
    let hex = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
        .ok_or_else(invalid)?;
    let number = u128::from_str_radix(hex, 16).map_err(|_| invalid())?;
    Ok(number.to_be_bytes())
}

/// Parses `<length>[@<offset>]`.
fn parse_byte_range(value: &str) -> Result<(u64, Option<u64>), ManifestError> {
    let invalid = || ManifestError::Malformed(format!("bad byte range {value:?}"));
    let (length, offset) = match value.trim().split_once('@') {
        Some((length, offset)) => (length, Some(offset.parse().map_err(|_| invalid())?)),
        None => (value.trim(), None),
    };
    let length: u64 = length.parse().map_err(|_| invalid())?;
    if length == 0 {
        return Err(invalid());
    }
    Ok((length, offset))
}

fn parse_resolution(value: &str) -> Option<(u32, u32)> {
    let (width, height) = value.split_once(['x', 'X'])?;
    Some((width.parse().ok()?, height.parse().ok()?))
}

fn container_from_extension(url: &Url) -> SegmentContainer {
    let path = url.path().to_ascii_lowercase();
    let extension = path.rsplit_once('.').map_or("", |(_, extension)| extension);
    match extension {
        "mp4" | "m4s" | "m4v" | "m4a" | "cmfv" | "cmfa" => SegmentContainer::Fmp4,
        "aac" | "mp3" | "ac3" | "ec3" => SegmentContainer::Elementary,
        "webm" => SegmentContainer::WebM,
        _ => SegmentContainer::MpegTs,
    }
}

fn join(base: &Url, reference: &str) -> Result<Url, ManifestError> {
    base.join(reference)
        .map_err(|error| ManifestError::Malformed(format!("bad URL {reference:?}: {error}")))
}

/// An HLS attribute list: `KEY=value,KEY="quoted, value"`.
struct Attributes(Vec<(String, String)>);

impl Attributes {
    fn parse(list: &str) -> Self {
        let mut pairs = Vec::new();
        let mut rest = list.trim();
        while !rest.is_empty() {
            let Some((key, after_key)) = rest.split_once('=') else {
                break;
            };
            let (value, after_value) = match after_key.strip_prefix('"') {
                Some(quoted) => match quoted.split_once('"') {
                    Some((value, after)) => (value, after),
                    None => (quoted, ""),
                },
                None => after_key.split_once(',').unwrap_or((after_key, "")),
            };
            pairs.push((key.trim().to_ascii_uppercase(), value.to_owned()));
            rest = after_value.trim_start_matches(',').trim_start();
        }
        Self(pairs)
    }

    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Url {
        Url::parse("https://cdn.example/video/master.m3u8?token=abc").unwrap()
    }

    fn media(text: &str) -> MediaPlaylist {
        match parse(text, &base()).unwrap() {
            Playlist::Media(media) => media,
            other => panic!("expected a media playlist, got {other:?}"),
        }
    }

    #[test]
    fn parses_multivariant_with_separate_audio() {
        let text = "#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"English\",DEFAULT=YES,URI=\"audio/en.m3u8\"\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aud\",NAME=\"French\",URI=\"audio/fr.m3u8\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=5000000,RESOLUTION=1920x1080,CODECS=\"avc1.640028,mp4a.40.2\",AUDIO=\"aud\"\n\
            1080/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=1500000,RESOLUTION=854x480,AUDIO=\"aud\"\n\
            https://other.example/480.m3u8\n";
        let Playlist::Multivariant(playlist) = parse(text, &base()).unwrap() else {
            panic!("expected multivariant");
        };
        assert_eq!(playlist.variants.len(), 2);
        let first = &playlist.variants[0];
        assert_eq!(
            first.url.as_str(),
            "https://cdn.example/video/1080/index.m3u8"
        );
        assert_eq!(first.info.height, Some(1080));
        assert_eq!(first.info.codecs.as_deref(), Some("avc1.640028,mp4a.40.2"));
        let audio = playlist.audio_for(first).unwrap();
        assert_eq!(audio.name.as_deref(), Some("English"));
        assert_eq!(
            audio.url.as_ref().unwrap().as_str(),
            "https://cdn.example/video/audio/en.m3u8"
        );
    }

    #[test]
    fn audio_without_uri_is_interleaved() {
        let text = "#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"main\",DEFAULT=YES\n\
            #EXT-X-STREAM-INF:BANDWIDTH=1,AUDIO=\"a\"\nv.m3u8\n";
        let Playlist::Multivariant(playlist) = parse(text, &base()).unwrap() else {
            panic!("expected multivariant");
        };
        assert!(playlist.audio_for(&playlist.variants[0]).is_none());
    }

    #[test]
    fn parses_transport_stream_segments() {
        let playlist = media(
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6.0,\nseg0.ts?sig=1\n#EXTINF:4.5,\nseg1.ts\n#EXT-X-ENDLIST\n",
        );
        assert_eq!(playlist.container, SegmentContainer::MpegTs);
        assert_eq!(playlist.pieces.len(), 2);
        assert_eq!(
            playlist.pieces[0].url.as_str(),
            "https://cdn.example/video/seg0.ts?sig=1"
        );
        assert_eq!(playlist.pieces[1].encryption, PieceEncryption::Clear);
    }

    #[test]
    fn init_map_comes_first_and_marks_fmp4() {
        let playlist = media(
            "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\n1.m4s\n#EXTINF:4,\n2.m4s\n",
        );
        assert_eq!(playlist.container, SegmentContainer::Fmp4);
        assert_eq!(playlist.pieces.len(), 3);
        assert!(playlist.pieces[0].url.path().ends_with("init.mp4"));
    }

    #[test]
    fn aes_key_iv_defaults_to_sequence_number() {
        let playlist = media(
            "#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:7\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:2,\na.ts\n#EXTINF:2,\nb.ts\n\
             #EXT-X-KEY:METHOD=AES-128,URI=\"k2.bin\",IV=0x000000000000000000000000000000FF\n#EXTINF:2,\nc.ts\n\
             #EXT-X-KEY:METHOD=NONE\n#EXTINF:2,\nd.ts\n#EXT-X-ENDLIST\n",
        );
        let ivs: Vec<_> = playlist
            .pieces
            .iter()
            .map(|piece| match &piece.encryption {
                PieceEncryption::Aes128 { iv, .. } => Some(u128::from_be_bytes(*iv)),
                PieceEncryption::Clear => None,
            })
            .collect();
        assert_eq!(ivs, vec![Some(7), Some(8), Some(255), None]);
    }

    #[test]
    fn byte_ranges_continue_from_previous_segment() {
        let playlist = media(
            "#EXTM3U\n#EXTINF:2,\n#EXT-X-BYTERANGE:100@0\nall.ts\n#EXTINF:2,\n#EXT-X-BYTERANGE:50\nall.ts\n#EXT-X-ENDLIST\n",
        );
        let ranges: Vec<_> = playlist
            .pieces
            .iter()
            .map(|piece| piece.range.unwrap())
            .collect();
        assert_eq!(
            ranges[0],
            ByteRange {
                start: 0,
                length: 100
            }
        );
        assert_eq!(
            ranges[1],
            ByteRange {
                start: 100,
                length: 50
            }
        );
    }

    #[test]
    fn rejects_drm_live_and_non_playlists() {
        let drm = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://x\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:2,\na.ts\n#EXT-X-ENDLIST\n";
        assert!(matches!(parse(drm, &base()), Err(ManifestError::Drm(_))));
        let live = "#EXTM3U\n#EXTINF:2,\na.ts\n";
        assert!(matches!(parse(live, &base()), Err(ManifestError::Live)));
        assert!(matches!(
            parse("<html>", &base()),
            Err(ManifestError::NotHls)
        ));
    }

    #[test]
    fn attribute_values_may_contain_commas() {
        let attributes = Attributes::parse("CODECS=\"avc1,mp4a\",BANDWIDTH=10, NAME=\"x\"");
        assert_eq!(attributes.get("CODECS"), Some("avc1,mp4a"));
        assert_eq!(attributes.get("BANDWIDTH"), Some("10"));
        assert_eq!(attributes.get("NAME"), Some("x"));
    }
}
