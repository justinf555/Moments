use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use tracing::debug;

use crate::library::media::MediaMetadataRecord;
use crate::app_event::AppEvent;
use crate::event_bus::EventSender;
use crate::library::Library;
use crate::ui::photo_grid::item::MediaItemObject;
use crate::ui::viewer::info_panel::InfoPanel;

// ── Inner state ───────────────────────────────────────────────────────────────

struct VideoViewerInner {
    nav_page: adw::NavigationPage,
    video: gtk::Video,
    prev_btn: gtk::Button,
    next_btn: gtk::Button,
    star_btn: gtk::Button,
    info_split: adw::OverlaySplitView,
    info_panel: InfoPanel,
    items: RefCell<Vec<MediaItemObject>>,
    current_index: Cell<usize>,
    current_metadata: RefCell<Option<MediaMetadataRecord>>,
    library: Arc<dyn Library>,
    tokio: tokio::runtime::Handle,
    bus_sender: EventSender,
}

impl VideoViewerInner {
    #[tracing::instrument(skip(self), fields(index))]
    fn show_at(self: &Rc<Self>, index: usize) {
        let (id, filename, count) = {
            let items = self.items.borrow();
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

        self.current_index.set(index);
        *self.current_metadata.borrow_mut() = None;

        self.nav_page.set_title(&filename);
        self.prev_btn.set_visible(index > 0);
        self.next_btn.set_visible(index + 1 < count);

        // Sync star button.
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

        self.info_split.set_show_sidebar(false);

        // Resolve the original file path and set it on the video widget.
        self.load_video(id.clone());
        self.load_metadata_async(id);
    }

    fn load_video(self: &Rc<Self>, id: crate::library::media::MediaId) {
        let inner = Rc::clone(self);
        let library = Arc::clone(&self.library);
        let tokio = self.tokio.clone();

        debug!(%id, "load_video: resolving path");

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
                None => {
                    tracing::warn!("load_video: could not resolve original path");
                    return;
                }
            };

            debug!(path = %path.display(), exists = path.exists(), "load_video: setting file on GtkVideo");
            let file = gio::File::for_path(&path);
            inner.video.set_file(Some(&file));
            debug!("load_video: file set, playback should start (autoplay=true)");
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
        let items = self.items.borrow();
        let mut idx = self.current_index.get();
        // Skip image items — they belong in PhotoViewer.
        while idx > 0 {
            idx -= 1;
            if items.get(idx).map(|o| o.item().media_type == crate::library::media::MediaType::Video).unwrap_or(false) {
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
        // Skip image items — they belong in PhotoViewer.
        while idx + 1 < len {
            idx += 1;
            if items.get(idx).map(|o| o.item().media_type == crate::library::media::MediaType::Video).unwrap_or(false) {
                drop(items);
                self.show_at(idx);
                return;
            }
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
    pub fn new(library: Arc<dyn Library>, tokio: tokio::runtime::Handle, bus_sender: EventSender) -> Self {
        // ── Header bar ───────────────────────────────────────────────────────
        //
        // Layout (pack_end is right-to-left):
        //   start: [← back]
        //   end:   [★] [ℹ] [⋮]
        //
        // Album, Share, Export, Show in Files, and Delete live in the
        // overflow menu (⋮).
        let header = adw::HeaderBar::new();

        // ── Overflow menu (far right) ────────────────────────────────────
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Menu")
            .build();
        let menu_popover = crate::ui::viewer::build_viewer_menu_popover(false, "Delete video");
        menu_btn.set_popover(Some(&menu_popover));
        header.pack_end(&menu_btn);

        // ── Info toggle ─────────────────────────────────────────────────
        let info_toggle = gtk::ToggleButton::builder()
            .icon_name("dialog-information-symbolic")
            .tooltip_text("Video Information (F9)")
            .build();
        header.pack_end(&info_toggle);

        // ── Favourite ───────────────────────────────────────────────────
        let star_btn = gtk::Button::builder()
            .icon_name("non-starred-symbolic")
            .tooltip_text("Toggle Favourite")
            .build();
        star_btn.add_css_class("flat");
        header.pack_end(&star_btn);

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
            info_split,
            info_panel,
            items: RefCell::new(Vec::new()),
            current_index: Cell::new(0),
            current_metadata: RefCell::new(None),
            library,
            tokio,
            bus_sender,
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
                if new_fav {
                    btn.add_css_class("warning");
                } else {
                    btn.remove_css_class("warning");
                }
                obj.set_is_favorite(new_fav);

                let id = obj.item().id.clone();
                inner.bus_sender.send(AppEvent::FavoriteRequested {
                    ids: vec![id],
                    state: new_fav,
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

        // Keyboard navigation (← → F9)
        {
            let key_ctrl = gtk::EventControllerKey::new();
            toolbar_view.add_controller(key_ctrl.clone());
            let i = Rc::downgrade(&inner);
            let info_toggle = info_toggle.clone();
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
                        let active = info_toggle.is_active();
                        info_toggle.set_active(!active);
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
            if let Some(btn) = crate::ui::viewer::find_menu_button(&popover, "add-to-album") {
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

            // Delete video — trash + pop back to grid.
            // Stub items — just close the popover on click.
            for name in &["share", "export-original", "show-in-files"] {
                if let Some(btn) = crate::ui::viewer::find_menu_button(&popover, name) {
                    let pop = popover.downgrade();
                    btn.connect_clicked(move |_| {
                        if let Some(p) = pop.upgrade() { p.popdown(); }
                    });
                }
            }

            // Delete video — trash + pop back to grid.
            if let Some(btn) = crate::ui::viewer::find_menu_button(&popover, "delete") {
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

        Self { inner }
    }

    pub fn nav_page(&self) -> &adw::NavigationPage {
        &self.inner.nav_page
    }

    pub fn show(&self, items: Vec<MediaItemObject>, index: usize) {
        debug!(index, item_count = items.len(), "VideoViewer::show");
        // Stop any previous playback.
        self.inner.video.set_file(None::<&gio::File>);
        *self.inner.items.borrow_mut() = items;
        self.inner.show_at(index);
        self.inner.nav_page.grab_focus();
    }
}

