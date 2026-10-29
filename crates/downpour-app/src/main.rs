#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_settings;
mod browser_link;
mod desktop;
mod duplicates;
mod extensions;
mod inbox;
mod ipc_server;
mod platform;
mod tray;
mod ui;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use downpour_core::{DownloadManager, Store};
use downpour_protocol::ipc::Endpoint;
use downpour_protocol::messages::{AppReply, AppRequest};

use crate::app_settings::{AppSettings, WindowSize};
use crate::inbox::Inbox;
use crate::tray::WindowStart;

const SHOW_EXISTING_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

fn init_logging(data_dir: &std::path::Path) {
    let filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    match std::fs::File::create(data_dir.join("downpour.log")) {
        Ok(file) => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(Mutex::new(file))
            .init(),
        Err(_) => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
}

/// Whether another instance owns the downloads. Opened by the user, it is
/// asked to come to the front; started at sign-in, it is left as it is.
fn is_already_running(data_dir: &std::path::Path, start: WindowStart) -> bool {
    let Ok(endpoint) = Endpoint::load(data_dir) else {
        return false;
    };
    match start {
        WindowStart::Open => endpoint
            .send(AppRequest::Show, SHOW_EXISTING_TIMEOUT)
            .is_ok_and(|reply| reply == AppReply::Shown),
        WindowStart::InTray => endpoint
            .send(AppRequest::Ping, SHOW_EXISTING_TIMEOUT)
            .is_ok_and(|reply| matches!(reply, AppReply::Pong { .. })),
    }
}

/// `downpour --add-to-firefox`: used by the installer's "Add to Firefox"
/// button. Prints what the user should do next.
fn add_to_firefox() -> ExitCode {
    match extensions::add_to_firefox() {
        Ok(next_step) => {
            println!("{next_step}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Launch {
    /// Open the window (or bring the running one forward), or start in the
    /// tray.
    Window(WindowStart),
    AddToFirefox,
}

/// Windows starts the app with this flag at sign-in.
use downpour_protocol::desktop::START_IN_TRAY_FLAG;
/// The native host starts the app with this flag; it and no options both
/// open the window.
use downpour_protocol::{ADD_TO_FIREFOX_FLAG, FROM_BROWSER_FLAG};

/// Where eframe keeps egui's own state, inside the data folder.
const UI_STATE_FOLDER: &str = "ui-state";

fn parse_launch(mut args: impl Iterator<Item = String>) -> Result<Launch, String> {
    match args.next().as_deref() {
        None | Some(FROM_BROWSER_FLAG) => Ok(Launch::Window(WindowStart::Open)),
        Some(START_IN_TRAY_FLAG) => Ok(Launch::Window(WindowStart::InTray)),
        Some(ADD_TO_FIREFOX_FLAG) => Ok(Launch::AddToFirefox),
        Some(other) => Err(format!("unknown option {other}")),
    }
}

fn main() -> ExitCode {
    let start = match parse_launch(std::env::args().skip(1)) {
        Ok(Launch::Window(start)) => start,
        Ok(Launch::AddToFirefox) => return add_to_firefox(),
        Err(error) => {
            eprintln!("downpour: {error}");
            return ExitCode::FAILURE;
        }
    };
    let Some(data_dir) = downpour_protocol::data_dir() else {
        eprintln!("downpour: cannot determine the per-user data directory");
        return ExitCode::FAILURE;
    };
    if let Err(error) = std::fs::create_dir_all(&data_dir) {
        eprintln!("downpour: cannot create {}: {error}", data_dir.display());
        return ExitCode::FAILURE;
    }
    if is_already_running(&data_dir, start) {
        return ExitCode::SUCCESS;
    }
    init_logging(&data_dir);
    desktop::identify_process();

    match run(data_dir, start) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "downpour exited with an error");
            eprintln!("downpour: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error(transparent)]
    Store(#[from] downpour_core::StoreError),
    #[error(transparent)]
    Manager(#[from] downpour_core::ManagerError),
    #[error("cannot start the async runtime: {0}")]
    Runtime(std::io::Error),
    #[error("cannot open the browser integration port: {0}")]
    Ipc(std::io::Error),
    #[error("window: {0}")]
    Window(String),
}

fn run(data_dir: PathBuf, start: WindowStart) -> Result<(), StartupError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("downpour-worker")
        .build()
        .map_err(StartupError::Runtime)?;

    let store = Store::open(&data_dir)?;
    let (records, mut startup_notices) = load_records(&store);
    let settings = AppSettings::load(&store).unwrap_or_else(|error| {
        startup_notices.push(format!("Settings were reset: {error}"));
        AppSettings::default()
    });
    desktop::sign_in::repair_at_startup(settings.sign_in_start);
    let manager = DownloadManager::start(
        store.clone(),
        records,
        settings.engine.clone(),
        runtime.handle().clone(),
    )?;

    let inbox = Arc::new(Inbox::new(manager.clone(), settings.new_downloads));
    let _ipc = ipc_server::start(&data_dir, Arc::clone(&inbox)).map_err(StartupError::Ipc)?;

    let size = settings.window_size.unwrap_or(WindowSize::DEFAULT);
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Downpour")
            .with_inner_size(size.points())
            .with_min_inner_size(WindowSize::MINIMUM.points())
            .with_icon(ui::icons::app_icon()),
        // eframe would save the window as it is at exit, and in the tray
        // that is minimized off-screen. The app remembers the size itself.
        persist_window: false,
        // In the data folder, so every copy (and every test copy) keeps its
        // own state instead of sharing one file per app name.
        persistence_path: Some(data_dir.join(UI_STATE_FOLDER)),
        ..Default::default()
    };
    let exit_manager = manager.clone();
    let result = eframe::run_native(
        "Downpour",
        options,
        Box::new(move |cc| {
            Ok(Box::new(ui::App::new(
                cc,
                manager,
                store,
                settings,
                inbox,
                startup_notices,
                start,
            )))
        }),
    );
    exit_manager.shutdown(SHUTDOWN_TIMEOUT);
    result.map_err(|error| StartupError::Window(error.to_string()))
}

/// A corrupt download list is moved aside rather than overwritten, so the
/// user's history can still be recovered by hand.
fn load_records(store: &Store) -> (Vec<downpour_core::DownloadRecord>, Vec<String>) {
    let error = match store.load_downloads() {
        Ok(records) => return (records, Vec::new()),
        Err(error) => error,
    };
    tracing::error!(%error, "download list unreadable");
    let notice = match store.quarantine(Store::downloads_file_name()) {
        Ok(aside) => format!(
            "The download list could not be read and was moved to {}.",
            aside.display()
        ),
        Err(move_error) => {
            format!("The download list could not be read ({error}) or moved aside ({move_error}).")
        }
    };
    (Vec::new(), vec![notice])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(args: &[&str]) -> Result<Launch, String> {
        parse_launch(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn the_native_host_flag_opens_the_window() {
        // downpour-host starts the app with this flag; rejecting it would leave
        // every browser download waiting for an app that never starts.
        assert_eq!(
            launch(&[FROM_BROWSER_FLAG]),
            Ok(Launch::Window(WindowStart::Open))
        );
        assert_eq!(launch(&[]), Ok(Launch::Window(WindowStart::Open)));
    }

    #[test]
    fn windows_starts_the_app_in_the_tray_at_sign_in() {
        // The Run value and this parser must agree on the flag.
        let command = downpour_protocol::desktop::run_at_sign_in_command(std::path::Path::new(
            r"C:\Programs\Downpour\downpour.exe",
        ));
        let flag = command.rsplit(' ').next().unwrap();
        assert_eq!(launch(&[flag]), Ok(Launch::Window(WindowStart::InTray)));
    }

    #[test]
    fn add_to_firefox_and_unknown_options() {
        assert_eq!(launch(&[ADD_TO_FIREFOX_FLAG]), Ok(Launch::AddToFirefox));
        assert!(launch(&["--bogus"]).is_err());
    }
}
