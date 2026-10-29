//! Registers this executable as Firefox's native messaging host.
//!
//! The manifest is written next to the executable. On Windows Firefox finds
//! it through a per-user registry key; elsewhere it is linked into Firefox's
//! per-user manifest directory. Every planned change is shown first and
//! nothing is written without confirmation. A value we are about to replace
//! is saved to `native-host-backup.json`.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use downpour_protocol::{FIREFOX_EXTENSION_ID, NATIVE_HOST_NAME};
use serde_json::json;

const FIREFOX_MANIFEST: &str = "downpour.native_host.firefox.json";
const BACKUP_FILE: &str = "native-host-backup.json";
const BROWSER: &str = "Mozilla Firefox";

#[derive(Debug, thiserror::Error)]
pub enum RegistrationError {
    #[error("{action}: {source}")]
    Io { action: String, source: io::Error },
    #[error("cancelled; nothing was changed")]
    Declined,
}

fn io_error(action: impl Into<String>) -> impl FnOnce(io::Error) -> RegistrationError {
    let action = action.into();
    move |source| RegistrationError::Io { action, source }
}

/// Where Firefox looks for the pointer to our manifest file.
struct ManifestPointer {
    #[cfg(windows)]
    registry_key: String,
    #[cfg(not(windows))]
    manifest_dir: Option<PathBuf>,
}

#[cfg(windows)]
fn firefox_pointer() -> ManifestPointer {
    ManifestPointer {
        registry_key: format!(r"Software\Mozilla\NativeMessagingHosts\{NATIVE_HOST_NAME}"),
    }
}

#[cfg(not(windows))]
fn firefox_pointer() -> ManifestPointer {
    let relative = if cfg!(target_os = "macos") {
        "Library/Application Support/Mozilla/NativeMessagingHosts"
    } else {
        ".mozilla/native-messaging-hosts"
    };
    ManifestPointer {
        manifest_dir: directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(relative)),
    }
}

fn manifest_json(host_path: &Path) -> serde_json::Value {
    json!({
        "name": NATIVE_HOST_NAME,
        "description": "Downpour browser integration",
        "path": host_path,
        "type": "stdio",
        "allowed_extensions": [FIREFOX_EXTENSION_ID],
    })
}

fn host_paths() -> Result<(PathBuf, PathBuf), RegistrationError> {
    let host = std::env::current_exe().map_err(io_error("locate downpour-host"))?;
    let dir = host.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok((host, dir))
}

/// Whether the planned changes still need the user's yes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmation {
    /// Show the plan and ask on the terminal.
    Ask,
    /// Given up front (`--yes`), e.g. by the installer.
    Given,
}

fn confirm(confirmation: Confirmation) -> Result<(), RegistrationError> {
    if confirmation == Confirmation::Given {
        return Ok(());
    }
    print!("Apply these changes? [y/N] ");
    io::stdout().flush().map_err(io_error("write prompt"))?;
    let mut answer = String::new();
    io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(io_error("read answer"))?;
    if answer.trim().eq_ignore_ascii_case("y") {
        Ok(())
    } else {
        Err(RegistrationError::Declined)
    }
}

/// What Firefox's pointer currently names, compared with our manifest.
#[derive(Debug, PartialEq, Eq)]
enum PointerState {
    /// It names our manifest.
    Ours,
    /// It names another manifest, which we must not lose.
    Foreign(String),
    /// There is no pointer.
    Absent,
}

fn pointer_state(current: Option<String>, ours: &Path) -> PointerState {
    match current {
        None => PointerState::Absent,
        Some(current) if Path::new(&current) == ours => PointerState::Ours,
        Some(current) => PointerState::Foreign(current),
    }
}

pub fn install(confirmation: Confirmation) -> Result<(), RegistrationError> {
    let (host, dir) = host_paths()?;
    let pointer = firefox_pointer();
    let manifest = dir.join(FIREFOX_MANIFEST);
    let state = pointer_state(platform::current_pointer(&pointer), &manifest);

    println!("Downpour native host registration");
    println!("Host executable: {}", host.display());
    println!("Planned changes:");
    println!("  write manifest {}", manifest.display());
    let note = match &state {
        PointerState::Ours => " (already set)".to_owned(),
        PointerState::Foreign(previous) => format!(" (replaces {previous})"),
        PointerState::Absent => String::new(),
    };
    println!(
        "  {BROWSER}: {}{note}",
        platform::describe_pointer(&pointer, &manifest)
    );
    if let PointerState::Foreign(_) = state {
        println!(
            "  save the replaced value to {}",
            dir.join(BACKUP_FILE).display()
        );
    }
    confirm(confirmation)?;

    if let PointerState::Foreign(previous) = &state {
        let backup =
            serde_json::to_vec_pretty(&json!({ BROWSER: previous })).expect("json map serializes");
        std::fs::write(dir.join(BACKUP_FILE), backup).map_err(io_error("write backup"))?;
    }
    let contents = serde_json::to_vec_pretty(&manifest_json(&host)).expect("manifest serializes");
    std::fs::write(&manifest, contents)
        .map_err(io_error(format!("write {}", manifest.display())))?;
    platform::set_pointer(&pointer, &manifest)?;
    println!("Done. Add the extension, then click its toolbar icon to check the connection.");
    Ok(())
}

pub fn uninstall(confirmation: Confirmation) -> Result<(), RegistrationError> {
    let (_, dir) = host_paths()?;
    let pointer = firefox_pointer();
    let manifest = dir.join(FIREFOX_MANIFEST);
    let state = pointer_state(platform::current_pointer(&pointer), &manifest);
    println!("Planned changes:");
    match &state {
        PointerState::Ours => println!(
            "  {BROWSER}: remove {}",
            platform::describe_pointer(&pointer, &manifest)
        ),
        PointerState::Foreign(current) => {
            println!("  {BROWSER}: leave alone (points to {current})")
        }
        PointerState::Absent => {}
    }
    println!("  delete {}", manifest.display());
    confirm(confirmation)?;

    if state == PointerState::Ours {
        platform::remove_pointer(&pointer)?;
    }
    match std::fs::remove_file(&manifest) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("delete manifest")(error)),
    }
    println!("Done.");
    Ok(())
}

#[cfg(windows)]
mod platform {
    use std::path::Path;

    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    use super::{io_error, ManifestPointer, RegistrationError};

    pub fn current_pointer(pointer: &ManifestPointer) -> Option<String> {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(&pointer.registry_key)
            .ok()?
            .get_value("")
            .ok()
    }

    pub fn describe_pointer(pointer: &ManifestPointer, manifest: &Path) -> String {
        format!(
            r"set HKCU\{} = {}",
            pointer.registry_key,
            manifest.display()
        )
    }

    pub fn set_pointer(
        pointer: &ManifestPointer,
        manifest: &Path,
    ) -> Result<(), RegistrationError> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(&pointer.registry_key)
            .map_err(io_error(format!(r"create HKCU\{}", pointer.registry_key)))?;
        key.set_value("", &manifest.display().to_string())
            .map_err(io_error(format!(r"set HKCU\{}", pointer.registry_key)))
    }

    pub fn remove_pointer(pointer: &ManifestPointer) -> Result<(), RegistrationError> {
        RegKey::predef(HKEY_CURRENT_USER)
            .delete_subkey(&pointer.registry_key)
            .map_err(io_error(format!(r"delete HKCU\{}", pointer.registry_key)))
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::{Path, PathBuf};

    use super::{io_error, ManifestPointer, RegistrationError};

    fn installed_path(pointer: &ManifestPointer) -> Option<PathBuf> {
        Some(
            pointer
                .manifest_dir
                .as_ref()?
                .join(format!("{}.json", downpour_protocol::NATIVE_HOST_NAME)),
        )
    }

    /// The installed copy records which manifest it came from in its path.
    pub fn current_pointer(pointer: &ManifestPointer) -> Option<String> {
        let installed = installed_path(pointer)?;
        installed.exists().then(|| {
            std::fs::read_link(&installed)
                .unwrap_or(installed)
                .display()
                .to_string()
        })
    }

    pub fn describe_pointer(pointer: &ManifestPointer, manifest: &Path) -> String {
        match installed_path(pointer) {
            Some(path) => format!("link {} -> {}", path.display(), manifest.display()),
            None => "skipped (no home directory)".to_owned(),
        }
    }

    pub fn set_pointer(
        pointer: &ManifestPointer,
        manifest: &Path,
    ) -> Result<(), RegistrationError> {
        let Some(path) = installed_path(pointer) else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io_error(format!("create {}", dir.display())))?;
        }
        let _ = std::fs::remove_file(&path);
        std::os::unix::fs::symlink(manifest, &path)
            .map_err(io_error(format!("link {}", path.display())))
    }

    pub fn remove_pointer(pointer: &ManifestPointer) -> Result<(), RegistrationError> {
        let Some(path) = installed_path(pointer) else {
            return Ok(());
        };
        std::fs::remove_file(&path).map_err(io_error(format!("delete {}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn firefox_manifest_allows_only_our_extension() {
        let manifest = manifest_json(Path::new("/opt/downpour/downpour-host"));
        assert_eq!(manifest["name"], NATIVE_HOST_NAME);
        assert_eq!(manifest["type"], "stdio");
        assert_eq!(
            manifest["allowed_extensions"],
            json!([FIREFOX_EXTENSION_ID])
        );
        assert!(manifest.get("allowed_origins").is_none());
    }

    #[test]
    fn the_pointer_is_ours_foreign_or_absent() {
        let ours = Path::new("/opt/downpour/downpour.native_host.firefox.json");
        assert_eq!(
            pointer_state(Some(ours.display().to_string()), ours),
            PointerState::Ours
        );
        let other = "/opt/other/native_host.json".to_owned();
        assert_eq!(
            pointer_state(Some(other.clone()), ours),
            PointerState::Foreign(other)
        );
        assert_eq!(pointer_state(None, ours), PointerState::Absent);
    }
}
