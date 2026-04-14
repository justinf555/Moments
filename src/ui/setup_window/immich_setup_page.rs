use std::cell::RefCell;
use std::sync::OnceLock;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use tracing::{debug, error, instrument};

use crate::library::bundle::Bundle;
use crate::library::config::LibraryConfig;
use crate::library::keyring;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/setup_window/immich_setup_page.ui")]
    pub struct MomentsImmichSetupPage {
        #[template_child]
        pub server_url_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub email_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub password_row: TemplateChild<adw::PasswordEntryRow>,
        #[template_child]
        pub test_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub status_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub connect_btn: TemplateChild<gtk::Button>,

        /// Stored after a successful test connection, used by Connect.
        pub access_token: RefCell<Option<String>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MomentsImmichSetupPage {
        const NAME: &'static str = "MomentsImmichSetupPage";
        type Type = super::MomentsImmichSetupPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MomentsImmichSetupPage {
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

            self.test_btn.connect_clicked(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    obj.test_connection();
                }
            ));

            self.connect_btn.connect_clicked(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    obj.on_connect();
                }
            ));
        }
    }

    impl WidgetImpl for MomentsImmichSetupPage {}
    impl NavigationPageImpl for MomentsImmichSetupPage {}
}

glib::wrapper! {
    pub struct MomentsImmichSetupPage(ObjectSubclass<imp::MomentsImmichSetupPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MomentsImmichSetupPage {
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

    /// Test the connection by logging in to the Immich server.
    ///
    /// TODO: Immich login will be restored when `src/sync/immich/` is wired
    /// up (phase 4 of the library refactor). For now this shows an error.
    #[instrument(skip(self))]
    fn test_connection(&self) {
        let imp = self.imp();
        imp.status_label
            .set_text("Immich login is not yet available.");
        imp.status_label.remove_css_class("success");
        imp.status_label.add_css_class("error");
    }

    /// Called when the user clicks "Connect" after a successful test.
    #[instrument(skip(self))]
    fn on_connect(&self) {
        let imp = self.imp();
        let server_url = imp.server_url_row.text().to_string();
        let access_token = imp.access_token.borrow().clone().unwrap_or_default();

        if access_token.is_empty() {
            imp.status_label
                .set_text("Please test the connection first.");
            imp.status_label.add_css_class("error");
            return;
        }

        // Store session token in GNOME Keyring.
        if let Err(e) = keyring::store_access_token(&server_url, &access_token) {
            error!("failed to store access token: {e}");
            imp.status_label
                .set_text(&format!("Failed to store credentials: {e}"));
            imp.status_label.add_css_class("error");
            return;
        }

        // Create the Immich bundle.
        let bundle_path = default_immich_library_path();
        let config = LibraryConfig::Immich {
            server_url,
            access_token,
        };
        if let Err(e) = Bundle::create(&bundle_path, &config) {
            error!("failed to create Immich bundle: {e}");
            imp.status_label
                .set_text(&format!("Failed to create library: {e}"));
            imp.status_label.add_css_class("error");
            return;
        }

        let path_str = bundle_path.to_string_lossy().to_string();
        debug!(path = %path_str, "emitting create-requested for Immich bundle");
        self.emit_by_name::<()>("create-requested", &[&path_str]);
    }
}

impl Default for MomentsImmichSetupPage {
    fn default() -> Self {
        Self::new()
    }
}

fn default_immich_library_path() -> std::path::PathBuf {
    glib::user_data_dir().join("moments").join("immich.library")
}
