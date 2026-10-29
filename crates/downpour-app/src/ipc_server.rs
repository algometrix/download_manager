//! Loopback listener the native host talks to (see `downpour_protocol::ipc`).

use std::io::{self, BufReader};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use downpour_protocol::ipc::{
    read_line, tokens_match, write_line, Endpoint, Envelope, IpcError, ENDPOINT_FILE,
};
use downpour_protocol::messages::{AppReply, AppRequest, BrowserDownload};

use crate::browser_link::BrowserLink;
use crate::inbox::{Inbox, IncomingRequest};

const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);

/// Deletes `ipc.json` when the app exits, but only if it is still ours: a
/// newer instance may have replaced it.
pub struct EndpointFileGuard {
    path: PathBuf,
    endpoint: Endpoint,
}

impl Drop for EndpointFileGuard {
    fn drop(&mut self) {
        let still_ours = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|text| serde_json::from_str::<Endpoint>(&text).ok())
            .is_some_and(|current| current == self.endpoint);
        if still_ours {
            if let Err(error) = std::fs::remove_file(&self.path) {
                tracing::warn!(%error, "could not remove the ipc endpoint file");
            }
        }
    }
}

pub fn start(data_dir: &Path, inbox: Arc<Inbox>) -> io::Result<EndpointFileGuard> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let endpoint = Endpoint {
        port: listener.local_addr()?.port(),
        token: random_token()?,
        pid: std::process::id(),
    };
    let path = data_dir.join(ENDPOINT_FILE);
    let temporary = data_dir.join(format!("{ENDPOINT_FILE}.tmp"));
    std::fs::write(&temporary, serde_json::to_vec(&endpoint)?)?;
    std::fs::rename(&temporary, &path)?;
    tracing::info!(port = endpoint.port, "browser integration listening");

    let token = endpoint.token.clone();
    std::thread::Builder::new()
        .name("downpour-ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let token = token.clone();
                        let inbox = Arc::clone(&inbox);
                        let spawned = std::thread::Builder::new()
                            .name("downpour-ipc-client".into())
                            .spawn(move || serve_client(stream, &token, &inbox));
                        if let Err(error) = spawned {
                            tracing::warn!(%error, "could not start an ipc client thread");
                        }
                    }
                    Err(error) => tracing::warn!(%error, "ipc accept failed"),
                }
            }
        })?;
    Ok(EndpointFileGuard { path, endpoint })
}

fn random_token() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn serve_client(stream: TcpStream, token: &str, inbox: &Inbox) {
    if let Err(error) = try_serve_client(stream, token, inbox) {
        tracing::warn!(%error, "ipc request failed");
    }
}

fn try_serve_client(mut stream: TcpStream, token: &str, inbox: &Inbox) -> Result<(), IpcError> {
    stream.set_read_timeout(Some(CLIENT_TIMEOUT))?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT))?;
    let Some(envelope) = read_line::<Envelope>(&mut BufReader::new(&stream))? else {
        return Ok(());
    };
    let reply = if tokens_match(token, &envelope.token) {
        handle(envelope.request, inbox)
    } else {
        tracing::warn!("rejected ipc request with a wrong token");
        AppReply::Error {
            message: "not authorized".into(),
        }
    };
    write_line(&mut stream, &reply)
}

fn handle(request: AppRequest, inbox: &Inbox) -> AppReply {
    match request {
        AppRequest::Ping => AppReply::Pong {
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        AppRequest::Show => {
            inbox.push(IncomingRequest::Show);
            AppReply::Shown
        }
        AppRequest::Quit => {
            tracing::info!("asked to quit (by the installer)");
            inbox.push(IncomingRequest::Quit);
            AppReply::Quitting
        }
        AppRequest::Download { items } => {
            let total = items.len();
            let accepted: Vec<BrowserLink> = items.into_iter().filter_map(downloadable).collect();
            let rejected = total - accepted.len();
            if accepted.is_empty() {
                return AppReply::Error {
                    message: format!("none of the {rejected} links can be downloaded"),
                };
            }
            let count = accepted.len();
            tracing::info!(count, rejected, "received downloads from the browser");
            inbox.push(IncomingRequest::Downloads(accepted));
            AppReply::Accepted { count }
        }
    }
}

/// The link, if it is a web address Downpour can download.
fn downloadable(item: BrowserDownload) -> Option<BrowserLink> {
    BrowserLink::parse(item)
        .ok()
        .filter(|link| matches!(link.url().scheme(), "http" | "https"))
}
