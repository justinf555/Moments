//! A real Library backed by a temp-dir SQLite DB for integration tests.

use std::sync::Arc;

use moments::library::bundle::Bundle;
use moments::library::config::{LibraryConfig, LocalStorageMode};
use moments::library::Library;

/// Create a Library backed by a temporary database and a Tokio Handle.
pub fn stub_deps() -> (Arc<Library>, tokio::runtime::Handle) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let handle = rt.handle().clone();

    let library = rt.block_on(async {
        let dir = tempfile::tempdir().unwrap();
        let bundle_path = dir.path().join("Test.library");
        let bundle = Bundle::create(
            &bundle_path,
            &LibraryConfig::Local {
                mode: LocalStorageMode::Managed,
            },
        )
        .unwrap();
        // Leak the tempdir so it lives for the test duration.
        std::mem::forget(dir);
        Arc::new(
            Library::open(
                bundle,
                LocalStorageMode::Managed,
                std::sync::Arc::new(moments::sync::outbox::NoOpRecorder),
            )
            .await
            .unwrap(),
        )
    });

    // Leak the runtime so it stays alive for the test.
    std::mem::forget(rt);
    (library, handle)
}
