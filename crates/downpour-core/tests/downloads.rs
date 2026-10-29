//! End-to-end downloads through the public manager API against a local server.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use downpour_core::{
    DownloadManager, DownloadStatus, EngineSettings, NewDownload, StartMode, Store,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

#[derive(Clone, Copy)]
enum RangeSupport {
    Ranges,
    NoRanges,
}

/// How the server answers a request beyond its connection limit.
#[derive(Clone, Copy)]
enum Refusal {
    TooManyRequests,
    /// Some download servers answer extra connections like this, as if
    /// the file did not exist.
    NotFound,
}

impl Refusal {
    fn response(self) -> &'static [u8] {
        match self {
            Refusal::TooManyRequests => {
                b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            }
            Refusal::NotFound => {
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            }
        }
    }
}

/// How the test server behaves.
#[derive(Clone, Copy)]
struct Behavior {
    ranges: RangeSupport,
    /// Requests beyond this many in flight are refused with `refusal`.
    max_concurrent: usize,
    refusal: Refusal,
    /// After this many accepted requests, every new one is refused with
    /// `refusal`; connections already open keep streaming. Some download
    /// links behave like this some minutes into a download.
    accepted_requests: usize,
}

impl From<RangeSupport> for Behavior {
    fn from(ranges: RangeSupport) -> Self {
        Behavior {
            ranges,
            max_concurrent: usize::MAX,
            refusal: Refusal::TooManyRequests,
            accepted_requests: usize::MAX,
        }
    }
}

struct TestServer {
    url: Url,
    range_requests: Arc<AtomicUsize>,
    refused_requests: Arc<AtomicUsize>,
}

/// Minimal HTTP/1.1 server that serves `body`, honoring `Range` when asked.
/// Each response is written in small slices with a delay so downloads last
/// long enough to pause and to open several connections.
async fn serve(body: Arc<Vec<u8>>, behavior: impl Into<Behavior>) -> TestServer {
    let behavior = behavior.into();
    let in_flight = Arc::new(AtomicUsize::new(0));
    let accepted_so_far = Arc::new(AtomicUsize::new(0));
    let refused_requests = Arc::new(AtomicUsize::new(0));
    let refused = Arc::clone(&refused_requests);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!(
        "http://{}/files/sample.bin",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    let range_requests = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&range_requests);
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let body = Arc::clone(&body);
            let counter = Arc::clone(&counter);
            let in_flight = Arc::clone(&in_flight);
            let refused = Arc::clone(&refused);
            let accepted_so_far = Arc::clone(&accepted_so_far);
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buffer = [0u8; 1024];
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&buffer[..read]);
                }
                let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
                let range = request
                    .lines()
                    .find_map(|line| line.strip_prefix("range: bytes="))
                    .map(|spec| {
                        let (start, end) = spec.trim().split_once('-').unwrap();
                        let start: usize = start.parse().unwrap();
                        let end = end.parse::<usize>().map(|e| e + 1).unwrap_or(body.len());
                        (start, end.min(body.len()))
                    });
                let link_expired =
                    accepted_so_far.fetch_add(1, Ordering::SeqCst) >= behavior.accepted_requests;
                if link_expired {
                    refused.fetch_add(1, Ordering::SeqCst);
                    let _ = socket.write_all(behavior.refusal.response()).await;
                    return;
                }
                if in_flight.fetch_add(1, Ordering::SeqCst) >= behavior.max_concurrent {
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    refused.fetch_add(1, Ordering::SeqCst);
                    let _ = socket.write_all(behavior.refusal.response()).await;
                    return;
                }
                let _release = InFlight(Arc::clone(&in_flight));
                let (head, slice) = match (behavior.ranges, range) {
                    (RangeSupport::Ranges, Some((start, end))) => {
                        counter.fetch_add(1, Ordering::Relaxed);
                        let head = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n",
                            end - start, start, end - 1, body.len()
                        );
                        (head, &body[start..end])
                    }
                    _ => (
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        ),
                        &body[..],
                    ),
                };
                if socket.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for piece in slice.chunks(64 * 1024) {
                    if socket.write_all(piece).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            });
        }
    });
    TestServer {
        url,
        range_requests,
        refused_requests,
    }
}

/// Decrements the in-flight request count when a response ends.
struct InFlight(Arc<AtomicUsize>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn sample_body(len: usize) -> Arc<Vec<u8>> {
    Arc::new((0..len).map(|i| (i * 31 % 251) as u8).collect())
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("downpour-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn manager_in(dir: &Path, connections: u8) -> DownloadManager {
    let settings = EngineSettings {
        download_dir: dir.join("downloads"),
        organize_by_category: false,
        connections_per_download: connections,
        ..Default::default()
    };
    let store = Store::open(dir.join("state")).unwrap();
    DownloadManager::start(
        store,
        Vec::new(),
        settings,
        tokio::runtime::Handle::current(),
    )
    .unwrap()
}

async fn wait_for(
    manager: &DownloadManager,
    timeout: Duration,
    done: impl Fn(&downpour_core::DownloadView) -> bool,
) -> downpour_core::DownloadView {
    let deadline = Instant::now() + timeout;
    loop {
        let view = manager.snapshot().into_iter().next().expect("one download");
        if done(&view) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "timed out; last status {:?}",
            view.status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn segmented_download_matches_source() {
    let body = sample_body(6 * 1024 * 1024 + 123);
    let server = serve(Arc::clone(&body), RangeSupport::Ranges).await;
    let dir = temp_dir("segmented");
    let manager = manager_in(&dir, 6);

    manager
        .add(NewDownload::from_user(
            server.url.clone(),
            None,
            None,
            StartMode::Queue,
        ))
        .unwrap();
    let view = wait_for(&manager, Duration::from_secs(30), |v| {
        v.status == DownloadStatus::Completed
    })
    .await;

    let path = view.target.unwrap();
    assert_eq!(path.file_name().unwrap(), "sample.bin");
    assert_eq!(std::fs::read(&path).unwrap(), *body);
    assert!(
        server.range_requests.load(Ordering::Relaxed) > 1,
        "extra connections were opened"
    );
    assert!(view.segments.len() > 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn paused_download_resumes_to_identical_file() {
    let body = sample_body(8 * 1024 * 1024);
    let server = serve(Arc::clone(&body), RangeSupport::Ranges).await;
    let dir = temp_dir("resume");
    let manager = manager_in(&dir, 4);

    let id = manager
        .add(NewDownload::from_user(
            server.url.clone(),
            Some("resumed.bin"),
            None,
            StartMode::Queue,
        ))
        .unwrap();
    wait_for(&manager, Duration::from_secs(30), |v| {
        v.received > 1024 * 1024
    })
    .await;
    manager.pause(id).unwrap();
    let paused = wait_for(&manager, Duration::from_secs(10), |v| {
        v.status == DownloadStatus::Paused
    })
    .await;
    assert!(
        paused.received < body.len() as u64,
        "paused before completion"
    );

    manager.resume(id).unwrap();
    let view = wait_for(&manager, Duration::from_secs(30), |v| {
        v.status == DownloadStatus::Completed
    })
    .await;
    assert_eq!(std::fs::read(view.target.unwrap()).unwrap(), *body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn connection_limited_server_completes_without_failing() {
    let body = sample_body(6 * 1024 * 1024);
    let server = serve(
        Arc::clone(&body),
        Behavior {
            ranges: RangeSupport::Ranges,
            max_concurrent: 2,
            refusal: Refusal::TooManyRequests,
            accepted_requests: usize::MAX,
        },
    )
    .await;
    let dir = temp_dir("limited");
    let manager = manager_in(&dir, 8);

    manager
        .add(NewDownload::from_user(
            server.url.clone(),
            None,
            None,
            StartMode::Queue,
        ))
        .unwrap();
    let view = wait_for(&manager, Duration::from_secs(30), |v| {
        matches!(
            v.status,
            DownloadStatus::Completed | DownloadStatus::Failed { .. }
        )
    })
    .await;

    assert_eq!(view.status, DownloadStatus::Completed);
    assert_eq!(std::fs::read(view.target.unwrap()).unwrap(), *body);
    // Only the initial burst of extra connections is refused; after that the
    // download stays within the limit it learned.
    let refused = server.refused_requests.load(Ordering::SeqCst);
    assert!(
        refused <= 7,
        "kept exceeding the server limit: {refused} refused requests"
    );
}

/// A server that answers connections beyond its limit with 404 must not
/// fail a download it is serving: a download once failed at 99% when a
/// connection replacing a finished one was refused this way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn extra_connections_refused_as_not_found_still_complete() {
    let body = sample_body(6 * 1024 * 1024);
    let server = serve(
        Arc::clone(&body),
        Behavior {
            ranges: RangeSupport::Ranges,
            max_concurrent: 2,
            refusal: Refusal::NotFound,
            accepted_requests: usize::MAX,
        },
    )
    .await;
    let dir = temp_dir("refused-404");
    let manager = manager_in(&dir, 8);

    manager
        .add(NewDownload::from_user(
            server.url.clone(),
            None,
            None,
            StartMode::Queue,
        ))
        .unwrap();
    let view = wait_for(&manager, Duration::from_secs(60), |v| {
        matches!(
            v.status,
            DownloadStatus::Completed | DownloadStatus::Failed { .. }
        )
    })
    .await;

    assert_eq!(view.status, DownloadStatus::Completed);
    assert_eq!(std::fs::read(view.target.unwrap()).unwrap(), *body);
    assert!(
        server.refused_requests.load(Ordering::SeqCst) > 0,
        "the server never refused a connection"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_without_ranges_streams_whole_file() {
    let body = sample_body(700 * 1024);
    let server = serve(Arc::clone(&body), RangeSupport::NoRanges).await;
    let dir = temp_dir("stream");
    let manager = manager_in(&dir, 8);

    manager
        .add(NewDownload::from_user(
            server.url.clone(),
            None,
            None,
            StartMode::Queue,
        ))
        .unwrap();
    let view = wait_for(&manager, Duration::from_secs(30), |v| {
        v.status == DownloadStatus::Completed
    })
    .await;

    assert_eq!(view.resumable, Some(false));
    assert_eq!(std::fs::read(view.target.unwrap()).unwrap(), *body);
}

async fn wait_for_download(
    manager: &DownloadManager,
    id: downpour_core::DownloadId,
    timeout: Duration,
    done: impl Fn(&downpour_core::DownloadView) -> bool,
) -> downpour_core::DownloadView {
    let deadline = Instant::now() + timeout;
    loop {
        let view = manager
            .snapshot()
            .into_iter()
            .find(|view| view.id == id)
            .expect("download exists");
        if done(&view) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "timed out; last status {:?}",
            view.status
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Answers every request with a tiny body that claims to be part of a
/// one-petabyte file, which can never fit on disk.
async fn serve_impossibly_large_file() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!(
        "http://{}/huge.iso",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buffer = [0u8; 1024];
            let _ = socket.read(&mut buffer).await;
            let petabyte: u64 = 1 << 50;
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: 4\r\nContent-Range: bytes 0-3/{petabyte}\r\nConnection: close\r\n\r\nabcd"
            );
            let _ = socket.write_all(head.as_bytes()).await;
        }
    });
    url
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn file_too_large_for_disk_waits_for_space() {
    let url = serve_impossibly_large_file().await;
    let dir = temp_dir("no-space");
    let manager = manager_in(&dir, 2);

    let id = manager
        .add(NewDownload::from_user(url, None, None, StartMode::Queue))
        .unwrap();
    let view = wait_for_download(&manager, id, Duration::from_secs(10), |v| {
        matches!(
            v.status,
            DownloadStatus::WaitingForSpace { .. } | DownloadStatus::Failed { .. }
        )
    })
    .await;

    let DownloadStatus::WaitingForSpace {
        needed_bytes,
        directory,
    } = view.status
    else {
        panic!("expected to wait for disk space, got {:?}", view.status);
    };
    assert_eq!(needed_bytes, Some(1 << 50));
    assert_eq!(directory, dir.join("downloads"));
    // Nothing was allocated and the reserved name was given back.
    assert!(view.target.is_none());
    let leftovers: Vec<_> = std::fs::read_dir(&directory).unwrap().collect();
    assert!(leftovers.is_empty(), "files left behind: {leftovers:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stopped_queue_holds_its_downloads_until_started() {
    let body = sample_body(512 * 1024);
    let server = serve(Arc::clone(&body), RangeSupport::Ranges).await;
    let dir = temp_dir("queues");
    let manager = manager_in(&dir, 2);
    let later = manager.add_queue("Later").unwrap();
    manager.stop_queue(later).unwrap();

    let held = manager
        .add(NewDownload {
            queue: Some(later),
            ..NewDownload::from_user(server.url.clone(), Some("held.bin"), None, StartMode::Queue)
        })
        .unwrap();
    let main = manager
        .add(NewDownload::from_user(
            server.url.clone(),
            Some("main.bin"),
            None,
            StartMode::Queue,
        ))
        .unwrap();

    wait_for_download(&manager, main, Duration::from_secs(30), |v| {
        v.status == DownloadStatus::Completed
    })
    .await;
    let still_held = wait_for_download(&manager, held, Duration::from_secs(1), |_| true).await;
    assert_eq!(still_held.status, DownloadStatus::Queued);

    manager.start_queue(later).unwrap();
    let finished = wait_for_download(&manager, held, Duration::from_secs(30), |v| {
        v.status == DownloadStatus::Completed
    })
    .await;
    assert_eq!(std::fs::read(finished.target.unwrap()).unwrap(), *body);
    // The queue survives a restart of the manager.
    let saved = Store::open(dir.join("state"))
        .unwrap()
        .load_queues()
        .unwrap()
        .unwrap();
    assert!(saved.all().iter().any(|queue| queue.name == "Later"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn queue_runs_no_more_than_its_limit() {
    let body = sample_body(2 * 1024 * 1024);
    let server = serve(Arc::clone(&body), RangeSupport::Ranges).await;
    let dir = temp_dir("queue-limit");
    let manager = manager_in(&dir, 2);
    manager
        .set_queue_limit(downpour_core::QueueId::MAIN, 1)
        .unwrap();

    for name in ["a.bin", "b.bin", "c.bin"] {
        manager
            .add(NewDownload::from_user(
                server.url.clone(),
                Some(name),
                None,
                StartMode::Queue,
            ))
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let views = manager.snapshot();
        let running = views.iter().filter(|v| v.status.is_active()).count();
        assert!(
            running <= 1,
            "{running} downloads ran at once in a queue limited to 1"
        );
        if views.iter().all(|v| v.status == DownloadStatus::Completed) {
            break;
        }
        assert!(Instant::now() < deadline, "queue did not finish");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_file_fails_with_status() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!(
        "http://{}/missing",
        listener.local_addr().unwrap()
    ))
    .unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buffer = [0u8; 1024];
            let _ = socket.read(&mut buffer).await;
            let _ = socket
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
        }
    });
    let dir = temp_dir("missing");
    let manager = manager_in(&dir, 2);

    manager
        .add(NewDownload::from_user(url, None, None, StartMode::Queue))
        .unwrap();
    let view = wait_for(&manager, Duration::from_secs(10), |v| {
        matches!(v.status, DownloadStatus::Failed { .. })
    })
    .await;
    let DownloadStatus::Failed { reason } = view.status else {
        unreachable!()
    };
    assert!(reason.contains("404"), "{reason}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn download_later_waits_until_its_queue_is_started() {
    let body = sample_body(256 * 1024);
    let server = serve(Arc::clone(&body), RangeSupport::Ranges).await;
    let dir = temp_dir("download-later");
    let manager = manager_in(&dir, 2);
    let later = manager.add_queue("Later").unwrap();

    let id = manager
        .add(NewDownload {
            queue: Some(later),
            ..NewDownload::from_user(server.url.clone(), Some("later.bin"), None, StartMode::Hold)
        })
        .unwrap();
    // A running queue does not start a download added "later".
    let held = wait_for_download(&manager, id, Duration::from_secs(1), |_| true).await;
    assert_eq!(held.status, DownloadStatus::Paused);

    manager.start_queue(later).unwrap();
    let finished = wait_for_download(&manager, id, Duration::from_secs(30), |v| {
        v.status == DownloadStatus::Completed
    })
    .await;
    assert_eq!(std::fs::read(finished.target.unwrap()).unwrap(), *body);
}

/// The order the user sets is the order a queue starts its downloads in,
/// and the order the list shows them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moving_a_download_to_the_top_starts_it_first() {
    let body = sample_body(2 * 1024 * 1024);
    let server = serve(Arc::clone(&body), RangeSupport::Ranges).await;
    let dir = temp_dir("reorder");
    let manager = manager_in(&dir, 2);
    manager
        .set_queue_limit(downpour_core::QueueId::MAIN, 1)
        .unwrap();
    manager.stop_queue(downpour_core::QueueId::MAIN).unwrap();

    let mut ids = Vec::new();
    for name in ["a.bin", "b.bin", "c.bin"] {
        ids.push(
            manager
                .add(NewDownload::from_user(
                    server.url.clone(),
                    Some(name),
                    None,
                    StartMode::Queue,
                ))
                .unwrap(),
        );
    }
    manager
        .move_in_queue(ids[2], downpour_core::QueuePlace::Before(ids[0]))
        .unwrap();
    let names: Vec<String> = manager
        .snapshot()
        .into_iter()
        .map(|v| v.file_name)
        .collect();
    assert_eq!(names, ["c.bin", "a.bin", "b.bin"]);

    manager.start_queue(downpour_core::QueueId::MAIN).unwrap();
    let first = wait_for(&manager, Duration::from_secs(20), |v| {
        v.status.is_active() || v.status == DownloadStatus::Completed
    })
    .await;
    assert_eq!(
        first.file_name, "c.bin",
        "the download moved to the top did not start first"
    );
}

/// A link that stops accepting new requests partway (while open connections
/// keep streaming) must still finish: the connections that are running own
/// their ranges until a split's new connection is accepted. A download
/// once stalled at 99% when every late split was refused with 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_link_that_stops_accepting_new_requests_still_finishes() {
    let body = sample_body(6 * 1024 * 1024);
    let server = serve(
        Arc::clone(&body),
        Behavior {
            ranges: RangeSupport::Ranges,
            max_concurrent: usize::MAX,
            refusal: Refusal::NotFound,
            // The probe and one more connection, then nothing new.
            accepted_requests: 2,
        },
    )
    .await;
    let dir = temp_dir("link-stops-accepting");
    let manager = manager_in(&dir, 8);

    manager
        .add(NewDownload::from_user(
            server.url.clone(),
            None,
            None,
            StartMode::Queue,
        ))
        .unwrap();
    let view = wait_for(&manager, Duration::from_secs(60), |v| {
        matches!(
            v.status,
            DownloadStatus::Completed | DownloadStatus::Failed { .. }
        )
    })
    .await;

    assert_eq!(view.status, DownloadStatus::Completed);
    assert_eq!(std::fs::read(view.target.unwrap()).unwrap(), *body);
    assert!(
        server.refused_requests.load(Ordering::SeqCst) > 0,
        "the link never refused a request"
    );
}

/// "Start now" runs a waiting download although its queue is stopped; the
/// others keep waiting, and a download that is not waiting is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_now_runs_a_download_its_stopped_queue_holds_back() {
    let body = sample_body(512 * 1024);
    let server = serve(Arc::clone(&body), RangeSupport::Ranges).await;
    let dir = temp_dir("start-now");
    let manager = manager_in(&dir, 2);
    manager.stop_queue(downpour_core::QueueId::MAIN).unwrap();
    let add = |name: &str| {
        manager
            .add(NewDownload::from_user(
                server.url.clone(),
                Some(name),
                None,
                StartMode::Queue,
            ))
            .unwrap()
    };
    let forced = add("forced.bin");
    let waiting = add("waiting.bin");

    manager.start_now(forced).unwrap();
    let done = wait_for_download(&manager, forced, Duration::from_secs(20), |v| {
        v.status == DownloadStatus::Completed
    })
    .await;
    assert_eq!(std::fs::read(done.target.unwrap()).unwrap(), *body);
    let other = manager
        .snapshot()
        .into_iter()
        .find(|v| v.id == waiting)
        .unwrap();
    assert_eq!(other.status, DownloadStatus::Queued);
    assert!(matches!(
        manager.start_now(forced),
        Err(downpour_core::ManagerError::NotWaiting(_))
    ));
}

/// A download set to "no limit" is not held to the speed limit from
/// Settings: 2 MiB at 16 KiB/s would take two minutes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_download_without_a_limit_ignores_the_global_one() {
    let body = sample_body(2 * 1024 * 1024);
    let server = serve(Arc::clone(&body), RangeSupport::Ranges).await;
    let dir = temp_dir("no-limit");
    let manager = manager_in(&dir, 2);
    manager.update_settings(EngineSettings {
        speed_limit_kib_per_second: std::num::NonZeroU32::new(16),
        ..manager.settings()
    });
    manager.stop_queue(downpour_core::QueueId::MAIN).unwrap();
    let id = manager
        .add(NewDownload::from_user(
            server.url.clone(),
            Some("free.bin"),
            None,
            StartMode::Queue,
        ))
        .unwrap();
    manager
        .set_speed_limit(id, downpour_core::limiter::DownloadLimit::Unlimited)
        .unwrap();
    manager.start_queue(downpour_core::QueueId::MAIN).unwrap();
    let done = wait_for_download(&manager, id, Duration::from_secs(20), |v| {
        v.status == DownloadStatus::Completed
    })
    .await;
    assert_eq!(
        done.speed_limit,
        downpour_core::limiter::DownloadLimit::Unlimited
    );
    assert_eq!(std::fs::read(done.target.unwrap()).unwrap(), *body);
}

/// Lifting the limit of a running download takes effect at once: 2 MiB at
/// the 16 KiB/s global limit would take two minutes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lifting_the_limit_of_a_running_download_speeds_it_up() {
    let body = sample_body(2 * 1024 * 1024);
    let server = serve(Arc::clone(&body), RangeSupport::Ranges).await;
    let dir = temp_dir("lift-limit");
    let manager = manager_in(&dir, 2);
    manager.update_settings(EngineSettings {
        speed_limit_kib_per_second: std::num::NonZeroU32::new(16),
        ..manager.settings()
    });
    let id = manager
        .add(NewDownload::from_user(
            server.url.clone(),
            Some("lifted.bin"),
            None,
            StartMode::Queue,
        ))
        .unwrap();
    wait_for_download(&manager, id, Duration::from_secs(20), |v| {
        v.status == DownloadStatus::Downloading
    })
    .await;
    // Long enough for the connections to be deep in the global limit's debt.
    tokio::time::sleep(Duration::from_secs(1)).await;
    manager
        .set_speed_limit(id, downpour_core::limiter::DownloadLimit::Unlimited)
        .unwrap();
    let done = wait_for_download(&manager, id, Duration::from_secs(20), |v| {
        v.status == DownloadStatus::Completed
    })
    .await;
    assert_eq!(std::fs::read(done.target.unwrap()).unwrap(), *body);
}

/// "Start now" also runs a download its full queue holds back, next to the
/// one already running.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn start_now_runs_a_download_past_a_full_queue() {
    let body = sample_body(1024 * 1024);
    let server = serve(Arc::clone(&body), RangeSupport::Ranges).await;
    let dir = temp_dir("start-now-full");
    let manager = manager_in(&dir, 2);
    manager
        .set_queue_limit(downpour_core::QueueId::MAIN, 1)
        .unwrap();
    // The first download crawls at the global limit and keeps the one place.
    manager.update_settings(EngineSettings {
        speed_limit_kib_per_second: std::num::NonZeroU32::new(16),
        ..manager.settings()
    });
    let add = |name: &str| {
        manager
            .add(NewDownload::from_user(
                server.url.clone(),
                Some(name),
                None,
                StartMode::Queue,
            ))
            .unwrap()
    };
    let slow = add("slow.bin");
    let forced = add("forced.bin");
    wait_for_download(&manager, slow, Duration::from_secs(20), |v| {
        v.status.is_active()
    })
    .await;
    let waiting = manager
        .snapshot()
        .into_iter()
        .find(|v| v.id == forced)
        .unwrap();
    assert!(
        waiting.can_start_now,
        "the second download should be waiting"
    );

    manager
        .set_speed_limit(forced, downpour_core::limiter::DownloadLimit::Unlimited)
        .unwrap();
    manager.start_now(forced).unwrap();
    let done = wait_for_download(&manager, forced, Duration::from_secs(20), |v| {
        v.status == DownloadStatus::Completed
    })
    .await;
    assert_eq!(std::fs::read(done.target.unwrap()).unwrap(), *body);
    let first = manager
        .snapshot()
        .into_iter()
        .find(|v| v.id == slow)
        .unwrap();
    assert!(first.status.is_active(), "the slow download keeps running");
}
