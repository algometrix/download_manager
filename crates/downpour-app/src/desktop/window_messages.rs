//! Window messages winit does not pass on, seen by subclassing the main
//! window:
//! - `WM_ENDSESSION`: Windows is shutting down or the user signs out. The
//!   process ends as soon as this message returns, so eframe never gets to
//!   its exit code; downloads and settings are saved right here instead.
//! - `TaskbarButtonCreated`: the taskbar button was (re)created and has
//!   forgotten its progress bar.
//! - `WM_SETTINGCHANGE` and `WM_THEMECHANGED`: winit repaints the title bar
//!   for the system theme on these, over an app theme chosen in Settings.
//!
//! The window owns the hooks (they are freed when it is destroyed); the
//! [`WindowWatch`] shares the flags they set, which the next frame (each
//! message requests one) reads.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use eframe::egui;

use crate::platform::MainWindow;

/// Runs on the UI thread, inside `WM_ENDSESSION`.
pub type OnSessionEnd = Box<dyn Fn() + Send + Sync>;

/// What the subclass procedure reaches through its reference data.
#[cfg_attr(not(windows), allow(dead_code))]
struct Hooks {
    on_session_end: OnSessionEnd,
    repaint: egui::Context,
    taskbar_button_created: Arc<AtomicBool>,
    system_colors_changed: Arc<AtomicBool>,
    /// The message number Windows registered for "TaskbarButtonCreated";
    /// `None` if it would not register one.
    taskbar_button_created_message: Option<u32>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Hooks {
    fn flag_and_repaint(&self, flag: &AtomicBool) {
        flag.store(true, Ordering::SeqCst);
        self.repaint.request_repaint();
    }
}

/// Whether Windows' theme or colors may have changed since the last frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SystemColors {
    Unchanged,
    MayHaveChanged,
}

/// The flags the main window's messages set.
pub struct WindowWatch {
    taskbar_button_created: Arc<AtomicBool>,
    system_colors_changed: Arc<AtomicBool>,
}

impl WindowWatch {
    /// Whether the taskbar button was recreated since the last call.
    pub fn take_taskbar_button_created(&self) -> bool {
        self.taskbar_button_created.swap(false, Ordering::SeqCst)
    }

    pub fn take_system_colors(&self) -> SystemColors {
        match self.system_colors_changed.swap(false, Ordering::SeqCst) {
            true => SystemColors::MayHaveChanged,
            false => SystemColors::Unchanged,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("Windows refused to subclass the main window")]
    SubclassRefused,
}

/// Starts watching `window`'s messages. Call on the UI thread, which owns
/// the window.
pub fn watch_main_window(
    window: MainWindow,
    on_session_end: OnSessionEnd,
    repaint: egui::Context,
) -> Result<WindowWatch, WatchError> {
    let watch = WindowWatch {
        taskbar_button_created: Arc::new(AtomicBool::new(false)),
        system_colors_changed: Arc::new(AtomicBool::new(false)),
    };
    let hooks = Hooks {
        on_session_end,
        repaint,
        taskbar_button_created: Arc::clone(&watch.taskbar_button_created),
        system_colors_changed: Arc::clone(&watch.system_colors_changed),
        taskbar_button_created_message: native::taskbar_button_created_message(),
    };
    native::subclass(window, hooks)?;
    Ok(watch)
}

#[cfg(windows)]
mod native {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        RegisterWindowMessageW, WM_ENDSESSION, WM_NCDESTROY, WM_QUERYENDSESSION, WM_SETTINGCHANGE,
        WM_THEMECHANGED,
    };

    use super::{Hooks, WatchError};
    use crate::platform::MainWindow;

    /// Tells this subclass apart from others on the same window.
    const SUBCLASS_ID: usize = 0x444F_574E; // "DOWN"

    pub fn taskbar_button_created_message() -> Option<u32> {
        let name: Vec<u16> = "TaskbarButtonCreated\0".encode_utf16().collect();
        // SAFETY: `name` is a NUL-terminated UTF-16 string.
        let message = unsafe { RegisterWindowMessageW(name.as_ptr()) };
        if message == 0 {
            tracing::warn!("no TaskbarButtonCreated message; the progress bar may not come back after Explorer restarts");
            return None;
        }
        Some(message)
    }

    pub fn subclass(window: MainWindow, hooks: Hooks) -> Result<(), WatchError> {
        let data = Box::into_raw(Box::new(hooks));
        // SAFETY: the window belongs to this thread and is alive; `data`
        // stays valid until `WM_NCDESTROY` frees it, after removing the
        // subclass.
        let subclassed = unsafe {
            SetWindowSubclass(
                window.hwnd(),
                Some(main_window_proc),
                SUBCLASS_ID,
                data as usize,
            )
        };
        if subclassed == 0 {
            // SAFETY: Windows did not take `data`; it is still ours alone.
            drop(unsafe { Box::from_raw(data) });
            return Err(WatchError::SubclassRefused);
        }
        Ok(())
    }

    unsafe extern "system" fn main_window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
        _id: usize,
        data: usize,
    ) -> LRESULT {
        // SAFETY: `data` is the `Hooks` box from `subclass`, alive until
        // `WM_NCDESTROY` below.
        let hooks = unsafe { &*(data as *const Hooks) };
        match message {
            WM_QUERYENDSESSION => {
                tracing::info!("Windows asked whether the session may end");
            }
            // A zero `wparam` means the shutdown was called off.
            WM_ENDSESSION if wparam != 0 => (hooks.on_session_end)(),
            WM_SETTINGCHANGE | WM_THEMECHANGED => {
                // winit repaints the title bar during this message; the
                // next frame paints it back to the app's theme.
                // SAFETY: forwards the message unchanged to the next handler.
                let result = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
                hooks.flag_and_repaint(&hooks.system_colors_changed);
                return result;
            }
            WM_NCDESTROY => {
                // SAFETY: forwards the message, then removes this subclass and
                // frees its data; nothing reaches `hooks` afterwards.
                unsafe {
                    let result = DefSubclassProc(hwnd, message, wparam, lparam);
                    RemoveWindowSubclass(hwnd, Some(main_window_proc), SUBCLASS_ID);
                    drop(Box::from_raw(data as *mut Hooks));
                    return result;
                }
            }
            other if hooks.taskbar_button_created_message == Some(other) => {
                hooks.flag_and_repaint(&hooks.taskbar_button_created);
            }
            _ => {}
        }
        // SAFETY: forwards the message unchanged to the next handler.
        unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
    }
}

#[cfg(not(windows))]
mod native {
    use super::{Hooks, WatchError};
    use crate::platform::MainWindow;

    pub fn taskbar_button_created_message() -> Option<u32> {
        None
    }

    pub fn subclass(_window: MainWindow, _hooks: Hooks) -> Result<(), WatchError> {
        // Other platforms end sessions through eframe's own exit path.
        Ok(())
    }
}
