// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end folder import through `ImportClient`: the GIO scan on the
//! GTK main context (#705) feeding the pipeline on Tokio, against a real
//! library in a temp dir.
//!
//! Run with:
//!   cargo test --features integration-tests --test import_scan -- --test-threads=1

#![cfg(feature = "integration-tests")]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::prelude::*;
use gtk::{gio, glib};

use moments::client::{ImportClient, ImportState};
use moments::library::bundle::Bundle;
use moments::library::config::{LibraryConfig, LocalStorageMode};
use moments::library::db::Database;
use moments::library::resolver::LocalResolver;
use moments::library::Library;
use moments::renderer::pipeline::RenderPipeline;

fn write_png(path: &Path, shade: u8) {
    image::RgbImage::from_pixel(2, 2, image::Rgb([shade, 40, 80]))
        .save(path)
        .unwrap();
}

/// Iterate the main context until `done` or the timeout elapses.
fn run_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let ctx = glib::MainContext::default();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(5));
    }
    done()
}

fn configured_client(dir: &Path, rt: &tokio::runtime::Runtime) -> ImportClient {
    let bundle = Bundle::create(
        &dir.join("Test.library"),
        &LibraryConfig::Local {
            mode: LocalStorageMode::Managed,
        },
    )
    .unwrap();
    let originals = bundle.originals.clone();
    let thumbnails = bundle.thumbnails.clone();
    let library = rt
        .block_on(Library::open(
            bundle,
            LocalStorageMode::Managed,
            Database::new(),
            Arc::new(moments::sync::outbox::NoOpRecorder),
            Arc::new(LocalResolver::new(
                originals.clone(),
                LocalStorageMode::Managed,
            )),
        ))
        .unwrap();

    let client = ImportClient::new();
    client.configure(
        Arc::new(library),
        originals,
        thumbnails,
        Arc::new(RenderPipeline::new()),
        LocalStorageMode::Managed,
        rt.handle().clone(),
    );
    client
}

#[gtk::test]
fn nested_folder_scan_imports_every_level() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let deep = source.join("2024/06/trip");
    std::fs::create_dir_all(&deep).unwrap();
    write_png(&source.join("top.png"), 1);
    write_png(&source.join("2024/mid.png"), 2);
    write_png(&deep.join("deep.png"), 3);
    std::fs::write(source.join("2024/notes.txt"), b"not media").unwrap();

    let client = configured_client(dir.path(), &rt);
    let saw_scanning = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = saw_scanning.clone();
    client.connect_notify_local(Some("state"), move |c, _| {
        if c.state() == ImportState::Scanning {
            flag.set(true);
        }
    });

    client.import_folder(gio::File::for_path(&source));
    assert!(run_until(Duration::from_secs(30), || client.state()
        == ImportState::Complete));

    assert!(saw_scanning.get());
    assert_eq!(client.scanned(), 4);
    assert_eq!(client.imported(), 3);
    assert_eq!(client.skipped(), 1);
    assert_eq!(client.unreadable(), 0);
    assert!(!client.cancelled());
}

#[gtk::test]
fn cancel_during_scan_reports_cancelled_and_imports_nothing() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir_all(source.join("a/b")).unwrap();
    write_png(&source.join("a/b/photo.png"), 9);

    let client = configured_client(dir.path(), &rt);
    client.import_folder(gio::File::for_path(&source));
    assert_eq!(client.state(), ImportState::Scanning);
    client.cancel();

    assert!(run_until(Duration::from_secs(30), || client.state()
        == ImportState::Complete));
    assert!(client.cancelled());
    assert_eq!(client.imported(), 0);
}
