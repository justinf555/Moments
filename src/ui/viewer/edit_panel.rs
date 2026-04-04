use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gdk, glib};
use image::DynamicImage;
use tracing::{debug, error, warn};

use crate::library::edit_renderer::{apply_edits, filter_preset, FILTER_NAMES};
use crate::library::editing::EditState;
use crate::library::media::MediaId;
use crate::library::Library;
use crate::ui::widgets::{expander_row, section_label, wire_single_expansion, wrap_in_row};

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
    ///
    /// The caller should provide a pre-downscaled preview image (~1200px)
    /// to avoid blocking the GTK thread with a resize during the sidebar
    /// slide-in animation.
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
    ///
    /// Skips the write if another save is already in-flight (the next
    /// navigate-away or inactivity timeout will catch it). Logs timing
    /// to help detect DB contention.
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
                            tx.send(crate::app_event::AppEvent::Error("Could not revert edits".into()));
                        }
                        Err(e) => {
                            error!("delete edit state join failed: {e}");
                            tx.send(crate::app_event::AppEvent::Error("Could not revert edits".into()));
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
                    tx.send(crate::app_event::AppEvent::Error("Could not save edits".into()));
                }
                Err(e) => {
                    error!("save edit state join failed: {e}");
                    tx.send(crate::app_event::AppEvent::Error("Could not save edits".into()));
                }
            }
        });
    }

    // ── UI construction ──────────────────────────────────────────────────────

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

        // ── Scrolled content ─────────────────────────────────────────────────
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
        {
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
                                w, h,
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
                                tx.send(crate::app_event::AppEvent::Error("Could not revert edits".into()));
                            }
                            Err(e) => {
                                error!("revert edits join failed: {e}");
                                tx.send(crate::app_event::AppEvent::Error("Could not revert edits".into()));
                            }
                        }
                    });
                }
            });
        }
    }

    /// Populate the Transform expander with rotate/flip buttons.
    fn build_transform_content(&self, expander: &adw::ExpanderRow) {
        let grid = gtk::Grid::builder()
            .column_spacing(8)
            .row_spacing(8)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .column_homogeneous(true)
            .build();

        let rotate_ccw_btn = make_transform_button("object-rotate-left-symbolic", "Rotate CCW", &gettext("Rotate Left"));
        let rotate_cw_btn = make_transform_button("object-rotate-right-symbolic", "Rotate CW", &gettext("Rotate Right"));
        let flip_h_btn = make_transform_button("object-flip-horizontal-symbolic", "Flip H", &gettext("Flip Horizontal"));
        let flip_v_btn = make_transform_button("object-flip-vertical-symbolic", "Flip V", &gettext("Flip Vertical"));

        grid.attach(&rotate_ccw_btn, 0, 0, 1, 1);
        grid.attach(&rotate_cw_btn, 1, 0, 1, 1);
        grid.attach(&flip_h_btn, 0, 1, 1, 1);
        grid.attach(&flip_v_btn, 1, 1, 1, 1);

        let grid_row = gtk::ListBoxRow::builder()
            .activatable(false)
            .selectable(false)
            .child(&grid)
            .build();
        expander.add_row(&grid_row);

        // Wire rotate CCW.
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            let auto_save = self.auto_save_closure();
            rotate_ccw_btn.connect_clicked(move |_| {
                let preview = {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };
                    s.state.transforms.rotate_degrees =
                        (s.state.transforms.rotate_degrees - 90).rem_euclid(360);
                    s.render_gen += 1;
                    (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                };
                render_to_picture(&picture, &tokio, &session, preview);
                auto_save();
            });
        }

        // Wire rotate CW.
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            let auto_save = self.auto_save_closure();
            rotate_cw_btn.connect_clicked(move |_| {
                let preview = {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };
                    s.state.transforms.rotate_degrees =
                        (s.state.transforms.rotate_degrees + 90).rem_euclid(360);
                    s.render_gen += 1;
                    (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                };
                render_to_picture(&picture, &tokio, &session, preview);
                auto_save();
            });
        }

        // Wire flip horizontal.
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            let auto_save = self.auto_save_closure();
            flip_h_btn.connect_clicked(move |_| {
                let preview = {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };
                    s.state.transforms.flip_horizontal = !s.state.transforms.flip_horizontal;
                    s.render_gen += 1;
                    (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                };
                render_to_picture(&picture, &tokio, &session, preview);
                auto_save();
            });
        }

        // Wire flip vertical.
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            let auto_save = self.auto_save_closure();
            flip_v_btn.connect_clicked(move |_| {
                let preview = {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };
                    s.state.transforms.flip_vertical = !s.state.transforms.flip_vertical;
                    s.render_gen += 1;
                    (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                };
                render_to_picture(&picture, &tokio, &session, preview);
                auto_save();
            });
        }
    }

    /// Populate the Filters expander with preset grid and strength slider.
    fn build_filters_content(&self, expander: &adw::ExpanderRow) {
        // ── Filter preset grid ───────────────────────────────────────────────
        let filter_box = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .max_children_per_line(3)
            .min_children_per_line(2)
            .row_spacing(8)
            .column_spacing(8)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .build();

        // "None" button to clear the filter.
        let original_btn = make_filter_swatch("None", None);
        filter_box.append(&original_btn);

        {
            let filter_buttons = Rc::clone(&self.filter_buttons);
            self.filter_buttons.borrow_mut().push(("original".to_string(), original_btn.clone()));

            for name in FILTER_NAMES {
                let display_name = filter_display_name(name);
                let btn = make_filter_swatch(display_name, Some(name));
                filter_box.append(&btn);
                filter_buttons.borrow_mut().push((name.to_string(), btn));
            }
        }

        // Wire filter button clicks.
        let buttons = self.filter_buttons.borrow().clone();
        for (name, btn) in &buttons {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            let all_buttons = Rc::clone(&self.filter_buttons);
            let auto_save = self.auto_save_closure();
            let filter_subtitle = Rc::clone(&self.filter_subtitle);
            let name = name.clone();

            btn.connect_clicked(move |clicked_btn| {
                if !clicked_btn.is_active() {
                    return;
                }

                // Deactivate other filter buttons.
                for (other_name, other_btn) in all_buttons.borrow().iter() {
                    if *other_name != name {
                        other_btn.set_active(false);
                    }
                }

                // Update the expander subtitle.
                let display = if name == "original" {
                    "None"
                } else {
                    filter_display_name(&name)
                };
                if let Some(ref lbl) = *filter_subtitle.borrow() {
                    lbl.set_label(display);
                }

                let preview = {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };

                    if name == "original" {
                        s.state.filter = None;
                        s.state.exposure = Default::default();
                        s.state.color = Default::default();
                    } else if let Some(preset) = filter_preset(&name) {
                        s.state.exposure = preset.exposure;
                        s.state.color = preset.color;
                        s.state.filter = Some(name.clone());
                    }

                    s.render_gen += 1;
                    (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                };

                render_to_picture(&picture, &tokio, &session, preview);
                auto_save();
            });
        }

        let grid_row = gtk::ListBoxRow::builder()
            .activatable(false)
            .selectable(false)
            .child(&filter_box)
            .build();
        expander.add_row(&grid_row);

        // ── Strength slider ──────────────────────────────────────────────────
        let strength_row = self.make_slider_row("Strength", 0.0, 1.0, 1.0, move |val| {
            (val * 100.0).round() as i32
        });
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            let render_debounce = Rc::clone(&self.render_debounce);
            let auto_save = self.auto_save_closure();
            let scale = strength_row.1.clone();

            scale.connect_value_changed(move |scale| {
                let strength = scale.value();

                {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };

                    if let Some(ref filter_name) = s.state.filter.clone() {
                        if let Some(preset) = filter_preset(filter_name) {
                            s.state.exposure.brightness = preset.exposure.brightness * strength;
                            s.state.exposure.contrast = preset.exposure.contrast * strength;
                            s.state.exposure.highlights = preset.exposure.highlights * strength;
                            s.state.exposure.shadows = preset.exposure.shadows * strength;
                            s.state.exposure.white_balance = preset.exposure.white_balance * strength;
                            s.state.color.saturation = preset.color.saturation * strength;
                            s.state.color.vibrance = preset.color.vibrance * strength;
                            s.state.color.hue_shift = preset.color.hue_shift * strength;
                            s.state.color.temperature = preset.color.temperature * strength;
                            s.state.color.tint = preset.color.tint * strength;
                        }
                    }
                    s.state.filter_strength = strength;
                    s.render_gen += 1;
                }

                if let Some(id) = render_debounce.take() {
                    id.remove();
                }

                let session_inner = Rc::clone(&session);
                let picture_inner = picture.clone();
                let tokio_inner = tokio.clone();
                let debounce_cell = Rc::clone(&render_debounce);
                let source_id = glib::timeout_add_local_once(
                    std::time::Duration::from_millis(RENDER_DEBOUNCE_MS as u64),
                    move || {
                        debounce_cell.set(None);
                        let preview = {
                            let session = session_inner.borrow();
                            let Some(s) = session.as_ref() else { return };
                            (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                        };
                        render_to_picture(&picture_inner, &tokio_inner, &session_inner, preview);
                    },
                );
                render_debounce.set(Some(source_id));

                auto_save();
            });
        }
        expander.add_row(&strength_row.0);
    }

    /// Populate the Adjust expander with Light and Colour slider groups.
    fn build_adjust_content(&self, expander: &adw::ExpanderRow) {
        // ── Light group ──────────────────────────────────────────────────────
        let light_label = section_label("LIGHT");
        expander.add_row(&wrap_in_row(&light_label));

        for (name, accessor) in [
            ("Brightness", accessor_fn(|s: &mut EditState| &mut s.exposure.brightness)),
            ("Contrast", accessor_fn(|s: &mut EditState| &mut s.exposure.contrast)),
            ("Highlights", accessor_fn(|s: &mut EditState| &mut s.exposure.highlights)),
            ("Shadows", accessor_fn(|s: &mut EditState| &mut s.exposure.shadows)),
            ("White Balance", accessor_fn(|s: &mut EditState| &mut s.exposure.white_balance)),
        ] {
            let slider = self.make_slider(name, accessor);
            expander.add_row(&wrap_in_row(&slider));
        }

        // ── Colour group ─────────────────────────────────────────────────────
        let colour_label = section_label("COLOUR");
        expander.add_row(&wrap_in_row(&colour_label));

        for (name, accessor) in [
            ("Saturation", accessor_fn(|s: &mut EditState| &mut s.color.saturation)),
            ("Vibrance", accessor_fn(|s: &mut EditState| &mut s.color.vibrance)),
            ("Temperature", accessor_fn(|s: &mut EditState| &mut s.color.temperature)),
            ("Tint", accessor_fn(|s: &mut EditState| &mut s.color.tint)),
        ] {
            let slider = self.make_slider(name, accessor);
            expander.add_row(&wrap_in_row(&slider));
        }
    }

    /// Create a slider row with label, value label, and scale.
    /// Returns (ListBoxRow, Scale) so callers can wire custom handlers.
    fn make_slider_row<D: Fn(f64) -> i32 + 'static>(
        &self,
        label: &str,
        min: f64,
        max: f64,
        initial: f64,
        display_fn: D,
    ) -> (gtk::ListBoxRow, gtk::Scale) {
        let value_label = gtk::Label::builder()
            .label(format!("{}", display_fn(initial)))
            .halign(gtk::Align::End)
            .width_chars(4)
            .build();
        value_label.add_css_class("dim-label");

        let header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .build();
        let label_widget = gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();
        header_box.append(&label_widget);
        header_box.append(&value_label);

        let scale = gtk::Scale::builder()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .margin_start(12)
            .margin_end(12)
            .build();
        scale.set_range(min, max);
        scale.set_value(initial);
        scale.set_draw_value(false);
        scale.set_increments(0.01, 0.1);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_top(4)
            .margin_bottom(4)
            .build();
        content.append(&header_box);
        content.append(&scale);

        // Update numeric display on value change.
        scale.connect_value_changed(move |s| {
            value_label.set_label(&format!("{}", display_fn(s.value())));
        });

        let row = gtk::ListBoxRow::builder()
            .activatable(false)
            .selectable(false)
            .child(&content)
            .build();

        (row, scale)
    }

    /// Create a slider with label left, numeric value right, and scale below.
    fn make_slider<F>(&self, label: &str, accessor: F) -> gtk::Box
    where
        F: Fn(&mut EditState) -> &mut f64 + 'static,
    {
        let label_widget = gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .hexpand(true)
            .build();

        let value_label = gtk::Label::builder()
            .label("0")
            .halign(gtk::Align::End)
            .width_chars(4)
            .build();
        value_label.add_css_class("dim-label");

        let header_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        header_box.append(&label_widget);
        header_box.append(&value_label);

        let scale = gtk::Scale::builder()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .build();
        scale.set_range(-1.0, 1.0);
        scale.set_value(0.0);
        scale.set_draw_value(false);
        scale.set_increments(0.01, 0.1);

        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        row.append(&header_box);
        row.append(&scale);

        // Connect value-changed to update the edit state and schedule a
        // debounced render. The state is updated immediately so it's always
        // current, but the expensive render only fires after the slider stops
        // moving for RENDER_DEBOUNCE_MS milliseconds.
        // Register this scale for revert.
        self.adjust_scales.borrow_mut().push(scale.clone());

        let session = Rc::clone(&self.session);
        let picture = self.picture.clone();
        let tokio = self.tokio.clone();
        let render_debounce = Rc::clone(&self.render_debounce);
        let auto_save = self.auto_save_closure();
        let adjust_subtitle = Rc::clone(&self.adjust_subtitle);
        let adjust_scales = Rc::clone(&self.adjust_scales);

        scale.connect_value_changed(move |scale| {
            let value = scale.value();
            let value = if value.abs() < 0.02 { 0.0 } else { value };

            // Update the numeric display (mapped to -100..100 range).
            value_label.set_label(&format!("{}", (value * 100.0).round() as i32));

            {
                let mut session = session.borrow_mut();
                let Some(s) = session.as_mut() else { return };
                *accessor(&mut s.state) = value;
                s.render_gen += 1;
            }

            // Update adjust subtitle with count of non-default sliders.
            update_adjust_subtitle(&adjust_subtitle, &adjust_scales);

            // Cancel any pending render debounce timer.
            if let Some(id) = render_debounce.take() {
                id.remove();
            }

            // Schedule a new render after the debounce period.
            let session = Rc::clone(&session);
            let picture = picture.clone();
            let tokio = tokio.clone();
            let debounce_cell = Rc::clone(&render_debounce);
            let source_id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(RENDER_DEBOUNCE_MS as u64),
                move || {
                    debounce_cell.set(None);
                    let preview = {
                        let session = session.borrow();
                        let Some(s) = session.as_ref() else { return };
                        (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                    };
                    render_to_picture(&picture, &tokio, &session, preview);
                },
            );
            render_debounce.set(Some(source_id));

            // Schedule auto-save.
            auto_save();
        });

        row
    }

    /// Create a closure that schedules a debounced auto-save.
    ///
    /// Used by sliders, transform buttons, and filter buttons to
    /// trigger persistence after changes.
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
            session
                .as_ref()
                .and_then(|s| s.state.filter.clone())
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

// ── Free functions ───────────────────────────────────────────────────────────

/// Perform a debounced DB save. Shared by auto_save_closure and filter buttons.
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
                tx.send(crate::app_event::AppEvent::Error("Could not save edits".into()));
            }
            Err(e) => {
                error!("auto-save join failed: {e}");
                tx.send(crate::app_event::AppEvent::Error("Could not save edits".into()));
            }
        }
    });
}

/// Render an edit state preview and display it on the picture widget.
///
/// Shared by slider handlers and transform button handlers. The preview
/// image is passed via `Arc` and borrowed inside the blocking task —
/// no pixel data is cloned.
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

/// Update the Adjust expander subtitle with the count of non-default sliders.
fn update_adjust_subtitle(
    subtitle: &Rc<RefCell<Option<gtk::Label>>>,
    scales: &Rc<RefCell<Vec<gtk::Scale>>>,
) {
    let count = scales
        .borrow()
        .iter()
        .filter(|s| s.value().abs() > 0.02)
        .count();

    let text = match count {
        0 => "No changes".to_string(),
        1 => "1 change".to_string(),
        n => format!("{n} changes"),
    };

    if let Some(ref lbl) = *subtitle.borrow() {
        lbl.set_label(&text);
    }
}

/// Create a transform action button with icon and label for the 2×2 grid.
fn make_transform_button(icon_name: &str, label: &str, tooltip: &str) -> gtk::Button {
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(24);

    let lbl = gtk::Label::new(Some(label));
    lbl.add_css_class("caption");

    let vbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .margin_top(8)
        .margin_bottom(8)
        .build();
    vbox.append(&icon);
    vbox.append(&lbl);

    let btn = gtk::Button::builder()
        .child(&vbox)
        .tooltip_text(tooltip)
        .build();
    btn.add_css_class("flat");
    btn
}

/// Type-erased accessor closure for EditState fields.
fn accessor_fn(
    f: fn(&mut EditState) -> &mut f64,
) -> Box<dyn Fn(&mut EditState) -> &mut f64> {
    Box::new(f)
}

/// Convert a filter preset name to a user-facing display name.
fn filter_display_name(name: &str) -> &str {
    match name {
        "bw" => "B&W",
        "vintage" => "Vintage",
        "warm" => "Warm",
        "cool" => "Cool",
        "vivid" => "Vivid",
        "fade" => "Fade",
        "noir" => "Noir",
        "chrome" => "Chrome",
        "matte" => "Matte",
        "golden" => "Golden",
        _ => name,
    }
}

/// Create a filter swatch toggle button with a coloured background and label.
fn make_filter_swatch(display_name: &str, preset_name: Option<&str>) -> gtk::ToggleButton {
    let label = gtk::Label::new(Some(display_name));
    label.add_css_class("caption");

    let swatch = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();

    let colour_box = gtk::Box::builder()
        .width_request(80)
        .height_request(80)
        .build();
    colour_box.add_css_class("filter-swatch");

    // Apply a CSS class specific to this filter for colouring.
    let css_class = match preset_name {
        Some(name) => format!("filter-{name}"),
        None => "filter-none".to_string(),
    };
    colour_box.add_css_class(&css_class);

    swatch.append(&colour_box);
    swatch.append(&label);

    let btn = gtk::ToggleButton::builder()
        .child(&swatch)
        .build();
    btn.add_css_class("flat");
    btn.add_css_class("filter-button");

    btn
}
