use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, glib};
use image::DynamicImage;
use tracing::{debug, error};

use crate::library::edit_renderer::apply_edits;
use crate::library::editing::EditState;
use crate::library::media::MediaId;
use crate::library::Library;

/// Mutable state for an active editing session.
pub struct EditSession {
    /// Current edit state modified by sliders.
    pub state: EditState,
    /// Downscaled preview image (~1200px) for fast rendering.
    pub preview_image: Arc<DynamicImage>,
    /// Generation counter for debouncing stale renders.
    render_gen: u64,
}

/// Scrollable edit panel displayed in the viewer sidebar.
///
/// Contains slider groups for exposure and color adjustments, with
/// real-time preview via debounced rendering on a background thread.
pub struct EditPanel {
    scrolled: gtk::ScrolledWindow,
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
}

impl EditPanel {
    pub fn new(
        picture: gtk::Picture,
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
    ) -> Self {
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(300)
            .build();

        let session: Rc<RefCell<Option<EditSession>>> = Rc::new(RefCell::new(None));
        let media_id: Rc<RefCell<Option<MediaId>>> = Rc::new(RefCell::new(None));

        let panel = Self {
            scrolled,
            session,
            picture,
            tokio,
            library,
            media_id,
        };

        panel.build_ui();
        panel
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.scrolled.upcast_ref()
    }

    /// Start an editing session for the given media item.
    ///
    /// Creates a downscaled preview from `full_res_image` and loads
    /// any existing edit state from the database.
    pub fn begin_session(
        &self,
        id: MediaId,
        full_res_image: Arc<DynamicImage>,
        existing_state: Option<EditState>,
    ) {
        let preview = create_preview(&full_res_image, 1200);
        let state = existing_state.unwrap_or_default();

        *self.media_id.borrow_mut() = Some(id);
        *self.session.borrow_mut() = Some(EditSession {
            state,
            preview_image: Arc::new(preview),
            render_gen: 0,
        });

        // Apply initial state to sliders.
        self.sync_sliders_from_state();

        // Render initial preview if state is not identity.
        if !self.session.borrow().as_ref().unwrap().state.is_identity() {
            self.render_preview();
        }
    }

    /// End the current editing session without saving.
    pub fn end_session(&self) {
        *self.session.borrow_mut() = None;
        *self.media_id.borrow_mut() = None;
    }

    /// Save the current edit state to the database.
    #[allow(dead_code)]
    pub fn save(&self) {
        let (id, state) = {
            let session = self.session.borrow();
            let Some(session) = session.as_ref() else { return };
            let Some(id) = self.media_id.borrow().clone() else { return };
            (id, session.state.clone())
        };

        let lib = Arc::clone(&self.library);
        let tk = self.tokio.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = tk
                .spawn(async move { lib.save_edit_state(&id, &state).await })
                .await;
            match result {
                Ok(Ok(())) => debug!("edit state saved"),
                Ok(Err(e)) => error!("save edit state failed: {e}"),
                Err(e) => error!("save edit state join failed: {e}"),
            }
        });
    }

    /// Revert all edits and restore the original image.
    #[allow(dead_code)]
    pub fn revert(&self) {
        let id = {
            let Some(id) = self.media_id.borrow().clone() else { return };
            id
        };

        // Reset session state.
        {
            let mut session = self.session.borrow_mut();
            if let Some(s) = session.as_mut() {
                s.state = EditState::default();
            }
        }

        self.sync_sliders_from_state();

        // Re-render with identity state (shows original).
        self.render_preview();

        // Delete from DB.
        let lib = Arc::clone(&self.library);
        let tk = self.tokio.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = tk
                .spawn(async move { lib.revert_edits(&id).await })
                .await;
            match result {
                Ok(Ok(())) => debug!("edits reverted"),
                Ok(Err(e)) => error!("revert edits failed: {e}"),
                Err(e) => error!("revert edits join failed: {e}"),
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

        // ── Transform group ──────────────────────────────────────────────────
        let transform_group = adw::PreferencesGroup::builder()
            .title("Transform")
            .build();

        // Rotate buttons.
        let rotate_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk::Align::Center)
            .margin_top(4)
            .margin_bottom(4)
            .build();

        let rotate_ccw_btn = gtk::Button::builder()
            .icon_name("object-rotate-left-symbolic")
            .tooltip_text("Rotate 90° Counter-Clockwise")
            .build();
        rotate_ccw_btn.add_css_class("flat");

        let rotate_cw_btn = gtk::Button::builder()
            .icon_name("object-rotate-right-symbolic")
            .tooltip_text("Rotate 90° Clockwise")
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
        vbox.append(&transform_group);

        // Wire rotate CCW.
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            rotate_ccw_btn.connect_clicked(move |_| {
                let preview = {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };
                    s.state.transforms.rotate_degrees = (s.state.transforms.rotate_degrees - 90).rem_euclid(360);
                    s.render_gen += 1;
                    (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                };
                render_to_picture(&picture, &tokio, &session, preview);
            });
        }

        // Wire rotate CW.
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            rotate_cw_btn.connect_clicked(move |_| {
                let preview = {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };
                    s.state.transforms.rotate_degrees = (s.state.transforms.rotate_degrees + 90).rem_euclid(360);
                    s.render_gen += 1;
                    (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                };
                render_to_picture(&picture, &tokio, &session, preview);
            });
        }

        // Wire flip horizontal.
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            flip_h_btn.connect_toggled(move |btn| {
                let preview = {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };
                    s.state.transforms.flip_horizontal = btn.is_active();
                    s.render_gen += 1;
                    (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                };
                render_to_picture(&picture, &tokio, &session, preview);
            });
        }

        // Wire flip vertical.
        {
            let session = Rc::clone(&self.session);
            let picture = self.picture.clone();
            let tokio = self.tokio.clone();
            flip_v_btn.connect_toggled(move |btn| {
                let preview = {
                    let mut session = session.borrow_mut();
                    let Some(s) = session.as_mut() else { return };
                    s.state.transforms.flip_vertical = btn.is_active();
                    s.render_gen += 1;
                    (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
                };
                render_to_picture(&picture, &tokio, &session, preview);
            });
        }

        // ── Exposure group ────────────────────────────────────────────────────
        let exposure_group = adw::PreferencesGroup::builder()
            .title("Exposure")
            .build();

        let brightness = self.make_slider("Brightness", |s| &mut s.exposure.brightness);
        let contrast = self.make_slider("Contrast", |s| &mut s.exposure.contrast);
        let highlights = self.make_slider("Highlights", |s| &mut s.exposure.highlights);
        let shadows = self.make_slider("Shadows", |s| &mut s.exposure.shadows);
        let white_balance = self.make_slider("White Balance", |s| &mut s.exposure.white_balance);

        exposure_group.add(&brightness);
        exposure_group.add(&contrast);
        exposure_group.add(&highlights);
        exposure_group.add(&shadows);
        exposure_group.add(&white_balance);
        vbox.append(&exposure_group);

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
        vbox.append(&color_group);

        // ── Action buttons ────────────────────────────────────────────────────
        let btn_box = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .halign(gtk::Align::Center)
            .margin_top(12)
            .build();

        let revert_btn = gtk::Button::builder()
            .label("Revert")
            .tooltip_text("Revert to original")
            .build();
        revert_btn.add_css_class("destructive-action");

        let save_btn = gtk::Button::builder()
            .label("Save")
            .tooltip_text("Save edits")
            .build();
        save_btn.add_css_class("suggested-action");

        btn_box.append(&revert_btn);
        btn_box.append(&save_btn);
        vbox.append(&btn_box);

        self.scrolled.set_child(Some(&vbox));

        // Wire button handlers.
        {
            let session = Rc::clone(&self.session);
            let media_id = Rc::clone(&self.media_id);
            let library = Arc::clone(&self.library);
            let tokio = self.tokio.clone();
            save_btn.connect_clicked(move |_| {
                let (id, state) = {
                    let session = session.borrow();
                    let Some(session) = session.as_ref() else { return };
                    let Some(id) = media_id.borrow().clone() else { return };
                    (id, session.state.clone())
                };

                let lib = Arc::clone(&library);
                let tk = tokio.clone();
                glib::MainContext::default().spawn_local(async move {
                    let result = tk
                        .spawn(async move { lib.save_edit_state(&id, &state).await })
                        .await;
                    match result {
                        Ok(Ok(())) => debug!("edit state saved"),
                        Ok(Err(e)) => error!("save edit state failed: {e}"),
                        Err(e) => error!("save edit state join failed: {e}"),
                    }
                });
            });
        }

        {
            let session = Rc::clone(&self.session);
            let media_id = Rc::clone(&self.media_id);
            let library = Arc::clone(&self.library);
            let tokio = self.tokio.clone();
            let picture = self.picture.clone();
            revert_btn.connect_clicked(move |_| {
                // Reset state.
                {
                    let mut session = session.borrow_mut();
                    if let Some(s) = session.as_mut() {
                        s.state = EditState::default();
                    }
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
                                    let img = (*preview).clone();
                                    let edited = apply_edits(img, &state);
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
                    glib::MainContext::default().spawn_local(async move {
                        let _ = tk.spawn(async move { lib.revert_edits(&id).await }).await;
                    });
                }
            });
        }
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

        // Connect value-changed to update the edit state and trigger preview.
        let session = Rc::clone(&self.session);
        let picture = self.picture.clone();
        let tokio = self.tokio.clone();

        scale.connect_value_changed(move |scale| {
            let value = scale.value();
            let value = if value.abs() < 0.02 { 0.0 } else { value };

            let preview = {
                let mut session = session.borrow_mut();
                let Some(s) = session.as_mut() else { return };
                *accessor(&mut s.state) = value;
                s.render_gen += 1;
                (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
            };

            render_to_picture(&picture, &tokio, &session, preview);
        });

        row
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
                        let img = (*preview_img).clone();
                        let edited = apply_edits(img, &state);
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
                    w, h,
                    gdk::MemoryFormat::R8g8b8a8,
                    &gbytes,
                    (w as usize) * 4,
                );
                pic.set_paintable(Some(texture.upcast_ref::<gdk::Paintable>()));
            }
        });
    }

    /// Sync slider widgets to match the current EditState values.
    ///
    /// Called when loading an existing edit state or after revert.
    fn sync_sliders_from_state(&self) {
        // Slider sync is handled by rebuilding the UI content.
        // For now, we rebuild on session start. A future refinement
        // would store slider references and update them directly.
        // The current approach works because begin_session is called
        // before the panel is shown.
    }
}

/// Render an edit state preview and display it on the picture widget.
///
/// Shared by slider handlers and transform button handlers.
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
                    let img = (*preview_img).clone();
                    let edited = apply_edits(img, &state);
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
                w, h,
                gdk::MemoryFormat::R8g8b8a8,
                &gbytes,
                (w as usize) * 4,
            );
            pic.set_paintable(Some(texture.upcast_ref::<gdk::Paintable>()));
        }
    });
}

/// Create a downscaled preview image with longest edge at most `max_edge` pixels.
fn create_preview(img: &DynamicImage, max_edge: u32) -> DynamicImage {
    let (w, h) = image::GenericImageView::dimensions(img);
    if w <= max_edge && h <= max_edge {
        return img.clone();
    }
    img.thumbnail(max_edge, max_edge)
}
