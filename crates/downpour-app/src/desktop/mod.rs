//! Windows desktop integration around the window: notifications, taskbar
//! progress, keeping the PC awake, title bar colors, starting at sign-in,
//! and saving everything when Windows ends the session. Each part does
//! nothing on other platforms.
//!
//! The app owns one [`Desktop`], fed once per frame from the UI thread,
//! which is where Windows wants the taskbar, power, and title bar calls.

#[cfg(windows)]
mod com;
mod notifications;
mod power;
pub mod sign_in;
mod taskbar;
mod title_bar;
mod window_messages;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use downpour_core::{DownloadManager, DownloadView, Store};
use eframe::egui;

use crate::app_settings::{AppSettings, DownloadNotifications};
use crate::platform::MainWindow;
use notifications::{AppFocus, Notifier};
use power::SleepBlocker;
use taskbar::{ProgressTracker, TaskbarButton, TransferState};
use title_bar::TitleBars;
use window_messages::{SystemColors, WindowWatch};

pub use notifications::{Announcement, OnClick, Outcome};
pub use title_bar::TitleBarColors;

/// How long downloads get to pause when Windows ends the session. Windows
/// itself waits about five seconds before it offers to end the app.
const SESSION_END_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// What the desktop integration needs from the app when it starts.
pub struct DesktopStart<'a> {
    pub ctx: &'a egui::Context,
    /// `None` where the platform gave no native handle; the taskbar and
    /// session-end parts are off then.
    pub window: Option<MainWindow>,
    pub manager: DownloadManager,
    pub store: Store,
    pub settings: &'a AppSettings,
    pub on_notification_click: OnClick,
}

pub struct Desktop {
    ctx: egui::Context,
    /// `None` when the notification thread did not start; logged then.
    notifier: Option<Notifier>,
    progress: ProgressTracker,
    /// `None` without a native main window.
    taskbar: Option<TaskbarButton>,
    /// `None` when the main window's messages cannot be watched; logged then.
    watch: Option<WindowWatch>,
    sleep: SleepBlocker,
    title_bars: TitleBars,
    /// The settings as last seen, for saving when the session ends.
    latest_settings: Arc<Mutex<AppSettings>>,
}

impl Desktop {
    /// Call on the UI thread once the main window exists.
    pub fn start(start: DesktopStart<'_>) -> Desktop {
        let notifier = match Notifier::start(start.on_notification_click) {
            Ok(notifier) => Some(notifier),
            Err(error) => {
                tracing::warn!(%error, "Windows notifications are off");
                None
            }
        };
        let latest_settings = Arc::new(Mutex::new(start.settings.clone()));
        let watch = start.window.and_then(|window| {
            let on_session_end =
                save_on_session_end(start.manager, start.store, Arc::clone(&latest_settings));
            match window_messages::watch_main_window(window, on_session_end, start.ctx.clone()) {
                Ok(watch) => Some(watch),
                Err(error) => {
                    tracing::warn!(%error, "shutting down Windows will not save the latest progress");
                    None
                }
            }
        });
        Desktop {
            ctx: start.ctx.clone(),
            notifier,
            progress: ProgressTracker::default(),
            taskbar: start.window.map(TaskbarButton::new),
            watch,
            sleep: SleepBlocker::new(),
            title_bars: TitleBars::new(),
            latest_settings,
        }
    }

    /// Follows this frame's downloads, settings, and theme.
    pub fn update(
        &mut self,
        downloads: &[DownloadView],
        settings: &AppSettings,
        colors: TitleBarColors,
    ) {
        let progress = self
            .progress
            .update(downloads.iter().map(TransferState::from));
        if let Some(taskbar) = &mut self.taskbar {
            if self
                .watch
                .as_ref()
                .is_some_and(WindowWatch::take_taskbar_button_created)
            {
                taskbar.forget_shown();
            }
            taskbar.show(progress);
        }
        self.sleep.set(power::wanted_hold(
            settings.sleep,
            downloads.iter().map(|download| &download.status),
        ));
        let system = self
            .watch
            .as_ref()
            .map_or(SystemColors::Unchanged, WindowWatch::take_system_colors);
        self.title_bars.follow(colors, system);
        let mut latest = self
            .latest_settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if *latest != *settings {
            latest.clone_from(settings);
        }
    }

    /// Announces one frame's finished and failed downloads, when the setting
    /// allows it and the user is not in the app.
    pub fn announce(&self, batch: Vec<Announcement>, setting: DownloadNotifications) {
        let Some(notifier) = &self.notifier else {
            return;
        };
        if batch.is_empty() || !notifications::should_notify(setting, app_focus(&self.ctx)) {
            return;
        }
        notifier.announce(batch);
    }
}

/// Whether any Downpour window (the main one or a dialog) has the focus.
fn app_focus(ctx: &egui::Context) -> AppFocus {
    let focused = ctx.input(|input| {
        input
            .raw
            .viewports
            .values()
            .any(|viewport| viewport.focused == Some(true))
    });
    match focused {
        true => AppFocus::Focused,
        false => AppFocus::Elsewhere,
    }
}

fn save_on_session_end(
    manager: DownloadManager,
    store: Store,
    latest_settings: Arc<Mutex<AppSettings>>,
) -> window_messages::OnSessionEnd {
    Box::new(move || {
        tracing::info!("Windows is ending the session; saving settings and downloads");
        let settings = latest_settings
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Err(error) = settings.save(&store) {
            tracing::error!(%error, "saving settings as the session ends failed");
        }
        // Pauses the transfers so their progress is saved, then saves the list.
        manager.shutdown(SESSION_END_SHUTDOWN_TIMEOUT);
    })
}

/// Gives this process Downpour's AppUserModelID, the one its shortcuts and
/// notification registration carry, so the taskbar groups the window with a
/// pinned shortcut and notifications show Downpour's name. Must run before
/// the first window is created.
pub fn identify_process() {
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        let id: Vec<u16> = downpour_protocol::desktop::APP_USER_MODEL_ID
            .encode_utf16()
            .chain([0])
            .collect();
        // SAFETY: `id` is a NUL-terminated UTF-16 string.
        let result = unsafe { SetCurrentProcessExplicitAppUserModelID(id.as_ptr()) };
        // An HRESULT fails when negative; S_FALSE and other positive codes
        // are successes.
        if result < 0 {
            tracing::warn!(
                hresult = format_args!("{result:#010x}"),
                "could not set the AppUserModelID"
            );
        }
    }
}
