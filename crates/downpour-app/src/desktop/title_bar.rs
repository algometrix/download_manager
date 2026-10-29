//! Light or dark title bars, following the theme the window is drawn in.
//!
//! winit paints the title bar for the system theme when a window is created
//! and whenever a system setting changes, but not for a theme picked in
//! Settings. So every window with a title bar on the UI thread (the main
//! window and dialogs in their own windows) is set with
//! `DWMWA_USE_IMMERSIVE_DARK_MODE` whenever the theme, the set of windows,
//! or the system's colors change.

use super::window_messages::SystemColors;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TitleBarColors {
    Light,
    Dark,
}

pub struct TitleBars {
    /// The colors last applied, and to which windows.
    applied: Option<(TitleBarColors, Vec<isize>)>,
}

impl TitleBars {
    pub fn new() -> Self {
        Self { applied: None }
    }

    pub fn follow(&mut self, colors: TitleBarColors, system: SystemColors) {
        let windows = native::captioned_windows_of_this_thread();
        let unchanged = self
            .applied
            .as_ref()
            .is_some_and(|(applied, known)| *applied == colors && *known == windows);
        if unchanged && system == SystemColors::Unchanged {
            return;
        }
        for &window in &windows {
            if let Err(error) = native::set_title_bar(window, colors) {
                tracing::debug!(%error, ?colors, "could not color a title bar");
            }
        }
        self.applied = Some((colors, windows));
    }
}

#[derive(Debug, thiserror::Error)]
#[error("DwmSetWindowAttribute failed with HRESULT {0:#010x}")]
pub struct TitleBarError(pub i32);

#[cfg(windows)]
mod native {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, S_OK};
    use windows_sys::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE};
    use windows_sys::Win32::System::Threading::GetCurrentThreadId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumThreadWindows, GetWindowLongPtrW, GWL_STYLE, WS_CAPTION,
    };

    use super::{TitleBarColors, TitleBarError};

    /// The attribute's number before Windows 10 20H1.
    const DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_20H1: u32 = 19;

    pub fn captioned_windows_of_this_thread() -> Vec<isize> {
        unsafe extern "system" fn collect(hwnd: HWND, found: LPARAM) -> BOOL {
            // SAFETY: `found` is the `Vec` passed below, alive for the call.
            let found = unsafe { &mut *(found as *mut Vec<isize>) };
            // SAFETY: reads the style of a window Windows just listed.
            let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
            if style & WS_CAPTION == WS_CAPTION {
                found.push(hwnd as isize);
            }
            1
        }
        let mut found: Vec<isize> = Vec::new();
        // SAFETY: the callback only touches `found`, which outlives the call.
        unsafe {
            EnumThreadWindows(
                GetCurrentThreadId(),
                Some(collect),
                &mut found as *mut Vec<isize> as LPARAM,
            );
        }
        found
    }

    pub fn set_title_bar(window: isize, colors: TitleBarColors) -> Result<(), TitleBarError> {
        let dark: BOOL = match colors {
            TitleBarColors::Dark => 1,
            TitleBarColors::Light => 0,
        };
        let set = |attribute: u32| {
            // SAFETY: `window` belongs to this thread, and the value is a
            // BOOL of the size given.
            unsafe {
                DwmSetWindowAttribute(
                    window as HWND,
                    attribute,
                    (&dark as *const BOOL).cast(),
                    std::mem::size_of::<BOOL>() as u32,
                )
            }
        };
        let result = set(DWMWA_USE_IMMERSIVE_DARK_MODE as u32);
        if result == S_OK {
            return Ok(());
        }
        match set(DWMWA_USE_IMMERSIVE_DARK_MODE_BEFORE_20H1) {
            S_OK => Ok(()),
            _ => Err(TitleBarError(result)),
        }
    }
}

#[cfg(not(windows))]
mod native {
    use super::{TitleBarColors, TitleBarError};

    pub fn captioned_windows_of_this_thread() -> Vec<isize> {
        Vec::new()
    }

    pub fn set_title_bar(_window: isize, _colors: TitleBarColors) -> Result<(), TitleBarError> {
        Ok(())
    }
}
