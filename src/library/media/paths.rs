// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! On-disk layout of originals under the bundle's `originals/` directory.

use std::path::{Path, PathBuf};

use super::MediaId;

/// Compute the two-level sharded original path (no file extension).
///
/// Decoders detect format from magic bytes, so the extension is not
/// needed. The path is a pure function of `id` — no DB lookup required.
pub fn sharded_original_path(originals_dir: &Path, id: &MediaId) -> PathBuf {
    let hex = id.as_str();
    if hex.len() < 4 {
        return originals_dir.join(hex);
    }
    originals_dir.join(&hex[..2]).join(&hex[2..4]).join(hex)
}

/// Return the relative portion of a sharded original path (no directory prefix).
///
/// Stored in the `relative_path` DB column. Joining with `originals_dir`
/// gives the absolute path.
pub fn sharded_original_relative(id: &MediaId) -> String {
    let hex = id.as_str();
    if hex.len() < 4 {
        return hex.to_string();
    }
    format!("{}/{}/{hex}", &hex[..2], &hex[2..4])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_path_normal_id() {
        let id = MediaId::new("abcdef1234567890".to_string());
        let path = sharded_original_path(Path::new("/originals"), &id);
        assert_eq!(path, Path::new("/originals/ab/cd/abcdef1234567890"));
    }

    #[test]
    fn original_path_uuid_format() {
        let id = MediaId::new("550e8400-e29b-41d4-a716-446655440000".to_string());
        let path = sharded_original_path(Path::new("/originals"), &id);
        assert_eq!(
            path,
            Path::new("/originals/55/0e/550e8400-e29b-41d4-a716-446655440000")
        );
    }

    #[test]
    fn original_path_short_id() {
        let id = MediaId::new("ab".to_string());
        let path = sharded_original_path(Path::new("/originals"), &id);
        assert_eq!(path, Path::new("/originals/ab"));
    }

    #[test]
    fn original_relative_normal_id() {
        let id = MediaId::new("abcdef1234567890".to_string());
        assert_eq!(sharded_original_relative(&id), "ab/cd/abcdef1234567890");
    }

    #[test]
    fn original_relative_uuid_format() {
        let id = MediaId::new("550e8400-e29b-41d4-a716-446655440000".to_string());
        assert_eq!(
            sharded_original_relative(&id),
            "55/0e/550e8400-e29b-41d4-a716-446655440000"
        );
    }

    #[test]
    fn original_relative_short_id() {
        let id = MediaId::new("ab".to_string());
        assert_eq!(sharded_original_relative(&id), "ab");
    }
}
