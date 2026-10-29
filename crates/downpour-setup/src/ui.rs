//! The setup window: options, progress, and a finish page with an "Add to
//! Firefox" button. Work runs on a background thread so the window stays
//! responsive.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, CornerRadius, RichText, Stroke, Ui};

use crate::install::{
    self, BrowserIntegration, InstallFolder, InstallOptions, SetupError, Shortcut, UserData,
    APP_NAME,
};
use crate::report::{LogLine, SetupReport};
use crate::running::{self, RunningProgram, StillRunning};
use crate::Mode;

const INK: Color32 = Color32::from_rgb(0x1E, 0x21, 0x42);
const INK_SOFT: Color32 = Color32::from_rgb(0x6B, 0x70, 0x90);
const PRIMARY: Color32 = Color32::from_rgb(0x5B, 0x4F, 0xE0);
const PRIMARY_SOFT: Color32 = Color32::from_rgb(0xEE, 0xEC, 0xFD);
const DANGER: Color32 = Color32::from_rgb(0xE5, 0x48, 0x4D);
const SUCCESS: Color32 = Color32::from_rgb(0x1F, 0xA9, 0x7A);
const WARNING: Color32 = Color32::from_rgb(0xB4, 0x5F, 0x06);
const BACKDROP: Color32 = Color32::from_rgb(0xEE, 0xF0, 0xF7);

enum Page {
    InstallOptions(OptionsForm),
    UninstallConfirm {
        remove_user_data: bool,
    },
    /// Programs from the install folder did not close when asked; the user
    /// decides whether to stop them.
    StillRunning {
        programs: Vec<RunningProgram>,
        work: Work,
    },
    Working(Job),
    Finished(Finished),
    Failed {
        message: String,
    },
}

struct OptionsForm {
    folder: String,
    /// Why the folder typed cannot be used, after the last Install click.
    folder_error: Option<String>,
    start_menu: bool,
    desktop: bool,
    browsers: bool,
}

/// An install or uninstall, as chosen on the first page.
#[derive(Clone, Debug)]
enum Work {
    Install(InstallOptions),
    Uninstall {
        folder: PathBuf,
        user_data: UserData,
    },
}

impl Work {
    fn folder(&self) -> &Path {
        match self {
            Work::Install(options) => options.folder.path(),
            Work::Uninstall { folder, .. } => folder,
        }
    }

    fn run(&self, log: &mut dyn FnMut(LogLine)) -> Result<SetupReport, SetupError> {
        match self {
            Work::Install(options) => install::install(options, log),
            Work::Uninstall { folder, user_data } => install::uninstall(folder, *user_data, log),
        }
    }
}

/// A background install or uninstall and what it has reported so far.
struct Job {
    log: Arc<Mutex<Vec<LogLine>>>,
    outcome: Arc<Mutex<Option<Result<SetupReport, SetupError>>>>,
    work: Work,
}

struct Finished {
    log: Vec<LogLine>,
    report: SetupReport,
    folder: PathBuf,
    /// What the last "Add to Firefox" click said.
    browser_message: Option<Result<String, String>>,
}

pub struct Setup {
    mode: Mode,
    page: Page,
}

impl Setup {
    pub fn new(cc: &eframe::CreationContext<'_>, mode: Mode) -> Self {
        style(&cc.egui_ctx);
        Self {
            mode,
            page: Setup::first_page(mode),
        }
    }

    /// Runs `work` on a background thread, first stopping `to_stop` (the
    /// programs the user agreed to stop).
    fn start(&mut self, work: Work, to_stop: Vec<RunningProgram>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let outcome = Arc::new(Mutex::new(None));
        let (thread_log, thread_outcome) = (Arc::clone(&log), Arc::clone(&outcome));
        let thread_work = work.clone();
        std::thread::spawn(move || {
            let mut log = |line: LogLine| {
                thread_log
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(line)
            };
            let result = running::stop_programs(&to_stop, &mut log)
                .map_err(SetupError::from)
                .and_then(|()| thread_work.run(&mut log));
            *thread_outcome.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
        });
        self.page = Page::Working(Job { log, outcome, work });
    }
}

impl eframe::App for Setup {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Page::Working(job) = &self.page {
            let finished = job.outcome.lock().unwrap_or_else(|p| p.into_inner()).take();
            match finished {
                Some(Ok(report)) => {
                    let log = job.log.lock().unwrap_or_else(|p| p.into_inner()).clone();
                    let folder = job.work.folder().to_path_buf();
                    self.page = Page::Finished(Finished {
                        log,
                        report,
                        folder,
                        browser_message: None,
                    });
                }
                Some(Err(SetupError::StillRunning(StillRunning::Programs(programs)))) => {
                    self.page = Page::StillRunning {
                        programs,
                        work: job.work.clone(),
                    };
                }
                Some(Err(error)) => {
                    self.page = Page::Failed {
                        message: error.to_string(),
                    }
                }
                None => ctx.request_repaint_after(std::time::Duration::from_millis(100)),
            }
        }

        let mut next: Option<Action> = None;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(BACKDROP)
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(Color32::WHITE)
                    .corner_radius(CornerRadius::same(16))
                    .inner_margin(egui::Margin::same(24))
                    .show(ui, |ui| {
                        ui.set_min_size(ui.available_size());
                        header(ui, self.mode);
                        ui.add_space(16.0);
                        next = match &mut self.page {
                            Page::InstallOptions(form) => install_options(ui, form),
                            Page::UninstallConfirm { remove_user_data } => {
                                uninstall_confirm(ui, remove_user_data)
                            }
                            Page::StillRunning { programs, work } => {
                                still_running(ui, programs, work)
                            }
                            Page::Working(job) => {
                                working(ui, job);
                                None
                            }
                            Page::Finished(finished) => finished_page(ui, self.mode, finished),
                            Page::Failed { message } => failed(ui, message),
                        };
                    });
            });
        if let Some(action) = next {
            self.apply(ctx, action);
        }
    }
}

enum Action {
    Install(InstallOptions),
    Uninstall(UserData),
    /// Stop the listed programs, then do the work again.
    StopAndContinue,
    AddToFirefox,
    OpenApp,
    BackToStart,
    Close,
}

impl Setup {
    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::Install(options) => self.start(Work::Install(options), Vec::new()),
            Action::Uninstall(user_data) => {
                // Without an installation, uninstalling the default folder
                // fails and the failure page says why.
                let folder = install::folder_to_uninstall().unwrap_or_else(install::default_folder);
                self.start(Work::Uninstall { folder, user_data }, Vec::new());
            }
            Action::StopAndContinue => {
                let page = std::mem::replace(&mut self.page, Setup::first_page(self.mode));
                if let Page::StillRunning { programs, work } = page {
                    self.start(work, programs);
                }
            }
            Action::AddToFirefox => {
                if let Page::Finished(finished) = &mut self.page {
                    finished.browser_message = Some(install::add_to_firefox(&finished.folder));
                }
            }
            Action::OpenApp => {
                if let Page::Finished(finished) = &self.page {
                    if let Err(error) = install::launch_app(&finished.folder) {
                        self.page = Page::Failed {
                            message: format!("Could not start Downpour: {error}"),
                        };
                        return;
                    }
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Action::BackToStart => {
                *self = Setup {
                    mode: self.mode,
                    page: Setup::first_page(self.mode),
                };
            }
            Action::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    fn first_page(mode: Mode) -> Page {
        match mode {
            Mode::Install => Page::InstallOptions(OptionsForm {
                folder: install::suggested_folder().display().to_string(),
                folder_error: None,
                start_menu: true,
                desktop: true,
                browsers: true,
            }),
            Mode::Uninstall => Page::UninstallConfirm {
                remove_user_data: false,
            },
        }
    }
}

fn header(ui: &mut Ui, mode: Mode) {
    ui.horizontal(|ui| {
        let (mark, _) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::hover());
        paint_mark(ui.painter(), mark);
        ui.add_space(6.0);
        ui.vertical(|ui| {
            let title = match mode {
                Mode::Install => format!("Install {APP_NAME}"),
                Mode::Uninstall => format!("Uninstall {APP_NAME}"),
            };
            ui.label(RichText::new(title).size(22.0).strong().color(INK));
            ui.label(
                RichText::new(format!("Version {}", env!("CARGO_PKG_VERSION")))
                    .size(13.0)
                    .color(INK_SOFT),
            );
        });
    });
}

/// The download-arrow mark, drawn with shapes (fonts may lack an arrow glyph).
fn paint_mark(painter: &egui::Painter, rect: egui::Rect) {
    painter.rect_filled(rect, CornerRadius::same(12), PRIMARY);
    let unit = rect.width() / 64.0;
    let at = |x: f32, y: f32| rect.min + egui::vec2(x * unit, y * unit);
    let stroke = Stroke::new(6.0 * unit, Color32::WHITE);
    painter.line_segment([at(32.0, 14.0), at(32.0, 40.0)], stroke);
    painter.add(egui::Shape::line(
        vec![at(21.0, 30.0), at(32.0, 41.0), at(43.0, 30.0)],
        stroke,
    ));
    painter.rect_filled(
        egui::Rect::from_min_max(at(17.0, 47.0), at(47.0, 52.0)),
        CornerRadius::same(2),
        Color32::WHITE,
    );
}

fn primary(ui: &mut Ui, label: &str) -> bool {
    ui.add(
        egui::Button::new(
            RichText::new(label)
                .size(14.5)
                .strong()
                .color(Color32::WHITE),
        )
        .fill(PRIMARY)
        .corner_radius(CornerRadius::same(10))
        .min_size(egui::vec2(0.0, 36.0)),
    )
    .clicked()
}

fn secondary(ui: &mut Ui, label: &str) -> bool {
    ui.add(
        egui::Button::new(RichText::new(label).size(14.5).color(INK))
            .fill(Color32::WHITE)
            .stroke(Stroke::new(1.0, Color32::from_rgb(0xE7, 0xE9, 0xF3)))
            .corner_radius(CornerRadius::same(10))
            .min_size(egui::vec2(0.0, 36.0)),
    )
    .clicked()
}

fn text(ui: &mut Ui, value: &str) {
    ui.add(egui::Label::new(RichText::new(value).size(13.5).color(INK_SOFT)).wrap());
}

fn footer(ui: &mut Ui, buttons: impl FnOnce(&mut Ui)) {
    ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), buttons);
    });
}

fn install_options(ui: &mut Ui, form: &mut OptionsForm) -> Option<Action> {
    if !install::has_payload() {
        ui.label(
            RichText::new("This installer was built without the program files.").color(DANGER),
        );
        text(ui, "Build it with scripts\\build-installer.ps1.");
        return None;
    }
    text(ui, "Installs for your Windows account only; no administrator rights are needed. Your downloads and settings are kept when you upgrade.");
    ui.add_space(14.0);
    ui.label(RichText::new("Install to").strong().color(INK));
    ui.horizontal(|ui| {
        egui::Frame::new()
            .stroke(Stroke::new(1.0, Color32::from_rgb(0xE7, 0xE9, 0xF3)))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut form.folder)
                        .frame(false)
                        .desired_width(ui.available_width() - 110.0),
                );
            });
        if secondary(ui, "Browse") {
            if let Some(picked) = rfd::FileDialog::new().pick_folder() {
                form.folder = picked.join("Downpour").display().to_string();
            }
        }
    });
    ui.add_space(12.0);
    ui.checkbox(
        &mut form.start_menu,
        "Add Downpour and its uninstaller to the Start menu",
    );
    ui.checkbox(&mut form.desktop, "Add a desktop shortcut");
    ui.checkbox(&mut form.browsers, "Connect Firefox (browser integration)");
    if form.browsers {
        ui.indent("browser-note", |ui| {
            text(ui, "Registers Downpour's helper with Firefox (a per-user registry key). You add the extension itself on the next page.");
        });
    }
    if let Some(error) = &form.folder_error {
        ui.add_space(8.0);
        ui.add(egui::Label::new(RichText::new(error).size(13.5).color(DANGER)).wrap());
    }
    let mut action = None;
    footer(ui, |ui| {
        if primary(ui, "  Install  ") {
            match InstallFolder::new(PathBuf::from(form.folder.trim())) {
                Ok(folder) => {
                    form.folder_error = None;
                    let choose = |on: bool| if on { Shortcut::Create } else { Shortcut::Skip };
                    action = Some(Action::Install(InstallOptions {
                        folder,
                        start_menu_shortcut: choose(form.start_menu),
                        desktop_shortcut: choose(form.desktop),
                        browser_integration: if form.browsers {
                            BrowserIntegration::Register
                        } else {
                            BrowserIntegration::Skip
                        },
                    }));
                }
                Err(error) => form.folder_error = Some(capitalized(&error.to_string())),
            }
        }
        if secondary(ui, "Cancel") {
            action = Some(Action::Close);
        }
    });
    action
}

fn capitalized(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

fn uninstall_confirm(ui: &mut Ui, remove_user_data: &mut bool) -> Option<Action> {
    text(ui, "Removes the program, its shortcuts, and the browser integration. Files you downloaded are never deleted.");
    ui.add_space(12.0);
    ui.checkbox(
        remove_user_data,
        "Also remove my download list, queues, history, and settings",
    );
    let mut action = None;
    footer(ui, |ui| {
        if primary(ui, "  Uninstall  ") {
            action = Some(Action::Uninstall(if *remove_user_data {
                UserData::Remove
            } else {
                UserData::Keep
            }));
        }
        if secondary(ui, "Cancel") {
            action = Some(Action::Close);
        }
    });
    action
}

fn still_running(ui: &mut Ui, programs: &[RunningProgram], work: &Work) -> Option<Action> {
    ui.label(
        RichText::new("Downpour is still running")
            .size(16.0)
            .strong()
            .color(INK),
    );
    ui.add_space(6.0);
    text(
        ui,
        &format!(
            "These Downpour programs in {} did not close when asked, so their files cannot be changed:",
            work.folder().display()
        ),
    );
    ui.add_space(4.0);
    for program in programs {
        text(
            ui,
            &format!("• {} (process {})", program.exe.display(), program.pid),
        );
    }
    ui.add_space(8.0);
    text(ui, "Stopping them ends them at once. Downloads continue from their last saved progress the next time Downpour starts. You can also close them yourself and choose Try again.");
    let mut action = None;
    footer(ui, |ui| {
        if primary(ui, "  Stop them and continue  ") {
            action = Some(Action::StopAndContinue);
        }
        if secondary(ui, "Try again") {
            action = Some(match work {
                Work::Install(options) => Action::Install(options.clone()),
                Work::Uninstall { user_data, .. } => Action::Uninstall(*user_data),
            });
        }
        if secondary(ui, "Cancel") {
            action = Some(Action::Close);
        }
    });
    action
}

fn working(ui: &mut Ui, job: &Job) {
    ui.horizontal(|ui| {
        ui.spinner();
        ui.label(RichText::new("Working…").color(INK));
    });
    for line in job.log.lock().unwrap_or_else(|p| p.into_inner()).iter() {
        log_line(ui, line);
    }
}

/// A finished step with a check mark; a warning in its own color.
fn log_line(ui: &mut Ui, line: &LogLine) {
    let (mark, value, color) = match line {
        LogLine::Done(value) => ("✓", value, INK_SOFT),
        LogLine::Warning(value) => ("!", value, WARNING),
    };
    ui.add(
        egui::Label::new(
            RichText::new(format!("{mark} {value}"))
                .size(13.5)
                .color(color),
        )
        .wrap(),
    );
}

fn finished_page(ui: &mut Ui, mode: Mode, finished: &mut Finished) -> Option<Action> {
    let left_behind = finished.report.leftovers.len();
    let (headline, color) = match (mode, left_behind) {
        (Mode::Install, 0) => ("Downpour is installed".to_owned(), SUCCESS),
        (Mode::Install, count) => (
            format!("Downpour is installed; {count} old item(s) could not be removed"),
            WARNING,
        ),
        (Mode::Uninstall, 0) => ("Downpour was removed".to_owned(), SUCCESS),
        (Mode::Uninstall, count) => (
            format!("Downpour was removed, but {count} item(s) were left behind"),
            WARNING,
        ),
    };
    ui.label(RichText::new(headline).size(16.0).strong().color(color));
    ui.add_space(6.0);
    egui::ScrollArea::vertical()
        .max_height(220.0)
        .show(ui, |ui| {
            for line in &finished.log {
                log_line(ui, line);
            }
        });
    let mut action = None;
    if mode == Mode::Install {
        ui.add_space(14.0);
        egui::Frame::new()
            .fill(PRIMARY_SOFT)
            .corner_radius(CornerRadius::same(12))
            .inner_margin(egui::Margin::same(14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Add the browser extension").strong().color(INK));
                text(ui, "It hands downloads and videos from Firefox to Downpour. Firefox installs the signed add-on after you click Add.");
                ui.add_space(6.0);
                if secondary(ui, "Add to Firefox") {
                    action = Some(Action::AddToFirefox);
                }
                match &finished.browser_message {
                    Some(Ok(message)) => {
                        ui.add(egui::Label::new(RichText::new(message).color(SUCCESS)).wrap());
                    }
                    Some(Err(message)) => {
                        ui.add(egui::Label::new(RichText::new(message).color(DANGER)).wrap());
                    }
                    None => {}
                }
            });
    }
    footer(ui, |ui| match mode {
        Mode::Install => {
            if primary(ui, "  Open Downpour  ") {
                action = Some(Action::OpenApp);
            }
            if secondary(ui, "Close") {
                action = Some(Action::Close);
            }
        }
        Mode::Uninstall => {
            if primary(ui, "  Close  ") {
                action = Some(Action::Close);
            }
        }
    });
    action
}

fn failed(ui: &mut Ui, message: &str) -> Option<Action> {
    ui.label(
        RichText::new("That did not work")
            .size(16.0)
            .strong()
            .color(DANGER),
    );
    ui.add_space(6.0);
    ui.add(egui::Label::new(RichText::new(message).size(13.5).color(INK)).wrap());
    let mut action = None;
    footer(ui, |ui| {
        if primary(ui, "  Try again  ") {
            action = Some(Action::BackToStart);
        }
        if secondary(ui, "Close") {
            action = Some(Action::Close);
        }
    });
    action
}

fn style(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::light());
    ctx.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(16.0, 8.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(14.5));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.5));
    });
}
