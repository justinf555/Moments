use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use tracing::{debug, info};

use crate::library::Library;
use crate::ui::photo_grid::texture_cache::TextureCache;

mod actions;
pub mod cell;
pub mod factory;
pub mod item;

use factory::ThumbnailStyle;
use item::{CollectionItemData, CollectionItemObject};

/// Shared filter state for the people grid.
struct PeopleFilter {
    include_hidden: Cell<bool>,
    include_unnamed: Cell<bool>,
}

// ── GObject subclass ─────────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::OnceCell;

    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/collection_grid.ui")]
    pub struct CollectionGridView {
        #[template_child]
        pub nav_view: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub grid_view: TemplateChild<gtk::GridView>,
        #[template_child]
        pub unnamed_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub hidden_toggle: TemplateChild<gtk::ToggleButton>,

        // Service dependencies
        pub library: OnceCell<Arc<dyn Library>>,

        // State
        pub(super) store: OnceCell<gio::ListStore>,
        pub(super) filter: OnceCell<Rc<PeopleFilter>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CollectionGridView {
        const NAME: &'static str = "MomentsCollectionGridView";
        type Type = super::CollectionGridView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for CollectionGridView {
        fn dispose(&self) {
            self.dispose_template();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for CollectionGridView {}
}

glib::wrapper! {
    pub struct CollectionGridView(ObjectSubclass<imp::CollectionGridView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for CollectionGridView {
    fn default() -> Self {
        Self::new()
    }
}

impl CollectionGridView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Set up the People collection grid view.
    pub fn setup_people(
        &self,
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        settings: gio::Settings,
        texture_cache: Rc<TextureCache>,
        bus_sender: crate::event_bus::EventSender,
    ) {
        let imp = self.imp();
        assert!(imp.library.set(Arc::clone(&library)).is_ok(), "setup called twice");

        let filter = Rc::new(PeopleFilter {
            include_hidden: Cell::new(false),
            include_unnamed: Cell::new(false),
        });
        let store = gio::ListStore::new::<CollectionItemObject>();

        let cell_size = 140;
        let factory = factory::build_factory(cell_size, ThumbnailStyle::Circular);
        let selection = gtk::NoSelection::new(Some(store.clone()));
        imp.grid_view.set_model(Some(&selection));
        imp.grid_view.set_factory(Some(&factory));

        // Wire toggle buttons to reload.
        {
            let f = Rc::clone(&filter);
            let s = store.clone();
            let lib = Arc::clone(&library);
            imp.unnamed_toggle.connect_toggled(move |btn| {
                f.include_unnamed.set(btn.is_active());
                debug!(include_unnamed = btn.is_active(), "unnamed toggle changed");
                full_reload(&s, &lib, &f);
            });
        }
        {
            let f = Rc::clone(&filter);
            let s = store.clone();
            let lib = Arc::clone(&library);
            imp.hidden_toggle.connect_toggled(move |btn| {
                f.include_hidden.set(btn.is_active());
                debug!(include_hidden = btn.is_active(), "hidden toggle changed");
                full_reload(&s, &lib, &f);
            });
        }

        // Wire item activation and context menu.
        actions::wire_activation(
            &imp.grid_view, &store, &imp.nav_view, &library,
            &tokio, &settings, &texture_cache, &bus_sender,
        );
        actions::wire_context_menu(
            &imp.grid_view, &store, &library, &tokio, &filter,
        );

        // Load people asynchronously.
        load_people(&store, &library, &filter);

        assert!(imp.store.set(store).is_ok());
        assert!(imp.filter.set(filter).is_ok());
    }

    /// Reload the people grid from the database.
    pub fn reload(&self) {
        let imp = self.imp();
        if let (Some(store), Some(library), Some(filter)) =
            (imp.store.get(), imp.library.get(), imp.filter.get())
        {
            incremental_reload(store, library, filter);
        }
    }
}

// ── Free functions ───────────────────────────────────────────────────────────

/// Load people from the library and populate the store (initial load).
fn load_people(store: &gio::ListStore, library: &Arc<dyn Library>, filter: &Rc<PeopleFilter>) {
    let lib = Arc::clone(library);
    let store = store.clone();
    let lib_thumb = Arc::clone(library);
    let include_hidden = filter.include_hidden.get();
    let include_unnamed = filter.include_unnamed.get();
    glib::MainContext::default().spawn_local(async move {
        let lib_q = Arc::clone(&lib);
        let result = crate::application::MomentsApplication::default()
            .tokio_handle()
            .spawn(async move { lib_q.list_people(include_hidden, include_unnamed).await })
            .await;

        match result {
            Ok(Ok(people)) => {
                info!(count = people.len(), include_hidden, include_unnamed, "loaded people for collection grid");
                for person in &people {
                    let thumbnail_path = lib_thumb.person_thumbnail_path(&person.id);

                    let subtitle = format!(
                        "{} {}",
                        person.face_count,
                        if person.face_count == 1 { "photo" } else { "photos" }
                    );

                    let item = CollectionItemObject::new(CollectionItemData {
                        id: person.id.as_str().to_string(),
                        name: person.name.clone(),
                        subtitle,
                        thumbnail_path,
                        is_hidden: person.is_hidden,
                    });
                    store.append(&item);
                }
            }
            Ok(Err(e)) => tracing::error!("list_people failed: {e}"),
            Err(e) => tracing::error!("list_people join failed: {e}"),
        }
    });
}

/// Remove a single item from the store by person ID.
fn remove_by_id(store: &gio::ListStore, person_id: &str) {
    for i in 0..store.n_items() {
        if let Some(obj) = store
            .item(i)
            .and_then(|o| o.downcast::<CollectionItemObject>().ok())
        {
            if obj.data().id == person_id {
                store.remove(i);
                return;
            }
        }
    }
}

/// Replace an item in the store with updated data, preserving its position.
fn replace_item(store: &gio::ListStore, person_id: &str, data: CollectionItemData) {
    for i in 0..store.n_items() {
        if let Some(obj) = store
            .item(i)
            .and_then(|o| o.downcast::<CollectionItemObject>().ok())
        {
            if obj.data().id == person_id {
                store.remove(i);
                let new_item = CollectionItemObject::new(data);
                store.insert(i, &new_item);
                return;
            }
        }
    }
}

/// Full reload: clear store and re-populate (used when filter toggles change).
fn full_reload(store: &gio::ListStore, library: &Arc<dyn Library>, filter: &Rc<PeopleFilter>) {
    store.remove_all();
    load_people(store, library, filter);
}

/// Incremental update: insert new, remove deleted (used for sync refresh).
fn incremental_reload(
    store: &gio::ListStore,
    library: &Arc<dyn Library>,
    filter: &Rc<PeopleFilter>,
) {
    use std::collections::HashMap;

    let lib = Arc::clone(library);
    let store = store.clone();
    let lib_thumb = Arc::clone(library);
    let include_hidden = filter.include_hidden.get();
    let include_unnamed = filter.include_unnamed.get();
    glib::MainContext::default().spawn_local(async move {
        let lib_q = Arc::clone(&lib);
        let result = crate::application::MomentsApplication::default()
            .tokio_handle()
            .spawn(async move { lib_q.list_people(include_hidden, include_unnamed).await })
            .await;

        let people = match result {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => {
                tracing::error!("list_people failed: {e}");
                return;
            }
            Err(e) => {
                tracing::error!("list_people join failed: {e}");
                return;
            }
        };

        let fresh: HashMap<String, _> = people
            .iter()
            .map(|p| (p.id.as_str().to_string(), p))
            .collect();

        let mut existing: HashMap<String, u32> = HashMap::new();
        for i in 0..store.n_items() {
            if let Some(obj) = store
                .item(i)
                .and_then(|o| o.downcast::<CollectionItemObject>().ok())
            {
                existing.insert(obj.data().id.clone(), i);
            }
        }

        // Remove items no longer in fresh data (reverse order for stable indices).
        let mut to_remove: Vec<u32> = existing
            .iter()
            .filter(|(id, _)| !fresh.contains_key(id.as_str()))
            .map(|(_, &pos)| pos)
            .collect();
        to_remove.sort_unstable_by(|a, b| b.cmp(a));
        for pos in &to_remove {
            debug!(position = pos, "removing person from grid");
            store.remove(*pos);
        }

        // Insert new items.
        for person in &people {
            let pid = person.id.as_str().to_string();
            if existing.contains_key(&pid) {
                continue;
            }

            let thumbnail_path = lib_thumb.person_thumbnail_path(&person.id);
            let subtitle = format!(
                "{} {}",
                person.face_count,
                if person.face_count == 1 { "photo" } else { "photos" }
            );

            let item = CollectionItemObject::new(CollectionItemData {
                id: pid,
                name: person.name.clone(),
                subtitle,
                thumbnail_path,
                is_hidden: person.is_hidden,
            });
            debug!(person = %person.name, "inserting person into grid");
            store.append(&item);
        }
    });
}
