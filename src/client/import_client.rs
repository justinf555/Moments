use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::Arc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use tracing::{error, info};

use crate::event_bus::EventSender;
use crate::importer::ImportPipeline;
use crate::library::config::LocalStorageMode;
use crate::renderer::format::FormatRegistry;
use crate::library::Library;

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
    formats: Arc<FormatRegistry>,
    mode: LocalStorageMode,
    tokio: tokio::runtime::Handle,
    bus: EventSender,
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
    /// Holds import progress as GObject properties. UI components access it
    /// via `MomentsApplication::default().import_client()` and connect to
    /// `notify::` signals or bind properties.
    ///
    /// The [`ImportPipeline`] is ephemeral — created per import run, writes
    /// to this client's properties while running, dropped on completion.
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

    /// Set the dependencies required for building import pipelines.
    ///
    /// Must be called once after construction, before the first `import()` call.
    pub fn configure(
        &self,
        library: Arc<Library>,
        originals_dir: PathBuf,
        thumbnails_dir: PathBuf,
        formats: Arc<FormatRegistry>,
        mode: LocalStorageMode,
        tokio: tokio::runtime::Handle,
        bus: EventSender,
    ) {
        *self.imp().deps.borrow_mut() = Some(ImportDeps {
            library,
            originals_dir,
            thumbnails_dir,
            formats,
            mode,
            tokio,
            bus,
        });
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

    // ── Import action ────────────────────────────────────────────────

    /// Start an import of the given source files/directories.
    ///
    /// Sets `state` to `Running`, resets counters, builds an ephemeral
    /// [`ImportPipeline`], and spawns it on Tokio. Progress and completion
    /// are reflected as property updates on this GObject (marshalled to the
    /// GTK main thread via `glib::idle_add_once`).
    pub fn import(&self, sources: Vec<PathBuf>) {
        // Extract dependencies (clone under borrow, then release).
        let (library, originals_dir, thumbnails_dir, formats, mode, tokio, bus) = {
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
                deps.formats.clone(),
                deps.mode.clone(),
                deps.tokio.clone(),
                deps.bus.clone(),
            )
        };

        // Reset state.
        self.set_state(ImportState::Running);
        self.set_current(0);
        self.set_total(0);
        self.set_imported(0);
        self.set_skipped(0);
        self.set_failed(0);
        self.set_elapsed_secs(0.0);

        // Use SendWeakRef for the Tokio-spawned future (GObject is !Send).
        let progress_weak: glib::SendWeakRef<ImportClient> = self.downgrade().into();
        let complete_weak: glib::SendWeakRef<ImportClient> = self.downgrade().into();

        let pipeline = match ImportPipeline::builder()
            .originals_dir(originals_dir)
            .thumbnails_dir(thumbnails_dir)
            .library(library)
            .formats(formats)
            .mode(mode)
            .on_progress(move |p| {
                let weak = progress_weak.clone();
                let imported_id = p.imported_id.clone();
                let bus = bus.clone();
                glib::idle_add_once(move || {
                    if let Some(client) = weak.upgrade() {
                        client.set_current(p.current as u32);
                        client.set_total(p.total as u32);
                        client.set_imported(p.imported as u32);
                        client.set_skipped(p.skipped as u32);
                        client.set_failed(p.failed as u32);
                    }
                    if let Some(id) = imported_id {
                        bus.send(crate::app_event::AppEvent::AssetImported { id });
                    }
                });
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

            // Marshal final state to GTK thread.
            glib::idle_add_once(move || {
                if let Some(client) = complete_weak.upgrade() {
                    client.set_imported(summary.imported as u32);
                    client.set_skipped(
                        (summary.skipped_duplicates + summary.skipped_unsupported) as u32,
                    );
                    client.set_failed(summary.failed as u32);
                    client.set_elapsed_secs(summary.elapsed_secs);
                    client.set_state(ImportState::Complete);
                }
            });
        });
    }
}
