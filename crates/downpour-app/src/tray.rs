//! The notification-area (tray) icon: Downpour keeps running there when its
//! window is closed or minimized.
//!
//! Only this module talks to the platform's tray, through `tray-icon`, which
//! also covers macOS and Linux. A hidden window may not run frames, so menu
//! choices act right away from the tray's event handler instead of waiting
//! for the next frame.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use downpour_core::DownloadManager;
use eframe::egui;
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

/// Set when the user chose Quit, so closing the window really exits
/// instead of hiding to the tray.
#[derive(Clone, Default)]
pub struct QuitRequest(Arc<AtomicBool>);

impl QuitRequest {
    pub fn is_requested(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    fn request(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// How the main window starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowStart {
    Open,
    /// Straight into the tray, as when Windows starts Downpour at sign-in.
    InTray,
}

/// Exits the app the way Quit in the tray menu does: downloads pause and
/// resume at the next start.
pub fn quit(ctx: &egui::Context, quit_request: &QuitRequest) {
    quit_request.request();
    // The window must be visible to process the close request.
    crate::ui::show_window(ctx);
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
}

pub struct Tray {
    icon: TrayIcon,
    tooltip: String,
}

/// What each menu entry does; run on the UI event loop's thread.
struct MenuActions {
    show: MenuId,
    pause_all: MenuId,
    resume_all: MenuId,
    quit: MenuId,
    ctx: egui::Context,
    manager: DownloadManager,
    quit_request: QuitRequest,
}

impl MenuActions {
    fn run(&self, id: &MenuId) {
        if *id == self.show {
            crate::ui::show_window(&self.ctx);
        } else if *id == self.pause_all {
            self.manager.pause_all();
        } else if *id == self.resume_all {
            self.manager.resume_all();
        } else if *id == self.quit {
            quit(&self.ctx, &self.quit_request);
        }
        self.ctx.request_repaint();
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TrayError {
    #[error("tray icon image: {0}")]
    Icon(String),
    #[error("tray menu: {0}")]
    Menu(String),
    #[error("tray icon: {0}")]
    Create(String),
}

impl Tray {
    /// Puts the icon in the tray. Must run on the UI event loop's thread.
    pub fn create(
        ctx: &egui::Context,
        manager: DownloadManager,
        quit_request: QuitRequest,
    ) -> Result<Tray, TrayError> {
        use downpour_protocol::brand::{icon_rgba, ICON_SIZE};
        let image = Icon::from_rgba(icon_rgba(), ICON_SIZE as u32, ICON_SIZE as u32)
            .map_err(|error| TrayError::Icon(error.to_string()))?;

        let show = MenuItem::new("Open Downpour", true, None);
        let pause_all = MenuItem::new("Pause all", true, None);
        let resume_all = MenuItem::new("Resume all", true, None);
        let quit = MenuItem::new("Quit Downpour", true, None);
        let menu = Menu::new();
        menu.append_items(&[
            &show,
            &PredefinedMenuItem::separator(),
            &pause_all,
            &resume_all,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|error| TrayError::Menu(error.to_string()))?;
        let actions = MenuActions {
            show: show.id().clone(),
            pause_all: pause_all.id().clone(),
            resume_all: resume_all.id().clone(),
            quit: quit.id().clone(),
            ctx: ctx.clone(),
            manager,
            quit_request,
        };

        let icon = TrayIconBuilder::new()
            .with_icon(image)
            .with_tooltip("Downpour")
            .with_menu(Box::new(menu))
            // The menu opens on right click; left click shows the window.
            .with_menu_on_left_click(false)
            .build()
            .map_err(|error| TrayError::Create(error.to_string()))?;

        MenuEvent::set_event_handler(Some(move |event: MenuEvent| actions.run(&event.id)));
        let repaint = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                crate::ui::show_window(&repaint);
            }
        }));

        Ok(Tray {
            icon,
            tooltip: String::new(),
        })
    }

    /// Shows progress on hover, e.g. "Downpour · 2 downloading · 3.4 MB/s".
    pub fn set_tooltip(&mut self, text: String) {
        if text == self.tooltip {
            return;
        }
        if let Err(error) = self.icon.set_tooltip(Some(&text)) {
            tracing::warn!(%error, "could not update the tray tooltip");
        }
        self.tooltip = text;
    }
}
