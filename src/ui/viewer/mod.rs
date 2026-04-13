use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::media::{MediaId, MediaMetadataRecord};
use crate::library::Library;
use crate::ui::photo_grid::item::MediaItemObject;

pub mod edit_panel;
pub mod info_panel;
mod loading;
mod menu;

use edit_panel::EditPanel;
use info_panel::InfoPanel;

// Re-export shared menu utilities used by video_viewer.
pub use menu::{build_viewer_menu_popover, ViewerMenuButtons};

// ── GObject subclass ─────────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::{Cell, OnceCell, RefCell};

    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/viewer/viewer.ui")]
    pub struct PhotoViewer {
        // Template children (from Blueprint)
        #[template_child]
        pub toolbar_view: TemplateChild<adw::ToolbarView>,
        #[template_child]
        pub picture: TemplateChild<gtk::Picture>,
        #[template_child]
        pub spinner: TemplateChild<gtk::Spinner>,
        #[template_child]
        pub prev_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub next_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub star_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub info_split: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub sidebar_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub info_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub edit_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub menu_btn: TemplateChild<gtk::MenuButton>,

        // Service dependencies (set once in setup)
        pub library: OnceCell<Arc<dyn Library>>,
        pub tokio: OnceCell<tokio::runtime::Handle>,
        pub bus_sender: OnceCell<EventSender>,

        // Owned sub-panels (set in setup, not GObject yet)
        pub info_panel: RefCell<Option<InfoPanel>>,
        pub edit_panel: RefCell<Option<EditPanel>>,

        // Mutable state
        pub items: RefCell<Vec<MediaItemObject>>,
        pub current_index: Cell<usize>,
        /// Monotonically increasing counter. Async loads compare against this
        /// value captured at launch to discard stale results.
        pub load_gen: Cell<u64>,
        /// Set by `show_at` when the viewer is being pushed onto the
        /// NavigationView. The `shown` signal handler reads this to start
        /// the full-res load after the slide-in animation completes.
        pub pending_load: RefCell<Option<MediaId>>,
        /// Cached metadata for the currently displayed item.
        pub current_metadata: RefCell<Option<MediaMetadataRecord>>,
        /// Tracks a pending optimistic favourite toggle for rollback on failure.
        /// Contains `(media_id, previous_favourite_state)`.
        pub pending_fav: RefCell<Option<(MediaId, bool)>>,
        /// Keeps the event bus subscription alive for this viewer's lifetime.
        pub _subscription: RefCell<Option<crate::event_bus::Subscription>>,
    }

    impl PhotoViewer {
        pub fn library(&self) -> &Arc<dyn Library> {
            self.library.get().expect("library not initialized")
        }
        pub fn tokio(&self) -> &tokio::runtime::Handle {
            self.tokio.get().expect("tokio not initialized")
        }
        pub fn bus_sender(&self) -> &EventSender {
            self.bus_sender.get().expect("bus_sender not initialized")
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PhotoViewer {
        const NAME: &'static str = "MomentsPhotoViewer";
        type Type = super::PhotoViewer;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PhotoViewer {
        fn dispose(&self) {
            self.dispose_template();
        }
    }
    impl WidgetImpl for PhotoViewer {
        fn realize(&self) {
            self.parent_realize();

            let viewer = self.obj().downgrade();
            let sub = crate::event_bus::subscribe(move |event| {
                if let crate::app_event::AppEvent::FavoriteChanged { ids, .. } = event {
                    let Some(viewer) = viewer.upgrade() else {
                        return;
                    };
                    let imp = viewer.imp();
                    let mut pf = imp.pending_fav.borrow_mut();
                    if let Some((ref pending_id, _)) = *pf {
                        if ids.contains(pending_id) {
                            *pf = None;
                        }
                    }
                }
            });
            *self._subscription.borrow_mut() = Some(sub);
        }

        fn unrealize(&self) {
            self._subscription.borrow_mut().take();
            self.parent_unrealize();
        }
    }
    impl NavigationPageImpl for PhotoViewer {}
}

glib::wrapper! {
    pub struct PhotoViewer(ObjectSubclass<imp::PhotoViewer>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PhotoViewer {
    fn default() -> Self {
        Self::new()
    }
}

impl PhotoViewer {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Inject service dependencies, build sub-panels, and wire signal handlers.
    pub fn setup(
        &self,
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        bus_sender: EventSender,
    ) {
        let imp = self.imp();

        // Store service deps.
        assert!(
            imp.library.set(library.clone()).is_ok(),
            "setup called twice"
        );
        assert!(imp.tokio.set(tokio.clone()).is_ok(), "setup called twice");
        assert!(
            imp.bus_sender.set(bus_sender.clone()).is_ok(),
            "setup called twice"
        );

        // Build sub-panels and add to sidebar stack.
        let info_panel = InfoPanel::new();
        let edit_panel = EditPanel::new();
        edit_panel.setup(imp.picture.clone(), library, tokio, bus_sender);
        imp.sidebar_stack.add_named(&info_panel, Some("info"));
        imp.sidebar_stack.add_named(&edit_panel, Some("edit"));
        *imp.info_panel.borrow_mut() = Some(info_panel);
        *imp.edit_panel.borrow_mut() = Some(edit_panel);

        // Build and attach overflow menu.
        let (menu_popover, menu_buttons) = menu::build_viewer_menu_popover(true, "Delete photo");
        imp.menu_btn.set_popover(Some(&menu_popover));

        // Wire all signal handlers.
        self.setup_signals(&menu_popover, &menu_buttons);
    }

    /// Load `items` and navigate to `index`.
    ///
    /// Replaces the current item list, resets async state, and starts loading
    /// the new photo. Call this every time the user activates a grid item.
    pub fn show(&self, items: Vec<MediaItemObject>, index: usize) {
        *self.imp().items.borrow_mut() = items;
        self.show_at(index);
    }

    /// Switch to the item at `index`.
    ///
    /// Updates the title, sets the thumbnail immediately, updates navigation
    /// button visibility, and kicks off async loads for full-res and metadata.
    fn show_at(&self, index: usize) {
        let imp = self.imp();

        // Extract what we need before releasing the borrow.
        let (id, filename, texture, count) = {
            let items = imp.items.borrow();
            let Some(obj) = items.get(index) else { return };
            (
                obj.item().id.clone(),
                obj.item().original_filename.clone(),
                obj.texture(),
                items.len(),
            )
        };

        imp.current_index.set(index);
        let gen = imp.load_gen.get() + 1;
        imp.load_gen.set(gen);
        *imp.current_metadata.borrow_mut() = None;

        // AdwHeaderBar reads the title directly from the NavigationPage.
        self.set_title(&filename);

        // Show cached thumbnail while full-res loads.
        imp.picture
            .set_paintable(texture.as_ref().map(|t| t.upcast_ref::<gdk::Paintable>()));

        imp.prev_btn.set_visible(index > 0);
        imp.next_btn.set_visible(index + 1 < count);

        // Sync star button with the current item's favourite state.
        {
            let items = imp.items.borrow();
            if let Some(obj) = items.get(index) {
                crate::ui::widgets::update_star_button(&imp.star_btn, obj.is_favorite());
            }
        }

        // Close the sidebar only on initial open — during next/prev
        // navigation the info panel stays open and updates in-place.
        if !self.is_mapped() {
            imp.info_split.set_show_sidebar(false);
        } else if imp.info_split.shows_sidebar() {
            // Clear stale metadata immediately so the panel doesn't
            // show the previous photo's EXIF data while the async
            // fetch completes.
            let items = imp.items.borrow();
            if let Some(obj) = items.get(index) {
                let item = obj.item().clone();
                drop(items);
                if let Some(ref panel) = *imp.info_panel.borrow() {
                    panel.set_item(&item, None);
                }
            }
        }

        // Defer full-res load until the page transition completes (shown
        // signal) to avoid a stutter as the large image replaces the
        // thumbnail mid-animation. If the page is already visible (e.g.
        // prev/next navigation), start immediately.
        if self.is_mapped() {
            self.start_full_res_load(gen, id.clone());
            self.load_metadata_async(gen, id);
        } else {
            *imp.pending_load.borrow_mut() = Some(id);
        }
    }

    fn navigate_prev(&self) {
        let imp = self.imp();
        let items = imp.items.borrow();
        let mut idx = imp.current_index.get();
        // Skip video items — they belong in VideoViewer.
        while idx > 0 {
            idx -= 1;
            if items
                .get(idx)
                .map(|o| o.item().media_type != crate::library::media::MediaType::Video)
                .unwrap_or(false)
            {
                drop(items);
                self.show_at(idx);
                return;
            }
        }
    }

    fn navigate_next(&self) {
        let imp = self.imp();
        let items = imp.items.borrow();
        let len = items.len();
        let mut idx = imp.current_index.get();
        // Skip video items — they belong in VideoViewer.
        while idx + 1 < len {
            idx += 1;
            if items
                .get(idx)
                .map(|o| o.item().media_type != crate::library::media::MediaType::Video)
                .unwrap_or(false)
            {
                drop(items);
                self.show_at(idx);
                return;
            }
        }
    }

    fn setup_signals(&self, menu_popover: &gtk::Popover, menu_buttons: &menu::ViewerMenuButtons) {
        let imp = self.imp();

        // Start deferred full-res load after the slide-in animation completes,
        // and grab focus so the key controller (← → F9 Escape) works immediately.
        {
            let viewer = self.downgrade();
            self.connect_shown(move |_| {
                let Some(viewer) = viewer.upgrade() else {
                    return;
                };
                let imp = viewer.imp();
                imp.toolbar_view.grab_focus();
                let pending = imp.pending_load.borrow_mut().take();
                if let Some(id) = pending {
                    let gen = imp.load_gen.get();
                    viewer.start_full_res_load(gen, id.clone());
                    viewer.load_metadata_async(gen, id);
                }
            });
        }

        // Prev button
        {
            let viewer = self.downgrade();
            imp.prev_btn.connect_clicked(move |_| {
                if let Some(viewer) = viewer.upgrade() {
                    viewer.navigate_prev();
                }
            });
        }

        // Next button
        {
            let viewer = self.downgrade();
            imp.next_btn.connect_clicked(move |_| {
                if let Some(viewer) = viewer.upgrade() {
                    viewer.navigate_next();
                }
            });
        }

        // Star (favourite) button — optimistic toggle with rollback on failure.
        {
            let viewer = self.downgrade();
            imp.star_btn.connect_clicked(move |btn| {
                let Some(viewer) = viewer.upgrade() else {
                    return;
                };
                let imp = viewer.imp();
                let items = imp.items.borrow();
                let idx = imp.current_index.get();
                let Some(obj) = items.get(idx) else { return };

                let was_fav = obj.is_favorite();
                let new_fav = !was_fav;

                // Optimistic: update icon and current item immediately.
                crate::ui::widgets::update_star_button(btn, new_fav);
                obj.set_is_favorite(new_fav);

                let id = obj.item().id.clone();
                *imp.pending_fav.borrow_mut() = Some((id.clone(), was_fav));

                imp.bus_sender().send(AppEvent::FavoriteRequested {
                    ids: vec![id],
                    state: new_fav,
                });
            });
        }

        // Info toggle → show info sidebar
        {
            let viewer = self.downgrade();
            imp.info_toggle.connect_toggled(move |btn| {
                let Some(viewer) = viewer.upgrade() else {
                    return;
                };
                let imp = viewer.imp();
                if btn.is_active() {
                    // Deactivate edit toggle (mutually exclusive).
                    imp.edit_toggle.set_active(false);
                    imp.sidebar_stack.set_visible_child_name("info");
                    imp.info_split.set_show_sidebar(true);

                    // Populate info panel.
                    let items = imp.items.borrow();
                    let idx = imp.current_index.get();
                    if let Some(obj) = items.get(idx) {
                        let item = obj.item().clone();
                        let meta = imp.current_metadata.borrow();
                        if let Some(ref panel) = *imp.info_panel.borrow() {
                            panel.set_item(&item, meta.as_ref());
                        }
                    }
                } else if !imp.edit_toggle.is_active() {
                    imp.info_split.set_show_sidebar(false);
                }
            });
        }

        // Edit toggle → show edit sidebar and start edit session
        {
            let viewer = self.downgrade();
            imp.edit_toggle.connect_toggled(move |btn| {
                let Some(viewer) = viewer.upgrade() else {
                    return;
                };
                let imp = viewer.imp();
                if btn.is_active() {
                    // Deactivate info toggle (mutually exclusive).
                    imp.info_toggle.set_active(false);
                    imp.sidebar_stack.set_visible_child_name("edit");
                    imp.info_split.set_show_sidebar(true);

                    // Start edit session — load original image for preview.
                    viewer.start_edit_session();
                } else {
                    if !imp.info_toggle.is_active() {
                        imp.info_split.set_show_sidebar(false);
                    }
                    if let Some(ref panel) = *imp.edit_panel.borrow() {
                        panel.end_session();
                    }
                }
            });
        }

        // Split view sidebar closed externally → sync toggles
        {
            let viewer = self.downgrade();
            imp.info_split.connect_show_sidebar_notify(move |split| {
                if !split.shows_sidebar() {
                    let Some(viewer) = viewer.upgrade() else {
                        return;
                    };
                    let imp = viewer.imp();
                    imp.info_toggle.set_active(false);
                    if imp.edit_toggle.is_active() {
                        imp.edit_toggle.set_active(false);
                        if let Some(ref panel) = *imp.edit_panel.borrow() {
                            panel.end_session();
                        }
                    }
                }
            });
        }

        // Keyboard navigation (← → F9 Escape)
        {
            let key_ctrl = gtk::EventControllerKey::new();
            imp.toolbar_view.add_controller(key_ctrl.clone());
            let viewer = self.downgrade();
            key_ctrl.connect_key_pressed(move |_, keyval, _, _| {
                let Some(viewer) = viewer.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                match keyval {
                    gdk::Key::Left | gdk::Key::KP_Left => {
                        viewer.navigate_prev();
                        glib::Propagation::Stop
                    }
                    gdk::Key::Right | gdk::Key::KP_Right => {
                        viewer.navigate_next();
                        glib::Propagation::Stop
                    }
                    gdk::Key::F9 => {
                        let active = viewer.imp().info_toggle.is_active();
                        viewer.imp().info_toggle.set_active(!active);
                        glib::Propagation::Stop
                    }
                    gdk::Key::Escape => {
                        // Pop the viewer page to return to the grid.
                        if let Some(nav_view) = viewer
                            .parent()
                            .and_then(|p| p.downcast::<adw::NavigationView>().ok())
                        {
                            nav_view.pop();
                            glib::Propagation::Stop
                        } else {
                            glib::Propagation::Proceed
                        }
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }

        // Wire overflow menu buttons.
        menu::wire_overflow_menu(menu_popover, menu_buttons, self);
    }
}
