mod adjust_section;
mod adjustments;
mod filter_section;
mod filter_swatch;
mod filters;
mod transform_section;
mod transforms;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gdk, glib};
use image::DynamicImage;
use tracing::{debug, error};

use adjust_section::EditAdjustSection;
use filter_section::EditFilterSection;
use transform_section::EditTransformSection;

use crate::library::editing::EditState;
use crate::library::media::MediaId;
use crate::renderer::edits::apply_edits;
use crate::ui::widgets::wire_single_expansion;

/// Delay before auto-saving edit state to DB after the last change (milliseconds).
const SAVE_DEBOUNCE_MS: u32 = 100;

/// Mutable state for an active editing session.
pub struct EditSession {
    /// Current edit state modified by sections.
    pub state: EditState,
    /// Downscaled preview image (~1200px) for fast rendering.
    /// Shared via `Arc` — render tasks read from it without cloning.
    pub preview_image: Arc<DynamicImage>,
    /// Generation counter for discarding stale render results.
    pub render_gen: u64,
}

// ── GObject subclass ─────────────────────────────────────────────────────────

mod imp {
    use super::*;
    use std::cell::OnceCell;

    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/viewer/edit_panel/edit_panel.ui")]
    pub struct EditPanel {
        // Section template children
        #[template_child]
        pub transform_section: TemplateChild<EditTransformSection>,
        #[template_child]
        pub filter_section: TemplateChild<EditFilterSection>,
        #[template_child]
        pub adjust_section: TemplateChild<EditAdjustSection>,
        #[template_child]
        pub revert_btn: TemplateChild<gtk::Button>,

        // Service dependencies (set once in setup)
        pub picture: OnceCell<gtk::Picture>,
        pub bus_sender: OnceCell<crate::event_bus::EventSender>,

        // Shared session — same Rc passed to all sections
        pub session: OnceCell<Rc<RefCell<Option<EditSession>>>>,

        // Per-item tracking
        pub media_id: RefCell<Option<MediaId>>,
        pub save_debounce: Cell<Option<glib::SourceId>>,
        pub save_in_flight: Cell<bool>,
    }

    impl EditPanel {
        pub fn picture(&self) -> &gtk::Picture {
            self.picture.get().expect("picture not initialized")
        }
        pub fn bus_sender(&self) -> &crate::event_bus::EventSender {
            self.bus_sender.get().expect("bus_sender not initialized")
        }
        pub fn session_rc(&self) -> &Rc<RefCell<Option<EditSession>>> {
            self.session.get().expect("session not initialized")
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EditPanel {
        const NAME: &'static str = "MomentsEditPanel";
        type Type = super::EditPanel;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            EditTransformSection::ensure_type();
            EditFilterSection::ensure_type();
            EditAdjustSection::ensure_type();

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
    pub fn setup(&self, picture: gtk::Picture, bus_sender: crate::event_bus::EventSender) {
        let imp = self.imp();
        assert!(imp.picture.set(picture).is_ok(), "setup called twice");
        assert!(imp.bus_sender.set(bus_sender).is_ok(), "setup called twice");

        // Create the shared session.
        let session: Rc<RefCell<Option<EditSession>>> = Rc::new(RefCell::new(None));
        let _ = imp.session.set(Rc::clone(&session));

        // Wire single-expansion: only one section open at a time.
        wire_single_expansion(&[
            imp.transform_section.expander(),
            imp.filter_section.expander(),
            imp.adjust_section.expander(),
        ]);

        // Create the shared changed callback that triggers render + save.
        let changed = self.make_changed_callback();

        // Set up each section with the shared session and callback.
        imp.transform_section
            .setup(Rc::clone(&session), changed.clone());
        imp.filter_section
            .setup(Rc::clone(&session), changed.clone());
        imp.adjust_section.setup(Rc::clone(&session), changed);

        self.wire_revert_button();
    }

    /// Create a callback closure that sections call after mutating the session.
    ///
    /// This triggers a preview render and schedules an auto-save.
    fn make_changed_callback(&self) -> impl Fn() + Clone + 'static {
        let weak = self.downgrade();
        let auto_save = self.auto_save_closure();

        move || {
            let Some(panel) = weak.upgrade() else { return };
            panel.render_preview();
            auto_save();
        }
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
        *imp.session_rc().borrow_mut() = Some(EditSession {
            state,
            preview_image,
            render_gen: 0,
        });

        // Sync section UI from state.
        imp.filter_section.sync_from_state();
        imp.adjust_section.sync_from_state();

        // Render initial preview if state is not identity.
        let is_identity = imp
            .session_rc()
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

        *imp.session_rc().borrow_mut() = None;
        *imp.media_id.borrow_mut() = None;
    }

    // ── Auto-save ────────────────────────────────────────────────────────────

    /// Persist the current edit state to the database.
    fn save_to_db(&self, reason: &'static str) {
        let imp = self.imp();
        let (id, state) = {
            let session = imp.session_rc().borrow();
            let Some(session) = session.as_ref() else {
                return;
            };
            let Some(id) = imp.media_id.borrow().clone() else {
                return;
            };

            // Don't persist identity state — delete instead if it exists.
            if session.state.is_identity() {
                let id_log = id.clone();
                let mc = crate::application::MomentsApplication::default()
                    .media_client()
                    .expect("media client available");
                mc.revert_edits(&id, move |result| match result {
                    Ok(()) => debug!(media_id = %id_log, reason, "delete identity edit state"),
                    Err(e) => {
                        error!("delete edit state failed: {e}");
                        crate::client::show_toast("Could not revert edits");
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
        let id_log = id.clone();
        let mc = crate::application::MomentsApplication::default()
            .media_client()
            .expect("media client available");

        let weak = self.downgrade();
        mc.save_edit_state(&id, &state, move |result| {
            if let Some(panel) = weak.upgrade() {
                panel.imp().save_in_flight.set(false);
            }

            match result {
                Ok(()) => {
                    debug!(media_id = %id_log, reason, "save edit state");
                }
                Err(e) => {
                    error!("save edit state failed: {e}");
                    crate::client::show_toast("Could not save edits");
                }
            }
        });
    }

    /// Create a closure that schedules a debounced auto-save.
    fn auto_save_closure(&self) -> impl Fn() + Clone + 'static {
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
    fn render_preview(&self) {
        let imp = self.imp();
        let preview = {
            let session = imp.session_rc().borrow();
            let Some(s) = session.as_ref() else { return };
            (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
        };
        self.render_to_picture(preview);
    }

    /// Render an edit state preview and display it on the picture widget.
    fn render_to_picture(&self, preview: (Arc<DynamicImage>, EditState, u64)) {
        let imp = self.imp();
        let pic = imp.picture().clone();
        let tk = crate::application::MomentsApplication::default().tokio_handle();
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
            let current_gen = panel
                .imp()
                .session_rc()
                .borrow()
                .as_ref()
                .map(|s| s.render_gen);
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

    // ── Revert ───────────────────────────────────────────────────────────────

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
                let mut session = imp.session_rc().borrow_mut();
                if let Some(s) = session.as_mut() {
                    s.state = EditState::default();
                }
            }

            // Reset section UI.
            imp.filter_section.reset();
            imp.adjust_section.reset();

            // Re-render original via the shared render pipeline.
            let preview = {
                let session = imp.session_rc().borrow();
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
                let id_log = id.clone();
                let mc = crate::application::MomentsApplication::default()
                    .media_client()
                    .expect("media client available");
                mc.revert_edits(&id, move |result| match result {
                    Ok(()) => debug!(media_id = %id_log, "revert edits"),
                    Err(e) => {
                        error!("revert edits failed: {e}");
                        crate::client::show_toast("Could not revert edits");
                    }
                });
            }
        });
    }
}
