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
    store: gio::ListStore,
    library: Arc<dyn Library>,
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

        // Wire right-click context menu.
        {
            let gesture = gtk::GestureClick::new();
            gesture.set_button(3);

            let gv = grid_view.clone();
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let store_ctx = store.clone();

            gesture.connect_pressed(move |gesture, _, x, y| {
                let Some(picked) = gv.pick(x, y, gtk::PickFlags::DEFAULT) else {
                    return;
                };

                // Walk up to the direct child of the GridView.
                let grid_widget = gv.upcast_ref::<gtk::Widget>();
                let mut target = Some(picked);
                while let Some(ref w) = target {
                    if w.parent().as_ref() == Some(grid_widget) {
                        break;
                    }
                    target = w.parent();
                }
                let Some(target) = target else { return };

                // Find position by counting siblings.
                let mut pos = 0u32;
                let mut child = gv.first_child();
                loop {
                    let Some(c) = child else { return };
                    if c == target {
                        break;
                    }
                    pos += 1;
                    child = c.next_sibling();
                }

                let Some(obj) = store_ctx
                    .item(pos)
                    .and_then(|o| o.downcast::<CollectionItemObject>().ok())
                else {
                    return;
                };

                let data = obj.data();
                let person_id = data.id.clone();
                let current_name = data.name.clone();

                // Build context menu popover.
                let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
                vbox.set_margin_top(6);
                vbox.set_margin_bottom(6);
                vbox.set_margin_start(6);
                vbox.set_margin_end(6);

                let popover = gtk::Popover::new();

                // ── Rename button ──
                let rename_btn = gtk::Button::with_label("Rename");
                rename_btn.add_css_class("flat");
                vbox.append(&rename_btn);

                // ── Hide button ──
                let hide_btn = gtk::Button::with_label("Hide");
                hide_btn.add_css_class("flat");
                vbox.append(&hide_btn);

                let pop_weak = popover.downgrade();
                let lib_r = Arc::clone(&lib);
                let tk_r = tk.clone();
                let store_r = store_ctx.clone();
                let lib_r2 = Arc::clone(&lib);
                let pid_r = person_id.clone();
                let gv_ref = gv.clone();
                rename_btn.connect_clicked(move |_| {
                    if let Some(p) = pop_weak.upgrade() {
                        p.popdown();
                    }

                    let dialog = adw::AlertDialog::builder()
                        .heading("Rename Person")
                        .build();
                    dialog.add_response("cancel", "Cancel");
                    dialog.add_response("rename", "Rename");
                    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
                    dialog.set_default_response(Some("rename"));
                    dialog.set_close_response("cancel");

                    let entry = gtk::Entry::new();
                    entry.set_text(&current_name);
                    entry.set_activates_default(true);
                    dialog.set_extra_child(Some(&entry));

                    let lib = Arc::clone(&lib_r);
                    let tk = tk_r.clone();
                    let store = store_r.clone();
                    let pid = pid_r.clone();
                    let lib_reload = Arc::clone(&lib_r2);
                    dialog.connect_response(None, move |_, response| {
                        if response != "rename" {
                            return;
                        }
                        let new_name = entry.text().to_string();
                        if new_name.is_empty() {
                            return;
                        }
                        let pid = PersonId::from_raw(pid.clone());
                        let lib = Arc::clone(&lib);
                        let tk = tk.clone();
                        let store = store.clone();
                        let lib_reload = Arc::clone(&lib_reload);
                        debug!(person_id = %pid, name = %new_name, "renaming person");
                        glib::MainContext::default().spawn_local(async move {
                            let name = new_name.clone();
                            let result = tk
                                .spawn(async move { lib.rename_person(&pid, &name).await })
                                .await;
                            match result {
                                Ok(Ok(())) => {
                                    info!("person renamed successfully");
                                    reload_people(&store, &lib_reload);
                                }
                                Ok(Err(e)) => tracing::error!("rename_person failed: {e}"),
                                Err(e) => tracing::error!("rename_person join failed: {e}"),
                            }
                        });
                    });
                    dialog.present(
                        gv_ref
                            .root()
                            .as_ref()
                            .and_then(|r| r.downcast_ref::<gtk::Window>()),
                    );
                });

                let pop_weak = popover.downgrade();
                let lib_h = Arc::clone(&lib);
                let tk_h = tk.clone();
                let store_h = store_ctx.clone();
                let lib_h2 = Arc::clone(&lib);
                hide_btn.connect_clicked(move |_| {
                    if let Some(p) = pop_weak.upgrade() {
                        p.popdown();
                    }
                    let pid = PersonId::from_raw(person_id.clone());
                    let lib = Arc::clone(&lib_h);
                    let tk = tk_h.clone();
                    let store = store_h.clone();
                    let lib_reload = Arc::clone(&lib_h2);
                    debug!(person_id = %pid, "hiding person");
                    glib::MainContext::default().spawn_local(async move {
                        let result = tk
                            .spawn(async move { lib.set_person_hidden(&pid, true).await })
                            .await;
                        match result {
                            Ok(Ok(())) => {
                                info!("person hidden successfully");
                                reload_people(&store, &lib_reload);
                            }
                            Ok(Err(e)) => tracing::error!("set_person_hidden failed: {e}"),
                            Err(e) => tracing::error!("set_person_hidden join failed: {e}"),
                        }
                    });
                });

                popover.set_child(Some(&vbox));
                popover.set_parent(&gv);
                popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
                    x as i32, y as i32, 1, 1,
                )));
                popover.set_has_arrow(true);

                popover.connect_closed(move |p| {
                    p.unparent();
                });

                popover.popup();
                gesture.set_state(gtk::EventSequenceState::Claimed);
            });

            grid_view.add_controller(gesture);
        }

        // Load people asynchronously.
        load_people(&store, &library);

        Self {
            widget,
            store,
            library,
        }
    }

    /// Reload the people grid from the database.
    pub fn reload(&self) {
        reload_people(&self.store, &self.library);
    }
}

/// Load people from the library and populate the store.
fn load_people(store: &gio::ListStore, library: &Arc<dyn Library>) {
    let lib = Arc::clone(library);
    let store = store.clone();
    let lib_thumb = Arc::clone(library);
    glib::MainContext::default().spawn_local(async move {
        let lib_q = Arc::clone(&lib);
        let result = crate::application::MomentsApplication::default()
            .tokio_handle()
            .spawn(async move { lib_q.list_people(false, false).await })
            .await;

        match result {
            Ok(Ok(people)) => {
                info!(count = people.len(), "loaded people for collection grid");
                for person in &people {
                    let thumbnail_path = lib_thumb.person_thumbnail_path(&person.id);

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

/// Clear and reload the people store after a management action.
fn reload_people(store: &gio::ListStore, library: &Arc<dyn Library>) {
    store.remove_all();
    load_people(store, library);
}

impl ContentView for CollectionGridView {
    fn widget(&self) -> &gtk::Widget {
        &self.widget
    }
}
