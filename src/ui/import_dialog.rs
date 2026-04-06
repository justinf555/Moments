use adw::{prelude::*, subclass::prelude::*};
use gtk::glib;
use std::cell::Cell;

use crate::library::import::ImportSummary;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/import_dialog.ui")]
    pub struct ImportDialog {
        #[template_child]
        pub phase_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub progress_bar: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub counts_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub action_button: TemplateChild<gtk::Button>,
        /// True once `ImportComplete` has been received.
        pub complete: Cell<bool>,
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
            // Both "Cancel" (in-progress) and "Done" (complete) close the dialog.
            // Actual cancellation of the import pipeline is a future enhancement.
            self.action_button.connect_clicked(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    obj.close();
                }
            ));
        }
    }

    impl WidgetImpl for ImportDialog {}
    impl AdwDialogImpl for ImportDialog {}
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

    /// Update the dialog with current import progress.
    ///
    /// Called for each `LibraryEvent::ImportProgress` while the dialog is open.
    pub fn set_progress(&self, current: usize, total: usize) {
        let imp = self.imp();
        imp.phase_label.set_label("Importing…");
        if total > 0 {
            imp.progress_bar.set_fraction(current as f64 / total as f64);
            imp.counts_label.set_label(&format!("{current} of {total}"));
        } else {
            imp.progress_bar.pulse();
        }
    }

    /// Transition the dialog to its completed state.
    ///
    /// Called once `LibraryEvent::ImportComplete` arrives. Shows a summary
    /// and changes the action button to "Done".
    pub fn set_complete(&self, summary: &ImportSummary) {
        let imp = self.imp();
        imp.complete.set(true);
        imp.progress_bar.set_fraction(1.0);
        imp.phase_label.set_label("Import Complete");

        let msg = match summary.imported {
            0 => "No new photos found.".to_string(),
            1 => "1 photo imported.".to_string(),
            n => format!("{n} photos imported."),
        };
        let extra = match summary.skipped_duplicates {
            0 => String::new(),
            n => format!(" {n} duplicate{} skipped.", if n == 1 { "" } else { "s" }),
        };
        imp.counts_label.set_label(&format!("{msg}{extra}"));
        imp.action_button.set_label("Done");
    }
}

impl Default for ImportDialog {
    fn default() -> Self {
        Self::new()
    }
}
