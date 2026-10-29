//! Custom-painted widgets in the app's visual style.
//!
//! Every widget that shows text measures the laid-out text first and sizes
//! itself from that measurement, so labels never overflow their buttons.

use std::sync::Arc;

use downpour_core::segment::Segment;
use eframe::egui::text::{LayoutJob, TextWrapping};
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, FontId, Galley, Painter, Rect,
    Response, Sense, Stroke, StrokeKind, Ui,
};

use super::icons::{arc, Icon};
use super::theme::{
    self, ButtonInk, IconButtonInk, IconButtonShape, NavInk, NavSelection, ProgressStyle,
    SwitchInk, TileStyle,
};

const BUTTON_HEIGHT: f32 = 36.0;
pub const NAV_ITEM_HEIGHT: f32 = 38.0;
const BUTTON_PADDING_X: f32 = 16.0;
const BUTTON_ICON: f32 = 16.0;
const BUTTON_GAP: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Danger,
}

pub fn galley(ui: &Ui, text: impl Into<String>, font: FontId, color: Color32) -> Arc<Galley> {
    ui.painter().layout_no_wrap(text.into(), font, color)
}

/// One line of text, cut with an ellipsis to fit `max_width`.
pub fn elided(
    ui: &Ui,
    text: impl Into<String>,
    font: FontId,
    color: Color32,
    max_width: f32,
) -> Arc<Galley> {
    let mut job = LayoutJob::simple_singleline(text.into(), font, color);
    job.wrap = TextWrapping {
        max_width: max_width.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.fonts(|fonts| fonts.layout_job(job))
}

/// The largest font size between `min` and `max` at which `text` fits.
pub fn fitted_font(
    ui: &Ui,
    text: &str,
    font: fn(f32) -> FontId,
    max: f32,
    min: f32,
    width: f32,
) -> FontId {
    let mut size = max;
    while size > min && galley(ui, text, font(size), Color32::WHITE).size().x > width {
        size -= 1.0;
    }
    font(size)
}

/// Width that [`button`] will take for `label`, for layouts that must plan ahead.
pub fn button_width(ui: &Ui, icon: Option<Icon>, label: &str) -> f32 {
    let icon_space = if icon.is_some() {
        BUTTON_ICON + BUTTON_GAP
    } else {
        0.0
    };
    (BUTTON_PADDING_X * 2.0
        + icon_space
        + galley(ui, label, theme::semibold(14.0), Color32::WHITE)
            .size()
            .x)
        .ceil()
}

fn button_ink(kind: ButtonKind) -> ButtonInk {
    let control = theme::control_look();
    match kind {
        ButtonKind::Primary => theme::main_button_ink(),
        ButtonKind::Secondary => ButtonInk {
            fill: control.fill,
            hover_fill: theme::primary_soft(),
            text: theme::ink(),
            outline: control.outline,
        },
        ButtonKind::Danger => ButtonInk {
            fill: control.fill,
            hover_fill: theme::danger_soft(),
            text: theme::danger(),
            outline: control.outline,
        },
    }
}

/// Button sized to its label (and optional icon), in the theme's shape.
pub fn button(ui: &mut Ui, kind: ButtonKind, icon: Option<Icon>, label: &str) -> Response {
    let enabled = ui.is_enabled();
    let ink = button_ink(kind);
    let text_color = ink.text;
    let text = galley(ui, label, theme::semibold(14.0), text_color);
    let icon_space = if icon.is_some() {
        BUTTON_ICON + BUTTON_GAP
    } else {
        0.0
    };
    let (rect, response) = ui.allocate_exact_size(
        vec2(button_width(ui, icon, label), BUTTON_HEIGHT),
        Sense::click(),
    );

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = theme::control_radius(rect.height());
        let fill = if enabled && response.hovered() {
            ink.hover_fill
        } else {
            ink.fill
        };
        theme::paint_control(painter, rect, radius, fill, ink.outline);
        let mut x = rect.left() + BUTTON_PADDING_X;
        if let Some(icon) = icon {
            let icon_rect = Rect::from_min_size(
                pos2(x, rect.center().y - BUTTON_ICON / 2.0),
                vec2(BUTTON_ICON, BUTTON_ICON),
            );
            icon.paint(painter, icon_rect, text_color);
            x += icon_space;
        }
        painter.galley(
            pos2(x, rect.center().y - text.size().y / 2.0),
            text,
            text_color,
        );
        if !enabled {
            painter.rect_filled(
                rect,
                radius,
                // Washes the button out toward the surface it sits on.
                theme::surface().gamma_multiply(0.55),
            );
        }
    }
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

/// Square icon-only button with a tooltip.
pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::click());
    paint_icon_button(
        ui.painter(),
        rect,
        icon,
        &response,
        IconButtonInk::on_page(theme::ink_soft()),
    );
    response
        .on_hover_text(tooltip)
        .on_hover_cursor(CursorIcon::PointingHand)
}

/// Icon button placed at an exact rect, for rows painted by hand.
pub fn icon_button_at(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    icon: Icon,
    tooltip: &str,
    ink: IconButtonInk,
) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    paint_icon_button(ui.painter(), rect, icon, &response, ink);
    response
        .on_hover_text(tooltip)
        .on_hover_cursor(CursorIcon::PointingHand)
}

fn paint_icon_button(
    painter: &Painter,
    rect: Rect,
    icon: Icon,
    response: &Response,
    ink: IconButtonInk,
) {
    let hovered = response.hovered();
    match theme::form().icon_buttons {
        IconButtonShape::Square => {
            if hovered {
                painter.rect_filled(rect, theme::control_radius(rect.height()), ink.hover_fill);
            }
        }
        IconButtonShape::Circle => {
            let fill = if hovered { ink.hover_fill } else { ink.fill };
            theme::paint_control(painter, rect, theme::round_ends(rect.height()), fill, None);
        }
    }
    let color = if hovered { ink.hover_icon } else { ink.icon };
    icon.paint(painter, rect.shrink(8.0), color);
}

/// Width a pill needs for `text`.
pub fn pill_width(ui: &Ui, text: &str) -> f32 {
    galley(ui, text, theme::semibold(12.0), Color32::WHITE)
        .size()
        .x
        + 20.0
}

/// Paints a rounded label whose left edge sits at `left_center`; returns its rect.
pub fn pill(ui: &Ui, left_center: egui::Pos2, text: &str, fg: Color32, bg: Color32) -> Rect {
    let text = galley(ui, text, theme::semibold(12.0), fg);
    let rect = Rect::from_min_size(
        pos2(left_center.x, left_center.y - 12.0),
        vec2(text.size().x + 20.0, 24.0),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, theme::round_ends(rect.height()), bg);
    painter.galley(
        pos2(rect.left() + 10.0, rect.center().y - text.size().y / 2.0),
        text,
        fg,
    );
    rect
}

/// The height a progress bar takes in the theme's style. Callers reserve
/// it; [`progress_bar`] draws centered on the rect it is given.
pub fn progress_height() -> f32 {
    match theme::form().progress {
        ProgressStyle::Bar => 6.0,
        ProgressStyle::Chunky => 10.0,
        ProgressStyle::Knob => 14.0,
        ProgressStyle::Waveform => 18.0,
    }
}

const KNOB_TRACK_HEIGHT: f32 = 4.0;
const WAVE_BAR_WIDTH: f32 = 3.0;
const WAVE_GAP: f32 = 3.0;
/// Unplayed bars shrink to dots, as in an audio player.
const WAVE_DOT_HEIGHT: f32 = 3.0;

/// How far along `fraction` is, drawn across `rect` in the theme's style.
/// Each style is [`progress_height`] tall, centered on `rect`.
pub fn progress_bar(painter: &Painter, rect: Rect, fraction: f32, color: Color32) {
    let fraction = fraction.clamp(0.0, 1.0);
    let band = Rect::from_center_size(rect.center(), vec2(rect.width(), progress_height()));
    match theme::form().progress {
        ProgressStyle::Bar | ProgressStyle::Chunky => rounded_bar(painter, band, fraction, color),
        ProgressStyle::Knob => knob_bar(painter, band, fraction, color),
        ProgressStyle::Waveform => waveform(painter, band, fraction, color),
    }
}

fn rounded_bar(painter: &Painter, rect: Rect, fraction: f32, color: Color32) {
    let radius = theme::round_ends(rect.height());
    painter.rect_filled(rect, radius, theme::border());
    if fraction > 0.0 {
        let width = (rect.width() * fraction).max(rect.height());
        painter.rect_filled(
            Rect::from_min_size(rect.min, vec2(width, rect.height())),
            radius,
            color,
        );
    }
}

fn knob_bar(painter: &Painter, rect: Rect, fraction: f32, color: Color32) {
    let knob_radius = rect.height() / 2.0;
    // The knob's center travels between the track's rounded ends.
    let track = Rect::from_center_size(
        rect.center(),
        vec2(rect.width() - 2.0 * knob_radius, KNOB_TRACK_HEIGHT),
    );
    let round = theme::round_ends(KNOB_TRACK_HEIGHT);
    painter.rect_filled(track, round, theme::border());
    let knob_x = track.left() + track.width() * fraction;
    painter.rect_filled(
        Rect::from_x_y_ranges(track.left()..=knob_x, track.y_range()),
        round,
        color,
    );
    paint_knob(painter, pos2(knob_x, rect.center().y), knob_radius);
}

/// A round knob, raised on raised themes, like a slider's handle.
pub fn paint_knob(painter: &Painter, center: egui::Pos2, radius: f32) {
    let knob = Rect::from_center_size(center, egui::Vec2::splat(radius * 2.0));
    theme::paint_control(
        painter,
        knob,
        theme::round_ends(knob.height()),
        theme::surface(),
        Some(theme::border()),
    );
}

fn waveform(painter: &Painter, rect: Rect, fraction: f32, color: Color32) {
    let step = WAVE_BAR_WIDTH + WAVE_GAP;
    let count = ((rect.width() + WAVE_GAP) / step).floor().max(1.0) as usize;
    let played_until = rect.left() + rect.width() * fraction;
    let round = theme::round_ends(WAVE_BAR_WIDTH);
    for index in 0..count {
        let x = rect.left() + index as f32 * step;
        let (height, fill) = if x + WAVE_BAR_WIDTH / 2.0 <= played_until {
            (rect.height() * wave_height(index), color)
        } else {
            (WAVE_DOT_HEIGHT, theme::ink_faint())
        };
        let bar = Rect::from_center_size(
            pos2(x + WAVE_BAR_WIDTH / 2.0, rect.center().y),
            vec2(WAVE_BAR_WIDTH, height),
        );
        painter.rect_filled(bar, round, fill);
    }
}

/// The height of a waveform's `index`th bar, from 0.3 to 1 of the full
/// height: fixed per bar, so the wave stands still as progress moves.
fn wave_height(index: usize) -> f32 {
    let noise = ((index as f32 * 12.9898).sin() * 43_758.547).fract().abs();
    0.3 + 0.7 * noise
}

/// Map of which byte ranges are on disk, one block per connection segment.
pub fn segment_bar(painter: &Painter, rect: Rect, size: u64, segments: &[Segment]) {
    let radius = CornerRadius::same(4);
    painter.rect_filled(rect, radius, theme::border());
    if size == 0 {
        return;
    }
    let x_at = |offset: u64| rect.left() + rect.width() * (offset as f64 / size as f64) as f32;
    for segment in segments {
        let filled = Rect::from_x_y_ranges(
            x_at(segment.start)..=x_at(segment.write_position()),
            rect.y_range(),
        );
        if filled.width() > 0.0 {
            painter.rect_filled(filled, CornerRadius::ZERO, theme::primary());
        }
    }
    for segment in segments.iter().skip(1) {
        let x = x_at(segment.start);
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.5, theme::surface()),
        );
    }
    painter.rect_stroke(
        rect,
        radius,
        Stroke::new(1.0, theme::border()),
        StrokeKind::Inside,
    );
}

pub fn progress_ring(
    painter: &Painter,
    center: egui::Pos2,
    radius: f32,
    fraction: f32,
    color: Color32,
) {
    let width = 10.0;
    painter.circle_stroke(center, radius, Stroke::new(width, theme::primary_soft()));
    if fraction > 0.0 {
        let start = -std::f32::consts::FRAC_PI_2;
        let end = start + std::f32::consts::TAU * fraction.clamp(0.0, 1.0);
        let points = arc(center, radius, start, end, 96);
        painter.add(egui::Shape::line(points, Stroke::new(width, color)));
        for cap in [start, end] {
            painter.circle_filled(
                center + vec2(cap.cos(), cap.sin()) * radius,
                width / 2.0,
                color,
            );
        }
    }
}

/// Colored rounded square with the file's extension, like a document thumbnail.
pub fn file_tile(ui: &Ui, rect: Rect, badge: &str, fg: Color32, bg: Color32) {
    let painter = ui.painter();
    painter.rect_filled(rect, theme::control_radius(rect.height()), bg);
    let font = fitted_font(
        ui,
        badge,
        theme::bold,
        rect.height() * 0.3,
        8.0,
        rect.width() - 8.0,
    );
    painter.text(rect.center(), Align2::CENTER_CENTER, badge, font, fg);
}

/// A summary tile's color and its soft background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TileColors {
    pub accent: Color32,
    pub background: Color32,
}

impl TileColors {
    pub const fn new(accent: Color32, background: Color32) -> Self {
        Self { accent, background }
    }
}

/// A row of four solid tiles, left to right: the theme's solid blocks. A
/// solid tile is all accent, so its background is never drawn.
pub fn solid_tile_colors() -> [TileColors; 4] {
    theme::solid_blocks().map(|block| TileColors::new(block, block))
}

#[derive(Clone, Copy)]
pub struct StatTile<'a> {
    pub value: &'a str,
    pub label: &'a str,
    pub icon: Icon,
    pub colors: TileColors,
}

const STAT_TILE_HEIGHT: f32 = 64.0;
const STAT_TILE_GAP: f32 = 12.0;
/// Below this width the tiles wrap two to a row.
const STAT_ROW_WRAP_WIDTH: f32 = 620.0;

/// Summary tiles in one row (two per row when narrow), equal widths.
pub fn stat_tile_row(ui: &mut Ui, tiles: &[StatTile<'_>]) {
    let per_row = if ui.available_width() < STAT_ROW_WRAP_WIDTH {
        2
    } else {
        tiles.len().max(1)
    };
    let width = (ui.available_width() - STAT_TILE_GAP * (per_row - 1) as f32) / per_row as f32;
    for row in tiles.chunks(per_row) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = STAT_TILE_GAP;
            for tile in row {
                stat_tile(ui, width, *tile);
            }
        });
    }
}

/// A tile's corners: a little tighter than a card's.
fn tile_radius() -> CornerRadius {
    theme::inner_card_radius(2)
}

/// Compact card: icon badge on the left, the number and its label beside
/// it. The number shrinks to fit narrow tiles. The theme decides the fill:
/// the tile's pastel, the card surface, or the tile's color as a block.
fn stat_tile(ui: &mut Ui, width: f32, tile: StatTile<'_>) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, STAT_TILE_HEIGHT), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    let badge = Rect::from_center_size(
        pos2(rect.left() + 14.0 + 18.0, rect.center().y),
        vec2(36.0, 36.0),
    );
    let (value_color, label_color) = match theme::form().tiles {
        TileStyle::Tinted => {
            painter.rect_filled(rect, tile_radius(), tile.colors.background);
            painter.rect_filled(
                badge,
                theme::control_radius(badge.height()),
                theme::surface(),
            );
            tile.icon
                .paint(painter, badge.shrink(9.0), tile.colors.accent);
            (theme::ink(), theme::ink_soft())
        }
        TileStyle::Neutral => {
            theme::paint_surface(painter, rect, tile_radius(), theme::surface());
            painter.circle_filled(badge.center(), badge.width() / 2.0, tile.colors.background);
            tile.icon
                .paint(painter, badge.shrink(9.0), tile.colors.accent);
            (theme::ink(), theme::ink_soft())
        }
        TileStyle::Solid => {
            theme::paint_surface(painter, rect, tile_radius(), tile.colors.accent);
            let text = theme::ink_on(tile.colors.accent);
            painter.circle_filled(
                badge.center(),
                badge.width() / 2.0,
                text.gamma_multiply(0.18),
            );
            tile.icon.paint(painter, badge.shrink(9.0), text);
            (text, text.gamma_multiply(0.8))
        }
    };

    let text_left = badge.right() + 12.0;
    let text_width = rect.right() - 14.0 - text_left;
    let value_font = fitted_font(ui, tile.value, theme::bold, 20.0, 13.0, text_width);
    let value = galley(ui, tile.value, value_font, value_color);
    let label = elided(
        ui,
        tile.label,
        theme::regular(12.0),
        label_color,
        text_width,
    );
    let text_height = value.size().y + label.size().y;
    let top = rect.center().y - text_height / 2.0;
    let value_height = value.size().y;
    painter.galley(pos2(text_left, top), value, value_color);
    painter.galley(pos2(text_left, top + value_height), label, label_color);
}

/// The bar under an underlined navigation entry's label, and its gap below
/// the text.
const NAV_UNDERLINE_SIZE: egui::Vec2 = vec2(18.0, 3.0);
const NAV_UNDERLINE_GAP: f32 = 1.0;

/// The colors one navigation entry is drawn in.
struct EntryLook {
    text: Color32,
    icon: Color32,
    /// Under the label, for an underlined chosen entry.
    bar: Option<Color32>,
    /// The count's text and its badge.
    badge: (Color32, Color32),
}

/// One entry of a navigation list.
pub struct NavEntry<'a> {
    pub icon: Icon,
    pub icon_color: Color32,
    pub label: &'a str,
    pub count: Option<usize>,
    pub selected: bool,
}

/// Navigation entry with icon, label, and an optional count badge, in the
/// colors of the list it is in (see [`theme::sidebar_ink`]).
pub fn nav_item(ui: &mut Ui, ink: &NavInk, entry: NavEntry<'_>) -> Response {
    let NavEntry {
        icon,
        icon_color,
        label,
        count,
        selected,
    } = entry;
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), NAV_ITEM_HEIGHT), Sense::click());
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let radius = theme::control_radius(rect.height());
        // The chosen entry's fill, its colors, the bar under its label (drawn
        // once the label's size is known), and its count badge.
        let look = match (selected, ink.selection) {
            (true, NavSelection::Fill { fill, ink }) => {
                painter.rect_filled(rect, radius, fill);
                EntryLook {
                    text: ink,
                    icon: ink,
                    bar: None,
                    badge: (fill, ink),
                }
            }
            // A solid badge, as the entry itself has no fill to sit on.
            (true, NavSelection::Underline { bar, ink }) => EntryLook {
                text: ink,
                icon: ink,
                bar: Some(bar),
                badge: (theme::ink_on(bar), bar),
            },
            (false, _) => {
                if response.hovered() {
                    painter.rect_filled(rect, radius, ink.hover_fill);
                }
                EntryLook {
                    text: ink.ink_soft,
                    icon: icon_color,
                    bar: None,
                    badge: (ink.ink_soft, ink.badge_fill),
                }
            }
        };
        let icon_rect =
            Rect::from_center_size(pos2(rect.left() + 22.0, rect.center().y), vec2(17.0, 17.0));
        icon.paint(painter, icon_rect, look.icon);

        let count_text = count.map(|count| count.to_string());
        let badge_width = count_text
            .as_deref()
            .map_or(0.0, |text| pill_width(ui, text));
        let label_width = rect.width() - 44.0 - badge_width - 12.0;
        let font = if selected {
            theme::semibold(14.0)
        } else {
            theme::regular(14.0)
        };
        let text = elided(ui, label, font, look.text, label_width);
        let text_rect = Rect::from_min_size(
            pos2(rect.left() + 42.0, rect.center().y - text.size().y / 2.0),
            text.size(),
        );
        painter.galley(text_rect.min, text, look.text);
        if let Some(color) = look.bar {
            let bar = Rect::from_min_size(
                pos2(text_rect.left(), text_rect.bottom() + NAV_UNDERLINE_GAP),
                NAV_UNDERLINE_SIZE,
            );
            painter.rect_filled(bar, theme::round_ends(bar.height()), color);
        }

        if let Some(count_text) = count_text {
            let (fg, bg) = look.badge;
            pill(
                ui,
                pos2(rect.right() - badge_width - 8.0, rect.center().y),
                &count_text,
                fg,
                bg,
            );
        }
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A sidebar section's heading, in the sidebar's faint ink.
pub fn section_label(ui: &mut Ui, text: &str) {
    ui.add_space(6.0);
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .font(theme::semibold(11.5))
            .color(theme::sidebar_ink().ink_faint),
    );
    ui.add_space(2.0);
}

/// Sidebar section heading with a small icon button at its right edge.
pub fn section_label_with_action(ui: &mut Ui, text: &str, icon: Icon, tooltip: &str) -> Response {
    let ink = theme::sidebar_ink();
    ui.add_space(6.0);
    let height = 22.0;
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let label = galley(
        ui,
        text.to_uppercase(),
        theme::semibold(11.5),
        ink.ink_faint,
    );
    ui.painter().galley(
        pos2(rect.left(), rect.center().y - label.size().y / 2.0),
        label,
        ink.ink_faint,
    );
    let button = Rect::from_min_size(
        pos2(rect.right() - height, rect.top()),
        vec2(height, height),
    );
    let id = ui.id().with(("section-action", text));
    let response = ui.interact(button, id, Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(
            button,
            theme::control_radius(height),
            ink.icon_button.hover_fill,
        );
    }
    let color = if response.hovered() {
        ink.icon_button.hover_icon
    } else {
        ink.ink_faint
    };
    icon.paint(ui.painter(), button.shrink(5.0), color);
    ui.add_space(2.0);
    response
        .on_hover_text(tooltip)
        .on_hover_cursor(CursorIcon::PointingHand)
}

/// An on/off switch painted at `rect` inside a card drawn by hand. It takes
/// no layout space, so the card's own allocation decides what follows it.
/// Returns the clicked response so the caller flips the value.
pub fn toggle_switch_at(
    ui: &mut Ui,
    rect: Rect,
    id: egui::Id,
    on: bool,
    ink: SwitchInk,
) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    let progress = ui.ctx().animate_bool(response.id, on);
    paint_switch(ui.painter(), rect, progress, ink);
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// How far the knob sits inside the track's edge.
const SWITCH_KNOB_INSET: f32 = 2.5;

/// A switch `progress` of the way from off to on.
fn paint_switch(painter: &Painter, rect: Rect, progress: f32, ink: SwitchInk) {
    let round = rect.height() / 2.0;
    let track = ink.off_track.lerp_to_gamma(ink.on_track, progress);
    painter.rect_filled(rect, theme::round_ends(rect.height()), track);
    let knob_x = egui::lerp((rect.left() + round)..=(rect.right() - round), progress);
    let knob = ink.off_knob.lerp_to_gamma(ink.on_knob, progress);
    painter.circle_filled(
        pos2(knob_x, rect.center().y),
        round - SWITCH_KNOB_INSET,
        knob,
    );
}

/// Rounded single-line input with an optional leading icon.
pub fn text_field(
    ui: &mut Ui,
    value: &mut String,
    hint: &str,
    icon: Option<Icon>,
    width: f32,
) -> Response {
    let frame = field_frame().inner_margin(egui::Margin::symmetric(12, 8));
    frame
        .show(ui, |ui| {
            // A hidden or tiny window can leave less room than the padding.
            ui.set_width((width - 24.0).max(0.0));
            ui.horizontal(|ui| {
                if let Some(icon) = icon {
                    let (rect, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                    icon.paint(ui.painter(), rect, theme::ink_faint());
                }
                let edit = egui::TextEdit::singleline(value)
                    .hint_text(egui::RichText::new(hint).color(theme::ink_faint()))
                    .frame(false)
                    .font(theme::regular(14.0))
                    .desired_width(f32::INFINITY);
                ui.add(edit)
            })
            .inner
        })
        .inner
}

/// The box around a text field, in the theme's [`theme::field_look`].
pub fn field_frame() -> egui::Frame {
    let look = theme::field_look();
    let frame = egui::Frame::new()
        .corner_radius(theme::control_radius(FIELD_HEIGHT))
        .fill(look.fill);
    frame.stroke(look.stroke())
}

/// Small label above a form field.
pub fn field_label(ui: &mut Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .font(theme::semibold(13.0))
            .color(theme::ink_soft()),
    );
}

/// Height shared by text fields, dropdowns, and steppers.
pub const FIELD_HEIGHT: f32 = 38.0;

/// One option in a [`choice_list`].
pub struct Choice<'a, T> {
    pub value: T,
    pub title: &'a str,
    /// A short line under the title; empty for none.
    pub detail: &'a str,
}

/// Options as full-width rows with a radio mark; the selected row is tinted.
pub fn choice_list<T: PartialEq + Copy>(ui: &mut Ui, selected: &mut T, choices: &[Choice<'_, T>]) {
    for choice in choices {
        let title = galley(ui, choice.title, theme::semibold(13.5), theme::ink());
        let detail = (!choice.detail.is_empty())
            .then(|| galley(ui, choice.detail, theme::regular(12.5), theme::ink_soft()));
        let text_height = title.size().y + detail.as_ref().map_or(0.0, |d| d.size().y + 2.0);
        let height = (text_height + 20.0).max(FIELD_HEIGHT);
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
        if response.clicked() {
            *selected = choice.value;
        }
        let is_selected = *selected == choice.value;
        let painter = ui.painter();
        let control = theme::control_look();
        let (fill, outline) = match (is_selected, response.hovered()) {
            (true, _) => (theme::primary_soft(), Some(theme::primary())),
            (false, true) => (control.fill, Some(theme::ink_faint())),
            (false, false) => (control.fill, control.outline),
        };
        let radius = theme::control_radius(rect.height());
        theme::paint_control(painter, rect, radius, fill, outline);

        let mark = pos2(rect.left() + 22.0, rect.center().y);
        if is_selected {
            painter.circle_filled(mark, 8.0, theme::primary());
            painter.circle_filled(mark, 3.2, theme::on_primary());
        } else {
            painter.circle_stroke(mark, 7.5, Stroke::new(1.5, theme::ink_faint()));
        }
        let text_left = rect.left() + 42.0;
        let top = rect.center().y - text_height / 2.0;
        let title_height = title.size().y;
        painter.galley(pos2(text_left, top), title, theme::ink());
        if let Some(detail) = detail {
            painter.galley(
                pos2(text_left, top + title_height + 2.0),
                detail,
                theme::ink_soft(),
            );
        }
        response.on_hover_cursor(CursorIcon::PointingHand);
        ui.add_space(2.0);
    }
}

/// How far a segment sits inside its track.
const SEGMENT_INSET: f32 = 3.0;

/// A row of equal-width segments for a few short, exclusive choices.
pub fn segmented<T: PartialEq + Copy>(ui: &mut Ui, selected: &mut T, choices: &[(T, &str)]) {
    let (track, track_response) =
        ui.allocate_exact_size(vec2(ui.available_width(), FIELD_HEIGHT), Sense::hover());
    let radius = theme::control_radius(track.height());
    let control = theme::control_look();
    theme::paint_control(ui.painter(), track, radius, control.fill, control.outline);
    let width = track.width() / choices.len().max(1) as f32;
    for (index, (value, label)) in choices.iter().enumerate() {
        let rect = Rect::from_min_size(
            pos2(track.left() + width * index as f32, track.top()),
            vec2(width, track.height()),
        )
        .shrink(SEGMENT_INSET);
        let response = ui.interact(rect, track_response.id.with(index), Sense::click());
        if response.clicked() {
            *selected = *value;
        }
        let is_selected = *selected == *value;
        let (fill, text) = match (is_selected, response.hovered()) {
            (true, _) => (Some(theme::primary()), theme::on_primary()),
            (false, true) => (Some(theme::surface_muted()), theme::ink()),
            (false, false) => (None, theme::ink_soft()),
        };
        if let Some(fill) = fill {
            ui.painter().rect_filled(
                rect,
                theme::nested_control_radius(rect.height(), SEGMENT_INSET),
                fill,
            );
        }
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            *label,
            theme::semibold(13.0),
            text,
        );
        response.on_hover_cursor(CursorIcon::PointingHand);
    }
}

/// A setting that is on or off: title and detail on the left, a switch on
/// the right. The whole row toggles it.
pub fn toggle_row(ui: &mut Ui, on: &mut bool, title: &str, detail: &str) {
    let switch_width = 36.0;
    let text_width = ui.available_width() - switch_width - 16.0;
    let title_galley = galley(ui, title, theme::semibold(13.5), theme::ink());
    let detail_galley = (!detail.is_empty()).then(|| {
        let job = LayoutJob::simple(
            detail.to_owned(),
            theme::regular(12.5),
            theme::ink_soft(),
            text_width,
        );
        ui.fonts(|fonts| fonts.layout_job(job))
    });
    let title_height = title_galley.size().y;
    let text_height = title_height + detail_galley.as_ref().map_or(0.0, |d| d.size().y + 2.0);
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), text_height.max(24.0)),
        Sense::click(),
    );
    if response.clicked() {
        *on = !*on;
    }
    let painter = ui.painter();
    painter.galley(rect.left_top(), title_galley, theme::ink());
    if let Some(detail) = detail_galley {
        painter.galley(
            rect.left_top() + vec2(0.0, title_height + 2.0),
            detail,
            theme::ink_soft(),
        );
    }
    let switch = Rect::from_center_size(
        pos2(rect.right() - switch_width / 2.0, rect.center().y),
        vec2(switch_width, 20.0),
    );
    let progress = ui.ctx().animate_bool(response.id, *on);
    paint_switch(painter, switch, progress, SwitchInk::on_page());
    response.on_hover_cursor(CursorIcon::PointingHand);
}

/// A field-styled button that opens a list below it. `add_choices` fills the
/// list (usually with `selectable_value`); picking one closes it.
pub fn dropdown(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    width: f32,
    selected_text: &str,
    add_choices: impl FnOnce(&mut Ui),
) -> Response {
    let (rect, response) = ui.allocate_exact_size(vec2(width, FIELD_HEIGHT), Sense::click());
    let popup_id = ui.make_persistent_id(id);
    if response.clicked() {
        ui.memory_mut(|memory| memory.toggle_popup(popup_id));
    }
    let is_open = ui.memory(|memory| memory.is_popup_open(popup_id));
    let painter = ui.painter();
    let control = theme::control_look();
    let outline = if is_open || response.hovered() {
        Some(theme::primary())
    } else {
        control.outline
    };
    theme::paint_control(
        painter,
        rect,
        theme::control_radius(rect.height()),
        control.fill,
        outline,
    );
    let text = galley(ui, selected_text, theme::regular(14.0), theme::ink());
    let text_rect = Rect::from_min_max(
        pos2(rect.left() + 12.0, rect.top()),
        pos2(rect.right() - 36.0, rect.bottom()),
    );
    ui.painter().with_clip_rect(text_rect).galley(
        pos2(text_rect.left(), rect.center().y - text.size().y / 2.0),
        text,
        theme::ink(),
    );
    let chevron =
        Rect::from_center_size(pos2(rect.right() - 20.0, rect.center().y), vec2(14.0, 14.0));
    Icon::ChevronDown.paint(ui.painter(), chevron, theme::ink_soft());

    egui::popup::popup_below_widget(
        ui,
        popup_id,
        &response,
        egui::PopupCloseBehavior::CloseOnClick,
        |ui| {
            ui.set_min_width((width - 12.0).max(0.0));
            ui.spacing_mut().button_padding = vec2(10.0, 7.0);
            add_choices(ui);
        },
    );
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A whole number with − and + buttons around it; the value can also be
/// dragged or typed.
pub fn stepper(
    ui: &mut Ui,
    value: &mut u32,
    range: std::ops::RangeInclusive<u32>,
    step: u32,
    unit: &str,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let at_min = *value <= *range.start();
        let at_max = *value >= *range.end();
        let room = |at_limit: bool| {
            if at_limit {
                StepRoom::AtLimit
            } else {
                StepRoom::Available
            }
        };
        if step_button(ui, Icon::Minus, "Less", room(at_min)) {
            *value = value.saturating_sub(step).max(*range.start());
        }
        ui.scope(|ui| {
            let control = theme::control_look();
            let widgets = &mut ui.style_mut().visuals.widgets;
            for widget in [
                &mut widgets.inactive,
                &mut widgets.hovered,
                &mut widgets.active,
            ] {
                widget.weak_bg_fill = control.fill;
                widget.bg_fill = control.fill;
            }
            widgets.inactive.bg_stroke = control.stroke();
            ui.add_sized(
                vec2(84.0, FIELD_HEIGHT),
                egui::DragValue::new(value)
                    .range(range.clone())
                    .speed(f64::from(step) / 4.0),
            );
        });
        if step_button(ui, Icon::Plus, "More", room(at_max)) {
            *value = value.saturating_add(step).min(*range.end());
        }
        if !unit.is_empty() {
            ui.label(egui::RichText::new(unit).color(theme::ink_soft()));
        }
    });
}

/// Whether a stepper's value can still move in a button's direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepRoom {
    Available,
    AtLimit,
}

/// One of a stepper's buttons; returns whether it stepped.
fn step_button(ui: &mut Ui, icon: Icon, tooltip: &str, room: StepRoom) -> bool {
    let enabled = room == StepRoom::Available;
    let (rect, response) = ui.allocate_exact_size(vec2(FIELD_HEIGHT, FIELD_HEIGHT), Sense::click());
    let painter = ui.painter();
    let control = theme::control_look();
    let fill = if enabled && response.hovered() {
        theme::primary_soft()
    } else {
        control.fill
    };
    theme::paint_control(
        painter,
        rect,
        theme::control_radius(rect.height()),
        fill,
        control.outline,
    );
    let color = if enabled {
        theme::ink()
    } else {
        theme::ink_faint()
    };
    icon.paint(painter, rect.shrink(12.0), color);
    let response = response.on_hover_text(tooltip);
    enabled && response.clicked()
}
