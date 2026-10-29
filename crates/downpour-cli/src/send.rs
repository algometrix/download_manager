//! `downpour-cli send`: hands links to the running Downpour app over the same
//! local connection the browser extension uses. The app then asks or starts
//! them as its settings say.

use std::time::Duration;

use downpour_protocol::ipc::{Endpoint, IpcError};
use downpour_protocol::messages::{AppReply, AppRequest, BrowserDownload};
use url::Url;

use crate::args::SendArgs;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("Downpour is not running. Start it, or download here with `downpour-cli get`.")]
    NotRunning,
    #[error("could not find Downpour's data folder")]
    NoDataDir,
    #[error("could not reach Downpour: {0}")]
    Ipc(IpcError),
    #[error("Downpour refused the links: {0}")]
    Refused(String),
    #[error("Downpour sent an unexpected answer: {0:?}")]
    UnexpectedReply(AppReply),
}

/// Returns how many links the app accepted.
pub fn run(args: &SendArgs, urls: Vec<Url>) -> Result<usize, SendError> {
    let items = urls
        .into_iter()
        .map(|url| BrowserDownload {
            url: url.to_string(),
            file_name: args.file_name.clone(),
            referrer: args.referer.clone(),
            cookies: None,
            user_agent: None,
            mime: None,
            size: None,
            // The app detects streams from the address itself.
            stream: None,
            quality: None,
            title: None,
            page_url: args.referer.clone(),
            origin: None,
            accept_language: None,
        })
        .collect();
    let data_dir = downpour_protocol::data_dir().ok_or(SendError::NoDataDir)?;
    let endpoint = Endpoint::load(&data_dir).map_err(not_running_or)?;
    match endpoint
        .send(AppRequest::Download { items }, REQUEST_TIMEOUT)
        .map_err(not_running_or)?
    {
        AppReply::Accepted { count } => Ok(count),
        AppReply::Error { message } => Err(SendError::Refused(message)),
        other => Err(SendError::UnexpectedReply(other)),
    }
}

fn not_running_or(error: IpcError) -> SendError {
    match error {
        IpcError::NotRunning => SendError::NotRunning,
        other => SendError::Ipc(other),
    }
}
