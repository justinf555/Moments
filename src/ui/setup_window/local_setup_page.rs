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

            self.referenced_row.set_visible(true);

            // Managed row: create bundle immediately in the app data dir.
            self.managed_row.connect_activated(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    obj.create_managed_library();
                }
            ));

            // Referenced row: open folder picker, then create bundle.
            self.referenced_row.connect_activated(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    obj.open_folder_dialog();
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
        let bundle_path = default_local_library_path();

        if !bundle_path.exists() {
            let config = LibraryConfig::Local {
                mode: LocalStorageMode::Managed,
            };
            if let Err(e) = Bundle::create(&bundle_path, &config) {
                error!("failed to create managed library bundle: {e}");
                return;
            }
            debug!(path = %bundle_path.display(), "managed library bundle created");
        } else {
            debug!(path = %bundle_path.display(), "re-using existing managed library bundle");
        }

        let path_str = bundle_path.to_string_lossy().to_string();
        self.emit_by_name::<()>("create-requested", &[&path_str]);
    }

    /// Referenced mode: open a folder picker, then create the bundle.
    #[instrument(skip(self))]
    fn open_folder_dialog(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Choose Photos Folder")
            .modal(true)
            .build();

        glib::MainContext::default().spawn_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let window = page.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                let result = dialog.select_folder_future(window.as_ref()).await;

                if let Ok(_file) = result {
                    let bundle_path = default_local_library_path();

                    if !bundle_path.exists() {
                        let config = LibraryConfig::Local {
                            mode: LocalStorageMode::Referenced,
                        };
                        if let Err(e) = Bundle::create(&bundle_path, &config) {
                            error!("failed to create referenced library bundle: {e}");
                            return;
                        }
                        debug!(path = %bundle_path.display(), "referenced library bundle created");
                    } else {
                        debug!(path = %bundle_path.display(), "re-using existing library bundle");
                    }

                    let path_str = bundle_path.to_string_lossy().to_string();
                    page.emit_by_name::<()>("create-requested", &[&path_str]);
                }
            }
        ));
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
