use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::instrument;

use crate::app_event::AppEvent;
use crate::client::MediaItemObject;
use crate::library::media::{MediaFilter, MediaType};
use crate::ui::video_viewer::VideoViewer;
use crate::ui::viewer::PhotoViewer;

pub mod action_bar;
pub mod actions;
pub mod cell;
pub mod factory;
pub mod texture_cache;

/// Available cell sizes (px), smallest to largest.
const ZOOM_SIZES: &[i32] = &[96, 128, 160, 200, 256, 320];
/// Default zoom level index (160 px).
const DEFAULT_ZOOM_INDEX: usize = 2;

// ── PhotoGrid (inner GObject — unchanged) ────────────────────────────────────

mod photo_grid_imp {
    use super::*;
    use std::cell::OnceCell;

    pub struct PhotoGrid {
        pub content_stack: OnceCell<gtk::Stack>,
        pub scrolled: OnceCell<gtk::ScrolledWindow>,
        pub grid_view: OnceCell<gtk::GridView>,
        pub empty_page: OnceCell<adw::StatusPage>,
        pub selection: RefCell<Option<gtk::MultiSelection>>,
        pub store: RefCell<Option<gio::ListStore>>,
        pub zoom_level: Cell<usize>,
        pub media_client: OnceCell<crate::client::MediaClient>,
        pub bus_sender: OnceCell<crate::event_bus::EventSender>,
        pub filter: RefCell<crate::library::media::MediaFilter>,
        pub texture_cache: OnceCell<Rc<super::texture_cache::TextureCache>>,
        /// Shared selection mode flag for the factory.
        pub selection_mode: Rc<Cell<bool>>,
        /// Enter-selection action for checkbox click → selection mode.
        pub enter_selection: RefCell<Option<gio::SimpleAction>>,
    }

    impl Default for PhotoGrid {
        fn default() -> Self {
            Self {
                content_stack: OnceCell::default(),
                scrolled: OnceCell::default(),
                grid_view: OnceCell::default(),
                empty_page: OnceCell::default(),
                selection: RefCell::default(),
                store: RefCell::default(),
                zoom_level: Cell::new(DEFAULT_ZOOM_INDEX),
                media_client: OnceCell::default(),
                bus_sender: OnceCell::default(),
                filter: RefCell::new(crate::library::media::MediaFilter::All),
                texture_cache: OnceCell::default(),
                selection_mode: Rc::new(Cell::new(false)),
                enter_selection: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PhotoGrid {
        const NAME: &'static str = "MomentsPhotoGrid";
        type Type = super::PhotoGrid;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("photo-grid");
        }
    }

    impl PhotoGrid {
        pub fn grid_view(&self) -> &gtk::GridView {
            self.grid_view.get().expect("grid_view not initialized")
        }
        pub fn scrolled(&self) -> &gtk::ScrolledWindow {
            self.scrolled.get().expect("scrolled not initialized")
        }
        pub fn empty_page(&self) -> &adw::StatusPage {
            self.empty_page.get().expect("empty_page not initialized")
        }
        pub fn content_stack(&self) -> &gtk::Stack {
            self.content_stack
                .get()
                .expect("content_stack not initialized")
        }
        pub fn media_client(&self) -> &crate::client::MediaClient {
            self.media_client
                .get()
                .expect("media_client not initialized")
        }
        pub fn bus_sender(&self) -> &crate::event_bus::EventSender {
            self.bus_sender.get().expect("bus_sender not initialized")
        }
        pub fn texture_cache(&self) -> &Rc<super::texture_cache::TextureCache> {
            self.texture_cache
                .get()
                .expect("texture_cache not initialized")
        }
    }

    impl ObjectImpl for PhotoGrid {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            let grid_view =
                gtk::GridView::new(None::<gtk::NoSelection>, None::<gtk::SignalListItemFactory>);
            grid_view.set_min_columns(2);
            grid_view.set_max_columns(20);

            let scrolled = gtk::ScrolledWindow::new();
            scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
            scrolled.set_vexpand(true);
            scrolled.set_child(Some(&grid_view));

            let empty_page = adw::StatusPage::builder()
                .icon_name("folder-pictures-symbolic")
                .title("No photos yet")
                .description("Import photos to get started")
                .vexpand(true)
                .build();

            let stack = gtk::Stack::new();
            stack.set_transition_type(gtk::StackTransitionType::Crossfade);
            stack.add_named(&scrolled, Some("grid"));
            stack.add_named(&empty_page, Some("empty"));
            stack.set_visible_child_name("empty");
            stack.set_parent(&*obj);

            self.grid_view.set(grid_view).unwrap();
            self.scrolled.set(scrolled).unwrap();
            self.empty_page.set(empty_page).unwrap();
            self.content_stack.set(stack).unwrap();
        }

        fn dispose(&self) {
            if let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for PhotoGrid {}
}

glib::wrapper! {
    pub struct PhotoGrid(ObjectSubclass<photo_grid_imp::PhotoGrid>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PhotoGrid {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Current cell size in pixels based on the active zoom level.
    pub fn current_cell_size(&self) -> i32 {
        ZOOM_SIZES[self.imp().zoom_level.get()]
    }

    /// Increase thumbnail size. Returns `true` if there is still room to zoom in.
    pub fn zoom_in(&self) -> bool {
        let imp = self.imp();
        let level = imp.zoom_level.get();
        if level + 1 < ZOOM_SIZES.len() {
            imp.zoom_level.set(level + 1);
            self.apply_zoom();
        }
        imp.zoom_level.get() + 1 < ZOOM_SIZES.len()
    }

    /// Decrease thumbnail size. Returns `true` if there is still room to zoom out.
    pub fn zoom_out(&self) -> bool {
        let imp = self.imp();
        let level = imp.zoom_level.get();
        if level > 0 {
            imp.zoom_level.set(level - 1);
            self.apply_zoom();
        }
        imp.zoom_level.get() > 0
    }

    /// Set the zoom level directly (e.g. from a saved setting).
    pub fn set_zoom_level(&self, level: usize) {
        let clamped = level.min(ZOOM_SIZES.len() - 1);
        self.imp().zoom_level.set(clamped);
    }

    /// Current zoom level index.
    pub fn zoom_level(&self) -> usize {
        self.imp().zoom_level.get()
    }

    /// Rebuild the cell factory with the current zoom size.
    fn apply_zoom(&self) {
        let imp = self.imp();
        let grid_view = imp.grid_view();
        let media_client = imp.media_client().clone();
        let bus_sender = imp.bus_sender().clone();
        let filter = imp.filter.borrow().clone();
        let cache = imp.texture_cache().clone();
        let sm = Rc::clone(&imp.selection_mode);
        let selection = imp.selection.borrow().clone().unwrap();
        let enter = imp.enter_selection.borrow().clone().unwrap();
        grid_view.set_factory(Some(&factory::build_factory(
            self.current_cell_size(),
            media_client,
            bus_sender,
            filter,
            cache,
            sm,
            selection,
            enter,
        )));
    }

    /// Attach a media list store to the grid.
    #[allow(clippy::too_many_arguments)]
    #[instrument(skip_all)]
    pub fn set_store(
        &self,
        store: gio::ListStore,
        media_client: crate::client::MediaClient,
        bus_sender: crate::event_bus::EventSender,
        filter: crate::library::media::MediaFilter,
        cache: Rc<texture_cache::TextureCache>,
        on_activate: impl Fn(Vec<MediaItemObject>, usize) + 'static,
    ) {
        let imp = self.imp();
        let _ = imp.media_client.set(media_client.clone());
        let _ = imp.bus_sender.set(bus_sender.clone());
        let _ = imp.texture_cache.set(Rc::clone(&cache));
        *imp.filter.borrow_mut() = filter.clone();

        let grid_view = imp.grid_view();
        let scrolled = imp.scrolled();

        let selection = gtk::MultiSelection::new(Some(store.clone()));
        grid_view.set_model(Some(&selection));
        *imp.selection.borrow_mut() = Some(selection.clone());

        let sm = Rc::clone(&imp.selection_mode);
        let enter = imp.enter_selection.borrow().clone().unwrap();
        grid_view.set_factory(Some(&factory::build_factory(
            self.current_cell_size(),
            media_client.clone(),
            bus_sender,
            filter.clone(),
            cache,
            sm,
            selection.clone(),
            enter,
        )));

        // Configure empty state message based on filter.
        let empty_page = imp.empty_page();
        let stack = imp.content_stack();
        set_empty_state_for_filter(empty_page, &filter);

        // Toggle empty ↔ grid based on store count.
        {
            let stack = stack.clone();
            let store_ref = store.clone();
            store.connect_items_changed(move |_, _, _, _| {
                let name = if store_ref.n_items() == 0 {
                    "empty"
                } else {
                    "grid"
                };
                stack.set_visible_child_name(name);
            });
        }

        // Infinite scroll — load more when nearing the bottom.
        {
            let mc = media_client.clone();
            let store_scroll = store.clone();
            let adj = scrolled.vadjustment();
            adj.connect_value_changed(move |adj| {
                let visible_end = adj.value() + adj.page_size();
                let trigger_point = adj.upper() * 0.75;
                if visible_end >= trigger_point {
                    mc.load_more(&store_scroll);
                }
            });
        }

        let selection_ref = selection.clone();
        grid_view.connect_activate(move |_, position| {
            let n = selection_ref.n_items();
            let items: Vec<MediaItemObject> = (0..n)
                .filter_map(|i| {
                    selection_ref
                        .item(i)
                        .and_then(|obj| obj.downcast::<MediaItemObject>().ok())
                })
                .collect();
            on_activate(items, position as usize);
        });

        *imp.store.borrow_mut() = Some(store);
    }
}

impl Default for PhotoGrid {
    fn default() -> Self {
        Self::new()
    }
}

// ── PhotoGridView (GObject subclass) ─────────────────────────────────────────

mod view_imp {
    use super::*;
    use std::cell::OnceCell;

    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/photo_grid/photo_grid.ui")]
    pub struct PhotoGridView {
        #[template_child]
        pub nav_view: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub header: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub zoom_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub cancel_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub content_menu_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub photo_grid: TemplateChild<PhotoGrid>,
        #[template_child]
        pub action_bar: TemplateChild<gtk::ActionBar>,
        #[template_child]
        pub restore_all_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub empty_trash_btn: TemplateChild<gtk::Button>,

        // Service dependencies
        pub bus_sender: OnceCell<crate::event_bus::EventSender>,
        pub texture_cache: OnceCell<Rc<texture_cache::TextureCache>>,

        // Viewers (reused across activations)
        pub photo_viewer: OnceCell<PhotoViewer>,
        pub video_viewer: OnceCell<VideoViewer>,

        // Selection mode state
        pub selection_mode: Rc<Cell<bool>>,
        pub exit_selection: OnceCell<gio::SimpleAction>,
        pub selection_title: OnceCell<gtk::Label>,
        pub bar_box: OnceCell<gtk::Box>,
        pub fav_btn: RefCell<Option<gtk::Button>>,
        /// Keeps the event bus subscription alive for this view's lifetime.
        pub _subscription: RefCell<Option<crate::event_bus::Subscription>>,
    }

    impl PhotoGridView {
        pub fn bus_sender(&self) -> &crate::event_bus::EventSender {
            self.bus_sender.get().expect("bus_sender not initialized")
        }
        pub fn texture_cache(&self) -> &Rc<texture_cache::TextureCache> {
            self.texture_cache
                .get()
                .expect("texture_cache not initialized")
        }
        pub fn photo_viewer(&self) -> &PhotoViewer {
            self.photo_viewer
                .get()
                .expect("photo_viewer not initialized")
        }
        pub fn video_viewer(&self) -> &VideoViewer {
            self.video_viewer
                .get()
                .expect("video_viewer not initialized")
        }
        pub fn exit_selection(&self) -> &gio::SimpleAction {
            self.exit_selection
                .get()
                .expect("exit_selection not initialized")
        }
        pub fn selection_title(&self) -> &gtk::Label {
            self.selection_title
                .get()
                .expect("selection_title not initialized")
        }
        pub fn bar_box(&self) -> &gtk::Box {
            self.bar_box.get().expect("bar_box not initialized")
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PhotoGridView {
        const NAME: &'static str = "MomentsPhotoGridView";
        type Type = super::PhotoGridView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            // Ensure PhotoGrid type is registered before template parsing.
            PhotoGrid::ensure_type();
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PhotoGridView {
        fn dispose(&self) {
            self.dispose_template();
        }
    }
    impl WidgetImpl for PhotoGridView {
        fn realize(&self) {
            self.parent_realize();

            // Trigger initial page load via MediaClient.
            if let (Some(store), Some(mc)) = (
                self.photo_grid.imp().store.borrow().as_ref(),
                self.photo_grid.imp().media_client.get(),
            ) {
                mc.populate(store);
            }

            // Subscribe for exit-selection on result events.
            if let Some(exit) = self.exit_selection.get() {
                let exit = exit.clone();
                let sub = crate::event_bus::subscribe(move |event| match event {
                    crate::app_event::AppEvent::Trashed { .. }
                    | crate::app_event::AppEvent::Deleted { .. }
                    | crate::app_event::AppEvent::Restored { .. }
                    | crate::app_event::AppEvent::AlbumMediaChanged { .. }
                    | crate::app_event::AppEvent::FavoriteChanged { .. } => {
                        exit.activate(None);
                    }
                    _ => {}
                });
                *self._subscription.borrow_mut() = Some(sub);
            }
        }

        fn unrealize(&self) {
            self._subscription.borrow_mut().take();
            self.parent_unrealize();
        }
    }
}

glib::wrapper! {
    pub struct PhotoGridView(ObjectSubclass<view_imp::PhotoGridView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PhotoGridView {
    fn default() -> Self {
        Self::new()
    }
}

impl PhotoGridView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn setup(
        &self,
        settings: gio::Settings,
        texture_cache: Rc<texture_cache::TextureCache>,
        bus_sender: crate::event_bus::EventSender,
    ) {
        let imp = self.imp();
        assert!(
            imp.bus_sender.set(bus_sender.clone()).is_ok(),
            "setup called twice"
        );
        assert!(
            imp.texture_cache.set(Rc::clone(&texture_cache)).is_ok(),
            "setup called twice"
        );

        // Viewers.
        let photo_viewer = PhotoViewer::new();
        photo_viewer.setup(bus_sender.clone());
        let video_viewer = VideoViewer::new();
        video_viewer.setup(bus_sender.clone());
        assert!(imp.photo_viewer.set(photo_viewer).is_ok());
        assert!(imp.video_viewer.set(video_viewer).is_ok());

        // Zoom level from settings.
        imp.photo_grid
            .set_zoom_level(settings.uint("zoom-level") as usize);

        // Stop zoom button clicks from propagating to HeaderBar drag gesture.
        let controller = gtk::EventControllerLegacy::new();
        controller.connect_event(|_, event| {
            use gtk::gdk::EventType;
            match event.event_type() {
                EventType::ButtonPress | EventType::ButtonRelease => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            }
        });
        imp.zoom_box.add_controller(controller);

        // Content overflow menu.
        let content_menu = gio::Menu::new();
        let content_section = gio::Menu::new();
        content_section.append(Some("_Select"), Some("view.enter-selection"));
        content_menu.append_section(None, &content_section);
        imp.content_menu_btn.set_menu_model(Some(&content_menu));

        // Selection title label.
        let selection_title = gtk::Label::new(Some("0 selected"));
        selection_title.add_css_class("heading");
        selection_title.set_visible(false);
        assert!(imp.selection_title.set(selection_title).is_ok());

        // Action bar center widget.
        let bar_box = gtk::Box::new(gtk::Orientation::Horizontal, 24);
        bar_box.set_halign(gtk::Align::Center);
        imp.action_bar.set_center_widget(Some(&bar_box));
        assert!(imp.bar_box.set(bar_box).is_ok());

        // ── Zoom actions ─────────────────────────────────────────────────
        let action_group = gio::SimpleActionGroup::new();

        let zoom_in_action = gio::SimpleAction::new("zoom-in", None);
        let zoom_out_action = gio::SimpleAction::new("zoom-out", None);

        zoom_in_action.set_enabled(imp.photo_grid.imp().zoom_level.get() + 1 < ZOOM_SIZES.len());
        zoom_out_action.set_enabled(imp.photo_grid.imp().zoom_level.get() > 0);

        {
            let grid = imp.photo_grid.clone();
            let zi = zoom_in_action.clone();
            let zo = zoom_out_action.clone();
            let s = settings.clone();
            zoom_in_action.connect_activate(move |_, _| {
                let can_zoom_more = grid.zoom_in();
                zi.set_enabled(can_zoom_more);
                zo.set_enabled(true);
                let _ = s.set_uint("zoom-level", grid.zoom_level() as u32);
            });
        }
        {
            let grid = imp.photo_grid.clone();
            let zi = zoom_in_action.clone();
            let zo = zoom_out_action.clone();
            zoom_out_action.connect_activate(move |_, _| {
                let can_zoom_more = grid.zoom_out();
                zo.set_enabled(can_zoom_more);
                zi.set_enabled(true);
                let _ = settings.set_uint("zoom-level", grid.zoom_level() as u32);
            });
        }

        action_group.add_action(&zoom_in_action);
        action_group.add_action(&zoom_out_action);

        // ── Selection mode actions ───────────────────────────────────────
        let selection_mode = Rc::clone(&imp.selection_mode);

        let enter_selection = gio::SimpleAction::new("enter-selection", None);
        {
            let sm = Rc::clone(&selection_mode);
            let weak = self.downgrade();
            enter_selection.connect_activate(move |_, _| {
                let Some(view) = weak.upgrade() else { return };
                let imp = view.imp();
                sm.set(true);
                imp.zoom_box.set_visible(false);
                imp.content_menu_btn.set_visible(false);
                imp.cancel_btn.set_visible(true);
                let title = imp.selection_title();
                title.set_visible(true);
                imp.header.set_title_widget(Some(title));
                imp.action_bar.set_revealed(true);

                let grid_view = imp.photo_grid.imp().grid_view();
                grid_view.add_css_class("selection-active");
                let mut child = grid_view.first_child();
                while let Some(c) = child {
                    if let Some(cell) = c
                        .first_child()
                        .and_then(|w| w.downcast::<cell::PhotoGridCell>().ok())
                    {
                        cell.set_selection_mode(true);
                    }
                    child = c.next_sibling();
                }
            });
        }

        let exit_selection = gio::SimpleAction::new("exit-selection", None);
        {
            let sm = Rc::clone(&selection_mode);
            let weak = self.downgrade();
            exit_selection.connect_activate(move |_, _| {
                let Some(view) = weak.upgrade() else { return };
                let imp = view.imp();
                sm.set(false);
                imp.zoom_box.set_visible(true);
                imp.content_menu_btn.set_visible(true);
                imp.cancel_btn.set_visible(false);
                imp.selection_title().set_visible(false);
                imp.header.set_title_widget(None::<&gtk::Widget>);
                imp.action_bar.set_revealed(false);

                if let Some(ref sel) = *imp.photo_grid.imp().selection.borrow() {
                    sel.unselect_all();
                }

                let grid_view = imp.photo_grid.imp().grid_view();
                grid_view.remove_css_class("selection-active");
                let mut child = grid_view.first_child();
                while let Some(c) = child {
                    if let Some(cell) = c
                        .first_child()
                        .and_then(|w| w.downcast::<cell::PhotoGridCell>().ok())
                    {
                        cell.set_selection_mode(false);
                    }
                    child = c.next_sibling();
                }
            });
        }

        // Cancel button.
        {
            let exit = exit_selection.clone();
            imp.cancel_btn.connect_clicked(move |_| {
                exit.activate(None);
            });
        }

        // Escape key exits selection mode.
        {
            let grid_view = imp.photo_grid.imp().grid_view();
            let exit = exit_selection.clone();
            let sm = Rc::clone(&selection_mode);
            let key_ctrl = gtk::EventControllerKey::new();
            key_ctrl.connect_key_pressed(move |_, keyval, _, _| {
                if keyval == gtk::gdk::Key::Escape && sm.get() {
                    exit.activate(None);
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            grid_view.add_controller(key_ctrl);
        }

        action_group.add_action(&enter_selection);
        action_group.add_action(&exit_selection.clone());

        *imp.photo_grid.imp().enter_selection.borrow_mut() = Some(enter_selection);
        assert!(imp.exit_selection.set(exit_selection).is_ok());

        // Install view actions on the nav_view.
        imp.nav_view
            .insert_action_group("view", Some(&action_group));
    }

    pub fn set_store(&self, store: gio::ListStore, filter: MediaFilter) {
        let imp = self.imp();
        let media_client = crate::application::MomentsApplication::default()
            .media_client()
            .expect("media client available");
        let bus_sender = imp.bus_sender().clone();
        let texture_cache = Rc::clone(imp.texture_cache());

        imp.photo_grid.set_store(
            store.clone(),
            media_client,
            bus_sender.clone(),
            filter.clone(),
            Rc::clone(&texture_cache),
            {
                let nav_view = imp.nav_view.clone();
                let photo_viewer = imp.photo_viewer().clone();
                let video_viewer = imp.video_viewer().clone();
                move |items: Vec<MediaItemObject>, index: usize| {
                    let media_type = items
                        .get(index)
                        .map(|obj| obj.item().media_type)
                        .unwrap_or(MediaType::Image);

                    let filename = items
                        .get(index)
                        .map(|obj| obj.item().original_filename.clone())
                        .unwrap_or_default();

                    tracing::debug!(index, ?media_type, %filename, "grid item activated");

                    let (tag, nav_page): (&str, adw::NavigationPage) =
                        if media_type == MediaType::Video {
                            video_viewer.show(items, index);
                            ("video-viewer", video_viewer.clone().upcast())
                        } else {
                            photo_viewer.show(items, index);
                            ("viewer", photo_viewer.clone().upcast())
                        };

                    let visible_tag = nav_view
                        .visible_page()
                        .and_then(|p| p.tag())
                        .unwrap_or_default();
                    tracing::debug!(target_tag = tag, %visible_tag, "pushing viewer page");
                    if visible_tag != tag {
                        nav_view.push(&nav_page);
                    }
                }
            },
        );

        let selection = imp.photo_grid.imp().selection.borrow().clone().unwrap();
        let grid_view = imp.photo_grid.imp().grid_view().clone();

        let ctx = actions::ActionContext {
            selection: selection.clone(),
            filter: filter.clone(),
            grid_view,
            bus_sender: bus_sender.clone(),
        };

        actions::wire_context_menu(&ctx);

        // ── Build action bar buttons for this filter ────────────────────
        let bar_box = imp.bar_box();
        while let Some(child) = bar_box.first_child() {
            bar_box.remove(&child);
        }

        let bar_buttons = action_bar::build_for_filter(&filter, &ctx.selection, &bus_sender);
        bar_box.append(&bar_buttons.container);
        *imp.fav_btn.borrow_mut() = bar_buttons.fav_btn;

        if let Some(ref album_btn) = bar_buttons.album_btn {
            actions::wire_album_controls(&ctx, album_btn);
        }

        // ── Trash header buttons (Restore All / Empty Trash) ───────────
        if filter == MediaFilter::Trashed {
            // Show buttons only when there are trashed items (GNOME HIG).
            let restore_btn = imp.restore_all_btn.clone();
            let empty_btn = imp.empty_trash_btn.clone();
            let update_trash_buttons: Rc<dyn Fn()> = {
                let store_ref = store.clone();
                Rc::new(move || {
                    let has_items = store_ref.n_items() > 0;
                    restore_btn.set_visible(has_items);
                    empty_btn.set_visible(has_items);
                })
            };
            update_trash_buttons();
            {
                let update = Rc::clone(&update_trash_buttons);
                store.connect_items_changed(move |_, _, _, _| update());
            }

            {
                let bs = bus_sender.clone();
                imp.restore_all_btn.connect_clicked(move |b| {
                    let bs = bs.clone();
                    let win = b.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                    let dialog = adw::AlertDialog::new(
                        Some(&gettext("Restore all photos?")),
                        Some(&gettext(
                            "All trashed photos will be moved back to the library.",
                        )),
                    );
                    dialog.add_response("cancel", &gettext("Cancel"));
                    dialog.add_response("restore", &gettext("Restore All"));
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");
                    dialog.connect_response(None, move |_, response| {
                        if response == "restore" {
                            bs.send(AppEvent::RestoreAllTrashRequested);
                        }
                    });
                    dialog.present(win.as_ref());
                });
            }

            {
                let bs = bus_sender.clone();
                imp.empty_trash_btn.connect_clicked(move |b| {
                    let bs = bs.clone();
                    let win = b.root().and_then(|r| r.downcast::<gtk::Window>().ok());
                    let dialog = adw::AlertDialog::new(
                        Some(&gettext("Empty Trash?")),
                        Some(&gettext("All trashed photos will be permanently deleted. This cannot be undone.")),
                    );
                    dialog.add_response("cancel", &gettext("Cancel"));
                    dialog.add_response("delete", &gettext("Empty Trash"));
                    dialog.set_response_appearance(
                        "delete",
                        adw::ResponseAppearance::Destructive,
                    );
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");
                    dialog.connect_response(None, move |_, response| {
                        if response == "delete" {
                            bs.send(AppEvent::EmptyTrashRequested);
                        }
                    });
                    dialog.present(win.as_ref());
                });
            }
        }

        // ── Selection changed → update count, auto-exit ─────────────────
        {
            let sm = Rc::clone(&imp.selection_mode);
            let exit = imp.exit_selection().clone();
            let title = imp.selection_title().clone();
            let fav_btn = imp.fav_btn.borrow().clone();
            selection.connect_selection_changed(move |sel, _, _| {
                let count = sel.selection().size();
                let text = match count {
                    0 => "0 selected".to_string(),
                    1 => "1 selected".to_string(),
                    n => format!("{n} selected"),
                };
                title.set_label(&text);

                if let Some(ref fav) = fav_btn {
                    if count > 0 {
                        let bitset = sel.selection();
                        let all_fav = (0..bitset.size() as u32).all(|i| {
                            sel.item(bitset.nth(i))
                                .and_then(|o| o.downcast::<MediaItemObject>().ok())
                                .map(|o| o.is_favorite())
                                .unwrap_or(false)
                        });
                        actions::update_fav_button(fav, all_fav);
                    }
                }

                if count == 0 && sm.get() {
                    exit.activate(None);
                }
            });
        }
    }
}

/// Collect media IDs from the current selection.
pub(super) fn collect_selected_ids(
    selection: &gtk::MultiSelection,
) -> Vec<crate::library::media::MediaId> {
    let bitset = selection.selection();
    let n = bitset.size();
    let mut ids = Vec::with_capacity(n as usize);
    for i in 0..n {
        let pos = bitset.nth(i as u32);
        if let Some(obj) = selection
            .item(pos)
            .and_then(|o| o.downcast::<MediaItemObject>().ok())
        {
            ids.push(obj.item().id.clone());
        }
    }
    ids
}

/// Configure the empty state status page for the given filter.
fn set_empty_state_for_filter(page: &adw::StatusPage, filter: &crate::library::media::MediaFilter) {
    use crate::library::media::MediaFilter;
    let (icon, title, description) = match filter {
        MediaFilter::All => (
            "folder-pictures-symbolic",
            "No photos yet",
            "Import photos to get started",
        ),
        MediaFilter::Favorites => (
            "starred-symbolic",
            "No favourites yet",
            "Star a photo to add it here",
        ),
        MediaFilter::RecentImports { .. } => (
            "document-send-symbolic",
            "No recent imports",
            "Import photos from the hamburger menu",
        ),
        MediaFilter::Trashed => (
            "user-trash-symbolic",
            "Trash is empty",
            "Deleted photos appear here for 30 days",
        ),
        MediaFilter::Album { .. } => (
            "folder-symbolic",
            "This album is empty",
            "Use Add to Album to add photos",
        ),
        MediaFilter::Person { .. } => (
            "avatar-default-symbolic",
            "No photos found",
            "Photos of this person will appear here",
        ),
    };
    page.set_icon_name(Some(icon));
    page.set_title(title);
    page.set_description(Some(description));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_sizes_are_sorted_ascending() {
        for pair in ZOOM_SIZES.windows(2) {
            assert!(pair[0] < pair[1], "{} should be < {}", pair[0], pair[1]);
        }
    }

    #[test]
    fn default_zoom_index_in_bounds() {
        assert!(DEFAULT_ZOOM_INDEX < ZOOM_SIZES.len());
    }
}
