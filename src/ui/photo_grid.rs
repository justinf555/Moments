use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib, subclass::prelude::*};
use tracing::{debug, instrument};

use crate::library::media::{MediaFilter, MediaType};
use crate::library::Library;
use crate::ui::video_viewer::VideoViewer;
use crate::ui::viewer::PhotoViewer;
use crate::ui::ContentView;

pub mod cell;
pub mod factory;
pub mod item;
pub mod model;

pub use model::PhotoGridModel;

/// Available cell sizes (px), smallest to largest.
const ZOOM_SIZES: &[i32] = &[96, 128, 160, 200, 256, 320];
/// Default zoom level index (160 px).
const DEFAULT_ZOOM_INDEX: usize = 2;

mod imp {
    use super::*;
    use std::cell::OnceCell;

    pub struct PhotoGrid {
        pub scrolled: OnceCell<gtk::ScrolledWindow>,
        pub grid_view: OnceCell<gtk::GridView>,
        pub selection: RefCell<Option<gtk::MultiSelection>>,
        /// Kept alive so lazy-loading stays wired after `set_model`.
        pub model: RefCell<Option<Rc<PhotoGridModel>>>,
        pub zoom_level: Cell<usize>,
        /// Library reference for the factory (star button persist).
        pub library: OnceCell<Arc<dyn Library>>,
        pub tokio: OnceCell<tokio::runtime::Handle>,
        pub registry: OnceCell<Rc<crate::ui::model_registry::ModelRegistry>>,
        pub filter: RefCell<crate::library::media::MediaFilter>,
    }

    impl Default for PhotoGrid {
        fn default() -> Self {
            Self {
                scrolled: OnceCell::default(),
                grid_view: OnceCell::default(),
                selection: RefCell::default(),
                model: RefCell::default(),
                zoom_level: Cell::new(DEFAULT_ZOOM_INDEX),
                library: OnceCell::default(),
                tokio: OnceCell::default(),
                registry: OnceCell::default(),
                filter: RefCell::new(crate::library::media::MediaFilter::All),
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

    impl ObjectImpl for PhotoGrid {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            let grid_view = gtk::GridView::new(
                None::<gtk::NoSelection>,
                None::<gtk::SignalListItemFactory>,
            );
            grid_view.set_min_columns(2);
            grid_view.set_max_columns(20);

            let scrolled = gtk::ScrolledWindow::new();
            scrolled.set_hscrollbar_policy(gtk::PolicyType::Never);
            scrolled.set_vexpand(true);
            scrolled.set_child(Some(&grid_view));
            scrolled.set_parent(&*obj);

            self.grid_view.set(grid_view).unwrap();
            self.scrolled.set(scrolled).unwrap();
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
    pub struct PhotoGrid(ObjectSubclass<imp::PhotoGrid>)
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
    ///
    /// Clamps to valid bounds. Does not rebuild the factory — call before
    /// `set_model` so the initial factory uses the correct size.
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
        let grid_view = imp.grid_view.get().unwrap();
        let library = imp.library.get().unwrap().clone();
        let tokio = imp.tokio.get().unwrap().clone();
        let registry = imp.registry.get().unwrap().clone();
        let filter = imp.filter.borrow().clone();
        grid_view.set_factory(Some(&factory::build_factory(
            self.current_cell_size(),
            library,
            tokio,
            registry,
            filter,
        )));
    }

    /// Attach a `PhotoGridModel` to the grid.
    ///
    /// Wires the model's `ListStore` to `GridView` via `MultiSelection`, builds
    /// the cell factory, triggers the initial page load, and connects
    /// scroll-based lazy loading for subsequent pages.
    ///
    /// `on_activate` is called with `(items, position)` when the user
    /// double-clicks or presses Enter on a grid item.
    #[instrument(skip_all)]
    pub fn set_model(
        &self,
        model: Rc<PhotoGridModel>,
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        registry: Rc<crate::ui::model_registry::ModelRegistry>,
        filter: crate::library::media::MediaFilter,
        on_activate: impl Fn(Vec<item::MediaItemObject>, usize) + 'static,
    ) {
        let imp = self.imp();
        let _ = imp.library.set(Arc::clone(&library));
        let _ = imp.tokio.set(tokio.clone());
        let _ = imp.registry.set(Rc::clone(&registry));
        *imp.filter.borrow_mut() = filter.clone();

        let grid_view = imp.grid_view.get().unwrap();
        let scrolled = imp.scrolled.get().unwrap();

        let selection = gtk::MultiSelection::new(Some(model.store.clone()));
        grid_view.set_model(Some(&selection));
        *imp.selection.borrow_mut() = Some(selection.clone());
        grid_view.set_factory(Some(&factory::build_factory(
            self.current_cell_size(),
            Arc::clone(&library),
            tokio,
            Rc::clone(&registry),
            filter.clone(),
        )));

        // Fetch the first page immediately.
        model.load_more();

        // Load further pages as the user scrolls toward the bottom.
        let model_scroll = Rc::clone(&model);
        scrolled
            .vadjustment()
            .connect_value_changed(move |adj| {
                // Trigger when within half a page of the bottom.
                let threshold = adj.upper() - adj.page_size() - (adj.page_size() * 0.5);
                if adj.value() >= threshold {
                    model_scroll.load_more();
                }
            });

        // Wire item activation — snapshot all items, then call on_activate.
        let selection_ref = selection.clone();
        grid_view.connect_activate(move |_, position| {
            let n = selection_ref.n_items();
            let items: Vec<item::MediaItemObject> = (0..n)
                .filter_map(|i| {
                    selection_ref
                        .item(i)
                        .and_then(|obj| obj.downcast::<item::MediaItemObject>().ok())
                })
                .collect();
            on_activate(items, position as usize);
        });

        *imp.model.borrow_mut() = Some(model);
    }
}

/// Wraps `PhotoGrid` in an `AdwNavigationView` so that activating a grid item
/// pushes a [`PhotoViewer`] page without leaving the main shell.
///
/// The root page of the `NavigationView` contains the grid's `AdwToolbarView`.
/// The viewer page is pushed on activation and popped by the back button.
impl std::fmt::Debug for PhotoGridView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhotoGridView").finish_non_exhaustive()
    }
}

pub struct PhotoGridView {
    /// The `NavigationView` is the outermost widget returned by `widget()`.
    nav_view: adw::NavigationView,
    photo_grid: PhotoGrid,
    photo_viewer: Rc<PhotoViewer>,
    video_viewer: Rc<VideoViewer>,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    trash_btn: gtk::Button,
    restore_btn: gtk::Button,
    delete_btn: gtk::Button,
    album_btn: gtk::Button,
    remove_from_album_btn: gtk::Button,
    widget: gtk::Widget,
    /// Zoom actions — must be installed on the window so accelerators work
    /// regardless of which widget has focus.
    view_actions: gio::SimpleActionGroup,
}

impl PhotoGridView {
    pub fn new(
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        settings: gio::Settings,
        registry: Rc<crate::ui::model_registry::ModelRegistry>,
    ) -> Self {
        // ── Grid header bar ──────────────────────────────────────────────────
        let header = adw::HeaderBar::new();

        let import_button = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Import Photos")
            .action_name("app.import")
            .build();
        import_button.add_css_class("flat");
        header.pack_start(&import_button);

        // ── Zoom controls ───────────────────────────────────────────────────
        let zoom_out_btn = gtk::Button::builder()
            .icon_name("zoom-out-symbolic")
            .tooltip_text("Zoom Out")
            .action_name("view.zoom-out")
            .build();
        zoom_out_btn.add_css_class("flat");
        let zoom_in_btn = gtk::Button::builder()
            .icon_name("zoom-in-symbolic")
            .tooltip_text("Zoom In")
            .action_name("view.zoom-in")
            .build();
        zoom_in_btn.add_css_class("flat");
        let zoom_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        zoom_box.append(&zoom_out_btn);
        zoom_box.append(&zoom_in_btn);

        // Stop button clicks from propagating to the HeaderBar's
        // drag/maximize gesture.
        let controller = gtk::EventControllerLegacy::new();
        controller.connect_event(|_, event| {
            use gtk::gdk::EventType;
            match event.event_type() {
                EventType::ButtonPress | EventType::ButtonRelease => glib::Propagation::Stop,
                _ => glib::Propagation::Proceed,
            }
        });
        zoom_box.add_controller(controller);

        header.pack_start(&zoom_box);

        // ── Selection action buttons (right side, before menu) ─────────
        // Trash view gets Restore + Delete; other views get Trash.
        let restore_btn = gtk::Button::builder()
            .icon_name("edit-undo-symbolic")
            .tooltip_text("Restore")
            .sensitive(false)
            .visible(false)
            .build();
        restore_btn.add_css_class("flat");
        header.pack_end(&restore_btn);

        let delete_btn = gtk::Button::builder()
            .icon_name("edit-delete-symbolic")
            .tooltip_text("Delete Permanently")
            .sensitive(false)
            .visible(false)
            .build();
        delete_btn.add_css_class("flat");
        delete_btn.add_css_class("error");
        header.pack_end(&delete_btn);

        let trash_btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Move to Trash")
            .sensitive(false)
            .build();
        trash_btn.add_css_class("flat");
        header.pack_end(&trash_btn);

        let remove_from_album_btn = gtk::Button::builder()
            .icon_name("list-remove-symbolic")
            .tooltip_text("Remove from Album")
            .sensitive(false)
            .visible(false)
            .build();
        remove_from_album_btn.add_css_class("flat");
        header.pack_end(&remove_from_album_btn);

        let album_btn = gtk::Button::builder()
            .icon_name("folder-new-symbolic")
            .tooltip_text("Add to Album")
            .sensitive(false)
            .build();
        album_btn.add_css_class("flat");
        header.pack_end(&album_btn);

        let menu_button = gtk::MenuButton::builder()
            .primary(true)
            .icon_name("open-menu-symbolic")
            .tooltip_text("Main Menu")
            .build();
        let menu = gio::Menu::new();
        let section = gio::Menu::new();
        section.append(Some("_Preferences"), Some("app.preferences"));
        section.append(Some("_Keyboard Shortcuts"), Some("app.shortcuts"));
        section.append(Some("_About Moments"), Some("app.about"));
        menu.append_section(None, &section);
        menu_button.set_menu_model(Some(&menu));
        header.pack_end(&menu_button);

        // ── Grid toolbar view (root nav page content) ────────────────────────
        let photo_grid = PhotoGrid::new();
        photo_grid.set_zoom_level(settings.uint("zoom-level") as usize);
        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&photo_grid));

        let grid_page = adw::NavigationPage::builder()
            .tag("grid")
            .title("Photos")
            .child(&toolbar_view)
            .build();

        // ── NavigationView wraps both grid and viewer ────────────────────────
        let nav_view = adw::NavigationView::new();
        nav_view.push(&grid_page);

        // ── Viewers (reused across activations) ──────────────────────────────
        let photo_viewer = Rc::new(PhotoViewer::new(Arc::clone(&library), tokio.clone(), Rc::clone(&registry)));
        let video_viewer = Rc::new(VideoViewer::new(Arc::clone(&library), tokio.clone(), Rc::clone(&registry)));

        // ── Zoom actions ─────────────────────────────────────────────────────
        let action_group = gio::SimpleActionGroup::new();

        let zoom_in_action = gio::SimpleAction::new("zoom-in", None);
        let zoom_out_action = gio::SimpleAction::new("zoom-out", None);

        // Disable zoom-in at max, zoom-out at min.
        zoom_in_action.set_enabled(
            photo_grid.imp().zoom_level.get() + 1 < ZOOM_SIZES.len(),
        );
        zoom_out_action.set_enabled(photo_grid.imp().zoom_level.get() > 0);

        {
            let grid = photo_grid.clone();
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
            let grid = photo_grid.clone();
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

        let widget = nav_view.clone().upcast::<gtk::Widget>();

        Self {
            nav_view,
            photo_grid,
            photo_viewer,
            video_viewer,
            library,
            tokio,
            trash_btn,
            restore_btn,
            delete_btn,
            album_btn,
            remove_from_album_btn,
            widget,
            view_actions: action_group,
        }
    }

    /// Action group containing `zoom-in` and `zoom-out` actions.
    ///
    /// Install on the window with prefix `"view"` so accelerators work
    /// regardless of focus.
    pub fn view_actions(&self) -> &gio::SimpleActionGroup {
        &self.view_actions
    }

    pub fn set_model(&self, model: Rc<PhotoGridModel>, registry: Rc<crate::ui::model_registry::ModelRegistry>) {
        let nav_view = self.nav_view.clone();
        let photo_viewer = Rc::clone(&self.photo_viewer);
        let photo_nav_page = self.photo_viewer.nav_page().clone();
        let video_viewer = Rc::clone(&self.video_viewer);
        let video_nav_page = self.video_viewer.nav_page().clone();

        let filter = model.filter();
        self.photo_grid.set_model(
            Rc::clone(&model),
            Arc::clone(&self.library),
            self.tokio.clone(),
            Rc::clone(&registry),
            filter.clone(),
            move |items, index| {
                // Choose viewer based on media type.
                let media_type = items
                    .get(index)
                    .map(|obj| obj.item().media_type)
                    .unwrap_or(MediaType::Image);

                let filename = items
                    .get(index)
                    .map(|obj| obj.item().original_filename.clone())
                    .unwrap_or_default();

                tracing::debug!(
                    index,
                    ?media_type,
                    %filename,
                    "grid item activated"
                );

                let (tag, nav_page) = if media_type == MediaType::Video {
                    video_viewer.show(items, index);
                    ("video-viewer", &video_nav_page)
                } else {
                    photo_viewer.show(items, index);
                    ("viewer", &photo_nav_page)
                };

                // Push viewer page if it isn't already the visible page.
                let visible_tag = nav_view
                    .visible_page()
                    .and_then(|p| p.tag())
                    .unwrap_or_default();
                tracing::debug!(target_tag = tag, %visible_tag, "pushing viewer page");
                if visible_tag != tag {
                    nav_view.push(nav_page);
                }
            },
        );

        let selection = self.photo_grid.imp().selection.borrow().clone().unwrap();
        let is_trash_view = filter == crate::library::media::MediaFilter::Trashed;
        let is_album_view = matches!(filter, crate::library::media::MediaFilter::Album { .. });

        // Show the right buttons for this view.
        self.trash_btn.set_visible(!is_trash_view);
        self.restore_btn.set_visible(is_trash_view);
        self.delete_btn.set_visible(is_trash_view);
        self.album_btn.set_visible(!is_trash_view && !is_album_view);
        self.remove_from_album_btn.set_visible(is_album_view);

        // Enable/disable action buttons based on selection.
        {
            let trash_btn = self.trash_btn.clone();
            let restore_btn = self.restore_btn.clone();
            let delete_btn = self.delete_btn.clone();
            let album_btn = self.album_btn.clone();
            let remove_btn = self.remove_from_album_btn.clone();
            selection.connect_selection_changed(move |sel, _, _| {
                let has_selection = sel.selection().size() > 0;
                trash_btn.set_sensitive(has_selection);
                restore_btn.set_sensitive(has_selection);
                delete_btn.set_sensitive(has_selection);
                album_btn.set_sensitive(has_selection);
                remove_btn.set_sensitive(has_selection);
            });
        }

        if is_trash_view {
            // ── Restore button ──────────────────────────────────────────
            {
                let selection = selection.clone();
                let lib = Arc::clone(&self.library);
                let tk = self.tokio.clone();
                let reg = Rc::clone(&registry);
                let restore_btn = self.restore_btn.clone();
                restore_btn.connect_clicked(move |btn| {
                    let ids = collect_selected_ids(&selection);
                    if ids.is_empty() { return; }
                    selection.unselect_all();
                    btn.set_sensitive(false);

                    let lib = Arc::clone(&lib);
                    let tk = tk.clone();
                    let reg = Rc::clone(&reg);
                    glib::MainContext::default().spawn_local(async move {
                        let ids_bc = ids.clone();
                        let result = tk
                            .spawn(async move { lib.restore(&ids).await })
                            .await;
                        match result {
                            Ok(Ok(())) => {
                                for id in &ids_bc {
                                    reg.on_trashed(id, false);
                                }
                            }
                            Ok(Err(e)) => tracing::error!("restore failed: {e}"),
                            Err(e) => tracing::error!("restore join failed: {e}"),
                        }
                    });
                });
            }

            // ── Delete permanently button ───────────────────────────────
            {
                let selection = selection.clone();
                let lib = Arc::clone(&self.library);
                let tk = self.tokio.clone();
                let reg = Rc::clone(&registry);
                let delete_btn = self.delete_btn.clone();
                let nav_view = self.nav_view.clone();
                delete_btn.connect_clicked(move |btn| {
                    let ids = collect_selected_ids(&selection);
                    if ids.is_empty() { return; }

                    // Confirmation dialog.
                    let count = ids.len();
                    let dialog = adw::AlertDialog::builder()
                        .heading("Delete Permanently?")
                        .body(format!(
                            "This will permanently delete {count} {} and cannot be undone.",
                            if count == 1 { "photo" } else { "photos" }
                        ))
                        .build();
                    dialog.add_response("cancel", "Cancel");
                    dialog.add_response("delete", "Delete");
                    dialog.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
                    dialog.set_default_response(Some("cancel"));
                    dialog.set_close_response("cancel");

                    let selection = selection.clone();
                    let lib = Arc::clone(&lib);
                    let tk = tk.clone();
                    let reg = Rc::clone(&reg);
                    let btn = btn.clone();
                    dialog.connect_response(None, move |_, response| {
                        if response != "delete" { return; }

                        let ids = ids.clone();
                        selection.unselect_all();
                        btn.set_sensitive(false);

                        let lib = Arc::clone(&lib);
                        let tk = tk.clone();
                        let reg = Rc::clone(&reg);
                        glib::MainContext::default().spawn_local(async move {
                            let ids_bc = ids.clone();
                            let result = tk
                                .spawn(async move { lib.delete_permanently(&ids).await })
                                .await;
                            match result {
                                Ok(Ok(())) => {
                                    for id in &ids_bc {
                                        reg.on_deleted(id);
                                    }
                                }
                                Ok(Err(e)) => tracing::error!("delete_permanently failed: {e}"),
                                Err(e) => tracing::error!("delete_permanently join failed: {e}"),
                            }
                        });
                    });
                    dialog.present(nav_view.root().as_ref().and_then(|r| r.downcast_ref::<gtk::Window>()));
                });
            }
        } else {
            // ── Trash button ────────────────────────────────────────────
            let selection = selection.clone();
            let lib = Arc::clone(&self.library);
            let tk = self.tokio.clone();
            let trash_btn = self.trash_btn.clone();
            let registry_for_trash = Rc::clone(&registry);
            trash_btn.connect_clicked(move |btn| {
                let ids = collect_selected_ids(&selection);
                if ids.is_empty() { return; }
                selection.unselect_all();
                btn.set_sensitive(false);

                let lib = Arc::clone(&lib);
                let tk = tk.clone();
                let reg = Rc::clone(&registry_for_trash);
                glib::MainContext::default().spawn_local(async move {
                    let ids_bc = ids.clone();
                    let result = tk
                        .spawn(async move { lib.trash(&ids).await })
                        .await;
                    match result {
                        Ok(Ok(())) => {
                            for id in &ids_bc {
                                reg.on_trashed(id, true);
                            }
                        }
                        Ok(Err(e)) => tracing::error!("trash failed: {e}"),
                        Err(e) => tracing::error!("trash join failed: {e}"),
                    }
                });
            });
        }

        // ── "Add to Album" popover ──────────────────────────────────────
        {
            let lib = Arc::clone(&self.library);
            let tk = self.tokio.clone();
            let reg = Rc::clone(&registry);
            let selection = selection.clone();
            let album_btn = self.album_btn.clone();

            // Load albums async on click, then show a Popover with the list.
            album_btn.connect_clicked(move |btn: &gtk::Button| {
                debug!("album button clicked, loading albums async");

                let lib = Arc::clone(&lib);
                let tk = tk.clone();
                let reg = Rc::clone(&reg);
                let sel = selection.clone();
                let btn_weak: glib::WeakRef<gtk::Button> = btn.downgrade();

                glib::MainContext::default().spawn_local(async move {
                    let lib_q = Arc::clone(&lib);
                    debug!("fetching album list from library");
                    let albums = match tk.spawn(async move { lib_q.list_albums().await }).await {
                        Ok(Ok(a)) => a,
                        Ok(Err(e)) => {
                            tracing::error!("list_albums failed: {e}");
                            return;
                        }
                        Err(e) => {
                            tracing::error!("list_albums join failed: {e}");
                            return;
                        }
                    };

                    let Some(btn) = btn_weak.upgrade() else {
                        debug!("album button weak ref gone");
                        return;
                    };

                    debug!(count = albums.len(), "albums loaded, building popover");

                    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
                    vbox.set_margin_top(6);
                    vbox.set_margin_bottom(6);
                    vbox.set_margin_start(6);
                    vbox.set_margin_end(6);

                    // Create the popover first so album buttons can close it.
                    let popover = gtk::Popover::new();
                    popover.set_parent(btn.upcast_ref::<gtk::Widget>());

                    if albums.is_empty() {
                        let label = gtk::Label::new(Some("No albums"));
                        label.add_css_class("dim-label");
                        vbox.append(&label);
                    } else {
                        for album in &albums {
                            let ab = gtk::Button::with_label(&album.name);
                            ab.add_css_class("flat");
                            let aid = album.id.clone();
                            let lib_add = Arc::clone(&lib);
                            let tk_add = tk.clone();
                            let reg_add = Rc::clone(&reg);
                            let sel_add = sel.clone();
                            let pop_weak = popover.downgrade();
                            ab.connect_clicked(move |_| {
                                debug!(album_id = %aid, "album selected in popover");
                                let ids = collect_selected_ids(&sel_add);
                                if ids.is_empty() {
                                    debug!("no photos selected, skipping");
                                    return;
                                }
                                debug!(count = ids.len(), album_id = %aid, "adding photos to album");

                                if let Some(p) = pop_weak.upgrade() {
                                    p.popdown();
                                }

                                let lib = Arc::clone(&lib_add);
                                let tk = tk_add.clone();
                                let reg = Rc::clone(&reg_add);
                                let aid = aid.clone();
                                glib::MainContext::default().spawn_local(async move {
                                    let aid_bc = aid.clone();
                                    let result = tk
                                        .spawn(async move { lib.add_to_album(&aid, &ids).await })
                                        .await;
                                    match result {
                                        Ok(Ok(())) => {
                                            debug!(album_id = %aid_bc, "photos added to album");
                                            reg.on_album_media_changed(&aid_bc);
                                        }
                                        Ok(Err(e)) => tracing::error!("add_to_album failed: {e}"),
                                        Err(e) => tracing::error!("add_to_album join failed: {e}"),
                                    }
                                });
                            });
                            vbox.append(&ab);
                        }
                    }

                    popover.set_child(Some(&vbox));

                    popover.connect_closed(move |p| {
                        debug!("album popover closed");
                        p.unparent();
                    });

                    debug!("showing album popover");
                    popover.popup();
                });
            });
        }

        // ── Right-click context menu on grid cells ─────────────────────
        {
            let gesture = gtk::GestureClick::new();
            gesture.set_button(3);

            let grid_view = self.photo_grid.imp().grid_view.get().unwrap().clone();
            let selection = selection.clone();
            let lib = Arc::clone(&self.library);
            let tk = self.tokio.clone();
            let reg = Rc::clone(&registry);
            let filter_for_ctx = self.photo_grid.imp().filter.borrow().clone();

            gesture.connect_pressed(move |gesture, _, x, y| {
                // Find the grid child widget at the click position, then walk up
                // to the direct child of the GridView to determine its index.
                let Some(picked) = grid_view.pick(x, y, gtk::PickFlags::DEFAULT) else { return };

                // Walk up to the direct child of the GridView.
                let grid_widget = grid_view.upcast_ref::<gtk::Widget>();
                let mut target = Some(picked);
                while let Some(ref w) = target {
                    if w.parent().as_ref() == Some(grid_widget) {
                        break;
                    }
                    target = w.parent();
                }
                let Some(target) = target else { return };

                // Find the position by counting siblings.
                let mut pos = 0u32;
                let mut child = grid_view.first_child();
                loop {
                    let Some(c) = child else { return };
                    if c == target { break; }
                    pos += 1;
                    child = c.next_sibling();
                }

                let Some(pos) = Some(pos) else { return };

                // Select the item if not already selected.
                if !selection.is_selected(pos) {
                    selection.unselect_all();
                    selection.select_item(pos, true);
                }

                // Get the item for context.
                let Some(obj) = selection.item(pos)
                    .and_then(|o| o.downcast::<item::MediaItemObject>().ok()) else { return };

                let is_favorite = obj.is_favorite();
                let is_trash = matches!(filter_for_ctx, MediaFilter::Trashed);
                let is_album = matches!(filter_for_ctx, MediaFilter::Album { .. });

                // Build popover with action buttons.
                let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
                vbox.set_margin_top(6);
                vbox.set_margin_bottom(6);
                vbox.set_margin_start(6);
                vbox.set_margin_end(6);

                // Create popover early so buttons can dismiss it.
                let popover = gtk::Popover::new();
                let pop_ref = popover.downgrade();

                if is_trash {
                    // Trash view: Restore, Delete Permanently
                    let restore_btn = gtk::Button::with_label("Restore");
                    restore_btn.add_css_class("flat");
                    vbox.append(&restore_btn);

                    let delete_btn = gtk::Button::with_label("Delete Permanently");
                    delete_btn.add_css_class("flat");
                    delete_btn.add_css_class("error");
                    vbox.append(&delete_btn);

                    let sel = selection.clone();
                    let lib_r = Arc::clone(&lib);
                    let tk_r = tk.clone();
                    let reg_r = Rc::clone(&reg);
                    let pw = pop_ref.clone();
                    restore_btn.connect_clicked(move |_| {
                        if let Some(p) = pw.upgrade() { p.popdown(); }
                        let ids = collect_selected_ids(&sel);
                        if ids.is_empty() { return; }
                        sel.unselect_all();
                        let lib = Arc::clone(&lib_r);
                        let tk = tk_r.clone();
                        let reg = Rc::clone(&reg_r);
                        glib::MainContext::default().spawn_local(async move {
                            let ids_bc = ids.clone();
                            if let Ok(Ok(())) = tk.spawn(async move { lib.restore(&ids).await }).await {
                                for id in &ids_bc { reg.on_trashed(id, false); }
                            }
                        });
                    });

                    let sel = selection.clone();
                    let lib_d = Arc::clone(&lib);
                    let tk_d = tk.clone();
                    let reg_d = Rc::clone(&reg);
                    let pw = pop_ref.clone();
                    delete_btn.connect_clicked(move |_| {
                        if let Some(p) = pw.upgrade() { p.popdown(); }
                        let ids = collect_selected_ids(&sel);
                        if ids.is_empty() { return; }
                        sel.unselect_all();
                        let lib = Arc::clone(&lib_d);
                        let tk = tk_d.clone();
                        let reg = Rc::clone(&reg_d);
                        glib::MainContext::default().spawn_local(async move {
                            let ids_bc = ids.clone();
                            if let Ok(Ok(())) = tk.spawn(async move { lib.delete_permanently(&ids).await }).await {
                                for id in &ids_bc { reg.on_deleted(id); }
                            }
                        });
                    });
                } else {
                    // Non-trash: Favourite toggle, Trash
                    let fav_label = if is_favorite { "Unfavourite" } else { "Favourite" };
                    let fav_btn = gtk::Button::with_label(fav_label);
                    fav_btn.add_css_class("flat");
                    vbox.append(&fav_btn);

                    let trash_ctx_btn = gtk::Button::with_label("Move to Trash");
                    trash_ctx_btn.add_css_class("flat");
                    trash_ctx_btn.add_css_class("error");
                    vbox.append(&trash_ctx_btn);

                    if is_album {
                        let remove_btn = gtk::Button::with_label("Remove from Album");
                        remove_btn.add_css_class("flat");
                        vbox.append(&remove_btn);

                        if let MediaFilter::Album { ref album_id } = filter_for_ctx {
                            let sel = selection.clone();
                            let lib_ra = Arc::clone(&lib);
                            let tk_ra = tk.clone();
                            let reg_ra = Rc::clone(&reg);
                            let aid = album_id.clone();
                            let pw = pop_ref.clone();
                            remove_btn.connect_clicked(move |_| {
                                if let Some(p) = pw.upgrade() { p.popdown(); }
                                let ids = collect_selected_ids(&sel);
                                if ids.is_empty() { return; }
                                sel.unselect_all();
                                let lib = Arc::clone(&lib_ra);
                                let tk = tk_ra.clone();
                                let reg = Rc::clone(&reg_ra);
                                let aid = aid.clone();
                                glib::MainContext::default().spawn_local(async move {
                                    let aid_bc = aid.clone();
                                    if let Ok(Ok(())) = tk.spawn(async move { lib.remove_from_album(&aid, &ids).await }).await {
                                        reg.on_album_media_changed(&aid_bc);
                                    }
                                });
                            });
                        }
                    }

                    let new_fav = !is_favorite;
                    let sel = selection.clone();
                    let lib_f = Arc::clone(&lib);
                    let tk_f = tk.clone();
                    let reg_f = Rc::clone(&reg);
                    let pw = pop_ref.clone();
                    fav_btn.connect_clicked(move |_| {
                        if let Some(p) = pw.upgrade() { p.popdown(); }
                        let ids = collect_selected_ids(&sel);
                        if ids.is_empty() { return; }
                        let lib = Arc::clone(&lib_f);
                        let tk = tk_f.clone();
                        let reg = Rc::clone(&reg_f);
                        glib::MainContext::default().spawn_local(async move {
                            let ids_bc = ids.clone();
                            if let Ok(Ok(())) = tk.spawn(async move { lib.set_favorite(&ids, new_fav).await }).await {
                                for id in &ids_bc { reg.on_favorite_changed(id, new_fav); }
                            }
                        });
                    });

                    let sel = selection.clone();
                    let lib_t = Arc::clone(&lib);
                    let tk_t = tk.clone();
                    let reg_t = Rc::clone(&reg);
                    let pw = pop_ref.clone();
                    trash_ctx_btn.connect_clicked(move |_| {
                        if let Some(p) = pw.upgrade() { p.popdown(); }
                        let ids = collect_selected_ids(&sel);
                        if ids.is_empty() { return; }
                        sel.unselect_all();
                        let lib = Arc::clone(&lib_t);
                        let tk = tk_t.clone();
                        let reg = Rc::clone(&reg_t);
                        glib::MainContext::default().spawn_local(async move {
                            let ids_bc = ids.clone();
                            if let Ok(Ok(())) = tk.spawn(async move { lib.trash(&ids).await }).await {
                                for id in &ids_bc { reg.on_trashed(id, true); }
                            }
                        });
                    });
                }

                // Show the popover on the grid view.
                popover.set_child(Some(&vbox));
                popover.set_parent(&grid_view);
                popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
                popover.set_has_arrow(true);

                popover.connect_closed(move |p| {
                    p.unparent();
                });

                popover.popup();
                gesture.set_state(gtk::EventSequenceState::Claimed);
            });

            self.photo_grid.imp().grid_view.get().unwrap().add_controller(gesture);
        }

        // ── "Remove from Album" button ──────────────────────────────────
        if let MediaFilter::Album { album_id } = filter {
            let selection = selection.clone();
            let lib = Arc::clone(&self.library);
            let tk = self.tokio.clone();
            let reg = Rc::clone(&registry);
            let remove_btn = self.remove_from_album_btn.clone();

            remove_btn.connect_clicked(move |btn| {
                let ids = collect_selected_ids(&selection);
                if ids.is_empty() { return; }
                selection.unselect_all();
                btn.set_sensitive(false);

                let lib = Arc::clone(&lib);
                let tk = tk.clone();
                let reg = Rc::clone(&reg);
                let aid = album_id.clone();
                glib::MainContext::default().spawn_local(async move {
                    let aid_bc = aid.clone();
                    let result = tk
                        .spawn(async move { lib.remove_from_album(&aid, &ids).await })
                        .await;
                    match result {
                        Ok(Ok(())) => {
                            tracing::debug!(album_id = %aid_bc, "photos removed from album");
                            reg.on_album_media_changed(&aid_bc);
                        }
                        Ok(Err(e)) => tracing::error!("remove_from_album failed: {e}"),
                        Err(e) => tracing::error!("remove_from_album join failed: {e}"),
                    }
                });
            });
        }
    }
}

/// Collect media IDs from the current selection.
fn collect_selected_ids(selection: &gtk::MultiSelection) -> Vec<crate::library::media::MediaId> {
    let bitset = selection.selection();
    let n = bitset.size();
    let mut ids = Vec::with_capacity(n as usize);
    for i in 0..n {
        let pos = bitset.nth(i as u32);
        if let Some(obj) = selection
            .item(pos)
            .and_then(|o| o.downcast::<item::MediaItemObject>().ok())
        {
            ids.push(obj.item().id.clone());
        }
    }
    ids
}

impl ContentView for PhotoGridView {
    fn widget(&self) -> &gtk::Widget {
        &self.widget
    }
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

    #[test]
    fn default_zoom_size_is_160() {
        assert_eq!(ZOOM_SIZES[DEFAULT_ZOOM_INDEX], 160);
    }
}
