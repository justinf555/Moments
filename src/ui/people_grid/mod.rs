use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};

use crate::client::PeopleClientV2;
use crate::ui::photo_grid::texture_cache::TextureCache;

mod actions;
pub mod cell;
pub mod factory;

/// Shared filter state for the people grid.
///
/// Toggle buttons mutate this state, then call `changed()` on the
/// `gtk::CustomFilter` so the `FilterListModel` re-evaluates visibility.
pub(crate) struct PeopleFilter {
    include_hidden: Cell<bool>,
    include_unnamed: Cell<bool>,
}

// ── GObject subclass ─────────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::OnceCell;

    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/people_grid/people_grid.ui")]
    pub struct PeopleGridView {
        #[template_child]
        pub nav_view: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub grid_view: TemplateChild<gtk::GridView>,
        #[template_child]
        pub unnamed_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub hidden_toggle: TemplateChild<gtk::ToggleButton>,

        // Service dependencies
        pub people_client: OnceCell<PeopleClientV2>,

        // State
        pub(super) store: OnceCell<gio::ListStore>,
        pub(super) filter_model: OnceCell<gtk::FilterListModel>,
        pub(super) filter_state: OnceCell<Rc<PeopleFilter>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PeopleGridView {
        const NAME: &'static str = "MomentsPeopleGridView";
        type Type = super::PeopleGridView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PeopleGridView {
        fn dispose(&self) {
            self.dispose_template();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for PeopleGridView {}
}

glib::wrapper! {
    pub struct PeopleGridView(ObjectSubclass<imp::PeopleGridView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PeopleGridView {
    fn default() -> Self {
        Self::new()
    }
}

impl PeopleGridView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Set up the People collection grid view.
    pub fn setup_people(
        &self,
        settings: gio::Settings,
        texture_cache: Rc<TextureCache>,
        bus_sender: crate::event_bus::EventSender,
    ) {
        let imp = self.imp();

        let people_client = crate::application::MomentsApplication::default()
            .people_client()
            .expect("people client available after library load");
        assert!(
            imp.people_client.set(people_client.clone()).is_ok(),
            "setup called twice"
        );

        let filter_state = Rc::new(PeopleFilter {
            include_hidden: Cell::new(false),
            include_unnamed: Cell::new(false),
        });

        // Client returns all people — filtering happens here via FilterListModel.
        let store = people_client.create_model();

        let fs = Rc::clone(&filter_state);
        let custom_filter = gtk::CustomFilter::new(move |obj| {
            let Some(person) = obj.downcast_ref::<crate::client::PersonItemObject>() else {
                return false;
            };
            if person.is_hidden() && !fs.include_hidden.get() {
                return false;
            }
            if person.name().is_empty() && !fs.include_unnamed.get() {
                return false;
            }
            true
        });

        let filter_model =
            gtk::FilterListModel::new(Some(store.clone()), Some(custom_filter.clone()));

        let cell_size = 140;
        let factory = factory::build_factory(cell_size);
        let selection = gtk::NoSelection::new(Some(filter_model.clone()));
        imp.grid_view.set_model(Some(&selection));
        imp.grid_view.set_factory(Some(&factory));

        // Wire toggle buttons to update filter state and re-evaluate.
        {
            let fs = Rc::clone(&filter_state);
            let cf = custom_filter.clone();
            imp.unnamed_toggle.connect_toggled(move |btn| {
                fs.include_unnamed.set(btn.is_active());
                cf.changed(gtk::FilterChange::Different);
            });
        }
        {
            let fs = Rc::clone(&filter_state);
            let cf = custom_filter;
            imp.hidden_toggle.connect_toggled(move |btn| {
                fs.include_hidden.set(btn.is_active());
                cf.changed(gtk::FilterChange::Different);
            });
        }

        // Wire item activation and context menu.
        actions::wire_activation(
            &imp.grid_view,
            &filter_model,
            &imp.nav_view,
            &settings,
            &texture_cache,
            &bus_sender,
        );
        actions::wire_context_menu(&imp.grid_view, &filter_model, &people_client);

        // Initial populate.
        people_client.list_people(&store);

        assert!(imp.store.set(store).is_ok());
        assert!(imp.filter_model.set(filter_model).is_ok());
        assert!(imp.filter_state.set(filter_state).is_ok());
    }
}
