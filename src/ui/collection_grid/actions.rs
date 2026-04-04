use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};
use tracing::{debug, info};

use crate::library::faces::PersonId;
use crate::library::media::MediaFilter;
use crate::library::Library;
use crate::ui::photo_grid::model::PhotoGridModel;
use crate::ui::photo_grid::texture_cache::TextureCache;
use crate::ui::photo_grid::PhotoGridView;
use crate::ui::ContentView;

use super::item::{CollectionItemData, CollectionItemObject};
use super::PeopleFilter;

/// Wire item activation — clicking a person pushes a filtered PhotoGridView.
#[allow(clippy::too_many_arguments)]
pub(super) fn wire_activation(
    grid_view: &gtk::GridView,
    store: &gio::ListStore,
    nav_view: &adw::NavigationView,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    settings: &gio::Settings,
    texture_cache: &Rc<TextureCache>,
    bus_sender: &crate::event_bus::EventSender,
) {
    let nav = nav_view.clone();
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let s = settings.clone();
    let tc = Rc::clone(texture_cache);
    let bs = bus_sender.clone();
    let store_ref = store.clone();

    grid_view.connect_activate(move |_, position| {
        let Some(obj) = store_ref
            .item(position)
            .and_then(|o| o.downcast::<CollectionItemObject>().ok())
        else {
            return;
        };

        let data = obj.data();
        let person_id = PersonId::from_raw(data.id.clone());
        debug!(person = %data.name, id = %data.id, "person activated");

        let filter = MediaFilter::Person {
            person_id: person_id.clone(),
        };
        let model = Rc::new(PhotoGridModel::new(
            Arc::clone(&lib),
            tk.clone(),
            filter,
            bs.clone(),
        ));
        let view = Rc::new(PhotoGridView::new(
            Arc::clone(&lib),
            tk.clone(),
            s.clone(),
            Rc::clone(&tc),
            bs.clone(),
        ));
        view.set_model(Rc::clone(&model));
        model.subscribe_to_bus();

        let display_name = if data.name.is_empty() {
            "Unnamed".to_string()
        } else {
            data.name.clone()
        };

        let person_page = adw::NavigationPage::builder()
            .tag(format!("person:{}", data.id))
            .title(&display_name)
            .child(view.widget())
            .build();

        // Install the person grid's zoom actions on the window.
        if let Some(actions) = view.view_actions() {
            if let Some(win) = nav.root().and_then(|r| r.downcast::<gtk::Window>().ok()) {
                win.insert_action_group("view", Some(actions));
            }
        }

        nav.push(&person_page);
    });
}

/// Wire the right-click context menu on people grid cells.
#[allow(clippy::too_many_arguments)]
pub(super) fn wire_context_menu(
    grid_view: &gtk::GridView,
    store: &gio::ListStore,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    filter: &Rc<PeopleFilter>,
) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(3);

    let gv = grid_view.clone();
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let store_ctx = store.clone();
    let filter_ctx = Rc::clone(filter);

    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(pos) = find_clicked_position(&gv, &store_ctx, x, y) else {
            return;
        };

        let Some(obj) = store_ctx
            .item(pos)
            .and_then(|o| o.downcast::<CollectionItemObject>().ok())
        else {
            return;
        };

        let data = obj.data();
        let person_id = data.id.clone();
        let is_hidden = data.is_hidden;

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
        let hide_label = if is_hidden { gettext("Unhide") } else { gettext("Hide") };
        let hide_btn = gtk::Button::with_label(&hide_label);
        hide_btn.add_css_class("flat");
        vbox.append(&hide_btn);

        // Wire rename.
        wire_rename_button(
            &rename_btn,
            &popover,
            &gv,
            &lib,
            &tk,
            &store_ctx,
            data,
        );

        // Wire hide/unhide.
        wire_hide_button(
            &hide_btn,
            &popover,
            &gv,
            &lib,
            &tk,
            &store_ctx,
            &filter_ctx,
            &person_id,
            is_hidden,
        );

        popover.set_child(Some(&vbox));
        popover.set_parent(&gv);
        popover.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
            x as i32, y as i32, 1, 1,
        )));
        popover.set_has_arrow(true);

        popover.connect_closed(move |p| {
            p.unparent();
        });

        popover.popup();
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });

    grid_view.add_controller(gesture);
}

/// Find the store position of the item at (x, y) by resolving the cell's
/// bound data. This is correct even when the grid is scrolled (unlike
/// counting siblings, which only works for non-virtualized lists).
fn find_clicked_position(
    grid_view: &gtk::GridView,
    store: &gio::ListStore,
    x: f64,
    y: f64,
) -> Option<u32> {
    let picked = grid_view.pick(x, y, gtk::PickFlags::DEFAULT)?;

    // Walk up from the picked widget to find the CollectionGridCell.
    let mut widget = Some(picked);
    while let Some(ref w) = widget {
        if let Some(cell) = w.downcast_ref::<super::cell::CollectionGridCell>() {
            let item = cell.bound_item()?;
            let target_id = item.data().id.clone();
            // Search the store for the matching item.
            for i in 0..store.n_items() {
                if let Some(obj) = store
                    .item(i)
                    .and_then(|o| o.downcast::<CollectionItemObject>().ok())
                {
                    if obj.data().id == target_id {
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
#[allow(clippy::too_many_arguments)]
fn wire_rename_button(
    btn: &gtk::Button,
    popover: &gtk::Popover,
    grid_view: &gtk::GridView,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    store: &gio::ListStore,
    data: &CollectionItemData,
) {
    let pop_weak = popover.downgrade();
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let store = store.clone();
    let person_id = data.id.clone();
    let current_name = data.name.clone();
    let subtitle = data.subtitle.clone();
    let thumb = data.thumbnail_path.clone();
    let hidden = data.is_hidden;
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

        let lib = Arc::clone(&lib);
        let tk = tk.clone();
        let store = store.clone();
        let pid = person_id.clone();
        let subtitle = subtitle.clone();
        let thumb = thumb.clone();
        let gv_toast = gv_ref.clone();
        dialog.connect_response(None, move |_, response| {
            if response != "rename" {
                return;
            }
            let new_name = entry.text().to_string();
            if new_name.is_empty() {
                return;
            }
            let pid_str = pid.clone();
            let pid = PersonId::from_raw(pid.clone());
            let lib = Arc::clone(&lib);
            let tk = tk.clone();
            let store = store.clone();
            let subtitle = subtitle.clone();
            let thumb = thumb.clone();
            let gv_toast = gv_toast.clone();
            debug!(person_id = %pid, name = %new_name, "renaming person");
            glib::MainContext::default().spawn_local(async move {
                let name = new_name.clone();
                let result = tk
                    .spawn(async move { lib.rename_person(&pid, &name).await })
                    .await;
                match result {
                    Ok(Ok(())) => {
                        info!("person renamed successfully");
                        super::replace_item(
                            &store,
                            &pid_str,
                            CollectionItemData {
                                id: pid_str.clone(),
                                name: new_name,
                                subtitle,
                                thumbnail_path: thumb,
                                is_hidden: hidden,
                            },
                        );
                    }
                    Ok(Err(e)) => {
                        tracing::error!("rename_person failed: {e}");
                        let _ = gv_toast.activate_action(
                            "win.show-toast",
                            Some(&"Failed to rename person".to_variant()),
                        );
                    }
                    Err(e) => tracing::error!("rename_person join failed: {e}"),
                }
            });
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
#[allow(clippy::too_many_arguments)]
fn wire_hide_button(
    btn: &gtk::Button,
    popover: &gtk::Popover,
    grid_view: &gtk::GridView,
    library: &Arc<dyn Library>,
    tokio: &tokio::runtime::Handle,
    store: &gio::ListStore,
    filter: &Rc<PeopleFilter>,
    person_id: &str,
    is_hidden: bool,
) {
    let pop_weak = popover.downgrade();
    let lib = Arc::clone(library);
    let tk = tokio.clone();
    let store = store.clone();
    let f = Rc::clone(filter);
    let new_hidden = !is_hidden;
    let gv_hide = grid_view.clone();
    let pid_str = person_id.to_owned();

    btn.connect_clicked(move |_| {
        if let Some(p) = pop_weak.upgrade() {
            p.popdown();
        }
        let pid = PersonId::from_raw(pid_str.clone());
        let lib = Arc::clone(&lib);
        let tk = tk.clone();
        let store = store.clone();
        let f = Rc::clone(&f);
        let action = if new_hidden { "hiding" } else { "unhiding" };
        debug!(person_id = %pid, action, "toggling person visibility");
        let pid_for_remove = pid.to_string();
        let gv_hide = gv_hide.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = tk
                .spawn(async move { lib.set_person_hidden(&pid, new_hidden).await })
                .await;
            match result {
                Ok(Ok(())) => {
                    info!("person visibility changed successfully");
                    if new_hidden && !f.include_hidden.get() {
                        super::remove_by_id(&store, &pid_for_remove);
                    }
                }
                Ok(Err(e)) => {
                    tracing::error!("set_person_hidden failed: {e}");
                    let _ = gv_hide.activate_action(
                        "win.show-toast",
                        Some(&"Failed to update person visibility".to_variant()),
                    );
                }
                Err(e) => tracing::error!("set_person_hidden join failed: {e}"),
            }
        });
    });
}
