use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib, subclass::prelude::*};
use tracing::instrument;

use crate::app_event::AppEvent;
use crate::library::media::MediaType;
use crate::library::Library;
use crate::ui::video_viewer::VideoViewer;
use crate::ui::viewer::PhotoViewer;
use crate::ui::ContentView;

pub mod action_bar;
pub mod actions;
pub mod cell;
pub mod factory;
pub mod item;
pub mod model;
pub mod texture_cache;

pub use model::PhotoGridModel;

/// Available cell sizes (px), smallest to largest.
const ZOOM_SIZES: &[i32] = &[96, 128, 160, 200, 256, 320];
/// Default zoom level index (160 px).
const DEFAULT_ZOOM_INDEX: usize = 2;

mod imp {
    use super::*;
    use std::cell::OnceCell;

    pub struct PhotoGrid {
        pub content_stack: OnceCell<gtk::Stack>,
        pub scrolled: OnceCell<gtk::ScrolledWindow>,
        pub grid_view: OnceCell<gtk::GridView>,
        pub empty_page: OnceCell<adw::StatusPage>,
        pub selection: RefCell<Option<gtk::MultiSelection>>,
        /// Kept alive so lazy-loading stays wired after `set_model`.
        pub model: RefCell<Option<Rc<PhotoGridModel>>>,
        pub zoom_level: Cell<usize>,
        /// Library reference for the factory (star button persist).
        pub library: OnceCell<Arc<dyn Library>>,
        pub tokio: OnceCell<tokio::runtime::Handle>,
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
                model: RefCell::default(),
                zoom_level: Cell::new(DEFAULT_ZOOM_INDEX),
                library: OnceCell::default(),
                tokio: OnceCell::default(),
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
            stack.set_visible_child_name("grid");
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
        let bus_sender = imp.bus_sender.get().unwrap().clone();
        let filter = imp.filter.borrow().clone();
        let cache = imp.texture_cache.get().unwrap().clone();
        let sm = Rc::clone(&imp.selection_mode);
        let selection = imp.selection.borrow().clone().unwrap();
        let enter = imp.enter_selection.borrow().clone().unwrap();
        grid_view.set_factory(Some(&factory::build_factory(
            self.current_cell_size(),
            library,
            tokio,
            bus_sender.clone(),
            filter,
            cache,
            sm,
            selection,
            enter,
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
        bus_sender: crate::event_bus::EventSender,
        filter: crate::library::media::MediaFilter,
        cache: Rc<texture_cache::TextureCache>,
        on_activate: impl Fn(Vec<item::MediaItemObject>, usize) + 'static,
    ) {
        let imp = self.imp();
        let _ = imp.library.set(Arc::clone(&library));
        let _ = imp.tokio.set(tokio.clone());
        let _ = imp.bus_sender.set(bus_sender.clone());
        let _ = imp.texture_cache.set(Rc::clone(&cache));
        *imp.filter.borrow_mut() = filter.clone();

        let grid_view = imp.grid_view.get().unwrap();
        let scrolled = imp.scrolled.get().unwrap();

        let selection = gtk::MultiSelection::new(Some(model.store.clone()));
        grid_view.set_model(Some(&selection));
        *imp.selection.borrow_mut() = Some(selection.clone());

        let sm = Rc::clone(&imp.selection_mode);
        let enter = imp.enter_selection.borrow().clone().unwrap();
        grid_view.set_factory(Some(&factory::build_factory(
            self.current_cell_size(),
            Arc::clone(&library),
            tokio,
            bus_sender,
            filter.clone(),
            cache,
            sm,
            selection.clone(),
            enter,
        )));

        // Configure empty state message based on filter.
        let empty_page = imp.empty_page.get().unwrap();
        let stack = imp.content_stack.get().unwrap();
        set_empty_state_for_filter(empty_page, &filter);

        // Show/hide empty state based on model item count.
        // Shared closure: called from items_changed (when items are added/
        // removed) and from on_page_ready (covers the case where load_more
        // returns 0 items and items_changed never fires).
        let update_empty: Rc<dyn Fn()> = {
            let stack = stack.clone();
            let store = model.store.clone();
            Rc::new(move || {
                let name = if store.n_items() == 0 { "empty" } else { "grid" };
                stack.set_visible_child_name(name);
            })
        };
        {
            let update = Rc::clone(&update_empty);
            model.store.connect_items_changed(move |_, _, _, _| update());
        }

        // Fetch the first page immediately.
        model.load_more();

        // Load further pages as the user scrolls toward the bottom.
        let model_scroll = Rc::clone(&model);
        let adj = scrolled.vadjustment();
        adj.connect_value_changed(move |adj| {
            let visible_end = adj.value() + adj.page_size();
            let trigger_point = adj.upper() * 0.75;
            if visible_end >= trigger_point {
                model_scroll.load_more();
            }
        });

        // After each page loads, re-check whether more pages are needed
        // and update the empty state.
        let model_ready = Rc::clone(&model);
        let adj_ready = scrolled.vadjustment();
        let update_on_ready = Rc::clone(&update_empty);
        model.set_on_page_ready(move || {
            update_on_ready();
            let visible_end = adj_ready.value() + adj_ready.page_size();
            let trigger_point = adj_ready.upper() * 0.75;
            if visible_end >= trigger_point {
                model_ready.load_more();
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
    texture_cache: Rc<texture_cache::TextureCache>,
    widget: gtk::Widget,
    view_actions: gio::SimpleActionGroup,
    /// Shared selection mode flag — read by factory closures.
    selection_mode: Rc<Cell<bool>>,
    /// Selection mode exit action — triggered by cancel, escape, or auto-exit.
    exit_selection: gio::SimpleAction,
    /// Selection count label shown in selection mode headerbar.
    selection_title: gtk::Label,
    /// Action bar — kept alive; buttons rebuilt per-filter in `set_model`.
    #[allow(dead_code)]
    action_bar: gtk::ActionBar,
    /// Box inside the action bar holding the current buttons.
    bar_box: gtk::Box,
    /// Current favourite button (if any) — for dynamic label updates.
    fav_btn: RefCell<Option<gtk::Button>>,
    /// Bus sender for action bar commands.
    bus_sender: crate::event_bus::EventSender,
}

impl PhotoGridView {
    pub fn new(
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        settings: gio::Settings,
        texture_cache: Rc<texture_cache::TextureCache>,
        bus_sender: crate::event_bus::EventSender,
    ) -> Self {
        // ── Grid header bar ──────────────────────────────────────────────────
        let header = adw::HeaderBar::new();

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

        // ── Content overflow menu (⋮) ────────────────────────────────────
        let content_menu = gio::Menu::new();
        let content_section = gio::Menu::new();
        content_section.append(Some("_Select"), Some("view.enter-selection"));
        content_menu.append_section(None, &content_section);

        let content_menu_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Menu")
            .menu_model(&content_menu)
            .build();
        content_menu_btn.add_css_class("flat");
        header.pack_end(&content_menu_btn);

        // ── Selection mode header widgets (hidden by default) ────────────────
        let cancel_btn = gtk::Button::with_label("Cancel");
        cancel_btn.add_css_class("outlined");
        cancel_btn.set_visible(false);
        header.pack_start(&cancel_btn);

        let selection_title = gtk::Label::new(Some("0 selected"));
        selection_title.add_css_class("heading");
        selection_title.set_visible(false);

        // ── Action bar (bottom, revealed in selection mode) ──────────────────
        // Buttons are built per-filter in set_model via ActionBarFactory.
        let action_bar = gtk::ActionBar::new();
        action_bar.set_revealed(false);
        action_bar.add_css_class("photo-action-bar");
        let bar_box = gtk::Box::new(gtk::Orientation::Horizontal, 24);
        bar_box.set_halign(gtk::Align::Center);
        action_bar.set_center_widget(Some(&bar_box));

        // ── Grid toolbar view (root nav page content) ────────────────────────
        let photo_grid = PhotoGrid::new();
        photo_grid.set_zoom_level(settings.uint("zoom-level") as usize);
        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.add_bottom_bar(&action_bar);
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
        let photo_viewer = Rc::new(PhotoViewer::new(Arc::clone(&library), tokio.clone(), bus_sender.clone()));
        let video_viewer = Rc::new(VideoViewer::new(Arc::clone(&library), tokio.clone(), bus_sender.clone()));

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

        // ── Selection mode actions ──────────────────────────────────────────
        let selection_mode = Rc::clone(&photo_grid.imp().selection_mode);

        let enter_selection = gio::SimpleAction::new("enter-selection", None);
        {
            let sm = Rc::clone(&selection_mode);
            let zoom_box = zoom_box.clone();
            let content_menu_btn = content_menu_btn.clone();
            let cancel_btn = cancel_btn.clone();
            let selection_title = selection_title.clone();
            let action_bar = action_bar.clone();
            let header = header.clone();
            let grid = photo_grid.clone();
            enter_selection.connect_activate(move |_, _| {
                sm.set(true);
                zoom_box.set_visible(false);
                content_menu_btn.set_visible(false);
                cancel_btn.set_visible(true);
                selection_title.set_visible(true);
                header.set_title_widget(Some(&selection_title));
                action_bar.set_revealed(true);

                // Show checkboxes and selection border on all visible cells.
                let grid_view = grid.imp().grid_view.get().unwrap();
                grid_view.add_css_class("selection-active");
                let mut child = grid_view.first_child();
                while let Some(c) = child {
                    if let Some(cell) = c.first_child()
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
            let zoom_box = zoom_box.clone();
            let content_menu_btn = content_menu_btn.clone();
            let cancel_btn = cancel_btn.clone();
            let selection_title = selection_title.clone();
            let action_bar = action_bar.clone();
            let header = header.clone();
            let grid = photo_grid.clone();
            exit_selection.connect_activate(move |_, _| {
                sm.set(false);
                zoom_box.set_visible(true);
                content_menu_btn.set_visible(true);
                cancel_btn.set_visible(false);
                selection_title.set_visible(false);
                header.set_title_widget(None::<&gtk::Widget>);
                action_bar.set_revealed(false);

                // Clear selection.
                if let Some(ref sel) = *grid.imp().selection.borrow() {
                    sel.unselect_all();
                }

                // Hide checkboxes and selection border on all visible cells.
                let grid_view = grid.imp().grid_view.get().unwrap();
                grid_view.remove_css_class("selection-active");
                let mut child = grid_view.first_child();
                while let Some(c) = child {
                    if let Some(cell) = c.first_child()
                        .and_then(|w| w.downcast::<cell::PhotoGridCell>().ok())
                    {
                        cell.set_selection_mode(false);
                    }
                    child = c.next_sibling();
                }
            });
        }

        // Cancel button wires to exit-selection.
        {
            let exit = exit_selection.clone();
            cancel_btn.connect_clicked(move |_| {
                exit.activate(None);
            });
        }

        action_group.add_action(&enter_selection);
        action_group.add_action(&exit_selection);

        // Store on the inner grid so apply_zoom/set_model can access them.
        *photo_grid.imp().enter_selection.borrow_mut() = Some(enter_selection.clone());

        let widget = nav_view.clone().upcast::<gtk::Widget>();

        Self {
            nav_view,
            photo_grid,
            photo_viewer,
            video_viewer,
            library,
            tokio,
            texture_cache,
            widget,
            view_actions: action_group,
            selection_mode,
            exit_selection,
            selection_title,
            action_bar,
            bar_box,
            fav_btn: RefCell::new(None),
            bus_sender,
        }
    }

    pub fn set_model(&self, model: Rc<PhotoGridModel>) {
        let filter = model.filter();
        self.photo_grid.set_model(
            Rc::clone(&model),
            Arc::clone(&self.library),
            self.tokio.clone(),
            self.bus_sender.clone(),
            filter.clone(),
            Rc::clone(&self.texture_cache),
            {
                let nav_view = self.nav_view.clone();
                let photo_viewer = Rc::clone(&self.photo_viewer);
                let photo_nav_page = self.photo_viewer.nav_page().clone();
                let video_viewer = Rc::clone(&self.video_viewer);
                let video_nav_page = self.video_viewer.nav_page().clone();
                move |items, index| {
                    let media_type = items
                        .get(index)
                        .map(|obj| obj.item().media_type)
                        .unwrap_or(MediaType::Image);

                    let filename = items
                        .get(index)
                        .map(|obj| obj.item().original_filename.clone())
                        .unwrap_or_default();

                    tracing::debug!(index, ?media_type, %filename, "grid item activated");

                    let (tag, nav_page) = if media_type == MediaType::Video {
                        video_viewer.show(items, index);
                        ("video-viewer", &video_nav_page)
                    } else {
                        photo_viewer.show(items, index);
                        ("viewer", &photo_nav_page)
                    };

                    let visible_tag = nav_view
                        .visible_page()
                        .and_then(|p| p.tag())
                        .unwrap_or_default();
                    tracing::debug!(target_tag = tag, %visible_tag, "pushing viewer page");
                    if visible_tag != tag {
                        nav_view.push(nav_page);
                    }
                }
            },
        );

        let selection = self.photo_grid.imp().selection.borrow().clone().unwrap();
        let grid_view = self.photo_grid.imp().grid_view.get().unwrap().clone();

        let ctx = actions::ActionContext {
            selection,
            library: Arc::clone(&self.library),
            tokio: self.tokio.clone(),
            filter: filter.clone(),
            grid_view,
            bus_sender: self.bus_sender.clone(),
        };

        actions::wire_context_menu(&ctx);

        // ── Build action bar buttons for this filter ────────────────────────
        // Clear previous buttons.
        while let Some(child) = self.bar_box.first_child() {
            self.bar_box.remove(&child);
        }

        let bar_buttons = action_bar::build_for_filter(
            &filter,
            &ctx.selection,
            &self.bus_sender,
        );
        self.bar_box.append(&bar_buttons.container);
        *self.fav_btn.borrow_mut() = bar_buttons.fav_btn;

        // Wire "Add to album" popover — requires library queries, so it
        // uses the old ActionContext wiring until album commands are migrated.
        if let Some(ref album_btn) = bar_buttons.album_btn {
            actions::wire_album_controls(&ctx, album_btn);
        }

        // Subscribe for exit-selection on destructive result events.
        {
            let exit = self.exit_selection.clone();
            crate::event_bus::subscribe(move |event| {
                match event {
                    AppEvent::Trashed { .. }
                    | AppEvent::Deleted { .. }
                    | AppEvent::Restored { .. }
                    | AppEvent::AlbumMediaChanged { .. } => {
                        exit.activate(None);
                    }
                    _ => {}
                }
            });
        }

        // ── Selection changed → update count, auto-exit ─────────────────────
        {
            let sm = Rc::clone(&self.selection_mode);
            let exit = self.exit_selection.clone();
            let title = self.selection_title.clone();
            let fav_btn = self.fav_btn.borrow().clone();
            ctx.selection.connect_selection_changed(move |sel, _, _| {
                let count = sel.selection().size();
                let text = match count {
                    0 => "0 selected".to_string(),
                    1 => "1 selected".to_string(),
                    n => format!("{n} selected"),
                };
                title.set_label(&text);

                // Update favourite button if present.
                if let Some(ref fav) = fav_btn {
                    if count > 0 {
                        let bitset = sel.selection();
                        let all_fav = (0..bitset.size() as u32).all(|i| {
                            sel.item(bitset.nth(i))
                                .and_then(|o| o.downcast::<item::MediaItemObject>().ok())
                                .map(|o| o.is_favorite())
                                .unwrap_or(false)
                        });
                        actions::update_fav_button(fav, all_fav);
                    }
                }

                // Auto-exit selection mode when last item deselected.
                if count == 0 && sm.get() {
                    exit.activate(None);
                }
            });
        }
    }
}

/// Collect media IDs from the current selection.
pub(super) fn collect_selected_ids(selection: &gtk::MultiSelection) -> Vec<crate::library::media::MediaId> {
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

/// Configure the empty state status page for the given filter.
fn set_empty_state_for_filter(
    page: &adw::StatusPage,
    filter: &crate::library::media::MediaFilter,
) {
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

impl ContentView for PhotoGridView {
    fn widget(&self) -> &gtk::Widget {
        &self.widget
    }

    fn view_actions(&self) -> Option<&gio::SimpleActionGroup> {
        Some(&self.view_actions)
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
