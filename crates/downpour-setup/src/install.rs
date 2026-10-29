//! Installing and uninstalling, per user and without administrator rights.
//!
//! Every installed file is listed in a receipt next to the program. The
//! receipt is written before the first file and again at the end, so an
//! install that stops halfway can still be retried or uninstalled. The
//! uninstaller removes exactly the files on that list (plus what install
//! created elsewhere: shortcuts, the uninstall entry, the notification
//! registration, browser registration, and the app's start-with-Windows
//! entry), so it can never delete anything the user put there. Entries
//! outside the program folder are removed only when they still describe
//! this installation; anything left behind is reported.
//!
//! The program folder keeps one uninstaller, `Uninstall Downpour.exe`: a
//! copy of this setup program that opens the uninstall window when started
//! under that name.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use downpour_protocol::desktop::{is_within, same_path, APP_USER_MODEL_ID};
use serde::{Deserialize, Serialize};

use crate::payload::{PayloadFile, FILES};
use crate::report::{CleanupFailure, Log, LogLine, SetupReport};
use crate::running::{self, StillRunning, StopError};
use crate::shortcut::{self, ShortcutSpec};

pub const APP_NAME: &str = "Downpour";
/// This setup program's copy in the program folder; started under this
/// name, it uninstalls.
pub const UNINSTALLER_EXE: &str = "Uninstall Downpour.exe";
const UNINSTALLER_NAME: &str = "Uninstall Downpour";
const APP_EXE: &str = "downpour.exe";
const HOST_EXE: &str = "downpour-host.exe";
const RECEIPT_FILE: &str = "downpour-install.json";
const APP_SHORTCUT_FILE: &str = "Downpour.lnk";
const UNINSTALLER_SHORTCUT_FILE: &str = "Uninstall Downpour.lnk";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Downpour";
/// The uninstall entry's value naming the folder it describes.
const INSTALL_LOCATION_VALUE: &str = "InstallLocation";
/// The notification registration's value naming its icon file.
const ICON_URI_VALUE: &str = "IconUri";
/// The icon Windows shows on Downpour's notifications. It must be an image
/// file; an icon inside an .exe is not accepted there.
const NOTIFICATION_ICON_FILE: &str = "downpour.ico";
const NOTIFICATION_ICON: &[u8] = include_bytes!("../../../assets/downpour.ico");

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("this installer was built without the program files; build it with scripts\\build-installer.ps1")]
    EmptyPayload,
    #[error(transparent)]
    StillRunning(#[from] StillRunning),
    #[error(transparent)]
    Stop(#[from] StopError),
    #[error("{} already has other files in it. Choose an empty folder or the folder Downpour was installed in.", .0.display())]
    FolderNotEmpty(PathBuf),
    #[error("{} is not a Downpour installation (it has no {RECEIPT_FILE})", .0.display())]
    NotInstalled(PathBuf),
    #[error("{action} {}: {source}", .path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    #[error("registering browser integration failed: {0}")]
    BrowserIntegration(String),
    #[error("the receipt {} is unreadable: {source}", .path.display())]
    Receipt {
        path: PathBuf,
        source: serde_json::Error,
    },
}

fn io_error<'a>(action: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> SetupError + 'a {
    move |source| SetupError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

/// The folder to install into: a full path, so the install never lands
/// relative to whatever the current folder happens to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallFolder(PathBuf);

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FolderError {
    #[error("choose a folder to install into")]
    Empty,
    #[error("{} is not a full path; give one that starts with a drive, such as C:\\", .0.display())]
    Relative(PathBuf),
}

impl InstallFolder {
    pub fn new(path: PathBuf) -> Result<InstallFolder, FolderError> {
        if path.as_os_str().is_empty() {
            return Err(FolderError::Empty);
        }
        if !path.is_absolute() {
            return Err(FolderError::Relative(path));
        }
        Ok(InstallFolder(path))
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// For tests with paths that are not absolute on every platform.
    #[cfg(test)]
    pub(crate) fn assume_absolute(path: PathBuf) -> InstallFolder {
        InstallFolder(path)
    }
}

/// What the user chose on the first page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallOptions {
    pub folder: InstallFolder,
    pub start_menu_shortcut: Shortcut,
    pub desktop_shortcut: Shortcut,
    pub browser_integration: BrowserIntegration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    Create,
    Skip,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserIntegration {
    Register,
    Skip,
}

/// What to do with the download list and settings when uninstalling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserData {
    Keep,
    Remove,
}

/// Written next to the program; lists what install created.
#[derive(Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Receipt {
    version: String,
    files: Vec<String>,
    shortcuts: Vec<PathBuf>,
    browser_integration: bool,
}

pub fn default_folder() -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.data_local_dir().join("Programs").join("Downpour"))
        .unwrap_or_else(|| PathBuf::from(r"C:\Downpour"))
}

pub fn has_payload() -> bool {
    !FILES.is_empty()
}

pub fn installed_folder() -> Option<PathBuf> {
    let candidate = default_folder();
    candidate.join(RECEIPT_FILE).is_file().then_some(candidate)
}

/// The folder the install page starts with: the existing installation's,
/// or the default.
pub fn suggested_folder() -> PathBuf {
    installed_folder().unwrap_or_else(default_folder)
}

/// The installation to remove: the folder this uninstaller runs from when
/// it holds a receipt, otherwise the default folder's installation.
pub fn folder_to_uninstall() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .filter(|dir| dir.join(RECEIPT_FILE).is_file())
        .or_else(installed_folder)
}

/// A shortcut install creates, decided before anything is written.
#[derive(Debug, PartialEq, Eq)]
struct PlannedShortcut {
    /// Where the `.lnk` file goes.
    link: PathBuf,
    /// The program it starts.
    target: PathBuf,
    /// The name Windows shows for it.
    name: &'static str,
    /// Set on shortcuts to the app, so a pinned shortcut groups with its
    /// window and notifications are known to come from it.
    app_user_model_id: Option<&'static str>,
    /// What the install log says once it exists.
    done: &'static str,
}

/// The shortcuts `options` asks for, in the Start menu folder and on the
/// desktop given. The Start menu gets the uninstaller next to the app.
fn planned_shortcuts(
    options: &InstallOptions,
    start_menu: Option<&Path>,
    desktop: Option<&Path>,
) -> Vec<PlannedShortcut> {
    let folder = options.folder.path();
    let mut planned = Vec::new();
    if let (Shortcut::Create, Some(start_menu)) = (options.start_menu_shortcut, start_menu) {
        planned.push(PlannedShortcut {
            link: start_menu.join(APP_SHORTCUT_FILE),
            target: folder.join(APP_EXE),
            name: APP_NAME,
            app_user_model_id: Some(APP_USER_MODEL_ID),
            done: "Added Downpour to the Start menu",
        });
        planned.push(PlannedShortcut {
            link: start_menu.join(UNINSTALLER_SHORTCUT_FILE),
            target: folder.join(UNINSTALLER_EXE),
            name: UNINSTALLER_NAME,
            app_user_model_id: None,
            done: "Added Uninstall Downpour to the Start menu",
        });
    }
    if let (Shortcut::Create, Some(desktop)) = (options.desktop_shortcut, desktop) {
        planned.push(PlannedShortcut {
            link: desktop.join(APP_SHORTCUT_FILE),
            target: folder.join(APP_EXE),
            name: APP_NAME,
            app_user_model_id: Some(APP_USER_MODEL_ID),
            done: "Added a desktop shortcut",
        });
    }
    planned
}

/// Whether a shortcut a receipt lists is one install creates: a Downpour
/// shortcut file directly in the Start menu or desktop folder. The receipt
/// is a file anyone can edit, so other paths are never deleted.
fn is_our_shortcut(path: &Path, start_menu: Option<&Path>, desktop: Option<&Path>) -> bool {
    let has_our_name = path.file_name().is_some_and(|name| {
        [APP_SHORTCUT_FILE, UNINSTALLER_SHORTCUT_FILE]
            .iter()
            .any(|ours| name.eq_ignore_ascii_case(ours))
    });
    let in_our_folder = path.parent().is_some_and(|parent| {
        [start_menu, desktop]
            .into_iter()
            .flatten()
            .any(|folder| same_path(parent, folder))
    });
    has_our_name && in_our_folder
}

/// The files an install of `payload` leaves in the program folder: the
/// payload, the uninstaller, and the notification icon.
fn installed_files<'a>(payload: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    payload
        .into_iter()
        .chain([UNINSTALLER_EXE, NOTIFICATION_ICON_FILE])
        .map(str::to_owned)
        .collect()
}

/// The receipt written before an install changes anything: everything the
/// previous install and this one list, so whatever exists if the install
/// stops halfway is on it.
fn receipt_while_installing(previous: Option<&Receipt>, planned: &Receipt) -> Receipt {
    let mut files: Vec<String> = previous
        .map(|receipt| receipt.files.clone())
        .unwrap_or_default();
    for file in &planned.files {
        if !files.contains(file) {
            files.push(file.clone());
        }
    }
    let mut shortcuts: Vec<PathBuf> = previous
        .map(|receipt| receipt.shortcuts.clone())
        .unwrap_or_default();
    for shortcut in &planned.shortcuts {
        if !shortcuts.contains(shortcut) {
            shortcuts.push(shortcut.clone());
        }
    }
    Receipt {
        version: planned.version.clone(),
        files,
        shortcuts,
        browser_integration: planned.browser_integration
            || previous.is_some_and(|receipt| receipt.browser_integration),
    }
}

/// Installs or upgrades. Each step is reported through `log`.
pub fn install(options: &InstallOptions, log: &mut Log<'_>) -> Result<SetupReport, SetupError> {
    if !has_payload() {
        return Err(SetupError::EmptyPayload);
    }
    let folder = options.folder.path();
    let previous = load_receipt(folder)?;
    if previous.is_none() && folder_has_files(folder) {
        return Err(SetupError::FolderNotEmpty(folder.to_path_buf()));
    }
    let (start_menu, desktop) = (start_menu_dir(), desktop_dir());
    let shortcuts = planned_shortcuts(options, start_menu.as_deref(), desktop.as_deref());
    let receipt = Receipt {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        files: installed_files(FILES.iter().map(|file| file.path)),
        shortcuts: shortcuts
            .iter()
            .map(|shortcut| shortcut.link.clone())
            .collect(),
        browser_integration: options.browser_integration == BrowserIntegration::Register,
    };
    let listed = receipt_while_installing(previous.as_ref(), &receipt);
    let programs = running::installation_programs(folder, listed.files.iter().map(String::as_str));
    running::close_programs(&programs, log)?;

    std::fs::create_dir_all(folder).map_err(io_error("create", folder))?;
    save_receipt(folder, &listed)?;
    for file in FILES {
        write_payload_file(folder, file)?;
    }
    log(LogLine::Done(format!(
        "Copied {} files to {}",
        FILES.len(),
        folder.display()
    )));
    copy_uninstaller(folder)?;
    write_payload_file(
        folder,
        &PayloadFile {
            path: NOTIFICATION_ICON_FILE,
            bytes: NOTIFICATION_ICON,
        },
    )?;
    for shortcut in &shortcuts {
        create_shortcut(shortcut)?;
        log(LogLine::Done(shortcut.done.to_owned()));
    }
    let mut report = SetupReport::default();
    if let Some(previous) = &previous {
        // An older install's own uninstaller (or a shortcut no longer
        // wanted) goes too, so the folder never holds two uninstallers.
        let dirs = ShortcutFolders {
            start_menu: start_menu.as_deref(),
            desktop: desktop.as_deref(),
        };
        remove_obsolete(folder, previous, &receipt, dirs, &mut report, log);
    }
    if options.browser_integration == BrowserIntegration::Register {
        run_host(folder, "install")?;
        log(LogLine::Done(
            "Registered browser integration for Firefox".to_owned(),
        ));
    }
    save_receipt(folder, &receipt)?;
    registry::register_notifications(folder)?;
    log(LogLine::Done(
        "Registered Downpour for Windows notifications".to_owned(),
    ));
    registry::register_uninstaller(folder)?;
    log(LogLine::Done(
        "Added Downpour to Windows' installed apps".to_owned(),
    ));
    Ok(report)
}

/// Where Downpour's shortcuts may live.
#[derive(Clone, Copy, Debug)]
struct ShortcutFolders<'a> {
    start_menu: Option<&'a Path>,
    desktop: Option<&'a Path>,
}

/// Removes what [`install`] created. Downloaded files are never touched.
pub fn uninstall(
    folder: &Path,
    user_data: UserData,
    log: &mut Log<'_>,
) -> Result<SetupReport, SetupError> {
    let receipt =
        load_receipt(folder)?.ok_or_else(|| SetupError::NotInstalled(folder.to_path_buf()))?;
    let programs = running::installation_programs(folder, receipt.files.iter().map(String::as_str));
    running::close_programs(&programs, log)?;

    let mut report = SetupReport::default();
    if receipt.browser_integration {
        match run_host(folder, "uninstall") {
            Ok(()) => log(LogLine::Done("Removed browser integration".to_owned())),
            // Keep going: leaving registry keys is better than leaving everything.
            Err(error) => report.record(
                CleanupFailure {
                    what: "browser integration",
                    location: folder.join(HOST_EXE).display().to_string(),
                    error: error.to_string(),
                },
                log,
            ),
        }
    }
    let (start_menu, desktop) = (start_menu_dir(), desktop_dir());
    let dirs = ShortcutFolders {
        start_menu: start_menu.as_deref(),
        desktop: desktop.as_deref(),
    };
    for shortcut in &receipt.shortcuts {
        remove_shortcut(shortcut, dirs, &mut report, log);
    }
    report_registration(
        "start-with-Windows entry",
        downpour_protocol::desktop::RUN_KEY,
        registry::remove_sign_in_start(folder),
        &mut report,
        log,
    );
    report_registration(
        "notification registration",
        downpour_protocol::desktop::APP_USER_MODEL_ID_KEY,
        registry::remove_key_if_registered_for(
            downpour_protocol::desktop::APP_USER_MODEL_ID_KEY,
            ICON_URI_VALUE,
            folder,
        ),
        &mut report,
        log,
    );
    report_registration(
        "installed-apps entry",
        UNINSTALL_KEY,
        registry::remove_key_if_registered_for(UNINSTALL_KEY, INSTALL_LOCATION_VALUE, folder),
        &mut report,
        log,
    );
    if user_data == UserData::Remove {
        remove_user_data(&mut report, log);
    }
    remove_program_files(folder, &receipt, &mut report, log);
    Ok(report)
}

/// How a registry entry fared on uninstall.
#[derive(Debug, PartialEq, Eq)]
enum Registration {
    Absent,
    Removed,
    /// It describes another installation, so it stays.
    OtherCopy(String),
    /// It does not say which installation it belongs to, so it stays.
    Unproven {
        reason: String,
    },
}

/// Whether a registry value naming a file or folder (`InstallLocation`,
/// `IconUri`, a `Run` command's program) describes the installation in
/// `folder`.
fn registered_for(folder: &Path, value: &Path) -> bool {
    is_within(value, folder)
}

fn report_registration(
    what: &'static str,
    location: &str,
    result: io::Result<Registration>,
    report: &mut SetupReport,
    log: &mut Log<'_>,
) {
    match result {
        Ok(Registration::Removed) => log(LogLine::Done(format!("Removed the {what}"))),
        Ok(Registration::Absent) => {}
        Ok(Registration::OtherCopy(value)) => log(LogLine::Warning(format!(
            "Left the {what}: it belongs to another copy ({value})"
        ))),
        Ok(Registration::Unproven { reason }) => log(LogLine::Warning(format!(
            "Left the {what}: cannot tell which installation it belongs to ({reason})"
        ))),
        Err(error) => report.record(
            CleanupFailure {
                what,
                location: location.to_owned(),
                error: error.to_string(),
            },
            log,
        ),
    }
}

fn remove_shortcut(
    shortcut: &Path,
    dirs: ShortcutFolders<'_>,
    report: &mut SetupReport,
    log: &mut Log<'_>,
) {
    if !is_our_shortcut(shortcut, dirs.start_menu, dirs.desktop) {
        log(LogLine::Warning(format!(
            "Skipped {}: not a Downpour shortcut in the Start menu or on the desktop",
            shortcut.display()
        )));
        return;
    }
    match remove_if_present(shortcut) {
        Ok(()) => log(LogLine::Done(format!("Removed {}", shortcut.display()))),
        Err(error) => report.record(
            CleanupFailure {
                what: "shortcut",
                location: shortcut.display().to_string(),
                error: error.to_string(),
            },
            log,
        ),
    }
}

fn remove_user_data(report: &mut SetupReport, log: &mut Log<'_>) {
    let Some(data) = downpour_protocol::data_dir() else {
        log(LogLine::Warning(
            "Could not find the download list and settings to remove".to_owned(),
        ));
        return;
    };
    match std::fs::remove_dir_all(&data) {
        Ok(()) => log(LogLine::Done(format!(
            "Removed the download list and settings in {}",
            data.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => report.record(
            CleanupFailure {
                what: "download list and settings",
                location: data.display().to_string(),
                error: error.to_string(),
            },
            log,
        ),
    }
}

fn remove_program_files(
    folder: &Path,
    receipt: &Receipt,
    report: &mut SetupReport,
    log: &mut Log<'_>,
) {
    let running_exe = std::env::current_exe().ok();
    let mut removed = 0usize;
    for relative in &receipt.files {
        // The receipt is a file on disk anyone can edit; only paths inside
        // the install folder are ever deleted.
        if !is_inside_folder(Path::new(relative)) {
            log(LogLine::Warning(format!(
                "Skipped {relative}: not a file inside the install folder"
            )));
            continue;
        }
        let path = folder.join(relative);
        // The uninstaller cannot delete itself while running; see below.
        if running_exe
            .as_deref()
            .is_some_and(|exe| same_file(exe, &path))
        {
            continue;
        }
        match remove_if_present(&path) {
            Ok(()) => removed += 1,
            Err(error) => report.record(
                CleanupFailure {
                    what: "program file",
                    location: path.display().to_string(),
                    error: error.to_string(),
                },
                log,
            ),
        }
    }
    let receipt_path = folder.join(RECEIPT_FILE);
    if let Err(error) = remove_if_present(&receipt_path) {
        report.record(
            CleanupFailure {
                what: "install receipt",
                location: receipt_path.display().to_string(),
                error: error.to_string(),
            },
            log,
        );
    }
    // Only this installation's own uninstaller removes itself; a copy of
    // the setup program run from elsewhere (or a stray copy in the folder)
    // is never deleted.
    let programs = running::installation_programs(folder, receipt.files.iter().map(String::as_str));
    let is_installed_uninstaller = running_exe
        .as_deref()
        .is_some_and(|exe| running::is_one_of(exe, &programs));
    let root = match is_installed_uninstaller {
        true => RootFolder::LeaveForSelfRemoval,
        false => RootFolder::Remove,
    };
    for failure in remove_empty_dirs(folder, root) {
        report.record(failure, log);
    }
    log(LogLine::Done(format!(
        "Removed {removed} program files from {}",
        folder.display()
    )));
    if let (RootFolder::LeaveForSelfRemoval, Some(exe)) = (root, running_exe.as_deref()) {
        if let Err(error) = schedule_self_removal(exe, folder) {
            report.record(
                CleanupFailure {
                    what: "uninstaller",
                    location: exe.display().to_string(),
                    error: error.to_string(),
                },
                log,
            );
        }
    }
}

fn write_payload_file(folder: &Path, file: &PayloadFile) -> Result<(), SetupError> {
    let target = folder.join(file.path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(io_error("create", parent))?;
    }
    // Written beside the target and renamed, so an interrupted upgrade never
    // leaves a half-written program.
    let temporary = target.with_extension("dpnew");
    std::fs::write(&temporary, file.bytes).map_err(io_error("write", &temporary))?;
    std::fs::rename(&temporary, &target).map_err(io_error("replace", &target))
}

fn copy_uninstaller(folder: &Path) -> Result<(), SetupError> {
    let source =
        std::env::current_exe().map_err(io_error("locate", Path::new("the setup program")))?;
    let target = folder.join(UNINSTALLER_EXE);
    if same_file(&source, &target) {
        return Ok(());
    }
    std::fs::copy(&source, &target)
        .map(drop)
        .map_err(io_error("copy", &target))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// What an earlier install created that the current one no longer has.
#[derive(Debug, PartialEq, Eq)]
struct Obsolete<'a> {
    /// Relative to the install folder.
    files: Vec<&'a str>,
    shortcuts: Vec<&'a Path>,
}

fn obsolete<'a>(previous: &'a Receipt, current: &Receipt) -> Obsolete<'a> {
    Obsolete {
        files: previous
            .files
            .iter()
            .filter(|old| !current.files.contains(old))
            .map(String::as_str)
            .collect(),
        shortcuts: previous
            .shortcuts
            .iter()
            .filter(|old| !current.shortcuts.contains(old))
            .map(PathBuf::as_path)
            .collect(),
    }
}

fn remove_obsolete(
    folder: &Path,
    previous: &Receipt,
    current: &Receipt,
    dirs: ShortcutFolders<'_>,
    report: &mut SetupReport,
    log: &mut Log<'_>,
) {
    let obsolete = obsolete(previous, current);
    for old in obsolete.files {
        // Same rule as uninstalling: an edited receipt never reaches
        // outside the install folder.
        if !is_inside_folder(Path::new(old)) {
            log(LogLine::Warning(format!(
                "Skipped {old}: not a file inside the install folder"
            )));
            continue;
        }
        let path = folder.join(old);
        if let Err(error) = remove_if_present(&path) {
            report.record(
                CleanupFailure {
                    what: "old program file",
                    location: path.display().to_string(),
                    error: error.to_string(),
                },
                log,
            );
        }
    }
    for old in obsolete.shortcuts {
        remove_shortcut(old, dirs, report, log);
    }
}

fn folder_has_files(folder: &Path) -> bool {
    std::fs::read_dir(folder).is_ok_and(|mut entries| entries.next().is_some())
}

fn load_receipt(folder: &Path) -> Result<Option<Receipt>, SetupError> {
    let path = folder.join(RECEIPT_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error("read", &path)(error)),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|source| SetupError::Receipt { path, source })
}

fn save_receipt(folder: &Path, receipt: &Receipt) -> Result<(), SetupError> {
    let path = folder.join(RECEIPT_FILE);
    let json = serde_json::to_vec_pretty(receipt).expect("receipts always serialize");
    std::fs::write(&path, json).map_err(io_error("write", &path))
}

/// Whether `relative` stays inside the folder it is joined to: no root,
/// drive, or `..` part.
pub(crate) fn is_inside_folder(relative: &Path) -> bool {
    relative.components().all(|part| {
        matches!(
            part,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    })
}

/// Removes a file; one that is already gone counts as removed.
fn remove_if_present(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// What happens to the install folder itself once its contents are gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RootFolder {
    Remove,
    /// The running uninstaller is still in it; the self-removal helper
    /// removes both once this process has exited.
    LeaveForSelfRemoval,
}

/// Removes now-empty folders bottom-up, and `folder` itself unless `root`
/// says otherwise. A folder the user put other files in stays, and is
/// reported.
fn remove_empty_dirs(folder: &Path, root: RootFolder) -> Vec<CleanupFailure> {
    let mut failures = Vec::new();
    match std::fs::read_dir(folder) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) if entry.path().is_dir() => {
                        failures.extend(remove_empty_dirs(&entry.path(), RootFolder::Remove));
                    }
                    Ok(_) => {}
                    Err(error) => failures.push(folder_failure(folder, &error)),
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return failures,
        Err(error) => failures.push(folder_failure(folder, &error)),
    }
    if root == RootFolder::Remove {
        match std::fs::remove_dir(folder) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => failures.push(folder_failure(folder, &error)),
        }
    }
    failures
}

fn folder_failure(folder: &Path, error: &io::Error) -> CleanupFailure {
    let error = match error.kind() {
        io::ErrorKind::DirectoryNotEmpty => {
            "it still holds files Downpour did not install".to_owned()
        }
        _ => error.to_string(),
    };
    CleanupFailure {
        what: "folder",
        location: folder.display().to_string(),
        error,
    }
}

/// Deletion attempts, a second apart, in case something (a virus scanner)
/// still holds the uninstaller open after it exits.
const SELF_REMOVAL_ATTEMPTS: u32 = 10;
/// The variables the helper reads the paths from. Paths never appear in
/// the script itself, so no quote character in a folder name can end a
/// string or run as code.
const REMOVE_EXE_VARIABLE: &str = "DOWNPOUR_REMOVE_EXE";
const REMOVE_FOLDER_VARIABLE: &str = "DOWNPOUR_REMOVE_FOLDER";

/// This process, as the helper can recognize it: its number and its
/// creation time (a FILETIME), since a number alone may be reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessIdentity {
    pid: u32,
    created_filetime: u64,
}

/// The helper's command, decided before anything runs.
#[derive(Debug, PartialEq, Eq)]
struct SelfRemoval {
    program: PathBuf,
    args: Vec<String>,
    env: [(&'static str, PathBuf); 2],
}

/// The PowerShell script that waits for this uninstaller to exit, then
/// deletes it and its then-empty folder. If the process number now belongs
/// to another process (its start time differs), or is gone, it does not
/// wait.
fn self_removal_script(process: ProcessIdentity) -> String {
    let ProcessIdentity {
        pid,
        created_filetime,
    } = process;
    format!(
        "$uninstaller = Get-Process -Id {pid} -ErrorAction SilentlyContinue; \
         if ($uninstaller -and $uninstaller.StartTime.ToFileTimeUtc() -eq {created_filetime}) {{ $uninstaller.WaitForExit() }}; \
         foreach ($attempt in 1..{SELF_REMOVAL_ATTEMPTS}) {{ \
         Remove-Item -LiteralPath $env:{REMOVE_EXE_VARIABLE} -Force -ErrorAction SilentlyContinue; \
         if (-not (Test-Path -LiteralPath $env:{REMOVE_EXE_VARIABLE})) {{ break }}; Start-Sleep -Seconds 1 }}; \
         Remove-Item -LiteralPath $env:{REMOVE_FOLDER_VARIABLE} -ErrorAction SilentlyContinue"
    )
}

fn plan_self_removal(
    system_dir: &Path,
    process: ProcessIdentity,
    exe: &Path,
    folder: &Path,
) -> SelfRemoval {
    SelfRemoval {
        program: system_dir
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe"),
        args: [
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &self_removal_script(process),
        ]
        .map(str::to_owned)
        .to_vec(),
        env: [
            (REMOVE_EXE_VARIABLE, exe.to_path_buf()),
            (REMOVE_FOLDER_VARIABLE, folder.to_path_buf()),
        ],
    }
}

/// The running uninstaller cannot delete itself, so a hidden PowerShell
/// (from the system folder, never one found next to this program) waits
/// for this process to exit, however long the window stays open, and then
/// deletes it and the then-empty folder. Only starting the helper can be
/// reported: once this process has exited, nobody is left to hear about
/// the helper's own failures.
fn schedule_self_removal(exe: &Path, folder: &Path) -> io::Result<()> {
    let plan = plan_self_removal(&native::system_dir()?, native::this_process()?, exe, folder);
    if !plan.program.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is missing", plan.program.display()),
        ));
    }
    let mut command = Command::new(&plan.program);
    command
        .args(&plan.args)
        .envs(plan.env.iter().map(|(name, value)| (name, value)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().map(drop)
}

#[cfg(windows)]
mod native {
    use std::io;
    use std::path::PathBuf;

    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::SystemInformation::GetSystemDirectoryW;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    use super::ProcessIdentity;

    /// Room for any system folder path.
    const MAX_SYSTEM_DIR: usize = 1024;

    pub fn system_dir() -> io::Result<PathBuf> {
        let mut buffer = vec![0u16; MAX_SYSTEM_DIR];
        // SAFETY: the buffer is writable for its whole length.
        let length = unsafe { GetSystemDirectoryW(Some(&mut buffer)) } as usize;
        if length == 0 || length > buffer.len() {
            return Err(io::Error::last_os_error());
        }
        buffer.truncate(length);
        Ok(PathBuf::from(String::from_utf16_lossy(&buffer)))
    }

    pub fn this_process() -> io::Result<ProcessIdentity> {
        let (mut created, mut exited, mut kernel, mut user) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        // SAFETY: the pseudo handle of this process needs no closing, and
        // each FILETIME is writable.
        unsafe {
            GetProcessTimes(
                GetCurrentProcess(),
                &mut created,
                &mut exited,
                &mut kernel,
                &mut user,
            )
        }
        .map_err(io::Error::other)?;
        Ok(ProcessIdentity {
            pid: std::process::id(),
            created_filetime: (u64::from(created.dwHighDateTime) << 32)
                | u64::from(created.dwLowDateTime),
        })
    }
}

#[cfg(not(windows))]
mod native {
    use std::io;
    use std::path::PathBuf;

    use super::ProcessIdentity;

    pub fn system_dir() -> io::Result<PathBuf> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "self-removal needs Windows",
        ))
    }

    pub fn this_process() -> io::Result<ProcessIdentity> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "self-removal needs Windows",
        ))
    }
}

fn run_host(folder: &Path, action: &str) -> Result<(), SetupError> {
    let host = folder.join(HOST_EXE);
    let output = Command::new(&host)
        .args([action, "--yes"])
        .output()
        .map_err(io_error("run", &host))?;
    if output.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    Err(SetupError::BrowserIntegration(if text.is_empty() {
        format!("{} exited with {}", host.display(), output.status)
    } else {
        text
    }))
}

/// Runs `downpour --add-to-firefox`; returns what the user should do next.
pub fn add_to_firefox(folder: &Path) -> Result<String, String> {
    let app = folder.join(APP_EXE);
    let output = Command::new(&app)
        .arg(downpour_protocol::ADD_TO_FIREFOX_FLAG)
        .output()
        .map_err(|error| format!("Could not run {}: {error}", app.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if output.status.success() {
        Ok(stdout)
    } else {
        Err(if stderr.is_empty() { stdout } else { stderr })
    }
}

pub fn launch_app(folder: &Path) -> io::Result<()> {
    Command::new(folder.join(APP_EXE)).spawn().map(drop)
}

fn start_menu_dir() -> Option<PathBuf> {
    directories::BaseDirs::new().map(|dirs| {
        dirs.config_dir()
            .join("Microsoft")
            .join("Windows")
            .join("Start Menu")
            .join("Programs")
    })
}

fn desktop_dir() -> Option<PathBuf> {
    directories::UserDirs::new().and_then(|dirs| dirs.desktop_dir().map(Path::to_path_buf))
}

#[cfg(windows)]
fn create_shortcut(planned: &PlannedShortcut) -> Result<(), SetupError> {
    let link = &planned.link;
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).map_err(io_error("create", parent))?;
    }
    shortcut::create(&shortcut_spec(planned))
        .map_err(|error| io_error("create shortcut", link)(io::Error::other(error)))
}

fn shortcut_spec(planned: &PlannedShortcut) -> ShortcutSpec<'_> {
    ShortcutSpec {
        link: &planned.link,
        target: &planned.target,
        working_dir: planned.target.parent(),
        description: planned.name,
        app_user_model_id: planned.app_user_model_id,
    }
}

#[cfg(not(windows))]
fn create_shortcut(_planned: &PlannedShortcut) -> Result<(), SetupError> {
    Ok(())
}

/// The uninstall entry's text values for the installation in `folder`.
fn uninstall_values(folder: &Path) -> [(&'static str, String); 7] {
    let uninstaller = folder.join(UNINSTALLER_EXE);
    [
        ("DisplayName", APP_NAME.to_owned()),
        ("DisplayVersion", env!("CARGO_PKG_VERSION").to_owned()),
        (
            "Publisher",
            downpour_protocol::desktop::PUBLISHER.to_owned(),
        ),
        (INSTALL_LOCATION_VALUE, folder.display().to_string()),
        ("DisplayIcon", folder.join(APP_EXE).display().to_string()),
        (
            "UninstallString",
            format!("\"{}\" --uninstall", uninstaller.display()),
        ),
        (
            "QuietUninstallString",
            format!("\"{}\" --uninstall --quiet", uninstaller.display()),
        ),
    ]
}

/// The values that name Downpour and its icon on its notifications.
fn notification_values(folder: &Path) -> [(&'static str, String); 2] {
    [
        ("DisplayName", APP_NAME.to_owned()),
        (
            ICON_URI_VALUE,
            folder.join(NOTIFICATION_ICON_FILE).display().to_string(),
        ),
    ]
}

/// The registry, per user. Reads decide; writes happen only here.
#[cfg(windows)]
mod registry {
    use std::io;
    use std::path::{Path, PathBuf};

    use downpour_protocol::desktop::{
        program_of_command, APP_USER_MODEL_ID_KEY, RUN_KEY, RUN_VALUE,
    };
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE};
    use winreg::RegKey;

    use super::{
        notification_values, registered_for, uninstall_values, Registration, SetupError, FILES,
        UNINSTALL_KEY,
    };

    fn write_values(
        key_path: &'static str,
        action: &'static str,
        text: &[(&str, String)],
        numbers: &[(&str, u32)],
    ) -> Result<(), SetupError> {
        let key_error = |source: io::Error| SetupError::Io {
            action,
            path: PathBuf::from(key_path),
            source,
        };
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER)
            .create_subkey(key_path)
            .map_err(key_error)?;
        for (name, value) in text {
            key.set_value(name, value).map_err(key_error)?;
        }
        for (name, value) in numbers {
            key.set_value(name, value).map_err(key_error)?;
        }
        Ok(())
    }

    /// Lists Downpour under Settings > Apps, with an Uninstall button that
    /// runs the uninstaller kept in the program folder.
    pub fn register_uninstaller(folder: &Path) -> Result<(), SetupError> {
        let size_kib: u32 = FILES
            .iter()
            .map(|file| file.bytes.len() as u64)
            .sum::<u64>()
            .div_ceil(1024)
            .try_into()
            .unwrap_or(u32::MAX);
        write_values(
            UNINSTALL_KEY,
            "write the uninstall entry",
            &uninstall_values(folder),
            &[
                ("NoModify", 1),
                ("NoRepair", 1),
                ("EstimatedSize", size_kib),
            ],
        )
    }

    pub fn register_notifications(folder: &Path) -> Result<(), SetupError> {
        write_values(
            APP_USER_MODEL_ID_KEY,
            "write the notification registration",
            &notification_values(folder),
            &[],
        )
    }

    /// Deletes the key when its `value` names something in `folder`.
    pub fn remove_key_if_registered_for(
        key_path: &str,
        value: &str,
        folder: &Path,
    ) -> io::Result<Registration> {
        let root = RegKey::predef(HKEY_CURRENT_USER);
        let key = match root.open_subkey_with_flags(key_path, KEY_READ) {
            Ok(key) => key,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Registration::Absent)
            }
            Err(error) => return Err(error),
        };
        let named: String = match key.get_value(value) {
            Ok(named) => named,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Registration::Unproven {
                    reason: format!("it has no {value}"),
                })
            }
            Err(error) => return Err(error),
        };
        if !registered_for(folder, Path::new(&named)) {
            return Ok(Registration::OtherCopy(named));
        }
        drop(key);
        root.delete_subkey_all(key_path)?;
        Ok(Registration::Removed)
    }

    /// Deletes the app's `Run` value when it starts a program from `folder`.
    pub fn remove_sign_in_start(folder: &Path) -> io::Result<Registration> {
        let key = match RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_READ | KEY_SET_VALUE)
        {
            Ok(key) => key,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Registration::Absent)
            }
            Err(error) => return Err(error),
        };
        let command: String = match key.get_value(RUN_VALUE) {
            Ok(command) => command,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Registration::Absent)
            }
            Err(error) => return Err(error),
        };
        let ours =
            program_of_command(&command).is_some_and(|program| registered_for(folder, &program));
        if !ours {
            return Ok(Registration::OtherCopy(command));
        }
        key.delete_value(RUN_VALUE)?;
        Ok(Registration::Removed)
    }
}

#[cfg(not(windows))]
mod registry {
    use std::io;
    use std::path::Path;

    use super::{Registration, SetupError};

    pub fn register_uninstaller(_folder: &Path) -> Result<(), SetupError> {
        Ok(())
    }

    pub fn register_notifications(_folder: &Path) -> Result<(), SetupError> {
        Ok(())
    }

    pub fn remove_key_if_registered_for(
        _key_path: &str,
        _value: &str,
        _folder: &Path,
    ) -> io::Result<Registration> {
        Ok(Registration::Absent)
    }

    pub fn remove_sign_in_start(_folder: &Path) -> io::Result<Registration> {
        Ok(Registration::Absent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_paths_must_stay_in_the_install_folder() {
        assert!(is_inside_folder(Path::new("downpour.exe")));
        assert!(is_inside_folder(Path::new(
            "extensions/downpour-firefox.xpi"
        )));
        assert!(!is_inside_folder(Path::new("../outside.txt")));
        assert!(!is_inside_folder(Path::new("extensions/../../outside.txt")));
        assert!(!is_inside_folder(Path::new("/etc/passwd")));
        if cfg!(windows) {
            assert!(!is_inside_folder(Path::new(r"C:\Windows\win.ini")));
            assert!(!is_inside_folder(Path::new(r"\\server\share\x")));
        }
    }

    fn options(start_menu: Shortcut, desktop: Shortcut) -> InstallOptions {
        InstallOptions {
            folder: InstallFolder::assume_absolute(PathBuf::from("/programs/downpour")),
            start_menu_shortcut: start_menu,
            desktop_shortcut: desktop,
            browser_integration: BrowserIntegration::Skip,
        }
    }

    #[test]
    fn the_start_menu_gets_the_uninstaller_next_to_the_app() {
        let start_menu = Path::new("/start-menu");
        let desktop = Path::new("/desktop");
        let folder = Path::new("/programs/downpour");
        let planned = planned_shortcuts(
            &options(Shortcut::Create, Shortcut::Create),
            Some(start_menu),
            Some(desktop),
        );
        let links: Vec<(PathBuf, PathBuf)> = planned
            .iter()
            .map(|shortcut| (shortcut.link.clone(), shortcut.target.clone()))
            .collect();
        assert_eq!(
            links,
            [
                (start_menu.join(APP_SHORTCUT_FILE), folder.join(APP_EXE)),
                (
                    start_menu.join(UNINSTALLER_SHORTCUT_FILE),
                    folder.join(UNINSTALLER_EXE)
                ),
                (desktop.join(APP_SHORTCUT_FILE), folder.join(APP_EXE)),
            ]
        );
        assert_eq!(planned[1].name, "Uninstall Downpour");
        // The app's shortcuts carry its AppUserModelID; the uninstaller's
        // does not, or the taskbar would group it with the app.
        let ids: Vec<Option<&str>> = planned
            .iter()
            .map(|shortcut| shortcut.app_user_model_id)
            .collect();
        assert_eq!(
            ids,
            [Some(APP_USER_MODEL_ID), None, Some(APP_USER_MODEL_ID)]
        );
        let spec = shortcut_spec(&planned[0]);
        assert_eq!(spec.working_dir, Some(folder));
        assert_eq!(spec.description, APP_NAME);
    }

    #[test]
    fn only_downpour_shortcuts_in_the_start_menu_or_on_the_desktop_are_ours() {
        let start_menu = Path::new("/start-menu");
        let desktop = Path::new("/desktop");
        let ours = |path: &str| is_our_shortcut(Path::new(path), Some(start_menu), Some(desktop));
        assert!(ours("/start-menu/Downpour.lnk"));
        assert!(ours("/start-menu/Uninstall Downpour.lnk"));
        assert!(ours("/desktop/downpour.LNK"));
        // An edited receipt cannot point the uninstaller anywhere else.
        assert!(!ours("/desktop/My notes.lnk"));
        assert!(!ours("/start-menu/Games/Downpour.lnk"));
        assert!(!ours("/documents/Downpour.lnk"));
        assert!(!is_our_shortcut(
            Path::new("/desktop/Downpour.lnk"),
            None,
            None
        ));
    }

    #[test]
    fn notifications_show_the_app_name_and_the_installed_icon() {
        let folder = Path::new("/programs/downpour");
        let values = notification_values(folder);
        assert_eq!(values[0], ("DisplayName", "Downpour".to_owned()));
        assert_eq!(values[1].0, ICON_URI_VALUE);
        assert_eq!(
            PathBuf::from(&values[1].1),
            folder.join(NOTIFICATION_ICON_FILE)
        );
        assert!(installed_files([]).contains(&NOTIFICATION_ICON_FILE.to_owned()));
    }

    #[test]
    fn the_uninstall_entry_names_its_folder_and_publisher() {
        let folder = Path::new("/programs/downpour");
        let values = uninstall_values(folder);
        let value = |name: &str| {
            values
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(
            value(INSTALL_LOCATION_VALUE).map(PathBuf::from),
            Some(folder.to_path_buf())
        );
        assert_eq!(
            value("Publisher").as_deref(),
            Some(downpour_protocol::desktop::PUBLISHER)
        );
    }

    #[cfg(windows)]
    #[test]
    fn registry_entries_are_removed_only_for_this_installation() {
        use downpour_protocol::desktop::program_of_command;
        let folder = Path::new(r"C:\Users\Someone\AppData\Local\Programs\Downpour");
        assert!(registered_for(folder, folder));
        assert!(registered_for(folder, &folder.join(NOTIFICATION_ICON_FILE)));
        assert!(registered_for(
            folder,
            Path::new(r"c:\users\someone\appdata\local\programs\downpour")
        ));
        assert!(!registered_for(folder, Path::new(r"D:\Downpour")));
        let ours = downpour_protocol::desktop::run_at_sign_in_command(&folder.join(APP_EXE));
        let program = program_of_command(&ours).unwrap();
        assert!(registered_for(folder, &program));
        let dev =
            program_of_command(r#""D:\dev\downpour\target\debug\downpour.exe" --start-in-tray"#)
                .unwrap();
        assert!(!registered_for(folder, &dev));
    }

    #[test]
    fn without_the_start_menu_option_there_is_no_uninstall_shortcut() {
        let planned = planned_shortcuts(
            &options(Shortcut::Skip, Shortcut::Create),
            Some(Path::new("/start-menu")),
            Some(Path::new("/desktop")),
        );
        assert_eq!(planned.len(), 1);
        assert_eq!(
            planned[0].target,
            Path::new("/programs/downpour").join(APP_EXE)
        );
        let unknown_folders =
            planned_shortcuts(&options(Shortcut::Create, Shortcut::Create), None, None);
        assert!(unknown_folders.is_empty());
    }

    #[test]
    fn the_receipt_lists_the_uninstaller_with_the_payload() {
        let files = installed_files(["downpour.exe", "extensions/downpour-firefox.xpi"]);
        assert_eq!(
            files,
            [
                "downpour.exe",
                "extensions/downpour-firefox.xpi",
                UNINSTALLER_EXE,
                NOTIFICATION_ICON_FILE
            ]
        );
    }

    #[test]
    fn the_receipt_written_first_lists_both_the_old_and_the_new_install() {
        let previous = Receipt {
            version: "0.2.0".into(),
            files: vec!["downpour.exe".into(), "Downpour-Setup.exe".into()],
            shortcuts: vec![PathBuf::from("/desktop/Downpour.lnk")],
            browser_integration: true,
        };
        let planned = Receipt {
            version: "1.0.0".into(),
            files: installed_files(["downpour.exe"]),
            shortcuts: vec![PathBuf::from("/start-menu/Downpour.lnk")],
            browser_integration: false,
        };
        let early = receipt_while_installing(Some(&previous), &planned);
        assert_eq!(
            early.files,
            [
                "downpour.exe",
                "Downpour-Setup.exe",
                UNINSTALLER_EXE,
                NOTIFICATION_ICON_FILE
            ]
        );
        assert_eq!(
            early.shortcuts,
            [
                PathBuf::from("/desktop/Downpour.lnk"),
                PathBuf::from("/start-menu/Downpour.lnk")
            ]
        );
        assert!(early.browser_integration);
        assert_eq!(early.version, "1.0.0");
        // A first install lists exactly what it plans.
        assert_eq!(receipt_while_installing(None, &planned), planned);
    }

    #[test]
    fn an_upgrade_removes_the_old_setup_copy_and_dropped_shortcuts() {
        let old_desktop = PathBuf::from("/desktop/Downpour.lnk");
        let start_menu = PathBuf::from("/start-menu/Downpour.lnk");
        let previous = Receipt {
            version: "0.1.0".into(),
            files: vec!["downpour.exe".into(), "Downpour-Setup.exe".into()],
            shortcuts: vec![start_menu.clone(), old_desktop.clone()],
            browser_integration: true,
        };
        let current = Receipt {
            version: "0.2.0".into(),
            files: installed_files(["downpour.exe"]),
            shortcuts: vec![start_menu],
            browser_integration: true,
        };
        assert_eq!(
            obsolete(&previous, &current),
            Obsolete {
                files: vec!["Downpour-Setup.exe"],
                shortcuts: vec![old_desktop.as_path()],
            }
        );
    }

    #[test]
    fn the_self_removal_keeps_paths_out_of_the_script() {
        let exe = Path::new("/programs/O\u{2019}Brien's \"x\"/Uninstall Downpour.exe");
        let folder = Path::new("/programs/O\u{2019}Brien's \"x\"");
        let process = ProcessIdentity {
            pid: 4242,
            created_filetime: 133_000_000_000_000_000,
        };
        let plan = plan_self_removal(Path::new("/windows/system32"), process, exe, folder);
        assert_eq!(
            plan.program,
            Path::new("/windows/system32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")
        );
        assert_eq!(
            plan.env,
            [
                (REMOVE_EXE_VARIABLE, exe.to_path_buf()),
                (REMOVE_FOLDER_VARIABLE, folder.to_path_buf())
            ]
        );
        for arg in &plan.args {
            assert!(
                !arg.contains("Brien"),
                "a path reached the command line: {arg}"
            );
        }
        let script = plan.args.last().unwrap();
        assert!(script.starts_with("$uninstaller = Get-Process -Id 4242 "));
        assert!(script.contains("ToFileTimeUtc() -eq 133000000000000000"));
        assert!(script.contains("-LiteralPath $env:DOWNPOUR_REMOVE_EXE"));
        assert!(script.contains("-LiteralPath $env:DOWNPOUR_REMOVE_FOLDER"));
    }

    #[test]
    fn receipts_round_trip_through_json() {
        let receipt = Receipt {
            version: "0.2.0".into(),
            files: installed_files(["downpour.exe"]),
            shortcuts: vec![PathBuf::from("/start-menu/Uninstall Downpour.lnk")],
            browser_integration: false,
        };
        let json = serde_json::to_string(&receipt).unwrap();
        assert!(json.contains(UNINSTALLER_EXE));
        assert_eq!(serde_json::from_str::<Receipt>(&json).unwrap(), receipt);
    }
}
