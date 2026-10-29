//! Download engine for Downpour: segmented, resumable HTTP(S)
//! transfers with a queue, a global speed limit, and JSON persistence.

pub mod category;
pub mod disk;
pub mod history;
pub mod http;
pub mod limiter;
pub mod link_check;
pub mod manager;
pub mod naming;
pub mod ordering;
pub mod paths;
pub mod queue;
pub mod record;
pub mod segment;
pub mod settings;
pub mod speed;
pub mod status;
pub mod store;
pub mod stream;
pub mod transfer;
pub mod units;

pub use category::{Categories, Category, CategoryError, CategoryId, CategoryLook};
pub use http::{RequestContext, ScopedContext};
pub use manager::{
    DownloadKind, DownloadManager, DownloadView, ManagerError, NewDownload, RemoveFiles, StartMode,
    StreamView,
};
pub use naming::{FileName, FileNameSource};
pub use ordering::QueuePlace;
pub use queue::{Queue, QueueError, QueueId, QueueState, Queues};
pub use record::{DownloadId, DownloadRecord};
pub use settings::EngineSettings;
pub use status::DownloadStatus;
pub use store::{Store, StoreError};
