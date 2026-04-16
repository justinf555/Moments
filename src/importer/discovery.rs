use std::path::{Path, PathBuf};

use tracing::warn;

/// Recursively collect all files reachable from `sources`.
///
/// Directories are walked recursively; individual files are included as-is.
/// Symlinks are followed. Unreadable directories are logged and skipped.
pub fn collect_candidates(sources: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for source in sources {
        if source.is_file() {
            out.push(source);
        } else if source.is_dir() {
            walk_dir(&source, &mut out);
        }
    }
    out
}

fn walk_dir(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(path = %dir.display(), error = %e, "could not read directory");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_dir(&path, out);
        } else {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn collects_files_from_directory() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.jpg"), b"photo").unwrap();
        std::fs::write(dir.path().join("b.png"), b"photo").unwrap();

        let candidates = collect_candidates(vec![dir.path().to_path_buf()]);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn walks_subdirectories_recursively() {
        let dir = tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("a.jpg"), b"photo").unwrap();
        std::fs::write(sub.join("b.jpg"), b"photo").unwrap();

        let candidates = collect_candidates(vec![dir.path().to_path_buf()]);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn individual_files_included_directly() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("photo.jpg");
        std::fs::write(&file, b"photo").unwrap();

        let candidates = collect_candidates(vec![file.clone()]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0], file);
    }

    #[test]
    fn nonexistent_paths_are_skipped() {
        let candidates = collect_candidates(vec![PathBuf::from("/nonexistent/path")]);
        assert!(candidates.is_empty());
    }
}
