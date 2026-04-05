use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::debug;

use crate::library::album::{Album, AlbumId};
use crate::library::Library;
use crate::ui::album_dialogs;
use crate::ui::photo_grid::texture_cache::TextureCache;

mod actions;
pub mod card;
pub mod factory;
pub mod item;
mod selection;

use item::AlbumItemObject;

/// Sort order for the album grid.
/// Values match the GSettings `album-sort-order` key.
const SORT_NAME: u32 = 1;
const SORT_CREATED: u32 = 2;

// ── GObject subclass ─────────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::OnceCell;

    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/album_grid.ui")]
    pub struct AlbumGridView {
        #[template_child]
        pub nav_view: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub toolbar_view: TemplateChild<adw::ToolbarView>,
        #[template_child]
        pub header: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub new_album_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub cancel_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub menu_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub grid_view: TemplateChild<gtk::GridView>,
        #[template_child]
        pub empty_new_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub action_bar: TemplateChild<gtk::ActionBar>,

        // Service dependencies
        pub library: OnceCell<Arc<dyn Library>>,
        pub tokio: OnceCell<tokio::runtime::Handle>,

        // State
        pub(super) store: OnceCell<gio::ListStore>,
        pub(super) sort_order: OnceCell<Rc<Cell<u32>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AlbumGridView {
        const NAME: &'static str = "MomentsAlbumGridView";
        type Type = super::AlbumGridView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for AlbumGridView {
        fn dispose(&self) {
            self.dispose_template();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for AlbumGridView {}
}

glib::wrapper! {
    pub struct AlbumGridView(ObjectSubclass<imp::AlbumGridView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for AlbumGridView {
    fn default() -> Self {
        Self::new()
    }
}

impl AlbumGridView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn setup(
        &self,
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        settings: gio::Settings,
        texture_cache: Rc<TextureCache>,
        bus_sender: crate::event_bus::EventSender,
    ) {
        let imp = self.imp();
        assert!(imp.library.set(Arc::clone(&library)).is_ok(), "setup called twice");
        assert!(imp.tokio.set(tokio.clone()).is_ok(), "setup called twice");

        // ── Sort state ──────────────────────────────────────────────────
        let sort_order = Rc::new(Cell::new(settings.uint("album-sort-order")));

        // Sort menu.
        let sort_menu = build_sort_menu();
        imp.menu_btn.set_menu_model(Some(&sort_menu));

        // Sort action group — radio action with u32 state.
        let sort_action = gio::SimpleAction::new_stateful(
            "sort",
            Some(&u32::static_variant_type()),
            &sort_order.get().to_variant(),
        );

        let action_group = gio::SimpleActionGroup::new();
        action_group.add_action(&sort_action);

        // ── Selection mode state ────────────────────────────────────────
        let selection_mode = Rc::new(Cell::new(false));

        let selection_title = gtk::Label::new(Some("0 selected"));
        selection_title.add_css_class("heading");

        // Enter-selection action — created early so the factory can reference it.
        let enter_selection = gio::SimpleAction::new("select", None);

        // ── Grid ────────────────────────────────────────────────────────
        let store = gio::ListStore::new::<AlbumItemObject>();
        let multi_selection = gtk::MultiSelection::new(Some(store.clone()));

        imp.grid_view.set_model(Some(&multi_selection));
        imp.grid_view.set_factory(Some(&factory::build_factory(
            Arc::clone(&library),
            tokio.clone(),
            Rc::clone(&selection_mode),
            multi_selection.clone(),
            enter_selection.clone(),
        )));

        // ── Wire selection mode with the real widgets ────────────────────
        selection::wire_selection_mode(
            &enter_selection,
            &imp.header,
            &imp.new_album_btn,
            &imp.menu_btn,
            &imp.cancel_btn,
            &selection_title,
            &imp.action_bar,
            &imp.grid_view,
            &multi_selection,
            &store,
            &selection_mode,
            &library,
            &tokio,
            &bus_sender,
        );
        action_group.add_action(&enter_selection);

        // ── Toggle empty ↔ grid based on store count ────────────────────
        {
            let stack = imp.content_stack.clone();
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
            let bs = bus_sender.clone();
            let connect_create = move |btn: &gtk::Button| {
                let lib = Arc::clone(&lib);
                let tk = tk.clone();
                let bs = bs.clone();
                album_dialogs::show_create_album_dialog(
                    btn,
                    move |name| {
                        let lib = Arc::clone(&lib);
                        let tk = tk.clone();
                        let bs = bs.clone();
                        glib::MainContext::default().spawn_local(async move {
                            let n = name.clone();
                            match tk.spawn(async move { lib.create_album(&n).await }).await {
                                Ok(Ok(id)) => {
                                    debug!(album_id = %id, name = %name, "album created from albums view");
                                    bs.send(crate::app_event::AppEvent::AlbumCreated {
                                        id,
                                        name,
                                    });
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
            imp.new_album_btn.connect_clicked(move |btn| cb(btn));
            imp.empty_new_btn.connect_clicked(move |btn| connect_create(btn));
        }

        // ── Wire item activation (click → open album photo grid) ────────
        {
            let lib = Arc::clone(&library);
            let tk = tokio.clone();
            let s = settings.clone();
            let tc = Rc::clone(&texture_cache);
            let bs = bus_sender.clone();
            let st = store.clone();
            let nav = imp.nav_view.clone();

            imp.grid_view.connect_activate(move |_, position| {
                let Some(obj) = st.item(position) else { return };
                let Some(item) = obj.downcast_ref::<AlbumItemObject>() else { return };
                let album = item.album();
                let album_id = AlbumId::from_raw(album.id.as_str().to_owned());
                let album_name = album.name.clone();

                debug!(album_id = %album.id, name = %album_name, "album activated");

                actions::open_album_drilldown(
                    &lib, &tk, &s, &tc, &bs, &nav, album_id, &album_name,
                );
            });
        }

        // ── Right-click context menu ────────────────────────────────────
        {
            let gesture = gtk::GestureClick::new();
            gesture.set_button(3);

            let gv = imp.grid_view.clone();
            let lib_ctx = Arc::clone(&library);
            let tk_ctx = tokio.clone();
            let nav_ctx = imp.nav_view.clone();
            let s_ctx = settings.clone();
            let tc_ctx = Rc::clone(&texture_cache);
            let bs_ctx = bus_sender.clone();

            gesture.connect_pressed(move |gesture, _, x, y| {
                actions::show_context_menu(
                    &gv, &lib_ctx, &tk_ctx, &s_ctx,
                    &tc_ctx, &bs_ctx, &nav_ctx, x, y,
                );
                gesture.set_state(gtk::EventSequenceState::Claimed);
            });

            imp.grid_view.add_controller(gesture);
        }

        imp.toolbar_view.insert_action_group("album", Some(&action_group));

        // ── Load albums asynchronously ──────────────────────────────────
        reload_albums(&store, &library, &tokio, Rc::clone(&sort_order));

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

        assert!(imp.store.set(store).is_ok());
        assert!(imp.sort_order.set(sort_order).is_ok());
    }

    pub fn reload(&self) {
        let imp = self.imp();
        if let (Some(store), Some(library), Some(tokio), Some(sort_order)) =
            (imp.store.get(), imp.library.get(), imp.tokio.get(), imp.sort_order.get())
        {
            reload_albums(store, library, tokio, Rc::clone(sort_order));
        }
    }
}

// ── Free functions ───────────────────────────────────────────────────────────

/// Build the overflow menu model with sort radio actions.
fn build_sort_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let sort_section = gio::Menu::new();
    sort_section.append(Some(&gettext("Most Recent Photo")), Some("album.sort(uint32 0)"));
    sort_section.append(Some(&gettext("Name (A–Z)")), Some("album.sort(uint32 1)"));
    sort_section.append(Some(&gettext("Date Created")), Some("album.sort(uint32 2)"));
    menu.append_section(Some(&gettext("Sort by")), &sort_section);

    let select_section = gio::Menu::new();
    select_section.append(Some(&gettext("Select Albums")), Some("album.select"));
    menu.append_section(None, &select_section);

    menu
}

/// Sort the store in-place by the given sort order.
fn sort_store(store: &gio::ListStore, order: u32) {
    store.sort(|a, b| {
        let a = a.downcast_ref::<AlbumItemObject>().expect("store holds AlbumItemObject").album();
        let b = b.downcast_ref::<AlbumItemObject>().expect("store holds AlbumItemObject").album();
        sort_albums(a, b, order)
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
