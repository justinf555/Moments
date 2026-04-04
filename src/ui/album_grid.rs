use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::debug;

use crate::library::album::{Album, AlbumId};
use crate::library::media::MediaFilter;
use crate::library::Library;
use crate::ui::album_dialogs;
use crate::ui::photo_grid::model::PhotoGridModel;
use crate::ui::photo_grid::texture_cache::TextureCache;
use crate::ui::photo_grid::PhotoGridView;
use crate::ui::ContentView;

pub mod card;
pub mod factory;
pub mod item;

use item::AlbumItemObject;

/// Sort order for the album grid.
/// Values match the GSettings `album-sort-order` key.
const SORT_RECENT: u32 = 0;
const SORT_NAME: u32 = 1;
const SORT_CREATED: u32 = 2;

/// Grid view displaying all user albums as cards.
pub struct AlbumGridView {
    widget: gtk::Widget,
    store: gio::ListStore,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
}

impl std::fmt::Debug for AlbumGridView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlbumGridView").finish_non_exhaustive()
    }
}

impl AlbumGridView {
    pub fn new(
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        settings: gio::Settings,
        texture_cache: Rc<TextureCache>,
        bus_sender: crate::event_bus::EventSender,
    ) -> Self {
        // ── Sort state ──────────────────────────────────────────────────
        let sort_order = Rc::new(Cell::new(settings.uint("album-sort-order")));

        // ── Headerbar ───────────────────────────────────────────────────
        let header = adw::HeaderBar::new();

        let new_album_btn = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .label(&gettext("New Album"))
            .build();
        new_album_btn.add_css_class("outlined");
        header.pack_start(&new_album_btn);

        // Overflow menu (⋮) with sort options.
        let sort_menu = build_sort_menu();
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text(&gettext("Menu"))
            .menu_model(&sort_menu)
            .build();
        menu_btn.add_css_class("flat");
        header.pack_end(&menu_btn);

        // Sort action group — radio action with u32 state.
        let sort_action = gio::SimpleAction::new_stateful(
            "sort",
            Some(&u32::static_variant_type()),
            &sort_order.get().to_variant(),
        );

        let action_group = gio::SimpleActionGroup::new();
        action_group.add_action(&sort_action);

        // ── Grid ────────────────────────────────────────────────────────
        let store = gio::ListStore::new::<AlbumItemObject>();
        let selection = gtk::NoSelection::new(Some(store.clone()));

        let grid_view = gtk::GridView::new(
            Some(selection),
            Some(factory::build_factory(
                Arc::clone(&library),
                tokio.clone(),
            )),
        );
        grid_view.set_min_columns(2);
        grid_view.set_max_columns(8);

        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
        scrolled.set_vexpand(true);
        scrolled.set_child(Some(&grid_view));

        // ── Empty state ─────────────────────────────────────────────────
        let empty_page = adw::StatusPage::builder()
            .icon_name("folder-symbolic")
            .title(&gettext("No Albums Yet"))
            .description(&gettext(
                "Create an album to start organising your photos into collections.",
            ))
            .vexpand(true)
            .build();

        let empty_new_btn = gtk::Button::builder()
            .label(&gettext("New Album"))
            .halign(gtk::Align::Center)
            .build();
        empty_new_btn.add_css_class("pill");
        empty_new_btn.add_css_class("suggested-action");
        empty_page.set_child(Some(&empty_new_btn));

        // Stack to switch between grid and empty state.
        let content_stack = gtk::Stack::new();
        content_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        content_stack.add_named(&scrolled, Some("grid"));
        content_stack.add_named(&empty_page, Some("empty"));
        content_stack.set_visible_child_name("empty");

        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&content_stack));
        toolbar_view.insert_action_group("album", Some(&action_group));

        let grid_page = adw::NavigationPage::builder()
            .tag("albums")
            .title(&gettext("Albums"))
            .child(&toolbar_view)
            .build();

        let nav_view = adw::NavigationView::new();
        nav_view.push(&grid_page);

        let widget = nav_view.clone().upcast::<gtk::Widget>();

        // Remove drill-down view actions when popping back to the album grid.
        nav_view.connect_popped(|nav, _page| {
            let is_albums = nav
                .visible_page()
                .and_then(|p| p.tag())
                .map(|t| t == "albums")
                .unwrap_or(false);
            if is_albums {
                if let Some(win) = nav.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                    win.insert_action_group("view", None::<&gtk::gio::SimpleActionGroup>);
                }
            }
        });

        // ── Toggle empty ↔ grid based on store count ────────────────────
        {
            let stack = content_stack.clone();
            store.connect_items_changed(move |store, _, _, _| {
                let target = if store.n_items() > 0 { "grid" } else { "empty" };
                stack.set_visible_child_name(target);
            });
        }

        // ── Wire sort action ────────────────────────────────────────────
        {
            let so = Rc::clone(&sort_order);
            let s = settings.clone();
            let st = store.clone();
            sort_action.connect_activate(move |action, param| {
                let Some(value) = param.and_then(|v| v.get::<u32>()) else { return };
                action.set_state(&value.to_variant());
                so.set(value);
                s.set_uint("album-sort-order", value).ok();
                sort_store(&st, value);
                debug!(sort_order = value, "album sort changed");
            });
        }

        // ── Wire "New Album" buttons ────────────────────────────────────
        {
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let connect_create = move |btn: &gtk::Button| {
                let lib = Arc::clone(&lib);
                let tk = tk.clone();
                album_dialogs::show_create_album_dialog(
                    btn,
                    move |name| {
                        let lib = Arc::clone(&lib);
                        let tk = tk.clone();
                        glib::MainContext::default().spawn_local(async move {
                            let n = name.clone();
                            match tk.spawn(async move { lib.create_album(&n).await }).await {
                                Ok(Ok(id)) => {
                                    debug!(album_id = %id, name = %name, "album created from albums view");
                                }
                                Ok(Err(e)) => {
                                    tracing::error!("failed to create album: {e}");
                                }
                                Err(e) => tracing::error!("tokio join error: {e}"),
                            }
                        });
                    },
                );
            };

            let cb = connect_create.clone();
            new_album_btn.connect_clicked(move |btn| cb(btn));
            empty_new_btn.connect_clicked(move |btn| connect_create(btn));
        }

        // ── Wire item activation (click → open album photo grid) ────────
        {
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let s = settings.clone();
            let tc = Rc::clone(&texture_cache);
            let bs = bus_sender.clone();
            let st = store.clone();
            let nav = nav_view.clone();

            grid_view.connect_activate(move |_, position| {
                let Some(obj) = st.item(position) else { return };
                let Some(item) = obj.downcast_ref::<AlbumItemObject>() else { return };
                let album = item.album();
                let album_id = AlbumId::from_raw(album.id.as_str().to_owned());
                let album_name = album.name.clone();

                debug!(album_id = %album.id, name = %album_name, "album activated");

                let model = Rc::new(PhotoGridModel::new(
                    Arc::clone(&lib),
                    tk.clone(),
                    MediaFilter::Album { album_id },
                    bs.clone(),
                ));
                let view = Rc::new(PhotoGridView::new(
                    Arc::clone(&lib),
                    tk.clone(),
                    s.clone(),
                    Rc::clone(&tc),
                    bs.clone(),
                ));
                view.set_model(Rc::clone(&model));
                model.subscribe_to_bus();

                let page = adw::NavigationPage::builder()
                    .tag("album-detail")
                    .title(&album_name)
                    .child(view.widget())
                    .build();

                if let Some(actions) = view.view_actions() {
                    if let Some(win) = nav.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                        win.insert_action_group("view", Some(actions));
                    }
                }

                nav.push(&page);
            });
        }

        // ── Load albums asynchronously ──────────────────────────────────
        let view = Self {
            widget,
            store: store.clone(),
            library: Arc::clone(&library),
            tokio: tokio.clone(),
        };

        {
            let st = store.clone();
            let so = Rc::clone(&sort_order);
            reload_albums(&st, &library, &tokio, so);
        }

        // ── Subscribe to bus for album changes ──────────────────────────
        {
            let st = store.clone();
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let so = Rc::clone(&sort_order);
            crate::event_bus::subscribe(move |event| {
                match event {
                    crate::app_event::AppEvent::AlbumCreated { .. }
                    | crate::app_event::AppEvent::AlbumRenamed { .. }
                    | crate::app_event::AppEvent::AlbumDeleted { .. } => {
                        reload_albums(&st, &lib, &tk, Rc::clone(&so));
                    }
                    _ => {}
                }
            });
        }

        view
    }

    pub fn reload(&self) {
        // Can't easily pass sort_order here — reload uses default (recent).
        reload_albums(&self.store, &self.library, &self.tokio, Rc::new(Cell::new(SORT_RECENT)));
    }
}

impl ContentView for AlbumGridView {
    fn widget(&self) -> &gtk::Widget {
        &self.widget
    }
}

/// Build the overflow menu model with sort radio actions.
fn build_sort_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let sort_section = gio::Menu::new();
    sort_section.append(Some(&gettext("Most Recent Photo")), Some("album.sort(uint32 0)"));
    sort_section.append(Some(&gettext("Name (A–Z)")), Some("album.sort(uint32 1)"));
    sort_section.append(Some(&gettext("Date Created")), Some("album.sort(uint32 2)"));
    menu.append_section(Some(&gettext("Sort by")), &sort_section);

    menu
}

/// Sort the store in-place by the given sort order.
fn sort_store(store: &gio::ListStore, order: u32) {
    store.sort(|a, b| {
        let a = a.downcast_ref::<AlbumItemObject>().unwrap().album();
        let b = b.downcast_ref::<AlbumItemObject>().unwrap().album();
        match order {
            SORT_NAME => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SORT_CREATED => b.created_at.cmp(&a.created_at),
            _ => b.updated_at.cmp(&a.updated_at), // SORT_RECENT (default)
        }
    });
}

/// Async-load all albums into the store, then sort.
fn reload_albums(
    store: &gio::ListStore,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    sort_order: Rc<Cell<u32>>,
) {
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let store = store.clone();

    glib::MainContext::default().spawn_local(async move {
        let result = tk.spawn(async move { lib.list_albums().await }).await;
        match result {
            Ok(Ok(mut albums)) => {
                // Sort before building objects.
                let order = sort_order.get();
                albums.sort_by(|a, b| sort_albums(a, b, order));

                let objects: Vec<glib::Object> = albums
                    .into_iter()
                    .map(|a| AlbumItemObject::new(a).upcast())
                    .collect();
                store.splice(0, store.n_items(), &objects);
                debug!(count = store.n_items(), sort_order = order, "albums loaded");
            }
            Ok(Err(e)) => tracing::error!("failed to load albums: {e}"),
            Err(e) => tracing::error!("tokio join error loading albums: {e}"),
        }
    });
}

/// Compare two albums for sorting.
fn sort_albums(a: &Album, b: &Album, order: u32) -> std::cmp::Ordering {
    match order {
        SORT_NAME => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        SORT_CREATED => b.created_at.cmp(&a.created_at),
        _ => b.updated_at.cmp(&a.updated_at),
    }
}
