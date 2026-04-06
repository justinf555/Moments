use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::OnceLock;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use tracing::{debug, instrument};

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/setup_window/local_setup_page.ui")]
    pub struct MomentsLocalSetupPage {
        #[template_child]
        pub path_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub create_button: TemplateChild<gtk::Button>,

        pub chosen_path: RefCell<Option<PathBuf>>,
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

            // Set default path subtitle
            let default = super::default_library_path();
            self.path_row.set_subtitle(&default.to_string_lossy());
            *self.chosen_path.borrow_mut() = Some(default);
            self.create_button.set_sensitive(true);

            // path_row activation → open folder chooser
            self.path_row.connect_activated(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    obj.open_folder_dialog();
                }
            ));

            // create_button clicked → emit signal
            self.create_button.connect_clicked(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    let path = obj.imp().chosen_path.borrow().clone();
                    if let Some(p) = path {
                        let path_str = p.to_string_lossy().to_string();
                        debug!(path = %path_str, "user confirmed library path");
                        obj.emit_by_name::<()>("create-requested", &[&path_str]);
                    }
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

    #[instrument(skip(self))]
    fn open_folder_dialog(&self) {
        let dialog = gtk::FileDialog::builder()
            .title("Choose Library Location")
            .modal(true)
            .build();

        glib::MainContext::default().spawn_local(glib::clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                let window = page.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                let result = dialog.select_folder_future(window.as_ref()).await;

                if let Ok(file) = result {
                    if let Some(mut path) = file.path() {
                        // Append Moments.library if not already a .library bundle
                        if path.extension().and_then(|e| e.to_str()) != Some("library") {
                            path = path.join("Moments.library");
                        }
                        let path_str = path.to_string_lossy().to_string();
                        debug!(path = %path_str, "folder chosen");
                        page.imp().path_row.set_subtitle(&path_str);
                        page.imp().create_button.set_sensitive(true);
                        *page.imp().chosen_path.borrow_mut() = Some(path);
                    }
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

/// Returns the default bundle path: `~/Pictures/Moments.library`.
fn default_library_path() -> PathBuf {
    glib::home_dir().join("Pictures").join("Moments.library")
}
