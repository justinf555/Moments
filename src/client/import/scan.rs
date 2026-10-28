// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Asynchronous folder scan over GIO.
//!
//! The import folder comes from the file chooser portal as a `gio::File`.
//! Inside Flatpak the document portal grants access to that object, not to
//! the `/run/user/…/doc/…` path behind it, so the walk has to stay on GIO
//! rather than moving to Tokio with `std::fs`. It runs on the GTK main
//! context, but every enumeration call is asynchronous, so the main loop
//! keeps turning between batches and the window stays responsive.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use tracing::{debug, info, warn};

/// Entries requested per `next_files` call.
const BATCH_SIZE: i32 = 100;

/// Consecutive `next_files` errors after which a directory is abandoned.
///
/// GIO doesn't document whether an enumerator can continue after an error,
/// so a few retries pick up the rest of a directory that hit one bad entry
/// without looping forever on one that keeps failing.
const MAX_CONSECUTIVE_ERRORS: u32 = 3;

const ATTRIBUTES: &str = "standard::name,standard::type,id::file";

/// Outcome of [`scan_folder`].
#[derive(Debug, Default)]
pub struct ScanResult {
    /// Regular (non-directory) files found, in discovery order.
    pub files: Vec<PathBuf>,
    /// Directories that could not be enumerated, failed enumeration
    /// batches, and entries with no local path.
    pub unreadable: usize,
    /// True when the scan stopped early because `cancellable` fired.
    pub cancelled: bool,
}

/// Walk `root` breadth-first and collect every file below it.
///
/// Uses a work queue rather than recursion, so deep trees can't exhaust the
/// stack. Errors are logged at `warn!`, counted in
/// [`ScanResult::unreadable`] and skipped; they never stop the scan.
/// Directories already visited (by `id::file`) are not walked again, so a
/// symlink loop terminates.
///
/// `on_progress` is called with the running file count after each batch.
/// Must be awaited on a GLib main context (it uses GIO async calls).
pub async fn scan_folder(
    root: gio::File,
    cancellable: &gio::Cancellable,
    mut on_progress: impl FnMut(usize),
) -> ScanResult {
    let mut result = ScanResult::default();
    let mut visited: HashSet<glib::GString> = HashSet::new();

    // Record the root's identity so a link back to it isn't walked twice.
    // A failure here is not fatal: enumeration below reports it properly.
    if let Ok(Ok(info)) = gio::CancellableFuture::new(
        root.query_info_future(
            "id::file",
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        ),
        cancellable.clone(),
    )
    .await
    {
        if let Some(id) = info.attribute_string("id::file") {
            visited.insert(id);
        }
    }

    let mut queue = VecDeque::from([root]);

    'dirs: while let Some(dir) = queue.pop_front() {
        let enumerate = gio::CancellableFuture::new(
            dir.enumerate_children_future(
                ATTRIBUTES,
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
            ),
            cancellable.clone(),
        )
        .await;

        let enumerator = match enumerate {
            Err(gio::Cancelled) => {
                result.cancelled = true;
                break;
            }
            Ok(Err(e)) => {
                warn!(uri = %dir.uri(), error = %e, "could not enumerate directory");
                result.unreadable += 1;
                continue;
            }
            Ok(Ok(e)) => e,
        };

        let mut consecutive_errors = 0;
        loop {
            let batch = gio::CancellableFuture::new(
                enumerator.next_files_future(BATCH_SIZE, glib::Priority::DEFAULT),
                cancellable.clone(),
            )
            .await;

            let infos = match batch {
                Err(gio::Cancelled) => {
                    result.cancelled = true;
                    break 'dirs;
                }
                Ok(Err(e)) => {
                    warn!(uri = %dir.uri(), error = %e, "error reading directory entries");
                    result.unreadable += 1;
                    consecutive_errors += 1;
                    if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                        warn!(uri = %dir.uri(), "giving up on directory after repeated errors");
                        break;
                    }
                    continue;
                }
                Ok(Ok(infos)) => infos,
            };
            consecutive_errors = 0;

            if infos.is_empty() {
                break;
            }

            for info in infos {
                let child = enumerator.child(&info);
                if info.file_type() == gio::FileType::Directory {
                    let id = info.attribute_string("id::file");
                    if let Some(id) = id {
                        if !visited.insert(id) {
                            debug!(uri = %child.uri(), "directory already scanned, skipping");
                            continue;
                        }
                    }
                    queue.push_back(child);
                } else if let Some(path) = child.path() {
                    result.files.push(path);
                } else {
                    warn!(uri = %child.uri(), "entry has no local path, skipping");
                    result.unreadable += 1;
                }
            }
            on_progress(result.files.len());
        }
    }

    info!(
        files = result.files.len(),
        unreadable = result.unreadable,
        cancelled = result.cancelled,
        "folder scan finished"
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(dir: &std::path::Path, cancellable: &gio::Cancellable) -> ScanResult {
        let ctx = glib::MainContext::new();
        let root = gio::File::for_path(dir);
        ctx.block_on(scan_folder(root, cancellable, |_| {}))
    }

    #[test]
    fn finds_files_in_nested_directories() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a/b/c");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(dir.path().join("top.jpg"), b"x").unwrap();
        std::fs::write(dir.path().join("a/mid.jpg"), b"x").unwrap();
        std::fs::write(deep.join("deep.jpg"), b"x").unwrap();

        let result = scan(dir.path(), &gio::Cancellable::new());
        assert_eq!(result.files.len(), 3);
        assert_eq!(result.unreadable, 0);
        assert!(!result.cancelled);
        assert!(result.files.contains(&deep.join("deep.jpg")));
    }

    #[test]
    fn reads_directories_larger_than_one_batch() {
        let dir = tempfile::tempdir().unwrap();
        let n = BATCH_SIZE as usize * 2 + 7;
        for i in 0..n {
            std::fs::write(dir.path().join(format!("{i}.jpg")), b"x").unwrap();
        }

        let ctx = glib::MainContext::new();
        let mut calls = Vec::new();
        let result = ctx.block_on(scan_folder(
            gio::File::for_path(dir.path()),
            &gio::Cancellable::new(),
            |count| calls.push(count),
        ));
        assert_eq!(result.files.len(), n);
        assert!(calls.len() >= 3, "progress per batch, got {calls:?}");
        assert_eq!(calls.last(), Some(&n));
    }

    #[test]
    fn already_cancelled_scan_returns_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.jpg"), b"x").unwrap();

        let cancellable = gio::Cancellable::new();
        cancellable.cancel();
        let result = scan(dir.path(), &cancellable);
        assert!(result.cancelled);
        assert!(result.files.is_empty());
    }

    #[test]
    fn missing_root_counts_as_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let result = scan(&dir.path().join("missing"), &gio::Cancellable::new());
        assert_eq!(result.unreadable, 1);
        assert!(result.files.is_empty());
    }

    #[test]
    fn unreadable_subdirectory_is_counted_not_fatal() {
        use std::os::unix::fs::PermissionsExt;

        if is_root() {
            return; // root ignores directory permissions
        }
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("hidden.jpg"), b"x").unwrap();
        std::fs::write(dir.path().join("visible.jpg"), b"x").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let result = scan(dir.path(), &gio::Cancellable::new());

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(result.files, vec![dir.path().join("visible.jpg")]);
        assert_eq!(result.unreadable, 1);
    }

    #[test]
    fn symlink_loop_terminates() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("a.jpg"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.path(), sub.join("loop")).unwrap();

        let result = scan(dir.path(), &gio::Cancellable::new());
        assert_eq!(result.files, vec![sub.join("a.jpg")]);
    }

    /// True when directory permissions aren't enforced (running as root).
    fn is_root() -> bool {
        use std::os::unix::fs::PermissionsExt;
        let probe = tempfile::tempdir().unwrap();
        let locked = probe.path().join("probe");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let readable = std::fs::read_dir(&locked).is_ok();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        readable
    }
}
