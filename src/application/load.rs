// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

//! Loading an opened library: build the backend on Tokio, then create
//! the client singletons and start background tasks on the GTK thread.

use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::glib;
use tracing::{error, info};

use super::open::with_details;
use crate::library::bundle::Bundle;
use crate::library::config::LibraryConfig;
use crate::library::Library;
use crate::ui::MomentsWindow;

use super::MomentsApplication;

impl MomentsApplication {
    /// Open the library on Tokio, then wire up the app on the GTK thread.
    ///
    /// On success: creates the client singletons, sets up the window's
    /// content, and starts the trash purge task and (for Immich) the sync
    /// engine. On failure: offers to re-run setup or quit.
    pub(super) fn load_library_async(
        &self,
        bundle: Bundle,
        config: LibraryConfig,
        window: MomentsWindow,
    ) {
        // Extract Immich connection info before the config is consumed.
        let immich_info = match &config {
            LibraryConfig::Immich {
                server_url,
                access_token,
            } => Some((server_url.clone(), access_token.clone())),
            _ => None,
        };

        // Extract paths and mode for the ImportClient before the factory
        // consumes the bundle and config.
        let originals_dir = bundle.originals.clone();
        let thumbnails_dir = bundle.thumbnails.clone();
        let storage_mode = match &config {
            LibraryConfig::Local { mode } => mode.clone(),
            LibraryConfig::Immich { .. } => crate::library::config::LocalStorageMode::Managed,
        };

        glib::MainContext::default().spawn_local(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            window,
            async move {
                let tokio = app.imp().tokio.get().expect("tokio handle set").clone();

                let import_mode = storage_mode.clone();
                let db = crate::library::db::Database::new();

                // Build Immich client + recorder + resolver based on config.
                // A client that fails to build must stop the open: falling
                // back to None would run an Immich library as local, with
                // no sync and no outbox.
                let immich_client = match immich_info
                    .as_ref()
                    .map(|(url, token)| {
                        crate::sync::providers::immich::client::ImmichClient::new(url, token)
                    })
                    .transpose()
                {
                    Ok(client) => client,
                    Err(e) => {
                        error!("failed to create Immich client: {e}");
                        app.show_open_failed_dialog(&window, &e);
                        return;
                    }
                };

                let recorder: Arc<dyn crate::library::recorder::MutationRecorder> =
                    if immich_client.is_some() {
                        Arc::new(crate::sync::outbox::QueueWriterOutbox::new(db.clone()))
                    } else {
                        Arc::new(crate::sync::outbox::NoOpRecorder)
                    };

                // Originals in use (viewer, export), shared by the media
                // client and the Immich cache so eviction skips them.
                let original_pins = Arc::new(crate::library::resolver::OriginalPins::default());
                let settings = app.settings();
                let cache_limit = super::originals_cache::limit_from_settings(&settings);

                // Keep the concrete Immich resolver to enable eviction and
                // follow the limit setting once the library is open.
                let cached_resolver = immich_client.as_ref().map(|client| {
                    Arc::new(
                        crate::sync::providers::immich::resolver::CachedResolver::new(
                            Arc::new(client.clone()),
                            originals_dir.clone(),
                            Arc::clone(&cache_limit),
                            Arc::clone(&original_pins),
                        ),
                    )
                });

                let resolver: Arc<dyn crate::library::resolver::OriginalResolver> =
                    if let Some(ref cached) = cached_resolver {
                        Arc::clone(cached) as _
                    } else {
                        Arc::new(crate::library::resolver::LocalResolver::new(
                            originals_dir.clone(),
                            import_mode.clone(),
                        ))
                    };

                let db_for_sync = db.clone();
                let open_result = tokio
                    .spawn(async move {
                        Library::open(bundle, storage_mode, db, recorder, resolver).await
                    })
                    .await
                    .map_err(|e| crate::library::error::LibraryError::Runtime(e.to_string()));
                let storage_mode = import_mode;
                match open_result.and_then(|r| r) {
                    Ok(library) => {
                        let library = Arc::new(library);
                        info!("library ready");

                        // Store library on the application.
                        *app.imp().library.borrow_mut() = Some(Arc::clone(&library));

                        // Store the backend type for the preferences dialog,
                        // only once the library has actually opened.
                        if let Some((ref server_url, _)) = immich_info {
                            app.imp().is_immich.set(true);
                            *app.imp().immich_server_url.borrow_mut() = Some(server_url.clone());
                        }

                        // Create the import client (GObject singleton).
                        let sync_thumbnails_dir = thumbnails_dir.clone();
                        {
                            let render_pipeline =
                                Arc::new(crate::renderer::pipeline::RenderPipeline::new());
                            *app.imp().render_pipeline.borrow_mut() =
                                Some(Arc::clone(&render_pipeline));

                            let import_client = crate::client::ImportClient::new();
                            import_client.configure(
                                Arc::clone(&library),
                                originals_dir,
                                thumbnails_dir,
                                Arc::clone(&render_pipeline),
                                storage_mode,
                                tokio.clone(),
                            );
                            app.set_import_client(import_client);
                        }

                        // Create the album client (GObject singleton).
                        // Subscribe to AlbumEvent for reactive model updates.
                        {
                            let albums_rx = library.albums().subscribe();
                            let album_client_v2 = crate::client::AlbumClientV2::new();
                            album_client_v2.configure(
                                Arc::clone(&library),
                                tokio.clone(),
                                albums_rx,
                            );
                            *app.imp().album_client_v2.borrow_mut() = Some(album_client_v2);
                        }

                        // Create the people client (GObject singleton).
                        // Subscribe to FacesEvent for reactive model updates.
                        {
                            let faces_rx = library.faces().subscribe();
                            let people_client = crate::client::PeopleClientV2::new();
                            people_client.configure(Arc::clone(&library), tokio.clone(), faces_rx);
                            *app.imp().people_client.borrow_mut() = Some(people_client);
                        }

                        // Create the MediaClient (GObject singleton).
                        // Subscribes to MediaEvent via the service's fan-out
                        // channel for reactive model updates.
                        {
                            let media_client_v2 = crate::client::MediaClientV2::new();
                            media_client_v2.configure(Arc::clone(&library), tokio.clone());
                            media_client_v2.set_original_pins(Arc::clone(&original_pins));
                            *app.imp().media_client_v2.borrow_mut() = Some(media_client_v2);
                        }

                        // Immich originals cache: enable eviction, trim to
                        // the limit once now, and follow the setting.
                        if let Some(ref cached) = cached_resolver {
                            super::originals_cache::start(
                                &settings,
                                cached,
                                &library,
                                db_for_sync.clone(),
                                Arc::clone(&cache_limit),
                                &tokio,
                            );
                        }

                        // Wire the shell: builds sidebar, registers views,
                        // and switches to the content page. Components react
                        // to mutations via per-service event channels and
                        // GObject signals on the client singletons.
                        window.setup(settings.clone());

                        // Start periodic trash purge task.
                        {
                            let retention_days = settings.uint("trash-retention-days");
                            let handle = crate::library::purge_trash::start(
                                Arc::clone(&library),
                                retention_days,
                                tokio.clone(),
                            );
                            *app.imp().purge_handle.borrow_mut() = Some(handle);
                        }

                        // Start Immich sync engine and sync client.
                        if let Some(client) = immich_client {
                            let sync_interval = u64::from(settings.uint("sync-interval-seconds"));

                            let (sync_events_tx, sync_events_rx) =
                                tokio::sync::mpsc::unbounded_channel();

                            let handle = crate::sync::SyncHandle::start(
                                client,
                                Arc::clone(&library),
                                db_for_sync.clone(),
                                sync_events_tx,
                                sync_thumbnails_dir,
                                sync_interval,
                                tokio.clone(),
                            );
                            *app.imp().sync_handle.borrow_mut() = Some(handle);

                            let sync_client = crate::client::SyncClient::new();
                            sync_client.configure(sync_events_rx, tokio.clone());
                            sync_client.set_outbox_repository(
                                crate::sync::outbox::OutboxRepository::new(db_for_sync),
                            );
                            app.set_sync_client(sync_client);
                        }
                    }
                    Err(e) => {
                        error!("failed to open library: {e}");
                        app.show_open_failed_dialog(&window, &e);
                    }
                }
            }
        ));
    }

    /// Tell the user the library could not be opened, offering to re-run
    /// setup (closing `window`) or quit.
    fn show_open_failed_dialog(&self, window: &MomentsWindow, err: &impl std::fmt::Display) {
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Could not open library"))
            .body(with_details(
                &gettext("An error occurred while opening the library."),
                err,
            ))
            .build();
        dialog.add_response("setup", &gettext("Set Up Library"));
        dialog.add_response("quit", &gettext("Quit"));
        dialog.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("setup"));
        dialog.set_close_response("setup");

        let app_weak = self.downgrade();
        let win_weak = window.downgrade();
        dialog.connect_response(None, move |_, response| {
            if response == "setup" {
                if let Some(app) = app_weak.upgrade() {
                    if let Some(win) = win_weak.upgrade() {
                        win.close();
                    }
                    app.show_setup_window();
                }
            } else if let Some(app) = app_weak.upgrade() {
                app.quit();
            }
        });

        dialog.present(Some(window));
    }
}
