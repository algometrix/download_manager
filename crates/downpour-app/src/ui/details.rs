use downpour_core::{DownloadStatus, DownloadView};
use eframe::egui::{self, pos2, vec2, Align2, CornerRadius, Rect, Sense, Ui};

use super::icons::Icon;
use super::list::{primary_row_action, speed_limit_menu, status_text, unix_now};
use super::theme::Depth;
use super::widgets::{
    button, elided, file_tile, galley, icon_button, pill, progress_ring, segment_bar, ButtonKind,
};
use super::{download_page, format, hotkeys, theme, Action, Catalog};

const RING_RADIUS: f32 = 62.0;
/// The ring's stroke, as `widgets::progress_ring` draws it.
const RING_WIDTH: f32 = 10.0;
/// Room between a raised dial and the ring around it.
const DIAL_GAP: f32 = 8.0;
/// How much tighter the corners of the panel's small blocks (metric
/// tiles, notices) are than a card's.
const BLOCK_CORNER_TIGHTENING: u8 = 4;

/// The corners of the panel's small blocks.
fn block_radius() -> CornerRadius {
    theme::inner_card_radius(BLOCK_CORNER_TIGHTENING)
}

pub fn show(
    ui: &mut Ui,
    download: &DownloadView,
    catalog: &Catalog<'_>,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("Details")
                .font(theme::title(18.0))
                .color(theme::ink()),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if icon_button(ui, Icon::Close, &hotkeys::DESELECT.label("Close details")).clicked() {
                actions.push(Action::Select(None));
            }
        });
    });
    ui.add_space(10.0);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            identity(ui, download, catalog);
            ui.add_space(14.0);
            ring(ui, download);
            ui.add_space(14.0);
            metrics(ui, download);
            match &download.status {
                DownloadStatus::Failed { reason } => {
                    ui.add_space(12.0);
                    notice(ui, NoticeKind::Failure, "Download failed", reason);
                }
                DownloadStatus::WaitingForSpace {
                    directory,
                    needed_bytes,
                } => {
                    ui.add_space(12.0);
                    let text =
                        format::space_shortage(&download.file_name, directory, *needed_bytes);
                    notice(ui, NoticeKind::Warning, "Waiting for disk space", &text);
                }
                _ => {}
            }
            if !download.segments.is_empty() {
                ui.add_space(16.0);
                connections_map(ui, download);
            }
            ui.add_space(16.0);
            information(ui, download, catalog, actions);
            ui.add_space(18.0);
            buttons(ui, download, actions);
            ui.add_space(8.0);
        });
}

fn identity(ui: &mut Ui, download: &DownloadView, catalog: &Catalog<'_>) {
    ui.horizontal(|ui| {
        let (tile, _) = ui.allocate_exact_size(vec2(52.0, 52.0), Sense::hover());
        let (fg, bg) = theme::look_colors(catalog.categories.get(download.category).look);
        file_tile(
            ui,
            tile,
            &format::extension_badge(&download.file_name),
            fg,
            bg,
        );
        ui.vertical(|ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&download.file_name)
                        .font(theme::semibold(15.0))
                        .color(theme::ink()),
                )
                .wrap(),
            );
            let (fg, bg) = theme::status_colors(&download.status);
            let (slot, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
            pill(
                ui,
                pos2(slot.left(), slot.center().y),
                status_text(download),
                fg,
                bg,
            );
        });
    });
}

fn ring(ui: &mut Ui, download: &DownloadView) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 150.0), Sense::hover());
    let center = rect.center();
    let fraction = download.fraction();
    let color = theme::status_colors(&download.status).0;
    match theme::form().depth {
        // A raised dial inside the ring, like a soft player's round knob.
        Depth::Raised { .. } => {
            let dial_radius = RING_RADIUS - RING_WIDTH / 2.0 - DIAL_GAP;
            theme::paint_control(
                ui.painter(),
                Rect::from_center_size(center, vec2(2.0 * dial_radius, 2.0 * dial_radius)),
                theme::round_ends(2.0 * dial_radius),
                theme::surface(),
                None,
            );
        }
        Depth::Shadow { .. } | Depth::Bezel { .. } | Depth::Flat { .. } => {}
    }
    progress_ring(
        ui.painter(),
        center,
        RING_RADIUS,
        fraction.unwrap_or(0.0),
        color,
    );
    let headline = match fraction {
        Some(fraction) => format!("{:.0}%", fraction * 100.0),
        None => format::bytes(download.received),
    };
    let caption = match download.size {
        Some(size) if download.size_is_estimate => format!("of about {}", format::bytes(size)),
        Some(size) => format!("of {}", format::bytes(size)),
        None => "size unknown".to_owned(),
    };
    let headline_font = super::widgets::fitted_font(ui, &headline, theme::bold, 26.0, 14.0, 100.0);
    ui.painter().text(
        center - vec2(0.0, 8.0),
        Align2::CENTER_CENTER,
        headline,
        headline_font,
        theme::ink(),
    );
    ui.painter().text(
        center + vec2(0.0, 16.0),
        Align2::CENTER_CENTER,
        caption,
        theme::regular(12.0),
        theme::ink_soft(),
    );
}

/// 2x2 grid of small metric cards.
fn metrics(ui: &mut Ui, download: &DownloadView) {
    let active = download.status.is_active();
    let speed = if active {
        format::speed(download.bytes_per_second)
    } else {
        "—".to_owned()
    };
    let eta = match (active, download.eta()) {
        (true, Some(eta)) => format::duration(eta),
        _ => "—".to_owned(),
    };
    let connections = if active {
        download.connections.to_string()
    } else {
        "—".to_owned()
    };
    let resume = match download.resumable {
        Some(true) => "Supported",
        Some(false) => "Restarts",
        None => "Unknown",
    };
    let cards = [
        ("Speed", speed),
        ("Time left", eta),
        ("Connections", connections),
        ("Resume", resume.to_owned()),
    ];
    let gap = 10.0;
    let width = (ui.available_width() - gap) / 2.0;
    for (index, pair) in cards.chunks(2).enumerate() {
        if index > 0 {
            // Raised tiles stand out by their shadows, which need room.
            ui.add_space(theme::control_shadow_reach());
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (label, value) in pair {
                let (rect, _) = ui.allocate_exact_size(vec2(width, 62.0), Sense::hover());
                theme::paint_control(
                    ui.painter(),
                    rect,
                    block_radius(),
                    theme::inset_fill(),
                    None,
                );
                let inner = rect.shrink2(vec2(12.0, 10.0));
                let label = elided(
                    ui,
                    *label,
                    theme::regular(12.0),
                    theme::ink_soft(),
                    inner.width(),
                );
                let value = elided(
                    ui,
                    value.as_str(),
                    theme::semibold(15.0),
                    theme::ink(),
                    inner.width(),
                );
                ui.painter().galley(inner.min, label, theme::ink_soft());
                ui.painter().galley(
                    pos2(inner.left(), inner.bottom() - value.size().y),
                    value,
                    theme::ink(),
                );
            }
        });
    }
}

#[derive(Clone, Copy)]
enum NoticeKind {
    Failure,
    Warning,
}

/// A tinted card explaining a failure or a wait, with an icon and a title.
fn notice(ui: &mut Ui, kind: NoticeKind, title: &str, reason: &str) {
    let (fg, bg) = match kind {
        NoticeKind::Failure => (theme::danger(), theme::danger_soft()),
        NoticeKind::Warning => (theme::warning(), theme::warning_soft()),
    };
    egui::Frame::new()
        .fill(bg)
        .corner_radius(block_radius())
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (icon, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                Icon::Alert.paint(ui.painter(), icon, fg);
                ui.label(
                    egui::RichText::new(title)
                        .font(theme::semibold(13.0))
                        .color(fg),
                );
            });
            ui.add(
                egui::Label::new(
                    egui::RichText::new(reason)
                        .font(theme::regular(12.5))
                        .color(theme::ink()),
                )
                .wrap(),
            );
        });
}

/// Which parts of the file each connection has written, like a disk map.
fn connections_map(ui: &mut Ui, download: &DownloadView) {
    let title = galley(
        ui,
        "Connection map",
        theme::semibold(13.0),
        theme::ink_soft(),
    );
    let count = format!("{} segments", download.segments.len());
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), title.size().y), Sense::hover());
    ui.painter().galley(rect.min, title, theme::ink_soft());
    ui.painter().text(
        rect.right_top(),
        Align2::RIGHT_TOP,
        count,
        theme::regular(12.0),
        theme::ink_faint(),
    );
    ui.add_space(6.0);
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 16.0), Sense::hover());
    segment_bar(
        ui.painter(),
        bar,
        download.size.unwrap_or(0),
        &download.segments,
    );
}

fn information(
    ui: &mut Ui,
    download: &DownloadView,
    catalog: &Catalog<'_>,
    actions: &mut Vec<Action>,
) {
    let location = download
        .target
        .clone()
        .unwrap_or_else(|| download.directory.clone());
    info_row(ui, "Saved to", &location.display().to_string());
    if link(ui, "Show in folder") {
        actions.push(Action::Reveal(location));
    }
    ui.add_space(6.0);
    if let Some(stream) = &download.stream {
        let quality = match &stream.variant {
            Some(variant) => variant.label(),
            None => stream.quality.label(),
        };
        info_row(
            ui,
            "Video stream",
            &format!("{} · {quality}", stream.format.label()),
        );
        ui.add_space(6.0);
    }
    if let Some(queue) = catalog.queue(download.queue) {
        info_row(ui, "Queue", &queue.name);
        if download.can_start_now && link(ui, "Start now") {
            actions.push(Action::StartNow(download.id));
        }
        ui.add_space(6.0);
    }
    if download.status.can_change_speed_limit() {
        info_row(
            ui,
            "Speed limit",
            &format::download_limit(download.speed_limit, catalog.global_limit_kib_per_second),
        );
        // Looks like the panel's other links, but opens the menu.
        let change = egui::Button::new(
            egui::RichText::new("Change speed limit")
                .font(theme::semibold(12.5))
                .color(theme::primary()),
        )
        .frame(false);
        egui::menu::menu_custom_button(ui, change, |ui| {
            speed_limit_menu(ui, download, catalog, actions)
        });
        ui.add_space(6.0);
    }
    info_row(ui, "Address", download.url.as_str());
    if link(ui, &hotkeys::COPY_ADDRESS.label("Copy address")) {
        actions.push(Action::CopyText(download.url.to_string()));
    }
    if let Some(page) = download_page(download) {
        ui.add_space(6.0);
        info_row(ui, "Found on", page.as_str());
        ui.horizontal(|ui| {
            if link(ui, "Open download page") {
                actions.push(Action::OpenPage(page.clone()));
            }
            ui.label(egui::RichText::new("·").color(theme::ink_faint()));
            if link(ui, "Copy page address") {
                actions.push(Action::CopyText(page.to_string()));
            }
        });
    }
    ui.add_space(6.0);
    info_row(
        ui,
        "Added",
        &format::added_at(download.added_at_unix, unix_now()),
    );
    if let Some(finished) = download.finished_at_unix {
        ui.add_space(6.0);
        info_row(ui, "Finished", &format::added_at(finished, unix_now()));
    }
}

/// Primary-colored text that acts like a link; returns whether it was clicked.
fn link(ui: &mut Ui, text: &str) -> bool {
    ui.add(
        egui::Label::new(
            egui::RichText::new(text)
                .font(theme::semibold(12.5))
                .color(theme::primary()),
        )
        .sense(Sense::click()),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
    .clicked()
}

fn info_row(ui: &mut Ui, label: &str, value: &str) {
    ui.label(
        egui::RichText::new(label)
            .font(theme::regular(12.0))
            .color(theme::ink_faint()),
    );
    ui.add(
        egui::Label::new(
            egui::RichText::new(value)
                .font(theme::regular(13.0))
                .color(theme::ink()),
        )
        .wrap()
        .selectable(true),
    );
}

/// Action buttons wrap onto a new line instead of overflowing the panel.
fn buttons(ui: &mut Ui, download: &DownloadView, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        if let Some((icon, label, action)) = primary_row_action(download) {
            if button(
                ui,
                ButtonKind::Primary,
                Some(icon),
                &hotkeys::PRIMARY.label(label),
            )
            .clicked()
            {
                actions.push(action);
            }
        }
        if let (DownloadStatus::Completed, Some(target)) = (&download.status, &download.target) {
            if button(
                ui,
                ButtonKind::Primary,
                Some(Icon::File),
                &hotkeys::OPEN_FILE.label("Open file"),
            )
            .clicked()
            {
                actions.push(Action::OpenFile(target.clone()));
            }
        }
        let can_start_over = download.status.can_resume() && download.received > 0;
        if can_start_over
            && button(ui, ButtonKind::Secondary, Some(Icon::Refresh), "Start over")
                .on_hover_text("Discard downloaded bytes and download again")
                .clicked()
        {
            actions.push(Action::Restart(download.id));
        }
        let stuck = matches!(
            download.status,
            DownloadStatus::Failed { .. } | DownloadStatus::Paused
        );
        if stuck
            && button(
                ui,
                ButtonKind::Secondary,
                Some(Icon::Link),
                "Refresh address",
            )
            .on_hover_text("Get a fresh link from the page this download came from")
            .clicked()
        {
            actions.push(Action::RefreshAddress(download.id));
        }
        if button(
            ui,
            ButtonKind::Danger,
            Some(Icon::Trash),
            &hotkeys::REMOVE.label("Remove"),
        )
        .clicked()
        {
            actions.push(Action::AskRemove(download.id));
        }
    });
}
