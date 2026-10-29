use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use downpour_protocol::ipc::{Endpoint, IpcError};
use downpour_protocol::messages::{AppReply, AppRequest, ExtensionRequest, HostReply};

const PING_TIMEOUT: Duration = Duration::from_secs(1);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);
const LAUNCH_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub fn handle(request: ExtensionRequest) -> HostReply {
    match request {
        ExtensionRequest::Ping => HostReply::Status {
            app_running: matches!(
                send_to_running_app(AppRequest::Ping, PING_TIMEOUT),
                Ok(AppReply::Pong { .. })
            ),
            host_version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        ExtensionRequest::Show => reply_from(deliver(AppRequest::Show)),
        ExtensionRequest::Download { items } if items.is_empty() => {
            HostReply::Accepted { count: 0 }
        }
        ExtensionRequest::Download { items } => reply_from(deliver(AppRequest::Download { items })),
    }
}

fn reply_from(result: Result<AppReply, DeliveryError>) -> HostReply {
    match result {
        Ok(AppReply::Accepted { count }) => HostReply::Accepted { count },
        Ok(AppReply::Shown) => HostReply::Shown,
        Ok(AppReply::Error { message }) => HostReply::Error { message },
        Ok(AppReply::Pong { .. } | AppReply::Quitting) => HostReply::Error {
            message: "the app answered with an unexpected reply".into(),
        },
        Err(error) => {
            tracing::error!(%error, "delivery to the app failed");
            HostReply::Error {
                message: error.to_string(),
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum DeliveryError {
    #[error(transparent)]
    Ipc(#[from] IpcError),
    #[error("cannot find the per-user data directory")]
    NoDataDir,
    #[error("cannot start {path}: {source}")]
    Launch {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("the app did not start within {LAUNCH_TIMEOUT:?}")]
    LaunchTimedOut,
}

fn data_dir() -> Result<PathBuf, DeliveryError> {
    downpour_protocol::data_dir().ok_or(DeliveryError::NoDataDir)
}

fn send_to_running_app(request: AppRequest, timeout: Duration) -> Result<AppReply, DeliveryError> {
    Ok(Endpoint::load(&data_dir()?)?.send(request, timeout)?)
}

/// Sends the request, starting the app first if it is not running.
fn deliver(request: AppRequest) -> Result<AppReply, DeliveryError> {
    match send_to_running_app(request.clone(), REQUEST_TIMEOUT) {
        Err(DeliveryError::Ipc(IpcError::NotRunning)) => {}
        other => return other,
    }
    launch_app()?;
    let deadline = Instant::now() + LAUNCH_TIMEOUT;
    while Instant::now() < deadline {
        std::thread::sleep(LAUNCH_POLL_INTERVAL);
        match send_to_running_app(request.clone(), REQUEST_TIMEOUT) {
            Err(DeliveryError::Ipc(IpcError::NotRunning)) => continue,
            other => return other,
        }
    }
    Err(DeliveryError::LaunchTimedOut)
}

fn app_path() -> Result<PathBuf, DeliveryError> {
    let host = std::env::current_exe().map_err(|source| DeliveryError::Launch {
        path: "downpour-host".into(),
        source,
    })?;
    let name = if cfg!(windows) {
        "downpour.exe"
    } else {
        "downpour"
    };
    Ok(host.with_file_name(name))
}

/// Starts the app detached from the host, which the browser kills when the
/// extension's port closes.
fn launch_app() -> Result<(), DeliveryError> {
    let path = app_path()?;
    tracing::info!(path = %path.display(), "starting the app");
    let spawn = |detach: Detach| {
        let mut command = Command::new(&path);
        command
            .arg(downpour_protocol::FROM_BROWSER_FLAG)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        detach.apply(&mut command);
        command.spawn()
    };
    match spawn(Detach::BreakAwayFromJob) {
        Ok(_) => Ok(()),
        // Browsers that do not permit breaking away from their job object
        // reject the flag; the app then lives as long as the browser allows.
        Err(_) => spawn(Detach::Plain)
            .map(drop)
            .map_err(|source| DeliveryError::Launch { path, source }),
    }
}

#[derive(Clone, Copy)]
enum Detach {
    BreakAwayFromJob,
    Plain,
}

impl Detach {
    #[cfg(windows)]
    fn apply(self, command: &mut Command) {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
        command.creation_flags(match self {
            Detach::BreakAwayFromJob => base | CREATE_BREAKAWAY_FROM_JOB,
            Detach::Plain => base,
        });
    }

    #[cfg(not(windows))]
    fn apply(self, _command: &mut Command) {}
}
