use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use tokio::sync::mpsc;
use tracing::{debug, error, info};

use super::event::ImportEvent;
use crate::importer::ImportPipeline;
use crate::library::config::LocalStorageMode;
use crate::library::Library;
use crate::renderer::pipeline::RenderPipeline;

/// Import lifecycle state exposed as a GObject property.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "MomentsImportState")]
pub enum ImportState {
    #[default]
    Idle,
    Running,
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

        // ── Non-property dependencies ───────────────────────────────
        pub(super) deps: RefCell<Option<ImportDeps>>,
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
                deps: RefCell::new(None),
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
    /// The [`ImportPipeline`] is ephemeral — created per `import()` call.
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
                match event {
                    ImportEvent::Progress(p) => {
                        client.set_current(p.current as u32);
                        client.set_total(p.total as u32);
                        client.set_imported(p.imported as u32);
                        client.set_skipped(p.skipped as u32);
                        client.set_failed(p.failed as u32);
                    }
                    ImportEvent::Complete(summary) => {
                        client.set_imported(summary.imported as u32);
                        client.set_skipped(
                            (summary.skipped_duplicates + summary.skipped_unsupported) as u32,
                        );
                        client.set_failed(summary.failed as u32);
                        client.set_elapsed_secs(summary.elapsed_secs);
                        client.set_state(ImportState::Complete);
                    }
                }
            });
        }
        debug!("import event listener shutting down");
    }

    // ── Import action ────────────────────────────────────────────────

    /// Start an import of the given source files/directories.
    ///
    /// Sets `state` to `Running`, resets counters, builds an ephemeral
    /// [`ImportPipeline`], and spawns it on Tokio. Progress and completion
    /// flow through the internal event channel to `listen()`.
    pub fn import(&self, sources: Vec<PathBuf>) {
        // Extract dependencies (clone under borrow, then release).
        let (library, originals_dir, thumbnails_dir, render_pipeline, mode, tokio, events_tx) = {
            let deps = self.imp().deps.borrow();
            let deps = match deps.as_ref() {
                Some(d) => d,
                None => {
                    error!("import called before configure()");
                    return;
                }
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

        // Reset state (called from GTK thread, safe to set directly).
        self.set_state(ImportState::Running);
        self.set_current(0);
        self.set_total(0);
        self.set_imported(0);
        self.set_skipped(0);
        self.set_failed(0);
        self.set_elapsed_secs(0.0);

        let progress_tx = events_tx.clone();
        let pipeline = match ImportPipeline::builder()
            .originals_dir(originals_dir)
            .thumbnails_dir(thumbnails_dir)
            .library(library)
            .render_pipeline(render_pipeline)
            .mode(mode)
            .on_progress(move |p| {
                let _ = progress_tx.send(ImportEvent::Progress(p));
            })
            .build()
        {
            Ok(p) => p,
            Err(e) => {
                error!("failed to build import pipeline: {e}");
                self.set_state(ImportState::Idle);
                return;
            }
        };

        info!(count = sources.len(), "starting import");
        tokio.spawn(async move {
            let summary = pipeline.run(sources).await;
            let _ = events_tx.send(ImportEvent::Complete(summary));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::importer::{ImportProgress, ImportSummary};

    // ── Construction & defaults ──────────────────────────────────────

    #[test]
    fn new_client_has_idle_state() {
        let client = ImportClient::new();
        assert_eq!(client.state(), ImportState::Idle);
    }

    #[test]
    fn new_client_has_zero_counters() {
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
        let client = ImportClient::new();
        client.set_state(ImportState::Running);
        assert_eq!(client.state(), ImportState::Running);

        client.set_state(ImportState::Complete);
        assert_eq!(client.state(), ImportState::Complete);
    }

    #[test]
    fn set_counters_update_values() {
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

    // ── ImportEvent ─────────────────────────────────────────────────

    #[test]
    fn import_event_progress_wraps_data() {
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
        let s = ImportSummary {
            imported: 5,
            skipped_duplicates: 2,
            skipped_unsupported: 0,
            failed: 1,
            elapsed_secs: 3.5,
        };
        let event = ImportEvent::Complete(s);
        assert!(matches!(event, ImportEvent::Complete(ref inner) if inner.imported == 5));
    }
}
