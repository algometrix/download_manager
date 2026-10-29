use downpour_core::{DownloadView, QueueState};
use eframe::egui::{self, pos2, vec2, Align, Color32, CornerRadius, Layout, Rect, Sense, Ui};

use super::icons::{brand_mark, Icon};
use super::theme::{ControlShape, NavInk};
use super::widgets::{
    elided, icon_button_at, nav_item, section_label, section_label_with_action, toggle_switch_at,
    NavEntry, NAV_ITEM_HEIGHT,
};
use super::{format, hotkeys, theme, Action, Catalog, Filter, Page};
use crate::app_settings::{AppSettings, Appearance, NewDownloadMode};

const SPEED_CARD_HEIGHT: f32 = 58.0;
const AUTO_START_HEIGHT: f32 = 56.0;
/// Space between the footer cards, so each reads as its own control.
const FOOTER_GAP: f32 = 14.0;
/// Below this much room for navigation, the speed card (also shown in the
/// summary tiles) gives its space to the list.
const MIN_NAV_HEIGHT: f32 = 420.0;
const BRAND_MARK_SIZE: f32 = 38.0;
const SPEED_BADGE_SIZE: f32 = 32.0;
/// Corners of the speed card and speed badge on rounded forms.
const SPEED_CARD_RADIUS: u8 = 14;
const SPEED_BADGE_RADIUS: u8 = 9;
const SWITCH_SIZE: egui::Vec2 = vec2(36.0, 20.0);

pub struct SidebarState<'a> {
    pub downloads: &'a [DownloadView],
    pub catalog: &'a Catalog<'a>,
    pub current: Filter,
    pub page: Page,
    pub settings: &'a AppSettings,
}

pub fn show(ui: &mut Ui, state: SidebarState<'_>, actions: &mut Vec<Action>) {
    brand(ui);
    ui.add_space(18.0);

    // The footer stays pinned to the bottom; the navigation scrolls above it
    // when the window is short, so the two never overlap.
    // The footer is laid out top-down in a rect of exactly its height, with
    // no item spacing, so its cards cannot drift into each other.
    let fixed_footer = AUTO_START_HEIGHT + FOOTER_GAP + NAV_ITEM_HEIGHT;
    let card_height = SPEED_CARD_HEIGHT + FOOTER_GAP;
    let show_speed_card =
        ui.available_height() - fixed_footer - card_height - FOOTER_GAP >= MIN_NAV_HEIGHT;
    let footer_height = fixed_footer + if show_speed_card { card_height } else { 0.0 };

    egui::ScrollArea::vertical()
        .max_height((ui.available_height() - footer_height - FOOTER_GAP).max(0.0))
        .auto_shrink([false, true])
        .show(ui, |ui| navigation(ui, &state, actions));

    let area = ui.max_rect();
    let footer = Rect::from_min_max(
        pos2(area.left(), area.bottom() - footer_height),
        area.right_bottom(),
    );
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(footer)
            .layout(Layout::top_down(Align::Min)),
        |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if show_speed_card {
                speed_card(ui, state.downloads, state.settings, actions);
                ui.add_space(FOOTER_GAP);
            }
            auto_start_card(ui, state.settings.new_downloads, actions);
            ui.add_space(FOOTER_GAP);
            settings_row(ui, actions);
        },
    );
}

fn navigation(ui: &mut Ui, state: &SidebarState<'_>, actions: &mut Vec<Action>) {
    let ink = theme::sidebar_ink();
    let on_downloads = state.page == Page::Downloads;
    if section_label_with_action(ui, "Queues", Icon::Plus, "New queue").clicked() {
        actions.push(Action::NewQueue);
    }
    for queue in state.catalog.queues {
        let filter = Filter::Queue(queue.id);
        // A stopped queue's icon turns amber, so its state is visible here.
        let color = match queue.state {
            QueueState::Running => ink.ink_soft,
            QueueState::Stopped => ink.readable(theme::warning()),
        };
        let count = state
            .downloads
            .iter()
            .filter(|d| filter.includes(d))
            .count();
        let response = nav_item(
            ui,
            &ink,
            NavEntry {
                icon: Icon::Layers,
                icon_color: color,
                label: &queue.name,
                count: Some(count),
                selected: on_downloads && state.current == filter,
            },
        );
        if response.clicked() {
            actions.push(Action::SetFilter(filter));
        }
        response.context_menu(|ui| {
            let (label, action) = match queue.state {
                QueueState::Running => ("Stop queue", Action::StopQueue(queue.id)),
                QueueState::Stopped => ("Start queue", Action::StartQueue(queue.id)),
            };
            if ui.button(label).clicked() {
                actions.push(action);
                ui.close_menu();
            }
            if ui.button("Rename or change limit…").clicked() {
                actions.push(Action::RenameQueue(queue.id));
                ui.close_menu();
            }
            if queue.id != downpour_core::QueueId::MAIN && ui.button("Delete queue").clicked() {
                actions.push(Action::RemoveQueue(queue.id));
                ui.close_menu();
            }
        });
    }

    ui.add_space(10.0);
    section_label(ui, "Library");
    let library = [
        (Filter::All, Icon::Grid),
        (Filter::Downloading, Icon::Download),
        (Filter::Completed, Icon::Check),
        (Filter::Unfinished, Icon::Pause),
    ];
    for (filter, icon) in library {
        nav_entry(ui, state, &ink, filter, icon, ink.ink_soft, actions);
    }

    ui.add_space(10.0);
    if section_label_with_action(ui, "Categories", Icon::Tag, "Edit categories").clicked() {
        actions.push(Action::ManageCategories);
    }
    for category in state.catalog.categories.all() {
        let (color, _) = theme::look_colors(category.look);
        nav_entry(
            ui,
            state,
            &ink,
            Filter::Category(category.id),
            Icon::for_look(category.look),
            ink.readable(color),
            actions,
        );
    }

    ui.add_space(10.0);
    section_label(ui, "Insights");
    let statistics = nav_item(
        ui,
        &ink,
        NavEntry {
            icon: Icon::Chart,
            icon_color: ink.ink_soft,
            label: "Statistics",
            count: None,
            selected: state.page == Page::Statistics,
        },
    );
    if statistics.clicked() {
        actions.push(Action::ShowStatistics);
    }
}

fn nav_entry(
    ui: &mut Ui,
    state: &SidebarState<'_>,
    ink: &NavInk,
    filter: Filter,
    icon: Icon,
    icon_color: Color32,
    actions: &mut Vec<Action>,
) {
    let count = state
        .downloads
        .iter()
        .filter(|download| filter.includes(download))
        .count();
    let selected = state.page == Page::Downloads && state.current == filter;
    let title = state.catalog.title(filter);
    let entry = NavEntry {
        icon,
        icon_color,
        label: &title,
        count: Some(count),
        selected,
    };
    if nav_item(ui, ink, entry).clicked() {
        actions.push(Action::SetFilter(filter));
    }
}

fn brand(ui: &mut Ui) {
    let ink = theme::sidebar_ink();
    ui.horizontal(|ui| {
        let mark = brand_mark(ui.ctx());
        ui.add(egui::Image::new(&mark).fit_to_exact_size(vec2(BRAND_MARK_SIZE, BRAND_MARK_SIZE)));
        ui.add_space(4.0);
        ui.vertical(|ui| {
            ui.add_space(1.0);
            ui.label(
                egui::RichText::new("Downpour")
                    .font(theme::bold(18.0))
                    .color(ink.ink),
            );
            ui.label(
                egui::RichText::new("Download manager")
                    .font(theme::regular(12.0))
                    .color(ink.ink_faint),
            );
        });
    });
}

/// "Auto-start" switch: when on, downloads from the browser are queued
/// without asking, and start as soon as their queue has a free slot.
fn auto_start_card(ui: &mut Ui, mode: NewDownloadMode, actions: &mut Vec<Action>) {
    let ink = theme::sidebar_ink();
    let (rect, _) = ui.allocate_exact_size(
        vec2(ui.available_width(), AUTO_START_HEIGHT),
        Sense::hover(),
    );
    let painter = ui.painter();
    theme::paint_control(
        painter,
        rect,
        theme::control_radius(rect.height()),
        ink.panel_fill,
        None,
    );
    let inner = rect.shrink2(vec2(12.0, 9.0));
    let on = mode == NewDownloadMode::StartRightAway;
    let switch_rect = Rect::from_min_size(
        pos2(
            inner.right() - SWITCH_SIZE.x,
            inner.center().y - SWITCH_SIZE.y / 2.0,
        ),
        SWITCH_SIZE,
    );
    let text_width = inner.width() - 44.0;
    let title = elided(ui, "Auto-start", theme::semibold(13.0), ink.ink, text_width);
    let detail = if on {
        "Starts without asking"
    } else {
        "Asks before starting"
    };
    let detail = elided(ui, detail, theme::regular(11.5), ink.ink_soft, text_width);
    painter.galley(inner.min, title, ink.ink);
    painter.galley(
        pos2(inner.left(), inner.bottom() - detail.size().y),
        detail,
        ink.ink_soft,
    );
    let response = toggle_switch_at(ui, switch_rect, ui.id().with("auto-start"), on, ink.switch)
        .on_hover_text("When on, downloads sent by the browser skip the confirmation dialog");
    if response.clicked() {
        let next = if on {
            NewDownloadMode::AskFirst
        } else {
            NewDownloadMode::StartRightAway
        };
        actions.push(Action::SetNewDownloadMode(next));
    }
}

/// Card showing live throughput and the configured limit.
fn speed_card(
    ui: &mut Ui,
    downloads: &[DownloadView],
    settings: &AppSettings,
    actions: &mut Vec<Action>,
) {
    let ink = theme::sidebar_ink();
    let total: u64 = downloads
        .iter()
        .map(|download| download.bytes_per_second)
        .sum();
    let limit = match settings.engine.speed_limit_kib_per_second {
        None => "no limit".to_owned(),
        Some(kib) => format!("Limit {}", format::kib_rate(kib)),
    };
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), SPEED_CARD_HEIGHT),
        Sense::click(),
    );
    let painter = ui.painter();
    theme::paint_control(
        painter,
        rect,
        block_radius(rect.height(), SPEED_CARD_RADIUS),
        ink.card_fill,
        None,
    );
    let badge = Rect::from_center_size(
        pos2(rect.left() + 12.0 + SPEED_BADGE_SIZE / 2.0, rect.center().y),
        vec2(SPEED_BADGE_SIZE, SPEED_BADGE_SIZE),
    );
    // The surface with the primary color on it reads on every sidebar fill.
    painter.rect_filled(
        badge,
        block_radius(SPEED_BADGE_SIZE, SPEED_BADGE_RADIUS),
        theme::surface(),
    );
    Icon::Bolt.paint(painter, badge.shrink(8.0), theme::primary());

    let text_left = badge.right() + 10.0;
    let text_width = rect.right() - 12.0 - text_left;
    let value = elided(
        ui,
        format::speed(total),
        theme::bold(17.0),
        ink.ink,
        text_width,
    );
    let detail = elided(
        ui,
        format!("Current speed · {limit}"),
        theme::regular(11.5),
        ink.ink_soft,
        text_width,
    );
    let top = rect.center().y - (value.size().y + detail.size().y) / 2.0;
    let value_height = value.size().y;
    painter.galley(pos2(text_left, top), value, ink.ink);
    painter.galley(pos2(text_left, top + value_height), detail, ink.ink_soft);

    if response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("Change the speed limit")
        .clicked()
    {
        actions.push(Action::OpenSettings);
    }
}

/// Settings, with a light/dark switch beside it, in a row of exactly
/// `NAV_ITEM_HEIGHT` so its hover box keeps the footer gap above it.
fn settings_row(ui: &mut Ui, actions: &mut Vec<Action>) {
    let ink = theme::sidebar_ink();
    let (row, _) =
        ui.allocate_exact_size(vec2(ui.available_width(), NAV_ITEM_HEIGHT), Sense::hover());
    let toggle = Rect::from_center_size(pos2(row.right() - 19.0, row.center().y), vec2(32.0, 32.0));
    let settings = Rect::from_min_max(row.min, pos2(toggle.left() - 6.0, row.bottom()));
    ui.scope_builder(egui::UiBuilder::new().max_rect(settings), |ui| {
        let entry = NavEntry {
            icon: Icon::Gear,
            icon_color: ink.ink_soft,
            label: "Settings",
            count: None,
            selected: false,
        };
        if nav_item(ui, &ink, entry).clicked() {
            actions.push(Action::OpenSettings);
        }
    });
    let (icon, tooltip, next) = match theme::mode() {
        theme::Mode::Light => (Icon::Moon, "Switch to dark mode", Appearance::Dark),
        theme::Mode::Dark => (Icon::Sun, "Switch to light mode", Appearance::Light),
    };
    let tooltip = hotkeys::SWITCH_THEME.label(tooltip);
    if icon_button_at(
        ui,
        toggle,
        ui.id().with("theme-toggle"),
        icon,
        &tooltip,
        ink.icon_button,
    )
    .clicked()
    {
        actions.push(Action::SetAppearance(next));
    }
}

/// Corners for a small block of fixed shape: `rounded` on forms with rounded
/// controls, fully round ends on pill forms.
fn block_radius(height: f32, rounded: u8) -> CornerRadius {
    match theme::form().controls {
        ControlShape::Rounded { .. } => CornerRadius::same(rounded),
        ControlShape::Pill => theme::control_radius(height),
    }
}
