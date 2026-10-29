//! Requests from the browser, arriving on the IPC thread.
//!
//! The window may be hidden in the tray, and a hidden window runs no
//! frames. So downloads that need no dialog (auto-start on) go straight to
//! the engine here. Handed-over downloads never bring the main window
//! forward: one that needs a dialog gets the dialog alone, in its own small
//! window. Only the user's explicit request (open Downpour) brings the
//! main window back. A link the user already has always waits for their
//! answer, even with auto-start on.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use downpour_core::{DownloadId, DownloadManager, ManagerError};
use eframe::egui;

use crate::app_settings::NewDownloadMode;
use crate::browser_link::{self, BrowserLink};
use crate::duplicates::{self, Arrival};

pub enum IncomingRequest {
    Show,
    Downloads(Vec<BrowserLink>),
    /// The installer needs the program files; exit.
    Quit,
}

/// What a request does to the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowEffect {
    /// The user asked for the window (or a view in it): bring it forward.
    Open,
    /// Show a dialog in its own window and leave the main window as it is.
    PopUpDialog,
}

/// What the window receives: browser links arrive told apart, so whether
/// one is a duplicate is decided once, when it comes in.
pub enum Delivery {
    Show,
    Downloads(Vec<Arrival>),
    Quit,
}

impl Delivery {
    pub fn window_effect(&self) -> WindowEffect {
        match self {
            Delivery::Show => WindowEffect::Open,
            // A window in the tray must be shown to process the close.
            Delivery::Quit => WindowEffect::Open,
            // Queuing a download from the browser must not pull the user
            // away from what they are doing; at most its dialog appears.
            Delivery::Downloads(_) => WindowEffect::PopUpDialog,
        }
    }
}

/// Adds a browser link the way the file-info dialog would with its
/// defaults untouched; for auto-start, and for "Download again" there.
pub fn add_browser_link(
    manager: &DownloadManager,
    link: BrowserLink,
) -> Result<DownloadId, ManagerError> {
    manager.add(browser_link::request_without_asking(
        link,
        &manager.settings(),
    ))
}

/// Whether a browser download can skip the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserHandling {
    /// Add downloads directly (auto-start on).
    AddDirectly,
    /// Show the confirmation dialog (or the refresh-address dialog).
    Confirm,
}

/// Filled by the IPC thread, drained once per frame by the UI. Requests that
/// arrive before the window exists wait here until the first frame.
pub struct Inbox {
    manager: DownloadManager,
    queue: Mutex<VecDeque<Delivery>>,
    handling: Mutex<BrowserHandling>,
    repaint: OnceLock<egui::Context>,
}

impl Inbox {
    pub fn new(manager: DownloadManager, mode: NewDownloadMode) -> Self {
        Self {
            manager,
            queue: Mutex::default(),
            handling: Mutex::new(handling_for(mode)),
            repaint: OnceLock::new(),
        }
    }

    pub fn attach(&self, ctx: &egui::Context) {
        let _ = self.repaint.set(ctx.clone());
    }

    /// Set by the app when the setting changes, or while it waits for a
    /// fresh address (which always needs the refresh dialog).
    pub fn set_handling(&self, handling: BrowserHandling) {
        *self.handling.lock().unwrap_or_else(|p| p.into_inner()) = handling;
    }

    pub fn push(&self, request: IncomingRequest) {
        let delivery = match request {
            IncomingRequest::Show => Delivery::Show,
            IncomingRequest::Quit => Delivery::Quit,
            IncomingRequest::Downloads(links) => {
                let classified =
                    duplicates::classify(links, &self.manager.snapshot(), Path::exists);
                if classified.repeats > 0 {
                    tracing::info!(
                        repeats = classified.repeats,
                        "the browser sent the same link more than once; keeping one"
                    );
                }
                let waiting = self.add_new_ones_directly(classified.arrivals);
                if waiting.is_empty() {
                    self.repaint();
                    return;
                }
                Delivery::Downloads(waiting)
            }
        };
        let effect = delivery.window_effect();
        self.queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push_back(delivery);
        if effect == WindowEffect::Open {
            crate::platform::show_main_window();
        }
        self.repaint();
    }

    pub fn drain(&self) -> Vec<Delivery> {
        self.queue
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
            .collect()
    }

    /// With auto-start on, adds the new links at once and returns the ones
    /// the user already has, which wait for their answer. Without auto-start
    /// every link waits for the dialog.
    fn add_new_ones_directly(&self, arrivals: Vec<Arrival>) -> Vec<Arrival> {
        let handling = *self.handling.lock().unwrap_or_else(|p| p.into_inner());
        if handling == BrowserHandling::Confirm {
            return arrivals;
        }
        let mut waiting = Vec::new();
        for arrival in arrivals {
            match arrival {
                Arrival::New(link) => self.add_logged(link),
                known @ Arrival::Known(..) => waiting.push(known),
            }
        }
        waiting
    }

    fn add_logged(&self, link: BrowserLink) {
        let url = downpour_core::http::url_for_logs(link.url());
        if let Err(error) = add_browser_link(&self.manager, link) {
            tracing::warn!(%url, %error, "could not add a browser download");
        }
    }

    fn repaint(&self) {
        if let Some(ctx) = self.repaint.get() {
            ctx.request_repaint();
        }
    }
}

pub fn handling_for(mode: NewDownloadMode) -> BrowserHandling {
    match mode {
        NewDownloadMode::StartRightAway => BrowserHandling::AddDirectly,
        NewDownloadMode::AskFirst => BrowserHandling::Confirm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_explicit_requests_bring_the_main_window_forward() {
        assert_eq!(Delivery::Show.window_effect(), WindowEffect::Open);
        assert_eq!(Delivery::Quit.window_effect(), WindowEffect::Open);
        assert_eq!(
            Delivery::Downloads(Vec::new()).window_effect(),
            WindowEffect::PopUpDialog
        );
    }
}
