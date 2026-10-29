//! Contracts between the Firefox extension, the native messaging host, and
//! the desktop app.
//!
//! ```text
//! extension --(native messaging, stdio)--> downpour-host --(loopback TCP + token)--> downpour app
//! ```

pub mod brand;
pub mod desktop;
pub mod ipc;
pub mod messages;
pub mod native;

/// Name the extension passes to `runtime.sendNativeMessage`.
pub const NATIVE_HOST_NAME: &str = "downpour.native_host";

/// `browser_specific_settings.gecko.id` in the Firefox manifest.
pub const FIREFOX_EXTENSION_ID: &str = "downpour-dm@downpour.local";

/// Argument the native host passes when it starts the app for a browser request.
pub const FROM_BROWSER_FLAG: &str = "--from-browser";

/// The installer starts the app with this flag to open the signed add-on
/// in Firefox, so that logic has one owner.
pub const ADD_TO_FIREFOX_FLAG: &str = "--add-to-firefox";

/// Per-user data directory shared by the app and the native host.
pub const APP_QUALIFIER: (&str, &str, &str) = ("dev", "Downpour", "Downpour");

/// The folder holding downloads, settings, and `ipc.json`. Debug builds
/// honor `DOWNPOUR_DATA_DIR`, so tests and UI reviews (including a browser that
/// starts the native host) never touch the user's real data.
pub fn data_dir() -> Option<std::path::PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(dir) = std::env::var_os("DOWNPOUR_DATA_DIR") {
        // Shown in the app, so in the platform's form even if typed with `/`.
        return Some(std::path::Path::new(&dir).components().collect());
    }
    let (qualifier, organization, application) = APP_QUALIFIER;
    directories::ProjectDirs::from(qualifier, organization, application)
        .map(|dirs| dirs.data_local_dir().to_path_buf())
}
