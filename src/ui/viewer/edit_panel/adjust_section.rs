use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use super::adjustments::{adjustment_registry, AdjustGroup};
use crate::ui::widgets::{section_label, wrap_in_row};

use super::EditSession;

/// Delay before rendering preview after the last slider change (ms).
const RENDER_DEBOUNCE_MS: u32 = 50;

/// Snap values smaller than this deadzone to zero.
const DEADZONE: f64 = 0.02;

mod imp {
    use super::*;
    use std::cell::{Cell, OnceCell};

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/justinf555/Moments/ui/viewer/edit_panel/adjust_section.ui")]
    pub struct EditAdjustSection {
        #[template_child]
        pub expander: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub subtitle_label: TemplateChild<gtk::Label>,

        pub session: OnceCell<Rc<RefCell<Option<EditSession>>>>,
        pub changed_cb: OnceCell<Rc<dyn Fn()>>,
        pub render_debounce: Cell<Option<glib::SourceId>>,
        /// All slider scales (for reset and subtitle counting).
        pub scales: RefCell<Vec<gtk::Scale>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EditAdjustSection {
        const NAME: &'static str = "MomentsEditAdjustSection";
        type Type = super::EditAdjustSection;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for EditAdjustSection {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }
    impl WidgetImpl for EditAdjustSection {}
}

glib::wrapper! {
    pub struct EditAdjustSection(ObjectSubclass<imp::EditAdjustSection>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl EditAdjustSection {
    /// Access the expander row (for single-expansion wiring).
    pub fn expander(&self) -> &adw::ExpanderRow {
        &self.imp().expander
    }

    /// Build adjustment sliders from the registry and wire them.
    pub fn setup(&self, session: Rc<RefCell<Option<EditSession>>>, changed: impl Fn() + 'static) {
        let imp = self.imp();
        let changed: Rc<dyn Fn()> = Rc::new(changed);
        let _ = imp.session.set(Rc::clone(&session));
        let _ = imp.changed_cb.set(Rc::clone(&changed));

        let adjustments = adjustment_registry();
        let mut current_group: Option<AdjustGroup> = None;

        for adjustment in adjustments {
            // Add group header when the group changes.
            let group = adjustment.group();
            if current_group != Some(group) {
                let group_name = match group {
                    AdjustGroup::Light => "LIGHT",
                    AdjustGroup::Colour => "COLOUR",
                };
                let label = section_label(group_name);
                imp.expander.add_row(&wrap_in_row(&label));
                current_group = Some(group);
            }

            let (min, max) = adjustment.range();
            let slider_widget = self.build_slider(
                adjustment.display_name(),
                min,
                max,
                &session,
                &changed,
                adjustment,
            );
            imp.expander.add_row(&wrap_in_row(&slider_widget));
        }
    }

    /// Build a single adjustment slider with label, value display, and scale.
    fn build_slider(
        &self,
        label: &str,
        min: f64,
        max: f64,
        session: &Rc<RefCell<Option<EditSession>>>,
        changed: &Rc<dyn Fn()>,
        adjustment: Box<dyn super::adjustments::Adjustment>,
    ) -> gtk::Box {
        let imp = self.imp();

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
        scale.set_range(min, max);
        scale.set_value(0.0);
        scale.set_draw_value(false);
        scale.set_increments(0.01, 0.1);

        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();
        row.append(&header_box);
        row.append(&scale);

        // Register this scale for reset and subtitle tracking.
        imp.scales.borrow_mut().push(scale.clone());

        let session_ref = Rc::clone(session);
        let changed_ref = Rc::clone(changed);
        let weak = self.downgrade();

        scale.connect_value_changed(move |scale| {
            let Some(section) = weak.upgrade() else {
                return;
            };
            let simp = section.imp();
            let value = scale.value();
            let value = if value.abs() < DEADZONE { 0.0 } else { value };

            // Update the numeric display (mapped to -100..100 range).
            value_label.set_label(&format!("{}", (value * 100.0).round() as i32));

            // Update state via the adjustment trait.
            {
                let mut session = session_ref.borrow_mut();
                let Some(s) = session.as_mut() else { return };
                adjustment.set(&mut s.state, value);
                s.render_gen += 1;
            }

            // Update subtitle with count of non-default sliders.
            section.update_subtitle();

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

        row
    }

    /// Update the expander subtitle with the count of non-default sliders.
    fn update_subtitle(&self) {
        let imp = self.imp();
        let count = imp
            .scales
            .borrow()
            .iter()
            .filter(|s| s.value().abs() > DEADZONE)
            .count();

        let text = match count {
            0 => "No changes".to_string(),
            1 => "1 change".to_string(),
            n => format!("{n} changes"),
        };

        imp.subtitle_label.set_label(&text);
    }

    /// Update slider values from the current session state.
    pub fn sync_from_state(&self) {
        let imp = self.imp();
        let Some(session_rc) = imp.session.get() else {
            return;
        };

        let session = session_rc.borrow();
        let Some(s) = session.as_ref() else { return };

        let adjustments = adjustment_registry();
        let scales = imp.scales.borrow();

        for (adj, scale) in adjustments.iter().zip(scales.iter()) {
            scale.set_value(adj.get(&s.state));
        }

        drop(session);
        self.update_subtitle();
    }

    /// Reset all sliders to 0.0 and update subtitle.
    pub fn reset(&self) {
        let imp = self.imp();
        for scale in imp.scales.borrow().iter() {
            scale.set_value(0.0);
        }
        imp.subtitle_label.set_label("No changes");
    }
}
