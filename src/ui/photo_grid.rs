use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib, subclass::prelude::*};
use tracing::instrument;

use crate::library::Library;
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
        grid_view.set_factory(Some(&factory::build_factory(
            self.current_cell_size(),
            library,
            tokio,
            registry,
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
        on_activate: impl Fn(Vec<item::MediaItemObject>, usize) + 'static,
    ) {
        let imp = self.imp();
        let _ = imp.library.set(Arc::clone(&library));
        let _ = imp.tokio.set(tokio.clone());
        let _ = imp.registry.set(Rc::clone(&registry));

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
    viewer: Rc<PhotoViewer>,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    trash_btn: gtk::Button,
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

        // ── Trash button (enabled when items are selected) ──────────────
        let trash_btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Move to Trash")
            .sensitive(false)
            .build();
        trash_btn.add_css_class("flat");
        header.pack_end(&trash_btn);

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

        // ── Viewer (reused across activations) ───────────────────────────────
        let viewer = Rc::new(PhotoViewer::new(Arc::clone(&library), tokio.clone(), Rc::clone(&registry)));

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
            viewer,
            library,
            tokio,
            trash_btn,
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
        let viewer = Rc::clone(&self.viewer);
        let viewer_nav_page = self.viewer.nav_page().clone();

        self.photo_grid.set_model(
            Rc::clone(&model),
            Arc::clone(&self.library),
            self.tokio.clone(),
            Rc::clone(&registry),
            move |items, index| {
                viewer.show(items, index);

                // Push viewer page if it isn't already the visible page.
                let visible_tag = nav_view
                    .visible_page()
                    .and_then(|p| p.tag())
                    .unwrap_or_default();
                if visible_tag != "viewer" {
                    nav_view.push(&viewer_nav_page);
                }
            },
        );

        // Wire trash toolbar button to selection.
        let selection = self.photo_grid.imp().selection.borrow().clone().unwrap();
        let trash_btn = self.trash_btn.clone();
        selection.connect_selection_changed(move |sel, _, _| {
            let has_selection = sel.selection().size() > 0;
            trash_btn.set_sensitive(has_selection);
        });

        // Trash button click → trash all selected items.
        let selection = self.photo_grid.imp().selection.borrow().clone().unwrap();
        let lib = Arc::clone(&self.library);
        let tk = self.tokio.clone();
        let trash_btn = self.trash_btn.clone();
        trash_btn.connect_clicked(move |btn| {
            let bitset = selection.selection();
            let n = bitset.size();
            if n == 0 {
                return;
            }

            // Collect selected item IDs.
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

            // Clear selection and disable button.
            selection.unselect_all();
            btn.set_sensitive(false);

            let lib = Arc::clone(&lib);
            let tk = tk.clone();
            let reg = Rc::clone(&registry);
            glib::MainContext::default().spawn_local(async move {
                let ids_for_broadcast = ids.clone();
                let result = tk
                    .spawn(async move { lib.trash(&ids).await })
                    .await;
                match result {
                    Ok(Ok(())) => {
                        for id in &ids_for_broadcast {
                            reg.on_trashed(id, true);
                        }
                    }
                    Ok(Err(e)) => tracing::error!("trash failed: {e}"),
                    Err(e) => tracing::error!("trash join failed: {e}"),
                }
            });
        });
    }
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
