use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::debug;

use crate::client::AlbumClient;
use crate::library::album::AlbumId;
use crate::ui::album_dialogs;
use crate::ui::photo_grid::texture_cache::TextureCache;

mod actions;
pub mod card;
pub mod factory;
mod selection;

use crate::client::AlbumItemObject;

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
    #[template(resource = "/io/github/justinf555/Moments/ui/album_grid/album_grid.ui")]
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
        pub album_client: OnceCell<AlbumClient>,

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
    impl WidgetImpl for AlbumGridView {
        fn realize(&self) {
            self.parent_realize();

            let (Some(store), Some(album_client)) = (self.store.get(), self.album_client.get())
            else {
                tracing::warn!("AlbumGridView realized before setup()");
                return;
            };

            album_client.populate(store);
        }
    }
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
        settings: gio::Settings,
        texture_cache: Rc<TextureCache>,
        bus_sender: crate::event_bus::EventSender,
    ) {
        let imp = self.imp();

        let album_client = crate::application::MomentsApplication::default()
            .album_client()
            .expect("album client available after library load");
        assert!(
            imp.album_client.set(album_client.clone()).is_ok(),
            "setup called twice"
        );

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
        let store = album_client.create_model();
        let multi_selection = gtk::MultiSelection::new(Some(store.clone()));

        imp.grid_view.set_model(Some(&multi_selection));
        imp.grid_view.set_factory(Some(&factory::build_factory(
            album_client.clone(),
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
                let Some(value) = param.and_then(|v| v.get::<u32>()) else {
                    return;
                };
                action.set_state(&value.to_variant());
                so.set(value);
                s.set_uint("album-sort-order", value).ok();
                sort_store(&st, value);
                debug!(sort_order = value, "album sort changed");
            });
        }

        // ── Wire "New Album" buttons ────────────────────────────────────
        {
            let ac = album_client.clone();
            let connect_create = move |btn: &gtk::Button| {
                let ac = ac.clone();
                album_dialogs::show_create_album_dialog(btn, move |name| {
                    ac.create_album(name);
                });
            };

            let cb = connect_create.clone();
            imp.new_album_btn.connect_clicked(move |btn| cb(btn));
            imp.empty_new_btn
                .connect_clicked(move |btn| connect_create(btn));
        }

        // ── Wire item activation (click → open album photo grid) ────────
        {
            let s = settings.clone();
            let tc = Rc::clone(&texture_cache);
            let bs = bus_sender.clone();
            let st = store.clone();
            let nav = imp.nav_view.clone();

            imp.grid_view.connect_activate(move |_, position| {
                let Some(obj) = st.item(position) else { return };
                let Some(item) = obj.downcast_ref::<AlbumItemObject>() else {
                    return;
                };
                let album_id_str = item.id();
                let album_name = item.name();
                let album_id = AlbumId::from_raw(album_id_str.clone());

                debug!(album_id = %album_id_str, name = %album_name, "album activated");

                actions::open_album_drilldown(&s, &tc, &bs, &nav, album_id, &album_name);
            });
        }

        // ── Right-click context menu ────────────────────────────────────
        {
            let gesture = gtk::GestureClick::new();
            gesture.set_button(3);

            let gv = imp.grid_view.clone();
            let nav_ctx = imp.nav_view.clone();
            let s_ctx = settings.clone();
            let tc_ctx = Rc::clone(&texture_cache);
            let bs_ctx = bus_sender.clone();

            gesture.connect_pressed(move |gesture, _, x, y| {
                actions::show_context_menu(&gv, &s_ctx, &tc_ctx, &bs_ctx, &nav_ctx, x, y);
                gesture.set_state(gtk::EventSequenceState::Claimed);
            });

            imp.grid_view.add_controller(gesture);
        }

        imp.toolbar_view
            .insert_action_group("album", Some(&action_group));

        assert!(imp.store.set(store).is_ok());
        assert!(imp.sort_order.set(sort_order).is_ok());
    }

    pub fn reload(&self) {
        let imp = self.imp();
        if let (Some(store), Some(album_client)) = (imp.store.get(), imp.album_client.get()) {
            album_client.populate(store);
        }
    }
}

// ── Free functions ───────────────────────────────────────────────────────────

/// Build the overflow menu model with sort radio actions.
fn build_sort_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let sort_section = gio::Menu::new();
    sort_section.append(
        Some(&gettext("Most Recent Photo")),
        Some("album.sort(uint32 0)"),
    );
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
        let a = a
            .downcast_ref::<AlbumItemObject>()
            .expect("store holds AlbumItemObject");
        let b = b
            .downcast_ref::<AlbumItemObject>()
            .expect("store holds AlbumItemObject");
        sort_album_items(a, b, order)
    });
}

/// Compare two album items for sorting.
fn sort_album_items(a: &AlbumItemObject, b: &AlbumItemObject, order: u32) -> std::cmp::Ordering {
    match order {
        SORT_NAME => a.name().to_lowercase().cmp(&b.name().to_lowercase()),
        SORT_CREATED => b.created_at().cmp(&a.created_at()),
        _ => b.updated_at().cmp(&a.updated_at()),
    }
}
