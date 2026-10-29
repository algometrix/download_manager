//! Closing Downpour before its files are replaced or removed.
//!
//! The running app is first asked to quit over its IPC channel, and given
//! time to pause its downloads. Programs still running after that are
//! found only among the installation's own program files (the receipt's
//! files, or the payload on a first install): a process counts only when
//! its executable is exactly one of those paths, never because it sits
//! somewhere under the install folder or has a familiar name. They are
//! listed, and stopped only when the user agrees in the setup window, after
//! checking that the process still runs that same file.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use downpour_protocol::desktop::same_path;
use downpour_protocol::ipc::{Endpoint, IpcError};
use downpour_protocol::messages::{AppReply, AppRequest};

use crate::report::{Log, LogLine};

/// How long one IPC request may take.
const IPC_TIMEOUT: Duration = Duration::from_secs(2);
/// How long the app gets to pause its downloads and exit; it waits up to
/// five seconds for transfers itself.
const QUIT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a stopped program gets to end.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the app is asked whether it is still there, when its process
/// cannot be watched directly.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// A running process whose executable is one of the installation's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunningProgram {
    pub pid: u32,
    pub exe: PathBuf,
}

/// Why the installation cannot be changed yet.
#[derive(Debug, thiserror::Error)]
pub enum StillRunning {
    #[error("still running: {}", program_list(.0))]
    Programs(Vec<RunningProgram>),
    #[error("Downpour is running and did not close when asked ({0}). Quit it from its tray icon, then try again.")]
    AppDidNotQuit(NotClosed),
    #[error("listing running programs failed: {0}")]
    Listing(std::io::Error),
}

fn program_list(programs: &[RunningProgram]) -> String {
    programs
        .iter()
        .map(|program| format!("{} (process {})", program.exe.display(), program.pid))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Why asking the app to quit did not end it.
#[derive(Debug)]
pub enum NotClosed {
    /// The per-user data folder, where the app's IPC endpoint lives, is unknown.
    NoDataFolder,
    Ipc(IpcError),
    /// An older app that does not know the request answers this way.
    UnexpectedReply(AppReply),
    DidNotExit,
}

impl fmt::Display for NotClosed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NotClosed::NoDataFolder => write!(f, "cannot find Downpour's data folder"),
            NotClosed::Ipc(error) => write!(f, "{error}"),
            NotClosed::UnexpectedReply(reply) => {
                write!(
                    f,
                    "it answered {reply:?}; an older version may not know the request"
                )
            }
            NotClosed::DidNotExit => write!(
                f,
                "it did not exit within {} seconds",
                QUIT_TIMEOUT.as_secs()
            ),
        }
    }
}

/// What asking the app to quit achieved.
#[derive(Debug)]
pub enum QuitOutcome {
    NotRunning,
    Exited,
    StillRunning { reason: NotClosed },
}

/// The programs an installation consists of: its listed files that lie
/// inside `folder`. Only processes running exactly one of these are ever
/// listed or stopped.
pub fn installation_programs<'a>(
    folder: &Path,
    files: impl IntoIterator<Item = &'a str>,
) -> Vec<PathBuf> {
    files
        .into_iter()
        .filter(|relative| crate::install::is_inside_folder(Path::new(relative)))
        .map(|relative| folder.join(relative))
        .collect()
}

/// Whether `exe` is one of `programs`.
pub fn is_one_of(exe: &Path, programs: &[PathBuf]) -> bool {
    programs.iter().any(|program| same_path(exe, program))
}

/// Asks the running app to quit, then checks that none of `programs` still
/// runs. Reports each step through `log`.
pub fn close_programs(programs: &[PathBuf], log: &mut Log<'_>) -> Result<(), StillRunning> {
    let still_answering = match ask_app_to_quit() {
        QuitOutcome::NotRunning => None,
        QuitOutcome::Exited => {
            log(LogLine::Done(
                "Closed Downpour; its downloads continue at the next start".to_owned(),
            ));
            None
        }
        QuitOutcome::StillRunning { reason } => {
            log(LogLine::Warning(format!(
                "Downpour did not close when asked: {reason}"
            )));
            Some(reason)
        }
    };
    let running = native::running_programs(programs).map_err(StillRunning::Listing)?;
    if !running.is_empty() {
        return Err(StillRunning::Programs(running));
    }
    // An app answering from another folder does not lock these files, but
    // it would keep writing the data an uninstall may remove.
    if let Some(reason) = still_answering {
        if app_answers() {
            return Err(StillRunning::AppDidNotQuit(reason));
        }
    }
    Ok(())
}

/// Stops the programs the user agreed to stop, each only if its process
/// still runs the same file, and waits for them to end.
pub fn stop_programs(programs: &[RunningProgram], log: &mut Log<'_>) -> Result<(), StopError> {
    for program in programs {
        match native::stop_if_running(program, STOP_TIMEOUT)? {
            Stopped::Ended => log(LogLine::Done(format!(
                "Stopped {} (process {})",
                program.exe.display(),
                program.pid
            ))),
            Stopped::AlreadyGone => {}
            Stopped::OtherProgram(now) => log(LogLine::Warning(format!(
                "Left process {} alone: it now runs {}",
                program.pid,
                now.display()
            ))),
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Stopped {
    Ended,
    AlreadyGone,
    /// The process number now belongs to another program.
    OtherProgram(PathBuf),
}

#[derive(Debug, thiserror::Error)]
pub enum StopError {
    #[error("could not check process {pid}: {source}")]
    Inspect { pid: u32, source: std::io::Error },
    #[error("could not stop process {pid}: {source}")]
    Stop { pid: u32, source: std::io::Error },
    #[error("process {pid} did not end within {} seconds", STOP_TIMEOUT.as_secs())]
    DidNotEnd { pid: u32 },
}

fn ask_app_to_quit() -> QuitOutcome {
    let Some(data_dir) = downpour_protocol::data_dir() else {
        return QuitOutcome::StillRunning {
            reason: NotClosed::NoDataFolder,
        };
    };
    let endpoint = match Endpoint::load(&data_dir) {
        Ok(endpoint) => endpoint,
        Err(IpcError::NotRunning) => return QuitOutcome::NotRunning,
        Err(error) => {
            return QuitOutcome::StillRunning {
                reason: NotClosed::Ipc(error),
            }
        }
    };
    // Opened before asking: once the app has answered, this handle is the
    // app's process, and it stays valid even after the process ends.
    let process = native::ProcessWatch::open(endpoint.pid);
    match endpoint.send(AppRequest::Quit, IPC_TIMEOUT) {
        Ok(AppReply::Quitting) => {}
        Err(IpcError::NotRunning) => return QuitOutcome::NotRunning,
        Ok(other) => {
            return QuitOutcome::StillRunning {
                reason: NotClosed::UnexpectedReply(other),
            }
        }
        Err(error) => {
            return QuitOutcome::StillRunning {
                reason: NotClosed::Ipc(error),
            }
        }
    }
    let exited = match process {
        Some(process) => process.wait_for_exit(QUIT_TIMEOUT),
        None => wait_until_silent(QUIT_TIMEOUT),
    };
    if exited {
        QuitOutcome::Exited
    } else {
        QuitOutcome::StillRunning {
            reason: NotClosed::DidNotExit,
        }
    }
}

/// Whether a running app answers. Any failure to reach it counts as no.
fn app_answers() -> bool {
    downpour_protocol::data_dir()
        .and_then(|dir| Endpoint::load(&dir).ok())
        .and_then(|endpoint| endpoint.send(AppRequest::Ping, IPC_TIMEOUT).ok())
        .is_some_and(|reply| matches!(reply, AppReply::Pong { .. }))
}

fn wait_until_silent(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !app_answers() {
            return true;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    false
}

#[cfg(windows)]
mod native {
    use std::io;
    use std::path::PathBuf;
    use std::time::Duration;

    use windows::core::{HRESULT, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, HANDLE, WAIT_OBJECT_0,
    };
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
        PROCESS_ACCESS_RIGHTS, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    };

    use super::{is_one_of, RunningProgram, StopError, Stopped};
    use downpour_protocol::desktop::same_path;

    /// The exit code a stopped program ends with.
    const STOPPED_EXIT_CODE: u32 = 1;
    /// Room for a long (`\\?\`) path.
    const MAX_IMAGE_PATH: usize = 32_768;

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: the handle came from `OpenProcess` or
            // `CreateToolhelp32Snapshot` and is closed exactly once, here.
            let closed = unsafe { CloseHandle(self.0) };
            // Closing a valid handle owned once cannot fail outside a bug in
            // this module; there is no one to tell at runtime.
            debug_assert!(
                closed.is_ok(),
                "closing a process handle failed: {closed:?}"
            );
        }
    }

    fn is_win32(
        error: &windows::core::Error,
        code: windows::Win32::Foundation::WIN32_ERROR,
    ) -> bool {
        error.code() == HRESULT::from_win32(code.0)
    }

    fn open(pid: u32, access: PROCESS_ACCESS_RIGHTS) -> windows::core::Result<OwnedHandle> {
        // SAFETY: opening a process handle has no other effect.
        unsafe { OpenProcess(access, false, pid) }.map(OwnedHandle)
    }

    fn image_path(process: &OwnedHandle) -> windows::core::Result<PathBuf> {
        let mut buffer = vec![0u16; MAX_IMAGE_PATH];
        let mut length = buffer.len() as u32;
        // SAFETY: `buffer` holds `length` UTF-16 units.
        unsafe {
            QueryFullProcessImageNameW(
                process.0,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )?;
        }
        buffer.truncate(length as usize);
        Ok(PathBuf::from(String::from_utf16_lossy(&buffer)))
    }

    /// Waits for a process to end, without any right to change it.
    pub struct ProcessWatch(OwnedHandle);

    impl ProcessWatch {
        pub fn open(pid: u32) -> Option<ProcessWatch> {
            open(pid, PROCESS_SYNCHRONIZE).ok().map(ProcessWatch)
        }

        pub fn wait_for_exit(&self, timeout: Duration) -> bool {
            wait(&self.0, timeout)
        }
    }

    fn wait(process: &OwnedHandle, timeout: Duration) -> bool {
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: waits on a live handle owned by `process`.
        unsafe { WaitForSingleObject(process.0, millis) == WAIT_OBJECT_0 }
    }

    pub fn running_programs(programs: &[PathBuf]) -> io::Result<Vec<RunningProgram>> {
        let own = std::process::id();
        // SAFETY: takes a snapshot of the process list; closed on drop.
        let snapshot = OwnedHandle(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }?);
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut found = Vec::new();
        // SAFETY: `entry` is sized for the call, and the snapshot is live.
        let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) }.is_ok();
        while more {
            let pid = entry.th32ProcessID;
            if pid != own {
                if let Some(exe) = image_of(pid)? {
                    if is_one_of(&exe, programs) {
                        found.push(RunningProgram { pid, exe });
                    }
                }
            }
            // SAFETY: as above.
            more = unsafe { Process32NextW(snapshot.0, &mut entry) }.is_ok();
        }
        Ok(found)
    }

    /// The program a listed process runs, or `None` for one that ended
    /// since the snapshot or that this user may not inspect (another
    /// user's, or a protected one; neither can run a per-user install's
    /// files with this user's rights to stop it anyway).
    fn image_of(pid: u32) -> io::Result<Option<PathBuf>> {
        let process = match open(pid, PROCESS_QUERY_LIMITED_INFORMATION) {
            Ok(process) => process,
            Err(error)
                if is_win32(&error, ERROR_INVALID_PARAMETER)
                    || is_win32(&error, ERROR_ACCESS_DENIED) =>
            {
                return Ok(None)
            }
            Err(error) => return Err(io::Error::other(error)),
        };
        match image_path(&process) {
            Ok(exe) => Ok(Some(exe)),
            // System processes report no Win32 path.
            Err(_) => Ok(None),
        }
    }

    pub fn stop_if_running(
        program: &RunningProgram,
        timeout: Duration,
    ) -> Result<Stopped, StopError> {
        let pid = program.pid;
        if pid == std::process::id() {
            return Ok(Stopped::AlreadyGone);
        }
        let access = PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
        let process = match open(pid, access) {
            Ok(process) => process,
            // No process has this number any more.
            Err(error) if is_win32(&error, ERROR_INVALID_PARAMETER) => {
                return Ok(Stopped::AlreadyGone)
            }
            Err(error) => {
                return Err(StopError::Inspect {
                    pid,
                    source: io::Error::other(error),
                })
            }
        };
        // Checked through the same handle that stops it: if the process
        // ended and its number was reused, this is another program.
        let exe = image_path(&process).map_err(|error| StopError::Inspect {
            pid,
            source: io::Error::other(error),
        })?;
        if !same_path(&exe, &program.exe) {
            return Ok(Stopped::OtherProgram(exe));
        }
        // SAFETY: the handle has terminate rights to the process checked above.
        unsafe { TerminateProcess(process.0, STOPPED_EXIT_CODE) }.map_err(|error| {
            StopError::Stop {
                pid,
                source: io::Error::other(error),
            }
        })?;
        if !wait(&process, timeout) {
            return Err(StopError::DidNotEnd { pid });
        }
        Ok(Stopped::Ended)
    }
}

#[cfg(not(windows))]
mod native {
    use std::io;
    use std::path::PathBuf;
    use std::time::Duration;

    use super::{RunningProgram, StopError, Stopped};

    pub struct ProcessWatch;

    impl ProcessWatch {
        pub fn open(_pid: u32) -> Option<ProcessWatch> {
            None
        }

        pub fn wait_for_exit(&self, _timeout: Duration) -> bool {
            false
        }
    }

    pub fn running_programs(_programs: &[PathBuf]) -> io::Result<Vec<RunningProgram>> {
        Ok(Vec::new())
    }

    pub fn stop_if_running(
        _program: &RunningProgram,
        _timeout: Duration,
    ) -> Result<Stopped, StopError> {
        Ok(Stopped::AlreadyGone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn only_the_installations_own_files_count_as_its_programs() {
        let folder = Path::new(r"C:\Tools");
        let programs = installation_programs(
            folder,
            [
                "downpour.exe",
                "downpour-host.exe",
                "../evil.exe",
                "extensions/x.xpi",
            ],
        );
        assert_eq!(
            programs,
            [
                folder.join("downpour.exe"),
                folder.join("downpour-host.exe"),
                folder.join("extensions/x.xpi"),
            ]
        );
        assert!(is_one_of(Path::new(r"c:\tools\DOWNPOUR.exe"), &programs));
        // Other programs in the same folder, or below it, are never ours,
        // even when the user picked a busy folder such as C:\Tools.
        assert!(!is_one_of(Path::new(r"C:\Tools\editor.exe"), &programs));
        assert!(!is_one_of(
            Path::new(r"C:\Tools\sub\downpour.exe"),
            &programs
        ));
        assert!(!is_one_of(Path::new(r"C:\evil.exe"), &programs));
    }
}
