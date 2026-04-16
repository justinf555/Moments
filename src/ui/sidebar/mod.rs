pub mod route;

use std::cell::Cell;
use std::cell::RefCell;

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{gio, glib, subclass::prelude::*};
use tracing::debug;

use route::ROUTES;

mod imp {
    use super::*;
    use std::cell::OnceCell;
    use std::rc::Rc;

    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/sidebar/sidebar.ui")]
    pub struct MomentsSidebar {
        #[template_child]
        pub sidebar: TemplateChild<adw::Sidebar>,
        #[template_child]
        pub toolbar_view: TemplateChild<adw::ToolbarView>,

        pub route_section: OnceCell<adw::SidebarSection>,
        pub people_item: OnceCell<adw::SidebarItem>,
        /// Route IDs in display order (matches sidebar item indices).
        pub active_routes: RefCell<Vec<&'static str>>,
        pub pinned_section: OnceCell<adw::SidebarSection>,
        /// Underlying album store — must be kept alive for the filter to work.
        pub pinned_store: OnceCell<gio::ListStore>,
        /// Filter for pinned albums — stored so it can be invalidated.
        pub pinned_filter: OnceCell<gtk::CustomFilter>,
        /// Filtered model of pinned albums — drives the pinned sidebar section.
        pub pinned_model: OnceCell<gtk::FilterListModel>,
        pub trash_badge: OnceCell<gtk::Label>,
        pub trash_count: Cell<u32>,

        /// Keeps the event bus subscription alive for this sidebar's lifetime.
        pub _subscription: RefCell<Option<crate::event_bus::Subscription>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MomentsSidebar {
        const NAME: &'static str = "MomentsSidebar";
        type Type = super::MomentsSidebar;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            // Ensure ActivityIndicator type is registered before the
            // template is bound, so the blueprint can instantiate it.
            crate::ui::widgets::ActivityIndicator::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl MomentsSidebar {
        pub fn pinned_section(&self) -> &adw::SidebarSection {
            self.pinned_section
                .get()
                .expect("pinned_section not initialized")
        }
    }

    impl ObjectImpl for MomentsSidebar {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            // Append dynamic sections to the template sidebar.
            self.sidebar.append(self.build_route_section());
            self.build_pinned_section(&obj);
        }
    }

    impl MomentsSidebar {
        fn build_route_section(&self) -> adw::SidebarSection {
            let section = adw::SidebarSection::new();
            let mut active = Vec::new();

            for route in ROUTES.iter() {
                let mut builder = adw::SidebarItem::builder()
                    .title(gettext(route.label))
                    .icon_name(route.icon);

                if route.id == "trash" {
                    let badge = gtk::Label::new(None);
                    badge.add_css_class("sidebar-badge");
                    badge.set_visible(false);
                    builder = builder.suffix(&badge);
                    let _ = self.trash_badge.set(badge);
                }

                let item = builder.build();

                if route.id == "people" {
                    let _ = self.people_item.set(item.clone());
                }

                section.append(item);
                active.push(route.id);
            }

            *self.active_routes.borrow_mut() = active;
            let _ = self.route_section.set(section.clone());

            section
        }

        fn build_pinned_section(&self, obj: &<Self as ObjectSubclass>::Type) {
            let pinned_section = adw::SidebarSection::new();
            pinned_section.set_title(Some(&gettext("Pinned")));

            let unpin_menu = gio::Menu::new();
            unpin_menu.append(Some(&gettext("Unpin from Sidebar")), Some("sidebar.unpin"));
            pinned_section.set_menu_model(Some(&unpin_menu));

            self.sidebar.append(pinned_section.clone());
            let _ = self.pinned_section.set(pinned_section.clone());

            // Build the filtered model — populated later in setup_pinned_albums.
            let filter = gtk::CustomFilter::new(|obj| {
                obj.downcast_ref::<crate::client::AlbumItemObject>()
                    .is_some_and(|item| item.pinned())
            });
            let pinned_model =
                gtk::FilterListModel::new(None::<gio::ListModel>, Some(filter.clone()));
            let _ = self.pinned_filter.set(filter);
            let _ = self.pinned_model.set(pinned_model);

            // Unpin action — resolves album from pinned model index.
            let menu_target_index: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
            {
                let mti = Rc::clone(&menu_target_index);
                let ps = pinned_section.clone();
                self.sidebar.connect_setup_menu(move |_, item| {
                    if let Some(item) = item {
                        let n = ps.items().n_items();
                        for i in 0..n {
                            if let Some(pinned_item) = ps.item(i) {
                                if pinned_item == *item {
                                    mti.set(Some(i));
                                    return;
                                }
                            }
                        }
                    }
                    mti.set(None);
                });
            }

            let unpin_action = gio::SimpleAction::new("unpin", None);
            {
                let mti = Rc::clone(&menu_target_index);
                let obj_weak = obj.downgrade();
                unpin_action.connect_activate(move |_, _| {
                    let Some(index) = mti.get() else { return };
                    let Some(sidebar) = obj_weak.upgrade() else {
                        return;
                    };
                    let Some(pinned_model) = sidebar.imp().pinned_model.get() else {
                        return;
                    };
                    if let Some(obj) = pinned_model
                        .item(index)
                        .and_then(|o| o.downcast::<crate::client::AlbumItemObject>().ok())
                    {
                        let album_client = crate::application::MomentsApplication::default()
                            .album_client_v2()
                            .expect("album client v2 available");
                        album_client
                            .unpin_album(crate::library::album::AlbumId::from_raw(obj.id()));
                    }
                });
            }

            let sidebar_action_group = gio::SimpleActionGroup::new();
            sidebar_action_group.add_action(&unpin_action);
            self.toolbar_view
                .insert_action_group("sidebar", Some(&sidebar_action_group));
        }
    }

    impl WidgetImpl for MomentsSidebar {
        fn realize(&self) {
            self.parent_realize();

            let weak = self.obj().downgrade();
            let sub = crate::event_bus::subscribe(move |event| {
                let Some(sidebar) = weak.upgrade() else {
                    return;
                };
                match event {
                    crate::app_event::AppEvent::Trashed { ids } => {
                        sidebar.adjust_trash_count(ids.len() as i32);
                    }
                    crate::app_event::AppEvent::Restored { ids } => {
                        sidebar.adjust_trash_count(-(ids.len() as i32));
                    }
                    crate::app_event::AppEvent::Deleted { ids } => {
                        sidebar.adjust_trash_count(-(ids.len() as i32));
                    }
                    _ => {}
                }
            });
            *self._subscription.borrow_mut() = Some(sub);
        }

        fn unrealize(&self) {
            self._subscription.borrow_mut().take();
            self.parent_unrealize();
        }
    }
    impl adw::subclass::prelude::NavigationPageImpl for MomentsSidebar {}
}

glib::wrapper! {
    pub struct MomentsSidebar(ObjectSubclass<imp::MomentsSidebar>)
        @extends gtk::Widget, adw::NavigationPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for MomentsSidebar {
    fn default() -> Self {
        Self::new()
    }
}

impl MomentsSidebar {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Connect a callback that fires when the user activates a sidebar item.
    ///
    /// System routes (index 0-5) map to `ROUTES[index].id`.
    /// Pinned album items (index 6+) map to `"album:{album_id}"`.
    pub fn connect_route_selected<F: Fn(&str) + 'static>(&self, f: F) {
        let sidebar = self.imp().sidebar.clone();
        let weak = self.downgrade();
        sidebar.connect_activated(move |_, index| {
            let Some(sb) = weak.upgrade() else { return };
            let active = sb.imp().active_routes.borrow();
            let system_count = active.len() as u32;
            if index < system_count {
                if let Some(route_id) = active.get(index as usize) {
                    debug!(route = %route_id, "sidebar route selected");
                    f(route_id);
                }
            } else {
                // Pinned album item — resolve from filtered model.
                let pinned_index = (index - system_count) as usize;
                if let Some(pinned_model) = sb.imp().pinned_model.get() {
                    if let Some(obj) = pinned_model
                        .item(pinned_index as u32)
                        .and_then(|o| o.downcast::<crate::client::AlbumItemObject>().ok())
                    {
                        let route = format!("album:{}", obj.id());
                        debug!(route = %route, "pinned album selected");
                        f(&route);
                    }
                }
            }
        });
    }

    /// Hide the People sidebar item (used for Local backend which has no face detection).
    pub fn hide_people(&self) {
        let imp = self.imp();
        if let (Some(section), Some(item)) = (imp.route_section.get(), imp.people_item.get()) {
            section.remove(item);
            imp.active_routes.borrow_mut().retain(|id| *id != "people");
            debug!("people route hidden (local backend)");
        }
    }

    /// Pre-select the first item (Photos) so the shell always has an active route.
    pub fn select_first(&self) {
        self.imp().sidebar.set_selected(0);
    }

    /// Set the initial trash count (called once at startup after querying the library).
    pub fn set_trash_count(&self, count: u32) {
        let imp = self.imp();
        imp.trash_count.set(count);
        self.update_trash_badge();
    }

    /// Adjust the trash count by a signed delta.
    fn adjust_trash_count(&self, delta: i32) {
        let imp = self.imp();
        let current = imp.trash_count.get() as i32;
        let new_count = (current + delta).max(0) as u32;
        imp.trash_count.set(new_count);
        self.update_trash_badge();
    }

    // ── Pinned albums ───────────────────────────────────────────────

    /// Wire the pinned album section to the album client's model.
    ///
    /// Creates a filtered view of pinned albums and reactively syncs
    /// sidebar items when albums are pinned or unpinned.
    pub fn setup_pinned_albums(&self) {
        let imp = self.imp();

        let album_client = crate::application::MomentsApplication::default()
            .album_client_v2()
            .expect("album client v2 available");

        let store = album_client.create_model();
        album_client.list_albums(&store);

        // Keep the store alive — the FilterListModel holds a weak-ish
        // reference and the store would be dropped otherwise.
        let _ = imp.pinned_store.set(store.clone());

        let Some(pinned_model) = imp.pinned_model.get() else {
            return;
        };
        pinned_model.set_model(Some(&store));

        // When items are added to the store, watch their `pinned` property
        // and invalidate the filter when it changes.
        let filter_weak = imp.pinned_filter.get().unwrap().downgrade();
        store.connect_items_changed(move |store, pos, _removed, added| {
            let Some(filter) = filter_weak.upgrade() else {
                return;
            };
            for i in pos..pos + added {
                if let Some(obj) = store
                    .item(i)
                    .and_then(|o| o.downcast::<crate::client::AlbumItemObject>().ok())
                {
                    let f = filter.clone();
                    obj.connect_notify_local(Some("pinned"), move |_, _| {
                        debug!("pinned property changed — invalidating filter");
                        f.changed(gtk::FilterChange::Different);
                    });
                }
            }
        });

        // React to filter model changes — rebuild sidebar items.
        // Initial rebuild happens when list_albums completes and splices data.
        let weak = self.downgrade();
        pinned_model.connect_items_changed(move |model, pos, removed, added| {
            debug!(
                pos,
                removed,
                added,
                total = model.n_items(),
                "pinned filter items_changed"
            );
            if let Some(sidebar) = weak.upgrade() {
                sidebar.rebuild_pinned_items();
            }
        });
    }

    /// Rebuild the pinned sidebar section from the filtered model.
    fn rebuild_pinned_items(&self) {
        debug!("rebuild_pinned_items called");
        let imp = self.imp();
        let section = imp.pinned_section();
        let Some(pinned_model) = imp.pinned_model.get() else {
            return;
        };

        section.remove_all();

        for i in 0..pinned_model.n_items() {
            if let Some(obj) = pinned_model
                .item(i)
                .and_then(|o| o.downcast::<crate::client::AlbumItemObject>().ok())
            {
                let item = adw::SidebarItem::builder()
                    .title(obj.name())
                    .icon_name("folder-symbolic")
                    .build();
                section.append(item);
            }
        }

        debug!(count = pinned_model.n_items(), "pinned albums rebuilt");
    }

    /// Update the Trash badge with the current count.
    fn update_trash_badge(&self) {
        let imp = self.imp();
        if let Some(badge) = imp.trash_badge.get() {
            let count = imp.trash_count.get();
            if count > 0 {
                let label =
                    ngettext("{} item", "{} items", count).replace("{}", &count.to_string());
                badge.set_label(&label);
                badge.set_visible(true);
            } else {
                badge.set_visible(false);
            }
        }
    }
}
