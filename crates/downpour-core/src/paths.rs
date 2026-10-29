//! Paths in the platform's own form.

use std::path::{Path, PathBuf};

/// Rebuilds `path` from its parts, so every separator is the platform's
/// own. Windows accepts `/` but shows it as typed, so a folder entered as
/// `C:\Users\me/Downloads` would otherwise be shown and stored mixed.
pub fn native(path: &Path) -> PathBuf {
    path.components().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_paths_use_backslashes_throughout() {
        assert_eq!(
            native(Path::new(r"C:\Users\me/AppData/Local\Temp/")),
            PathBuf::from(r"C:\Users\me\AppData\Local\Temp")
        );
        assert_eq!(
            native(Path::new(r"\server\share/films/a.mkv")),
            PathBuf::from(r"\server\share\films\a.mkv")
        );
    }

    #[test]
    fn relative_paths_stay_relative() {
        let path = native(Path::new("Downloads/Downpour"));
        assert!(path.is_relative());
        assert_eq!(path, Path::new("Downloads").join("Downpour"));
    }
}
