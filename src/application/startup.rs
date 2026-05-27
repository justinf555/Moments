// SPDX-License-Identifier: GPL-3.0-or-later
//
// Phase-based startup for `MomentsApplication`.
//
// The legacy entry point was the 270-line `load_library_async` on
// `MomentsApplication`. That function did seven distinct things in one
// place: open the library, build the render pipeline, wire the import
// client, the album / people / media clients, kick off the periodic
// trash purge, and conditionally start the Immich sync engine + sync
// client. There was no compile-time ordering between those steps.
//
// Step 5 of the LibraryContext refactor splits that work into four
// typed phases. Each phase function consumes the previous phase's
// output, so the compiler enforces the ordering: you cannot build a
// client before the `LibraryContext` exists, because the constructor
// arguments do not exist. See `docs/design-library-context.md`,
// "Startup phases".
//
// The phases:
//
// 1. [`phase1_domain`] — open the `Library`, build the
//    `RenderPipeline`, wrap them in a [`LibraryContext`].
// 2. [`phase2_background_services`] — conditionally start the Immich
//    sync engine. Step 6 will reshape `SyncHandle` into a dedicated
//    `SyncEngine` top-level service; today the existing `SyncHandle`
//    shape is preserved unchanged.
// 3. [`phase3_clients`] — build the UI-facing client GObjects with
//    minimal concrete dependencies from the context.
// 4. [`phase4_install`] — install the context, sync handle, and
//    clients on the application, then start the periodic purge task
//    and wire the main window.
//
// Error handling matches the legacy function: open failures surface as
// an `AdwAlertDialog` over the main window with "Set Up Library" /
// "Quit" responses. Internal phase functions return `LibraryError` and
// the orchestrator translates the failure into that dialog. The
// orchestrator itself is fire-and-forget on the GTK main context —
// `start` does not return a `Result` because there is no caller
// equipped to handle a startup failure differently from the dialog.

use std::path::PathBuf;
use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use tracing::{error, info, instrument, warn};

use crate::application::context::LibraryContext;
use crate::application::MomentsApplication;
use crate::client::{AlbumClientV2, ImportClient, MediaClientV2, PeopleClientV2, SyncClient};
use crate::library::bundle::Bundle;
use crate::library::config::{LibraryConfig, LocalStorageMode};
use crate::library::db::Database;
use crate::library::error::LibraryError;
use crate::library::Library;
use crate::renderer::pipeline::RenderPipeline;
use crate::sync::providers::immich::client::ImmichClient;
use crate::sync::SyncHandle;
use crate::ui::MomentsWindow;

/// Connection details for an Immich backend pulled out of the
/// [`LibraryConfig`] before the config is consumed by `Library::open`.
#[derive(Clone)]
struct ImmichInfo {
    server_url: String,
    access_token: String,
}

/// Static paths and storage mode the [`ImportClient`] needs after the
/// bundle has been consumed by the library open call.
struct ImportPaths {
    originals_dir: PathBuf,
    thumbnails_dir: PathBuf,
    storage_mode: LocalStorageMode,
}

/// Output of [`phase1_domain`]: the constructed [`LibraryContext`]
/// plus the [`Database`] handle, paths, and the (optional) Immich
/// client that later phases need.
///
/// The `db` field is carried forward so the sync push/pull pipeline
/// and the [`crate::sync::outbox::OutboxRepository`] used by
/// [`SyncClient`] share the same `SqlitePool` as the library.
struct DomainArtifacts {
    ctx: Arc<LibraryContext>,
    db: Database,
    paths: ImportPaths,
    immich_client: Option<ImmichClient>,
    thumbnails_dir_for_sync: PathBuf,
}

/// Output of [`phase2_background_services`]. When Immich is configured
/// the sync engine is started and the receiving end of its event
/// channel is handed to phase 3 so [`SyncClient`] can subscribe.
struct BackgroundServices {
    sync_handle: Option<SyncHandle>,
    sync_events_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::sync::event::SyncEvent>>,
    db: Database,
}

/// Output of [`phase3_clients`]: every client GObject that phase 4
/// installs on the application.
///
/// `sync_client` is optional because the Immich sync engine is only
/// started for Immich-backed libraries.
struct BuiltClients {
    import_client: ImportClient,
    album_client: AlbumClientV2,
    people_client: PeopleClientV2,
    media_client: MediaClientV2,
    sync_client: Option<SyncClient>,
}

/// Run all four startup phases for the given library bundle.
///
/// Spawns the work on the GLib main context so the GTK side stays
/// responsive while `Library::open` runs on the Tokio runtime. On any
/// failure during phase 1 (the only fallible phase today) an
/// `AdwAlertDialog` is presented over `window` so the user can either
/// return to the setup wizard or quit.
#[instrument(skip(app, bundle, config, window))]
pub(in crate::application) fn start(
    app: &MomentsApplication,
    bundle: Bundle,
    config: LibraryConfig,
    window: MomentsWindow,
) {
    // Pull the values phase 1 needs out of the config + bundle before
    // they are consumed.
    let immich_info = match &config {
        LibraryConfig::Immich {
            server_url,
            access_token,
        } => Some(ImmichInfo {
            server_url: server_url.clone(),
            access_token: access_token.clone(),
        }),
        _ => None,
    };

    // Store backend type for the preferences dialog. This is read
    // synchronously when the user opens preferences, so it must be in
    // place before the main window finishes setup.
    if let Some(ref info) = immich_info {
        app.imp().is_immich.set(true);
        *app.imp().immich_server_url.borrow_mut() = Some(info.server_url.clone());
    }

    let paths = ImportPaths {
        originals_dir: bundle.originals.clone(),
        thumbnails_dir: bundle.thumbnails.clone(),
        storage_mode: match &config {
            LibraryConfig::Local { mode } => mode.clone(),
            LibraryConfig::Immich { .. } => LocalStorageMode::Managed,
        },
    };
    let thumbnails_dir_for_sync = bundle.thumbnails.clone();
    let tokio = app.tokio_handle();

    glib::MainContext::default().spawn_local(glib::clone!(
        #[weak]
        app,
        #[weak]
        window,
        async move {
            let domain = match phase1_domain(
                bundle,
                config,
                paths,
                immich_info,
                thumbnails_dir_for_sync,
                tokio,
            )
            .await
            {
                Ok(d) => d,
                Err(e) => {
                    present_open_error(&app, &window, e);
                    return;
                }
            };

            let background = phase2_background_services(&app, &domain);

            let clients = phase3_clients(&domain, background.sync_events_rx, background.db.clone());

            phase4_install(&app, &window, domain, background.sync_handle, clients);
        }
    ));
}

/// Phase 1 — open the library and build the render pipeline.
///
/// Returns the [`LibraryContext`] alongside the [`Database`], the
/// import paths, and the optional Immich HTTP client. The database
/// handle is carried forward (rather than re-opened from the bundle)
/// so phases 2 and 3 share the exact `SqlitePool` that `Library`
/// holds.
#[instrument(skip_all)]
async fn phase1_domain(
    bundle: Bundle,
    config: LibraryConfig,
    paths: ImportPaths,
    immich_info: Option<ImmichInfo>,
    thumbnails_dir_for_sync: PathBuf,
    tokio: tokio::runtime::Handle,
) -> Result<DomainArtifacts, LibraryError> {
    let db = Database::new();

    let immich_client = immich_info
        .as_ref()
        .and_then(|info| ImmichClient::new(&info.server_url, &info.access_token).ok());

    let recorder: Arc<dyn crate::library::recorder::MutationRecorder> = if immich_client.is_some() {
        Arc::new(crate::sync::outbox::QueueWriterOutbox::new(db.clone()))
    } else {
        Arc::new(crate::sync::outbox::NoOpRecorder)
    };

    let resolver: Arc<dyn crate::library::resolver::OriginalResolver> =
        if let Some(ref client) = immich_client {
            Arc::new(
                crate::sync::providers::immich::resolver::CachedResolver::new(
                    Arc::new(client.clone()),
                    paths.originals_dir.clone(),
                ),
            )
        } else {
            Arc::new(crate::library::resolver::LocalResolver::new(
                paths.originals_dir.clone(),
                paths.storage_mode.clone(),
            ))
        };

    let storage_mode = match &config {
        LibraryConfig::Local { mode } => mode.clone(),
        LibraryConfig::Immich { .. } => LocalStorageMode::Managed,
    };

    let db_for_open = db.clone();
    let library =
        tokio
            .spawn(async move {
                Library::open(bundle, storage_mode, db_for_open, recorder, resolver).await
            })
            .await
            .map_err(|e| LibraryError::Runtime(e.to_string()))??;
    let library = Arc::new(library);
    info!("library ready");

    let render_pipeline = Arc::new(RenderPipeline::new());
    let ctx = LibraryContext::build(Arc::clone(&library), render_pipeline, tokio);

    Ok(DomainArtifacts {
        ctx,
        db,
        paths,
        immich_client,
        thumbnails_dir_for_sync,
    })
}

/// Phase 2 — start long-running background services.
///
/// Today the only long-running service in scope is the Immich
/// [`SyncHandle`]. Step 6 of the refactor reshapes this into a
/// dedicated `SyncEngine` top-level service with a paired
/// [`SyncClient`]; until then the existing handle structure is kept
/// unchanged and threaded through to phase 4.
///
/// The periodic trash-purge task is *not* started here — it is
/// recorded on the [`LibraryContext`] itself and started in phase 4
/// alongside window setup so the retention-days setting and the
/// `JoinHandle` registration happen in one place.
#[instrument(skip_all)]
fn phase2_background_services(
    app: &MomentsApplication,
    domain: &DomainArtifacts,
) -> BackgroundServices {
    let Some(client) = domain.immich_client.clone() else {
        return BackgroundServices {
            sync_handle: None,
            sync_events_rx: None,
            db: domain.db.clone(),
        };
    };

    let sync_interval = app
        .imp()
        .settings
        .get()
        .expect("settings initialised")
        .uint("sync-interval-seconds") as u64;

    let (sync_events_tx, sync_events_rx) = tokio::sync::mpsc::unbounded_channel();
    let handle = SyncHandle::start(
        client,
        Arc::clone(domain.ctx.library()),
        domain.db.clone(),
        sync_events_tx,
        domain.thumbnails_dir_for_sync.clone(),
        sync_interval,
        domain.ctx.tokio().clone(),
    );

    BackgroundServices {
        sync_handle: Some(handle),
        sync_events_rx: Some(sync_events_rx),
        db: domain.db.clone(),
    }
}

/// Phase 3 — build the UI-facing client GObjects.
///
/// Each client is given the exact concrete sub-services it needs —
/// per [`docs/design-library-context.md`](../../docs/design-library-context.md),
/// clients do not see the whole [`LibraryContext`]. The `sync_client`
/// is only built when phase 2 produced a sync handle.
#[instrument(skip_all)]
fn phase3_clients(
    domain: &DomainArtifacts,
    sync_events_rx: Option<tokio::sync::mpsc::UnboundedReceiver<crate::sync::event::SyncEvent>>,
    db: Database,
) -> BuiltClients {
    let ctx = &domain.ctx;

    let import_client = ImportClient::build(
        Arc::clone(ctx.library()),
        domain.paths.originals_dir.clone(),
        domain.paths.thumbnails_dir.clone(),
        Arc::clone(ctx.render_pipeline()),
        domain.paths.storage_mode.clone(),
        ctx.tokio().clone(),
    );

    let albums_rx = ctx.library().albums().subscribe();
    let album_client =
        AlbumClientV2::build(Arc::clone(ctx.library()), ctx.tokio().clone(), albums_rx);

    let faces_rx = ctx.library().faces().subscribe();
    let people_client =
        PeopleClientV2::build(Arc::clone(ctx.library()), ctx.tokio().clone(), faces_rx);

    let media_client = MediaClientV2::build(
        Arc::clone(ctx.library()),
        ctx.tokio().clone(),
        Arc::clone(ctx.render_pipeline()),
    );

    let sync_client = sync_events_rx.map(|rx| {
        let client = SyncClient::build(rx, ctx.tokio().clone());
        client.set_outbox_repository(crate::sync::outbox::OutboxRepository::new(db));
        client
    });

    BuiltClients {
        import_client,
        album_client,
        people_client,
        media_client,
        sync_client,
    }
}

/// Phase 4 — install everything on the application and start the
/// periodic purge task.
///
/// This is the only phase that mutates [`MomentsApplication`] state.
/// Earlier phases hand back typed values; phase 4 owns the policy of
/// "when in startup are these visible to widgets". Window setup is
/// the very last step so widgets only resolve their client
/// dependencies once every singleton is in place.
#[instrument(skip_all)]
fn phase4_install(
    app: &MomentsApplication,
    window: &MomentsWindow,
    domain: DomainArtifacts,
    sync_handle: Option<SyncHandle>,
    clients: BuiltClients,
) {
    let DomainArtifacts { ctx, .. } = domain;

    if app.imp().library_context.borrow().is_some() {
        warn!("library_context was already initialised — overwriting");
    }
    *app.imp().library_context.borrow_mut() = Some(Arc::clone(&ctx));

    app.set_import_client(clients.import_client);
    app.imp()
        .album_client_v2
        .set(clients.album_client)
        .expect("album_client_v2 set once per startup");
    app.imp()
        .people_client
        .set(clients.people_client)
        .expect("people_client set once per startup");
    app.imp()
        .media_client_v2
        .set(clients.media_client)
        .expect("media_client_v2 set once per startup");

    if let Some(handle) = sync_handle {
        *app.imp().sync_handle.borrow_mut() = Some(handle);
    }
    if let Some(client) = clients.sync_client {
        app.set_sync_client(client);
    }

    // Start the periodic trash-purge task. The context is the single
    // canonical owner of its `JoinHandle` (handles are not `Clone`).
    let retention_days = app
        .imp()
        .settings
        .get()
        .expect("settings initialised")
        .uint("trash-retention-days");
    let purge_handle = crate::tasks::purge_trash::start(
        Arc::clone(ctx.library()),
        retention_days,
        ctx.tokio().clone(),
    );
    if let Err(unused) = ctx.set_purge_handle(purge_handle) {
        // Defensive: a duplicate populate could only happen if `start`
        // ran twice on the same Application, which the rest of the
        // code prevents. Abort the orphan task so it doesn't outlive
        // its (now-redundant) state.
        warn!("purge_handle already recorded on LibraryContext — aborting duplicate task");
        unused.abort();
    }

    // Wire the shell: builds sidebar, registers views, switches to
    // the content page. Widgets resolve their client dependencies
    // through `MomentsApplication::default()` accessors, which are
    // now all populated.
    let settings = app
        .imp()
        .settings
        .get()
        .expect("settings initialised")
        .clone();
    window.setup(settings);
}

/// Present an `AdwAlertDialog` for a phase-1 library open failure and
/// wire the response actions (return to setup wizard / quit).
fn present_open_error(app: &MomentsApplication, window: &MomentsWindow, err: LibraryError) {
    error!("failed to open library: {err}");

    let dialog = adw::AlertDialog::builder()
        .heading("Could not open library")
        .body(format!(
            "An error occurred while opening the library.\n\nDetails: {err}"
        ))
        .build();
    dialog.add_response("setup", "Set Up Library");
    dialog.add_response("quit", "Quit");
    dialog.set_response_appearance("quit", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("setup"));
    dialog.set_close_response("setup");

    let app_weak = app.downgrade();
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
