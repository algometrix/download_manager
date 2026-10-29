//! A download link handed over by the browser, and what follows from it.
//!
//! The link is read once where it arrives: its address, the page it was
//! found on, whether it is a video stream, and the request headers. The
//! file-info dialog, the duplicate check, and auto-start all read these
//! facts from here instead of working them out again.

use std::path::PathBuf;

use downpour_core::category::CategoryId;
use downpour_core::stream::{StreamFormat, VideoQuality};
use downpour_core::{
    Categories, DownloadKind, EngineSettings, FileName, FileNameSource, NewDownload, QueueId,
    RequestContext, StartMode,
};
use downpour_protocol::messages::{BrowserDownload, QualityPick, StreamHint};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrowserLink {
    item: BrowserDownload,
    url: Url,
    page: Option<Url>,
    stream: Option<StreamFormat>,
}

impl BrowserLink {
    pub fn parse(item: BrowserDownload) -> Result<Self, url::ParseError> {
        let url = Url::parse(&item.url)?;
        // The page the extension reported, or the Referer for links captured
        // before it reported pages; only web pages count.
        let page = item
            .page_url
            .as_deref()
            .or(item.referrer.as_deref())
            .and_then(|page| Url::parse(page).ok())
            .filter(|page| matches!(page.scheme(), "http" | "https"));
        let stream = match item.stream {
            Some(StreamHint::Hls) => Some(StreamFormat::Hls),
            Some(StreamHint::Dash) => Some(StreamFormat::Dash),
            None => StreamFormat::from_url(&url),
        };
        Ok(Self {
            item,
            url,
            page,
            stream,
        })
    }

    /// The size the browser reported, if any.
    pub fn size(&self) -> Option<u64> {
        self.item.size
    }

    /// The file name the browser chose, if it chose one.
    pub fn browser_file_name(&self) -> Option<&str> {
        self.item.file_name.as_deref()
    }

    pub fn url(&self) -> &Url {
        &self.url
    }

    pub fn page(&self) -> Option<&Url> {
        self.page.as_ref()
    }

    /// `Some` when the link is a video stream's manifest.
    pub fn stream(&self) -> Option<StreamFormat> {
        self.stream
    }

    /// The quality picked on the page's video button, if any.
    pub fn picked_quality(&self) -> Option<VideoQuality> {
        self.item.quality.map(|pick| match pick {
            QualityPick::Best => VideoQuality::Best,
            QualityPick::MaxHeight { pixels } => VideoQuality::MaxHeight { pixels },
        })
    }

    /// The headers the page's own requests carried.
    pub fn context(&self) -> RequestContext {
        RequestContext {
            referrer: self.item.referrer.clone(),
            user_agent: self.item.user_agent.clone(),
            cookies: self.item.cookies.clone(),
            origin: self.item.origin.clone(),
            accept_language: self.item.accept_language.clone(),
        }
    }

    /// The name to offer: a stream is named after the page's title (a far
    /// better name than `master.m3u8`), a file as the browser named it.
    pub fn suggested_name(&self) -> String {
        match self.stream {
            Some(_) => self
                .item
                .title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(|title| FileName::new(title, FileNameSource::Browser).value)
                .unwrap_or_else(|| FileName::guess_for_stream(&self.url).value),
            None => self
                .item
                .file_name
                .clone()
                .unwrap_or_else(|| FileName::guess_from_url(&self.url).value),
        }
    }

    /// The category a file named `name` falls in; streams become `.mp4`
    /// videos whatever the manifest's own extension.
    pub fn category_for(&self, name: &str, categories: &Categories) -> CategoryId {
        match self.stream {
            Some(_) => categories.classify(&format!("{name}.mp4")),
            None => categories.classify(name),
        }
    }
}

/// Where a download of `category` is saved by default.
pub fn default_folder(settings: &EngineSettings, category: CategoryId) -> PathBuf {
    if settings.organize_by_category {
        settings
            .categories
            .get(category)
            .save_folder(&settings.download_dir)
    } else {
        settings.download_dir.clone()
    }
}

/// What the file-info dialog would add with its defaults untouched: the
/// suggested name, the category's folder, the Main queue, and the quality
/// picked on the page. Without a pick the engine applies the quality from
/// Settings when the download starts.
pub fn request_without_asking(link: BrowserLink, settings: &EngineSettings) -> NewDownload {
    let name = link.suggested_name();
    // A name the server may still improve on stays unset.
    let keep_name = link.stream.is_some() || link.item.file_name.is_some();
    let file_name = keep_name.then(|| FileName::new(&name, FileNameSource::Browser));
    // Without a name the engine picks the category folder once the server
    // has named the file.
    let folder = keep_name.then(|| {
        let category = link.category_for(&name, &settings.categories);
        downpour_core::paths::native(&default_folder(settings, category))
    });
    let kind = match link.stream {
        Some(format) => DownloadKind::Stream {
            format,
            quality: link.picked_quality(),
        },
        None => DownloadKind::Detect,
    };
    NewDownload {
        context: link.context(),
        url: link.url,
        file_name,
        folder,
        start: StartMode::Queue,
        kind,
        queue: Some(QueueId::MAIN),
        page_url: link.page,
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn item(url: &str) -> BrowserDownload {
        BrowserDownload {
            url: url.into(),
            file_name: None,
            referrer: None,
            cookies: None,
            user_agent: None,
            mime: None,
            size: None,
            stream: None,
            quality: None,
            title: None,
            page_url: None,
            origin: None,
            accept_language: None,
        }
    }

    pub fn link(url: &str) -> BrowserLink {
        BrowserLink::parse(item(url)).unwrap()
    }

    fn stream_item() -> BrowserDownload {
        BrowserDownload {
            referrer: Some("https://site.example/watch/3".into()),
            user_agent: Some("UA".into()),
            stream: Some(StreamHint::Hls),
            title: Some("Scene 3: The Return".into()),
            page_url: Some("https://site.example/watch/3".into()),
            origin: Some("https://site.example".into()),
            accept_language: Some("en-US".into()),
            ..item("https://cdn.example/v/master.m3u8")
        }
    }

    #[test]
    fn the_extension_sends_qualities_in_the_engines_own_json() {
        for quality in VideoQuality::CHOICES {
            let json = serde_json::to_string(&quality).unwrap();
            let pick: QualityPick = serde_json::from_str(&json).unwrap();
            let link = BrowserLink::parse(BrowserDownload {
                quality: Some(pick),
                ..item("https://cdn.example/a.m3u8")
            })
            .unwrap();
            assert_eq!(link.picked_quality(), Some(quality), "{json}");
        }
    }

    #[test]
    fn the_page_is_the_reported_one_or_the_referrer_and_only_a_web_page() {
        let reported = BrowserLink::parse(BrowserDownload {
            page_url: Some("https://site.example/watch".into()),
            referrer: Some("https://other.example/".into()),
            ..item("https://cdn.example/a.zip")
        })
        .unwrap();
        assert_eq!(
            reported.page().unwrap().as_str(),
            "https://site.example/watch"
        );
        let referred = BrowserLink::parse(BrowserDownload {
            referrer: Some("https://other.example/".into()),
            ..item("https://cdn.example/a.zip")
        })
        .unwrap();
        assert_eq!(referred.page().unwrap().as_str(), "https://other.example/");
        let local = BrowserLink::parse(BrowserDownload {
            page_url: Some("file:///C:/page.html".into()),
            ..item("https://cdn.example/a.zip")
        })
        .unwrap();
        assert_eq!(local.page(), None);
    }

    #[test]
    fn a_stream_is_known_from_the_hint_or_the_address() {
        assert_eq!(
            BrowserLink::parse(stream_item()).unwrap().stream(),
            Some(StreamFormat::Hls)
        );
        assert_eq!(
            link("https://cdn.example/v/manifest.mpd").stream(),
            Some(StreamFormat::Dash)
        );
        assert_eq!(link("https://cdn.example/a.zip").stream(), None);
    }

    #[test]
    fn auto_started_streams_are_named_after_the_page_and_keep_headers() {
        let request = request_without_asking(
            BrowserLink::parse(stream_item()).unwrap(),
            &EngineSettings::default(),
        );
        assert_eq!(request.file_name.unwrap().value, "Scene 3_ The Return");
        assert_eq!(
            request.kind,
            DownloadKind::Stream {
                format: StreamFormat::Hls,
                quality: None
            }
        );
        assert_eq!(
            request.context.origin.as_deref(),
            Some("https://site.example")
        );
        assert_eq!(request.context.accept_language.as_deref(), Some("en-US"));
        assert_eq!(
            request.page_url.unwrap().as_str(),
            "https://site.example/watch/3"
        );
    }

    #[test]
    fn a_quality_picked_on_the_page_goes_with_an_auto_started_stream() {
        let link = BrowserLink::parse(BrowserDownload {
            quality: Some(QualityPick::MaxHeight { pixels: 720 }),
            ..stream_item()
        })
        .unwrap();
        let request = request_without_asking(link, &EngineSettings::default());
        assert_eq!(
            request.kind,
            DownloadKind::Stream {
                format: StreamFormat::Hls,
                quality: Some(VideoQuality::MaxHeight { pixels: 720 })
            }
        );
    }

    #[test]
    fn auto_started_files_go_to_their_categorys_folder() {
        let settings = EngineSettings {
            download_dir: PathBuf::from("D:/dl"),
            ..EngineSettings::default()
        };
        let named = BrowserLink::parse(BrowserDownload {
            file_name: Some("movie.mp4".into()),
            ..item("https://cdn.example/get?id=7")
        })
        .unwrap();
        let request = request_without_asking(named, &settings);
        assert_eq!(request.file_name.unwrap().value, "movie.mp4");
        assert_eq!(
            request.folder,
            Some(downpour_core::paths::native(
                &PathBuf::from("D:/dl").join("Video")
            ))
        );
        // Without the browser's name, the server may still name the file,
        // and the engine picks its category folder then.
        let unnamed = request_without_asking(link("https://cdn.example/get?id=7"), &settings);
        assert_eq!(unnamed.file_name, None);
        assert_eq!(unnamed.folder, None);
        assert_eq!(unnamed.queue, Some(QueueId::MAIN));
    }
}
