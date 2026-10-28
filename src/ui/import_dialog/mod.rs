// SPDX-FileCopyrightText: 2026 Justin F
// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::time::Duration;

use adw::{prelude::*, subclass::prelude::*};
use gettextrs::{gettext, ngettext};
use gtk::glib;

use crate::client::{ImportClient, ImportState};

/// How often the progress bar pulses while the folder is being scanned.
const PULSE_INTERVAL: Duration = Duration::from_millis(100);

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/import_dialog/import_dialog.ui")]
    pub struct ImportDialog {
        #[template_child]
        pub phase_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub progress_bar: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub counts_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub action_button: TemplateChild<gtk::Button>,
        /// True once the final summary is shown; the button then closes.
        pub complete: Cell<bool>,
        /// Pulses the progress bar while scanning.
        pub pulse_source: RefCell<Option<glib::SourceId>>,
        /// Signal handler IDs for ImportClient property notifications.
        pub import_handlers: RefCell<Vec<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ImportDialog {
        const NAME: &'static str = "MomentsImportDialog";
        type Type = super::ImportDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ImportDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            // While an import is in progress the dialog can't simply close:
            // Escape / the close button emit `close-attempt`, which cancels
            // instead. The dialog stays up until the final summary arrives.
            obj.set_can_close(false);

            // "Cancel" while in progress, "Done" once complete.
            self.action_button.connect_clicked(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    if obj.imp().complete.get() {
                        obj.close();
                    } else {
                        obj.request_cancel();
                    }
                }
            ));
        }
    }

    impl WidgetImpl for ImportDialog {
        fn realize(&self) {
            self.parent_realize();

            let Some(import_client) =
                crate::application::MomentsApplication::default().import_client()
            else {
                // Nothing to report on; don't trap the user.
                self.obj().set_can_close(true);
                return;
            };
            let obj = self.obj().clone();
            let mut handlers = self.import_handlers.borrow_mut();

            for property in ["state", "scanned", "current"] {
                let weak = obj.downgrade();
                handlers.push(import_client.connect_notify_local(
                    Some(property),
                    move |client, _| {
                        if let Some(dialog) = weak.upgrade() {
                            dialog.sync_from_client(client);
                        }
                    },
                ));
            }
            drop(handlers);

            // The dialog may realize after the import started.
            obj.sync_from_client(&import_client);
        }

        fn unrealize(&self) {
            self.obj().stop_pulsing();
            if let Some(import_client) =
                crate::application::MomentsApplication::default().import_client()
            {
                for handler_id in self.import_handlers.borrow_mut().drain(..) {
                    import_client.disconnect(handler_id);
                }
                // Torn down mid-import (e.g. the main window closed):
                // don't leave the import running unseen.
                if !self.complete.get() {
                    import_client.cancel();
                }
            }
            self.parent_unrealize();
        }
    }

    impl AdwDialogImpl for ImportDialog {
        fn close_attempt(&self) {
            if !self.complete.get() {
                self.obj().request_cancel();
            }
        }
    }
}

glib::wrapper! {
    pub struct ImportDialog(ObjectSubclass<imp::ImportDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ImportDialog {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Ask the import client to cancel; the final summary arrives later.
    fn request_cancel(&self) {
        let Some(import_client) = crate::application::MomentsApplication::default().import_client()
        else {
            return;
        };
        if !import_client.is_busy() {
            return;
        }
        import_client.cancel();
        let imp = self.imp();
        imp.phase_label.set_label(&gettext("Cancelling…"));
        imp.action_button.set_sensitive(false);
    }

    /// Bring the dialog in line with the client's current state.
    fn sync_from_client(&self, client: &ImportClient) {
        if self.imp().complete.get() {
            return;
        }
        match client.state() {
            ImportState::Idle => {}
            ImportState::Scanning => self.show_scanning(client.scanned()),
            ImportState::Running => self.show_progress(client.current(), client.total()),
            ImportState::Complete => self.show_complete(client),
        }
    }

    fn show_scanning(&self, found: u32) {
        let imp = self.imp();
        if !imp.action_button.is_sensitive() {
            return; // Cancelling; keep that label.
        }
        imp.phase_label.set_label(&gettext("Scanning…"));
        imp.counts_label.set_label(
            &ngettext("{} file found", "{} files found", found).replace("{}", &found.to_string()),
        );
        self.start_pulsing();
    }

    fn show_progress(&self, current: u32, total: u32) {
        let imp = self.imp();
        self.stop_pulsing();
        if !imp.action_button.is_sensitive() {
            return; // Cancelling; the in-flight file is finishing.
        }
        imp.phase_label.set_label(&gettext("Importing…"));
        if total > 0 {
            imp.progress_bar
                .set_fraction(f64::from(current) / f64::from(total));
            // Translators: import progress, e.g. "3 of 120".
            imp.counts_label.set_label(
                &gettext("{current} of {total}")
                    .replace("{current}", &current.to_string())
                    .replace("{total}", &total.to_string()),
            );
        }
    }

    fn show_complete(&self, client: &ImportClient) {
        let imp = self.imp();
        self.stop_pulsing();
        imp.complete.set(true);
        self.set_can_close(true);

        let cancelled = client.cancelled();
        if !cancelled {
            imp.progress_bar.set_fraction(1.0);
        }
        imp.phase_label.set_label(&if cancelled {
            gettext("Import Cancelled")
        } else {
            gettext("Import Complete")
        });
        imp.counts_label.set_label(&summary_text(
            client.imported(),
            client.skipped(),
            client.failed(),
            client.unreadable(),
            cancelled,
        ));
        imp.action_button.set_label(&gettext("Done"));
        imp.action_button.set_sensitive(true);
    }

    fn start_pulsing(&self) {
        let imp = self.imp();
        if imp.pulse_source.borrow().is_some() {
            return;
        }
        let bar = imp.progress_bar.downgrade();
        let id = glib::timeout_add_local(PULSE_INTERVAL, move || match bar.upgrade() {
            Some(bar) => {
                bar.pulse();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        imp.pulse_source.replace(Some(id));
    }

    fn stop_pulsing(&self) {
        if let Some(id) = self.imp().pulse_source.take() {
            id.remove();
        }
    }
}

impl Default for ImportDialog {
    fn default() -> Self {
        Self::new()
    }
}

/// Completion text: what was imported, then any skips, failures and
/// unreadable entries (each only when non-zero).
fn summary_text(
    imported: u32,
    skipped: u32,
    failed: u32,
    unreadable: u32,
    cancelled: bool,
) -> String {
    let count = |n: u32, text: String| text.replace("{}", &n.to_string());

    let mut parts = vec![match (imported, cancelled) {
        (0, true) => gettext("No photos were imported."),
        (0, false) => gettext("No new photos found."),
        (n, _) => count(n, ngettext("{} photo imported.", "{} photos imported.", n)),
    }];
    if skipped > 0 {
        parts.push(count(
            skipped,
            ngettext("{} file skipped.", "{} files skipped.", skipped),
        ));
    }
    if failed > 0 {
        parts.push(count(
            failed,
            ngettext("{} file failed.", "{} files failed.", failed),
        ));
    }
    if unreadable > 0 {
        parts.push(count(
            unreadable,
            ngettext(
                "{} folder or file could not be read.",
                "{} folders or files could not be read.",
                unreadable,
            ),
        ));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::summary_text;

    #[test]
    fn summary_omits_zero_counts() {
        let text = summary_text(3, 0, 0, 0, false);
        assert!(text.contains('3'));
        assert!(!text.contains("skipped"));
        assert!(!text.contains("read"));
    }

    #[test]
    fn summary_reports_unreadable_when_nonzero() {
        let text = summary_text(1, 0, 0, 2, false);
        assert!(
            text.contains("2 folders or files could not be read"),
            "{text}"
        );
    }

    #[test]
    fn cancelled_with_nothing_imported_says_so() {
        let text = summary_text(0, 0, 0, 0, true);
        assert!(text.contains("No photos were imported"), "{text}");
    }
}
