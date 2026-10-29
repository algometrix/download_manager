use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

use std::io::Write as _;

use crate::history::CompletedDownload;
use crate::queue::Queues;
use crate::record::{DownloadId, DownloadRecord};
use crate::stream::StreamPlan;

const DOWNLOADS_FILE: &str = "downloads.json";
const QUEUES_FILE: &str = "queues.json";
const HISTORY_FILE: &str = "history.jsonl";
/// Stream plans list every piece URL, so each lives in its own file
/// instead of bloating the download list that is saved every few seconds.
const STREAM_PLANS_DIR: &str = "streams";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("{path} is not valid: {source}")]
    Corrupt {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("write {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
}

/// JSON files in the application data directory.
#[derive(Clone, Debug)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(|source| StoreError::Write {
            path: dir.clone(),
            source,
        })?;
        Ok(Self { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn load_downloads(&self) -> Result<Vec<DownloadRecord>, StoreError> {
        Ok(self.load(DOWNLOADS_FILE)?.unwrap_or_default())
    }

    pub fn save_downloads(&self, records: &[DownloadRecord]) -> Result<(), StoreError> {
        self.save(DOWNLOADS_FILE, &records)
    }

    pub fn load_stream_plan(&self, id: DownloadId) -> Result<Option<StreamPlan>, StoreError> {
        self.load(&stream_plan_name(id))
    }

    pub fn save_stream_plan(&self, id: DownloadId, plan: &StreamPlan) -> Result<(), StoreError> {
        let dir = self.dir.join(STREAM_PLANS_DIR);
        std::fs::create_dir_all(&dir).map_err(|source| StoreError::Write { path: dir, source })?;
        self.save(&stream_plan_name(id), plan)
    }

    /// The saved plan of a removed or finished download; it may not exist.
    pub fn stream_plan_path(&self, id: DownloadId) -> PathBuf {
        self.dir.join(stream_plan_name(id))
    }

    pub fn load_queues(&self) -> Result<Option<Queues>, StoreError> {
        self.load(QUEUES_FILE)
    }

    pub fn save_queues(&self, queues: &Queues) -> Result<(), StoreError> {
        self.save(QUEUES_FILE, queues)
    }

    /// Whether any download was ever recorded; the first run seeds the
    /// history from downloads that completed before it existed.
    pub fn has_history(&self) -> bool {
        self.dir.join(HISTORY_FILE).exists()
    }

    /// Appends finished downloads, one JSON object per line.
    pub fn append_history(&self, entries: &[CompletedDownload]) -> Result<(), StoreError> {
        let path = self.dir.join(HISTORY_FILE);
        let mut lines = String::new();
        for entry in entries {
            lines
                .push_str(&serde_json::to_string(entry).expect("history entries always serialize"));
            lines.push('\n');
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut file| file.write_all(lines.as_bytes()))
            .map_err(|source| StoreError::Write { path, source })
    }

    /// Every recorded download. A damaged line (from a crash mid-append)
    /// is skipped rather than losing the rest of the history.
    pub fn load_history(&self) -> Result<Vec<CompletedDownload>, StoreError> {
        let path = self.dir.join(HISTORY_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(StoreError::Read { path, source }),
        };
        let mut entries = Vec::new();
        for (number, line) in text
            .lines()
            .enumerate()
            .filter(|(_, line)| !line.trim().is_empty())
        {
            match serde_json::from_str(line) {
                Ok(entry) => entries.push(entry),
                Err(error) => {
                    tracing::warn!(path = %path.display(), line = number + 1, %error, "skipped a damaged history line")
                }
            }
        }
        Ok(entries)
    }

    /// Returns `None` when the file does not exist yet.
    pub fn load<T: DeserializeOwned>(&self, name: &str) -> Result<Option<T>, StoreError> {
        let path = self.dir.join(name);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(StoreError::Read { path, source }),
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|source| StoreError::Corrupt { path, source })
    }

    /// Writes through a temporary file and rename, so a crash mid-write never
    /// leaves a truncated file behind.
    pub fn save<T: Serialize + ?Sized>(&self, name: &str, value: &T) -> Result<(), StoreError> {
        let path = self.dir.join(name);
        let temporary = self.dir.join(format!("{name}.tmp"));
        let json = serde_json::to_vec_pretty(value).expect("store types always serialize");
        std::fs::write(&temporary, json)
            .and_then(|()| std::fs::rename(&temporary, &path))
            .map_err(|source| StoreError::Write { path, source })
    }

    /// Moves an unreadable file aside so the next save does not destroy it.
    pub fn quarantine(&self, name: &str) -> io::Result<PathBuf> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let aside = self.dir.join(format!("{name}.corrupt-{stamp}"));
        std::fs::rename(self.dir.join(name), &aside)?;
        Ok(aside)
    }

    pub fn downloads_file_name() -> &'static str {
        DOWNLOADS_FILE
    }
}

fn stream_plan_name(id: DownloadId) -> String {
    format!("{STREAM_PLANS_DIR}/{}.json", id.0)
}
