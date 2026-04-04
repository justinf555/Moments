use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use crate::library::edit_renderer::{filter_preset, FILTER_NAMES};

use super::{render_to_picture, EditPanel, RENDER_DEBOUNCE_MS};

/// Convert a filter preset name to a user-facing display name.
pub(super) fn filter_display_name(name: &str) -> &str {
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

    let btn = gtk::ToggleButton::builder().child(&swatch).build();
    btn.add_css_class("flat");
    btn.add_css_class("filter-button");

    btn
}

impl EditPanel {
    /// Populate the Filters expander with preset grid and strength slider.
    pub(super) fn build_filters_content(&self, expander: &adw::ExpanderRow) {
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
            self.filter_buttons
                .borrow_mut()
                .push(("original".to_string(), original_btn.clone()));

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
                            s.state.exposure.white_balance =
                                preset.exposure.white_balance * strength;
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
}
