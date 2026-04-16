//! Compact activity indicator for the sidebar header bar.
//!
//! Shows a spinner when sync or import is active, a status icon for
//! completion/error/offline, and a popover with detail rows on click.
//! Driven entirely by `SyncClient` and `ImportClient` GObject properties.

use std::cell::Cell;
use std::cell::RefCell;

use gettextrs::gettext;
use gtk::{glib, prelude::*, subclass::prelude::*};
use tracing::debug;

use crate::client::{ImportClient, ImportState, SyncClient, SyncState};

/// Duration (ms) to show the tick icon after a successful sync/import.
const TICK_DURATION_MS: u64 = 5_000;

mod imp {
    use super::*;
    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/widgets/activity_indicator/activity_indicator.ui")]
    pub struct ActivityIndicator {
        #[template_child]
        pub(super) menu_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(super) stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(super) spinner: TemplateChild<gtk::Spinner>,
        #[template_child]
        pub(super) status_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) popover: TemplateChild<gtk::Popover>,
        #[template_child]
        pub(super) popover_box: TemplateChild<gtk::Box>,

        // Popover rows
        #[template_child]
        pub(super) sync_row: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) sync_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) sync_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) import_row: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) import_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) import_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) import_progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub(super) error_row: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) error_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) error_label: TemplateChild<gtk::Label>,

        // State
        pub(super) tick_timer: RefCell<Option<glib::SourceId>>,
        pub(super) app_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub(super) sync_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub(super) import_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        /// Whether we have an active error/offline state that persists.
        pub(super) has_persistent_error: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ActivityIndicator {
        const NAME: &'static str = "MomentsActivityIndicator";
        type Type = super::ActivityIndicator;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ActivityIndicator {
        fn dispose(&self) {
            if let Some(id) = self.tick_timer.borrow_mut().take() {
                id.remove();
            }
            self.menu_button.unparent();
        }
    }

    impl WidgetImpl for ActivityIndicator {
        fn realize(&self) {
            self.parent_realize();
            self.obj().bind_clients();
        }
    }
}

glib::wrapper! {
    /// Compact activity indicator widget for the sidebar header bar.
    ///
    /// Shows a spinner during sync/import activity, a tick icon on
    /// completion, and error/offline icons for persistent issues.
    /// A popover provides detail when clicked.
    pub struct ActivityIndicator(ObjectSubclass<imp::ActivityIndicator>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ActivityIndicator {
    /// Bind to SyncClient and ImportClient singletons on the application.
    ///
    /// Called from `realize()`. Binds eagerly if the clients already exist,
    /// and subscribes to `notify::sync-client` / `notify::import-client`
    /// on the application so late-arriving clients are picked up too.
    fn bind_clients(&self) {
        let app = crate::application::MomentsApplication::default();

        // Bind any clients that already exist.
        if let Some(sc) = app.sync_client() {
            self.bind_sync_client(&sc);
        }
        if let Some(ic) = app.import_client() {
            self.bind_import_client(&ic);
        }

        // Subscribe for late-arriving clients.
        let mut app_handlers = self.imp().app_handlers.borrow_mut();

        let weak = self.downgrade();
        app_handlers.push(app.connect_notify_local(
            Some("sync-client"),
            move |app, _| {
                let Some(this) = weak.upgrade() else { return };
                if let Some(sc) = app.sync_client() {
                    this.bind_sync_client(&sc);
                }
            },
        ));

        let weak = self.downgrade();
        app_handlers.push(app.connect_notify_local(
            Some("import-client"),
            move |app, _| {
                let Some(this) = weak.upgrade() else { return };
                if let Some(ic) = app.import_client() {
                    this.bind_import_client(&ic);
                }
            },
        ));
    }

    fn bind_sync_client(&self, client: &SyncClient) {
        let imp = self.imp();
        let mut handlers = imp.sync_handlers.borrow_mut();

        // Guard against double-bind.
        if !handlers.is_empty() {
            return;
        }

        // React to state changes.
        let weak = self.downgrade();
        handlers.push(client.connect_notify_local(Some("state"), move |client, _| {
            let Some(this) = weak.upgrade() else { return };
            this.on_sync_state_changed(client);
        }));

        // React to items-processed changes (update label while syncing).
        let weak = self.downgrade();
        handlers.push(client.connect_notify_local(
            Some("items-processed"),
            move |client, _| {
                let Some(this) = weak.upgrade() else { return };
                if client.state() == SyncState::Syncing {
                    let items = client.items_processed();
                    let text = if items > 0 {
                        format!(
                            "{} {}",
                            gettext("Syncing\u{2026}"),
                            ngettext_items(items)
                        )
                    } else {
                        gettext("Syncing\u{2026}")
                    };
                    this.imp().sync_label.set_text(&text);
                }
            },
        ));

        // React to last-synced-at changes (update idle popover text).
        let weak = self.downgrade();
        handlers.push(client.connect_notify_local(
            Some("last-synced-at"),
            move |client, _| {
                let Some(this) = weak.upgrade() else { return };
                if client.state() == SyncState::Idle
                    || client.state() == SyncState::Complete
                {
                    this.update_idle_sync_label(client.last_synced_at());
                }
            },
        ));

        // Set initial state.
        self.on_sync_state_changed(client);
    }

    fn bind_import_client(&self, client: &ImportClient) {
        let imp = self.imp();
        let mut handlers = imp.import_handlers.borrow_mut();

        // Guard against double-bind.
        if !handlers.is_empty() {
            return;
        }

        // React to state changes.
        let weak = self.downgrade();
        handlers.push(client.connect_notify_local(Some("state"), move |client, _| {
            let Some(this) = weak.upgrade() else { return };
            this.on_import_state_changed(client);
        }));

        // React to progress (current) changes.
        let weak = self.downgrade();
        handlers.push(client.connect_notify_local(Some("current"), move |client, _| {
            let Some(this) = weak.upgrade() else { return };
            if client.state() == ImportState::Running {
                this.update_import_progress(client);
            }
        }));
    }

    // ── Sync state transitions ──────────────────────────────────────

    fn on_sync_state_changed(&self, client: &SyncClient) {
        let imp = self.imp();
        let state = client.state();
        debug!(?state, "activity indicator: sync state changed");

        match state {
            SyncState::Idle => {
                imp.has_persistent_error.set(false);
                imp.sync_row.set_visible(false);
                imp.error_row.set_visible(false);
                self.update_visibility();
            }
            SyncState::Syncing => {
                imp.has_persistent_error.set(false);
                imp.error_row.set_visible(false);
                let items = client.items_processed();
                let text = if items > 0 {
                    format!("{} {}", gettext("Syncing\u{2026}"), ngettext_items(items))
                } else {
                    gettext("Syncing\u{2026}")
                };
                imp.sync_label.set_text(&text);
                imp.sync_icon
                    .set_icon_name(Some("emblem-synchronizing-symbolic"));
                imp.sync_row.set_visible(true);
                self.show_spinner();
            }
            SyncState::Complete => {
                let items = client.items_processed();
                let text = if items > 0 {
                    format!("{} {}", gettext("Synced"), ngettext_items(items))
                } else {
                    synced_ago_text(client.last_synced_at())
                };
                imp.sync_label.set_text(&text);
                imp.sync_icon
                    .set_icon_name(Some("object-select-symbolic"));
                imp.sync_row.set_visible(true);
                if items > 0 {
                    self.show_tick();
                } else {
                    self.update_visibility();
                }
            }
            SyncState::Error => {
                imp.has_persistent_error.set(true);
                imp.error_label.set_text(&client.error_message());
                imp.error_icon
                    .set_icon_name(Some("dialog-warning-symbolic"));
                imp.error_row.set_visible(true);
                imp.sync_row.set_visible(false);
                self.show_error_icon("dialog-warning-symbolic");
            }
            SyncState::Offline => {
                imp.has_persistent_error.set(true);
                let mut text = gettext("Server unreachable");
                let last = client.last_synced_at();
                if last > 0 {
                    text.push_str(&format!("\n{}", synced_ago_text(last)));
                }
                imp.error_label.set_text(&text);
                imp.error_icon
                    .set_icon_name(Some("network-offline-symbolic"));
                imp.error_row.set_visible(true);
                imp.sync_row.set_visible(false);
                self.show_error_icon("network-offline-symbolic");
            }
        }
    }

    // ── Import state transitions ────────────────────────────────────

    fn on_import_state_changed(&self, client: &ImportClient) {
        let imp = self.imp();
        let state = client.state();
        debug!(?state, "activity indicator: import state changed");

        match state {
            ImportState::Idle => {
                imp.import_row.set_visible(false);
                imp.import_progress.set_visible(false);
                self.update_visibility();
            }
            ImportState::Running => {
                self.update_import_progress(client);
                imp.import_progress.set_visible(true);
                imp.import_row.set_visible(true);
                self.show_spinner();
            }
            ImportState::Complete => {
                let imported = client.imported();
                let skipped = client.skipped();
                let failed = client.failed();
                let mut text = format!("{} {}", imported, gettext("imported"));
                if skipped > 0 {
                    text.push_str(&format!(", {} {}", skipped, gettext("skipped")));
                }
                if failed > 0 {
                    text.push_str(&format!(", {} {}", failed, gettext("failed")));
                }
                imp.import_label.set_text(&text);
                imp.import_icon
                    .set_icon_name(Some("object-select-symbolic"));
                imp.import_progress.set_visible(false);
                imp.import_row.set_visible(true);
                self.show_tick();
            }
        }
    }

    fn update_import_progress(&self, client: &ImportClient) {
        let imp = self.imp();
        let current = client.current();
        let total = client.total();
        imp.import_label.set_text(&format!(
            "{} {}/{}",
            gettext("Importing"),
            current,
            total
        ));
        imp.import_icon
            .set_icon_name(Some("document-open-symbolic"));
        if total > 0 {
            imp.import_progress
                .set_fraction(current as f64 / total as f64);
        }
    }

    // ── Visual state helpers ────────────────────────────────────────

    fn show_spinner(&self) {
        let imp = self.imp();
        if let Some(id) = imp.tick_timer.borrow_mut().take() {
            id.remove();
        }
        imp.spinner.set_spinning(true);
        imp.stack.set_visible_child_name("active");
        imp.menu_button.set_visible(true);
    }

    fn show_tick(&self) {
        let imp = self.imp();
        // Don't show tick if something is still actively running.
        if self.is_any_active() {
            return;
        }

        imp.status_icon
            .set_icon_name(Some("object-select-symbolic"));
        imp.stack.set_visible_child_name("status");
        imp.spinner.set_spinning(false);
        imp.menu_button.set_visible(true);

        // Start 5s timer to hide.
        if let Some(id) = imp.tick_timer.borrow_mut().take() {
            id.remove();
        }
        let weak = self.downgrade();
        let id = glib::timeout_add_local_once(
            std::time::Duration::from_millis(TICK_DURATION_MS),
            move || {
                let Some(this) = weak.upgrade() else { return };
                debug!("activity indicator: tick timer expired");
                let imp = this.imp();
                // Clear the source ID — the one-shot timer has already fired.
                imp.tick_timer.borrow_mut().take();
                if imp.has_persistent_error.get() {
                    return;
                }
                if !this.is_any_active() {
                    this.hide();
                }
            },
        );
        *imp.tick_timer.borrow_mut() = Some(id);
    }

    fn show_error_icon(&self, icon_name: &str) {
        let imp = self.imp();
        // Don't override spinner if something is actively running.
        if self.is_any_active() {
            imp.menu_button.set_visible(true);
            return;
        }
        if let Some(id) = imp.tick_timer.borrow_mut().take() {
            id.remove();
        }
        imp.status_icon.set_icon_name(Some(icon_name));
        imp.stack.set_visible_child_name("status");
        imp.spinner.set_spinning(false);
        imp.menu_button.set_visible(true);
    }

    fn hide(&self) {
        let imp = self.imp();
        imp.stack.set_visible_child_name("hidden");
        imp.spinner.set_spinning(false);
        imp.menu_button.set_visible(false);
    }

    /// Update button visibility based on whether any rows are shown.
    fn update_visibility(&self) {
        let imp = self.imp();
        if imp.has_persistent_error.get() {
            return;
        }
        let any_visible = imp.sync_row.is_visible()
            || imp.import_row.is_visible()
            || imp.error_row.is_visible();
        if !any_visible && !self.is_any_active() {
            self.hide();
        }
    }

    /// Whether any operation is currently running (spinner should show).
    fn is_any_active(&self) -> bool {
        let app = crate::application::MomentsApplication::default();
        let sync_active = app
            .sync_client()
            .is_some_and(|c| c.state() == SyncState::Syncing);
        let import_active = app
            .import_client()
            .is_some_and(|c| c.state() == ImportState::Running);
        sync_active || import_active
    }

    /// Update the sync row label with a "Synced X ago" message.
    fn update_idle_sync_label(&self, last_synced_at: i64) {
        let imp = self.imp();
        if last_synced_at > 0 {
            imp.sync_label.set_text(&synced_ago_text(last_synced_at));
        } else {
            imp.sync_label.set_text(&gettext("Not yet synced"));
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn ngettext_items(count: u32) -> String {
    gettextrs::ngettext("{} item", "{} items", count).replace("{}", &count.to_string())
}

fn synced_ago_text(last_synced_at: i64) -> String {
    if last_synced_at == 0 {
        return gettext("Not yet synced");
    }
    let elapsed = chrono::Utc::now().timestamp() - last_synced_at;
    if elapsed < 10 {
        gettext("Synced just now")
    } else if elapsed < 60 {
        gettext("Synced moments ago")
    } else if elapsed < 3600 {
        let mins = elapsed / 60;
        gettextrs::ngettext("Synced {} minute ago", "Synced {} minutes ago", mins as u32)
            .replace("{}", &mins.to_string())
    } else {
        let hours = elapsed / 3600;
        gettextrs::ngettext("Synced {} hour ago", "Synced {} hours ago", hours as u32)
            .replace("{}", &hours.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ngettext_items_singular() {
        let text = ngettext_items(1);
        assert!(text.contains('1'));
    }

    #[test]
    fn ngettext_items_plural() {
        let text = ngettext_items(42);
        assert!(text.contains("42"));
    }

    #[test]
    fn synced_ago_zero_returns_not_yet() {
        let text = synced_ago_text(0);
        assert!(!text.is_empty());
    }

    #[test]
    fn synced_ago_recent_returns_just_now() {
        let now = chrono::Utc::now().timestamp();
        let text = synced_ago_text(now);
        assert!(!text.is_empty());
    }

    #[test]
    fn synced_ago_minutes() {
        let five_min_ago = chrono::Utc::now().timestamp() - 300;
        let text = synced_ago_text(five_min_ago);
        assert!(text.contains('5'));
    }

    #[test]
    fn synced_ago_hours() {
        let two_hours_ago = chrono::Utc::now().timestamp() - 7200;
        let text = synced_ago_text(two_hours_ago);
        assert!(text.contains('2'));
    }
}
