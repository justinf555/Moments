use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::{debug, info};

use crate::library::Library;
use crate::ui::photo_grid::texture_cache::TextureCache;
use crate::ui::ContentView;

mod actions;
pub mod cell;
pub mod factory;
pub mod item;

use factory::ThumbnailStyle;
use item::{CollectionItemData, CollectionItemObject};

/// Shared filter state for the people grid, wrapped in `Rc` so that
/// toggle buttons, load, reload, and context menu closures can all access it.
struct PeopleFilter {
    include_hidden: Cell<bool>,
    include_unnamed: Cell<bool>,
}

/// A reusable grid view for browsing collections (people, memories, etc.).
///
/// Displays a grid of items with thumbnails and labels. Clicking an item
/// pushes a `PhotoGridView` onto the internal `NavigationView`, filtered
/// to show that item's media.
pub struct CollectionGridView {
    widget: gtk::Widget,
    store: gio::ListStore,
    library: Arc<dyn Library>,
    filter: Rc<PeopleFilter>,
}

impl std::fmt::Debug for CollectionGridView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CollectionGridView").finish_non_exhaustive()
    }
}

impl CollectionGridView {
    /// Create a new collection grid view for People.
    ///
    /// Loads people from the library asynchronously and populates the grid.
    /// Clicking a person pushes a `PhotoGridView` filtered to that person.
    pub fn new_people(
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        settings: gio::Settings,
        texture_cache: Rc<TextureCache>,
        bus_sender: crate::event_bus::EventSender,
    ) -> Self {
        let header = adw::HeaderBar::new();

        // ── Filter toggle buttons ────────────────────────────────────────
        let filter = Rc::new(PeopleFilter {
            include_hidden: Cell::new(false),
            include_unnamed: Cell::new(false),
        });

        let unnamed_toggle = gtk::ToggleButton::builder()
            .icon_name("avatar-default-symbolic")
            .tooltip_text(gettext("Show Unnamed"))
            .build();
        unnamed_toggle.add_css_class("flat");

        let hidden_toggle = gtk::ToggleButton::builder()
            .icon_name("view-reveal-symbolic")
            .tooltip_text(gettext("Show Hidden"))
            .build();
        hidden_toggle.add_css_class("flat");

        let toggle_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        toggle_box.append(&unnamed_toggle);
        toggle_box.append(&hidden_toggle);
        header.pack_start(&toggle_box);

        let grid_view = gtk::GridView::new(
            None::<gtk::NoSelection>,
            None::<gtk::SignalListItemFactory>,
        );
        grid_view.set_min_columns(3);
        grid_view.set_max_columns(8);

        let cell_size = 140;
        let factory = factory::build_factory(cell_size, ThumbnailStyle::Circular);
        let store = gio::ListStore::new::<CollectionItemObject>();
        let selection = gtk::NoSelection::new(Some(store.clone()));
        grid_view.set_model(Some(&selection));
        grid_view.set_factory(Some(&factory));

        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
        scrolled.set_vexpand(true);
        scrolled.set_child(Some(&grid_view));

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&scrolled));

        let grid_page = adw::NavigationPage::builder()
            .tag("collection")
            .title("People")
            .child(&toolbar_view)
            .build();

        let nav_view = adw::NavigationView::new();
        nav_view.push(&grid_page);

        let widget = nav_view.clone().upcast::<gtk::Widget>();

        // Remove the person grid's zoom actions when navigating back.
        nav_view.connect_popped(|nav, _page| {
            let is_collection = nav
                .visible_page()
                .and_then(|p| p.tag())
                .map(|t| t == "collection")
                .unwrap_or(false);
            if is_collection {
                if let Some(win) = nav.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    win.insert_action_group("view", None::<&gtk::gio::SimpleActionGroup>);
                }
            }
        });

        // ── Wire toggle buttons to reload ────────────────────────────────
        {
            let f = Rc::clone(&filter);
            let s = store.clone();
            let lib = Arc::clone(&library);
            unnamed_toggle.connect_toggled(move |btn| {
                f.include_unnamed.set(btn.is_active());
                debug!(include_unnamed = btn.is_active(), "unnamed toggle changed");
                full_reload(&s, &lib, &f);
            });
        }
        {
            let f = Rc::clone(&filter);
            let s = store.clone();
            let lib = Arc::clone(&library);
            hidden_toggle.connect_toggled(move |btn| {
                f.include_hidden.set(btn.is_active());
                debug!(include_hidden = btn.is_active(), "hidden toggle changed");
                full_reload(&s, &lib, &f);
            });
        }

        // ── Wire item activation and context menu ────────────────────────
        actions::wire_activation(
            &grid_view, &store, &nav_view, &library,
            &tokio, &settings, &texture_cache, &bus_sender,
        );
        actions::wire_context_menu(
            &grid_view, &store, &library, &tokio, &filter,
        );

        // Load people asynchronously.
        load_people(&store, &library, &filter);

        Self {
            widget,
            store,
            library,
            filter,
        }
    }

    /// Reload the people grid from the database.
    pub fn reload(&self) {
        incremental_reload(&self.store, &self.library, &self.filter);
    }
}

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

impl ContentView for CollectionGridView {
    fn widget(&self) -> &gtk::Widget {
        &self.widget
    }
}
