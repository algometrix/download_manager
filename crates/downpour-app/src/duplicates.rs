//! Whether a link from the browser is a download the user already has.
//!
//! The same address is the same file. A video stream's address usually
//! carries a token that changes with every visit, so a stream counts as the
//! same when it was found on the same page.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use downpour_core::category::CategoryId;
use downpour_core::{DownloadId, DownloadStatus, DownloadView};
use url::Url;

use crate::browser_link::BrowserLink;

/// A browser link, told apart by whether the user already has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Arrival {
    New(BrowserLink),
    /// Waits for the user to say whether to download it again.
    Known(BrowserLink, Duplicate),
}

/// An earlier download of the same link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Duplicate {
    pub id: DownloadId,
    pub file_name: String,
    pub category: CategoryId,
    pub standing: Standing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Finished, and the file is still where it was saved.
    Downloaded {
        file: PathBuf,
        finished_at_unix: Option<u64>,
    },
    Unfinished(Unfinished),
}

/// Where a download that is not finished stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unfinished {
    Waiting,
    Running,
    Paused,
    Failed,
    WaitingForSpace,
}

impl Unfinished {
    /// `None` for a finished download.
    fn of(status: &DownloadStatus) -> Option<Unfinished> {
        Some(match status {
            DownloadStatus::Queued => Unfinished::Waiting,
            DownloadStatus::Connecting | DownloadStatus::Downloading => Unfinished::Running,
            DownloadStatus::Paused => Unfinished::Paused,
            DownloadStatus::Failed { .. } => Unfinished::Failed,
            DownloadStatus::WaitingForSpace { .. } => Unfinished::WaitingForSpace,
            DownloadStatus::Completed => return None,
        })
    }
}

impl Duplicate {
    /// Lower is the better match to show: one that is not finished, since
    /// it is the one to act on, then the most recently finished.
    fn preference(&self) -> (bool, Reverse<Option<u64>>) {
        match &self.standing {
            Standing::Unfinished(_) => (false, Reverse(None)),
            Standing::Downloaded {
                finished_at_unix, ..
            } => (true, Reverse(*finished_at_unix)),
        }
    }
}

/// The links of one hand-off, told apart; a link that repeats an earlier
/// one of the same hand-off is not kept, only counted.
#[derive(Debug)]
pub struct Classified {
    pub arrivals: Vec<Arrival>,
    pub repeats: usize,
}

/// What makes two links one download: the same address, or, for streams
/// (whose addresses carry changing tokens), the same page.
#[derive(Debug)]
struct Identity {
    address: Url,
    stream_page: Option<Url>,
}

impl Identity {
    fn of_link(link: &BrowserLink) -> Identity {
        Identity {
            address: address_key(link.url()),
            stream_page: link.stream().and(link.page().cloned()),
        }
    }

    fn of_download(download: &DownloadView) -> Identity {
        Identity {
            address: address_key(&download.url),
            stream_page: download.stream.as_ref().and(download.page_url.clone()),
        }
    }

    fn same_download(&self, other: &Identity) -> bool {
        self.address == other.address
            || (self.stream_page.is_some() && self.stream_page == other.stream_page)
    }
}

/// Tells each link apart against the current downloads.
pub fn classify(
    links: Vec<BrowserLink>,
    downloads: &[DownloadView],
    file_exists: impl Fn(&Path) -> bool,
) -> Classified {
    let mut addresses: HashSet<Url> = HashSet::new();
    let mut stream_pages: HashSet<Url> = HashSet::new();
    let mut classified = Classified {
        arrivals: Vec::new(),
        repeats: 0,
    };
    for link in links {
        let identity = Identity::of_link(&link);
        let repeated = addresses.contains(&identity.address)
            || identity
                .stream_page
                .as_ref()
                .is_some_and(|page| stream_pages.contains(page));
        if repeated {
            classified.repeats += 1;
            continue;
        }
        addresses.insert(identity.address.clone());
        stream_pages.extend(identity.stream_page.clone());
        classified
            .arrivals
            .push(match find_duplicate(&identity, downloads, &file_exists) {
                Some(duplicate) => Arrival::Known(link, duplicate),
                None => Arrival::New(link),
            });
    }
    classified
}

/// The earlier download with this identity, if any. A finished download whose file
/// is gone does not count: downloading it again is the obvious thing to do.
fn find_duplicate(
    identity: &Identity,
    downloads: &[DownloadView],
    file_exists: impl Fn(&Path) -> bool,
) -> Option<Duplicate> {
    downloads
        .iter()
        .filter(|download| identity.same_download(&Identity::of_download(download)))
        .filter_map(|download| {
            let standing = match (Unfinished::of(&download.status), &download.target) {
                (Some(unfinished), _) => Standing::Unfinished(unfinished),
                (None, Some(file)) if file_exists(file) => Standing::Downloaded {
                    file: file.clone(),
                    finished_at_unix: download.finished_at_unix,
                },
                (None, _) => return None,
            };
            Some(Duplicate {
                id: download.id,
                file_name: download.file_name.clone(),
                category: download.category,
                standing,
            })
        })
        .min_by_key(Duplicate::preference)
}

/// The address without its `#fragment`, which only scrolls the page and
/// is not a different file.
fn address_key(url: &Url) -> Url {
    let mut url = url.clone();
    url.set_fragment(None);
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser_link::tests::{item, link};
    use downpour_core::limiter::DownloadLimit;
    use downpour_core::stream::{StreamFormat, VideoQuality};
    use downpour_core::{QueueId, StreamView};
    use downpour_protocol::messages::BrowserDownload;

    fn view(id: u64, url: &str, status: DownloadStatus) -> DownloadView {
        DownloadView {
            id: DownloadId(id),
            file_name: format!("file-{id}.bin"),
            url: Url::parse(url).unwrap(),
            page_url: None,
            referrer: None,
            queue: QueueId::MAIN,
            directory: PathBuf::from("D:/dl"),
            target: Some(PathBuf::from(format!("D:/dl/file-{id}.bin"))),
            category: CategoryId(0),
            status,
            size: None,
            received: 0,
            bytes_per_second: 0,
            connections: 0,
            segments: Vec::new(),
            resumable: None,
            size_is_estimate: false,
            progress_fraction: None,
            stream: None,
            added_at_unix: 0,
            finished_at_unix: None,
            stopping: false,
            speed_limit: DownloadLimit::FollowSettings,
            can_start_now: false,
        }
    }

    fn finished(id: u64, url: &str, at: Option<u64>) -> DownloadView {
        DownloadView {
            finished_at_unix: at,
            ..view(id, url, DownloadStatus::Completed)
        }
    }

    fn stream_on_page(id: u64, url: &str, page: &str) -> DownloadView {
        DownloadView {
            page_url: Some(Url::parse(page).unwrap()),
            stream: Some(StreamView {
                format: StreamFormat::Hls,
                quality: VideoQuality::Best,
                variant: None,
                merging: false,
            }),
            ..finished(id, url, Some(10))
        }
    }

    fn link_on_page(url: &str, page: &str) -> BrowserLink {
        BrowserLink::parse(BrowserDownload {
            page_url: Some(page.into()),
            ..item(url)
        })
        .unwrap()
    }

    fn duplicate_of(
        link: &BrowserLink,
        downloads: &[DownloadView],
        file_exists: impl Fn(&Path) -> bool,
    ) -> Option<Duplicate> {
        find_duplicate(&Identity::of_link(link), downloads, file_exists)
    }

    const EVERY_FILE_EXISTS: fn(&Path) -> bool = |_| true;

    #[test]
    fn the_same_address_is_a_duplicate_whatever_its_fragment() {
        let downloads = [finished(1, "https://cdn.example/a.zip", Some(5))];
        let found = duplicate_of(
            &link("https://cdn.example/a.zip#top"),
            &downloads,
            EVERY_FILE_EXISTS,
        )
        .unwrap();
        assert_eq!(found.id, DownloadId(1));
        assert!(matches!(found.standing, Standing::Downloaded { .. }));
        assert_eq!(
            duplicate_of(
                &link("https://cdn.example/b.zip"),
                &downloads,
                EVERY_FILE_EXISTS
            ),
            None
        );
    }

    #[test]
    fn a_finished_download_whose_file_is_gone_is_not_a_duplicate() {
        let downloads = [finished(1, "https://cdn.example/a.zip", Some(5))];
        assert_eq!(
            duplicate_of(&link("https://cdn.example/a.zip"), &downloads, |_| false),
            None
        );
    }

    #[test]
    fn an_unfinished_copy_wins_then_the_newest_then_one_of_unknown_age() {
        let address = "https://cdn.example/a.zip";
        let downloads = [
            finished(1, address, None),
            finished(2, address, Some(5)),
            finished(3, address, Some(9)),
            view(4, address, DownloadStatus::Paused),
        ];
        let best = |downloads: &[DownloadView]| {
            duplicate_of(&link(address), downloads, EVERY_FILE_EXISTS)
                .unwrap()
                .id
        };
        assert_eq!(best(&downloads), DownloadId(4));
        assert_eq!(best(&downloads[..3]), DownloadId(3));
        assert_eq!(best(&downloads[..2]), DownloadId(2));
        let paused = duplicate_of(&link(address), &downloads, EVERY_FILE_EXISTS).unwrap();
        assert_eq!(paused.standing, Standing::Unfinished(Unfinished::Paused));
    }

    #[test]
    fn a_stream_from_the_same_page_is_a_duplicate_despite_a_new_token() {
        let page = "https://site.example/watch/3";
        let downloads = [stream_on_page(
            1,
            "https://cdn.example/v/master.m3u8?token=old",
            page,
        )];
        let stream = link_on_page("https://cdn.example/v/master.m3u8?token=new", page);
        let found = duplicate_of(&stream, &downloads, EVERY_FILE_EXISTS).unwrap();
        assert_eq!(found.id, DownloadId(1));
        // A file from the same page is not the same file.
        let file = link_on_page("https://cdn.example/other.zip", page);
        assert_eq!(duplicate_of(&file, &downloads, EVERY_FILE_EXISTS), None);
    }

    #[test]
    fn a_mixed_batch_is_told_apart_link_by_link() {
        let downloads = [view(7, "https://cdn.example/a.zip", DownloadStatus::Queued)];
        let arrivals = classify(
            vec![
                link("https://cdn.example/new.zip"),
                link("https://cdn.example/a.zip"),
            ],
            &downloads,
            EVERY_FILE_EXISTS,
        )
        .arrivals;
        assert!(matches!(&arrivals[0], Arrival::New(link) if link.url().path() == "/new.zip"));
        assert!(matches!(
            &arrivals[1],
            Arrival::Known(
                _,
                Duplicate {
                    id: DownloadId(7),
                    standing: Standing::Unfinished(Unfinished::Waiting),
                    ..
                }
            )
        ));
        assert!(classify(Vec::new(), &downloads, EVERY_FILE_EXISTS)
            .arrivals
            .is_empty());
    }

    #[test]
    fn the_same_link_twice_in_one_hand_off_counts_once() {
        let classified = classify(
            vec![
                link("https://cdn.example/a.zip"),
                link("https://cdn.example/a.zip#again"),
                link("https://cdn.example/b.zip"),
            ],
            &[],
            EVERY_FILE_EXISTS,
        );
        assert_eq!(classified.repeats, 1);
        assert_eq!(classified.arrivals.len(), 2);
        assert!(classified
            .arrivals
            .iter()
            .all(|arrival| matches!(arrival, Arrival::New(_))));
    }

    #[test]
    fn two_streams_from_one_page_in_one_hand_off_count_once() {
        let page = "https://site.example/watch/3";
        let classified = classify(
            vec![
                link_on_page("https://cdn.example/v/master.m3u8?token=a", page),
                link_on_page("https://cdn.example/v/master.m3u8?token=b", page),
            ],
            &[],
            EVERY_FILE_EXISTS,
        );
        assert_eq!(classified.repeats, 1);
        assert_eq!(classified.arrivals.len(), 1);
    }
}
