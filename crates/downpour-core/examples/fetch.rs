//! Downloads one URL with the engine and prints its progress; a development
//! aid for reproducing problems with a particular server.
//!
//! cargo run -p downpour-core --example fetch -- <url> <folder> [connections]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use downpour_core::{
    DownloadManager, DownloadStatus, EngineSettings, NewDownload, StartMode, Store,
};
use url::Url;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let url = Url::parse(&args.next().ok_or("missing <url>")?)?;
    let folder = PathBuf::from(args.next().ok_or("missing <folder>")?);
    let connections: u8 = args.next().map_or(Ok(8), |value| value.parse())?;

    tracing_subscriber::fmt()
        .with_env_filter("downpour_core=info")
        .init();
    let settings = EngineSettings {
        download_dir: folder.join("downloads"),
        organize_by_category: false,
        connections_per_download: connections,
        ..Default::default()
    };
    let manager = DownloadManager::start(
        Store::open(folder.join("state"))?,
        Vec::new(),
        settings,
        tokio::runtime::Handle::current(),
    )?;
    let id = manager.add(NewDownload::from_user(url, None, None, StartMode::Queue))?;
    let started = Instant::now();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let view = manager
            .snapshot()
            .into_iter()
            .find(|view| view.id == id)
            .ok_or("download vanished")?;
        println!(
            "{:>5.0}s {:?} {} / {:?} bytes, {} connections, {} B/s",
            started.elapsed().as_secs_f64(),
            view.status,
            view.received,
            view.size,
            view.connections,
            view.bytes_per_second
        );
        match view.status {
            DownloadStatus::Completed => {
                println!(
                    "saved to {}",
                    view.target
                        .map(|t| t.display().to_string())
                        .unwrap_or_default()
                );
                return Ok(());
            }
            DownloadStatus::Failed { reason } => return Err(reason.into()),
            _ => {}
        }
    }
}
