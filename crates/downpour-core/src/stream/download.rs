//! Downloads a resolved video stream. Pieces are fetched several at a time
//! but written strictly in order, one file per track, so the saved progress
//! is always "the first N pieces". When every piece is on disk the tracks
//! are joined into the target.

use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::{header, Client, Response, StatusCode};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use url::Url;

use super::crypto::{decrypt_segment, DecryptError, KEY_BYTES};
use super::mux::{self, FfmpegRequired, MergeError, OutputFormat};
use super::plan::{MediaPiece, PieceEncryption, StreamPlan, TrackPlan, VideoQuality};
use super::resolve::{self, StreamError};
use crate::disk::{free_space, is_disk_full, NoSpace};
use crate::http::{url_for_logs, ScopedContext, TransferError, Validators};
use crate::limiter::Throttle;
use crate::naming::FileName;
use crate::record::{part_path, track_path, TrackProgress, TransferProgress};
use crate::speed::SpeedMeter;
use crate::transfer::{
    check_space, directory_of, finalize, reserve_target, write_at_blocking, Events, TransferEvent,
    TransferJob, TransferOutcome, TransferServices,
};

const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const MAX_PIECE_ATTEMPTS: u32 = 5;
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(1);
const FLUSH_THRESHOLD_BYTES: usize = 512 * 1024;
/// Pieces can finish out of order; at most this many per connection wait in
/// memory for an earlier piece, which bounds memory use.
const READ_AHEAD_PER_CONNECTION: usize = 2;
/// Extensions a stream's name may carry from its URL or the chosen output.
const REPLACED_EXTENSIONS: [&str; 9] = [
    "m3u8", "mpd", "mp4", "ts", "mkv", "webm", "m4v", "m4a", "aac",
];

#[derive(Debug, thiserror::Error)]
enum StreamRunError {
    #[error(transparent)]
    Resolve(#[from] StreamError),
    #[error(transparent)]
    FfmpegRequired(#[from] FfmpegRequired),
    #[error(transparent)]
    Piece(#[from] PieceError),
    #[error(transparent)]
    Merge(MergeError),
    #[error(transparent)]
    Transfer(#[from] TransferError),
    #[error("cancelled")]
    Cancelled,
}

impl From<io::Error> for StreamRunError {
    fn from(error: io::Error) -> Self {
        StreamRunError::Transfer(TransferError::Disk(error))
    }
}

impl From<MergeError> for StreamRunError {
    fn from(error: MergeError) -> Self {
        match error {
            MergeError::Cancelled => StreamRunError::Cancelled,
            other => StreamRunError::Merge(other),
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum PieceError {
    #[error(
        "the server refused a video piece ({0}). Stream addresses expire after a while: \
         play the video in the browser again and send it to Downpour"
    )]
    Refused(StatusCode),
    #[error("a video piece failed: {0}")]
    Transfer(TransferError),
    /// `url` is the log-safe form: key addresses often carry a token.
    #[error("the encryption key at {url} has {length} bytes instead of {KEY_BYTES}")]
    BadKey { url: String, length: usize },
    #[error("a video piece is larger than {} MB; Downpour keeps each piece in memory", .limit / (1024 * 1024))]
    TooLarge { limit: usize },
    #[error(transparent)]
    Decrypt(#[from] DecryptError),
    #[error("cancelled")]
    Cancelled,
}

impl From<reqwest::Error> for PieceError {
    fn from(error: reqwest::Error) -> Self {
        PieceError::Transfer(error.into())
    }
}

/// How much of a response is held in memory. Pieces are decrypted and
/// written whole, and a key is 16 bytes; a server that streams more is
/// broken or hostile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyLimit {
    Piece,
    Key,
}

const MAX_PIECE_BYTES: usize = 1024 * 1024 * 1024;

impl BodyLimit {
    fn bytes(self) -> usize {
        match self {
            BodyLimit::Piece => MAX_PIECE_BYTES,
            // Room to report a wrong length instead of cutting it off.
            BodyLimit::Key => KEY_BYTES * 64,
        }
    }
}

pub(crate) async fn run(
    job: &TransferJob,
    quality: VideoQuality,
    services: &TransferServices,
    cancel: &CancellationToken,
    events: &Events,
) -> TransferOutcome {
    match download(job, quality, services, cancel, events).await {
        Ok(path) => TransferOutcome::Completed { path },
        Err(StreamRunError::Cancelled | StreamRunError::Piece(PieceError::Cancelled)) => {
            TransferOutcome::Stopped
        }
        Err(StreamRunError::Transfer(error)) => TransferOutcome::from_error(error, Path::new(".")),
        Err(error) => TransferOutcome::Failed {
            reason: error.to_string(),
        },
    }
}

async fn download(
    job: &TransferJob,
    quality: VideoQuality,
    services: &TransferServices,
    cancel: &CancellationToken,
    events: &Events,
) -> Result<PathBuf, StreamRunError> {
    let plan = obtain_plan(job, quality, services, cancel).await?;
    events.send(TransferEvent::StreamChosen {
        variant: plan.variant.clone(),
    });
    let output = OutputFormat::choose(&plan, job.ffmpeg.as_deref())?;
    let target = match &job.target {
        Some(target) => target.clone(),
        None => {
            let file_name = stream_file_name(&job.file_name, output.extension());
            let directory = job
                .destination
                .directory_for(&file_name.value, &job.categories);
            reserve_target(&directory, &file_name.value)?.0
        }
    };
    let directory = directory_of(&target).to_path_buf();
    download_into(job, &plan, output, &target, services, cancel, events)
        .await
        .map_err(|error| error.located(&directory))
}

/// Downloads every track next to `target`, then joins them into it.
async fn download_into(
    job: &TransferJob,
    plan: &StreamPlan,
    output: OutputFormat,
    target: &Path,
    services: &TransferServices,
    cancel: &CancellationToken,
    events: &Events,
) -> Result<PathBuf, StreamRunError> {
    let directory = directory_of(target);
    // The final size is unknown until the pieces arrive; a full disk midway
    // is still caught by the writes themselves.
    check_space(directory, 0)?;
    let (files, tracks) = open_tracks(plan, &job.progress, target)?;
    let mut status = RunStatus::new(tracks, events);
    events.send(TransferEvent::Connected {
        file_name: job_file_name(job, target),
        target: target.to_path_buf(),
        validators: Validators::default(),
        progress: status.progress(),
    });

    let fetcher = PieceFetcher::new(services, &job.context, Arc::clone(&status.received));
    let connections = job.max_connections.max(1);
    for (index, (track, file)) in plan.tracks.iter().zip(&files).enumerate() {
        let run = TrackRun {
            index,
            track,
            file: Arc::clone(file),
            fetcher: &fetcher,
            cancel,
        };
        run.download(&mut status, connections).await?;
    }
    status.report(0);
    // Windows refuses to rename or let ffmpeg replace files that are open.
    drop(files);

    // Merging writes a second copy before the tracks are deleted.
    let track_bytes: u64 = status.tracks.iter().map(|track| track.bytes).sum();
    if matches!(output, OutputFormat::Remuxed(_)) {
        check_space(directory, track_bytes)?;
    }
    events.send(TransferEvent::Merging);
    assemble(plan, output, job.ffmpeg.as_deref(), target, cancel).await?;
    let path = finalize(target)?;
    forget_plan(services, job);
    Ok(path)
}

/// The name shown for the download: the reserved target's, once there is one.
fn job_file_name(job: &TransferJob, target: &Path) -> FileName {
    match target.file_name() {
        Some(name) => FileName::new(&name.to_string_lossy(), job.file_name.source),
        None => job.file_name.clone(),
    }
}

impl StreamRunError {
    /// A full disk reported by a write names no folder; attach the one
    /// being written so the download can wait for room there.
    fn located(self, directory: &Path) -> StreamRunError {
        match self {
            StreamRunError::Transfer(TransferError::Disk(error)) if is_disk_full(&error) => {
                StreamRunError::Transfer(TransferError::NoSpace(NoSpace {
                    directory: directory.to_path_buf(),
                    needed: None,
                    available: free_space(directory).unwrap_or(0),
                }))
            }
            other => other,
        }
    }
}

/// Reads the manifest again, because piece addresses are often signed and
/// expire. Falls back to the saved piece list when the manifest itself is
/// gone and the download already has progress worth keeping.
async fn obtain_plan(
    job: &TransferJob,
    quality: VideoQuality,
    services: &TransferServices,
    cancel: &CancellationToken,
) -> Result<StreamPlan, StreamRunError> {
    let fresh = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(StreamRunError::Cancelled),
        fresh = resolve::resolve(&services.client, &job.url, &job.context, quality) => fresh,
    };
    let store = services.store.clone();
    let id = job.id;
    let plan = match fresh {
        Ok(plan) => plan,
        Err(error) if job.progress.received_bytes() > 0 => {
            let saved = tokio::task::spawn_blocking(move || store.load_stream_plan(id))
                .await
                .map_err(io::Error::other)?;
            match saved {
                Ok(Some(saved)) => {
                    tracing::warn!(download = %id, %error, "manifest unavailable, resuming with the saved piece list");
                    return Ok(saved);
                }
                Ok(None) => return Err(error.into()),
                Err(load_error) => {
                    tracing::warn!(download = %id, %load_error, "saved piece list unreadable");
                    return Err(error.into());
                }
            }
        }
        Err(error) => return Err(error.into()),
    };
    let saved_plan = plan.clone();
    let saved = tokio::task::spawn_blocking(move || store.save_stream_plan(id, &saved_plan))
        .await
        .map_err(io::Error::other)?;
    if let Err(error) = saved {
        // Only resuming without a working manifest needs the saved copy.
        tracing::warn!(download = %id, %error, "could not save the piece list");
    }
    Ok(plan)
}

fn forget_plan(services: &TransferServices, job: &TransferJob) {
    let path = services.store.stream_plan_path(job.id);
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not delete the piece list")
        }
    }
}

/// `"master.m3u8"` or `"Movie title"` becomes `"master.mp4"` or `"Movie title.mp4"`.
fn stream_file_name(name: &FileName, extension: &str) -> FileName {
    let stem = match name.value.rsplit_once('.') {
        Some((stem, current))
            if !stem.is_empty()
                && REPLACED_EXTENSIONS.contains(&current.to_ascii_lowercase().as_str()) =>
        {
            stem
        }
        _ => name.value.as_str(),
    };
    FileName::new(&format!("{stem}.{extension}"), name.source)
}

type OpenedTracks = (Vec<Arc<File>>, Vec<TrackProgress>);

/// Opens one file per track. Saved progress is kept only when it was made
/// against the same layout and the file still holds the bytes it claims.
fn open_tracks(
    plan: &StreamPlan,
    saved: &TransferProgress,
    target: &Path,
) -> io::Result<OpenedTracks> {
    let saved_tracks = match saved {
        TransferProgress::Media { tracks }
            if tracks.len() == plan.tracks.len()
                && tracks.iter().zip(&plan.tracks).all(|(progress, track)| {
                    progress.role == track.role && progress.pieces_total == track.pieces.len()
                }) =>
        {
            Some(tracks.as_slice())
        }
        _ => None,
    };
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut files = Vec::with_capacity(plan.tracks.len());
    let mut progress = Vec::with_capacity(plan.tracks.len());
    for (index, track) in plan.tracks.iter().enumerate() {
        let path = track_path(target, index);
        let fresh = TrackProgress {
            role: track.role,
            pieces_total: track.pieces.len(),
            pieces_done: 0,
            bytes: 0,
        };
        let resumed = saved_tracks
            .map(|tracks| tracks[index].clone())
            .filter(|track| track.bytes > 0);
        let reopened = match resumed {
            Some(track) => reopen_track(&path, track.bytes)?.map(|file| (file, track)),
            None => None,
        };
        let (file, track) = match reopened {
            Some(reopened) => reopened,
            None => (create_track(&path)?, fresh),
        };
        files.push(Arc::new(file));
        progress.push(track);
    }
    Ok((files, progress))
}

fn reopen_track(path: &Path, bytes: u64) -> io::Result<Option<File>> {
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() < bytes {
        return Ok(None);
    }
    // Bytes past the last complete piece belong to a piece that was cut off.
    file.set_len(bytes)?;
    Ok(Some(file))
}

fn create_track(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

/// Turns the finished track files into the part file that becomes the target.
async fn assemble(
    plan: &StreamPlan,
    output: OutputFormat,
    ffmpeg: Option<&Path>,
    target: &Path,
    cancel: &CancellationToken,
) -> Result<(), StreamRunError> {
    let tracks: Vec<PathBuf> = (0..plan.tracks.len())
        .map(|index| track_path(target, index))
        .collect();
    match output {
        OutputFormat::Remuxed(muxer) => {
            let ffmpeg = ffmpeg.ok_or(FfmpegRequired)?;
            mux::remux(ffmpeg, &tracks, &part_path(target), muxer, cancel).await?;
            for track in &tracks {
                if let Err(error) = std::fs::remove_file(track) {
                    tracing::warn!(path = %track.display(), %error, "could not delete a merged track");
                }
            }
        }
        OutputFormat::AsDownloaded(_) => std::fs::rename(&tracks[0], part_path(target))?,
    }
    Ok(())
}

/// Progress of every track, reported to the manager.
struct RunStatus<'a> {
    tracks: Vec<TrackProgress>,
    /// Every byte received, including pieces not yet written; drives the speed.
    received: Arc<AtomicU64>,
    meter: SpeedMeter,
    events: &'a Events,
    last_report: Instant,
}

impl<'a> RunStatus<'a> {
    fn new(tracks: Vec<TrackProgress>, events: &'a Events) -> Self {
        Self {
            tracks,
            received: Arc::new(AtomicU64::new(0)),
            meter: SpeedMeter::default(),
            events,
            last_report: Instant::now(),
        }
    }

    fn progress(&self) -> TransferProgress {
        TransferProgress::Media {
            tracks: self.tracks.clone(),
        }
    }

    fn report(&mut self, connections: usize) {
        let now = Instant::now();
        self.meter
            .record(now, self.received.load(Ordering::Relaxed));
        self.events.send(TransferEvent::Progress {
            progress: self.progress(),
            bytes_per_second: self.meter.bytes_per_second(),
            connections,
        });
        self.last_report = now;
    }

    fn report_if_due(&mut self, connections: usize) {
        if self.last_report.elapsed() >= PROGRESS_INTERVAL {
            self.report(connections);
        }
    }
}

/// Downloads the remaining pieces of one track.
struct TrackRun<'a> {
    index: usize,
    track: &'a TrackPlan,
    file: Arc<File>,
    fetcher: &'a PieceFetcher,
    cancel: &'a CancellationToken,
}

impl TrackRun<'_> {
    async fn download(
        &self,
        status: &mut RunStatus<'_>,
        connections: usize,
    ) -> Result<(), StreamRunError> {
        if status.tracks[self.index].is_complete() {
            return Ok(());
        }
        match self.track.pieces.as_slice() {
            // A whole representation in one file can be gigabytes: stream it
            // to disk instead of holding it in memory.
            [piece] if piece.encryption == PieceEncryption::Clear => {
                self.stream_single_piece(piece, status).await?
            }
            _ => self.download_pieces(status, connections).await?,
        }
        self.file.sync_all()?;
        Ok(())
    }

    async fn download_pieces(
        &self,
        status: &mut RunStatus<'_>,
        connections: usize,
    ) -> Result<(), StreamRunError> {
        let total = self.track.pieces.len();
        let read_ahead = connections * READ_AHEAD_PER_CONNECTION;
        let mut next_write = status.tracks[self.index].pieces_done;
        let mut next_start = next_write;
        let mut finished: BTreeMap<usize, Vec<u8>> = BTreeMap::new();
        let mut in_flight: JoinSet<(usize, Result<Vec<u8>, PieceError>)> = JoinSet::new();
        let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);

        while next_write < total {
            while in_flight.len() < connections
                && next_start < total
                && next_start < next_write + read_ahead
            {
                let piece = self.track.pieces[next_start].clone();
                let fetcher = self.fetcher.clone();
                let cancel = self.cancel.clone();
                let position = next_start;
                in_flight.spawn(async move { (position, fetcher.fetch(&piece, &cancel).await) });
                next_start += 1;
            }
            tokio::select! {
                biased;
                // Returning drops the JoinSet, which aborts the other fetches.
                _ = self.cancel.cancelled() => return Err(StreamRunError::Cancelled),
                Some(joined) = in_flight.join_next() => {
                    let (position, body) = joined.map_err(io::Error::other)?;
                    finished.insert(position, body?);
                    while let Some(body) = finished.remove(&next_write) {
                        let length = body.len() as u64;
                        let offset = status.tracks[self.index].bytes;
                        write_at_blocking(Arc::clone(&self.file), offset, body).await?;
                        let track = &mut status.tracks[self.index];
                        track.bytes += length;
                        track.pieces_done += 1;
                        next_write += 1;
                    }
                }
                _ = ticker.tick() => status.report(in_flight.len()),
            }
        }
        Ok(())
    }

    async fn stream_single_piece(
        &self,
        piece: &MediaPiece,
        status: &mut RunStatus<'_>,
    ) -> Result<(), StreamRunError> {
        let mut delay = FIRST_RETRY_DELAY;
        let mut attempt = 1;
        loop {
            let result = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Err(StreamRunError::Cancelled),
                result = self.stream_piece_once(piece, status) => result,
            };
            match result {
                Ok(()) => {
                    status.tracks[self.index].pieces_done = 1;
                    return Ok(());
                }
                Err(StreamRunError::Piece(PieceError::Transfer(error)))
                    if error.is_transient() && attempt < MAX_PIECE_ATTEMPTS =>
                {
                    tracing::warn!(url = %url_for_logs(&piece.url), %error, attempt, "stream interrupted, resuming");
                    tokio::select! {
                        _ = self.cancel.cancelled() => return Err(StreamRunError::Cancelled),
                        _ = tokio::time::sleep(delay) => {}
                    }
                    delay *= 2;
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Appends the rest of `piece` to the track file, continuing after the
    /// bytes already written when the server honors ranges.
    async fn stream_piece_once(
        &self,
        piece: &MediaPiece,
        status: &mut RunStatus<'_>,
    ) -> Result<(), StreamRunError> {
        let written = status.tracks[self.index].bytes;
        if piece.range.is_some_and(|range| written >= range.length) {
            return Ok(());
        }
        let range = match piece.range {
            Some(range) => Some(format!(
                "bytes={}-{}",
                range.start + written,
                range.start + range.length - 1
            )),
            None if written > 0 => Some(format!("bytes={written}-")),
            None => None,
        };
        let response = self.fetcher.send(&piece.url, range.as_deref()).await?;
        match response.status() {
            StatusCode::PARTIAL_CONTENT if range.is_some() => {}
            StatusCode::OK if piece.range.is_none() => {
                // The server ignored the resume request and sent everything.
                status.tracks[self.index].bytes = 0;
                self.file.set_len(0)?;
            }
            status => return Err(PieceError::from_status(status, range.is_some()).into()),
        }

        let mut body = response.bytes_stream();
        let mut buffer = Vec::with_capacity(FLUSH_THRESHOLD_BYTES);
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(PieceError::from)?;
            self.fetcher.count(chunk.len()).await;
            buffer.extend_from_slice(&chunk);
            if buffer.len() >= FLUSH_THRESHOLD_BYTES {
                self.append(std::mem::take(&mut buffer), status).await?;
                status.report_if_due(1);
            }
        }
        self.append(buffer, status).await?;
        if piece
            .range
            .is_some_and(|range| status.tracks[self.index].bytes != range.length)
        {
            return Err(PieceError::Transfer(TransferError::TruncatedBody).into());
        }
        Ok(())
    }

    async fn append(&self, bytes: Vec<u8>, status: &mut RunStatus<'_>) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let length = bytes.len() as u64;
        let offset = status.tracks[self.index].bytes;
        write_at_blocking(Arc::clone(&self.file), offset, bytes).await?;
        status.tracks[self.index].bytes += length;
        Ok(())
    }
}

impl PieceError {
    fn from_status(status: StatusCode, ranged: bool) -> PieceError {
        match status {
            StatusCode::UNAUTHORIZED
            | StatusCode::FORBIDDEN
            | StatusCode::NOT_FOUND
            | StatusCode::GONE => PieceError::Refused(status),
            StatusCode::OK if ranged => PieceError::Transfer(TransferError::RangeNotHonored),
            other => PieceError::Transfer(TransferError::HttpStatus(other)),
        }
    }
}

/// Fetches pieces with the page's headers, the download's speed limit, and
/// cached HLS keys.
#[derive(Clone)]
struct PieceFetcher {
    client: Client,
    context: ScopedContext,
    throttle: Arc<Throttle>,
    received: Arc<AtomicU64>,
    keys: Arc<AsyncMutex<HashMap<Url, [u8; KEY_BYTES]>>>,
}

impl PieceFetcher {
    fn new(services: &TransferServices, context: &ScopedContext, received: Arc<AtomicU64>) -> Self {
        Self {
            client: services.client.clone(),
            context: context.clone(),
            throttle: Arc::clone(&services.throttle),
            received,
            keys: Arc::default(),
        }
    }

    async fn send(&self, url: &Url, range: Option<&str>) -> Result<Response, PieceError> {
        let mut request = self
            .client
            .get(url.clone())
            .headers(self.context.headers_for(url));
        if let Some(range) = range {
            request = request.header(header::RANGE, range);
        }
        Ok(request.send().await?)
    }

    async fn count(&self, bytes: usize) {
        self.throttle.acquire(bytes as u64).await;
        self.received.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// The piece's bytes, decrypted when the playlist encrypts them.
    async fn fetch(
        &self,
        piece: &MediaPiece,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, PieceError> {
        let body = self
            .fetch_with_retries(piece, BodyLimit::Piece, cancel)
            .await?;
        match &piece.encryption {
            PieceEncryption::Clear => Ok(body),
            PieceEncryption::Aes128 { key_url, iv } => {
                let key = self.key(key_url, cancel).await?;
                Ok(decrypt_segment(&key, iv, body)?)
            }
        }
    }

    async fn key(
        &self,
        url: &Url,
        cancel: &CancellationToken,
    ) -> Result<[u8; KEY_BYTES], PieceError> {
        // Held across the fetch so concurrent pieces wait for one key request.
        let mut keys = self.keys.lock().await;
        if let Some(key) = keys.get(url) {
            return Ok(*key);
        }
        let body = self
            .fetch_with_retries(&MediaPiece::clear(url.clone()), BodyLimit::Key, cancel)
            .await?;
        let key: [u8; KEY_BYTES] = body.as_slice().try_into().map_err(|_| PieceError::BadKey {
            url: url_for_logs(url),
            length: body.len(),
        })?;
        keys.insert(url.clone(), key);
        Ok(key)
    }

    /// Retries network errors, timeouts, and 5xx/429 answers with doubling
    /// delays; refusals (403, 404, …) fail at once because retrying an
    /// expired address cannot help.
    async fn fetch_with_retries(
        &self,
        piece: &MediaPiece,
        limit: BodyLimit,
        cancel: &CancellationToken,
    ) -> Result<Vec<u8>, PieceError> {
        let mut delay = FIRST_RETRY_DELAY;
        let mut attempt = 1;
        loop {
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(PieceError::Cancelled),
                result = self.fetch_once(piece, limit) => result,
            };
            match result {
                Err(PieceError::Transfer(error))
                    if error.is_transient() && attempt < MAX_PIECE_ATTEMPTS =>
                {
                    tracing::debug!(url = %url_for_logs(&piece.url), %error, attempt, "piece failed, will retry");
                    tokio::select! {
                        _ = cancel.cancelled() => return Err(PieceError::Cancelled),
                        _ = tokio::time::sleep(delay) => {}
                    }
                    delay *= 2;
                    attempt += 1;
                }
                other => return other,
            }
        }
    }

    async fn fetch_once(
        &self,
        piece: &MediaPiece,
        limit: BodyLimit,
    ) -> Result<Vec<u8>, PieceError> {
        let range = piece.range.map(|range| range.header_value());
        let response = self.send(&piece.url, range.as_deref()).await?;
        let status = response.status();
        let accepted = match range {
            Some(_) => status == StatusCode::PARTIAL_CONTENT,
            None => status.is_success(),
        };
        if !accepted {
            return Err(PieceError::from_status(status, range.is_some()));
        }
        let limit = limit.bytes();
        let expected = response.content_length();
        if expected.is_some_and(|expected| expected > limit as u64) {
            return Err(PieceError::TooLarge { limit });
        }
        let mut body = Vec::with_capacity(expected.unwrap_or(0).min(64 * 1024 * 1024) as usize);
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            if body.len() + chunk.len() > limit {
                return Err(PieceError::TooLarge { limit });
            }
            self.count(chunk.len()).await;
            body.extend_from_slice(&chunk);
        }
        if expected.is_some_and(|expected| expected != body.len() as u64) {
            return Err(PieceError::Transfer(TransferError::TruncatedBody));
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::naming::FileNameSource;

    #[test]
    fn stream_names_get_the_output_extension() {
        let from_url = FileName::new("master.m3u8", FileNameSource::Url);
        assert_eq!(stream_file_name(&from_url, "mp4").value, "master.mp4");
        let title = FileName::new("Scene 3 - Site.com", FileNameSource::Browser);
        assert_eq!(
            stream_file_name(&title, "mp4").value,
            "Scene 3 - Site.com.mp4"
        );
        let previous = FileName::new("clip.ts", FileNameSource::Browser);
        assert_eq!(stream_file_name(&previous, "mkv").value, "clip.mkv");
    }
}
