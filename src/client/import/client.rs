// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use gtk::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{gio, glib};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use super::event::ImportEvent;
use super::scan::scan_folder;
use crate::importer::{ImportPipeline, ImportProgress, ImportSummary};
use crate::library::config::LocalStorageMode;
use crate::library::Library;
use crate::renderer::pipeline::RenderPipeline;

/// Import lifecycle state exposed as a GObject property.
///
/// `Idle → Scanning → Running → Complete`. A cancelled import still ends
/// in `Complete` with the `cancelled` property set, so consumers that
/// react to a finished import (navigation, the activity indicator) handle
/// partial imports without a separate branch.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "MomentsImportState")]
pub enum ImportState {
    #[default]
    Idle,
    /// Walking the chosen folder to find files; `scanned` counts them.
    Scanning,
    /// The pipeline is processing files; `current` / `total` track it.
    Running,
    /// Finished (or cancelled); the counters hold the final summary.
    Complete,
}

/// Non-GObject dependencies for building import pipelines.
struct ImportDeps {
    library: Arc<Library>,
    originals_dir: PathBuf,
    thumbnails_dir: PathBuf,
    render_pipeline: Arc<RenderPipeline>,
    mode: LocalStorageMode,
    tokio: tokio::runtime::Handle,
    events_tx: mpsc::UnboundedSender<ImportEvent>,
}

mod imp {
    use super::*;
    use std::sync::OnceLock;

    pub struct ImportClient {
        // ── GObject properties ──────────────────────────────────────
        pub(super) state: Cell<ImportState>,
        pub(super) current: Cell<u32>,
        pub(super) total: Cell<u32>,
        pub(super) imported: Cell<u32>,
        pub(super) skipped: Cell<u32>,
        pub(super) failed: Cell<u32>,
        pub(super) elapsed_secs: Cell<f64>,
        pub(super) scanned: Cell<u32>,
        pub(super) unreadable: Cell<u32>,
        pub(super) cancelled: Cell<bool>,

        // ── Non-property dependencies ───────────────────────────────
        pub(super) deps: RefCell<Option<ImportDeps>>,
        /// Cancels the running import's pipeline; set while busy.
        pub(super) cancel_token: RefCell<Option<CancellationToken>>,
        /// Cancels the in-progress GIO folder scan; set while scanning.
        pub(super) scan_cancellable: RefCell<Option<gio::Cancellable>>,
    }

    impl Default for ImportClient {
        fn default() -> Self {
            Self {
                state: Cell::new(ImportState::Idle),
                current: Cell::new(0),
                total: Cell::new(0),
                imported: Cell::new(0),
                skipped: Cell::new(0),
                failed: Cell::new(0),
                elapsed_secs: Cell::new(0.0),
                scanned: Cell::new(0),
                unreadable: Cell::new(0),
                cancelled: Cell::new(false),
                deps: RefCell::new(None),
                cancel_token: RefCell::new(None),
                scan_cancellable: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ImportClient {
        const NAME: &'static str = "MomentsImportClient";
        type Type = super::ImportClient;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for ImportClient {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecEnum::builder::<ImportState>("state")
                        .read_only()
                        .build(),
                    glib::ParamSpecUInt::builder("current").read_only().build(),
                    glib::ParamSpecUInt::builder("total").read_only().build(),
                    glib::ParamSpecUInt::builder("imported").read_only().build(),
                    glib::ParamSpecUInt::builder("skipped").read_only().build(),
                    glib::ParamSpecUInt::builder("failed").read_only().build(),
                    glib::ParamSpecDouble::builder("elapsed-secs")
                        .read_only()
                        .build(),
                    glib::ParamSpecUInt::builder("scanned").read_only().build(),
                    glib::ParamSpecUInt::builder("unreadable")
                        .read_only()
                        .build(),
                    glib::ParamSpecBoolean::builder("cancelled")
                        .read_only()
                        .build(),
                ]
            })
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "state" => self.state.get().to_value(),
                "current" => self.current.get().to_value(),
                "total" => self.total.get().to_value(),
                "imported" => self.imported.get().to_value(),
                "skipped" => self.skipped.get().to_value(),
                "failed" => self.failed.get().to_value(),
                "elapsed-secs" => self.elapsed_secs.get().to_value(),
                "scanned" => self.scanned.get().to_value(),
                "unreadable" => self.unreadable.get().to_value(),
                "cancelled" => self.cancelled.get().to_value(),
                _ => unimplemented!(),
            }
        }
    }
}

glib::wrapper! {
    /// GObject singleton that manages import state.
    ///
    /// Holds import progress as GObject properties. UI components bind to
    /// `notify::` signals for live updates.
    ///
    /// The [`ImportPipeline`] is ephemeral — created per `import_folder()` call.
    /// Progress flows through an internal channel to a `listen()` loop
    /// that marshals updates to the GTK main thread.
    pub struct ImportClient(ObjectSubclass<imp::ImportClient>);
}

impl Default for ImportClient {
    fn default() -> Self {
        Self::new()
    }
}

impl ImportClient {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Set the dependencies required for building import pipelines
    /// and start the event listener.
    ///
    /// Must be called once after construction, before the first `import()` call.
    pub fn configure(
        &self,
        library: Arc<Library>,
        originals_dir: PathBuf,
        thumbnails_dir: PathBuf,
        render_pipeline: Arc<RenderPipeline>,
        mode: LocalStorageMode,
        tokio: tokio::runtime::Handle,
    ) {
        let (events_tx, events_rx) = mpsc::unbounded_channel();

        *self.imp().deps.borrow_mut() = Some(ImportDeps {
            library,
            originals_dir,
            thumbnails_dir,
            render_pipeline,
            mode,
            tokio: tokio.clone(),
            events_tx,
        });

        let client_weak: glib::SendWeakRef<ImportClient> = self.downgrade().into();
        tokio.spawn(Self::listen(events_rx, client_weak));
    }

    // ── Property accessors ───────────────────────────────────────────

    pub fn state(&self) -> ImportState {
        self.imp().state.get()
    }

    pub fn current(&self) -> u32 {
        self.imp().current.get()
    }

    pub fn total(&self) -> u32 {
        self.imp().total.get()
    }

    pub fn imported(&self) -> u32 {
        self.imp().imported.get()
    }

    pub fn skipped(&self) -> u32 {
        self.imp().skipped.get()
    }

    pub fn failed(&self) -> u32 {
        self.imp().failed.get()
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.imp().elapsed_secs.get()
    }

    /// Files found so far by the folder scan.
    pub fn scanned(&self) -> u32 {
        self.imp().scanned.get()
    }

    /// Unreadable directories/entries met by the last import.
    pub fn unreadable(&self) -> u32 {
        self.imp().unreadable.get()
    }

    /// Whether the last import was cancelled before finishing.
    pub fn cancelled(&self) -> bool {
        self.imp().cancelled.get()
    }

    /// Whether an import is scanning or running.
    pub fn is_busy(&self) -> bool {
        matches!(self.state(), ImportState::Scanning | ImportState::Running)
    }

    // ── Property setters (notify on change) ──────────────────────────

    fn set_state(&self, value: ImportState) {
        if self.imp().state.replace(value) != value {
            self.notify("state");
        }
    }

    fn set_current(&self, value: u32) {
        if self.imp().current.replace(value) != value {
            self.notify("current");
        }
    }

    fn set_total(&self, value: u32) {
        if self.imp().total.replace(value) != value {
            self.notify("total");
        }
    }

    fn set_imported(&self, value: u32) {
        if self.imp().imported.replace(value) != value {
            self.notify("imported");
        }
    }

    fn set_skipped(&self, value: u32) {
        if self.imp().skipped.replace(value) != value {
            self.notify("skipped");
        }
    }

    fn set_failed(&self, value: u32) {
        if self.imp().failed.replace(value) != value {
            self.notify("failed");
        }
    }

    fn set_elapsed_secs(&self, value: f64) {
        self.imp().elapsed_secs.set(value);
        self.notify("elapsed-secs");
    }

    fn set_scanned(&self, value: u32) {
        if self.imp().scanned.replace(value) != value {
            self.notify("scanned");
        }
    }

    fn set_unreadable(&self, value: u32) {
        if self.imp().unreadable.replace(value) != value {
            self.notify("unreadable");
        }
    }

    fn set_cancelled(&self, value: bool) {
        if self.imp().cancelled.replace(value) != value {
            self.notify("cancelled");
        }
    }

    // ── Event listener ───────────────────────────────────────────────

    async fn listen(
        mut rx: mpsc::UnboundedReceiver<ImportEvent>,
        client_weak: glib::SendWeakRef<ImportClient>,
    ) {
        while let Some(event) = rx.recv().await {
            let weak = client_weak.clone();
            glib::idle_add_once(move || {
                let Some(client) = weak.upgrade() else {
                    return;
                };
                client.apply_event(event);
            });
        }
        debug!("import event listener shutting down");
    }

    /// Apply one pipeline event to the properties (GTK thread).
    ///
    /// The final summary of every import — finished, cancelled, or empty —
    /// arrives here as [`ImportEvent::Complete`], so this is the one place
    /// that moves the client to `Complete`.
    fn apply_event(&self, event: ImportEvent) {
        match event {
            ImportEvent::Progress(p) => {
                self.set_current(p.current as u32);
                self.set_total(p.total as u32);
                self.set_imported(p.imported as u32);
                self.set_skipped(p.skipped as u32);
                self.set_failed(p.failed as u32);
            }
            ImportEvent::Complete(summary) => {
                self.imp().cancel_token.take();
                self.imp().scan_cancellable.take();
                self.set_imported(summary.imported as u32);
                self.set_skipped((summary.skipped_duplicates + summary.skipped_unsupported) as u32);
                self.set_failed(summary.failed as u32);
                self.set_unreadable(summary.unreadable as u32);
                self.set_cancelled(summary.cancelled);
                self.set_elapsed_secs(summary.elapsed_secs);
                // Last, so `notify::state` handlers see the final counters.
                self.set_state(ImportState::Complete);
            }
        }
    }

    // ── Import actions ───────────────────────────────────────────────

    /// Scan `folder` and import every file found below it.
    ///
    /// Takes the `gio::File` from the file chooser rather than a path: in
    /// Flatpak the document portal grants access to that object, and the
    /// `/run/user/…/doc/…` path behind it is only reachable through it. The
    /// scan therefore runs over GIO's async API on the GTK main context
    /// (the state is `Scanning` meanwhile, with `scanned` counting files),
    /// then the file list goes to an [`ImportPipeline`] on Tokio (`Running`).
    ///
    /// Ignored with a warning while another import is in progress.
    pub fn import_folder(&self, folder: gio::File) {
        if self.imp().deps.borrow().is_none() {
            error!("import called before configure()");
            return;
        }
        let Some((token, cancellable)) = self.begin() else {
            return;
        };
        info!(uri = %folder.uri(), "scanning folder for import");

        let client_weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let progress_weak = client_weak.clone();
            let scan = scan_folder(folder, &cancellable, move |count| {
                if let Some(client) = progress_weak.upgrade() {
                    client.set_scanned(count as u32);
                }
            })
            .await;

            let Some(client) = client_weak.upgrade() else {
                return;
            };
            client.imp().scan_cancellable.take();
            client.set_scanned(scan.files.len() as u32);

            let cancelled = scan.cancelled || token.is_cancelled();
            if cancelled || scan.files.is_empty() {
                if !cancelled {
                    warn!("no files found in folder");
                }
                client.send_complete(ImportSummary {
                    unreadable: scan.unreadable,
                    cancelled,
                    ..ImportSummary::default()
                });
                return;
            }
            client.start_pipeline(scan.files, scan.unreadable, token);
        });
    }

    /// Request cancellation of the current import.
    ///
    /// Stops the folder scan, or stops the pipeline before its next file
    /// (the file in flight finishes). The final summary still arrives
    /// through the usual completion path, with `cancelled` set. No-op when
    /// idle or already complete.
    pub fn cancel(&self) {
        if !self.is_busy() {
            return;
        }
        info!(state = ?self.state(), "import cancellation requested");
        if let Some(token) = self.imp().cancel_token.borrow().as_ref() {
            token.cancel();
        }
        if let Some(cancellable) = self.imp().scan_cancellable.borrow().as_ref() {
            cancellable.cancel();
        }
    }

    /// Reset the counters and enter `Scanning`, returning the new import's
    /// cancellation handles. `None` if an import is already in progress.
    fn begin(&self) -> Option<(CancellationToken, gio::Cancellable)> {
        if self.is_busy() {
            warn!("import already in progress; ignoring new request");
            return None;
        }

        self.set_current(0);
        self.set_total(0);
        self.set_imported(0);
        self.set_skipped(0);
        self.set_failed(0);
        self.set_scanned(0);
        self.set_unreadable(0);
        self.set_cancelled(false);
        self.set_elapsed_secs(0.0);

        let token = CancellationToken::new();
        let cancellable = gio::Cancellable::new();
        *self.imp().cancel_token.borrow_mut() = Some(token.clone());
        *self.imp().scan_cancellable.borrow_mut() = Some(cancellable.clone());
        self.set_state(ImportState::Scanning);
        Some((token, cancellable))
    }

    /// Route a final summary through the event listener.
    fn send_complete(&self, summary: ImportSummary) {
        if let Some(deps) = self.imp().deps.borrow().as_ref() {
            let _ = deps.events_tx.send(ImportEvent::Complete(summary));
        }
    }

    /// Build an ephemeral [`ImportPipeline`] for `files` and spawn it on
    /// Tokio. Progress and completion flow through the event channel.
    fn start_pipeline(&self, files: Vec<PathBuf>, unreadable: usize, token: CancellationToken) {
        // Extract dependencies (clone under borrow, then release).
        let (library, originals_dir, thumbnails_dir, render_pipeline, mode, tokio, events_tx) = {
            let deps = self.imp().deps.borrow();
            let Some(deps) = deps.as_ref() else {
                error!("import called before configure()");
                return;
            };
            (
                deps.library.clone(),
                deps.originals_dir.clone(),
                deps.thumbnails_dir.clone(),
                deps.render_pipeline.clone(),
                deps.mode.clone(),
                deps.tokio.clone(),
                deps.events_tx.clone(),
            )
        };

        let total = files.len();
        let progress_tx = events_tx.clone();
        // Last progress seen, so a panicked run can still be summarised.
        let last_progress: Arc<Mutex<Option<ImportProgress>>> = Arc::default();
        let progress_slot = Arc::clone(&last_progress);
        let pipeline = match ImportPipeline::builder()
            .originals_dir(originals_dir)
            .thumbnails_dir(thumbnails_dir)
            .library(library)
            .render_pipeline(render_pipeline)
            .mode(mode)
            .cancellation_token(token)
            .on_progress(move |p| {
                if let Ok(mut slot) = progress_slot.lock() {
                    *slot = Some(p.clone());
                }
                let _ = progress_tx.send(ImportEvent::Progress(p));
            })
            .build()
        {
            Ok(p) => p,
            Err(e) => {
                // Nothing was attempted, so report the error rather than
                // a summary of "failed" files, and go back to Idle.
                error!(error = %e, "failed to build import pipeline");
                crate::client::show_error_toast(&e);
                self.imp().cancel_token.take();
                self.imp().scan_cancellable.take();
                self.set_state(ImportState::Idle);
                return;
            }
        };

        self.set_total(total as u32);
        self.set_state(ImportState::Running);

        info!(count = total, "starting import");
        let run = tokio.spawn(pipeline.run_files(files));
        tokio.spawn(async move {
            let mut summary = await_pipeline(run, &last_progress, total).await;
            summary.unreadable += unreadable;
            let _ = events_tx.send(ImportEvent::Complete(summary));
        });
    }
}

/// Await the pipeline task, turning a panic into a summary.
///
/// Without this a panicking run would never send `Complete`, leaving the
/// client `Running` (busy) for the rest of the session and refusing every
/// later import. On a panic the counts come from the last progress update
/// and every file not accounted for is reported as failed.
async fn await_pipeline(
    run: tokio::task::JoinHandle<ImportSummary>,
    last_progress: &Mutex<Option<ImportProgress>>,
    total: usize,
) -> ImportSummary {
    match run.await {
        Ok(summary) => summary,
        Err(e) => {
            error!(error = %e, "import pipeline task failed");
            let last = last_progress.lock().ok().and_then(|p| p.clone());
            let (imported, skipped, failed, done) = last
                .map(|p| (p.imported, p.skipped, p.failed, p.current))
                .unwrap_or_default();
            ImportSummary {
                imported,
                skipped_duplicates: skipped,
                failed: failed + total.saturating_sub(done),
                ..ImportSummary::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::importer::{ImportProgress, ImportSummary};

    // ── Construction & defaults ──────────────────────────────────────

    #[test]
    fn new_client_has_idle_state() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        assert_eq!(client.state(), ImportState::Idle);
    }

    #[test]
    fn new_client_has_zero_counters() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        assert_eq!(client.current(), 0);
        assert_eq!(client.total(), 0);
        assert_eq!(client.imported(), 0);
        assert_eq!(client.skipped(), 0);
        assert_eq!(client.failed(), 0);
        assert_eq!(client.elapsed_secs(), 0.0);
    }

    // ── Property setters ────────────────────────────────────────────

    #[test]
    fn set_state_updates_value() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        client.set_state(ImportState::Running);
        assert_eq!(client.state(), ImportState::Running);

        client.set_state(ImportState::Complete);
        assert_eq!(client.state(), ImportState::Complete);
    }

    #[test]
    fn set_counters_update_values() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        client.set_current(5);
        client.set_total(10);
        client.set_imported(3);
        client.set_skipped(1);
        client.set_failed(1);
        client.set_elapsed_secs(2.5);

        assert_eq!(client.current(), 5);
        assert_eq!(client.total(), 10);
        assert_eq!(client.imported(), 3);
        assert_eq!(client.skipped(), 1);
        assert_eq!(client.failed(), 1);
        assert_eq!(client.elapsed_secs(), 2.5);
    }

    #[test]
    fn set_state_no_notify_on_same_value() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        let notified = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = notified.clone();
        client.connect_notify_local(Some("state"), move |_, _| {
            flag.set(true);
        });

        client.set_state(ImportState::Idle);
        assert!(!notified.get());

        client.set_state(ImportState::Running);
        assert!(notified.get());
    }

    // ── GObject property access ─────────────────────────────────────

    #[test]
    fn gobject_property_reads_match_accessors() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        client.set_state(ImportState::Running);
        client.set_current(7);
        client.set_total(20);
        client.set_imported(5);
        client.set_skipped(1);
        client.set_failed(1);

        let state: ImportState = client.property("state");
        assert_eq!(state, ImportState::Running);

        let current: u32 = client.property("current");
        assert_eq!(current, 7);

        let total: u32 = client.property("total");
        assert_eq!(total, 20);
    }

    // ── Scan / cancel state machine ─────────────────────────────────

    fn summary(imported: usize, cancelled: bool, unreadable: usize) -> ImportSummary {
        ImportSummary {
            imported,
            cancelled,
            unreadable,
            ..ImportSummary::default()
        }
    }

    #[test]
    fn begin_enters_scanning_and_is_busy() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        assert!(client.begin().is_some());
        assert_eq!(client.state(), ImportState::Scanning);
        assert!(client.is_busy());
    }

    #[test]
    fn begin_while_busy_is_refused() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        let (first, _) = client.begin().unwrap();
        assert!(client.begin().is_none());
        client.set_state(ImportState::Running);
        assert!(client.begin().is_none());
        assert!(!first.is_cancelled());
    }

    #[test]
    fn cancel_while_scanning_fires_token_and_cancellable() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        let (token, cancellable) = client.begin().unwrap();
        client.cancel();
        assert!(token.is_cancelled());
        assert!(cancellable.is_cancelled());
        // Still busy until the final summary arrives.
        assert_eq!(client.state(), ImportState::Scanning);
    }

    #[test]
    fn cancel_while_running_fires_token() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        let (token, _) = client.begin().unwrap();
        client.imp().scan_cancellable.take();
        client.set_state(ImportState::Running);
        client.cancel();
        assert!(token.is_cancelled());
        assert_eq!(client.state(), ImportState::Running);
    }

    #[test]
    fn cancel_when_idle_is_noop() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        client.cancel();
        assert_eq!(client.state(), ImportState::Idle);
        assert!(!client.cancelled());
    }

    #[test]
    fn cancelled_summary_completes_with_flag() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        client.begin().unwrap();
        client.set_state(ImportState::Running);
        client.cancel();

        // Counters must be final by the time `state` notifies.
        let seen = std::rc::Rc::new(std::cell::Cell::new(None));
        let flag = seen.clone();
        client.connect_notify_local(Some("state"), move |c, _| {
            flag.set(Some((c.imported(), c.cancelled(), c.unreadable())));
        });
        client.apply_event(ImportEvent::Complete(summary(3, true, 2)));

        assert_eq!(client.state(), ImportState::Complete);
        assert_eq!(seen.get(), Some((3, true, 2)));
        assert!(!client.is_busy());
        assert!(client.imp().cancel_token.borrow().is_none());
        assert!(client.imp().scan_cancellable.borrow().is_none());
    }

    #[test]
    fn new_import_resets_scan_and_cancel_state() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        client.begin().unwrap();
        client.set_scanned(42);
        client.apply_event(ImportEvent::Complete(summary(1, true, 4)));
        assert!(client.cancelled());

        client.begin().unwrap();
        assert_eq!(client.state(), ImportState::Scanning);
        assert!(!client.cancelled());
        assert_eq!(client.unreadable(), 0);
        assert_eq!(client.scanned(), 0);
        assert_eq!(client.imported(), 0);
    }

    #[test]
    fn second_import_after_complete_enters_scanning() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        client.begin().unwrap();
        client.apply_event(ImportEvent::Complete(summary(2, false, 0)));
        assert_eq!(client.state(), ImportState::Complete);

        let states = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let log = states.clone();
        client.connect_notify_local(Some("state"), move |c, _| {
            log.borrow_mut().push(c.state());
        });
        assert!(client.begin().is_some());
        assert_eq!(*states.borrow(), vec![ImportState::Scanning]);
        assert!(client.is_busy());
    }

    #[tokio::test]
    async fn panicked_pipeline_still_yields_summary() {
        let last = Mutex::new(Some(ImportProgress {
            current: 3,
            total: 10,
            imported: 2,
            skipped: 1,
            failed: 0,
            imported_id: None,
        }));
        let run = tokio::spawn(async {
            if true {
                panic!("pipeline blew up");
            }
            ImportSummary::default()
        });

        let summary = await_pipeline(run, &last, 10).await;
        assert_eq!(summary.imported, 2);
        assert_eq!(summary.skipped_duplicates, 1);
        assert_eq!(summary.failed, 7, "the 7 unprocessed files count as failed");
        assert!(!summary.cancelled);
    }

    #[tokio::test]
    async fn panic_before_any_progress_fails_every_file() {
        let last = Mutex::new(None);
        let run = tokio::spawn(async {
            if true {
                panic!("pipeline blew up");
            }
            ImportSummary::default()
        });
        let summary = await_pipeline(run, &last, 4).await;
        assert_eq!(summary.failed, 4);
        assert_eq!(summary.imported, 0);
    }

    #[test]
    fn import_folder_without_configure_stays_idle() {
        crate::client::test_support::ensure_gobject_types();
        let client = ImportClient::new();
        client.import_folder(gio::File::for_path("/nonexistent"));
        assert_eq!(client.state(), ImportState::Idle);
    }

    // ── ImportEvent ─────────────────────────────────────────────────

    #[test]
    fn import_event_progress_wraps_data() {
        crate::client::test_support::ensure_gobject_types();
        let p = ImportProgress {
            current: 3,
            total: 10,
            imported: 2,
            skipped: 1,
            failed: 0,
            imported_id: None,
        };
        let event = ImportEvent::Progress(p.clone());
        assert!(matches!(event, ImportEvent::Progress(ref inner) if inner.current == 3));
    }

    #[test]
    fn import_event_complete_wraps_summary() {
        crate::client::test_support::ensure_gobject_types();
        let s = ImportSummary {
            imported: 5,
            skipped_duplicates: 2,
            skipped_unsupported: 0,
            failed: 1,
            unreadable: 0,
            cancelled: false,
            elapsed_secs: 3.5,
        };
        let event = ImportEvent::Complete(s);
        assert!(matches!(event, ImportEvent::Complete(ref inner) if inner.imported == 5));
    }
}
