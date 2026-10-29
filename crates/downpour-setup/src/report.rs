//! What an install or uninstall tells the user as it goes, and what it
//! could not clean up.

use std::fmt;

/// One line of progress.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LogLine {
    /// A step that worked.
    Done(String),
    /// Something the user should know about: a step that failed without
    /// stopping the rest, or something left in place on purpose.
    Warning(String),
}

/// Receives progress as it happens.
pub type Log<'a> = dyn FnMut(LogLine) + 'a;

/// Something that could not be removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupFailure {
    /// What it is, for example "shortcut" or "uninstall entry".
    pub what: &'static str,
    /// Its file path or registry key.
    pub location: String,
    pub error: String,
}

impl fmt::Display for CleanupFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not remove the {} {}: {}",
            self.what, self.location, self.error
        )
    }
}

/// How a finished install or uninstall went beyond its main work.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupReport {
    /// Things left behind; each was also logged as a warning.
    pub leftovers: Vec<CleanupFailure>,
}

impl SetupReport {
    pub fn record(&mut self, failure: CleanupFailure, log: &mut Log<'_>) {
        log(LogLine::Warning(failure.to_string()));
        self.leftovers.push(failure);
    }
}
