//! End-to-end stream downloads: ffmpeg packages a short test video as HLS
//! and DASH, a local server serves the files, and the downloaded result is
//! checked with ffprobe. Skipped (with a message) when ffmpeg is missing.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use downpour_core::stream::mux::locate_ffmpeg;
use downpour_core::{
    DownloadKind, DownloadManager, DownloadStatus, EngineSettings, NewDownload, RequestContext,
    StartMode, Store,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

const CLIP_SECONDS: f64 = 6.0;

struct Tools {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
}

fn tools() -> Option<Tools> {
    let ffmpeg = locate_ffmpeg(None)?;
    let ffprobe = ffmpeg.with_file_name(if cfg!(windows) {
        "ffprobe.exe"
    } else {
        "ffprobe"
    });
    ffprobe.is_file().then_some(Tools { ffmpeg, ffprobe })
}

/// ffmpeg renames its playlist into place as it writes; on Windows a virus
/// scanner occasionally holds the fresh file and the rename fails. That is
/// a fixture problem, so packaging is retried a few times.
const PACKAGING_ATTEMPTS: u32 = 3;

fn run(tool: &Path, args: &[&str], dir: &Path) {
    let mut last_error = String::new();
    for _ in 0..PACKAGING_ATTEMPTS {
        let output = Command::new(tool)
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        if output.status.success() {
            return;
        }
        last_error = String::from_utf8_lossy(&output.stderr).into_owned();
        std::thread::sleep(Duration::from_millis(300));
    }
    panic!(
        "{} failed {PACKAGING_ATTEMPTS} times: {last_error}",
        tool.display()
    );
}

/// Inputs for a 6 second 320x240 clip with a tone.
const SOURCE: [&str; 8] = [
    "-f",
    "lavfi",
    "-i",
    "testsrc=duration=6:size=320x240:rate=25",
    "-f",
    "lavfi",
    "-i",
    "sine=frequency=440:duration=6",
];
const ENCODE: [&str; 10] = [
    "-c:v",
    "libx264",
    "-g",
    "25",
    "-c:a",
    "aac",
    "-b:a",
    "64k",
    "-y",
    "-hide_banner",
];

/// Media playlist of encrypted MPEG-TS segments behind a multivariant playlist.
fn package_encrypted_hls(tools: &Tools, dir: &Path) {
    std::fs::write(dir.join("enc.key"), [0x11u8; 16]).unwrap();
    std::fs::write(dir.join("keyinfo.txt"), "enc.key\nenc.key\n").unwrap();
    let mut args: Vec<&str> = SOURCE.to_vec();
    args.extend(ENCODE);
    args.extend([
        "-f",
        "hls",
        "-hls_time",
        "1",
        "-hls_playlist_type",
        "vod",
        "-hls_key_info_file",
        "keyinfo.txt",
        "-master_pl_name",
        "master.m3u8",
        "-hls_segment_filename",
        "ts_%03d.ts",
        "video.m3u8",
    ]);
    run(&tools.ffmpeg, &args, dir);
}

/// DASH with separate video and audio adaptation sets (fMP4 + SegmentTimeline).
fn package_dash(tools: &Tools, dir: &Path) {
    let mut args: Vec<&str> = SOURCE.to_vec();
    args.extend(ENCODE);
    args.extend([
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-f",
        "dash",
        "-seg_duration",
        "1",
        "-use_template",
        "1",
        "-use_timeline",
        "1",
        "manifest.mpd",
    ]);
    run(&tools.ffmpeg, &args, dir);
}

/// HLS with fMP4 segments and the audio in its own rendition group.
fn package_fmp4_hls(tools: &Tools, dir: &Path) {
    let mut args: Vec<&str> = SOURCE.to_vec();
    args.extend(ENCODE);
    args.extend([
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-f",
        "hls",
        "-hls_time",
        "1",
        "-hls_playlist_type",
        "vod",
        "-hls_segment_type",
        "fmp4",
        "-master_pl_name",
        "master.m3u8",
        "-var_stream_map",
        "v:0,agroup:aud a:0,agroup:aud,default:yes",
        "-hls_segment_filename",
        "stream_%v/seg_%03d.m4s",
        "stream_%v/index.m3u8",
    ]);
    run(&tools.ffmpeg, &args, dir);
}

/// Serves files under `root`, honoring single byte ranges.
async fn serve_dir(root: PathBuf) -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let root = Arc::new(root);
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let root = Arc::clone(&root);
            tokio::spawn(async move {
                loop {
                    let mut request = Vec::new();
                    let mut buffer = [0u8; 2048];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        let read = socket.read(&mut buffer).await.unwrap_or(0);
                        if read == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..read]);
                    }
                    let text = String::from_utf8_lossy(&request).into_owned();
                    let path = text.split_whitespace().nth(1).unwrap_or("/");
                    let path = path.split('?').next().unwrap().trim_start_matches('/');
                    let Ok(body) = std::fs::read(root.join(path)) else {
                        let _ = socket
                            .write_all(b"HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\n\r\n")
                            .await;
                        continue;
                    };
                    let range = text.lines().find_map(|line| {
                        let lower = line.to_ascii_lowercase();
                        let spec = lower.strip_prefix("range: bytes=")?.trim().to_owned();
                        let (start, end) = spec.split_once('-')?;
                        let start: usize = start.parse().ok()?;
                        let end: usize = end.parse().unwrap_or(body.len() - 1);
                        Some((start, end.min(body.len() - 1)))
                    });
                    let (status, slice, extra) = match range {
                        Some((start, end)) => (
                            "206 Partial Content",
                            &body[start..=end],
                            format!("content-range: bytes {start}-{end}/{}\r\n", body.len()),
                        ),
                        None => ("200 OK", &body[..], String::new()),
                    };
                    let head = format!(
                        "HTTP/1.1 {status}\r\ncontent-length: {}\r\naccept-ranges: bytes\r\n{extra}\r\n",
                        slice.len()
                    );
                    if socket.write_all(head.as_bytes()).await.is_err()
                        || socket.write_all(slice).await.is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    url
}

struct Probe {
    duration: f64,
    has_video: bool,
    has_audio: bool,
}

fn probe(tools: &Tools, file: &Path) -> Probe {
    let output = Command::new(&tools.ffprobe)
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_type",
            "-of",
            "default=nw=1",
        ])
        .arg(file)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    Probe {
        duration: text
            .lines()
            .find_map(|line| line.strip_prefix("duration="))
            .and_then(|value| value.parse().ok())
            .unwrap_or(0.0),
        has_video: text.contains("codec_type=video"),
        has_audio: text.contains("codec_type=audio"),
    }
}

async fn download_stream(manifest: Url, downloads: &Path, data: &Path) -> PathBuf {
    let settings = EngineSettings {
        download_dir: downloads.to_path_buf(),
        organize_by_category: false,
        connections_per_download: 4,
        ..Default::default()
    };
    let manager = DownloadManager::start(
        Store::open(data).unwrap(),
        Vec::new(),
        settings,
        tokio::runtime::Handle::current(),
    )
    .unwrap();
    let id = manager
        .add(NewDownload {
            url: manifest,
            file_name: None,
            folder: None,
            context: RequestContext::default(),
            start: StartMode::Queue,
            kind: DownloadKind::Detect,
            queue: None,
            page_url: None,
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let view = manager
            .snapshot()
            .into_iter()
            .find(|view| view.id == id)
            .unwrap();
        match &view.status {
            DownloadStatus::Completed => return view.target.unwrap(),
            DownloadStatus::Failed { reason } => panic!("stream download failed: {reason}"),
            _ if Instant::now() > deadline => {
                panic!("stream download timed out: {:?}", view.status)
            }
            _ => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

fn workspace(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("downpour-stream-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (media, downloads, data) = (
        root.join("media"),
        root.join("downloads"),
        root.join("data"),
    );
    for dir in [&media, &downloads, &data] {
        std::fs::create_dir_all(dir).unwrap();
    }
    (media, downloads, data)
}

fn assert_playable(tools: &Tools, file: &Path) {
    let probe = probe(tools, file);
    assert!(probe.has_video, "{} has no video", file.display());
    assert!(probe.has_audio, "{} has no audio", file.display());
    assert!(
        (probe.duration - CLIP_SECONDS).abs() < 0.6,
        "{} lasts {}s, expected about {CLIP_SECONDS}s",
        file.display(),
        probe.duration
    );
}

macro_rules! require_tools {
    () => {
        match tools() {
            Some(tools) => tools,
            None => {
                eprintln!("skipping: ffmpeg and ffprobe are not installed");
                return;
            }
        }
    };
}

#[tokio::test(flavor = "multi_thread")]
async fn downloads_encrypted_hls_into_mp4() {
    let tools = require_tools!();
    let (media, downloads, data) = workspace("hls-aes");
    package_encrypted_hls(&tools, &media);
    let base = serve_dir(media).await;
    let file = download_stream(base.join("master.m3u8").unwrap(), &downloads, &data).await;
    assert_eq!(file.extension().and_then(|e| e.to_str()), Some("mp4"));
    assert_playable(&tools, &file);
}

#[tokio::test(flavor = "multi_thread")]
async fn merges_dash_video_and_audio() {
    let tools = require_tools!();
    let (media, downloads, data) = workspace("dash");
    package_dash(&tools, &media);
    let base = serve_dir(media).await;
    let file = download_stream(base.join("manifest.mpd").unwrap(), &downloads, &data).await;
    assert_playable(&tools, &file);
    let leftovers: Vec<_> = std::fs::read_dir(file.parent().unwrap())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".dppart"))
        .collect();
    assert!(
        leftovers.is_empty(),
        "track files left behind: {leftovers:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn merges_fmp4_hls_with_separate_audio() {
    let tools = require_tools!();
    let (media, downloads, data) = workspace("hls-fmp4");
    package_fmp4_hls(&tools, &media);
    let base = serve_dir(media).await;
    let file = download_stream(base.join("master.m3u8").unwrap(), &downloads, &data).await;
    assert_playable(&tools, &file);
}
