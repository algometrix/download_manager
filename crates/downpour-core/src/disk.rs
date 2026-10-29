//! Free disk space checks, so a download waits for room instead of failing
//! halfway or filling the disk to the last byte.

use std::io;
use std::path::{Path, PathBuf};

/// Kept free beyond what a download needs: a full disk breaks other programs.
pub const SPACE_RESERVE_BYTES: u64 = 64 * 1024 * 1024;

#[cfg(windows)]
const DISK_FULL_CODES: [i32; 2] = [
    39,  // ERROR_HANDLE_DISK_FULL
    112, // ERROR_DISK_FULL
];
#[cfg(not(windows))]
const DISK_FULL_CODES: [i32; 2] = [
    28,  // ENOSPC
    122, // EDQUOT
];

/// Not enough room in `directory`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not enough disk space in {}: {} needed, {available} bytes free", .directory.display(), describe_needed(*.needed))]
pub struct NoSpace {
    pub directory: PathBuf,
    /// Bytes the download still has to write, when known.
    pub needed: Option<u64>,
    pub available: u64,
}

fn describe_needed(needed: Option<u64>) -> String {
    match needed {
        Some(bytes) => format!("{bytes} bytes"),
        None => "more space".to_owned(),
    }
}

/// Free bytes on the volume holding `path`, which may not exist yet.
pub fn free_space(path: &Path) -> io::Result<u64> {
    let existing = path
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .unwrap_or(path);
    fs4::available_space(existing)
}

/// Fails with [`NoSpace`] unless `needed` bytes plus the reserve fit.
pub fn ensure_space(directory: &Path, needed: u64) -> Result<(), NoSpaceCheck> {
    let available = free_space(directory).map_err(NoSpaceCheck::Unknown)?;
    if available >= needed.saturating_add(SPACE_RESERVE_BYTES) {
        return Ok(());
    }
    Err(NoSpaceCheck::Short(NoSpace {
        directory: directory.to_path_buf(),
        needed: Some(needed),
        available,
    }))
}

#[derive(Debug, thiserror::Error)]
pub enum NoSpaceCheck {
    #[error(transparent)]
    Short(NoSpace),
    #[error("cannot read free disk space: {0}")]
    Unknown(io::Error),
}

/// Whether a write failed because the volume is full.
pub fn is_disk_full(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::StorageFull
        || error.kind() == io::ErrorKind::QuotaExceeded
        || error
            .raw_os_error()
            .is_some_and(|code| DISK_FULL_CODES.contains(&code))
}

/// Whether a download waiting for room in `directory` can continue.
pub fn has_room_for(directory: &Path, needed: Option<u64>) -> bool {
    match free_space(directory) {
        Ok(available) => available >= needed.unwrap_or(0).saturating_add(SPACE_RESERVE_BYTES),
        Err(error) => {
            tracing::warn!(directory = %directory.display(), %error, "cannot read free disk space");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_disk_full_errors() {
        assert!(is_disk_full(&io::Error::from(io::ErrorKind::StorageFull)));
        assert!(is_disk_full(&io::Error::from_raw_os_error(
            DISK_FULL_CODES[1]
        )));
        assert!(!is_disk_full(&io::Error::from(
            io::ErrorKind::PermissionDenied
        )));
    }

    #[test]
    fn free_space_works_for_folders_that_do_not_exist_yet() {
        let missing = std::env::temp_dir()
            .join("downpour-no-such-folder")
            .join("deeper");
        assert!(free_space(&missing).unwrap() > 0);
    }

    #[test]
    fn impossible_requirement_reports_no_space() {
        let result = ensure_space(&std::env::temp_dir(), u64::MAX / 2);
        assert!(matches!(
            result,
            Err(NoSpaceCheck::Short(NoSpace {
                needed: Some(_),
                ..
            }))
        ));
    }
}
