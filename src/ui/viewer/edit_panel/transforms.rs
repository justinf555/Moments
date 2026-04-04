use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gettextrs::gettext;

use super::{render_to_picture, EditPanel, EditSession};

/// Create a transform action button with icon and label for the 2x2 grid.
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

impl EditPanel {
    /// Populate the Transform expander with rotate/flip buttons.
    pub(super) fn build_transform_content(&self, expander: &adw::ExpanderRow) {
        let grid = gtk::Grid::builder()
            .column_spacing(8)
            .row_spacing(8)
            .margin_top(8)
            .margin_bottom(8)
            .margin_start(12)
            .margin_end(12)
            .column_homogeneous(true)
            .build();

        let rotate_ccw_btn = make_transform_button(
            "object-rotate-left-symbolic",
            "Rotate CCW",
            &gettext("Rotate Left"),
        );
        let rotate_cw_btn = make_transform_button(
            "object-rotate-right-symbolic",
            "Rotate CW",
            &gettext("Rotate Right"),
        );
        let flip_h_btn = make_transform_button(
            "object-flip-horizontal-symbolic",
            "Flip H",
            &gettext("Flip Horizontal"),
        );
        let flip_v_btn = make_transform_button(
            "object-flip-vertical-symbolic",
            "Flip V",
            &gettext("Flip Vertical"),
        );

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
        wire_transform_button(&rotate_ccw_btn, self, |s| {
            s.state.transforms.rotate_degrees =
                (s.state.transforms.rotate_degrees - 90).rem_euclid(360);
        });

        // Wire rotate CW.
        wire_transform_button(&rotate_cw_btn, self, |s| {
            s.state.transforms.rotate_degrees =
                (s.state.transforms.rotate_degrees + 90).rem_euclid(360);
        });

        // Wire flip horizontal.
        wire_transform_button(&flip_h_btn, self, |s| {
            s.state.transforms.flip_horizontal = !s.state.transforms.flip_horizontal;
        });

        // Wire flip vertical.
        wire_transform_button(&flip_v_btn, self, |s| {
            s.state.transforms.flip_vertical = !s.state.transforms.flip_vertical;
        });
    }
}

/// Wire a transform button to mutate the edit state and re-render.
fn wire_transform_button<F>(btn: &gtk::Button, panel: &EditPanel, mutate: F)
where
    F: Fn(&mut EditSession) + 'static,
{
    let session = Rc::clone(&panel.session);
    let picture = panel.picture.clone();
    let tokio = panel.tokio.clone();
    let auto_save = panel.auto_save_closure();

    btn.connect_clicked(move |_| {
        let preview = {
            let mut session = session.borrow_mut();
            let Some(s) = session.as_mut() else { return };
            mutate(s);
            s.render_gen += 1;
            (Arc::clone(&s.preview_image), s.state.clone(), s.render_gen)
        };
        render_to_picture(&picture, &tokio, &session, preview);
        auto_save();
    });
}
