pub mod backend_picker_page;
pub mod immich_setup_page;
pub mod local_setup_page;

use std::sync::OnceLock;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use tracing::debug;

use backend_picker_page::MomentsBackendPickerPage;
use immich_setup_page::MomentsImmichSetupPage;
use local_setup_page::MomentsLocalSetupPage;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/setup_window.ui")]
    pub struct MomentsSetupWindow {
        #[template_child]
        pub navigation_view: TemplateChild<adw::NavigationView>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MomentsSetupWindow {
        const NAME: &'static str = "MomentsSetupWindow";
        type Type = super::MomentsSetupWindow;
        type ParentType = adw::Window;

        fn class_init(klass: &mut Self::Class) {
            MomentsBackendPickerPage::ensure_type();
            MomentsLocalSetupPage::ensure_type();
            MomentsImmichSetupPage::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MomentsSetupWindow {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![glib::subclass::Signal::builder("setup-complete")
                    .param_types([glib::Type::STRING])
                    .build()]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            let win = self.obj();

            let picker = MomentsBackendPickerPage::new();

            picker.connect_local_selected(glib::clone!(
                #[weak]
                win,
                move |_| {
                    debug!("local backend selected, pushing setup page");
                    let local_page = MomentsLocalSetupPage::new();
                    local_page.connect_create_requested(glib::clone!(
                        #[weak]
                        win,
                        move |_, path| {
                            win.emit_by_name::<()>("setup-complete", &[&path]);
                        }
                    ));
                    win.imp().navigation_view.push(&local_page);
                }
            ));

            picker.connect_immich_selected(glib::clone!(
                #[weak]
                win,
                move |_| {
                    debug!("immich backend selected, pushing setup page");
                    let immich_page = MomentsImmichSetupPage::new();
                    immich_page.connect_create_requested(glib::clone!(
                        #[weak]
                        win,
                        move |_, path| {
                            win.emit_by_name::<()>("setup-complete", &[&path]);
                        }
                    ));
                    win.imp().navigation_view.push(&immich_page);
                }
            ));

            self.navigation_view.push(&picker);
        }
    }

    impl WidgetImpl for MomentsSetupWindow {}
    impl WindowImpl for MomentsSetupWindow {}
    impl AdwWindowImpl for MomentsSetupWindow {}
}

glib::wrapper! {
    pub struct MomentsSetupWindow(ObjectSubclass<imp::MomentsSetupWindow>)
        @extends adw::Window, gtk::Window, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl MomentsSetupWindow {
    pub fn new<P: IsA<gtk::Application>>(application: &P) -> Self {
        glib::Object::builder()
            .property("application", application)
            .build()
    }

    pub fn connect_setup_complete<F: Fn(&Self, String) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "setup-complete",
            false,
            glib::closure_local!(move |obj: &Self, path: String| {
                f(obj, path);
            }),
        )
    }
}
