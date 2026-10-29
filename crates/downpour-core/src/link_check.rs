//! Checking whether pasted links still work, by asking each server for the
//! first byte: that shows whether the file exists, its size, and its name,
//! without downloading it.

use std::sync::Arc;

use reqwest::header::{self, HeaderValue};
use reqwest::{Client, StatusCode};
use url::Url;

use crate::http::{describe_status, parse_content_range, RequestContext, TransferError};
use crate::naming::{file_name_from_content_disposition, FileName, FileNameSource};

/// Links checked at the same time; servers throttle bursts.
const CHECK_CONCURRENCY: usize = 4;

/// What checking a link found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkStatus {
    /// The file can be downloaded. The name says where it came from, so
    /// a guess from the URL can still give way to the server's name later.
    Online { name: FileName, size: Option<u64> },
    /// The server says the file is gone (deleted, expired, or never existed).
    Offline { reason: String },
    /// The check itself failed (network, unexpected answer, a web page).
    Unknown { reason: String },
}

impl LinkStatus {
    pub fn is_online(&self) -> bool {
        matches!(self, LinkStatus::Online { .. })
    }
}

/// One pasted link and what checking it found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedLink {
    pub url: Url,
    pub status: LinkStatus,
}

/// Finds every http(s) link in pasted text (one per line, or mixed with
/// other text), without duplicates, in the order they appear.
pub fn links_in(text: &str) -> Vec<Url> {
    let mut links: Vec<Url> = Vec::new();
    for word in
        text.split(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>')
    {
        let word = word.trim_matches(|c: char| matches!(c, ',' | ';' | ')' | '(' | '[' | ']'));
        let Some(start) = word.find("http://").or_else(|| word.find("https://")) else {
            continue;
        };
        if let Ok(url) = Url::parse(&word[start..]) {
            if url.host_str().is_some() && !links.contains(&url) {
                links.push(url);
            }
        }
    }
    links
}

/// Checks every link, a few at a time, keeping their order.
pub async fn check_all(
    client: &Client,
    links: Vec<Url>,
    context: RequestContext,
) -> Vec<CheckedLink> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(CHECK_CONCURRENCY));
    let mut checks = Vec::with_capacity(links.len());
    for url in links {
        let client = client.clone();
        let context = context.clone();
        let semaphore = Arc::clone(&semaphore);
        let checked_url = url.clone();
        checks.push((
            checked_url,
            tokio::spawn(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .expect("the semaphore is never closed");
                check(&client, &url, &context).await
            }),
        ));
    }
    let mut checked = Vec::with_capacity(checks.len());
    // One result per link, in order, even when a check itself failed.
    for (url, check) in checks {
        let status = check.await.unwrap_or_else(|error| LinkStatus::Unknown {
            reason: format!("the check stopped unexpectedly ({error})"),
        });
        checked.push(CheckedLink { url, status });
    }
    checked
}

/// Checks one link by asking for its first byte.
async fn check(client: &Client, url: &Url, context: &RequestContext) -> LinkStatus {
    let mut headers = context.scoped_to(url).headers_for(url);
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=0-0"));
    let response = match client.get(url.clone()).headers(headers).send().await {
        Ok(response) => response,
        Err(error) => {
            return LinkStatus::Unknown {
                reason: TransferError::from(error).to_string(),
            }
        }
    };
    let status = response.status();
    if matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE) {
        return LinkStatus::Offline {
            reason: format!("the server answered {}", describe_status(status)),
        };
    }
    if !status.is_success() {
        return LinkStatus::Unknown {
            reason: format!("the server answered {}", describe_status(status)),
        };
    }
    let text = |name| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    if text(header::CONTENT_TYPE).is_some_and(|kind| kind.starts_with("text/html")) {
        // A web page, not a file: usually a download page that only the
        // browser can get through to reach the file.
        return LinkStatus::Unknown {
            reason: "this is a web page, not a file".to_owned(),
        };
    }
    let size = match status {
        StatusCode::PARTIAL_CONTENT => text(header::CONTENT_RANGE)
            .as_deref()
            .and_then(parse_content_range)
            .and_then(|range| range.total),
        _ => response.content_length(),
    };
    let name = text(header::CONTENT_DISPOSITION)
        .as_deref()
        .and_then(file_name_from_content_disposition)
        .map(|name| FileName::new(&name, FileNameSource::Server))
        .unwrap_or_else(|| FileName::guess_from_url(response.url()));
    LinkStatus::Online { name, size }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_links_in_pasted_text() {
        let text = "Part 1: https://example.org/file/abc123/movie.part1.rar\n\
                    https://example.org/file/abc123/movie.part1.rar (duplicate)\n\
                    see <https://example.com/a.zip>, and ftp://ignored.example/x";
        let links = links_in(text);
        assert_eq!(links.len(), 2);
        assert_eq!(links[1].as_str(), "https://example.com/a.zip");
    }
}
