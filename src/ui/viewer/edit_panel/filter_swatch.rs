use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::filters::Filter;

use super::EditSession;

/// Callback type for when a swatch is selected.
pub type OnFilterSelected = Rc<dyn Fn(&Rc<dyn Filter>)>;

mod imp {
    use super::*;
    use std::cell::OnceCell;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/viewer/edit_panel/filter_swatch.ui")]
    pub struct EditFilterSwatch {
        #[template_child]
        pub toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub colour_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub name_label: TemplateChild<gtk::Label>,

        /// The filter trait object.
        pub filter: OnceCell<Rc<dyn Filter>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EditFilterSwatch {
        const NAME: &'static str = "MomentsEditFilterSwatch";
        type Type = super::EditFilterSwatch;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for EditFilterSwatch {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for EditFilterSwatch {}
}

glib::wrapper! {
    pub struct EditFilterSwatch(ObjectSubclass<imp::EditFilterSwatch>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl EditFilterSwatch {
    /// Create a swatch for the given filter trait object.
    pub fn new(filter: Rc<dyn Filter>) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.name_label.set_label(filter.display_name());
        imp.colour_box
            .add_css_class(&format!("filter-{}", filter.name()));
        let _ = imp.filter.set(filter);
        obj
    }

    /// Wire the click handler. When clicked, applies the filter preset to
    /// the session and calls `changed`.
    ///
    /// `on_selected` is called with this swatch's filter so the section can
    /// deactivate other swatches and update its subtitle/active_filter.
    pub fn setup(
        &self,
        session: Rc<RefCell<Option<EditSession>>>,
        changed: Rc<dyn Fn()>,
        on_selected: OnFilterSelected,
    ) {
        let filter = Rc::clone(self.filter());

        self.toggle().connect_clicked(move |clicked_btn| {
            if !clicked_btn.is_active() {
                return;
            }

            // Notify the section so it can deactivate other swatches.
            on_selected(&filter);

            // Apply the filter preset to the session.
            {
                let mut session = session.borrow_mut();
                let Some(s) = session.as_mut() else { return };
                let preset = filter.preset();
                s.state.exposure = preset.exposure;
                s.state.color = preset.color;
                s.state.filter = preset.filter;
                s.render_gen += 1;
            }

            changed();
        });
    }

    /// The filter trait object.
    pub fn filter(&self) -> &Rc<dyn Filter> {
        self.imp().filter.get().expect("filter not set")
    }

    /// Access the toggle button (for active state).
    pub fn toggle(&self) -> &gtk::ToggleButton {
        &self.imp().toggle
    }

    /// Set the toggle active state without triggering the click handler.
    pub fn set_active(&self, active: bool) {
        self.toggle().set_active(active);
    }
}
