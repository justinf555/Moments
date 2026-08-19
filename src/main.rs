// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use moments::application::MomentsApplication;
use moments::config;

use config::{GETTEXT_PACKAGE, LOCALEDIR, PKGDATADIR};
use gettextrs::{bind_textdomain_codeset, bindtextdomain, textdomain};
use gtk::prelude::*;
use gtk::{gio, glib};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() -> glib::ExitCode {
    // Initialise tracing first so the dhat-heap feature can use info!
    // (project convention: never println!/eprintln!). RUST_LOG controls
    // verbosity (e.g. RUST_LOG=moments=debug). Debug builds default to
    // `moments=debug` so dev runs (make run-dev, GNOME Builder) get
    // verbose output without needing the env var wired through the
    // Flatpak sandbox; release defaults to `info`.
    let default_filter = if cfg!(debug_assertions) {
        "moments=debug"
    } else {
        "moments=info"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter)),
        )
        .init();

    // Heap profiler. Captures every allocation/deallocation and writes a
    // JSON dump on drop; viewer: https://nnethercote.github.io/dh_view/.
    // Lives near the top of `main` and is moved into a binding that drops
    // when `main` returns — the JSON is written from `Drop`.
    #[cfg(feature = "dhat-heap")]
    let _dhat_profiler = {
        let path = glib::user_cache_dir().join("moments-dhat-heap.json");
        let profiler = dhat::Profiler::builder().file_name(&path).build();
        info!(path = %path.display(), "dhat-heap profiler active");
        profiler
    };

    // Register libheif-rs as a decoder plugin for the `image` crate so that
    // image::open() transparently handles HEIC and HEIF files throughout the app.
    libheif_rs::integration::image::register_all_decoding_hooks();

    // Initialise GStreamer for video poster-frame extraction.
    gstreamer::init().expect("failed to initialise GStreamer");

    info!(version = config::VERSION, "Moments starting");

    // Set up gettext translations
    bindtextdomain(GETTEXT_PACKAGE, LOCALEDIR).expect("Unable to bind the text domain");
    bind_textdomain_codeset(GETTEXT_PACKAGE, "UTF-8")
        .expect("Unable to set the text domain encoding");
    textdomain(GETTEXT_PACKAGE).expect("Unable to switch to the text domain");

    // Load resources
    let resources = gio::Resource::load(PKGDATADIR.to_owned() + "/moments.gresource")
        .expect("Could not load resources");
    gio::resources_register(&resources);

    // Build the Tokio runtime — the library executor for all backend async
    // work (database, file I/O, future Immich HTTP). It is created before the
    // GTK main loop and dropped after it exits, so it outlives every library
    // operation. All backends share this single runtime via a Handle.
    let tokio = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("moments-library")
        .enable_all()
        .build()
        .expect("failed to build Tokio runtime");

    let app = MomentsApplication::new(
        config::APP_ID,
        &gio::ApplicationFlags::empty(),
        tokio.handle().clone(),
    );

    let exit_code = app.run();

    // Explicitly drop the Tokio runtime after the GTK main loop exits so any
    // in-flight async tasks are cleanly shut down before the process ends.
    drop(tokio);
    exit_code
}
