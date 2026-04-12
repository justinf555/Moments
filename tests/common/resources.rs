//! Compile and register GResources for integration tests.
//!
//! Composite template widgets (PhotoGridCell, AlbumCard, etc.) require the
//! compiled `.ui` resources to be registered before construction. In production,
//! `main()` loads the pre-built gresource bundle. In tests, we compile the
//! Blueprint templates on-the-fly using `blueprint-compiler` and
//! `glib-compile-resources`, then register the result.
//!
//! Call `ensure_resources()` before creating any composite template widget.

use std::path::Path;
use std::process::Command;
use std::sync::Once;

use gtk::gio;

static INIT: Once = Once::new();

/// Collect all `.blp` files under `dir` recursively.
fn find_blp_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                results.extend(find_blp_files(&path));
            } else if path.extension().is_some_and(|ext| ext == "blp") {
                results.push(path);
            }
        }
    }
    results
}

/// Compile blueprints, build the gresource bundle, and register it.
///
/// Safe to call multiple times — only runs once per process.
pub fn ensure_resources() {
    INIT.call_once(|| {
        let project_root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let src_dir = project_root.join("src");
        let out_dir = std::env::temp_dir().join("moments-test-resources");
        std::fs::create_dir_all(&out_dir).expect("create temp output dir");

        // Collect all .blp files and compile them to .ui
        let blp_files = find_blp_files(&src_dir);
        if blp_files.is_empty() {
            return;
        }

        let mut cmd = Command::new("blueprint-compiler");
        cmd.arg("batch-compile").arg(&out_dir).arg(&src_dir);
        for blp in &blp_files {
            cmd.arg(blp);
        }
        let status = cmd.status().expect("blueprint-compiler not found");
        assert!(status.success(), "blueprint-compiler failed");

        // Rewrite gresource.xml to point at the compiled .ui files in out_dir
        let gresource_xml = src_dir.join("moments.gresource.xml");
        let content = std::fs::read_to_string(&gresource_xml).expect("read gresource.xml");

        let test_xml_path = out_dir.join("moments-test.gresource.xml");
        std::fs::write(&test_xml_path, &content).expect("write test gresource.xml");

        // Compile the gresource bundle
        let gresource_path = out_dir.join("moments.gresource");
        let status = Command::new("glib-compile-resources")
            .arg("--sourcedir")
            .arg(&out_dir)
            .arg("--sourcedir")
            .arg(&src_dir)
            .arg("--target")
            .arg(&gresource_path)
            .arg(&test_xml_path)
            .status()
            .expect("glib-compile-resources not found");
        assert!(status.success(), "glib-compile-resources failed");

        // Register the compiled resources
        let resource = gio::Resource::load(&gresource_path).expect("load compiled gresource");
        gio::resources_register(&resource);
    });
}
