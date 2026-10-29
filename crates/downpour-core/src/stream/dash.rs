//! Parses static MPEG-DASH manifests (ISO/IEC 23009-1) into representations
//! and the media pieces each one consists of.

use roxmltree::{Document, Node};
use url::Url;

use super::plan::{ByteRange, MediaPiece, SegmentContainer, TrackRole, VariantInfo};
use super::ManifestError;

/// Codec prefixes that mean a representation carries audio.
const AUDIO_CODECS: [&str; 7] = ["mp4a", "opus", "ac-3", "ec-3", "vorbis", "flac", "mp3"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashManifest {
    /// Representations carrying video (possibly with interleaved audio).
    pub video: Vec<Representation>,
    pub audio: Vec<Representation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Representation {
    pub info: VariantInfo,
    pub role: TrackRole,
    pub container: SegmentContainer,
    /// The initialization piece first (if any), then every media segment.
    pub pieces: Vec<MediaPiece>,
}

pub fn parse(text: &str, manifest_url: &Url) -> Result<DashManifest, ManifestError> {
    let document =
        Document::parse(text.trim_start_matches('\u{feff}')).map_err(|_| ManifestError::NotDash)?;
    let mpd = document.root_element();
    if mpd.tag_name().name() != "MPD" {
        return Err(ManifestError::NotDash);
    }
    if mpd.attribute("type") == Some("dynamic") {
        return Err(ManifestError::Live);
    }
    let periods: Vec<Node> = children(mpd, "Period").collect();
    let period = match periods.as_slice() {
        [] => return Err(ManifestError::Empty),
        [period] => *period,
        _ => {
            return Err(ManifestError::Unsupported(
                "manifests with several periods (often ad breaks)".into(),
            ))
        }
    };
    let period_seconds = period_duration(mpd, period)?;
    let base = resolve_base(&resolve_base(manifest_url, mpd)?, period)?;
    let period_layout = SegmentLayout::default().inherit(period);

    let mut manifest = DashManifest {
        video: Vec::new(),
        audio: Vec::new(),
    };
    let mut skipped_protected = false;
    for set in children(period, "AdaptationSet") {
        let set_base = resolve_base(&base, set)?;
        let set_layout = period_layout.clone().inherit(set);
        let set_protected = children(set, "ContentProtection").next().is_some();
        for representation in children(set, "Representation") {
            if set_protected
                || children(representation, "ContentProtection")
                    .next()
                    .is_some()
            {
                skipped_protected = true;
                continue;
            }
            let Some(role) = media_role(set, representation) else {
                continue; // subtitles, thumbnails
            };
            let parsed = Representation {
                info: VariantInfo {
                    width: inherited(set, representation, "width").and_then(|v| v.parse().ok()),
                    height: inherited(set, representation, "height").and_then(|v| v.parse().ok()),
                    bandwidth: representation
                        .attribute("bandwidth")
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0),
                    codecs: inherited(set, representation, "codecs").map(str::to_owned),
                },
                role,
                container: container_of(inherited(set, representation, "mimeType")),
                pieces: set_layout.clone().inherit(representation).pieces(
                    &resolve_base(&set_base, representation)?,
                    &TemplateValues::of(representation),
                    period_seconds,
                )?,
            };
            match role {
                TrackRole::Audio => manifest.audio.push(parsed),
                TrackRole::Video | TrackRole::Combined => manifest.video.push(parsed),
            }
        }
    }
    if manifest.video.is_empty() && manifest.audio.is_empty() {
        return Err(if skipped_protected {
            ManifestError::Drm("DASH ContentProtection".into())
        } else {
            ManifestError::Empty
        });
    }
    Ok(manifest)
}

fn children<'a, 'input>(
    node: Node<'a, 'input>,
    name: &'static str,
) -> impl Iterator<Item = Node<'a, 'input>> {
    node.children()
        .filter(move |child| child.is_element() && child.tag_name().name() == name)
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &'static str) -> Option<Node<'a, 'input>> {
    children(node, name).next()
}

/// An attribute set on the representation or, failing that, its adaptation set.
fn inherited<'a>(set: Node<'a, '_>, representation: Node<'a, '_>, name: &str) -> Option<&'a str> {
    representation
        .attribute(name)
        .or_else(|| set.attribute(name))
}

fn resolve_base(parent: &Url, node: Node) -> Result<Url, ManifestError> {
    let Some(base) = child(node, "BaseURL").and_then(|base| base.text()) else {
        return Ok(parent.clone());
    };
    parent
        .join(base.trim())
        .map_err(|error| ManifestError::Malformed(format!("bad BaseURL {base:?}: {error}")))
}

fn media_role(set: Node, representation: Node) -> Option<TrackRole> {
    let content_type = set.attribute("contentType").unwrap_or_default();
    let mime = inherited(set, representation, "mimeType").unwrap_or_default();
    let codecs = inherited(set, representation, "codecs").unwrap_or_default();
    let has_audio_codec = codecs.split(',').any(|codec| {
        AUDIO_CODECS
            .iter()
            .any(|prefix| codec.trim().starts_with(prefix))
    });
    let is_video = content_type == "video" || mime.starts_with("video/");
    let is_audio = content_type == "audio" || mime.starts_with("audio/");
    match (is_video, is_audio) {
        (true, _) if has_audio_codec && codecs.contains(',') => Some(TrackRole::Combined),
        (true, _) => Some(TrackRole::Video),
        (false, true) => Some(TrackRole::Audio),
        (false, false) => None,
    }
}

fn container_of(mime: Option<&str>) -> SegmentContainer {
    match mime.unwrap_or_default() {
        "video/webm" | "audio/webm" => SegmentContainer::WebM,
        "video/mp2t" => SegmentContainer::MpegTs,
        _ => SegmentContainer::Fmp4,
    }
}

fn period_duration(mpd: Node, period: Node) -> Result<Option<f64>, ManifestError> {
    if let Some(duration) = period.attribute("duration") {
        return parse_duration(duration).map(Some);
    }
    let Some(total) = mpd.attribute("mediaPresentationDuration") else {
        return Ok(None);
    };
    let start = period.attribute("start").map(parse_duration).transpose()?;
    Ok(Some(parse_duration(total)? - start.unwrap_or(0.0)))
}

/// Parses an `xs:duration` such as `PT1H2M3.5S` into seconds.
pub fn parse_duration(text: &str) -> Result<f64, ManifestError> {
    let invalid = || ManifestError::Malformed(format!("bad duration {text:?}"));
    let rest = text.trim().strip_prefix('P').ok_or_else(invalid)?;
    let (date, time) = rest.split_once('T').unwrap_or((rest, ""));
    let mut seconds = 0.0;
    let mut accumulate = |part: &str, units: &[(char, f64)]| -> Result<(), ManifestError> {
        let mut number = String::new();
        for c in part.chars() {
            if c.is_ascii_digit() || c == '.' {
                number.push(c);
                continue;
            }
            let scale = units
                .iter()
                .find(|(unit, _)| *unit == c)
                .map(|(_, scale)| *scale)
                .ok_or_else(invalid)?;
            seconds += number.parse::<f64>().map_err(|_| invalid())? * scale;
            number.clear();
        }
        if number.is_empty() {
            Ok(())
        } else {
            Err(invalid())
        }
    };
    // Years and months have no fixed length; real manifests leave them at 0.
    accumulate(date, &[('Y', 0.0), ('M', 0.0), ('D', 86_400.0)])?;
    accumulate(time, &[('H', 3_600.0), ('M', 60.0), ('S', 1.0)])?;
    Ok(seconds)
}

/// Segment addressing collected down the Period → AdaptationSet →
/// Representation hierarchy; values set lower override those set higher.
#[derive(Clone, Default)]
struct SegmentLayout<'a, 'input> {
    template: Option<TemplateSpec<'a, 'input>>,
    list: Option<Node<'a, 'input>>,
}

#[derive(Clone, Default)]
struct TemplateSpec<'a, 'input> {
    media: Option<String>,
    initialization: Option<String>,
    start_number: Option<u64>,
    timescale: Option<u64>,
    duration: Option<u64>,
    timeline: Option<Node<'a, 'input>>,
}

impl<'a, 'input> SegmentLayout<'a, 'input> {
    fn inherit(mut self, node: Node<'a, 'input>) -> Self {
        if let Some(template) = child(node, "SegmentTemplate") {
            let mut spec = self.template.take().unwrap_or_default();
            let number = |name| template.attribute(name).and_then(|v: &str| v.parse().ok());
            if let Some(media) = template.attribute("media") {
                spec.media = Some(media.to_owned());
            }
            if let Some(initialization) = template.attribute("initialization") {
                spec.initialization = Some(initialization.to_owned());
            }
            spec.start_number = number("startNumber").or(spec.start_number);
            spec.timescale = number("timescale").or(spec.timescale);
            spec.duration = number("duration").or(spec.duration);
            spec.timeline = child(template, "SegmentTimeline").or(spec.timeline);
            self.template = Some(spec);
        }
        if let Some(list) = child(node, "SegmentList") {
            self.list = Some(list);
        }
        self
    }

    fn pieces(
        self,
        base: &Url,
        values: &TemplateValues,
        period_seconds: Option<f64>,
    ) -> Result<Vec<MediaPiece>, ManifestError> {
        if let Some(template) = self.template {
            return template.pieces(base, values, period_seconds);
        }
        if let Some(list) = self.list {
            return list_pieces(list, base);
        }
        // SegmentBase or a bare BaseURL: the whole representation is one file.
        Ok(vec![MediaPiece::clear(base.clone())])
    }
}

impl TemplateSpec<'_, '_> {
    fn pieces(
        self,
        base: &Url,
        values: &TemplateValues,
        period_seconds: Option<f64>,
    ) -> Result<Vec<MediaPiece>, ManifestError> {
        let media = self
            .media
            .as_deref()
            .ok_or_else(|| ManifestError::Malformed("SegmentTemplate without media".into()))?;
        let start_number = self.start_number.unwrap_or(1);
        let timescale = self.timescale.unwrap_or(1).max(1);
        let mut pieces = Vec::new();
        if let Some(initialization) = &self.initialization {
            pieces.push(MediaPiece::clear(join(
                base,
                &values.expand(initialization, None)?,
            )?));
        }
        let addresses = match (self.timeline, self.duration) {
            (Some(timeline), _) => timeline_addresses(timeline, timescale, period_seconds)?,
            (None, Some(duration)) if duration > 0 => {
                let seconds = period_seconds.ok_or_else(|| {
                    ManifestError::Malformed(
                        "SegmentTemplate duration without a period length".into(),
                    )
                })?;
                let count = (seconds * timescale as f64 / duration as f64).ceil() as u64;
                (0..count).map(|index| index * duration).collect()
            }
            _ => {
                return Err(ManifestError::Malformed(
                    "SegmentTemplate needs a duration or a SegmentTimeline".into(),
                ))
            }
        };
        for (index, time) in addresses.into_iter().enumerate() {
            let address = SegmentAddress {
                number: start_number + index as u64,
                time,
            };
            pieces.push(MediaPiece::clear(join(
                base,
                &values.expand(media, Some(address))?,
            )?));
        }
        Ok(pieces)
    }
}

/// Start times of every segment a `SegmentTimeline` describes.
fn timeline_addresses(
    timeline: Node,
    timescale: u64,
    period_seconds: Option<f64>,
) -> Result<Vec<u64>, ManifestError> {
    let entries: Vec<Node> = children(timeline, "S").collect();
    let number = |node: Node, name: &str| -> Result<Option<i64>, ManifestError> {
        node.attribute(name)
            .map(|value| {
                value
                    .parse()
                    .map_err(|_| ManifestError::Malformed(format!("bad S@{name} {value:?}")))
            })
            .transpose()
    };
    let period_end = period_seconds.map(|seconds| (seconds * timescale as f64).round() as u64);
    let mut times = Vec::new();
    let mut time: u64 = 0;
    for (index, entry) in entries.iter().enumerate() {
        if let Some(start) = number(*entry, "t")? {
            time = u64::try_from(start)
                .map_err(|_| ManifestError::Malformed("negative S@t".into()))?;
        }
        let duration = number(*entry, "d")?
            .and_then(|d| u64::try_from(d).ok())
            .filter(|d| *d > 0)
            .ok_or_else(|| ManifestError::Malformed("S without a positive duration".into()))?;
        let repeat = number(*entry, "r")?.unwrap_or(0);
        let next_start = match entries.get(index + 1) {
            Some(next) => number(*next, "t")?.and_then(|t| u64::try_from(t).ok()),
            None => None,
        };
        // r = -1 repeats until the next entry's start or the end of the period.
        let count = if repeat < 0 {
            let end = next_start.or(period_end).ok_or_else(|| {
                ManifestError::Malformed("open-ended SegmentTimeline repeat without an end".into())
            })?;
            end.saturating_sub(time).div_ceil(duration)
        } else {
            repeat as u64 + 1
        };
        for _ in 0..count {
            times.push(time);
            time += duration;
        }
    }
    Ok(times)
}

fn list_pieces(list: Node, base: &Url) -> Result<Vec<MediaPiece>, ManifestError> {
    let mut pieces = Vec::new();
    if let Some(initialization) = child(list, "Initialization") {
        pieces.push(listed_piece(
            base,
            initialization.attribute("sourceURL"),
            initialization.attribute("range"),
        )?);
    }
    for segment in children(list, "SegmentURL") {
        pieces.push(listed_piece(
            base,
            segment.attribute("media"),
            segment.attribute("mediaRange"),
        )?);
    }
    if pieces.is_empty() {
        return Err(ManifestError::Empty);
    }
    Ok(pieces)
}

fn listed_piece(
    base: &Url,
    url: Option<&str>,
    range: Option<&str>,
) -> Result<MediaPiece, ManifestError> {
    let url = match url {
        Some(url) => join(base, url)?,
        None => base.clone(),
    };
    let range = range.map(parse_inclusive_range).transpose()?;
    Ok(MediaPiece {
        url,
        range,
        ..MediaPiece::clear(base.clone())
    })
}

/// Parses `first-last` (inclusive), as in `mediaRange="0-999"`.
fn parse_inclusive_range(text: &str) -> Result<ByteRange, ManifestError> {
    let invalid = || ManifestError::Malformed(format!("bad byte range {text:?}"));
    let (first, last) = text.split_once('-').ok_or_else(invalid)?;
    let first: u64 = first.trim().parse().map_err(|_| invalid())?;
    let last: u64 = last.trim().parse().map_err(|_| invalid())?;
    if last < first {
        return Err(invalid());
    }
    Ok(ByteRange {
        start: first,
        length: last - first + 1,
    })
}

fn join(base: &Url, reference: &str) -> Result<Url, ManifestError> {
    base.join(reference)
        .map_err(|error| ManifestError::Malformed(format!("bad URL {reference:?}: {error}")))
}

#[derive(Clone, Copy)]
struct SegmentAddress {
    number: u64,
    time: u64,
}

/// Values substituted into `$Identifier$` placeholders.
struct TemplateValues {
    representation_id: String,
    bandwidth: u64,
}

impl TemplateValues {
    fn of(representation: Node) -> Self {
        Self {
            representation_id: representation
                .attribute("id")
                .unwrap_or_default()
                .to_owned(),
            bandwidth: representation
                .attribute("bandwidth")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0),
        }
    }

    /// Expands `$RepresentationID$`, `$Bandwidth$`, `$Number$`, `$Time$`
    /// (each numeric one optionally with a `%0Nd` width) and `$$`.
    fn expand(
        &self,
        template: &str,
        address: Option<SegmentAddress>,
    ) -> Result<String, ManifestError> {
        let invalid =
            |reason: &str| ManifestError::Malformed(format!("template {template:?}: {reason}"));
        let mut output = String::with_capacity(template.len() + 16);
        let mut rest = template;
        while let Some(start) = rest.find('$') {
            output.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            let end = after.find('$').ok_or_else(|| invalid("unclosed $"))?;
            let placeholder = &after[..end];
            rest = &after[end + 1..];
            if placeholder.is_empty() {
                output.push('$');
                continue;
            }
            let (name, format) = match placeholder.split_once('%') {
                Some((name, format)) => (name, Some(format)),
                None => (placeholder, None),
            };
            let number = match name {
                "RepresentationID" => {
                    output.push_str(&self.representation_id);
                    continue;
                }
                "Bandwidth" => self.bandwidth,
                "Number" => {
                    address
                        .ok_or_else(|| invalid("$Number$ outside media"))?
                        .number
                }
                "Time" => address.ok_or_else(|| invalid("$Time$ outside media"))?.time,
                _ => return Err(invalid("unknown placeholder")),
            };
            let width = match format {
                None => 0,
                Some(format) => format
                    .strip_prefix('0')
                    .unwrap_or(format)
                    .strip_suffix('d')
                    .and_then(|digits| digits.parse().ok())
                    .ok_or_else(|| invalid("unsupported number format"))?,
            };
            output.push_str(&format!("{number:0width$}"));
        }
        output.push_str(rest);
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_url() -> Url {
        Url::parse("https://cdn.example/scene/manifest.mpd?token=t").unwrap()
    }

    const TEMPLATE_MPD: &str = r#"<?xml version="1.0"?>
<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT0H0M10.5S">
  <Period>
    <AdaptationSet mimeType="video/mp4" contentType="video">
      <SegmentTemplate timescale="1000" duration="4000" startNumber="1"
        initialization="$RepresentationID$/init.mp4" media="$RepresentationID$/seg-$Number%05d$.m4s"/>
      <Representation id="v1080" bandwidth="5000000" width="1920" height="1080" codecs="avc1.640028"/>
      <Representation id="v720" bandwidth="2500000" width="1280" height="720" codecs="avc1.4d401f"/>
    </AdaptationSet>
    <AdaptationSet mimeType="audio/mp4" lang="en">
      <Representation id="a128" bandwidth="128000" codecs="mp4a.40.2">
        <BaseURL>audio/</BaseURL>
        <SegmentTemplate timescale="48000" media="chunk-$Time$.m4s" initialization="init.m4a">
          <SegmentTimeline><S t="0" d="96000" r="2"/><S d="48000"/></SegmentTimeline>
        </SegmentTemplate>
      </Representation>
    </AdaptationSet>
    <AdaptationSet mimeType="text/vtt"><Representation id="subs" bandwidth="1"/></AdaptationSet>
  </Period>
</MPD>"#;

    #[test]
    fn expands_number_templates_from_duration() {
        let manifest = parse(TEMPLATE_MPD, &manifest_url()).unwrap();
        assert_eq!(manifest.video.len(), 2);
        let video = &manifest.video[0];
        assert_eq!(video.info.height, Some(1080));
        assert_eq!(video.role, TrackRole::Video);
        // ceil(10.5 s / 4 s) = 3 segments plus the initialization piece.
        let urls: Vec<&str> = video
            .pieces
            .iter()
            .map(|piece| piece.url.as_str())
            .collect();
        assert_eq!(
            urls,
            [
                "https://cdn.example/scene/v1080/init.mp4",
                "https://cdn.example/scene/v1080/seg-00001.m4s",
                "https://cdn.example/scene/v1080/seg-00002.m4s",
                "https://cdn.example/scene/v1080/seg-00003.m4s",
            ]
        );
    }

    #[test]
    fn expands_timeline_with_repeats_and_representation_base() {
        let manifest = parse(TEMPLATE_MPD, &manifest_url()).unwrap();
        let audio = &manifest.audio[0];
        let urls: Vec<&str> = audio
            .pieces
            .iter()
            .map(|piece| piece.url.as_str())
            .collect();
        assert_eq!(
            urls,
            [
                "https://cdn.example/scene/audio/init.m4a",
                "https://cdn.example/scene/audio/chunk-0.m4s",
                "https://cdn.example/scene/audio/chunk-96000.m4s",
                "https://cdn.example/scene/audio/chunk-192000.m4s",
                "https://cdn.example/scene/audio/chunk-288000.m4s",
            ]
        );
    }

    #[test]
    fn open_ended_repeat_runs_to_period_end() {
        let mpd = r#"<MPD mediaPresentationDuration="PT10S"><Period><AdaptationSet mimeType="video/mp4">
            <SegmentTemplate timescale="10" media="s$Number$.m4s">
              <SegmentTimeline><S t="0" d="40" r="-1"/></SegmentTimeline>
            </SegmentTemplate>
            <Representation id="v" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        let manifest = parse(mpd, &manifest_url()).unwrap();
        assert_eq!(manifest.video[0].pieces.len(), 3); // 0, 4 and 8 seconds
    }

    #[test]
    fn segment_list_with_ranges() {
        let mpd = r#"<MPD mediaPresentationDuration="PT8S"><Period>
          <AdaptationSet mimeType="video/mp4"><Representation id="v" bandwidth="1">
            <BaseURL>video.mp4</BaseURL>
            <SegmentList><Initialization range="0-799"/>
              <SegmentURL mediaRange="800-1799"/><SegmentURL mediaRange="1800-2499"/></SegmentList>
          </Representation></AdaptationSet></Period></MPD>"#;
        let manifest = parse(mpd, &manifest_url()).unwrap();
        let pieces = &manifest.video[0].pieces;
        assert_eq!(pieces.len(), 3);
        assert_eq!(
            pieces[1].url.as_str(),
            "https://cdn.example/scene/video.mp4"
        );
        assert_eq!(
            pieces[2].range,
            Some(ByteRange {
                start: 1800,
                length: 700
            })
        );
    }

    #[test]
    fn single_file_representation_and_combined_codecs() {
        let mpd = r#"<MPD mediaPresentationDuration="PT1M"><Period><AdaptationSet>
            <Representation id="m" mimeType="video/mp4" codecs="avc1.4d401e,mp4a.40.2" bandwidth="900000">
              <BaseURL>https://media.example/full.mp4</BaseURL><SegmentBase indexRange="0-100"/>
            </Representation></AdaptationSet></Period></MPD>"#;
        let manifest = parse(mpd, &manifest_url()).unwrap();
        let video = &manifest.video[0];
        assert_eq!(video.role, TrackRole::Combined);
        assert_eq!(
            video.pieces,
            vec![MediaPiece::clear(
                Url::parse("https://media.example/full.mp4").unwrap()
            )]
        );
    }

    #[test]
    fn rejects_drm_live_and_other_documents() {
        let protected = r#"<MPD mediaPresentationDuration="PT1M"><Period><AdaptationSet mimeType="video/mp4">
            <ContentProtection schemeIdUri="urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed"/>
            <Representation id="v" bandwidth="1"/></AdaptationSet></Period></MPD>"#;
        assert!(matches!(
            parse(protected, &manifest_url()),
            Err(ManifestError::Drm(_))
        ));
        let live = r#"<MPD type="dynamic"><Period/></MPD>"#;
        assert!(matches!(
            parse(live, &manifest_url()),
            Err(ManifestError::Live)
        ));
        assert!(matches!(
            parse("#EXTM3U", &manifest_url()),
            Err(ManifestError::NotDash)
        ));
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("PT1H2M3.5S").unwrap(), 3723.5);
        assert_eq!(parse_duration("P0Y0M0DT0H3M30.000S").unwrap(), 210.0);
        assert_eq!(parse_duration("P1DT1S").unwrap(), 86_401.0);
        assert!(parse_duration("1H").is_err());
    }
}
