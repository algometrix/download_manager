#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! `Downpour-Setup.exe`: installs Downpour for the current user. Its copy in
//! the program folder, `Uninstall Downpour.exe`, uninstalls it.
//!
//! Downpour-Setup.exe                      setup window
//! Downpour-Setup.exe --quiet [--folder <dir>] [--no-shortcuts] [--no-browser-integration]
//!                                       install without a window
//! Uninstall Downpour.exe                  uninstall window (Start menu)
//! Downpour-Setup.exe --uninstall          uninstall window (Settings → Apps)
//! Downpour-Setup.exe --uninstall --quiet  uninstall without a window

mod install;
mod payload;
mod report;
mod running;
mod shortcut;
mod ui;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use install::{
    BrowserIntegration, FolderError, InstallFolder, InstallOptions, Shortcut, UserData, APP_NAME,
    UNINSTALLER_EXE,
};
use report::{LogLine, SetupReport};

const UNINSTALL_FLAG: &str = "--uninstall";
const QUIET_FLAG: &str = "--quiet";
const FOLDER_FLAG: &str = "--folder";
const NO_SHORTCUTS_FLAG: &str = "--no-shortcuts";
const NO_BROWSER_INTEGRATION_FLAG: &str = "--no-browser-integration";

/// Exit code of a quiet run that finished but left something behind.
const FINISHED_WITH_LEFTOVERS: u8 = 2;
/// Exit code of a command line that could not be understood.
const USAGE_ERROR: u8 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Install,
    Uninstall,
}

/// Uninstalls when asked with `--uninstall`, or when this program runs as
/// the uninstaller copy in the program folder (its file name decides,
/// compared without case as Windows compares file names). Anything else
/// installs.
fn mode_for(exe: &Path, args: &[String]) -> Mode {
    let runs_as_uninstaller = exe
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(UNINSTALLER_EXE));
    if runs_as_uninstaller || args.iter().any(|arg| arg == UNINSTALL_FLAG) {
        Mode::Uninstall
    } else {
        Mode::Install
    }
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
enum UsageError {
    #[error("unknown option {0}")]
    UnknownOption(String),
    #[error("{FOLDER_FLAG} needs a folder after it")]
    MissingFolder,
    #[error("{FOLDER_FLAG}: {0}")]
    Folder(#[from] FolderError),
}

/// The options of `--quiet` install, from the whole argument list.
fn parse_quiet_install(args: &[String]) -> Result<InstallOptions, UsageError> {
    let mut folder: Option<PathBuf> = None;
    let mut shortcuts = Shortcut::Create;
    let mut browser_integration = BrowserIntegration::Register;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            QUIET_FLAG => {}
            NO_SHORTCUTS_FLAG => shortcuts = Shortcut::Skip,
            NO_BROWSER_INTEGRATION_FLAG => browser_integration = BrowserIntegration::Skip,
            FOLDER_FLAG => {
                let value = args
                    .next()
                    .filter(|value| !value.is_empty() && !value.starts_with("--"))
                    .ok_or(UsageError::MissingFolder)?;
                folder = Some(PathBuf::from(value));
            }
            other => return Err(UsageError::UnknownOption(other.to_owned())),
        }
    }
    Ok(InstallOptions {
        folder: InstallFolder::new(folder.unwrap_or_else(install::default_folder))?,
        start_menu_shortcut: shortcuts,
        desktop_shortcut: shortcuts,
        browser_integration,
    })
}

/// Checks a `--quiet` uninstall's arguments: it takes no options of its own.
fn parse_quiet_uninstall(args: &[String]) -> Result<(), UsageError> {
    match args
        .iter()
        .find(|arg| !matches!(arg.as_str(), QUIET_FLAG | UNINSTALL_FLAG))
    {
        Some(other) => Err(UsageError::UnknownOption(other.clone())),
        None => Ok(()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe = std::env::current_exe().unwrap_or_default();
    let mode = mode_for(&exe, &args);
    let quiet = args.iter().any(|arg| arg == QUIET_FLAG);
    match (quiet, mode) {
        (true, Mode::Uninstall) => return quiet_uninstall(&args),
        (true, Mode::Install) => return quiet_install(&args),
        (false, _) => {}
    }
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(match mode {
                Mode::Install => format!("Install {APP_NAME}"),
                Mode::Uninstall => format!("Uninstall {APP_NAME}"),
            })
            .with_inner_size([620.0, 540.0])
            .with_resizable(false)
            .with_icon(eframe::egui::IconData {
                rgba: downpour_protocol::brand::icon_rgba(),
                width: downpour_protocol::brand::ICON_SIZE as u32,
                height: downpour_protocol::brand::ICON_SIZE as u32,
            }),
        ..Default::default()
    };
    match eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |cc| Ok(Box::new(ui::Setup::new(cc, mode)))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("setup window failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_line(line: LogLine) {
    match line {
        LogLine::Done(text) => println!("{text}"),
        LogLine::Warning(text) => eprintln!("warning: {text}"),
    }
}

fn exit_code_for(report: &SetupReport) -> ExitCode {
    match report.leftovers.len() {
        0 => ExitCode::SUCCESS,
        _ => ExitCode::from(FINISHED_WITH_LEFTOVERS),
    }
}

fn quiet_install(args: &[String]) -> ExitCode {
    let options = match parse_quiet_install(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("Downpour-Setup: {error}");
            return ExitCode::from(USAGE_ERROR);
        }
    };
    match install::install(&options, &mut print_line) {
        Ok(report) => exit_code_for(&report),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn quiet_uninstall(args: &[String]) -> ExitCode {
    if let Err(error) = parse_quiet_uninstall(args) {
        eprintln!("Downpour-Setup: {error}");
        return ExitCode::from(USAGE_ERROR);
    }
    let Some(folder) = install::folder_to_uninstall() else {
        eprintln!("cannot find the Downpour installation");
        return ExitCode::FAILURE;
    };
    match install::uninstall(&folder, UserData::Keep, &mut print_line) {
        Ok(report) => exit_code_for(&report),
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn the_setup_program_installs_unless_asked_to_uninstall() {
        let setup = Path::new("/downloads/Downpour-Setup.exe");
        assert_eq!(mode_for(setup, &args(&[])), Mode::Install);
        assert_eq!(mode_for(setup, &args(&["--quiet"])), Mode::Install);
        assert_eq!(mode_for(setup, &args(&[UNINSTALL_FLAG])), Mode::Uninstall);
    }

    #[test]
    fn the_uninstaller_copy_uninstalls_without_arguments() {
        let folder = Path::new("/programs/downpour");
        assert_eq!(
            mode_for(&folder.join(UNINSTALLER_EXE), &args(&[])),
            Mode::Uninstall
        );
        assert_eq!(
            mode_for(&folder.join("uninstall downpour.EXE"), &args(&["--quiet"])),
            Mode::Uninstall
        );
        assert_eq!(
            mode_for(&folder.join(UNINSTALLER_EXE), &args(&[UNINSTALL_FLAG])),
            Mode::Uninstall
        );
    }

    #[test]
    fn a_similar_name_does_not_uninstall() {
        let renamed = Path::new("/downloads/Uninstall Downpour (1).exe");
        assert_eq!(mode_for(renamed, &args(&[])), Mode::Install);
    }

    #[cfg(windows)]
    #[test]
    fn quiet_install_reads_its_options() {
        let folder = std::env::temp_dir().join("downpour-quiet-options");
        let options = parse_quiet_install(&args(&[
            "--quiet",
            "--folder",
            folder.to_str().unwrap(),
            "--no-shortcuts",
            "--no-browser-integration",
        ]))
        .unwrap();
        assert_eq!(
            options,
            InstallOptions {
                folder: InstallFolder::new(folder).unwrap(),
                start_menu_shortcut: Shortcut::Skip,
                desktop_shortcut: Shortcut::Skip,
                browser_integration: BrowserIntegration::Skip,
            }
        );
        let defaults = parse_quiet_install(&args(&["--quiet"])).unwrap();
        assert_eq!(defaults.folder.path(), install::default_folder());
        assert_eq!(defaults.start_menu_shortcut, Shortcut::Create);
        assert_eq!(defaults.browser_integration, BrowserIntegration::Register);
    }

    #[test]
    fn quiet_install_refuses_a_relative_folder() {
        assert_eq!(
            parse_quiet_install(&args(&["--quiet", "--folder", "downpour"])),
            Err(UsageError::Folder(FolderError::Relative(PathBuf::from(
                "downpour"
            ))))
        );
        assert_eq!(InstallFolder::new(PathBuf::new()), Err(FolderError::Empty));
    }

    #[test]
    fn quiet_install_refuses_what_it_does_not_understand() {
        assert_eq!(
            parse_quiet_install(&args(&["--quiet", "--folder"])),
            Err(UsageError::MissingFolder)
        );
        // A flag where the folder should be is not a folder.
        assert_eq!(
            parse_quiet_install(&args(&["--folder", "--quiet"])),
            Err(UsageError::MissingFolder)
        );
        assert_eq!(
            parse_quiet_install(&args(&["--quiet", "--no-shortcut"])),
            Err(UsageError::UnknownOption("--no-shortcut".into()))
        );
    }

    #[test]
    fn quiet_uninstall_takes_no_options() {
        assert_eq!(
            parse_quiet_uninstall(&args(&["--uninstall", "--quiet"])),
            Ok(())
        );
        assert_eq!(
            parse_quiet_uninstall(&args(&["--uninstall", "--quiet", "--folder"])),
            Err(UsageError::UnknownOption("--folder".into()))
        );
    }
}
