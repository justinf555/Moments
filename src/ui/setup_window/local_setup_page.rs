use std::sync::OnceLock;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use tracing::{debug, error, instrument};

use crate::library::bundle::Bundle;
use crate::library::config::{LibraryConfig, LocalStorageMode};

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/setup_window/local_setup_page.ui")]
    pub struct MomentsLocalSetupPage {
        #[template_child]
        pub managed_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub referenced_row: TemplateChild<adw::ActionRow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MomentsLocalSetupPage {
        const NAME: &'static str = "MomentsLocalSetupPage";
        type Type = super::MomentsLocalSetupPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MomentsLocalSetupPage {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![glib::subclass::Signal::builder("create-requested")
                    .param_types([glib::Type::STRING])
                    .build()]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            // Managed row: create bundle immediately in the app data dir.
            self.managed_row.connect_activated(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    obj.create_managed_library();
                }
            ));

            // Referenced row: create bundle immediately (photos are added
            // later via the import dialog, which uses the portal).
            self.referenced_row.connect_activated(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    obj.create_referenced_library();
                }
            ));
        }
    }

    impl WidgetImpl for MomentsLocalSetupPage {}
    impl NavigationPageImpl for MomentsLocalSetupPage {}
}

glib::wrapper! {
    pub struct MomentsLocalSetupPage(ObjectSubclass<imp::MomentsLocalSetupPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MomentsLocalSetupPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn connect_create_requested<F: Fn(&Self, String) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "create-requested",
            false,
            glib::closure_local!(move |obj: &Self, path: String| {
                f(obj, path);
            }),
        )
    }

    /// Managed mode: create (or re-use) the bundle in the app's sandbox data directory.
    #[instrument(skip(self))]
    fn create_managed_library(&self) {
        self.create_library(LocalStorageMode::Managed);
    }

    /// Referenced mode: create the bundle and proceed to the empty library.
    /// Photos are added later via the import dialog (which uses the portal).
    #[instrument(skip(self))]
    fn create_referenced_library(&self) {
        self.create_library(LocalStorageMode::Referenced);
    }

    /// Shared helper: create (or re-use) a local library bundle with the given mode.
    fn create_library(&self, mode: LocalStorageMode) {
        let bundle_path = default_local_library_path();

        if !bundle_path.exists() {
            let config = LibraryConfig::Local { mode };
            if let Err(e) = Bundle::create(&bundle_path, &config) {
                error!("failed to create library bundle: {e}");
                let dialog = adw::AlertDialog::builder()
                    .heading("Could Not Create Library")
                    .body(format!(
                        "Failed to create library at {}.\n\n{e}",
                        bundle_path.display()
                    ))
                    .build();
                dialog.add_response("ok", "OK");
                dialog.present(self.root().and_downcast_ref::<gtk::Window>());
                return;
            }
            debug!(path = %bundle_path.display(), "library bundle created");
        } else {
            debug!(path = %bundle_path.display(), "re-using existing library bundle");
        }

        let path_str = bundle_path.to_string_lossy().to_string();
        self.emit_by_name::<()>("create-requested", &[&path_str]);
    }
}

impl Default for MomentsLocalSetupPage {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns the default bundle path for local libraries.
///
/// Uses the app's XDG data directory so the bundle is inside the Flatpak
/// sandbox: `~/.local/share/moments/local.library` (or the sandbox equivalent).
fn default_local_library_path() -> std::path::PathBuf {
    glib::user_data_dir().join("moments").join("local.library")
}
