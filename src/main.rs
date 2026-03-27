/* main.rs
 *
 * Copyright 2026 Unknown
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

mod application;
mod config;
mod library;
mod ui;

use self::application::MomentsApplication;

use config::{GETTEXT_PACKAGE, LOCALEDIR, PKGDATADIR};
use gettextrs::{bind_textdomain_codeset, bindtextdomain, textdomain};
use gtk::{gio, glib};
use gtk::prelude::*;
use tracing::info;
use tracing_subscriber::EnvFilter;

fn main() -> glib::ExitCode {
    // Register libheif-rs as a decoder plugin for the `image` crate so that
    // image::open() transparently handles HEIC and HEIF files throughout the app.
    libheif_rs::integration::image::register_all_decoding_hooks();

    // Initialise GStreamer for video poster-frame extraction.
    gstreamer::init().expect("failed to initialise GStreamer");

    // Initialise tracing — RUST_LOG controls verbosity (e.g. RUST_LOG=moments=debug)
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("moments=info")),
        )
        .init();

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
        "io.github.justinf555.Moments",
        &gio::ApplicationFlags::empty(),
        tokio.handle().clone(),
    );

    let exit_code = app.run();

    // Explicitly drop the Tokio runtime after the GTK main loop exits so any
    // in-flight async tasks are cleanly shut down before the process ends.
    drop(tokio);
    exit_code
}
