use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, glib};
use tracing::{debug, error};

use crate::library::media::MediaMetadataRecord;
use crate::library::Library;
use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::ui::photo_grid::item::MediaItemObject;

pub mod edit_panel;
pub mod info_panel;

use edit_panel::EditPanel;
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
    star_btn: gtk::Button,
    info_split: adw::OverlaySplitView,
    info_panel: InfoPanel,
    edit_panel: EditPanel,
    /// Stack in the sidebar to switch between info and edit panels.
    sidebar_stack: gtk::Stack,
    info_toggle: gtk::ToggleButton,
    edit_toggle: gtk::ToggleButton,
    /// Snapshot of the grid's item list taken at activation time.
    items: RefCell<Vec<MediaItemObject>>,
    current_index: Cell<usize>,
    /// Monotonically increasing counter. Async loads compare against this
    /// value captured at launch to discard stale results.
    load_gen: Cell<u64>,
    /// Set by `show_at` when the viewer is being pushed onto the
    /// NavigationView. The `shown` signal handler reads this to start
    /// the full-res load after the slide-in animation completes.
    /// `None` when no deferred load is pending.
    pending_load: RefCell<Option<crate::library::media::MediaId>>,
    /// Cached metadata for the currently displayed item.
    current_metadata: RefCell<Option<MediaMetadataRecord>>,
    /// Tracks a pending optimistic favourite toggle for rollback on failure.
    /// Contains `(media_id, previous_favourite_state)`.
    pending_fav: RefCell<Option<(crate::library::media::MediaId, bool)>>,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    bus_sender: EventSender,
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

        // Sync star button with the current item's favourite state.
        {
            let items = self.items.borrow();
            if let Some(obj) = items.get(index) {
                let is_fav = obj.is_favorite();
                self.star_btn.set_icon_name(if is_fav {
                    "starred-symbolic"
                } else {
                    "non-starred-symbolic"
                });
                if is_fav {
                    self.star_btn.add_css_class("warning");
                } else {
                    self.star_btn.remove_css_class("warning");
                }
            }
        }

        // Collapse info panel to avoid showing stale metadata.
        self.info_split.set_show_sidebar(false);

        // Defer full-res load until the page transition completes (shown
        // signal) to avoid a stutter as the large image replaces the
        // thumbnail mid-animation. If the page is already visible (e.g.
        // prev/next navigation), start immediately.
        if self.nav_page.is_mapped() {
            self.start_full_res_load(gen, id.clone());
            self.load_metadata_async(gen, id);
        } else {
            *self.pending_load.borrow_mut() = Some(id);
        }
    }

    fn navigate_prev(self: &Rc<Self>) {
        let items = self.items.borrow();
        let mut idx = self.current_index.get();
        // Skip video items — they belong in VideoViewer.
        while idx > 0 {
            idx -= 1;
            if items.get(idx).map(|o| o.item().media_type != crate::library::media::MediaType::Video).unwrap_or(false) {
                drop(items);
                self.show_at(idx);
                return;
            }
        }
    }

    fn navigate_next(self: &Rc<Self>) {
        let items = self.items.borrow();
        let len = items.len();
        let mut idx = self.current_index.get();
        // Skip video items — they belong in VideoViewer.
        while idx + 1 < len {
            idx += 1;
            if items.get(idx).map(|o| o.item().media_type != crate::library::media::MediaType::Video).unwrap_or(false) {
                drop(items);
                self.show_at(idx);
                return;
            }
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

            // Guard: skip decode for video files (they use VideoViewer).
            let is_video = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| crate::library::format::registry::VIDEO_EXTENSIONS.contains(&e.to_lowercase().as_str()))
                .unwrap_or(false);
            if is_video {
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
                        // Skip orientation for HEIC/HEIF — libheif applies it
                        // automatically during decode. Applying again would
                        // double-rotate.
                        let ext = path
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.to_lowercase())
                            .unwrap_or_default();
                        let img = if matches!(ext.as_str(), "heic" | "heif") {
                            img
                        } else {
                            let orientation = crate::library::exif::extract_exif(&path)
                                .orientation
                                .unwrap_or(1);
                            crate::library::thumbnailer::apply_orientation(img, orientation)
                        };
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

    /// Start an edit session by loading the full-res image and existing edit state.
    fn start_edit_session(self: &Rc<Self>) {
        let id = {
            let items = self.items.borrow();
            let idx = self.current_index.get();
            items.get(idx).map(|obj| obj.item().id.clone())
        };
        let Some(id) = id else { return };

        let inner = Rc::clone(self);
        let library = Arc::clone(&self.library);
        let tokio = self.tokio.clone();
        let id_for_state = id.clone();

        glib::MainContext::default().spawn_local(async move {
            // Load existing edit state and the original image in parallel.
            let lib = Arc::clone(&library);
            let tk = tokio.clone();

            let state_result = tk
                .spawn({
                    let lib = Arc::clone(&lib);
                    let id = id_for_state.clone();
                    async move { lib.get_edit_state(&id).await }
                })
                .await;

            let path = tk
                .spawn({
                    let lib = Arc::clone(&lib);
                    let id = id.clone();
                    async move { lib.original_path(&id).await }
                })
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten();

            let Some(path) = path else {
                error!("could not resolve original path for edit session");
                return;
            };

            // Decode the full-res image and create a downscaled preview on
            // a blocking thread so the GTK thread is free for the sidebar
            // slide-in animation.
            let preview = tk
                .spawn(async move {
                    tokio::task::spawn_blocking(move || -> Option<Arc<image::DynamicImage>> {
                        let img = image::open(&path)
                            .map_err(|e| error!("edit session decode failed: {e}"))
                            .ok()?;
                        // Apply EXIF orientation (skip for HEIC).
                        let ext = path
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| e.to_lowercase())
                            .unwrap_or_default();
                        let img = if matches!(ext.as_str(), "heic" | "heif") {
                            img
                        } else {
                            let orientation = crate::library::exif::extract_exif(&path)
                                .orientation
                                .unwrap_or(1);
                            crate::library::thumbnailer::apply_orientation(img, orientation)
                        };
                        // Downscale to ~1200px for fast preview rendering.
                        let (w, h) = image::GenericImageView::dimensions(&img);
                        let preview = if w <= 1200 && h <= 1200 {
                            img
                        } else {
                            img.thumbnail(1200, 1200)
                        };
                        Some(Arc::new(preview))
                    })
                    .await
                    .ok()?
                })
                .await
                .ok()
                .flatten();

            let Some(preview) = preview else {
                error!("failed to decode image for edit session");
                return;
            };

            let existing_state = state_result.ok().and_then(|r| r.ok()).flatten();

            inner.edit_panel.begin_session(
                id,
                preview,
                existing_state,
            );
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
    pub fn new(library: Arc<dyn Library>, tokio: tokio::runtime::Handle, bus_sender: EventSender) -> Self {
        // ── Header bar ───────────────────────────────────────────────────────
        //
        // Layout (pack_end is right-to-left):
        //   start: [← back]
        //   end:   [★] [ℹ] [✏] [⋮]
        //
        // Album, Share, Export, Wallpaper, Show in Files, and Delete
        // live in the overflow menu (⋮).
        let header = adw::HeaderBar::new();

        // ── Overflow menu (far right) ────────────────────────────────────
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Menu")
            .build();
        let menu_popover = build_viewer_menu_popover(true, "Delete photo");
        menu_btn.set_popover(Some(&menu_popover));
        header.pack_end(&menu_btn);

        // ── Edit toggle ─────────────────────────────────────────────────
        let edit_toggle = gtk::ToggleButton::builder()
            .icon_name("document-edit-symbolic")
            .tooltip_text("Edit Photo")
            .build();
        #[cfg(feature = "editing")]
        header.pack_end(&edit_toggle);

        // ── Info toggle ─────────────────────────────────────────────────
        let info_toggle = gtk::ToggleButton::builder()
            .icon_name("dialog-information-symbolic")
            .tooltip_text("Photo Information (F9)")
            .build();
        header.pack_end(&info_toggle);

        // ── Favourite ───────────────────────────────────────────────────
        let star_btn = gtk::Button::builder()
            .icon_name("non-starred-symbolic")
            .tooltip_text("Toggle Favourite")
            .build();
        star_btn.add_css_class("flat");
        header.pack_end(&star_btn);

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

        // ── Edit panel ──────────────────────────────────────────────────────
        let edit_panel = EditPanel::new(
            picture.clone(),
            Arc::clone(&library),
            tokio.clone(),
        );

        // ── Sidebar stack (info | edit) ──────────────────────────────────────
        let sidebar_stack = gtk::Stack::new();
        sidebar_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
        sidebar_stack.add_named(info_panel.widget(), Some("info"));
        sidebar_stack.add_named(edit_panel.widget(), Some("edit"));

        // ── Overlay split view (content | sidebar stack) ─────────────────────
        let info_split = adw::OverlaySplitView::new();
        info_split.set_content(Some(&overlay));
        info_split.set_sidebar(Some(&sidebar_stack));
        info_split.set_sidebar_position(gtk::PackType::End);
        info_split.set_show_sidebar(false);
        info_split.set_min_sidebar_width(340.0);
        info_split.set_max_sidebar_width(400.0);

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
            star_btn,
            info_split,
            info_panel,
            edit_panel,
            sidebar_stack,
            info_toggle: info_toggle.clone(),
            edit_toggle: edit_toggle.clone(),
            items: RefCell::new(Vec::new()),
            current_index: Cell::new(0),
            load_gen: Cell::new(0),
            pending_load: RefCell::new(None),
            current_metadata: RefCell::new(None),
            pending_fav: RefCell::new(None),
            library,
            tokio,
            bus_sender,
        });

        // ── Signal handlers ───────────────────────────────────────────────────

        // Start deferred full-res load after the slide-in animation completes.
        {
            let i = Rc::downgrade(&inner);
            inner.nav_page.connect_shown(move |_| {
                let Some(inner) = i.upgrade() else { return };
                let pending = inner.pending_load.borrow_mut().take();
                if let Some(id) = pending {
                    let gen = inner.load_gen.get();
                    inner.start_full_res_load(gen, id.clone());
                    inner.load_metadata_async(gen, id);
                }
            });
        }

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

        // Star (favourite) button — optimistic toggle with rollback on failure.
        {
            let i = Rc::downgrade(&inner);
            inner.star_btn.connect_clicked(move |btn| {
                let Some(inner) = i.upgrade() else { return };
                let items = inner.items.borrow();
                let idx = inner.current_index.get();
                let Some(obj) = items.get(idx) else { return };

                let was_fav = obj.is_favorite();
                let new_fav = !was_fav;

                // Optimistic: update icon and current item immediately.
                btn.set_icon_name(if new_fav {
                    "starred-symbolic"
                } else {
                    "non-starred-symbolic"
                });
                if new_fav {
                    btn.add_css_class("warning");
                } else {
                    btn.remove_css_class("warning");
                }
                obj.set_is_favorite(new_fav);

                let id = obj.item().id.clone();
                *inner.pending_fav.borrow_mut() = Some((id.clone(), was_fav));

                inner.bus_sender.send(AppEvent::FavoriteRequested {
                    ids: vec![id],
                    state: new_fav,
                });
            });
        }

        // Info toggle → show info sidebar
        {
            let i = Rc::downgrade(&inner);
            info_toggle.connect_toggled(move |btn| {
                let Some(inner) = i.upgrade() else { return };
                if btn.is_active() {
                    // Deactivate edit toggle (mutually exclusive).
                    inner.edit_toggle.set_active(false);
                    inner.sidebar_stack.set_visible_child_name("info");
                    inner.info_split.set_show_sidebar(true);

                    // Populate info panel.
                    let items = inner.items.borrow();
                    let idx = inner.current_index.get();
                    if let Some(obj) = items.get(idx) {
                        let item = obj.item().clone();
                        let meta = inner.current_metadata.borrow();
                        inner.info_panel.populate(&item, meta.as_ref());
                    }
                } else if !inner.edit_toggle.is_active() {
                    inner.info_split.set_show_sidebar(false);
                }
            });
        }

        // Edit toggle → show edit sidebar and start edit session
        {
            let i = Rc::downgrade(&inner);
            edit_toggle.connect_toggled(move |btn| {
                let Some(inner) = i.upgrade() else { return };
                if btn.is_active() {
                    // Deactivate info toggle (mutually exclusive).
                    inner.info_toggle.set_active(false);
                    inner.sidebar_stack.set_visible_child_name("edit");
                    inner.info_split.set_show_sidebar(true);

                    // Start edit session — load original image for preview.
                    inner.start_edit_session();
                } else {
                    if !inner.info_toggle.is_active() {
                        inner.info_split.set_show_sidebar(false);
                    }
                    inner.edit_panel.end_session();
                }
            });
        }

        // Split view sidebar closed externally → sync toggles
        {
            let i = Rc::downgrade(&inner);
            inner.info_split.connect_show_sidebar_notify(move |split| {
                if !split.shows_sidebar() {
                    if let Some(inner) = i.upgrade() {
                        inner.info_toggle.set_active(false);
                        if inner.edit_toggle.is_active() {
                            inner.edit_toggle.set_active(false);
                            inner.edit_panel.end_session();
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
                    gdk::Key::F9 => {
                        let active = inner.info_toggle.is_active();
                        inner.info_toggle.set_active(!active);
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }

        // ── Wire overflow menu buttons ──────────────────────────────────
        {
            let popover = menu_popover;

            // Add to album
            if let Some(btn) = find_menu_button(&popover, "add-to-album") {
                let i = Rc::downgrade(&inner);
                let mb = menu_btn.clone();
                let pop = popover.downgrade();
                btn.connect_clicked(move |_| {
                    if let Some(p) = pop.upgrade() { p.popdown(); }
                    let Some(inner) = i.upgrade() else { return };
                    let id = {
                        let items = inner.items.borrow();
                        let idx = inner.current_index.get();
                        items.get(idx).map(|obj| obj.item().id.clone())
                    };
                    let Some(id) = id else { return };
                    crate::ui::album_picker_dialog::show_album_picker_dialog(
                        mb.upcast_ref::<gtk::Widget>(),
                        vec![id],
                        Arc::clone(&inner.library),
                        inner.tokio.clone(),
                        inner.bus_sender.clone(),
                    );
                });
            }

            // Stub items — just close the popover on click.
            for name in &["share", "export-original", "set-wallpaper", "show-in-files"] {
                if let Some(btn) = find_menu_button(&popover, name) {
                    let pop = popover.downgrade();
                    btn.connect_clicked(move |_| {
                        if let Some(p) = pop.upgrade() { p.popdown(); }
                    });
                }
            }

            // Delete photo — trash + pop back to grid.
            if let Some(btn) = find_menu_button(&popover, "delete") {
                let i = Rc::downgrade(&inner);
                let pop = popover.downgrade();
                btn.connect_clicked(move |_| {
                    if let Some(p) = pop.upgrade() { p.popdown(); }
                    let Some(inner) = i.upgrade() else { return };
                    let id = {
                        let items = inner.items.borrow();
                        let idx = inner.current_index.get();
                        items.get(idx).map(|obj| obj.item().id.clone())
                    };
                    let Some(id) = id else { return };
                    inner.bus_sender.send(AppEvent::TrashRequested {
                        ids: vec![id],
                    });
                    if let Some(nav_view) = inner.nav_page
                        .parent()
                        .and_then(|p| p.downcast::<adw::NavigationView>().ok())
                    {
                        nav_view.pop();
                    }
                });
            }
        }

        // Subscribe to bus for favourite rollback on failure.
        {
            let i = Rc::downgrade(&inner);
            crate::event_bus::subscribe(move |event| {
                let Some(inner) = i.upgrade() else { return };
                match event {
                    AppEvent::FavoriteChanged { .. } => {
                        // Confirmed — clear the pending state.
                        *inner.pending_fav.borrow_mut() = None;
                    }
                    AppEvent::Error(_) => {
                        // If we have a pending favourite toggle, roll it back.
                        let pending = inner.pending_fav.borrow_mut().take();
                        if let Some((id, was_fav)) = pending {
                            let items = inner.items.borrow();
                            let idx = inner.current_index.get();
                            if let Some(obj) = items.get(idx) {
                                if obj.item().id == id {
                                    obj.set_is_favorite(was_fav);
                                    inner.star_btn.set_icon_name(if was_fav {
                                        "starred-symbolic"
                                    } else {
                                        "non-starred-symbolic"
                                    });
                                    if was_fav {
                                        inner.star_btn.add_css_class("warning");
                                    } else {
                                        inner.star_btn.remove_css_class("warning");
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
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

/// Build the overflow menu popover content for photo/video viewers.
///
/// `include_wallpaper` controls whether "Set as wallpaper" is shown
/// (photos only, not videos). `delete_label` sets the destructive
/// action label ("Delete photo" vs "Delete video").
pub fn build_viewer_menu_popover(include_wallpaper: bool, delete_label: &str) -> gtk::Popover {
    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.set_margin_top(6);
    vbox.set_margin_bottom(6);
    vbox.set_margin_start(6);
    vbox.set_margin_end(6);

    // Section 1: actions
    vbox.append(&overflow_btn("Add to album", "folder-new-symbolic", "add-to-album"));
    vbox.append(&overflow_btn("Share", "send-to-symbolic", "share"));
    vbox.append(&overflow_btn("Export original", "document-save-symbolic", "export-original"));
    if include_wallpaper {
        vbox.append(&overflow_btn("Set as wallpaper", "preferences-desktop-wallpaper-symbolic", "set-wallpaper"));
    }

    // Separator
    let sep1 = gtk::Separator::new(gtk::Orientation::Horizontal);
    sep1.set_margin_top(4);
    sep1.set_margin_bottom(4);
    vbox.append(&sep1);

    // Section 2: file system
    vbox.append(&overflow_btn("Show in Files", "folder-open-symbolic", "show-in-files"));

    // Separator
    let sep2 = gtk::Separator::new(gtk::Orientation::Horizontal);
    sep2.set_margin_top(4);
    sep2.set_margin_bottom(4);
    vbox.append(&sep2);

    // Section 3: destructive
    let delete_btn = overflow_btn(delete_label, "user-trash-symbolic", "delete");
    delete_btn.add_css_class("error");
    vbox.append(&delete_btn);

    let popover = gtk::Popover::new();
    popover.set_child(Some(&vbox));
    popover
}

/// Create a flat button with icon + label for the overflow menu.
fn overflow_btn(label: &str, icon_name: &str, widget_name: &str) -> gtk::Button {
    let hbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    hbox.append(&gtk::Image::from_icon_name(icon_name));
    hbox.append(&gtk::Label::new(Some(label)));

    let btn = gtk::Button::builder()
        .child(&hbox)
        .build();
    btn.add_css_class("flat");
    btn.set_widget_name(widget_name);
    btn
}

/// Find a button in the popover by its widget name.
pub fn find_menu_button(popover: &gtk::Popover, name: &str) -> Option<gtk::Button> {
    let child = popover.child()?;
    let vbox = child.downcast_ref::<gtk::Box>()?;
    let mut widget = vbox.first_child();
    while let Some(w) = widget {
        if let Some(btn) = w.downcast_ref::<gtk::Button>() {
            if btn.widget_name() == name {
                return Some(btn.clone());
            }
        }
        widget = w.next_sibling();
    }
    None
}

