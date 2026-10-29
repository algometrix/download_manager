//! The link checker: paste many links, see which files are online or
//! offline, pick the ones to download, and add them in one go.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use downpour_core::link_check::{links_in, CheckedLink, LinkStatus};
use downpour_core::{DownloadKind, NewDownload, Queue, QueueId, RequestContext, StartMode};
use eframe::egui::{self, vec2, Ui};
use url::Url;

use super::dialogs::{
    chosen_folder, folder_picker, footer, hint, queue_picker, shortcut_pressed, title, Shortcut,
};
use super::icons::Icon;
use super::widgets::{button, field_frame, pill, pill_width, ButtonKind};
use super::{format, hotkeys, theme};

/// Below this many rows the list grows; above it, it scrolls.
const LIST_HEIGHT: f32 = 300.0;
/// Width of the status label column ("Online", "Offline", "Unknown").
const STATUS_COLUMN: f32 = 84.0;

pub struct LinkGrabberForm {
    text: String,
    phase: Phase,
    folder: String,
    queue: QueueId,
    /// Links waiting for the app to start checking them.
    pending: Option<Vec<Url>>,
}

enum Phase {
    Editing,
    Checking {
        count: usize,
        result: Arc<Mutex<Option<Vec<CheckedLink>>>>,
    },
    Checked(Vec<Row>),
}

struct Row {
    link: CheckedLink,
    selected: bool,
}

/// What the dialog asks the app to do.
pub enum LinkGrabberCommand {
    Close,
    Add(Vec<NewDownload>),
}

/// A check the app should start: the links, and where to put the result.
pub struct LinkCheck {
    pub links: Vec<Url>,
    pub done: Box<dyn FnOnce(Vec<CheckedLink>) + Send>,
}

impl LinkGrabberForm {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            phase: Phase::Editing,
            folder: String::new(),
            queue: QueueId::MAIN,
            pending: None,
        }
    }

    /// Hands a requested check to the app, once.
    pub fn take_check(&mut self) -> Option<LinkCheck> {
        let links = self.pending.take()?;
        let result = Arc::new(Mutex::new(None));
        self.phase = Phase::Checking {
            count: links.len(),
            result: Arc::clone(&result),
        };
        Some(LinkCheck {
            links,
            done: Box::new(move |checked| {
                *result.lock().unwrap_or_else(|p| p.into_inner()) = Some(checked)
            }),
        })
    }

    pub fn is_checking(&self) -> bool {
        matches!(self.phase, Phase::Checking { .. })
    }

    /// Moves a finished check into the list; online files start selected.
    fn collect_result(&mut self) {
        let Phase::Checking { result, .. } = &self.phase else {
            return;
        };
        let Some(checked) = result.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            return;
        };
        let rows = checked
            .into_iter()
            .map(|link| Row {
                selected: link.status.is_online(),
                link,
            })
            .collect();
        self.phase = Phase::Checked(rows);
    }
}

pub fn show(
    ui: &mut Ui,
    form: &mut LinkGrabberForm,
    queues: &[Queue],
) -> Option<LinkGrabberCommand> {
    form.collect_result();
    title(
        ui,
        "Add links",
        "Paste any number of links. Downpour checks each one, so only files that still exist are downloaded.",
    );
    match &form.phase {
        Phase::Editing => editing(ui, form),
        Phase::Checking { count, .. } => {
            ui.horizontal(|ui| {
                ui.spinner();
                hint(ui, &format!("Checking {count} links…"));
            });
            cancel_only(ui)
        }
        Phase::Checked(_) => checked(ui, form, queues),
    }
}

fn editing(ui: &mut Ui, form: &mut LinkGrabberForm) -> Option<LinkGrabberCommand> {
    field_frame()
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut form.text)
                            .hint_text(
                                "https://example.com/file.zip\nhttps://example.org/video.mp4",
                            )
                            .frame(false)
                            .desired_rows(8)
                            .desired_width(f32::INFINITY)
                            .font(egui::FontId::monospace(12.5)),
                    );
                });
        });
    let found = links_in(&form.text);
    hint(
        ui,
        &match found.len() {
            0 => "No links found yet.".to_owned(),
            1 => "1 link found.".to_owned(),
            count => format!("{count} links found."),
        },
    );
    let mut command = None;
    footer(ui, |ui| {
        let check = ui
            .add_enabled_ui(!found.is_empty(), |ui| {
                button(ui, ButtonKind::Primary, Some(Icon::Search), "Check links")
            })
            .inner;
        if check.clicked() {
            form.pending = Some(found.clone());
        }
        if button(
            ui,
            ButtonKind::Secondary,
            None,
            &hotkeys::CANCEL.label("Cancel"),
        )
        .clicked()
        {
            command = Some(LinkGrabberCommand::Close);
        }
    });
    command
}

fn cancel_only(ui: &mut Ui) -> Option<LinkGrabberCommand> {
    let mut command = None;
    footer(ui, |ui| {
        if button(
            ui,
            ButtonKind::Secondary,
            None,
            &hotkeys::CANCEL.label("Cancel"),
        )
        .clicked()
        {
            command = Some(LinkGrabberCommand::Close);
        }
    });
    command
}

fn checked(
    ui: &mut Ui,
    form: &mut LinkGrabberForm,
    queues: &[Queue],
) -> Option<LinkGrabberCommand> {
    let Phase::Checked(rows) = &mut form.phase else {
        return None;
    };
    summary(ui, rows);
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.small_button("Select all online").clicked() {
            for row in rows.iter_mut() {
                row.selected = row.link.status.is_online();
            }
        }
        if ui.small_button("Select none").clicked() {
            for row in rows.iter_mut() {
                row.selected = false;
            }
        }
    });
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .max_height(LIST_HEIGHT)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for row in rows.iter_mut() {
                link_row(ui, row);
            }
        });
    ui.add_space(8.0);
    folder_picker(ui, "Save to", &mut form.folder, "Each category's folder");
    ui.add_space(6.0);
    queue_picker(ui, "grabber-queue", &mut form.queue, queues);

    let selected = rows.iter().filter(|row| row.selected).count();
    let mut command = None;
    let shortcut = shortcut_pressed(ui);
    let start_label = match selected {
        1 => "Download 1 file".to_owned(),
        count => format!("Download {count} files"),
    };
    let requests = |start: StartMode| -> Vec<NewDownload> {
        rows.iter()
            .filter(|row| row.selected)
            .map(|row| request_for(&row.link, chosen_folder(&form.folder), form.queue, start))
            .collect()
    };
    footer(ui, |ui| {
        let can_add = selected > 0;
        let start = ui
            .add_enabled_ui(can_add, |ui| {
                button(
                    ui,
                    ButtonKind::Primary,
                    Some(Icon::Download),
                    &hotkeys::DIALOG_START.label(&start_label),
                )
            })
            .inner;
        if can_add && (start.clicked() || shortcut == Some(Shortcut::Start)) {
            command = Some(LinkGrabberCommand::Add(requests(StartMode::Queue)));
        }
        let later = ui
            .add_enabled_ui(can_add, |ui| {
                button(
                    ui,
                    ButtonKind::Secondary,
                    Some(Icon::Clock),
                    &hotkeys::DIALOG_LATER.label("Download later"),
                )
            })
            .inner
            .on_hover_text("Adds them to the queue without starting");
        if can_add && (later.clicked() || shortcut == Some(Shortcut::Later)) {
            command = Some(LinkGrabberCommand::Add(requests(StartMode::Hold)));
        }
        let cancel = button(
            ui,
            ButtonKind::Secondary,
            None,
            &hotkeys::DIALOG_CANCEL.label("Cancel"),
        );
        if cancel.clicked() || shortcut == Some(Shortcut::Cancel) {
            command = Some(LinkGrabberCommand::Close);
        }
    });
    command
}

/// "4 online · 2 no longer available · 1 could not be checked".
fn summary(ui: &mut Ui, rows: &[Row]) {
    let count = |wanted: fn(&LinkStatus) -> bool| {
        rows.iter().filter(|row| wanted(&row.link.status)).count()
    };
    let online = count(LinkStatus::is_online);
    let offline = count(|status| matches!(status, LinkStatus::Offline { .. }));
    let unknown = count(|status| matches!(status, LinkStatus::Unknown { .. }));
    let mut parts = vec![format!("{online} online")];
    if offline > 0 {
        parts.push(format!("{offline} no longer available"));
    }
    if unknown > 0 {
        parts.push(format!("{unknown} could not be checked"));
    }
    ui.label(
        egui::RichText::new(parts.join(" · "))
            .font(theme::semibold(13.5))
            .color(theme::ink()),
    );
}

fn link_row(ui: &mut Ui, row: &mut Row) {
    let (label, fg, bg) = status_style(&row.link.status);
    let online = row.link.status.is_online();
    ui.horizontal(|ui| {
        ui.add_enabled_ui(online, |ui| ui.checkbox(&mut row.selected, ""));
        // A fixed column, so names line up whatever the status label's length.
        let width = pill_width(ui, label).max(STATUS_COLUMN);
        let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0), egui::Sense::hover());
        pill(ui, rect.left_center(), label, fg, bg);
        ui.vertical(|ui| {
            let name = match &row.link.status {
                LinkStatus::Online { name, .. } => name.value.clone(),
                LinkStatus::Offline { .. } | LinkStatus::Unknown { .. } => row.link.url.to_string(),
            };
            let detail = match &row.link.status {
                LinkStatus::Online {
                    size: Some(size), ..
                } => format!("{} · {}", format::bytes(*size), host_of(&row.link)),
                LinkStatus::Online { size: None, .. } => {
                    format!("size unknown · {}", host_of(&row.link))
                }
                LinkStatus::Offline { reason } | LinkStatus::Unknown { reason } => reason.clone(),
            };
            ui.add(
                egui::Label::new(
                    egui::RichText::new(name)
                        .font(theme::semibold(13.0))
                        .color(theme::ink()),
                )
                .truncate(),
            )
            .on_hover_text(row.link.url.as_str());
            ui.label(
                egui::RichText::new(detail)
                    .font(theme::regular(12.0))
                    .color(theme::ink_soft()),
            );
        });
    });

    ui.add_space(4.0);
}

fn status_style(status: &LinkStatus) -> (&'static str, egui::Color32, egui::Color32) {
    match status {
        LinkStatus::Online { .. } => ("Online", theme::success(), theme::success_soft()),
        LinkStatus::Offline { .. } => ("Offline", theme::danger(), theme::danger_soft()),
        LinkStatus::Unknown { .. } => ("Unknown", theme::ink_soft(), theme::surface_muted()),
    }
}

fn host_of(link: &CheckedLink) -> &str {
    link.url.host_str().unwrap_or("unknown host")
}

/// The download for one checked link: the name the check found (keeping
/// where it came from), and the chosen folder and queue.
fn request_for(
    link: &CheckedLink,
    folder: Option<PathBuf>,
    queue: QueueId,
    start: StartMode,
) -> NewDownload {
    let file_name = match &link.status {
        LinkStatus::Online { name, .. } => Some(name.clone()),
        // Only online rows can be selected; the server names the others.
        LinkStatus::Offline { .. } | LinkStatus::Unknown { .. } => None,
    };
    NewDownload {
        url: link.url.clone(),
        file_name,
        folder,
        context: RequestContext::default(),
        start,
        kind: DownloadKind::Detect,
        queue: Some(queue),
        page_url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use downpour_core::{FileName, FileNameSource};

    fn row(url: &str, status: LinkStatus) -> Row {
        let link = CheckedLink {
            url: Url::parse(url).unwrap(),
            status,
        };
        Row {
            selected: link.status.is_online(),
            link,
        }
    }

    #[test]
    fn only_online_links_start_selected_and_requests_keep_the_name() {
        let server_name = FileName::new("a.rar", FileNameSource::Server);
        let rows = [
            row(
                "https://example.com/files/a.rar",
                LinkStatus::Online {
                    name: server_name.clone(),
                    size: Some(10),
                },
            ),
            row(
                "https://example.com/files/b.rar",
                LinkStatus::Offline {
                    reason: "the server answered 404 Not Found".into(),
                },
            ),
        ];
        assert!(rows[0].selected && !rows[1].selected);
        let request = request_for(&rows[0].link, None, QueueId::MAIN, StartMode::Hold);
        assert_eq!(request.file_name, Some(server_name));
        assert_eq!(request.url, rows[0].link.url);
        assert_eq!(request.start, StartMode::Hold);
    }

    #[test]
    fn a_name_guessed_from_the_url_stays_replaceable_by_the_server() {
        let url = Url::parse("https://example.com/files/c.zip").unwrap();
        let link = row(
            url.as_str(),
            LinkStatus::Online {
                name: FileName::guess_from_url(&url),
                size: None,
            },
        )
        .link;
        let request = request_for(&link, None, QueueId::MAIN, StartMode::Queue);
        assert!(request.file_name.unwrap().is_replaceable_by_server());
    }
}
