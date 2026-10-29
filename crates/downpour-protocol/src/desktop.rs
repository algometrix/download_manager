//! Names the app and the installer must agree on for Windows desktop
//! integration: the app writes and uses them, the uninstaller removes them.

use std::path::{Component, Path, PathBuf};

/// Who publishes Downpour: the executables' `CompanyName` and the
/// `Publisher` under Settings > Apps.
pub const PUBLISHER: &str = "Downpour contributors";

/// Identifies Downpour to Windows for notifications and taskbar grouping.
/// The process, its Start menu shortcut, and the registry entry under
/// [`APP_USER_MODEL_ID_KEY`] carry the same ID, so notifications show
/// Downpour's name and icon and a pinned shortcut groups with the window.
pub const APP_USER_MODEL_ID: &str = "Downpour.Downpour";

/// `HKEY_CURRENT_USER` key whose values (`DisplayName`, `IconUri`) tell
/// Windows how to show notifications from [`APP_USER_MODEL_ID`].
pub const APP_USER_MODEL_ID_KEY: &str = r"Software\Classes\AppUserModelId\Downpour.Downpour";

/// `HKEY_CURRENT_USER` key of programs started when the user signs in.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

/// Downpour's value under [`RUN_KEY`].
pub const RUN_VALUE: &str = "Downpour";

/// Windows starts the app with this flag when the user signs in: it starts
/// quietly in the tray instead of opening its window.
pub const START_IN_TRAY_FLAG: &str = "--start-in-tray";

/// The command line Windows runs at sign-in for the app at `exe`.
pub fn run_at_sign_in_command(exe: &Path) -> String {
    format!("\"{}\" {START_IN_TRAY_FLAG}", exe.display())
}

/// The program a [`RUN_KEY`] command line starts: the quoted path at its
/// start, or everything before the first space when it is not quoted.
/// `None` for an empty command.
pub fn program_of_command(command: &str) -> Option<PathBuf> {
    let command = command.trim_start();
    let program = match command.strip_prefix('"') {
        Some(quoted) => quoted.split('"').next()?,
        None => command.split(' ').next()?,
    };
    (!program.is_empty()).then(|| PathBuf::from(program))
}

/// Whether two paths name the same file, compared as Windows compares
/// paths: part by part and without regard to case.
pub fn same_path(a: &Path, b: &Path) -> bool {
    path_parts(a) == path_parts(b)
}

/// Whether `path` is `folder` itself or lies inside it, compared as
/// [`same_path`] compares.
pub fn is_within(path: &Path, folder: &Path) -> bool {
    path_parts(path).starts_with(&path_parts(folder))
}

fn path_parts(path: &Path) -> Vec<String> {
    path.components()
        .filter(|part| !matches!(part, Component::CurDir))
        .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn paths_compare_without_case_and_by_whole_parts() {
        let folder = Path::new(r"C:\Users\Someone\AppData\Local\Programs\Downpour");
        let exe = Path::new(r"c:\users\someone\appdata\local\programs\downpour\downpour.exe");
        assert!(same_path(exe, &folder.join("DOWNPOUR.EXE")));
        assert!(!same_path(exe, folder));
        assert!(is_within(exe, folder));
        assert!(is_within(folder, folder));
        let neighbor = Path::new(r"C:\Users\Someone\AppData\Local\Programs\Downpour2\x.exe");
        assert!(!is_within(neighbor, folder));
    }

    #[test]
    fn the_sign_in_command_quotes_the_program_and_starts_in_the_tray() {
        let exe = Path::new(r"C:\Users\Someone\AppData\Local\Programs\Downpour\downpour.exe");
        let command = run_at_sign_in_command(exe);
        assert_eq!(
            command,
            r#""C:\Users\Someone\AppData\Local\Programs\Downpour\downpour.exe" --start-in-tray"#
        );
        assert_eq!(program_of_command(&command).as_deref(), Some(exe));
    }

    #[test]
    fn unquoted_and_empty_commands() {
        assert_eq!(
            program_of_command(r"C:\Tools\downpour.exe --start-in-tray").as_deref(),
            Some(Path::new(r"C:\Tools\downpour.exe"))
        );
        assert_eq!(program_of_command(""), None);
        assert_eq!(program_of_command("\"\""), None);
    }
}
