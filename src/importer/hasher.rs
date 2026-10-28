// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::io::Read;
use std::path::Path;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use sha1::{Digest, Sha1};
use tracing::instrument;

use super::error::ImportError;

/// Compute the SHA-1 content hash of a file, base64-encoded.
///
/// Matches Immich's `checksum` wire format byte-for-byte so that
/// locally-imported and server-pulled rows share a single `content_hash`
/// column and can dedup symmetrically (issue #626 follow-up).
///
/// Used for deduplication only — never as the asset's identity (which is
/// a UUID). SHA-1 is sufficient for accidental-collision detection in a
/// personal photo library; we are not in an adversarial threat model.
///
/// Runs on a blocking thread via [`tokio::task::spawn_blocking`] so the
/// async executor stays free during the streaming hash. Large video files
/// are never fully loaded into memory.
#[instrument(skip_all, fields(path = %path.display()))]
pub async fn hash_file(path: &Path) -> Result<String, ImportError> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<String, ImportError> {
        let file = std::fs::File::open(&path).map_err(ImportError::Io)?;
        let mut reader = std::io::BufReader::new(file);
        let mut hasher = Sha1::new();
        // sha1 0.11 no longer implements `std::io::Write`, so stream manually.
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(ImportError::Io(e)),
            };
            hasher.update(&buf[..n]);
        }
        let digest = hasher.finalize();
        Ok(B64.encode(digest))
    })
    .await
    .map_err(|e| ImportError::Runtime(e.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[tokio::test]
    async fn same_content_produces_same_hash() {
        let mut f1 = NamedTempFile::new().unwrap();
        let mut f2 = NamedTempFile::new().unwrap();
        f1.write_all(b"identical content").unwrap();
        f2.write_all(b"identical content").unwrap();

        let h1 = hash_file(f1.path()).await.unwrap();
        let h2 = hash_file(f2.path()).await.unwrap();
        assert_eq!(h1, h2);
    }

    #[tokio::test]
    async fn different_content_produces_different_hash() {
        let mut f1 = NamedTempFile::new().unwrap();
        let mut f2 = NamedTempFile::new().unwrap();
        f1.write_all(b"content a").unwrap();
        f2.write_all(b"content b").unwrap();

        let h1 = hash_file(f1.path()).await.unwrap();
        let h2 = hash_file(f2.path()).await.unwrap();
        assert_ne!(h1, h2);
    }

    /// SHA-1 base64 of "abc" is `qZk+NkcGgWq6PiVxeFDCbJzQ2J0=` — a fixed
    /// vector from the SHA-1 spec, encoded the way Immich emits it. Using
    /// a known-answer test pins both the algorithm and the encoding so
    /// that a future swap can't silently desync our local hashes from
    /// what we receive over the sync stream.
    #[tokio::test]
    async fn matches_immich_sha1_base64_for_known_input() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"abc").unwrap();
        let hash = hash_file(f.path()).await.unwrap();
        assert_eq!(hash, "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=");
    }

    /// Fixed vector for "identical content". Expected value derived
    /// independently of this crate: `printf 'identical content' | sha1sum`
    /// gives 35fa158495d259492bc1495b5d29e842226ed7be, hex-decoded and
    /// base64-encoded (Python `base64.b64encode(bytes.fromhex(..))`).
    #[tokio::test]
    async fn fixed_vector_identical_content() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"identical content").unwrap();
        let hash = hash_file(f.path()).await.unwrap();
        assert_eq!(hash, "NfoVhJXSWUkrwUlbXSnoQiJu174=");
    }

    #[tokio::test]
    async fn missing_file_returns_error() {
        let result = hash_file(Path::new("/nonexistent/file.jpg")).await;
        assert!(result.is_err());
    }
}
