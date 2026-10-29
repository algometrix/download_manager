//! HTTP requests for probing a resource and fetching byte ranges.

use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{Client, Response, StatusCode};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::disk::NoSpace;
use crate::naming::file_name_from_content_disposition;

/// Browser context needed to fetch a resource the way the page did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContext {
    pub referrer: Option<String>,
    pub user_agent: Option<String>,
    /// `Cookie` header value captured by the browser extension.
    pub cookies: Option<String>,
    /// `Origin` the page's player sent. Video CDNs often check it on
    /// playlist and segment requests.
    #[serde(default)]
    pub origin: Option<String>,
    /// Some CDNs refuse requests without `Accept-Language`, which every
    /// browser sends.
    #[serde(default)]
    pub accept_language: Option<String>,
}

/// Values that let a resumed transfer prove the remote file is unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl Validators {
    /// Strong ETags are preferred; weak ETags are not allowed in `If-Range`.
    fn if_range(&self) -> Option<&str> {
        self.etag
            .as_deref()
            .filter(|etag| !etag.starts_with("W/"))
            .or(self.last_modified.as_deref())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    /// Built only through `From`, which drops the request URL: signed links
    /// and tokens live in query strings, and this text is saved, logged, and
    /// shown.
    #[error("network error: {}", describe_network_error(.0))]
    Network(#[source] reqwest::Error),
    #[error("server answered {}", describe_status(*.0))]
    HttpStatus(StatusCode),
    #[error("server ignored the requested byte range")]
    RangeNotHonored,
    #[error("the file changed on the server since the download started")]
    RemoteFileChanged,
    #[error("server sent fewer bytes than announced")]
    TruncatedBody,
    #[error("disk error: {0}")]
    Disk(#[from] std::io::Error),
    #[error(transparent)]
    NoSpace(#[from] NoSpace),
}

/// An address as it may appear in logs: scheme, host, port, and path.
/// Query strings, fragments, and user info often carry tokens or signed
/// parameters, so they are left out.
pub fn url_for_logs(url: &Url) -> String {
    let host = url.host_str().unwrap_or("");
    match url.port() {
        Some(port) => format!("{}://{host}:{port}{}", url.scheme(), url.path()),
        None => format!("{}://{host}{}", url.scheme(), url.path()),
    }
}

impl From<reqwest::Error> for TransferError {
    fn from(error: reqwest::Error) -> Self {
        TransferError::Network(error.without_url())
    }
}

/// HTTP defines codes up to 599. CDNs occasionally send others (one file
/// server was seen answering 618 mid-download); they are glitches
/// worth retrying, not a verdict on the file.
const LAST_STANDARD_STATUS: u16 = 599;

fn is_nonstandard(status: StatusCode) -> bool {
    status.as_u16() > LAST_STANDARD_STATUS
}

pub(crate) fn describe_status(status: StatusCode) -> String {
    if is_nonstandard(status) {
        format!(
            "{} (not a standard HTTP status; usually a temporary server problem)",
            status.as_u16()
        )
    } else {
        status.to_string()
    }
}

/// reqwest's own message ("error sending request for url ...") omits why;
/// the innermost source (a refused connection, a TLS failure) is the useful part.
fn describe_network_error(error: &reqwest::Error) -> String {
    let mut innermost: &dyn std::error::Error = error;
    while let Some(source) = innermost.source() {
        innermost = source;
    }
    if std::ptr::addr_eq(innermost, error as &dyn std::error::Error) {
        error.to_string()
    } else {
        format!("{error} ({innermost})")
    }
}

impl TransferError {
    /// Whether retrying the same request may succeed.
    pub fn is_transient(&self) -> bool {
        match self {
            TransferError::Network(_) | TransferError::TruncatedBody => true,
            TransferError::HttpStatus(status) => {
                status.is_server_error()
                    || *status == StatusCode::REQUEST_TIMEOUT
                    || *status == StatusCode::TOO_MANY_REQUESTS
                    || is_nonstandard(*status)
            }
            TransferError::RangeNotHonored
            | TransferError::RemoteFileChanged
            | TransferError::Disk(_)
            | TransferError::NoSpace(_) => false,
        }
    }

    /// The server refused the address itself (403, 404, 410). Right after
    /// a start that means the link is wrong or expired; on an address that
    /// is already serving the download it is usually a connection cap.
    pub fn is_refusal(&self) -> bool {
        matches!(
            self,
            TransferError::HttpStatus(
                StatusCode::FORBIDDEN | StatusCode::NOT_FOUND | StatusCode::GONE
            )
        )
    }

    /// The server is refusing more concurrent requests from this client.
    pub fn is_connection_limit(&self) -> bool {
        matches!(
            self,
            TransferError::HttpStatus(
                StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
            )
        )
    }
}

/// How the transfer can proceed, decided from the probe response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferPlan {
    /// Server supports ranges and announced a total size.
    Segmented { size: u64 },
    /// Single sequential stream; cannot resume or use extra connections.
    Stream { size: Option<u64> },
}

/// The first response, kept open so its body feeds the first connection.
pub struct Probe {
    pub response: Response,
    pub final_url: Url,
    pub plan: TransferPlan,
    pub server_file_name: Option<String>,
    pub validators: Validators,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentRange {
    pub start: u64,
    pub end_inclusive: u64,
    pub total: Option<u64>,
}

/// Parses `bytes START-END/TOTAL` where TOTAL may be `*`.
pub fn parse_content_range(value: &str) -> Option<ContentRange> {
    let rest = value.trim().strip_prefix("bytes")?.trim_start();
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end_inclusive: u64 = end.trim().parse().ok()?;
    if end_inclusive < start {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        digits => Some(digits.parse().ok()?),
    };
    Some(ContentRange {
        start,
        end_inclusive,
        total,
    })
}

impl RequestContext {
    /// Binds the context to the address the browser handed over, whose
    /// origin its cookies were captured for.
    pub fn scoped_to(&self, handed_over: &Url) -> ScopedContext {
        ScopedContext {
            context: self.clone(),
            cookie_origin: handed_over.origin(),
        }
    }
}

/// A request context that knows where its cookies may go.
///
/// Downloads end up on other hosts: redirects, a direct link on a CDN,
/// stream pieces on a third-party CDN. The browser would never send the
/// page's cookies there, so neither does Downpour.
#[derive(Clone, Debug)]
pub struct ScopedContext {
    context: RequestContext,
    cookie_origin: url::Origin,
}

impl ScopedContext {
    pub fn context(&self) -> &RequestContext {
        &self.context
    }

    /// The headers for a request to `target`.
    pub(crate) fn headers_for(&self, target: &Url) -> HeaderMap {
        let mut headers = HeaderMap::new();
        // Compressed transfer encodings make byte offsets meaningless for ranges.
        headers.insert(
            header::ACCEPT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        let context = &self.context;
        let cookies = context
            .cookies
            .as_ref()
            .filter(|_| target.origin() == self.cookie_origin);
        let optional = [
            (header::REFERER, context.referrer.as_ref()),
            (header::USER_AGENT, context.user_agent.as_ref()),
            (header::COOKIE, cookies),
            (header::ORIGIN, context.origin.as_ref()),
            (header::ACCEPT_LANGUAGE, context.accept_language.as_ref()),
        ];
        for (name, value) in optional {
            let Some(text) = value else {
                continue;
            };
            match HeaderValue::from_str(text) {
                Ok(value) => {
                    headers.insert(name, value);
                }
                // The text itself may be a cookie, so only the name is logged.
                Err(_) => {
                    tracing::debug!(header = %name, "skipped a header value that is not valid HTTP")
                }
            }
        }
        headers
    }
}

fn header_text(response: &Response, name: header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)?
        .to_str()
        .ok()
        .map(str::to_owned)
}

/// Opens the resource with `Range: bytes=0-` so one round trip reveals the
/// size, range support, file name, and validators.
pub async fn probe(
    client: &Client,
    url: &Url,
    context: &ScopedContext,
) -> Result<Probe, TransferError> {
    let response = client
        .get(url.clone())
        .headers(context.headers_for(url))
        .header(header::RANGE, "bytes=0-")
        .send()
        .await?;

    let status = response.status();
    let plan = match status {
        StatusCode::PARTIAL_CONTENT => {
            let range = header_text(&response, header::CONTENT_RANGE)
                .as_deref()
                .and_then(parse_content_range)
                .ok_or(TransferError::RangeNotHonored)?;
            if range.start != 0 {
                return Err(TransferError::RangeNotHonored);
            }
            match range.total {
                Some(size) => TransferPlan::Segmented { size },
                None => TransferPlan::Stream { size: None },
            }
        }
        StatusCode::OK => TransferPlan::Stream {
            size: response.content_length(),
        },
        other => return Err(TransferError::HttpStatus(other)),
    };

    Ok(Probe {
        final_url: response.url().clone(),
        server_file_name: header_text(&response, header::CONTENT_DISPOSITION)
            .as_deref()
            .and_then(file_name_from_content_disposition),
        validators: Validators {
            etag: header_text(&response, header::ETAG),
            last_modified: header_text(&response, header::LAST_MODIFIED),
        },
        plan,
        response,
    })
}

/// Requests `[start, end)` and verifies the server honored exactly that start.
/// With validators, a changed remote file is reported instead of silently
/// mixing bytes from two versions.
pub async fn fetch_range(
    client: &Client,
    url: &Url,
    context: &ScopedContext,
    validators: &Validators,
    start: u64,
    end: u64,
) -> Result<Response, TransferError> {
    let mut request = client
        .get(url.clone())
        .headers(context.headers_for(url))
        .header(header::RANGE, format!("bytes={start}-{}", end - 1));
    if let Some(if_range) = validators.if_range() {
        request = request.header(header::IF_RANGE, if_range);
    }
    let response = request.send().await?;

    match response.status() {
        StatusCode::PARTIAL_CONTENT => {}
        StatusCode::OK if validators.if_range().is_some() => {
            return Err(TransferError::RemoteFileChanged)
        }
        StatusCode::OK => return Err(TransferError::RangeNotHonored),
        other => return Err(TransferError::HttpStatus(other)),
    }
    let range = header_text(&response, header::CONTENT_RANGE)
        .as_deref()
        .and_then(parse_content_range)
        .ok_or(TransferError::RangeNotHonored)?;
    if range.start != start {
        return Err(TransferError::RangeNotHonored);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_cookie() -> ScopedContext {
        RequestContext {
            cookies: Some("session=abc".into()),
            referrer: Some("https://files.example/page".into()),
            ..RequestContext::default()
        }
        .scoped_to(&Url::parse("https://files.example/get/1").unwrap())
    }

    #[test]
    fn logged_addresses_leave_out_queries_and_credentials() {
        let signed = Url::parse("https://me:pw@cdn.example:8443/f/a.zip?token=s3cret#x").unwrap();
        assert_eq!(url_for_logs(&signed), "https://cdn.example:8443/f/a.zip");
    }

    #[test]
    fn cookies_go_only_to_the_origin_they_came_from() {
        let context = with_cookie();
        let same = context.headers_for(&Url::parse("https://files.example/other").unwrap());
        assert_eq!(same.get(header::COOKIE).unwrap(), "session=abc");
        for elsewhere in [
            "https://cdn.example/piece.ts",
            "http://files.example/get/1",
            "https://files.example:8443/get/1",
            "https://sub.files.example/get/1",
        ] {
            let headers = context.headers_for(&Url::parse(elsewhere).unwrap());
            assert!(
                headers.get(header::COOKIE).is_none(),
                "cookie sent to {elsewhere}"
            );
            // The rest of the browser context still goes everywhere.
            assert!(headers.get(header::REFERER).is_some());
        }
    }

    #[test]
    fn parses_content_range() {
        assert_eq!(
            parse_content_range("bytes 0-99/1000"),
            Some(ContentRange {
                start: 0,
                end_inclusive: 99,
                total: Some(1000)
            })
        );
        assert_eq!(parse_content_range("bytes 5-9/*").unwrap().total, None);
        assert_eq!(parse_content_range("bytes 9-5/10"), None);
        assert_eq!(parse_content_range("items 0-1/2"), None);
    }

    #[test]
    fn if_range_skips_weak_etags() {
        let validators = Validators {
            etag: Some("W/\"x\"".into()),
            last_modified: Some("Tue".into()),
        };
        assert_eq!(validators.if_range(), Some("Tue"));
        let strong = Validators {
            etag: Some("\"x\"".into()),
            last_modified: None,
        };
        assert_eq!(strong.if_range(), Some("\"x\""));
    }

    #[test]
    fn classifies_transient_errors() {
        assert!(TransferError::HttpStatus(StatusCode::SERVICE_UNAVAILABLE).is_transient());
        assert!(TransferError::HttpStatus(StatusCode::TOO_MANY_REQUESTS).is_transient());
        assert!(!TransferError::HttpStatus(StatusCode::NOT_FOUND).is_transient());
        assert!(!TransferError::RemoteFileChanged.is_transient());
        let odd = TransferError::HttpStatus(StatusCode::from_u16(618).unwrap());
        assert!(odd.is_transient());
        assert!(odd.to_string().contains("618 (not a standard HTTP status"));
    }
}
