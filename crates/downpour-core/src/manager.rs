//! Owns every download, runs the queue, and persists state.
//!
//! All mutation happens under one mutex and never across an `.await` or file
//! operation. Transfers report back through a channel that a single event
//! loop applies, so the state machine in [`DownloadStatus`] is the only path
//! by which a status changes.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use reqwest::Client;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, Notify};
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::category::{Categories, CategoryId};
use crate::disk;
use crate::history::CompletedDownload;
use crate::http::{url_for_logs, RequestContext, Validators};
use crate::limiter::{DownloadLimit, RateLimiter, Throttle};
use crate::link_check::{self, CheckedLink};
use crate::naming::{FileName, FileNameSource};
use crate::ordering::{self, QueuePlace};
use crate::queue::{Queue, QueueError, QueueId, QueueState, Queues};
use crate::record::{
    part_path, track_path, Destination, DownloadId, DownloadRecord, DownloadSource,
    TransferProgress, MAX_STREAM_TRACKS,
};
use crate::segment::Segment;
use crate::settings::EngineSettings;
use crate::status::{DownloadStatus, InvalidTransition, StatusEvent};
use crate::store::{Store, StoreError};
use crate::stream::{self, mux, StreamError, StreamFormat, StreamOffer, VariantInfo, VideoQuality};
use crate::transfer::{
    run_transfer, TransferEvent, TransferJob, TransferOutcome, TransferServices,
};

const PERSIST_INTERVAL: Duration = Duration::from_secs(2);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How often downloads waiting for disk space check whether room was freed.
const SPACE_CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Whether a new download begins as soon as a queue slot is free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartMode {
    Queue,
    Hold,
}

/// What to do with files on disk when removing a download from the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoveFiles {
    Keep,
    Delete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewDownload {
    pub url: Url,
    pub file_name: Option<FileName>,
    /// `None` uses the configured download folder.
    pub folder: Option<PathBuf>,
    pub context: RequestContext,
    pub start: StartMode,
    pub kind: DownloadKind,
    /// `None` (or a queue that no longer exists) means the Main queue.
    pub queue: Option<QueueId>,
    /// The page the download was found on.
    pub page_url: Option<Url>,
}

/// What the new download's URL points to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadKind {
    /// Manifest URLs (`.m3u8`, `.mpd`) become streams in the default
    /// quality; anything else is a file.
    Detect,
    Stream {
        format: StreamFormat,
        /// `None` uses the quality from the settings.
        quality: Option<VideoQuality>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    #[error("only http and https URLs can be downloaded, not {0}:")]
    UnsupportedScheme(String),
    #[error("download {0} does not exist")]
    UnknownDownload(DownloadId),
    #[error(transparent)]
    InvalidTransition(#[from] InvalidTransition),
    #[error("cannot build HTTP client: {0}")]
    Client(#[from] reqwest::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Queue(#[from] QueueError),
    #[error("download {0} is running; pause it first")]
    Running(DownloadId),
    #[error("download {0} is not waiting in a queue")]
    NotWaiting(DownloadId),
}

/// Read-only snapshot of one download for display.
#[derive(Clone, Debug)]
pub struct DownloadView {
    pub id: DownloadId,
    pub file_name: String,
    pub url: Url,
    pub page_url: Option<Url>,
    /// `Referer` the browser sent; the page for downloads captured before
    /// the extension reported pages separately.
    pub referrer: Option<String>,
    pub queue: QueueId,
    pub directory: PathBuf,
    pub target: Option<PathBuf>,
    pub category: CategoryId,
    pub status: DownloadStatus,
    pub size: Option<u64>,
    pub received: u64,
    pub bytes_per_second: u64,
    pub connections: usize,
    pub segments: Vec<Segment>,
    pub resumable: Option<bool>,
    /// `size` is extrapolated from the pieces downloaded so far.
    pub size_is_estimate: bool,
    pub progress_fraction: Option<f32>,
    pub stream: Option<StreamView>,
    pub added_at_unix: u64,
    pub finished_at_unix: Option<u64>,
    /// A pause or removal was requested and the transfer is winding down.
    pub stopping: bool,
    pub speed_limit: DownloadLimit,
    /// Waiting in its queue; "Start now" can start it ahead of the queue.
    pub can_start_now: bool,
}

/// The stream-specific part of a download, for display.
#[derive(Clone, Debug)]
pub struct StreamView {
    pub format: StreamFormat,
    pub quality: VideoQuality,
    /// The variant chosen once the manifest was read.
    pub variant: Option<VariantInfo>,
    /// ffmpeg is joining the downloaded tracks.
    pub merging: bool,
}

impl DownloadView {
    pub fn fraction(&self) -> Option<f32> {
        if self.status == DownloadStatus::Completed {
            return Some(1.0);
        }
        self.progress_fraction
    }

    pub fn eta(&self) -> Option<Duration> {
        let remaining = self.size?.checked_sub(self.received)?;
        (self.bytes_per_second > 0).then(|| Duration::from_secs(remaining / self.bytes_per_second))
    }
}

type ChangeListener = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone)]
pub struct DownloadManager {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    client: Client,
    /// The speed limit from Settings, shared by the downloads that follow it.
    shared_limit: Arc<RateLimiter>,
    store: Store,
    runtime: Handle,
    events: mpsc::UnboundedSender<(DownloadId, TransferEvent)>,
    dirty: AtomicBool,
    persist_now: Notify,
    listener: Mutex<Option<ChangeListener>>,
}

struct State {
    downloads: BTreeMap<DownloadId, Entry>,
    next_id: u64,
    settings: EngineSettings,
    queues: Queues,
}

impl State {
    /// Downloads in the start order the user sees and can rearrange.
    fn in_order(&self) -> impl Iterator<Item = &Entry> {
        let mut entries: Vec<&Entry> = self.downloads.values().collect();
        entries.sort_by_key(|entry| (entry.record.position, entry.record.id));
        entries.into_iter()
    }

    fn last_position(&self) -> u64 {
        self.downloads
            .values()
            .map(|entry| entry.record.position)
            .max()
            .unwrap_or(0)
    }
}

struct Entry {
    record: DownloadRecord,
    live: Option<Live>,
    /// What to do once the running transfer reports that it stopped.
    after_stop: AfterStop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AfterStop {
    /// The user paused it (or the app is closing).
    Pause,
    /// Its queue was stopped: it waits in the queue again.
    Requeue,
    /// The user removed it.
    Remove(RemoveFiles),
}

impl AfterStop {
    /// Another stop request arrived before the transfer stopped. A removal
    /// stands: pausing or stopping the queue afterwards must not bring a
    /// deleted download back. Otherwise the newer request wins.
    fn then(self, requested: AfterStop) -> AfterStop {
        match self {
            AfterStop::Remove(_) => self,
            AfterStop::Pause | AfterStop::Requeue => requested,
        }
    }
}

/// Work left after an event was applied, done outside the state lock.
#[derive(Default)]
struct Aftermath {
    leftovers: Option<Leftovers>,
    completed: Option<DownloadRecord>,
}

struct Live {
    cancel: CancellationToken,
    throttle: Arc<Throttle>,
    bytes_per_second: u64,
    connections: usize,
    merging: bool,
}

/// Files a removed download leaves behind, deleted outside the state lock.
struct Leftovers(Vec<PathBuf>);

impl DownloadManager {
    /// Loads saved downloads and starts the event loop and persister on `runtime`.
    pub fn start(
        store: Store,
        records: Vec<DownloadRecord>,
        settings: EngineSettings,
        runtime: Handle,
    ) -> Result<Self, ManagerError> {
        let client = Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()?;
        let shared_limit = Arc::new(RateLimiter::new(settings.speed_limit()));
        let next_id = records
            .iter()
            .map(|record| record.id.0 + 1)
            .max()
            .unwrap_or(1);
        let queues = match store.load_queues() {
            Ok(Some(queues)) => queues,
            Ok(None) => Queues::with_main(settings.max_active_downloads),
            Err(error) => {
                tracing::error!(%error, "queues unreadable, starting with the Main queue");
                Queues::with_main(settings.max_active_downloads)
            }
        };
        if !store.has_history() {
            seed_history(&store, &records, &settings.categories);
        }
        let mut records = records;
        records.sort_by_key(|record| (record.position, record.id));
        let downloads = records
            .into_iter()
            .zip(1..)
            .map(|(mut record, position)| {
                record.status = record.status.after_restart();
                record.position = position;
                (
                    record.id,
                    Entry {
                        record,
                        live: None,
                        after_stop: AfterStop::Pause,
                    },
                )
            })
            .collect();
        let (events, receiver) = mpsc::unbounded_channel();

        let manager = Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    downloads,
                    next_id,
                    settings,
                    queues,
                }),
                client,
                shared_limit,
                store,
                runtime: runtime.clone(),
                events,
                dirty: AtomicBool::new(false),
                persist_now: Notify::new(),
                listener: Mutex::new(None),
            }),
        };
        runtime.spawn(manager.clone().apply_events(receiver));
        runtime.spawn(manager.clone().persist_periodically());
        runtime.spawn(manager.clone().watch_disk_space());
        manager.start_queued();
        Ok(manager)
    }

    /// Called after every state change, e.g. to repaint the UI.
    pub fn set_change_listener(&self, listener: impl Fn() + Send + Sync + 'static) {
        *self
            .inner
            .listener
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(Arc::new(listener));
    }

    pub fn add(&self, request: NewDownload) -> Result<DownloadId, ManagerError> {
        if !matches!(request.url.scheme(), "http" | "https") {
            return Err(ManagerError::UnsupportedScheme(
                request.url.scheme().to_owned(),
            ));
        }
        let id = {
            let mut state = self.state();
            let id = DownloadId(state.next_id);
            state.next_id += 1;
            let destination = match request.folder {
                Some(path) => Destination::Folder { path },
                None if state.settings.organize_by_category => Destination::ByCategory {
                    base: state.settings.download_dir.clone(),
                },
                None => Destination::Folder {
                    path: state.settings.download_dir.clone(),
                },
            };
            let mut context = request.context;
            context
                .user_agent
                .get_or_insert_with(|| state.settings.user_agent.clone());
            let source = source_for(&request.url, request.kind, state.settings.video_quality);
            let file_name = request.file_name.unwrap_or_else(|| match source {
                DownloadSource::File => FileName::guess_from_url(&request.url),
                DownloadSource::Stream { .. } => FileName::guess_for_stream(&request.url),
            });
            let queue = request
                .queue
                .filter(|queue| state.queues.contains(*queue))
                .unwrap_or(QueueId::MAIN);
            let record = DownloadRecord {
                id,
                file_name,
                source,
                queue,
                page_url: request.page_url,
                url: request.url,
                context,
                destination,
                target: None,
                progress: TransferProgress::NotStarted,
                validators: Validators::default(),
                status: match request.start {
                    StartMode::Queue => DownloadStatus::Queued,
                    StartMode::Hold => DownloadStatus::Paused,
                },
                connections: state.settings.connections_per_download,
                added_at_unix: unix_now(),
                finished_at_unix: None,
                // New downloads go to the end of the line.
                position: state.last_position() + 1,
                speed_limit: DownloadLimit::FollowSettings,
            };
            tracing::info!(download = %id, url = %url_for_logs(&record.url), "download added");
            state.downloads.insert(
                id,
                Entry {
                    record,
                    live: None,
                    after_stop: AfterStop::Pause,
                },
            );
            id
        };
        self.changed_and_persist();
        self.start_queued();
        Ok(id)
    }

    pub fn pause(&self, id: DownloadId) -> Result<(), ManagerError> {
        {
            let mut state = self.state();
            let entry = state
                .downloads
                .get_mut(&id)
                .ok_or(ManagerError::UnknownDownload(id))?;
            match &entry.live {
                // The status changes when the transfer reports that it stopped.
                Some(live) => {
                    live.cancel.cancel();
                    entry.after_stop = entry.after_stop.then(AfterStop::Pause);
                }
                None => entry.record.status = entry.record.status.apply(StatusEvent::Paused)?,
            }
        }
        self.changed_and_persist();
        Ok(())
    }

    pub fn resume(&self, id: DownloadId) -> Result<(), ManagerError> {
        {
            let mut state = self.state();
            let entry = state
                .downloads
                .get_mut(&id)
                .ok_or(ManagerError::UnknownDownload(id))?;
            entry.record.status = entry.record.status.apply(StatusEvent::Enqueued)?;
        }
        self.changed_and_persist();
        self.start_queued();
        Ok(())
    }

    /// Starts a queued download now, even when its queue is stopped or full.
    /// It then takes one of its queue's places until it finishes.
    pub fn start_now(&self, id: DownloadId) -> Result<(), ManagerError> {
        {
            let mut guard = self.state();
            let state = &mut *guard;
            let entry = state
                .downloads
                .get_mut(&id)
                .ok_or(ManagerError::UnknownDownload(id))?;
            if !entry.waits_in_queue() {
                return Err(ManagerError::NotWaiting(id));
            }
            self.launch(entry, &state.settings);
        }
        self.changed_and_persist();
        Ok(())
    }

    /// Changes how fast one download may go; a running one changes at once.
    pub fn set_speed_limit(
        &self,
        id: DownloadId,
        limit: DownloadLimit,
    ) -> Result<(), ManagerError> {
        {
            let mut state = self.state();
            let entry = state
                .downloads
                .get_mut(&id)
                .ok_or(ManagerError::UnknownDownload(id))?;
            entry.record.speed_limit = limit;
            if let Some(live) = &entry.live {
                live.throttle.set_limit(limit);
            }
        }
        self.changed_and_persist();
        Ok(())
    }

    /// Discards downloaded bytes and starts over, e.g. after the server's
    /// file changed. Keeps the reserved target name.
    pub fn restart(&self, id: DownloadId) -> Result<(), ManagerError> {
        {
            let mut state = self.state();
            let entry = state
                .downloads
                .get_mut(&id)
                .ok_or(ManagerError::UnknownDownload(id))?;
            let status = entry.record.status.apply(StatusEvent::Enqueued)?;
            entry.record.status = status;
            entry.record.progress = TransferProgress::NotStarted;
            entry.record.validators = Validators::default();
        }
        forget_stream_plan(&self.inner.store, id);
        self.changed_and_persist();
        self.start_queued();
        Ok(())
    }

    pub fn remove(&self, id: DownloadId, files: RemoveFiles) -> Result<(), ManagerError> {
        let leftovers = {
            let mut state = self.state();
            let entry = state
                .downloads
                .get_mut(&id)
                .ok_or(ManagerError::UnknownDownload(id))?;
            if let Some(live) = &entry.live {
                // Removal completes once the transfer stops and closes its file.
                live.cancel.cancel();
                entry.after_stop = entry.after_stop.then(AfterStop::Remove(files));
                None
            } else {
                let entry = state.downloads.remove(&id).expect("entry was just found");
                Some(leftovers(&entry.record, files, &self.inner.store))
            }
        };
        if let Some(leftovers) = leftovers {
            delete_leftovers(leftovers);
        }
        self.changed_and_persist();
        Ok(())
    }

    pub fn pause_all(&self) {
        let ids: Vec<_> = self
            .state()
            .downloads
            .values()
            .filter(|entry| entry.record.status.can_pause())
            .map(|entry| entry.record.id)
            .collect();
        for id in ids {
            if let Err(error) = self.pause(id) {
                tracing::warn!(download = %id, %error, "pause all skipped a download");
            }
        }
    }

    pub fn resume_all(&self) {
        let ids: Vec<_> = self
            .state()
            .downloads
            .values()
            .filter(|entry| entry.record.status.can_resume() && entry.live.is_none())
            .map(|entry| entry.record.id)
            .collect();
        for id in ids {
            if let Err(error) = self.resume(id) {
                tracing::warn!(download = %id, %error, "resume all skipped a download");
            }
        }
    }

    /// Gives a stopped download a fresh address, e.g. after its signed link
    /// expired, and resumes it. Progress is kept: resumed requests still
    /// verify the file is unchanged, and streams re-check their layout.
    pub fn refresh_address(
        &self,
        id: DownloadId,
        url: Url,
        mut context: RequestContext,
    ) -> Result<(), ManagerError> {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(ManagerError::UnsupportedScheme(url.scheme().to_owned()));
        }
        {
            let mut state = self.state();
            let user_agent = state.settings.user_agent.clone();
            let entry = state
                .downloads
                .get_mut(&id)
                .ok_or(ManagerError::UnknownDownload(id))?;
            if entry.live.is_some() {
                return Err(ManagerError::Running(id));
            }
            context.user_agent.get_or_insert(user_agent);
            tracing::info!(download = %id, url = %url_for_logs(&url), "download address refreshed");
            entry.record.url = url;
            entry.record.context = context;
            if entry.record.status.can_resume() {
                entry.record.status = entry.record.status.apply(StatusEvent::Enqueued)?;
            }
        }
        self.changed_and_persist();
        self.start_queued();
        Ok(())
    }

    pub fn queues(&self) -> Vec<Queue> {
        self.state().queues.all().to_vec()
    }

    pub fn add_queue(&self, name: &str) -> Result<QueueId, ManagerError> {
        let limit = self.state().settings.max_active_downloads;
        let id = self.change_queues(|queues| queues.add(name, limit))?;
        Ok(id)
    }

    pub fn rename_queue(&self, id: QueueId, name: &str) -> Result<(), ManagerError> {
        self.change_queues(|queues| queues.rename(id, name))
    }

    pub fn set_queue_limit(&self, id: QueueId, max_active: u8) -> Result<(), ManagerError> {
        self.change_queues(|queues| queues.set_limit(id, max_active))?;
        self.start_queued();
        Ok(())
    }

    /// Removes a queue; its downloads move to the Main queue.
    pub fn remove_queue(&self, id: QueueId) -> Result<(), ManagerError> {
        self.change_queues(|queues| queues.remove(id))?;
        for entry in self.state().downloads.values_mut() {
            if entry.record.queue == id {
                entry.record.queue = QueueId::MAIN;
            }
        }
        self.changed_and_persist();
        self.start_queued();
        Ok(())
    }

    /// Starts a queue: every paused download in it (including
    /// those added with "Download later") is queued again and runs as slots
    /// free up. Failed downloads wait for the user to retry them.
    pub fn start_queue(&self, id: QueueId) -> Result<(), ManagerError> {
        self.change_queues(|queues| queues.set_state(id, QueueState::Running))?;
        for entry in self.state().downloads.values_mut() {
            if entry.record.queue == id
                && entry.record.status == DownloadStatus::Paused
                && entry.live.is_none()
            {
                transition(&mut entry.record, StatusEvent::Enqueued);
            }
        }
        self.changed_and_persist();
        self.start_queued();
        Ok(())
    }

    /// Stops a queue: its running downloads stop and wait in the queue
    /// until it is started again.
    pub fn stop_queue(&self, id: QueueId) -> Result<(), ManagerError> {
        self.change_queues(|queues| queues.set_state(id, QueueState::Stopped))?;
        for entry in self.state().downloads.values_mut() {
            if entry.record.queue != id {
                continue;
            }
            if let Some(live) = &entry.live {
                live.cancel.cancel();
                entry.after_stop = entry.after_stop.then(AfterStop::Requeue);
            }
        }
        self.notify_changed();
        Ok(())
    }

    pub fn move_to_queue(&self, download: DownloadId, queue: QueueId) -> Result<(), ManagerError> {
        {
            let mut state = self.state();
            if !state.queues.contains(queue) {
                return Err(QueueError::Unknown(queue).into());
            }
            let entry = state
                .downloads
                .get_mut(&download)
                .ok_or(ManagerError::UnknownDownload(download))?;
            entry.record.queue = queue;
        }
        self.changed_and_persist();
        self.start_queued();
        Ok(())
    }

    /// Applies `change` and saves the queues; nothing changes if it fails.
    fn change_queues<T>(
        &self,
        change: impl FnOnce(&mut Queues) -> Result<T, QueueError>,
    ) -> Result<T, ManagerError> {
        let (value, snapshot) = {
            let mut state = self.state();
            let mut queues = state.queues.clone();
            let value = change(&mut queues)?;
            state.queues = queues.clone();
            (value, queues)
        };
        self.inner.store.save_queues(&snapshot)?;
        self.notify_changed();
        Ok(value)
    }

    /// Every finished download ever recorded, for statistics.
    pub fn history(&self) -> Result<Vec<CompletedDownload>, ManagerError> {
        Ok(self.inner.store.load_history()?)
    }

    pub fn settings(&self) -> EngineSettings {
        self.state().settings.clone()
    }

    pub fn update_settings(&self, settings: EngineSettings) {
        let settings = settings.normalized();
        self.inner.shared_limit.set_limit(settings.speed_limit());
        self.state().settings = settings;
        self.notify_changed();
        self.start_queued();
    }

    /// Reads a stream's manifest on the engine's runtime and reports the
    /// qualities it offers through `done`.
    pub fn inspect_stream(
        &self,
        url: Url,
        mut context: RequestContext,
        done: impl FnOnce(Result<StreamOffer, StreamError>) + Send + 'static,
    ) {
        let client = self.inner.client.clone();
        context
            .user_agent
            .get_or_insert_with(|| self.state().settings.user_agent.clone());
        self.inner.runtime.spawn(async move {
            done(stream::resolve::inspect(&client, &url, &context.scoped_to(&url)).await);
        });
    }

    /// Checks pasted links: online (with name and size), offline, or
    /// unknown. `done` gets them in the order given.
    pub fn check_links(
        &self,
        links: Vec<Url>,
        done: impl FnOnce(Vec<CheckedLink>) + Send + 'static,
    ) {
        let context = RequestContext {
            user_agent: Some(self.state().settings.user_agent.clone()),
            ..RequestContext::default()
        };
        let client = self.inner.client.clone();
        self.inner
            .runtime
            .spawn(async move { done(link_check::check_all(&client, links, context).await) });
    }

    pub fn snapshot(&self) -> Vec<DownloadView> {
        let state = self.state();
        let categories = &state.settings.categories;
        state
            .in_order()
            .map(|entry| entry.view(categories))
            .collect()
    }

    /// Moves a download within the start order. Downloads already running
    /// keep running; the order decides which waiting one starts next.
    pub fn move_in_queue(&self, id: DownloadId, place: QueuePlace) -> Result<(), ManagerError> {
        {
            let mut state = self.state();
            for known in std::iter::once(id).chain(match place {
                QueuePlace::Before(target) => Some(target),
                QueuePlace::End => None,
            }) {
                if !state.downloads.contains_key(&known) {
                    return Err(ManagerError::UnknownDownload(known));
                }
            }
            let order: Vec<DownloadId> = state.in_order().map(|entry| entry.record.id).collect();
            let order = ordering::reordered(&order, id, place);
            for (id, position) in order.into_iter().zip(1..) {
                if let Some(entry) = state.downloads.get_mut(&id) {
                    entry.record.position = position;
                }
            }
        }
        self.changed_and_persist();
        Ok(())
    }

    /// Stops every transfer, waits up to `timeout` for them to save their
    /// progress, and writes the final state.
    pub fn shutdown(&self, timeout: Duration) {
        for entry in self.state().downloads.values() {
            if let Some(live) = &entry.live {
                live.cancel.cancel();
            }
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline
            && self
                .state()
                .downloads
                .values()
                .any(|entry| entry.live.is_some())
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        if let Err(error) = self.inner.store.save_downloads(&self.records()) {
            tracing::error!(%error, "final save failed");
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn records(&self) -> Vec<DownloadRecord> {
        self.state()
            .downloads
            .values()
            .map(|entry| entry.record.clone())
            .collect()
    }

    /// Starts queued downloads in the user's order while their queue is
    /// running and has a free slot.
    fn start_queued(&self) {
        let mut guard = self.state();
        let state = &mut *guard;
        let mut active: BTreeMap<QueueId, usize> = BTreeMap::new();
        for entry in state
            .downloads
            .values()
            .filter(|entry| entry.live.is_some())
        {
            *active
                .entry(state.queues.get(entry.record.queue).id)
                .or_default() += 1;
        }
        let mut started = false;
        let order: Vec<DownloadId> = state.in_order().map(|entry| entry.record.id).collect();
        for id in order {
            let Some(entry) = state.downloads.get_mut(&id) else {
                continue;
            };
            if !entry.waits_in_queue() {
                continue;
            }
            let queue = state.queues.get(entry.record.queue);
            let running = active.entry(queue.id).or_default();
            if queue.state == QueueState::Stopped || *running >= usize::from(queue.max_active) {
                continue;
            }
            *running += 1;
            self.launch(entry, &state.settings);
            started = true;
        }
        drop(guard);
        if started {
            self.changed_and_persist();
        }
    }

    /// Starts the transfer of a queued download.
    fn launch(&self, entry: &mut Entry, settings: &EngineSettings) {
        transition(&mut entry.record, StatusEvent::Started);
        let record = &entry.record;
        let job = TransferJob {
            id: record.id,
            url: record.url.clone(),
            context: record.context.scoped_to(&record.url),
            file_name: record.file_name.clone(),
            destination: record.destination.clone(),
            target: record.target.clone(),
            progress: record.progress.clone(),
            validators: record.validators.clone(),
            max_connections: usize::from(settings.connections_per_download),
            source: record.source.clone(),
            categories: settings.categories.clone(),
            ffmpeg: match record.source {
                DownloadSource::File => None,
                DownloadSource::Stream { .. } => {
                    mux::locate_ffmpeg(settings.ffmpeg_path.as_deref())
                }
            },
        };
        let cancel = CancellationToken::new();
        let throttle = Arc::new(Throttle::new(
            Arc::clone(&self.inner.shared_limit),
            record.speed_limit,
        ));
        let services = TransferServices {
            client: self.inner.client.clone(),
            throttle: Arc::clone(&throttle),
            store: self.inner.store.clone(),
        };
        tracing::info!(download = %job.id, "transfer starting");
        self.inner.runtime.spawn(run_transfer(
            job,
            services,
            cancel.clone(),
            self.inner.events.clone(),
        ));
        entry.live = Some(Live {
            cancel,
            throttle,
            bytes_per_second: 0,
            connections: 0,
            merging: false,
        });
    }

    async fn apply_events(
        self,
        mut receiver: mpsc::UnboundedReceiver<(DownloadId, TransferEvent)>,
    ) {
        while let Some((id, event)) = receiver.recv().await {
            let finished = matches!(event, TransferEvent::Finished(_));
            let aftermath = self.apply_event(id, event);
            if let Some(leftovers) = aftermath.leftovers {
                delete_leftovers(leftovers);
            }
            if let Some(record) = aftermath.completed {
                self.record_history(record);
            }
            if finished {
                self.changed_and_persist();
                self.start_queued();
            } else {
                self.inner.dirty.store(true, Ordering::Relaxed);
                self.notify_changed();
            }
        }
    }

    fn apply_event(&self, id: DownloadId, event: TransferEvent) -> Aftermath {
        let mut state = self.state();
        let Some(entry) = state.downloads.get_mut(&id) else {
            return Aftermath::default();
        };
        let record = &mut entry.record;
        let mut aftermath = Aftermath::default();
        match event {
            TransferEvent::Connected {
                file_name,
                target,
                validators,
                progress,
            } => {
                record.file_name = file_name;
                record.target = Some(target);
                record.validators = validators;
                record.progress = progress;
                transition(record, StatusEvent::Connected);
            }
            TransferEvent::Progress {
                progress,
                bytes_per_second,
                connections,
            } => {
                record.progress = progress;
                if let Some(live) = &mut entry.live {
                    live.bytes_per_second = bytes_per_second;
                    live.connections = connections;
                }
            }
            TransferEvent::StreamChosen { variant } => {
                if let DownloadSource::Stream {
                    variant: chosen, ..
                } = &mut record.source
                {
                    *chosen = variant;
                }
            }
            TransferEvent::Merging => {
                if let Some(live) = &mut entry.live {
                    live.merging = true;
                }
            }
            TransferEvent::Finished(outcome) => {
                entry.live = None;
                let after_stop = std::mem::replace(&mut entry.after_stop, AfterStop::Pause);
                match outcome {
                    TransferOutcome::Completed { path } => {
                        record.target = Some(path);
                        record.finished_at_unix = Some(unix_now());
                        transition(record, StatusEvent::Completed);
                        aftermath.completed = Some(record.clone());
                    }
                    TransferOutcome::Stopped => {
                        transition(record, StatusEvent::Paused);
                        if after_stop == AfterStop::Requeue {
                            transition(record, StatusEvent::Enqueued);
                        }
                    }
                    TransferOutcome::Failed { reason } => {
                        tracing::warn!(download = %id, %reason, "download failed");
                        transition(record, StatusEvent::Failed { reason });
                    }
                    TransferOutcome::OutOfSpace {
                        directory,
                        needed_bytes,
                    } => {
                        tracing::warn!(download = %id, directory = %directory.display(), ?needed_bytes, "waiting for disk space");
                        transition(
                            record,
                            StatusEvent::OutOfSpace {
                                directory,
                                needed_bytes,
                            },
                        );
                    }
                }
                if let AfterStop::Remove(files) = after_stop {
                    let entry = state.downloads.remove(&id).expect("entry was just found");
                    aftermath.leftovers = Some(leftovers(&entry.record, files, &self.inner.store));
                }
            }
        }
        aftermath
    }

    /// Adds a finished download to the statistics history.
    fn record_history(&self, record: DownloadRecord) {
        let store = self.inner.store.clone();
        let categories = self.state().settings.categories.clone();
        self.inner.runtime.spawn_blocking(move || {
            let Some(entry) = history_entry(&record, &categories) else {
                return;
            };
            if let Err(error) = store.append_history(&[entry]) {
                tracing::warn!(download = %record.id, %error, "could not record the download in the history");
            }
        });
    }

    /// Re-queues downloads that waited for disk space once their folder has
    /// room again.
    async fn watch_disk_space(self) {
        loop {
            tokio::time::sleep(SPACE_CHECK_INTERVAL).await;
            let waiting: Vec<(DownloadId, PathBuf, Option<u64>)> = self
                .state()
                .downloads
                .values()
                .filter_map(|entry| match &entry.record.status {
                    DownloadStatus::WaitingForSpace {
                        directory,
                        needed_bytes,
                    } => Some((entry.record.id, directory.clone(), *needed_bytes)),
                    _ => None,
                })
                .collect();
            if waiting.is_empty() {
                continue;
            }
            let ready = tokio::task::spawn_blocking(move || {
                waiting
                    .into_iter()
                    .filter(|(_, directory, needed)| disk::has_room_for(directory, *needed))
                    .map(|(id, _, _)| id)
                    .collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            for id in ready {
                tracing::info!(download = %id, "disk space available again");
                if let Err(error) = self.resume(id) {
                    tracing::warn!(download = %id, %error, "could not resume after disk space returned");
                }
            }
        }
    }

    async fn persist_periodically(self) {
        loop {
            tokio::select! {
                _ = self.inner.persist_now.notified() => {}
                _ = tokio::time::sleep(PERSIST_INTERVAL) => {}
            }
            if !self.inner.dirty.swap(false, Ordering::Relaxed) {
                continue;
            }
            let records = self.records();
            let store = self.inner.store.clone();
            let saved = tokio::task::spawn_blocking(move || store.save_downloads(&records)).await;
            match saved {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::error!(%error, "saving downloads failed");
                    self.inner.dirty.store(true, Ordering::Relaxed);
                }
                Err(error) => tracing::error!(%error, "save task panicked"),
            }
        }
    }

    fn changed_and_persist(&self) {
        self.inner.dirty.store(true, Ordering::Relaxed);
        self.inner.persist_now.notify_one();
        self.notify_changed();
    }

    fn notify_changed(&self) {
        let listener = self
            .inner
            .listener
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(listener) = listener {
            listener();
        }
    }
}

/// Applies a transfer-reported event. A rejected transition means the
/// transfer and the state machine disagree, which is a bug worth logging but
/// not worth crashing the app over.
fn transition(record: &mut DownloadRecord, event: StatusEvent) {
    match record.status.apply(event) {
        Ok(status) => record.status = status,
        Err(error) => {
            tracing::error!(download = %record.id, %error, "ignored invalid status transition")
        }
    }
}

fn leftovers(record: &DownloadRecord, files: RemoveFiles, store: &Store) -> Leftovers {
    let mut paths = match (files, &record.target) {
        (RemoveFiles::Delete, Some(target)) => {
            let mut paths = vec![target.clone()];
            paths.extend(unfinished_files(target));
            paths
        }
        // Unfinished pieces are useless without their download entry.
        (RemoveFiles::Keep, Some(target)) if record.status != DownloadStatus::Completed => {
            unfinished_files(target)
        }
        _ => Vec::new(),
    };
    paths.push(store.stream_plan_path(record.id));
    Leftovers(paths)
}

/// The part file and any stream track files that belong to `target`.
fn unfinished_files(target: &std::path::Path) -> Vec<PathBuf> {
    std::iter::once(part_path(target))
        .chain((0..MAX_STREAM_TRACKS).map(|index| track_path(target, index)))
        .collect()
}

fn forget_stream_plan(store: &Store, id: DownloadId) {
    delete_leftovers(Leftovers(vec![store.stream_plan_path(id)]));
}

fn source_for(url: &Url, kind: DownloadKind, default_quality: VideoQuality) -> DownloadSource {
    let (format, quality) = match kind {
        DownloadKind::Stream { format, quality } => (format, quality),
        DownloadKind::Detect => match StreamFormat::from_url(url) {
            Some(format) => (format, None),
            None => return DownloadSource::File,
        },
    };
    DownloadSource::Stream {
        format,
        quality: quality.unwrap_or(default_quality),
        variant: None,
    }
}

fn history_entry(record: &DownloadRecord, categories: &Categories) -> Option<CompletedDownload> {
    // A merged stream's file differs in size from the pieces downloaded.
    let bytes = record
        .target
        .as_ref()
        .and_then(|target| std::fs::metadata(target).ok())
        .map_or_else(
            || record.progress.received_bytes(),
            |metadata| metadata.len(),
        );
    let category = &categories
        .get(categories.classify(&record.file_name.value))
        .name;
    CompletedDownload::from_record(record, bytes, category)
}

/// Starts the history with downloads that completed before it existed.
fn seed_history(store: &Store, records: &[DownloadRecord], categories: &Categories) {
    let entries: Vec<_> = records
        .iter()
        .filter(|record| record.status == DownloadStatus::Completed)
        .filter_map(|record| history_entry(record, categories))
        .collect();
    if let Err(error) = store.append_history(&entries) {
        tracing::warn!(%error, "could not start the download history");
    }
}

fn delete_leftovers(Leftovers(paths): Leftovers) {
    for path in paths {
        match std::fs::remove_file(&path) {
            Ok(()) => tracing::info!(path = %path.display(), "deleted file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(path = %path.display(), %error, "could not delete file"),
        }
    }
}

impl Entry {
    /// Queued and not yet started, so it can start, now or when its turn comes.
    fn waits_in_queue(&self) -> bool {
        self.record.status == DownloadStatus::Queued && self.live.is_none()
    }

    fn view(&self, categories: &Categories) -> DownloadView {
        let record = &self.record;
        let segments = match &record.progress {
            TransferProgress::Segmented { segments, .. } => segments.clone(),
            _ => Vec::new(),
        };
        let stream = match &record.source {
            DownloadSource::File => None,
            DownloadSource::Stream {
                format,
                quality,
                variant,
            } => Some(StreamView {
                format: *format,
                quality: *quality,
                variant: variant.clone(),
                merging: self.live.as_ref().is_some_and(|live| live.merging),
            }),
        };
        DownloadView {
            id: record.id,
            file_name: record.file_name.value.clone(),
            url: record.url.clone(),
            page_url: record.page_url.clone(),
            referrer: record.context.referrer.clone(),
            queue: record.queue,
            directory: record.directory(categories),
            target: record.target.clone(),
            category: categories.classify(&record.file_name.value),
            status: record.status.clone(),
            size: record.progress.total_size(),
            received: record.progress.received_bytes(),
            bytes_per_second: self.live.as_ref().map_or(0, |live| live.bytes_per_second),
            connections: self.live.as_ref().map_or(0, |live| live.connections),
            segments,
            resumable: record.progress.is_resumable(),
            size_is_estimate: record.progress.size_is_estimate(),
            progress_fraction: record.progress.fraction(),
            stream,
            added_at_unix: record.added_at_unix,
            finished_at_unix: record.finished_at_unix,
            stopping: self
                .live
                .as_ref()
                .is_some_and(|live| live.cancel.is_cancelled()),
            speed_limit: record.speed_limit,
            can_start_now: self.waits_in_queue(),
        }
    }
}

impl NewDownload {
    /// A download as requested by the user from the Add URL dialog.
    pub fn from_user(
        url: Url,
        file_name: Option<&str>,
        folder: Option<PathBuf>,
        start: StartMode,
    ) -> Self {
        Self {
            file_name: file_name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| FileName::new(name, FileNameSource::User)),
            url,
            folder,
            context: RequestContext::default(),
            start,
            kind: DownloadKind::Detect,
            queue: None,
            page_url: None,
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_removal_outlasts_later_stop_requests() {
        let removed = AfterStop::Pause.then(AfterStop::Remove(RemoveFiles::Delete));
        assert_eq!(removed, AfterStop::Remove(RemoveFiles::Delete));
        assert_eq!(removed.then(AfterStop::Pause), removed);
        assert_eq!(removed.then(AfterStop::Requeue), removed);
        assert_eq!(
            AfterStop::Pause.then(AfterStop::Requeue),
            AfterStop::Requeue
        );
    }
}
