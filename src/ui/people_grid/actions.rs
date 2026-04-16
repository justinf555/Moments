use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::gio;
use tracing::debug;

use crate::client::{PeopleClientV2, PersonItemObject};
use crate::library::faces::PersonId;
use crate::library::media::MediaFilter;
use crate::ui::photo_grid::texture_cache::TextureCache;
use crate::ui::photo_grid::PhotoGridView;

/// Wire item activation — clicking a person pushes a filtered PhotoGridView.
pub(super) fn wire_activation(
    grid_view: &gtk::GridView,
    filter_model: &gtk::FilterListModel,
    nav_view: &adw::NavigationView,
    settings: &gio::Settings,
    texture_cache: &Rc<TextureCache>,
    bus_sender: &crate::event_bus::EventSender,
) {
    let nav = nav_view.clone();
    let s = settings.clone();
    let tc = Rc::clone(texture_cache);
    let bs = bus_sender.clone();
    let fm = filter_model.clone();

    grid_view.connect_activate(move |_, position| {
        let Some(obj) = fm
            .item(position)
            .and_then(|o| o.downcast::<PersonItemObject>().ok())
        else {
            return;
        };

        let person_id = PersonId::from_raw(obj.id());
        let name = obj.name();
        debug!(person = %name, id = %obj.id(), "person activated");

        let filter = MediaFilter::Person {
            person_id: person_id.clone(),
        };
        let mc = crate::application::MomentsApplication::default()
            .media_client()
            .expect("media client available");
        let store = mc.create_model(filter.clone());
        let view = PhotoGridView::new();
        view.setup(s.clone(), Rc::clone(&tc), bs.clone());
        view.set_store(store, filter);

        let display_name = if name.is_empty() {
            gettext("Unnamed")
        } else {
            name
        };

        let person_page = adw::NavigationPage::builder()
            .tag(format!("person:{}", obj.id()))
            .title(&display_name)
            .child(&view)
            .build();

        nav.push(&person_page);
    });
}

/// Wire the right-click context menu on people grid cells.
pub(super) fn wire_context_menu(
    grid_view: &gtk::GridView,
    filter_model: &gtk::FilterListModel,
    people_client: &PeopleClientV2,
) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);

    let gv = grid_view.clone();
    let fm = filter_model.clone();
    let pc = people_client.clone();

    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(pos) = find_clicked_position(&gv, &fm, x, y) else {
            return;
        };

        let Some(obj) = fm
            .item(pos)
            .and_then(|o| o.downcast::<PersonItemObject>().ok())
        else {
            return;
        };

        let person_id = obj.id();
        let current_name = obj.name();
        let is_hidden = obj.is_hidden();

        let vbox = gtk::Box::new(gtk::Orientation::Vertical, 0);
        vbox.set_margin_top(6);
        vbox.set_margin_bottom(6);
        vbox.set_margin_start(6);
        vbox.set_margin_end(6);

        let popover = gtk::Popover::new();

        // ── Rename button ──
        let rename_btn = gtk::Button::with_label(&gettext("Rename"));
        rename_btn.add_css_class("flat");
        vbox.append(&rename_btn);

        // ── Hide/Unhide button ──
        let hide_label = if is_hidden {
            gettext("Unhide")
        } else {
            gettext("Hide")
        };
        let hide_btn = gtk::Button::with_label(&hide_label);
        hide_btn.add_css_class("flat");
        vbox.append(&hide_btn);

        // Wire rename.
        wire_rename_button(&rename_btn, &popover, &gv, &pc, &person_id, &current_name);

        // Wire hide/unhide.
        wire_hide_button(&hide_btn, &popover, &pc, &person_id, is_hidden);

        popover.set_child(Some(&vbox));
        popover.set_parent(&gv);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.set_has_arrow(true);

        popover.connect_closed(move |p| {
            p.unparent();
        });

        popover.popup();
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });

    grid_view.add_controller(gesture);
}

/// Find the filtered model position of the item at (x, y) by resolving
/// the cell's bound data.
fn find_clicked_position(
    grid_view: &gtk::GridView,
    filter_model: &gtk::FilterListModel,
    x: f64,
    y: f64,
) -> Option<u32> {
    let picked = grid_view.pick(x, y, gtk::PickFlags::DEFAULT)?;

    let mut widget = Some(picked);
    while let Some(ref w) = widget {
        if let Some(cell) = w.downcast_ref::<super::cell::PeopleGridCell>() {
            let item = cell.bound_item()?;
            let target_id = item.id();
            for i in 0..filter_model.n_items() {
                if let Some(obj) = filter_model
                    .item(i)
                    .and_then(|o| o.downcast::<PersonItemObject>().ok())
                {
                    if obj.id() == target_id {
                        return Some(i);
                    }
                }
            }
            return None;
        }
        widget = w.parent();
    }
    None
}

/// Wire the Rename button to show a rename dialog.
fn wire_rename_button(
    btn: &gtk::Button,
    popover: &gtk::Popover,
    grid_view: &gtk::GridView,
    people_client: &PeopleClientV2,
    person_id: &str,
    current_name: &str,
) {
    let pop_weak = popover.downgrade();
    let pc = people_client.clone();
    let pid = person_id.to_owned();
    let current_name = current_name.to_owned();
    let gv_ref = grid_view.clone();

    btn.connect_clicked(move |_| {
        if let Some(p) = pop_weak.upgrade() {
            p.popdown();
        }

        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Rename Person"))
            .build();
        dialog.add_response("cancel", &gettext("Cancel"));
        dialog.add_response("rename", &gettext("Rename"));
        dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("rename"));
        dialog.set_close_response("cancel");

        let entry = gtk::Entry::new();
        entry.set_text(&current_name);
        entry.set_activates_default(true);
        dialog.set_extra_child(Some(&entry));

        let pc = pc.clone();
        let pid = pid.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "rename" {
                return;
            }
            let new_name = entry.text().to_string();
            if new_name.is_empty() {
                return;
            }
            debug!(person_id = %pid, name = %new_name, "renaming person");
            pc.rename_person(PersonId::from_raw(pid.clone()), new_name);
        });
        dialog.present(
            gv_ref
                .root()
                .as_ref()
                .and_then(|r| r.downcast_ref::<gtk::Window>()),
        );
    });
}

/// Wire the Hide/Unhide button.
fn wire_hide_button(
    btn: &gtk::Button,
    popover: &gtk::Popover,
    people_client: &PeopleClientV2,
    person_id: &str,
    is_hidden: bool,
) {
    let pop_weak = popover.downgrade();
    let pc = people_client.clone();
    let new_hidden = !is_hidden;
    let pid = person_id.to_owned();

    btn.connect_clicked(move |_| {
        if let Some(p) = pop_weak.upgrade() {
            p.popdown();
        }
        let action = if new_hidden { "hiding" } else { "unhiding" };
        debug!(person_id = %pid, action, "toggling person visibility");
        pc.set_person_hidden(PersonId::from_raw(pid.clone()), new_hidden);
    });
}
