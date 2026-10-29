use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use downpour_core::category::parse_extensions;
use downpour_core::queue::QUEUE_LIMIT_RANGE;
use downpour_core::settings::CONNECTION_RANGE;
use downpour_core::stream::mux::locate_ffmpeg;
use downpour_core::stream::{StreamError, StreamFormat, StreamOffer, VideoQuality};
use downpour_core::{
    Categories, Category, CategoryId, CategoryLook, DownloadId, DownloadKind, EngineSettings,
    FileName, FileNameSource, NewDownload, Queue, QueueId, RemoveFiles, RequestContext, StartMode,
};
use eframe::egui::{self, vec2, Align, Key, Layout, Ui};
use url::Url;

use super::icons::Icon;
use super::link_grabber::{self, LinkGrabberCommand, LinkGrabberForm};
use super::widgets::{
    button, button_width, choice_list, dropdown, field_frame, field_label, file_tile, galley,
    nav_item, segmented, stepper, text_field, toggle_row, ButtonKind, Choice, NavEntry,
};
use super::{format, hotkeys, theme};
use crate::app_settings::{
    AppSettings, Appearance, CloseAction, ColorTheme, DownloadNotifications, MinimizeAction,
    NewDownloadMode, SignInStart, SleepWhileDownloading,
};
use crate::browser_link::{default_folder, BrowserLink};
use crate::duplicates::{Duplicate, Standing, Unfinished};

const DIALOG_WIDTH: f32 = 480.0;
const WIDE_DIALOG_WIDTH: f32 = 660.0;
const DEFAULT_SPEED_LIMIT_KIB: u32 = 1024;
const CATEGORY_LIST_WIDTH: f32 = 180.0;

pub enum Dialog {
    AddUrl(AddUrlForm),
    Browser(BrowserForm),
    Duplicate(DuplicateForm),
    Refresh(RefreshForm),
    Remove(RemoveForm),
    Settings(SettingsForm),
    Categories(CategoriesForm),
    Queue(QueueForm),
    LinkGrabber(LinkGrabberForm),
}

pub enum DialogCommand {
    Close,
    Add(NewDownload),
    /// Several downloads at once, from the link checker.
    AddMany(Vec<NewDownload>),
    /// Use the browser's new address for an existing download.
    Refresh {
        id: DownloadId,
        url: Url,
        context: RequestContext,
    },
    /// The browser's link was not meant as a refresh: offer it as a new download.
    AddInstead(BrowserLink),
    /// The user wants a link they already have once more.
    DownloadAgain(BrowserLink),
    OpenFile(PathBuf),
    /// Select the earlier download in the list and show the window.
    ShowExisting(DownloadId),
    Remove {
        id: DownloadId,
        files: RemoveFiles,
    },
    SaveSettings(AppSettings),
    SaveCategories(Categories),
    CreateQueue {
        name: String,
        max_active: u8,
    },
    UpdateQueue {
        id: QueueId,
        name: String,
        max_active: u8,
    },
    RegisterBrowserIntegration,
    AddToFirefox,
}

pub struct AddUrlForm {
    url: String,
    file_name: String,
    folder: String,
    queue: QueueId,
    error: Option<String>,
}

impl AddUrlForm {
    pub fn new(queue: QueueId) -> Self {
        Self {
            url: String::new(),
            file_name: String::new(),
            folder: String::new(),
            queue,
            error: None,
        }
    }
}

/// What reading a stream's manifest has shown so far.
enum OfferState {
    Loading,
    Ready(StreamOffer),
    Failed(String),
}

/// A manifest read the app should start for the browser dialog.
pub struct StreamProbe {
    pub url: Url,
    pub context: RequestContext,
    pub done: Box<dyn FnOnce(Result<StreamOffer, StreamError>) + Send>,
}

pub struct BrowserForm {
    link: BrowserLink,
    file_name: String,
    category: CategoryId,
    folder: String,
    folder_source: FolderSource,
    queue: QueueId,
    /// Set when the item is an HLS or DASH manifest.
    stream: Option<StreamChoice>,
    /// The engine settings the dialog opened with, for category folders.
    settings: EngineSettings,
}

/// Whether the save folder still follows the chosen category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FolderSource {
    Category,
    /// The user typed or browsed to a folder; changing the category keeps it.
    Custom,
}

/// Whether the stream's quality was picked for this download.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum QualitySource {
    /// The default from Settings; the engine applies Settings when it starts.
    Settings,
    /// Picked on the page's video button or in this dialog.
    Chosen,
}

struct StreamChoice {
    format: StreamFormat,
    quality: VideoQuality,
    quality_source: QualitySource,
    offer: Arc<Mutex<OfferState>>,
    ffmpeg_found: bool,
}

impl BrowserForm {
    pub fn new(link: BrowserLink, settings: &EngineSettings) -> Self {
        let file_name = link.suggested_name();
        let (quality, quality_source) = match link.picked_quality() {
            Some(picked) => (picked, QualitySource::Chosen),
            None => (settings.video_quality, QualitySource::Settings),
        };
        let stream = link.stream().map(|format| StreamChoice {
            format,
            quality,
            quality_source,
            offer: Arc::new(Mutex::new(OfferState::Loading)),
            ffmpeg_found: locate_ffmpeg(settings.ffmpeg_path.as_deref()).is_some(),
        });
        let category = link.category_for(&file_name, &settings.categories);
        let mut form = Self {
            link,
            file_name,
            category,
            folder: String::new(),
            folder_source: FolderSource::Category,
            queue: QueueId::MAIN,
            stream,
            settings: settings.clone(),
        };
        form.folder = form.category_folder().display().to_string();
        form
    }

    fn category_folder(&self) -> PathBuf {
        default_folder(&self.settings, self.category)
    }

    /// For a stream, the manifest read that fills in the quality choices.
    pub fn stream_probe(&self) -> Option<StreamProbe> {
        let stream = self.stream.as_ref()?;
        let offer = Arc::clone(&stream.offer);
        Some(StreamProbe {
            url: self.link.url().clone(),
            context: self.link.context(),
            done: Box::new(move |result| {
                let state = match result {
                    Ok(offer) => OfferState::Ready(offer),
                    Err(error) => OfferState::Failed(error.to_string()),
                };
                *offer.lock().unwrap_or_else(|p| p.into_inner()) = state;
            }),
        })
    }

    pub fn is_probing(&self) -> bool {
        self.stream.as_ref().is_some_and(|stream| {
            matches!(
                *stream.offer.lock().unwrap_or_else(|p| p.into_inner()),
                OfferState::Loading
            )
        })
    }
}

pub struct RefreshForm {
    id: DownloadId,
    name: String,
    link: BrowserLink,
}

impl RefreshForm {
    pub fn new(id: DownloadId, name: String, link: BrowserLink) -> Self {
        Self { id, name, link }
    }
}

/// Asks before downloading a link the user already has.
pub struct DuplicateForm {
    link: BrowserLink,
    duplicate: Duplicate,
    /// The earlier download's category look, for its tile.
    look: CategoryLook,
}

impl DuplicateForm {
    pub fn new(link: BrowserLink, duplicate: Duplicate, look: CategoryLook) -> Self {
        Self {
            link,
            duplicate,
            look,
        }
    }
}

pub struct RemoveForm {
    pub id: DownloadId,
    pub name: String,
    pub completed: bool,
    delete_file: bool,
}

impl RemoveForm {
    pub fn new(id: DownloadId, name: String, completed: bool) -> Self {
        Self {
            id,
            name,
            completed,
            delete_file: false,
        }
    }
}

pub struct SettingsForm {
    base: AppSettings,
    download_dir: String,
    organize_by_category: bool,
    connections: u8,
    limit_speed: bool,
    limit_kib: u32,
    new_downloads: NewDownloadMode,
    appearance: Appearance,
    color_theme: ColorTheme,
    on_close: CloseAction,
    on_minimize: MinimizeAction,
    start_with_windows: bool,
    notify: bool,
    keep_awake: bool,
    video_quality: VideoQuality,
    ffmpeg_path: String,
    pub integration_result: Option<Result<String, String>>,
}

impl SettingsForm {
    pub fn appearance(&self) -> Appearance {
        self.appearance
    }

    pub fn color_theme(&self) -> ColorTheme {
        self.color_theme
    }

    pub fn new(settings: &AppSettings) -> Self {
        let engine = &settings.engine;
        Self {
            base: settings.clone(),
            download_dir: engine.download_dir.display().to_string(),
            organize_by_category: engine.organize_by_category,
            connections: engine.connections_per_download,
            limit_speed: engine.speed_limit_kib_per_second.is_some(),
            limit_kib: engine
                .speed_limit_kib_per_second
                .map_or(DEFAULT_SPEED_LIMIT_KIB, NonZeroU32::get),
            new_downloads: settings.new_downloads,
            appearance: settings.appearance,
            color_theme: settings.color_theme,
            on_close: settings.on_close,
            on_minimize: settings.on_minimize,
            start_with_windows: settings.sign_in_start == SignInStart::StartInTray,
            notify: settings.notifications == DownloadNotifications::Show,
            keep_awake: settings.sleep == SleepWhileDownloading::KeepAwake,
            video_quality: engine.video_quality,
            ffmpeg_path: engine
                .ffmpeg_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            integration_result: None,
        }
    }

    fn to_settings(&self) -> AppSettings {
        let mut settings = self.base.clone();
        if let Some(folder) = chosen_folder(&self.download_dir) {
            settings.engine.download_dir = folder;
        }
        settings.engine.organize_by_category = self.organize_by_category;
        settings.engine.connections_per_download = self.connections;
        settings.engine.speed_limit_kib_per_second = if self.limit_speed {
            NonZeroU32::new(self.limit_kib)
        } else {
            None
        };
        settings.engine.video_quality = self.video_quality;
        settings.engine.ffmpeg_path = chosen_folder(&self.ffmpeg_path);
        settings.new_downloads = self.new_downloads;
        settings.appearance = self.appearance;
        settings.color_theme = self.color_theme;
        settings.on_close = self.on_close;
        settings.on_minimize = self.on_minimize;
        settings.sign_in_start = if self.start_with_windows {
            SignInStart::StartInTray
        } else {
            SignInStart::Manual
        };
        settings.notifications = if self.notify {
            DownloadNotifications::Show
        } else {
            DownloadNotifications::Off
        };
        settings.sleep = if self.keep_awake {
            SleepWhileDownloading::KeepAwake
        } else {
            SleepWhileDownloading::Allow
        };
        settings
    }
}

pub struct CategoriesForm {
    categories: Categories,
    download_dir: PathBuf,
    selected: CategoryId,
    editor: CategoryEditor,
    error: Option<String>,
}

/// Text fields for the selected category, applied when it changes.
struct CategoryEditor {
    name: String,
    extensions: String,
    folder: String,
    look: CategoryLook,
}

impl CategoryEditor {
    fn of(category: &Category) -> Self {
        Self {
            name: category.name.clone(),
            extensions: category.extensions.join(" "),
            folder: category
                .folder
                .as_ref()
                .map(|folder| folder.display().to_string())
                .unwrap_or_default(),
            look: category.look,
        }
    }
}

impl CategoriesForm {
    pub fn new(settings: &EngineSettings) -> Self {
        let categories = settings.categories.clone();
        let first = &categories.all()[0];
        Self {
            selected: first.id,
            editor: CategoryEditor::of(first),
            categories,
            download_dir: settings.download_dir.clone(),
            error: None,
        }
    }

    /// Writes the editor back into the working list. "Other" keeps no
    /// extensions: it holds whatever no other category claims.
    fn commit(&mut self) -> Result<(), String> {
        let extensions = if self.selected == CategoryId::OTHER {
            Vec::new()
        } else {
            parse_extensions(&self.editor.extensions)
        };
        let edited = Category {
            id: self.selected,
            name: self.editor.name.clone(),
            extensions,
            folder: chosen_folder(&self.editor.folder),
            look: self.editor.look,
        };
        self.categories
            .update(edited)
            .map_err(|error| error.to_string())
    }

    fn select(&mut self, id: CategoryId) {
        match self.commit() {
            Ok(()) => {
                self.selected = id;
                self.editor = CategoryEditor::of(self.categories.get(id));
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }
}

pub struct QueueForm {
    editing: Option<QueueId>,
    name: String,
    max_active: u8,
    pub error: Option<String>,
}

impl QueueForm {
    pub fn create() -> Self {
        Self {
            editing: None,
            name: String::new(),
            max_active: 3,
            error: None,
        }
    }

    pub fn rename(queue: &Queue) -> Self {
        Self {
            editing: Some(queue.id),
            name: queue.name.clone(),
            max_active: queue.max_active,
            error: None,
        }
    }
}

/// Shows the dialog as a modal; returns what the user decided, if anything.
/// Where a dialog appears.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Presentation {
    /// Over the main window, which the user is looking at.
    OverMainWindow,
    /// In a small window of its own, like a download dialog: it comes
    /// from the browser while the user is elsewhere, and the main window
    /// stays as it is.
    OwnWindow,
}

impl Dialog {
    pub fn presentation(&self) -> Presentation {
        match self {
            Dialog::Browser(_) | Dialog::Duplicate(_) | Dialog::Refresh(_) => {
                Presentation::OwnWindow
            }
            Dialog::AddUrl(_)
            | Dialog::Remove(_)
            | Dialog::Settings(_)
            | Dialog::Categories(_)
            | Dialog::Queue(_)
            | Dialog::LinkGrabber(_) => Presentation::OverMainWindow,
        }
    }

    fn width(&self) -> f32 {
        match self {
            Dialog::Categories(_) | Dialog::LinkGrabber(_) | Dialog::Settings(_) => {
                WIDE_DIALOG_WIDTH
            }
            _ => DIALOG_WIDTH,
        }
    }

    fn window_title(&self) -> &'static str {
        match self {
            Dialog::Refresh(_) => "Downpour: use this new address?",
            Dialog::Duplicate(_) => "Downpour: download this again?",
            _ => "Downpour: download file info",
        }
    }
}

const OWN_WINDOW_MARGIN: i8 = 24;

/// Shows the dialog where it belongs; returns what the user decided, if anything.
pub fn show(
    ctx: &egui::Context,
    dialog: &mut Dialog,
    default_folder: &str,
    queues: &[Queue],
) -> Option<DialogCommand> {
    match dialog.presentation() {
        Presentation::OverMainWindow => show_over_main_window(ctx, dialog, default_folder, queues),
        Presentation::OwnWindow => show_in_own_window(ctx, dialog, default_folder, queues),
    }
}

fn show_over_main_window(
    ctx: &egui::Context,
    dialog: &mut Dialog,
    default_folder: &str,
    queues: &[Queue],
) -> Option<DialogCommand> {
    let frame = theme::card_frame()
        .inner_margin(egui::Margin::same(24))
        .corner_radius(egui::CornerRadius::same(theme::form().window_radius));
    let modal = egui::Modal::new(egui::Id::new("downpour-dialog"))
        .frame(frame)
        .backdrop_color(theme::modal_backdrop());
    let width = dialog.width();
    let response = modal.show(ctx, |ui| {
        ui.set_width(width.min(ctx.screen_rect().width() - 80.0).max(0.0));
        body(ui, dialog, default_folder, queues)
    });
    if response.inner.is_none() && response.should_close() {
        return Some(DialogCommand::Close);
    }
    response.inner
}

/// A separate OS window, sized to the dialog. Closing it or pressing Esc
/// cancels, as in the main window.
fn show_in_own_window(
    ctx: &egui::Context,
    dialog: &mut Dialog,
    default_folder: &str,
    queues: &[Queue],
) -> Option<DialogCommand> {
    let width = dialog.width() + 2.0 * f32::from(OWN_WINDOW_MARGIN);
    let id = egui::ViewportId::from_hash_of("downpour-own-window-dialog");
    let builder = egui::ViewportBuilder::default()
        .with_title(dialog.window_title())
        .with_icon(super::icons::app_icon())
        .with_inner_size([width, 560.0])
        .with_resizable(false)
        .with_minimize_button(false)
        .with_maximize_button(false)
        .with_always_on_top();
    let title_bar = title_bar_theme_to_send(ctx, id);
    let mut command = None;
    ctx.show_viewport_immediate(id, builder, |ctx, _class| {
        if let Some(theme) = title_bar {
            ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(theme));
        }
        let panel = egui::Frame::new()
            .fill(theme::surface())
            .inner_margin(egui::Margin::same(OWN_WINDOW_MARGIN));
        let used = egui::CentralPanel::default()
            .frame(panel)
            .show(ctx, |ui| {
                // The content's own height: the panel itself always fills
                // the window, so it could only ever grow.
                let content = ui.vertical(|ui| body(ui, dialog, default_folder, queues));
                command = content.inner;
                content.response.rect.height()
            })
            .inner;
        // Fit the window to its content, which changes with the dialog.
        let wanted = used + 2.0 * f32::from(OWN_WINDOW_MARGIN);
        let current = ctx.input(|input| input.screen_rect().height());
        if (wanted - current).abs() > 1.0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(width, wanted)));
        }
        let cancelled = ctx.input(|input| {
            input.viewport().close_requested() || input.key_pressed(egui::Key::Escape)
        });
        if cancelled && command.is_none() {
            command = Some(DialogCommand::Close);
        }
    });
    command
}

const SWATCH_HEIGHT: f32 = 74.0;
const SWATCH_GAP: f32 = 10.0;
const SWATCH_NAME_FONT: f32 = 12.5;
/// The tiny window inside a swatch: its inset, card, button, and dot.
const SWATCH_INSET: f32 = 10.0;
const SWATCH_CARD_HEIGHT: f32 = 24.0;
const SWATCH_BUTTON: egui::Vec2 = vec2(26.0, 12.0);
const SWATCH_BUTTON_INSET_X: f32 = 6.0;
const SWATCH_DOT_RADIUS: f32 = 5.0;
/// A sidebar strip, drawn only for themes whose sidebar is not the plain
/// one along the edge, so the dashboard swatches stay as they were.
const SWATCH_SIDEBAR_WIDTH: f32 = 14.0;
/// A floating sidebar's gap from the swatch's edges.
const SWATCH_FLOAT_GAP: f32 = 5.0;
/// The tiny window is a few times smaller than the real one, so its
/// corners and frame shrink with it. These keep the dashboard swatch's
/// corners (12 outside, 6 on the card) as they were.
const SWATCH_WINDOW_SCALE: f32 = 2.0 / 3.0;
const SWATCH_CARD_SCALE: f32 = 3.0 / 8.0;
const SWATCH_CONTROL_SCALE: f32 = 0.4;
const SWATCH_BEZEL_SCALE: f32 = 0.6;
/// How far a raised swatch surface lifts, and how soft its edges are.
const SWATCH_RAISE: i8 = 2;
const SWATCH_RAISE_BLUR: u8 = 5;
/// Where the theme's name sits, above the swatch's bottom edge.
const SWATCH_NAME_RISE: f32 = 18.0;
/// How far an unchosen name is mixed toward ink: readable, but quieter.
const SWATCH_NAME_EMPHASIS: f32 = 0.6;

/// How the swatches wrap: as many per row as fit at their narrowest, then
/// spread evenly over the rows (ten at the settings width: two rows of five).
#[derive(Clone, Copy, Debug, PartialEq)]
struct SwatchGrid {
    per_row: usize,
    width: f32,
}

fn swatch_grid(available: f32, min_width: f32, count: usize) -> SwatchGrid {
    let count = count.max(1);
    let fit = ((available + SWATCH_GAP) / (min_width + SWATCH_GAP)).floor() as usize;
    let rows = count.div_ceil(fit.clamp(1, count));
    let per_row = count.div_ceil(rows);
    let width = ((available - SWATCH_GAP * (per_row - 1) as f32) / per_row as f32).max(0.0);
    SwatchGrid { per_row, width }
}

/// The narrowest swatch that shows every theme's name in full beside a
/// sidebar strip.
fn swatch_min_width(ui: &Ui) -> f32 {
    let widest_name = ColorTheme::ALL
        .iter()
        .map(|colors| {
            galley(
                ui,
                colors.name(),
                theme::semibold(SWATCH_NAME_FONT),
                theme::ink(),
            )
            .size()
            .x
        })
        .fold(0.0, f32::max);
    widest_name + SWATCH_FLOAT_GAP + SWATCH_SIDEBAR_WIDTH + 2.0 * SWATCH_INSET
}

/// One card per color theme, drawn as a tiny window in that theme's own
/// colors (in the current light or dark) and its own form: corners,
/// button shape, depth, and sidebar.
fn theme_picker(ui: &mut Ui, selected: &mut ColorTheme) {
    let grid = swatch_grid(
        ui.available_width(),
        swatch_min_width(ui),
        ColorTheme::ALL.len(),
    );
    // A scope, so the row gap stays out of the rest of the settings card.
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = SWATCH_GAP;
        for row in ColorTheme::ALL.chunks(grid.per_row) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = SWATCH_GAP;
                for &colors in row {
                    let (rect, response) = ui
                        .allocate_exact_size(vec2(grid.width, SWATCH_HEIGHT), egui::Sense::click());
                    if response.clicked() {
                        *selected = colors;
                    }
                    let chosen = *selected == colors;
                    let name_center_x = paint_swatch(ui.painter(), rect, colors);
                    let outline = match (chosen, response.hovered()) {
                        (true, _) => egui::Stroke::new(2.0, theme::primary()),
                        (false, true) => egui::Stroke::new(1.0, theme::ink_faint()),
                        (false, false) => egui::Stroke::new(1.0, theme::border()),
                    };
                    let form = theme::form_for(colors);
                    ui.painter().rect_stroke(
                        rect,
                        scaled_radius(form.window_radius, SWATCH_WINDOW_SCALE),
                        outline,
                        egui::StrokeKind::Inside,
                    );
                    let name_color = if chosen {
                        theme::ink()
                    } else {
                        theme::swatch(swatch_look(colors))
                            .ink_faint
                            .lerp_to_gamma(theme::ink(), SWATCH_NAME_EMPHASIS)
                    };
                    ui.painter().text(
                        egui::pos2(name_center_x, rect.bottom() - SWATCH_NAME_RISE),
                        egui::Align2::CENTER_CENTER,
                        colors.name(),
                        theme::semibold(SWATCH_NAME_FONT),
                        name_color,
                    );
                    response.on_hover_cursor(egui::CursorIcon::PointingHand);
                }
            });
        }
    });
}

/// A theme in the light or dark being drawn now.
fn swatch_look(colors: ColorTheme) -> theme::Look {
    theme::Look {
        colors,
        mode: theme::mode(),
    }
}

/// Paints `colors` as a tiny window filling `rect`: the backdrop, its
/// sidebar when that is part of the theme's look, and a card holding a
/// button and an accent dot. Returns the x the name centers on, under the
/// card.
fn paint_swatch(painter: &egui::Painter, rect: egui::Rect, colors: ColorTheme) -> f32 {
    let form = theme::form_for(colors);
    let swatch = theme::swatch(swatch_look(colors));
    let window_radius = scaled_radius(form.window_radius, SWATCH_WINDOW_SCALE);
    painter.rect_filled(rect, window_radius, swatch.backdrop);

    let content_left = match swatch_sidebar(rect, window_radius, form.sidebar, form.panels) {
        Some((strip, strip_radius)) => {
            paint_swatch_surface(painter, strip, strip_radius, swatch.sidebar, &swatch, form);
            strip.right() + SWATCH_INSET
        }
        None => rect.left() + SWATCH_INSET,
    };
    let card = egui::Rect::from_min_max(
        egui::pos2(content_left, rect.top() + SWATCH_INSET),
        egui::pos2(
            rect.right() - SWATCH_INSET,
            rect.top() + SWATCH_INSET + SWATCH_CARD_HEIGHT,
        ),
    );
    let card_radius = scaled_radius(form.card_radius, SWATCH_CARD_SCALE);
    paint_swatch_surface(painter, card, card_radius, swatch.surface, &swatch, form);

    let button = egui::Rect::from_min_size(
        egui::pos2(
            card.left() + SWATCH_BUTTON_INSET_X,
            card.center().y - SWATCH_BUTTON.y / 2.0,
        ),
        SWATCH_BUTTON,
    );
    let button_radius = match form.controls {
        theme::ControlShape::Rounded { radius } => scaled_radius(radius, SWATCH_CONTROL_SCALE),
        theme::ControlShape::Pill => theme::round_ends(SWATCH_BUTTON.y),
    };
    painter.rect_filled(button, button_radius, swatch.primary);
    painter.circle_filled(
        egui::pos2(
            card.right() - SWATCH_BUTTON_INSET_X - SWATCH_DOT_RADIUS,
            card.center().y,
        ),
        SWATCH_DOT_RADIUS,
        swatch.accent,
    );
    card.center().x
}

/// Where a swatch's sidebar strip goes, with its corners; `None` for the
/// plain sidebar along the edge, which the dashboard swatches never showed.
fn swatch_sidebar(
    rect: egui::Rect,
    window_radius: egui::CornerRadius,
    sidebar: theme::SidebarFill,
    panels: theme::Placement,
) -> Option<(egui::Rect, egui::CornerRadius)> {
    match (sidebar, panels) {
        (theme::SidebarFill::Plain, theme::Placement::Edge) => None,
        (theme::SidebarFill::Contrast { .. }, theme::Placement::Edge) => {
            let strip =
                egui::Rect::from_min_size(rect.min, vec2(SWATCH_SIDEBAR_WIDTH, rect.height()));
            // Flush with the swatch, so its outer corners follow the swatch's.
            let corners = egui::CornerRadius {
                nw: window_radius.nw,
                sw: window_radius.sw,
                ne: 0,
                se: 0,
            };
            Some((strip, corners))
        }
        (_, theme::Placement::Floating) => {
            let strip = egui::Rect::from_min_max(
                rect.min + vec2(SWATCH_FLOAT_GAP, SWATCH_FLOAT_GAP),
                egui::pos2(
                    rect.left() + SWATCH_FLOAT_GAP + SWATCH_SIDEBAR_WIDTH,
                    rect.bottom() - SWATCH_FLOAT_GAP,
                ),
            );
            let corners = egui::CornerRadius::same(
                window_radius
                    .nw
                    .saturating_sub(SWATCH_FLOAT_GAP as u8)
                    .min(theme::round_ends(SWATCH_SIDEBAR_WIDTH).nw),
            );
            Some((strip, corners))
        }
    }
}

/// A surface inside a swatch with the theme's depth, scaled down: raised
/// themes get a lit edge and a shade, the bezel theme its dark frame. A
/// drop shadow is too soft to read at this size, so shadowed themes stay
/// flat, as the swatches always were.
fn paint_swatch_surface(
    painter: &egui::Painter,
    rect: egui::Rect,
    radius: egui::CornerRadius,
    fill: egui::Color32,
    swatch: &theme::Swatch,
    form: &theme::Form,
) {
    match form.depth {
        theme::Depth::Shadow { .. } | theme::Depth::Flat { .. } => {
            painter.rect_filled(rect, radius, fill);
        }
        theme::Depth::Raised { .. } => {
            let edges = [
                (-SWATCH_RAISE, swatch.highlight),
                (SWATCH_RAISE, swatch.shadow),
            ];
            for (offset, color) in edges {
                let shadow = egui::Shadow {
                    offset: [offset, offset],
                    blur: SWATCH_RAISE_BLUR,
                    spread: 0,
                    color,
                };
                painter.add(shadow.as_shape(rect, radius));
            }
            painter.rect_filled(rect, radius, fill);
        }
        theme::Depth::Bezel { width, color, .. } => {
            painter.rect_filled(rect, radius, fill);
            painter.rect_stroke(
                rect,
                radius,
                egui::Stroke::new(f32::from(width) * SWATCH_BEZEL_SCALE, color),
                egui::StrokeKind::Inside,
            );
        }
    }
}

/// A full-size corner radius shrunk for the swatch's tiny window.
fn scaled_radius(radius: u8, scale: f32) -> egui::CornerRadius {
    egui::CornerRadius::same((f32::from(radius) * scale).round() as u8)
}

/// Windows draws the title bar light unless told otherwise. The theme is
/// sent when the window opens (it was not shown last frame) and when the
/// app's look changes, not every frame.
fn title_bar_theme_to_send(ctx: &egui::Context, id: egui::ViewportId) -> Option<egui::SystemTheme> {
    let wanted = match theme::mode() {
        theme::Mode::Light => egui::SystemTheme::Light,
        theme::Mode::Dark => egui::SystemTheme::Dark,
    };
    let pass = ctx.cumulative_pass_nr();
    let key = egui::Id::new(("downpour-own-window-title-bar", id));
    let last: Option<(u64, egui::SystemTheme)> = ctx.data(|data| data.get_temp(key));
    ctx.data_mut(|data| data.insert_temp(key, (pass, wanted)));
    match last {
        Some((shown_at, sent)) if shown_at + 1 == pass && sent == wanted => None,
        _ => Some(wanted),
    }
}

/// The file a dialog is about: its type tile, name, and one line of detail.
struct FileSummary<'a> {
    badge: String,
    look: CategoryLook,
    name: &'a str,
    detail: String,
}

/// A card nested in a dialog: softly tinted on most themes, pressed out of
/// the dialog on raised ones, with a control's lighter depth since a card's
/// full shadow is too heavy this close together. Its corners follow the
/// theme's cards, a little tighter (the more so the smaller the card), so
/// they stay parallel to the dialog's own.
pub(super) struct InsetCard {
    pub tighter_by: u8,
    pub margin: i8,
}

impl InsetCard {
    /// Draws the card behind `add_contents`, once their size is known.
    pub fn show<R>(
        &self,
        ui: &mut Ui,
        add_contents: impl FnOnce(&mut Ui) -> R,
    ) -> egui::InnerResponse<R> {
        let background = ui.painter().add(egui::Shape::Noop);
        let inner = egui::Frame::new()
            .inner_margin(egui::Margin::same(self.margin))
            .show(ui, add_contents);
        let radius = theme::inner_card_radius(self.tighter_by);
        ui.painter().set(
            background,
            theme::control_shapes(inner.response.rect, radius, theme::inset_fill(), None),
        );
        inner
    }
}

const FILE_CARD: InsetCard = InsetCard {
    tighter_by: 2,
    margin: 14,
};
const SETTINGS_CARD: InsetCard = InsetCard {
    tighter_by: 4,
    margin: 16,
};

/// A soft card with the file's summary; `more` adds rows inside it.
fn file_card(ui: &mut Ui, summary: FileSummary<'_>, more: impl FnOnce(&mut Ui)) {
    FILE_CARD.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let (tile, _) = ui.allocate_exact_size(vec2(44.0, 44.0), egui::Sense::hover());
            let (fg, bg) = match theme::look_colors(summary.look) {
                // The plain tile would vanish into the card; lift it off.
                (fg, bg) if bg == theme::inset_fill() => (fg, theme::surface()),
                colors => colors,
            };
            file_tile(ui, tile, &summary.badge, fg, bg);
            ui.add_space(4.0);
            ui.vertical(|ui| {
                ui.add_space(3.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(summary.name)
                            .font(theme::semibold(15.0))
                            .color(theme::ink()),
                    )
                    .truncate(),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(summary.detail)
                            .font(theme::regular(12.5))
                            .color(theme::ink_soft()),
                    )
                    .truncate(),
                );
            });
        });
        more(ui);
    });
}

fn body(
    ui: &mut Ui,
    dialog: &mut Dialog,
    default_folder: &str,
    queues: &[Queue],
) -> Option<DialogCommand> {
    {
        match dialog {
            Dialog::AddUrl(form) => add_url(ui, form, default_folder, queues),
            Dialog::Browser(form) => browser(ui, form, queues),
            Dialog::Refresh(form) => refresh(ui, form),
            Dialog::Duplicate(form) => duplicate(ui, form),
            Dialog::Remove(form) => remove(ui, form),
            Dialog::Settings(form) => settings(ui, form),
            Dialog::Categories(form) => categories(ui, form),
            Dialog::Queue(form) => queue(ui, form),
            Dialog::LinkGrabber(form) => {
                link_grabber::show(ui, form, queues).map(|command| match command {
                    LinkGrabberCommand::Close => DialogCommand::Close,
                    LinkGrabberCommand::Add(requests) => DialogCommand::AddMany(requests),
                })
            }
        }
    }
}

pub(super) fn title(ui: &mut Ui, text: &str, subtitle: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::title(20.0))
            .color(theme::ink()),
    );
    ui.add(
        egui::Label::new(
            egui::RichText::new(subtitle)
                .font(theme::regular(13.0))
                .color(theme::ink_soft()),
        )
        .wrap(),
    );
    ui.add_space(14.0);
}

pub(super) fn hint(ui: &mut Ui, text: &str) {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::regular(12.5))
                .color(theme::ink_soft()),
        )
        .wrap(),
    );
}

fn error_text(ui: &mut Ui, error: &Option<String>) {
    if let Some(error) = error {
        ui.add_space(6.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(error)
                    .font(theme::regular(12.5))
                    .color(theme::danger()),
            )
            .wrap(),
        );
    }
}

/// Folder field with a Browse button; the field gets the space the button leaves.
pub(super) fn folder_picker(ui: &mut Ui, label: &str, folder: &mut String, placeholder: &str) {
    field_label(ui, label);
    ui.horizontal(|ui| {
        let browse_width = button_width(ui, Some(Icon::Folder), "Browse");
        let field_width = ui.available_width() - browse_width - ui.spacing().item_spacing.x;
        text_field(ui, folder, placeholder, None, field_width);
        if button(ui, ButtonKind::Secondary, Some(Icon::Folder), "Browse").clicked() {
            if let Some(picked) = rfd::FileDialog::new().pick_folder() {
                *folder = picked.display().to_string();
            }
        }
    });
}

pub(super) fn chosen_folder(folder: &str) -> Option<PathBuf> {
    let folder = folder.trim();
    (!folder.is_empty()).then(|| downpour_core::paths::native(Path::new(folder)))
}

pub(super) fn queue_picker(ui: &mut Ui, id: &str, selected: &mut QueueId, queues: &[Queue]) {
    field_label(ui, "Queue");
    let current = queues
        .iter()
        .find(|queue| queue.id == *selected)
        .map_or("Main queue", |queue| queue.name.as_str());
    let width = ui.available_width();
    dropdown(ui, id, width, current, |ui| {
        for queue in queues {
            ui.selectable_value(selected, queue.id, &queue.name);
        }
    });
}

/// Buttons laid out right to left: the primary action is rightmost.
pub(super) fn footer(ui: &mut Ui, add_buttons: impl FnOnce(&mut Ui)) {
    ui.add_space(18.0);
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), 36.0),
        Layout::right_to_left(Align::Center),
        add_buttons,
    );
}

/// Accepts pasted links without a scheme by assuming HTTPS.
pub fn parse_user_url(text: &str) -> Result<Url, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("Enter the address of the file to download.".into());
    }
    let parsed = Url::parse(text).or_else(|error| {
        if text.contains("://") {
            Err(error)
        } else {
            Url::parse(&format!("https://{text}"))
        }
    });
    match parsed {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() => Ok(url),
        Ok(url) => Err(format!(
            "{}: links are not supported; use http or https.",
            url.scheme()
        )),
        Err(error) => Err(format!("This is not a valid address ({error}).")),
    }
}

fn add_url(
    ui: &mut Ui,
    form: &mut AddUrlForm,
    default_folder: &str,
    queues: &[Queue],
) -> Option<DialogCommand> {
    title(
        ui,
        "Add download",
        &format!(
            "Paste a link to one file, or to an HLS (.m3u8) or DASH (.mpd) video stream. \
             For many links at once, use {}.",
            hotkeys::ADD_LINKS.label("Add links")
        ),
    );
    field_label(ui, "Address");
    let url_field = text_field(
        ui,
        &mut form.url,
        "https://example.com/file.zip",
        Some(Icon::Download),
        ui.available_width(),
    );
    if ui.memory(|memory| memory.focused().is_none()) {
        url_field.request_focus();
    }
    ui.add_space(8.0);
    field_label(ui, "File name (optional)");
    text_field(
        ui,
        &mut form.file_name,
        "Detected from the server",
        None,
        ui.available_width(),
    );
    ui.add_space(8.0);
    folder_picker(ui, "Save to", &mut form.folder, default_folder);
    ui.add_space(8.0);
    queue_picker(ui, "add-url-queue", &mut form.queue, queues);
    error_text(ui, &form.error);

    let submit_with_enter = ui.input(|input| input.key_pressed(Key::Enter));
    let mut command = None;
    let submit = |form: &mut AddUrlForm, start: StartMode| match parse_user_url(&form.url) {
        Ok(url) => {
            let request = NewDownload {
                queue: Some(form.queue),
                ..NewDownload::from_user(
                    url,
                    Some(&form.file_name),
                    chosen_folder(&form.folder),
                    start,
                )
            };
            Some(DialogCommand::Add(request))
        }
        Err(error) => {
            form.error = Some(error);
            None
        }
    };
    footer(ui, |ui| {
        if button(
            ui,
            ButtonKind::Primary,
            Some(Icon::Download),
            "Start download",
        )
        .clicked()
            || submit_with_enter
        {
            command = submit(form, StartMode::Queue);
        }
        if button(ui, ButtonKind::Secondary, Some(Icon::Pause), "Add paused").clicked() {
            command = submit(form, StartMode::Hold);
        }
        if button(
            ui,
            ButtonKind::Secondary,
            None,
            &hotkeys::CANCEL.label("Cancel"),
        )
        .clicked()
        {
            command = Some(DialogCommand::Close);
        }
    });
    command
}

/// The download file info: what is downloaded, from where, to where.
/// S starts, L downloads later, C cancels (while no text field is focused).
fn browser(ui: &mut Ui, form: &mut BrowserForm, queues: &[Queue]) -> Option<DialogCommand> {
    let host = form.link.url().host_str().unwrap_or("unknown host");
    let (badge, look, size) = match &form.stream {
        Some(stream) => (
            stream.format.label().to_owned(),
            CategoryLook::Video,
            "Video stream, joined into one file".to_owned(),
        ),
        None => (
            format::extension_badge(&form.file_name),
            CategoryLook::Generic,
            form.link
                .size()
                .map_or_else(|| "Size unknown".to_owned(), format::bytes),
        ),
    };
    let name = match form.file_name.trim() {
        "" => host,
        typed => typed,
    };
    let summary = FileSummary {
        badge,
        look,
        name,
        detail: format!("{size} · {host}"),
    };
    let page = form.link.page().cloned();
    let address = form.link.url().clone();
    file_card(ui, summary, |ui| {
        ui.add_space(10.0);
        address_row(ui, "Address", Some(address.as_str()));
        address_row(ui, "From page", page.as_ref().map(Url::as_str));
    });
    ui.add_space(12.0);
    field_label(
        ui,
        if form.stream.is_some() {
            "Video name"
        } else {
            "File name"
        },
    );
    text_field(
        ui,
        &mut form.file_name,
        "download",
        None,
        ui.available_width(),
    );
    if let Some(stream) = &mut form.stream {
        ui.add_space(8.0);
        quality_picker(ui, stream);
    }
    ui.add_space(8.0);
    ui.columns(2, |columns| {
        category_picker(&mut columns[0], form);
        queue_picker(&mut columns[1], "browser-queue", &mut form.queue, queues);
    });
    ui.add_space(8.0);
    let before = form.folder.clone();
    folder_picker(ui, "Save to", &mut form.folder, "");
    if form.folder != before {
        form.folder_source = FolderSource::Custom;
    }

    let mut command = None;
    let shortcut = shortcut_pressed(ui);
    footer(ui, |ui| {
        let start = button(
            ui,
            ButtonKind::Primary,
            Some(Icon::Download),
            &hotkeys::DIALOG_START.label("Start download"),
        );
        if start.clicked() || shortcut == Some(Shortcut::Start) {
            command = Some(DialogCommand::Add(browser_request(form, StartMode::Queue)));
        }
        let later = button(
            ui,
            ButtonKind::Secondary,
            Some(Icon::Clock),
            &hotkeys::DIALOG_LATER.label("Download later"),
        )
        .on_hover_text("Adds it to the queue without starting it. Start the queue to run it.");
        if later.clicked() || shortcut == Some(Shortcut::Later) {
            command = Some(DialogCommand::Add(browser_request(form, StartMode::Hold)));
        }
        let cancel = button(
            ui,
            ButtonKind::Secondary,
            None,
            &hotkeys::DIALOG_CANCEL.label("Cancel"),
        );
        if cancel.clicked() || shortcut == Some(Shortcut::Cancel) {
            command = Some(DialogCommand::Close);
        }
    });
    command
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Shortcut {
    Start,
    Later,
    Cancel,
}

/// A dialog hotkey, ignored while a text field has focus so typing an "s"
/// into the file name does not start the download.
pub(super) fn shortcut_pressed(ui: &Ui) -> Option<Shortcut> {
    if ui.ctx().wants_keyboard_input() {
        return None;
    }
    let ctx = ui.ctx();
    [
        (hotkeys::DIALOG_START, Shortcut::Start),
        (hotkeys::DIALOG_LATER, Shortcut::Later),
        (hotkeys::DIALOG_CANCEL, Shortcut::Cancel),
    ]
    .into_iter()
    .find(|(hotkey, _)| hotkey.pressed(ctx))
    .map(|(_, shortcut)| shortcut)
}

/// A labeled, selectable address that wraps instead of widening the dialog.
fn address_row(ui: &mut Ui, label: &str, address: Option<&str>) {
    ui.horizontal_top(|ui| {
        ui.add_sized(
            vec2(78.0, 18.0),
            egui::Label::new(
                egui::RichText::new(label)
                    .font(theme::semibold(12.5))
                    .color(theme::ink_soft()),
            ),
        );
        let text = address.unwrap_or("Not known");
        let color = if address.is_some() {
            theme::ink()
        } else {
            theme::ink_faint()
        };
        ui.add(
            egui::Label::new(
                egui::RichText::new(text)
                    .font(theme::regular(12.5))
                    .color(color),
            )
            .wrap()
            .selectable(true),
        );
    });
}

/// Choosing a category moves the save folder to that category's folder,
/// unless the user picked a folder of their own.
fn category_picker(ui: &mut Ui, form: &mut BrowserForm) {
    field_label(ui, "Category");
    let before = form.category;
    let current = form.settings.categories.get(form.category).name.clone();
    let width = ui.available_width();
    dropdown(ui, "browser-category", width, &current, |ui| {
        for category in form.settings.categories.all() {
            ui.selectable_value(&mut form.category, category.id, &category.name);
        }
    });
    if form.category != before && form.folder_source == FolderSource::Category {
        form.folder = form.category_folder().display().to_string();
    }
}

/// Quality choices read from the manifest, best first.
fn quality_picker(ui: &mut Ui, stream: &mut StreamChoice) {
    field_label(ui, "Quality");
    let state = stream.offer.lock().unwrap_or_else(|p| p.into_inner());
    match &*state {
        OfferState::Loading => {
            ui.horizontal(|ui| {
                ui.spinner();
                hint(ui, "Reading the stream's qualities…");
            });
        }
        OfferState::Failed(error) => {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("Could not read the stream: {error}"))
                        .font(theme::regular(12.5))
                        .color(theme::danger()),
                )
                .wrap(),
            );
        }
        OfferState::Ready(offer) => {
            let mut variants = offer.variants.clone();
            variants.sort_by_key(|variant| std::cmp::Reverse((variant.height, variant.bandwidth)));
            variants.dedup_by_key(|variant| variant.height);
            let label = |quality: VideoQuality| match quality {
                VideoQuality::Best => match variants.first() {
                    Some(best) => format!("Best available ({})", best.label()),
                    None => "Best available".to_owned(),
                },
                VideoQuality::MaxHeight { pixels } => variants
                    .iter()
                    .find(|variant| variant.height == Some(pixels))
                    .map_or_else(|| quality.label(), |variant| variant.label()),
            };
            let selected = label(stream.quality);
            let width = ui.available_width();
            let before = stream.quality;
            dropdown(ui, "stream-quality", width, &selected, |ui| {
                ui.selectable_value(
                    &mut stream.quality,
                    VideoQuality::Best,
                    label(VideoQuality::Best),
                );
                for variant in &variants {
                    if let Some(pixels) = variant.height {
                        let quality = VideoQuality::MaxHeight { pixels };
                        ui.selectable_value(&mut stream.quality, quality, variant.label());
                    }
                }
            });
            if stream.quality != before {
                stream.quality_source = QualitySource::Chosen;
            }
            if offer.separate_audio {
                let note = if stream.ffmpeg_found {
                    "Video and audio come as separate tracks; ffmpeg will merge them."
                } else {
                    "Video and audio come as separate tracks. Install ffmpeg (or set it in Settings) to merge them."
                };
                hint(ui, note);
            }
        }
    }
}

fn browser_request(form: &BrowserForm, start: StartMode) -> NewDownload {
    let typed = form.file_name.trim();
    let file_name = match (&form.stream, form.link.browser_file_name()) {
        (Some(_), _) if !typed.is_empty() => Some(FileName::new(typed, FileNameSource::Browser)),
        (Some(_), _) => None,
        // The user edited the browser's suggestion.
        (None, Some(suggested)) if suggested != typed && !typed.is_empty() => {
            Some(FileName::new(typed, FileNameSource::User))
        }
        (None, Some(suggested)) => Some(FileName::new(suggested, FileNameSource::Browser)),
        (None, None)
            if !typed.is_empty() && typed != FileName::guess_from_url(form.link.url()).value =>
        {
            Some(FileName::new(typed, FileNameSource::User))
        }
        (None, None) => None,
    };
    let kind = match &form.stream {
        Some(stream) => DownloadKind::Stream {
            format: stream.format,
            // Left to Settings, the engine applies them when it starts.
            quality: (stream.quality_source == QualitySource::Chosen).then_some(stream.quality),
        },
        None => DownloadKind::Detect,
    };
    // An untouched folder for a file the server will name is decided by the
    // engine once the name is known; see `request_without_asking`.
    let folder = match (form.folder_source, &file_name) {
        (FolderSource::Category, None) => None,
        _ => chosen_folder(&form.folder),
    };
    NewDownload {
        url: form.link.url().clone(),
        file_name,
        folder,
        context: form.link.context(),
        start,
        kind,
        queue: Some(form.queue),
        page_url: form.link.page().cloned(),
    }
}

fn refresh(ui: &mut Ui, form: &mut RefreshForm) -> Option<DialogCommand> {
    title(
        ui,
        "Use this new address?",
        "You asked Downpour to wait for a fresh link. Downloaded parts are kept and the download continues.",
    );
    ui.add(
        egui::Label::new(
            egui::RichText::new(&form.name)
                .font(theme::semibold(14.0))
                .color(theme::ink()),
        )
        .wrap(),
    );
    ui.add_space(4.0);
    hint(
        ui,
        &format!(
            "New address from {}",
            form.link.url().host_str().unwrap_or("unknown host")
        ),
    );
    let mut command = None;
    footer(ui, |ui| {
        if button(
            ui,
            ButtonKind::Primary,
            Some(Icon::Refresh),
            "Update and resume",
        )
        .clicked()
        {
            command = Some(DialogCommand::Refresh {
                id: form.id,
                url: form.link.url().clone(),
                context: form.link.context(),
            });
        }
        if button(ui, ButtonKind::Secondary, Some(Icon::Plus), "Add as new").clicked() {
            command = Some(DialogCommand::AddInstead(form.link.clone()));
        }
        if button(
            ui,
            ButtonKind::Secondary,
            None,
            &hotkeys::CANCEL.label("Cancel"),
        )
        .clicked()
        {
            command = Some(DialogCommand::Close);
        }
    });
    command
}

fn duplicate(ui: &mut Ui, form: &mut DuplicateForm) -> Option<DialogCommand> {
    let (heading, detail) = match &form.duplicate.standing {
        Standing::Downloaded {
            finished_at_unix, ..
        } => (
            "You already downloaded this",
            match finished_at_unix {
                Some(at) => format!("Finished {}.", format::date(*at)),
                None => "The file is still on your disk.".to_owned(),
            },
        ),
        Standing::Unfinished(unfinished) => (
            "This is already in your list",
            match unfinished {
                Unfinished::Waiting => "It is waiting in its queue.",
                Unfinished::Running => "It is downloading right now.",
                Unfinished::Paused => "It is paused.",
                Unfinished::Failed => "It failed; you can retry it from the list.",
                Unfinished::WaitingForSpace => "It is waiting for disk space.",
            }
            .to_owned(),
        ),
    };
    title(ui, heading, &detail);
    let summary = FileSummary {
        badge: format::extension_badge(&form.duplicate.file_name),
        look: form.look,
        name: &form.duplicate.file_name,
        detail: form
            .link
            .url()
            .host_str()
            .unwrap_or("unknown host")
            .to_owned(),
    };
    file_card(ui, summary, |_| {});
    let mut command = None;
    footer(ui, |ui| {
        if button(
            ui,
            ButtonKind::Primary,
            Some(Icon::Download),
            "Download again",
        )
        .clicked()
        {
            command = Some(DialogCommand::DownloadAgain(form.link.clone()));
        }
        let existing = match &form.duplicate.standing {
            Standing::Downloaded { file, .. } => {
                button(ui, ButtonKind::Secondary, Some(Icon::File), "Open file")
                    .clicked()
                    .then(|| DialogCommand::OpenFile(file.clone()))
            }
            Standing::Unfinished(_) => {
                button(ui, ButtonKind::Secondary, Some(Icon::Grid), "Show in list")
                    .clicked()
                    .then_some(DialogCommand::ShowExisting(form.duplicate.id))
            }
        };
        if existing.is_some() {
            command = existing;
        }
        if button(
            ui,
            ButtonKind::Secondary,
            None,
            &hotkeys::CANCEL.label("Cancel"),
        )
        .clicked()
        {
            command = Some(DialogCommand::Close);
        }
    });
    command
}

fn remove(ui: &mut Ui, form: &mut RemoveForm) -> Option<DialogCommand> {
    title(
        ui,
        "Remove download?",
        "The entry will be removed from the list.",
    );
    ui.add(
        egui::Label::new(
            egui::RichText::new(&form.name)
                .font(theme::semibold(14.0))
                .color(theme::ink()),
        )
        .wrap(),
    );
    ui.add_space(8.0);
    if form.completed {
        ui.checkbox(
            &mut form.delete_file,
            "Also delete the downloaded file from disk",
        );
    } else {
        hint(ui, "Partially downloaded data will be deleted.");
    }
    let mut command = None;
    footer(ui, |ui| {
        if button(ui, ButtonKind::Danger, Some(Icon::Trash), "Remove").clicked() {
            let files = if form.delete_file {
                RemoveFiles::Delete
            } else {
                RemoveFiles::Keep
            };
            command = Some(DialogCommand::Remove { id: form.id, files });
        }
        if button(
            ui,
            ButtonKind::Secondary,
            None,
            &hotkeys::CANCEL.label("Cancel"),
        )
        .clicked()
        {
            command = Some(DialogCommand::Close);
        }
    });
    command
}

fn queue(ui: &mut Ui, form: &mut QueueForm) -> Option<DialogCommand> {
    let (heading, action) = match form.editing {
        Some(_) => ("Edit queue", "Save"),
        None => ("New queue", "Create queue"),
    };
    title(
        ui,
        heading,
        "Each queue runs its own downloads, a set number at a time, and can be started or stopped.",
    );
    field_label(ui, "Name");
    let name_field = text_field(
        ui,
        &mut form.name,
        "For example: Night downloads",
        None,
        ui.available_width(),
    );
    if form.editing.is_none() && ui.memory(|memory| memory.focused().is_none()) {
        name_field.request_focus();
    }
    ui.add_space(8.0);
    field_label(ui, "Downloads at the same time");
    ui.add(egui::Slider::new(&mut form.max_active, QUEUE_LIMIT_RANGE));
    error_text(ui, &form.error);
    let mut command = None;
    footer(ui, |ui| {
        if button(ui, ButtonKind::Primary, Some(Icon::Check), action).clicked() {
            let name = form.name.trim().to_owned();
            command = Some(match form.editing {
                Some(id) => DialogCommand::UpdateQueue {
                    id,
                    name,
                    max_active: form.max_active,
                },
                None => DialogCommand::CreateQueue {
                    name,
                    max_active: form.max_active,
                },
            });
        }
        if button(
            ui,
            ButtonKind::Secondary,
            None,
            &hotkeys::CANCEL.label("Cancel"),
        )
        .clicked()
        {
            command = Some(DialogCommand::Close);
        }
    });
    command
}

fn categories(ui: &mut Ui, form: &mut CategoriesForm) -> Option<DialogCommand> {
    title(
        ui,
        "Categories",
        "Choose which file extensions belong to each category and where its files are saved.",
    );
    let mut pick = None;
    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(CATEGORY_LIST_WIDTH);
            egui::ScrollArea::vertical()
                .max_height(300.0)
                .id_salt("category-list")
                .show(ui, |ui| {
                    for category in form.categories.all() {
                        let (color, _) = theme::look_colors(category.look);
                        let selected = category.id == form.selected;
                        let count =
                            (category.id != CategoryId::OTHER).then_some(category.extensions.len());
                        let entry = NavEntry {
                            icon: Icon::for_look(category.look),
                            icon_color: color,
                            label: &category.name,
                            count,
                            selected,
                        };
                        let row = nav_item(ui, &theme::plain_nav_ink(), entry)
                            .on_hover_text("Number of file extensions in this category");
                        if row.clicked() && !selected {
                            pick = Some(CategoryPick::Existing(category.id));
                        }
                    }
                });
            ui.add_space(8.0);
            if button(ui, ButtonKind::Secondary, Some(Icon::Plus), "Add category").clicked() {
                pick = Some(CategoryPick::New);
            }
        });
        ui.add_space(12.0);
        ui.vertical(|ui| category_editor(ui, form));
    });
    match pick {
        Some(CategoryPick::New) => add_category(form),
        Some(CategoryPick::Existing(id)) => form.select(id),
        None => {}
    }
    error_text(ui, &form.error);

    let mut command = None;
    footer(ui, |ui| {
        if button(ui, ButtonKind::Primary, Some(Icon::Check), "Save").clicked() {
            match form.commit() {
                Ok(()) => command = Some(DialogCommand::SaveCategories(form.categories.clone())),
                Err(error) => form.error = Some(error),
            }
        }
        if button(
            ui,
            ButtonKind::Secondary,
            None,
            &hotkeys::CANCEL.label("Cancel"),
        )
        .clicked()
        {
            command = Some(DialogCommand::Close);
        }
        let removable = form.selected != CategoryId::OTHER;
        if ui
            .add_enabled_ui(removable, |ui| {
                button(ui, ButtonKind::Danger, Some(Icon::Trash), "Remove category")
            })
            .inner
            .clicked()
        {
            if let Err(error) = form.categories.remove(form.selected) {
                form.error = Some(error.to_string());
            } else {
                let first = form.categories.all()[0].clone();
                form.selected = first.id;
                form.editor = CategoryEditor::of(&first);
                form.error = None;
            }
        }
    });
    command
}

/// What the user clicked in the category list.
enum CategoryPick {
    Existing(CategoryId),
    New,
}

fn add_category(form: &mut CategoriesForm) {
    if let Err(error) = form.commit() {
        form.error = Some(error);
        return;
    }
    let name = (1..)
        .map(|n| {
            if n == 1 {
                "New category".to_owned()
            } else {
                format!("New category {n}")
            }
        })
        .find(|name| {
            !form
                .categories
                .all()
                .iter()
                .any(|c| c.name.eq_ignore_ascii_case(name))
        })
        .expect("an unbounded counter finds a free name");
    match form.categories.add(&name, CategoryLook::Generic) {
        Ok(id) => {
            form.selected = id;
            form.editor = CategoryEditor::of(form.categories.get(id));
            form.error = None;
        }
        Err(error) => form.error = Some(error.to_string()),
    }
}

fn category_editor(ui: &mut Ui, form: &mut CategoriesForm) {
    let is_other = form.selected == CategoryId::OTHER;
    field_label(ui, "Name");
    text_field(
        ui,
        &mut form.editor.name,
        "Category name",
        None,
        ui.available_width(),
    );
    ui.add_space(8.0);
    field_label(ui, "File extensions");
    if is_other {
        hint(
            ui,
            "Files whose extension no other category lists end up here.",
        );
    } else {
        field_frame()
            .inner_margin(egui::Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut form.editor.extensions)
                        .hint_text("zip rar 7z")
                        .frame(false)
                        .desired_rows(3)
                        .desired_width(ui.available_width())
                        .font(theme::regular(13.5)),
                );
            });
        hint(
            ui,
            "Separate with spaces or commas. An extension moves here from any other category.",
        );
    }
    ui.add_space(8.0);
    let default_folder = form
        .download_dir
        .join(downpour_core::naming::sanitize_file_name(&form.editor.name))
        .display()
        .to_string();
    folder_picker(
        ui,
        "Save files to",
        &mut form.editor.folder,
        &default_folder,
    );
    ui.add_space(8.0);
    field_label(ui, "Icon");
    let width = ui.available_width();
    dropdown(ui, "category-look", width, form.editor.look.label(), |ui| {
        for look in CategoryLook::ALL {
            ui.selectable_value(&mut form.editor.look, look, look.label());
        }
    });
}

fn settings(ui: &mut Ui, form: &mut SettingsForm) -> Option<DialogCommand> {
    title(
        ui,
        "Settings",
        "Changes apply to new connections right away.",
    );
    let mut command = None;
    dialog_scroll_area(ui)
        .max_height(ui.ctx().screen_rect().height() * 0.62)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
        .show(ui, |ui| {
            // Keeps the cards clear of the scroll bar.
            ui.set_width((ui.available_width() - 14.0).max(0.0));
            // Raised cards are lit past their left and top edges. The list
            // never scrolls sideways, so the clip can reach into the
            // dialog's margin on the left; at the top, space makes room.
            // Where a raised card's lit edge reaches past the scroll area.
            let reach = theme::control_shadow_reach();
            if reach > 0.0 {
                let mut clip = ui.clip_rect();
                clip.min.x -= reach;
                ui.set_clip_rect(clip);
                ui.add_space(reach);
            }

            settings_card(ui, "Saving", |ui| {
                folder_picker(ui, "Download folder", &mut form.download_dir, "");
                ui.add_space(10.0);
                toggle_row(
                    ui,
                    &mut form.organize_by_category,
                    "Save into each category's folder",
                    "Edit categories, their extensions, and their folders from the sidebar.",
                );
            });

            settings_card(ui, "New downloads from the browser", |ui| {
                choice_list(
                    ui,
                    &mut form.new_downloads,
                    &[
                        Choice {
                            value: NewDownloadMode::AskFirst,
                            title: "Ask first",
                            detail: "Show the file name, folder, and queue before it starts.",
                        },
                        Choice {
                            value: NewDownloadMode::StartRightAway,
                            title: "Start right away (auto-start)",
                            detail: "Each queue still runs only as many at once as its limit allows.",
                        },
                    ],
                );
            });

            settings_card(ui, "Appearance", |ui| {
                segmented(
                    ui,
                    &mut form.appearance,
                    &[
                        (Appearance::MatchSystem, "Match Windows"),
                        (Appearance::Light, "Light"),
                        (Appearance::Dark, "Dark"),
                    ],
                );
                ui.add_space(10.0);
                field_label(ui, "Colors");
                theme_picker(ui, &mut form.color_theme);
            });

            settings_card(ui, "Window", |ui| {
                field_label(ui, "Closing the window");
                segmented(
                    ui,
                    &mut form.on_close,
                    &[
                        (CloseAction::HideToTray, "Keep running in the tray"),
                        (CloseAction::Quit, "Quit Downpour"),
                    ],
                );
                ui.add_space(8.0);
                field_label(ui, "Minimizing the window");
                segmented(
                    ui,
                    &mut form.on_minimize,
                    &[
                        (MinimizeAction::HideToTray, "Hide to the tray"),
                        (MinimizeAction::Taskbar, "Keep on the taskbar"),
                    ],
                );
                ui.add_space(4.0);
                hint(ui, "Click the tray icon to open Downpour; right-click it to pause, resume, or quit.");
            });

            settings_card(ui, "System", |ui| {
                toggle_row(
                    ui,
                    &mut form.start_with_windows,
                    "Start with Windows",
                    "Starts quietly in the tray when you sign in.",
                );
                ui.add_space(10.0);
                toggle_row(
                    ui,
                    &mut form.notify,
                    "Notify when downloads finish or fail",
                    "Shows a Windows notification while Downpour is in the tray or in the background.",
                );
                ui.add_space(10.0);
                toggle_row(
                    ui,
                    &mut form.keep_awake,
                    "Keep the PC awake while downloading",
                    "The PC can sleep again once no download is running. The screen may still turn off.",
                );
            });

            settings_card(ui, "Speed", |ui| {
                field_label(ui, "Connections per download");
                let mut connections = u32::from(form.connections);
                stepper(
                    ui,
                    &mut connections,
                    u32::from(*CONNECTION_RANGE.start())..=u32::from(*CONNECTION_RANGE.end()),
                    1,
                    "",
                );
                form.connections = u8::try_from(connections).unwrap_or(*CONNECTION_RANGE.end());
                ui.add_space(10.0);
                toggle_row(
                    ui,
                    &mut form.limit_speed,
                    "Limit total download speed",
                    "Shared by all running downloads.",
                );
                if form.limit_speed {
                    ui.add_space(6.0);
                    stepper(ui, &mut form.limit_kib, 1..=1_000_000, 128, "KB/s");
                }
            });

            settings_card(ui, "Video streams", |ui| {
                field_label(ui, "Preferred quality");
                let width = ui.available_width();
                dropdown(ui, "settings-quality", width, &form.video_quality.label(), |ui| {
                    for quality in VideoQuality::CHOICES {
                        ui.selectable_value(&mut form.video_quality, quality, quality.label());
                    }
                });
                ui.add_space(10.0);
                field_label(ui, "ffmpeg");
                ui.horizontal(|ui| {
                    let browse_width = button_width(ui, Some(Icon::Folder), "Browse");
                    let field_width = ui.available_width() - browse_width - ui.spacing().item_spacing.x;
                    text_field(ui, &mut form.ffmpeg_path, "Found automatically", None, field_width);
                    if button(ui, ButtonKind::Secondary, Some(Icon::Folder), "Browse").clicked() {
                        if let Some(picked) = rfd::FileDialog::new().add_filter("ffmpeg", &["exe"]).pick_file() {
                            form.ffmpeg_path = picked.display().to_string();
                        }
                    }
                });
                let configured = chosen_folder(&form.ffmpeg_path);
                match locate_ffmpeg(configured.as_deref()) {
                    Some(found) => hint(ui, &format!("Using {}", found.display())),
                    None => {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(
                                    "ffmpeg not found. Streams with separate video and audio need it to be merged.",
                                )
                                .font(theme::regular(12.5))
                                .color(theme::warning()),
                            )
                            .wrap(),
                        );
                    }
                }
            });

            settings_card(ui, "Browser", |ui| {
                hint(
                    ui,
                    "Registering adds a per-user registry key for Firefox and a manifest file next to downpour-host.",
                );
                ui.add_space(4.0);
                if button(ui, ButtonKind::Secondary, Some(Icon::Program), "Register browser integration").clicked() {
                    command = Some(DialogCommand::RegisterBrowserIntegration);
                }
                ui.add_space(6.0);
                if button(ui, ButtonKind::Secondary, Some(Icon::Plus), "Add to Firefox").clicked() {
                    command = Some(DialogCommand::AddToFirefox);
                }
                match &form.integration_result {
                    Some(Ok(output)) => {
                        ui.add(egui::Label::new(egui::RichText::new(output).color(theme::success())).wrap());
                    }
                    Some(Err(error)) => {
                        ui.add(egui::Label::new(egui::RichText::new(error).color(theme::danger())).wrap());
                    }
                    None => {}
                }
            });
        });

    footer(ui, |ui| {
        if button(ui, ButtonKind::Primary, Some(Icon::Check), "Save").clicked() {
            command = Some(DialogCommand::SaveSettings(form.to_settings()));
        }
        if button(
            ui,
            ButtonKind::Secondary,
            None,
            &hotkeys::CANCEL.label("Cancel"),
        )
        .clicked()
        {
            command = Some(DialogCommand::Close);
        }
    });
    command
}

#[cfg(debug_assertions)]
const DEBUG_SCROLL: &str = "downpour-debug-dialog-scroll";

/// Lets the screenshot hook scroll the next dialog frame (debug builds).
#[cfg(debug_assertions)]
pub(super) fn set_debug_scroll(ctx: &egui::Context, offset: f32) {
    ctx.data_mut(|data| data.insert_temp(egui::Id::new(DEBUG_SCROLL), offset));
}

/// A vertical scroll area for a dialog body.
fn dialog_scroll_area(ui: &Ui) -> egui::ScrollArea {
    let area = egui::ScrollArea::vertical();
    #[cfg(debug_assertions)]
    if let Some(offset) = ui
        .ctx()
        .data_mut(|data| data.remove_temp::<f32>(egui::Id::new(DEBUG_SCROLL)))
    {
        return area.vertical_scroll_offset(offset);
    }
    #[cfg(not(debug_assertions))]
    let _ = ui;
    area
}

/// A titled group of related settings on a softly tinted card.
fn settings_card(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui)) {
    SETTINGS_CARD.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(
            egui::RichText::new(title)
                .font(theme::semibold(14.5))
                .color(theme::ink()),
        );
        ui.add_space(6.0);
        add_contents(ui);
    });
    ui.add_space(10.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use downpour_protocol::messages::{BrowserDownload, QualityPick};

    #[test]
    fn accepts_links_without_scheme() {
        assert_eq!(
            parse_user_url(" example.com/a.zip ").unwrap().as_str(),
            "https://example.com/a.zip"
        );
        assert_eq!(
            parse_user_url("http://a.example/f").unwrap().scheme(),
            "http"
        );
    }

    #[test]
    fn rejects_unsupported_links() {
        assert!(parse_user_url("").is_err());
        assert!(parse_user_url("ftp://a.example/f")
            .unwrap_err()
            .contains("ftp"));
        assert!(parse_user_url("javascript:alert(1)").is_err());
    }

    fn link_found_on_a_page() -> BrowserLink {
        BrowserLink::parse(BrowserDownload {
            file_name: Some("sample-video.mp4".into()),
            referrer: Some("https://example.org/abc123def456".into()),
            mime: Some("video/mp4".into()),
            size: Some(1_000),
            ..crate::browser_link::tests::item("https://files1.example.org/abc123/sample-video.mp4")
        })
        .unwrap()
    }

    #[test]
    fn file_info_keeps_both_the_link_and_the_page() {
        let settings = EngineSettings {
            download_dir: PathBuf::from("D:/dl"),
            ..EngineSettings::default()
        };
        let form = BrowserForm::new(link_found_on_a_page(), &settings);
        assert_eq!(
            form.link.page().unwrap().as_str(),
            "https://example.org/abc123def456"
        );
        assert_eq!(
            form.folder,
            PathBuf::from("D:/dl").join("Video").display().to_string()
        );
        assert_eq!(form.file_name, "sample-video.mp4");
        let request = browser_request(&form, StartMode::Hold);
        assert_eq!(request.url.host_str(), Some("files1.example.org"));
        assert_eq!(
            request.page_url.unwrap().as_str(),
            "https://example.org/abc123def456"
        );
        assert_eq!(request.start, StartMode::Hold);
    }

    #[test]
    fn choosing_a_category_moves_the_folder_unless_the_user_picked_one() {
        let settings = EngineSettings {
            download_dir: PathBuf::from("D:/dl"),
            ..EngineSettings::default()
        };
        let mut form = BrowserForm::new(link_found_on_a_page(), &settings);
        let documents = settings.categories.classify("a.pdf");
        form.category = documents;
        form.folder = form.category_folder().display().to_string();
        assert!(form.folder.ends_with("Documents"));
        form.folder_source = FolderSource::Custom;
        form.folder = "E:/mine".into();
        let request = browser_request(&form, StartMode::Queue);
        assert_eq!(
            request.folder,
            Some(downpour_core::paths::native(Path::new("E:/mine")))
        );
    }

    #[test]
    fn the_dialog_untouched_adds_what_auto_start_adds() {
        use crate::browser_link::{request_without_asking, tests::item};
        let settings = EngineSettings {
            download_dir: PathBuf::from("D:/dl"),
            ..EngineSettings::default()
        };
        let links = [
            link_found_on_a_page(),
            BrowserLink::parse(item("https://cdn.example/get?id=7")).unwrap(),
            BrowserLink::parse(BrowserDownload {
                title: Some("Scene 3".into()),
                ..item("https://cdn.example/v/master.m3u8")
            })
            .unwrap(),
        ];
        for link in links {
            let form = BrowserForm::new(link.clone(), &settings);
            assert_eq!(
                browser_request(&form, StartMode::Queue),
                request_without_asking(link, &settings)
            );
        }
    }

    #[test]
    fn a_quality_picked_on_the_page_is_preselected_in_the_dialog() {
        let link = BrowserLink::parse(BrowserDownload {
            quality: Some(QualityPick::MaxHeight { pixels: 720 }),
            ..crate::browser_link::tests::item("https://cdn.example/v/master.m3u8")
        })
        .unwrap();
        let settings = EngineSettings {
            video_quality: VideoQuality::Best,
            ..EngineSettings::default()
        };
        let form = BrowserForm::new(link, &settings);
        assert_eq!(
            form.stream.map(|stream| stream.quality),
            Some(VideoQuality::MaxHeight { pixels: 720 })
        );
    }

    #[test]
    fn nine_swatches_at_the_settings_width_wrap_five_then_four() {
        let grid = swatch_grid(614.0, 94.0, 9);
        assert_eq!(grid.per_row, 5);
        assert!(grid.width >= 94.0, "{grid:?}");
    }

    #[test]
    fn swatches_fill_one_row_when_wide_and_stack_when_narrow() {
        assert_eq!(swatch_grid(2000.0, 94.0, 9).per_row, 9);
        let narrow = swatch_grid(60.0, 94.0, 9);
        assert_eq!(narrow.per_row, 1);
        assert_eq!(narrow.width, 60.0);
    }

    #[test]
    fn swatch_rows_are_balanced_rather_than_leaving_one_alone() {
        // Room for eight, but eight and one would strand the last swatch.
        let grid = swatch_grid(8.0 * 94.0 + 7.0 * SWATCH_GAP, 94.0, 9);
        assert_eq!(grid.per_row, 5);
    }
}
