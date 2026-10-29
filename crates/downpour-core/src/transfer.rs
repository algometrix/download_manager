//! Executes one download: probe, reserve the file, run connections, finish.
//!
//! The controller owns the [`SegmentTable`] and decides when to open another
//! connection. Each worker fetches one segment, writes it at its offset in a
//! pre-allocated `.dppart` file, and stops early when a split shrinks its
//! segment. The part file is renamed to the target only after every byte is
//! written.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use reqwest::{Client, Response};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::category::Categories;
use crate::disk::{self, is_disk_full, NoSpaceCheck};
use crate::http::{self, Probe, ScopedContext, TransferError, TransferPlan, Validators};
use crate::limiter::Throttle;
use crate::naming::{first_free_path, FileName, FileNameSource};
use crate::record::{part_path, Destination, DownloadId, DownloadSource, TransferProgress};
use crate::segment::{SegmentLayoutError, SegmentTable, SlotId, SplitConfirmation};
use crate::speed::SpeedMeter;
use crate::store::Store;
use crate::stream::{self, VariantInfo};

/// Segments are never split below this size; extra connections for smaller
/// pieces cost more in handshakes than they save.
const MIN_SPLIT_BYTES: u64 = 512 * 1024;
const FLUSH_THRESHOLD_BYTES: usize = 512 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);
const MAX_CONSECUTIVE_SEGMENT_FAILURES: u32 = 8;
const MAX_STREAM_ATTEMPTS: u32 = 3;
const REOPEN_COOLDOWN: Duration = Duration::from_millis(300);
const MAX_NAME_RESERVATION_ATTEMPTS: u32 = 100;

/// Everything the transfer needs to know about the download it executes.
#[derive(Clone, Debug)]
pub struct TransferJob {
    pub id: DownloadId,
    pub url: Url,
    /// Scoped to the address the download was added with, so a
    /// redirect never receives the page's cookies.
    pub context: ScopedContext,
    pub file_name: FileName,
    pub destination: Destination,
    pub target: Option<PathBuf>,
    pub progress: TransferProgress,
    pub validators: Validators,
    pub max_connections: usize,
    pub source: DownloadSource,
    /// Category folders as configured when the job started.
    pub categories: Categories,
    /// ffmpeg, found when the job started; streams need it to merge tracks.
    pub ffmpeg: Option<PathBuf>,
}

/// The services one transfer uses.
#[derive(Clone)]
pub struct TransferServices {
    pub client: Client,
    /// This download's speed limit.
    pub throttle: Arc<Throttle>,
    /// Keeps the piece lists of stream downloads.
    pub store: Store,
}

#[derive(Debug)]
pub enum TransferEvent {
    /// The server answered (or a saved part file was reopened) and bytes will flow.
    Connected {
        file_name: FileName,
        target: PathBuf,
        validators: Validators,
        progress: TransferProgress,
    },
    Progress {
        progress: TransferProgress,
        bytes_per_second: u64,
        connections: usize,
    },
    /// A stream's manifest was read and this quality chosen.
    StreamChosen {
        variant: Option<VariantInfo>,
    },
    /// Every piece is on disk; ffmpeg is joining the tracks.
    Merging,
    Finished(TransferOutcome),
}

#[derive(Debug)]
pub enum TransferOutcome {
    Completed {
        path: PathBuf,
    },
    /// Stopped by the cancellation token; the last `Progress` is resumable state.
    Stopped,
    Failed {
        reason: String,
    },
    /// The disk is too full; the download can continue once there is room.
    OutOfSpace {
        directory: PathBuf,
        needed_bytes: Option<u64>,
    },
}

impl TransferOutcome {
    /// The outcome for a transfer that stopped on `error` while writing
    /// into `directory`: a full disk means "wait", anything else "failed".
    pub(crate) fn from_error(error: TransferError, directory: &Path) -> TransferOutcome {
        match error {
            TransferError::NoSpace(no_space) => TransferOutcome::OutOfSpace {
                directory: no_space.directory,
                needed_bytes: no_space.needed,
            },
            TransferError::Disk(io) if is_disk_full(&io) => TransferOutcome::OutOfSpace {
                directory: directory.to_path_buf(),
                needed_bytes: None,
            },
            other => TransferOutcome::Failed {
                reason: other.to_string(),
            },
        }
    }
}

/// The folder a target lives in, for space checks and error messages.
pub(crate) fn directory_of(target: &Path) -> &Path {
    target.parent().unwrap_or(Path::new("."))
}

/// Refuses to start writing `needed` bytes into `directory` when they would
/// not fit. When free space cannot be read, the transfer goes ahead and a
/// full disk is still caught by the write itself.
pub(crate) fn check_space(directory: &Path, needed: u64) -> Result<(), TransferError> {
    match disk::ensure_space(directory, needed) {
        Ok(()) => Ok(()),
        Err(NoSpaceCheck::Short(no_space)) => Err(TransferError::NoSpace(no_space)),
        Err(NoSpaceCheck::Unknown(error)) => {
            tracing::warn!(directory = %directory.display(), %error, "cannot read free disk space");
            Ok(())
        }
    }
}

pub type EventSender = mpsc::UnboundedSender<(DownloadId, TransferEvent)>;

pub(crate) struct Events {
    pub(crate) id: DownloadId,
    sender: EventSender,
}

impl Events {
    pub(crate) fn send(&self, event: TransferEvent) {
        // The manager outlives transfers; a closed channel means the app is
        // shutting down and nobody needs the event.
        let _ = self.sender.send((self.id, event));
    }
}

#[derive(Debug, thiserror::Error)]
enum PrepareError {
    #[error(transparent)]
    Transfer(#[from] TransferError),
    #[error("saved progress is inconsistent: {0}")]
    Layout(#[from] SegmentLayoutError),
    #[error("cancelled")]
    Cancelled,
}

/// Runs the download until it completes, fails, or `cancel` fires. Always
/// ends with exactly one [`TransferEvent::Finished`].
pub async fn run_transfer(
    job: TransferJob,
    services: TransferServices,
    cancel: CancellationToken,
    sender: EventSender,
) {
    let events = Events { id: job.id, sender };
    let outcome = match &job.source {
        DownloadSource::File => run_file(&job, &services, &cancel, &events).await,
        DownloadSource::Stream { quality, .. } => {
            stream::download::run(&job, *quality, &services, &cancel, &events).await
        }
    };
    tracing::info!(download = %job.id, ?outcome, "transfer finished");
    events.send(TransferEvent::Finished(outcome));
}

async fn run_file(
    job: &TransferJob,
    services: &TransferServices,
    cancel: &CancellationToken,
    events: &Events,
) -> TransferOutcome {
    match prepare(job, services, cancel, events).await {
        Ok(Prepared::Segmented(run)) => run.drive(cancel, events).await,
        Ok(Prepared::Stream(run)) => run.drive(job, services, cancel, events).await,
        Err(PrepareError::Cancelled) => TransferOutcome::Stopped,
        Err(PrepareError::Transfer(error)) => {
            let directory = match &job.target {
                Some(target) => directory_of(target).to_path_buf(),
                None => job
                    .destination
                    .directory_for(&job.file_name.value, &job.categories),
            };
            TransferOutcome::from_error(error, &directory)
        }
        Err(error) => TransferOutcome::Failed {
            reason: error.to_string(),
        },
    }
}

enum Prepared {
    Segmented(SegmentedRun),
    Stream(StreamRun),
}

async fn prepare(
    job: &TransferJob,
    services: &TransferServices,
    cancel: &CancellationToken,
    events: &Events,
) -> Result<Prepared, PrepareError> {
    if let Some(resumed) = reopen_segmented(job, services)? {
        events.send(TransferEvent::Connected {
            file_name: job.file_name.clone(),
            target: resumed.target.clone(),
            validators: job.validators.clone(),
            progress: resumed.progress(),
        });
        return Ok(Prepared::Segmented(resumed));
    }

    let probe = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(PrepareError::Cancelled),
        probe = http::probe(&services.client, &job.url, &job.context) => probe?,
    };
    let file_name = match &probe.server_file_name {
        Some(name) if job.file_name.is_replaceable_by_server() => {
            FileName::new(name, FileNameSource::Server)
        }
        _ => job.file_name.clone(),
    };
    let (target, part) = match &job.target {
        Some(target) => (target.clone(), recreate_part_file(target)?),
        None => reserve_target(
            &job.destination
                .directory_for(&file_name.value, &job.categories),
            &file_name.value,
        )?,
    };

    let Probe {
        response,
        final_url,
        plan,
        validators,
        ..
    } = probe;
    let prepared = match plan {
        TransferPlan::Segmented { size } => {
            // The whole file is allocated up front, so all of it must fit now.
            if let Err(error) = check_space(directory_of(&target), size) {
                drop(part);
                discard_reservation(&target);
                return Err(error.into());
            }
            part.set_len(size).map_err(TransferError::Disk)?;
            Prepared::Segmented(SegmentedRun {
                shared: Arc::new(Shared {
                    table: Mutex::new(SegmentTable::whole_file(size)),
                    part: Arc::new(part),
                    url: final_url,
                    context: job.context.clone(),
                    validators: validators.clone(),
                    services: services.clone(),
                    accepted_connections: AtomicUsize::new(0),
                    address_served: AtomicBool::new(false),
                }),
                target: target.clone(),
                first_response: Some(response),
                max_connections: job.max_connections.max(1),
                server_limit: ServerConnectionLimit::Unknown,
            })
        }
        TransferPlan::Stream { size } => Prepared::Stream(StreamRun {
            part,
            target: target.clone(),
            first_response: Some(response),
            size,
        }),
    };
    let progress = match &prepared {
        Prepared::Segmented(run) => run.progress(),
        Prepared::Stream(run) => TransferProgress::Streamed {
            size: run.size,
            received: 0,
        },
    };
    events.send(TransferEvent::Connected {
        file_name,
        target,
        validators,
        progress,
    });
    Ok(prepared)
}

/// Reopens a partially downloaded range-capable file, if one exists.
fn reopen_segmented(
    job: &TransferJob,
    services: &TransferServices,
) -> Result<Option<SegmentedRun>, PrepareError> {
    let (TransferProgress::Segmented { size, segments }, Some(target)) =
        (&job.progress, &job.target)
    else {
        return Ok(None);
    };
    let part = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(part_path(target))
    {
        Ok(part) => part,
        // The user deleted the part file: start over through a fresh probe.
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            tracing::info!(download = %job.id, "part file is gone; starting over");
            return Ok(None);
        }
        Err(error) => return Err(TransferError::Disk(error).into()),
    };
    let table = SegmentTable::from_saved(*size, segments.clone())?;
    Ok(Some(SegmentedRun {
        shared: Arc::new(Shared {
            table: Mutex::new(table),
            part: Arc::new(part),
            url: job.url.clone(),
            context: job.context.clone(),
            validators: job.validators.clone(),
            services: services.clone(),
            accepted_connections: AtomicUsize::new(0),
            address_served: AtomicBool::new(false),
        }),
        target: target.clone(),
        first_response: None,
        max_connections: job.max_connections.max(1),
        server_limit: ServerConnectionLimit::Unknown,
    }))
}

/// Picks a free `name (n).ext` in `dir` and atomically creates its part file,
/// so two downloads can never write to the same target.
pub(crate) fn reserve_target(dir: &Path, name: &str) -> Result<(PathBuf, File), TransferError> {
    std::fs::create_dir_all(dir)?;
    for _ in 0..MAX_NAME_RESERVATION_ATTEMPTS {
        let target = first_free_path(dir, name, |path| path.exists() || part_path(path).exists());
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(part_path(&target))
        {
            Ok(part) => return Ok((target, part)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("no free file name for {name} in {}", dir.display()),
    )
    .into())
}

/// Gives up a name reserved by [`reserve_target`] when the transfer cannot start.
fn discard_reservation(target: &Path) {
    match std::fs::remove_file(part_path(target)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(path = %target.display(), %error, "could not release a reserved name")
        }
    }
}

fn recreate_part_file(target: &Path) -> Result<File, TransferError> {
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(part_path(target))?)
}

/// Moves the finished part file into place. If something else created the
/// target meanwhile, the download gets the next free numbered name instead of
/// overwriting it.
pub(crate) fn finalize(target: &Path) -> Result<PathBuf, TransferError> {
    let dir = target.parent().unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let destination = first_free_path(dir, &name, Path::exists);
    std::fs::rename(part_path(target), &destination)?;
    Ok(destination)
}

fn write_at(file: &File, mut offset: u64, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        #[cfg(windows)]
        let written = std::os::windows::fs::FileExt::seek_write(file, bytes, offset)?;
        #[cfg(unix)]
        let written = std::os::unix::fs::FileExt::write_at(file, bytes, offset)?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[written..];
        offset += written as u64;
    }
    Ok(())
}

pub(crate) async fn write_at_blocking(
    file: Arc<File>,
    offset: u64,
    bytes: Vec<u8>,
) -> io::Result<()> {
    tokio::task::spawn_blocking(move || write_at(&file, offset, &bytes))
        .await
        .map_err(io::Error::other)?
}

struct Shared {
    table: Mutex<SegmentTable>,
    part: Arc<File>,
    url: Url,
    context: ScopedContext,
    validators: Validators,
    services: TransferServices,
    /// Workers whose request the server accepted and that are still running.
    accepted_connections: AtomicUsize,
    /// Whether the address has served this download during this run, which
    /// turns a later refusal from "wrong link" into "too many connections".
    address_served: AtomicBool,
}

/// Counts one accepted connection for as long as its worker runs.
struct AcceptedConnection<'a>(&'a AtomicUsize);

impl Drop for AcceptedConnection<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Shared {
    fn accept_connection(&self) -> AcceptedConnection<'_> {
        self.address_served.store(true, Ordering::Relaxed);
        self.accepted_connections.fetch_add(1, Ordering::Relaxed);
        AcceptedConnection(&self.accepted_connections)
    }

    fn accepted_connections(&self) -> usize {
        self.accepted_connections.load(Ordering::Relaxed)
    }

    fn address_served(&self) -> bool {
        self.address_served.load(Ordering::Relaxed)
    }

    fn table(&self) -> std::sync::MutexGuard<'_, SegmentTable> {
        // A worker panicking mid-update is a bug; the table stays usable
        // because every update is a single field assignment.
        self.table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct SegmentedRun {
    shared: Arc<Shared>,
    target: PathBuf,
    first_response: Option<Response>,
    max_connections: usize,
    server_limit: ServerConnectionLimit,
}

/// What the controller knows about the server's per-client connection cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ServerConnectionLimit {
    /// No refusal seen; open up to the configured number of connections.
    Unknown,
    /// The server refused extra connections and `max_connections` was lowered
    /// to what it accepted. A freed slot is reopened only after a cooldown,
    /// because servers notice closed sockets late and would refuse an
    /// immediate replacement.
    Learned { reopen_after: Option<Instant> },
}

impl SegmentedRun {
    fn progress(&self) -> TransferProgress {
        let table = self.shared.table();
        TransferProgress::Segmented {
            size: table.size(),
            segments: table.segments(),
        }
    }

    async fn drive(mut self, cancel: &CancellationToken, events: &Events) -> TransferOutcome {
        let worker_cancel = cancel.child_token();
        let mut workers: JoinSet<(SlotId, Result<(), TransferError>)> = JoinSet::new();
        let mut fatal: Option<TransferError> = None;
        let mut meter = SpeedMeter::default();
        let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);

        loop {
            let stopping = cancel.is_cancelled() || fatal.is_some();
            if !stopping {
                self.open_connections(&mut workers, &worker_cancel);
            }
            if workers.is_empty() && (stopping || self.shared.table().is_complete()) {
                break;
            }

            tokio::select! {
                Some(joined) = workers.join_next() => {
                    let now = Instant::now();
                    match joined {
                        Ok((slot, Ok(()))) => {
                            self.shared.table().release(slot);
                            if let ServerConnectionLimit::Learned { reopen_after } = &mut self.server_limit {
                                *reopen_after = Some(now + REOPEN_COOLDOWN);
                            }
                        }
                        // Servers that cap connections per client refuse the extra ones
                        // while the first keep working. The first refusal teaches the cap;
                        // any later one is retried like other transient errors rather than
                        // shrinking the cap further.
                        Ok((slot, Err(error)))
                            if error.is_connection_limit()
                                && self.server_limit == ServerConnectionLimit::Unknown
                                && self.shared.accepted_connections() > 0 =>
                        {
                            self.shared.table().release(slot);
                            let accepted = self.shared.accepted_connections();
                            tracing::info!(download = %events.id, %error, accepted, "server limits connections");
                            self.max_connections = accepted.min(self.max_connections);
                            self.server_limit = ServerConnectionLimit::Learned { reopen_after: None };
                        }
                        // Some download servers answer an extra connection
                        // with 403 or 404 instead of 429/503, often near the end,
                        // when a finished connection is replaced while the server
                        // still counts the old one. The address works (it served
                        // this run), so this is a cap: fewer connections, and the
                        // range is retried with backoff. Refusals that go on
                        // after MAX_CONSECUTIVE_SEGMENT_FAILURES fail as before.
                        Ok((slot, Err(error))) if error.is_refusal() && self.shared.address_served() => {
                            let failures = self.shared.table().release_after_failure(slot, now);
                            let accepted = self.shared.accepted_connections();
                            if accepted > 0 && accepted < self.max_connections {
                                self.max_connections = accepted;
                                self.server_limit = ServerConnectionLimit::Learned {
                                    reopen_after: Some(now + REOPEN_COOLDOWN),
                                };
                            }
                            tracing::info!(download = %events.id, %error, accepted, failures, "address refused an extra connection; retrying with fewer");
                            if failures >= MAX_CONSECUTIVE_SEGMENT_FAILURES {
                                fatal = Some(error);
                                worker_cancel.cancel();
                            }
                        }
                        Ok((slot, Err(error))) if error.is_transient() => {
                            let failures = self.shared.table().release_after_failure(slot, now);
                            if error.is_connection_limit() {
                                // Expected while a burst of extra connections drains after the cap was learned.
                                tracing::debug!(download = %events.id, %error, failures, "connection refused, will retry");
                            } else {
                                tracing::warn!(download = %events.id, %error, failures, "connection failed, will retry");
                            }
                            if failures >= MAX_CONSECUTIVE_SEGMENT_FAILURES {
                                fatal = Some(error);
                                worker_cancel.cancel();
                            }
                        }
                        Ok((slot, Err(error))) => {
                            self.shared.table().release(slot);
                            fatal.get_or_insert(error);
                            worker_cancel.cancel();
                        }
                        Err(join_error) => {
                            fatal.get_or_insert(TransferError::Disk(io::Error::other(join_error)));
                            worker_cancel.cancel();
                        }
                    }
                }
                _ = ticker.tick() => self.report(&mut meter, workers.len(), events),
                _ = cancel.cancelled(), if !stopping => {}
            }
        }
        self.report(&mut meter, 0, events);

        if let Some(error) = fatal {
            return TransferOutcome::from_error(error, directory_of(&self.target));
        }
        if !self.shared.table().is_complete() {
            return TransferOutcome::Stopped;
        }
        if let Err(error) = self.shared.part.sync_all() {
            return TransferOutcome::from_error(
                TransferError::Disk(error),
                directory_of(&self.target),
            );
        }
        // Every worker has exited, so this drops the last handle to the part
        // file; Windows refuses to rename open files.
        drop(self.shared);
        match finalize(&self.target) {
            Ok(path) => TransferOutcome::Completed { path },
            Err(error) => TransferOutcome::from_error(error, directory_of(&self.target)),
        }
    }

    fn open_connections(
        &mut self,
        workers: &mut JoinSet<(SlotId, Result<(), TransferError>)>,
        cancel: &CancellationToken,
    ) {
        if let ServerConnectionLimit::Learned {
            reopen_after: Some(at),
        } = self.server_limit
        {
            if Instant::now() < at {
                return;
            }
        }
        while workers.len() < self.max_connections {
            let Some(slot) = self
                .shared
                .table()
                .assign_next(Instant::now(), MIN_SPLIT_BYTES)
            else {
                return;
            };
            let shared = Arc::clone(&self.shared);
            let first_response = self.first_response.take();
            let cancel = cancel.clone();
            workers.spawn(async move {
                (
                    slot,
                    fetch_segment(shared, slot, first_response, cancel).await,
                )
            });
        }
    }

    fn report(&self, meter: &mut SpeedMeter, connections: usize, events: &Events) {
        let progress = self.progress();
        meter.record(Instant::now(), progress.received_bytes());
        events.send(TransferEvent::Progress {
            progress,
            bytes_per_second: meter.bytes_per_second(),
            connections,
        });
    }
}

/// Downloads one segment until it is fully claimed, the stream ends, or the
/// worker is cancelled. Buffered bytes are always flushed before returning,
/// including on errors, because they are valid data.
async fn fetch_segment(
    shared: Arc<Shared>,
    slot: SlotId,
    first_response: Option<Response>,
    cancel: CancellationToken,
) -> Result<(), TransferError> {
    let segment = shared.table().segment(slot);
    let response = match first_response {
        Some(response) => response,
        None => {
            let request = http::fetch_range(
                &shared.services.client,
                &shared.url,
                &shared.context,
                &shared.validators,
                segment.write_position(),
                segment.end,
            );
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Ok(()),
                response = request => response?,
            }
        }
    };

    let _accepted = shared.accept_connection();
    // A split's range belongs to this connection only from now on; the
    // donor may have downloaded the start of it while the request was out.
    let skip = match shared.table().confirm_split(slot) {
        SplitConfirmation::Proceed { skip } => skip,
        SplitConfirmation::NothingLeft => return Ok(()),
    };
    let segment = shared.table().segment(slot);
    let mut pending = PendingWrite {
        offset: segment.write_position(),
        bytes: Vec::with_capacity(FLUSH_THRESHOLD_BYTES),
    };
    let received = receive_segment(&shared, slot, response, skip, &cancel, &mut pending).await;
    pending.flush(&shared, slot).await?;
    received
}

struct PendingWrite {
    offset: u64,
    bytes: Vec<u8>,
}

impl PendingWrite {
    async fn flush(&mut self, shared: &Arc<Shared>, slot: SlotId) -> Result<(), TransferError> {
        if self.bytes.is_empty() {
            return Ok(());
        }
        let bytes = std::mem::replace(&mut self.bytes, Vec::with_capacity(FLUSH_THRESHOLD_BYTES));
        let length = bytes.len() as u64;
        write_at_blocking(Arc::clone(&shared.part), self.offset, bytes).await?;
        self.offset += length;
        shared.table().record_written(slot, length);
        Ok(())
    }
}

async fn receive_segment(
    shared: &Arc<Shared>,
    slot: SlotId,
    response: Response,
    mut skip: u64,
    cancel: &CancellationToken,
    pending: &mut PendingWrite,
) -> Result<(), TransferError> {
    let mut body = response.bytes_stream();
    loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(()),
            next = body.next() => next,
        };
        let Some(chunk) = next else {
            return if shared.table().is_fully_claimed(slot) {
                Ok(())
            } else {
                Err(TransferError::TruncatedBody)
            };
        };
        let mut chunk = chunk?;
        if skip > 0 {
            let dropped = skip.min(chunk.len() as u64);
            skip -= dropped;
            chunk = chunk.slice(dropped as usize..);
            if chunk.is_empty() {
                continue;
            }
        }
        let accepted = shared.table().claim(slot, chunk.len() as u64) as usize;
        shared.services.throttle.acquire(accepted as u64).await;
        pending.bytes.extend_from_slice(&chunk[..accepted]);
        if pending.bytes.len() >= FLUSH_THRESHOLD_BYTES {
            pending.flush(shared, slot).await?;
        }
        if shared.table().is_fully_claimed(slot) {
            return Ok(());
        }
    }
}

struct StreamRun {
    part: File,
    target: PathBuf,
    first_response: Option<Response>,
    size: Option<u64>,
}

impl StreamRun {
    async fn drive(
        mut self,
        job: &TransferJob,
        services: &TransferServices,
        cancel: &CancellationToken,
        events: &Events,
    ) -> TransferOutcome {
        let part = Arc::new(self.part);
        let mut first_response = self.first_response.take();
        let mut attempt = 1;
        loop {
            let ended = match first_response.take() {
                Some(response) => {
                    stream_body(&part, response, self.size, services, cancel, events).await
                }
                None => match restart_stream(job, services, &part, cancel).await {
                    Ok(Some(response)) => {
                        stream_body(&part, response, self.size, services, cancel, events).await
                    }
                    Ok(None) => Ok(StreamEnd::Cancelled),
                    Err(error) => Err(error),
                },
            };
            match ended {
                Ok(StreamEnd::Complete) => break,
                Ok(StreamEnd::Cancelled) => return TransferOutcome::Stopped,
                Err(error) if error.is_transient() && attempt < MAX_STREAM_ATTEMPTS => {
                    tracing::warn!(download = %events.id, %error, attempt, "stream interrupted, restarting from zero");
                    attempt += 1;
                    tokio::select! {
                        _ = cancel.cancelled() => return TransferOutcome::Stopped,
                        _ = tokio::time::sleep(Duration::from_secs(1u64 << attempt)) => {}
                    }
                }
                Err(error) => {
                    return TransferOutcome::from_error(error, directory_of(&self.target))
                }
            }
        }
        if let Err(error) = part.sync_all() {
            return TransferOutcome::from_error(
                TransferError::Disk(error),
                directory_of(&self.target),
            );
        }
        drop(part);
        match finalize(&self.target) {
            Ok(path) => TransferOutcome::Completed { path },
            Err(error) => TransferOutcome::from_error(error, directory_of(&self.target)),
        }
    }
}

/// Re-requests a non-resumable resource from the beginning.
async fn restart_stream(
    job: &TransferJob,
    services: &TransferServices,
    part: &File,
    cancel: &CancellationToken,
) -> Result<Option<Response>, TransferError> {
    part.set_len(0)?;
    let probe = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Ok(None),
        probe = http::probe(&services.client, &job.url, &job.context) => probe?,
    };
    Ok(Some(probe.response))
}

enum StreamEnd {
    Complete,
    Cancelled,
}

async fn stream_body(
    part: &Arc<File>,
    response: Response,
    size: Option<u64>,
    services: &TransferServices,
    cancel: &CancellationToken,
    events: &Events,
) -> Result<StreamEnd, TransferError> {
    let mut body = response.bytes_stream();
    let mut offset = 0u64;
    let mut buffer = Vec::with_capacity(FLUSH_THRESHOLD_BYTES);
    let mut meter = SpeedMeter::default();
    let mut ticker = tokio::time::interval(PROGRESS_INTERVAL);
    let report = |received: u64, meter: &mut SpeedMeter| {
        meter.record(Instant::now(), received);
        events.send(TransferEvent::Progress {
            progress: TransferProgress::Streamed { size, received },
            bytes_per_second: meter.bytes_per_second(),
            connections: 1,
        });
    };

    let end = loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => break StreamEnd::Cancelled,
            _ = ticker.tick() => {
                report(offset + buffer.len() as u64, &mut meter);
                continue;
            }
            next = body.next() => next,
        };
        let Some(chunk) = next else {
            break StreamEnd::Complete;
        };
        let chunk = chunk?;
        services.throttle.acquire(chunk.len() as u64).await;
        buffer.extend_from_slice(&chunk);
        if buffer.len() >= FLUSH_THRESHOLD_BYTES {
            let length = buffer.len() as u64;
            write_at_blocking(Arc::clone(part), offset, std::mem::take(&mut buffer)).await?;
            offset += length;
        }
    };
    if !buffer.is_empty() {
        let length = buffer.len() as u64;
        write_at_blocking(Arc::clone(part), offset, buffer).await?;
        offset += length;
    }
    report(offset, &mut meter);
    if matches!(end, StreamEnd::Complete) && size.is_some_and(|size| size != offset) {
        return Err(TransferError::TruncatedBody);
    }
    Ok(end)
}
