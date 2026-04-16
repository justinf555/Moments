use std::cell::{Cell, RefCell};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use tokio::sync::mpsc;
use tracing::debug;

use crate::sync::event::SyncEvent;

/// Sync lifecycle state exposed as a GObject property.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "MomentsSyncState")]
pub enum SyncState {
    #[default]
    Idle,
    Syncing,
    Complete,
    Error,
    Offline,
}

mod imp {
    use super::*;
    use std::sync::OnceLock;

    pub struct SyncClient {
        // ── GObject properties ──────────────────────────────────────
        pub(super) state: Cell<SyncState>,
        pub(super) items_processed: Cell<u32>,
        pub(super) errors: Cell<u32>,
        pub(super) last_synced_at: Cell<i64>,
        pub(super) error_message: RefCell<String>,
    }

    impl Default for SyncClient {
        fn default() -> Self {
            Self {
                state: Cell::new(SyncState::Idle),
                items_processed: Cell::new(0),
                errors: Cell::new(0),
                last_synced_at: Cell::new(0),
                error_message: RefCell::new(String::new()),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SyncClient {
        const NAME: &'static str = "MomentsSyncClient";
        type Type = super::SyncClient;
        type ParentType = glib::Object;
    }

    impl ObjectImpl for SyncClient {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecEnum::builder::<SyncState>("state")
                        .read_only()
                        .build(),
                    glib::ParamSpecUInt::builder("items-processed")
                        .read_only()
                        .build(),
                    glib::ParamSpecUInt::builder("errors").read_only().build(),
                    glib::ParamSpecInt64::builder("last-synced-at")
                        .read_only()
                        .build(),
                    glib::ParamSpecString::builder("error-message")
                        .read_only()
                        .build(),
                ]
            })
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "state" => self.state.get().to_value(),
                "items-processed" => self.items_processed.get().to_value(),
                "errors" => self.errors.get().to_value(),
                "last-synced-at" => self.last_synced_at.get().to_value(),
                "error-message" => self.error_message.borrow().to_value(),
                _ => unimplemented!(),
            }
        }
    }
}

glib::wrapper! {
    /// GObject singleton that exposes sync engine state to the UI.
    ///
    /// Holds sync progress and status as GObject properties. The future
    /// `ActivityIndicator` widget binds to these properties for display.
    ///
    /// Unlike Album/People clients, SyncClient does not manage ListStore
    /// models — it is purely a state holder.
    pub struct SyncClient(ObjectSubclass<imp::SyncClient>);
}

impl Default for SyncClient {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncClient {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Start listening for sync events.
    ///
    /// Must be called once after construction. Spawns a background task
    /// on the Tokio runtime that receives `SyncEvent`s and updates
    /// GObject properties on the GTK main thread.
    pub fn configure(
        &self,
        events_rx: mpsc::UnboundedReceiver<SyncEvent>,
        tokio: tokio::runtime::Handle,
    ) {
        let client_weak: glib::SendWeakRef<SyncClient> = self.downgrade().into();
        tokio.spawn(Self::listen(events_rx, client_weak));
    }

    // ── Property accessors ───────────────────────────────────────────

    pub fn state(&self) -> SyncState {
        self.imp().state.get()
    }

    pub fn items_processed(&self) -> u32 {
        self.imp().items_processed.get()
    }

    pub fn errors(&self) -> u32 {
        self.imp().errors.get()
    }

    pub fn last_synced_at(&self) -> i64 {
        self.imp().last_synced_at.get()
    }

    pub fn error_message(&self) -> String {
        self.imp().error_message.borrow().clone()
    }

    // ── Property setters (notify on change) ──────────────────────────

    fn set_state(&self, value: SyncState) {
        if self.imp().state.replace(value) != value {
            self.notify("state");
        }
    }

    fn set_items_processed(&self, value: u32) {
        if self.imp().items_processed.replace(value) != value {
            self.notify("items-processed");
        }
    }

    fn set_errors(&self, value: u32) {
        if self.imp().errors.replace(value) != value {
            self.notify("errors");
        }
    }

    fn set_last_synced_at(&self, value: i64) {
        if self.imp().last_synced_at.replace(value) != value {
            self.notify("last-synced-at");
        }
    }

    fn set_error_message(&self, value: &str) {
        let changed = {
            let mut current = self.imp().error_message.borrow_mut();
            if current.as_str() != value {
                *current = value.to_string();
                true
            } else {
                false
            }
        };
        if changed {
            self.notify("error-message");
        }
    }

    // ── Event listener ───────────────────────────────────────────────

    async fn listen(
        mut rx: mpsc::UnboundedReceiver<SyncEvent>,
        client_weak: glib::SendWeakRef<SyncClient>,
    ) {
        while let Some(event) = rx.recv().await {
            let weak = client_weak.clone();
            glib::idle_add_once(move || {
                let Some(client) = weak.upgrade() else {
                    return;
                };
                match event {
                    SyncEvent::Processing { items } => {
                        client.set_state(SyncState::Syncing);
                        client.set_items_processed(items as u32);
                    }
                    SyncEvent::Complete { items, errors } => {
                        let now = chrono::Utc::now().timestamp();
                        client.set_last_synced_at(now);
                        if items > 0 || errors > 0 {
                            client.set_items_processed(items as u32);
                            client.set_errors(errors as u32);
                            client.set_state(SyncState::Complete);
                        }
                        // items == 0 && errors == 0: silent update of
                        // last_synced_at only — don't change state so we
                        // don't overwrite a concurrent Complete from the
                        // other sync direction.
                    }
                    SyncEvent::Error { message } => {
                        client.set_error_message(&message);
                        client.set_state(SyncState::Error);
                    }
                    SyncEvent::Offline => {
                        client.set_error_message("Server unreachable");
                        client.set_state(SyncState::Offline);
                    }
                }
            });
        }
        debug!("sync event listener shutting down");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::error::LibraryError;
    use crate::sync::event::is_connectivity_error;

    // ── Construction & defaults ─────────────���────────────────────────

    #[test]
    fn new_client_has_idle_state() {
        let client = SyncClient::new();
        assert_eq!(client.state(), SyncState::Idle);
    }

    #[test]
    fn new_client_has_zero_items_processed() {
        let client = SyncClient::new();
        assert_eq!(client.items_processed(), 0);
    }

    #[test]
    fn new_client_has_zero_last_synced_at() {
        let client = SyncClient::new();
        assert_eq!(client.last_synced_at(), 0);
    }

    #[test]
    fn new_client_has_zero_errors() {
        let client = SyncClient::new();
        assert_eq!(client.errors(), 0);
    }

    #[test]
    fn new_client_has_empty_error_message() {
        let client = SyncClient::new();
        assert!(client.error_message().is_empty());
    }

    // ── Property setters ────────────────────────────────────────────

    #[test]
    fn set_state_updates_value() {
        let client = SyncClient::new();
        client.set_state(SyncState::Syncing);
        assert_eq!(client.state(), SyncState::Syncing);

        client.set_state(SyncState::Error);
        assert_eq!(client.state(), SyncState::Error);
    }

    #[test]
    fn set_items_processed_updates_value() {
        let client = SyncClient::new();
        client.set_items_processed(42);
        assert_eq!(client.items_processed(), 42);
    }

    #[test]
    fn set_last_synced_at_updates_value() {
        let client = SyncClient::new();
        client.set_last_synced_at(1_700_000_000);
        assert_eq!(client.last_synced_at(), 1_700_000_000);
    }

    #[test]
    fn set_error_message_updates_value() {
        let client = SyncClient::new();
        client.set_error_message("auth failed");
        assert_eq!(client.error_message(), "auth failed");
    }

    #[test]
    fn set_state_no_notify_on_same_value() {
        let client = SyncClient::new();
        let notified = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = notified.clone();
        client.connect_notify_local(Some("state"), move |_, _| {
            flag.set(true);
        });

        // Same as default — should not notify.
        client.set_state(SyncState::Idle);
        assert!(!notified.get());

        // Different — should notify.
        client.set_state(SyncState::Syncing);
        assert!(notified.get());
    }

    #[test]
    fn set_error_message_no_notify_on_same_value() {
        let client = SyncClient::new();
        client.set_error_message("err");

        let notified = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = notified.clone();
        client.connect_notify_local(Some("error-message"), move |_, _| {
            flag.set(true);
        });

        // Same value — should not notify.
        client.set_error_message("err");
        assert!(!notified.get());

        // Different — should notify.
        client.set_error_message("new err");
        assert!(notified.get());
    }

    // ── GObject property access ─────────��───────────────────────────

    #[test]
    fn gobject_property_reads_match_accessors() {
        let client = SyncClient::new();
        client.set_state(SyncState::Offline);
        client.set_items_processed(10);
        client.set_errors(3);
        client.set_last_synced_at(999);
        client.set_error_message("test");

        let state: SyncState = client.property("state");
        assert_eq!(state, SyncState::Offline);

        let items: u32 = client.property("items-processed");
        assert_eq!(items, 10);

        let errors: u32 = client.property("errors");
        assert_eq!(errors, 3);

        let synced_at: i64 = client.property("last-synced-at");
        assert_eq!(synced_at, 999);

        let msg: String = client.property("error-message");
        assert_eq!(msg, "test");
    }

    // ── is_connectivity_error ─────────────────────────────────────

    #[test]
    fn connectivity_error_detects_connectivity_variant() {
        let err = LibraryError::Connectivity("POST /sync/stream failed: connection refused".into());
        assert!(is_connectivity_error(&err));
    }

    #[test]
    fn connectivity_error_false_for_immich_variant() {
        let err = LibraryError::Immich("POST /sync/stream returned 401: Unauthorized".into());
        assert!(!is_connectivity_error(&err));
    }

    #[test]
    fn connectivity_error_false_for_other_variants() {
        let err = LibraryError::Runtime("task panicked".into());
        assert!(!is_connectivity_error(&err));
    }
}
