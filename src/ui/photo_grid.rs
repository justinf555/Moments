use std::cell::RefCell;
use std::rc::Rc;

use gtk::{gio, glib, prelude::*, subclass::prelude::*};
use tracing::instrument;

use crate::ui::ContentView;

pub mod cell;
pub mod factory;
pub mod item;
pub mod model;

pub use model::PhotoGridModel;

mod imp {
    use super::*;
    use std::cell::OnceCell;

    #[derive(Default)]
    pub struct PhotoGrid {
        pub scrolled: OnceCell<gtk::ScrolledWindow>,
        pub grid_view: OnceCell<gtk::GridView>,
        /// Kept alive so lazy-loading stays wired after `set_model`.
        pub model: RefCell<Option<Rc<PhotoGridModel>>>,
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

    /// Attach a `PhotoGridModel` to the grid.
    ///
    /// Wires the model's `ListStore` to `GridView` via `MultiSelection`, builds
    /// the cell factory, triggers the initial page load, and connects
    /// scroll-based lazy loading for subsequent pages.
    #[instrument(skip_all)]
    pub fn set_model(&self, model: Rc<PhotoGridModel>) {
        let imp = self.imp();
        let grid_view = imp.grid_view.get().unwrap();
        let scrolled = imp.scrolled.get().unwrap();

        let selection = gtk::MultiSelection::new(Some(model.store.clone()));
        grid_view.set_model(Some(&selection));
        grid_view.set_factory(Some(&factory::build_factory()));

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

        *imp.model.borrow_mut() = Some(model);
    }
}

/// Wraps `PhotoGrid` in an `AdwToolbarView` + `AdwHeaderBar` so it can be
/// registered as a `ContentView` in the main shell.
pub struct PhotoGridView {
    /// Kept alive so the widget tree stays valid for the lifetime of the view.
    _toolbar_view: adw::ToolbarView,
    photo_grid: PhotoGrid,
    widget: gtk::Widget,
}

impl PhotoGridView {
    pub fn new() -> Self {
        let toolbar_view = adw::ToolbarView::new();
        let header = adw::HeaderBar::new();

        let import_button = gtk::Button::builder()
            .icon_name("list-add-symbolic")
            .tooltip_text("Import Photos")
            .action_name("app.import")
            .build();
        import_button.add_css_class("flat");
        header.pack_start(&import_button);

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

        toolbar_view.add_top_bar(&header);

        let photo_grid = PhotoGrid::new();
        toolbar_view.set_content(Some(&photo_grid));

        let widget = toolbar_view.clone().upcast::<gtk::Widget>();

        Self {
            _toolbar_view: toolbar_view,
            photo_grid,
            widget,
        }
    }

    pub fn set_model(&self, model: Rc<PhotoGridModel>) {
        self.photo_grid.set_model(model);
    }
}

impl ContentView for PhotoGridView {
    fn widget(&self) -> &gtk::Widget {
        &self.widget
    }
}
