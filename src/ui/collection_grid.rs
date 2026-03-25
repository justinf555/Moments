use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use tracing::{debug, info};

use crate::library::faces::PersonId;
use crate::library::media::MediaFilter;
use crate::library::Library;
use crate::ui::model_registry::ModelRegistry;
use crate::ui::photo_grid::model::PhotoGridModel;
use crate::ui::photo_grid::texture_cache::TextureCache;
use crate::ui::photo_grid::PhotoGridView;
use crate::ui::ContentView;

pub mod cell;
pub mod factory;
pub mod item;

use factory::ThumbnailStyle;
use item::{CollectionItemData, CollectionItemObject};

/// A reusable grid view for browsing collections (people, memories, etc.).
///
/// Displays a grid of items with thumbnails and labels. Clicking an item
/// pushes a `PhotoGridView` onto the internal `NavigationView`, filtered
/// to show that item's media.
pub struct CollectionGridView {
    widget: gtk::Widget,
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
        registry: Rc<ModelRegistry>,
        texture_cache: Rc<TextureCache>,
    ) -> Self {
        let header = adw::HeaderBar::new();

        let grid_view = gtk::GridView::new(
            None::<gtk::NoSelection>,
            None::<gtk::SignalListItemFactory>,
        );
        grid_view.set_min_columns(3);
        grid_view.set_max_columns(8);
        grid_view.set_valign(gtk::Align::Center);
        grid_view.set_vexpand(true);

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

        // Wire item activation — clicking a person pushes their PhotoGridView.
        {
            let nav_clone = nav_view.clone();
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let s = settings.clone();
            let reg = Rc::clone(&registry);
            let tc = Rc::clone(&texture_cache);
            let store_ref = store.clone();
            grid_view.connect_activate(move |_, position| {
                let Some(obj) = store_ref
                    .item(position)
                    .and_then(|o| o.downcast::<CollectionItemObject>().ok())
                else {
                    return;
                };

                let data = obj.data();
                let person_id = PersonId::from_raw(data.id.clone());
                debug!(person = %data.name, id = %data.id, "person activated");

                let filter = MediaFilter::Person {
                    person_id: person_id.clone(),
                };
                let model = Rc::new(PhotoGridModel::new(
                    Arc::clone(&lib),
                    tk.clone(),
                    filter,
                ));
                let view = Rc::new(PhotoGridView::new(
                    Arc::clone(&lib),
                    tk.clone(),
                    s.clone(),
                    Rc::clone(&reg),
                    Rc::clone(&tc),
                ));
                view.set_model(Rc::clone(&model), Rc::clone(&reg));
                reg.register(&model);

                let display_name = if data.name.is_empty() {
                    "Unnamed".to_string()
                } else {
                    data.name.clone()
                };

                let person_page = adw::NavigationPage::builder()
                    .tag(&format!("person:{}", data.id))
                    .title(&display_name)
                    .child(view.widget())
                    .build();

                nav_clone.push(&person_page);
            });
        }

        // Load people asynchronously.
        {
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            glib::MainContext::default().spawn_local(async move {
                let lib_q = Arc::clone(&lib);
                let result = tk
                    .spawn(async move { lib_q.list_people(false, false).await })
                    .await;

                match result {
                    Ok(Ok(people)) => {
                        info!(count = people.len(), "loaded people for collection grid");
                        for person in &people {
                            let thumbnail_path = lib.person_thumbnail_path(&person.id);

                            let subtitle = format!(
                                "{} {}",
                                person.face_count,
                                if person.face_count == 1 {
                                    "photo"
                                } else {
                                    "photos"
                                }
                            );

                            let item = CollectionItemObject::new(CollectionItemData {
                                id: person.id.as_str().to_string(),
                                name: person.name.clone(),
                                subtitle,
                                thumbnail_path,
                            });
                            store.append(&item);
                        }
                    }
                    Ok(Err(e)) => tracing::error!("list_people failed: {e}"),
                    Err(e) => tracing::error!("list_people join failed: {e}"),
                }
            });
        }

        Self { widget }
    }
}

impl ContentView for CollectionGridView {
    fn widget(&self) -> &gtk::Widget {
        &self.widget
    }
}
