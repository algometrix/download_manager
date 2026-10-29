//! The "Add to Firefox" button. Firefox installs a signed add-on when asked
//! to open the `.xpi` file and shows its own confirmation.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Signed Firefox add-on, shipped next to the app by the installer.
const FIREFOX_PACKAGE: &str = "downpour-firefox.xpi";
const FIREFOX_EXECUTABLE: &str = "firefox.exe";
const EXTENSIONS_FOLDER: &str = "extensions";

/// Starts installing the extension; Firefox asks the user to confirm.
/// Returns what the user should do next.
pub fn add_to_firefox() -> Result<String, String> {
    let extensions = extensions_dir().ok_or_else(|| {
        "The extension files are not next to Downpour. Install Downpour with its installer, \
         or run scripts\\package-extensions.ps1 for a development build."
            .to_owned()
    })?;
    let firefox = firefox_path().ok_or_else(|| {
        format!("{FIREFOX_EXECUTABLE} is not installed (or its location is unknown).")
    })?;
    let package = extensions.join(FIREFOX_PACKAGE);
    if !package.is_file() {
        return Err(format!("{} is missing.", package.display()));
    }
    launch(&firefox, &[package.as_os_str()])?;
    Ok("Firefox is asking to add Downpour. Click Add, then allow it in private windows if you like.".to_owned())
}

fn launch(browser: &Path, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    Command::new(browser)
        .args(args)
        .spawn()
        .map(drop)
        .map_err(|error| format!("Could not start {}: {error}", browser.display()))
}

/// `extensions\` next to the app, or the packaged `dist\extensions\` of a
/// development checkout (`target\debug\downpour.exe` → `dist\extensions`).
fn extensions_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let beside = exe.parent()?.join(EXTENSIONS_FOLDER);
    if beside.is_dir() {
        return Some(beside);
    }
    let checkout = exe
        .parent()?
        .parent()?
        .parent()?
        .join("dist")
        .join(EXTENSIONS_FOLDER);
    checkout.is_dir().then_some(checkout)
}

/// Where Firefox is installed, from Windows' "App Paths" registration.
#[cfg(windows)]
fn firefox_path() -> Option<PathBuf> {
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use winreg::RegKey;

    let key = format!(r"SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{FIREFOX_EXECUTABLE}");
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .into_iter()
        .filter_map(|hive| RegKey::predef(hive).open_subkey(&key).ok())
        .filter_map(|key| key.get_value::<String, _>("").ok())
        .map(|path| PathBuf::from(path.trim_matches('"')))
        .find(|path| path.is_file())
}

#[cfg(not(windows))]
fn firefox_path() -> Option<PathBuf> {
    let name = FIREFOX_EXECUTABLE.trim_end_matches(".exe");
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}
