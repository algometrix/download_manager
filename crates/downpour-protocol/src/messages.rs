use serde::{Deserialize, Serialize};

/// A download captured in the browser, with the context needed to fetch it
/// the same way the page would.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserDownload {
    pub url: String,
    #[serde(default)]
    pub file_name: Option<String>,
    #[serde(default)]
    pub referrer: Option<String>,
    /// `Cookie` header value for `url`.
    #[serde(default)]
    pub cookies: Option<String>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    /// Set when `url` is a video stream manifest rather than a file.
    #[serde(default)]
    pub stream: Option<StreamHint>,
    /// The quality the user picked on the page's video button; `None`
    /// leaves it to the app's settings.
    #[serde(default)]
    pub quality: Option<QualityPick>,
    /// The page's title, used to name video streams.
    #[serde(default)]
    pub title: Option<String>,
    /// The page the download was found on.
    #[serde(default)]
    pub page_url: Option<String>,
    /// `Origin` header the page's player sent with the request.
    #[serde(default)]
    pub origin: Option<String>,
    /// `Accept-Language` header the browser sent with the request.
    #[serde(default)]
    pub accept_language: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamHint {
    Hls,
    Dash,
}

/// A video quality chosen in the browser. Same JSON as the app's own
/// `VideoQuality`: `{"kind":"best"}` or `{"kind":"max_height","pixels":720}`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QualityPick {
    Best,
    /// The best variant no taller than this many pixels.
    MaxHeight {
        pixels: u32,
    },
}

/// Sent by the extension to the native host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionRequest {
    /// Asks whether the app is running; never launches it.
    Ping,
    /// Hands downloads to the app, launching it if needed.
    Download { items: Vec<BrowserDownload> },
    /// Brings the app window to the front, launching it if needed.
    Show,
}

/// Sent by the native host back to the extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostReply {
    Status {
        app_running: bool,
        host_version: String,
    },
    Accepted {
        count: usize,
    },
    Shown,
    Error {
        message: String,
    },
}

/// Sent by the native host (or a second app instance) to the running app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppRequest {
    Ping,
    Show,
    Download {
        items: Vec<BrowserDownload>,
    },
    /// The installer asks the app to exit before it replaces or removes the
    /// program files. Downloads pause and resume at the next start.
    Quit,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppReply {
    Pong {
        app_version: String,
    },
    Shown,
    /// The app is exiting; its process ends once downloads are paused.
    Quitting,
    /// The downloads were received; the user may still confirm or cancel them.
    Accepted {
        count: usize,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_download_uses_camel_case_and_optional_fields() {
        let json = r#"{"type":"download","items":[{"url":"https://a/b.zip","fileName":"b.zip"}]}"#;
        let request: ExtensionRequest = serde_json::from_str(json).unwrap();
        let ExtensionRequest::Download { items } = request else {
            panic!("wrong variant")
        };
        assert_eq!(items[0].file_name.as_deref(), Some("b.zip"));
        assert_eq!(items[0].cookies, None);
    }

    #[test]
    fn stream_items_carry_page_details() {
        let json = r#"{"type":"download","items":[{"url":"https://cdn/v/master.m3u8","stream":"hls",
            "title":"Scene 3","pageUrl":"https://site/watch/3","origin":"https://site","acceptLanguage":"en-US"}]}"#;
        let ExtensionRequest::Download { items } = serde_json::from_str(json).unwrap() else {
            panic!("wrong variant")
        };
        assert_eq!(items[0].stream, Some(StreamHint::Hls));
        assert_eq!(items[0].page_url.as_deref(), Some("https://site/watch/3"));
        assert_eq!(items[0].accept_language.as_deref(), Some("en-US"));
        assert_eq!(items[0].quality, None);
    }

    #[test]
    fn a_quality_picked_on_the_page_is_read() {
        let json = r#"{"type":"download","items":[
            {"url":"https://cdn/a.m3u8","quality":{"kind":"best"}},
            {"url":"https://cdn/b.m3u8","quality":{"kind":"max_height","pixels":720}}]}"#;
        let ExtensionRequest::Download { items } = serde_json::from_str(json).unwrap() else {
            panic!("wrong variant")
        };
        assert_eq!(items[0].quality, Some(QualityPick::Best));
        assert_eq!(
            items[1].quality,
            Some(QualityPick::MaxHeight { pixels: 720 })
        );
    }

    #[test]
    fn the_quit_request_and_reply_are_tagged() {
        let json = serde_json::to_string(&AppRequest::Quit).unwrap();
        assert_eq!(json, r#"{"type":"quit"}"#);
        let reply: AppReply = serde_json::from_str(r#"{"type":"quitting"}"#).unwrap();
        assert_eq!(reply, AppReply::Quitting);
    }

    #[test]
    fn host_reply_is_tagged() {
        let json = serde_json::to_string(&HostReply::Accepted { count: 2 }).unwrap();
        assert_eq!(json, r#"{"type":"accepted","count":2}"#);
    }
}
