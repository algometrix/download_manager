//! "Start with Windows": Downpour's value under the per-user `Run` key,
//! which starts it in the tray when the user signs in.
//!
//! The value belongs to the copy it starts. Turning the setting off removes
//! it only when it starts this copy; at startup a copy with the setting on
//! points it back at itself (after the program folder moved, for example)
//! but a copy with the setting off leaves it alone.

use std::path::Path;

use downpour_protocol::desktop::{program_of_command, run_at_sign_in_command, same_path};

use crate::app_settings::SignInStart;

/// What to do with the `Run` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunValueChange {
    Keep,
    Write(String),
    Remove,
}

/// The change that applies the user's `choice`.
pub fn change_for_choice(choice: SignInStart, current: Option<&str>, exe: &Path) -> RunValueChange {
    match (choice, current) {
        (SignInStart::Manual, Some(current)) if starts(current, exe) => RunValueChange::Remove,
        // Nothing there, or another copy's entry: not this copy's to remove.
        (SignInStart::Manual, _) => RunValueChange::Keep,
        (SignInStart::StartInTray, current) => write_unless_current(current, exe),
    }
}

/// The change made at startup: only repairs a value the setting asks for.
pub fn change_at_startup(choice: SignInStart, current: Option<&str>, exe: &Path) -> RunValueChange {
    match choice {
        SignInStart::Manual => RunValueChange::Keep,
        SignInStart::StartInTray => write_unless_current(current, exe),
    }
}

/// Whether the `Run` command starts the program at `exe`.
fn starts(command: &str, exe: &Path) -> bool {
    program_of_command(command).is_some_and(|program| same_path(&program, exe))
}

fn write_unless_current(current: Option<&str>, exe: &Path) -> RunValueChange {
    let wanted = run_at_sign_in_command(exe);
    if current == Some(wanted.as_str()) {
        return RunValueChange::Keep;
    }
    RunValueChange::Write(wanted)
}

#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    #[error("cannot find this program's own path: {0}")]
    OwnPath(std::io::Error),
    #[error("cannot {action} the sign-in entry in the registry: {source}")]
    Registry {
        action: &'static str,
        source: std::io::Error,
    },
}

/// Applies the user's choice to the registry.
pub fn apply_choice(choice: SignInStart) -> Result<(), SignInError> {
    let exe = std::env::current_exe().map_err(SignInError::OwnPath)?;
    let current = native::read_run_value()?;
    native::make(change_for_choice(choice, current.as_deref(), &exe))
}

/// Points the registry back at this copy when the setting is on. Problems
/// are logged: the user did not ask for anything just now.
pub fn repair_at_startup(choice: SignInStart) {
    let result = std::env::current_exe()
        .map_err(SignInError::OwnPath)
        .and_then(|exe| {
            let current = native::read_run_value()?;
            native::make(change_at_startup(choice, current.as_deref(), &exe))
        });
    if let Err(error) = result {
        tracing::warn!(%error, "could not check the start-with-Windows entry");
    }
}

#[cfg(windows)]
mod native {
    use std::io;

    use downpour_protocol::desktop::{RUN_KEY, RUN_VALUE};
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    use winreg::RegKey;

    use super::{RunValueChange, SignInError};

    fn registry_error(action: &'static str) -> impl FnOnce(io::Error) -> SignInError {
        move |source| SignInError::Registry { action, source }
    }

    pub fn read_run_value() -> Result<Option<String>, SignInError> {
        let key = match RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, KEY_READ)
        {
            Ok(key) => key,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(registry_error("open")(error)),
        };
        match key.get_value::<String, _>(RUN_VALUE) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(registry_error("read")(error)),
        }
    }

    pub fn make(change: RunValueChange) -> Result<(), SignInError> {
        match change {
            RunValueChange::Keep => Ok(()),
            RunValueChange::Write(command) => {
                let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
                    .create_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
                    .map_err(registry_error("open"))?;
                key.set_value(RUN_VALUE, &command)
                    .map_err(registry_error("write"))?;
                tracing::info!(%command, "Downpour starts with Windows");
                Ok(())
            }
            RunValueChange::Remove => {
                let key = RegKey::predef(HKEY_CURRENT_USER)
                    .open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)
                    .map_err(registry_error("open"))?;
                match key.delete_value(RUN_VALUE) {
                    Ok(()) => {
                        tracing::info!("Downpour no longer starts with Windows");
                        Ok(())
                    }
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(registry_error("remove")(error)),
                }
            }
        }
    }
}

#[cfg(not(windows))]
mod native {
    use super::{RunValueChange, SignInError};

    pub fn read_run_value() -> Result<Option<String>, SignInError> {
        Ok(None)
    }

    pub fn make(_change: RunValueChange) -> Result<(), SignInError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exe() -> &'static Path {
        Path::new(r"C:\Programs\Downpour\downpour.exe")
    }

    #[test]
    fn turning_it_on_writes_a_quiet_start_from_this_copy() {
        let wanted = run_at_sign_in_command(exe());
        assert!(wanted.ends_with(downpour_protocol::desktop::START_IN_TRAY_FLAG));
        assert_eq!(
            change_for_choice(SignInStart::StartInTray, None, exe()),
            RunValueChange::Write(wanted.clone())
        );
        assert_eq!(
            change_for_choice(SignInStart::StartInTray, Some(&wanted), exe()),
            RunValueChange::Keep
        );
        assert_eq!(
            change_for_choice(
                SignInStart::StartInTray,
                Some(r#""D:\old\downpour.exe""#),
                exe()
            ),
            RunValueChange::Write(wanted)
        );
    }

    #[test]
    fn turning_it_off_removes_only_this_copys_value() {
        let ours = run_at_sign_in_command(exe());
        assert_eq!(
            change_for_choice(SignInStart::Manual, Some(&ours), exe()),
            RunValueChange::Remove
        );
        // The same program, written with other letter case.
        assert_eq!(
            change_for_choice(
                SignInStart::Manual,
                Some(r#""c:\programs\downpour\DOWNPOUR.EXE" --start-in-tray"#),
                exe()
            ),
            RunValueChange::Remove
        );
        assert_eq!(
            change_for_choice(
                SignInStart::Manual,
                Some(r#""D:\other\downpour.exe" --start-in-tray"#),
                exe()
            ),
            RunValueChange::Keep
        );
        assert_eq!(
            change_for_choice(SignInStart::Manual, None, exe()),
            RunValueChange::Keep
        );
    }

    #[test]
    fn startup_repairs_but_never_removes() {
        let moved = r#""D:\old\downpour.exe" --start-in-tray"#;
        assert_eq!(
            change_at_startup(SignInStart::StartInTray, Some(moved), exe()),
            RunValueChange::Write(run_at_sign_in_command(exe()))
        );
        // Another copy may own the value; a copy with the setting off
        // leaves it alone.
        assert_eq!(
            change_at_startup(SignInStart::Manual, Some(moved), exe()),
            RunValueChange::Keep
        );
    }
}
