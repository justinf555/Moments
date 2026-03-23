use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;
use std::sync::OnceLock;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/backend_picker_page.ui")]
    pub struct MomentsBackendPickerPage {
        #[template_child]
        pub local_row: TemplateChild<adw::ActionRow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MomentsBackendPickerPage {
        const NAME: &'static str = "MomentsBackendPickerPage";
        type Type = super::MomentsBackendPickerPage;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MomentsBackendPickerPage {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![glib::subclass::Signal::builder("local-selected").build()]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.local_row.connect_activated(glib::clone!(
                #[weak]
                obj,
                move |_| {
                    obj.emit_by_name::<()>("local-selected", &[]);
                }
            ));
        }
    }

    impl WidgetImpl for MomentsBackendPickerPage {}
    impl NavigationPageImpl for MomentsBackendPickerPage {}
}

glib::wrapper! {
    pub struct MomentsBackendPickerPage(ObjectSubclass<imp::MomentsBackendPickerPage>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl MomentsBackendPickerPage {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn connect_local_selected<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "local-selected",
            false,
            glib::closure_local!(move |obj: &Self| {
                f(obj);
            }),
        )
    }
}

impl Default for MomentsBackendPickerPage {
    fn default() -> Self {
        Self::new()
    }
}
