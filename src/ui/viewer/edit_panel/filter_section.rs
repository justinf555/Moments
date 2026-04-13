use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::filters::{filter_registry, Filter};

use super::filter_swatch::{EditFilterSwatch, OnFilterSelected};
use super::EditSession;

/// Delay before rendering preview after the last strength slider change (ms).
const RENDER_DEBOUNCE_MS: u32 = 50;

mod imp {
    use super::*;
    use std::cell::{Cell, OnceCell};

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/viewer/edit_panel/filter_section.ui")]
    pub struct EditFilterSection {
        #[template_child]
        pub expander: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub subtitle_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub filter_grid: TemplateChild<gtk::FlowBox>,
        #[template_child]
        pub strength_scale: TemplateChild<gtk::Scale>,
        #[template_child]
        pub strength_value_label: TemplateChild<gtk::Label>,

        pub session: OnceCell<Rc<RefCell<Option<EditSession>>>>,
        pub changed_cb: OnceCell<Rc<dyn Fn()>>,
        pub render_debounce: Cell<Option<glib::SourceId>>,
        /// All filter swatch widgets.
        pub swatches: RefCell<Vec<EditFilterSwatch>>,
        /// The currently active filter trait object (for strength scaling).
        /// Always `Some` after `setup()`.
        pub active_filter: RefCell<Option<Rc<dyn Filter>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EditFilterSection {
        const NAME: &'static str = "MomentsEditFilterSection";
        type Type = super::EditFilterSection;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            EditFilterSwatch::ensure_type();

            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for EditFilterSection {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for EditFilterSection {}
}

glib::wrapper! {
    pub struct EditFilterSection(ObjectSubclass<imp::EditFilterSection>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl EditFilterSection {
    /// Access the expander row (for single-expansion wiring).
    pub fn expander(&self) -> &adw::ExpanderRow {
        &self.imp().expander
    }

    /// Build filter swatches and wire the strength slider.
    pub fn setup(&self, session: Rc<RefCell<Option<EditSession>>>, changed: impl Fn() + 'static) {
        let imp = self.imp();
        let changed: Rc<dyn Fn()> = Rc::new(changed);
        let _ = imp.session.set(Rc::clone(&session));
        let _ = imp.changed_cb.set(Rc::clone(&changed));

        // ── Build swatch grid from registry ─────────────────────────────────
        for filter in filter_registry() {
            let filter: Rc<dyn Filter> = Rc::from(filter);
            let swatch = EditFilterSwatch::new(Rc::clone(&filter));
            imp.filter_grid.append(&swatch);
            imp.swatches.borrow_mut().push(swatch);
        }

        // ── Wire filter swatch clicks ───────────────────────────────────────
        let on_selected: OnFilterSelected = {
            let weak = self.downgrade();
            Rc::new(move |filter: &Rc<dyn Filter>| {
                let Some(section) = weak.upgrade() else {
                    return;
                };
                let simp = section.imp();

                // Deactivate other swatches — the clicked one is already active.
                let active_name = filter.name();
                for sw in simp.swatches.borrow().iter() {
                    if sw.filter().name() != active_name {
                        sw.toggle().set_active(false);
                    }
                }

                // Update subtitle and active filter.
                simp.subtitle_label.set_label(filter.display_name());
                *simp.active_filter.borrow_mut() = Some(Rc::clone(filter));
            })
        };

        for sw in imp.swatches.borrow().iter() {
            sw.setup(
                Rc::clone(&session),
                Rc::clone(&changed),
                Rc::clone(&on_selected),
            );
        }

        // ── Wire strength slider ────────────────────────────────────────────
        let session_ref = Rc::clone(&session);
        let changed_ref = Rc::clone(&changed);
        let weak = self.downgrade();

        imp.strength_scale.connect_value_changed(move |scale| {
            let Some(section) = weak.upgrade() else {
                return;
            };
            let simp = section.imp();
            let strength = scale.value();

            // Update numeric display.
            simp.strength_value_label
                .set_label(&format!("{}", (strength * 100.0).round() as i32));

            // Scale the active filter's values by strength.
            {
                let active = simp.active_filter.borrow();
                let Some(ref active) = *active else { return };
                let preset = active.preset();
                let mut session = session_ref.borrow_mut();
                let Some(s) = session.as_mut() else { return };

                s.state.apply_filter_at_strength(&preset, strength);
                s.render_gen += 1;
            }

            // Debounce render for continuous slider movement.
            if let Some(id) = simp.render_debounce.take() {
                id.remove();
            }

            let changed_inner = Rc::clone(&changed_ref);
            let weak_inner = section.downgrade();
            let source_id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(RENDER_DEBOUNCE_MS as u64),
                move || {
                    if let Some(s) = weak_inner.upgrade() {
                        s.imp().render_debounce.set(None);
                    }
                    changed_inner();
                },
            );
            simp.render_debounce.set(Some(source_id));
        });
    }

    /// Update UI to match the current session state.
    pub fn sync_from_state(&self) {
        let imp = self.imp();
        let Some(session_rc) = imp.session.get() else {
            return;
        };

        let filter_name = {
            let session = session_rc.borrow();
            session
                .as_ref()
                .and_then(|s| s.state.filter.clone())
                .unwrap_or_else(|| "none".to_string())
        };

        // Sync swatch toggles and active_filter.
        for sw in imp.swatches.borrow().iter() {
            let is_active = sw.filter().name() == filter_name;
            sw.toggle().set_active(is_active);
            if is_active {
                *imp.active_filter.borrow_mut() = Some(Rc::clone(sw.filter()));
                imp.subtitle_label.set_label(sw.filter().display_name());
            }
        }
    }

    /// Reset all filter UI to default state.
    pub fn reset(&self) {
        let imp = self.imp();
        // Activate the "none" swatch (first in registry).
        for sw in imp.swatches.borrow().iter() {
            let is_none = sw.filter().name() == "none";
            sw.toggle().set_active(is_none);
        }
        imp.subtitle_label.set_label("None");
        imp.strength_scale.set_value(1.0);
        imp.strength_value_label.set_label("100");
    }
}
