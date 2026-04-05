use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::glib;

use crate::library::editing::EditState;
use crate::ui::widgets::{section_label, wrap_in_row};

use super::{EditPanel, RENDER_DEBOUNCE_MS};

/// Type-erased accessor closure for EditState fields.
pub(super) fn accessor_fn(
    f: fn(&mut EditState) -> &mut f64,
) -> Box<dyn Fn(&mut EditState) -> &mut f64> {
    Box::new(f)
}

/// Update the Adjust expander subtitle with the count of non-default sliders.
fn update_adjust_subtitle(panel: &EditPanel) {
    let imp = panel.imp();
    let count = imp
        .adjust_scales
        .borrow()
        .iter()
        .filter(|s| s.value().abs() > 0.02)
        .count();

    let text = match count {
        0 => "No changes".to_string(),
        1 => "1 change".to_string(),
        n => format!("{n} changes"),
    };

    imp.adjust_subtitle.set_label(&text);
}

impl EditPanel {
    /// Populate the Adjust expander with Light and Colour slider groups.
    pub(super) fn build_adjust_content(&self) {
        let imp = self.imp();

        // ── Light group ──────────────────────────────────────────────────────
        let light_label = section_label("LIGHT");
        imp.adjust_expander.add_row(&wrap_in_row(&light_label));

        for (name, accessor) in [
            (
                "Brightness",
                accessor_fn(|s: &mut EditState| &mut s.exposure.brightness),
            ),
            (
                "Contrast",
                accessor_fn(|s: &mut EditState| &mut s.exposure.contrast),
            ),
            (
                "Highlights",
                accessor_fn(|s: &mut EditState| &mut s.exposure.highlights),
            ),
            (
                "Shadows",
                accessor_fn(|s: &mut EditState| &mut s.exposure.shadows),
            ),
            (
                "White Balance",
                accessor_fn(|s: &mut EditState| &mut s.exposure.white_balance),
            ),
        ] {
            let slider = self.make_slider(name, accessor);
            imp.adjust_expander.add_row(&wrap_in_row(&slider));
        }

        // ── Colour group ─────────────────────────────────────────────────────
        let colour_label = section_label("COLOUR");
        imp.adjust_expander.add_row(&wrap_in_row(&colour_label));

        for (name, accessor) in [
            (
                "Saturation",
                accessor_fn(|s: &mut EditState| &mut s.color.saturation),
            ),
            (
                "Vibrance",
                accessor_fn(|s: &mut EditState| &mut s.color.vibrance),
            ),
            (
                "Temperature",
                accessor_fn(|s: &mut EditState| &mut s.color.temperature),
            ),
            ("Tint", accessor_fn(|s: &mut EditState| &mut s.color.tint)),
        ] {
            let slider = self.make_slider(name, accessor);
            imp.adjust_expander.add_row(&wrap_in_row(&slider));
        }
    }

    /// Create a slider row with label, value label, and scale.
    pub(super) fn make_slider_row<D: Fn(f64) -> i32 + 'static>(
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

    /// Create an adjust slider with label, numeric value, and scale.
    pub(super) fn make_slider<F>(&self, label: &str, accessor: F) -> gtk::Box
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

        // Register this scale for revert.
        self.imp().adjust_scales.borrow_mut().push(scale.clone());

        let weak = self.downgrade();
        let auto_save = self.auto_save_closure();

        scale.connect_value_changed(move |scale| {
            let Some(panel) = weak.upgrade() else { return };
            let imp = panel.imp();
            let value = scale.value();
            let value = if value.abs() < 0.02 { 0.0 } else { value };

            // Update the numeric display (mapped to -100..100 range).
            value_label.set_label(&format!("{}", (value * 100.0).round() as i32));

            {
                let mut session = imp.session.borrow_mut();
                let Some(s) = session.as_mut() else { return };
                *accessor(&mut s.state) = value;
                s.render_gen += 1;
            }

            // Update adjust subtitle with count of non-default sliders.
            update_adjust_subtitle(&panel);

            // Cancel any pending render debounce timer.
            if let Some(id) = imp.render_debounce.take() {
                id.remove();
            }

            // Schedule a new render after the debounce period.
            let weak_inner = panel.downgrade();
            let source_id = glib::timeout_add_local_once(
                std::time::Duration::from_millis(RENDER_DEBOUNCE_MS as u64),
                move || {
                    let Some(panel) = weak_inner.upgrade() else { return };
                    panel.imp().render_debounce.set(None);
                    panel.render_preview();
                },
            );
            imp.render_debounce.set(Some(source_id));

            // Schedule auto-save.
            auto_save();
        });

        row
    }
}
