use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gdk, glib};
use image::DynamicImage;
use tracing::{debug, error, warn};

use crate::library::edit_renderer::apply_edits;
use crate::library::editing::EditState;
use crate::library::media::MediaId;
use crate::library::Library;
use crate::ui::widgets::{expander_row, wire_single_expansion};

mod filters;
mod sliders;
mod transforms;

use filters::filter_display_name;

/// Delay before rendering preview after the last slider change (milliseconds).
const RENDER_DEBOUNCE_MS: u32 = 50;

/// Delay before auto-saving edit state to DB after the last change (milliseconds).
const SAVE_DEBOUNCE_MS: u32 = 100;

/// Mutable state for an active editing session.
pub struct EditSession {
    /// Current edit state modified by sliders.
    pub state: EditState,
    /// Downscaled preview image (~1200px) for fast rendering.
    /// Shared via `Arc` — render tasks read from it without cloning.
    pub preview_image: Arc<DynamicImage>,
    /// Generation counter for discarding stale render results.
    render_gen: u64,
}

/// Scrollable edit panel displayed in the viewer sidebar.
///
/// Uses three `AdwExpanderRow` sections:
/// - **Transform**: crop, rotate, flip (collapsed by default)
/// - **Filters**: preset filter grid + strength slider (expanded by default)
/// - **Adjust**: Light, Colour slider groups (expanded by default)
///
/// Edit state is auto-saved to the database after 100ms of inactivity
/// and on session end. No explicit Save button — follows GNOME HIG.
pub struct EditPanel {
    /// Root widget containing the view switcher and stack.
    root: gtk::Box,
    /// The currently active editing session, if any.
    session: Rc<RefCell<Option<EditSession>>>,
    /// Reference to the picture widget for updating the preview.
    picture: gtk::Picture,
    /// Tokio handle for spawning blocking render tasks.
    tokio: tokio::runtime::Handle,
    /// Library for persisting edit state.
    library: Arc<dyn Library>,
    /// The media ID being edited.
    media_id: Rc<RefCell<Option<MediaId>>>,
    /// Source ID of the pending render debounce timer, if any.
    render_debounce: Rc<Cell<Option<glib::SourceId>>>,
    /// Source ID of the pending save debounce timer, if any.
    save_debounce: Rc<Cell<Option<glib::SourceId>>>,
    /// Whether a DB write is currently in-flight.
    save_in_flight: Rc<Cell<bool>>,
    /// Filter toggle buttons, keyed by name, for programmatic updates.
    filter_buttons: Rc<RefCell<Vec<(String, gtk::ToggleButton)>>>,
    /// Subtitle label on the Filters expander, updated when filter changes.
    filter_subtitle: Rc<RefCell<Option<gtk::Label>>>,
    /// Subtitle label on the Adjust expander, updated when sliders change.
    adjust_subtitle: Rc<RefCell<Option<gtk::Label>>>,
    /// All adjust slider scales, for resetting on revert.
    adjust_scales: Rc<RefCell<Vec<gtk::Scale>>>,
    /// Bus sender for emitting user-facing error toasts.
    bus_sender: crate::event_bus::EventSender,
}

impl EditPanel {
    pub fn new(
        picture: gtk::Picture,
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        bus_sender: crate::event_bus::EventSender,
    ) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let session: Rc<RefCell<Option<EditSession>>> = Rc::new(RefCell::new(None));
        let media_id: Rc<RefCell<Option<MediaId>>> = Rc::new(RefCell::new(None));
        let render_debounce: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
        let save_debounce: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
        let save_in_flight: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let filter_buttons: Rc<RefCell<Vec<(String, gtk::ToggleButton)>>> =
            Rc::new(RefCell::new(Vec::new()));
        let filter_subtitle: Rc<RefCell<Option<gtk::Label>>> = Rc::new(RefCell::new(None));
        let adjust_subtitle: Rc<RefCell<Option<gtk::Label>>> = Rc::new(RefCell::new(None));
        let adjust_scales: Rc<RefCell<Vec<gtk::Scale>>> = Rc::new(RefCell::new(Vec::new()));

        let panel = Self {
            root,
            session,
            picture,
            tokio,
            library,
            media_id,
            render_debounce,
            save_debounce,
            save_in_flight,
            filter_buttons,
            filter_subtitle,
            adjust_subtitle,
            adjust_scales,
            bus_sender,
        };

        panel.build_ui();
        panel
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.root.upcast_ref()
    }

    /// Start an editing session for the given media item.
    pub fn begin_session(
        &self,
        id: MediaId,
        preview_image: Arc<DynamicImage>,
        existing_state: Option<EditState>,
    ) {
        let state = existing_state.unwrap_or_default();

        debug!(media_id = %id, "begin edit session");

        *self.media_id.borrow_mut() = Some(id);
        *self.session.borrow_mut() = Some(EditSession {
            state,
            preview_image,
            render_gen: 0,
        });

        // Sync filter button and slider state.
        self.sync_ui_from_state();

        // Render initial preview if state is not identity.
        if !self.session.borrow().as_ref().unwrap().state.is_identity() {
            self.render_preview();
        }
    }

    /// End the current editing session, auto-saving any pending changes.
    pub fn end_session(&self) {
        // Cancel any pending save debounce — we'll save immediately.
        if let Some(id) = self.save_debounce.take() {
            id.remove();
        }

        // Persist current state before closing.
        self.save_to_db("navigate away");

        let media_id = self.media_id.borrow().clone();
        if let Some(id) = &media_id {
            debug!(media_id = %id, "end edit session");
        }

        *self.session.borrow_mut() = None;
        *self.media_id.borrow_mut() = None;
    }

    // ── Auto-save ────────────────────────────────────────────────────────────

    /// Persist the current edit state to the database.
    fn save_to_db(&self, reason: &'static str) {
        let (id, state) = {
            let session = self.session.borrow();
            let Some(session) = session.as_ref() else { return };
            let Some(id) = self.media_id.borrow().clone() else { return };

            // Don't persist identity state — delete instead if it exists.
            if session.state.is_identity() {
                let lib = Arc::clone(&self.library);
                let tk = self.tokio.clone();
                let id_log = id.clone();
                let tx = self.bus_sender.clone();
                glib::MainContext::default().spawn_local(async move {
                    let start = Instant::now();
                    let result = tk
                        .spawn(async move { lib.revert_edits(&id).await })
                        .await;
                    let elapsed = start.elapsed();
                    match result {
                        Ok(Ok(())) => debug!(
                            media_id = %id_log,
                            elapsed_ms = elapsed.as_millis(),
                            reason,
                            "delete identity edit state"
                        ),
                        Ok(Err(e)) => {
                            error!("delete edit state failed: {e}");
                            tx.send(crate::app_event::AppEvent::Error(
                                "Could not revert edits".into(),
                            ));
                        }
                        Err(e) => {
                            error!("delete edit state join failed: {e}");
                            tx.send(crate::app_event::AppEvent::Error(
                                "Could not revert edits".into(),
                            ));
                        }
                    }
                });
                return;
            }

            (id, session.state.clone())
        };

        if self.save_in_flight.get() {
            debug!(media_id = %id, reason, "save skipped — write already in-flight");
            return;
        }

        self.save_in_flight.set(true);
        let in_flight = Rc::clone(&self.save_in_flight);
        let lib = Arc::clone(&self.library);
        let tk = self.tokio.clone();
        let id_log = id.clone();
        let tx = self.bus_sender.clone();

        glib::MainContext::default().spawn_local(async move {
            let start = Instant::now();
            let result = tk
                .spawn(async move { lib.save_edit_state(&id, &state).await })
                .await;
            let elapsed = start.elapsed();
            in_flight.set(false);

            match result {
                Ok(Ok(())) => {
                    if elapsed.as_millis() > 20 {
                        warn!(
                            media_id = %id_log,
                            elapsed_ms = elapsed.as_millis(),
                            reason,
                            "save edit state slow"
                        );
                    } else {
                        debug!(
                            media_id = %id_log,
                            elapsed_ms = elapsed.as_millis(),
                            reason,
                            "save edit state"
                        );
                    }
                }
                Ok(Err(e)) => {
                    error!("save edit state failed: {e}");
                    tx.send(crate::app_event::AppEvent::Error(
                        "Could not save edits".into(),
                    ));
                }
                Err(e) => {
                    error!("save edit state join failed: {e}");
                    tx.send(crate::app_event::AppEvent::Error(
                        "Could not save edits".into(),
                    ));
                }
            }
        });
    }

    // ── UI construction ──────────────────────────��───────────────────────────

    fn build_ui(&self) {
        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 12);
        vbox.set_margin_top(12);
        vbox.set_margin_bottom(12);
        vbox.set_margin_start(12);
        vbox.set_margin_end(12);

        // ── Transform section (collapsed by default) ─────────────────────────
        let list_transform = gtk::ListBox::new();
        list_transform.add_css_class("boxed-list");
        list_transform.set_selection_mode(gtk::SelectionMode::None);

        let (transform_exp, _) = expander_row(
            Some("object-rotate-right-symbolic"),
            "Transform",
            "Crop, rotate, flip",
            false,
        );
        self.build_transform_content(&transform_exp);
        list_transform.append(&transform_exp);
        vbox.append(&list_transform);

        // ── Filters section (expanded by default) ────────────────────────────
        let list_filters = gtk::ListBox::new();
        list_filters.add_css_class("boxed-list");
        list_filters.set_selection_mode(gtk::SelectionMode::None);

        let (filters_exp, filter_subtitle_label) = expander_row(
            Some("color-select-symbolic"),
            "Filters",
            "None",
            true,
        );
        *self.filter_subtitle.borrow_mut() = Some(filter_subtitle_label);
        self.build_filters_content(&filters_exp);
        list_filters.append(&filters_exp);
        vbox.append(&list_filters);

        // ── Adjust section (collapsed by default) ────────────────────────────
        let list_adjust = gtk::ListBox::new();
        list_adjust.add_css_class("boxed-list");
        list_adjust.set_selection_mode(gtk::SelectionMode::None);

        let (adjust_exp, adjust_subtitle_label) = expander_row(
            Some("preferences-other-symbolic"),
            "Adjust",
            "No changes",
            false,
        );
        *self.adjust_subtitle.borrow_mut() = Some(adjust_subtitle_label);
        self.build_adjust_content(&adjust_exp);
        list_adjust.append(&adjust_exp);
        vbox.append(&list_adjust);

        // Wire single-expansion: only one section open at a time.
        wire_single_expansion(&[&transform_exp, &filters_exp, &adjust_exp]);

        // ── Scrolled content ─────────────────────��───────────────────────────
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        scrolled.set_child(Some(&vbox));
        self.root.append(&scrolled);

        // ── Revert button (always visible at bottom) ─────────────────────────
        let revert_btn = gtk::Button::builder()
            .label("Revert to Original")
            .tooltip_text(gettext("Remove all edits and restore the original image"))
            .hexpand(true)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        revert_btn.add_css_class("destructive-action");
        self.root.append(&revert_btn);

        // Wire revert.
        self.wire_revert_button(&revert_btn);
    }

    /// Wire the Revert to Original button.
    fn wire_revert_button(&self, revert_btn: &gtk::Button) {
        let session = Rc::clone(&self.session);
        let media_id = Rc::clone(&self.media_id);
        let library = Arc::clone(&self.library);
        let tokio = self.tokio.clone();
        let picture = self.picture.clone();
        let save_debounce = Rc::clone(&self.save_debounce);
        let filter_buttons = Rc::clone(&self.filter_buttons);
        let filter_subtitle = Rc::clone(&self.filter_subtitle);
        let adjust_subtitle = Rc::clone(&self.adjust_subtitle);
        let adjust_scales = Rc::clone(&self.adjust_scales);
        let tx = self.bus_sender.clone();
        revert_btn.connect_clicked(move |_| {
            // Cancel any pending auto-save.
            if let Some(id) = save_debounce.take() {
                id.remove();
            }

            // Reset state.
            {
                let mut session = session.borrow_mut();
                if let Some(s) = session.as_mut() {
                    s.state = EditState::default();
                }
            }

            // Reset filter buttons and subtitle.
            for (_, btn) in filter_buttons.borrow().iter() {
                btn.set_active(false);
            }
            if let Some(ref lbl) = *filter_subtitle.borrow() {
                lbl.set_label("None");
            }

            // Reset all adjust sliders to 0.
            for scale in adjust_scales.borrow().iter() {
                scale.set_value(0.0);
            }
            if let Some(ref lbl) = *adjust_subtitle.borrow() {
                lbl.set_label("No changes");
            }

            // Re-render original.
            let preview = {
                let session = session.borrow();
                session.as_ref().map(|s| Arc::clone(&s.preview_image))
            };
            if let Some(preview) = preview {
                let pic = picture.clone();
                let tk = tokio.clone();
                glib::MainContext::default().spawn_local(async move {
                    let state = EditState::default();
                    let result = tk
                        .spawn(async move {
                            tokio::task::spawn_blocking(move || {
                                let edited = apply_edits(&preview, &state);
                                let rgba = edited.into_rgba8();
                                let (w, h) = image::GenericImageView::dimensions(&rgba);
                                (rgba.into_raw(), w as i32, h as i32)
                            })
                            .await
                        })
                        .await;

                    if let Ok(Ok((raw, w, h))) = result {
                        let gbytes = glib::Bytes::from_owned(raw);
                        let texture = gdk::MemoryTexture::new(
                            w,
                            h,
                            gdk::MemoryFormat::R8g8b8a8,
                            &gbytes,
                            (w as usize) * 4,
                        );
                        pic.set_paintable(Some(texture.upcast_ref::<gdk::Paintable>()));
                    }
                });
            }

            // Delete from DB.
            let id = media_id.borrow().clone();
            if let Some(id) = id {
                let lib = Arc::clone(&library);
                let tk = tokio.clone();
                let id_log = id.clone();
                let tx = tx.clone();
                glib::MainContext::default().spawn_local(async move {
                    let start = Instant::now();
                    let result =
                        tk.spawn(async move { lib.revert_edits(&id).await }).await;
                    let elapsed = start.elapsed();
                    match result {
                        Ok(Ok(())) => debug!(
                            media_id = %id_log,
                            elapsed_ms = elapsed.as_millis(),
                            "revert edits"
                        ),
                        Ok(Err(e)) => {
                            error!("revert edits failed: {e}");
                            tx.send(crate::app_event::AppEvent::Error(
                                "Could not revert edits".into(),
                            ));
                        }
                        Err(e) => {
                            error!("revert edits join failed: {e}");
                            tx.send(crate::app_event::AppEvent::Error(
                                "Could not revert edits".into(),
                            ));
                        }
                    }
                });
            }
        });
    }

    /// Create a closure that schedules a debounced auto-save.
    fn auto_save_closure(&self) -> impl Fn() {
        let save_debounce = Rc::clone(&self.save_debounce);
        let session = Rc::clone(&self.session);
        let media_id = Rc::clone(&self.media_id);
        let library = Arc::clone(&self.library);
        let tokio = self.tokio.clone();
        let save_in_flight = Rc::clone(&self.save_in_flight);
        let bus_sender = self.bus_sender.clone();

        move || {
            // Cancel any pending save timer.
            if let Some(id) = save_debounce.take() {
                id.remove();
            }

            let session = Rc::clone(&session);
            let media_id = Rc::clone(&media_id);
            let library = Arc::clone(&library);
            let tokio = tokio.clone();
            let save_in_flight = Rc::clone(&save_in_flight);
            let save_debounce_inner = Rc::clone(&save_debounce);
            let tx = bus_sender.clone();

            let source_id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(SAVE_DEBOUNCE_MS as u64),
                move || {
                    save_debounce_inner.set(None);
                    schedule_save(&session, &media_id, &library, &tokio, &save_in_flight, &tx);
                },
            );
            save_debounce.set(Some(source_id));
        }
    }

    /// Render the current edit state as a preview.
    fn render_preview(&self) {
        let (preview_img, state, gen) = {
            let session = self.session.borrow();
            let Some(s) = session.as_ref() else { return };
            (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
        };

        let pic = self.picture.clone();
        let tk = self.tokio.clone();
        let session_ref = Rc::clone(&self.session);

        glib::MainContext::default().spawn_local(async move {
            let result = tk
                .spawn(async move {
                    tokio::task::spawn_blocking(move || {
                        let edited = apply_edits(&preview_img, &state);
                        let rgba = edited.into_rgba8();
                        let (w, h) = image::GenericImageView::dimensions(&rgba);
                        (rgba.into_raw(), w as i32, h as i32)
                    })
                    .await
                })
                .await;

            let current_gen = session_ref.borrow().as_ref().map(|s| s.render_gen);
            if current_gen != Some(gen) {
                return;
            }

            if let Ok(Ok((raw, w, h))) = result {
                let gbytes = glib::Bytes::from_owned(raw);
                let texture = gdk::MemoryTexture::new(
                    w,
                    h,
                    gdk::MemoryFormat::R8g8b8a8,
                    &gbytes,
                    (w as usize) * 4,
                );
                pic.set_paintable(Some(texture.upcast_ref::<gdk::Paintable>()));
            }
        });
    }

    /// Sync UI widgets (filter buttons, sliders) to match the current EditState.
    fn sync_ui_from_state(&self) {
        let filter_name = {
            let session = self.session.borrow();
            session.as_ref().and_then(|s| s.state.filter.clone())
        };

        // Sync filter buttons.
        for (name, btn) in self.filter_buttons.borrow().iter() {
            let should_be_active = match &filter_name {
                Some(f) => *name == *f,
                None => *name == "original",
            };
            btn.set_active(should_be_active);
        }

        // Sync filter subtitle.
        let display = match &filter_name {
            Some(f) => filter_display_name(f),
            None => "None",
        };
        if let Some(ref lbl) = *self.filter_subtitle.borrow() {
            lbl.set_label(display);
        }
    }
}

// ── Free functions ────────────────────────────────��──────────────────────────

/// Perform a debounced DB save.
#[allow(clippy::too_many_arguments)]
fn schedule_save(
    session: &Rc<RefCell<Option<EditSession>>>,
    media_id: &Rc<RefCell<Option<MediaId>>>,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    save_in_flight: &Rc<Cell<bool>>,
    bus_sender: &crate::event_bus::EventSender,
) {
    let (id, state) = {
        let session = session.borrow();
        let Some(session) = session.as_ref() else { return };
        let Some(id) = media_id.borrow().clone() else { return };
        if session.state.is_identity() {
            return;
        }
        (id, session.state.clone())
    };

    if save_in_flight.get() {
        debug!(media_id = %id, "auto-save skipped — write already in-flight");
        return;
    }

    save_in_flight.set(true);
    let in_flight = Rc::clone(save_in_flight);
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let id_log = id.clone();
    let tx = bus_sender.clone();

    glib::MainContext::default().spawn_local(async move {
        let start = Instant::now();
        let result = tk
            .spawn(async move { lib.save_edit_state(&id, &state).await })
            .await;
        let elapsed = start.elapsed();
        in_flight.set(false);

        match result {
            Ok(Ok(())) => {
                if elapsed.as_millis() > 20 {
                    warn!(
                        media_id = %id_log,
                        elapsed_ms = elapsed.as_millis(),
                        "auto-save slow"
                    );
                } else {
                    debug!(
                        media_id = %id_log,
                        elapsed_ms = elapsed.as_millis(),
                        "auto-save"
                    );
                }
            }
            Ok(Err(e)) => {
                error!("auto-save failed: {e}");
                tx.send(crate::app_event::AppEvent::Error(
                    "Could not save edits".into(),
                ));
            }
            Err(e) => {
                error!("auto-save join failed: {e}");
                tx.send(crate::app_event::AppEvent::Error(
                    "Could not save edits".into(),
                ));
            }
        }
    });
}

/// Render an edit state preview and display it on the picture widget.
fn render_to_picture(
    picture: &gtk::Picture,
    tokio: &tokio::runtime::Handle,
    session: &Rc<RefCell<Option<EditSession>>>,
    preview: (Arc<DynamicImage>, EditState, u64),
) {
    let pic = picture.clone();
    let tk = tokio.clone();
    let session_ref = Rc::clone(session);
    let (preview_img, state, gen) = preview;

    glib::MainContext::default().spawn_local(async move {
        let result = tk
            .spawn(async move {
                tokio::task::spawn_blocking(move || {
                    let edited = apply_edits(&preview_img, &state);
                    let rgba = edited.into_rgba8();
                    let (w, h) = image::GenericImageView::dimensions(&rgba);
                    (rgba.into_raw(), w as i32, h as i32)
                })
                .await
            })
            .await;

        let current_gen = session_ref.borrow().as_ref().map(|s| s.render_gen);
        if current_gen != Some(gen) {
            return;
        }

        if let Ok(Ok((raw, w, h))) = result {
            let gbytes = glib::Bytes::from_owned(raw);
            let texture = gdk::MemoryTexture::new(
                w,
                h,
                gdk::MemoryFormat::R8g8b8a8,
                &gbytes,
                (w as usize) * 4,
            );
            pic.set_paintable(Some(texture.upcast_ref::<gdk::Paintable>()));
        }
    });
}
