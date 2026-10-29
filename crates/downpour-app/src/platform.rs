//! Opening files and folders with the operating system's shell, and
//! bringing the main window back from the tray.

use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

/// The main window's native handle, kept so it can be shown again while it
/// is hidden in the tray. A hidden egui window runs no frames, so it cannot
/// process a "show yourself" command on its own.
static MAIN_WINDOW: OnceLock<MainWindow> = OnceLock::new();

/// The main window's native handle (a Win32 `HWND`). It stays valid until
/// the process exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MainWindow {
    #[cfg_attr(not(windows), allow(dead_code))]
    handle: isize,
}

impl MainWindow {
    #[cfg(windows)]
    pub fn hwnd(self) -> windows_sys::Win32::Foundation::HWND {
        self.handle as windows_sys::Win32::Foundation::HWND
    }
}

/// Remembers the main window; call once when the window is created.
/// Returns it where this platform has a native handle to keep.
pub fn remember_main_window(
    window: &impl raw_window_handle::HasWindowHandle,
) -> Option<MainWindow> {
    let handle = match window.window_handle() {
        Ok(handle) => handle,
        Err(error) => {
            tracing::warn!(%error, "no native handle for the main window; the tray cannot bring it back");
            return None;
        }
    };
    let raw = match handle.as_raw() {
        #[cfg(windows)]
        raw_window_handle::RawWindowHandle::Win32(win32) => win32.hwnd.get(),
        other => {
            tracing::debug!(
                ?other,
                "the main window is shown through egui on this platform"
            );
            return None;
        }
    };
    let window = MainWindow { handle: raw };
    if let Err(already) = MAIN_WINDOW.set(window) {
        tracing::warn!(
            ?already,
            "the main window was remembered already; keeping the first"
        );
    }
    MAIN_WINDOW.get().copied()
}

/// Shows, restores, and focuses the main window natively. Returns whether
/// this platform did it; elsewhere the egui viewport commands must do.
pub fn show_main_window() -> bool {
    let Some(window) = MAIN_WINDOW.get() else {
        return false;
    };
    show_native(*window)
}

#[cfg(windows)]
fn show_native(window: MainWindow) -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
    };
    let hwnd = window.hwnd();
    // SAFETY: the handle belongs to this process's main window, which lives
    // until the process exits; these calls only change how it is shown.
    unsafe {
        set_taskbar_button(hwnd, TaskbarButton::Shown);
        ShowWindow(hwnd, SW_SHOW);
        if IsIconic(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
        SetForegroundWindow(hwnd);
    }
    true
}

#[cfg(not(windows))]
fn show_native(_window: MainWindow) -> bool {
    false
}

/// Puts the main window in the tray: minimized, with no taskbar button and
/// not in Alt+Tab. It is not hidden, because a hidden window runs no frames,
/// and dialogs that pop out of the main window are drawn from its frames.
/// Windows parks minimized windows off-screen, so nothing is visible.
/// Returns whether this platform did it; elsewhere the caller hides the
/// window instead.
pub fn minimize_to_tray() -> bool {
    let Some(window) = MAIN_WINDOW.get() else {
        return false;
    };
    minimize_to_tray_native(*window)
}

#[cfg(windows)]
fn minimize_to_tray_native(window: MainWindow) -> bool {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowPlacement, SetWindowPlacement, ShowWindow, SW_SHOWMINNOACTIVE, WINDOWPLACEMENT,
        WPF_SETMINPOSITION,
    };
    /// Where Windows itself parks minimized windows. A minimized tool window
    /// would otherwise sit as a small title bar at the bottom of the screen.
    const OFF_SCREEN: POINT = POINT {
        x: -32000,
        y: -32000,
    };
    let hwnd = window.hwnd();
    // SAFETY: as in `show_native`; the placement struct is sized for the call.
    unsafe {
        set_taskbar_button(hwnd, TaskbarButton::Hidden);
        let mut placement: WINDOWPLACEMENT = std::mem::zeroed();
        placement.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
        if GetWindowPlacement(hwnd, &mut placement) != 0 {
            // The normal position stays as it was, for restoring later.
            placement.flags |= WPF_SETMINPOSITION;
            placement.ptMinPosition = OFF_SCREEN;
            placement.showCmd = SW_SHOWMINNOACTIVE as u32;
            SetWindowPlacement(hwnd, &placement);
        } else {
            ShowWindow(hwnd, SW_SHOWMINNOACTIVE);
        }
    }
    true
}

#[cfg(not(windows))]
fn minimize_to_tray_native(_window: MainWindow) -> bool {
    false
}

#[cfg(windows)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskbarButton {
    Shown,
    Hidden,
}

/// A tool window has no taskbar button. Windows applies a change of this
/// style only while the window is hidden, so the window is hidden for that
/// moment when the style actually changes.
///
/// # Safety
/// `hwnd` must be a live window of this process.
#[cfg(windows)]
unsafe fn set_taskbar_button(hwnd: windows_sys::Win32::Foundation::HWND, button: TaskbarButton) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetWindowLongPtrW, ShowWindow, GWL_EXSTYLE, SW_HIDE, WS_EX_APPWINDOW,
        WS_EX_TOOLWINDOW,
    };
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) };
    let tool = WS_EX_TOOLWINDOW as isize;
    let app = WS_EX_APPWINDOW as isize;
    let wanted = match button {
        TaskbarButton::Shown => (style & !tool) | app,
        TaskbarButton::Hidden => (style | tool) & !app,
    };
    if wanted == style {
        return;
    }
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted);
    }
}

fn shell_opener() -> &'static str {
    if cfg!(windows) {
        "explorer.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

/// Opens the file with its default application.
pub fn open_file(path: &Path) -> io::Result<()> {
    Command::new(shell_opener()).arg(path).spawn().map(drop)
}

/// Opens a web page in the default browser. Only http(s) addresses are
/// passed to the shell, so a crafted link cannot launch a local program.
pub fn open_web_page(url: &url::Url) -> io::Result<()> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a web address: {url}"),
        ));
    }
    Command::new(shell_opener())
        .arg(url.as_str())
        .spawn()
        .map(drop)
}

/// Shows the file selected in its folder, or opens the folder if the file
/// does not exist yet.
pub fn reveal(path: &Path) -> io::Result<()> {
    if !path.exists() {
        let folder = path.parent().filter(|dir| dir.exists()).unwrap_or(path);
        return open_file(folder);
    }
    if cfg!(windows) {
        let mut select = std::ffi::OsString::from("/select,");
        select.push(path);
        Command::new("explorer.exe").arg(select).spawn().map(drop)
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg("-R").arg(path).spawn().map(drop)
    } else {
        open_file(path.parent().unwrap_or(path))
    }
}
