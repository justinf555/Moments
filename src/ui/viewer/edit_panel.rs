use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::time::Instant;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};
use image::DynamicImage;
use tracing::{debug, error, warn};

use crate::library::edit_renderer::apply_edits;
use crate::library::editing::EditState;
use crate::library::media::MediaId;
use crate::library::Library;
use crate::ui::widgets::wire_single_expansion;

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
    pub(super) render_gen: u64,
}

// ── GObject subclass ─────────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::OnceCell;

    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/viewer/edit_panel.ui")]
    pub struct EditPanel {
        // Template children
        #[template_child]
        pub transform_expander: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub filters_expander: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub adjust_expander: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub filter_subtitle: TemplateChild<gtk::Label>,
        #[template_child]
        pub adjust_subtitle: TemplateChild<gtk::Label>,
        #[template_child]
        pub revert_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub rotate_ccw_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub rotate_cw_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub flip_h_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub flip_v_btn: TemplateChild<gtk::Button>,

        // Service dependencies (set once in setup)
        pub picture: OnceCell<gtk::Picture>,
        pub library: OnceCell<Arc<dyn Library>>,
        pub tokio: OnceCell<tokio::runtime::Handle>,
        pub bus_sender: OnceCell<crate::event_bus::EventSender>,

        // Mutable state
        pub session: RefCell<Option<super::EditSession>>,
        pub media_id: RefCell<Option<MediaId>>,
        pub render_debounce: Cell<Option<glib::SourceId>>,
        pub save_debounce: Cell<Option<glib::SourceId>>,
        pub save_in_flight: Cell<bool>,
        pub filter_buttons: RefCell<Vec<(String, gtk::ToggleButton)>>,
        pub adjust_scales: RefCell<Vec<gtk::Scale>>,
    }

    impl EditPanel {
        pub fn picture(&self) -> &gtk::Picture {
            self.picture.get().expect("picture not initialized")
        }
        pub fn library(&self) -> &Arc<dyn Library> {
            self.library.get().expect("library not initialized")
        }
        pub fn tokio(&self) -> &tokio::runtime::Handle {
            self.tokio.get().expect("tokio not initialized")
        }
        pub fn bus_sender(&self) -> &crate::event_bus::EventSender {
            self.bus_sender.get().expect("bus_sender not initialized")
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EditPanel {
        const NAME: &'static str = "MomentsEditPanel";
        type Type = super::EditPanel;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for EditPanel {
        fn dispose(&self) {
            self.dispose_template();
        }
    }
    impl WidgetImpl for EditPanel {}
}

glib::wrapper! {
    pub struct EditPanel(ObjectSubclass<imp::EditPanel>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for EditPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl EditPanel {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Inject service dependencies and wire signal handlers.
    pub fn setup(
        &self,
        picture: gtk::Picture,
        library: Arc<dyn Library>,
        tokio: tokio::runtime::Handle,
        bus_sender: crate::event_bus::EventSender,
    ) {
        let imp = self.imp();
        assert!(imp.picture.set(picture).is_ok(), "setup called twice");
        assert!(imp.library.set(library).is_ok(), "setup called twice");
        assert!(imp.tokio.set(tokio).is_ok(), "setup called twice");
        assert!(imp.bus_sender.set(bus_sender).is_ok(), "setup called twice");

        // Wire single-expansion: only one section open at a time.
        wire_single_expansion(&[
            &imp.transform_expander,
            &imp.filters_expander,
            &imp.adjust_expander,
        ]);

        // Build dynamic content and wire signals.
        self.build_filters_content();
        self.build_adjust_content();
        self.wire_transform_buttons();
        self.wire_revert_button();
    }

    /// Start an editing session for the given media item.
    pub fn begin_session(
        &self,
        id: MediaId,
        preview_image: Arc<DynamicImage>,
        existing_state: Option<EditState>,
    ) {
        let imp = self.imp();
        let state = existing_state.unwrap_or_default();

        debug!(media_id = %id, "begin edit session");

        *imp.media_id.borrow_mut() = Some(id);
        *imp.session.borrow_mut() = Some(EditSession {
            state,
            preview_image,
            render_gen: 0,
        });

        // Sync filter button and slider state.
        self.sync_ui_from_state();

        // Render initial preview if state is not identity.
        let is_identity = imp
            .session
            .borrow()
            .as_ref()
            .map(|s| s.state.is_identity())
            .unwrap_or(true);
        if !is_identity {
            self.render_preview();
        }
    }

    /// End the current editing session, auto-saving any pending changes.
    pub fn end_session(&self) {
        let imp = self.imp();

        // Cancel any pending save debounce — we'll save immediately.
        if let Some(id) = imp.save_debounce.take() {
            id.remove();
        }

        // Persist current state before closing.
        self.save_to_db("navigate away");

        let media_id = imp.media_id.borrow().clone();
        if let Some(id) = &media_id {
            debug!(media_id = %id, "end edit session");
        }

        *imp.session.borrow_mut() = None;
        *imp.media_id.borrow_mut() = None;
    }

    // ── Auto-save ────────────────────────────────────────────────────────────

    /// Persist the current edit state to the database.
    fn save_to_db(&self, reason: &'static str) {
        let imp = self.imp();
        let (id, state) = {
            let session = imp.session.borrow();
            let Some(session) = session.as_ref() else {
                return;
            };
            let Some(id) = imp.media_id.borrow().clone() else {
                return;
            };

            // Don't persist identity state — delete instead if it exists.
            if session.state.is_identity() {
                let lib = Arc::clone(imp.library());
                let tk = imp.tokio().clone();
                let id_log = id.clone();
                let tx = imp.bus_sender().clone();
                glib::MainContext::default().spawn_local(async move {
                    let start = Instant::now();
                    let result = tk.spawn(async move { lib.revert_edits(&id).await }).await;
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

        if imp.save_in_flight.get() {
            debug!(media_id = %id, reason, "save skipped — write already in-flight");
            return;
        }

        imp.save_in_flight.set(true);
        let lib = Arc::clone(imp.library());
        let tk = imp.tokio().clone();
        let id_log = id.clone();
        let tx = imp.bus_sender().clone();

        let weak = self.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let start = Instant::now();
            let result = tk
                .spawn(async move { lib.save_edit_state(&id, &state).await })
                .await;
            let elapsed = start.elapsed();

            if let Some(panel) = weak.upgrade() {
                panel.imp().save_in_flight.set(false);
            }

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

    /// Create a closure that schedules a debounced auto-save.
    pub(super) fn auto_save_closure(&self) -> impl Fn() {
        let weak = self.downgrade();

        move || {
            let Some(panel) = weak.upgrade() else { return };
            let imp = panel.imp();

            // Cancel any pending save timer.
            if let Some(id) = imp.save_debounce.take() {
                id.remove();
            }

            let weak_inner = panel.downgrade();
            let source_id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(SAVE_DEBOUNCE_MS as u64),
                move || {
                    let Some(panel) = weak_inner.upgrade() else {
                        return;
                    };
                    panel.imp().save_debounce.set(None);
                    panel.save_to_db("auto-save");
                },
            );
            imp.save_debounce.set(Some(source_id));
        }
    }

    /// Render the current edit state as a preview.
    pub(super) fn render_preview(&self) {
        let imp = self.imp();
        let preview = {
            let session = imp.session.borrow();
            let Some(s) = session.as_ref() else { return };
            (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
        };
        self.render_to_picture(preview);
    }

    /// Render an edit state preview and display it on the picture widget.
    pub(super) fn render_to_picture(&self, preview: (Arc<DynamicImage>, EditState, u64)) {
        let imp = self.imp();
        let pic = imp.picture().clone();
        let tk = imp.tokio().clone();
        let (preview_img, state, gen) = preview;

        let weak = self.downgrade();
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

            let Some(panel) = weak.upgrade() else { return };
            let current_gen = panel.imp().session.borrow().as_ref().map(|s| s.render_gen);
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
        let imp = self.imp();
        let filter_name = {
            let session = imp.session.borrow();
            session.as_ref().and_then(|s| s.state.filter.clone())
        };

        // Sync filter buttons.
        for (name, btn) in imp.filter_buttons.borrow().iter() {
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
        imp.filter_subtitle.set_label(display);
    }

    // ── Signal wiring ────────────────────────────────────────────────────────

    fn wire_revert_button(&self) {
        let weak = self.downgrade();
        self.imp().revert_btn.connect_clicked(move |_| {
            let Some(panel) = weak.upgrade() else { return };
            let imp = panel.imp();

            // Cancel any pending auto-save.
            if let Some(id) = imp.save_debounce.take() {
                id.remove();
            }

            // Reset state.
            {
                let mut session = imp.session.borrow_mut();
                if let Some(s) = session.as_mut() {
                    s.state = EditState::default();
                }
            }

            // Reset filter buttons and subtitle.
            for (_, btn) in imp.filter_buttons.borrow().iter() {
                btn.set_active(false);
            }
            imp.filter_subtitle.set_label("None");

            // Reset all adjust sliders to 0.
            for scale in imp.adjust_scales.borrow().iter() {
                scale.set_value(0.0);
            }
            imp.adjust_subtitle.set_label("No changes");

            // Re-render original via the shared render pipeline.
            let preview = {
                let session = imp.session.borrow();
                session.as_ref().map(|s| {
                    (
                        Arc::clone(&s.preview_image),
                        EditState::default(),
                        s.render_gen,
                    )
                })
            };
            if let Some(preview) = preview {
                panel.render_to_picture(preview);
            }

            // Delete from DB.
            let id = imp.media_id.borrow().clone();
            if let Some(id) = id {
                let lib = Arc::clone(imp.library());
                let tk = imp.tokio().clone();
                let id_log = id.clone();
                let tx = imp.bus_sender().clone();
                glib::MainContext::default().spawn_local(async move {
                    let start = Instant::now();
                    let result = tk.spawn(async move { lib.revert_edits(&id).await }).await;
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
}
