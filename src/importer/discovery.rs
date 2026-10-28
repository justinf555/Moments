// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::path::PathBuf;

use tracing::warn;

/// Files found by [`collect_candidates`].
#[derive(Debug, Default)]
pub struct Candidates {
    /// Files to import, in discovery order.
    pub files: Vec<PathBuf>,
    /// Directories or entries that could not be read.
    pub unreadable: usize,
}

/// Collect all files reachable from `sources`.
///
/// Directories are walked (iteratively); individual files are included
/// as-is. Symlinks are followed. Unreadable directories and entries are
/// logged, counted in [`Candidates::unreadable`] and skipped.
///
/// The app's folder import scans over GIO in the client and hands the
/// pipeline plain files via [`super::ImportPipeline::run_files`], which
/// skips this step. This walk serves callers passing directories.
pub fn collect_candidates(sources: Vec<PathBuf>) -> Candidates {
    let mut out = Candidates::default();
    let mut dirs = Vec::new();
    for source in sources {
        if source.is_file() {
            out.files.push(source);
        } else if source.is_dir() {
            dirs.push(source);
        }
    }

    while let Some(dir) = dirs.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => {
                warn!(path = %dir.display(), error = %e, "could not read directory");
                out.unreadable += 1;
                continue;
            }
        };
        for entry in entries {
            match entry {
                Ok(entry) => {
                    let path = entry.path();
                    if path.is_dir() {
                        dirs.push(path);
                    } else {
                        out.files.push(path);
                    }
                }
                Err(e) => {
                    warn!(path = %dir.display(), error = %e, "could not read directory entry");
                    out.unreadable += 1;
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn collects_files_from_directory() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.jpg"), b"photo").unwrap();
        std::fs::write(dir.path().join("b.png"), b"photo").unwrap();

        let candidates = collect_candidates(vec![dir.path().to_path_buf()]);
        assert_eq!(candidates.files.len(), 2);
        assert_eq!(candidates.unreadable, 0);
    }

    #[test]
    fn walks_subdirectories_recursively() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("a.jpg"), b"photo").unwrap();
        std::fs::write(sub.join("b.jpg"), b"photo").unwrap();

        let candidates = collect_candidates(vec![dir.path().to_path_buf()]);
        assert_eq!(candidates.files.len(), 2);
    }

    #[test]
    fn individual_files_included_directly() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("photo.jpg");
        std::fs::write(&file, b"photo").unwrap();

        let candidates = collect_candidates(vec![file.clone()]);
        assert_eq!(candidates.files, vec![file]);
    }

    #[test]
    fn nonexistent_paths_are_skipped() {
        let candidates = collect_candidates(vec![PathBuf::from("/nonexistent/path")]);
        assert!(candidates.files.is_empty());
    }

    #[test]
    fn unreadable_directory_is_counted_not_fatal() {
        let dir = tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("hidden.jpg"), b"photo").unwrap();
        std::fs::write(dir.path().join("visible.jpg"), b"photo").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        // Root ignores directory permissions; nothing to test then.
        let enforced = std::fs::read_dir(&locked).is_err();
        let candidates = collect_candidates(vec![dir.path().to_path_buf()]);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        if !enforced {
            return;
        }

        assert_eq!(candidates.files, vec![dir.path().join("visible.jpg")]);
        assert_eq!(candidates.unreadable, 1);
    }
}
