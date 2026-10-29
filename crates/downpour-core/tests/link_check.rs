//! Link checks against a local file server.

use downpour_core::link_check::{check_all, links_in, CheckedLink, LinkStatus};
use downpour_core::{FileName, FileNameSource, RequestContext};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

/// Serves `/file.zip` (1000 bytes, ranges supported, named by the server),
/// `/plain.bin` (1000 bytes, ranges ignored, no name), `/page` (HTML),
/// `/gone` (410), `/forbidden` (403), and 404 for anything else.
async fn file_server() -> Url {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
                let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let reply: Vec<u8> = match path.as_str() {
                    "/file.zip" => {
                        let body = file_body();
                        // Honors single ranges, like a real file server.
                        let (start, end) = request
                            .lines()
                            .find_map(|line| line.to_ascii_lowercase().strip_prefix("range: bytes=").map(str::to_owned))
                            .and_then(|spec| {
                                let (start, end) = spec.trim().split_once('-')?;
                                let start: usize = start.parse().ok()?;
                                let end = end.parse().unwrap_or(body.len() - 1).min(body.len() - 1);
                                Some((start, end))
                            })
                            .unwrap_or((0, body.len() - 1));
                        let head = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/zip\r\nContent-Range: bytes {start}-{end}/{}\r\nContent-Disposition: attachment; filename=\"backup.zip\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len(),
                            end - start + 1
                        );
                        [head.as_bytes(), &body[start..=end]].concat()
                    }
                    "/plain.bin" => {
                        let body = file_body();
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        [head.as_bytes(), &body].concat()
                    }
                    "/page" => b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 6\r\nConnection: close\r\n\r\n<html>".to_vec(),
                    "/gone" => b"HTTP/1.1 410 Gone\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                    "/forbidden" => b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                    _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
                };
                let _ = socket.write_all(&reply).await;
            });
        }
    });
    url
}

/// The 1000 bytes of the served files.
fn file_body() -> Vec<u8> {
    (0..1000u32).map(|i| (i * 7 % 251) as u8).collect()
}

async fn check(links: Vec<Url>) -> Vec<CheckedLink> {
    check_all(&reqwest::Client::new(), links, RequestContext::default()).await
}

#[tokio::test]
async fn pasted_links_are_sorted_into_online_offline_and_unknown() {
    let files = file_server().await;
    let pasted = format!(
        "{files}file.zip\n{files}missing.zip\n{files}page\n{files}gone\n{files}forbidden\n"
    );

    let checked = check(links_in(&pasted)).await;

    assert_eq!(checked.len(), 5);
    assert_eq!(checked[0].url.as_str(), format!("{files}file.zip"));
    assert_eq!(
        checked[0].status,
        LinkStatus::Online {
            name: FileName::new("backup.zip", FileNameSource::Server),
            size: Some(1000)
        }
    );
    assert!(matches!(checked[1].status, LinkStatus::Offline { .. }));
    assert!(
        matches!(&checked[2].status, LinkStatus::Unknown { reason } if reason.contains("web page"))
    );
    assert!(matches!(&checked[3].status, LinkStatus::Offline { reason } if reason.contains("410")));
    assert!(matches!(&checked[4].status, LinkStatus::Unknown { reason } if reason.contains("403")));
}

#[tokio::test]
async fn a_server_ignoring_ranges_gives_the_size_and_the_url_names_the_file() {
    let files = file_server().await;
    let link = files.join("plain.bin").unwrap();

    let checked = check(vec![link.clone()]).await;

    assert_eq!(
        checked[0].status,
        LinkStatus::Online {
            name: FileName::guess_from_url(&link),
            size: Some(1000)
        }
    );
    let LinkStatus::Online { name, .. } = &checked[0].status else {
        unreachable!("checked above");
    };
    assert_eq!(name.value, "plain.bin");
    assert_eq!(name.source, FileNameSource::Url);
}

#[tokio::test]
async fn unreachable_servers_are_unknown() {
    // A port that was free a moment ago and has nobody listening now.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let link = Url::parse(&format!("http://{address}/file.zip")).unwrap();

    let checked = check(vec![link.clone()]).await;

    assert_eq!(checked.len(), 1);
    assert_eq!(checked[0].url, link);
    assert!(matches!(checked[0].status, LinkStatus::Unknown { .. }));
}
