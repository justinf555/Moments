use std::cell::Cell;
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
        registry: Rc<ModelRegistry>,
        texture_cache: Rc<TextureCache>,
    ) -> Self {
        let header = adw::HeaderBar::new();

        // ── Filter toggle buttons ────────────────────────────────────────
        let filter = Rc::new(PeopleFilter {
            include_hidden: Cell::new(false),
            include_unnamed: Cell::new(false),
        });

        let unnamed_toggle = gtk::ToggleButton::builder()
            .icon_name("person-symbolic")
            .tooltip_text("Show Unnamed")
            .build();
        unnamed_toggle.add_css_class("flat");

        let hidden_toggle = gtk::ToggleButton::builder()
            .icon_name("view-reveal-symbolic")
            .tooltip_text("Show Hidden")
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

        // ── Wire item activation ─────────────────────────────────────────
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

        // ── Wire right-click context menu ────────────────────────────────
        {
            let gesture = gtk::GestureClick::new();
            gesture.set_button(3);

            let gv = grid_view.clone();
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let store_ctx = store.clone();
            let filter_ctx = Rc::clone(&filter);

            gesture.connect_pressed(move |gesture, _, x, y| {
                let Some(picked) = gv.pick(x, y, gtk::PickFlags::DEFAULT) else {
                    return;
                };

                let grid_widget = gv.upcast_ref::<gtk::Widget>();
                let mut target = Some(picked);
                while let Some(ref w) = target {
                    if w.parent().as_ref() == Some(grid_widget) {
                        break;
                    }
                    target = w.parent();
                }
                let Some(target) = target else { return };

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
                let is_hidden = data.is_hidden;

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

                // ── Hide/Unhide button ──
                let hide_label = if is_hidden { "Unhide" } else { "Hide" };
                let hide_btn = gtk::Button::with_label(hide_label);
                hide_btn.add_css_class("flat");
                vbox.append(&hide_btn);

                // Wire rename.
                let pop_weak = popover.downgrade();
                let lib_r = Arc::clone(&lib);
                let tk_r = tk.clone();
                let store_r = store_ctx.clone();
                let pid_r = person_id.clone();
                let gv_ref = gv.clone();
                let item_subtitle = data.subtitle.clone();
                let item_thumb = data.thumbnail_path.clone();
                let item_hidden = data.is_hidden;
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
                    let subtitle = item_subtitle.clone();
                    let thumb = item_thumb.clone();
                    let hidden = item_hidden;
                    dialog.connect_response(None, move |_, response| {
                        if response != "rename" {
                            return;
                        }
                        let new_name = entry.text().to_string();
                        if new_name.is_empty() {
                            return;
                        }
                        let pid_str = pid.clone();
                        let pid = PersonId::from_raw(pid.clone());
                        let lib = Arc::clone(&lib);
                        let tk = tk.clone();
                        let store = store.clone();
                        let subtitle = subtitle.clone();
                        let thumb = thumb.clone();
                        debug!(person_id = %pid, name = %new_name, "renaming person");
                        glib::MainContext::default().spawn_local(async move {
                            let name = new_name.clone();
                            let result = tk
                                .spawn(async move { lib.rename_person(&pid, &name).await })
                                .await;
                            match result {
                                Ok(Ok(())) => {
                                    info!("person renamed successfully");
                                    replace_item(&store, &pid_str, CollectionItemData {
                                        id: pid_str.clone(),
                                        name: new_name,
                                        subtitle,
                                        thumbnail_path: thumb,
                                        is_hidden: hidden,
                                    });
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

                // Wire hide/unhide.
                let pop_weak = popover.downgrade();
                let lib_h = Arc::clone(&lib);
                let tk_h = tk.clone();
                let store_h = store_ctx.clone();
                let filter_h = Rc::clone(&filter_ctx);
                let new_hidden = !is_hidden;
                hide_btn.connect_clicked(move |_| {
                    if let Some(p) = pop_weak.upgrade() {
                        p.popdown();
                    }
                    let pid = PersonId::from_raw(person_id.clone());
                    let lib = Arc::clone(&lib_h);
                    let tk = tk_h.clone();
                    let store = store_h.clone();
                    let f = Rc::clone(&filter_h);
                    let action = if new_hidden { "hiding" } else { "unhiding" };
                    debug!(person_id = %pid, action, "toggling person visibility");
                    let pid_for_remove = pid.to_string();
                    glib::MainContext::default().spawn_local(async move {
                        let result = tk
                            .spawn(async move { lib.set_person_hidden(&pid, new_hidden).await })
                            .await;
                        match result {
                            Ok(Ok(())) => {
                                info!("person visibility changed successfully");
                                // If hiding and "Show Hidden" is off, just remove the item.
                                // If unhiding and "Show Hidden" is on, item stays — no change needed.
                                if new_hidden && !f.include_hidden.get() {
                                    remove_by_id(&store, &pid_for_remove);
                                } else if !new_hidden && f.include_hidden.get() {
                                    // Unhiding while showing hidden — item stays, no action needed.
                                }
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
fn incremental_reload(store: &gio::ListStore, library: &Arc<dyn Library>, filter: &Rc<PeopleFilter>) {
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
