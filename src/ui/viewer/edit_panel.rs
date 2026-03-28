use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use adw::prelude::*;
use gtk::{gdk, glib};
use image::DynamicImage;
use tracing::{debug, error, warn};

use crate::library::edit_renderer::{apply_edits, filter_preset, FILTER_NAMES};
use crate::library::editing::EditState;
use crate::library::media::MediaId;
use crate::library::Library;

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
/// Uses an `AdwViewSwitcher` with two pages:
/// - **Filters**: preset filter buttons + transform controls (rotate, flip)
/// - **Adjust**: individual exposure and color sliders
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
}

impl EditPanel {
    pub fn new(
        picture: gtk::Picture,
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
    ) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let session: Rc<RefCell<Option<EditSession>>> = Rc::new(RefCell::new(None));
        let media_id: Rc<RefCell<Option<MediaId>>> = Rc::new(RefCell::new(None));
        let render_debounce: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
        let save_debounce: Rc<Cell<Option<glib::SourceId>>> = Rc::new(Cell::new(None));
        let save_in_flight: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let filter_buttons: Rc<RefCell<Vec<(String, gtk::ToggleButton)>>> =
            Rc::new(RefCell::new(Vec::new()));

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
            preview_image: preview_image,
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
                        Ok(Err(e)) => error!("delete edit state failed: {e}"),
                        Err(e) => error!("delete edit state join failed: {e}"),
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
                Ok(Err(e)) => error!("save edit state failed: {e}"),
                Err(e) => error!("save edit state join failed: {e}"),
            }
        });
    }

    // ── UI construction ──────────────────────────────────────────────────────

    fn build_ui(&self) {
        // ── View stack with two pages ────────────────────────────────────────
        let stack = adw::ViewStack::new();

        let filters_page = self.build_filters_page();
        stack.add_titled(&filters_page, Some("filters"), "Filters");

        let adjust_page = self.build_adjust_page();
        stack.add_titled(&adjust_page, Some("adjust"), "Adjust");

        // ── View switcher at top ─────────────────────────────────────────────
        let switcher = adw::ViewSwitcher::builder()
            .stack(&stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .margin_start(12)
            .margin_end(12)
            .margin_top(8)
            .margin_bottom(4)
            .build();

        self.root.append(&switcher);

        // ── Scrolled stack ───────────────────────────────────────────────────
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        scrolled.set_child(Some(&stack));
        self.root.append(&scrolled);

        // ── Revert button (shared across both views) ─────────────────────────
        let revert_btn = gtk::Button::builder()
            .label("Revert to Original")
            .tooltip_text("Remove all edits and restore the original image")
            .halign(gtk::Align::Center)
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

                // Reset filter buttons.
                for (_, btn) in filter_buttons.borrow().iter() {
                    btn.set_active(false);
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
                            Ok(Err(e)) => error!("revert edits failed: {e}"),
                            Err(e) => error!("revert edits join failed: {e}"),
                        }
                    });
                }
            });
        }
    }

    /// Build the "Filters" page: preset buttons + transform controls.
    fn build_filters_page(&self) -> gtk::Box {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.set_margin_top(12);
        page.set_margin_bottom(12);
        page.set_margin_start(12);
        page.set_margin_end(12);

        // ── Filter presets ───────────────────────────────────────────────────
        let filter_group = adw::PreferencesGroup::builder()
            .title("Filter")
            .build();

        let filter_box = gtk::FlowBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .homogeneous(true)
            .max_children_per_line(3)
            .min_children_per_line(2)
            .row_spacing(8)
            .column_spacing(8)
            .build();

        // "Original" button to clear the filter.
        let original_btn = gtk::ToggleButton::builder()
            .label("Original")
            .build();
        original_btn.add_css_class("flat");
        filter_box.append(&original_btn);

        {
            let filter_buttons = Rc::clone(&self.filter_buttons);
            self.filter_buttons.borrow_mut().push(("original".to_string(), original_btn.clone()));

            for name in FILTER_NAMES {
                let display_name = filter_display_name(name);
                let btn = gtk::ToggleButton::builder()
                    .label(display_name)
                    .build();
                btn.add_css_class("flat");
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
            let save_debounce_rc = Rc::clone(&self.save_debounce);
            let save_in_flight_rc = Rc::clone(&self.save_in_flight);
            let library_rc = Arc::clone(&self.library);
            let media_id_rc = Rc::clone(&self.media_id);
            let name = name.clone();

            btn.connect_clicked(move |clicked_btn| {
                if !clicked_btn.is_active() {
                    // Allow un-toggling — treat as "Original".
                    return;
                }

                // Deactivate other filter buttons.
                for (other_name, other_btn) in all_buttons.borrow().iter() {
                    if *other_name != name {
                        other_btn.set_active(false);
                    }
                }

                let preview = {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };

                    if name == "original" {
                        // Clear filter, reset exposure/color to defaults.
                        s.state.filter = None;
                        s.state.exposure = Default::default();
                        s.state.color = Default::default();
                    } else if let Some(preset) = filter_preset(&name) {
                        // Apply filter preset values.
                        s.state.exposure = preset.exposure;
                        s.state.color = preset.color;
                        s.state.filter = Some(name.clone());
                    }

                    s.render_gen += 1;
                    (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                };

                render_to_picture(&picture, &tokio, &session, preview);

                // Schedule auto-save.
                if let Some(id) = save_debounce_rc.take() {
                    id.remove();
                }
                let session = Rc::clone(&session);
                let media_id = Rc::clone(&media_id_rc);
                let library = Arc::clone(&library_rc);
                let tokio = tokio.clone();
                let save_in_flight = Rc::clone(&save_in_flight_rc);
                let save_debounce = Rc::clone(&save_debounce_rc);
                let source_id = glib::timeout_add_local_once(
                    std::time::Duration::from_millis(SAVE_DEBOUNCE_MS as u64),
                    move || {
                        save_debounce.set(None);
                        schedule_save(
                            &session,
                            &media_id,
                            &library,
                            &tokio,
                            &save_in_flight,
                        );
                    },
                );
                save_debounce_rc.set(Some(source_id));
            });
        }

        filter_group.add(&filter_box);
        page.append(&filter_group);

        // ── Transform group ──────────────────────────────────────────────────
        let transform_group = adw::PreferencesGroup::builder()
            .title("Transform")
            .build();

        let rotate_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .margin_top(4)
            .margin_bottom(4)
            .build();

        let rotate_ccw_btn = gtk::Button::builder()
            .icon_name("object-rotate-left-symbolic")
            .tooltip_text("Rotate 90\u{b0} Counter-Clockwise")
            .build();
        rotate_ccw_btn.add_css_class("flat");

        let rotate_cw_btn = gtk::Button::builder()
            .icon_name("object-rotate-right-symbolic")
            .tooltip_text("Rotate 90\u{b0} Clockwise")
            .build();
        rotate_cw_btn.add_css_class("flat");

        let flip_h_btn = gtk::ToggleButton::builder()
            .icon_name("object-flip-horizontal-symbolic")
            .tooltip_text("Flip Horizontal")
            .build();
        flip_h_btn.add_css_class("flat");

        let flip_v_btn = gtk::ToggleButton::builder()
            .icon_name("object-flip-vertical-symbolic")
            .tooltip_text("Flip Vertical")
            .build();
        flip_v_btn.add_css_class("flat");

        rotate_box.append(&rotate_ccw_btn);
        rotate_box.append(&rotate_cw_btn);
        rotate_box.append(&gtk::Separator::new(gtk::Orientation::Vertical));
        rotate_box.append(&flip_h_btn);
        rotate_box.append(&flip_v_btn);

        transform_group.add(&rotate_box);
        page.append(&transform_group);

        // Wire rotate CCW.
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            let panel_self = self.auto_save_closure();
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
                panel_self();
            });
        }

        // Wire rotate CW.
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            let panel_self = self.auto_save_closure();
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
                panel_self();
            });
        }

        // Wire flip horizontal.
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            let panel_self = self.auto_save_closure();
            flip_h_btn.connect_toggled(move |btn| {
                let preview = {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };
                    s.state.transforms.flip_horizontal = btn.is_active();
                    s.render_gen += 1;
                    (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                };
                render_to_picture(&picture, &tokio, &session, preview);
                panel_self();
            });
        }

        // Wire flip vertical.
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            let panel_self = self.auto_save_closure();
            flip_v_btn.connect_toggled(move |btn| {
                let preview = {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };
                    s.state.transforms.flip_vertical = btn.is_active();
                    s.render_gen += 1;
                    (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                };
                render_to_picture(&picture, &tokio, &session, preview);
                panel_self();
            });
        }

        page
    }

    /// Build the "Adjust" page: exposure and color sliders.
    fn build_adjust_page(&self) -> gtk::Box {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.set_margin_top(12);
        page.set_margin_bottom(12);
        page.set_margin_start(12);
        page.set_margin_end(12);

        // ── Exposure group ────────────────────────────────────────────────────
        let exposure_group = adw::PreferencesGroup::builder()
            .title("Exposure")
            .build();

        let brightness = self.make_slider("Brightness", |s| &mut s.exposure.brightness);
        let contrast = self.make_slider("Contrast", |s| &mut s.exposure.contrast);
        let highlights = self.make_slider("Highlights", |s| &mut s.exposure.highlights);
        let shadows = self.make_slider("Shadows", |s| &mut s.exposure.shadows);
        let white_balance =
            self.make_slider("White Balance", |s| &mut s.exposure.white_balance);

        exposure_group.add(&brightness);
        exposure_group.add(&contrast);
        exposure_group.add(&highlights);
        exposure_group.add(&shadows);
        exposure_group.add(&white_balance);
        page.append(&exposure_group);

        // ── Color group ───────────────────────────────────────────────────────
        let color_group = adw::PreferencesGroup::builder()
            .title("Color")
            .build();

        let saturation = self.make_slider("Saturation", |s| &mut s.color.saturation);
        let vibrance = self.make_slider("Vibrance", |s| &mut s.color.vibrance);
        let hue = self.make_slider("Hue", |s| &mut s.color.hue_shift);
        let temperature = self.make_slider("Temperature", |s| &mut s.color.temperature);
        let tint = self.make_slider("Tint", |s| &mut s.color.tint);

        color_group.add(&saturation);
        color_group.add(&vibrance);
        color_group.add(&hue);
        color_group.add(&temperature);
        color_group.add(&tint);
        page.append(&color_group);

        page
    }

    /// Create a slider with label above and scale below.
    fn make_slider<F>(&self, label: &str, accessor: F) -> gtk::Box
    where
        F: Fn(&mut EditState) -> &mut f64 + 'static,
    {
        let label_widget = gtk::Label::builder()
            .label(label)
            .halign(gtk::Align::Start)
            .build();
        label_widget.add_css_class("dim-label");
        label_widget.add_css_class("caption");

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
        row.append(&label_widget);
        row.append(&scale);

        // Connect value-changed to update the edit state and schedule a
        // debounced render. The state is updated immediately so it's always
        // current, but the expensive render only fires after the slider stops
        // moving for RENDER_DEBOUNCE_MS milliseconds.
        let session = Rc::clone(&self.session);
        let picture = self.picture.clone();
        let tokio = self.tokio.clone();
        let render_debounce = Rc::clone(&self.render_debounce);
        let auto_save = self.auto_save_closure();

        scale.connect_value_changed(move |scale| {
            let value = scale.value();
            let value = if value.abs() < 0.02 { 0.0 } else { value };

            {
                let mut session = session.borrow_mut();
                let Some(s) = session.as_mut() else { return };
                *accessor(&mut s.state) = value;
                s.render_gen += 1;
            }

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

            let source_id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(SAVE_DEBOUNCE_MS as u64),
                move || {
                    save_debounce_inner.set(None);
                    schedule_save(&session, &media_id, &library, &tokio, &save_in_flight);
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
            Ok(Err(e)) => error!("auto-save failed: {e}"),
            Err(e) => error!("auto-save join failed: {e}"),
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

/// Convert a filter preset name to a user-facing display name.
fn filter_display_name(name: &str) -> &str {
    match name {
        "bw" => "B\u{26}W",
        "vintage" => "Vintage",
        "warm" => "Warm",
        "cool" => "Cool",
        "vivid" => "Vivid",
        "fade" => "Fade",
        _ => name,
    }
}
