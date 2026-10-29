//! Fetches a manifest (and the playlists it points to) and turns it into a
//! [`StreamPlan`] for the chosen quality.

use futures_util::StreamExt;
use reqwest::header::{self, HeaderValue};
use reqwest::{Client, StatusCode};
use url::Url;

use super::dash::{self, DashManifest, Representation};
use super::hls::{self, MultivariantPlaylist, Playlist};
use super::plan::{StreamFormat, StreamOffer, StreamPlan, TrackPlan, TrackRole, VideoQuality};
use super::ManifestError;
use crate::http::{ScopedContext, TransferError};

/// Manifests are text; anything bigger is not one.
const MAX_MANIFEST_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum StreamError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("cannot fetch the {what}: {source}")]
    Fetch {
        what: &'static str,
        source: TransferError,
    },
    #[error(
        "the server refused the {what} ({status}). Stream addresses expire after a while: \
         play the video in the browser again and send it to Downpour"
    )]
    Refused {
        what: &'static str,
        status: StatusCode,
    },
    #[error(
        "the server sent a web page instead of the {what}; the address may have expired or need a login"
    )]
    WebPage { what: &'static str },
    #[error("the {what} is larger than {MAX_MANIFEST_BYTES} bytes, so it is not a manifest")]
    TooLarge { what: &'static str },
    #[error("the address is neither an HLS playlist nor a DASH manifest")]
    UnknownFormat,
}

/// A manifest fetched and parsed, before choosing a quality.
enum Manifest {
    Hls(Playlist),
    Dash(DashManifest),
}

/// Describes the qualities a stream offers, for the user to choose from.
pub async fn inspect(
    client: &Client,
    url: &Url,
    context: &ScopedContext,
) -> Result<StreamOffer, StreamError> {
    Ok(match fetch_manifest(client, url, context).await? {
        Manifest::Hls(Playlist::Multivariant(playlist)) => StreamOffer {
            format: StreamFormat::Hls,
            separate_audio: playlist
                .variants
                .iter()
                .any(|variant| playlist.audio_for(variant).is_some()),
            variants: playlist
                .variants
                .into_iter()
                .map(|variant| variant.info)
                .collect(),
        },
        Manifest::Hls(Playlist::Media(_)) => StreamOffer {
            format: StreamFormat::Hls,
            variants: Vec::new(),
            separate_audio: false,
        },
        Manifest::Dash(manifest) => StreamOffer {
            format: StreamFormat::Dash,
            separate_audio: dash_needs_audio_track(&manifest),
            variants: manifest.video.into_iter().map(|video| video.info).collect(),
        },
    })
}

/// Resolves the manifest at `url` into the tracks to download.
pub async fn resolve(
    client: &Client,
    url: &Url,
    context: &ScopedContext,
    quality: VideoQuality,
) -> Result<StreamPlan, StreamError> {
    match fetch_manifest(client, url, context).await? {
        Manifest::Hls(Playlist::Media(media)) => Ok(StreamPlan {
            format: StreamFormat::Hls,
            tracks: vec![TrackPlan {
                role: TrackRole::Combined,
                container: media.container,
                pieces: media.pieces,
            }],
            variant: None,
        }),
        Manifest::Hls(Playlist::Multivariant(playlist)) => {
            resolve_multivariant(client, context, playlist, quality).await
        }
        Manifest::Dash(manifest) => resolve_dash(manifest, quality),
    }
}

async fn resolve_multivariant(
    client: &Client,
    context: &ScopedContext,
    playlist: MultivariantPlaylist,
    quality: VideoQuality,
) -> Result<StreamPlan, StreamError> {
    let infos: Vec<_> = playlist
        .variants
        .iter()
        .map(|variant| variant.info.clone())
        .collect();
    let chosen = quality.pick(&infos).ok_or(ManifestError::Empty)?;
    let variant = &playlist.variants[chosen];
    let video = fetch_media_playlist(client, &variant.url, context, "video playlist").await?;
    let mut tracks = Vec::with_capacity(2);
    match playlist
        .audio_for(variant)
        .and_then(|rendition| rendition.url.as_ref())
    {
        Some(audio_url) => {
            let audio = fetch_media_playlist(client, audio_url, context, "audio playlist").await?;
            tracks.push(TrackPlan {
                role: TrackRole::Video,
                container: video.container,
                pieces: video.pieces,
            });
            tracks.push(TrackPlan {
                role: TrackRole::Audio,
                container: audio.container,
                pieces: audio.pieces,
            });
        }
        None => tracks.push(TrackPlan {
            role: TrackRole::Combined,
            container: video.container,
            pieces: video.pieces,
        }),
    }
    Ok(StreamPlan {
        format: StreamFormat::Hls,
        tracks,
        variant: Some(variant.info.clone()),
    })
}

async fn fetch_media_playlist(
    client: &Client,
    url: &Url,
    context: &ScopedContext,
    what: &'static str,
) -> Result<hls::MediaPlaylist, StreamError> {
    let (text, final_url) = fetch_text(client, url, context, what).await?;
    match hls::parse(&text, &final_url)? {
        Playlist::Media(media) => Ok(media),
        Playlist::Multivariant(_) => Err(ManifestError::Malformed(format!(
            "the {what} lists qualities instead of segments"
        ))
        .into()),
    }
}

/// Audio needs its own track unless the chosen video already carries it.
fn dash_needs_audio_track(manifest: &DashManifest) -> bool {
    !manifest.audio.is_empty()
        && manifest
            .video
            .iter()
            .any(|video| video.role == TrackRole::Video)
}

fn resolve_dash(manifest: DashManifest, quality: VideoQuality) -> Result<StreamPlan, StreamError> {
    let infos: Vec<_> = manifest
        .video
        .iter()
        .map(|video| video.info.clone())
        .collect();
    let video = quality.pick(&infos).map(|index| &manifest.video[index]);
    let audio = manifest
        .audio
        .iter()
        .max_by_key(|audio| audio.info.bandwidth);
    let track = |representation: &Representation| TrackPlan {
        role: representation.role,
        container: representation.container,
        pieces: representation.pieces.clone(),
    };
    let tracks = match (video, audio) {
        (Some(video), Some(audio)) if video.role == TrackRole::Video => {
            vec![track(video), track(audio)]
        }
        (Some(video), _) => vec![track(video)],
        (None, Some(audio)) => vec![track(audio)],
        (None, None) => return Err(ManifestError::Empty.into()),
    };
    Ok(StreamPlan {
        format: StreamFormat::Dash,
        tracks,
        variant: video.map(|video| video.info.clone()),
    })
}

async fn fetch_manifest(
    client: &Client,
    url: &Url,
    context: &ScopedContext,
) -> Result<Manifest, StreamError> {
    let (text, final_url) = fetch_text(client, url, context, "manifest").await?;
    let start = text.trim_start_matches('\u{feff}').trim_start();
    if start.starts_with("#EXTM3U") {
        return Ok(Manifest::Hls(hls::parse(&text, &final_url)?));
    }
    if start.starts_with('<') && text.contains("<MPD") {
        return Ok(Manifest::Dash(dash::parse(&text, &final_url)?));
    }
    if looks_like_web_page(start) {
        return Err(StreamError::WebPage { what: "manifest" });
    }
    Err(StreamError::UnknownFormat)
}

fn looks_like_web_page(text: &str) -> bool {
    let head: String = text
        .chars()
        .take(64)
        .collect::<String>()
        .to_ascii_lowercase();
    head.starts_with("<!doctype html") || head.starts_with("<html")
}

/// GETs a text resource and returns it with the URL it was finally served
/// from, which relative references resolve against.
async fn fetch_text(
    client: &Client,
    url: &Url,
    context: &ScopedContext,
    what: &'static str,
) -> Result<(String, Url), StreamError> {
    let mut headers = context.headers_for(url);
    headers.insert(header::ACCEPT, HeaderValue::from_static("*/*"));
    let response = client
        .get(url.clone())
        .headers(headers)
        .send()
        .await
        .map_err(|error| StreamError::Fetch {
            what,
            source: error.into(),
        })?;
    let status = response.status();
    if matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::NOT_FOUND | StatusCode::GONE
    ) {
        return Err(StreamError::Refused { what, status });
    }
    if !status.is_success() {
        return Err(StreamError::Fetch {
            what,
            source: TransferError::HttpStatus(status),
        });
    }
    let final_url = response.url().clone();
    let mut body = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.map_err(|error| StreamError::Fetch {
            what,
            source: error.into(),
        })?;
        if body.len() + chunk.len() > MAX_MANIFEST_BYTES {
            return Err(StreamError::TooLarge { what });
        }
        body.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&body).into_owned();
    if looks_like_web_page(text.trim_start()) {
        return Err(StreamError::WebPage { what });
    }
    Ok((text, final_url))
}
