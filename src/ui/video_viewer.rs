use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};
use tracing::{debug, error};

use crate::library::media::MediaMetadataRecord;
use crate::library::Library;
use crate::ui::model_registry::ModelRegistry;
use crate::ui::photo_grid::item::MediaItemObject;
use crate::ui::viewer::info_panel::InfoPanel;

// ── Inner state ───────────────────────────────────────────────────────────────

struct VideoViewerInner {
    nav_page: adw::NavigationPage,
    video: gtk::Video,
    prev_btn: gtk::Button,
    next_btn: gtk::Button,
    star_btn: gtk::Button,
    trash_btn: gtk::Button,
    info_split: adw::OverlaySplitView,
    info_panel: InfoPanel,
    items: RefCell<Vec<MediaItemObject>>,
    current_index: Cell<usize>,
    current_metadata: RefCell<Option<MediaMetadataRecord>>,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    registry: Rc<ModelRegistry>,
}

impl VideoViewerInner {
    fn show_at(self: &Rc<Self>, index: usize) {
        let (id, filename, count) = {
            let items = self.items.borrow();
            let Some(obj) = items.get(index) else { return };
            (
                obj.item().id.clone(),
                obj.item().original_filename.clone(),
                items.len(),
            )
        };

        self.current_index.set(index);
        *self.current_metadata.borrow_mut() = None;

        self.nav_page.set_title(&filename);
        self.prev_btn.set_visible(index > 0);
        self.next_btn.set_visible(index + 1 < count);

        // Sync star button.
        {
            let items = self.items.borrow();
            if let Some(obj) = items.get(index) {
                self.star_btn.set_icon_name(if obj.is_favorite() {
                    "starred-symbolic"
                } else {
                    "non-starred-symbolic"
                });
            }
        }

        self.info_split.set_show_sidebar(false);

        // Resolve the original file path and set it on the video widget.
        self.load_video(id.clone());
        self.load_metadata_async(id);
    }

    fn load_video(self: &Rc<Self>, id: crate::library::media::MediaId) {
        let inner = Rc::clone(self);
        let library = Arc::clone(&self.library);
        let tokio = self.tokio.clone();

        // Stop any current playback.
        self.video.set_file(None::<&gio::File>);

        glib::MainContext::default().spawn_local(async move {
            let path = match tokio
                .spawn(async move { library.original_path(&id).await })
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
            {
                Some(p) => p,
                None => return,
            };

            let file = gio::File::for_path(&path);
            inner.video.set_file(Some(&file));
            debug!(path = %path.display(), "video loaded");
        });
    }

    fn load_metadata_async(self: &Rc<Self>, id: crate::library::media::MediaId) {
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

            *inner.current_metadata.borrow_mut() = metadata;

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
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Video player with prev/next navigation and a metadata panel.
///
/// Same activation pattern as [`PhotoViewer`] — pushed onto an
/// [`adw::NavigationView`] when a video grid item is activated.
pub struct VideoViewer {
    inner: Rc<VideoViewerInner>,
}

impl VideoViewer {
    pub fn new(library: Arc<dyn Library>, tokio: tokio::runtime::Handle, registry: Rc<ModelRegistry>) -> Self {
        // ── Header bar ───────────────────────────────────────────────────────
        let header = adw::HeaderBar::new();
        let info_toggle = gtk::ToggleButton::builder()
            .icon_name("info-symbolic")
            .tooltip_text("Video Information")
            .build();
        header.pack_end(&info_toggle);

        let star_btn = gtk::Button::builder()
            .icon_name("non-starred-symbolic")
            .tooltip_text("Toggle Favourite")
            .build();
        star_btn.add_css_class("flat");
        header.pack_end(&star_btn);

        let trash_btn = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Move to Trash")
            .build();
        trash_btn.add_css_class("flat");
        header.pack_start(&trash_btn);

        // ── Video ─────────────────────────────────────────────────────────────
        let video = gtk::Video::builder()
            .hexpand(true)
            .vexpand(true)
            .autoplay(true)
            .build();

        // ── OSD prev / next buttons ──────────────────────────────────────────
        let prev_btn = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text("Previous")
            .valign(gtk::Align::Center)
            .halign(gtk::Align::Start)
            .margin_start(12)
            .visible(false)
            .build();
        prev_btn.add_css_class("circular");
        prev_btn.add_css_class("osd");

        let next_btn = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text("Next")
            .valign(gtk::Align::Center)
            .halign(gtk::Align::End)
            .margin_end(12)
            .visible(false)
            .build();
        next_btn.add_css_class("circular");
        next_btn.add_css_class("osd");

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&video));
        overlay.add_overlay(&prev_btn);
        overlay.add_overlay(&next_btn);

        // ── Info panel ───────────────────────────────────────────────────────
        let info_panel = InfoPanel::new();

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

        let nav_page = adw::NavigationPage::builder()
            .tag("video-viewer")
            .title("Video")
            .child(&toolbar_view)
            .build();

        // ── Assemble ─────────────────────────────────────────────────────────
        let inner = Rc::new(VideoViewerInner {
            nav_page,
            video,
            prev_btn,
            next_btn,
            star_btn,
            trash_btn,
            info_split,
            info_panel,
            items: RefCell::new(Vec::new()),
            current_index: Cell::new(0),
            current_metadata: RefCell::new(None),
            library,
            tokio,
            registry,
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

        // Star (favourite) button
        {
            let i = Rc::downgrade(&inner);
            inner.star_btn.connect_clicked(move |btn| {
                let Some(inner) = i.upgrade() else { return };
                let items = inner.items.borrow();
                let idx = inner.current_index.get();
                let Some(obj) = items.get(idx) else { return };

                let new_fav = !obj.is_favorite();
                btn.set_icon_name(if new_fav { "starred-symbolic" } else { "non-starred-symbolic" });
                obj.set_is_favorite(new_fav);

                let id = obj.item().id.clone();
                let lib = Arc::clone(&inner.library);
                let tk = inner.tokio.clone();
                let reg = Rc::clone(&inner.registry);
                glib::MainContext::default().spawn_local(async move {
                    let result = tk
                        .spawn(async move { lib.set_favorite(&[id.clone()], new_fav).await.map(|_| id) })
                        .await;
                    match result {
                        Ok(Ok(id)) => reg.on_favorite_changed(&id, new_fav),
                        Ok(Err(e)) => error!("set_favorite failed: {e}"),
                        Err(e) => error!("set_favorite join failed: {e}"),
                    }
                });
            });
        }

        // Trash button
        {
            let i = Rc::downgrade(&inner);
            inner.trash_btn.connect_clicked(move |_| {
                let Some(inner) = i.upgrade() else { return };
                let id = {
                    let items = inner.items.borrow();
                    let idx = inner.current_index.get();
                    items.get(idx).map(|obj| obj.item().id.clone())
                };
                let Some(id) = id else { return };

                let lib = Arc::clone(&inner.library);
                let tk = inner.tokio.clone();
                let reg = Rc::clone(&inner.registry);
                let nav = inner.nav_page.clone();
                glib::MainContext::default().spawn_local(async move {
                    let result = tk
                        .spawn(async move { lib.trash(&[id.clone()]).await.map(|_| id) })
                        .await;
                    match result {
                        Ok(Ok(id)) => {
                            reg.on_trashed(&id, true);
                            if let Some(nav_view) = nav
                                .parent()
                                .and_then(|p| p.downcast::<adw::NavigationView>().ok())
                            {
                                nav_view.pop();
                            }
                        }
                        Ok(Err(e)) => error!("trash failed: {e}"),
                        Err(e) => error!("trash join failed: {e}"),
                    }
                });
            });
        }

        // Info toggle → split view
        {
            let split = inner.info_split.clone();
            info_toggle.connect_toggled(move |btn| {
                split.set_show_sidebar(btn.is_active());
            });
        }

        // Split view sidebar change → sync toggle + populate
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
                    gtk::gdk::Key::Left | gtk::gdk::Key::KP_Left => {
                        inner.navigate_prev();
                        glib::Propagation::Stop
                    }
                    gtk::gdk::Key::Right | gtk::gdk::Key::KP_Right => {
                        inner.navigate_next();
                        glib::Propagation::Stop
                    }
                    _ => glib::Propagation::Proceed,
                }
            });
        }

        Self { inner }
    }

    pub fn nav_page(&self) -> &adw::NavigationPage {
        &self.inner.nav_page
    }

    pub fn show(&self, items: Vec<MediaItemObject>, index: usize) {
        // Stop any previous playback.
        self.inner.video.set_file(None::<&gio::File>);
        *self.inner.items.borrow_mut() = items;
        self.inner.show_at(index);
        self.inner.nav_page.grab_focus();
    }
}
