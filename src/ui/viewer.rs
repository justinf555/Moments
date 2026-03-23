use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, glib};
use tracing::debug;

use crate::library::media::MediaMetadataRecord;
use crate::library::Library;
use crate::ui::photo_grid::item::MediaItemObject;

pub mod info_panel;

use info_panel::InfoPanel;

// ── Inner state ───────────────────────────────────────────────────────────────

/// All mutable viewer state plus convenience widget handles.
///
/// Wrapped in `Rc<ViewerInner>` so signal-handler closures can share it without
/// unsafe code. Closures hold `Rc::clone(inner)` for async work or
/// `Rc::downgrade(inner)` for purely reactive handlers where outliving the
/// viewer would be a bug.
struct ViewerInner {
    nav_page: adw::NavigationPage,
    picture: gtk::Picture,
    spinner: gtk::Spinner,
    prev_btn: gtk::Button,
    next_btn: gtk::Button,
    info_split: adw::OverlaySplitView,
    info_panel: InfoPanel,
    /// Snapshot of the grid's item list taken at activation time.
    items: RefCell<Vec<MediaItemObject>>,
    current_index: Cell<usize>,
    /// Monotonically increasing counter. Async loads compare against this
    /// value captured at launch to discard stale results.
    load_gen: Cell<u64>,
    /// Cached metadata for the currently displayed item.
    current_metadata: RefCell<Option<MediaMetadataRecord>>,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
}

impl ViewerInner {
    /// Switch to the item at `index`.
    ///
    /// Updates the title, sets the thumbnail immediately, updates navigation
    /// button visibility, and kicks off async loads for full-res and metadata.
    fn show_at(self: &Rc<Self>, index: usize) {
        // Extract what we need before releasing the borrow.
        let (id, filename, texture, count) = {
            let items = self.items.borrow();
            let Some(obj) = items.get(index) else { return };
            (
                obj.item().id.clone(),
                obj.item().original_filename.clone(),
                obj.texture(),
                items.len(),
            )
        };

        self.current_index.set(index);
        let gen = self.load_gen.get() + 1;
        self.load_gen.set(gen);
        *self.current_metadata.borrow_mut() = None;

        // AdwHeaderBar reads the title directly from the NavigationPage.
        self.nav_page.set_title(&filename);

        // Show cached thumbnail while full-res loads.
        self.picture
            .set_paintable(texture.as_ref().map(|t| t.upcast_ref::<gdk::Paintable>()));

        self.prev_btn.set_visible(index > 0);
        self.next_btn.set_visible(index + 1 < count);

        // Collapse info panel to avoid showing stale metadata.
        self.info_split.set_show_sidebar(false);

        self.start_full_res_load(gen, id.clone());
        self.load_metadata_async(gen, id);
    }

    fn navigate_prev(self: &Rc<Self>) {
        let idx = self.current_index.get();
        if idx > 0 {
            self.show_at(idx - 1);
        }
    }

    fn navigate_next(self: &Rc<Self>) {
        let idx = self.current_index.get();
        if idx + 1 < self.items.borrow().len() {
            self.show_at(idx + 1);
        }
    }

    /// Asynchronously load the original file at full resolution.
    ///
    /// Strategy:
    /// 1. Resolve the original path from the library.
    /// 2. Decode via `image::open()` on a blocking thread and upload RGBA
    ///    bytes as a `gdk::MemoryTexture`.
    /// 3. EXIF orientation is always applied before display.
    ///
    /// Falls back silently to the cached thumbnail on any error.
    fn start_full_res_load(
        self: &Rc<Self>,
        gen: u64,
        id: crate::library::media::MediaId,
    ) {
        let inner = Rc::clone(self);
        let library = Arc::clone(&self.library);
        let tokio = self.tokio.clone();

        self.spinner.set_spinning(true);
        self.spinner.set_visible(true);

        glib::MainContext::default().spawn_local(async move {
            // Resolve path on Tokio (async DB call).
            let path = match tokio
                .spawn(async move { library.original_path(&id).await })
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
            {
                Some(p) => p,
                None => {
                    inner.spinner.set_spinning(false);
                    inner.spinner.set_visible(false);
                    return;
                }
            };

            if inner.load_gen.get() != gen {
                inner.spinner.set_spinning(false);
                inner.spinner.set_visible(false);
                return;
            }

            // Decode via `image` crate with EXIF orientation applied.
            let pixels: Option<(Vec<u8>, i32, i32)> = tokio
                .spawn(async move {
                    tokio::task::spawn_blocking(move || -> Option<(Vec<u8>, i32, i32)> {
                        let img = image::open(&path)
                            .map_err(|e| debug!("full-res decode failed: {e}"))
                            .ok()?;
                        let orientation = crate::library::exif::extract_exif(&path)
                            .orientation
                            .unwrap_or(1);
                        let img =
                            crate::library::thumbnailer::apply_orientation(img, orientation);
                        let rgba = img.into_rgba8();
                        let (w, h) = rgba.dimensions();
                        Some((rgba.into_raw(), w as i32, h as i32))
                    })
                    .await
                    .ok()?
                })
                .await
                .ok()
                .flatten();

            inner.spinner.set_spinning(false);
            inner.spinner.set_visible(false);

            if inner.load_gen.get() != gen {
                return;
            }

            if let Some((raw, width, height)) = pixels {
                let gbytes = glib::Bytes::from_owned(raw);
                let texture = gdk::MemoryTexture::new(
                    width,
                    height,
                    gdk::MemoryFormat::R8g8b8a8,
                    &gbytes,
                    (width as usize) * 4,
                )
                .upcast::<gdk::Texture>();
                inner
                    .picture
                    .set_paintable(Some(texture.upcast_ref::<gdk::Paintable>()));
                debug!("full-res via MemoryTexture: {width}×{height}");
            }
        });
    }

    /// Asynchronously fetch EXIF metadata and cache it for the info panel.
    fn load_metadata_async(
        self: &Rc<Self>,
        gen: u64,
        id: crate::library::media::MediaId,
    ) {
        let inner = Rc::clone(self);
        let library = Arc::clone(&self.library);
        let tokio = self.tokio.clone();

        glib::MainContext::default().spawn_local(async move {
            let metadata = tokio
                .spawn(async move { library.media_metadata(&id).await })
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten();

            if inner.load_gen.get() != gen {
                return; // stale
            }

            *inner.current_metadata.borrow_mut() = metadata;

            // If the panel is open, refresh it with the newly arrived metadata.
            if inner.info_split.shows_sidebar() {
                let items = inner.items.borrow();
                let idx = inner.current_index.get();
                if let Some(obj) = items.get(idx) {
                    let item = obj.item().clone();
                    let meta = inner.current_metadata.borrow();
                    inner.info_panel.populate(&item, meta.as_ref());
                }
            }
        });
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Full-resolution photo viewer with prev/next navigation and a metadata panel.
///
/// Designed to be pushed onto an [`adw::NavigationView`] when a grid item is
/// activated. The same `PhotoViewer` instance is reused across activations —
/// call [`PhotoViewer::show`] to load a new item list and navigate to an index.
pub struct PhotoViewer {
    inner: Rc<ViewerInner>,
}

impl PhotoViewer {
    pub fn new(library: Arc<dyn Library>, tokio: tokio::runtime::Handle) -> Self {
        // ── Header bar ───────────────────────────────────────────────────────
        let header = adw::HeaderBar::new();
        let info_toggle = gtk::ToggleButton::builder()
            .icon_name("info-symbolic")
            .tooltip_text("Photo Information")
            .build();
        header.pack_end(&info_toggle);

        // ── Picture ──────────────────────────────────────────────────────────
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .hexpand(true)
            .vexpand(true)
            .build();

        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hexpand(true)
            .vexpand(true)
            .child(&picture)
            .build();

        // ── Spinner (centred over picture) ───────────────────────────────────
        let spinner = gtk::Spinner::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .width_request(32)
            .height_request(32)
            .visible(false)
            .build();

        // ── OSD prev / next buttons ──────────────────────────────────────────
        let prev_btn = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text("Previous Photo")
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Start)
            .margin_start(12)
            .visible(false)
            .build();
        prev_btn.add_css_class("circular");
        prev_btn.add_css_class("osd");

        let next_btn = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text("Next Photo")
            .valign(gtk::Align::Center)
            .halign(gtk::Align::End)
            .margin_end(12)
            .visible(false)
            .build();
        next_btn.add_css_class("circular");
        next_btn.add_css_class("osd");

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&scrolled));
        overlay.add_overlay(&spinner);
        overlay.add_overlay(&prev_btn);
        overlay.add_overlay(&next_btn);

        // ── Info panel ───────────────────────────────────────────────────────
        let info_panel = InfoPanel::new();

        // ── Overlay split view (content | info sidebar) ──────────────────────
        let info_split = adw::OverlaySplitView::new();
        info_split.set_content(Some(&overlay));
        info_split.set_sidebar(Some(info_panel.widget()));
        info_split.set_sidebar_position(gtk::PackType::End);
        info_split.set_show_sidebar(false);

        // ── Toolbar view ─────────────────────────────────────────────────────
        let toolbar_view = adw::ToolbarView::new();
        toolbar_view.add_top_bar(&header);
        toolbar_view.set_content(Some(&info_split));
        toolbar_view.set_focusable(true);

        // ── Navigation page ───────────────────────────────────────────────────
        let nav_page = adw::NavigationPage::builder()
            .tag("viewer")
            .title("Photo")
            .child(&toolbar_view)
            .build();

        // ── Assemble ─────────────────────────────────────────────────────────
        let inner = Rc::new(ViewerInner {
            nav_page,
            picture,
            spinner,
            prev_btn,
            next_btn,
            info_split,
            info_panel,
            items: RefCell::new(Vec::new()),
            current_index: Cell::new(0),
            load_gen: Cell::new(0),
            current_metadata: RefCell::new(None),
            library,
            tokio,
        });

        // ── Signal handlers ───────────────────────────────────────────────────

        // Prev button
        {
            let i = Rc::downgrade(&inner);
            inner.prev_btn.connect_clicked(move |_| {
                if let Some(i) = i.upgrade() {
                    i.navigate_prev();
                }
            });
        }

        // Next button
        {
            let i = Rc::downgrade(&inner);
            inner.next_btn.connect_clicked(move |_| {
                if let Some(i) = i.upgrade() {
                    i.navigate_next();
                }
            });
        }

        // Info toggle → split view
        {
            let split = inner.info_split.clone();
            info_toggle.connect_toggled(move |btn| {
                split.set_show_sidebar(btn.is_active());
            });
        }

        // Split view sidebar visible change → sync toggle + populate if open
        {
            let i = Rc::downgrade(&inner);
            let toggle = info_toggle.clone();
            inner.info_split.connect_show_sidebar_notify(move |split| {
                toggle.set_active(split.shows_sidebar());
                if split.shows_sidebar() {
                    if let Some(inner) = i.upgrade() {
                        let items = inner.items.borrow();
                        let idx = inner.current_index.get();
                        if let Some(obj) = items.get(idx) {
                            let item = obj.item().clone();
                            let meta = inner.current_metadata.borrow();
                            inner.info_panel.populate(&item, meta.as_ref());
                        }
                    }
                }
            });
        }

        // Keyboard navigation (← →)
        {
            let key_ctrl = gtk::EventControllerKey::new();
            toolbar_view.add_controller(key_ctrl.clone());
            let i = Rc::downgrade(&inner);
            key_ctrl.connect_key_pressed(move |_, keyval, _, _| {
                let Some(inner) = i.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                match keyval {
                    gdk::Key::Left | gdk::Key::KP_Left => {
                        inner.navigate_prev();
                        glib::Propagation::Stop
                    }
                    gdk::Key::Right | gdk::Key::KP_Right => {
                        inner.navigate_next();
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }

        Self { inner }
    }

    /// The `NavigationPage` to push onto an [`adw::NavigationView`].
    pub fn nav_page(&self) -> &adw::NavigationPage {
        &self.inner.nav_page
    }

    /// Load `items` and navigate to `index`.
    ///
    /// Replaces the current item list, resets async state, and starts loading
    /// the new photo. Call this every time the user activates a grid item.
    pub fn show(&self, items: Vec<MediaItemObject>, index: usize) {
        *self.inner.items.borrow_mut() = items;
        self.inner.show_at(index);
        // Grab keyboard focus so arrow-key navigation works immediately.
        self.inner.nav_page.grab_focus();
    }
}

