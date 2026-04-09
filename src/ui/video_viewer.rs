use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, gio, glib};
use tracing::debug;

use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::media::{MediaId, MediaMetadataRecord};
use crate::library::Library;
use crate::ui::photo_grid::item::MediaItemObject;
use crate::ui::viewer::info_panel::InfoPanel;

// ── GObject subclass ─────────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::{Cell, OnceCell, RefCell};

    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/video_viewer.ui")]
    pub struct VideoViewer {
        // Template children (from Blueprint)
        #[template_child]
        pub toolbar_view: TemplateChild<adw::ToolbarView>,
        #[template_child]
        pub video: TemplateChild<gtk::Video>,
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
        pub info_toggle: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub menu_btn: TemplateChild<gtk::MenuButton>,

        // Service dependencies (set once in setup)
        pub library: OnceCell<Arc<dyn Library>>,
        pub tokio: OnceCell<tokio::runtime::Handle>,
        pub bus_sender: OnceCell<EventSender>,

        // Owned sub-panel (set in setup, not GObject yet)
        pub info_panel: RefCell<Option<InfoPanel>>,

        // Mutable state
        pub items: RefCell<Vec<MediaItemObject>>,
        pub current_index: Cell<usize>,
        pub current_metadata: RefCell<Option<MediaMetadataRecord>>,
        /// Tracks a pending optimistic favourite toggle for rollback on failure.
        pub pending_fav: RefCell<Option<(MediaId, bool)>>,
        /// Keeps the event bus subscription alive for this viewer's lifetime.
        pub _subscription: RefCell<Option<crate::event_bus::Subscription>>,
    }

    impl VideoViewer {
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
    impl ObjectSubclass for VideoViewer {
        const NAME: &'static str = "MomentsVideoViewer";
        type Type = super::VideoViewer;
        type ParentType = adw::NavigationPage;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for VideoViewer {
        fn dispose(&self) {
            self.dispose_template();
        }
    }
    impl WidgetImpl for VideoViewer {}
    impl NavigationPageImpl for VideoViewer {}
}

glib::wrapper! {
    pub struct VideoViewer(ObjectSubclass<imp::VideoViewer>)
        @extends adw::NavigationPage, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for VideoViewer {
    fn default() -> Self {
        Self::new()
    }
}

impl VideoViewer {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Inject service dependencies, build info panel, and wire signal handlers.
    pub fn setup(
        &self,
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        bus_sender: EventSender,
    ) {
        let imp = self.imp();

        // Store service deps.
        assert!(imp.library.set(library).is_ok(), "setup called twice");
        assert!(imp.tokio.set(tokio).is_ok(), "setup called twice");
        assert!(imp.bus_sender.set(bus_sender).is_ok(), "setup called twice");

        // Build info panel and set as sidebar.
        let info_panel = InfoPanel::new();
        imp.info_split.set_sidebar(Some(info_panel.widget()));
        *imp.info_panel.borrow_mut() = Some(info_panel);

        // Build and attach overflow menu.
        let (menu_popover, menu_buttons) =
            crate::ui::viewer::build_viewer_menu_popover(false, "Delete video");
        imp.menu_btn.set_popover(Some(&menu_popover));

        // Wire all signal handlers.
        self.setup_signals(&menu_popover, &menu_buttons);
    }

    /// Load `items` and navigate to `index`.
    pub fn show(&self, items: Vec<MediaItemObject>, index: usize) {
        let imp = self.imp();
        debug!(index, item_count = items.len(), "VideoViewer::show");
        // Stop any previous playback.
        imp.video.set_file(None::<&gio::File>);
        *imp.items.borrow_mut() = items;
        self.show_at(index);
        self.grab_focus();
    }

    #[tracing::instrument(skip(self), fields(index))]
    fn show_at(&self, index: usize) {
        let imp = self.imp();

        let (id, filename, count) = {
            let items = imp.items.borrow();
            let Some(obj) = items.get(index) else {
                tracing::warn!(index, "show_at: index out of bounds");
                return;
            };
            (
                obj.item().id.clone(),
                obj.item().original_filename.clone(),
                items.len(),
            )
        };

        debug!(index, %id, %filename, count, "VideoViewer::show_at");

        imp.current_index.set(index);
        *imp.current_metadata.borrow_mut() = None;

        self.set_title(&filename);
        imp.prev_btn.set_visible(index > 0);
        imp.next_btn.set_visible(index + 1 < count);

        // Grab focus so the key controller works immediately.
        imp.toolbar_view.grab_focus();

        // Sync star button.
        {
            let items = imp.items.borrow();
            if let Some(obj) = items.get(index) {
                crate::ui::widgets::update_star_button(&imp.star_btn, obj.is_favorite());
            }
        }

        imp.info_split.set_show_sidebar(false);

        // Resolve the original file path and set it on the video widget.
        self.load_video(id.clone());
        self.load_metadata_async(id);
    }

    fn load_video(&self, id: MediaId) {
        let imp = self.imp();
        let library = Arc::clone(imp.library());
        let tokio = imp.tokio().clone();
        let bus_sender = imp.bus_sender().clone();

        debug!(%id, "load_video: resolving path");

        // Stop any current playback and show loading spinner.
        imp.video.set_file(None::<&gio::File>);
        imp.spinner.set_spinning(true);
        imp.spinner.set_visible(true);

        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let path = match tokio
                .spawn(async move { library.original_path(&id).await })
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
            {
                Some(p) => p,
                None => {
                    if let Some(viewer) = weak.upgrade() {
                        let imp = viewer.imp();
                        imp.spinner.set_spinning(false);
                        imp.spinner.set_visible(false);
                    }
                    tracing::warn!("load_video: could not resolve original path");
                    bus_sender.send(AppEvent::Error(
                        "Could not find original video".into(),
                    ));
                    return;
                }
            };

            let Some(viewer) = weak.upgrade() else { return };
            let imp = viewer.imp();

            debug!(path = %path.display(), exists = path.exists(), "load_video: setting file on GtkVideo");
            let file = gio::File::for_path(&path);
            imp.video.set_file(Some(&file));
            imp.spinner.set_spinning(false);
            imp.spinner.set_visible(false);
            debug!("load_video: file set, playback should start (autoplay=true)");
        });
    }

    fn load_metadata_async(&self, id: MediaId) {
        let imp = self.imp();
        let library = Arc::clone(imp.library());
        let tokio = imp.tokio().clone();

        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let metadata = tokio
                .spawn(async move { library.media_metadata(&id).await })
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten();

            let Some(viewer) = weak.upgrade() else { return };
            let imp = viewer.imp();

            *imp.current_metadata.borrow_mut() = metadata;

            if imp.info_split.shows_sidebar() {
                let items = imp.items.borrow();
                let idx = imp.current_index.get();
                if let Some(obj) = items.get(idx) {
                    let item = obj.item().clone();
                    let meta = imp.current_metadata.borrow();
                    if let Some(ref panel) = *imp.info_panel.borrow() {
                        panel.populate(&item, meta.as_ref());
                    }
                    drop(meta);
                }
            }
        });
    }

    fn navigate_prev(&self) {
        let imp = self.imp();
        let items = imp.items.borrow();
        let mut idx = imp.current_index.get();
        // Skip image items — they belong in PhotoViewer.
        while idx > 0 {
            idx -= 1;
            if items
                .get(idx)
                .map(|o| o.item().media_type == crate::library::media::MediaType::Video)
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
        // Skip image items — they belong in PhotoViewer.
        while idx + 1 < len {
            idx += 1;
            if items
                .get(idx)
                .map(|o| o.item().media_type == crate::library::media::MediaType::Video)
                .unwrap_or(false)
            {
                drop(items);
                self.show_at(idx);
                return;
            }
        }
    }

    fn setup_signals(
        &self,
        menu_popover: &gtk::Popover,
        menu_buttons: &crate::ui::viewer::ViewerMenuButtons,
    ) {
        let imp = self.imp();

        // Prev button
        {
            let weak = self.downgrade();
            imp.prev_btn.connect_clicked(move |_| {
                if let Some(viewer) = weak.upgrade() {
                    viewer.navigate_prev();
                }
            });
        }

        // Next button
        {
            let weak = self.downgrade();
            imp.next_btn.connect_clicked(move |_| {
                if let Some(viewer) = weak.upgrade() {
                    viewer.navigate_next();
                }
            });
        }

        // Star (favourite) button — optimistic toggle with rollback on failure.
        {
            let weak = self.downgrade();
            imp.star_btn.connect_clicked(move |btn| {
                let Some(viewer) = weak.upgrade() else { return };
                let imp = viewer.imp();
                let items = imp.items.borrow();
                let idx = imp.current_index.get();
                let Some(obj) = items.get(idx) else { return };

                let was_fav = obj.is_favorite();
                let new_fav = !was_fav;
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

        // Info toggle → split view
        {
            let weak = self.downgrade();
            imp.info_toggle.connect_toggled(move |btn| {
                let Some(viewer) = weak.upgrade() else { return };
                let imp = viewer.imp();
                imp.info_split.set_show_sidebar(btn.is_active());

                if btn.is_active() {
                    let items = imp.items.borrow();
                    let idx = imp.current_index.get();
                    if let Some(obj) = items.get(idx) {
                        let item = obj.item().clone();
                        let meta = imp.current_metadata.borrow();
                        if let Some(ref panel) = *imp.info_panel.borrow() {
                            panel.populate(&item, meta.as_ref());
                        }
                        drop(meta);
                    }
                }
            });
        }

        // Split view sidebar closed externally → sync toggle
        {
            let weak = self.downgrade();
            imp.info_split.connect_show_sidebar_notify(move |split| {
                if let Some(viewer) = weak.upgrade() {
                    viewer.imp().info_toggle.set_active(split.shows_sidebar());
                }
            });
        }

        // Keyboard navigation (← → F9 Escape)
        {
            let key_ctrl = gtk::EventControllerKey::new();
            imp.toolbar_view.add_controller(key_ctrl.clone());
            let weak = self.downgrade();
            key_ctrl.connect_key_pressed(move |_, keyval, _, _| {
                let Some(viewer) = weak.upgrade() else {
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

        // ── Wire overflow menu buttons ──────────────────────────────────────
        wire_overflow_menu(menu_popover, menu_buttons, self);

        // Subscribe to bus: clear pending favourite state on confirmation.
        {
            let weak = self.downgrade();
            let sub = crate::event_bus::subscribe(move |event| {
                if let AppEvent::FavoriteChanged { ids, .. } = event {
                    let Some(viewer) = weak.upgrade() else { return };
                    let imp = viewer.imp();
                    let mut pf = imp.pending_fav.borrow_mut();
                    if let Some((ref pending_id, _)) = *pf {
                        if ids.contains(pending_id) {
                            *pf = None;
                        }
                    }
                }
            });
            *self.imp()._subscription.borrow_mut() = Some(sub);
        }
    }
}

/// Wire overflow menu button handlers for the video viewer.
fn wire_overflow_menu(
    popover: &gtk::Popover,
    buttons: &crate::ui::viewer::ViewerMenuButtons,
    viewer: &VideoViewer,
) {
    // Add to album
    {
        let weak = viewer.downgrade();
        let pop = popover.downgrade();
        buttons.add_to_album.connect_clicked(move |_| {
            if let Some(p) = pop.upgrade() {
                p.popdown();
            }
            let Some(viewer) = weak.upgrade() else { return };
            let imp = viewer.imp();
            let id = {
                let items = imp.items.borrow();
                let idx = imp.current_index.get();
                items.get(idx).map(|obj| obj.item().id.clone())
            };
            let Some(id) = id else { return };
            crate::ui::album_picker_dialog::show_album_picker_dialog(
                viewer.upcast_ref::<gtk::Widget>(),
                vec![id],
                Arc::clone(imp.library()),
                imp.tokio().clone(),
                imp.bus_sender().clone(),
            );
        });
    }

    // Stub items — just close the popover on click.
    for btn in [
        &buttons.share,
        &buttons.export_original,
        &buttons.show_in_files,
    ] {
        let pop = popover.downgrade();
        btn.connect_clicked(move |_| {
            if let Some(p) = pop.upgrade() {
                p.popdown();
            }
        });
    }

    // Delete video — trash + pop back to grid.
    {
        let weak = viewer.downgrade();
        let pop = popover.downgrade();
        buttons.delete.connect_clicked(move |_| {
            if let Some(p) = pop.upgrade() {
                p.popdown();
            }
            let Some(viewer) = weak.upgrade() else { return };
            let imp = viewer.imp();
            let id = {
                let items = imp.items.borrow();
                let idx = imp.current_index.get();
                items.get(idx).map(|obj| obj.item().id.clone())
            };
            let Some(id) = id else { return };
            imp.bus_sender()
                .send(AppEvent::TrashRequested { ids: vec![id] });
            if let Some(nav_view) = viewer
                .parent()
                .and_then(|p| p.downcast::<adw::NavigationView>().ok())
            {
                nav_view.pop();
            }
        });
    }
}
