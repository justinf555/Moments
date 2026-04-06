//! Shared reusable UI components used across the info and edit panels.

use adw::prelude::*;
use gettextrs::gettext;
use gtk::glib::object::IsA;

/// Create an expander row with an optional icon prefix, title left,
/// and subtitle right-aligned as a suffix label.
///
/// Returns `(ExpanderRow, subtitle_label)` so callers can update the
/// subtitle dynamically (e.g. filter name, change count).
pub fn expander_row(
    icon_name: Option<&str>,
    title: &str,
    subtitle: &str,
    expanded: bool,
) -> (adw::ExpanderRow, gtk::Label) {
    let suffix = gtk::Label::builder()
        .label(subtitle)
        .halign(gtk::Align::End)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(20)
        .build();
    suffix.add_css_class("dim-label");

    let expander = adw::ExpanderRow::builder()
        .title(title)
        .show_enable_switch(false)
        .expanded(expanded)
        .build();

    if let Some(icon) = icon_name {
        expander.add_prefix(&gtk::Image::from_icon_name(icon));
    }

    expander.add_suffix(&suffix);

    (expander, suffix)
}

/// Create a detail row — label left (dimmed), value right (selectable).
///
/// Used inside expander rows for key-value metadata display.
pub fn detail_row(label: &str, value: &str) -> gtk::ListBoxRow {
    let hbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_start(12)
        .margin_end(12)
        .margin_top(8)
        .margin_bottom(8)
        .build();

    let label_widget = gtk::Label::builder()
        .label(label)
        .halign(gtk::Align::Start)
        .hexpand(true)
        .build();
    label_widget.add_css_class("dim-label");

    let value_widget = gtk::Label::builder()
        .label(value)
        .halign(gtk::Align::End)
        .selectable(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(24)
        .build();

    hbox.append(&label_widget);
    hbox.append(&value_widget);

    gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .child(&hbox)
        .build()
}

/// Create a section label (e.g. "LIGHT", "COLOUR") for inside an expander.
pub fn section_label(text: &str) -> gtk::Label {
    let label = gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .margin_top(12)
        .margin_start(12)
        .build();
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    label
}

/// Wrap a widget in a non-activatable ListBoxRow for use inside an ExpanderRow.
pub fn wrap_in_row(widget: &impl IsA<gtk::Widget>) -> gtk::ListBoxRow {
    gtk::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .child(widget)
        .build()
}

/// Update a star (favourite) button's icon, CSS class, and accessible label.
///
/// Used by photo viewer, video viewer, and anywhere an icon-only favourite
/// toggle button is needed. Sets `starred-symbolic` / `non-starred-symbolic`
/// icon and toggles the `warning` CSS class for the gold star colour.
pub fn update_star_button(btn: &gtk::Button, is_favourite: bool) {
    btn.set_icon_name(if is_favourite {
        "starred-symbolic"
    } else {
        "non-starred-symbolic"
    });
    if is_favourite {
        btn.add_css_class("warning");
    } else {
        btn.remove_css_class("warning");
    }
    let label = if is_favourite {
        gettext("Remove from favourites")
    } else {
        gettext("Add to favourites")
    };
    btn.update_property(&[gtk::accessible::Property::Label(&label)]);
}

/// Wire a group of expander rows so that only one can be expanded at a time.
///
/// When any expander in the group is expanded, all others are collapsed.
pub fn wire_single_expansion(expanders: &[&adw::ExpanderRow]) {
    for (i, expander) in expanders.iter().enumerate() {
        let others: Vec<adw::ExpanderRow> = expanders
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, e)| (*e).clone())
            .collect();

        expander.connect_expanded_notify(move |exp| {
            if exp.is_expanded() {
                for other in &others {
                    other.set_expanded(false);
                }
            }
        });
    }
}
