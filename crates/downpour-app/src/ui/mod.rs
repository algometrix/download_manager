//! The main window: sidebar (queues, library, categories), the download list
//! or the statistics page, and a detail panel.
//!
//! Widgets never mutate state while drawing. They push [`Action`]s, which
//! the app applies after the frame is laid out.

mod details;
mod dialogs;
pub mod format;
mod hotkeys;
pub mod icons;
mod link_grabber;
mod list;
mod live_speed;
#[cfg(debug_assertions)]
mod screenshot;
mod sidebar;
mod stats;
mod theme;
mod toasts;
mod widgets;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use downpour_core::limiter::DownloadLimit;
use downpour_core::{
    Categories, CategoryId, DownloadId, DownloadManager, DownloadStatus, DownloadView,
    ManagerError, Queue, QueueId, QueuePlace, QueueState, RemoveFiles, Store,
};
use eframe::egui;
use url::Url;

use crate::app_settings::{
    AppSettings, Appearance, CloseAction, MinimizeAction, NewDownloadMode, WindowSize,
};
use crate::browser_link::BrowserLink;
use crate::desktop::{Announcement, Desktop, DesktopStart, Outcome, TitleBarColors};
use crate::duplicates::Arrival;
use crate::inbox::{handling_for, BrowserHandling, Delivery, Inbox, IncomingRequest, WindowEffect};
use crate::platform;
use crate::tray::{QuitRequest, Tray, WindowStart};
use dialogs::{
    AddUrlForm, BrowserForm, CategoriesForm, Dialog, DialogCommand, DuplicateForm, QueueForm,
    RefreshForm, RemoveForm, SettingsForm,
};
use theme::Placement;
use toasts::{ToastKind, Toasts};

const ACTIVE_REPAINT_INTERVAL: Duration = Duration::from_millis(250);
const SIDEBAR_WIDTH: f32 = 248.0;
const DETAILS_WIDTH: f32 = 320.0;
const COMPACT_DETAILS_WIDTH: f32 = 284.0;
/// Below this window width the details panel narrows to leave room for the list.
const COMPACT_WINDOW_WIDTH: f32 = 1200.0;
const SIDEBAR_PADDING: egui::Margin = egui::Margin::symmetric(16, 20);
const DETAILS_PADDING: egui::Margin = egui::Margin::same(20);
const PAGE_MARGIN: i8 = 24;
/// A bezel card's deep shadow and thick frame need more room than a soft
/// shadow, or they press against the panel edges.
const BEZEL_PAGE_MARGIN: i8 = 30;
/// Flat cards have no shadow to set them off, so the page gives them more air.
const FLAT_PAGE_MARGIN: i8 = 28;
/// Gap between a floating panel card and the window edge.
const FLOATING_GAP: i8 = 16;
/// Room between floating cards across the window: a little more than the
/// gap at the window edge, so the cards breathe.
const FLOATING_PAGE_MARGIN_X: i8 = 26;
/// Padding inside a floating sidebar card. It is narrower than the edge
/// sidebar's, so the card and its gap fit in `SIDEBAR_WIDTH` and the list
/// keeps its room.
const FLOATING_SIDEBAR_PADDING: egui::Vec2 = egui::vec2(14.0, 20.0);
const FLOATING_DETAILS_PADDING: egui::Vec2 = egui::vec2(18.0, 20.0);

/// Which downloads the list shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Queue(QueueId),
    All,
    Downloading,
    Completed,
    Unfinished,
    Category(CategoryId),
}

impl Filter {
    pub fn includes(self, download: &DownloadView) -> bool {
        match self {
            // A queue lists the work still to do; finished files
            // stay under All downloads, Completed, and their category.
            Filter::Queue(queue) => {
                download.queue == queue && download.status != DownloadStatus::Completed
            }
            Filter::All => true,
            Filter::Downloading => download.status.is_active(),
            Filter::Completed => download.status == DownloadStatus::Completed,
            Filter::Unfinished => download.status.can_resume(),
            Filter::Category(category) => download.category == category,
        }
    }
}

/// Names the sidebar and headings need: the queues and categories.
pub struct Catalog<'a> {
    pub queues: &'a [Queue],
    pub categories: &'a Categories,
    /// The speed limit from Settings, which downloads follow by default.
    pub global_limit_kib_per_second: Option<std::num::NonZeroU32>,
}

impl Catalog<'_> {
    pub fn title(&self, filter: Filter) -> String {
        match filter {
            Filter::Queue(id) => self
                .queues
                .iter()
                .find(|queue| queue.id == id)
                .map_or_else(|| "Main queue".to_owned(), |queue| queue.name.clone()),
            Filter::All => "All downloads".to_owned(),
            Filter::Downloading => "Downloading".to_owned(),
            Filter::Completed => "Completed".to_owned(),
            Filter::Unfinished => "Unfinished".to_owned(),
            Filter::Category(id) => self.categories.get(id).name.clone(),
        }
    }

    pub fn queue(&self, id: QueueId) -> Option<&Queue> {
        self.queues.iter().find(|queue| queue.id == id)
    }
}

/// What the central area shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Downloads,
    Statistics,
}

/// Something the user asked for while the frame was drawn.
pub enum Action {
    Select(Option<DownloadId>),
    SetFilter(Filter),
    ShowStatistics,
    RefreshStatistics,
    Pause(DownloadId),
    Resume(DownloadId),
    /// Start a queued download without waiting for its queue.
    StartNow(DownloadId),
    SetSpeedLimit(DownloadId, DownloadLimit),
    Restart(DownloadId),
    AskRemove(DownloadId),
    /// Drop finished downloads from the list; their files and history stay.
    ClearCompleted,
    OpenFile(PathBuf),
    Reveal(PathBuf),
    OpenAddUrl,
    OpenLinkGrabber,
    OpenSettings,
    PauseAll,
    ResumeAll,
    SetNewDownloadMode(NewDownloadMode),
    SetAppearance(Appearance),
    StartQueue(QueueId),
    StopQueue(QueueId),
    SetQueueLimit(QueueId, u8),
    NewQueue,
    RenameQueue(QueueId),
    RemoveQueue(QueueId),
    MoveToQueue(DownloadId, QueueId),
    /// Change where a download stands in the start order.
    MoveInQueue(DownloadId, QueuePlace),
    /// Say that the order can be changed only where a queue is shown.
    ExplainQueueOrder,
    ManageCategories,
    OpenPage(Url),
    /// Wait for the browser to send a fresh address for this download.
    RefreshAddress(DownloadId),
    CopyText(String),
}

pub struct App {
    ctx: egui::Context,
    manager: DownloadManager,
    store: Store,
    settings: AppSettings,
    inbox: Arc<Inbox>,
    page: Page,
    filter: Filter,
    search: String,
    selected: Option<DownloadId>,
    dialog: Option<Dialog>,
    pending_browser: VecDeque<Arrival>,
    /// The next link the browser sends replaces this download's address.
    awaiting_refresh: Option<DownloadId>,
    statistics: Option<stats::Statistics>,
    /// When the Statistics page was opened or refreshed, in egui time; its
    /// charts grow in from then.
    statistics_opened_at: f64,
    toasts: Toasts,
    /// Last seen status per download, to announce changes once.
    seen_status: HashMap<DownloadId, DownloadStatus>,
    /// `None` when the tray icon could not be created; closing then quits.
    tray: Option<Tray>,
    quit_request: QuitRequest,
    window: WindowPlace,
    /// Whether the window was minimized last frame; `None` before the first.
    was_minimized: Option<bool>,
    /// `InTray` until a start at sign-in has put the window in the tray.
    start: WindowStart,
    /// Notifications, taskbar progress, sleep, and title bar colors.
    desktop: Desktop,
    #[cfg(debug_assertions)]
    screenshot: Option<screenshot::ScreenshotHook>,
    /// `None` if the sampling thread could not start; the graph then stays empty.
    speed_sampler: Option<live_speed::SpeedSampler>,
    /// Shortcuts taken from this frame's raw input, acted on in `update`.
    pending_hotkeys: Vec<hotkeys::Command>,
    /// Input the screenshot hook delivers at the start of the next frame.
    #[cfg(debug_assertions)]
    injected_input: Vec<egui::Event>,
    /// A scroll the screenshot hook delivers at the start of the next frame.
    #[cfg(debug_assertions)]
    injected_scroll: Option<f32>,
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        manager: DownloadManager,
        store: Store,
        settings: AppSettings,
        inbox: Arc<Inbox>,
        startup_notices: Vec<String>,
        start: WindowStart,
    ) -> Self {
        theme::install(
            &cc.egui_ctx,
            theme::Look::chosen(
                settings.appearance,
                settings.color_theme,
                cc.egui_ctx.system_theme(),
            ),
        );
        let main_window = crate::platform::remember_main_window(cc);
        let ctx = cc.egui_ctx.clone();
        let repaint = ctx.clone();
        manager.set_change_listener(move || repaint.request_repaint());
        inbox.attach(&cc.egui_ctx);

        let mut toasts = Toasts::default();
        for notice in startup_notices {
            toasts.push(ToastKind::Error, "Recovered from a problem", notice);
        }
        let speed_sampler = match live_speed::SpeedSampler::start(manager.clone(), ctx.clone()) {
            Ok(sampler) => Some(sampler),
            Err(error) => {
                tracing::warn!(%error, "the live speed graph is off: its sampler did not start");
                None
            }
        };
        let quit_request = QuitRequest::default();
        let tray = match Tray::create(&cc.egui_ctx, manager.clone(), quit_request.clone()) {
            Ok(tray) => Some(tray),
            Err(error) => {
                tracing::warn!(%error, "no tray icon; closing the window will quit");
                toasts.push(
                    ToastKind::Error,
                    "No tray icon",
                    format!("Closing the window will quit Downpour ({error})."),
                );
                None
            }
        };
        let seen_status = manager
            .snapshot()
            .into_iter()
            .map(|download| (download.id, download.status))
            .collect();
        let clicked_inbox = Arc::clone(&inbox);
        let desktop = Desktop::start(DesktopStart {
            ctx: &ctx,
            window: main_window,
            manager: manager.clone(),
            store: store.clone(),
            settings: &settings,
            on_notification_click: Arc::new(move || clicked_inbox.push(IncomingRequest::Show)),
        });
        Self {
            ctx,
            manager,
            store,
            settings,
            inbox,
            page: Page::Downloads,
            // The window opens on the main queue.
            filter: Filter::Queue(QueueId::MAIN),
            search: String::new(),
            selected: None,
            dialog: None,
            pending_browser: VecDeque::new(),
            awaiting_refresh: None,
            statistics: None,
            statistics_opened_at: 0.0,
            toasts,
            seen_status,
            tray,
            quit_request,
            window: WindowPlace::Shown,
            was_minimized: None,
            start,
            desktop,
            #[cfg(debug_assertions)]
            screenshot: screenshot::ScreenshotHook::from_env(),
            speed_sampler,
            pending_hotkeys: Vec::new(),
            #[cfg(debug_assertions)]
            injected_input: Vec::new(),
            #[cfg(debug_assertions)]
            injected_scroll: None,
        }
    }

    fn handle_inbox(&mut self, ctx: &egui::Context) {
        for request in self.inbox.drain() {
            match request.window_effect() {
                WindowEffect::Open => show_window(ctx),
                // The dialog pops out on its own; the main window stays put.
                WindowEffect::PopUpDialog => {}
            }
            match request {
                Delivery::Show => {}
                Delivery::Quit => crate::tray::quit(ctx, &self.quit_request),
                // Auto-start downloads were added by the inbox already; only
                // those that need the dialog reach the window.
                Delivery::Downloads(arrivals) => self.pending_browser.extend(arrivals),
            }
        }
        if self.dialog.is_some() {
            return;
        }
        if let Some(arrival) = self.pending_browser.pop_front() {
            if let Some(id) = self.awaiting_refresh.take() {
                self.sync_inbox_handling();
                let name = self
                    .manager
                    .snapshot()
                    .into_iter()
                    .find(|download| download.id == id)
                    .map(|download| download.file_name);
                if let Some(name) = name {
                    let link = match arrival {
                        Arrival::New(link) | Arrival::Known(link, _) => link,
                    };
                    self.dialog = Some(Dialog::Refresh(RefreshForm::new(id, name, link)));
                    return;
                }
            }
            self.dialog = Some(match arrival {
                Arrival::Known(link, duplicate) => {
                    let look = self.settings.engine.categories.get(duplicate.category).look;
                    Dialog::Duplicate(DuplicateForm::new(link, duplicate, look))
                }
                Arrival::New(link) => self.browser_dialog(link),
            });
        }
    }

    /// The file-info dialog for a browser link, reading a stream's qualities.
    fn browser_dialog(&self, link: BrowserLink) -> Dialog {
        let form = BrowserForm::new(link, &self.settings.engine);
        if let Some(probe) = form.stream_probe() {
            self.manager
                .inspect_stream(probe.url, probe.context, probe.done);
        }
        Dialog::Browser(form)
    }

    /// Tells the inbox whether browser downloads may skip the window. While
    /// waiting for a fresh address they never may.
    fn sync_inbox_handling(&self) {
        let handling = match self.awaiting_refresh {
            Some(_) => BrowserHandling::Confirm,
            None => handling_for(self.settings.new_downloads),
        };
        self.inbox.set_handling(handling);
    }

    /// Announces each download that newly completed, failed, or ran out of room.
    fn announce_transitions(&mut self, downloads: &[DownloadView]) {
        let mut announcements = Vec::new();
        // Rebuilt from this frame's downloads, so removed ones drop out
        // without a second pass over the list.
        let mut earlier = std::mem::take(&mut self.seen_status);
        for download in downloads {
            let previous = earlier.remove(&download.id);
            self.seen_status
                .insert(download.id, download.status.clone());
            if previous.as_ref() == Some(&download.status) || previous.is_none() {
                continue;
            }
            if let Some(announcement) =
                Announcement::for_status(&download.file_name, &download.status)
            {
                let text = announcement.text();
                let kind = match announcement.outcome {
                    Outcome::Completed => {
                        // Totals changed; recompute on the next visit.
                        self.statistics = None;
                        ToastKind::Success
                    }
                    Outcome::Failed { .. } => ToastKind::Error,
                };
                self.toasts.push(kind, text.title, text.body);
                announcements.push(announcement);
                continue;
            }
            if let DownloadStatus::WaitingForSpace {
                directory,
                needed_bytes,
            } = &download.status
            {
                self.toasts.push(
                    ToastKind::Error,
                    "Not enough disk space",
                    format::space_shortage(&download.file_name, directory, *needed_bytes),
                );
            }
        }
        self.desktop
            .announce(announcements, self.settings.notifications);
    }

    fn apply(&mut self, action: Action, downloads: &[DownloadView]) {
        let result: Result<(), ManagerError> = match action {
            Action::Select(id) => {
                self.selected = id;
                Ok(())
            }
            Action::SetFilter(filter) => {
                self.filter = filter;
                self.page = Page::Downloads;
                Ok(())
            }
            Action::ShowStatistics => {
                self.page = Page::Statistics;
                self.selected = None;
                self.restart_statistics_intro();
                Ok(())
            }
            Action::RefreshStatistics => {
                self.statistics = None;
                self.restart_statistics_intro();
                Ok(())
            }
            Action::Pause(id) => self.manager.pause(id),
            Action::Resume(id) => self.manager.resume(id),
            Action::StartNow(id) => self.manager.start_now(id),
            Action::SetSpeedLimit(id, limit) => self.manager.set_speed_limit(id, limit),
            Action::Restart(id) => self.manager.restart(id),
            Action::AskRemove(id) => {
                if let Some(download) = downloads.iter().find(|download| download.id == id) {
                    let completed = download.status == DownloadStatus::Completed;
                    self.dialog = Some(Dialog::Remove(RemoveForm::new(
                        id,
                        download.file_name.clone(),
                        completed,
                    )));
                }
                Ok(())
            }
            Action::OpenFile(path) => {
                self.report_io("Could not open the file", platform::open_file(&path));
                Ok(())
            }
            Action::Reveal(path) => {
                self.report_io("Could not open the folder", platform::reveal(&path));
                Ok(())
            }
            Action::OpenAddUrl => {
                self.dialog = Some(Dialog::AddUrl(AddUrlForm::new(self.current_queue())));
                Ok(())
            }
            Action::OpenLinkGrabber => {
                self.dialog = Some(Dialog::LinkGrabber(link_grabber::LinkGrabberForm::new()));
                Ok(())
            }
            Action::OpenSettings => {
                self.dialog = Some(Dialog::Settings(SettingsForm::new(&self.settings)));
                Ok(())
            }
            Action::MoveInQueue(id, place) => self.manager.move_in_queue(id, place),
            Action::ExplainQueueOrder => {
                self.toasts.push(
                    ToastKind::Info,
                    "Open a queue to change the order",
                    format!(
                        "{} and {} move a download within its queue. Choose the queue on the left.",
                        hotkeys::MOVE_UP.keys(),
                        hotkeys::MOVE_DOWN.keys()
                    ),
                );
                Ok(())
            }
            Action::ClearCompleted => {
                self.clear_completed(downloads);
                Ok(())
            }
            Action::PauseAll => {
                self.manager.pause_all();
                Ok(())
            }
            Action::ResumeAll => {
                self.manager.resume_all();
                Ok(())
            }
            Action::SetAppearance(appearance) => {
                self.settings.appearance = appearance;
                self.save_settings();
                Ok(())
            }
            Action::SetNewDownloadMode(mode) => {
                self.settings.new_downloads = mode;
                self.sync_inbox_handling();
                self.save_settings();
                Ok(())
            }
            Action::StartQueue(id) => self.manager.start_queue(id),
            Action::StopQueue(id) => self.manager.stop_queue(id),
            Action::SetQueueLimit(id, limit) => self.manager.set_queue_limit(id, limit),
            Action::NewQueue => {
                self.dialog = Some(Dialog::Queue(QueueForm::create()));
                Ok(())
            }
            Action::RenameQueue(id) => {
                if let Some(queue) = self
                    .manager
                    .queues()
                    .into_iter()
                    .find(|queue| queue.id == id)
                {
                    self.dialog = Some(Dialog::Queue(QueueForm::rename(&queue)));
                }
                Ok(())
            }
            Action::RemoveQueue(id) => {
                let removed = self.manager.remove_queue(id);
                if removed.is_ok() && self.filter == Filter::Queue(id) {
                    self.filter = Filter::Queue(QueueId::MAIN);
                }
                removed
            }
            Action::MoveToQueue(download, queue) => self.manager.move_to_queue(download, queue),
            Action::ManageCategories => {
                self.dialog = Some(Dialog::Categories(CategoriesForm::new(
                    &self.settings.engine,
                )));
                Ok(())
            }
            Action::OpenPage(url) => {
                self.report_io("Could not open the page", platform::open_web_page(&url));
                Ok(())
            }
            Action::RefreshAddress(id) => {
                self.begin_refresh(id, downloads);
                Ok(())
            }
            Action::CopyText(text) => {
                self.ctx.copy_text(text);
                self.toasts.push(
                    ToastKind::Info,
                    "Copied",
                    "The address is on the clipboard.",
                );
                Ok(())
            }
        };
        if let Err(error) = result {
            self.toasts
                .push(ToastKind::Error, "That did not work", error.to_string());
        }
    }

    /// Refresh the download address: open the page the download came
    /// from; the next link the browser sends goes to this download.
    fn begin_refresh(&mut self, id: DownloadId, downloads: &[DownloadView]) {
        let Some(download) = downloads.iter().find(|download| download.id == id) else {
            return;
        };
        self.awaiting_refresh = Some(id);
        // The next link must open the refresh dialog, even with auto-start on.
        self.sync_inbox_handling();
        match download_page(download) {
            Some(page) => {
                self.report_io("Could not open the page", platform::open_web_page(&page));
                self.toasts.push(
                    ToastKind::Info,
                    "Waiting for a fresh address",
                    "Start the download (or play the video) again in the browser; Downpour will offer it for this entry.",
                );
            }
            None => self.toasts.push(
                ToastKind::Info,
                "Waiting for a fresh address",
                "This download has no saved page. Open the site in your browser and start the download again.",
            ),
        }
    }

    /// The queue new downloads from the Add URL box go to.
    fn current_queue(&self) -> QueueId {
        match self.filter {
            Filter::Queue(queue) => queue,
            _ => QueueId::MAIN,
        }
    }

    fn save_settings(&mut self) {
        if let Err(error) = self.settings.save(&self.store) {
            self.toasts.push(
                ToastKind::Error,
                "Settings were applied but not saved",
                error.to_string(),
            );
        }
    }

    #[cfg(debug_assertions)]
    fn run_screenshot_hook(&mut self, ctx: &egui::Context, downloads: &[DownloadView]) {
        let Some(hook) = &mut self.screenshot else {
            return;
        };
        for command in hook.update(ctx) {
            match command {
                screenshot::HookCommand::Apply(action) => self.apply(action, downloads),
                screenshot::HookCommand::CloseDialog => self.dialog = None,
                screenshot::HookCommand::Input(mut events) => {
                    self.injected_input.append(&mut events)
                }
                screenshot::HookCommand::Scroll(amount) => self.injected_scroll = Some(amount),
            }
        }
    }

    fn report_io(&mut self, title: &str, result: std::io::Result<()>) {
        if let Err(error) = result {
            self.toasts.push(ToastKind::Error, title, error.to_string());
        }
    }

    fn show_dialog(&mut self, ctx: &egui::Context, queues: &[Queue]) {
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let default_folder = self.settings.engine.download_dir.display().to_string();
        let Some(command) = dialogs::show(ctx, dialog, &default_folder, queues) else {
            return;
        };
        match command {
            DialogCommand::Close => self.dialog = None,
            DialogCommand::Add(request) => {
                let name = request.file_name.as_ref().map(|name| name.value.clone());
                match self.manager.add(request) {
                    Ok(id) => {
                        self.selected = Some(id);
                        let body = name
                            .unwrap_or_else(|| "The file name will come from the server.".into());
                        self.toasts.push(ToastKind::Info, "Download added", body);
                    }
                    Err(error) => self.toasts.push(
                        ToastKind::Error,
                        "Could not add the download",
                        error.to_string(),
                    ),
                }
                self.dialog = None;
            }
            DialogCommand::AddMany(requests) => {
                let total = requests.len();
                let failures: Vec<String> = requests
                    .into_iter()
                    .filter_map(|request| {
                        self.manager
                            .add(request)
                            .err()
                            .map(|error| error.to_string())
                    })
                    .collect();
                let added = total - failures.len();
                if added > 0 {
                    let noun = if added == 1 { "download" } else { "downloads" };
                    self.toasts.push(
                        ToastKind::Info,
                        format!("Added {added} {noun}"),
                        "Only files that are online were added.",
                    );
                }
                if let Some(first) = failures.first() {
                    self.toasts.push(
                        ToastKind::Error,
                        format!("{} could not be added", failures.len()),
                        first.clone(),
                    );
                }
                self.dialog = None;
            }
            DialogCommand::Refresh { id, url, context } => {
                match self.manager.refresh_address(id, url, context) {
                    Ok(()) => self.toasts.push(
                        ToastKind::Success,
                        "Address updated",
                        "The download continues where it stopped.",
                    ),
                    Err(error) => self.toasts.push(
                        ToastKind::Error,
                        "Could not update the address",
                        error.to_string(),
                    ),
                }
                self.selected = Some(id);
                self.dialog = None;
            }
            DialogCommand::AddInstead(link) => {
                self.dialog = Some(self.browser_dialog(link));
            }
            // Asked already, so it goes on as any new link would.
            DialogCommand::DownloadAgain(link) => match self.settings.new_downloads {
                NewDownloadMode::AskFirst => {
                    self.dialog = Some(self.browser_dialog(link));
                }
                NewDownloadMode::StartRightAway => {
                    self.dialog = None;
                    if let Err(error) = crate::inbox::add_browser_link(&self.manager, link) {
                        self.toasts.push(
                            ToastKind::Error,
                            "Could not add the download",
                            error.to_string(),
                        );
                    }
                }
            },
            DialogCommand::OpenFile(path) => {
                self.dialog = None;
                if let Err(error) = platform::open_file(&path) {
                    self.toasts.push(
                        ToastKind::Error,
                        "Could not open the file",
                        error.to_string(),
                    );
                }
            }
            DialogCommand::ShowExisting(id) => {
                self.dialog = None;
                self.page = Page::Downloads;
                self.filter = Filter::All;
                self.selected = Some(id);
                show_window(ctx);
            }
            DialogCommand::Remove { id, files } => {
                if let Err(error) = self.manager.remove(id, files) {
                    self.toasts.push(
                        ToastKind::Error,
                        "Could not remove the download",
                        error.to_string(),
                    );
                }
                if self.selected == Some(id) {
                    self.selected = None;
                }
                self.dialog = None;
            }
            DialogCommand::SaveSettings(settings) => {
                self.apply_settings(settings);
                self.dialog = None;
            }
            DialogCommand::SaveCategories(categories) => {
                let mut settings = self.settings.clone();
                settings.engine.categories = categories;
                self.apply_settings(settings);
                self.dialog = None;
            }
            DialogCommand::CreateQueue { name, max_active } => {
                match self
                    .manager
                    .add_queue(&name)
                    .and_then(|id| self.manager.set_queue_limit(id, max_active).map(|()| id))
                {
                    Ok(id) => {
                        self.filter = Filter::Queue(id);
                        self.page = Page::Downloads;
                        self.dialog = None;
                    }
                    Err(error) => {
                        if let Some(Dialog::Queue(form)) = &mut self.dialog {
                            form.error = Some(error.to_string());
                        }
                    }
                }
            }
            DialogCommand::UpdateQueue {
                id,
                name,
                max_active,
            } => {
                match self
                    .manager
                    .rename_queue(id, &name)
                    .and_then(|()| self.manager.set_queue_limit(id, max_active))
                {
                    Ok(()) => self.dialog = None,
                    Err(error) => {
                        if let Some(Dialog::Queue(form)) = &mut self.dialog {
                            form.error = Some(error.to_string());
                        }
                    }
                }
            }
            DialogCommand::RegisterBrowserIntegration => {
                let result = register_browser_integration();
                if let Some(Dialog::Settings(form)) = &mut self.dialog {
                    form.integration_result = Some(result);
                }
            }
            DialogCommand::AddToFirefox => {
                let result = crate::extensions::add_to_firefox();
                if let Some(Dialog::Settings(form)) = &mut self.dialog {
                    form.integration_result = Some(result);
                }
            }
        }
    }

    fn apply_settings(&mut self, mut settings: AppSettings) {
        if settings.sign_in_start != self.settings.sign_in_start {
            if let Err(error) = crate::desktop::sign_in::apply_choice(settings.sign_in_start) {
                self.toasts.push(
                    ToastKind::Error,
                    "Could not change starting with Windows",
                    error.to_string(),
                );
                // The setting keeps saying what Windows will really do.
                settings.sign_in_start = self.settings.sign_in_start;
            }
        }
        self.manager.update_settings(settings.engine.clone());
        // The engine normalizes what it was given; keep its version.
        settings.engine = self.manager.settings();
        self.settings = settings;
        self.sync_inbox_handling();
        self.save_settings();
    }

    /// The page's charts grow in again from now.
    fn restart_statistics_intro(&mut self) {
        self.statistics_opened_at = self.ctx.input(|input| input.time);
    }

    fn statistics(&mut self) -> &stats::Statistics {
        if self.statistics.is_none() {
            let history = match self.manager.history() {
                Ok(history) => history,
                Err(error) => {
                    self.toasts.push(
                        ToastKind::Error,
                        "Could not read the download history",
                        error.to_string(),
                    );
                    Vec::new()
                }
            };
            let today = chrono::Local::now().date_naive();
            self.statistics = Some(stats::summarize(&history, today, local_time));
        }
        self.statistics.as_ref().expect("just computed")
    }
}

/// The user's wall-clock time at `unix`, or `None` when it is out of range.
fn local_time(unix: u64) -> Option<chrono::NaiveDateTime> {
    let unix = i64::try_from(unix).ok()?;
    chrono::DateTime::from_timestamp(unix, 0)
        .map(|time| time.with_timezone(&chrono::Local).naive_local())
}

/// The page a download came from: the saved page, else the referrer the
/// browser sent (older entries and some captured downloads only have that).
pub fn download_page(download: &DownloadView) -> Option<Url> {
    download.page_url.clone().or_else(|| {
        download
            .referrer
            .as_deref()
            .and_then(|referrer| Url::parse(referrer).ok())
            .filter(|url| matches!(url.scheme(), "http" | "https"))
    })
}

/// Runs `downpour-host install --yes` from next to this executable. The user
/// clicking the button (after reading what it changes) is the approval.
fn register_browser_integration() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let host = exe.with_file_name(if cfg!(windows) {
        "downpour-host.exe"
    } else {
        "downpour-host"
    });
    let output = std::process::Command::new(&host)
        .args(["install", "--yes"])
        .output()
        .map_err(|error| format!("Could not run {}: {error}", host.display()))?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned()
        + &String::from_utf8_lossy(&output.stderr);
    if output.status.success() {
        Ok(text)
    } else {
        Err(text)
    }
}

/// Brings the window back from the tray, the taskbar, or behind others.
pub fn show_window(ctx: &egui::Context) {
    // A window hidden in the tray runs no frames, so the platform shows it
    // first; the viewport commands below then keep egui's state in step.
    crate::platform::show_main_window();
    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
}

/// Where the window is, so hiding to the tray is requested once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowPlace {
    Shown,
    InTray,
}

impl App {
    /// Closing or minimizing hides the window to the tray (per Settings),
    /// so downloads keep running; Quit from the tray menu really exits.
    fn handle_window_events(&mut self, ctx: &egui::Context) {
        let (close_requested, minimized) = ctx.input(|input| {
            (
                input.viewport().close_requested(),
                input.viewport().minimized == Some(true),
            )
        });
        // eframe opens the window invisible and Windows reports it as
        // minimized until the first frame is shown; only the user's change
        // from normal to minimized counts.
        self.remember_window_size(ctx, minimized);
        let was_minimized = self.was_minimized.replace(minimized);
        let just_minimized = minimized && was_minimized == Some(false);
        if self.tray.is_none() || self.quit_request.is_requested() {
            return;
        }
        if close_requested && self.settings.on_close == CloseAction::HideToTray {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.hide_to_tray(ctx);
        } else if just_minimized && self.settings.on_minimize == MinimizeAction::HideToTray {
            self.hide_to_tray(ctx);
        } else if !minimized && !close_requested {
            self.window = WindowPlace::Shown;
        }
    }

    /// Removes every finished download from the list, keeping its file.
    /// History is separate, so Statistics still counts them.
    fn clear_completed(&mut self, downloads: &[DownloadView]) {
        let finished: Vec<DownloadId> = downloads
            .iter()
            .filter(|download| download.status == DownloadStatus::Completed)
            .map(|download| download.id)
            .collect();
        let mut failures = Vec::new();
        for id in &finished {
            if let Err(error) = self.manager.remove(*id, RemoveFiles::Keep) {
                failures.push(error.to_string());
            }
        }
        if self.selected.is_some_and(|id| finished.contains(&id)) {
            self.selected = None;
        }
        let cleared = finished.len() - failures.len();
        tracing::info!(
            cleared,
            failed = failures.len(),
            "cleared finished downloads from the list"
        );
        if let Some(first) = failures.first() {
            self.toasts.push(
                ToastKind::Error,
                "Some finished downloads stayed in the list",
                format!("{} could not be removed: {first}", failures.len()),
            );
        }
        if cleared > 0 {
            let noun = if cleared == 1 {
                "download"
            } else {
                "downloads"
            };
            self.toasts.push(
                ToastKind::Success,
                "List cleared",
                format!("{cleared} finished {noun} removed from the list. The files stay on disk."),
            );
        }
    }

    /// Turns this frame's shortcuts into the actions their buttons send.
    fn hotkey_actions(
        &mut self,
        ctx: &egui::Context,
        downloads: &[DownloadView],
        queues: &[Queue],
    ) -> Vec<Action> {
        use hotkeys::Command;
        let commands = std::mem::take(&mut self.pending_hotkeys);
        if commands.is_empty() {
            return Vec::new();
        }
        let on_list = self.page == Page::Downloads;
        let visible = list::visible_downloads(downloads, self.filter, &self.search);
        let selected = self
            .selected
            .filter(|_| on_list)
            .and_then(|id| visible.iter().copied().find(|download| download.id == id));
        let shown_queue = match self.filter {
            Filter::Queue(id) if on_list => queues.iter().find(|queue| queue.id == id),
            _ => None,
        };
        let completed = |download: &&DownloadView| download.status == DownloadStatus::Completed;

        commands
            .into_iter()
            .filter_map(|command| match command {
                Command::AddUrl => Some(Action::OpenAddUrl),
                Command::AddLinks => Some(Action::OpenLinkGrabber),
                Command::PauseAll => Some(Action::PauseAll),
                Command::ResumeAll => Some(Action::ResumeAll),
                Command::SwitchTheme => Some(Action::SetAppearance(match theme::mode() {
                    theme::Mode::Light => Appearance::Dark,
                    theme::Mode::Dark => Appearance::Light,
                })),
                Command::Search => {
                    hotkeys::request_search_focus(ctx);
                    // From Statistics, go back to the list the search filters.
                    (!on_list).then_some(Action::SetFilter(self.filter))
                }
                Command::StartStopQueue => shown_queue.map(|queue| match queue.state {
                    QueueState::Running => Action::StopQueue(queue.id),
                    QueueState::Stopped => Action::StartQueue(queue.id),
                }),
                Command::ClearCompleted => {
                    (on_list && visible.iter().any(completed)).then_some(Action::ClearCompleted)
                }
                Command::Next | Command::Previous if on_list => {
                    let step = match command {
                        Command::Next => list::Step::Next,
                        _ => list::Step::Previous,
                    };
                    list::neighbor(&visible, self.selected, step)
                        .map(|download| Action::Select(Some(download.id)))
                }
                Command::Next | Command::Previous => None,
                Command::Deselect => selected.map(|_| Action::Select(None)),
                Command::Primary => selected
                    .and_then(list::primary_row_action)
                    .map(|(_, _, action)| action),
                Command::OpenFile => selected
                    .filter(completed)
                    .and_then(|download| download.target.clone())
                    .map(Action::OpenFile),
                Command::ShowInFolder => {
                    selected.map(|download| Action::Reveal(list::location(download)))
                }
                Command::CopyAddress => {
                    selected.map(|download| Action::CopyText(download.url.to_string()))
                }
                Command::Remove => selected.map(|download| Action::AskRemove(download.id)),
                Command::MoveUp | Command::MoveDown => {
                    let step = match command {
                        Command::MoveUp => list::Step::Previous,
                        _ => list::Step::Next,
                    };
                    let download = selected?;
                    // Only a queue view shows the start order.
                    if shown_queue.is_none() {
                        return Some(Action::ExplainQueueOrder);
                    }
                    list::place_after_step(&visible, download.id, step)
                        .map(|place| Action::MoveInQueue(download.id, place))
                }
            })
            .collect()
    }

    /// The saved look, or the one being tried in Settings so the window
    /// previews it; Cancel drops the form and with it the preview.
    fn shown_look(&self, ctx: &egui::Context) -> theme::Look {
        let (appearance, colors) = match &self.dialog {
            Some(Dialog::Settings(form)) => (form.appearance(), form.color_theme()),
            _ => (self.settings.appearance, self.settings.color_theme),
        };
        theme::Look::chosen(appearance, colors, ctx.system_theme())
    }

    /// Keeps the size of the open, normal window for the next start; it is
    /// saved with the settings.
    fn remember_window_size(&mut self, ctx: &egui::Context, minimized: bool) {
        let maximized = ctx.input(|input| input.viewport().maximized == Some(true));
        if minimized || maximized || self.window != WindowPlace::Shown {
            return;
        }
        let Some(rect) = ctx.input(|input| input.viewport().inner_rect) else {
            return;
        };
        if let Some(size) = WindowSize::remembered(rect.width(), rect.height()) {
            self.settings.window_size = Some(size);
        }
    }

    fn hide_to_tray(&mut self, ctx: &egui::Context) {
        if self.window == WindowPlace::InTray {
            return;
        }
        self.window = WindowPlace::InTray;
        // Minimized without a taskbar button where the platform allows, so
        // frames keep running for dialogs that pop out of the window.
        if !crate::platform::minimize_to_tray() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        tracing::info!("window hidden to the tray");
    }

    /// Started at sign-in, the window goes to the tray as soon as eframe has
    /// shown it (after the first frame; before that, eframe's own showing
    /// would bring it back).
    fn finish_start_in_tray(&mut self, ctx: &egui::Context) {
        if self.start != WindowStart::InTray {
            return;
        }
        if ctx.cumulative_pass_nr() == 0 {
            ctx.request_repaint();
            return;
        }
        self.start = WindowStart::Open;
        if self.tray.is_none() {
            // Without a tray icon the window is the only way back in.
            return;
        }
        self.hide_to_tray(ctx);
    }

    /// "Downpour · 2 downloading · 3.4 MB/s" on the tray icon.
    fn update_tray_tooltip(&mut self, downloads: &[DownloadView]) {
        let Some(tray) = &mut self.tray else {
            return;
        };
        let active = downloads
            .iter()
            .filter(|download| download.status.is_active())
            .count();
        let speed: u64 = downloads
            .iter()
            .map(|download| download.bytes_per_second)
            .sum();
        let text = match active {
            0 => "Downpour · nothing downloading".to_owned(),
            count => format!("Downpour · {count} downloading · {}", format::speed(speed)),
        };
        tray.set_tooltip(text);
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        #[cfg(debug_assertions)]
        raw_input.events.append(&mut self.injected_input);
        if hotkeys::main_window_has_keyboard(ctx, self.dialog.is_some()) {
            self.pending_hotkeys = hotkeys::take_pressed(raw_input);
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_window_events(ctx);
        self.finish_start_in_tray(ctx);
        // Follows a settings change, or Windows switching between light and dark.
        let look = self.shown_look(ctx);
        if look != theme::look() {
            theme::apply(ctx, look);
        }
        #[cfg(debug_assertions)]
        if let Some(offset) = self.injected_scroll.take() {
            dialogs::set_debug_scroll(ctx, offset);
        }
        self.handle_inbox(ctx);
        let downloads = self.manager.snapshot();
        self.update_tray_tooltip(&downloads);
        let title_bars = match theme::mode() {
            theme::Mode::Light => TitleBarColors::Light,
            theme::Mode::Dark => TitleBarColors::Dark,
        };
        self.desktop.update(&downloads, &self.settings, title_bars);
        let queues = self.manager.queues();
        self.announce_transitions(&downloads);
        if self
            .selected
            .is_some_and(|id| !downloads.iter().any(|download| download.id == id))
        {
            self.selected = None;
        }
        if let Filter::Queue(id) = self.filter {
            if !queues.iter().any(|queue| queue.id == id) {
                self.filter = Filter::Queue(QueueId::MAIN);
            }
        }

        let mut actions = self.hotkey_actions(ctx, &downloads, &queues);
        let categories = self.settings.engine.categories.clone();
        let catalog = Catalog {
            queues: &queues,
            categories: &categories,
            global_limit_kib_per_second: self.settings.engine.speed_limit_kib_per_second,
        };
        let placement = theme::form().panels;
        if placement == Placement::Floating {
            paint_floating_backdrop(ctx);
        }
        egui::SidePanel::left("sidebar")
            .exact_width(SIDEBAR_WIDTH)
            .resizable(false)
            .show_separator_line(false)
            .frame(sidebar_frame(placement))
            .show(ctx, |ui| {
                panel_card(
                    ui,
                    placement,
                    theme::sidebar(),
                    FLOATING_SIDEBAR_PADDING,
                    |ui| {
                        sidebar::show(
                            ui,
                            sidebar::SidebarState {
                                downloads: &downloads,
                                catalog: &catalog,
                                current: self.filter,
                                page: self.page,
                                settings: &self.settings,
                            },
                            &mut actions,
                        )
                    },
                )
            });

        let selected = self
            .selected
            .filter(|_| self.page == Page::Downloads)
            .and_then(|id| downloads.iter().find(|download| download.id == id));
        if let Some(download) = selected {
            let compact = ctx.screen_rect().width() < COMPACT_WINDOW_WIDTH;
            egui::SidePanel::right("details")
                .exact_width(if compact {
                    COMPACT_DETAILS_WIDTH
                } else {
                    DETAILS_WIDTH
                })
                .resizable(false)
                .show_separator_line(false)
                .frame(details_frame(placement))
                .show(ctx, |ui| {
                    panel_card(
                        ui,
                        placement,
                        theme::surface(),
                        FLOATING_DETAILS_PADDING,
                        |ui| details::show(ui, download, &catalog, &mut actions),
                    )
                });
        }

        let page = self.page;
        let statistics = (page == Page::Statistics).then(|| self.statistics().clone());
        let live = match &self.speed_sampler {
            Some(sampler) => {
                sampler.set_watching(page == Page::Statistics);
                sampler.history()
            }
            None => live_speed::SpeedHistory::default(),
        };
        egui::CentralPanel::default()
            .frame(page_frame(theme::form()))
            .show(ctx, |ui| match statistics {
                Some(statistics) => {
                    let intro = stats::Intro::since(
                        self.statistics_opened_at,
                        ui.input(|input| input.time),
                    );
                    stats::show(ui, &statistics, &live, intro, &mut actions)
                }
                None => {
                    let state = list::ListState {
                        downloads: &downloads,
                        catalog: &catalog,
                        filter: self.filter,
                        search: &mut self.search,
                        selected: self.selected,
                    };
                    list::show(ui, state, &mut actions);
                }
            });

        for action in actions {
            self.apply(action, &downloads);
        }
        self.show_dialog(ctx, &queues);
        if let Some(Dialog::LinkGrabber(form)) = &mut self.dialog {
            if let Some(check) = form.take_check() {
                let repaint = ctx.clone();
                let done = check.done;
                self.manager.check_links(check.links, move |checked| {
                    done(checked);
                    repaint.request_repaint();
                });
            }
        }
        self.toasts.show(ctx);

        let busy = downloads.iter().any(|download| download.status.is_active())
            || matches!(&self.dialog, Some(Dialog::Browser(form)) if form.is_probing())
            || matches!(&self.dialog, Some(Dialog::LinkGrabber(form)) if form.is_checking());
        if busy {
            ctx.request_repaint_after(ACTIVE_REPAINT_INTERVAL);
        }
        #[cfg(debug_assertions)]
        self.run_screenshot_hook(ctx, &downloads);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Err(error) = self.settings.save(&self.store) {
            tracing::error!(%error, "saving settings on exit failed");
        }
    }
}

/// Fills the whole window with the page backdrop, below every panel. Floating
/// panels leave their own frames unfilled, so their cards' shadows can reach
/// past the panel edges without the next panel's fill cutting them off.
fn paint_floating_backdrop(ctx: &egui::Context) {
    ctx.layer_painter(egui::LayerId::background()).rect_filled(
        ctx.screen_rect(),
        egui::CornerRadius::ZERO,
        theme::backdrop(),
    );
}

fn sidebar_frame(placement: Placement) -> egui::Frame {
    match placement {
        Placement::Edge => egui::Frame::new()
            .fill(theme::sidebar())
            .inner_margin(SIDEBAR_PADDING),
        // The page margin on the right side is the gap to the list.
        Placement::Floating => egui::Frame::new().inner_margin(egui::Margin {
            left: FLOATING_GAP,
            right: 0,
            top: FLOATING_GAP,
            bottom: FLOATING_GAP,
        }),
    }
}

fn details_frame(placement: Placement) -> egui::Frame {
    match placement {
        Placement::Edge => egui::Frame::new()
            .fill(theme::surface())
            .inner_margin(DETAILS_PADDING),
        Placement::Floating => egui::Frame::new().inner_margin(egui::Margin {
            left: 0,
            right: FLOATING_GAP,
            top: FLOATING_GAP,
            bottom: FLOATING_GAP,
        }),
    }
}

fn page_frame(form: &theme::Form) -> egui::Frame {
    let frame = egui::Frame::new().inner_margin(page_margin(form));
    match form.panels {
        Placement::Edge => frame.fill(theme::backdrop()),
        // See `paint_floating_backdrop`.
        Placement::Floating => frame,
    }
}

/// Room between the page's cards and the panel edges.
fn page_margin(form: &theme::Form) -> egui::Margin {
    match (form.panels, form.depth) {
        // Lines up with the floating panels' cards at the top and bottom.
        (Placement::Floating, _) => egui::Margin::symmetric(FLOATING_PAGE_MARGIN_X, FLOATING_GAP),
        (Placement::Edge, theme::Depth::Bezel { .. }) => egui::Margin::same(BEZEL_PAGE_MARGIN),
        (Placement::Edge, theme::Depth::Flat { .. }) => egui::Margin::same(FLAT_PAGE_MARGIN),
        (Placement::Edge, theme::Depth::Shadow { .. } | theme::Depth::Raised { .. }) => {
            egui::Margin::same(PAGE_MARGIN)
        }
    }
}

/// Shows a side panel's contents: straight in the panel at the window's edge,
/// or on a card of `fill` that fills the panel when it floats.
fn panel_card<R>(
    ui: &mut egui::Ui,
    placement: Placement,
    fill: egui::Color32,
    floating_padding: egui::Vec2,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    match placement {
        Placement::Edge => add_contents(ui),
        Placement::Floating => {
            let card = ui.max_rect();
            // The panel clips to its own rect, which would cut the shadow; a
            // painter for the whole layer clips to the window instead.
            // (`with_clip_rect` can only narrow the panel's clip.)
            let painter = ui.ctx().layer_painter(ui.layer_id());
            theme::paint_surface(&painter, card, theme::card_radius(), fill);
            ui.scope_builder(
                egui::UiBuilder::new()
                    .max_rect(card.shrink2(floating_padding))
                    .layout(*ui.layout()),
                add_contents,
            )
            .inner
        }
    }
}

/// A queue's running state as a short label.
pub fn queue_state_label(queue: &Queue) -> &'static str {
    match queue.state {
        QueueState::Running => "Running",
        QueueState::Stopped => "Stopped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_settings::ColorTheme;

    #[test]
    fn dashboard_pages_keep_their_margin() {
        for colors in [
            ColorTheme::Lavender,
            ColorTheme::Sunset,
            ColorTheme::Slate,
            ColorTheme::Ocean,
            ColorTheme::Graphite,
        ] {
            assert_eq!(
                page_margin(theme::form_for(colors)),
                egui::Margin::same(PAGE_MARGIN),
                "{colors:?}"
            );
        }
    }

    #[test]
    fn bezel_cards_get_more_room_than_soft_shadows() {
        let ember = page_margin(theme::form_for(ColorTheme::Ember));
        assert!(ember.left > PAGE_MARGIN && ember.top > PAGE_MARGIN);
    }
}
