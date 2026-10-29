//! `downpour-cli get`: downloads in this terminal with its own engine.
//!
//! The engine needs a place for its state. This run keeps it in a scratch
//! folder that is deleted at the end, so the app's own list is never
//! touched. Without that state a download cannot resume in a later run, so
//! unfinished pieces are removed too, instead of being left behind.

use std::collections::{HashMap, HashSet};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use downpour_core::{
    DownloadId, DownloadKind, DownloadManager, EngineSettings, FileName, FileNameSource,
    NewDownload, QueueId, RemoveFiles, RequestContext, StartMode, Store,
};
use url::Url;

use crate::args::GetArgs;
use crate::progress::{self, Outcome};

const TERMINAL_REFRESH: Duration = Duration::from_millis(250);
/// When output goes to a file or pipe, a status line this often is plenty.
const LOG_REFRESH: Duration = Duration::from_secs(5);
/// How long Ctrl+C waits for transfers to close their files.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum GetError {
    #[error("could not use the folder {path}: {source}")]
    Folder {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not prepare the engine's scratch folder: {0}")]
    Scratch(std::io::Error),
    #[error("could not start the download engine: {0}")]
    Engine(#[from] downpour_core::ManagerError),
    #[error("could not open the engine's state: {0}")]
    Store(#[from] downpour_core::StoreError),
    #[error("could not start the async runtime: {0}")]
    Runtime(std::io::Error),
}

/// How the run ended, for the exit code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunResult {
    AllSaved,
    SomeFailed,
    Interrupted,
}

/// A download this run added, with the name it is reported under.
struct Tracked {
    id: DownloadId,
    name: String,
}

pub fn run(args: GetArgs, urls: Vec<Url>) -> Result<RunResult, GetError> {
    let folder = prepare_folder(&args.output)?;
    let scratch = ScratchFolder::create()?;
    let runtime = tokio::runtime::Runtime::new().map_err(GetError::Runtime)?;
    let manager = DownloadManager::start(
        Store::open(scratch.path())?,
        Vec::new(),
        settings_for(&args, folder),
        runtime.handle().clone(),
    )?;
    manager.set_queue_limit(QueueId::MAIN, args.parallel)?;

    let result = runtime.block_on(download_all(&manager, &args, urls));
    drop(manager);
    // Engine tasks still winding down must not outlive the scratch folder.
    runtime.shutdown_timeout(STOP_TIMEOUT);
    drop(scratch);
    Ok(result)
}

fn prepare_folder(output: &Path) -> Result<PathBuf, GetError> {
    let folder_error = |source| GetError::Folder {
        path: output.to_path_buf(),
        source,
    };
    std::fs::create_dir_all(output).map_err(folder_error)?;
    let absolute = std::path::absolute(output).map_err(folder_error)?;
    Ok(downpour_core::paths::native(&absolute))
}

fn settings_for(args: &GetArgs, folder: PathBuf) -> EngineSettings {
    EngineSettings {
        download_dir: folder,
        organize_by_category: false,
        connections_per_download: args.connections,
        max_active_downloads: args.parallel,
        speed_limit_kib_per_second: args.limit.map(|limit| limit.0),
        video_quality: args.quality,
        ffmpeg_path: args.ffmpeg.clone(),
        ..EngineSettings::default()
    }
}

async fn download_all(manager: &DownloadManager, args: &GetArgs, urls: Vec<Url>) -> RunResult {
    let mut failed = 0usize;
    let mut tracked = Vec::new();
    for url in urls {
        match manager.add(request_for(args, url.clone())) {
            Ok(id) => tracked.push(Tracked {
                id,
                name: url.to_string(),
            }),
            Err(error) => {
                eprintln!("Failed  {url}: {error}");
                failed += 1;
            }
        }
    }
    let total = tracked.len() + failed;
    let display = Display::new(args.silent);
    let mut finished: HashSet<DownloadId> = HashSet::new();
    let mut interrupt = std::pin::pin!(tokio::signal::ctrl_c());

    loop {
        let views: HashMap<DownloadId, downpour_core::DownloadView> = manager
            .snapshot()
            .into_iter()
            .map(|view| (view.id, view))
            .collect();
        for download in &tracked {
            let Some(view) = views.get(&download.id) else {
                continue;
            };
            if finished.contains(&download.id) {
                continue;
            }
            if let Some(outcome) = progress::outcome(view) {
                finished.insert(download.id);
                if matches!(outcome, Outcome::Failed { .. }) {
                    failed += 1;
                }
                display.finish_line(&progress::outcome_line(&view.file_name, &outcome));
            }
        }
        if finished.len() == tracked.len() {
            display.clear();
            break;
        }
        let unfinished: Vec<&downpour_core::DownloadView> = tracked
            .iter()
            .filter(|download| !finished.contains(&download.id))
            .filter_map(|download| views.get(&download.id))
            .collect();
        display.status(&progress::status_line(
            &unfinished,
            total - unfinished.len(),
            total,
        ));

        tokio::select! {
            _ = tokio::time::sleep(display.refresh()) => {}
            _ = &mut interrupt => {
                display.clear();
                eprintln!("Stopping…");
                let unfinished_ids: Vec<DownloadId> = tracked
                    .iter()
                    .map(|download| download.id)
                    .filter(|id| !finished.contains(id))
                    .collect();
                forget(manager, &unfinished_ids).await;
                eprintln!("Stopped. Unfinished downloads were removed; finished files are kept.");
                return RunResult::Interrupted;
            }
        }
    }

    // Failed downloads leave pieces that no later run can resume.
    let ids: Vec<DownloadId> = tracked.iter().map(|download| download.id).collect();
    forget(manager, &ids).await;
    tracing::debug!(names = ?tracked.iter().map(|d| &d.name).collect::<Vec<_>>(), "run finished");
    if failed == 0 {
        RunResult::AllSaved
    } else {
        RunResult::SomeFailed
    }
}

fn request_for(args: &GetArgs, url: Url) -> NewDownload {
    let request = &args.request;
    NewDownload {
        url,
        file_name: args
            .file_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(|name| FileName::new(name, FileNameSource::User)),
        folder: None,
        context: RequestContext {
            referrer: request.referer.clone(),
            user_agent: request.user_agent.clone(),
            cookies: request.cookie.clone(),
            origin: request.origin.clone(),
            accept_language: None,
        },
        start: StartMode::Queue,
        kind: DownloadKind::Detect,
        queue: None,
        page_url: request
            .referer
            .as_deref()
            .and_then(|page| Url::parse(page).ok()),
    }
}

/// Removes the downloads from the engine, keeping finished files, and
/// waits (bounded) until running ones have closed their files.
async fn forget(manager: &DownloadManager, ids: &[DownloadId]) {
    for id in ids {
        if let Err(error) = manager.remove(*id, RemoveFiles::Keep) {
            tracing::warn!(download = %id, %error, "could not remove a download from the engine");
        }
    }
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        let remaining = manager
            .snapshot()
            .iter()
            .filter(|view| ids.contains(&view.id))
            .count();
        if remaining == 0 {
            return;
        }
        if Instant::now() >= deadline {
            eprintln!(
                "{remaining} download(s) did not stop in time; their pieces may be left behind."
            );
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Where progress goes: a line rewritten in place on a terminal, an
/// occasional line in a log, or nothing with --silent. Results always print.
struct Display {
    mode: DisplayMode,
    /// Width of the status line on screen, to blank it out before a result.
    shown: std::cell::Cell<usize>,
    last_logged: std::cell::Cell<Option<Instant>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DisplayMode {
    Terminal,
    Log,
    Silent,
}

impl Display {
    fn new(silent: bool) -> Self {
        let mode = match (silent, std::io::stderr().is_terminal()) {
            (true, _) => DisplayMode::Silent,
            (false, true) => DisplayMode::Terminal,
            (false, false) => DisplayMode::Log,
        };
        Self {
            mode,
            shown: std::cell::Cell::new(0),
            last_logged: std::cell::Cell::new(None),
        }
    }

    fn refresh(&self) -> Duration {
        match self.mode {
            DisplayMode::Terminal => TERMINAL_REFRESH,
            DisplayMode::Log | DisplayMode::Silent => TERMINAL_REFRESH * 2,
        }
    }

    fn status(&self, line: &str) {
        match self.mode {
            DisplayMode::Silent => {}
            DisplayMode::Terminal => {
                let width = line.chars().count();
                let padding = self.shown.get().saturating_sub(width);
                eprint!("\r{line}{}", " ".repeat(padding));
                let _ = std::io::stderr().flush();
                self.shown.set(width);
            }
            DisplayMode::Log => {
                let due = self
                    .last_logged
                    .get()
                    .is_none_or(|at| at.elapsed() >= LOG_REFRESH);
                if due {
                    eprintln!("{line}");
                    self.last_logged.set(Some(Instant::now()));
                }
            }
        }
    }

    fn clear(&self) {
        if self.mode == DisplayMode::Terminal && self.shown.get() > 0 {
            eprint!("\r{}\r", " ".repeat(self.shown.get()));
            self.shown.set(0);
        }
    }

    /// Results go to stdout, so `downpour-cli get … > saved.txt` collects them.
    fn finish_line(&self, line: &str) {
        self.clear();
        println!("{line}");
    }
}

/// A fresh folder for the engine's state, deleted when dropped.
struct ScratchFolder(PathBuf);

impl ScratchFolder {
    fn create() -> Result<Self, GetError> {
        let mut suffix = [0u8; 8];
        getrandom::fill(&mut suffix)
            .map_err(|error| GetError::Scratch(std::io::Error::other(error.to_string())))?;
        let name: String = suffix.iter().map(|byte| format!("{byte:02x}")).collect();
        let path = std::env::temp_dir().join(format!("downpour-cli-{name}"));
        std::fs::create_dir(&path).map_err(GetError::Scratch)?;
        Ok(Self(path))
    }

    fn path(&self) -> PathBuf {
        self.0.clone()
    }
}

impl Drop for ScratchFolder {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            tracing::warn!(path = %self.0.display(), %error, "could not remove the scratch folder");
        }
    }
}
