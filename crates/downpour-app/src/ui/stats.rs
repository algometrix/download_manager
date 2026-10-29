//! The Statistics page: totals, highlights, downloads per day and per hour
//! of the day, and breakdowns by file type, category, and site, computed
//! from the download history. Charts grow in when the page opens.

use std::collections::{BTreeMap, HashMap};

use chrono::{Days, NaiveDate, NaiveDateTime, Timelike};
use downpour_core::history::CompletedDownload;
use eframe::egui::{self, pos2, vec2, Color32, CornerRadius, Rect, Sense, Ui};

use super::icons::Icon;
use super::live_speed::{Sample, SpeedHistory, SpeedSummary, WINDOW_SECONDS};
use super::theme::{ProgressStyle, Series, TileStyle};
use super::widgets::{
    button, elided, galley, paint_knob, progress_bar, progress_height, solid_tile_colors,
    stat_tile_row, ButtonKind, StatTile, TileColors,
};
use super::{format, theme, Action};

/// Days shown in the per-day chart, today included.
pub const CHART_DAYS: u64 = 30;
/// Rows per breakdown; the rest are folded into one "Other" row.
const BREAKDOWN_ROWS: usize = 8;
const HOURS_PER_DAY: usize = 24;
const CHART_HEIGHT: f32 = 180.0;
const LIVE_CHART_HEIGHT: f32 = 150.0;
const BAR_ROW_HEIGHT: f32 = 30.0;
/// How far a breakdown's plain track (the dashboard's) reaches above and
/// below its row's middle.
const BREAKDOWN_TRACK_HALF_HEIGHT: f32 = 4.0;
/// Columns take the controls' corners tightened by this much, so the
/// dashboard keeps its small 4-point tabs while pill themes get round tops.
const COLUMN_CORNER_INSET: f32 = 6.0;
/// Columns of a waveform chart, like the bars of an audio waveform.
const WAVE_COLUMN_WIDTH: f32 = 4.0;
/// Empty days on a waveform chart, as unplayed dots.
const WAVE_DOT_HEIGHT: f32 = 3.0;
/// How long one bar or number takes to grow in.
const INTRO_SECONDS: f32 = 0.6;
/// Each column of a chart starts this much after the one before it.
const COLUMN_STAGGER_SECONDS: f32 = 0.02;
/// Each breakdown row starts this much after the one before it.
const ROW_STAGGER_SECONDS: f32 = 0.05;
/// The highlight numbers start just after the headline numbers.
const HIGHLIGHT_DELAY_SECONDS: f32 = 0.1;
/// The breakdowns start once the headline numbers are under way.
const BREAKDOWN_DELAY_SECONDS: f32 = 0.2;
/// When column `index` of a chart starts to grow.
const fn column_delay(index: usize) -> f32 {
    index as f32 * COLUMN_STAGGER_SECONDS
}

/// When breakdown row `row` starts to fill.
const fn row_delay(row: usize) -> f32 {
    BREAKDOWN_DELAY_SECONDS + row as f32 * ROW_STAGGER_SECONDS
}

/// When the last thing to grow in has finished: the per-day chart's last
/// column or the last breakdown row, whichever starts later.
const INTRO_TOTAL_SECONDS: f32 = {
    let last_column = column_delay(CHART_DAYS as usize - 1);
    let last_row = row_delay(BREAKDOWN_ROWS - 1);
    let last_start = if last_column > last_row {
        last_column
    } else {
        last_row
    };
    last_start + INTRO_SECONDS
};
const HOVER_FADE_SECONDS: f32 = 0.15;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DayTotal {
    pub date: NaiveDate,
    pub bytes: u64,
    pub files: usize,
}

/// One row of a breakdown: a label and what it adds up to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Share {
    pub label: String,
    pub bytes: u64,
    pub files: usize,
}

/// What a group of finished downloads adds up to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Totals {
    bytes: u64,
    files: usize,
}

impl Totals {
    fn add(&mut self, bytes: u64) {
        self.bytes += bytes;
        self.files += 1;
    }
}

/// The biggest file ever finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LargestFile {
    pub name: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Statistics {
    pub total_bytes: u64,
    pub files: usize,
    pub videos_from_streams: usize,
    pub today_bytes: u64,
    pub last_week_bytes: u64,
    /// Bytes finished in the chart's `CHART_DAYS` days.
    pub period_bytes: u64,
    /// Bytes finished in the `CHART_DAYS` days before those.
    pub previous_period_bytes: u64,
    /// Oldest first, one entry per day, ending today.
    pub days: Vec<DayTotal>,
    /// Files finished in each hour of the day (local time), midnight first.
    pub by_hour: [usize; HOURS_PER_DAY],
    pub largest: Option<LargestFile>,
    /// The day with the most bytes, over the whole history.
    pub busiest_day: Option<DayTotal>,
    /// Days in a row with a finished download, up to today. A streak that
    /// ended yesterday still counts: today is not over yet.
    pub streak_days: u32,
    pub by_extension: Vec<Share>,
    pub by_category: Vec<Share>,
    pub by_site: Vec<Share>,
}

impl Statistics {
    pub fn average_file_bytes(&self) -> Option<u64> {
        (self.files > 0).then(|| self.total_bytes / self.files as u64)
    }

    /// How the chart's days compare with the days before them, in whole
    /// percent; `None` when there is nothing to compare with.
    pub fn period_change_percent(&self) -> Option<i64> {
        if self.previous_period_bytes == 0 {
            return None;
        }
        let change = (self.period_bytes as f64 - self.previous_period_bytes as f64) * 100.0
            / self.previous_period_bytes as f64;
        Some(change.round() as i64)
    }
}

/// Adds up the history. `local_time` turns a Unix time into the user's
/// wall-clock time, or `None` for one out of range; it is a parameter so
/// the arithmetic stays testable. An entry whose time cannot be read counts
/// in the totals and breakdowns, but on no day or hour.
pub fn summarize(
    entries: &[CompletedDownload],
    today: NaiveDate,
    local_time: impl Fn(u64) -> Option<NaiveDateTime>,
) -> Statistics {
    let first_day = today - Days::new(CHART_DAYS - 1);
    let previous_first_day = first_day - Days::new(CHART_DAYS);
    let week_start = today - Days::new(6);
    let mut statistics = Statistics::default();
    let mut per_day: BTreeMap<NaiveDate, Totals> = BTreeMap::new();
    let mut by_extension = Tally::default();
    let mut by_category = Tally::default();
    let mut by_site = Tally::default();

    for entry in entries {
        statistics.total_bytes += entry.bytes;
        statistics.files += 1;
        statistics.videos_from_streams += usize::from(entry.is_stream);
        if let Some(finished) = local_time(entry.finished_at_unix) {
            let date = finished.date();
            if date == today {
                statistics.today_bytes += entry.bytes;
            }
            if (week_start..=today).contains(&date) {
                statistics.last_week_bytes += entry.bytes;
            }
            if (first_day..=today).contains(&date) {
                statistics.period_bytes += entry.bytes;
            } else if (previous_first_day..first_day).contains(&date) {
                statistics.previous_period_bytes += entry.bytes;
            }
            per_day.entry(date).or_default().add(entry.bytes);
            statistics.by_hour[finished.hour() as usize] += 1;
        }
        if statistics
            .largest
            .as_ref()
            .is_none_or(|largest| entry.bytes > largest.bytes)
        {
            statistics.largest = Some(LargestFile {
                name: entry.file_name.clone(),
                bytes: entry.bytes,
            });
        }
        let extension = match entry.extension.as_str() {
            "" => "No extension".to_owned(),
            extension => format!(".{extension}"),
        };
        by_extension.add(extension, entry.bytes);
        by_category.add(entry.category.clone(), entry.bytes);
        by_site.add(display_host(&entry.host), entry.bytes);
    }
    statistics.days = (0..CHART_DAYS)
        .map(|offset| {
            let date = first_day + Days::new(offset);
            let Totals { bytes, files } = per_day.get(&date).copied().unwrap_or_default();
            DayTotal { date, bytes, files }
        })
        .collect();
    // Ties go to the earlier day, so the answer does not flicker.
    statistics.busiest_day = per_day
        .iter()
        .max_by(|(day_a, a), (day_b, b)| a.bytes.cmp(&b.bytes).then_with(|| day_b.cmp(day_a)))
        .map(|(date, totals)| DayTotal {
            date: *date,
            bytes: totals.bytes,
            files: totals.files,
        });
    statistics.streak_days = streak_ending(today, |date| per_day.contains_key(&date));
    statistics.by_extension = by_extension.ranked();
    statistics.by_category = by_category.ranked();
    statistics.by_site = by_site.ranked();
    statistics
}

/// Consecutive days with a download, counting back from today, or from
/// yesterday when nothing finished today yet.
fn streak_ending(today: NaiveDate, has_download: impl Fn(NaiveDate) -> bool) -> u32 {
    let mut day = if has_download(today) {
        today
    } else {
        today - Days::new(1)
    };
    let mut streak = 0;
    while has_download(day) {
        streak += 1;
        day = day - Days::new(1);
    }
    streak
}

fn display_host(host: &str) -> String {
    match host.strip_prefix("www.") {
        Some(rest) => rest.to_owned(),
        None if host.is_empty() => "Unknown site".to_owned(),
        None => host.to_owned(),
    }
}

#[derive(Default)]
struct Tally(HashMap<String, Totals>);

impl Tally {
    fn add(&mut self, label: String, bytes: u64) {
        self.0.entry(label).or_default().add(bytes);
    }

    /// Largest first; ties by name so the order is stable between frames.
    /// Rows past [`BREAKDOWN_ROWS`] are folded into "Other".
    fn ranked(self) -> Vec<Share> {
        let mut shares: Vec<Share> = self
            .0
            .into_iter()
            .map(|(label, Totals { bytes, files })| Share {
                label,
                bytes,
                files,
            })
            .collect();
        shares.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.label.cmp(&b.label)));
        if shares.len() > BREAKDOWN_ROWS {
            let rest = shares.split_off(BREAKDOWN_ROWS - 1);
            shares.push(Share {
                label: format!("{} others", rest.len()),
                bytes: rest.iter().map(|share| share.bytes).sum(),
                files: rest.iter().map(|share| share.files).sum(),
            });
        }
        shares
    }
}

/// "↑ 23% vs the 30 days before", or nothing without earlier days.
fn trend_text(statistics: &Statistics) -> String {
    match statistics.period_change_percent() {
        None => String::new(),
        Some(0) => format!(" · same as the {CHART_DAYS} days before"),
        Some(change) if change > 0 => format!(" · ↑ {change}% vs the {CHART_DAYS} days before"),
        Some(change) => format!(" · ↓ {}% vs the {CHART_DAYS} days before", -change),
    }
}

fn files_text(count: usize) -> String {
    match count {
        1 => "1 file".to_owned(),
        count => format!("{count} files"),
    }
}

/// How far the page's opening animation has come.
#[derive(Clone, Copy, Debug)]
pub struct Intro {
    elapsed: f32,
}

impl Intro {
    /// `opened_at` and `now` are egui input times, in seconds.
    pub fn since(opened_at: f64, now: f64) -> Self {
        Self {
            elapsed: (now - opened_at).max(0.0) as f32,
        }
    }

    /// 0 to 1, eased, for something that starts growing after `delay`.
    fn reveal(self, delay: f32) -> f32 {
        let t = ((self.elapsed - delay) / INTRO_SECONDS).clamp(0.0, 1.0);
        1.0 - (1.0 - t).powi(3)
    }

    /// Whether anything on the page is still growing.
    fn is_running(self) -> bool {
        self.elapsed < INTRO_TOTAL_SECONDS
    }
}

pub fn show(
    ui: &mut Ui,
    statistics: &Statistics,
    live: &SpeedHistory,
    intro: Intro,
    actions: &mut Vec<Action>,
) {
    if intro.is_running() {
        ui.ctx().request_repaint();
    }
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new("Statistics")
                    .font(theme::title(26.0))
                    .color(theme::ink()),
            );
            ui.label(
                egui::RichText::new(
                    "Everything Downpour has finished downloading, including removed entries.",
                )
                .font(theme::regular(13.0))
                .color(theme::ink_soft()),
            );
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if button(ui, ButtonKind::Secondary, Some(Icon::Refresh), "Refresh").clicked() {
                actions.push(Action::RefreshStatistics);
            }
        });
    });
    ui.add_space(14.0);

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            headline_tiles(ui, statistics, intro);
            ui.add_space(12.0);
            highlight_tiles(ui, statistics, intro);
            ui.add_space(18.0);
            let summary = live.summary();
            card(
                ui,
                "Live speed",
                &format!(
                    "Last 5 minutes · now {} · peak {} · average {}",
                    format::speed(summary.now),
                    format::speed(summary.peak),
                    format::speed(summary.average)
                ),
                |ui| live_chart(ui, live, &summary),
            );
            ui.add_space(18.0);
            card(
                ui,
                "Downloaded per day",
                &format!(
                    "Last {CHART_DAYS} days · {}{}",
                    format::bytes(statistics.period_bytes),
                    trend_text(statistics)
                ),
                |ui| daily_chart(ui, &statistics.days, intro),
            );
            ui.add_space(18.0);
            let gap = 18.0;
            let panels = [
                Panel::Breakdown("By file type", &statistics.by_extension),
                Panel::Hours(&statistics.by_hour),
                Panel::Breakdown("By category", &statistics.by_category),
                Panel::Breakdown("By site", &statistics.by_site),
            ];
            if ui.available_width() >= 760.0 {
                ui.spacing_mut().item_spacing.x = gap;
                // Each column is a vertical layout, so cards stack inside it.
                ui.columns(2, |columns| {
                    for (index, panel) in panels.iter().enumerate() {
                        let column = &mut columns[index % 2];
                        panel.show(column, intro);
                        column.add_space(gap);
                    }
                });
            } else {
                for panel in &panels {
                    panel.show(ui, intro);
                    ui.add_space(gap);
                }
            }
        });
}

/// A card in the grid below the charts.
enum Panel<'a> {
    Breakdown(&'a str, &'a [Share]),
    Hours(&'a [usize; HOURS_PER_DAY]),
}

impl Panel<'_> {
    fn show(&self, ui: &mut Ui, intro: Intro) {
        match self {
            Panel::Breakdown(title, shares) => {
                card(ui, title, "Share of bytes", |ui| {
                    breakdown(ui, shares, intro)
                });
            }
            Panel::Hours(hours) => {
                card(ui, "Time of day", "When downloads finish", |ui| {
                    hour_chart(ui, hours, intro);
                });
            }
        }
    }
}

/// Grows from zero to `value` with the intro.
fn counted(value: u64, progress: f32) -> u64 {
    (value as f64 * f64::from(progress)).round() as u64
}

fn headline_tiles(ui: &mut Ui, statistics: &Statistics, intro: Intro) {
    let progress = intro.reveal(0.0);
    let total = format::bytes(counted(statistics.total_bytes, progress));
    let files = counted(statistics.files as u64, progress).to_string();
    let files_label = match statistics.videos_from_streams {
        0 => "Files".to_owned(),
        1 => "Files · 1 video stream".to_owned(),
        videos => format!("Files · {videos} video streams"),
    };
    let week = format::bytes(counted(statistics.last_week_bytes, progress));
    let today = format::bytes(counted(statistics.today_bytes, progress));
    let [total_colors, files_colors, week_colors, today_colors] = headline_colors();
    let tiles = [
        StatTile {
            value: &total,
            label: "Downloaded in total",
            icon: Icon::Download,
            colors: total_colors,
        },
        StatTile {
            value: &files,
            label: &files_label,
            icon: Icon::File,
            colors: files_colors,
        },
        StatTile {
            value: &week,
            label: "Last 7 days",
            icon: Icon::Clock,
            colors: week_colors,
        },
        StatTile {
            value: &today,
            label: "Today",
            icon: Icon::Bolt,
            colors: today_colors,
        },
    ];
    stat_tile_row(ui, &tiles);
}

/// One row of four tiles, left to right.
type TileRowColors = [TileColors; 4];

fn headline_colors() -> TileRowColors {
    match theme::form().tiles {
        TileStyle::Solid => solid_tile_colors(),
        TileStyle::Tinted | TileStyle::Neutral => [
            TileColors::new(theme::primary(), theme::primary_soft()),
            TileColors::new(theme::accent(), theme::tile_peach()),
            TileColors::new(theme::success(), theme::tile_mint()),
            TileColors::new(theme::danger(), theme::tile_pink()),
        ],
    }
}

fn highlight_colors() -> TileRowColors {
    let soft = [
        TileColors::new(theme::primary(), theme::primary_soft()),
        TileColors::new(theme::accent(), theme::tile_peach()),
        TileColors::new(theme::success(), theme::success_soft()),
        TileColors::new(theme::warning(), theme::warning_soft()),
    ];
    match theme::form().tiles {
        // Plain white tiles, quieter than the tinted row above them.
        TileStyle::Tinted => soft.map(|colors| TileColors::new(colors.accent, theme::surface())),
        // Neutral tiles show the background only on the icon's badge.
        TileStyle::Neutral => soft,
        // Reversed, so no block sits under one of its own color.
        TileStyle::Solid => {
            let mut blocks = solid_tile_colors();
            blocks.reverse();
            blocks
        }
    }
}

/// Records and habits: average and largest file, busiest day, streak.
fn highlight_tiles(ui: &mut Ui, statistics: &Statistics, intro: Intro) {
    let progress = intro.reveal(HIGHLIGHT_DELAY_SECONDS);
    let none = || "—".to_owned();
    let average = statistics
        .average_file_bytes()
        .map_or_else(none, |bytes| format::bytes(counted(bytes, progress)));
    let (largest, largest_label) = match &statistics.largest {
        Some(file) => (
            format::bytes(counted(file.bytes, progress)),
            format!("Largest · {}", file.name),
        ),
        None => (none(), "Largest file".to_owned()),
    };
    let (busiest, busiest_label) = match &statistics.busiest_day {
        Some(day) => (
            format::bytes(counted(day.bytes, progress)),
            format!("Busiest day · {}", day.date.format("%b %-d, %Y")),
        ),
        None => (none(), "Busiest day".to_owned()),
    };
    let streak = match counted(u64::from(statistics.streak_days), progress) {
        1 => "1 day".to_owned(),
        days => format!("{days} days"),
    };
    let [average_colors, largest_colors, busiest_colors, streak_colors] = highlight_colors();
    let tiles = [
        StatTile {
            value: &average,
            label: "Average file",
            icon: Icon::Layers,
            colors: average_colors,
        },
        StatTile {
            value: &largest,
            label: &largest_label,
            icon: Icon::Archive,
            colors: largest_colors,
        },
        StatTile {
            value: &busiest,
            label: &busiest_label,
            icon: Icon::Chart,
            colors: busiest_colors,
        },
        StatTile {
            value: &streak,
            label: "Download streak",
            icon: Icon::Bolt,
            colors: streak_colors,
        },
    ];
    stat_tile_row(ui, &tiles);
}

fn card(ui: &mut Ui, title: &str, subtitle: &str, body: impl FnOnce(&mut Ui)) {
    theme::card(ui, egui::Margin::same(18), |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(title)
                    .font(theme::semibold(15.0))
                    .color(theme::ink()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(subtitle)
                        .font(theme::regular(12.0))
                        .color(theme::ink_faint()),
                );
            });
        });
        ui.add_space(10.0);
        body(ui);
    });
}

/// One bar of a column chart.
struct Column {
    value: u64,
    /// Written under the bar; most bars have none.
    label: Option<String>,
    /// Shown while the bar is hovered.
    tooltip: String,
}

/// What a column chart is about, for its labels and hover fades.
struct ChartText<'a> {
    id: &'a str,
    axis_value: fn(u64) -> String,
    empty: &'a str,
    series: Series,
}

fn daily_chart(ui: &mut Ui, days: &[DayTotal], intro: Intro) {
    let last = days.len().saturating_sub(1);
    let columns: Vec<Column> = days
        .iter()
        .enumerate()
        .map(|(index, day)| Column {
            value: day.bytes,
            // The first day, every seventh day, and today; a weekly label
            // too close to today's would overlap it.
            label: (index == 0 || index == last || (index % 7 == 0 && last - index >= 4))
                .then(|| day.date.format("%b %-d").to_string()),
            tooltip: format!(
                "{}\n{} · {}",
                day.date.format("%A, %B %-d"),
                format::bytes(day.bytes),
                files_text(day.files)
            ),
        })
        .collect();
    let text = ChartText {
        id: "downpour-stats-days",
        axis_value: format::bytes,
        empty: "Nothing finished in this period yet",
        series: Series::Main,
    };
    column_chart(ui, &columns, &text, intro);
}

fn hour_chart(ui: &mut Ui, hours: &[usize; HOURS_PER_DAY], intro: Intro) {
    let columns: Vec<Column> = hours
        .iter()
        .enumerate()
        .map(|(hour, &files)| Column {
            value: files as u64,
            label: (hour % 6 == 0).then(|| format!("{hour:02}:00")),
            tooltip: format!(
                "{hour:02}:00 to {:02}:00\n{}",
                (hour + 1) % HOURS_PER_DAY,
                files_text(files)
            ),
        })
        .collect();
    let text = ChartText {
        id: "downpour-stats-hours",
        axis_value: |files| files_text(files as usize),
        empty: "No finished downloads yet",
        // Beside the breakdowns, in the second series' color.
        series: Series::Second,
    };
    column_chart(ui, &columns, &text, intro);
}

/// How a column chart draws its columns, following the theme's form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColumnLook {
    /// As wide as the slot allows, with the controls' corners on top.
    Blocks,
    /// Thin round-ended bars like the waveform progress, empty slots as dots.
    Waveform,
}

impl ColumnLook {
    fn of_theme() -> Self {
        match theme::form().progress {
            ProgressStyle::Waveform => ColumnLook::Waveform,
            ProgressStyle::Bar | ProgressStyle::Chunky | ProgressStyle::Knob => ColumnLook::Blocks,
        }
    }

    fn bar_width(self, slot: f32) -> f32 {
        match self {
            ColumnLook::Blocks => (slot - 2.0).clamp(2.0, 18.0),
            ColumnLook::Waveform => WAVE_COLUMN_WIDTH.min(slot - 1.0).max(1.0),
        }
    }

    fn corners(self, bar_width: f32) -> CornerRadius {
        match self {
            ColumnLook::Blocks => CornerRadius {
                sw: 0,
                se: 0,
                ..theme::nested_control_radius(bar_width, COLUMN_CORNER_INSET)
            },
            ColumnLook::Waveform => theme::round_ends(bar_width),
        }
    }

    /// The shortest a column with any value is drawn, so it stays visible
    /// (and, on a waveform, round at both ends).
    fn least_height(self, bar_width: f32) -> f32 {
        match self {
            ColumnLook::Blocks => 2.0,
            ColumnLook::Waveform => bar_width,
        }
    }
}

/// Thin bars anchored to a recessive baseline, with two gridlines. Bars
/// grow in one after another; a hovered bar lights up and shows its tooltip.
fn column_chart(ui: &mut Ui, columns: &[Column], text: &ChartText<'_>, intro: Intro) {
    let label_height = 18.0;
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), CHART_HEIGHT + label_height),
        Sense::hover(),
    );
    let plot = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - label_height));
    let painter = ui.painter();
    let peak = columns.iter().map(|column| column.value).max().unwrap_or(0);

    let label_width = 64.0;
    let bars = Rect::from_min_max(pos2(plot.left() + label_width, plot.top()), plot.max);
    for fraction in [0.5f32, 1.0] {
        let y = bars.bottom() - bars.height() * fraction;
        painter.line_segment(
            [pos2(bars.left(), y), pos2(bars.right(), y)],
            egui::Stroke::new(1.0, theme::border()),
        );
        if peak > 0 {
            let value = (text.axis_value)((peak as f64 * f64::from(fraction)) as u64);
            let label = galley(ui, value, theme::regular(11.0), theme::ink_faint());
            painter.galley(
                pos2(plot.left(), y - label.size().y / 2.0),
                label,
                theme::ink_faint(),
            );
        }
    }
    let look = ColumnLook::of_theme();
    // A waveform's dots stand in for the baseline.
    if look == ColumnLook::Blocks {
        painter.line_segment(
            [
                pos2(bars.left(), bars.bottom()),
                pos2(bars.right(), bars.bottom()),
            ],
            egui::Stroke::new(1.0, theme::ink_faint()),
        );
    }

    let slot = bars.width() / columns.len().max(1) as f32;
    let bar_width = look.bar_width(slot);
    let corners = look.corners(bar_width);
    let ink = theme::series_ink(text.series);
    let hovered = response
        .hover_pos()
        .filter(|pos| bars.x_range().contains(pos.x))
        .map(|pos| (((pos.x - bars.left()) / slot) as usize).min(columns.len().saturating_sub(1)));
    for (index, column) in columns.iter().enumerate() {
        let center = bars.left() + slot * (index as f32 + 0.5);
        let grown = intro.reveal(column_delay(index));
        if column.value == 0 && look == ColumnLook::Waveform && grown > 0.0 {
            let dot = Rect::from_center_size(
                pos2(center, bars.bottom() - WAVE_DOT_HEIGHT / 2.0),
                vec2(bar_width, WAVE_DOT_HEIGHT),
            );
            painter.rect_filled(dot, corners, theme::ink_faint().gamma_multiply(grown));
        }
        if column.value > 0 && peak > 0 && grown > 0.0 {
            let full = (bars.height() * column.value as f32 / peak as f32)
                .max(look.least_height(bar_width));
            let height = full * grown;
            let bar = Rect::from_min_max(
                pos2(center - bar_width / 2.0, bars.bottom() - height),
                pos2(center + bar_width / 2.0, bars.bottom()),
            );
            let lit = ui.ctx().animate_bool_with_time(
                egui::Id::new((text.id, index)),
                hovered == Some(index),
                HOVER_FADE_SECONDS,
            );
            let color = ink.color.lerp_to_gamma(ink.lit, lit);
            painter.rect_filled(bar, corners, color);
        }
        if let Some(label) = &column.label {
            let label = galley(ui, label, theme::regular(11.0), theme::ink_faint());
            let x =
                (center - label.size().x / 2.0).clamp(bars.left(), bars.right() - label.size().x);
            painter.galley(pos2(x, plot.bottom() + 4.0), label, theme::ink_faint());
        }
    }
    if peak == 0 {
        let message = galley(ui, text.empty, theme::regular(13.0), theme::ink_soft());
        painter.galley(
            bars.center() - message.size() / 2.0,
            message,
            theme::ink_soft(),
        );
    }
    if let Some(column) = hovered.and_then(|index| columns.get(index)) {
        response.on_hover_text(&column.tooltip);
    }
}

/// Horizontal bars, longest first; labels and values stay in ink. Rows
/// fill in one after another, alternating between the chart series' colors
/// (which on most themes are one color).
fn breakdown(ui: &mut Ui, shares: &[Share], intro: Intro) {
    if shares.is_empty() {
        ui.label(
            egui::RichText::new("No finished downloads yet.")
                .font(theme::regular(13.0))
                .color(theme::ink_soft()),
        );
        return;
    }
    let total: u64 = shares.iter().map(|share| share.bytes).sum::<u64>().max(1);
    let largest = shares[0].bytes.max(1);
    for (row, share) in shares.iter().enumerate() {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), BAR_ROW_HEIGHT), Sense::hover());
        let grown = intro.reveal(row_delay(row));
        let painter = ui.painter();
        let percent = share.bytes as f64 * 100.0 / total as f64;
        let value = galley(
            ui,
            format!("{} · {percent:.0}%", format::bytes(share.bytes)),
            theme::regular(12.0),
            theme::ink_soft(),
        );
        let label_width = (rect.width() * 0.34).min(150.0);
        let label = elided(
            ui,
            &share.label,
            theme::semibold(12.5),
            theme::ink(),
            label_width - 8.0,
        );
        let middle = rect.center().y;
        painter.galley(
            pos2(rect.left(), middle - label.size().y / 2.0),
            label,
            theme::ink(),
        );
        painter.galley(
            pos2(rect.right() - value.size().x, middle - value.size().y / 2.0),
            value.clone(),
            theme::ink_soft(),
        );
        let track = Rect::from_min_max(
            pos2(
                rect.left() + label_width,
                middle - BREAKDOWN_TRACK_HALF_HEIGHT,
            ),
            pos2(
                rect.right() - value.size().x - 12.0,
                middle + BREAKDOWN_TRACK_HALF_HEIGHT,
            ),
        );
        if track.width() > 8.0 {
            let lit = ui.ctx().animate_bool_with_time(
                response.id,
                response.hovered(),
                HOVER_FADE_SECONDS,
            );
            let ink = theme::series_ink(breakdown_series(row));
            let color = ink.color.lerp_to_gamma(ink.lit, lit);
            let fraction = share.bytes as f32 / largest as f32 * grown;
            share_bar(painter, track, fraction, color);
        }
        response.on_hover_text(format!(
            "{}: {} in {}",
            share.label,
            format::bytes(share.bytes),
            files_text(share.files)
        ));
    }
}

/// The series of the `row`th breakdown bar: the first row is the main one.
fn breakdown_series(row: usize) -> Series {
    if row % 2 == 0 {
        Series::Main
    } else {
        Series::Second
    }
}

/// One breakdown row's bar across `track`, `fraction` of the way. Chunky and
/// knob themes use their progress bar, so shares read like everything else
/// that fills. The dashboard keeps its soft pill track, and so does the
/// waveform: its bars vary in height for looks, which here would read as data.
fn share_bar(painter: &egui::Painter, track: Rect, fraction: f32, color: Color32) {
    match theme::form().progress {
        ProgressStyle::Chunky | ProgressStyle::Knob => {
            progress_bar(painter, track, fraction, color);
        }
        ProgressStyle::Bar | ProgressStyle::Waveform => {
            let round = theme::round_ends(track.height());
            painter.rect_filled(track, round, theme::surface_muted());
            let filled = track.width() * fraction;
            if filled > 0.0 {
                let bar = Rect::from_min_size(
                    track.min,
                    vec2(filled.max(track.height() / 2.0), track.height()),
                );
                painter.rect_filled(bar, round, color);
            }
        }
    }
}

/// How far the newest point's pulse ring swells past the point.
const PULSE_GROWTH: f32 = 8.0;
/// The colored center of a knob-style point.
const KNOB_CORE_RADIUS: f32 = 3.0;

/// The live chart's line and points, in the weight of the theme's progress.
struct LiveMarks {
    line_width: f32,
    point: LivePoint,
}

/// A point on the live chart: the newest sample or the hovered one.
#[derive(Clone, Copy)]
enum LivePoint {
    Dot {
        radius: f32,
    },
    /// The progress bar's raised knob, with a dot of the line's color.
    Knob,
}

impl LiveMarks {
    fn of_theme() -> Self {
        match theme::form().progress {
            ProgressStyle::Bar | ProgressStyle::Waveform => LiveMarks {
                line_width: 2.0,
                point: LivePoint::Dot { radius: 4.0 },
            },
            ProgressStyle::Chunky => LiveMarks {
                line_width: 3.0,
                point: LivePoint::Dot { radius: 5.0 },
            },
            ProgressStyle::Knob => LiveMarks {
                line_width: 2.0,
                point: LivePoint::Knob,
            },
        }
    }
}

impl LivePoint {
    fn radius(self) -> f32 {
        match self {
            LivePoint::Dot { radius } => radius,
            // Knob themes reserve the knob's full size as their progress height.
            LivePoint::Knob => progress_height() / 2.0,
        }
    }

    fn paint(self, painter: &egui::Painter, at: egui::Pos2) {
        match self {
            LivePoint::Dot { radius } => {
                painter.circle_filled(at, radius, theme::primary());
            }
            LivePoint::Knob => {
                paint_knob(painter, at, self.radius());
                painter.circle_filled(at, KNOB_CORE_RADIUS, theme::primary());
            }
        }
    }
}

/// Total speed over the last few minutes, newest at the right, as a line
/// over a soft fill. While something downloads, the newest point pulses.
/// Hovering shows the speed at that second.
fn live_chart(ui: &mut Ui, history: &SpeedHistory, summary: &SpeedSummary) {
    let samples = history.samples();
    let label_height = 18.0;
    let (rect, response) = ui.allocate_exact_size(
        vec2(ui.available_width(), LIVE_CHART_HEIGHT + label_height),
        Sense::hover(),
    );
    let plot = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - label_height));
    let painter = ui.painter();
    let peak = summary.peak;
    let marks = LiveMarks::of_theme();

    let label_width = 64.0;
    let area = Rect::from_min_max(pos2(plot.left() + label_width, plot.top()), plot.max);
    for fraction in [0.5f32, 1.0] {
        let y = area.bottom() - area.height() * fraction;
        painter.line_segment(
            [pos2(area.left(), y), pos2(area.right(), y)],
            egui::Stroke::new(1.0, theme::border()),
        );
        if peak > 0 {
            let value = format::speed((peak as f64 * f64::from(fraction)) as u64);
            let text = galley(ui, value, theme::regular(11.0), theme::ink_faint());
            painter.galley(
                pos2(plot.left(), y - text.size().y / 2.0),
                text,
                theme::ink_faint(),
            );
        }
    }
    painter.line_segment(
        [
            pos2(area.left(), area.bottom()),
            pos2(area.right(), area.bottom()),
        ],
        egui::Stroke::new(1.0, theme::ink_faint()),
    );
    for (text, align_right) in [("5 min ago", false), ("now", true)] {
        let text = galley(ui, text, theme::regular(11.0), theme::ink_faint());
        let x = if align_right {
            area.right() - text.size().x
        } else {
            area.left()
        };
        painter.galley(pos2(x, plot.bottom() + 4.0), text, theme::ink_faint());
    }

    // One slot per second of the window; samples fill it from the right.
    let step = area.width() / (WINDOW_SECONDS.saturating_sub(1)).max(1) as f32;
    let first_slot = WINDOW_SECONDS - samples.len();
    let point = |index: usize, sample: &Sample| {
        let fraction = if peak == 0 {
            0.0
        } else {
            sample.bytes_per_second as f32 / peak as f32
        };
        pos2(
            area.left() + step * (first_slot + index) as f32,
            area.bottom() - area.height() * fraction,
        )
    };
    let line: Vec<egui::Pos2> = samples
        .iter()
        .enumerate()
        .map(|(i, s)| point(i, s))
        .collect();
    if line.len() >= 2 {
        // The fill is drawn as thin vertical quads: egui fills convex shapes
        // only, and the area under a speed curve is rarely convex.
        let fill = theme::primary().gamma_multiply(0.18);
        for pair in line.windows(2) {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    pair[0],
                    pair[1],
                    pos2(pair[1].x, area.bottom()),
                    pos2(pair[0].x, area.bottom()),
                ],
                fill,
                egui::Stroke::NONE,
            ));
        }
        painter.add(egui::Shape::line(
            line.clone(),
            egui::Stroke::new(marks.line_width, theme::primary()),
        ));
    }
    if let Some(&newest) = line.last().filter(|_| summary.now > 0) {
        // A soft ring that swells and fades once a second.
        let phase = (ui.input(|input| input.time) % 1.0) as f32;
        painter.circle_filled(
            newest,
            marks.point.radius() + PULSE_GROWTH * phase,
            theme::primary().gamma_multiply(0.35 * (1.0 - phase)),
        );
        marks.point.paint(painter, newest);
        ui.ctx().request_repaint();
    }
    if peak == 0 {
        let message = if samples.is_empty() {
            "Measuring…"
        } else {
            "Nothing is downloading right now"
        };
        let text = galley(ui, message, theme::regular(13.0), theme::ink_soft());
        painter.galley(area.center() - text.size() / 2.0, text, theme::ink_soft());
    }

    let hovered = response
        .hover_pos()
        .filter(|pos| area.x_range().contains(pos.x))
        .and_then(|pos| {
            let slot = ((pos.x - area.left()) / step).round() as usize;
            let index = slot.checked_sub(first_slot)?;
            samples.get(index).map(|sample| (index, *sample))
        });
    if let Some((index, sample)) = hovered {
        let at = point(index, &sample);
        painter.line_segment(
            [pos2(at.x, area.top()), pos2(at.x, area.bottom())],
            egui::Stroke::new(1.0, theme::border()),
        );
        marks.point.paint(painter, at);
        let seconds_ago = samples.len() - 1 - index;
        let when = match seconds_ago {
            0 => "Now".to_owned(),
            seconds => format!("{seconds} s ago"),
        };
        let running = match sample.active {
            1 => "1 download running".to_owned(),
            count => format!("{count} downloads running"),
        };
        response.on_hover_text(format!(
            "{when}\n{} · {running}",
            format::speed(sample.bytes_per_second)
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;
    const HOUR: u64 = 3_600;

    fn entry_at(unix: u64, bytes: u64, extension: &str, host: &str) -> CompletedDownload {
        CompletedDownload {
            finished_at_unix: unix,
            bytes,
            file_name: format!("f{bytes}.{extension}"),
            extension: extension.to_owned(),
            category: if extension == "mp4" {
                "Video".into()
            } else {
                "Other".into()
            },
            host: host.to_owned(),
            is_stream: extension == "mp4",
        }
    }

    fn entry(day: u64, bytes: u64, extension: &str, host: &str) -> CompletedDownload {
        entry_at(day * DAY + HOUR, bytes, extension, host)
    }

    fn utc_time(unix: u64) -> Option<NaiveDateTime> {
        chrono::DateTime::from_timestamp(unix as i64, 0).map(|time| time.naive_utc())
    }

    fn utc_date(unix: u64) -> NaiveDate {
        utc_time(unix).unwrap().date()
    }

    #[test]
    fn totals_per_day_week_and_breakdowns() {
        let today_index = 20_000;
        let today = utc_date(today_index * DAY);
        let entries = [
            entry(today_index, 100, "mp4", "www.video.example"),
            entry(today_index, 50, "zip", "files.example"),
            entry(today_index - 3, 30, "mp4", "video.example"),
            entry(today_index - 40, 1_000, "iso", "files.example"),
        ];
        let statistics = summarize(&entries, today, utc_time);
        assert_eq!(statistics.total_bytes, 1_180);
        assert_eq!(statistics.files, 4);
        assert_eq!(statistics.videos_from_streams, 2);
        assert_eq!(statistics.today_bytes, 150);
        assert_eq!(statistics.last_week_bytes, 180);
        assert_eq!(statistics.days.len(), CHART_DAYS as usize);
        assert_eq!(
            statistics.days.last().unwrap(),
            &DayTotal {
                date: today,
                bytes: 150,
                files: 2
            }
        );
        // The 40-day-old download counts in totals but not in the chart.
        assert_eq!(
            statistics.days.iter().map(|day| day.bytes).sum::<u64>(),
            180
        );
        assert_eq!(statistics.by_extension[0].label, ".iso");
        assert_eq!(
            statistics.by_site[0],
            Share {
                label: "files.example".into(),
                bytes: 1_050,
                files: 2
            }
        );
        assert_eq!(statistics.by_site[1].label, "video.example");
    }

    #[test]
    fn records_and_the_comparison_with_the_days_before() {
        let today_index = 20_000;
        let today = utc_date(today_index * DAY);
        let entries = [
            entry(today_index, 300, "zip", "a"),
            entry(today_index - 1, 100, "zip", "a"),
            entry(today_index - 1, 250, "iso", "a"),
            // In the 30 days before the chart.
            entry(today_index - 45, 500, "zip", "a"),
        ];
        let statistics = summarize(&entries, today, utc_time);
        assert_eq!(statistics.average_file_bytes(), Some(1_150 / 4));
        assert_eq!(
            statistics.largest,
            Some(LargestFile {
                name: "f500.zip".into(),
                bytes: 500
            })
        );
        assert_eq!(
            statistics.busiest_day,
            Some(DayTotal {
                date: utc_date((today_index - 45) * DAY),
                bytes: 500,
                files: 1
            })
        );
        assert_eq!(statistics.period_bytes, 650);
        assert_eq!(statistics.previous_period_bytes, 500);
        assert_eq!(statistics.period_change_percent(), Some(30));
        assert_eq!(trend_text(&statistics), " · ↑ 30% vs the 30 days before");
        assert_eq!(statistics.streak_days, 2);
    }

    #[test]
    fn a_streak_that_ended_yesterday_still_counts() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        let days = [
            today - Days::new(1),
            today - Days::new(2),
            today - Days::new(4),
        ];
        assert_eq!(streak_ending(today, |day| days.contains(&day)), 2);
        assert_eq!(streak_ending(today, |_| false), 0);
    }

    #[test]
    fn downloads_are_counted_in_the_hour_they_finished() {
        let entries = [
            entry_at(14 * HOUR + 5, 1, "zip", "a"),
            entry_at(DAY + 14 * HOUR + 59 * 60, 1, "zip", "a"),
            entry_at(23 * HOUR, 1, "zip", "a"),
        ];
        let statistics = summarize(&entries, utc_date(DAY), utc_time);
        assert_eq!(statistics.by_hour[14], 2);
        assert_eq!(statistics.by_hour[23], 1);
        assert_eq!(statistics.by_hour.iter().sum::<usize>(), 3);
    }

    #[test]
    fn ties_for_the_busiest_day_go_to_the_earlier_day() {
        let entries = [entry(10, 500, "zip", "a"), entry(12, 500, "zip", "a")];
        let statistics = summarize(&entries, utc_date(12 * DAY), utc_time);
        assert_eq!(statistics.busiest_day.unwrap().date, utc_date(10 * DAY));
    }

    #[test]
    fn the_period_before_the_chart_has_exact_edges() {
        let today_index = 20_000;
        let today = utc_date(today_index * DAY);
        let chart_first = today_index - (CHART_DAYS - 1);
        let entries = [
            // The day before the chart, and the first of the 30 before it.
            entry(chart_first - 1, 10, "zip", "a"),
            entry(chart_first - CHART_DAYS, 20, "zip", "a"),
            // One day further back is outside both periods.
            entry(chart_first - CHART_DAYS - 1, 400, "zip", "a"),
            entry(chart_first, 1, "zip", "a"),
        ];
        let statistics = summarize(&entries, today, utc_time);
        assert_eq!(statistics.previous_period_bytes, 30);
        assert_eq!(statistics.period_bytes, 1);
    }

    #[test]
    fn an_unreadable_time_counts_in_totals_but_on_no_day() {
        const UNREADABLE: u64 = 7;
        let today = utc_date(20_000 * DAY);
        let entries = [
            entry(20_000, 100, "zip", "a"),
            entry_at(UNREADABLE, 50, "iso", "a"),
        ];
        let local_time = |unix: u64| (unix != UNREADABLE).then(|| utc_time(unix)).flatten();
        let statistics = summarize(&entries, today, local_time);
        assert_eq!(statistics.total_bytes, 150);
        assert_eq!(statistics.files, 2);
        assert_eq!(statistics.today_bytes, 100);
        assert_eq!(statistics.by_hour.iter().sum::<usize>(), 1);
        assert_eq!(statistics.by_extension.len(), 2);
    }

    #[test]
    fn nothing_to_compare_with_shows_no_trend() {
        let statistics = Statistics::default();
        assert_eq!(statistics.period_change_percent(), None);
        assert_eq!(trend_text(&statistics), "");
        assert_eq!(statistics.average_file_bytes(), None);
    }

    #[test]
    fn the_intro_eases_in_and_ends() {
        let start = Intro::since(10.0, 10.0);
        assert_eq!(start.reveal(0.0), 0.0);
        assert!(start.is_running());
        let halfway = Intro::since(10.0, 10.3);
        assert!(
            halfway.reveal(0.0) > 0.5,
            "ease-out is past half at half time"
        );
        assert_eq!(halfway.reveal(0.5), 0.0, "a later bar has not started");
        let done = Intro::since(10.0, 20.0);
        assert_eq!(done.reveal(0.5), 1.0);
        assert!(!done.is_running());
    }

    #[test]
    fn everything_has_grown_in_when_the_intro_ends() {
        let end = Intro {
            elapsed: INTRO_TOTAL_SECONDS,
        };
        assert_eq!(end.reveal(column_delay(CHART_DAYS as usize - 1)), 1.0);
        assert_eq!(end.reveal(row_delay(BREAKDOWN_ROWS - 1)), 1.0);
        assert_eq!(end.reveal(HIGHLIGHT_DELAY_SECONDS), 1.0);
        assert!(!end.is_running());
    }

    #[test]
    fn long_breakdowns_fold_into_others() {
        let today = utc_date(0);
        let entries: Vec<_> = (0..12)
            .map(|index| entry(0, 100 - index, &format!("e{index}"), "x"))
            .collect();
        let statistics = summarize(&entries, today, utc_time);
        assert_eq!(statistics.by_extension.len(), BREAKDOWN_ROWS);
        let others = statistics.by_extension.last().unwrap();
        assert_eq!(others.label, "5 others");
        assert_eq!(others.files, 5);
    }
}
