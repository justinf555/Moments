use std::sync::OnceLock;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use tracing::{debug, error, instrument};

use crate::library::bundle::Bundle;
use crate::library::config::LibraryConfig;
use crate::library::immich_client::ImmichClient;
use crate::library::keyring;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/setup_window/immich_setup_page.ui")]
    pub struct MomentsImmichSetupPage {
        #[template_child]
        pub server_url_row: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub api_key_row: TemplateChild<adw::PasswordEntryRow>,
        #[template_child]
        pub test_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub status_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub connect_btn: TemplateChild<gtk::Button>,
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

            // Test Connection button
            self.test_btn.connect_clicked(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    obj.test_connection();
                }
            ));

            // Connect button
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

    /// Test the connection to the Immich server.
    #[instrument(skip(self))]
    fn test_connection(&self) {
        let imp = self.imp();
        let server_url = imp.server_url_row.text().to_string();
        let api_key = imp.api_key_row.text().to_string();

        if server_url.is_empty() || api_key.is_empty() {
            imp.status_label.set_text("Please enter both URL and API key.");
            imp.status_label.remove_css_class("success");
            imp.status_label.add_css_class("error");
            return;
        }

        imp.test_btn.set_sensitive(false);
        imp.status_label.set_text("Connecting...");
        imp.status_label.remove_css_class("error");
        imp.status_label.remove_css_class("success");
        imp.connect_btn.set_sensitive(false);

        let client = match ImmichClient::new(&server_url, &api_key) {
            Ok(c) => c,
            Err(e) => {
                imp.status_label.set_text(&format!("Error: {e}"));
                imp.status_label.add_css_class("error");
                imp.test_btn.set_sensitive(true);
                return;
            }
        };

        let obj_weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let result = client.validate().await;
            let Some(obj) = obj_weak.upgrade() else { return };
            let imp = obj.imp();
            imp.test_btn.set_sensitive(true);

            match result {
                Ok(about) => {
                    debug!(version = %about.version, "connection successful");
                    imp.status_label.set_text(&format!("Connected — {about}"));
                    imp.status_label.remove_css_class("error");
                    imp.status_label.add_css_class("success");
                    imp.connect_btn.set_sensitive(true);
                }
                Err(e) => {
                    error!("connection test failed: {e}");
                    imp.status_label.set_text(&format!("Failed: {e}"));
                    imp.status_label.remove_css_class("success");
                    imp.status_label.add_css_class("error");
                    imp.connect_btn.set_sensitive(false);
                }
            }
        });
    }

    /// Called when the user clicks "Connect" after a successful test.
    #[instrument(skip(self))]
    fn on_connect(&self) {
        let imp = self.imp();
        let server_url = imp.server_url_row.text().to_string();
        let api_key = imp.api_key_row.text().to_string();

        // Store API key in GNOME Keyring.
        if let Err(e) = keyring::store_api_key(&server_url, &api_key) {
            error!("failed to store API key: {e}");
            imp.status_label.set_text(&format!("Failed to store credentials: {e}"));
            imp.status_label.add_css_class("error");
            return;
        }

        // Create the Immich bundle.
        let bundle_path = default_immich_library_path();
        let config = LibraryConfig::Immich {
            server_url,
            api_key,
        };
        if let Err(e) = Bundle::create(&bundle_path, &config) {
            error!("failed to create Immich bundle: {e}");
            imp.status_label.set_text(&format!("Failed to create library: {e}"));
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

/// Default bundle path for Immich libraries.
fn default_immich_library_path() -> std::path::PathBuf {
    glib::home_dir().join("Pictures").join("Moments-Immich.library")
}
