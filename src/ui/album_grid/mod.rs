use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::debug;

use crate::client::AlbumClientV2;
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
        pub album_client: OnceCell<AlbumClientV2>,

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

            album_client.list_albums(store);
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
            .album_client_v2()
            .expect("album client v2 available after library load");
        assert!(
            imp.album_client.set(album_client.clone()).is_ok(),
            "setup called twice"
        );

        let sort_order = Rc::new(Cell::new(settings.uint("album-sort-order")));
        let selection_mode = Rc::new(Cell::new(false));
        let enter_selection = gio::SimpleAction::new("select", None);

        let store = album_client.create_model();
        let sort_model = gtk::SortListModel::new(Some(store.clone()), None::<gtk::Sorter>);
        let multi_selection = gtk::MultiSelection::new(Some(sort_model.clone()));

        let action_group = self.wire_sort(&settings, &sort_order, &sort_model);
        self.wire_grid(
            &multi_selection,
            &selection_mode,
            &enter_selection,
            &settings,
            &texture_cache,
            &bus_sender,
        );
        self.wire_selection(
            &enter_selection,
            &multi_selection,
            &store,
            &selection_mode,
            &action_group,
        );
        self.wire_empty_toggle(&store);
        self.wire_create_buttons(&album_client);
        self.wire_activation(&settings, &texture_cache, &bus_sender, &store);

        imp.toolbar_view
            .insert_action_group("album", Some(&action_group));

        assert!(imp.store.set(store).is_ok());
        assert!(imp.sort_order.set(sort_order).is_ok());
    }

    // ── Setup helpers (private) ──────────────────────────────────────────

    fn wire_sort(
        &self,
        settings: &gio::Settings,
        sort_order: &Rc<Cell<u32>>,
        sort_model: &gtk::SortListModel,
    ) -> gio::SimpleActionGroup {
        let imp = self.imp();

        let sort_menu = build_sort_menu();
        imp.menu_btn.set_menu_model(Some(&sort_menu));

        let sort_action = gio::SimpleAction::new_stateful(
            "sort",
            Some(&u32::static_variant_type()),
            &sort_order.get().to_variant(),
        );

        // Apply the initial sort order.
        sort_model.set_sorter(Some(&build_sorter(sort_order.get())));

        let so = Rc::clone(sort_order);
        let s = settings.clone();
        let sm = sort_model.clone();
        sort_action.connect_activate(move |action, param| {
            let Some(value) = param.and_then(|v| v.get::<u32>()) else {
                return;
            };
            action.set_state(&value.to_variant());
            so.set(value);
            s.set_uint("album-sort-order", value).ok();
            sm.set_sorter(Some(&build_sorter(value)));
            debug!(sort_order = value, "album sort changed");
        });

        let action_group = gio::SimpleActionGroup::new();
        action_group.add_action(&sort_action);
        action_group
    }

    fn wire_grid(
        &self,
        multi_selection: &gtk::MultiSelection,
        selection_mode: &Rc<Cell<bool>>,
        enter_selection: &gio::SimpleAction,
        settings: &gio::Settings,
        texture_cache: &Rc<TextureCache>,
        bus_sender: &crate::event_bus::EventSender,
    ) {
        let imp = self.imp();
        imp.grid_view.set_model(Some(multi_selection));
        imp.grid_view.set_factory(Some(&factory::build_factory(
            Rc::clone(selection_mode),
            multi_selection.clone(),
            enter_selection.clone(),
            settings.clone(),
            Rc::clone(texture_cache),
            bus_sender.clone(),
            imp.nav_view.clone(),
        )));
    }

    fn wire_selection(
        &self,
        enter_selection: &gio::SimpleAction,
        multi_selection: &gtk::MultiSelection,
        store: &gio::ListStore,
        selection_mode: &Rc<Cell<bool>>,
        action_group: &gio::SimpleActionGroup,
    ) {
        let imp = self.imp();

        selection::wire_selection_mode(&selection::SelectionConfig {
            enter_selection,
            header: &imp.header,
            new_album_btn: &imp.new_album_btn,
            menu_btn: &imp.menu_btn,
            cancel_btn: &imp.cancel_btn,
            action_bar: &imp.action_bar,
            grid_view: &imp.grid_view,
            multi_selection,
            store,
            selection_mode,
        });
        action_group.add_action(enter_selection);
    }

    fn wire_empty_toggle(&self, store: &gio::ListStore) {
        let stack = self.imp().content_stack.clone();
        store.connect_items_changed(move |store, _, _, _| {
            let target = if store.n_items() > 0 { "grid" } else { "empty" };
            stack.set_visible_child_name(target);
        });
    }

    fn wire_create_buttons(&self, album_client: &AlbumClientV2) {
        let imp = self.imp();
        let ac = album_client.clone();
        let connect_create = move |btn: &gtk::Button| {
            let ac = ac.clone();
            album_dialogs::show_create_album_dialog(btn, move |name| {
                ac.create_album(name, vec![]);
            });
        };

        let cb = connect_create.clone();
        imp.new_album_btn.connect_clicked(move |btn| cb(btn));
        imp.empty_new_btn
            .connect_clicked(move |btn| connect_create(btn));
    }

    fn wire_activation(
        &self,
        settings: &gio::Settings,
        texture_cache: &Rc<TextureCache>,
        bus_sender: &crate::event_bus::EventSender,
        store: &gio::ListStore,
    ) {
        let s = settings.clone();
        let tc = Rc::clone(texture_cache);
        let bs = bus_sender.clone();
        let st = store.clone();
        let nav = self.imp().nav_view.clone();

        self.imp().grid_view.connect_activate(move |_, position| {
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

/// Build a `CustomSorter` for the given sort order.
fn build_sorter(order: u32) -> gtk::CustomSorter {
    gtk::CustomSorter::new(move |a, b| {
        let a = a
            .downcast_ref::<AlbumItemObject>()
            .expect("store holds AlbumItemObject");
        let b = b
            .downcast_ref::<AlbumItemObject>()
            .expect("store holds AlbumItemObject");
        match order {
            SORT_NAME => a.name().to_lowercase().cmp(&b.name().to_lowercase()),
            SORT_CREATED => b.created_at().cmp(&a.created_at()),
            _ => b.updated_at().cmp(&a.updated_at()),
        }
        .into()
    })
}
