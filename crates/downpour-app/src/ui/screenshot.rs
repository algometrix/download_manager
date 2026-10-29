//! Development aid, compiled into debug builds only, for reviewing the UI
//! where the window cannot be captured from the screen or clicked.
//!
//! With `DOWNPOUR_SCREENSHOT=<file>` set, writing `<file>.request` makes the app
//! run the commands in it (one per line), wait for the layout to settle, and
//! save the frame to `<file>` as raw RGBA: width and height as little-endian
//! u32, then pixels. Commands: `select <id>`, `pause <id>`, `resume <id>`,
//! `filter <all|completed>`, `queue <id>`, `stats`,
//! `open <add-url|settings|categories|new-queue>`, `close`, `key [ctrl+]<name>`,
//! `window <close|minimize>`, `scroll <points>`, `hover|press|release <x> <y>`,
//! `resize <width> <height>`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use downpour_core::{DownloadId, QueueId};
use eframe::egui;

use super::{Action, Filter};

const SETTLE: Duration = Duration::from_millis(700);

pub enum HookCommand {
    Apply(Action),
    CloseDialog,
    /// Deliver these input events (keys, pointer moves and presses) to the
    /// next frame, as if the user did them.
    Input(Vec<egui::Event>),
    /// Scroll the open dialog to this offset, in points from the top.
    Scroll(f32),
}

/// `ArrowDown` or `ctrl+ArrowDown`.
fn key_press(spec: &str) -> Option<egui::Event> {
    let (modifiers, name) = match spec.split_once('+') {
        Some(("ctrl", name)) => (egui::Modifiers::COMMAND, name),
        Some(_) => return None,
        None => (egui::Modifiers::NONE, spec),
    };
    let key =
        egui::Key::from_name(name).or_else(|| egui::Key::from_name(&name.to_ascii_uppercase()))?;
    Some(egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    })
}

/// The pointer moved to `x, y` (points), then optionally pressed or released there.
fn pointer_at(x: &str, y: &str, button: Option<bool>) -> Option<Vec<egui::Event>> {
    let pos = egui::pos2(x.parse().ok()?, y.parse().ok()?);
    let mut events = vec![egui::Event::PointerMoved(pos)];
    if let Some(pressed) = button {
        events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    Some(events)
}

enum Phase {
    Idle,
    Settling { until: Instant },
    Capturing,
}

pub struct ScreenshotHook {
    output: PathBuf,
    request: PathBuf,
    phase: Phase,
}

impl ScreenshotHook {
    pub fn from_env() -> Option<Self> {
        let output = PathBuf::from(std::env::var_os("DOWNPOUR_SCREENSHOT")?);
        let mut request = output.clone().into_os_string();
        request.push(".request");
        Some(Self {
            output,
            request: request.into(),
            phase: Phase::Idle,
        })
    }

    /// Returns commands for the app to run this frame.
    pub fn update(&mut self, ctx: &egui::Context) -> Vec<HookCommand> {
        ctx.request_repaint_after(Duration::from_millis(100));
        let mut commands = Vec::new();
        match self.phase {
            Phase::Idle => {
                if let Ok(script) = std::fs::read_to_string(&self.request) {
                    let _ = std::fs::remove_file(&self.request);
                    commands = script.lines().filter_map(|line| parse(ctx, line)).collect();
                    // A minimized or hidden window has no pixels, and
                    // egui_glow panics when asked to read a zero-sized
                    // screen; requests that hide the window get no capture.
                    let hides_window = script
                        .lines()
                        .any(|line| line.trim_start().starts_with("window "));
                    self.phase = if hides_window {
                        Phase::Idle
                    } else {
                        Phase::Settling {
                            until: Instant::now() + SETTLE,
                        }
                    };
                }
            }
            Phase::Settling { until } if Instant::now() >= until => {
                let minimized = ctx.input(|input| input.viewport().minimized == Some(true));
                if minimized {
                    self.phase = Phase::Idle;
                    return commands;
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
                self.phase = Phase::Capturing;
            }
            Phase::Settling { .. } => {}
            Phase::Capturing => {
                if let Some(image) = screenshot_event(ctx) {
                    self.save(&image);
                    self.phase = Phase::Idle;
                }
            }
        }
        commands
    }

    fn save(&self, image: &egui::ColorImage) {
        let mut bytes = Vec::with_capacity(8 + image.pixels.len() * 4);
        bytes.extend_from_slice(&(image.size[0] as u32).to_le_bytes());
        bytes.extend_from_slice(&(image.size[1] as u32).to_le_bytes());
        for pixel in &image.pixels {
            bytes.extend_from_slice(&pixel.to_array());
        }
        if let Err(error) = std::fs::write(&self.output, bytes) {
            tracing::warn!(%error, "screenshot write failed");
        }
    }
}

fn screenshot_event(ctx: &egui::Context) -> Option<std::sync::Arc<egui::ColorImage>> {
    ctx.input(|input| {
        input.events.iter().find_map(|event| match event {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        })
    })
}

fn parse(ctx: &egui::Context, line: &str) -> Option<HookCommand> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let command = match words.as_slice() {
        ["select", id] => HookCommand::Apply(Action::Select(Some(DownloadId(id.parse().ok()?)))),
        ["pause", id] => HookCommand::Apply(Action::Pause(DownloadId(id.parse().ok()?))),
        ["resume", id] => HookCommand::Apply(Action::Resume(DownloadId(id.parse().ok()?))),
        ["filter", "all"] => HookCommand::Apply(Action::SetFilter(Filter::All)),
        ["filter", "completed"] => HookCommand::Apply(Action::SetFilter(Filter::Completed)),
        ["open", "add-url"] => HookCommand::Apply(Action::OpenAddUrl),
        ["open", "settings"] => HookCommand::Apply(Action::OpenSettings),
        ["open", "categories"] => HookCommand::Apply(Action::ManageCategories),
        ["open", "new-queue"] => HookCommand::Apply(Action::NewQueue),
        ["queue", id] => {
            HookCommand::Apply(Action::SetFilter(Filter::Queue(QueueId(id.parse().ok()?))))
        }
        ["stats"] => HookCommand::Apply(Action::ShowStatistics),
        ["close"] => HookCommand::CloseDialog,
        // What the title bar's buttons send.
        ["window", "close"] => {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return None;
        }
        ["window", "minimize"] => {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            return None;
        }
        ["key", spec] => HookCommand::Input(vec![key_press(spec)?]),
        ["scroll", amount] => HookCommand::Scroll(amount.parse().ok()?),
        ["hover", x, y] => HookCommand::Input(pointer_at(x, y, None)?),
        ["press", x, y] => HookCommand::Input(pointer_at(x, y, Some(true))?),
        ["release", x, y] => HookCommand::Input(pointer_at(x, y, Some(false))?),
        ["resize", width, height] => {
            let size = egui::vec2(width.parse().ok()?, height.parse().ok()?);
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            return None;
        }
        _ => {
            tracing::warn!(line, "unknown screenshot command");
            return None;
        }
    };
    Some(command)
}
